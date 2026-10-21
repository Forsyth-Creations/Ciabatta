//! Remembering what a plan has already worked out, for as long as it takes to
//! make it.
//!
//! Planning a graph asks the same questions of the disk over and over: every
//! step walks the checkout for what git ignores, scans it for nested
//! sub-workspaces, resolves its `cache.cargo` closure (once for its inputs,
//! again for its outputs, again for its warnings), and hashes its inputs
//! twice — once to decide, once to report. In a monorepo of eighty crates
//! that was most of the wait before a run started.
//!
//! None of those answers can change while a plan is being made — nothing is
//! building yet — so [`planning`] remembers them until it returns, and no
//! longer. Outside it every call goes to the disk, as before: a build changes
//! the files, and a daemon that lives for days must never answer from a plan
//! it made yesterday.
//!
//! Per thread, so a plan on one thread never sees another's; work handed to
//! another thread simply isn't remembered.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::cargo::Resolved;

#[derive(Default)]
struct Memo {
    not_ignored: HashMap<PathBuf, HashSet<String>>,
    members: HashMap<PathBuf, Vec<String>>,
    resolved: HashMap<String, Resolved>,
    hashes: HashMap<PathBuf, String>,
}

thread_local! {
    static SCOPE: RefCell<Option<Memo>> = const { RefCell::new(None) };
}

/// Run `f` with the plan's answers remembered. Nested calls share the
/// outermost one's memory.
pub fn planning<T>(f: impl FnOnce() -> T) -> T {
    Remembered::default().run(f)
}

/// A plan's memory, kept between the stretches of work that share it — for a
/// step deciding across an `await`, where a thread-local scope can't follow
/// (the task may resume on another thread).
#[derive(Default)]
pub struct Remembered(Option<Memo>);

impl Remembered {
    /// Run `f` with this memory in force. Inside a [`planning`] already, the
    /// outer one's memory is used instead.
    pub fn run<T>(&mut self, f: impl FnOnce() -> T) -> T {
        /// Puts the memory back on the way out, a panic included.
        struct Restore<'a>(&'a mut Option<Memo>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                *self.0 = SCOPE.with(|s| s.borrow_mut().take());
            }
        }
        if SCOPE.with(|s| s.borrow().is_some()) {
            return f();
        }
        let memo = self.0.take().unwrap_or_default();
        SCOPE.with(|s| *s.borrow_mut() = Some(memo));
        let _restore = Restore(&mut self.0);
        f()
    }
}

/// `compute`'s answer, from memory inside [`planning`]. Errors aren't
/// remembered.
fn remembered<K, V, E>(
    table: fn(&mut Memo) -> &mut HashMap<K, V>,
    key: K,
    compute: impl FnOnce() -> Result<V, E>,
) -> Result<V, E>
where
    K: std::hash::Hash + Eq,
    V: Clone,
{
    let hit = SCOPE.with(|s| {
        s.borrow_mut()
            .as_mut()
            .map(|memo| table(memo).get(&key).cloned())
    });
    match hit {
        // Not planning: nothing to remember into.
        None => compute(),
        Some(Some(value)) => Ok(value),
        Some(None) => {
            let value = compute()?;
            SCOPE.with(|s| {
                if let Some(memo) = s.borrow_mut().as_mut() {
                    table(memo).insert(key, value.clone());
                }
            });
            Ok(value)
        }
    }
}

pub(super) fn not_ignored(
    dir: PathBuf,
    compute: impl FnOnce() -> anyhow::Result<HashSet<String>>,
) -> anyhow::Result<HashSet<String>> {
    remembered(|m| &mut m.not_ignored, dir, compute)
}

pub(super) fn members(dir: PathBuf, compute: impl FnOnce() -> Vec<String>) -> Vec<String> {
    remembered(|m| &mut m.members, dir, || Ok::<_, ()>(compute())).unwrap_or_default()
}

pub(super) fn resolved(
    key: String,
    compute: impl FnOnce() -> anyhow::Result<Resolved>,
) -> anyhow::Result<Resolved> {
    remembered(|m| &mut m.resolved, key, compute)
}

pub(super) fn hash(
    path: PathBuf,
    compute: impl FnOnce() -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    remembered(|m| &mut m.hashes, path, compute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_remembered_only_while_planning() {
        let calls = std::cell::Cell::new(0);
        let ask = || {
            members(PathBuf::from("/x"), || {
                calls.set(calls.get() + 1);
                vec![calls.get().to_string()]
            })
        };
        assert_eq!(ask(), ["1"]);
        assert_eq!(ask(), ["2"], "outside a plan, every call computes");
        planning(|| {
            assert_eq!(ask(), ["3"]);
            assert_eq!(ask(), ["3"], "inside, the first answer stands");
            planning(|| assert_eq!(ask(), ["3"], "a nested plan shares it"));
        });
        assert_eq!(ask(), ["4"], "and it is forgotten afterwards");

        // Carried between stretches of work, it remembers across them.
        let mut kept = Remembered::default();
        assert_eq!(kept.run(ask), ["5"]);
        assert_eq!(ask(), ["6"], "not in force between them");
        assert_eq!(kept.run(ask), ["5"]);
    }
}
