//! `ciabatta todo` — a task list, per project or global.
//!
//! Three ways to use it:
//!   ciabatta todo "take out the trash"            add it to the current project
//!   ciabatta todo --global "renew the domain"     add it to the global list
//!   ciabatta todo                                  open the todo page
//!
//! Tasks live in one JSON file under the user's home directory
//! (`~/.ciabatta/todos.json`) and each carries the project it belongs to — or
//! no project at all, which is the **global** list.
//!
//! The distinction is the useful one. Most of what you write down is about the
//! repo you're in and belongs beside it; some of it isn't about any repo, and
//! filing that under whichever project happened to be open is how it gets lost.
//! So global tasks have a home of their own on the dashboard, and a task can be
//! moved between the two with [`Store::set_project`] — the web app calls it
//! "make global" in one direction and "move here" in the other.
//!
//! One file rather than one per project because a todo list is small, and
//! because it keeps "show me everything I owe across all my repos" a filter
//! rather than a directory walk. Tasks written before todos were scoped have no
//! project, so they land on the global list — which is where something nobody
//! attached to a repo belongs anyway.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How important a task is. Higher-priority tasks sort to the top of the list.
/// Serialized as a lowercase string (`"high"`/`"medium"`/`"low"`) so the JSON
/// file stays readable for hand-editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    #[default]
    Medium,
    Low,
}

impl Priority {
    /// Sort rank: higher is more important, so it sorts first.
    fn rank(self) -> u8 {
        match self {
            Priority::High => 2,
            Priority::Medium => 1,
            Priority::Low => 0,
        }
    }
}

/// A single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// How important the task is; drives the list's sort order.
    #[serde(default)]
    pub priority: Priority,
    /// RFC 3339 timestamp of when the task was added.
    pub created_at: String,
    /// The project this task belongs to, or `None` for the global list.
    #[serde(default)]
    pub project: Option<String>,
}

impl Todo {
    /// Whether this task belongs to no project — the global list.
    pub fn is_global(&self) -> bool {
        self.project.is_none()
    }

    /// Whether this task falls within `scope`.
    pub fn is_in(&self, scope: &Scope) -> bool {
        match scope {
            Scope::Global => self.is_global(),
            Scope::Project(wanted) => self.project.as_deref() == Some(wanted.as_str()),
        }
    }
}

/// Which tasks a caller wants.
///
/// `Global` and `Project` are disjoint, deliberately. A global task could
/// instead be shown in every project's list — it belongs to none, after all —
/// but now that it has a list of its own on the dashboard, repeating it under
/// each project turns the thing you wanted set aside into the thing you see
/// most often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Only the tasks attached to no project.
    Global,
    /// Only the tasks attached to this project.
    Project(String),
}

impl Scope {
    /// The scope a `project` query parameter selects: a named project, or the
    /// global list when it's absent or empty.
    pub fn from_query(project: Option<&str>) -> Scope {
        match project.map(str::trim).filter(|p| !p.is_empty()) {
            Some(id) => Scope::Project(id.to_string()),
            None => Scope::Global,
        }
    }

    /// The scope for a project id that may be absent — `None` meaning global.
    pub fn of(project: Option<&str>) -> Scope {
        match project {
            Some(id) => Scope::Project(id.to_string()),
            None => Scope::Global,
        }
    }
}

/// The on-disk task list plus a monotonically increasing id counter, guarded by
/// a mutex so the web server can mutate it from multiple connection tasks.
pub struct Store {
    path: PathBuf,
    inner: Mutex<Vec<Todo>>,
}

impl Store {
    /// Open (or lazily create) the store backed by `~/.ciabatta/todos.json`.
    pub fn open() -> Result<Self> {
        let path = todos_path()?;
        let todos = load(&path)?;
        Ok(Self {
            path,
            inner: Mutex::new(todos),
        })
    }

    /// A snapshot of the tasks in `scope`, highest priority first and newest
    /// first within a priority.
    pub fn list(&self, scope: &Scope) -> Vec<Todo> {
        let todos = self.inner.lock().unwrap();
        let mut out: Vec<Todo> = todos.iter().filter(|t| t.is_in(scope)).cloned().collect();
        out.sort_by(|a, b| {
            b.priority
                .rank()
                .cmp(&a.priority.rank())
                .then(b.id.cmp(&a.id))
        });
        out
    }

    /// Add a task to a project and persist. Returns the created task.
    pub fn add(&self, text: &str, project: Option<&str>) -> Result<Todo> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "task text is empty");

        let mut todos = self.inner.lock().unwrap();
        // Ids are global rather than per-project, so every other operation can
        // stay a plain lookup by id and can't act on the wrong project's task.
        let next_id = todos.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let todo = Todo {
            id: next_id,
            text: text.to_string(),
            done: false,
            priority: Priority::default(),
            created_at: now_rfc3339(),
            project: project.map(str::to_string),
        };
        todos.push(todo.clone());
        save(&self.path, &todos)?;
        Ok(todo)
    }

    /// Move a task between a project and the global list.
    ///
    /// `None` promotes it to global; `Some(id)` files it under that project.
    /// Returns whether the task existed, so a caller acting on a stale list
    /// gets a 404 rather than a silent no-op.
    pub fn set_project(&self, id: u64, project: Option<&str>) -> Result<bool> {
        let mut todos = self.inner.lock().unwrap();
        let Some(task) = todos.iter_mut().find(|t| t.id == id) else {
            return Ok(false);
        };
        task.project = project.map(str::to_string);
        save(&self.path, &todos)?;
        Ok(true)
    }

    /// Move every task belonging to `project` onto the global list, returning
    /// how many moved.
    ///
    /// Called when a project is removed from the switcher. Without it those
    /// tasks would still be in the file, attached to an id nothing resolves any
    /// more, and so invisible in every list — deleted in effect but not in
    /// fact. Promoting them puts them somewhere the user will actually see
    /// them, which is the only outcome that doesn't quietly lose work.
    pub fn globalize(&self, project: &str) -> Result<usize> {
        let mut todos = self.inner.lock().unwrap();
        let mut moved = 0;
        for task in todos.iter_mut() {
            if task.project.as_deref() == Some(project) {
                task.project = None;
                moved += 1;
            }
        }
        if moved > 0 {
            save(&self.path, &todos)?;
        }
        Ok(moved)
    }

    /// Flip a task's completion state and persist.
    pub fn toggle(&self, id: u64) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.done = !t.done;
        }
        save(&self.path, &todos)
    }

    /// Set a task's completion state explicitly and persist. Used when the AI
    /// finishes a task that was shipped from the todo list.
    pub fn set_done(&self, id: u64, done: bool) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.done = done;
        }
        save(&self.path, &todos)
    }

    /// A task's text by id, if it exists (used to ship a todo to the AI).
    pub fn text_of(&self, id: u64) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.text.clone())
    }

    /// Set a task's priority and persist.
    pub fn set_priority(&self, id: u64, priority: Priority) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.priority = priority;
        }
        save(&self.path, &todos)
    }

    /// Replace a task's text and persist.
    pub fn set_text(&self, id: u64, text: &str) -> Result<()> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "task text is empty");

        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.text = text.to_string();
        }
        save(&self.path, &todos)
    }

    /// Remove a task and persist.
    pub fn remove(&self, id: u64) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        todos.retain(|t| t.id != id);
        save(&self.path, &todos)
    }
}

/// The path to the todos file: `$HOME/.ciabatta/todos.json` (creating the
/// `.ciabatta` directory if needed).
fn todos_path() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir.join("todos.json"))
}

/// Locate the user's home directory without pulling in an extra dependency.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Read and parse the task list, treating a missing file as an empty list.
fn load(path: &PathBuf) -> Result<Vec<Todo>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Vec::new()),
        Ok(s) => {
            serde_json::from_str(&s).with_context(|| format!("Failed to parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Serialize the task list back to disk (pretty-printed for easy hand-editing).
fn save(path: &PathBuf, todos: &[Todo]) -> Result<()> {
    let json = serde_json::to_string_pretty(todos)?;
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))
}

/// Current time as an RFC 3339 string.
fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u64, project: Option<&str>) -> Todo {
        Todo {
            id,
            text: format!("task {id}"),
            done: false,
            priority: Priority::default(),
            created_at: now_rfc3339(),
            project: project.map(str::to_string),
        }
    }

    /// A store backed by a throwaway file, so these don't touch the real list.
    fn scratch(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("ciab_todo_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store {
            path: dir.join("todos.json"),
            inner: Mutex::new(Vec::new()),
        }
    }

    /// The point of the two scopes: they're disjoint. A global task has a list
    /// of its own, so repeating it under every project would turn the thing you
    /// set aside into the thing you see most often.
    #[test]
    fn global_and_project_lists_do_not_overlap() {
        let global = task(1, None);
        let mine = task(2, Some("api"));
        let theirs = task(3, Some("web"));

        assert!(global.is_global());
        assert!(!mine.is_global());

        assert!(global.is_in(&Scope::Global));
        assert!(!mine.is_in(&Scope::Global));

        let api = Scope::Project("api".to_string());
        assert!(mine.is_in(&api));
        assert!(!theirs.is_in(&api));
        assert!(
            !global.is_in(&api),
            "a global task belongs to the global list, not to every project's"
        );
    }

    #[test]
    fn an_absent_project_selects_the_global_list() {
        assert_eq!(Scope::from_query(None), Scope::Global);
        assert_eq!(Scope::from_query(Some("")), Scope::Global);
        assert_eq!(Scope::from_query(Some("   ")), Scope::Global);
        assert_eq!(
            Scope::from_query(Some("api")),
            Scope::Project("api".to_string())
        );
        assert_eq!(Scope::of(None), Scope::Global);
        assert_eq!(Scope::of(Some("api")), Scope::Project("api".to_string()));
    }

    #[test]
    fn a_task_can_be_promoted_to_global_and_filed_back() {
        let store = scratch("promote");
        let added = store.add("renew the domain", Some("api")).unwrap();
        assert_eq!(store.list(&Scope::Project("api".into())).len(), 1);
        assert!(store.list(&Scope::Global).is_empty());

        assert!(store.set_project(added.id, None).unwrap());
        assert!(store.list(&Scope::Project("api".into())).is_empty());
        assert_eq!(store.list(&Scope::Global).len(), 1);
        assert!(store.list(&Scope::Global)[0].is_global());

        // …and back down again, to a different project this time.
        assert!(store.set_project(added.id, Some("web")).unwrap());
        assert!(store.list(&Scope::Global).is_empty());
        assert_eq!(store.list(&Scope::Project("web".into())).len(), 1);

        // Moving a task that isn't there is reported, not silently ignored —
        // a caller acting on a stale list should hear about it.
        assert!(!store.set_project(9999, None).unwrap());
    }

    /// Removing a project must not leave its tasks attached to an id nothing
    /// resolves: they'd be in the file but in no list, which is deletion
    /// without saying so.
    #[test]
    fn forgetting_a_project_promotes_its_tasks_rather_than_stranding_them() {
        let store = scratch("globalize");
        store.add("mine", Some("api")).unwrap();
        store.add("also mine", Some("api")).unwrap();
        store.add("someone else's", Some("web")).unwrap();
        store.add("already global", None).unwrap();

        assert_eq!(store.globalize("api").unwrap(), 2);
        assert!(store.list(&Scope::Project("api".into())).is_empty());
        assert_eq!(store.list(&Scope::Global).len(), 3);
        assert_eq!(
            store.list(&Scope::Project("web".into())).len(),
            1,
            "another project's tasks are none of this project's business"
        );

        // Nothing to move is not an error, and doesn't rewrite the file.
        assert_eq!(store.globalize("api").unwrap(), 0);
    }

    #[test]
    fn lists_sort_by_priority_then_newest_first() {
        let store = scratch("sort");
        let low = store.add("low", None).unwrap();
        let high = store.add("high", None).unwrap();
        let newer = store.add("newer medium", None).unwrap();

        store.set_priority(low.id, Priority::Low).unwrap();
        store.set_priority(high.id, Priority::High).unwrap();

        let ids: Vec<u64> = store.list(&Scope::Global).iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            vec![high.id, newer.id, low.id],
            "high first, then medium newest-first, then low"
        );
    }

    #[test]
    fn a_task_written_before_scoping_lands_on_the_global_list() {
        // No `project` field at all — the shape of a pre-0.2.0 todos.json.
        let legacy: Todo = serde_json::from_str(
            r#"{"id":1,"text":"from before","done":false,"created_at":"2020-01-01T00:00:00Z"}"#,
        )
        .expect("an old task still parses");

        assert!(legacy.is_global());
        assert!(legacy.is_in(&Scope::Global));
        assert!(!legacy.is_in(&Scope::Project("api".into())));
    }
}
