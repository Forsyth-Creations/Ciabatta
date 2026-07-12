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

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long to wait on a single request before giving up. Hosted APIs answer in
/// seconds; a local model loading weights on the first call can take much longer,
/// hence the generous ceiling.
const REQUEST_TIMEOUT_SECS: u64 = 600;

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
}

/// The configured LLM endpoint.
pub struct Provider {
    pub kind: ProviderKind,
    endpoint: String,
    pub model: String,
    api_key: String,
    client: reqwest::Client,
}

impl Provider {
    /// Build a provider from the `[ai]` config section, resolving the API key
    /// from the configured environment variable.
    pub fn from_config(cfg: &crate::config::AiConfig) -> Result<Self> {
        let raw_provider = cfg.provider.as_deref().unwrap_or("claude").trim().to_lowercase();
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
        })
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
            "max_tokens": 8192,
            "system": system,
            "messages": messages,
        });
        if !tool_defs.is_empty() {
            body["tools"] = Value::Array(tool_defs);
        }

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.endpoint))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| self.send_error(e))?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .map_err(|e| self.send_error(e))?;
        let payload: Value = serde_json::from_str(&raw).map_err(|e| {
            anyhow!(
                "Claude API returned a non-JSON response ({status}): {e}\n{}",
                snippet(&raw)
            )
        })?;
        if !status.is_success() {
            let msg = payload["error"]["message"].as_str().unwrap_or("unknown error");
            bail!("Claude API error ({status}): {msg}");
        }

        // A refusal is a successful HTTP response — check stop_reason before content.
        if payload["stop_reason"].as_str() == Some("refusal") {
            return Ok(AssistantTurn {
                text: "The model declined this request.".to_string(),
                refused: true,
                ..Default::default()
            });
        }

        let mut out = AssistantTurn::default();
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
        let resp = req.send().await.map_err(|e| self.send_error(e))?;

        let status = resp.status();
        let raw = resp.text().await.map_err(|e| self.send_error(e))?;
        let payload: Value = serde_json::from_str(&raw).map_err(|e| {
            anyhow!(
                "the AI endpoint at {} returned a non-JSON response ({status}): {e}\n{}",
                self.endpoint,
                snippet(&raw)
            )
        })?;
        if !status.is_success() {
            let msg = payload["error"]["message"].as_str().unwrap_or("unknown error");
            bail!("AI endpoint error ({status}): {msg}");
        }

        let message = &payload["choices"][0]["message"];
        let mut out = AssistantTurn {
            text: message["content"].as_str().unwrap_or_default().to_string(),
            ..Default::default()
        };
        for (i, call) in message["tool_calls"].as_array().into_iter().flatten().enumerate() {
            let args_raw = call["function"]["arguments"].as_str().unwrap_or("{}");
            out.tool_calls.push(ToolCall {
                id: call["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{i}")),
                name: call["function"]["name"].as_str().unwrap_or_default().to_string(),
                args: serde_json::from_str(args_raw).unwrap_or(json!({})),
            });
        }
        Ok(out)
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
