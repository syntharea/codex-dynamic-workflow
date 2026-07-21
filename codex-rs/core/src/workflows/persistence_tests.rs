use std::sync::Arc;
use std::sync::Mutex;

use codex_protocol::protocol::RolloutItem;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowSnapshot;

use super::persist_snapshot_with;

#[tokio::test]
async fn checked_persist_propagates_append_failure_with_exact_snapshot() {
    let definition = WorkflowDefinition::try_new("persist", Vec::new(), WorkflowLimits::default())
        .expect("definition");
    let snapshot = WorkflowSnapshot::new(WorkflowId::new(), definition);
    let captured = Arc::new(Mutex::new(None));
    let captured_item = Arc::clone(&captured);

    let error = persist_snapshot_with(&snapshot, move |item| async move {
        *captured_item.lock().expect("captured item lock") = Some(item);
        anyhow::bail!("append failed")
    })
    .await
    .expect_err("checked append should fail");

    assert_eq!(error.to_string(), "append failed");
    let RolloutItem::WorkflowState(item) = captured
        .lock()
        .expect("captured item lock")
        .take()
        .expect("captured workflow state")
    else {
        panic!("expected workflow state item")
    };
    assert_eq!(item.workflow_id(), snapshot.workflow_id.to_string());
    assert_eq!(
        item.snapshot(),
        &serde_json::to_value(snapshot).expect("serialize snapshot")
    );
}
