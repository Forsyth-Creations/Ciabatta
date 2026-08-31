//! Build features — `CIABATTA_FEAT_<NAME>` in the environment.
//!
//! A feature is a build-shaping switch: telemetry compiled in or not, the new
//! UI or the old one, the fast test suite or the slow one. Ciabatta already had
//! a way to *read* such a variable — a step's `when:` condition — but no notion
//! that the variable was one of a family, which left two problems.
//!
//! The first is that nothing knew a feature had been set. A run gave no sign of
//! which switches were on, and a step that wanted the whole set had to know
//! every name in advance to go looking for them.
//!
//! The second is the one that actually breaks builds. A cached artifact is only
//! reusable by a build that would have produced the same thing, and a feature
//! changes what a build produces — so a feature has to be part of the cache
//! key. Before this it was part of the key only if somebody remembered to list
//! the variable under `cache.env`, and forgetting meant a build silently handed
//! the other configuration's artifacts. Anything named with this prefix is now
//! in the key by construction, so the mistake is no longer available.
//!
//! Features are read from the run's fully resolved environment, which is what
//! makes `env_file:` work for them for free: by the time this sees the map, the
//! `.env` chain has been layered into it, so `CIABATTA_FEAT_NEW_UI=1` in a file
//! and the same on the command line are the same feature by the same route.

use std::collections::{BTreeSet, HashMap};

/// The prefix that marks an environment variable as a feature switch.
pub const PREFIX: &str = "CIABATTA_FEAT_";

/// The variable ciabatta sets for the steps it runs: every enabled feature,
/// sorted, comma-separated. A script that wants to branch on one feature reads
/// its `CIABATTA_FEAT_*` variable directly; this is for the scripts that want
/// to pass the whole set on to something else.
pub const ACTIVE_VAR: &str = "CIABATTA_FEATURES";

/// The features a run was started with.
///
/// Both halves are kept because they answer different questions. `on` is what
/// the build is: it goes in the cache key and into [`ACTIVE_VAR`]. `off` is
/// what someone explicitly turned off, which is worth reporting back — a
/// `CIABATTA_FEAT_TELEMETRY=0` that had no effect because the name was
/// misspelled looks exactly like one that worked, unless the run says what it
/// saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features {
    on: BTreeSet<String>,
    off: BTreeSet<String>,
}

impl Features {
    /// Read every `CIABATTA_FEAT_*` variable out of a resolved environment.
    pub fn from_env(env: &HashMap<String, String>) -> Self {
        Self::from_pairs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
    }

    /// The same, over any iterator of pairs — the run's environment is a
    /// `HashMap` and the cache's is a `BTreeMap`, and both have to give the
    /// same answer or a build would be keyed differently from how it ran.
    pub fn from_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let mut features = Features::default();
        for (key, value) in pairs {
            let Some(name) = name_of(key) else {
                continue;
            };
            if super::truthy(value) {
                features.on.insert(name);
            } else {
                features.off.insert(name);
            }
        }
        // A name can't be both: an environment has one value per variable, and
        // `on` is the one that changes what gets built.
        for name in &features.on {
            features.off.remove(name);
        }
        features
    }

    /// Whether a feature is enabled. The name is matched as it appears in
    /// [`ACTIVE_VAR`] — lowercase, underscores — not as the variable is spelled.
    pub fn is_on(&self, name: &str) -> bool {
        self.on.contains(&normalize(name))
    }

    /// Whether anything at all was declared, on or off.
    pub fn is_empty(&self) -> bool {
        self.on.is_empty() && self.off.is_empty()
    }

    /// The value of [`ACTIVE_VAR`]: enabled features, sorted, comma-separated.
    pub fn list(&self) -> String {
        self.on.iter().cloned().collect::<Vec<_>>().join(",")
    }

    /// What the cache key is derived from — the enabled set, and only that.
    ///
    /// A feature explicitly turned off is deliberately absent rather than
    /// recorded as false: a build with `CIABATTA_FEAT_X=0` produces the same
    /// artifacts as one that never mentioned `X`, and giving them different
    /// keys would cost a rebuild to prove they were the same.
    pub fn key_material(&self) -> BTreeSet<String> {
        self.on.clone()
    }

    /// The line a run prints about itself, or `None` when no feature was set
    /// and there is nothing to say.
    pub fn describe(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut line = match self.on.is_empty() {
            true => "features: none enabled".to_string(),
            false => format!("features: {}", self.list()),
        };
        if !self.off.is_empty() {
            line.push_str(&format!(
                " (off: {})",
                self.off.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        Some(line)
    }

    /// Put [`ACTIVE_VAR`] into an environment map.
    ///
    /// The `CIABATTA_FEAT_*` variables themselves are already there — they are
    /// where this came from — so only the summary is added. It is set even when
    /// empty, so a script can tell "no features" from "an old ciabatta that
    /// didn't set this".
    pub fn export_into(&self, env: &mut HashMap<String, String>) {
        env.insert(ACTIVE_VAR.to_string(), self.list());
    }
}

/// The feature name a variable declares, or `None` if it isn't a feature
/// variable. `CIABATTA_FEAT_NEW_UI` → `new_ui`.
pub fn name_of(var: &str) -> Option<String> {
    let rest = var.strip_prefix(PREFIX)?;
    // `CIABATTA_FEAT_` on its own names nothing.
    if rest.is_empty() {
        return None;
    }
    Some(normalize(rest))
}

/// Feature names are compared case-insensitively, and `-` reads as `_`, so
/// `CIABATTA_FEAT_NEW_UI` and a `--filter` or condition written `new-ui` are
/// the same feature.
fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn only_prefixed_variables_are_features() {
        let features = Features::from_env(&env(&[
            ("CIABATTA_FEAT_NEW_UI", "1"),
            ("CIABATTA_BRANCH", "main"),
            ("FEAT_NEW_UI", "1"),
            ("CIABATTA_FEAT_", "1"),
        ]));
        assert_eq!(features.list(), "new_ui");
    }

    #[test]
    fn a_falsey_value_declares_the_feature_off_rather_than_absent() {
        let features = Features::from_env(&env(&[
            ("CIABATTA_FEAT_TELEMETRY", "0"),
            ("CIABATTA_FEAT_NEW_UI", "true"),
        ]));
        assert!(features.is_on("new_ui"));
        assert!(!features.is_on("telemetry"));
        // Reported, so a misspelled name that did nothing is still visible.
        assert_eq!(
            features.describe().unwrap(),
            "features: new_ui (off: telemetry)"
        );
    }

    #[test]
    fn every_falsey_spelling_reads_the_same_way() {
        for value in ["", " ", "0", "false", "FALSE", "no", "off"] {
            let features = Features::from_env(&env(&[("CIABATTA_FEAT_X", value)]));
            assert!(
                !features.is_on("x"),
                "{value:?} should not enable a feature"
            );
        }
        for value in ["1", "true", "yes", "on", "anything"] {
            let features = Features::from_env(&env(&[("CIABATTA_FEAT_X", value)]));
            assert!(features.is_on("x"), "{value:?} should enable a feature");
        }
    }

    #[test]
    fn names_are_normalized_so_one_feature_has_one_spelling() {
        let features = Features::from_env(&env(&[("CIABATTA_FEAT_NEW_UI", "1")]));
        assert!(features.is_on("new_ui"));
        assert!(features.is_on("NEW_UI"));
        assert!(features.is_on("new-ui"));
    }

    /// The enabled set is what a build *is*; a feature turned off is the same
    /// build as one never mentioned, and must not cost a cache miss.
    #[test]
    fn the_key_is_the_enabled_set_alone() {
        let with_off = Features::from_env(&env(&[
            ("CIABATTA_FEAT_NEW_UI", "1"),
            ("CIABATTA_FEAT_TELEMETRY", "0"),
        ]));
        let without = Features::from_env(&env(&[("CIABATTA_FEAT_NEW_UI", "1")]));
        assert_eq!(with_off.key_material(), without.key_material());

        let different = Features::from_env(&env(&[("CIABATTA_FEAT_TELEMETRY", "1")]));
        assert_ne!(with_off.key_material(), different.key_material());
    }

    #[test]
    fn the_active_variable_is_set_even_when_nothing_is_enabled() {
        let mut map = HashMap::new();
        Features::default().export_into(&mut map);
        assert_eq!(map.get(ACTIVE_VAR).unwrap(), "");

        let mut map = HashMap::new();
        Features::from_env(&env(&[("CIABATTA_FEAT_B", "1"), ("CIABATTA_FEAT_A", "1")]))
            .export_into(&mut map);
        // Sorted, so the same set always renders the same way.
        assert_eq!(map.get(ACTIVE_VAR).unwrap(), "a,b");
    }

    #[test]
    fn nothing_declared_means_nothing_to_report() {
        assert!(
            Features::from_env(&env(&[("PATH", "/usr/bin")]))
                .describe()
                .is_none()
        );
    }
}
