use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde_json::Value;
use ts_rs::TS;

pub const WORKFLOW_SCHEMA_VERSION: u32 = 1;
pub const WORKFLOW_POLICY_SCHEMA_VERSION: u32 = 1;
pub const WORKFLOW_TEXT_MAX_BYTES: usize = 4 * 1024;
pub const WORKFLOW_TITLE_MAX_BYTES: usize = 256;
pub const WORKFLOW_ID_MAX_BYTES: usize = 256;
pub const WORKFLOW_TASK_ID_MAX_BYTES: usize = 256;
pub const WORKFLOW_NAME_MAX_BYTES: usize = 256;
pub const WORKFLOW_OBJECTIVE_MAX_BYTES: usize = 16 * 1024;
pub const WORKFLOW_SNAPSHOT_MAX_BYTES: usize = 256 * 1024;
pub const WORKFLOW_MAX_TASKS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowWireError {
    TextTooLong {
        field: &'static str,
        max_bytes: usize,
    },
    UnsupportedSchemaVersion(u32),
    SnapshotTooLarge {
        max_bytes: usize,
    },
    TaskCountTooLarge {
        max_tasks: usize,
    },
    MismatchedWorkflowId,
    InvalidSnapshot,
}

impl fmt::Display for WorkflowWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextTooLong { field, max_bytes } => {
                write!(formatter, "{field} exceeds {max_bytes} bytes")
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(formatter, "unsupported workflow schema version {version}")
            }
            Self::SnapshotTooLarge { max_bytes } => {
                write!(formatter, "workflow snapshot exceeds {max_bytes} bytes")
            }
            Self::TaskCountTooLarge { max_tasks } => {
                write!(formatter, "workflow task count exceeds {max_tasks}")
            }
            Self::MismatchedWorkflowId => {
                formatter.write_str("workflow event and summary have different workflow ids")
            }
            Self::InvalidSnapshot => formatter.write_str("workflow snapshot is not valid JSON"),
        }
    }
}

impl std::error::Error for WorkflowWireError {}

macro_rules! bounded_text {
    ($name:ident, $max:expr, $field:literal) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WorkflowWireError> {
                Self::try_new(value)
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, WorkflowWireError> {
                let value = value.into();
                if value.len() > $max {
                    return Err(WorkflowWireError::TextTooLong {
                        field: $field,
                        max_bytes: $max,
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

bounded_text!(WorkflowText, WORKFLOW_TEXT_MAX_BYTES, "workflow text");
bounded_text!(WorkflowTitle, WORKFLOW_TITLE_MAX_BYTES, "workflow title");
bounded_text!(WorkflowId, WORKFLOW_ID_MAX_BYTES, "workflow id");
bounded_text!(WorkflowName, WORKFLOW_NAME_MAX_BYTES, "workflow name");
bounded_text!(
    WorkflowObjective,
    WORKFLOW_OBJECTIVE_MAX_BYTES,
    "workflow objective"
);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Planning,
    Running,
    Pausing,
    Paused,
    Consolidating,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum WorkflowTaskStatus {
    Queued,
    Ready,
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowTaskSummary {
    task_id: String,
    #[schemars(with = "String")]
    #[ts(type = "string")]
    title: WorkflowTitle,
    status: WorkflowTaskStatus,
    #[serde(default)]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    summary: WorkflowText,
    #[serde(default)]
    attempt: u32,
}

impl WorkflowTaskSummary {
    pub fn try_new(
        task_id: impl Into<String>,
        title: WorkflowTitle,
        status: WorkflowTaskStatus,
        summary: WorkflowText,
        attempt: u32,
    ) -> Result<Self, WorkflowWireError> {
        let task_id = task_id.into();
        validate_bounded_string(&task_id, WORKFLOW_TASK_ID_MAX_BYTES, "workflow task id")?;
        Ok(Self {
            task_id,
            title,
            status,
            summary,
            attempt,
        })
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn title(&self) -> &WorkflowTitle {
        &self.title
    }

    pub fn status(&self) -> WorkflowTaskStatus {
        self.status
    }

    pub fn summary(&self) -> &WorkflowText {
        &self.summary
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

impl<'de> Deserialize<'de> for WorkflowTaskSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            task_id: String,
            title: WorkflowTitle,
            status: WorkflowTaskStatus,
            #[serde(default)]
            summary: WorkflowText,
            #[serde(default)]
            attempt: u32,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::try_new(
            wire.task_id,
            wire.title,
            wire.status,
            wire.summary,
            wire.attempt,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowSummary {
    workflow_id: WorkflowId,
    #[schemars(with = "String")]
    #[ts(type = "string")]
    title: WorkflowTitle,
    status: WorkflowStatus,
    #[serde(default)]
    tasks: Vec<WorkflowTaskSummary>,
    completed_tasks: usize,
    running_tasks: usize,
    queued_tasks: usize,
    blocked_tasks: usize,
    failed_tasks: usize,
    #[serde(default)]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    summary: WorkflowText,
}

impl WorkflowSummary {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        workflow_id: WorkflowId,
        title: WorkflowTitle,
        status: WorkflowStatus,
        tasks: Vec<WorkflowTaskSummary>,
        completed_tasks: usize,
        running_tasks: usize,
        queued_tasks: usize,
        blocked_tasks: usize,
        failed_tasks: usize,
        summary: WorkflowText,
    ) -> Result<Self, WorkflowWireError> {
        if tasks.len() > WORKFLOW_MAX_TASKS {
            return Err(WorkflowWireError::TaskCountTooLarge {
                max_tasks: WORKFLOW_MAX_TASKS,
            });
        }
        Ok(Self {
            workflow_id,
            title,
            status,
            tasks,
            completed_tasks,
            running_tasks,
            queued_tasks,
            blocked_tasks,
            failed_tasks,
            summary,
        })
    }

    pub fn workflow_id(&self) -> &str {
        self.workflow_id.as_str()
    }

    pub fn title(&self) -> &WorkflowTitle {
        &self.title
    }

    pub fn status(&self) -> WorkflowStatus {
        self.status
    }

    pub fn tasks(&self) -> &[WorkflowTaskSummary] {
        &self.tasks
    }

    pub fn completed_tasks(&self) -> usize {
        self.completed_tasks
    }

    pub fn running_tasks(&self) -> usize {
        self.running_tasks
    }

    pub fn queued_tasks(&self) -> usize {
        self.queued_tasks
    }

    pub fn blocked_tasks(&self) -> usize {
        self.blocked_tasks
    }

    pub fn failed_tasks(&self) -> usize {
        self.failed_tasks
    }

    pub fn summary(&self) -> &WorkflowText {
        &self.summary
    }
}

impl<'de> Deserialize<'de> for WorkflowSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            workflow_id: WorkflowId,
            title: WorkflowTitle,
            status: WorkflowStatus,
            #[serde(default)]
            tasks: Vec<WorkflowTaskSummary>,
            completed_tasks: usize,
            running_tasks: usize,
            queued_tasks: usize,
            blocked_tasks: usize,
            failed_tasks: usize,
            #[serde(default)]
            summary: WorkflowText,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::try_new(
            wire.workflow_id,
            wire.title,
            wire.status,
            wire.tasks,
            wire.completed_tasks,
            wire.running_tasks,
            wire.queued_tasks,
            wire.blocked_tasks,
            wire.failed_tasks,
            wire.summary,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowPolicy {
    Ultracode,
    DeepResearch,
}

#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowTaskOverride {
    task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    #[ts(type = "string | null")]
    title: Option<WorkflowTitle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    #[ts(type = "string | null")]
    instructions: Option<WorkflowText>,
}

impl WorkflowTaskOverride {
    pub fn try_new(
        task_id: impl Into<String>,
        title: Option<WorkflowTitle>,
        instructions: Option<WorkflowText>,
    ) -> Result<Self, WorkflowWireError> {
        let task_id = task_id.into();
        validate_bounded_string(&task_id, WORKFLOW_TASK_ID_MAX_BYTES, "workflow task id")?;
        Ok(Self {
            task_id,
            title,
            instructions,
        })
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn title(&self) -> Option<&WorkflowTitle> {
        self.title.as_ref()
    }

    pub fn instructions(&self) -> Option<&WorkflowText> {
        self.instructions.as_ref()
    }
}

impl<'de> Deserialize<'de> for WorkflowTaskOverride {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            task_id: String,
            #[serde(default)]
            title: Option<WorkflowTitle>,
            #[serde(default)]
            instructions: Option<WorkflowText>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::try_new(wire.task_id, wire.title, wire.instructions).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, JsonSchema, TS)]
#[serde(transparent)]
pub struct WorkflowTaskOverrides(Vec<WorkflowTaskOverride>);

impl WorkflowTaskOverrides {
    pub fn try_new(values: Vec<WorkflowTaskOverride>) -> Result<Self, WorkflowWireError> {
        if values.len() > WORKFLOW_MAX_TASKS {
            return Err(WorkflowWireError::TaskCountTooLarge {
                max_tasks: WORKFLOW_MAX_TASKS,
            });
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[WorkflowTaskOverride] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for WorkflowTaskOverrides {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = Vec::<WorkflowTaskOverride>::deserialize(deserializer)?;
        Self::try_new(values).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowStartParams {
    pub objective: WorkflowObjective,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<WorkflowPolicy>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowReadParams {
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct WorkflowListParams;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowPauseParams {
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowResumeParams {
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowCancelParams {
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowRetryParams {
    pub workflow_id: WorkflowId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowSaveParams {
    pub workflow_id: WorkflowId,
    pub name: WorkflowName,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowRunSavedParams {
    pub name: WorkflowName,
    #[serde(default)]
    pub task_overrides: WorkflowTaskOverrides,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowDeleteSavedParams {
    pub name: WorkflowName,
}

#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowProgressEvent {
    workflow_id: WorkflowId,
    state: WorkflowStatus,
    completed_tasks: usize,
    running_tasks: usize,
    queued_tasks: usize,
    blocked_tasks: usize,
    failed_tasks: usize,
    #[serde(default)]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    summary: WorkflowText,
}

impl WorkflowProgressEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_id: WorkflowId,
        state: WorkflowStatus,
        completed_tasks: usize,
        running_tasks: usize,
        queued_tasks: usize,
        blocked_tasks: usize,
        failed_tasks: usize,
        summary: WorkflowText,
    ) -> Self {
        Self {
            workflow_id,
            state,
            completed_tasks,
            running_tasks,
            queued_tasks,
            blocked_tasks,
            failed_tasks,
            summary,
        }
    }

    pub fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    pub fn state(&self) -> WorkflowStatus {
        self.state
    }

    pub fn summary(&self) -> &WorkflowText {
        &self.summary
    }
}

impl<'de> Deserialize<'de> for WorkflowProgressEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            workflow_id: WorkflowId,
            state: WorkflowStatus,
            completed_tasks: usize,
            running_tasks: usize,
            queued_tasks: usize,
            blocked_tasks: usize,
            failed_tasks: usize,
            #[serde(default)]
            summary: WorkflowText,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.workflow_id,
            wire.state,
            wire.completed_tasks,
            wire.running_tasks,
            wire.queued_tasks,
            wire.blocked_tasks,
            wire.failed_tasks,
            wire.summary,
        ))
    }
}

impl WorkflowProgressEvent {
    pub fn completed_tasks(&self) -> usize {
        self.completed_tasks
    }

    pub fn running_tasks(&self) -> usize {
        self.running_tasks
    }

    pub fn queued_tasks(&self) -> usize {
        self.queued_tasks
    }

    pub fn blocked_tasks(&self) -> usize {
        self.blocked_tasks
    }

    pub fn failed_tasks(&self) -> usize {
        self.failed_tasks
    }
}

macro_rules! lifecycle_event {
    ($name:ident) => {
        #[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
        #[serde(rename_all = "snake_case")]
        pub struct $name {
            workflow_id: WorkflowId,
            summary: WorkflowSummary,
        }

        impl $name {
            pub fn try_new(
                workflow_id: WorkflowId,
                summary: WorkflowSummary,
            ) -> Result<Self, WorkflowWireError> {
                if workflow_id.as_str() != summary.workflow_id() {
                    return Err(WorkflowWireError::MismatchedWorkflowId);
                }
                Ok(Self {
                    workflow_id,
                    summary,
                })
            }

            pub fn workflow_id(&self) -> &WorkflowId {
                &self.workflow_id
            }

            pub fn summary(&self) -> &WorkflowSummary {
                &self.summary
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                #[serde(rename_all = "snake_case")]
                struct Wire {
                    workflow_id: WorkflowId,
                    summary: WorkflowSummary,
                }

                let wire = Wire::deserialize(deserializer)?;
                Self::try_new(wire.workflow_id, wire.summary).map_err(serde::de::Error::custom)
            }
        }
    };
}

lifecycle_event!(WorkflowStartedEvent);
lifecycle_event!(WorkflowPausedEvent);
lifecycle_event!(WorkflowCompletedEvent);
lifecycle_event!(WorkflowCancelledEvent);

#[derive(Debug, Clone, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowFailedEvent {
    workflow_id: WorkflowId,
    summary: WorkflowSummary,
    #[serde(default)]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    error: WorkflowText,
}

impl WorkflowFailedEvent {
    pub fn try_new(
        workflow_id: WorkflowId,
        summary: WorkflowSummary,
        error: WorkflowText,
    ) -> Result<Self, WorkflowWireError> {
        if workflow_id.as_str() != summary.workflow_id() {
            return Err(WorkflowWireError::MismatchedWorkflowId);
        }
        Ok(Self {
            workflow_id,
            summary,
            error,
        })
    }

    pub fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    pub fn summary(&self) -> &WorkflowSummary {
        &self.summary
    }

    pub fn error(&self) -> &WorkflowText {
        &self.error
    }
}

impl<'de> Deserialize<'de> for WorkflowFailedEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            workflow_id: WorkflowId,
            summary: WorkflowSummary,
            #[serde(default)]
            error: WorkflowText,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::try_new(wire.workflow_id, wire.summary, wire.error).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowStateItem {
    schema_version: u32,
    workflow_id: WorkflowId,
    snapshot: Value,
}

impl WorkflowStateItem {
    pub fn new(workflow_id: impl Into<String>, snapshot: Value) -> Result<Self, WorkflowWireError> {
        Self::try_new(WORKFLOW_SCHEMA_VERSION, workflow_id, snapshot)
    }

    pub fn try_new(
        schema_version: u32,
        workflow_id: impl Into<String>,
        snapshot: Value,
    ) -> Result<Self, WorkflowWireError> {
        validate_workflow_snapshot(schema_version, &snapshot)?;
        let workflow_id = WorkflowId::try_new(workflow_id.into())?;
        Ok(Self {
            schema_version,
            workflow_id,
            snapshot,
        })
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn workflow_id(&self) -> &str {
        self.workflow_id.as_str()
    }

    pub fn snapshot(&self) -> &Value {
        &self.snapshot
    }
}

impl<'de> Deserialize<'de> for WorkflowStateItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            schema_version: u32,
            workflow_id: WorkflowId,
            snapshot: Value,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::try_new(
            wire.schema_version,
            wire.workflow_id.into_inner(),
            wire.snapshot,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn validate_workflow_snapshot(
    schema_version: u32,
    snapshot: &Value,
) -> Result<(), WorkflowWireError> {
    if schema_version != WORKFLOW_SCHEMA_VERSION {
        return Err(WorkflowWireError::UnsupportedSchemaVersion(schema_version));
    }
    let bytes = serde_json::to_vec(snapshot).map_err(|_| WorkflowWireError::InvalidSnapshot)?;
    if bytes.len() > WORKFLOW_SNAPSHOT_MAX_BYTES {
        return Err(WorkflowWireError::SnapshotTooLarge {
            max_bytes: WORKFLOW_SNAPSHOT_MAX_BYTES,
        });
    }
    Ok(())
}

fn validate_bounded_string(
    value: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), WorkflowWireError> {
    if value.len() > max_bytes {
        return Err(WorkflowWireError::TextTooLong { field, max_bytes });
    }
    Ok(())
}
