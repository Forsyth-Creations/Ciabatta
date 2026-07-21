//! Context compaction for long conversations.
//!
//! The agent loop re-sends the whole transcript every round, so a session that
//! runs long enough eventually exceeds the model's context window and every
//! request fails. To keep long sessions alive, once the transcript grows past a
//! budget we summarize the older turns into a short synthetic exchange and keep
//! only the most recent turns verbatim.
//!
//! The cut is made at a real user-message boundary, so we never orphan a
//! `tool_use` from its `tool_result` (which both wire formats reject). The
//! summarized prefix is replaced with a `User(summary)` + `Assistant(ack)` pair,
//! keeping strict role alternation for the tail that follows.

use super::provider::{AssistantTurn, Provider, Turn};

/// Don't bother summarizing unless the prefix we'd fold away is at least this
/// big; below it, compaction costs a model round for no real savings. (Even on
/// a small-window model, folding away a few KB isn't worth a round trip.)
const MIN_PREFIX_CHARS: usize = 20_000;

/// The system prompt for the summarization pass (adapted from opencode's
/// anchored-context summarizer).
const COMPACTION_SYSTEM: &str = "You are a context-summarization assistant for a coding session. \
    You will be given the earlier part of a conversation between a developer and an AI coding \
    assistant. Produce a terse, information-dense summary that lets the assistant continue the \
    work without the original text. Preserve exact file paths, identifiers, decisions made, and \
    any open threads or TODOs. Prefer bullets over prose. Do not answer the conversation itself \
    or mention that you are summarizing.";

/// Estimate a turn's contribution to the context in characters.
fn turn_len(turn: &Turn) -> usize {
    match turn {
        Turn::User(t) => t.len(),
        Turn::Assistant(a) => {
            a.text.len() + a.tool_calls.iter().map(|c| c.name.len() + c.args.to_string().len()).sum::<usize>()
        }
        Turn::ToolResults(rs) => rs.iter().map(|r| r.content.len()).sum(),
    }
}

/// Total estimated size of a transcript.
fn total_len(turns: &[Turn]) -> usize {
    turns.iter().map(turn_len).sum()
}

/// Render a slice of turns into compact plain text for the summarizer to read.
fn render(turns: &[Turn]) -> String {
    let mut out = String::new();
    for turn in turns {
        match turn {
            Turn::User(t) => {
                out.push_str("USER: ");
                out.push_str(t);
                out.push('\n');
            }
            Turn::Assistant(a) => {
                if !a.text.is_empty() {
                    out.push_str("ASSISTANT: ");
                    out.push_str(&a.text);
                    out.push('\n');
                }
                for c in &a.tool_calls {
                    out.push_str(&format!("ASSISTANT called {} {}\n", c.name, clip(&c.args.to_string(), 500)));
                }
            }
            Turn::ToolResults(rs) => {
                for r in rs {
                    out.push_str(&format!("TOOL RESULT: {}\n", clip(&r.content, 1_500)));
                }
            }
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

/// If `turns` has grown past the budget, summarize its older prefix in place.
/// Returns a one-line note when compaction happened (for a status event), or
/// `None` when nothing was done. Never fails the caller: on a summarization
/// error it still shrinks the transcript with a placeholder, since a valid but
/// lossy history beats a request that overflows the context window.
pub async fn maybe_compact(provider: &Provider, turns: &mut Vec<Turn>) -> Option<String> {
    if total_len(turns) < provider.context_budget_chars() {
        return None;
    }

    // Cut at the last real user message so the retained tail is self-contained
    // and wire-valid (no tool_result without its tool_use).
    let keep_from = turns
        .iter()
        .enumerate()
        .rev()
        .find(|(_, t)| matches!(t, Turn::User(_)))
        .map(|(i, _)| i)?;
    if keep_from == 0 {
        return None; // only one user message; nothing safe to fold away
    }

    let prefix = &turns[..keep_from];
    if total_len(prefix) < MIN_PREFIX_CHARS {
        return None;
    }

    // Anchor the original task: keep the very first user message verbatim so the
    // model never loses sight of the goal, even after its statement scrolls out
    // of the summarized window.
    let goal = turns
        .iter()
        .find_map(|t| match t {
            Turn::User(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    // Ask the model to summarize the prefix. Present it as a single user turn so
    // the summarized slice's own tool calls can't create wire-validity issues.
    let rendered = render(prefix);
    let summary = match provider
        .chat(
            COMPACTION_SYSTEM,
            &[Turn::User(format!("Summarize this earlier conversation:\n\n{rendered}"))],
            &[],
        )
        .await
    {
        Ok(t) if !t.text.trim().is_empty() => t.text,
        _ => "[Earlier conversation omitted to stay within the context window.]".to_string(),
    };

    let tail: Vec<Turn> = turns.split_off(keep_from);
    let mut compacted = Vec::with_capacity(tail.len() + 2);
    let goal_line = if goal.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nOriginal task (verbatim, do not lose sight of this):\n{goal}")
    };
    compacted.push(Turn::User(format!(
        "[Summary of earlier conversation in this session]\n{summary}{goal_line}"
    )));
    compacted.push(Turn::Assistant(AssistantTurn {
        text: "Acknowledged — continuing from the summary above.".to_string(),
        ..Default::default()
    }));
    compacted.extend(tail);
    *turns = compacted;

    Some("🗜  compacted earlier context to stay within the model's window".to_string())
}
