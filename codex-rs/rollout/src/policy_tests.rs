use super::*;
use codex_protocol::protocol::WorkflowProgressEvent;
use codex_protocol::protocol::WorkflowStateItem;
use codex_protocol::protocol::WorkflowStatus;
use codex_protocol::protocol::WorkflowText;
use codex_protocol::workflow::WorkflowId;
use serde_json::json;

#[test]
fn workflow_snapshot_is_persisted_but_lifecycle_event_is_not() {
    let snapshot = RolloutItem::WorkflowState(
        WorkflowStateItem::try_new(
            /*schema_version*/ 1,
            "wf_123",
            json!({"state": "running"}),
        )
        .expect("workflow snapshot"),
    );
    assert!(is_persisted_rollout_item(
        &snapshot,
        ThreadHistoryMode::Paginated
    ));

    let progress = EventMsg::WorkflowProgress(WorkflowProgressEvent::new(
        WorkflowId::try_new("wf_123").expect("workflow id"),
        WorkflowStatus::Running,
        /*completed_tasks*/ 0,
        /*running_tasks*/ 1,
        /*queued_tasks*/ 0,
        /*blocked_tasks*/ 0,
        /*failed_tasks*/ 0,
        WorkflowText::try_new("running").expect("summary"),
    ));
    assert!(!should_persist_event_msg(
        &progress,
        ThreadHistoryMode::Paginated
    ));
}
