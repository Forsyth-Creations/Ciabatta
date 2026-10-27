//! What the run TUI knows: every step of the run, what each one is doing, and
//! everything it has printed — fed by the engine's [`ProgressUpdate`]s.
//!
//! The screen is a list of steps down the left and the selected one's output on
//! the right, the way turbo shows tasks. Above the steps sits the run itself,
//! whose log is everything interleaved, as the plain runner prints it.

use std::time::{Duration, Instant};

use crate::runner::{ProgressUpdate, StepChoice};

/// Where a step has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Waiting for what it needs.
    Pending,
    /// The cache is deciding whether it has to run.
    Checking,
    /// Its outputs are coming down from the remote cache.
    Restoring,
    Running,
    /// Waiting on somebody: a `ciabatta input` question or a recovery choice.
    Asking,
    Succeeded,
    /// Up to date, or restored, without running.
    Cached,
    /// Its `when`/`skip_if` said not to run it.
    Skipped,
    /// Failed, but it declared `warn_on_error`, so the run carried on.
    Warned,
    Failed,
    /// Never got to run: the run failed or was stopped first.
    NotRun,
    /// Never ran, and the run succeeded without it — a recovery step nothing
    /// failed into.
    NotNeeded,
}

impl StepStatus {
    /// The emoji the step list shows. Each is a single code point with an
    /// emoji presentation of its own, so every terminal draws it two columns
    /// wide and the names beside them stay aligned.
    pub fn emoji(self) -> &'static str {
        match self {
            StepStatus::Pending => "⏳",
            StepStatus::Checking => "🔍",
            StepStatus::Restoring => "📥",
            StepStatus::Running => "🏃",
            StepStatus::Asking => "❓",
            StepStatus::Succeeded => "✅",
            StepStatus::Cached => "📦",
            StepStatus::Skipped => "⏩",
            StepStatus::Warned => "🟡",
            StepStatus::Failed => "❌",
            StepStatus::NotRun => "⛔",
            StepStatus::NotNeeded => "⚪",
        }
    }

    /// A word for the same thing, for the log pane's title and the summary
    /// printed once the TUI closes.
    pub fn word(self) -> &'static str {
        match self {
            StepStatus::Pending => "waiting",
            StepStatus::Checking => "checking the cache",
            StepStatus::Restoring => "restoring",
            StepStatus::Running => "running",
            StepStatus::Asking => "waiting for input",
            StepStatus::Succeeded => "done",
            StepStatus::Cached => "cached",
            StepStatus::Skipped => "skipped",
            StepStatus::Warned => "failed (warning)",
            StepStatus::Failed => "failed",
            StepStatus::NotRun => "not run",
            StepStatus::NotNeeded => "not needed",
        }
    }

    /// Whether the step has finished, one way or another.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            StepStatus::Succeeded
                | StepStatus::Cached
                | StepStatus::Skipped
                | StepStatus::Warned
                | StepStatus::Failed
                | StepStatus::NotRun
                | StepStatus::NotNeeded
        )
    }

    /// Whether the step is doing something right now.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            StepStatus::Checking | StepStatus::Restoring | StepStatus::Running | StepStatus::Asking
        )
    }
}

/// One step of the run, as the list and the log pane show it.
pub struct StepView {
    pub name: String,
    pub status: StepStatus,
    /// Everything this step printed, plus a line for each thing that happened
    /// to it.
    pub logs: Vec<String>,
    started: Option<Instant>,
    took: Option<Duration>,
    /// The cache's last word on it: whether a skip that follows was the cache
    /// serving it (`fresh`/`hit`) rather than a condition.
    cache_hit: bool,
}

impl StepView {
    fn new(name: &str) -> Self {
        StepView {
            name: name.to_string(),
            status: StepStatus::Pending,
            logs: Vec::new(),
            started: None,
            took: None,
            cache_hit: false,
        }
    }

    /// How long it ran, or has been running.
    pub fn elapsed(&self) -> Option<Duration> {
        self.took.or_else(|| self.started.map(|t| t.elapsed()))
    }
}

/// The run as a whole: the top entry in the list.
pub struct RunView {
    pub name: String,
    pub status: RunStatus,
    /// Everything, interleaved — what the plain runner would have printed.
    pub logs: Vec<String>,
    started: Instant,
    took: Option<Duration>,
}

impl RunView {
    pub fn elapsed(&self) -> Duration {
        self.took.unwrap_or_else(|| self.started.elapsed())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed(String),
}

/// What the right-hand pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The selected entry's output.
    Output,
    /// The environment the run resolved: every variable it depends on, its
    /// value, and where that value came from.
    Env,
}

/// Something the run is waiting for somebody here to answer.
#[derive(Debug, Clone)]
pub enum Prompt {
    /// A step ran `ciabatta input`.
    Input {
        pending: crate::run::input::Pending,
        /// What has been typed so far.
        typed: String,
    },
    /// A recovery node wants one of its fix options picked.
    Choice {
        workflow: String,
        step: String,
        message: String,
        options: Vec<String>,
        /// The highlighted option.
        selected: usize,
    },
}

/// What answering a [`Prompt`] sends back to the run.
#[derive(Debug, Clone)]
pub enum Reply {
    Input(crate::run::input::Answer),
    Choice(StepChoice),
}

/// How the step list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// The order the run schedules them in.
    Run,
    /// Alphabetical, so a step is quick to find in a big monorepo.
    Name,
}

impl SortOrder {
    pub fn word(self) -> &'static str {
        match self {
            SortOrder::Run => "run order",
            SortOrder::Name => "by name",
        }
    }
}

pub struct App {
    pub run: RunView,
    pub steps: Vec<StepView>,
    /// The highlighted entry: 0 is the run itself, `i + 1` is the `i`th step
    /// in the list as it's sorted.
    pub selected: usize,
    pub sort: SortOrder,
    /// Indices into `steps`, in the order the list shows them.
    order: Vec<usize>,
    /// How far the log pane is scrolled up from its newest line. `None`
    /// follows the output as it arrives.
    pub scroll: Option<usize>,
    /// How many rows the log pane held when it was last drawn, so output that
    /// arrives while scrolled up pushes `scroll` back rather than the view.
    /// `None` until the selected entry has been drawn once.
    pub seen_rows: Option<usize>,
    /// The first entry the list is drawn from, kept between frames so moving
    /// the selection only scrolls the list once it reaches an edge.
    pub list_offset: usize,
    /// The newest warning the run raised, shown as a banner over everything.
    pub banner: Option<String>,
    pub dry_run: bool,
    /// Set once the run's channel closes.
    pub finished: bool,
    /// Quit was asked for while the run was still going; it's being stopped.
    pub stopping: bool,
    /// Somebody has touched the keyboard since the run ended, so it shouldn't
    /// close on its own under them.
    pub browsing: bool,
    /// What the right-hand pane is showing.
    pub pane: Pane,
    /// The resolved environment, as `ciabatta env` would print it.
    pub env: Vec<String>,
    /// Questions waiting on an answer, oldest first. The first is on screen.
    pub prompts: std::collections::VecDeque<Prompt>,
}

impl App {
    pub fn new<'a>(name: &str, steps: impl IntoIterator<Item = &'a str>, dry_run: bool) -> Self {
        App {
            run: RunView {
                name: name.to_string(),
                status: RunStatus::Running,
                logs: Vec::new(),
                started: Instant::now(),
                took: None,
            },
            steps: steps.into_iter().map(StepView::new).collect(),
            selected: 0,
            sort: SortOrder::Run,
            order: Vec::new(),
            scroll: None,
            seen_rows: None,
            list_offset: 0,
            banner: None,
            dry_run,
            finished: false,
            stopping: false,
            browsing: false,
            pane: Pane::Output,
            env: Vec::new(),
            prompts: Default::default(),
        }
        .sorted()
    }

    fn sorted(mut self) -> Self {
        self.resort();
        self
    }

    fn resort(&mut self) {
        let mut order: Vec<usize> = (0..self.steps.len()).collect();
        if self.sort == SortOrder::Name {
            order.sort_by(|&a, &b| self.steps[a].name.cmp(&self.steps[b].name));
        }
        self.order = order;
    }

    /// Switch between run order and by name, keeping the same step selected.
    pub fn toggle_sort(&mut self) {
        let current = self.selected.checked_sub(1).map(|i| self.order[i]);
        self.sort = match self.sort {
            SortOrder::Run => SortOrder::Name,
            SortOrder::Name => SortOrder::Run,
        };
        self.resort();
        if let Some(step) = current {
            self.selected = 1 + self.order.iter().position(|&i| i == step).unwrap_or(0);
        }
    }

    /// The `i`th step as the list shows it.
    pub fn listed(&self, i: usize) -> &StepView {
        &self.steps[self.order[i]]
    }

    pub fn apply_update(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(_) => {}
            ProgressUpdate::StageStarted { stage, .. } => {
                self.run.logs.push(format!("▶ {}", stage.label()));
            }
            ProgressUpdate::StageFinished { stage, ran, .. } => {
                if ran {
                    self.run.logs.push(format!("✓ {}", stage.label()));
                }
            }
            ProgressUpdate::TransferProgress { done, total, .. } => {
                if total > 1 {
                    self.run.logs.push(format!("  {done}/{total} files"));
                }
            }
            ProgressUpdate::Log(_, line) => self.run.logs.push(clean(&line)),
            ProgressUpdate::StepProcessing { step, .. } => {
                self.set(&step, StepStatus::Checking);
            }
            ProgressUpdate::StepCache { step, report, .. } => {
                let hit = matches!(report.outcome.as_str(), "fresh" | "hit");
                let line = format!("cache: {}", report.summary);
                if let Some(s) = self.step_mut(&step) {
                    s.cache_hit = hit;
                    s.logs.push(line);
                }
            }
            ProgressUpdate::StepRestoring { step, progress, .. } => {
                let first = progress.phase == "downloading"
                    && progress.bytes_done == 0
                    && progress.current.is_none();
                if let Some(s) = self.step_mut(&step) {
                    s.status = StepStatus::Restoring;
                    if first {
                        s.logs.push(format!(
                            "↓ downloading {} file(s) from the remote cache",
                            progress.files_total
                        ));
                    }
                }
            }
            ProgressUpdate::StepStarted { step, .. } => {
                self.run.logs.push(format!("▶ {step}"));
                if let Some(s) = self.step_mut(&step) {
                    // A retry routes back here: the clock starts again.
                    s.status = StepStatus::Running;
                    s.started = Some(Instant::now());
                    s.took = None;
                    s.logs.push(format!("▶ started at {}", clock()));
                }
            }
            ProgressUpdate::StepLog { step, line, .. } => {
                let line = clean(&line);
                self.run.logs.push(format!("  [{step}] {line}"));
                if let Some(s) = self.step_mut(&step) {
                    s.logs.push(line);
                }
            }
            ProgressUpdate::StepFinished { step, ok, .. } => {
                let mark = if ok { "✓" } else { "✗" };
                let mut took = String::new();
                if let Some(s) = self.step_mut(&step) {
                    s.status = if ok {
                        StepStatus::Succeeded
                    } else {
                        StepStatus::Failed
                    };
                    s.took = s.started.map(|t| t.elapsed());
                    if let Some(d) = s.took {
                        took = format!("{} · ", crate::runner::elapsed(d));
                    }
                    let word = if ok { "finished" } else { "failed" };
                    s.logs.push(format!("{mark} {word} ({took}{})", clock()));
                }
                self.run
                    .logs
                    .push(format!("{mark} {step} ({took}{})", clock()));
            }
            ProgressUpdate::StepWarned { step, .. } => {
                let line = "⚠ failed as a warning (warn_on_error)";
                self.run.logs.push(format!("⚠ {step} failed as a warning"));
                if let Some(s) = self.step_mut(&step) {
                    s.status = StepStatus::Warned;
                    s.logs.push(line.to_string());
                }
            }
            ProgressUpdate::StepSkipped { step, reason, .. } => {
                self.run.logs.push(format!("⊘ {step} (skipped: {reason})"));
                if let Some(s) = self.step_mut(&step) {
                    s.status = if s.cache_hit {
                        StepStatus::Cached
                    } else if is_not_run(&reason) {
                        StepStatus::NotRun
                    } else {
                        StepStatus::Skipped
                    };
                    s.logs.push(format!("⊘ skipped: {reason}"));
                }
            }
            ProgressUpdate::StepNeedsChoice {
                workflow,
                step,
                message,
                options,
            } => {
                if let Some(s) = self.step_mut(&step) {
                    s.status = StepStatus::Asking;
                    s.logs.push(format!("⚠ {message}"));
                    for (i, opt) in options.iter().enumerate() {
                        s.logs.push(format!("    [{i}] {opt}"));
                    }
                }
                self.run.logs.push(format!("⚠ {step}: {message}"));
                self.prompts.push_back(Prompt::Choice {
                    workflow,
                    step,
                    message,
                    options,
                    selected: 0,
                });
            }
            // Put up for answering; the answer reaches the step's log as a
            // line of its own once the step has it.
            ProgressUpdate::InputRequested { step, input, .. } => {
                if let Some(s) = self.step_mut(&step) {
                    s.status = StepStatus::Asking;
                    s.logs.push(format!("? {}", input.prompt));
                }
                self.prompts.push_back(Prompt::Input {
                    pending: *input,
                    typed: String::new(),
                });
            }
            // Answered here, or settled some other way — timed out, or its
            // step went away: either way it comes down.
            ProgressUpdate::InputResolved { step, id, .. } => {
                self.prompts
                    .retain(|p| !matches!(p, Prompt::Input { pending, .. } if pending.id == id));
                if let Some(s) = self.step_mut(&step)
                    && s.status == StepStatus::Asking
                {
                    s.status = StepStatus::Running;
                }
            }
            ProgressUpdate::Warning { message, .. } => {
                self.run.logs.push(format!("⚠ {message}"));
                self.banner = Some(message);
            }
            ProgressUpdate::Completed(_) => {
                self.run.status = RunStatus::Succeeded;
                self.run.took = Some(self.run.started.elapsed());
            }
            ProgressUpdate::Failed(_, err) => {
                self.run.logs.push(format!("✗ failed: {err}"));
                self.run.status = RunStatus::Failed(err);
                self.run.took = Some(self.run.started.elapsed());
            }
        }
    }

    /// The run's channel closed: nothing more is coming. Whatever never
    /// started never will — and on a run that succeeded, wasn't needed to.
    pub fn finish(&mut self) {
        self.finished = true;
        self.run
            .took
            .get_or_insert_with(|| self.run.started.elapsed());
        let unneeded = if self.succeeded() {
            StepStatus::NotNeeded
        } else {
            StepStatus::NotRun
        };
        for s in &mut self.steps {
            if !s.status.is_settled() {
                s.status = unneeded;
                s.took = s.started.map(|t| t.elapsed());
            }
        }
    }

    /// Hand a key to the question on screen. Returns what to send the run
    /// when the key answered it; the question then comes down.
    pub fn prompt_key(&mut self, code: crossterm::event::KeyCode) -> Option<Reply> {
        use crossterm::event::KeyCode;

        let reply = match self.prompts.front_mut()? {
            Prompt::Input { pending, typed } => {
                let answer = |value: &str| {
                    Some(Reply::Input(crate::run::input::Answer {
                        id: pending.id,
                        value: value.to_string(),
                    }))
                };
                match code {
                    KeyCode::Enter if pending.enter => answer(""),
                    // Enter on an empty line takes the default, as it does at
                    // the plain terminal's prompt.
                    KeyCode::Enter if !pending.yes_no || pending.default.is_some() => {
                        answer(typed.as_str())
                    }
                    KeyCode::Char(c) if pending.yes_no && matches!(c, 'y' | 'Y' | 'n' | 'N') => {
                        answer(&c.to_ascii_lowercase().to_string())
                    }
                    KeyCode::Char(c) if !pending.enter && !pending.yes_no => {
                        typed.push(c);
                        None
                    }
                    KeyCode::Backspace => {
                        typed.pop();
                        None
                    }
                    _ => None,
                }
            }
            Prompt::Choice {
                workflow,
                step,
                options,
                selected,
                ..
            } => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = selected.saturating_sub(1);
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(options.len().saturating_sub(1));
                    None
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    if n < options.len() {
                        *selected = n;
                    }
                    None
                }
                KeyCode::Enter if !options.is_empty() => Some(Reply::Choice(StepChoice {
                    workflow: workflow.clone(),
                    step: step.clone(),
                    option: *selected,
                })),
                _ => None,
            },
        };
        // Down now rather than when the run confirms it, so a second Enter
        // can't answer twice. A recovery choice has no confirmation to wait
        // for, so its step's log says what was picked.
        if reply.is_some()
            && let Some(Prompt::Choice {
                step,
                options,
                selected,
                ..
            }) = self.prompts.pop_front()
            && let Some(s) = self.step_mut(&step)
        {
            s.logs.push(format!("→ chose: {}", options[selected]));
            s.status = StepStatus::Running;
        }
        reply
    }

    fn set(&mut self, step: &str, status: StepStatus) {
        if let Some(s) = self.step_mut(step) {
            s.status = status;
        }
    }

    fn step_mut(&mut self, name: &str) -> Option<&mut StepView> {
        self.steps.iter_mut().find(|s| s.name == name)
    }

    pub fn failed(&self) -> bool {
        matches!(self.run.status, RunStatus::Failed(_))
            || self.steps.iter().any(|s| s.status == StepStatus::Failed)
    }

    pub fn succeeded(&self) -> bool {
        self.run.status == RunStatus::Succeeded && !self.failed()
    }

    /// How many entries the list has: the run, then each step.
    pub fn entries(&self) -> usize {
        self.steps.len() + 1
    }

    /// The selected step, or `None` while the run itself is selected.
    pub fn selected_step(&self) -> Option<&StepView> {
        self.selected
            .checked_sub(1)
            .and_then(|i| self.order.get(i))
            .map(|&i| &self.steps[i])
    }

    pub fn selected_logs(&self) -> &[String] {
        match (self.pane, self.selected_step()) {
            (Pane::Env, _) => &self.env,
            (Pane::Output, Some(s)) => &s.logs,
            (Pane::Output, None) => &self.run.logs,
        }
    }

    /// Switch the right-hand pane between output and the environment.
    pub fn toggle_env(&mut self) {
        self.pane = match self.pane {
            Pane::Output => Pane::Env,
            Pane::Env => Pane::Output,
        };
        self.scroll = None;
        self.seen_rows = None;
    }

    /// Steps that have finished, for the header's count.
    pub fn settled(&self) -> usize {
        self.steps.iter().filter(|s| s.status.is_settled()).count()
    }

    fn select(&mut self, index: usize) {
        let index = index.min(self.entries() - 1);
        if index != self.selected {
            self.selected = index;
            // Picking a step is asking to see what it printed.
            self.pane = Pane::Output;
            // Another step's output starts at its newest line.
            self.scroll = None;
            self.seen_rows = None;
        }
    }

    pub fn select_next(&mut self) {
        self.select(self.selected + 1);
    }

    pub fn select_prev(&mut self) {
        self.select(self.selected.saturating_sub(1));
    }

    pub fn select_first(&mut self) {
        self.select(0);
    }

    pub fn select_last(&mut self) {
        self.select(self.entries() - 1);
    }

    /// Jump to the next step after the selection that failed, wrapping round —
    /// or, with none failed, the next one that's doing something.
    pub fn select_next_interesting(&mut self) {
        let n = self.steps.len();
        let start = self.selected; // `steps[start]` is the entry after the selection
        for wanted in [
            |s: StepStatus| s == StepStatus::Failed,
            |s: StepStatus| s.is_active(),
        ] {
            if let Some(i) = (0..n)
                .map(|k| (start + k) % n)
                .find(|&i| wanted(self.listed(i).status))
            {
                self.select(i + 1);
                return;
            }
        }
    }

    /// Scroll the log pane up (towards older output) by `rows`.
    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = Some(self.scroll.unwrap_or(0).saturating_add(rows));
    }

    /// Scroll the log pane down by `rows`; reaching the bottom follows again.
    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll = match self.scroll {
            Some(n) if n > rows => Some(n - rows),
            _ => None,
        };
    }

    pub fn follow(&mut self) {
        self.scroll = None;
    }

    /// The log pane now holds `rows` rows. Scrolled up, the rows that arrived
    /// since the last frame are added to `scroll` so what's on screen stays put.
    pub fn saw_rows(&mut self, rows: usize) {
        if let (Some(n), Some(seen)) = (self.scroll, self.seen_rows)
            && rows > seen
        {
            self.scroll = Some(n + (rows - seen));
        }
        self.seen_rows = Some(rows);
    }
}

/// Reasons the engine gives for a step it never got to, as opposed to one a
/// condition chose to skip.
fn is_not_run(reason: &str) -> bool {
    reason.starts_with("not started") || reason.starts_with("blocked by")
}

/// A line as a terminal would have left it: escape sequences a tool sent
/// anyway (colour is off for the TUI, but not every tool asks) are dropped, and
/// a progress bar redrawn with `\r` keeps only its last state.
fn clean(line: &str) -> String {
    static ESCAPES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b(\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(\x07|\x1b\\)|[@-Z\\-_])")
            .expect("valid regex")
    });
    let last = line
        .trim_end_matches('\r')
        .rsplit('\r')
        .next()
        .unwrap_or("");
    ESCAPES.replace_all(last, "").replace('\t', "    ")
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new("build", ["compile", "test", "lint"], false)
    }

    fn started(step: &str) -> ProgressUpdate {
        ProgressUpdate::StepStarted {
            workflow: "build".into(),
            step: step.into(),
        }
    }

    fn finished(step: &str, ok: bool) -> ProgressUpdate {
        ProgressUpdate::StepFinished {
            workflow: "build".into(),
            step: step.into(),
            ok,
        }
    }

    fn skipped(step: &str, reason: &str) -> ProgressUpdate {
        ProgressUpdate::StepSkipped {
            workflow: "build".into(),
            step: step.into(),
            reason: reason.into(),
        }
    }

    #[test]
    fn each_step_keeps_its_own_output() {
        let mut app = app();
        app.apply_update(started("compile"));
        app.apply_update(ProgressUpdate::StepLog {
            workflow: "build".into(),
            step: "compile".into(),
            line: "Compiling ciabatta".into(),
        });
        assert_eq!(app.steps[0].status, StepStatus::Running);
        assert!(app.steps[0].logs.iter().any(|l| l == "Compiling ciabatta"));
        assert!(app.steps[1].logs.is_empty());
        // The run's own log has everything, tagged with the step.
        assert!(
            app.run
                .logs
                .iter()
                .any(|l| l == "  [compile] Compiling ciabatta")
        );
    }

    #[test]
    fn escapes_and_redrawn_progress_are_cleaned_up() {
        assert_eq!(clean("\x1b[1;32mok\x1b[0m done"), "ok done");
        assert_eq!(clean("10%\r50%\r100%"), "100%");
        assert_eq!(clean("a\tb"), "a    b");
    }

    #[test]
    fn a_finished_step_says_how_long_it_took() {
        let mut app = app();
        app.apply_update(started("compile"));
        app.apply_update(finished("compile", true));
        let step = &app.steps[0];
        assert_eq!(step.status, StepStatus::Succeeded);
        assert!(step.took.is_some());
        let last = app.run.logs.last().unwrap();
        assert!(last.starts_with("✓ compile (0."), "{last}");
    }

    #[test]
    fn a_skip_says_whether_the_cache_served_it() {
        let mut app = app();
        app.apply_update(skipped("compile", "skip_if `env.CI == true`"));
        assert_eq!(app.steps[0].status, StepStatus::Skipped);
        app.apply_update(skipped("test", "blocked by a failed dependency"));
        assert_eq!(app.steps[1].status, StepStatus::NotRun);
    }

    #[test]
    fn steps_that_never_started_are_marked_when_the_run_ends() {
        let mut app = app();
        app.apply_update(started("compile"));
        app.apply_update(finished("compile", false));
        app.apply_update(ProgressUpdate::Failed(
            "build".into(),
            "compile failed".into(),
        ));
        app.finish();
        assert_eq!(app.steps[0].status, StepStatus::Failed);
        assert_eq!(app.steps[1].status, StepStatus::NotRun);
        assert!(app.failed());
        assert!(!app.succeeded());
    }

    #[test]
    fn selection_moves_through_the_run_then_its_steps() {
        let mut app = app();
        assert!(app.selected_step().is_none());
        app.select_next();
        assert_eq!(app.selected_step().unwrap().name, "compile");
        app.select_last();
        assert_eq!(app.selected_step().unwrap().name, "lint");
        app.select_next();
        assert_eq!(app.selected_step().unwrap().name, "lint");
        app.select_first();
        assert!(app.selected_step().is_none());
    }

    #[test]
    fn changing_step_goes_back_to_following_its_output() {
        let mut app = app();
        app.scroll_up(5);
        assert_eq!(app.scroll, Some(5));
        app.scroll_down(2);
        assert_eq!(app.scroll, Some(3));
        app.select_next();
        assert_eq!(app.scroll, None);
        app.scroll_up(2);
        app.scroll_down(10);
        assert_eq!(app.scroll, None);
    }

    #[test]
    fn new_output_holds_a_scrolled_view_still() {
        let mut app = app();
        app.saw_rows(40);
        // Following, new output just shows up at the bottom.
        app.saw_rows(45);
        assert_eq!(app.scroll, None);
        app.scroll_up(3);
        app.saw_rows(50);
        assert_eq!(app.scroll, Some(8));
        // Scrolling back to the bottom follows again, and so does `f`.
        app.scroll_down(8);
        assert_eq!(app.scroll, None);
        app.saw_rows(60);
        assert_eq!(app.scroll, None);
        app.scroll_up(3);
        app.saw_rows(70);
        app.follow();
        assert_eq!(app.scroll, None);
    }

    #[test]
    fn sorting_by_name_keeps_the_selected_step() {
        let mut app = app(); // compile, test, lint
        app.select_last();
        assert_eq!(app.selected_step().unwrap().name, "lint");
        app.toggle_sort();
        assert_eq!(app.sort, SortOrder::Name);
        let names: Vec<_> = (0..3).map(|i| app.listed(i).name.as_str()).collect();
        assert_eq!(names, ["compile", "lint", "test"]);
        assert_eq!(app.selected_step().unwrap().name, "lint");
        app.select_next();
        assert_eq!(app.selected_step().unwrap().name, "test");
        app.toggle_sort();
        assert_eq!(app.listed(2).name, "lint");
        assert_eq!(app.selected_step().unwrap().name, "test");
    }

    #[test]
    fn a_step_a_successful_run_never_needed_is_not_a_failure() {
        let mut app = app();
        app.apply_update(ProgressUpdate::Completed("build".into()));
        app.finish();
        assert_eq!(app.steps[0].status, StepStatus::NotNeeded);
        assert!(app.succeeded());
    }

    #[test]
    fn jumping_finds_a_failure_before_anything_running() {
        let mut app = app();
        app.apply_update(started("compile"));
        app.apply_update(started("lint"));
        app.apply_update(finished("lint", false));
        app.select_next_interesting();
        assert_eq!(app.selected_step().unwrap().name, "lint");
        app.apply_update(finished("lint", true));
        app.select_next_interesting();
        assert_eq!(app.selected_step().unwrap().name, "compile");
    }

    fn asked(id: u64, yes_no: bool, default: Option<&str>) -> ProgressUpdate {
        ProgressUpdate::InputRequested {
            workflow: "build".into(),
            step: "compile".into(),
            input: Box::new(crate::run::input::Pending {
                id,
                step: "compile".into(),
                prompt: "Which region?".into(),
                details: None,
                default: default.map(str::to_string),
                secret: false,
                env: None,
                enter: false,
                yes_no,
                asked_at: String::new(),
                deadline: None,
            }),
        }
    }

    #[test]
    fn a_question_is_typed_into_and_answered_with_enter() {
        use crossterm::event::KeyCode;
        let mut app = app();
        app.apply_update(asked(7, false, None));
        assert_eq!(app.steps[0].status, StepStatus::Asking);
        for c in "eu-west-2x".chars() {
            assert!(app.prompt_key(KeyCode::Char(c)).is_none());
        }
        assert!(app.prompt_key(KeyCode::Backspace).is_none());
        match app.prompt_key(KeyCode::Enter) {
            Some(Reply::Input(answer)) => {
                assert_eq!((answer.id, answer.value.as_str()), (7, "eu-west-2"))
            }
            other => panic!("expected an answer, got {other:?}"),
        }
        assert!(app.prompts.is_empty(), "answered, so it comes down");
    }

    #[test]
    fn a_yes_no_question_answers_on_the_key_and_waits_without_a_default() {
        use crossterm::event::KeyCode;
        let mut app = app();
        app.apply_update(asked(1, true, None));
        assert!(
            app.prompt_key(KeyCode::Enter).is_none(),
            "no default to take"
        );
        assert!(app.prompt_key(KeyCode::Char('x')).is_none());
        match app.prompt_key(KeyCode::Char('Y')) {
            Some(Reply::Input(answer)) => assert_eq!(answer.value, "y"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn a_question_settled_elsewhere_comes_down() {
        let mut app = app();
        app.apply_update(asked(3, false, Some("us-east-1")));
        app.apply_update(asked(4, false, None));
        app.apply_update(ProgressUpdate::InputResolved {
            workflow: "build".into(),
            step: "compile".into(),
            id: 3,
            outcome: crate::run::input::Outcome::TimedOut,
        });
        assert_eq!(app.prompts.len(), 1);
        assert!(matches!(&app.prompts[0], Prompt::Input { pending, .. } if pending.id == 4));
    }

    #[test]
    fn a_recovery_choice_is_picked_and_logged() {
        use crossterm::event::KeyCode;
        let mut app = app();
        app.apply_update(ProgressUpdate::StepNeedsChoice {
            workflow: "build".into(),
            step: "test".into(),
            message: "tests failed".into(),
            options: vec!["retry".into(), "skip".into()],
        });
        assert!(app.prompt_key(KeyCode::Down).is_none());
        match app.prompt_key(KeyCode::Enter) {
            Some(Reply::Choice(choice)) => {
                assert_eq!((choice.step.as_str(), choice.option), ("test", 1))
            }
            other => panic!("expected a choice, got {other:?}"),
        }
        assert!(app.prompts.is_empty());
        assert!(app.steps[1].logs.iter().any(|l| l == "→ chose: skip"));
    }

    #[test]
    fn the_env_tab_swaps_the_pane_and_choosing_a_step_swaps_it_back() {
        let mut app = app();
        app.env = vec!["GREETING  hello".into()];
        app.toggle_env();
        assert_eq!(app.selected_logs(), ["GREETING  hello".to_string()]);
        app.select_next();
        assert_eq!(app.pane, Pane::Output);
    }
}
