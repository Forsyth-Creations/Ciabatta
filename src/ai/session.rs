//! Persistent chat sessions.
//!
//! Every conversation with the assistant is saved as one JSON file under
//! `.ciabatta/ai/conversations/`, so a session survives quitting the TUI and
//! can be resumed later (`ciabatta ai --resume`, or `resume` to pick one).
//!
//! A file holds the whole provider-neutral transcript ([`Turn`]s), plus a
//! title (taken from the first thing you asked) and timestamps. The id is the
//! creation time, which also sorts conversations newest-last on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::provider::Turn;

/// Directory holding every saved conversation for a project.
pub fn conversations_dir(root: &Path) -> PathBuf {
    root.join(crate::config::CIABATTA_DIR)
        .join("ai")
        .join("conversations")
}

/// A saved conversation: its metadata and full transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Absolute path this conversation persists to (not serialized).
    #[serde(skip)]
    path: PathBuf,
}

/// A one-line summary of a saved conversation, for listings/pickers.
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub turns: usize,
}

impl Conversation {
    /// Start a fresh conversation (not yet written until [`Self::save`]).
    pub fn new(root: &Path) -> Self {
        let now = now();
        let id = now.replace([':', '.'], "-");
        let path = conversations_dir(root).join(format!("{id}.json"));
        Self {
            id,
            title: String::new(),
            created_at: now.clone(),
            updated_at: now,
            turns: Vec::new(),
            path,
        }
    }

    /// Load a conversation by id from a project's store.
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        let path = conversations_dir(root).join(format!("{id}.json"));
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("No saved conversation '{id}' ({})", path.display()))?;
        let mut conv: Conversation = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        conv.path = path;
        Ok(conv)
    }

    /// Load the most recently updated conversation, if any exist.
    pub fn latest(root: &Path) -> Result<Option<Self>> {
        let Some(newest) = list(root)?.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(Self::load(root, &newest.id)?))
    }

    /// Persist the conversation to disk, refreshing the title and timestamp.
    /// The title is the first user message (trimmed to one line).
    pub fn save(&mut self) -> Result<()> {
        if self.title.is_empty()
            && let Some(Turn::User(first)) = self.turns.iter().find(|t| matches!(t, Turn::User(_)))
        {
            self.title = first
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
        }
        self.updated_at = now();
        let dir = self
            .path
            .parent()
            .context("conversation path has no parent directory")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

/// Delete one saved conversation by id. Returns whether a file was removed.
pub fn delete(root: &Path, id: &str) -> Result<bool> {
    let path = conversations_dir(root).join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("Failed to delete {}", path.display())),
    }
}

/// Delete every saved conversation for a project. Returns how many were removed.
pub fn clear(root: &Path) -> Result<usize> {
    let dir = conversations_dir(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", dir.display())),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Every saved conversation for a project, newest first.
pub fn list(root: &Path) -> Result<Vec<ConversationSummary>> {
    let dir = conversations_dir(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", dir.display())),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(conv) = serde_json::from_str::<Conversation>(&raw) else {
            continue;
        };
        // Count only the user/assistant exchanges, not tool-result plumbing.
        let turns = conv
            .turns
            .iter()
            .filter(|t| !matches!(t, Turn::ToolResults(_)))
            .count();
        out.push(ConversationSummary {
            id: conv.id,
            title: if conv.title.is_empty() {
                "(untitled)".into()
            } else {
                conv.title
            },
            updated_at: conv.updated_at,
            turns,
        });
    }
    // updated_at is an RFC3339 string; lexical sort is chronological.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::{AssistantTurn, Turn};

    fn temp_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ciabatta-session-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn save_load_and_list_round_trip() {
        let root = temp_root();

        let mut conv = Conversation::new(&root);
        conv.turns.push(Turn::User("how does auth work?".into()));
        conv.turns.push(Turn::Assistant(AssistantTurn {
            text: "It lives in src/auth.rs.".into(),
            ..Default::default()
        }));
        conv.save().unwrap();
        let id = conv.id.clone();

        // Title is derived from the first user message.
        assert_eq!(conv.title, "how does auth work?");

        // Reload by id and confirm the transcript survived.
        let loaded = Conversation::load(&root, &id).unwrap();
        assert_eq!(loaded.turns.len(), 2);
        assert!(matches!(&loaded.turns[0], Turn::User(t) if t == "how does auth work?"));

        // It shows up in the listing.
        let summaries = list(&root).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, id);
        assert_eq!(summaries[0].turns, 2);

        // latest() returns it too.
        assert_eq!(Conversation::latest(&root).unwrap().unwrap().id, id);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_removes_one_and_clear_removes_all() {
        let root = temp_root();

        // Two saved conversations.
        let mut a = Conversation::new(&root);
        a.turns.push(Turn::User("first".into()));
        a.save().unwrap();
        let id_a = a.id.clone();
        // Ensure a distinct id (ids are timestamp-derived).
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut b = Conversation::new(&root);
        b.turns.push(Turn::User("second".into()));
        b.save().unwrap();
        assert_eq!(list(&root).unwrap().len(), 2);

        // Delete one by id.
        assert!(delete(&root, &id_a).unwrap());
        assert_eq!(list(&root).unwrap().len(), 1);
        // Deleting a missing id reports false, not an error.
        assert!(!delete(&root, "does-not-exist").unwrap());

        // Clear removes the rest and reports the count.
        assert_eq!(clear(&root).unwrap(), 1);
        assert!(list(&root).unwrap().is_empty());
        // Clearing an empty store is a no-op returning 0.
        assert_eq!(clear(&root).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_conversation_is_an_error_and_empty_store_lists_nothing() {
        let root = temp_root();
        assert!(Conversation::load(&root, "nope").is_err());
        assert!(list(&root).unwrap().is_empty());
        assert!(Conversation::latest(&root).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
