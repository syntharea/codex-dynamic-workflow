use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::workflow::WorkflowStartParams;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowState;
use tokio::sync::mpsc;

use crate::session::session::Session;

use super::WorkflowManager;
use super::WorkflowRuntimeEvent;
use super::agent_control::WorkflowAgentControlAdapter;
use super::background;
use super::events;
use super::persistence::persist_snapshot;
use super::planner::WorkflowPlanner;

pub(super) mod control;
pub(super) mod saved;

pub(crate) async fn handle_session_op(session: Arc<Session>, submission_id: String, op: Op) {
    if let Err(error) = handle_op(Arc::clone(&session), &submission_id, op).await {
        session
            .send_event_raw(Event {
                id: submission_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: error.to_string(),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            })
            .await;
    }
}

async fn handle_op(session: Arc<Session>, submission_id: &str, op: Op) -> Result<()> {
    ensure_available(&session)?;
    let manager = session.services.agent_control.workflow_manager();
    match op {
        Op::WorkflowStart(params) => start(session, submission_id, manager, params)
            .await
            .map(drop),
        Op::WorkflowRead(params) => {
            let snapshot = control::read(&session, &manager, params).await?;
            emit_event(
                &session,
                submission_id,
                events::snapshot_progress(&snapshot)?,
            )
            .await;
            Ok(())
        }
        Op::WorkflowList(_) => {
            for snapshot in control::list(&session, &manager).await {
                emit_event(
                    &session,
                    submission_id,
                    events::snapshot_progress(&snapshot)?,
                )
                .await;
            }
            Ok(())
        }
        Op::WorkflowPause(params) => control::pause(session, submission_id, manager, params)
            .await
            .map(drop),
        Op::WorkflowResume(params) => control::resume(session, submission_id, manager, params)
            .await
            .map(drop),
        Op::WorkflowCancel(params) => control::cancel(session, submission_id, manager, params)
            .await
            .map(drop),
        Op::WorkflowRetry(params) => control::retry(session, submission_id, manager, params)
            .await
            .map(drop),
        Op::WorkflowSave(params) => saved::save_workflow(session, manager, params)
            .await
            .map(drop),
        Op::WorkflowRunSaved(params) => {
            saved::run_saved_workflow(session, submission_id, manager, params)
                .await
                .map(drop)
        }
        Op::WorkflowDeleteSaved(params) => saved::delete_saved_workflow(session, manager, params)
            .await
            .map(drop),
        _ => Ok(()),
    }
}

pub(super) fn ensure_available(session: &Session) -> Result<()> {
    if !session.feature_enabled(Feature::DynamicWorkflows) {
        anyhow::bail!("dynamic workflows feature is disabled")
    }
    if !session
        .services
        .agent_control
        .workflow_manager()
        .config()
        .enabled
    {
        anyhow::bail!("dynamic workflows require multi-agent support")
    }
    Ok(())
}

pub(super) async fn start(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowStartParams,
) -> Result<WorkflowSnapshot> {
    let snapshot = manager
        .begin_planning(params.objective.as_str().to_string(), session.thread_id())
        .await?;
    let workflow_id = snapshot.workflow_id;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::started(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;

    let owner = session.thread_id();
    let task = background::spawn_planner(
        Arc::clone(&session),
        submission_id.to_string(),
        manager,
        workflow_id,
        params,
    );
    session
        .services
        .agent_control
        .register_workflow_task(owner, task)
        .await;
    Ok(snapshot)
}

async fn plan_and_drive(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
    params: WorkflowStartParams,
) -> Result<()> {
    let planner = WorkflowPlanner::new(
        session.services.agent_control.clone(),
        session.get_config().await,
        session.thread_id(),
        session.thread_config_snapshot().await.session_source,
    );
    let generation = manager.generation(workflow_id).await?;
    let cancellation = manager.cancellation_signal(workflow_id).await?;
    let definition = planner
        .plan(
            &params.objective,
            params.policy.as_ref(),
            &manager,
            workflow_id,
            cancellation,
        )
        .await?;
    let snapshot = manager
        .activate(workflow_id, generation, definition)
        .await?;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        &submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::snapshot_progress(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;
    drive(session, submission_id, manager, workflow_id).await
}

pub(super) async fn planner_task(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
    params: WorkflowStartParams,
) {
    if let Err(error) = plan_and_drive(
        Arc::clone(&session),
        submission_id.clone(),
        Arc::clone(&manager),
        workflow_id,
        params,
    )
    .await
        && manager
            .snapshot(workflow_id)
            .await
            .is_ok_and(|snapshot| snapshot.state != WorkflowState::Failed)
    {
        let _ = fail_workflow(
            &session,
            &submission_id,
            &manager,
            workflow_id,
            &error.to_string(),
        )
        .await;
    }
}

async fn spawn_drive(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
) {
    let owner = session.thread_id();
    let task = background::spawn_drive(Arc::clone(&session), submission_id, manager, workflow_id);
    session
        .services
        .agent_control
        .register_workflow_task(owner, task)
        .await;
}

pub(super) async fn drive_task(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
) {
    if let Err(error) = drive(
        Arc::clone(&session),
        submission_id.clone(),
        Arc::clone(&manager),
        workflow_id,
    )
    .await
        && manager
            .snapshot(workflow_id)
            .await
            .is_ok_and(|snapshot| snapshot.state != WorkflowState::Failed)
    {
        let _ = fail_workflow(
            &session,
            &submission_id,
            &manager,
            workflow_id,
            &error.to_string(),
        )
        .await;
    }
}

async fn drive(
    session: Arc<Session>,
    submission_id: String,
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
) -> Result<()> {
    let adapter = workflow_adapter(&session).await;
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let event_sink = Arc::new(move |event| {
        event_tx
            .send(event)
            .map_err(|_| WorkflowError::Agent("workflow event consumer is unavailable".to_string()))
    });
    let event_task = tokio::spawn(consume_runtime_events(
        Arc::clone(&session),
        submission_id,
        event_rx,
    ));
    let drive_task = manager
        .drive_with_events(workflow_id, Arc::clone(&adapter), event_sink)
        .await?;
    supervise_drive(manager, workflow_id, adapter, drive_task, event_task).await
}

async fn supervise_drive<A>(
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
    adapter: Arc<A>,
    mut drive_task: tokio::task::JoinHandle<Result<(), WorkflowError>>,
    mut event_task: tokio::task::JoinHandle<Result<()>>,
) -> Result<()>
where
    A: codex_workflows::WorkflowAgentControl + 'static,
{
    tokio::select! {
        drive_result = &mut drive_task => {
            let drive_result = drive_result.context("workflow driver task panicked")?;
            match event_task.await.context("workflow event task panicked")? {
                Ok(()) => drive_result.map_err(anyhow::Error::from),
                Err(error) => fail_after_persistence_error(
                    manager,
                    workflow_id,
                    adapter,
                    error,
                ).await,
            }
        }
        event_result = &mut event_task => {
            match event_result.context("workflow event task panicked")? {
                Ok(()) => drive_task
                    .await
                    .context("workflow driver task panicked")?
                    .map_err(anyhow::Error::from),
                Err(error) => {
                    let failure = fail_after_persistence_error(
                        manager,
                        workflow_id,
                        adapter,
                        error,
                    )
                    .await;
                    drive_task
                        .await
                        .context("workflow driver task panicked")??;
                    failure
                }
            }
        }
    }
}

async fn fail_after_persistence_error<A>(
    manager: Arc<WorkflowManager>,
    workflow_id: WorkflowId,
    adapter: Arc<A>,
    error: anyhow::Error,
) -> Result<()>
where
    A: codex_workflows::WorkflowAgentControl + 'static,
{
    manager
        .persistence_failed(workflow_id, adapter)
        .await
        .context("failed to stop workflow after persistence failure")?;
    Err(error)
}

async fn consume_runtime_events(
    session: Arc<Session>,
    submission_id: String,
    mut event_rx: mpsc::UnboundedReceiver<WorkflowRuntimeEvent>,
) -> Result<()> {
    while let Some(event) = event_rx.recv().await {
        match event {
            WorkflowRuntimeEvent::Persist(snapshot) => {
                persist_snapshot(&session, &snapshot).await?
            }
            WorkflowRuntimeEvent::Progress { progress, snapshot } => {
                commit_snapshot_event(
                    &session,
                    &submission_id,
                    &snapshot,
                    events::progress(&snapshot, &progress)?,
                )
                .await?;
            }
            WorkflowRuntimeEvent::Paused(snapshot) => {
                commit_snapshot_event(
                    &session,
                    &submission_id,
                    &snapshot,
                    events::paused(&snapshot)?,
                )
                .await?;
            }
            WorkflowRuntimeEvent::Completed(snapshot) => {
                commit_snapshot_event(
                    &session,
                    &submission_id,
                    &snapshot,
                    events::completed(&snapshot)?,
                )
                .await?;
            }
            WorkflowRuntimeEvent::Failed { snapshot, error } => {
                commit_snapshot_event(
                    &session,
                    &submission_id,
                    &snapshot,
                    events::failed(&snapshot, &error)?,
                )
                .await?;
            }
            WorkflowRuntimeEvent::Cancelled(snapshot) => {
                commit_snapshot_event(
                    &session,
                    &submission_id,
                    &snapshot,
                    events::cancelled(&snapshot)?,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn commit_or_fail(
    session: &Session,
    submission_id: &str,
    manager: &WorkflowManager,
    workflow_id: WorkflowId,
    adapter: Arc<WorkflowAgentControlAdapter>,
    snapshot: WorkflowSnapshot,
    event: Result<EventMsg>,
) -> Result<()> {
    let result = async {
        let event = event?;
        commit_snapshot_event(session, submission_id, &snapshot, event).await
    }
    .await;
    commit_or_fail_action(manager, workflow_id, adapter, result).await
}

async fn commit_or_fail_action<A>(
    manager: &WorkflowManager,
    workflow_id: WorkflowId,
    adapter: Arc<A>,
    result: Result<()>,
) -> Result<()>
where
    A: codex_workflows::WorkflowAgentControl + 'static,
{
    if let Err(error) = result {
        manager
            .persistence_failed(workflow_id, adapter)
            .await
            .context("failed to stop workflow after persistence failure")?;
        return Err(error);
    }
    Ok(())
}

async fn workflow_adapter(session: &Session) -> Arc<WorkflowAgentControlAdapter> {
    Arc::new(WorkflowAgentControlAdapter::new(
        session.services.agent_control.clone(),
        session.get_config().await,
        session.thread_id(),
        session.thread_config_snapshot().await.session_source,
    ))
}

async fn commit_snapshot_event(
    session: &Session,
    submission_id: &str,
    snapshot: &WorkflowSnapshot,
    event: EventMsg,
) -> Result<()> {
    persist_snapshot(session, snapshot).await?;
    emit_event(session, submission_id, event).await;
    Ok(())
}

async fn fail_workflow(
    session: &Session,
    submission_id: &str,
    manager: &WorkflowManager,
    workflow_id: WorkflowId,
    error: &str,
) -> Result<()> {
    let snapshot = manager.fail(workflow_id, error).await?;
    if snapshot.state != WorkflowState::Failed {
        return Ok(());
    }
    let error = truncate(error, 4 * 1024);
    let event = events::failed(&snapshot, &error)?;
    commit_snapshot_event(session, submission_id, &snapshot, event).await
}

async fn emit_event(session: &Session, submission_id: &str, msg: EventMsg) {
    session
        .send_event_raw(Event {
            id: submission_id.to_string(),
            msg,
        })
        .await;
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(anyhow::Error::from)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
