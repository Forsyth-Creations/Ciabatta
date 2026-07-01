//! The ciabatta *environment* — how build variables (branch / commit / tag /
//! build number) are resolved.
//!
//! Controlled by the `CIABATTA_ENV` environment variable:
//!   * `CIABATTA_ENV=local` → resolve everything from the local git repository,
//!     so `ciabatta push` / `pull` work on a developer machine without having to
//!     pass `--local` or `-e CIABATTA_BRANCH=…` on every invocation.
//!   * anything else (or unset) → resolve from the configured CI system.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

/// The environment variable that selects the resolution mode.
pub const ENV_VAR: &str = "CIABATTA_ENV";
/// The value of [`ENV_VAR`] that selects local (git) resolution.
pub const LOCAL: &str = "local";

/// How ciabatta resolves its build variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveMode {
    /// Resolve from the local git repository.
    Local,
    /// Resolve from the configured CI system (the default).
    Ci,
}

/// The active ciabatta environment, derived from `CIABATTA_ENV`.
#[derive(Debug, Clone, Copy)]
pub struct CiabattaEnv {
    mode: ResolveMode,
}

impl CiabattaEnv {
    /// Detect the environment from the process's `CIABATTA_ENV` variable.
    pub fn detect() -> Self {
        Self::from_value(std::env::var(ENV_VAR).ok().as_deref())
    }

    /// Detect the environment, treating an explicit `--local` flag as forcing
    /// local mode regardless of `CIABATTA_ENV`.
    pub fn detect_with_flag(force_local: bool) -> Self {
        if force_local {
            CiabattaEnv {
                mode: ResolveMode::Local,
            }
        } else {
            Self::detect()
        }
    }

    /// Build from an explicit `CIABATTA_ENV` value. Used for tests and to read
    /// the mode back out of an already-resolved variable map.
    pub fn from_value(value: Option<&str>) -> Self {
        let mode = match value {
            Some(v) if v.trim().eq_ignore_ascii_case(LOCAL) => ResolveMode::Local,
            _ => ResolveMode::Ci,
        };
        CiabattaEnv { mode }
    }

    pub fn is_local(&self) -> bool {
        self.mode == ResolveMode::Local
    }

    /// Resolve the `CIABATTA_*` build variables for this environment. In local
    /// mode they come from git; in CI mode this returns an empty map (the caller
    /// resolves those from the CI system).
    pub fn resolve_vars(&self, root: &Path) -> Result<HashMap<String, String>> {
        if self.is_local() {
            crate::git::local_git_vars(root)
        } else {
            Ok(HashMap::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_value_selects_local_mode() {
        assert!(CiabattaEnv::from_value(Some("local")).is_local());
        assert!(CiabattaEnv::from_value(Some("LOCAL")).is_local());
        assert!(CiabattaEnv::from_value(Some(" local ")).is_local());
    }

    #[test]
    fn other_values_select_ci_mode() {
        assert!(!CiabattaEnv::from_value(None).is_local());
        assert!(!CiabattaEnv::from_value(Some("")).is_local());
        assert!(!CiabattaEnv::from_value(Some("ci")).is_local());
        assert!(!CiabattaEnv::from_value(Some("production")).is_local());
    }

    #[test]
    fn flag_forces_local() {
        assert!(CiabattaEnv::detect_with_flag(true).is_local());
    }
}
