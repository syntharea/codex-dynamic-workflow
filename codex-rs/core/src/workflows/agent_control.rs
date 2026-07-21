use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::AgentControl;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::config::Config;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::TextElement;
use codex_protocol::user_input::UserInput;
use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowAgentOutcome;
use codex_workflows::WorkflowAgentOutcomeStatus;
use codex_workflows::WorkflowAgentRequest;
use codex_workflows::WorkflowDispatchMarker;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowTaskId;

enum WorkflowSpawnMode {
    Immediate,
    Dispatch(WorkflowDispatchMarker),
}

#[derive(Clone)]
struct PendingWorkflowDispatch {
    initial_input: Vec<UserInput>,
    task_id: WorkflowTaskId,
}

#[derive(Clone)]
pub struct WorkflowAgentControlAdapter {
    control: AgentControl,
    config: Arc<Config>,
    root_thread_id: ThreadId,
    parent_session_source: SessionSource,
    task_by_agent: Arc<tokio::sync::Mutex<BTreeMap<WorkflowAgentId, WorkflowTaskId>>>,
    pending_dispatches: Arc<tokio::sync::Mutex<BTreeMap<WorkflowAgentId, PendingWorkflowDispatch>>>,
}

impl WorkflowAgentControlAdapter {
    pub(crate) fn new(
        control: AgentControl,
        config: Arc<Config>,
        root_thread_id: ThreadId,
        parent_session_source: SessionSource,
    ) -> Self {
        Self {
            control,
            config,
            root_thread_id,
            parent_session_source,
            task_by_agent: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            pending_dispatches: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    async fn thread_id(&self, agent: &WorkflowAgentId) -> Result<ThreadId, WorkflowError> {
        ThreadId::try_from(agent.as_str())
            .map_err(|err| WorkflowError::Agent(format!("invalid workflow agent id: {err}")))
    }

    async fn wait_one(
        &self,
        agent_id: WorkflowAgentId,
        task_id: WorkflowTaskId,
        thread_id: ThreadId,
        deadline: tokio::time::Instant,
    ) -> Option<WorkflowAgentOutcome> {
        self.wait_for_status(thread_id, deadline)
            .await
            .map(|status| WorkflowAgentOutcome {
                agent_id,
                task_id,
                status: map_status(status),
            })
    }

    async fn spawn_request(
        &self,
        request: WorkflowAgentRequest,
        mode: WorkflowSpawnMode,
    ) -> Result<WorkflowAgentId, WorkflowError> {
        let depth = next_thread_spawn_depth(&self.parent_session_source);
        if exceeds_thread_spawn_depth_limit(depth, self.config.agent_max_depth) {
            return Err(WorkflowError::Agent(
                "agent depth limit reached".to_string(),
            ));
        }
        let WorkflowAgentRequest {
            task_id,
            instructions,
            agent_role,
            ..
        } = request;
        let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: self.root_thread_id,
            depth,
            agent_path: None,
            agent_nickname: None,
            agent_role,
        });
        let initial_input = vec![UserInput::Text {
            text: instructions,
            text_elements: Vec::<TextElement>::new(),
        }];
        let options = SpawnAgentOptions {
            parent_thread_id: Some(self.root_thread_id),
            ..Default::default()
        };
        match mode {
            WorkflowSpawnMode::Immediate => {
                let agent = self
                    .control
                    .spawn_agent_with_metadata(
                        self.config.as_ref().clone(),
                        initial_input,
                        Some(source),
                        options,
                    )
                    .await
                    .map_err(map_spawn_error)?;
                let agent_id = WorkflowAgentId::from(agent.thread_id.to_string());
                self.register_agent(agent_id.clone(), agent.thread_id, task_id)
                    .await;
                Ok(agent_id)
            }
            WorkflowSpawnMode::Dispatch(dispatch) => {
                let agent = self
                    .control
                    .prepare_workflow_agent_with_metadata(
                        self.config.as_ref().clone(),
                        Some(source),
                        options,
                        dispatch,
                    )
                    .await
                    .map_err(map_spawn_error)?;
                let agent_id = WorkflowAgentId::from(agent.thread_id.to_string());
                let pending = PendingWorkflowDispatch {
                    initial_input,
                    task_id,
                };
                let duplicate = {
                    let mut pending_dispatches = self.pending_dispatches.lock().await;
                    if pending_dispatches.contains_key(&agent_id) {
                        true
                    } else {
                        pending_dispatches.insert(agent_id.clone(), pending);
                        false
                    }
                };
                if duplicate {
                    let _ = self.control.close_agent(agent.thread_id).await;
                    return Err(WorkflowError::ExecutorFailure(format!(
                        "duplicate prepared workflow agent id {agent_id}"
                    )));
                }
                Ok(agent_id)
            }
        }
    }

    async fn register_agent(
        &self,
        agent_id: WorkflowAgentId,
        thread_id: ThreadId,
        task_id: WorkflowTaskId,
    ) {
        self.control
            .register_workflow_agent(self.root_thread_id, thread_id)
            .await;
        self.task_by_agent.lock().await.insert(agent_id, task_id);
    }
}

impl WorkflowAgentControl for WorkflowAgentControlAdapter {
    async fn spawn(&self, request: WorkflowAgentRequest) -> Result<WorkflowAgentId, WorkflowError> {
        self.spawn_request(request, WorkflowSpawnMode::Immediate)
            .await
    }

    async fn spawn_dispatch(
        &self,
        request: WorkflowAgentRequest,
        dispatch: WorkflowDispatchMarker,
    ) -> Result<WorkflowAgentId, WorkflowError> {
        self.spawn_request(request, WorkflowSpawnMode::Dispatch(dispatch))
            .await
    }

    async fn persist_dispatch(
        &self,
        agent: WorkflowAgentId,
        snapshot: WorkflowSnapshot,
    ) -> Result<(), WorkflowError> {
        if !self.pending_dispatches.lock().await.contains_key(&agent) {
            return Err(WorkflowError::ExecutorFailure(format!(
                "workflow agent {agent} has no prepared dispatch"
            )));
        }
        self.control
            .persist_workflow_dispatch_snapshot(self.root_thread_id, &snapshot)
            .await
            .map_err(|error| WorkflowError::ExecutorFailure(error.to_string()))
    }

    async fn start_dispatch(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        let thread_id = self.thread_id(&agent).await?;
        let pending = self
            .pending_dispatches
            .lock()
            .await
            .remove(&agent)
            .ok_or_else(|| {
                WorkflowError::ExecutorFailure(format!(
                    "workflow agent {agent} has no prepared dispatch"
                ))
            })?;
        self.register_agent(agent.clone(), thread_id, pending.task_id)
            .await;
        if let Err(error) = self
            .control
            .start_prepared_workflow_agent(thread_id, pending.initial_input)
            .await
        {
            self.control
                .unregister_workflow_agent(self.root_thread_id, thread_id)
                .await;
            self.task_by_agent.lock().await.remove(&agent);
            return Err(WorkflowError::ExecutorFailure(error.to_string()));
        }
        Ok(())
    }

    async fn wait(
        &self,
        agents: Vec<WorkflowAgentId>,
        timeout: Duration,
    ) -> Result<Vec<WorkflowAgentOutcome>, WorkflowError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut waits = futures::stream::FuturesUnordered::new();
        for agent_id in agents {
            let Ok(thread_id) = self.thread_id(&agent_id).await else {
                continue;
            };
            let task_id = self.task_by_agent.lock().await.get(&agent_id).cloned();
            let Some(task_id) = task_id else { continue };
            waits.push(self.wait_one(agent_id, task_id, thread_id, deadline));
        }
        let mut outcomes = Vec::new();
        while let Ok(Some(outcome)) =
            tokio::time::timeout_at(deadline, futures::StreamExt::next(&mut waits)).await
        {
            if let Some(outcome) = outcome {
                outcomes.push(outcome);
            }
        }
        Ok(outcomes)
    }

    async fn interrupt(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        let thread_id = self.thread_id(&agent).await?;
        self.control
            .interrupt_agent(thread_id)
            .await
            .map(|_| ())
            .map_err(|err| WorkflowError::Agent(err.to_string()))
    }

    async fn close(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        let thread_id = self.thread_id(&agent).await?;
        let result = self
            .control
            .close_agent(thread_id)
            .await
            .map(|_| ())
            .map_err(|err| WorkflowError::Agent(err.to_string()));
        self.control
            .unregister_workflow_agent(self.root_thread_id, thread_id)
            .await;
        self.task_by_agent.lock().await.remove(&agent);
        self.pending_dispatches.lock().await.remove(&agent);
        result
    }
}

fn map_spawn_error(error: CodexErr) -> WorkflowError {
    if matches!(&error, CodexErr::AgentLimitReached { .. }) {
        WorkflowError::ExecutorCapacity(error.to_string())
    } else {
        WorkflowError::ExecutorFailure(error.to_string())
    }
}

impl WorkflowAgentControlAdapter {
    async fn wait_for_status(
        &self,
        thread_id: ThreadId,
        deadline: tokio::time::Instant,
    ) -> Option<AgentStatus> {
        let mut status_rx = self.control.subscribe_status(thread_id).await.ok()?;
        let initial = status_rx.borrow().clone();
        if is_workflow_terminal(&initial) {
            return Some(initial);
        }
        loop {
            tokio::select! {
                result = status_rx.changed() => {
                    if result.is_err() {
                        let status = self.control.get_status(thread_id).await;
                        return is_workflow_terminal(&status).then_some(status);
                    }
                    let status = status_rx.borrow().clone();
                    if is_workflow_terminal(&status) {
                        return Some(status);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }
}

fn is_workflow_terminal(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Completed(_)
            | AgentStatus::Errored(_)
            | AgentStatus::Interrupted
            | AgentStatus::Shutdown
            | AgentStatus::NotFound
    )
}

fn map_status(status: AgentStatus) -> WorkflowAgentOutcomeStatus {
    match status {
        AgentStatus::Completed(summary) => WorkflowAgentOutcomeStatus::Succeeded {
            summary: summary.unwrap_or_default(),
        },
        AgentStatus::Errored(summary) => WorkflowAgentOutcomeStatus::Failed {
            summary,
            retryable: true,
        },
        AgentStatus::Interrupted | AgentStatus::Shutdown => WorkflowAgentOutcomeStatus::Cancelled,
        AgentStatus::NotFound => WorkflowAgentOutcomeStatus::Failed {
            summary: "agent not found".to_string(),
            retryable: false,
        },
        AgentStatus::PendingInit | AgentStatus::Running => unreachable!("non-final agent status"),
    }
}

#[cfg(test)]
#[path = "agent_control_tests.rs"]
mod tests;
