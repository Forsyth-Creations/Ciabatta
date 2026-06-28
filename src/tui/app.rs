use crate::runner::ProgressUpdate;

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

pub struct RecipeState {
    pub name: String,
    pub status: RecipeStatus,
    pub logs: Vec<String>,
}

pub struct App {
    pub recipes: Vec<RecipeState>,
    pub selected: usize,
    pub all_done: bool,
    pub dry_run: bool,
}

impl App {
    pub fn new(names: &[String], dry_run: bool) -> Self {
        let recipes = names
            .iter()
            .map(|n| RecipeState {
                name: n.clone(),
                status: RecipeStatus::Pending,
                logs: Vec::new(),
            })
            .collect();

        App {
            recipes,
            selected: 0,
            all_done: false,
            dry_run,
        }
    }

    pub fn apply_update(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(name) => {
                if let Some(r) = self.find_mut(&name) {
                    r.status = RecipeStatus::Running;
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
