use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use codex_core::workflows::manager_for_thread;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::protocol::Op;
use codex_workflows::WorkflowAttemptSnapshot;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTaskState;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::sleep;

use super::fixtures::body_contains;
use super::fixtures::request_has_input_type;
use super::fixtures::resumable_snapshot;
use super::fixtures::write_rollout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_restores_completed_tasks_and_requeues_unconfirmed_work() -> Result<()> {
    let server = start_mock_server().await;
    let root_thread_id = ThreadId::new();
    let mut snapshot = resumable_snapshot()?;
    let completed = snapshot.task("completed").expect("completed task").clone();
    let running = snapshot.task("running").expect("running task").clone();
    let ready = snapshot.task("ready").expect("ready task").clone();
    snapshot.execution_attempt = 2;
    snapshot.attempt_history.push(WorkflowAttemptSnapshot {
        execution_attempt: 1,
        state: WorkflowState::Failed,
        tasks: snapshot.tasks.clone(),
    });

    let home = Arc::new(TempDir::new()?);
    let rollout_path = write_rollout(home.as_ref(), root_thread_id, &snapshot)?;
    let mut builder = test_codex().with_config(|config| {
        config.agents_enabled = true;
        config
            .features
            .enable(Feature::Collab)
            .expect("enable collaboration");
        config
            .features
            .enable(Feature::DynamicWorkflows)
            .expect("enable dynamic workflows");
    });
    let resumed = builder.resume(&server, home, rollout_path).await?;

    let restored = manager_for_thread(&resumed.codex)
        .snapshot(snapshot.workflow_id)
        .await?;
    assert_eq!(
        restored.task("completed").expect("completed task"),
        &completed
    );
    assert_eq!(
        restored.task("running").expect("running task").state,
        WorkflowTaskState::Queued
    );
    assert_eq!(
        restored.task("running").expect("running task").agent_id,
        None
    );
    assert_eq!(
        restored.task("running").expect("running task").attempts,
        running.attempts
    );
    assert_eq!(
        restored.task("ready").expect("ready task").state,
        WorkflowTaskState::Queued
    );
    assert_eq!(restored.task("ready").expect("ready task").agent_id, None);
    assert_eq!(
        restored.task("ready").expect("ready task").attempts,
        ready.attempts
    );
    assert_eq!(restored.execution_attempt, 2);
    assert_eq!(restored.attempt_history, snapshot.attempt_history);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_resume_op_dispatches_queued_tasks_with_dependencies() -> Result<()> {
    let server = start_mock_server().await;
    for task_id in ["running", "ready"] {
        let (response_id, message_id, completed_id) = match task_id {
            "running" => (
                "resume-running-response",
                "resume-running-message",
                "resume-running-completed",
            ),
            "ready" => (
                "resume-ready-response",
                "resume-ready-message",
                "resume-ready-completed",
            ),
            _ => unreachable!("test task id"),
        };
        mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                request_has_input_type(request, "agent_message")
                    && body_contains(request, &format!("run {task_id}"))
            },
            sse(vec![
                ev_response_created(response_id),
                ev_assistant_message(message_id, "task complete"),
                ev_completed(completed_id),
            ]),
        )
        .await;
    }
    let root_thread_id = ThreadId::new();
    let snapshot = resumable_snapshot()?;
    let home = Arc::new(TempDir::new()?);
    let rollout_path = write_rollout(home.as_ref(), root_thread_id, &snapshot)?;
    let mut builder = test_codex().with_config(|config| {
        config.agents_enabled = true;
        config
            .features
            .enable(Feature::Collab)
            .expect("enable collaboration");
        config
            .features
            .enable(Feature::DynamicWorkflows)
            .expect("enable dynamic workflows");
    });
    let resumed = builder.resume(&server, home, rollout_path).await?;
    resumed
        .codex
        .submit(Op::WorkflowResume(
            codex_protocol::workflow::WorkflowResumeParams {
                workflow_id: serde_json::from_value(serde_json::Value::String(
                    snapshot.workflow_id.to_string(),
                ))?,
            },
        ))
        .await?;
    let manager = manager_for_thread(&resumed.codex);
    sleep(Duration::from_millis(500)).await;
    let restored = manager.snapshot(snapshot.workflow_id).await?;
    assert_eq!(restored.state, WorkflowState::Running);
    assert_eq!(
        restored
            .tasks
            .iter()
            .filter(|task| task.task_id.as_str() != "completed")
            .count(),
        2
    );
    assert!(
        restored
            .tasks
            .iter()
            .filter(|task| task.task_id.as_str() != "completed")
            .all(|task| task.agent_id.is_some())
    );
    manager.cancel(snapshot.workflow_id).await?;
    Ok(())
}
