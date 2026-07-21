use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

use crate::WorkflowAgentId;
use crate::WorkflowDispatchMarker;
use crate::WorkflowError;
use crate::WorkflowId;
use crate::WorkflowSnapshot;
use crate::WorkflowTaskId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAgentRequest {
    pub workflow_id: WorkflowId,
    pub task_id: WorkflowTaskId,
    pub title: String,
    pub instructions: String,
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAgentOutcome {
    pub agent_id: WorkflowAgentId,
    pub task_id: WorkflowTaskId,
    pub status: WorkflowAgentOutcomeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkflowAgentOutcomeStatus {
    Succeeded { summary: String },
    Failed { summary: String, retryable: bool },
    Cancelled,
}

/// Controls workflow agents without coupling the domain crate to an executor implementation.
///
/// Implementations should delegate lifecycle operations to the host's existing agent runtime.
pub trait WorkflowAgentControl: Send + Sync {
    fn spawn(
        &self,
        request: WorkflowAgentRequest,
    ) -> impl std::future::Future<Output = Result<WorkflowAgentId, WorkflowError>> + Send;

    /// Prepares an agent with durable workflow dispatch identity.
    ///
    /// Persistent implementations should record `dispatch` but defer visible work until
    /// [`Self::start_dispatch`] is called. In-memory executors may use the default delegation.
    fn spawn_dispatch(
        &self,
        request: WorkflowAgentRequest,
        dispatch: WorkflowDispatchMarker,
    ) -> impl std::future::Future<Output = Result<WorkflowAgentId, WorkflowError>> + Send {
        async move {
            let _ = dispatch;
            self.spawn(request).await
        }
    }

    /// Persists the snapshot that binds a prepared dispatch to its agent.
    ///
    /// Persistent implementations must durably commit `snapshot` before returning. The manager
    /// calls this before [`Self::start_dispatch`].
    fn persist_dispatch(
        &self,
        agent: WorkflowAgentId,
        snapshot: WorkflowSnapshot,
    ) -> impl std::future::Future<Output = Result<(), WorkflowError>> + Send {
        async move {
            let _ = (agent, snapshot);
            Ok(())
        }
    }

    /// Starts visible work for a prepared dispatch after its binding is durable.
    fn start_dispatch(
        &self,
        agent: WorkflowAgentId,
    ) -> impl std::future::Future<Output = Result<(), WorkflowError>> + Send {
        async move {
            let _ = agent;
            Ok(())
        }
    }

    fn wait(
        &self,
        agents: Vec<WorkflowAgentId>,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<Vec<WorkflowAgentOutcome>, WorkflowError>> + Send;

    fn interrupt(
        &self,
        agent: WorkflowAgentId,
    ) -> impl std::future::Future<Output = Result<(), WorkflowError>> + Send;

    fn close(
        &self,
        agent: WorkflowAgentId,
    ) -> impl std::future::Future<Output = Result<(), WorkflowError>> + Send;
}
