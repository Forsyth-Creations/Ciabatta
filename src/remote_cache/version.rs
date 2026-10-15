//! Telling a developer their ciabatta and their team's remote cache disagree.
//!
//! The cache and the binaries talking to it are released together, and a
//! laptop that has fallen behind is the usual reason a cache "stops working":
//! a key computed one way on one version and another way on the next is a
//! miss nobody can explain. So every run asks the cache which version it is —
//! one quick, unauthenticated request — and when the answer differs, the run
//! opens with a warning nobody can scroll past.
//!
//! Best-effort throughout. A cache that is unconfigured, unreachable, or slow
//! to answer is not this check's business: the cache session reports those on
//! its own, and a version check that delayed or failed a build would be worse
//! than none.

use std::time::Duration;

/// How long to wait for the cache to say what version it is. Short: this runs
/// before every workflow, and a cache that can't answer in this long is one the
/// cache session will complain about anyway.
const PROBE: Duration = Duration::from_millis(1500);

/// The warning to open a run with, if `config`'s remote cache is reachable and
/// runs a different version from this binary.
pub async fn mismatch(config: &crate::config::CiabattaConfig) -> Option<String> {
    let remote = config.cache.as_ref()?.remote()?;
    let client = super::client::Client::new(&remote.url, remote.tls_verify).ok()?;
    let health = tokio::time::timeout(PROBE, client.health())
        .await
        .ok()?
        .ok()?;
    describe(env!("CARGO_PKG_VERSION"), &health.version, &remote.url)
}

/// The warning for this binary at `ours` against a cache at `theirs`, or `None`
/// when they agree — or when the cache didn't say.
pub fn describe(ours: &str, theirs: &str, url: &str) -> Option<String> {
    let theirs = theirs.trim().trim_start_matches('v');
    if theirs.is_empty() || theirs == ours {
        return None;
    }
    let advice = match newer(theirs, ours) {
        Some(true) => "Update with `ciabatta self update`, so this machine computes cache keys \
                       and talks to the cache the way it expects."
            .to_string(),
        Some(false) => "The remote cache is older than this ciabatta. Ask whoever runs it to \
                        upgrade, or install the matching ciabatta."
            .to_string(),
        None => {
            "Match them — `ciabatta self update` installs the build the cache serves.".to_string()
        }
    };
    Some(format!(
        "This is ciabatta {ours}, but the remote cache at {url} runs {theirs}. {advice} \
         Until they match, cache hits and shared history may not line up."
    ))
}

/// Whether version `a` is newer than `b`, comparing dotted numbers; `None` when
/// either isn't one.
fn newer(a: &str, b: &str) -> Option<bool> {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse().ok())
            .collect()
    };
    Some(parse(a)? > parse(b)?)
}

/// `message` drawn as a box that stands out in a terminal: the run's first
/// lines, before any step's output, in a colour nothing else uses.
pub fn banner(message: &str) -> String {
    const WIDTH: usize = 72;
    let mut lines = vec![String::new()];
    for word in message.split_whitespace() {
        let line = lines.last_mut().expect("starts with one");
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > WIDTH {
            lines.push(word.to_string());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    let rule = "━".repeat(WIDTH + 4);
    let mut out = format!("┏{rule}┓\n");
    let title = "!!  VERSION MISMATCH WITH THE REMOTE CACHE";
    out.push_str(&format!("┃  {title:<WIDTH$}  ┃\n", WIDTH = WIDTH));
    out.push_str(&format!("┃  {:<WIDTH$}  ┃\n", "", WIDTH = WIDTH));
    for line in lines {
        out.push_str(&format!("┃  {line:<WIDTH$}  ┃\n", WIDTH = WIDTH));
    }
    out.push_str(&format!("┗{rule}┛"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_versions_say_nothing() {
        assert_eq!(describe("0.6.6", "0.6.6", "http://cache"), None);
        assert_eq!(describe("0.6.6", "v0.6.6", "http://cache"), None);
        assert_eq!(
            describe("0.6.6", "", "http://cache"),
            None,
            "a cache that didn't say"
        );
    }

    #[test]
    fn a_newer_cache_says_to_update_and_an_older_one_says_otherwise() {
        let behind = describe("0.6.5", "0.7.0", "http://cache").unwrap();
        assert!(behind.contains("ciabatta self update"), "{behind}");
        assert!(
            behind.contains("0.6.5") && behind.contains("0.7.0"),
            "{behind}"
        );

        let ahead = describe("0.10.0", "0.9.9", "http://cache").unwrap();
        assert!(ahead.contains("older than this ciabatta"), "{ahead}");
    }

    #[test]
    fn the_banner_keeps_every_word_and_a_straight_edge() {
        let message = describe("0.6.5", "0.7.0", "https://cache.example.com").unwrap();
        let drawn = banner(&message);
        let widths: Vec<usize> = drawn.lines().map(|l| l.chars().count()).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{drawn}");
        for word in message.split_whitespace() {
            assert!(drawn.contains(word), "lost {word:?}:\n{drawn}");
        }
    }
}
