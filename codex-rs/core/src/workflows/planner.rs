use std::sync::Arc;

use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::TextElement;
use codex_protocol::user_input::UserInput;
use codex_protocol::workflow::WorkflowObjective;
use codex_protocol::workflow::WorkflowPolicy;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use tokio::sync::Notify;

use crate::agent::AgentControl;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::config::Config;

use super::WorkflowManager;
use super::prompts::planner_prompt;
use super::prompts::repair_prompt;

const PLANNER_RESPONSE_MAX_BYTES: usize = 64 * 1024;

pub(super) struct WorkflowPlanner {
    control: AgentControl,
    config: Arc<Config>,
    root_thread_id: ThreadId,
    parent_session_source: SessionSource,
}

impl WorkflowPlanner {
    pub(super) fn new(
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
        }
    }

    pub(super) async fn plan(
        &self,
        objective: &WorkflowObjective,
        policy: Option<&WorkflowPolicy>,
        manager: &WorkflowManager,
        workflow_id: WorkflowId,
        cancellation: Arc<Notify>,
    ) -> Result<WorkflowDefinition, WorkflowError> {
        let first = self
            .attempt(
                planner_prompt(objective, policy),
                manager,
                workflow_id,
                Arc::clone(&cancellation),
            )
            .await?;
        match parse_definition(&first, manager) {
            Ok(definition) => Ok(definition),
            Err(first_error) => {
                if manager.is_cancelled(workflow_id).await? {
                    return Err(WorkflowError::Agent(
                        "workflow cancelled during planning".to_string(),
                    ));
                }
                let repaired = self
                    .attempt(
                        repair_prompt(objective, policy, &first, &first_error)?,
                        manager,
                        workflow_id,
                        cancellation,
                    )
                    .await?;
                parse_definition(&repaired, manager).map_err(|second_error| {
                    WorkflowError::InvalidSnapshot(format!(
                        "planner returned invalid workflow after repair: {second_error}"
                    ))
                })
            }
        }
    }

    async fn attempt(
        &self,
        prompt: String,
        manager: &WorkflowManager,
        workflow_id: WorkflowId,
        cancellation: Arc<Notify>,
    ) -> Result<String, WorkflowError> {
        if manager.is_cancelled(workflow_id).await? {
            return Err(WorkflowError::Agent(
                "workflow cancelled during planning".to_string(),
            ));
        }
        let planner_id = self.spawn(prompt).await?;
        let result = self
            .completed_message(planner_id, manager, workflow_id, cancellation)
            .await;
        let close_result = self
            .control
            .close_agent(planner_id)
            .await
            .map(|_| ())
            .map_err(|error| WorkflowError::Agent(error.to_string()));
        self.control
            .unregister_workflow_agent(self.root_thread_id, planner_id)
            .await;
        match (result, close_result) {
            (Ok(message), Ok(())) => Ok(message),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn spawn(&self, prompt: String) -> Result<ThreadId, WorkflowError> {
        let depth = next_thread_spawn_depth(&self.parent_session_source);
        if exceeds_thread_spawn_depth_limit(depth, self.config.agent_max_depth) {
            return Err(WorkflowError::Agent(
                "agent depth limit reached".to_string(),
            ));
        }
        let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: self.root_thread_id,
            depth,
            agent_path: None,
            agent_nickname: None,
            agent_role: Some("planner".to_string()),
        });
        let agent = self
            .control
            .spawn_agent_with_metadata(
                self.config.as_ref().clone(),
                text_input(prompt),
                Some(source),
                SpawnAgentOptions {
                    parent_thread_id: Some(self.root_thread_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(map_agent_error)?;
        self.control
            .register_workflow_agent(self.root_thread_id, agent.thread_id)
            .await;
        Ok(agent.thread_id)
    }

    async fn completed_message(
        &self,
        planner_id: ThreadId,
        manager: &WorkflowManager,
        workflow_id: WorkflowId,
        cancellation: Arc<Notify>,
    ) -> Result<String, WorkflowError> {
        let mut status_rx = self
            .control
            .subscribe_status(planner_id)
            .await
            .map_err(map_agent_error)?;
        loop {
            if manager.is_cancelled(workflow_id).await? {
                self.control
                    .interrupt_agent(planner_id)
                    .await
                    .map_err(map_agent_error)?;
                return Err(WorkflowError::Agent(
                    "workflow cancelled during planning".to_string(),
                ));
            }
            let status = status_rx.borrow().clone();
            match status {
                AgentStatus::Completed(Some(message)) => return Ok(message),
                AgentStatus::Completed(None) => {
                    return Err(WorkflowError::Agent(
                        "planner completed without a final message".to_string(),
                    ));
                }
                AgentStatus::Errored(message) => return Err(WorkflowError::Agent(message)),
                AgentStatus::Interrupted | AgentStatus::Shutdown | AgentStatus::NotFound => {
                    return Err(WorkflowError::Agent(format!(
                        "planner ended with status {status:?}"
                    )));
                }
                AgentStatus::PendingInit | AgentStatus::Running => {}
            }
            tokio::select! {
                changed = status_rx.changed() => {
                    changed.map_err(|_| WorkflowError::Agent("planner status channel closed".to_string()))?;
                }
                _ = cancellation.notified() => {
                    self.control
                        .interrupt_agent(planner_id)
                        .await
                        .map_err(map_agent_error)?;
                    return Err(WorkflowError::Agent(
                        "workflow cancelled during planning".to_string(),
                    ));
                }
            }
        }
    }
}

fn parse_definition(
    payload: &str,
    manager: &WorkflowManager,
) -> Result<WorkflowDefinition, String> {
    if payload.len() > PLANNER_RESPONSE_MAX_BYTES {
        return Err(format!(
            "planner response exceeded maximum response size of {PLANNER_RESPONSE_MAX_BYTES} bytes"
        ));
    }
    let definition = serde_json::from_str::<WorkflowDefinition>(payload)
        .map_err(|error| format!("invalid workflow JSON: {error}"))?;
    definition
        .validate(manager.config().workflow_limits())
        .map_err(|error| error.to_string())?;
    Ok(definition)
}

fn text_input(text: String) -> Vec<UserInput> {
    vec![UserInput::Text {
        text,
        text_elements: Vec::<TextElement>::new(),
    }]
}

fn map_agent_error(error: CodexErr) -> WorkflowError {
    if matches!(&error, CodexErr::AgentLimitReached { .. }) {
        WorkflowError::ExecutorCapacity(error.to_string())
    } else {
        WorkflowError::Agent(error.to_string())
    }
}
