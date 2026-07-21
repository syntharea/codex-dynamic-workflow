use codex_protocol::protocol::EventMsg;
use codex_protocol::workflow::WorkflowCancelledEvent;
use codex_protocol::workflow::WorkflowCompletedEvent;
use codex_protocol::workflow::WorkflowFailedEvent;
use codex_protocol::workflow::WorkflowId;
use codex_protocol::workflow::WorkflowPausedEvent;
use codex_protocol::workflow::WorkflowProgressEvent;
use codex_protocol::workflow::WorkflowStartedEvent;
use codex_protocol::workflow::WorkflowStatus;
use codex_protocol::workflow::WorkflowSummary;
use codex_protocol::workflow::WorkflowTaskStatus;
use codex_protocol::workflow::WorkflowTaskSummary;
use codex_protocol::workflow::WorkflowText;
use codex_protocol::workflow::WorkflowTitle;
use codex_protocol::workflow::WorkflowWireError;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTaskState;

use super::WorkflowProgress;

pub(super) fn started(snapshot: &WorkflowSnapshot) -> Result<EventMsg, WorkflowWireError> {
    let (workflow_id, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowStarted(WorkflowStartedEvent::try_new(
        workflow_id,
        summary,
    )?))
}

pub(super) fn paused(snapshot: &WorkflowSnapshot) -> Result<EventMsg, WorkflowWireError> {
    let (workflow_id, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowPaused(WorkflowPausedEvent::try_new(
        workflow_id,
        summary,
    )?))
}

pub(super) fn completed(snapshot: &WorkflowSnapshot) -> Result<EventMsg, WorkflowWireError> {
    let (workflow_id, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowCompleted(
        WorkflowCompletedEvent::try_new(workflow_id, summary)?,
    ))
}

pub(super) fn cancelled(snapshot: &WorkflowSnapshot) -> Result<EventMsg, WorkflowWireError> {
    let (workflow_id, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowCancelled(
        WorkflowCancelledEvent::try_new(workflow_id, summary)?,
    ))
}

pub(super) fn failed(
    snapshot: &WorkflowSnapshot,
    error: &str,
) -> Result<EventMsg, WorkflowWireError> {
    let (workflow_id, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowFailed(WorkflowFailedEvent::try_new(
        workflow_id,
        summary,
        WorkflowText::new(error)?,
    )?))
}

pub(super) fn progress(
    snapshot: &WorkflowSnapshot,
    progress: &WorkflowProgress,
) -> Result<EventMsg, WorkflowWireError> {
    Ok(EventMsg::WorkflowProgress(WorkflowProgressEvent::new(
        WorkflowId::new(progress.workflow_id.to_string())?,
        workflow_status(snapshot.state),
        progress.completed,
        progress.running,
        progress.queued,
        progress.blocked,
        progress.failed,
        WorkflowText::new(progress.summary.clone())?,
    )))
}

pub(super) fn snapshot_progress(
    snapshot: &WorkflowSnapshot,
) -> Result<EventMsg, WorkflowWireError> {
    let (_, summary) = summary(snapshot)?;
    Ok(EventMsg::WorkflowProgress(WorkflowProgressEvent::new(
        WorkflowId::new(snapshot.workflow_id.to_string())?,
        summary.status(),
        summary.completed_tasks(),
        summary.running_tasks(),
        summary.queued_tasks(),
        summary.blocked_tasks(),
        summary.failed_tasks(),
        summary.summary().clone(),
    )))
}

pub(super) fn summary(
    snapshot: &WorkflowSnapshot,
) -> Result<(WorkflowId, WorkflowSummary), WorkflowWireError> {
    let workflow_id = WorkflowId::new(snapshot.workflow_id.to_string())?;
    let tasks = snapshot
        .tasks
        .iter()
        .map(|task_snapshot| {
            let definition = snapshot
                .definition
                .task(&task_snapshot.task_id)
                .ok_or(WorkflowWireError::InvalidSnapshot)?;
            WorkflowTaskSummary::try_new(
                task_snapshot.task_id.to_string(),
                WorkflowTitle::new(definition.title.clone())?,
                task_status(task_snapshot.state),
                WorkflowText::new(task_snapshot.summary.clone().unwrap_or_default())?,
                u32::from(task_snapshot.attempts),
            )
        })
        .collect::<Result<Vec<_>, WorkflowWireError>>()?;
    let completed_tasks = count(snapshot, WorkflowTaskState::Succeeded);
    let running_tasks = snapshot
        .tasks
        .iter()
        .filter(|task| task.state.is_active())
        .count();
    let queued_tasks = count(snapshot, WorkflowTaskState::Queued);
    let blocked_tasks = count(snapshot, WorkflowTaskState::Blocked);
    let failed_tasks = count(snapshot, WorkflowTaskState::Failed);
    let text = snapshot
        .tasks
        .iter()
        .filter_map(|task| task.summary.as_deref())
        .next()
        .unwrap_or_default();
    let summary = WorkflowSummary::try_new(
        workflow_id.clone(),
        WorkflowTitle::new(snapshot.definition.title.clone())?,
        workflow_status(snapshot.state),
        tasks,
        completed_tasks,
        running_tasks,
        queued_tasks,
        blocked_tasks,
        failed_tasks,
        WorkflowText::new(text)?,
    )?;
    Ok((workflow_id, summary))
}

fn count(snapshot: &WorkflowSnapshot, state: WorkflowTaskState) -> usize {
    snapshot
        .tasks
        .iter()
        .filter(|task| task.state == state)
        .count()
}

fn workflow_status(state: WorkflowState) -> WorkflowStatus {
    match state {
        WorkflowState::Planning => WorkflowStatus::Planning,
        WorkflowState::Running => WorkflowStatus::Running,
        WorkflowState::Pausing => WorkflowStatus::Pausing,
        WorkflowState::Paused => WorkflowStatus::Paused,
        WorkflowState::Consolidating => WorkflowStatus::Consolidating,
        WorkflowState::Completed => WorkflowStatus::Completed,
        WorkflowState::Failed => WorkflowStatus::Failed,
        WorkflowState::Cancelled => WorkflowStatus::Cancelled,
    }
}

fn task_status(state: WorkflowTaskState) -> WorkflowTaskStatus {
    match state {
        WorkflowTaskState::Queued => WorkflowTaskStatus::Queued,
        WorkflowTaskState::Ready => WorkflowTaskStatus::Ready,
        WorkflowTaskState::Running => WorkflowTaskStatus::Running,
        WorkflowTaskState::WaitingForApproval => WorkflowTaskStatus::WaitingForApproval,
        WorkflowTaskState::Succeeded => WorkflowTaskStatus::Succeeded,
        WorkflowTaskState::Failed => WorkflowTaskStatus::Failed,
        WorkflowTaskState::Blocked => WorkflowTaskStatus::Blocked,
        WorkflowTaskState::Cancelled => WorkflowTaskStatus::Cancelled,
    }
}
