//! Provider-agnostic LLM client.
//!
//! The assistant speaks to exactly one of two wire formats, chosen in the
//! `[ai]` config section:
//!   * `provider = "claude"` — the Anthropic Messages API (`/v1/messages`)
//!   * `provider = "openai"` — any OpenAI-compatible chat-completions endpoint
//!     (`/v1/chat/completions`), which covers OpenAI itself plus local servers
//!     like Ollama, vLLM, or LM Studio.
//!
//! Both formats support native tool calling, so the conversation is modeled
//! here as provider-neutral [`Turn`]s and translated to each wire shape at
//! request time. No SDK is used — just `reqwest` and JSON.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long to wait on a single request before giving up. Hosted APIs answer in
/// seconds; a local model loading weights on the first call can take much longer,
/// hence the generous ceiling.
const REQUEST_TIMEOUT_SECS: u64 = 600;

/// Default reply-length ceiling when the config doesn't set one. Claude requires
/// an explicit `max_tokens`; this is a safe value that rarely truncates a normal
/// answer or tool call.
const DEFAULT_MAX_TOKENS: u64 = 8192;

/// How many times to re-send a request that failed for a transient reason
/// (rate-limit, 5xx, overloaded, or a network blip). A long autonomous run will
/// eventually hit one of these; without retries a single blip aborts the whole
/// task, which is the main thing that stops the agent finishing long jobs.
const MAX_SEND_ATTEMPTS: u32 = 5;

/// Base for exponential backoff between retries; the nth retry waits roughly
/// `RETRY_BASE * 2^(n-1)` plus jitter, capped at [`RETRY_CAP`].
const RETRY_BASE: Duration = Duration::from_millis(500);
const RETRY_CAP: Duration = Duration::from_secs(30);

/// Which wire format to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Claude,
    OpenAi,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "claude" | "anthropic" => Ok(ProviderKind::Claude),
            // vLLM, Ollama, LM Studio, etc. all speak the OpenAI chat format.
            "openai" | "openai-compatible" | "compatible" | "vllm" | "ollama" | "lmstudio"
            | "lm-studio" => Ok(ProviderKind::OpenAi),
            other => bail!("Unknown AI provider '{other}' (expected: claude, openai, or vllm)"),
        }
    }
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: String,
    pub parameters: Value,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// The result of executing one [`ToolCall`] locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// One provider-neutral conversation turn. Serializable so a whole session can
/// be saved under `.ciabatta/ai/conversations/` and resumed later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Turn {
    User(String),
    Assistant(AssistantTurn),
    ToolResults(Vec<ToolOutput>),
}

/// What the model said on one assistant turn: prose plus zero or more tool calls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistantTurn {
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// True when the provider declined the request (Claude `stop_reason: refusal`).
    #[serde(default)]
    pub refused: bool,
    /// True when the reply hit the `max_tokens` ceiling and was cut off mid-turn
    /// (Claude `stop_reason: max_tokens`, OpenAI `finish_reason: length`). A
    /// truncated turn's tool calls may be incomplete, so the loop must not treat
    /// it as a finished answer — it asks the model to continue instead.
    #[serde(default)]
    pub truncated: bool,
}

/// Token accounting for one request, surfaced so the front end can show cost and
/// so cache effectiveness stays observable. Cache fields are only populated by
/// providers that report them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

impl Usage {
    /// A compact one-line summary for a status pill, e.g. `1.2k in · 340 out`.
    pub fn label(&self) -> String {
        let k = |n: u64| {
            if n >= 1000 {
                format!("{:.1}k", n as f64 / 1000.0)
            } else {
                n.to_string()
            }
        };
        let mut s = format!(
            "{} in · {} out",
            k(self.input_tokens),
            k(self.output_tokens)
        );
        if self.cache_read_tokens > 0 {
            s.push_str(&format!(" · {} cached", k(self.cache_read_tokens)));
        }
        s
    }
}

/// The configured LLM endpoint.
pub struct Provider {
    pub kind: ProviderKind,
    endpoint: String,
    pub model: String,
    api_key: String,
    client: reqwest::Client,
    /// Reply-length ceiling per request (always sent to Claude; sent to
    /// OpenAI-compatible endpoints only when the user configured one).
    max_tokens: u64,
    max_tokens_configured: bool,
    /// Token usage from the most recent request, for the front end's status.
    last_usage: Mutex<Option<Usage>>,
}

impl Provider {
    /// Build a provider from the `[ai]` config section, resolving the API key
    /// from the configured environment variable.
    pub fn from_config(cfg: &crate::config::AiConfig) -> Result<Self> {
        let raw_provider = cfg
            .provider
            .as_deref()
            .unwrap_or("claude")
            .trim()
            .to_lowercase();
        let kind = ProviderKind::parse(&raw_provider)?;
        let is_vllm = raw_provider == "vllm";

        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| match kind {
                // vLLM's default serving port; override in config for a remote host.
                _ if is_vllm => "http://localhost:8000".to_string(),
                ProviderKind::Claude => "https://api.anthropic.com".to_string(),
                ProviderKind::OpenAi => "https://api.openai.com".to_string(),
            })
            .trim_end_matches('/')
            .to_string();

        let model = cfg.model.clone().unwrap_or_else(|| match kind {
            ProviderKind::Claude => "claude-opus-4-8".to_string(),
            ProviderKind::OpenAi => "gpt-4o".to_string(),
        });

        let key_env = cfg.api_key_env.clone().unwrap_or_else(|| match kind {
            ProviderKind::Claude => "ANTHROPIC_API_KEY".to_string(),
            ProviderKind::OpenAi => "OPENAI_API_KEY".to_string(),
        });
        // Local OpenAI-compatible servers often need no key; only Claude's
        // hosted API strictly requires one.
        let api_key = std::env::var(&key_env).unwrap_or_default();
        if api_key.is_empty() && kind == ProviderKind::Claude {
            bail!(
                "No API key found: set the {key_env} environment variable \
                 (or point [ai] api_key_env at the variable that holds your key)."
            );
        }

        // Self-hosted endpoints (vLLM behind a self-signed cert, local dev)
        // often can't present a CA-trusted certificate; `tls_verify = false`
        // lets the user opt out of verification for exactly those cases.
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));
        if !cfg.tls_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder
            .build()
            .context("Failed to build HTTP client for the AI provider")?;

        Ok(Self {
            kind,
            endpoint,
            model,
            api_key,
            client,
            max_tokens: cfg.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1),
            max_tokens_configured: cfg.max_tokens.is_some(),
            last_usage: Mutex::new(None),
        })
    }

    /// Token usage from the most recent request, if any has completed.
    pub fn last_usage(&self) -> Option<Usage> {
        *self.last_usage.lock().unwrap()
    }

    /// How many characters of transcript to keep before compacting, sized to the
    /// model's context window rather than a fixed constant — so a small local
    /// model (8–32k tokens) compacts long before it overflows, while a
    /// large-window hosted model isn't compacted needlessly. Roughly half the
    /// window (at ~4 chars/token), leaving room for the system prompt, tool
    /// definitions, the reply, and fresh tool output.
    pub fn context_budget_chars(&self) -> usize {
        let window_tokens = context_window_tokens(&self.model, self.kind);
        (window_tokens / 2) * 4
    }

    /// Send a prepared request, retrying transient failures (rate-limits, 5xx,
    /// overloaded responses, and network blips) with exponential backoff so a
    /// single hiccup doesn't abort a long-running task. Returns the final
    /// response's status and body; a retryable status that never clears is
    /// returned as-is for the caller's normal error handling.
    async fn send_retrying(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, String)> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let this = req
                .try_clone()
                .ok_or_else(|| anyhow!("internal error: request could not be cloned for retry"))?;
            match this.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // Honor a server-provided Retry-After (seconds) if present.
                    let retry_after = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let raw = resp.text().await.map_err(|e| self.send_error(e))?;
                    if is_retryable_status(status) && attempt < MAX_SEND_ATTEMPTS {
                        let delay = retry_after.unwrap_or_else(|| backoff(attempt));
                        tracing::debug!("retrying after {status} (attempt {attempt}) in {delay:?}");
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok((status, raw));
                }
                Err(e) => {
                    if is_retryable_err(&e) && attempt < MAX_SEND_ATTEMPTS {
                        let delay = backoff(attempt);
                        tracing::debug!(
                            "retrying after network error (attempt {attempt}) in {delay:?}"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(self.send_error(e));
                }
            }
        }
    }

    /// Turn a low-level transport failure into a message that says *why* the
    /// request never got an answer — the difference between "timed out",
    /// "nothing is listening", and "the host doesn't resolve" is exactly what a
    /// stuck "thinking" spinner otherwise hides.
    fn send_error(&self, err: reqwest::Error) -> anyhow::Error {
        let target = match self.kind {
            ProviderKind::Claude => "the Claude API",
            ProviderKind::OpenAi => "the AI endpoint",
        };
        let where_ = format!("{target} at {}", self.endpoint);
        let detail = if err.is_timeout() {
            format!(
                "no response from {where_} within {REQUEST_TIMEOUT_SECS}s (the request timed out). \
                 The server accepted the connection but never replied — a local model may still be \
                 loading its weights, or the endpoint is wrong."
            )
        } else if err.is_connect() {
            format!(
                "could not connect to {where_}. Check the endpoint is correct and reachable — \
                 for a local model, is the server running and listening on that port? \
                 (A self-signed certificate needs `tls_verify = false` in the [ai] config.)"
            )
        } else if err.is_request() {
            format!("could not send the request to {where_}: {err}")
        } else {
            format!("request to {where_} failed: {err}")
        };
        anyhow!(detail)
    }

    /// A short human-readable label for status lines.
    pub fn label(&self) -> String {
        match self.kind {
            ProviderKind::Claude => format!("claude ({})", self.model),
            ProviderKind::OpenAi => format!("openai ({} @ {})", self.model, self.endpoint),
        }
    }

    /// Send the conversation and return the model's next assistant turn.
    pub async fn chat(
        &self,
        system: &str,
        turns: &[Turn],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn> {
        match self.kind {
            ProviderKind::Claude => self.chat_claude(system, turns, tools).await,
            ProviderKind::OpenAi => self.chat_openai(system, turns, tools).await,
        }
    }

    // ─── Claude Messages API ───────────────────────────────────────────────

    async fn chat_claude(
        &self,
        system: &str,
        turns: &[Turn],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn> {
        let mut messages: Vec<Value> = Vec::new();
        for turn in turns {
            match turn {
                Turn::User(text) => messages.push(json!({"role": "user", "content": text})),
                Turn::Assistant(a) => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !a.text.is_empty() {
                        blocks.push(json!({"type": "text", "text": a.text}));
                    }
                    for call in &a.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.args,
                        }));
                    }
                    if blocks.is_empty() {
                        blocks.push(json!({"type": "text", "text": ""}));
                    }
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
                Turn::ToolResults(results) => {
                    let blocks: Vec<Value> = results
                        .iter()
                        .map(|r| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": r.call_id,
                                "content": r.content,
                                "is_error": r.is_error,
                            })
                        })
                        .collect();
                    messages.push(json!({"role": "user", "content": blocks}));
                }
            }
        }

        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": messages,
        });
        if !tool_defs.is_empty() {
            body["tools"] = Value::Array(tool_defs);
        }

        let req = self
            .client
            .post(format!("{}/v1/messages", self.endpoint))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        let (status, raw) = self.send_retrying(req).await?;
        let payload: Value = serde_json::from_str(&raw).map_err(|e| {
            anyhow!(
                "Claude API returned a non-JSON response ({status}): {e}\n{}",
                snippet(&raw)
            )
        })?;
        if !status.is_success() {
            let msg = payload["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Claude API error ({status}): {msg}");
        }

        self.record_usage(Usage {
            input_tokens: payload["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: payload["usage"]["output_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: payload["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0),
        });

        // A refusal is a successful HTTP response — check stop_reason before content.
        if payload["stop_reason"].as_str() == Some("refusal") {
            return Ok(AssistantTurn {
                text: "The model declined this request.".to_string(),
                refused: true,
                ..Default::default()
            });
        }

        let mut out = AssistantTurn {
            truncated: payload["stop_reason"].as_str() == Some("max_tokens"),
            ..Default::default()
        };
        for block in payload["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        if !out.text.is_empty() {
                            out.text.push('\n');
                        }
                        out.text.push_str(t);
                    }
                }
                Some("tool_use") => out.tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    args: block["input"].clone(),
                }),
                _ => {}
            }
        }
        Ok(out)
    }

    // ─── OpenAI-compatible chat completions ────────────────────────────────

    async fn chat_openai(
        &self,
        system: &str,
        turns: &[Turn],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn> {
        let mut messages: Vec<Value> = vec![json!({"role": "system", "content": system})];
        for turn in turns {
            match turn {
                Turn::User(text) => messages.push(json!({"role": "user", "content": text})),
                Turn::Assistant(a) => {
                    let mut msg = json!({"role": "assistant", "content": a.text});
                    if !a.tool_calls.is_empty() {
                        msg["tool_calls"] = Value::Array(
                            a.tool_calls
                                .iter()
                                .map(|c| {
                                    json!({
                                        "id": c.id,
                                        "type": "function",
                                        "function": {
                                            "name": c.name,
                                            "arguments": c.args.to_string(),
                                        },
                                    })
                                })
                                .collect(),
                        );
                    }
                    messages.push(msg);
                }
                Turn::ToolResults(results) => {
                    for r in results {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": r.call_id,
                            "content": r.content,
                        }));
                    }
                }
            }
        }

        let tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
        });
        // Only send max_tokens when the user asked for it: some local
        // OpenAI-compatible servers reject the field or cap output poorly, so we
        // don't impose a default the way Claude requires.
        if self.max_tokens_configured {
            body["max_tokens"] = json!(self.max_tokens);
        }
        if !tool_defs.is_empty() {
            body["tools"] = Value::Array(tool_defs);
        }

        let mut req = self
            .client
            .post(format!("{}/v1/chat/completions", self.endpoint))
            .json(&body);
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let (status, raw) = self.send_retrying(req).await?;
        let payload: Value = serde_json::from_str(&raw).map_err(|e| {
            anyhow!(
                "the AI endpoint at {} returned a non-JSON response ({status}): {e}\n{}",
                self.endpoint,
                snippet(&raw)
            )
        })?;
        if !status.is_success() {
            let msg = payload["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("AI endpoint error ({status}): {msg}");
        }

        self.record_usage(Usage {
            input_tokens: payload["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: payload["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: payload["usage"]["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        });

        let choice = &payload["choices"][0];
        let message = &choice["message"];
        let mut out = AssistantTurn {
            text: message["content"].as_str().unwrap_or_default().to_string(),
            truncated: choice["finish_reason"].as_str() == Some("length"),
            ..Default::default()
        };
        for (i, call) in message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let args_raw = call["function"]["arguments"].as_str().unwrap_or("{}");
            // A malformed argument blob (often a truncated stream) must not be
            // silently swallowed into an empty object — that produces a wrong,
            // confident edit. Tag it so the tool layer returns a clear error the
            // model can recover from.
            let args = match serde_json::from_str(args_raw) {
                Ok(v) => v,
                Err(e) => json!({ ARG_PARSE_ERROR_KEY: format!("{e}: {}", snippet(args_raw)) }),
            };
            out.tool_calls.push(ToolCall {
                id: call["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{i}")),
                name: call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                args,
            });
        }
        Ok(out)
    }

    /// Store the token usage from the latest request for the front end.
    fn record_usage(&self, usage: Usage) {
        *self.last_usage.lock().unwrap() = Some(usage);
    }
}

/// Reserved key marking a tool call whose arguments failed to parse as JSON, so
/// the tool executor can turn it into an actionable error instead of running
/// with empty arguments. See [`ToolCall::arg_parse_error`].
pub const ARG_PARSE_ERROR_KEY: &str = "__arg_parse_error__";

impl ToolCall {
    /// If this call's arguments failed to parse (a malformed or truncated blob),
    /// the human-readable reason; otherwise `None`.
    pub fn arg_parse_error(&self) -> Option<&str> {
        self.args.get(ARG_PARSE_ERROR_KEY).and_then(Value::as_str)
    }
}

/// Whether an HTTP status warrants a retry: rate-limiting (429), Anthropic's
/// overloaded signal (529), and the transient 5xx family.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529)
}

/// Whether a transport error is worth retrying: timeouts and connection/send
/// failures are transient; a malformed-request or decode error is not.
fn is_retryable_err(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

/// Exponential backoff with jitter for the nth retry (1-based), capped at
/// [`RETRY_CAP`]. Jitter spreads retries so concurrent callers don't resynchronize
/// into the same server, and avoids a `rand` dependency by deriving a small
/// pseudo-random offset from the current time.
fn backoff(attempt: u32) -> Duration {
    let exp = RETRY_BASE.saturating_mul(1u32 << (attempt - 1).min(5));
    let capped = exp.min(RETRY_CAP);
    let jitter_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        % 250) as u64;
    capped + Duration::from_millis(jitter_ms)
}

/// Best-effort context-window size (in tokens) for a model, from its name. Used
/// only to size compaction, so a conservative estimate is fine — the cost of
/// guessing low is an extra summarization, of guessing high is an overflow.
fn context_window_tokens(model: &str, kind: ProviderKind) -> usize {
    let m = model.to_lowercase();
    // Claude 3+/4 models all carry a 200k window.
    if kind == ProviderKind::Claude || m.contains("claude") {
        return 200_000;
    }
    // Common OpenAI families.
    if m.contains("gpt-4o")
        || m.contains("gpt-4.1")
        || m.contains("gpt-4-turbo")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        return 128_000;
    }
    if m.contains("gpt-4-32k") {
        return 32_000;
    }
    if m.contains("gpt-4") {
        return 8_192;
    }
    if m.contains("gpt-3.5") {
        return 16_385;
    }
    // Unknown, typically a self-hosted/local model: assume a modest 32k window
    // so we compact conservatively rather than overflowing an 8–32k context.
    32_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_statuses_are_transient_only() {
        for code in [429u16, 500, 502, 503, 504, 529] {
            assert!(
                is_retryable_status(reqwest::StatusCode::from_u16(code).unwrap()),
                "{code}"
            );
        }
        for code in [200u16, 400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_status(reqwest::StatusCode::from_u16(code).unwrap()),
                "{code}"
            );
        }
    }

    #[test]
    fn backoff_grows_but_stays_capped() {
        // Always within cap + max jitter, at every attempt.
        let ceiling = RETRY_CAP + Duration::from_millis(250);
        for attempt in 1..=10 {
            assert!(
                backoff(attempt) <= ceiling,
                "attempt {attempt} exceeded cap"
            );
        }
        // An early retry is meaningfully shorter than the cap.
        assert!(backoff(1) < RETRY_CAP);
    }

    #[test]
    fn context_window_scales_with_model() {
        assert_eq!(
            context_window_tokens("claude-opus-4-8", ProviderKind::Claude),
            200_000
        );
        assert_eq!(
            context_window_tokens("gpt-4o", ProviderKind::OpenAi),
            128_000
        );
        assert_eq!(
            context_window_tokens("gpt-3.5-turbo", ProviderKind::OpenAi),
            16_385
        );
        // An unknown local model gets the conservative default, so we compact
        // early rather than overflow its small window.
        assert_eq!(
            context_window_tokens("qwen2.5-coder", ProviderKind::OpenAi),
            32_000
        );
    }

    #[test]
    fn malformed_tool_args_are_flagged_not_swallowed() {
        let bad = ToolCall {
            id: "1".into(),
            name: "edit_file".into(),
            args: json!({ ARG_PARSE_ERROR_KEY: "unexpected end of input" }),
        };
        assert!(bad.arg_parse_error().is_some());
        let good = ToolCall {
            id: "2".into(),
            name: "read_file".into(),
            args: json!({"path": "x"}),
        };
        assert!(good.arg_parse_error().is_none());
    }

    #[test]
    fn usage_label_is_compact() {
        let u = Usage {
            input_tokens: 1234,
            output_tokens: 340,
            cache_read_tokens: 1000,
        };
        assert_eq!(u.label(), "1.2k in · 340 out · 1.0k cached");
    }
}

/// A trimmed, single-line preview of a response body for error messages — enough
/// to recognise an HTML error page or a proxy notice without dumping kilobytes.
fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty response body)".to_string();
    }
    let flat: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 300 {
        let head: String = flat.chars().take(300).collect();
        format!("body: {head}…")
    } else {
        format!("body: {flat}")
    }
}
