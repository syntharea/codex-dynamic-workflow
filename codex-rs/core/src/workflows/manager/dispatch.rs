use std::sync::Arc;
use std::time::Duration;

use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowAgentRequest;
use codex_workflows::WorkflowDispatchMarker;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskState;

use super::WorkflowManager;
use super::get_run;
use super::get_run_mut;

enum DispatchBindingDisposition {
    Defer,
    Fail(String),
}

impl WorkflowManager {
    pub(super) async fn spawn_task<A>(
        &self,
        workflow_id: WorkflowId,
        task_id: WorkflowTaskId,
        adapter: Arc<A>,
    ) -> Result<(), WorkflowError>
    where
        A: WorkflowAgentControl + 'static,
    {
        let (request, dispatch, generation) = {
            let runs = self.inner.runs.read().await;
            let run = get_run(&runs, workflow_id)?;
            let task = run
                .snapshot
                .definition
                .task(&task_id)
                .ok_or_else(|| WorkflowError::TaskNotFound(task_id.clone()))?;
            let task_attempt = run
                .snapshot
                .task_by_id(&task_id)
                .ok_or_else(|| WorkflowError::TaskNotFound(task_id.clone()))?
                .attempts;
            let dispatch = WorkflowDispatchMarker::try_new(
                workflow_id,
                task_id.clone(),
                run.snapshot.execution_attempt,
                task_attempt,
            )
            .map_err(|error| WorkflowError::InvalidSnapshot(error.to_string()))?;
            (
                WorkflowAgentRequest {
                    workflow_id,
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    instructions: task.instructions.clone(),
                    agent_role: task.agent_role.clone(),
                },
                dispatch,
                run.generation,
            )
        };
        let spawn_result = adapter.spawn_dispatch(request, dispatch).await;
        let agent_id = match spawn_result {
            Ok(agent_id) => agent_id,
            Err(error) => {
                let max_summary_bytes = self.config().max_summary_bytes;
                let result = {
                    let mut runs = self.inner.runs.write().await;
                    let run = get_run_mut(&mut runs, workflow_id)?;
                    let valid = run.generation == generation
                        && run.snapshot.state == WorkflowState::Running
                        && run
                            .snapshot
                            .task_by_id(&task_id)
                            .is_some_and(|task| task.state == WorkflowTaskState::Ready);
                    if !valid {
                        run.snapshot.defer_ready_task(&task_id)?;
                        Ok(())
                    } else if matches!(&error, WorkflowError::ExecutorCapacity(_)) {
                        run.snapshot.defer_ready_task(&task_id)?;
                        Err(error)
                    } else {
                        run.snapshot.record_task_failure_with_limit(
                            &task_id,
                            /*retryable*/ false,
                            error.to_string(),
                            max_summary_bytes,
                        )?;
                        Ok(())
                    }
                };
                return result;
            }
        };
        let dispatch_snapshot = {
            let mut runs = self.inner.runs.write().await;
            let run = get_run_mut(&mut runs, workflow_id)?;
            let valid = run.generation == generation
                && run.snapshot.state == WorkflowState::Running
                && run
                    .snapshot
                    .task_by_id(&task_id)
                    .is_some_and(|task| task.state == WorkflowTaskState::Ready);
            if valid {
                run.agents.insert(agent_id.clone(), task_id.clone());
                if let Some(task) = run
                    .snapshot
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_id == task_id)
                {
                    task.agent_id = Some(agent_id.clone());
                }
                run.capacity_backoff = Duration::from_millis(50);
                run.deferred_persisted = false;
            }
            valid.then(|| run.snapshot.clone())
        };
        let Some(dispatch_snapshot) = dispatch_snapshot else {
            let interrupt_result = adapter.interrupt(agent_id.clone()).await;
            let close_result = adapter.close(agent_id).await;
            if let Err(error) = interrupt_result {
                let _ = close_result;
                return Err(error);
            }
            close_result?;
            return Ok(());
        };
        if let Err(error) = adapter
            .persist_dispatch(agent_id.clone(), dispatch_snapshot)
            .await
        {
            let _ = self
                .discard_dispatch_binding(
                    workflow_id,
                    &task_id,
                    &agent_id,
                    DispatchBindingDisposition::Fail(error.to_string()),
                )
                .await;
            self.stop_prepared_dispatch(adapter.as_ref(), &agent_id)
                .await;
            return Err(error);
        }
        let still_valid = {
            let runs = self.inner.runs.read().await;
            let run = get_run(&runs, workflow_id)?;
            run.generation == generation
                && run.snapshot.state == WorkflowState::Running
                && run.snapshot.task_by_id(&task_id).is_some_and(|task| {
                    task.state == WorkflowTaskState::Ready
                        && task.agent_id.as_ref() == Some(&agent_id)
                })
        };
        if !still_valid {
            let discard_result = self
                .discard_dispatch_binding(
                    workflow_id,
                    &task_id,
                    &agent_id,
                    DispatchBindingDisposition::Defer,
                )
                .await;
            self.stop_prepared_dispatch(adapter.as_ref(), &agent_id)
                .await;
            discard_result?;
            return Ok(());
        }
        if let Err(error) = adapter.start_dispatch(agent_id.clone()).await {
            let _ = self
                .discard_dispatch_binding(
                    workflow_id,
                    &task_id,
                    &agent_id,
                    DispatchBindingDisposition::Fail(error.to_string()),
                )
                .await;
            self.stop_prepared_dispatch(adapter.as_ref(), &agent_id)
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn discard_dispatch_binding(
        &self,
        workflow_id: WorkflowId,
        task_id: &WorkflowTaskId,
        agent_id: &codex_workflows::WorkflowAgentId,
        disposition: DispatchBindingDisposition,
    ) -> Result<(), WorkflowError> {
        let mut runs = self.inner.runs.write().await;
        let run = get_run_mut(&mut runs, workflow_id)?;
        run.agents.remove(agent_id);
        let Some(task) = run.snapshot.task_by_id(task_id) else {
            return Err(WorkflowError::TaskNotFound(task_id.clone()));
        };
        if task.agent_id.as_ref() != Some(agent_id) {
            return Ok(());
        }
        let task_state = task.state;
        match disposition {
            DispatchBindingDisposition::Defer if task_state == WorkflowTaskState::Ready => {
                run.snapshot.defer_ready_task(task_id)
            }
            DispatchBindingDisposition::Fail(summary) if task_state == WorkflowTaskState::Ready => {
                run.snapshot.record_task_failure_with_limit(
                    task_id,
                    /*retryable*/ false,
                    summary,
                    self.config().max_summary_bytes,
                )
            }
            DispatchBindingDisposition::Defer | DispatchBindingDisposition::Fail(_) => {
                let task = run
                    .snapshot
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_id == *task_id)
                    .ok_or_else(|| WorkflowError::TaskNotFound(task_id.clone()))?;
                task.agent_id = None;
                Ok(())
            }
        }
    }

    async fn stop_prepared_dispatch<A>(
        &self,
        adapter: &A,
        agent_id: &codex_workflows::WorkflowAgentId,
    ) where
        A: WorkflowAgentControl,
    {
        let _ = adapter.interrupt(agent_id.clone()).await;
        let _ = adapter.close(agent_id.clone()).await;
    }
}
