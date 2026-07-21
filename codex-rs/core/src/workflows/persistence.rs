use std::future::Future;

use codex_protocol::protocol::RolloutItem;
use codex_protocol::workflow::WorkflowStateItem;
use codex_workflows::WorkflowSnapshot;

use crate::session::session::Session;

pub(super) async fn persist_snapshot(
    session: &Session,
    snapshot: &WorkflowSnapshot,
) -> anyhow::Result<()> {
    persist_snapshot_with(snapshot, |item| async move {
        session
            .persist_workflow_rollout_items_checked(&[item])
            .await
    })
    .await
}

async fn persist_snapshot_with<F, Fut>(snapshot: &WorkflowSnapshot, append: F) -> anyhow::Result<()>
where
    F: FnOnce(RolloutItem) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let value = serde_json::to_value(snapshot)?;
    let item = WorkflowStateItem::new(snapshot.workflow_id.to_string(), value)?;
    append(RolloutItem::WorkflowState(item)).await
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod tests;
