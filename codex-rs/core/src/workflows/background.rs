use std::sync::Arc;

use codex_protocol::workflow::WorkflowStartParams;
use codex_workflows::WorkflowId;
use tokio::task::JoinHandle;

use crate::session::session::Session;

use super::WorkflowManager;

pub(super) fn spawn_planner(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
    params: WorkflowStartParams,
) -> JoinHandle<()> {
    tokio::spawn(super::runtime::planner_task(
        session,
        submission_id,
        manager,
        workflow_id,
        params,
    ))
}

pub(super) fn spawn_drive(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
) -> JoinHandle<()> {
    tokio::spawn(super::runtime::drive_task(
        session,
        submission_id,
        manager,
        workflow_id,
    ))
}
