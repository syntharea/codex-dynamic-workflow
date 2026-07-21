use std::sync::Arc;

use anyhow::Result;
use codex_protocol::workflow::WorkflowCancelParams;
use codex_protocol::workflow::WorkflowPauseParams;
use codex_protocol::workflow::WorkflowReadParams;
use codex_protocol::workflow::WorkflowResumeParams;
use codex_protocol::workflow::WorkflowRetryParams;
use codex_workflows::WorkflowSnapshot;

use crate::session::session::Session;

use super::commit_or_fail;
use super::events;
use super::parse_workflow_id;
use super::spawn_drive;
use super::workflow_adapter;
use crate::workflows::WorkflowManager;
use crate::workflows::manager::WorkflowResumeTransition;

pub(in crate::workflows) async fn read(
    session: &Session,
    manager: &WorkflowManager,
    params: WorkflowReadParams,
) -> Result<WorkflowSnapshot> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    manager
        .snapshot(workflow_id)
        .await
        .map_err(anyhow::Error::from)
}

pub(in crate::workflows) async fn list(
    session: &Session,
    manager: &WorkflowManager,
) -> Vec<WorkflowSnapshot> {
    manager.snapshots_for_owner(session.thread_id()).await
}

pub(in crate::workflows) async fn pause(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowPauseParams,
) -> Result<WorkflowSnapshot> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    let snapshot = manager.pause(workflow_id).await?;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::snapshot_progress(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;
    Ok(snapshot)
}

pub(in crate::workflows) async fn resume(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowResumeParams,
) -> Result<WorkflowSnapshot> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    let adapter = workflow_adapter(&session).await;
    let snapshot = match manager.resume_transition(workflow_id).await? {
        WorkflowResumeTransition::Drive(snapshot) => {
            commit_or_fail(
                &session,
                submission_id,
                &manager,
                workflow_id,
                Arc::clone(&adapter),
                snapshot.clone(),
                events::snapshot_progress(&snapshot).map_err(anyhow::Error::from),
            )
            .await?;
            spawn_drive(
                Arc::clone(&session),
                submission_id.to_string(),
                Arc::clone(&manager),
                workflow_id,
            )
            .await;
            snapshot
        }
        WorkflowResumeTransition::Pending(snapshot) => {
            commit_or_fail(
                &session,
                submission_id,
                &manager,
                workflow_id,
                adapter,
                snapshot.clone(),
                events::snapshot_progress(&snapshot).map_err(anyhow::Error::from),
            )
            .await?;
            snapshot
        }
        WorkflowResumeTransition::Unchanged(snapshot) => snapshot,
    };
    Ok(snapshot)
}

pub(in crate::workflows) async fn cancel(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowCancelParams,
) -> Result<WorkflowSnapshot> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    let snapshot = manager.cancel(workflow_id).await?;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::cancelled(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;
    Ok(snapshot)
}

pub(in crate::workflows) async fn retry(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowRetryParams,
) -> Result<WorkflowSnapshot> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    let snapshot = manager.retry_transition(workflow_id).await?;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::snapshot_progress(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;
    spawn_drive(session, submission_id.to_string(), manager, workflow_id).await;
    Ok(snapshot)
}
