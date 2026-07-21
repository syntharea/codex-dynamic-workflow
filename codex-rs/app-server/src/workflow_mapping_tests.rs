use codex_core::workflows::SavedWorkflowSummary;
use codex_protocol::workflow::WorkflowId;
use codex_protocol::workflow::WorkflowStatus as CoreWorkflowStatus;
use codex_protocol::workflow::WorkflowSummary;
use codex_protocol::workflow::WorkflowTaskStatus as CoreWorkflowTaskStatus;
use codex_protocol::workflow::WorkflowTaskSummary;
use codex_protocol::workflow::WorkflowText;
use codex_protocol::workflow::WorkflowTitle;
use pretty_assertions::assert_eq;

use super::saved_workflow_from_core;
use super::workflow_from_core;

#[test]
fn maps_core_workflow_and_saved_summary() {
    let core = WorkflowSummary::try_new(
        WorkflowId::new("workflow-1").expect("workflow id"),
        WorkflowTitle::new("Release").expect("workflow title"),
        CoreWorkflowStatus::Running,
        vec![
            WorkflowTaskSummary::try_new(
                "verify",
                WorkflowTitle::new("Verify").expect("task title"),
                CoreWorkflowTaskStatus::WaitingForApproval,
                WorkflowText::new("Awaiting approval").expect("task summary"),
                /*attempt*/ 2,
            )
            .expect("task summary"),
        ],
        /*completed_tasks*/ 1,
        /*running_tasks*/ 1,
        /*queued_tasks*/ 2,
        /*blocked_tasks*/ 3,
        /*failed_tasks*/ 4,
        WorkflowText::new("Running checks").expect("workflow summary"),
    )
    .expect("workflow summary");

    assert_eq!(
        workflow_from_core(&core),
        codex_app_server_protocol::Workflow {
            workflow_id: "workflow-1".to_string(),
            title: "Release".to_string(),
            status: codex_app_server_protocol::WorkflowStatus::Running,
            tasks: vec![codex_app_server_protocol::WorkflowTask {
                task_id: "verify".to_string(),
                title: "Verify".to_string(),
                status: codex_app_server_protocol::WorkflowTaskStatus::WaitingForApproval,
                summary: "Awaiting approval".to_string(),
                attempt: 2,
            }],
            completed_tasks: 1,
            running_tasks: 1,
            queued_tasks: 2,
            blocked_tasks: 3,
            failed_tasks: 4,
            summary: "Running checks".to_string(),
        }
    );

    assert_eq!(
        saved_workflow_from_core(SavedWorkflowSummary {
            name: "release".to_string(),
            title: "Release".to_string(),
            objective: "Ship safely".to_string(),
            task_count: 3,
        }),
        codex_app_server_protocol::SavedWorkflow {
            name: "release".to_string(),
            title: "Release".to_string(),
            objective: "Ship safely".to_string(),
            task_count: 3,
        }
    );
}
