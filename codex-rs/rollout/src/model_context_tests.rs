use super::*;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::WorkflowStateItem;
use serde_json::json;

#[test]
fn workflow_snapshot_is_excluded_from_finished_model_context() {
    let snapshot = WorkflowStateItem::try_new(
        /*schema_version*/ 1,
        "wf_123",
        json!({"state": "running"}),
    )
    .expect("workflow snapshot");
    let mut scan = ModelContextScan::default();
    scan.push(RolloutItem::WorkflowState(snapshot));

    let context = scan.finish(SessionMetaLine {
        meta: SessionMeta::default(),
        git: None,
    });

    assert!(
        context
            .iter()
            .all(|item| !matches!(item, RolloutItem::WorkflowState(_)))
    );
}
