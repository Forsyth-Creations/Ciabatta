use crate::runner::{ProgressUpdate, RunMode, StageKind};

#[derive(Debug, Clone, PartialEq)]
pub enum RecipeStatus {
    Pending,
    Running,
    Success,
    Failed(String),
}

impl RecipeStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, RecipeStatus::Success | RecipeStatus::Failed(_))
    }
}

/// Per-stage status within a recipe's pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    Pending,
    Running,
    /// Ran a command successfully.
    Done,
    /// Fell through to a default no-op (nothing to do).
    Skipped,
    Failed,
}

pub struct RecipeState {
    pub name: String,
    pub status: RecipeStatus,
    pub stages: [StageStatus; 4],
    pub logs: Vec<String>,
}

impl RecipeState {
    /// Fraction of the pipeline completed (0.0..=1.0), for the progress gauge.
    pub fn progress(&self) -> f64 {
        let done = self
            .stages
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    StageStatus::Done | StageStatus::Skipped | StageStatus::Failed
                )
            })
            .count();
        done as f64 / self.stages.len() as f64
    }
}

pub struct App {
    pub recipes: Vec<RecipeState>,
    pub selected: usize,
    pub all_done: bool,
    pub dry_run: bool,
    pub mode: RunMode,
}

impl App {
    pub fn new(names: &[String], dry_run: bool, mode: RunMode) -> Self {
        let recipes = names
            .iter()
            .map(|n| RecipeState {
                name: n.clone(),
                status: RecipeStatus::Pending,
                stages: [StageStatus::Pending; 4],
                logs: Vec::new(),
            })
            .collect();

        App {
            recipes,
            selected: 0,
            all_done: false,
            dry_run,
            mode,
        }
    }

    pub fn apply_update(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(name) => {
                if let Some(r) = self.find_mut(&name) {
                    r.status = RecipeStatus::Running;
                }
            }
            ProgressUpdate::StageStarted { recipe, stage } => {
                if let Some(r) = self.find_mut(&recipe) {
                    r.stages[stage.index()] = StageStatus::Running;
                }
            }
            ProgressUpdate::StageFinished { recipe, stage, ran } => {
                if let Some(r) = self.find_mut(&recipe) {
                    r.stages[stage.index()] = if ran {
                        StageStatus::Done
                    } else {
                        StageStatus::Skipped
                    };
                }
            }
            ProgressUpdate::Log(name, line) => {
                if let Some(r) = self.find_mut(&name) {
                    r.logs.push(line);
                }
            }
            ProgressUpdate::Completed(name) => {
                if let Some(r) = self.find_mut(&name) {
                    r.status = RecipeStatus::Success;
                }
                self.check_all_done();
            }
            ProgressUpdate::Failed(name, err) => {
                if let Some(r) = self.find_mut(&name) {
                    // Mark the stage that was in flight as failed.
                    if let Some(idx) = r.stages.iter().position(|s| *s == StageStatus::Running) {
                        r.stages[idx] = StageStatus::Failed;
                    }
                    r.logs.push(format!("✗ failed: {err}"));
                    r.status = RecipeStatus::Failed(err);
                }
                self.check_all_done();
            }
        }
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut RecipeState> {
        self.recipes.iter_mut().find(|r| r.name == name)
    }

    fn check_all_done(&mut self) {
        self.all_done = self.recipes.iter().all(|r| r.status.is_terminal());
    }

    pub fn selected_logs(&self) -> &[String] {
        self.recipes
            .get(self.selected)
            .map(|r| r.logs.as_slice())
            .unwrap_or(&[])
    }

    /// Stage labels in order, for the currently active mode.
    pub fn stage_labels(&self) -> [&'static str; 4] {
        [
            StageKind::Login.short(self.mode),
            StageKind::Pre.short(self.mode),
            StageKind::Main.short(self.mode),
            StageKind::Post.short(self.mode),
        ]
    }

    pub fn select_next(&mut self) {
        if !self.recipes.is_empty() {
            self.selected = (self.selected + 1) % self.recipes.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.recipes.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }
}
