use codex_app_server_protocol::SavedWorkflow;
use codex_app_server_protocol::Workflow;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowTask;
use codex_app_server_protocol::WorkflowTaskStatus;
use codex_core::workflows::SavedWorkflowSummary;
use codex_protocol::workflow::WorkflowStatus as CoreWorkflowStatus;
use codex_protocol::workflow::WorkflowSummary;
use codex_protocol::workflow::WorkflowTaskStatus as CoreWorkflowTaskStatus;

pub(crate) fn workflow_from_core(workflow: &WorkflowSummary) -> Workflow {
    Workflow {
        workflow_id: workflow.workflow_id().to_string(),
        title: workflow.title().as_str().to_string(),
        status: workflow_status_from_core(workflow.status()),
        tasks: workflow
            .tasks()
            .iter()
            .map(|task| WorkflowTask {
                task_id: task.task_id().to_string(),
                title: task.title().as_str().to_string(),
                status: workflow_task_status_from_core(task.status()),
                summary: task.summary().as_str().to_string(),
                attempt: task.attempt(),
            })
            .collect(),
        completed_tasks: workflow.completed_tasks(),
        running_tasks: workflow.running_tasks(),
        queued_tasks: workflow.queued_tasks(),
        blocked_tasks: workflow.blocked_tasks(),
        failed_tasks: workflow.failed_tasks(),
        summary: workflow.summary().as_str().to_string(),
    }
}

pub(crate) fn saved_workflow_from_core(workflow: SavedWorkflowSummary) -> SavedWorkflow {
    SavedWorkflow {
        name: workflow.name,
        title: workflow.title,
        objective: workflow.objective,
        task_count: workflow.task_count,
    }
}

pub(crate) fn workflow_status_from_core(status: CoreWorkflowStatus) -> WorkflowStatus {
    match status {
        CoreWorkflowStatus::Planning => WorkflowStatus::Planning,
        CoreWorkflowStatus::Running => WorkflowStatus::Running,
        CoreWorkflowStatus::Pausing => WorkflowStatus::Pausing,
        CoreWorkflowStatus::Paused => WorkflowStatus::Paused,
        CoreWorkflowStatus::Consolidating => WorkflowStatus::Consolidating,
        CoreWorkflowStatus::Completed => WorkflowStatus::Completed,
        CoreWorkflowStatus::Failed => WorkflowStatus::Failed,
        CoreWorkflowStatus::Cancelled => WorkflowStatus::Cancelled,
    }
}

fn workflow_task_status_from_core(status: CoreWorkflowTaskStatus) -> WorkflowTaskStatus {
    match status {
        CoreWorkflowTaskStatus::Queued => WorkflowTaskStatus::Queued,
        CoreWorkflowTaskStatus::Ready => WorkflowTaskStatus::Ready,
        CoreWorkflowTaskStatus::Running => WorkflowTaskStatus::Running,
        CoreWorkflowTaskStatus::WaitingForApproval => WorkflowTaskStatus::WaitingForApproval,
        CoreWorkflowTaskStatus::Succeeded => WorkflowTaskStatus::Succeeded,
        CoreWorkflowTaskStatus::Failed => WorkflowTaskStatus::Failed,
        CoreWorkflowTaskStatus::Blocked => WorkflowTaskStatus::Blocked,
        CoreWorkflowTaskStatus::Cancelled => WorkflowTaskStatus::Cancelled,
    }
}

#[cfg(test)]
#[path = "workflow_mapping_tests.rs"]
mod tests;
