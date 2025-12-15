use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::executor::{ExecutorEvent, TaskState, TaskStatus};
use crate::output::LOG_DIR;

/// UI manager using indicatif
pub struct UI {
    /// Multi-progress container (kept alive so progress bars render correctly)
    _multi: MultiProgress,
    /// Progress bar for each recipe
    bars: HashMap<String, ProgressBar>,
    /// Maximum recipe name length (for alignment)
    max_name_len: usize,
}

impl UI {
    /// Create a new UI with the given recipe names
    pub fn new(recipes: &[String]) -> Self {
        let multi = MultiProgress::new();
        let max_name_len = recipes.iter().map(|r| r.len()).max().unwrap_or(20).max(15);

        let mut bars = HashMap::new();

        for recipe in recipes {
            let pb = multi.add(ProgressBar::new_spinner());
            pb.set_style(Self::waiting_style(max_name_len));
            pb.set_prefix(recipe.clone());
            pb.set_message("waiting");
            bars.insert(recipe.clone(), pb);
        }

        Self {
            _multi: multi,
            bars,
            max_name_len,
        }
    }

    /// Create style for waiting tasks
    fn waiting_style(name_len: usize) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .template(&format!("{{prefix:<{name_len}}}  ◦  {{msg}}"))
            .unwrap()
    }

    /// Create style for running tasks
    fn running_style(name_len: usize) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .template(&format!(
                "{{prefix:<{name_len}}}  {{spinner:.cyan}}  {{msg}}"
            ))
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
    }

    /// Create style for completed tasks
    fn completed_style(name_len: usize) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .template(&format!("{{prefix:<{name_len}}}  ✓  {{msg:.green}}"))
            .unwrap()
    }

    /// Create style for failed tasks
    fn failed_style(name_len: usize) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .template(&format!("{{prefix:<{name_len}}}  ✗  {{msg:.red}}"))
            .unwrap()
    }

    /// Create style for skipped tasks
    fn skipped_style(name_len: usize) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .template(&format!("{{prefix:<{name_len}}}  ⊘  {{msg:.yellow}}"))
            .unwrap()
    }

    /// Update a task's display based on its status
    pub fn update_task(&self, status: &TaskStatus) {
        let Some(pb) = self.bars.get(&status.recipe) else {
            return;
        };

        match &status.state {
            TaskState::Waiting => {
                pb.set_style(Self::waiting_style(self.max_name_len));
                let msg = if status.dependencies.is_empty() {
                    "waiting".to_string()
                } else {
                    format!("waiting (depends on: {})", status.dependencies.join(", "))
                };
                pb.set_message(msg);
            }
            TaskState::Running => {
                pb.set_style(Self::running_style(self.max_name_len));
                pb.enable_steady_tick(Duration::from_millis(100));
                let msg = if status.output_preview.is_empty() {
                    "running...".to_string()
                } else {
                    format!("running...  {}", status.output_preview)
                };
                pb.set_message(msg);
            }
            TaskState::Completed {
                duration,
                total_duration,
            } => {
                pb.set_style(Self::completed_style(self.max_name_len));
                pb.disable_steady_tick();
                let msg = match total_duration {
                    Some(total) => format!(
                        "completed in {:.1}s ({:.1}s total)",
                        duration.as_secs_f64(),
                        total.as_secs_f64()
                    ),
                    None => format!("completed in {:.1}s", duration.as_secs_f64()),
                };
                pb.set_message(msg);
            }
            TaskState::Failed { duration } => {
                pb.set_style(Self::failed_style(self.max_name_len));
                pb.disable_steady_tick();
                pb.set_message(format!(
                    "FAILED in {:.1}s (see {}/{}.log)",
                    duration.as_secs_f64(),
                    LOG_DIR,
                    status.recipe
                ));
            }
            TaskState::Skipped { failed_dep } => {
                pb.set_style(Self::skipped_style(self.max_name_len));
                pb.disable_steady_tick();
                pb.set_message(format!("skipped ({} failed)", failed_dep));
            }
        }
    }

    /// Update a task's output preview
    pub fn update_output(&self, recipe: &str, preview: &str) {
        if let Some(pb) = self.bars.get(recipe) {
            // Only update if the task is still running
            pb.set_message(format!("running...  {}", preview));
        }
    }

    /// Finish all progress bars
    pub fn finish(&self) {
        for pb in self.bars.values() {
            pb.finish();
        }
    }

    /// Run the UI event loop, processing events from the executor
    /// Returns (success_count, failed_count, total_duration)
    pub async fn run(
        &self,
        mut event_rx: mpsc::Receiver<ExecutorEvent>,
    ) -> (usize, usize, Duration) {
        let mut success_count = 0;
        let mut failed_count = 0;
        let mut total_duration = Duration::ZERO;

        while let Some(event) = event_rx.recv().await {
            match event {
                ExecutorEvent::TaskUpdated(status) => {
                    self.update_task(&status);
                }
                ExecutorEvent::TaskOutput { recipe, preview } => {
                    self.update_output(&recipe, &preview);
                }
                ExecutorEvent::AllDone {
                    success,
                    failed,
                    total_duration: duration,
                } => {
                    success_count = success;
                    failed_count = failed;
                    total_duration = duration;
                    break;
                }
            }
        }

        self.finish();
        (success_count, failed_count, total_duration)
    }
}
