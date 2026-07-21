use codex_app_server_protocol::Workflow;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowTaskStatus;

use crate::exec_events::WorkflowEventSnapshot;
use crate::exec_events::WorkflowEventStatus;
use crate::exec_events::WorkflowEventTask;
use crate::exec_events::WorkflowProgressEvent;
use crate::exec_events::WorkflowTaskEventStatus;

const WORKFLOW_EVENT_ID_MAX_BYTES: usize = 256;
const WORKFLOW_EVENT_TEXT_MAX_BYTES: usize = 4 * 1024;
const WORKFLOW_EVENT_TASKS_MAX: usize = 256;

pub(crate) fn workflow_snapshot(workflow: Workflow) -> WorkflowEventSnapshot {
    WorkflowEventSnapshot {
        workflow_id: bounded_id(workflow.workflow_id),
        title: bounded_text(workflow.title),
        status: workflow_status(workflow.status),
        tasks: workflow
            .tasks
            .into_iter()
            .take(WORKFLOW_EVENT_TASKS_MAX)
            .map(|task| WorkflowEventTask {
                task_id: bounded_id(task.task_id),
                title: bounded_text(task.title),
                status: workflow_task_status(task.status),
                summary: bounded_text(task.summary),
                attempt: task.attempt,
            })
            .collect(),
        completed_tasks: workflow.completed_tasks,
        running_tasks: workflow.running_tasks,
        queued_tasks: workflow.queued_tasks,
        blocked_tasks: workflow.blocked_tasks,
        failed_tasks: workflow.failed_tasks,
        summary: bounded_text(workflow.summary),
    }
}

pub(crate) fn workflow_progress(
    notification: WorkflowProgressNotification,
) -> WorkflowProgressEvent {
    WorkflowProgressEvent {
        thread_id: bounded_id(notification.thread_id),
        workflow_id: bounded_id(notification.workflow_id),
        status: workflow_status(notification.status),
        completed_tasks: notification.completed_tasks,
        running_tasks: notification.running_tasks,
        queued_tasks: notification.queued_tasks,
        blocked_tasks: notification.blocked_tasks,
        failed_tasks: notification.failed_tasks,
        summary: bounded_text(notification.summary),
    }
}

pub(crate) fn bounded_id(value: String) -> String {
    truncate_utf8(value, WORKFLOW_EVENT_ID_MAX_BYTES)
}

pub(crate) fn bounded_text(value: String) -> String {
    truncate_utf8(value, WORKFLOW_EVENT_TEXT_MAX_BYTES)
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn workflow_status(status: WorkflowStatus) -> WorkflowEventStatus {
    match status {
        WorkflowStatus::Planning => WorkflowEventStatus::Planning,
        WorkflowStatus::Running => WorkflowEventStatus::Running,
        WorkflowStatus::Pausing => WorkflowEventStatus::Pausing,
        WorkflowStatus::Paused => WorkflowEventStatus::Paused,
        WorkflowStatus::Consolidating => WorkflowEventStatus::Consolidating,
        WorkflowStatus::Completed => WorkflowEventStatus::Completed,
        WorkflowStatus::Failed => WorkflowEventStatus::Failed,
        WorkflowStatus::Cancelled => WorkflowEventStatus::Cancelled,
    }
}

fn workflow_task_status(status: WorkflowTaskStatus) -> WorkflowTaskEventStatus {
    match status {
        WorkflowTaskStatus::Queued => WorkflowTaskEventStatus::Queued,
        WorkflowTaskStatus::Ready => WorkflowTaskEventStatus::Ready,
        WorkflowTaskStatus::Running => WorkflowTaskEventStatus::Running,
        WorkflowTaskStatus::WaitingForApproval => WorkflowTaskEventStatus::WaitingForApproval,
        WorkflowTaskStatus::Succeeded => WorkflowTaskEventStatus::Succeeded,
        WorkflowTaskStatus::Failed => WorkflowTaskEventStatus::Failed,
        WorkflowTaskStatus::Blocked => WorkflowTaskEventStatus::Blocked,
        WorkflowTaskStatus::Cancelled => WorkflowTaskEventStatus::Cancelled,
    }
}
