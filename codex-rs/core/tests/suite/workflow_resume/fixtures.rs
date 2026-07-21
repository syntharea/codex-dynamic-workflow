use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::WorkflowStateItem;
use codex_workflows::WORKFLOW_DISPATCH_MARKER_PREFIX;
use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowDispatchMarker;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use codex_workflows::WorkflowTaskState;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::Instant;
use tokio::time::sleep;

fn decoded_body(request: &wiremock::Request) -> Option<Vec<u8>> {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    }
}

pub(super) fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

pub(super) fn request_has_input_type(request: &wiremock::Request, input_type: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .and_then(|body| body.get("input").and_then(Value::as_array).cloned())
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some(input_type))
        })
}

pub(super) struct ClosedChildFixture {
    server: wiremock::MockServer,
    home: Arc<TempDir>,
    pub(super) root_thread_id: ThreadId,
    pub(super) worker_thread_id: ThreadId,
    root_rollout_path: std::path::PathBuf,
    pub(super) worker_rollout_path: std::path::PathBuf,
}

pub(super) async fn completed_collab_child() -> Result<ClosedChildFixture> {
    let server = start_mock_server().await;
    let spawn_args = serde_json::json!({
        "message": "finish the durable task",
        "task_name": "workflow_worker",
        "fork_turns": "none",
    });
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "start the workflow worker"),
        sse(vec![
            ev_response_created("workflow-root-1"),
            ev_function_call_with_namespace(
                "workflow-spawn-1",
                "collaboration",
                "spawn_agent",
                &spawn_args.to_string(),
            ),
            ev_completed("workflow-root-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            request_has_input_type(request, "agent_message")
                && body_contains(request, "finish the durable task")
        },
        sse(vec![
            ev_response_created("workflow-worker-1"),
            ev_assistant_message("workflow-worker-message", "durable task complete"),
            ev_completed("workflow-worker-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "workflow-spawn-1")
                && !request_has_input_type(request, "agent_message")
        },
        sse(vec![
            ev_response_created("workflow-root-2"),
            ev_assistant_message("workflow-root-message", "worker recorded"),
            ev_completed("workflow-root-2"),
        ]),
    )
    .await;
    let close_args = serde_json::json!({"target": "workflow_worker"});
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "close the workflow worker"),
        sse(vec![
            ev_response_created("workflow-close-1"),
            ev_function_call_with_namespace(
                "workflow-close-call",
                "collaboration",
                "close_agent",
                &close_args.to_string(),
            ),
            ev_completed("workflow-close-1"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "workflow-close-call"),
        sse(vec![
            ev_response_created("workflow-close-2"),
            ev_assistant_message("workflow-close-message", "worker closed"),
            ev_completed("workflow-close-2"),
        ]),
    )
    .await;
    let initial = resume_builder().build_with_auto_env(&server).await?;
    let root_thread_id = initial.session_configured.thread_id;
    let home = initial.home.clone();
    let root_rollout_path = initial.codex.rollout_path().expect("root rollout path");
    initial.submit_turn("start the workflow worker").await?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let worker_thread_id = loop {
        if let Some(thread_id) = initial
            .thread_manager
            .list_thread_ids()
            .await
            .into_iter()
            .find(|thread_id| *thread_id != root_thread_id)
        {
            break thread_id;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for workflow worker");
        }
        sleep(Duration::from_millis(10)).await;
    };
    let worker_thread = initial.thread_manager.get_thread(worker_thread_id).await?;
    let worker_rollout_path = worker_thread.rollout_path().expect("worker rollout path");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(
            worker_thread.agent_status().await,
            AgentStatus::Completed(_)
        ) {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for workflow worker completion");
        }
        sleep(Duration::from_millis(10)).await;
    }
    initial.submit_turn("close the workflow worker").await?;
    worker_thread.flush_rollout().await?;
    initial.codex.flush_rollout().await?;
    drop(worker_thread);
    drop(initial);
    Ok(ClosedChildFixture {
        server,
        home,
        root_thread_id,
        worker_thread_id,
        root_rollout_path,
        worker_rollout_path,
    })
}

pub(super) fn resume_builder() -> core_test_support::test_codex::TestCodexBuilder {
    test_codex().with_config(|config| {
        config.agents_enabled = true;
        config
            .features
            .enable(Feature::Collab)
            .expect("enable collaboration");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("enable multi-agent v2");
        config
            .features
            .enable(Feature::DynamicWorkflows)
            .expect("enable dynamic workflows");
    })
}

pub(super) async fn resume_fixture(
    fixture: ClosedChildFixture,
    snapshot: &WorkflowSnapshot,
) -> Result<core_test_support::test_codex::TestCodex> {
    append_workflow_snapshot(&fixture.root_rollout_path, snapshot)?;
    let mut builder = resume_builder();
    builder
        .resume(&fixture.server, fixture.home, fixture.root_rollout_path)
        .await
}

pub(super) fn bind_running_task(snapshot: &mut WorkflowSnapshot, worker_thread_id: ThreadId) {
    snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "running")
        .expect("running task")
        .agent_id = Some(WorkflowAgentId::from(worker_thread_id.to_string()));
}

pub(super) fn assert_requeued(snapshot: &WorkflowSnapshot, task_id: &str) {
    let task = snapshot.task(task_id).expect("workflow task");
    assert_eq!(task.state, WorkflowTaskState::Queued);
    assert_eq!(task.agent_id, None);
    assert_eq!(task.summary, None);
}

pub(super) fn insert_dispatch_marker(
    rollout_path: &std::path::Path,
    root_thread_id: ThreadId,
    worker_thread_id: ThreadId,
    workflow_id: WorkflowId,
    task_id: &WorkflowTaskId,
    execution_attempt: u32,
    task_attempt: u8,
) -> Result<()> {
    let marker = serde_json::json!({
        "schema_version": 1,
        "workflow_id": workflow_id,
        "task_id": task_id,
        "execution_attempt": execution_attempt,
        "task_attempt": task_attempt,
        "agent_id": worker_thread_id.to_string(),
    });
    let marker_item = RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            session_id: root_thread_id.into(),
            id: root_thread_id,
            parent_thread_id: Some(root_thread_id),
            source: SessionSource::Custom(format!(
                "{WORKFLOW_DISPATCH_MARKER_PREFIX}{}",
                serde_json::to_string(&marker)?
            )),
            ..Default::default()
        },
        git: None,
    });
    let mut lines = read_rollout_items(rollout_path)?;
    let insert_at = lines
        .iter()
        .position(|line| matches!(line.item, RolloutItem::SessionMeta(_)))
        .map_or(0, |index| index + 1);
    lines.insert(
        insert_at,
        RolloutLine {
            timestamp: "2026-07-21T00:00:01.000Z".to_string(),
            ordinal: None,
            item: marker_item,
        },
    );
    let mut file = std::fs::File::create(rollout_path)?;
    for line in lines {
        writeln!(file, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(())
}

pub(super) fn append_rollout_item(rollout_path: &std::path::Path, item: RolloutItem) -> Result<()> {
    let line = RolloutLine {
        timestamp: "2026-07-21T00:00:03.000Z".to_string(),
        ordinal: None,
        item,
    };
    let mut file = OpenOptions::new().append(true).open(rollout_path)?;
    writeln!(file, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

pub(super) fn read_rollout_items(rollout_path: &std::path::Path) -> Result<Vec<RolloutLine>> {
    std::fs::read_to_string(rollout_path)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(anyhow::Error::from))
        .collect()
}

pub(super) fn persisted_active_binding(
    lines: &[RolloutLine],
    workflow_id: WorkflowId,
    task_id: &str,
    agent_id: &WorkflowAgentId,
) -> Option<(usize, WorkflowSnapshot)> {
    lines.iter().enumerate().find_map(|(index, line)| {
        let RolloutItem::WorkflowState(item) = &line.item else {
            return None;
        };
        let snapshot = serde_json::from_value::<WorkflowSnapshot>(item.snapshot().clone()).ok()?;
        let task = snapshot.task(task_id)?;
        (snapshot.workflow_id == workflow_id
            && snapshot.state == WorkflowState::Running
            && matches!(
                task.state,
                WorkflowTaskState::Ready | WorkflowTaskState::Running
            )
            && task.agent_id.as_ref() == Some(agent_id))
        .then_some((index, snapshot))
    })
}

pub(super) fn is_dispatch_marker(item: &RolloutItem) -> bool {
    dispatch_marker(item).is_some()
}

pub(super) fn dispatch_marker(item: &RolloutItem) -> Option<WorkflowDispatchMarker> {
    matches!(
        item,
        RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                source: SessionSource::Custom(source),
                ..
            },
            ..
        }) if source.starts_with(WORKFLOW_DISPATCH_MARKER_PREFIX)
    )
    .then(|| {
        let RolloutItem::SessionMeta(meta_line) = item else {
            unreachable!("matched session metadata")
        };
        let SessionSource::Custom(source) = &meta_line.meta.source else {
            unreachable!("matched custom session source")
        };
        WorkflowDispatchMarker::decode(source)
            .expect("valid dispatch marker")
            .expect("recognized dispatch marker")
    })
}

pub(super) fn resumable_snapshot() -> Result<WorkflowSnapshot> {
    let definition = WorkflowDefinition::try_new(
        "resume",
        vec![
            task("completed", &[]),
            task("running", &["completed"]),
            task("ready", &["completed"]),
        ],
        WorkflowLimits::default(),
    )?;
    let mut snapshot = WorkflowSnapshot::new(codex_workflows::WorkflowId::new(), definition);
    snapshot.state = WorkflowState::Running;
    let completed = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "completed")
        .expect("completed task");
    completed.state = WorkflowTaskState::Succeeded;
    completed.attempts = 1;
    completed.summary = Some("already done".to_string());
    let running = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "running")
        .expect("running task");
    running.state = WorkflowTaskState::Running;
    running.attempts = 2;
    running.agent_id = Some(WorkflowAgentId::from(ThreadId::new().to_string()));
    let ready = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.task_id.as_str() == "ready")
        .expect("ready task");
    ready.state = WorkflowTaskState::Ready;
    ready.attempts = 1;
    ready.agent_id = Some(WorkflowAgentId::from(ThreadId::new().to_string()));
    Ok(snapshot)
}

fn task(id: &str, dependencies: &[&str]) -> WorkflowTask {
    WorkflowTask {
        id: WorkflowTaskId::from(id),
        title: id.to_string(),
        instructions: format!("run {id}"),
        dependencies: dependencies
            .iter()
            .map(|dependency| WorkflowTaskId::from(*dependency))
            .collect(),
        agent_role: None,
        intent: WorkflowTaskIntent::ReadOnly,
        owned_paths: Vec::new(),
        verification: Vec::new(),
        max_attempts: 2,
    }
}

pub(super) fn write_rollout(
    home: &TempDir,
    root_thread_id: ThreadId,
    snapshot: &WorkflowSnapshot,
) -> Result<std::path::PathBuf> {
    let rollout_path = home.path().join("workflow-resume.jsonl");
    let workflow_state = WorkflowStateItem::new(
        snapshot.workflow_id.to_string(),
        serde_json::to_value(snapshot)?,
    )?;
    let lines = [
        RolloutLine {
            timestamp: "2026-07-21T00:00:00.000Z".to_string(),
            ordinal: None,
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    session_id: root_thread_id.into(),
                    id: root_thread_id,
                    parent_thread_id: None,
                    timestamp: "2026-07-21T00:00:00Z".to_string(),
                    cwd: home.path().to_path_buf(),
                    originator: "workflow-resume-test".to_string(),
                    cli_version: "test".to_string(),
                    model_provider: Some("test".to_string()),
                    ..Default::default()
                },
                git: None,
            }),
        },
        RolloutLine {
            timestamp: "2026-07-21T00:00:01.000Z".to_string(),
            ordinal: None,
            item: RolloutItem::WorkflowState(workflow_state),
        },
    ];
    let mut file = std::fs::File::create(&rollout_path)?;
    for line in lines {
        writeln!(file, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(rollout_path)
}

fn append_workflow_snapshot(
    rollout_path: &std::path::Path,
    snapshot: &WorkflowSnapshot,
) -> Result<()> {
    let workflow_state = WorkflowStateItem::new(
        snapshot.workflow_id.to_string(),
        serde_json::to_value(snapshot)?,
    )?;
    let line = RolloutLine {
        timestamp: "2026-07-21T00:00:02.000Z".to_string(),
        ordinal: None,
        item: RolloutItem::WorkflowState(workflow_state),
    };
    let mut file = OpenOptions::new().append(true).open(rollout_path)?;
    writeln!(file, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}
