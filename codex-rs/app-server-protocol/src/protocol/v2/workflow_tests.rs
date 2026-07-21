use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn workflow() -> Workflow {
    Workflow {
        workflow_id: "workflow-1".to_string(),
        title: "Ship workflow API".to_string(),
        status: WorkflowStatus::Running,
        tasks: vec![WorkflowTask {
            task_id: "task-1".to_string(),
            title: "Define DTOs".to_string(),
            status: WorkflowTaskStatus::WaitingForApproval,
            summary: "Waiting for review".to_string(),
            attempt: 2,
        }],
        completed_tasks: 1,
        running_tasks: 1,
        queued_tasks: 2,
        blocked_tasks: 3,
        failed_tasks: 4,
        summary: "Workflow is active".to_string(),
    }
}

#[test]
fn workflow_start_params_serialize_camel_case_and_nullable_policy() {
    assert_eq!(
        serde_json::to_value(WorkflowStartParams {
            thread_id: "thread-1".to_string(),
            objective: "Ship workflow API".to_string(),
            policy: None,
        })
        .expect("start params should serialize"),
        json!({
            "threadId": "thread-1",
            "objective": "Ship workflow API",
            "policy": null,
        })
    );
    assert_eq!(
        serde_json::to_value(WorkflowPolicy::DeepResearch)
            .expect("deep research policy should serialize"),
        json!({"type": "deepResearch"})
    );
    assert!(
        serde_json::from_value::<WorkflowPolicy>(json!({
            "type": "saved",
            "name": "release",
        }))
        .is_err(),
        "saved definitions must run through workflow/run"
    );
}

#[test]
fn workflow_list_params_preserve_nullable_cursor_pagination() {
    assert_eq!(
        serde_json::to_value(WorkflowListParams {
            thread_id: "thread-1".to_string(),
            cursor: None,
            limit: None,
        })
        .expect("list params should serialize"),
        json!({
            "threadId": "thread-1",
            "cursor": null,
            "limit": null,
        })
    );
}

#[test]
fn workflow_run_params_serialize_nullable_task_overrides() {
    assert_eq!(
        serde_json::to_value(WorkflowRunParams {
            thread_id: "thread-1".to_string(),
            name: "release".to_string(),
            task_overrides: Some(vec![WorkflowTaskOverride {
                task_id: "task-1".to_string(),
                title: None,
                instructions: Some("Use the v2 contract".to_string()),
            }]),
        })
        .expect("run params should serialize"),
        json!({
            "threadId": "thread-1",
            "name": "release",
            "taskOverrides": [{
                "taskId": "task-1",
                "title": null,
                "instructions": "Use the v2 contract",
            }],
        })
    );
}

#[test]
fn workflow_list_items_use_tagged_camel_case_union() {
    assert_eq!(
        serde_json::to_value(WorkflowListResponse {
            data: vec![
                WorkflowListItem::Run {
                    workflow: workflow(),
                },
                WorkflowListItem::Saved {
                    workflow: SavedWorkflow {
                        name: "release".to_string(),
                        title: "Release".to_string(),
                        objective: "Ship safely".to_string(),
                        task_count: 2,
                    },
                },
            ],
            next_cursor: Some("cursor-2".to_string()),
        })
        .expect("list response should serialize"),
        json!({
            "data": [
                {
                    "type": "run",
                    "workflow": {
                        "workflowId": "workflow-1",
                        "title": "Ship workflow API",
                        "status": "running",
                        "tasks": [{
                            "taskId": "task-1",
                            "title": "Define DTOs",
                            "status": "waitingForApproval",
                            "summary": "Waiting for review",
                            "attempt": 2,
                        }],
                        "completedTasks": 1,
                        "runningTasks": 1,
                        "queuedTasks": 2,
                        "blockedTasks": 3,
                        "failedTasks": 4,
                        "summary": "Workflow is active",
                    },
                },
                {
                    "type": "saved",
                    "workflow": {
                        "name": "release",
                        "title": "Release",
                        "objective": "Ship safely",
                        "taskCount": 2,
                    },
                },
            ],
            "nextCursor": "cursor-2",
        })
    );
}

#[test]
fn workflow_response_wrappers_use_contract_field_names() {
    let workflow = workflow();
    let workflow_value = serde_json::to_value(&workflow).expect("workflow should serialize");
    let workflow_responses = [
        serde_json::to_value(WorkflowStartResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowReadResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowPauseResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowResumeResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowCancelResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowRetryResponse {
            workflow: workflow.clone(),
        }),
        serde_json::to_value(WorkflowRunResponse { workflow }),
    ];

    for response in workflow_responses {
        assert_eq!(
            response.expect("workflow response should serialize"),
            json!({"workflow": workflow_value})
        );
    }
    assert_eq!(
        serde_json::to_value(WorkflowSaveResponse {
            saved_workflow: SavedWorkflow {
                name: "release".to_string(),
                title: "Release".to_string(),
                objective: "Ship safely".to_string(),
                task_count: 2,
            },
        })
        .expect("save response should serialize"),
        json!({
            "savedWorkflow": {
                "name": "release",
                "title": "Release",
                "objective": "Ship safely",
                "taskCount": 2,
            }
        })
    );
    assert_eq!(
        serde_json::to_value(WorkflowDeleteResponse { deleted: true })
            .expect("delete response should serialize"),
        json!({"deleted": true})
    );
}

#[test]
fn workflow_progress_and_failure_notifications_use_camel_case() {
    assert_eq!(
        serde_json::to_value(WorkflowProgressNotification {
            thread_id: "thread-1".to_string(),
            workflow_id: "workflow-1".to_string(),
            status: WorkflowStatus::Consolidating,
            completed_tasks: 1,
            running_tasks: 2,
            queued_tasks: 3,
            blocked_tasks: 4,
            failed_tasks: 5,
            summary: "Consolidating results".to_string(),
        })
        .expect("progress notification should serialize"),
        json!({
            "threadId": "thread-1",
            "workflowId": "workflow-1",
            "status": "consolidating",
            "completedTasks": 1,
            "runningTasks": 2,
            "queuedTasks": 3,
            "blockedTasks": 4,
            "failedTasks": 5,
            "summary": "Consolidating results",
        })
    );
    assert_eq!(
        serde_json::to_value(WorkflowFailedNotification {
            thread_id: "thread-1".to_string(),
            workflow: workflow(),
            error: "planner failed".to_string(),
        })
        .expect("failed notification should serialize"),
        json!({
            "threadId": "thread-1",
            "workflow": serde_json::to_value(workflow()).expect("workflow should serialize"),
            "error": "planner failed",
        })
    );
}
