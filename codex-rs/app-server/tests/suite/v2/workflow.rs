use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SavedWorkflow;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::Workflow;
use codex_app_server_protocol::WorkflowCancelParams;
use codex_app_server_protocol::WorkflowCancelResponse;
use codex_app_server_protocol::WorkflowCancelledNotification;
use codex_app_server_protocol::WorkflowCompletedNotification;
use codex_app_server_protocol::WorkflowDeleteParams;
use codex_app_server_protocol::WorkflowDeleteResponse;
use codex_app_server_protocol::WorkflowFailedNotification;
use codex_app_server_protocol::WorkflowListItem;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPauseParams;
use codex_app_server_protocol::WorkflowPauseResponse;
use codex_app_server_protocol::WorkflowPausedNotification;
use codex_app_server_protocol::WorkflowPolicy;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowResumeParams;
use codex_app_server_protocol::WorkflowResumeResponse;
use codex_app_server_protocol::WorkflowRetryParams;
use codex_app_server_protocol::WorkflowRetryResponse;
use codex_app_server_protocol::WorkflowRunParams;
use codex_app_server_protocol::WorkflowRunResponse;
use codex_app_server_protocol::WorkflowSaveParams;
use codex_app_server_protocol::WorkflowSaveResponse;
use codex_app_server_protocol::WorkflowStartParams;
use codex_app_server_protocol::WorkflowStartResponse;
use codex_app_server_protocol::WorkflowStartedNotification;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowTaskOverride;
use codex_app_server_protocol::WorkflowTaskStatus;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ResponseMock;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::sleep;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const SLOW_WORKER_DELAY: Duration = Duration::from_secs(30);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INVALID_PARAMS_ERROR_CODE: i64 = -32602;
const WORKFLOW_TEXT_MAX_BYTES: usize = 4 * 1024;
const WORKFLOW_OBJECTIVE_MAX_BYTES: usize = 16 * 1024;

#[tokio::test]
async fn workflow_start_returns_planning_and_emits_started_then_completed() -> Result<()> {
    let objective = "exercise workflow start";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &mock_server,
        vec![
            assistant_response(
                "planner-invalid",
                &workflow_plan(objective, "execute public task", &["missing"]),
            ),
            assistant_response(
                "planner-repaired",
                &workflow_plan(objective, "execute public task", &[]),
            ),
            assistant_response("worker", "public task complete"),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let thread_id = start_thread(&mut harness.app_server).await?;

    let start = start_workflow(&mut harness.app_server, &thread_id, objective).await?;
    let workflow_id = start.workflow.workflow_id.clone();
    assert_eq!(
        start.workflow,
        Workflow {
            workflow_id: workflow_id.clone(),
            title: "Planning workflow".to_string(),
            status: WorkflowStatus::Planning,
            tasks: Vec::new(),
            completed_tasks: 0,
            running_tasks: 0,
            queued_tasks: 0,
            blocked_tasks: 0,
            failed_tasks: 0,
            summary: String::new(),
        }
    );

    let started: WorkflowStartedNotification =
        workflow_notification(&mut harness.app_server, "workflow/started", &workflow_id).await?;
    assert_eq!(
        started,
        WorkflowStartedNotification {
            thread_id: thread_id.clone(),
            workflow: start.workflow,
        }
    );

    let completed: WorkflowCompletedNotification =
        workflow_notification(&mut harness.app_server, "workflow/completed", &workflow_id).await?;
    assert_eq!(completed.thread_id, thread_id);
    assert_eq!(completed.workflow.workflow_id, workflow_id);
    assert_eq!(completed.workflow.status, WorkflowStatus::Completed);
    assert_eq!(completed.workflow.completed_tasks, 1);
    assert_eq!(completed.workflow.tasks.len(), 1);
    assert_eq!(
        completed.workflow.tasks[0].status,
        WorkflowTaskStatus::Succeeded
    );

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].body_contains_text("depends on missing task missing"));
    assert!(requests[2].body_contains_text("execute public task"));
    Ok(())
}

#[tokio::test]
async fn workflow_read_and_list_return_running_run_and_paginated_saved_item() -> Result<()> {
    let objective = "inspect a running workflow";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_response_sequence(
        &mock_server,
        vec![
            responses::sse_response(assistant_response(
                "planner",
                &workflow_plan(objective, "hold running task", &[]),
            )),
            responses::sse_response(assistant_response("slow-worker", "too late"))
                .set_delay(SLOW_WORKER_DELAY),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let thread_id = start_thread(&mut harness.app_server).await?;
    let start = start_workflow(&mut harness.app_server, &thread_id, objective).await?;
    let workflow_id = start.workflow.workflow_id;

    workflow_progress(
        &mut harness.app_server,
        &workflow_id,
        WorkflowStatus::Running,
    )
    .await?;
    wait_for_request_count(&response_mock, /*expected_count*/ 2).await?;

    let read: WorkflowReadResponse = rpc(
        &mut harness.app_server,
        "workflow/read",
        WorkflowReadParams {
            thread_id: thread_id.clone(),
            workflow_id: workflow_id.clone(),
        },
    )
    .await?;
    assert_eq!(read.workflow.workflow_id, workflow_id);
    assert_eq!(read.workflow.status, WorkflowStatus::Running);
    assert_eq!(read.workflow.running_tasks, 1);
    assert_eq!(read.workflow.tasks.len(), 1);
    assert!(matches!(
        read.workflow.tasks[0].status,
        WorkflowTaskStatus::Ready | WorkflowTaskStatus::Running
    ));

    let saved: WorkflowSaveResponse = rpc(
        &mut harness.app_server,
        "workflow/save",
        WorkflowSaveParams {
            thread_id: thread_id.clone(),
            workflow_id: workflow_id.clone(),
            name: "running-copy".to_string(),
        },
    )
    .await?;
    assert_eq!(
        saved.saved_workflow,
        SavedWorkflow {
            name: "running-copy".to_string(),
            title: "Public workflow".to_string(),
            objective: objective.to_string(),
            task_count: 1,
        }
    );

    let first_page: WorkflowListResponse = rpc(
        &mut harness.app_server,
        "workflow/list",
        WorkflowListParams {
            thread_id: thread_id.clone(),
            cursor: None,
            limit: Some(1),
        },
    )
    .await?;
    assert_eq!(
        first_page.data,
        vec![WorkflowListItem::Run {
            workflow: read.workflow,
        }]
    );
    assert_eq!(first_page.next_cursor, Some("1".to_string()));

    let second_page: WorkflowListResponse = rpc(
        &mut harness.app_server,
        "workflow/list",
        WorkflowListParams {
            thread_id: thread_id.clone(),
            cursor: first_page.next_cursor,
            limit: Some(1),
        },
    )
    .await?;
    assert_eq!(
        second_page.data,
        vec![WorkflowListItem::Saved {
            workflow: saved.saved_workflow,
        }]
    );
    assert_eq!(second_page.next_cursor, None);

    let cancelled: WorkflowCancelResponse = rpc(
        &mut harness.app_server,
        "workflow/cancel",
        WorkflowCancelParams {
            thread_id: thread_id.clone(),
            workflow_id: workflow_id.clone(),
        },
    )
    .await?;
    assert_eq!(cancelled.workflow.status, WorkflowStatus::Cancelled);
    let notification: WorkflowCancelledNotification =
        workflow_notification(&mut harness.app_server, "workflow/cancelled", &workflow_id).await?;
    assert_eq!(notification.thread_id, thread_id);
    assert_eq!(notification.workflow, cancelled.workflow);
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn workflow_pause_and_resume_reach_paused_then_completed() -> Result<()> {
    let objective = "pause and resume a workflow";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_response_sequence(
        &mock_server,
        vec![
            responses::sse_response(assistant_response(
                "planner",
                &workflow_plan(objective, "interruptible task", &[]),
            )),
            responses::sse_response(assistant_response("interrupted-worker", "too late"))
                .set_delay(SLOW_WORKER_DELAY),
            responses::sse_response(assistant_response(
                "resumed-worker",
                "resumed task complete",
            )),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let thread_id = start_thread(&mut harness.app_server).await?;
    let start = start_workflow(&mut harness.app_server, &thread_id, objective).await?;
    let workflow_id = start.workflow.workflow_id;

    workflow_progress(
        &mut harness.app_server,
        &workflow_id,
        WorkflowStatus::Running,
    )
    .await?;
    wait_for_request_count(&response_mock, /*expected_count*/ 2).await?;

    let paused: WorkflowPauseResponse = rpc(
        &mut harness.app_server,
        "workflow/pause",
        WorkflowPauseParams {
            thread_id: thread_id.clone(),
            workflow_id: workflow_id.clone(),
        },
    )
    .await?;
    assert_eq!(paused.workflow.status, WorkflowStatus::Pausing);

    let paused_notification: WorkflowPausedNotification =
        workflow_notification(&mut harness.app_server, "workflow/paused", &workflow_id).await?;
    assert_eq!(paused_notification.thread_id, thread_id);
    assert_eq!(paused_notification.workflow.status, WorkflowStatus::Paused);

    let resumed: WorkflowResumeResponse = rpc(
        &mut harness.app_server,
        "workflow/resume",
        WorkflowResumeParams {
            thread_id: paused_notification.thread_id,
            workflow_id: workflow_id.clone(),
        },
    )
    .await?;
    assert_eq!(resumed.workflow.status, WorkflowStatus::Running);

    let completed: WorkflowCompletedNotification =
        workflow_notification(&mut harness.app_server, "workflow/completed", &workflow_id).await?;
    assert_eq!(completed.workflow.status, WorkflowStatus::Completed);
    assert_eq!(response_mock.requests().len(), 3);
    Ok(())
}

#[tokio::test]
async fn workflow_retry_restarts_failed_run_and_maps_invalid_state_error() -> Result<()> {
    let objective = "retry a failed workflow";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &mock_server,
        vec![
            assistant_response("planner", &workflow_plan(objective, "retryable task", &[])),
            responses::sse_failed("failed-worker", "server_error", "worker failed"),
            assistant_response("retried-worker", "retry complete"),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let thread_id = start_thread(&mut harness.app_server).await?;
    let start = start_workflow(&mut harness.app_server, &thread_id, objective).await?;
    let workflow_id = start.workflow.workflow_id;

    let failed: WorkflowFailedNotification =
        workflow_notification(&mut harness.app_server, "workflow/failed", &workflow_id).await?;
    assert_eq!(failed.thread_id, thread_id);
    assert_eq!(failed.workflow.status, WorkflowStatus::Failed);
    assert!(failed.error.contains("worker failed"));

    let retried: WorkflowRetryResponse = rpc(
        &mut harness.app_server,
        "workflow/retry",
        WorkflowRetryParams {
            thread_id: failed.thread_id,
            workflow_id: workflow_id.clone(),
        },
    )
    .await?;
    assert_eq!(retried.workflow.status, WorkflowStatus::Running);

    let completed: WorkflowCompletedNotification =
        workflow_notification(&mut harness.app_server, "workflow/completed", &workflow_id).await?;
    assert_eq!(completed.workflow.status, WorkflowStatus::Completed);
    assert_rpc_error(
        &mut harness.app_server,
        "workflow/retry",
        WorkflowRetryParams {
            thread_id: completed.thread_id,
            workflow_id,
        },
        INVALID_REQUEST_ERROR_CODE,
        "only failed workflows can be retried",
    )
    .await?;
    assert_eq!(response_mock.requests().len(), 3);
    Ok(())
}

#[tokio::test]
async fn workflow_save_run_and_delete_use_saved_summary_and_fresh_run_id() -> Result<()> {
    let objective = "save and rerun a workflow";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &mock_server,
        vec![
            assistant_response(
                "planner",
                &workflow_plan(objective, "original task instructions", &[]),
            ),
            assistant_response("source-worker", "source complete"),
            assistant_response("saved-worker", "saved run complete"),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let thread_id = start_thread(&mut harness.app_server).await?;
    let source = start_workflow(&mut harness.app_server, &thread_id, objective).await?;
    let source_workflow_id = source.workflow.workflow_id;
    let _: WorkflowCompletedNotification = workflow_notification(
        &mut harness.app_server,
        "workflow/completed",
        &source_workflow_id,
    )
    .await?;

    let saved: WorkflowSaveResponse = rpc(
        &mut harness.app_server,
        "workflow/save",
        WorkflowSaveParams {
            thread_id: thread_id.clone(),
            workflow_id: source_workflow_id.clone(),
            name: "release".to_string(),
        },
    )
    .await?;
    assert_eq!(
        saved.saved_workflow,
        SavedWorkflow {
            name: "release".to_string(),
            title: "Public workflow".to_string(),
            objective: objective.to_string(),
            task_count: 1,
        }
    );

    let run: WorkflowRunResponse = rpc(
        &mut harness.app_server,
        "workflow/run",
        WorkflowRunParams {
            thread_id: thread_id.clone(),
            name: saved.saved_workflow.name.clone(),
            task_overrides: Some(vec![WorkflowTaskOverride {
                task_id: "task".to_string(),
                title: Some("Overridden task".to_string()),
                instructions: Some("overridden task instructions".to_string()),
            }]),
        },
    )
    .await?;
    let run_workflow_id = run.workflow.workflow_id.clone();
    assert_ne!(run_workflow_id, source_workflow_id);
    assert_eq!(run.workflow.status, WorkflowStatus::Running);

    let started: WorkflowStartedNotification = workflow_notification(
        &mut harness.app_server,
        "workflow/started",
        &run_workflow_id,
    )
    .await?;
    assert_eq!(started.thread_id, thread_id);
    assert_eq!(started.workflow, run.workflow);

    let completed: WorkflowCompletedNotification = workflow_notification(
        &mut harness.app_server,
        "workflow/completed",
        &run_workflow_id,
    )
    .await?;
    assert_eq!(completed.workflow.status, WorkflowStatus::Completed);

    let deleted: WorkflowDeleteResponse = rpc(
        &mut harness.app_server,
        "workflow/delete",
        WorkflowDeleteParams {
            thread_id: thread_id.clone(),
            name: "release".to_string(),
        },
    )
    .await?;
    assert_eq!(deleted, WorkflowDeleteResponse { deleted: true });
    let deleted_again: WorkflowDeleteResponse = rpc(
        &mut harness.app_server,
        "workflow/delete",
        WorkflowDeleteParams {
            thread_id,
            name: "release".to_string(),
        },
    )
    .await?;
    assert_eq!(deleted_again, WorkflowDeleteResponse { deleted: false });

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].body_contains_text("overridden task instructions"));
    Ok(())
}

#[tokio::test]
async fn workflow_rpc_maps_unknown_foreign_and_invalid_inputs_to_jsonrpc_errors() -> Result<()> {
    let objective = "validate workflow errors";
    let mock_server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &mock_server,
        vec![
            assistant_response(
                "planner",
                &workflow_plan(objective, "valid task instructions", &[]),
            ),
            assistant_response("worker", "validation source complete"),
        ],
    )
    .await;
    let mut harness = workflow_harness(&mock_server).await?;
    let owner_thread_id = start_thread(&mut harness.app_server).await?;
    let foreign_thread_id = start_thread(&mut harness.app_server).await?;
    let source = start_workflow(&mut harness.app_server, &owner_thread_id, objective).await?;
    let workflow_id = source.workflow.workflow_id;
    let _: WorkflowCompletedNotification =
        workflow_notification(&mut harness.app_server, "workflow/completed", &workflow_id).await?;
    let _: WorkflowSaveResponse = rpc(
        &mut harness.app_server,
        "workflow/save",
        WorkflowSaveParams {
            thread_id: owner_thread_id.clone(),
            workflow_id: workflow_id.clone(),
            name: "validation-source".to_string(),
        },
    )
    .await?;

    assert_rpc_error(
        &mut harness.app_server,
        "workflow/list",
        WorkflowListParams {
            thread_id: "00000000-0000-4000-8000-000000000000".to_string(),
            cursor: None,
            limit: None,
        },
        INVALID_REQUEST_ERROR_CODE,
        "thread not found",
    )
    .await?;
    assert_rpc_error(
        &mut harness.app_server,
        "workflow/read",
        WorkflowReadParams {
            thread_id: foreign_thread_id,
            workflow_id: workflow_id.clone(),
        },
        INVALID_REQUEST_ERROR_CODE,
        "unknown workflow",
    )
    .await?;
    assert_rpc_error(
        &mut harness.app_server,
        "workflow/start",
        WorkflowStartParams {
            thread_id: owner_thread_id.clone(),
            objective: "x".repeat(WORKFLOW_OBJECTIVE_MAX_BYTES + 1),
            policy: None,
        },
        INVALID_PARAMS_ERROR_CODE,
        "workflow objective exceeds 16384 bytes",
    )
    .await?;
    assert_rpc_error(
        &mut harness.app_server,
        "workflow/run",
        WorkflowRunParams {
            thread_id: owner_thread_id.clone(),
            name: "validation-source".to_string(),
            task_overrides: Some(vec![WorkflowTaskOverride {
                task_id: "task".to_string(),
                title: None,
                instructions: Some("x".repeat(WORKFLOW_TEXT_MAX_BYTES + 1)),
            }]),
        },
        INVALID_PARAMS_ERROR_CODE,
        "workflow text exceeds 4096 bytes",
    )
    .await?;
    assert_rpc_error(
        &mut harness.app_server,
        "workflow/run",
        WorkflowRunParams {
            thread_id: owner_thread_id,
            name: "validation-source".to_string(),
            task_overrides: Some(vec![WorkflowTaskOverride {
                task_id: "missing".to_string(),
                title: None,
                instructions: None,
            }]),
        },
        INVALID_REQUEST_ERROR_CODE,
        "unknown task id",
    )
    .await?;
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

struct WorkflowHarness {
    app_server: TestAppServer,
    _codex_home: TempDir,
}

async fn workflow_harness(mock_server: &MockServer) -> Result<WorkflowHarness> {
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &mock_server.uri(),
        &BTreeMap::from([
            (Feature::DynamicWorkflows, true),
            (Feature::Collab, true),
            (Feature::MultiAgentV2, false),
        ]),
        /*auto_compact_limit*/ 100_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, app_server.initialize()).await??;
    Ok(WorkflowHarness {
        app_server,
        _codex_home: codex_home,
    })
}

async fn start_thread(app_server: &mut TestAppServer) -> Result<String> {
    let request_id = app_server
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    Ok(to_response::<ThreadStartResponse>(response)?.thread.id)
}

async fn start_workflow(
    app_server: &mut TestAppServer,
    thread_id: &str,
    objective: &str,
) -> Result<WorkflowStartResponse> {
    rpc(
        app_server,
        "workflow/start",
        WorkflowStartParams {
            thread_id: thread_id.to_string(),
            objective: objective.to_string(),
            policy: Some(WorkflowPolicy::Ultracode),
        },
    )
    .await
}

async fn rpc<Params, Response>(
    app_server: &mut TestAppServer,
    method: &str,
    params: Params,
) -> Result<Response>
where
    Params: Serialize,
    Response: DeserializeOwned,
{
    let request_id = app_server
        .send_raw_request(method, Some(serde_json::to_value(params)?))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

async fn assert_rpc_error<Params>(
    app_server: &mut TestAppServer,
    method: &str,
    params: Params,
    expected_code: i64,
    expected_message: &str,
) -> Result<()>
where
    Params: Serialize,
{
    let request_id = app_server
        .send_raw_request(method, Some(serde_json::to_value(params)?))
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, expected_code);
    assert!(
        error.error.message.contains(expected_message),
        "unexpected {method} error: {}",
        error.error.message
    );
    Ok(())
}

async fn workflow_notification<Notification>(
    app_server: &mut TestAppServer,
    method: &str,
    workflow_id: &str,
) -> Result<Notification>
where
    Notification: DeserializeOwned,
{
    let description = format!("{method} for workflow {workflow_id}");
    let notification: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(&description, |notification| {
            notification.method == method
                && notification.params.as_ref().is_some_and(|params| {
                    params
                        .pointer("/workflow/workflowId")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| params.get("workflowId").and_then(serde_json::Value::as_str))
                        == Some(workflow_id)
                })
        }),
    )
    .await??;
    let params = notification
        .params
        .with_context(|| format!("{method} notification must include params"))?;
    Ok(serde_json::from_value(params)?)
}

async fn workflow_progress(
    app_server: &mut TestAppServer,
    workflow_id: &str,
    status: WorkflowStatus,
) -> Result<WorkflowProgressNotification> {
    let description = format!("workflow/progress for workflow {workflow_id} in {status:?}");
    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_matching_notification(&description, |notification| {
            if notification.method != "workflow/progress" {
                return false;
            }
            notification
                .params
                .as_ref()
                .and_then(|params| {
                    serde_json::from_value::<WorkflowProgressNotification>(params.clone()).ok()
                })
                .is_some_and(|progress| {
                    progress.workflow_id == workflow_id && progress.status == status
                })
        }),
    )
    .await??;
    let params = notification
        .params
        .context("workflow/progress notification must include params")?;
    Ok(serde_json::from_value(params)?)
}

async fn wait_for_request_count(response_mock: &ResponseMock, expected_count: usize) -> Result<()> {
    timeout(DEFAULT_READ_TIMEOUT, async {
        while response_mock.requests().len() < expected_count {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {expected_count} Responses API requests"))?;
    Ok(())
}

fn workflow_plan(objective: &str, instructions: &str, dependencies: &[&str]) -> String {
    json!({
        "schema_version": 1,
        "title": "Public workflow",
        "objective": objective,
        "tasks": [{
            "id": "task",
            "title": "Public task",
            "instructions": instructions,
            "dependencies": dependencies,
            "agent_role": null,
            "intent": "read_only",
            "owned_paths": [],
            "verification": [],
            "max_attempts": 1,
        }],
    })
    .to_string()
}

fn assistant_response(id: &str, message: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_assistant_message(&format!("{id}-message"), message),
        responses::ev_completed(id),
    ])
}
