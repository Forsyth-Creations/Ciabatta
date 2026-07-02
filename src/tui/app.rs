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
    /// For multi-file recipes: (files done, total files) reported during the
    /// main push/pull stage. `None` for single-file recipes.
    pub transfer: Option<(usize, usize)>,
}

impl RecipeState {
    /// Fraction of the pipeline completed (0.0..=1.0), for the progress gauge.
    ///
    /// Each of the four stages is worth an equal slice. When the main stage is
    /// mid-flight on a multi-file recipe, its slice fills proportionally with the
    /// files transferred so far, so the bar advances within the push/pull step
    /// rather than jumping from 50% to 75% in one go.
    pub fn progress(&self) -> f64 {
        let n = self.stages.len() as f64;
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
        let mut p = done as f64 / n;

        if self.stages[StageKind::Main.index()] == StageStatus::Running
            && let Some((files_done, total)) = self.transfer
            && total > 0
        {
            p += (files_done as f64 / total as f64) / n;
        }
        p
    }

    /// A short "3/10 files" label while a multi-file transfer is in progress,
    /// for display over the progress gauge. `None` for single-file recipes.
    pub fn transfer_label(&self) -> Option<String> {
        match self.transfer {
            Some((done, total)) if total > 1 => Some(format!("{done}/{total} files")),
            _ => None,
        }
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
                transfer: None,
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
            ProgressUpdate::TransferProgress {
                recipe,
                done,
                total,
            } => {
                if let Some(r) = self.find_mut(&recipe) {
                    r.transfer = Some((done, total));
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

    /// True if any recipe ended in a failed state.
    pub fn any_failed(&self) -> bool {
        self.recipes
            .iter()
            .any(|r| matches!(r.status, RecipeStatus::Failed(_)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> RecipeState {
        RecipeState {
            name: "r".into(),
            status: RecipeStatus::Running,
            stages: [StageStatus::Pending; 4],
            logs: Vec::new(),
            transfer: None,
        }
    }

    #[test]
    fn progress_blends_file_transfer_into_running_main_stage() {
        let mut r = recipe();
        // login + pre done, push (main) running → 2 of 4 stages = 0.5.
        r.stages[StageKind::Login.index()] = StageStatus::Done;
        r.stages[StageKind::Pre.index()] = StageStatus::Skipped;
        r.stages[StageKind::Main.index()] = StageStatus::Running;
        assert!((r.progress() - 0.5).abs() < 1e-9);

        // Half the files done adds half of the main stage's 0.25 slice → 0.625.
        r.transfer = Some((2, 4));
        assert!((r.progress() - 0.625).abs() < 1e-9);

        // All files done fills the whole slice → 0.75 (still awaiting post).
        r.transfer = Some((4, 4));
        assert!((r.progress() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn transfer_label_only_for_multi_file() {
        let mut r = recipe();
        assert_eq!(r.transfer_label(), None);
        r.transfer = Some((0, 1)); // single-file recipe: no counter
        assert_eq!(r.transfer_label(), None);
        r.transfer = Some((3, 10));
        assert_eq!(r.transfer_label().as_deref(), Some("3/10 files"));
    }
}
