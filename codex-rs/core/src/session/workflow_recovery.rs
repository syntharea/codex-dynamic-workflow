use std::collections::BTreeMap;

use anyhow::Context;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::WorkflowStateItem;

use super::Session;

#[derive(Debug, thiserror::Error)]
enum WorkflowSnapshotRestoreError {
    #[error("failed to decode persisted workflow snapshot {workflow_id}")]
    Decode {
        workflow_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "persisted workflow snapshot id mismatch: outer workflow id {outer_workflow_id}, snapshot workflow id {snapshot_workflow_id}"
    )]
    IdMismatch {
        outer_workflow_id: String,
        snapshot_workflow_id: String,
    },
}

impl Session {
    pub(super) async fn recover_workflows(
        &self,
        workflow_snapshots: BTreeMap<String, WorkflowStateItem>,
        persist_workflows: bool,
    ) -> anyhow::Result<()> {
        if !persist_workflows {
            return Ok(());
        }

        let workflow_manager = self.services.agent_control.workflow_manager();
        for item in workflow_snapshots.into_values() {
            let outer_workflow_id = item.workflow_id().to_string();
            let snapshot = serde_json::from_value::<codex_workflows::WorkflowSnapshot>(
                item.snapshot().clone(),
            )
            .map_err(|source| WorkflowSnapshotRestoreError::Decode {
                workflow_id: outer_workflow_id.clone(),
                source,
            })?;
            let snapshot_workflow_id = snapshot.workflow_id.to_string();
            if outer_workflow_id != snapshot_workflow_id {
                return Err(WorkflowSnapshotRestoreError::IdMismatch {
                    outer_workflow_id,
                    snapshot_workflow_id,
                }
                .into());
            }
            let workflow_id = snapshot.workflow_id;
            let outcomes = self
                .services
                .agent_control
                .workflow_agent_outcomes(self.thread_id(), &snapshot)
                .await
                .with_context(|| {
                    format!(
                        "failed to reconcile persisted workflow snapshot {workflow_id} with agent outcomes"
                    )
                })?;
            let restored = workflow_manager
                .restore_snapshot(self.thread_id(), snapshot, &outcomes)
                .await
                .with_context(|| {
                    format!("failed to restore persisted workflow snapshot {workflow_id}")
                })?;
            self.persist_workflow_snapshot_checked(&restored)
                .await
                .with_context(|| {
                    format!("failed to persist reconciled workflow snapshot {workflow_id}")
                })?;
        }
        Ok(())
    }

    pub(crate) async fn persist_workflow_rollout_items_checked(
        &self,
        items: &[RolloutItem],
    ) -> anyhow::Result<()> {
        let live_thread = self
            .live_thread()
            .ok_or_else(|| anyhow::anyhow!("workflow persistence is unavailable"))?;
        live_thread.append_items(items).await?;
        Ok(())
    }

    async fn persist_workflow_snapshot_checked(
        &self,
        snapshot: &codex_workflows::WorkflowSnapshot,
    ) -> anyhow::Result<()> {
        let item = WorkflowStateItem::new(
            snapshot.workflow_id.to_string(),
            serde_json::to_value(snapshot)?,
        )?;
        self.persist_workflow_rollout_items_checked(&[RolloutItem::WorkflowState(item)])
            .await
    }
}
