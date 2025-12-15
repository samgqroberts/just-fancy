use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

use crate::graph::RecipeGraph;
use crate::output::OutputCollector;

/// State of a task
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// Waiting for dependencies to complete
    Waiting,
    /// Currently running
    Running,
    /// Completed successfully
    Completed {
        duration: Duration,
        /// Total time including dependencies (only set if has deps)
        total_duration: Option<Duration>,
    },
    /// Failed with an error
    Failed { duration: Duration },
    /// Skipped because a dependency failed
    Skipped { failed_dep: String },
}

/// Information about a task's current state
#[derive(Debug, Clone)]
pub struct TaskStatus {
    pub recipe: String,
    pub state: TaskState,
    pub output_preview: String,
    pub dependencies: Vec<String>,
}

/// Events sent from the executor to the UI
#[derive(Debug)]
pub enum ExecutorEvent {
    /// A task's state changed
    TaskUpdated(TaskStatus),
    /// A task has new output
    TaskOutput { recipe: String, preview: String },
    /// All tasks completed (success/failure count, total duration)
    AllDone {
        success: usize,
        failed: usize,
        total_duration: Duration,
    },
}

/// Executor that runs recipes in parallel
pub struct Executor {
    /// Maximum concurrent jobs
    max_jobs: usize,
    /// Path to justfile (if specified)
    justfile_path: Option<String>,
    /// Arguments to pass to the target recipe
    recipe_args: Vec<String>,
    /// Working directory for just commands
    working_dir: Option<String>,
}

impl Executor {
    /// Create a new executor
    pub fn new(max_jobs: usize) -> Self {
        Self {
            max_jobs,
            justfile_path: None,
            recipe_args: Vec::new(),
            working_dir: None,
        }
    }

    /// Set the justfile path
    pub fn justfile(mut self, path: Option<String>) -> Self {
        self.justfile_path = path;
        self
    }

    /// Set recipe arguments
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.recipe_args = args;
        self
    }

    /// Execute the recipe graph, sending events to the provided channel
    pub async fn execute(
        &self,
        graph: &RecipeGraph,
        target: &str,
        event_tx: mpsc::Sender<ExecutorEvent>,
    ) -> Result<bool> {
        let start_time = Instant::now();
        let recipes: Vec<String> = graph.execution_order()?;

        // Track state for each recipe
        let states: Arc<Mutex<HashMap<String, TaskState>>> = Arc::new(Mutex::new(
            recipes
                .iter()
                .map(|r| (r.clone(), TaskState::Waiting))
                .collect(),
        ));

        // Track completed recipes
        let completed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        // Track failed recipes
        let failed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        // Track running count
        let running_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

        // Track when each recipe's dependencies were all satisfied (for total time calculation)
        let ready_times: Arc<Mutex<HashMap<String, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Send initial states
        for recipe in &recipes {
            let deps = graph.dependencies_of(recipe);
            let status = TaskStatus {
                recipe: recipe.clone(),
                state: TaskState::Waiting,
                output_preview: String::new(),
                dependencies: deps,
            };
            let _ = event_tx.send(ExecutorEvent::TaskUpdated(status)).await;
        }

        // Main execution loop
        loop {
            // Check if we're done
            let (completed_count, failed_count, running) = {
                let completed = completed.lock().await;
                let failed = failed.lock().await;
                let running = *running_count.lock().await;
                (completed.len(), failed.len(), running)
            };

            // Done if all recipes finished, or if we have failures and nothing is running
            let all_finished = completed_count == recipes.len();
            let failed_and_idle = failed_count > 0 && running == 0;

            if all_finished || failed_and_idle {
                // Mark any remaining waiting tasks as skipped
                if failed_and_idle {
                    let states = states.lock().await;
                    let failed = failed.lock().await;
                    let failed_dep = failed.iter().next().cloned().unwrap_or_default();

                    for recipe in &recipes {
                        if states.get(recipe) == Some(&TaskState::Waiting) {
                            // Find which of this recipe's dependencies failed
                            let deps = graph.dependencies_of(recipe);
                            let my_failed_dep = deps
                                .iter()
                                .find(|d| failed.contains(*d))
                                .cloned()
                                .unwrap_or_else(|| failed_dep.clone());

                            let status = TaskStatus {
                                recipe: recipe.clone(),
                                state: TaskState::Skipped {
                                    failed_dep: my_failed_dep,
                                },
                                output_preview: String::new(),
                                dependencies: deps,
                            };
                            let _ = event_tx.send(ExecutorEvent::TaskUpdated(status)).await;
                        }
                    }
                }

                let _ = event_tx
                    .send(ExecutorEvent::AllDone {
                        success: completed_count - failed_count,
                        failed: failed_count,
                        total_duration: start_time.elapsed(),
                    })
                    .await;
                return Ok(failed_count == 0);
            }

            // Find tasks that can start
            let tasks_to_start = {
                let states = states.lock().await;
                let completed = completed.lock().await;
                let failed = failed.lock().await;
                let running = *running_count.lock().await;

                // If any task failed, don't start new tasks
                if !failed.is_empty() {
                    vec![]
                } else {
                    let available_slots = self.max_jobs.saturating_sub(running);
                    recipes
                        .iter()
                        .filter(|r| {
                            // Not already started
                            states.get(*r) == Some(&TaskState::Waiting)
                        })
                        .filter(|r| {
                            // All dependencies completed
                            let deps = graph.dependencies_of(r);
                            deps.iter().all(|d| completed.contains(d))
                        })
                        .take(available_slots)
                        .cloned()
                        .collect::<Vec<_>>()
                }
            };

            // Start tasks
            for recipe in tasks_to_start {
                let deps = graph.dependencies_of(&recipe);
                let has_deps = !deps.is_empty();

                // Record start_time for total duration calculation
                // Total duration = time from start of execution to task completion
                {
                    let mut ready_times = ready_times.lock().await;
                    if !ready_times.contains_key(&recipe) {
                        ready_times.insert(recipe.clone(), start_time);
                    }
                }

                // Update state to running
                {
                    let mut states = states.lock().await;
                    states.insert(recipe.clone(), TaskState::Running);
                    *running_count.lock().await += 1;
                }

                // Send update
                let status = TaskStatus {
                    recipe: recipe.clone(),
                    state: TaskState::Running,
                    output_preview: String::new(),
                    dependencies: deps.clone(),
                };
                let _ = event_tx.send(ExecutorEvent::TaskUpdated(status)).await;

                // Spawn task
                let states = Arc::clone(&states);
                let completed = Arc::clone(&completed);
                let failed = Arc::clone(&failed);
                let running_count = Arc::clone(&running_count);
                let ready_times = Arc::clone(&ready_times);
                let event_tx = event_tx.clone();
                let justfile_path = self.justfile_path.clone();
                let working_dir = self.working_dir.clone();
                let recipe_args = if recipe == target {
                    self.recipe_args.clone()
                } else {
                    vec![]
                };

                tokio::spawn(async move {
                    let task_start = Instant::now();
                    let result = run_recipe(
                        &recipe,
                        justfile_path.as_deref(),
                        working_dir.as_deref(),
                        &recipe_args,
                        event_tx.clone(),
                    )
                    .await;
                    let duration = task_start.elapsed();

                    // Calculate total duration (from when deps were satisfied)
                    let total_duration = {
                        let ready_times = ready_times.lock().await;
                        ready_times.get(&recipe).map(|t| t.elapsed())
                    };

                    // Only include total_duration if task has deps and it differs meaningfully
                    let total_duration = total_duration
                        .filter(|t| has_deps && (*t - duration) > Duration::from_millis(100));

                    // Update state
                    let new_state = match result {
                        Ok(true) => TaskState::Completed {
                            duration,
                            total_duration,
                        },
                        _ => TaskState::Failed { duration },
                    };

                    {
                        let mut states = states.lock().await;
                        states.insert(recipe.clone(), new_state.clone());
                        *running_count.lock().await -= 1;

                        let mut completed = completed.lock().await;
                        completed.insert(recipe.clone());

                        if matches!(new_state, TaskState::Failed { .. }) {
                            let mut failed = failed.lock().await;
                            failed.insert(recipe.clone());
                        }
                    }

                    // Send final update
                    let status = TaskStatus {
                        recipe: recipe.clone(),
                        state: new_state,
                        output_preview: String::new(),
                        dependencies: deps,
                    };
                    let _ = event_tx.send(ExecutorEvent::TaskUpdated(status)).await;
                });
            }

            // Small delay to avoid busy-waiting
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Run a single recipe and capture its output
async fn run_recipe(
    recipe: &str,
    justfile_path: Option<&str>,
    working_dir: Option<&str>,
    args: &[String],
    event_tx: mpsc::Sender<ExecutorEvent>,
) -> Result<bool> {
    let mut cmd = Command::new("just");

    if let Some(path) = justfile_path {
        cmd.arg("--justfile").arg(path);
    }

    cmd.arg("--no-deps");
    cmd.arg(recipe);

    for arg in args {
        cmd.arg(arg);
    }

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;

    let collector = Arc::new(Mutex::new(OutputCollector::new(recipe)));

    // Read stdout and stderr
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Read stdout in background, sending live updates
    let stdout_collector = Arc::clone(&collector);
    let stdout_recipe = recipe.to_string();
    let stdout_tx = event_tx.clone();
    let stdout_handle = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut buf = [0u8; 256];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let mut collector = stdout_collector.lock().await;
                    collector.append(&buf[..n]);
                    let preview = collector.latest_preview();
                    if !preview.is_empty() {
                        let _ = stdout_tx
                            .send(ExecutorEvent::TaskOutput {
                                recipe: stdout_recipe.clone(),
                                preview,
                            })
                            .await;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Read stderr in background, sending live updates
    let stderr_collector = Arc::clone(&collector);
    let stderr_recipe = recipe.to_string();
    let stderr_tx = event_tx.clone();
    let stderr_handle = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = [0u8; 256];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let mut collector = stderr_collector.lock().await;
                    collector.append(&buf[..n]);
                    let preview = collector.latest_preview();
                    if !preview.is_empty() {
                        let _ = stderr_tx
                            .send(ExecutorEvent::TaskOutput {
                                recipe: stderr_recipe.clone(),
                                preview,
                            })
                            .await;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for process and readers to complete
    let status = child.wait().await?;
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    // Write log file
    collector.lock().await.write_log()?;

    Ok(status.success())
}
