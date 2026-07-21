use std::time::Duration;

use crate::WorkflowAgentControl;
use crate::WorkflowAgentId;
use crate::WorkflowAgentOutcome;
use crate::WorkflowError;

struct CloseAwareControl;

impl WorkflowAgentControl for CloseAwareControl {
    async fn spawn(
        &self,
        _request: crate::WorkflowAgentRequest,
    ) -> Result<WorkflowAgentId, WorkflowError> {
        todo!()
    }

    async fn wait(
        &self,
        _agents: Vec<WorkflowAgentId>,
        _timeout: Duration,
    ) -> Result<Vec<WorkflowAgentOutcome>, WorkflowError> {
        todo!()
    }

    async fn interrupt(&self, _agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        todo!()
    }

    async fn close(&self, _agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        Ok(())
    }
}

fn assert_workflow_agent_control<T: WorkflowAgentControl>() {}
fn assert_send<T: Send>(_: T) {}

#[test]
fn workflow_agent_control_exposes_close_with_send_future() {
    let control = CloseAwareControl;
    assert_workflow_agent_control::<CloseAwareControl>();
    assert_send(control.close(WorkflowAgentId::from("agent")));
}
