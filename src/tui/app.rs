use crate::runner::{ProgressUpdate, StageKind};

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Success,
    Failed(String),
}

impl WorkflowStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, WorkflowStatus::Success | WorkflowStatus::Failed(_))
    }
}

/// Per-stage status within a workflow's pipeline.
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

pub struct WorkflowState {
    pub name: String,
    pub status: WorkflowStatus,
    pub stages: [StageStatus; 4],
    pub logs: Vec<String>,
    /// For multi-file workflows: (files done, total files) reported during the
    /// main push/pull stage. `None` for single-file workflows.
    pub transfer: Option<(usize, usize)>,
}

impl WorkflowState {
    /// Fraction of the pipeline completed (0.0..=1.0), for the progress gauge.
    ///
    /// Each of the four stages is worth an equal slice. When the main stage is
    /// mid-flight on a multi-file workflow, its slice fills proportionally with the
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
    /// for display over the progress gauge. `None` for single-file workflows.
    pub fn transfer_label(&self) -> Option<String> {
        match self.transfer {
            Some((done, total)) if total > 1 => Some(format!("{done}/{total} files")),
            _ => None,
        }
    }
}

pub struct App {
    pub workflows: Vec<WorkflowState>,
    pub selected: usize,
    pub all_done: bool,
    pub dry_run: bool,
}

impl App {
    pub fn new(name: &str, dry_run: bool) -> Self {
        App {
            workflows: vec![WorkflowState {
                name: name.to_string(),
                status: WorkflowStatus::Pending,
                stages: [StageStatus::Pending; 4],
                logs: Vec::new(),
                transfer: None,
            }],
            selected: 0,
            all_done: false,
            dry_run,
        }
    }

    pub fn apply_update(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(name) => {
                if let Some(r) = self.find_mut(&name) {
                    r.status = WorkflowStatus::Running;
                }
            }
            ProgressUpdate::StageStarted { workflow, stage } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.stages[stage.index()] = StageStatus::Running;
                }
            }
            ProgressUpdate::StageFinished {
                workflow,
                stage,
                ran,
            } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.stages[stage.index()] = if ran {
                        StageStatus::Done
                    } else {
                        StageStatus::Skipped
                    };
                }
            }
            ProgressUpdate::TransferProgress {
                workflow,
                done,
                total,
            } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.transfer = Some((done, total));
                }
            }
            ProgressUpdate::Log(name, line) => {
                if let Some(r) = self.find_mut(&name) {
                    r.logs.push(line);
                }
            }
            ProgressUpdate::StepStarted { workflow, step } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.logs.push(format!("▶ {step}"));
                }
            }
            ProgressUpdate::StepFinished { workflow, step, ok } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.logs
                        .push(format!("{} {step}", if ok { "✓" } else { "✗" }));
                }
            }
            ProgressUpdate::StepSkipped {
                workflow,
                step,
                reason,
            } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.logs.push(format!("⊘ {step} (skipped: {reason})"));
                }
            }
            ProgressUpdate::StepLog {
                workflow,
                step,
                line,
            } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.logs.push(format!("  [{step}] {line}"));
                }
            }
            ProgressUpdate::StepNeedsChoice {
                workflow,
                step,
                message,
                options,
            } => {
                if let Some(r) = self.find_mut(&workflow) {
                    r.logs.push(format!("⚠ {step}: {message}"));
                    for (i, opt) in options.iter().enumerate() {
                        r.logs.push(format!("    [{i}] {opt}"));
                    }
                    r.logs.push("    (choose a fix in --gui mode)".to_string());
                }
            }
            ProgressUpdate::Completed(name) => {
                if let Some(r) = self.find_mut(&name) {
                    r.status = WorkflowStatus::Success;
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
                    r.status = WorkflowStatus::Failed(err);
                }
                self.check_all_done();
            }
        }
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut WorkflowState> {
        self.workflows.iter_mut().find(|r| r.name == name)
    }

    fn check_all_done(&mut self) {
        self.all_done = self.workflows.iter().all(|r| r.status.is_terminal());
    }

    /// True if any workflow ended in a failed state.
    pub fn any_failed(&self) -> bool {
        self.workflows
            .iter()
            .any(|r| matches!(r.status, WorkflowStatus::Failed(_)))
    }

    pub fn selected_logs(&self) -> &[String] {
        self.workflows
            .get(self.selected)
            .map(|r| r.logs.as_slice())
            .unwrap_or(&[])
    }

    /// Stage labels in order.
    pub fn stage_labels(&self) -> [&'static str; 4] {
        [
            StageKind::Login.short(),
            StageKind::Pre.short(),
            StageKind::Main.short(),
            StageKind::Post.short(),
        ]
    }

    pub fn select_next(&mut self) {
        if !self.workflows.is_empty() {
            self.selected = (self.selected + 1) % self.workflows.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.workflows.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow() -> WorkflowState {
        WorkflowState {
            name: "r".into(),
            status: WorkflowStatus::Running,
            stages: [StageStatus::Pending; 4],
            logs: Vec::new(),
            transfer: None,
        }
    }

    #[test]
    fn progress_blends_file_transfer_into_running_main_stage() {
        let mut r = workflow();
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
        let mut r = workflow();
        assert_eq!(r.transfer_label(), None);
        r.transfer = Some((0, 1)); // single-file workflow: no counter
        assert_eq!(r.transfer_label(), None);
        r.transfer = Some((3, 10));
        assert_eq!(r.transfer_label().as_deref(), Some("3/10 files"));
    }
}
