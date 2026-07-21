use std::collections::HashSet;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

use crate::WORKFLOW_SCHEMA_VERSION;
use crate::WorkflowAgentId;
use crate::WorkflowAgentOutcome;
use crate::WorkflowAgentOutcomeStatus;
use crate::WorkflowDefinition;
use crate::WorkflowError;
use crate::WorkflowId;
use crate::WorkflowState;
use crate::WorkflowTaskId;
use crate::WorkflowTaskState;
use crate::model::WorkflowDefinitionState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowSnapshot {
    pub schema_version: u32,
    pub workflow_id: WorkflowId,
    pub definition: WorkflowDefinition,
    definition_state: WorkflowDefinitionState,
    pub state: WorkflowState,
    pub tasks: Vec<WorkflowTaskSnapshot>,
    pub resume_after_pause: bool,
    pub execution_attempt: u32,
    pub attempt_history: Vec<WorkflowAttemptSnapshot>,
}

impl WorkflowSnapshot {
    pub fn new(workflow_id: WorkflowId, definition: WorkflowDefinition) -> Self {
        let tasks = definition
            .tasks
            .iter()
            .map(|task| WorkflowTaskSnapshot {
                task_id: task.id.clone(),
                state: WorkflowTaskState::Queued,
                attempts: 0,
                agent_id: None,
                summary: None,
            })
            .collect();
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            workflow_id,
            definition,
            definition_state: WorkflowDefinitionState::Placeholder,
            state: WorkflowState::Planning,
            tasks,
            resume_after_pause: false,
            execution_attempt: 1,
            attempt_history: Vec::new(),
        }
    }

    pub fn accept_definition(&mut self) {
        self.definition_state = WorkflowDefinitionState::Accepted;
    }

    pub fn is_definition_accepted(&self) -> bool {
        self.definition_state == WorkflowDefinitionState::Accepted
    }

    pub fn task(&self, id: &str) -> Option<&WorkflowTaskSnapshot> {
        self.tasks.iter().find(|task| task.task_id.as_str() == id)
    }

    pub fn task_by_id(&self, id: &WorkflowTaskId) -> Option<&WorkflowTaskSnapshot> {
        self.tasks.iter().find(|task| task.task_id == *id)
    }

    pub fn transition_workflow(&mut self, state: WorkflowState) -> Result<(), WorkflowError> {
        self.state = self.state.transition_to(state)?;
        Ok(())
    }

    pub fn mark_persistence_failed(&mut self) {
        self.state = WorkflowState::Failed;
        self.resume_after_pause = false;
    }

    pub fn transition_task(
        &mut self,
        task_id: &WorkflowTaskId,
        state: WorkflowTaskState,
    ) -> Result<(), WorkflowError> {
        let task = self.task_mut(task_id)?;
        task.state = task.state.transition_to(state)?;
        Ok(())
    }

    pub fn record_task_failure(
        &mut self,
        task_id: &WorkflowTaskId,
        retryable: bool,
        summary: impl AsRef<str>,
    ) -> Result<(), WorkflowError> {
        self.record_task_failure_bounded(
            task_id,
            retryable,
            summary,
            crate::WorkflowLimits::default().max_summary_bytes,
        )
    }

    pub fn record_task_failure_with_limit(
        &mut self,
        task_id: &WorkflowTaskId,
        retryable: bool,
        summary: impl AsRef<str>,
        max_summary_bytes: usize,
    ) -> Result<(), WorkflowError> {
        self.record_task_failure_bounded(task_id, retryable, summary, max_summary_bytes)
    }

    pub fn defer_ready_task(&mut self, task_id: &WorkflowTaskId) -> Result<(), WorkflowError> {
        let task = self.task_mut(task_id)?;
        if task.state == WorkflowTaskState::Ready {
            task.state = WorkflowTaskState::Queued;
            task.attempts = task.attempts.saturating_sub(1);
            task.agent_id = None;
        }
        Ok(())
    }

    pub fn record_task_success(
        &mut self,
        task_id: &WorkflowTaskId,
        summary: impl AsRef<str>,
        max_summary_bytes: usize,
    ) -> Result<(), WorkflowError> {
        let task = self.task_mut(task_id)?;
        task.summary = Some(truncate_summary(summary.as_ref(), max_summary_bytes));
        task.state = task.state.transition_to(WorkflowTaskState::Succeeded)?;
        Ok(())
    }

    pub fn recover(
        &mut self,
        outcomes: &[WorkflowAgentOutcome],
        max_summary_bytes: usize,
    ) -> Result<(), WorkflowError> {
        let recovering: Vec<(WorkflowTaskId, Option<WorkflowAgentOutcomeStatus>)> = self
            .tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.state,
                    WorkflowTaskState::Ready | WorkflowTaskState::Running
                )
            })
            .map(|task| {
                let outcome = outcomes
                    .iter()
                    .find(|outcome| {
                        outcome.task_id == task.task_id
                            && task
                                .agent_id
                                .as_ref()
                                .is_none_or(|agent_id| *agent_id == outcome.agent_id)
                    })
                    .map(|outcome| outcome.status.clone());
                (task.task_id.clone(), outcome)
            })
            .collect();

        for (task_id, outcome) in recovering {
            if outcome.is_some()
                && self
                    .task_by_id(&task_id)
                    .is_some_and(|task| task.state == WorkflowTaskState::Ready)
            {
                self.transition_task(&task_id, WorkflowTaskState::Running)?;
            }
            match outcome {
                Some(WorkflowAgentOutcomeStatus::Succeeded { summary }) => {
                    self.record_task_success(&task_id, summary, max_summary_bytes)?;
                }
                Some(WorkflowAgentOutcomeStatus::Failed { summary, retryable }) => {
                    self.record_task_failure_bounded(
                        &task_id,
                        retryable,
                        summary,
                        max_summary_bytes,
                    )?;
                }
                Some(WorkflowAgentOutcomeStatus::Cancelled) => {
                    self.transition_task(&task_id, WorkflowTaskState::Cancelled)?;
                }
                None => {
                    self.transition_task(&task_id, WorkflowTaskState::Queued)?;
                    self.task_mut(&task_id)?.agent_id = None;
                }
            }
        }
        Ok(())
    }

    pub fn retry_failed_workflow(&mut self) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Failed {
            return Err(WorkflowError::RetryRequiresFailedWorkflow);
        }
        let next_attempt = self
            .execution_attempt
            .checked_add(1)
            .ok_or(WorkflowError::ExecutionAttemptOverflow)?;
        self.attempt_history.push(WorkflowAttemptSnapshot {
            execution_attempt: self.execution_attempt,
            state: self.state,
            tasks: self.tasks.clone(),
        });
        self.execution_attempt = next_attempt;
        self.state = WorkflowState::Planning;
        for task in &mut self.tasks {
            if task.state != WorkflowTaskState::Succeeded {
                task.state = WorkflowTaskState::Queued;
                task.attempts = 0;
                task.agent_id = None;
                task.summary = None;
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), WorkflowError> {
        self.definition
            .validate(crate::WorkflowLimits::for_deserialization())?;
        validate_task_snapshots(&self.definition, &self.tasks)?;
        for attempt in &self.attempt_history {
            if attempt.execution_attempt >= self.execution_attempt {
                return Err(WorkflowError::InvalidSnapshot(
                    "attempt history must precede the current execution attempt".to_string(),
                ));
            }
            validate_task_snapshots(&self.definition, &attempt.tasks)?;
        }
        if self.execution_attempt == 0 {
            return Err(WorkflowError::InvalidSnapshot(
                "execution attempt must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }

    fn record_task_failure_bounded(
        &mut self,
        task_id: &WorkflowTaskId,
        retryable: bool,
        summary: impl AsRef<str>,
        max_summary_bytes: usize,
    ) -> Result<(), WorkflowError> {
        let max_attempts = self
            .definition
            .task(task_id)
            .ok_or_else(|| WorkflowError::TaskNotFound(task_id.clone()))?
            .max_attempts;
        let task = self.task_mut(task_id)?;
        task.summary = Some(truncate_summary(summary.as_ref(), max_summary_bytes));
        let next_state = if retryable && task.attempts < max_attempts {
            WorkflowTaskState::Queued
        } else {
            WorkflowTaskState::Failed
        };
        task.state = task.state.transition_to(next_state)?;
        task.agent_id = None;
        Ok(())
    }

    pub(crate) fn task_mut(
        &mut self,
        task_id: &WorkflowTaskId,
    ) -> Result<&mut WorkflowTaskSnapshot, WorkflowError> {
        self.tasks
            .iter_mut()
            .find(|task| task.task_id == *task_id)
            .ok_or_else(|| WorkflowError::TaskNotFound(task_id.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowTaskSnapshot {
    pub task_id: WorkflowTaskId,
    pub state: WorkflowTaskState,
    pub attempts: u8,
    pub agent_id: Option<WorkflowAgentId>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAttemptSnapshot {
    pub execution_attempt: u32,
    pub state: WorkflowState,
    pub tasks: Vec<WorkflowTaskSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWorkflowDefinition {
    pub schema_version: u32,
    pub name: String,
    pub definition: WorkflowDefinition,
}

impl SavedWorkflowDefinition {
    pub fn new(name: impl Into<String>, definition: WorkflowDefinition) -> Self {
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            name: name.into(),
            definition,
        }
    }

    pub fn instantiate(&self) -> WorkflowSnapshot {
        let mut snapshot = WorkflowSnapshot::new(WorkflowId::new(), self.definition.clone());
        snapshot.accept_definition();
        snapshot
    }
}

fn truncate_summary(summary: &str, max_bytes: usize) -> String {
    if summary.len() <= max_bytes {
        return summary.to_string();
    }

    let mut boundary = max_bytes.min(summary.len());
    while !summary.is_char_boundary(boundary) {
        boundary -= 1;
    }
    summary[..boundary].to_string()
}

#[derive(Deserialize)]
struct WorkflowSnapshotWire {
    schema_version: u32,
    workflow_id: WorkflowId,
    definition: WorkflowDefinition,
    #[serde(default)]
    definition_state: WorkflowDefinitionState,
    state: WorkflowState,
    tasks: Vec<WorkflowTaskSnapshot>,
    #[serde(default)]
    resume_after_pause: bool,
    #[serde(default = "initial_execution_attempt")]
    execution_attempt: u32,
    #[serde(default)]
    attempt_history: Vec<WorkflowAttemptSnapshot>,
}

impl<'de> Deserialize<'de> for WorkflowSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkflowSnapshotWire::deserialize(deserializer)?;
        if wire.schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported workflow snapshot schema version: {}",
                wire.schema_version
            )));
        }
        let snapshot = Self {
            schema_version: wire.schema_version,
            workflow_id: wire.workflow_id,
            definition: wire.definition,
            definition_state: wire.definition_state,
            state: wire.state,
            tasks: wire.tasks,
            resume_after_pause: wire.resume_after_pause,
            execution_attempt: wire.execution_attempt,
            attempt_history: wire.attempt_history,
        };
        snapshot.validate().map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

fn initial_execution_attempt() -> u32 {
    1
}

fn validate_task_snapshots(
    definition: &WorkflowDefinition,
    tasks: &[WorkflowTaskSnapshot],
) -> Result<(), WorkflowError> {
    if tasks.len() != definition.tasks.len() {
        return Err(WorkflowError::InvalidSnapshot(
            "task snapshots must match the workflow definition".to_string(),
        ));
    }
    let mut task_ids = HashSet::with_capacity(tasks.len());
    for task in tasks {
        if !task_ids.insert(task.task_id.clone()) {
            return Err(WorkflowError::InvalidSnapshot(format!(
                "duplicate task snapshot: {}",
                task.task_id
            )));
        }
        let definition_task = definition
            .task(&task.task_id)
            .ok_or_else(|| WorkflowError::TaskNotFound(task.task_id.clone()))?;
        if task.attempts > definition_task.max_attempts {
            return Err(WorkflowError::InvalidSnapshot(format!(
                "task {} exceeds its maximum attempts",
                task.task_id
            )));
        }
    }
    Ok(())
}
