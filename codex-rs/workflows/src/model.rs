use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use thiserror::Error;

use crate::WorkflowTaskId;

pub const WORKFLOW_SCHEMA_VERSION: u32 = 1;
pub(crate) const WORKFLOW_TITLE_MAX_BYTES: usize = 256;
pub(crate) const WORKFLOW_TEXT_MAX_BYTES: usize = 4 * 1024;
pub(crate) const WORKFLOW_OBJECTIVE_MAX_BYTES: usize = 16 * 1024;
pub(crate) const WORKFLOW_ID_MAX_BYTES: usize = 256;
pub(crate) const WORKFLOW_DEFINITION_MAX_BYTES: usize = 192 * 1024;
pub(crate) const WORKFLOW_DEFINITION_MAX_TASKS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLimits {
    pub max_tasks: usize,
    pub max_instruction_bytes: usize,
    pub max_concurrent_tasks: usize,
    pub max_summary_bytes: usize,
    pub max_task_attempts: u8,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_tasks: 32,
            max_instruction_bytes: WORKFLOW_TEXT_MAX_BYTES,
            max_concurrent_tasks: 8,
            max_summary_bytes: 4 * 1024,
            max_task_attempts: 2,
        }
    }
}

impl WorkflowLimits {
    pub(crate) fn for_deserialization() -> Self {
        Self {
            max_tasks: WORKFLOW_DEFINITION_MAX_TASKS,
            max_task_attempts: u8::MAX,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowDefinition {
    pub schema_version: u32,
    pub title: String,
    pub objective: String,
    pub tasks: Vec<WorkflowTask>,
}

impl WorkflowDefinition {
    pub fn try_new(
        title: impl Into<String>,
        tasks: Vec<WorkflowTask>,
        limits: WorkflowLimits,
    ) -> Result<Self, WorkflowValidationError> {
        let title = title.into();
        Self::try_new_with_objective(title.clone(), title, tasks, limits)
    }

    pub fn try_new_with_objective(
        title: impl Into<String>,
        objective: impl Into<String>,
        tasks: Vec<WorkflowTask>,
        limits: WorkflowLimits,
    ) -> Result<Self, WorkflowValidationError> {
        let definition = Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: title.into(),
            objective: objective.into(),
            tasks,
        };
        definition.validate(limits)?;
        Ok(definition)
    }

    pub fn task(&self, id: &WorkflowTaskId) -> Option<&WorkflowTask> {
        self.tasks.iter().find(|task| task.id == *id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTask {
    pub id: WorkflowTaskId,
    pub title: String,
    pub instructions: String,
    pub dependencies: Vec<WorkflowTaskId>,
    pub agent_role: Option<String>,
    pub intent: WorkflowTaskIntent,
    pub owned_paths: Vec<String>,
    pub verification: Vec<String>,
    pub max_attempts: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTaskIntent {
    ReadOnly,
    Write,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowDefinitionState {
    #[default]
    Placeholder,
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Planning,
    Running,
    Pausing,
    Paused,
    Consolidating,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowState {
    pub fn transition_to(self, to: Self) -> Result<Self, WorkflowTransitionError> {
        let valid = matches!(
            (self, to),
            (
                Self::Planning,
                Self::Running | Self::Failed | Self::Cancelled
            ) | (
                Self::Running,
                Self::Pausing
                    | Self::Consolidating
                    | Self::Completed
                    | Self::Failed
                    | Self::Cancelled
            ) | (Self::Pausing, Self::Paused | Self::Failed | Self::Cancelled)
                | (Self::Paused, Self::Running | Self::Failed | Self::Cancelled)
                | (
                    Self::Consolidating,
                    Self::Completed | Self::Failed | Self::Cancelled
                )
        );
        if valid {
            Ok(to)
        } else {
            Err(WorkflowTransitionError::InvalidWorkflowTransition { from: self, to })
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTaskState {
    Queued,
    Ready,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

impl WorkflowTaskState {
    pub fn transition_to(self, to: Self) -> Result<Self, WorkflowTransitionError> {
        let valid = matches!(
            (self, to),
            (Self::Queued, Self::Ready | Self::Blocked | Self::Cancelled)
                | (
                    Self::Ready,
                    Self::Running | Self::Queued | Self::Failed | Self::Cancelled
                )
                | (
                    Self::Running,
                    Self::WaitingForApproval
                        | Self::Succeeded
                        | Self::Failed
                        | Self::Queued
                        | Self::Cancelled
                )
                | (
                    Self::WaitingForApproval,
                    Self::Running | Self::Failed | Self::Queued | Self::Cancelled
                )
                | (Self::Failed, Self::Queued)
        );
        if valid {
            Ok(to)
        } else {
            Err(WorkflowTransitionError::InvalidTaskTransition { from: self, to })
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Ready | Self::Running | Self::WaitingForApproval)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Blocked | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowValidationError {
    #[error("duplicate workflow task id: {0}")]
    DuplicateTaskId(WorkflowTaskId),
    #[error("task {task_id} depends on missing task {dependency}")]
    MissingDependency {
        task_id: WorkflowTaskId,
        dependency: WorkflowTaskId,
    },
    #[error("workflow dependency graph contains a cycle")]
    DependencyCycle,
    #[error("workflow exceeds the task limit of {max_tasks}")]
    TooManyTasks { max_tasks: usize },
    #[error("task {task_id} exceeds the instruction limit of {max_bytes} bytes")]
    InstructionsTooLong {
        task_id: WorkflowTaskId,
        max_bytes: usize,
    },
    #[error("task {0} must allow at least one attempt")]
    InvalidMaxAttempts(WorkflowTaskId),
    #[error("unsupported workflow schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("{field} exceeds {max_bytes} bytes")]
    TextTooLong {
        field: &'static str,
        max_bytes: usize,
    },
    #[error("task {task_id} exceeds the attempt limit of {max_attempts}")]
    TooManyTaskAttempts {
        task_id: WorkflowTaskId,
        max_attempts: u8,
    },
    #[error("workflow definition exceeds {max_bytes} bytes")]
    DefinitionTooLarge { max_bytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowTransitionError {
    #[error("invalid workflow transition from {from:?} to {to:?}")]
    InvalidWorkflowTransition {
        from: WorkflowState,
        to: WorkflowState,
    },
    #[error("invalid task transition from {from:?} to {to:?}")]
    InvalidTaskTransition {
        from: WorkflowTaskState,
        to: WorkflowTaskState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Validation(#[from] WorkflowValidationError),
    #[error(transparent)]
    Transition(#[from] WorkflowTransitionError),
    #[error("workflow task not found: {0}")]
    TaskNotFound(WorkflowTaskId),
    #[error("workflow concurrency must be greater than zero")]
    InvalidConcurrency,
    #[error("only failed workflows can be retried")]
    RetryRequiresFailedWorkflow,
    #[error("workflow execution attempt overflow")]
    ExecutionAttemptOverflow,
    #[error("invalid workflow snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("workflow agent operation failed: {0}")]
    Agent(String),
    #[error("workflow executor capacity unavailable: {0}")]
    ExecutorCapacity(String),
    #[error("workflow executor failed: {0}")]
    ExecutorFailure(String),
    #[error("workflows are disabled for this session")]
    WorkflowsDisabled,
    #[error("workflow is already being driven")]
    AlreadyRunning,
    #[error("workflow manager configuration is locked after the first workflow starts")]
    ConfigurationLocked,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDefinitionWire {
    schema_version: u32,
    title: String,
    objective: String,
    tasks: Vec<WorkflowTask>,
}

impl<'de> Deserialize<'de> for WorkflowDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkflowDefinitionWire::deserialize(deserializer)?;
        let definition = Self {
            schema_version: wire.schema_version,
            title: wire.title,
            objective: wire.objective,
            tasks: wire.tasks,
        };
        definition
            .validate(WorkflowLimits::for_deserialization())
            .map_err(serde::de::Error::custom)?;
        Ok(definition)
    }
}
