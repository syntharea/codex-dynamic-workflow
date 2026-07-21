use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use thiserror::Error;

use crate::WorkflowAgentId;
use crate::WorkflowId;
use crate::WorkflowTaskId;

pub const WORKFLOW_DISPATCH_MARKER_PREFIX: &str = "codex_workflow_dispatch:";
pub const WORKFLOW_DISPATCH_MARKER_MAX_BYTES: usize = 1024;
const WORKFLOW_DISPATCH_MARKER_SCHEMA_VERSION: u32 = 1;
const WORKFLOW_DISPATCH_TASK_ID_MAX_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowDispatchMarker {
    schema_version: u32,
    workflow_id: WorkflowId,
    task_id: WorkflowTaskId,
    execution_attempt: u32,
    task_attempt: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<WorkflowAgentId>,
}

impl WorkflowDispatchMarker {
    pub fn try_new(
        workflow_id: WorkflowId,
        task_id: WorkflowTaskId,
        execution_attempt: u32,
        task_attempt: u8,
    ) -> Result<Self, WorkflowDispatchMarkerError> {
        let marker = Self {
            schema_version: WORKFLOW_DISPATCH_MARKER_SCHEMA_VERSION,
            workflow_id,
            task_id,
            execution_attempt,
            task_attempt,
            agent_id: None,
        };
        marker.validate()?;
        Ok(marker)
    }

    pub fn encode(&self) -> Result<String, WorkflowDispatchMarkerError> {
        self.validate()?;
        if self.agent_id.is_none() {
            return Err(WorkflowDispatchMarkerError::MissingAgentId);
        }
        let payload = serde_json::to_string(self)
            .map_err(|error| WorkflowDispatchMarkerError::InvalidJson(error.to_string()))?;
        let encoded = format!("{WORKFLOW_DISPATCH_MARKER_PREFIX}{payload}");
        if encoded.len() > WORKFLOW_DISPATCH_MARKER_MAX_BYTES {
            return Err(WorkflowDispatchMarkerError::MarkerTooLarge {
                max_bytes: WORKFLOW_DISPATCH_MARKER_MAX_BYTES,
            });
        }
        Ok(encoded)
    }

    pub fn decode(value: &str) -> Result<Option<Self>, WorkflowDispatchMarkerError> {
        let Some(payload) = value.strip_prefix(WORKFLOW_DISPATCH_MARKER_PREFIX) else {
            return Ok(None);
        };
        if value.len() > WORKFLOW_DISPATCH_MARKER_MAX_BYTES {
            return Err(WorkflowDispatchMarkerError::MarkerTooLarge {
                max_bytes: WORKFLOW_DISPATCH_MARKER_MAX_BYTES,
            });
        }
        serde_json::from_str(payload)
            .map(Some)
            .map_err(|error| WorkflowDispatchMarkerError::InvalidJson(error.to_string()))
    }

    pub fn workflow_id(&self) -> WorkflowId {
        self.workflow_id
    }

    pub fn task_id(&self) -> &WorkflowTaskId {
        &self.task_id
    }

    pub fn execution_attempt(&self) -> u32 {
        self.execution_attempt
    }

    pub fn task_attempt(&self) -> u8 {
        self.task_attempt
    }

    pub fn bind_agent(
        mut self,
        agent_id: WorkflowAgentId,
    ) -> Result<Self, WorkflowDispatchMarkerError> {
        if self.agent_id.is_some() {
            return Err(WorkflowDispatchMarkerError::AgentAlreadyBound);
        }
        self.agent_id = Some(agent_id);
        self.validate()?;
        Ok(self)
    }

    pub fn agent_id(&self) -> Option<&WorkflowAgentId> {
        self.agent_id.as_ref()
    }

    fn validate(&self) -> Result<(), WorkflowDispatchMarkerError> {
        if self.schema_version != WORKFLOW_DISPATCH_MARKER_SCHEMA_VERSION {
            return Err(WorkflowDispatchMarkerError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.task_id.as_str().is_empty() {
            return Err(WorkflowDispatchMarkerError::EmptyTaskId);
        }
        if self.task_id.as_str().len() > WORKFLOW_DISPATCH_TASK_ID_MAX_BYTES {
            return Err(WorkflowDispatchMarkerError::TaskIdTooLong {
                max_bytes: WORKFLOW_DISPATCH_TASK_ID_MAX_BYTES,
            });
        }
        if self.execution_attempt == 0 {
            return Err(WorkflowDispatchMarkerError::InvalidExecutionAttempt);
        }
        if self.task_attempt == 0 {
            return Err(WorkflowDispatchMarkerError::InvalidTaskAttempt);
        }
        if let Some(agent_id) = self.agent_id.as_ref() {
            if agent_id.as_str().is_empty() {
                return Err(WorkflowDispatchMarkerError::EmptyAgentId);
            }
            if agent_id.as_str().len() > WORKFLOW_DISPATCH_TASK_ID_MAX_BYTES {
                return Err(WorkflowDispatchMarkerError::AgentIdTooLong {
                    max_bytes: WORKFLOW_DISPATCH_TASK_ID_MAX_BYTES,
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkflowDispatchMarker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: u32,
            workflow_id: WorkflowId,
            task_id: WorkflowTaskId,
            execution_attempt: u32,
            task_attempt: u8,
            agent_id: WorkflowAgentId,
        }

        let wire = Wire::deserialize(deserializer)?;
        let marker = Self {
            schema_version: wire.schema_version,
            workflow_id: wire.workflow_id,
            task_id: wire.task_id,
            execution_attempt: wire.execution_attempt,
            task_attempt: wire.task_attempt,
            agent_id: Some(wire.agent_id),
        };
        marker.validate().map_err(serde::de::Error::custom)?;
        Ok(marker)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowDispatchMarkerError {
    #[error("unsupported workflow dispatch marker schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("workflow dispatch task id must not be empty")]
    EmptyTaskId,
    #[error("workflow dispatch task id exceeds {max_bytes} bytes")]
    TaskIdTooLong { max_bytes: usize },
    #[error("workflow dispatch execution attempt must be greater than zero")]
    InvalidExecutionAttempt,
    #[error("workflow dispatch task attempt must be greater than zero")]
    InvalidTaskAttempt,
    #[error("workflow dispatch marker is missing its agent id")]
    MissingAgentId,
    #[error("workflow dispatch marker already has an agent id")]
    AgentAlreadyBound,
    #[error("workflow dispatch agent id must not be empty")]
    EmptyAgentId,
    #[error("workflow dispatch agent id exceeds {max_bytes} bytes")]
    AgentIdTooLong { max_bytes: usize },
    #[error("workflow dispatch marker exceeds {max_bytes} bytes")]
    MarkerTooLarge { max_bytes: usize },
    #[error("workflow dispatch marker is not valid JSON: {0}")]
    InvalidJson(String),
}
