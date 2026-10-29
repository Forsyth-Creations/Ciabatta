//! Chess between two ciabattas on the same network.
//!
//! Reached only by typing `/chess` into the daemon's web app — there's no nav
//! entry for it. [`rules`] knows chess, [`game`] runs one game under the
//! rules its host chose, and [`lan`] finds other players and carries the game
//! between the two daemons.

pub mod game;
pub mod lan;
pub mod rules;

use std::path::PathBuf;

use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub use lan::Lan;

/// What `~/.ciabatta/chess.json` holds: just the name you play as.
#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    name: Option<String>,
}

fn settings_path() -> Result<PathBuf> {
    Ok(crate::daemon::state_dir()?.join("chess.json"))
}

/// The display name saved from the chess page, if one was.
fn load_name() -> Option<String> {
    let text = std::fs::read_to_string(settings_path().ok()?).ok()?;
    let settings: Settings = serde_json::from_str(&text).ok()?;
    settings
        .name
        .map(|n| lan::clean_name(&n))
        .filter(|n| !n.is_empty())
}

fn save_name(name: &str) -> Result<()> {
    let path = settings_path()?;
    let settings = Settings {
        name: Some(name.to_string()),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&settings)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// A game id: random, and short enough to read out.
fn new_id() -> String {
    let mut bytes = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
