use super::*;

#[test]
fn maps_terminal_agent_statuses_without_inventing_success() {
    assert_eq!(
        map_status(AgentStatus::Completed(Some("done".to_string()))),
        WorkflowAgentOutcomeStatus::Succeeded {
            summary: "done".to_string()
        }
    );
    assert_eq!(
        map_status(AgentStatus::Errored("bad".to_string())),
        WorkflowAgentOutcomeStatus::Failed {
            summary: "bad".to_string(),
            retryable: true
        }
    );
    assert_eq!(
        map_status(AgentStatus::Interrupted),
        WorkflowAgentOutcomeStatus::Cancelled
    );
}
