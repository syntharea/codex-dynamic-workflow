use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use codex_core::workflows::manager_for_thread;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_workflows::WORKFLOW_DISPATCH_MARKER_PREFIX;
use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskState;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

use super::fixtures::append_rollout_item;
use super::fixtures::assert_requeued;
use super::fixtures::bind_running_task;
use super::fixtures::body_contains;
use super::fixtures::completed_collab_child;
use super::fixtures::dispatch_marker;
use super::fixtures::insert_dispatch_marker;
use super::fixtures::is_dispatch_marker;
use super::fixtures::persisted_active_binding;
use super::fixtures::read_rollout_items;
use super::fixtures::resumable_snapshot;
use super::fixtures::resume_builder;
use super::fixtures::resume_fixture;
use super::fixtures::write_rollout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_rejects_manually_bound_collab_child_terminal() -> Result<()> {
    let fixture = completed_collab_child().await?;
    let mut snapshot = resumable_snapshot()?;
    bind_running_task(&mut snapshot, fixture.worker_thread_id);
    let resumed = resume_fixture(fixture, &snapshot).await?;
    let restored = manager_for_thread(&resumed.codex)
        .snapshot(snapshot.workflow_id)
        .await?;
    assert_requeued(&restored, "running");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_reconciles_terminal_from_persisted_workflow_dispatch() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "durable workflow resume"),
        sse(vec![
            ev_response_created("workflow-planner"),
            ev_assistant_message(
                "workflow-plan",
                r#"{"schema_version":1,"title":"durable","objective":"durable workflow resume","tasks":[{"id":"durable","title":"durable","instructions":"finish durable workflow task","dependencies":[],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
            ),
            ev_completed("workflow-planner"),
        ]),
    )
    .await;
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let initial = test_codex()
        .with_auth(auth)
        .with_config(|config| {
            config.agents_enabled = true;
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collaboration");
            config
                .features
                .enable(Feature::DynamicWorkflows)
                .expect("enable dynamic workflows");
        })
        .build_with_auto_env(&server)
        .await?;
    let mut thread_created = initial.thread_manager.subscribe_thread_created();
    let root_rollout_path = initial.codex.rollout_path().expect("root rollout path");
    mount_response_once_match(
        &server,
        BodyContainsAfterWorkflowBinding {
            text: "finish durable workflow task",
            rollout_path: root_rollout_path.clone(),
            task_id: "durable",
        },
        sse_response(sse(vec![
            ev_response_created("workflow-worker"),
            ev_assistant_message("workflow-worker-message", "durable task complete"),
            ev_completed("workflow-worker"),
        ]))
        .set_delay(Duration::from_millis(300)),
    )
    .await;
    initial
        .codex
        .submit(Op::WorkflowStart(
            codex_protocol::workflow::WorkflowStartParams {
                objective: codex_protocol::workflow::WorkflowObjective::new(
                    "durable workflow resume",
                )?,
                policy: Some(codex_protocol::workflow::WorkflowPolicy::Ultracode),
            },
        ))
        .await?;
    let started = wait_for_event(&initial.codex, |event| {
        matches!(event, EventMsg::WorkflowStarted(_))
    })
    .await;
    let EventMsg::WorkflowStarted(started) = started else {
        unreachable!("workflow started event")
    };
    let workflow_id: WorkflowId =
        serde_json::from_value(Value::String(started.workflow_id().as_str().to_string()))?;
    let worker_thread_id = timeout(Duration::from_secs(2), async {
        loop {
            let candidate = thread_created.recv().await?;
            let candidate_agent = WorkflowAgentId::from(candidate.to_string());
            let root_items = read_rollout_items(&root_rollout_path)?;
            if persisted_active_binding(&root_items, workflow_id, "durable", &candidate_agent)
                .is_some()
            {
                return Ok::<_, anyhow::Error>(candidate);
            }
        }
    })
    .await??;
    let worker_agent_id = WorkflowAgentId::from(worker_thread_id.to_string());
    let worker_thread = initial.thread_manager.get_thread(worker_thread_id).await?;
    wait_for_event(&initial.codex, |event| {
        matches!(event, EventMsg::WorkflowCompleted(_))
    })
    .await;
    let home = initial.home.clone();
    let worker_rollout_path = worker_thread.rollout_path().expect("worker rollout path");
    initial.codex.flush_rollout().await?;
    let root_items = read_rollout_items(&root_rollout_path)?;
    let (dispatch_snapshot_index, snapshot) =
        persisted_active_binding(&root_items, workflow_id, "durable", &worker_agent_id)
            .expect("persisted workflow dispatch snapshot");
    let worker_items = read_rollout_items(&worker_rollout_path)?;
    let marker_index = worker_items
        .iter()
        .position(|line| is_dispatch_marker(&line.item))
        .expect("workflow dispatch marker");
    let marker = dispatch_marker(&worker_items[marker_index].item).expect("dispatch marker");
    let worker_agent_id = worker_thread_id.to_string();
    assert_eq!(
        marker.agent_id().map(WorkflowAgentId::as_str),
        Some(worker_agent_id.as_str())
    );
    let drive_index = worker_items
        .iter()
        .position(|line| {
            line.item.is_model_visible()
                || matches!(line.item, RolloutItem::EventMsg(EventMsg::TurnStarted(_)))
        })
        .expect("worker drive item");
    let terminal_index = worker_items
        .iter()
        .rposition(|line| matches!(line.item, RolloutItem::EventMsg(EventMsg::TurnComplete(_))))
        .expect("worker terminal event");
    assert!(marker_index < drive_index);
    assert!(drive_index < terminal_index);
    assert!(!worker_items[marker_index].item.is_model_visible());
    drop(worker_thread);
    drop(initial);
    let mut root_rollout = std::fs::File::create(&root_rollout_path)?;
    for line in &root_items[..=dispatch_snapshot_index] {
        writeln!(root_rollout, "{}", serde_json::to_string(line)?)?;
    }
    drop(root_rollout);

    let mut resume_builder = test_codex().with_config(|config| {
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
    let resumed = resume_builder
        .resume(&server, home, root_rollout_path)
        .await?;
    let restored = manager_for_thread(&resumed.codex)
        .snapshot(snapshot.workflow_id)
        .await?;
    assert_eq!(
        restored.task("durable").expect("durable task").state,
        WorkflowTaskState::Succeeded
    );
    assert_eq!(
        restored
            .task("durable")
            .expect("durable task")
            .summary
            .as_deref(),
        Some("durable task complete")
    );
    assert!(
        resumed
            .thread_manager
            .get_thread(worker_thread_id)
            .await
            .is_err(),
        "closed terminal descendant should not be reopened during root resume"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_rejects_dispatch_identity_mismatches() -> Result<()> {
    for mismatch in ["workflow", "task", "execution_attempt", "task_attempt"] {
        let fixture = completed_collab_child().await?;
        let mut snapshot = resumable_snapshot()?;
        bind_running_task(&mut snapshot, fixture.worker_thread_id);
        let running = snapshot.task("running").expect("running task");
        let mut marker_workflow_id = snapshot.workflow_id;
        let mut marker_task_id = running.task_id.clone();
        let mut marker_execution_attempt = snapshot.execution_attempt;
        let mut marker_task_attempt = running.attempts;
        match mismatch {
            "workflow" => marker_workflow_id = WorkflowId::new(),
            "task" => marker_task_id = WorkflowTaskId::from("ready"),
            "execution_attempt" => marker_execution_attempt += 1,
            "task_attempt" => marker_task_attempt += 1,
            _ => unreachable!("mismatch case"),
        }
        insert_dispatch_marker(
            &fixture.worker_rollout_path,
            fixture.root_thread_id,
            fixture.worker_thread_id,
            marker_workflow_id,
            &marker_task_id,
            marker_execution_attempt,
            marker_task_attempt,
        )?;
        let resumed = resume_fixture(fixture, &snapshot).await?;
        let restored = manager_for_thread(&resumed.codex)
            .snapshot(snapshot.workflow_id)
            .await?;
        assert_requeued(&restored, "running");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_rejects_duplicate_and_invalid_agent_bindings() -> Result<()> {
    let fixture = completed_collab_child().await?;
    let mut snapshot = resumable_snapshot()?;
    bind_running_task(&mut snapshot, fixture.worker_thread_id);
    let running = snapshot.task("running").expect("running task").clone();
    let ready = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "ready")
        .expect("ready task");
    ready.agent_id = running.agent_id.clone();
    insert_dispatch_marker(
        &fixture.worker_rollout_path,
        fixture.root_thread_id,
        fixture.worker_thread_id,
        snapshot.workflow_id,
        &running.task_id,
        snapshot.execution_attempt,
        running.attempts,
    )?;
    let resumed = resume_fixture(fixture, &snapshot).await?;
    let restored = manager_for_thread(&resumed.codex)
        .snapshot(snapshot.workflow_id)
        .await?;
    assert_requeued(&restored, "running");
    assert_requeued(&restored, "ready");

    let home = Arc::new(TempDir::new()?);
    let root_thread_id = ThreadId::new();
    let mut invalid = resumable_snapshot()?;
    invalid
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "running")
        .expect("running task")
        .agent_id = Some(WorkflowAgentId::from("not-a-thread-id"));
    let rollout_path = write_rollout(home.as_ref(), root_thread_id, &invalid)?;
    let mut builder = resume_builder();
    let resumed = builder
        .resume(&start_mock_server().await, home, rollout_path)
        .await?;
    let restored = manager_for_thread(&resumed.codex)
        .snapshot(invalid.workflow_id)
        .await?;
    assert_requeued(&restored, "running");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_rejects_interrupted_and_shutdown_dispatches() -> Result<()> {
    let terminal_events = [
        EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("interrupted-dispatch".to_string()),
            started_at: None,
            reason: TurnAbortReason::Interrupted,
            completed_at: None,
            duration_ms: None,
        }),
        EventMsg::ShutdownComplete,
    ];
    for terminal_event in terminal_events {
        let fixture = completed_collab_child().await?;
        let mut snapshot = resumable_snapshot()?;
        bind_running_task(&mut snapshot, fixture.worker_thread_id);
        let running = snapshot.task("running").expect("running task");
        insert_dispatch_marker(
            &fixture.worker_rollout_path,
            fixture.root_thread_id,
            fixture.worker_thread_id,
            snapshot.workflow_id,
            &running.task_id,
            snapshot.execution_attempt,
            running.attempts,
        )?;
        append_rollout_item(
            &fixture.worker_rollout_path,
            RolloutItem::EventMsg(terminal_event),
        )?;
        let resumed = resume_fixture(fixture, &snapshot).await?;
        let restored = manager_for_thread(&resumed.codex)
            .snapshot(snapshot.workflow_id)
            .await?;
        assert_requeued(&restored, "running");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_propagates_workflow_agent_history_io_errors() -> Result<()> {
    let fixture = completed_collab_child().await?;
    let mut snapshot = resumable_snapshot()?;
    bind_running_task(&mut snapshot, fixture.worker_thread_id);
    let running = snapshot.task("running").expect("running task");
    insert_dispatch_marker(
        &fixture.worker_rollout_path,
        fixture.root_thread_id,
        fixture.worker_thread_id,
        snapshot.workflow_id,
        &running.task_id,
        snapshot.execution_attempt,
        running.attempts,
    )?;
    let backup_path = fixture.worker_rollout_path.with_extension("jsonl.backup");
    std::fs::rename(&fixture.worker_rollout_path, backup_path)?;
    std::fs::create_dir(&fixture.worker_rollout_path)?;

    let error = match resume_fixture(fixture, &snapshot).await {
        Ok(_) => anyhow::bail!("cold resume unexpectedly ignored child rollout I/O failure"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("failed to read stored thread"),
        "{error:#}"
    );
    Ok(())
}

struct BodyContainsAfterWorkflowBinding {
    text: &'static str,
    rollout_path: std::path::PathBuf,
    task_id: &'static str,
}

impl wiremock::Match for BodyContainsAfterWorkflowBinding {
    fn matches(&self, request: &wiremock::Request) -> bool {
        if !body_contains(request, self.text)
            || body_contains(request, WORKFLOW_DISPATCH_MARKER_PREFIX)
        {
            return false;
        }
        let Ok(rollout) = std::fs::read_to_string(&self.rollout_path) else {
            return false;
        };
        rollout.lines().any(|line| {
            let Ok(line) = serde_json::from_str::<RolloutLine>(line) else {
                return false;
            };
            let RolloutItem::WorkflowState(item) = line.item else {
                return false;
            };
            serde_json::from_value::<WorkflowSnapshot>(item.snapshot().clone())
                .ok()
                .and_then(|snapshot| snapshot.task(self.task_id).cloned())
                .is_some_and(|task| {
                    matches!(
                        task.state,
                        WorkflowTaskState::Ready | WorkflowTaskState::Running
                    ) && task.agent_id.is_some()
                })
        })
    }
}
