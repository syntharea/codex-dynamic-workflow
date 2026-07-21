mod dispatch;
mod drive;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::Duration;

use codex_protocol::ThreadId;
use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowScheduler;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskState;
use tokio::sync::Notify;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::recovery::WorkflowTerminalEvidence;
use super::recovery::trusted_terminal_outcomes;

#[derive(Clone)]
pub struct WorkflowManager {
    inner: Arc<WorkflowManagerInner>,
}

struct WorkflowManagerInner {
    config: StdRwLock<WorkflowManagerConfig>,
    runs: RwLock<BTreeMap<WorkflowId, WorkflowRun>>,
}

#[derive(Clone)]
struct WorkflowRun {
    root_thread_id: ThreadId,
    snapshot: WorkflowSnapshot,
    scheduler: WorkflowScheduler,
    agents: BTreeMap<codex_workflows::WorkflowAgentId, WorkflowTaskId>,
    generation: u64,
    driving: bool,
    stopping: bool,
    last_progress: Option<WorkflowProgress>,
    capacity_backoff: Duration,
    deferred_persisted: bool,
    pause_started_at: Option<Instant>,
    wake: Arc<Notify>,
    shutdown: Arc<Notify>,
    cancellation: Arc<Notify>,
}

#[derive(Clone)]
pub struct WorkflowManagerConfig {
    pub enabled: bool,
    pub max_tasks: usize,
    pub max_instruction_bytes: usize,
    pub max_concurrent_tasks: usize,
    pub wait_timeout: Duration,
    pub max_summary_bytes: usize,
    pub max_task_attempts: u8,
    pub pause_timeout: Duration,
}

impl Default for WorkflowManagerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tasks: WorkflowLimits::default().max_tasks,
            max_instruction_bytes: WorkflowLimits::default().max_instruction_bytes,
            max_concurrent_tasks: WorkflowLimits::default().max_concurrent_tasks,
            wait_timeout: Duration::from_millis(100),
            max_summary_bytes: WorkflowLimits::default().max_summary_bytes,
            max_task_attempts: WorkflowLimits::default().max_task_attempts,
            pause_timeout: Duration::from_secs(30),
        }
    }
}

impl WorkflowManagerConfig {
    pub fn from_workflow_limits(limits: WorkflowLimits) -> Self {
        Self {
            max_tasks: limits.max_tasks,
            max_instruction_bytes: limits.max_instruction_bytes,
            max_concurrent_tasks: limits.max_concurrent_tasks,
            max_summary_bytes: limits.max_summary_bytes,
            max_task_attempts: limits.max_task_attempts,
            ..Default::default()
        }
    }

    pub(crate) fn workflow_limits(&self) -> WorkflowLimits {
        WorkflowLimits {
            max_tasks: self.max_tasks,
            max_instruction_bytes: self.max_instruction_bytes,
            max_concurrent_tasks: self.max_concurrent_tasks,
            max_summary_bytes: self.max_summary_bytes,
            max_task_attempts: self.max_task_attempts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowProgress {
    pub workflow_id: WorkflowId,
    pub completed: usize,
    pub running: usize,
    pub queued: usize,
    pub blocked: usize,
    pub failed: usize,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRuntimeEvent {
    Progress {
        progress: WorkflowProgress,
        snapshot: WorkflowSnapshot,
    },
    Persist(WorkflowSnapshot),
    Paused(WorkflowSnapshot),
    Completed(WorkflowSnapshot),
    Failed {
        snapshot: WorkflowSnapshot,
        error: String,
    },
    Cancelled(WorkflowSnapshot),
}

type WorkflowEventSink =
    Arc<dyn Fn(WorkflowRuntimeEvent) -> Result<(), WorkflowError> + Send + Sync>;

pub(crate) enum WorkflowResumeTransition {
    Pending(WorkflowSnapshot),
    Drive(WorkflowSnapshot),
    Unchanged(WorkflowSnapshot),
}

impl WorkflowManager {
    pub(crate) async fn restore_snapshot(
        &self,
        root_thread_id: ThreadId,
        mut snapshot: WorkflowSnapshot,
        evidence: &[WorkflowTerminalEvidence],
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let config = self.config();
        if !config.enabled {
            return Err(WorkflowError::WorkflowsDisabled);
        }
        snapshot.definition.validate(config.workflow_limits())?;
        let outcomes = trusted_terminal_outcomes(&snapshot, evidence);
        snapshot.recover(&outcomes, config.max_summary_bytes)?;
        let workflow_id = snapshot.workflow_id;
        let run = WorkflowRun {
            root_thread_id,
            snapshot: snapshot.clone(),
            scheduler: WorkflowScheduler::new(config.max_concurrent_tasks)?,
            agents: BTreeMap::new(),
            generation: 0,
            driving: false,
            stopping: false,
            last_progress: None,
            capacity_backoff: Duration::from_millis(50),
            deferred_persisted: false,
            pause_started_at: None,
            wake: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
            cancellation: Arc::new(Notify::new()),
        };
        let mut runs = self.inner.runs.write().await;
        if runs.contains_key(&workflow_id) {
            return Err(WorkflowError::InvalidSnapshot(format!(
                "workflow {workflow_id} is already registered"
            )));
        }
        runs.insert(workflow_id, run);
        Ok(snapshot)
    }

    pub(crate) async fn begin_planning(
        &self,
        objective: String,
        root_thread_id: ThreadId,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let config = self.config();
        if !config.enabled {
            return Err(WorkflowError::WorkflowsDisabled);
        }
        let definition = WorkflowDefinition::try_new_with_objective(
            "Planning workflow",
            objective,
            Vec::new(),
            config.workflow_limits(),
        )?;
        let workflow_id = WorkflowId::new();
        let snapshot = WorkflowSnapshot::new(workflow_id, definition);
        let run = WorkflowRun {
            root_thread_id,
            snapshot: snapshot.clone(),
            scheduler: WorkflowScheduler::new(config.max_concurrent_tasks)?,
            agents: BTreeMap::new(),
            generation: 0,
            driving: false,
            stopping: false,
            last_progress: None,
            capacity_backoff: Duration::from_millis(50),
            deferred_persisted: false,
            pause_started_at: None,
            wake: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
            cancellation: Arc::new(Notify::new()),
        };
        self.inner.runs.write().await.insert(workflow_id, run);
        Ok(snapshot)
    }

    pub(crate) async fn activate(
        &self,
        workflow_id: WorkflowId,
        expected_generation: u64,
        definition: WorkflowDefinition,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let config = self.config();
        definition.validate(config.workflow_limits())?;
        let mut snapshot = WorkflowSnapshot::new(workflow_id, definition);
        snapshot.accept_definition();
        snapshot.transition_workflow(WorkflowState::Running)?;
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        if run.stopping {
            return Err(WorkflowError::AlreadyRunning);
        }
        if run.snapshot.state != WorkflowState::Planning || run.generation != expected_generation {
            return Err(WorkflowError::InvalidSnapshot(
                "workflow planning result is stale".to_string(),
            ));
        }
        run.snapshot = snapshot.clone();
        run.scheduler = WorkflowScheduler::new(config.max_concurrent_tasks)?;
        run.last_progress = None;
        run.deferred_persisted = false;
        run.pause_started_at = None;
        run.snapshot.resume_after_pause = false;
        Ok(snapshot)
    }

    pub(crate) async fn fail(
        &self,
        workflow_id: WorkflowId,
        _error: &str,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        if !run.snapshot.state.is_terminal() {
            run.snapshot.transition_workflow(WorkflowState::Failed)?;
            run.generation = run.generation.wrapping_add(1);
            run.deferred_persisted = false;
            run.wake.notify_one();
        }
        Ok(run.snapshot.clone())
    }
    pub fn new(max_concurrent_tasks: usize) -> Result<Self, WorkflowError> {
        Self::with_config(WorkflowManagerConfig {
            max_concurrent_tasks,
            ..Default::default()
        })
    }

    pub fn with_config(config: WorkflowManagerConfig) -> Result<Self, WorkflowError> {
        if config.enabled {
            WorkflowScheduler::new(config.max_concurrent_tasks)?;
        }
        Ok(Self {
            inner: Arc::new(WorkflowManagerInner {
                config: StdRwLock::new(config),
                runs: RwLock::new(BTreeMap::new()),
            }),
        })
    }

    pub fn config(&self) -> WorkflowManagerConfig {
        self.inner
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn configure(&self, config: WorkflowManagerConfig) -> Result<(), WorkflowError> {
        if config.enabled {
            WorkflowScheduler::new(config.max_concurrent_tasks)?;
        }
        let runs = self.inner.runs.write().await;
        if !runs.is_empty() {
            return Err(WorkflowError::ConfigurationLocked);
        }
        *self
            .inner
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        Ok(())
    }

    pub async fn start(
        &self,
        definition: WorkflowDefinition,
        root_thread_id: ThreadId,
    ) -> Result<WorkflowId, WorkflowError> {
        let planning = self
            .begin_planning(definition.objective.clone(), root_thread_id)
            .await?;
        let workflow_id = planning.workflow_id;
        self.activate(workflow_id, /*expected_generation*/ 0, definition)
            .await?;
        Ok(workflow_id)
    }

    pub async fn drive<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
    ) -> Result<JoinHandle<Result<(), WorkflowError>>, WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        self.drive_with_events(workflow_id, adapter, Arc::new(|_| Ok(())))
            .await
    }

    pub async fn drive_with_events<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
        events: WorkflowEventSink,
    ) -> Result<JoinHandle<Result<(), WorkflowError>>, WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        {
            let mut runs = self.inner.runs.write().await;
            let run = get_run_mut(&mut runs, workflow_id)?;
            if run.driving || run.stopping {
                return Err(WorkflowError::AlreadyRunning);
            }
            run.driving = true;
        }
        Ok(self.spawn_claimed_drive(workflow_id, adapter, events))
    }

    fn spawn_claimed_drive<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
        events: WorkflowEventSink,
    ) -> JoinHandle<Result<(), WorkflowError>>
    where
        A: WorkflowAgentControl + 'static,
    {
        let manager = self.clone();
        tokio::spawn(async move {
            let drive_result = manager
                .drive_inner(workflow_id, Arc::clone(&adapter), Arc::clone(&events))
                .await;
            let result = match drive_result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let error_message = error.to_string();
                    let mut result = Err(error);
                    match manager.fail(workflow_id, &error_message).await {
                        Ok(snapshot) if snapshot.state == WorkflowState::Failed => {
                            if let Err(event_error) = events(WorkflowRuntimeEvent::Failed {
                                snapshot,
                                error: error_message,
                            }) {
                                result = Err(event_error);
                            }
                        }
                        Ok(_) => {}
                        Err(fail_error) => result = Err(fail_error),
                    }
                    if let Err(cleanup_error) = manager
                        .stop_active_agents(workflow_id, Arc::clone(&adapter))
                        .await
                    {
                        result = Err(cleanup_error);
                    }
                    result
                }
            };
            let mut runs = manager.inner.runs.write().await;
            if let Some(run) = runs.get_mut(&workflow_id) {
                run.driving = false;
            }
            result
        })
    }

    pub async fn root_thread_id(&self, workflow_id: WorkflowId) -> Result<ThreadId, WorkflowError> {
        let runs = self.inner.runs.read().await;
        Ok(get_run(&runs, workflow_id)?.root_thread_id)
    }

    pub async fn ensure_owner(
        &self,
        workflow_id: WorkflowId,
        root_thread_id: ThreadId,
    ) -> Result<(), WorkflowError> {
        let runs = self.inner.runs.read().await;
        let run = get_run(&runs, workflow_id)?;
        if run.root_thread_id != root_thread_id {
            return Err(WorkflowError::InvalidSnapshot(format!(
                "workflow {workflow_id} belongs to another root thread"
            )));
        }
        Ok(())
    }

    pub async fn snapshots_for_owner(&self, root_thread_id: ThreadId) -> Vec<WorkflowSnapshot> {
        self.inner
            .runs
            .read()
            .await
            .values()
            .filter(|run| run.root_thread_id == root_thread_id)
            .map(|run| run.snapshot.clone())
            .collect()
    }

    pub async fn snapshot(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let runs = self.inner.runs.read().await;
        runs.get(&workflow_id)
            .map(|run| run.snapshot.clone())
            .ok_or_else(|| {
                WorkflowError::InvalidSnapshot(format!("unknown workflow {workflow_id}"))
            })
    }

    pub(crate) async fn generation(&self, workflow_id: WorkflowId) -> Result<u64, WorkflowError> {
        let runs = self.inner.runs.read().await;
        Ok(get_run(&runs, workflow_id)?.generation)
    }

    pub(crate) async fn cancellation_signal(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<Arc<Notify>, WorkflowError> {
        let runs = self.inner.runs.read().await;
        Ok(get_run(&runs, workflow_id)?.cancellation.clone())
    }

    pub(crate) async fn is_cancelled(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<bool, WorkflowError> {
        let runs = self.inner.runs.read().await;
        Ok(get_run(&runs, workflow_id)?.snapshot.state == WorkflowState::Cancelled)
    }

    pub(crate) async fn persistence_failed<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
    ) -> Result<WorkflowSnapshot, WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        let snapshot = {
            let mut runs = self.inner.runs.write().await;
            let run = get_run_mut(&mut runs, workflow_id)?;
            run.snapshot.mark_persistence_failed();
            run.generation = run.generation.wrapping_add(1);
            run.deferred_persisted = false;
            run.pause_started_at = None;
            run.stopping = true;
            run.wake.notify_one();
            run.shutdown.notify_waiters();
            run.snapshot.clone()
        };
        self.stop_active_agents(workflow_id, adapter).await?;
        let mut runs = self.inner.runs.write().await;
        if let Some(run) = runs.get_mut(&workflow_id) {
            run.stopping = false;
        }
        Ok(snapshot)
    }

    pub async fn pause(&self, workflow_id: WorkflowId) -> Result<WorkflowSnapshot, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        match run.snapshot.state {
            WorkflowState::Running => {
                run.snapshot.transition_workflow(WorkflowState::Pausing)?;
                run.generation = run.generation.wrapping_add(1);
                run.deferred_persisted = false;
                run.pause_started_at = Some(Instant::now());
                run.snapshot.resume_after_pause = false;
                run.wake.notify_one();
            }
            WorkflowState::Pausing => run.snapshot.resume_after_pause = false,
            WorkflowState::Paused => {}
            WorkflowState::Planning
            | WorkflowState::Consolidating
            | WorkflowState::Completed
            | WorkflowState::Failed
            | WorkflowState::Cancelled => {
                run.snapshot.transition_workflow(WorkflowState::Pausing)?;
            }
        }
        Ok(run.snapshot.clone())
    }

    pub(crate) async fn resume_transition(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<WorkflowResumeTransition, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        match run.snapshot.state {
            WorkflowState::Pausing => {
                run.snapshot.resume_after_pause = true;
                Ok(WorkflowResumeTransition::Pending(run.snapshot.clone()))
            }
            WorkflowState::Paused => {
                run.snapshot.transition_workflow(WorkflowState::Running)?;
                run.generation = run.generation.wrapping_add(1);
                run.deferred_persisted = false;
                run.pause_started_at = None;
                run.snapshot.resume_after_pause = false;
                run.wake.notify_one();
                Ok(WorkflowResumeTransition::Drive(run.snapshot.clone()))
            }
            WorkflowState::Running if run.driving => {
                Ok(WorkflowResumeTransition::Unchanged(run.snapshot.clone()))
            }
            WorkflowState::Running => Ok(WorkflowResumeTransition::Drive(run.snapshot.clone())),
            WorkflowState::Planning
            | WorkflowState::Consolidating
            | WorkflowState::Completed
            | WorkflowState::Failed
            | WorkflowState::Cancelled => {
                run.snapshot.transition_workflow(WorkflowState::Running)?;
                unreachable!("invalid workflow transition unexpectedly succeeded")
            }
        }
    }

    pub async fn resume<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
        events: WorkflowEventSink,
    ) -> Result<JoinHandle<Result<(), WorkflowError>>, WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        match self.resume_transition(workflow_id).await? {
            WorkflowResumeTransition::Drive(_) => {
                self.drive_with_events(workflow_id, adapter, events).await
            }
            WorkflowResumeTransition::Pending(_) | WorkflowResumeTransition::Unchanged(_) => {
                Ok(tokio::spawn(async { Ok(()) }))
            }
        }
    }

    pub async fn cancel(&self, workflow_id: WorkflowId) -> Result<WorkflowSnapshot, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        run.snapshot.transition_workflow(WorkflowState::Cancelled)?;
        run.generation = run.generation.wrapping_add(1);
        run.deferred_persisted = false;
        run.pause_started_at = None;
        run.snapshot.resume_after_pause = false;
        run.wake.notify_one();
        run.cancellation.notify_one();
        let queued: Vec<_> = run
            .snapshot
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.state,
                    WorkflowTaskState::Queued | WorkflowTaskState::Ready
                )
            })
            .map(|task| task.task_id.clone())
            .collect();
        for task_id in queued {
            run.snapshot
                .transition_task(&task_id, WorkflowTaskState::Cancelled)?;
        }
        Ok(run.snapshot.clone())
    }

    pub(crate) async fn retry_transition(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        if run.driving || run.stopping {
            return Err(WorkflowError::AlreadyRunning);
        }
        run.snapshot.retry_failed_workflow()?;
        run.snapshot.transition_workflow(WorkflowState::Running)?;
        run.generation = run.generation.wrapping_add(1);
        run.deferred_persisted = false;
        run.wake.notify_one();
        Ok(run.snapshot.clone())
    }

    pub async fn retry<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
        events: WorkflowEventSink,
    ) -> Result<JoinHandle<Result<(), WorkflowError>>, WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        self.retry_transition(workflow_id).await?;
        self.drive_with_events(workflow_id, adapter, events).await
    }
}

impl Default for WorkflowManager {
    fn default() -> Self {
        match Self::with_config(WorkflowManagerConfig::default()) {
            Ok(manager) => manager,
            Err(error) => panic!("default workflow config is invalid: {error}"),
        }
    }
}

fn get_run(
    runs: &BTreeMap<WorkflowId, WorkflowRun>,
    workflow_id: WorkflowId,
) -> Result<&WorkflowRun, WorkflowError> {
    runs.get(&workflow_id)
        .ok_or_else(|| WorkflowError::InvalidSnapshot(format!("unknown workflow {workflow_id}")))
}

fn get_run_mut(
    runs: &mut BTreeMap<WorkflowId, WorkflowRun>,
    workflow_id: WorkflowId,
) -> Result<&mut WorkflowRun, WorkflowError> {
    runs.get_mut(&workflow_id)
        .ok_or_else(|| WorkflowError::InvalidSnapshot(format!("unknown workflow {workflow_id}")))
}
