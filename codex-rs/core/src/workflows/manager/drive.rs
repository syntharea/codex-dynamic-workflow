use std::sync::Arc;
use std::time::Duration;

use codex_workflows::WorkflowAction;
use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowAgentOutcomeStatus;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskState;

use super::WorkflowEventSink;
use super::WorkflowManager;
use super::WorkflowProgress;
use super::WorkflowRun;
use super::WorkflowRuntimeEvent;
use super::get_run;
use super::get_run_mut;

impl WorkflowManager {
    pub(super) async fn drive_inner<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
        events: WorkflowEventSink,
    ) -> Result<(), WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        loop {
            let mut capacity_wait = None;
            let actions = {
                let mut runs = self.inner.runs.write().await;
                let run = get_run_mut(&mut runs, workflow_id)?;
                if run.snapshot.state == WorkflowState::Failed {
                    return Ok(());
                }
                run.scheduler.next_actions(&mut run.snapshot)
            };

            for (index, action) in actions.iter().cloned().enumerate() {
                match action {
                    WorkflowAction::Spawn(task_id) => {
                        if let Err(error) = self
                            .spawn_task(workflow_id, task_id, Arc::clone(&adapter))
                            .await
                        {
                            if matches!(&error, WorkflowError::ExecutorCapacity(_)) {
                                let remaining = actions[index + 1..]
                                    .iter()
                                    .filter_map(|action| match action {
                                        WorkflowAction::Spawn(task_id) => Some(task_id.clone()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>();
                                self.defer_ready_tasks(workflow_id, &remaining).await?;
                                let (persist, delay) = {
                                    let mut runs = self.inner.runs.write().await;
                                    let run = get_run_mut(&mut runs, workflow_id)?;
                                    let persist = (!run.deferred_persisted).then(|| {
                                        run.deferred_persisted = true;
                                        run.snapshot.clone()
                                    });
                                    let delay = run.capacity_backoff;
                                    run.capacity_backoff = (delay * 2).min(Duration::from_secs(2));
                                    (persist, delay)
                                };
                                if let Some(snapshot) = persist {
                                    events(WorkflowRuntimeEvent::Persist(snapshot))?;
                                }
                                capacity_wait = Some(delay);
                                break;
                            }
                            return Err(error);
                        }
                    }
                    WorkflowAction::Interrupt(task_id) => {
                        let agent_id = {
                            let runs = self.inner.runs.read().await;
                            runs.get(&workflow_id).and_then(|run| {
                                run.agents.iter().find_map(|(agent_id, candidate)| {
                                    (*candidate == task_id).then_some(agent_id.clone())
                                })
                            })
                        };
                        if let Some(agent_id) = agent_id {
                            if let Some(remaining) = self.pause_time_remaining(workflow_id).await? {
                                tokio::time::timeout(
                                    remaining,
                                    adapter.interrupt(agent_id.clone()),
                                )
                                .await
                                .map_err(|_| self.pause_timeout_error())??;
                            } else {
                                adapter.interrupt(agent_id.clone()).await?;
                            }
                            let cancelled = {
                                let runs = self.inner.runs.read().await;
                                get_run(&runs, workflow_id)?.snapshot.state
                                    == WorkflowState::Cancelled
                            };
                            if cancelled {
                                let closed_agent_id = agent_id.clone();
                                adapter.close(agent_id).await?;
                                let mut runs = self.inner.runs.write().await;
                                get_run_mut(&mut runs, workflow_id)?
                                    .agents
                                    .remove(&closed_agent_id);
                            }
                        }
                    }
                    WorkflowAction::Complete(state) => {
                        let snapshot = self.snapshot(workflow_id).await?;
                        events(match state {
                            WorkflowState::Completed => WorkflowRuntimeEvent::Completed(snapshot),
                            WorkflowState::Failed => WorkflowRuntimeEvent::Failed {
                                error: terminal_error(&snapshot),
                                snapshot,
                            },
                            WorkflowState::Cancelled => WorkflowRuntimeEvent::Cancelled(snapshot),
                            _ => continue,
                        })?;
                    }
                    WorkflowAction::Persist => events(WorkflowRuntimeEvent::Persist(
                        self.snapshot(workflow_id).await?,
                    ))?,
                }
            }

            if let Some((progress, snapshot)) = self.take_changed_progress(workflow_id).await? {
                events(WorkflowRuntimeEvent::Progress { progress, snapshot })?;
            }

            let active_agents = {
                let runs = self.inner.runs.read().await;
                let run = get_run(&runs, workflow_id)?;
                run.agents.keys().cloned().collect::<Vec<_>>()
            };
            if !active_agents.is_empty() {
                let shutdown = {
                    let runs = self.inner.runs.read().await;
                    get_run(&runs, workflow_id)?.shutdown.clone()
                };
                let outcomes = tokio::select! {
                    outcomes = adapter.wait(active_agents, self.config().wait_timeout) => outcomes?,
                    _ = shutdown.notified() => return Ok(()),
                };
                if outcomes.is_empty() {
                    continue;
                }
                let mut close_error = None;
                for outcome in outcomes {
                    {
                        let mut runs = self.inner.runs.write().await;
                        let run = get_run_mut(&mut runs, workflow_id)?;
                        let Some(task_id) = run.agents.remove(&outcome.agent_id) else {
                            continue;
                        };
                        let workflow_state = run.snapshot.state;
                        let task = run.snapshot.task_by_id(&task_id);
                        if task.is_some_and(|task| task.state == WorkflowTaskState::Ready) {
                            run.snapshot
                                .transition_task(&task_id, WorkflowTaskState::Running)?;
                        }
                        let outcome_status = if workflow_state == WorkflowState::Cancelled {
                            WorkflowAgentOutcomeStatus::Cancelled
                        } else {
                            outcome.status
                        };
                        match outcome_status {
                            WorkflowAgentOutcomeStatus::Succeeded { summary } => {
                                run.snapshot.record_task_success(
                                    &task_id,
                                    summary,
                                    self.config().max_summary_bytes,
                                )?
                            }
                            WorkflowAgentOutcomeStatus::Failed { summary, retryable } => {
                                run.snapshot.record_task_failure_with_limit(
                                    &task_id,
                                    retryable,
                                    summary,
                                    self.config().max_summary_bytes,
                                )?
                            }
                            WorkflowAgentOutcomeStatus::Cancelled => {
                                if workflow_state == WorkflowState::Pausing {
                                    run.snapshot
                                        .transition_task(&task_id, WorkflowTaskState::Queued)?;
                                } else if run
                                    .snapshot
                                    .task_by_id(&task_id)
                                    .is_some_and(|task| task.state != WorkflowTaskState::Cancelled)
                                {
                                    run.snapshot
                                        .transition_task(&task_id, WorkflowTaskState::Cancelled)?;
                                }
                            }
                        }
                    }
                    if let Err(error) = adapter.close(outcome.agent_id).await {
                        close_error.get_or_insert(error);
                    }
                }
                if let Some(error) = close_error {
                    return Err(error);
                }
            } else {
                if let Some(delay) = capacity_wait {
                    let wake = {
                        let runs = self.inner.runs.read().await;
                        get_run(&runs, workflow_id)?.wake.clone()
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {},
                        _ = wake.notified() => {},
                    }
                    continue;
                }
                let (paused, resumed, done) = {
                    let mut runs = self.inner.runs.write().await;
                    let run = get_run_mut(&mut runs, workflow_id)?;
                    let mut paused = None;
                    let mut resumed = None;
                    if run.snapshot.state == WorkflowState::Pausing {
                        let active_tasks: Vec<_> = run
                            .snapshot
                            .tasks
                            .iter()
                            .filter(|task| task.state.is_active())
                            .map(|task| task.task_id.clone())
                            .collect();
                        for task_id in active_tasks {
                            run.snapshot
                                .transition_task(&task_id, WorkflowTaskState::Queued)?;
                        }
                        run.snapshot.transition_workflow(WorkflowState::Paused)?;
                        paused = Some(run.snapshot.clone());
                        run.pause_started_at = None;
                        if run.snapshot.resume_after_pause {
                            run.snapshot.transition_workflow(WorkflowState::Running)?;
                            run.generation = run.generation.wrapping_add(1);
                            run.deferred_persisted = false;
                            run.snapshot.resume_after_pause = false;
                            let progress = progress_for_run(run, self.config().max_summary_bytes);
                            run.last_progress = Some(progress.clone());
                            resumed = Some((progress, run.snapshot.clone()));
                        }
                    }
                    let done = run.snapshot.state.is_terminal()
                        || run.snapshot.state == WorkflowState::Paused
                        || (run.snapshot.state == WorkflowState::Running
                            && run
                                .snapshot
                                .tasks
                                .iter()
                                .all(|task| task.state.is_terminal()));
                    (paused, resumed, done)
                };
                if let Some(snapshot) = paused {
                    events(WorkflowRuntimeEvent::Paused(snapshot))?;
                }
                if let Some((progress, snapshot)) = resumed {
                    events(WorkflowRuntimeEvent::Progress { progress, snapshot })?;
                }
                if done {
                    return Ok(());
                }
            }
        }
    }

    async fn defer_ready_tasks(
        &self,
        workflow_id: WorkflowId,
        task_ids: &[WorkflowTaskId],
    ) -> Result<(), WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        for task_id in task_ids {
            run.snapshot.defer_ready_task(task_id)?;
        }
        Ok(())
    }

    async fn take_changed_progress(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<Option<(WorkflowProgress, WorkflowSnapshot)>, WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        let current = progress_for_run(run, self.config().max_summary_bytes);
        if run.last_progress.as_ref() == Some(&current) {
            return Ok(None);
        }
        run.last_progress = Some(current.clone());
        Ok(Some((current, run.snapshot.clone())))
    }

    async fn pause_time_remaining(
        &self,
        workflow_id: WorkflowId,
    ) -> Result<Option<Duration>, WorkflowError> {
        let runs = self.inner.runs.read().await;
        let run = get_run(&runs, workflow_id)?;
        if run.snapshot.state != WorkflowState::Pausing {
            return Ok(None);
        }
        let Some(started_at) = run.pause_started_at else {
            return Err(WorkflowError::Agent(
                "workflow pause deadline is unavailable".to_string(),
            ));
        };
        let pause_timeout = self.config().pause_timeout;
        pause_timeout
            .checked_sub(started_at.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .map(Some)
            .ok_or_else(|| self.pause_timeout_error())
    }

    fn pause_timeout_error(&self) -> WorkflowError {
        WorkflowError::Agent(format!(
            "workflow pause timed out after {:?}",
            self.config().pause_timeout
        ))
    }

    pub(super) async fn stop_active_agents<A>(
        &self,
        workflow_id: WorkflowId,
        adapter: Arc<A>,
    ) -> Result<(), WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        let agents = {
            let mut runs = self.inner.runs.write().await;
            let run = get_run_mut(&mut runs, workflow_id)?;
            std::mem::take(&mut run.agents)
                .into_keys()
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for agent_id in agents {
            if let Err(error) = adapter.interrupt(agent_id.clone()).await {
                first_error.get_or_insert(error);
            }
            if let Err(error) = adapter.close(agent_id).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn progress_for_run(run: &WorkflowRun, max_summary_bytes: usize) -> WorkflowProgress {
    let completed = run
        .snapshot
        .tasks
        .iter()
        .filter(|task| task.state == WorkflowTaskState::Succeeded)
        .count();
    let running = run
        .snapshot
        .tasks
        .iter()
        .filter(|task| task.state.is_active())
        .count();
    let queued = run
        .snapshot
        .tasks
        .iter()
        .filter(|task| task.state == WorkflowTaskState::Queued)
        .count();
    let blocked = run
        .snapshot
        .tasks
        .iter()
        .filter(|task| task.state == WorkflowTaskState::Blocked)
        .count();
    let failed = run
        .snapshot
        .tasks
        .iter()
        .filter(|task| task.state == WorkflowTaskState::Failed)
        .count();
    let summary = run
        .snapshot
        .tasks
        .iter()
        .filter_map(|task| task.summary.as_deref())
        .next()
        .map(|summary| truncate(summary, max_summary_bytes))
        .unwrap_or_default();
    WorkflowProgress {
        workflow_id: run.snapshot.workflow_id,
        completed,
        running,
        queued,
        blocked,
        failed,
        summary,
    }
}

fn terminal_error(snapshot: &WorkflowSnapshot) -> String {
    snapshot
        .tasks
        .iter()
        .filter_map(|task| task.summary.clone())
        .next()
        .unwrap_or_else(|| "workflow failed".to_string())
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
