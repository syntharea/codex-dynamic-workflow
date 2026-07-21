use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use codex_core::workflows::WorkflowManager;
use codex_core::workflows::WorkflowManagerConfig;
use codex_core::workflows::WorkflowRuntimeEvent;
use codex_core::workflows::adapter_for_thread;
use codex_core::workflows::manager_for_thread;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::ThreadId;
use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowAgentOutcome;
use codex_workflows::WorkflowAgentOutcomeStatus;
use codex_workflows::WorkflowAgentRequest;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowId;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use codex_workflows::WorkflowValidationError;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;

#[derive(Default)]
struct FakeAgentControl {
    spawned: Mutex<Vec<WorkflowTaskId>>,
    tasks: Mutex<std::collections::BTreeMap<WorkflowAgentId, WorkflowTaskId>>,
    next_agent: AtomicUsize,
    fail_next: std::sync::atomic::AtomicBool,
    hold_wait: AtomicBool,
    never_finish_wait: AtomicBool,
    wait_empty_once: AtomicBool,
    spawn_started: Option<Arc<tokio::sync::Notify>>,
    spawn_gate: Option<Arc<tokio::sync::Notify>>,
    spawn_error: Mutex<Option<WorkflowError>>,
    wait_error: Mutex<Option<WorkflowError>>,
    always_capacity: AtomicBool,
    max_live_agents: Option<usize>,
    interrupt_gate: Option<Arc<tokio::sync::Notify>>,
    interrupt_release: Option<Arc<tokio::sync::Notify>>,
    interrupted: Mutex<Vec<WorkflowAgentId>>,
    closed: Mutex<Vec<WorkflowAgentId>>,
}

impl WorkflowAgentControl for FakeAgentControl {
    async fn spawn(&self, request: WorkflowAgentRequest) -> Result<WorkflowAgentId, WorkflowError> {
        self.spawned
            .lock()
            .expect("spawn log lock")
            .push(request.task_id.clone());
        if let Some(started) = &self.spawn_started {
            started.notify_waiters();
        }
        if let Some(gate) = &self.spawn_gate {
            gate.notified().await;
        }
        if let Some(error) = self.spawn_error.lock().expect("spawn error lock").take() {
            return Err(error);
        }
        if self.always_capacity.load(Ordering::Relaxed) {
            return Err(WorkflowError::ExecutorCapacity("full".to_string()));
        }
        if self
            .max_live_agents
            .is_some_and(|limit| self.tasks.lock().expect("task log lock").len() >= limit)
        {
            return Err(WorkflowError::ExecutorCapacity("full".to_string()));
        }
        let agent_id = WorkflowAgentId::from(format!(
            "agent-{}",
            self.next_agent.fetch_add(1, Ordering::Relaxed)
        ));
        self.tasks
            .lock()
            .expect("task log lock")
            .insert(agent_id.clone(), request.task_id);
        Ok(agent_id)
    }

    async fn wait(
        &self,
        agents: Vec<WorkflowAgentId>,
        timeout: Duration,
    ) -> Result<Vec<WorkflowAgentOutcome>, WorkflowError> {
        if let Some(error) = self.wait_error.lock().expect("wait error lock").take() {
            return Err(error);
        }
        if self.never_finish_wait.load(Ordering::Relaxed) {
            tokio::time::sleep(timeout).await;
            return Ok(Vec::new());
        }
        if self.wait_empty_once.swap(false, Ordering::Relaxed)
            || (self.hold_wait.load(Ordering::Relaxed)
                && self
                    .interrupted
                    .lock()
                    .expect("interrupt log lock")
                    .is_empty())
        {
            return Ok(Vec::new());
        }
        Ok(agents
            .into_iter()
            .map(|agent_id| WorkflowAgentOutcome {
                agent_id: agent_id.clone(),
                task_id: self
                    .tasks
                    .lock()
                    .expect("task log lock")
                    .get(&agent_id)
                    .expect("task for agent")
                    .clone(),
                status: if self
                    .interrupted
                    .lock()
                    .expect("interrupt log lock")
                    .contains(&agent_id)
                {
                    WorkflowAgentOutcomeStatus::Cancelled
                } else if self.fail_next.swap(false, Ordering::Relaxed) {
                    WorkflowAgentOutcomeStatus::Failed {
                        summary: "transient".to_string(),
                        retryable: true,
                    }
                } else {
                    WorkflowAgentOutcomeStatus::Succeeded {
                        summary: "done".to_string(),
                    }
                },
            })
            .collect())
    }

    async fn interrupt(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.interrupted
            .lock()
            .expect("interrupt log lock")
            .push(agent);
        if let Some(gate) = &self.interrupt_gate {
            gate.notify_waiters();
        }
        if let Some(release) = &self.interrupt_release {
            release.notified().await;
        }
        Ok(())
    }

    async fn close(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.closed
            .lock()
            .expect("close log lock")
            .push(agent.clone());
        self.tasks.lock().expect("task log lock").remove(&agent);
        Ok(())
    }
}

#[tokio::test]
async fn manager_marks_activated_definition_as_accepted() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("accepted", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;

    assert!(
        manager
            .snapshot(workflow_id)
            .await?
            .is_definition_accepted()
    );
    Ok(())
}

#[tokio::test]
async fn manager_applies_lower_configured_task_limit_after_deserialization() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "configured-limit",
        vec![task("a", &[]), task("b", &[])],
        WorkflowLimits {
            max_tasks: 2,
            ..WorkflowLimits::default()
        },
    )?;
    let definition: WorkflowDefinition = serde_json::from_value(serde_json::to_value(definition)?)?;
    let manager = WorkflowManager::with_config(WorkflowManagerConfig {
        max_tasks: 1,
        ..WorkflowManagerConfig::default()
    })?;

    assert_eq!(
        manager
            .start(definition, ThreadId::default())
            .await
            .unwrap_err(),
        WorkflowError::Validation(WorkflowValidationError::TooManyTasks { max_tasks: 1 })
    );
    Ok(())
}

#[tokio::test]
async fn manager_retries_retryable_failures() -> Result<()> {
    let mut retry_task = task("retry", &[]);
    retry_task.max_attempts = 2;
    let definition =
        WorkflowDefinition::try_new("retry", vec![retry_task], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        fail_next: AtomicBool::new(true),
        ..Default::default()
    });

    manager.drive(workflow_id, adapter).await?.await??;

    assert_eq!(
        manager
            .snapshot(workflow_id)
            .await?
            .task("retry")
            .expect("task")
            .attempts,
        2
    );
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn manager_persists_spawned_agent_id_on_running_task() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "spawn-agent-id",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let spawn_started = Arc::new(tokio::sync::Notify::new());
    let spawn_release = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        never_finish_wait: AtomicBool::new(true),
        spawn_started: Some(Arc::clone(&spawn_started)),
        spawn_gate: Some(Arc::clone(&spawn_release)),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, Arc::clone(&adapter)).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().expect("spawn log lock").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    spawn_release.notify_waiters();
    for _ in 0..100 {
        if manager
            .snapshot(workflow_id)
            .await?
            .task("a")
            .expect("task")
            .agent_id
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        manager
            .snapshot(workflow_id)
            .await?
            .task("a")
            .expect("task")
            .agent_id
            .is_some()
    );
    adapter.never_finish_wait.store(false, Ordering::Relaxed);
    drive.await??;
    Ok(())
}

#[tokio::test]
async fn pause_then_resume_does_not_block_drive() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("pause", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    manager.pause(workflow_id).await?;
    let adapter = Arc::new(FakeAgentControl::default());

    manager.drive(workflow_id, adapter.clone()).await?.await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Paused
    );
    manager
        .resume(workflow_id, adapter.clone(), Arc::new(|_| Ok(())))
        .await?
        .await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn running_pause_interrupts_then_resume_starts_a_new_agent() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("pause", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter.clone()).await?;
    for _ in 0..100 {
        if adapter.spawned.lock().expect("spawn log lock").len() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    manager.pause(workflow_id).await?;
    drive.await??;

    let paused = manager.snapshot(workflow_id).await?;
    assert_eq!(paused.state, codex_workflows::WorkflowState::Paused);
    assert_eq!(
        paused.task("a").expect("task").state,
        codex_workflows::WorkflowTaskState::Queued
    );
    assert_eq!(
        adapter
            .interrupted
            .lock()
            .expect("interrupt log lock")
            .len(),
        1
    );
    assert_eq!(adapter.closed.lock().expect("close log lock").len(), 1);

    manager
        .resume(workflow_id, adapter.clone(), Arc::new(|_| Ok(())))
        .await?
        .await??;
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 2);
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn pausing_resume_records_intent_and_reuses_the_running_driver() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "resume-intent",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let interrupt_started = Arc::new(tokio::sync::Notify::new());
    let interrupt_release = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        interrupt_gate: Some(Arc::clone(&interrupt_started)),
        interrupt_release: Some(Arc::clone(&interrupt_release)),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, Arc::clone(&adapter)).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().expect("spawn log lock").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    manager.pause(workflow_id).await?;
    interrupt_started.notified().await;
    manager
        .resume(workflow_id, Arc::clone(&adapter), Arc::new(|_| Ok(())))
        .await?
        .await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Pausing
    );
    assert!(manager.snapshot(workflow_id).await?.resume_after_pause);

    interrupt_release.notify_waiters();
    drive.await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 2);
    Ok(())
}

#[tokio::test]
async fn pausing_resume_persists_intent_before_interrupt_finishes() -> Result<()> {
    let server = start_mock_server().await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    let manager = manager_for_thread(&test.codex);
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "persist-resume-intent",
                vec![task("a", &[])],
                WorkflowLimits::default(),
            )?,
            test.session_configured.thread_id,
        )
        .await?;
    let interrupt_started = Arc::new(tokio::sync::Notify::new());
    let interrupt_release = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        interrupt_gate: Some(Arc::clone(&interrupt_started)),
        interrupt_release: Some(Arc::clone(&interrupt_release)),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, Arc::clone(&adapter)).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().expect("spawn log lock").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowPause(
            codex_protocol::workflow::WorkflowPauseParams {
                workflow_id: protocol_workflow_id(workflow_id)?,
            },
        ))
        .await?;
    interrupt_started.notified().await;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowResume(
            codex_protocol::workflow::WorkflowResumeParams {
                workflow_id: protocol_workflow_id(workflow_id)?,
            },
        ))
        .await?;

    for _ in 0..100 {
        if manager.snapshot(workflow_id).await?.resume_after_pause {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    test.codex.flush_rollout().await?;
    let rollout = std::fs::read_to_string(test.codex.rollout_path().expect("rollout path"))?;
    let snapshots = persisted_workflow_snapshots(&rollout, &workflow_id.to_string())?;
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.get("state").and_then(Value::as_str) == Some("pausing")
            && snapshot.get("resume_after_pause").and_then(Value::as_bool) == Some(true)
    }));

    interrupt_release.notify_waiters();
    drive.await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 2);
    Ok(())
}

#[tokio::test]
async fn pause_timeout_fails_a_workflow_when_the_agent_never_stops() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "pause-timeout",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::with_config(WorkflowManagerConfig {
        wait_timeout: Duration::from_millis(10),
        pause_timeout: Duration::from_millis(50),
        ..Default::default()
    })?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        never_finish_wait: AtomicBool::new(true),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let drive = manager
        .drive_with_events(
            workflow_id,
            adapter,
            Arc::new(move |event| {
                captured.lock().expect("event lock").push(event);
                Ok(())
            }),
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    manager.pause(workflow_id).await?;

    assert!(
        tokio::time::timeout(Duration::from_millis(500), drive)
            .await??
            .is_err()
    );
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.state, codex_workflows::WorkflowState::Failed);
    assert!(events.lock().expect("event lock").iter().any(|event| {
        matches!(
            event,
            WorkflowRuntimeEvent::Failed { error, .. }
                if error.contains("pause timed out")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn cancel_during_spawn_closes_unregistered_agent() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("cancel", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        spawn_started: Some(started.clone()),
        spawn_gate: Some(gate.clone()),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter.clone()).await?;
    started.notified().await;
    manager.cancel(workflow_id).await?;
    gate.notify_waiters();
    drive.await??;

    assert_eq!(adapter.closed.lock().expect("close log lock").len(), 1);
    assert_eq!(
        adapter
            .interrupted
            .lock()
            .expect("interrupt log lock")
            .len(),
        1
    );
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Cancelled
    );
    assert_eq!(
        manager
            .snapshot(workflow_id)
            .await?
            .task("a")
            .expect("task")
            .state,
        codex_workflows::WorkflowTaskState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn capacity_spawn_failure_retries_without_stuck_ready_task() -> Result<()> {
    let mut retry_task = task("capacity", &[]);
    retry_task.max_attempts = 2;
    let definition =
        WorkflowDefinition::try_new("capacity", vec![retry_task], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        spawn_error: Mutex::new(Some(WorkflowError::ExecutorCapacity("full".to_string()))),
        ..Default::default()
    });
    manager.drive(workflow_id, adapter.clone()).await?.await??;
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.state, codex_workflows::WorkflowState::Completed);
    assert_eq!(snapshot.task("capacity").expect("task").attempts, 1);
    assert_eq!(adapter.closed.lock().expect("close log lock").len(), 1);
    Ok(())
}

#[tokio::test]
async fn capacity_defers_the_rest_of_a_spawn_batch_without_consuming_attempts() -> Result<()> {
    let mut first = task("first", &[]);
    first.max_attempts = 3;
    let mut second = task("second", &[]);
    second.max_attempts = 3;
    let definition = WorkflowDefinition::try_new(
        "capacity-batch",
        vec![first, second],
        WorkflowLimits {
            max_task_attempts: 3,
            ..WorkflowLimits::default()
        },
    )?;
    let manager = WorkflowManager::with_config(WorkflowManagerConfig {
        max_concurrent_tasks: 2,
        max_task_attempts: 3,
        ..Default::default()
    })?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        max_live_agents: Some(1),
        ..Default::default()
    });
    manager.drive(workflow_id, adapter.clone()).await?.await??;
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.state, codex_workflows::WorkflowState::Completed);
    assert_eq!(snapshot.task("first").expect("first").attempts, 1);
    assert_eq!(snapshot.task("second").expect("second").attempts, 1);
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 3);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn permanent_capacity_uses_bounded_backoff_and_deduplicates_persist() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("backoff", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        always_capacity: AtomicBool::new(true),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::<WorkflowRuntimeEvent>::new()));
    let sink = Arc::clone(&events);
    let drive = manager
        .drive_with_events(
            workflow_id,
            Arc::clone(&adapter),
            Arc::new(move |event| {
                sink.lock().expect("event lock").push(event);
                Ok(())
            }),
        )
        .await?;

    tokio::task::yield_now().await;
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    let spawn_count = adapter.spawned.lock().expect("spawn log lock").len();
    assert!(
        spawn_count <= 8,
        "spawned {spawn_count} times during backoff"
    );
    let persist_count = events
        .lock()
        .expect("event lock")
        .iter()
        .filter(|event| matches!(event, WorkflowRuntimeEvent::Persist(_)))
        .count();
    assert_eq!(persist_count, 1);

    adapter.always_capacity.store(false, Ordering::Relaxed);
    tokio::time::advance(Duration::from_secs(2)).await;
    drive.await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn cancel_before_capacity_wait_future_is_created_wakes_driver() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "cancel-wake",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        always_capacity: AtomicBool::new(true),
        ..Default::default()
    });
    let scheduled = Arc::new(AtomicBool::new(false));
    let cancel_manager = manager.clone();
    let cancel_scheduled = Arc::clone(&scheduled);
    let drive = manager
        .drive_with_events(
            workflow_id,
            adapter,
            Arc::new(move |event| {
                if matches!(event, WorkflowRuntimeEvent::Persist(_))
                    && !cancel_scheduled.swap(true, Ordering::Relaxed)
                {
                    let cancel_manager = cancel_manager.clone();
                    tokio::spawn(async move {
                        cancel_manager
                            .cancel(workflow_id)
                            .await
                            .expect("cancel workflow");
                    });
                }
                Ok(())
            }),
        )
        .await?;
    let _ = tokio::time::timeout(Duration::from_millis(500), drive).await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn pause_before_capacity_wait_future_is_created_wakes_driver() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "pause-wake",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        always_capacity: AtomicBool::new(true),
        ..Default::default()
    });
    let scheduled = Arc::new(AtomicBool::new(false));
    let pause_manager = manager.clone();
    let pause_scheduled = Arc::clone(&scheduled);
    let drive = manager
        .drive_with_events(
            workflow_id,
            adapter,
            Arc::new(move |event| {
                if matches!(event, WorkflowRuntimeEvent::Persist(_))
                    && !pause_scheduled.swap(true, Ordering::Relaxed)
                {
                    let pause_manager = pause_manager.clone();
                    tokio::spawn(async move {
                        pause_manager
                            .pause(workflow_id)
                            .await
                            .expect("pause workflow");
                    });
                }
                Ok(())
            }),
        )
        .await?;
    let _ = tokio::time::timeout(Duration::from_millis(500), drive).await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Paused
    );
    Ok(())
}

#[tokio::test]
async fn disabled_workflow_manager_rejects_start_without_zero_scheduler() -> Result<()> {
    let manager = WorkflowManager::with_config(WorkflowManagerConfig {
        enabled: false,
        ..WorkflowManagerConfig::default()
    })?;
    let definition =
        WorkflowDefinition::try_new("disabled", vec![task("a", &[])], WorkflowLimits::default())?;
    assert!(matches!(
        manager.start(definition, ThreadId::default()).await,
        Err(WorkflowError::WorkflowsDisabled)
    ));
    Ok(())
}

#[tokio::test]
async fn workflow_manager_filters_workflows_by_root_thread() -> Result<()> {
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let owner = ThreadId::new();
    let other = ThreadId::new();
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "owner isolation",
                vec![task("owner-task", &[])],
                WorkflowLimits::default(),
            )?,
            owner,
        )
        .await?;

    assert_eq!(manager.snapshots_for_owner(owner).await.len(), 1);
    assert!(manager.snapshots_for_owner(other).await.is_empty());
    assert!(manager.ensure_owner(workflow_id, owner).await.is_ok());
    assert!(manager.ensure_owner(workflow_id, other).await.is_err());
    Ok(())
}

#[tokio::test]
async fn cancelled_spawn_error_does_not_record_a_failure() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("race", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        spawn_started: Some(started.clone()),
        spawn_gate: Some(gate.clone()),
        spawn_error: Mutex::new(Some(WorkflowError::ExecutorFailure("boom".to_string()))),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter).await?;
    started.notified().await;
    manager.cancel(workflow_id).await?;
    gate.notify_waiters();
    drive.await??;
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.task("a").expect("task").attempts, 1);
    assert_eq!(
        snapshot.task("a").expect("task").state,
        codex_workflows::WorkflowTaskState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_capacity_error_does_not_record_a_failure() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "capacity-race",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(FakeAgentControl {
        spawn_started: Some(started.clone()),
        spawn_gate: Some(gate.clone()),
        spawn_error: Mutex::new(Some(WorkflowError::ExecutorCapacity("full".to_string()))),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter).await?;
    started.notified().await;
    manager.cancel(workflow_id).await?;
    gate.notify_waiters();
    drive.await??;
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.task("a").expect("task").attempts, 1);
    assert_eq!(
        snapshot.task("a").expect("task").state,
        codex_workflows::WorkflowTaskState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn retry_api_starts_a_new_driver() -> Result<()> {
    let mut retry_task = task("retry-api", &[]);
    retry_task.max_attempts = 1;
    let definition =
        WorkflowDefinition::try_new("retry-api", vec![retry_task], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        fail_next: AtomicBool::new(true),
        ..Default::default()
    });
    manager.drive(workflow_id, adapter.clone()).await?.await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Failed
    );
    manager
        .retry(workflow_id, adapter, Arc::new(|_| Ok(())))
        .await?
        .await??;
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_drive_is_rejected_and_partial_wait_does_not_respawn() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("drive", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        wait_empty_once: AtomicBool::new(true),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter.clone()).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().expect("spawn log lock").is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        manager.drive(workflow_id, adapter.clone()).await,
        Err(WorkflowError::AlreadyRunning)
    ));
    adapter.hold_wait.store(false, Ordering::Relaxed);
    drive.await??;
    assert_eq!(adapter.spawned.lock().expect("spawn log lock").len(), 1);
    Ok(())
}

#[tokio::test]
async fn retry_rejects_while_an_old_driver_is_running() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "retry-running",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter.clone()).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().expect("spawn log lock").is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        manager
            .retry(workflow_id, adapter.clone(), Arc::new(|_| Ok(())))
            .await,
        Err(WorkflowError::AlreadyRunning)
    ));
    manager.cancel(workflow_id).await?;
    adapter.hold_wait.store(false, Ordering::Relaxed);
    drive.await??;
    Ok(())
}

#[tokio::test]
async fn workflow_manager_configuration_locks_after_start() -> Result<()> {
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    manager
        .configure(WorkflowManagerConfig {
            max_concurrent_tasks: 2,
            ..manager.config()
        })
        .await?;
    assert_eq!(manager.config().max_concurrent_tasks, 2);
    manager
        .start(
            WorkflowDefinition::try_new("config", vec![task("a", &[])], WorkflowLimits::default())?,
            ThreadId::default(),
        )
        .await?;
    assert!(matches!(
        manager
            .configure(WorkflowManagerConfig {
                max_concurrent_tasks: 1,
                ..manager.config()
            })
            .await,
        Err(WorkflowError::ConfigurationLocked)
    ));
    Ok(())
}

#[tokio::test]
async fn runtime_events_persist_terminal_and_deduplicate_progress() -> Result<()> {
    let definition =
        WorkflowDefinition::try_new("events", vec![task("a", &[])], WorkflowLimits::default())?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl::default());
    let events = Arc::new(Mutex::new(Vec::<WorkflowRuntimeEvent>::new()));
    let sink = events.clone();
    manager
        .drive_with_events(
            workflow_id,
            adapter,
            Arc::new(move |event| {
                sink.lock().expect("event lock").push(event);
                Ok(())
            }),
        )
        .await?
        .await??;
    let events = events.lock().expect("event lock");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WorkflowRuntimeEvent::Persist(_)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WorkflowRuntimeEvent::Completed(_)))
    );
    let progress: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            WorkflowRuntimeEvent::Progress { progress, .. } => Some(progress),
            _ => None,
        })
        .collect();
    assert!(progress.windows(2).all(|window| window[0] != window[1]));
    Ok(())
}

#[tokio::test]
async fn event_sink_failure_fails_workflow_and_stops_active_agents() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "persist-failure",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl {
        hold_wait: AtomicBool::new(true),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::<WorkflowRuntimeEvent>::new()));
    let captured = Arc::clone(&events);
    let failed = Arc::new(AtomicBool::new(false));
    let failed_once = Arc::clone(&failed);

    let result = manager
        .drive_with_events(
            workflow_id,
            Arc::clone(&adapter),
            Arc::new(move |event| {
                if matches!(event, WorkflowRuntimeEvent::Persist(_))
                    && !failed_once.swap(true, Ordering::Relaxed)
                {
                    return Err(WorkflowError::Agent(
                        "checked workflow persistence failed".to_string(),
                    ));
                }
                captured.lock().expect("event lock").push(event);
                Ok(())
            }),
        )
        .await?
        .await?;

    assert!(result.is_err());
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Failed
    );
    assert!(
        !events
            .lock()
            .expect("event lock")
            .iter()
            .any(|event| matches!(event, WorkflowRuntimeEvent::Completed(_)))
    );
    assert_eq!(adapter.interrupted.lock().expect("interrupt log").len(), 1);
    assert_eq!(adapter.closed.lock().expect("close log").len(), 1);
    Ok(())
}

#[tokio::test]
async fn cancel_marks_queued_tasks_without_spawning() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "cancel",
        vec![task("a", &[]), task("b", &["a"])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    manager.cancel(workflow_id).await?;
    let adapter = Arc::new(FakeAgentControl::default());

    manager.drive(workflow_id, adapter.clone()).await?.await??;

    assert!(adapter.spawned.lock().expect("spawn log lock").is_empty());
    assert!(
        manager
            .snapshot(workflow_id)
            .await?
            .tasks
            .iter()
            .all(|task| task.state == codex_workflows::WorkflowTaskState::Cancelled)
    );
    Ok(())
}

#[tokio::test]
async fn real_agent_control_preserves_role_metadata_and_waits_for_completion() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collab");
            config
                .features
                .enable(Feature::MultiAgentV2)
                .expect("enable multi-agent v2");
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        });
    let test = builder.build_with_auto_env(&server).await?;
    let _mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("workflow-agent-response"),
            ev_assistant_message("workflow-agent-message", "completed"),
            ev_completed("workflow-agent-response"),
        ]),
    )
    .await;
    let adapter = adapter_for_thread(&test.codex).await;
    let manager = manager_for_thread(&test.codex);
    let expected_limits = test.config.workflows.workflow_limits(Some(
        test.config
            .multi_agent_v2
            .max_concurrent_threads_per_session
            .saturating_sub(1),
    ));
    assert_eq!(
        manager.config().max_concurrent_tasks,
        expected_limits.max_concurrent_tasks
    );
    assert!(manager.config().max_concurrent_tasks <= expected_limits.max_concurrent_tasks);
    let parent_config = test.codex.config_snapshot().await;
    let agent_id = adapter
        .spawn(WorkflowAgentRequest {
            workflow_id: codex_workflows::WorkflowId::new(),
            task_id: WorkflowTaskId::from("metadata"),
            title: "metadata".to_string(),
            instructions: "complete the workflow task".to_string(),
            agent_role: Some("explorer".to_string()),
        })
        .await?;
    let child_id = codex_protocol::ThreadId::try_from(agent_id.as_str())?;
    let child = test.thread_manager.get_thread(child_id).await?;
    let child_config = child.config_snapshot().await;
    assert_eq!(
        child_config.session_source.get_agent_role(),
        Some("explorer".to_string())
    );
    assert!(child_config.parent_thread_id.is_some());
    assert_eq!(child_config.approval_policy, parent_config.approval_policy);
    assert_eq!(
        child_config.permission_profile,
        parent_config.permission_profile
    );
    let outcomes = adapter
        .wait(vec![agent_id.clone()], Duration::from_secs(2))
        .await?;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].status,
        WorkflowAgentOutcomeStatus::Succeeded {
            summary: "completed".to_string()
        }
    );
    adapter.close(agent_id).await?;
    Ok(())
}

#[tokio::test]
async fn session_workflow_manager_uses_v1_v2_and_disabled_versions() -> Result<()> {
    let server = start_mock_server().await;
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();

    let v1_test = test_codex()
        .with_auth(auth.clone())
        .with_config(|config| {
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collab");
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("disable multi-agent v2");
        })
        .build_with_auto_env(&server)
        .await?;
    assert!(manager_for_thread(&v1_test.codex).config().enabled);
    assert!(
        manager_for_thread(&v1_test.codex)
            .config()
            .max_concurrent_tasks
            > 0
    );

    let v2_test = test_codex()
        .with_auth(auth.clone())
        .with_config(|config| {
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collab");
            config
                .features
                .enable(Feature::MultiAgentV2)
                .expect("enable multi-agent v2");
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        })
        .build_with_auto_env(&server)
        .await?;
    assert!(manager_for_thread(&v2_test.codex).config().enabled);
    assert_eq!(
        manager_for_thread(&v2_test.codex)
            .config()
            .max_concurrent_tasks,
        1
    );

    let disabled_test = test_codex()
        .with_auth(auth)
        .with_config(|config| {
            config.agents_enabled = false;
            config
                .features
                .disable(Feature::Collab)
                .expect("disable collab");
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("disable multi-agent v2");
        })
        .build_with_auto_env(&server)
        .await?;
    let manager = manager_for_thread(&disabled_test.codex);
    assert!(!manager.config().enabled);
    let definition = WorkflowDefinition::try_new(
        "disabled-session",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    assert!(matches!(
        manager.start(definition, ThreadId::default()).await,
        Err(WorkflowError::WorkflowsDisabled)
    ));
    Ok(())
}

fn task(id: &str, dependencies: &[&str]) -> WorkflowTask {
    WorkflowTask {
        id: WorkflowTaskId::from(id),
        title: id.to_string(),
        instructions: format!("run {id}"),
        dependencies: dependencies
            .iter()
            .copied()
            .map(WorkflowTaskId::from)
            .collect(),
        agent_role: None,
        intent: WorkflowTaskIntent::ReadOnly,
        owned_paths: Vec::new(),
        verification: Vec::new(),
        max_attempts: 1,
    }
}

#[tokio::test]
async fn manager_runs_independent_tasks_before_dependents() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "workflow",
        vec![task("a", &[]), task("b", &[]), task("c", &["a", "b"])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 2)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let adapter = Arc::new(FakeAgentControl::default());

    manager.drive(workflow_id, adapter.clone()).await?.await??;

    assert_eq!(
        adapter.spawned.lock().expect("spawn log lock").as_slice(),
        [
            WorkflowTaskId::from("a"),
            WorkflowTaskId::from("b"),
            WorkflowTaskId::from("c")
        ]
    );
    assert!(manager.snapshot(workflow_id).await?.state.is_terminal());
    Ok(())
}

#[tokio::test]
async fn workflow_start_plans_executes_dependencies_emits_events_and_persists() -> Result<()> {
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("planner-response"),
                ev_assistant_message(
                    "planner-message",
                    r#"{"schema_version":1,"title":"planned","objective":"implement the plan","tasks":[{"id":"first","title":"first","instructions":"run first","dependencies":[],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1},{"id":"second","title":"second","instructions":"run second","dependencies":["first"],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
                ),
                ev_completed("planner-response"),
            ]),
            sse(vec![
                ev_response_created("first-response"),
                ev_assistant_message("first-message", "first done"),
                ev_completed("first-response"),
            ]),
            sse(vec![
                ev_response_created("second-response"),
                ev_assistant_message("second-message", "second done"),
                ev_completed("second-response"),
            ]),
        ],
    )
    .await;
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let test = test_codex()
        .with_auth(auth)
        .with_config(|config| {
            config
                .features
                .enable(Feature::DynamicWorkflows)
                .expect("enable dynamic workflows");
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collab");
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("disable multi-agent v2");
        })
        .build_with_auto_env(&server)
        .await?;

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowStart(
            codex_protocol::workflow::WorkflowStartParams {
                objective: codex_protocol::workflow::WorkflowObjective::new("implement the plan")?,
                policy: Some(codex_protocol::workflow::WorkflowPolicy::Ultracode),
            },
        ))
        .await?;

    let completed = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowCompleted(completed) = completed else {
        unreachable!("workflow completed event")
    };
    assert_eq!(completed.summary().completed_tasks(), 2);

    let requests = responses.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .body_json()
            .to_string()
            .contains("schema_version")
    );
    assert!(requests[1].body_json().to_string().contains("run first"));
    assert!(requests[2].body_json().to_string().contains("run second"));

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    test.codex.flush_rollout().await?;
    let rollout = std::fs::read_to_string(rollout_path)?;
    assert!(rollout.contains("workflow_state"));
    let states = persisted_workflow_states(&rollout, completed.workflow_id().as_str())?;
    assert!(contains_state_subsequence(
        &states,
        &["planning", "running", "completed"]
    ));
    Ok(())
}

#[tokio::test]
async fn deep_research_policy_reaches_the_native_planner() -> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_sse_once(
        &server,
        planner_response(
            "deep-research-planner",
            r#"{"schema_version":1,"title":"research","objective":"compare schedulers","tasks":[]}"#,
        ),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowStart(
            codex_protocol::workflow::WorkflowStartParams {
                objective: codex_protocol::workflow::WorkflowObjective::new("compare schedulers")?,
                policy: Some(codex_protocol::workflow::WorkflowPolicy::DeepResearch),
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;

    let request = planner.single_request().body_json().to_string();
    for requirement in [
        "independent research dimensions",
        "read-only",
        "source or repository evidence",
        "disagreements",
        "known unknowns",
        "final report",
    ] {
        assert!(
            request.contains(requirement),
            "planner prompt omitted {requirement:?}: {request}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn planner_repairs_once_and_second_invalid_plan_fails_without_workers() -> Result<()> {
    let server = start_mock_server().await;
    let repaired = mount_sse_sequence(
        &server,
        vec![
            planner_response(
                "repair-invalid",
                r#"{"schema_version":1,"title":"bad","objective":"repair","tasks":[{"id":"a","title":"a","instructions":"a","dependencies":["missing"],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
            ),
            planner_response(
                "repair-valid",
                r#"{"schema_version":1,"title":"fixed","objective":"repair","tasks":[{"id":"a","title":"a","instructions":"run repaired","dependencies":[],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
            ),
            worker_response("repair-worker", "repaired"),
        ],
    )
    .await;
    let repaired_test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    submit_workflow_start(&repaired_test.codex, "repair").await?;
    wait_for_event(&repaired_test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;
    let requests = repaired.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("depends on missing task missing")
    );
    assert!(requests[1].body_json().to_string().contains("repair"));
    assert!(requests[1].body_json().to_string().contains("dependencies"));

    let failed = mount_sse_sequence(
        &server,
        vec![
            planner_response("failed-invalid-one", "not json"),
            planner_response("failed-invalid-two", "not json"),
        ],
    )
    .await;
    let failed_test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    submit_workflow_start(&failed_test.codex, "fail planning").await?;
    wait_for_event(&failed_test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::WorkflowFailed(_))
    })
    .await;
    assert_eq!(failed.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn planner_repairs_unknown_schema_version_with_exact_error() -> Result<()> {
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            planner_response(
                "version-invalid",
                r#"{"schema_version":2,"title":"bad","objective":"version","tasks":[]}"#,
            ),
            planner_response(
                "version-valid",
                r#"{"schema_version":1,"title":"fixed","objective":"version","tasks":[]}"#,
            ),
        ],
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, "version").await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("unsupported workflow schema version 2")
    );
    Ok(())
}

#[tokio::test]
async fn planner_repair_with_long_objective_preserves_exact_validation_error() -> Result<()> {
    let server = start_mock_server().await;
    let objective = "o".repeat(16 * 1024);
    let responses = mount_sse_sequence(
        &server,
        vec![
            planner_response(
                "long-objective-invalid",
                r#"{"schema_version":1,"title":"bad","objective":"bad","tasks":[{"id":"dependent","title":"dependent","instructions":"run","dependencies":["missing"],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
            ),
            planner_response(
                "long-objective-valid",
                r#"{"schema_version":1,"title":"fixed","objective":"fixed","tasks":[]}"#,
            ),
        ],
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, &objective).await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;

    assert!(
        responses.requests()[1]
            .body_json()
            .to_string()
            .contains("task dependent depends on missing task missing")
    );
    Ok(())
}

#[tokio::test]
async fn oversized_planner_title_fails_with_safe_planning_snapshot() -> Result<()> {
    let server = start_mock_server().await;
    let invalid = format!(
        r#"{{"schema_version":1,"title":"{}","objective":"unsafe","tasks":[]}}"#,
        "t".repeat(257)
    );
    let responses = mount_sse_sequence(
        &server,
        vec![
            planner_response("unsafe-title-first", &invalid),
            planner_response("unsafe-title-second", &invalid),
        ],
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, "unsafe").await?;
    let failed = wait_for_event(&test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::WorkflowFailed(_))
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowFailed(failed) = failed else {
        unreachable!("workflow failed event")
    };
    let workflow_id: codex_workflows::WorkflowId =
        serde_json::from_value(Value::String(failed.workflow_id().as_str().to_string()))?;
    let snapshot = manager_for_thread(&test.codex)
        .snapshot(workflow_id)
        .await?;
    assert_eq!(snapshot.state, codex_workflows::WorkflowState::Failed);
    assert_eq!(snapshot.definition.title, "Planning workflow");
    assert_eq!(snapshot.definition.objective, "unsafe");
    assert!(!snapshot.is_definition_accepted());
    assert_eq!(responses.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn planner_rejects_oversized_response_before_deserialization() -> Result<()> {
    let server = start_mock_server().await;
    let oversized = format!(
        r#"{{"schema_version":1,"title":"oversized","objective":"oversized","tasks":[],"padding":"{}"}}"#,
        "x".repeat(70 * 1024)
    );
    let responses = mount_sse_sequence(
        &server,
        vec![
            planner_response("oversized-first", &oversized),
            planner_response("oversized-second", &oversized),
        ],
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, "oversized").await?;
    let failed = wait_for_event(&test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::WorkflowFailed(_))
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowFailed(failed) = failed else {
        unreachable!("workflow failed event")
    };
    assert!(failed.error().as_str().contains("maximum response size"));
    assert_eq!(responses.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn workflow_start_then_list_observes_registered_planning_state_in_submission_order()
-> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_response_once_match(
        &server,
        BodyContains("ordering objective"),
        sse_response(planner_response(
            "ordering-planner",
            r#"{"schema_version":1,"title":"ordering","objective":"ordering objective","tasks":[]}"#,
        ))
        .set_delay(Duration::from_millis(500)),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, "ordering objective").await?;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowList(
            codex_protocol::workflow::WorkflowListParams,
        ))
        .await?;

    let started = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowStarted(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowStarted(started) = started else {
        unreachable!("workflow started event")
    };
    assert_eq!(
        started.summary().status(),
        codex_protocol::workflow::WorkflowStatus::Planning
    );
    let progress = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowProgress(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowProgress(progress) = progress else {
        unreachable!("workflow progress event")
    };
    assert_eq!(progress.workflow_id(), started.workflow_id());
    assert_eq!(
        progress.state(),
        codex_protocol::workflow::WorkflowStatus::Planning
    );
    for _ in 0..100 {
        if !planner.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    planner.single_request();
    Ok(())
}

#[tokio::test]
async fn workflow_cancel_during_planning_stays_cancelled_after_planner_returns() -> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_response_once_match(
        &server,
        BodyContains("cancel during planning"),
        sse_response(planner_response(
            "cancel-planner",
            r#"{"schema_version":1,"title":"late","objective":"cancel during planning","tasks":[{"id":"worker","title":"worker","instructions":"run worker","dependencies":[],"agent_role":"worker","intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
        ))
        .set_delay(Duration::from_millis(500)),
    )
    .await;
    let worker = mount_response_once_match(
        &server,
        BodyContains("run worker"),
        sse_response(worker_response("late-worker", "should not run")),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    let manager = manager_for_thread(&test.codex);

    submit_workflow_start(&test.codex, "cancel during planning").await?;
    let started = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowStarted(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowStarted(started) = started else {
        unreachable!("workflow started event")
    };
    let workflow_id: WorkflowId =
        serde_json::from_value(Value::String(started.workflow_id().as_str().to_string()))?;
    for _ in 0..100 {
        if !planner.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowCancel(
            codex_protocol::workflow::WorkflowCancelParams {
                workflow_id: protocol_workflow_id(workflow_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCancelled(_)
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Cancelled
    );
    assert_eq!(worker.requests().len(), 0);
    Ok(())
}

#[tokio::test]
async fn drive_error_atomically_fails_and_emits_the_same_failed_snapshot() -> Result<()> {
    let definition = WorkflowDefinition::try_new(
        "drive-error",
        vec![task("a", &[])],
        WorkflowLimits::default(),
    )?;
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let workflow_id = manager.start(definition, ThreadId::default()).await?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let adapter = Arc::new(FakeAgentControl {
        wait_error: Mutex::new(Some(WorkflowError::Agent("adapter failed".to_string()))),
        ..Default::default()
    });

    let result = manager
        .drive_with_events(
            workflow_id,
            adapter,
            Arc::new(move |event| {
                captured.lock().expect("event lock").push(event);
                Ok(())
            }),
        )
        .await?
        .await?;
    assert!(result.is_err());
    let snapshot = manager.snapshot(workflow_id).await?;
    assert_eq!(snapshot.state, codex_workflows::WorkflowState::Failed);
    assert!(events.lock().expect("event lock").iter().any(|event| {
        matches!(
            event,
            WorkflowRuntimeEvent::Failed {
                snapshot: event_snapshot,
                ..
            }
                if event_snapshot == &snapshot
        )
    }));
    Ok(())
}

#[tokio::test]
async fn workflow_ops_read_list_pause_resume_cancel_and_retry() -> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_response_once_match(
        &server,
        BodyContains("pause controlled workflow"),
        sse_response(planner_response(
            "pause-planner",
            r#"{"schema_version":1,"title":"paused","objective":"pause controlled workflow","tasks":[{"id":"paused","title":"paused","instructions":"run paused","dependencies":[],"agent_role":null,"intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
        )),
    )
    .await;
    let interrupted_worker = mount_response_once_match(
        &server,
        LastUserInput("run paused"),
        sse_response(worker_response("interrupted-worker", "too late"))
            .set_delay(Duration::from_secs(30)),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    let manager = manager_for_thread(&test.codex);

    submit_workflow_start(&test.codex, "pause controlled workflow").await?;
    let started = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowStarted(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowStarted(started) = started else {
        unreachable!("workflow started event")
    };
    let paused_id: WorkflowId =
        serde_json::from_value(Value::String(started.workflow_id().as_str().to_string()))?;
    for _ in 0..100 {
        if !interrupted_worker.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    interrupted_worker.single_request();
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowPause(
            codex_protocol::workflow::WorkflowPauseParams {
                workflow_id: protocol_workflow_id(paused_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::WorkflowPaused(_))
    })
    .await;

    let resumed_worker = mount_response_once_match(
        &server,
        LastUserInput("run paused"),
        sse_response(worker_response("resume-worker", "resumed")),
    )
    .await;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowResume(
            codex_protocol::workflow::WorkflowResumeParams {
                workflow_id: protocol_workflow_id(paused_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;

    test.codex.flush_rollout().await?;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let rollout = std::fs::read_to_string(rollout_path)?;
    let states = persisted_workflow_states(&rollout, &paused_id.to_string())?;
    assert!(contains_state_subsequence(
        &states,
        &["pausing", "paused", "running", "completed"]
    ));

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowRead(
            codex_protocol::workflow::WorkflowReadParams {
                workflow_id: protocol_workflow_id(paused_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowProgress(_)
        )
    })
    .await;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowList(
            codex_protocol::workflow::WorkflowListParams,
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowProgress(_)
        )
    })
    .await;

    let cancelled_id = manager
        .start(
            WorkflowDefinition::try_new(
                "cancelled",
                vec![task("cancelled", &[])],
                WorkflowLimits::default(),
            )?,
            test.session_configured.thread_id,
        )
        .await?;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowCancel(
            codex_protocol::workflow::WorkflowCancelParams {
                workflow_id: protocol_workflow_id(cancelled_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCancelled(_)
        )
    })
    .await;

    let mut retry_task = task("retry-op", &[]);
    retry_task.max_attempts = 1;
    let retry_id = manager
        .start(
            WorkflowDefinition::try_new("retry", vec![retry_task], WorkflowLimits::default())?,
            test.session_configured.thread_id,
        )
        .await?;
    manager
        .drive(
            retry_id,
            Arc::new(FakeAgentControl {
                fail_next: AtomicBool::new(true),
                ..Default::default()
            }),
        )
        .await?
        .await??;
    let retry_worker = mount_response_once_match(
        &server,
        LastUserInput("run retry-op"),
        sse_response(worker_response("retry-worker", "retried")),
    )
    .await;
    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowRetry(
            codex_protocol::workflow::WorkflowRetryParams {
                workflow_id: protocol_workflow_id(retry_id)?,
            },
        ))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowCompleted(_)
        )
    })
    .await;
    planner.single_request();
    resumed_worker.single_request();
    retry_worker.single_request();
    Ok(())
}

#[tokio::test]
async fn workflow_ops_report_feature_and_multi_agent_errors() -> Result<()> {
    let server = start_mock_server().await;
    let feature_disabled = workflow_session(
        &server, /*workflows_enabled*/ false, /*agents_enabled*/ true,
    )
    .await?;
    submit_workflow_start(&feature_disabled.codex, "disabled").await?;
    let error = wait_for_event(&feature_disabled.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::Error(_))
    })
    .await;
    let codex_protocol::protocol::EventMsg::Error(error) = error else {
        unreachable!("workflow error")
    };
    assert!(error.message.contains("feature is disabled"));

    let agents_disabled = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ false,
    )
    .await?;
    submit_workflow_start(&agents_disabled.codex, "disabled").await?;
    let error = wait_for_event(&agents_disabled.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::Error(_))
    })
    .await;
    let codex_protocol::protocol::EventMsg::Error(error) = error else {
        unreachable!("workflow error")
    };
    assert!(error.message.contains("multi-agent support"));

    Ok(())
}

#[tokio::test]
async fn workflow_background_execution_does_not_block_a_regular_turn() -> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_sse_once(
        &server,
        planner_response(
            "background-planner",
            r#"{"schema_version":1,"title":"background","objective":"background","tasks":[{"id":"slow","title":"slow","instructions":"run slowly","dependencies":[],"agent_role":"worker","intent":"read_only","owned_paths":[],"verification":[],"max_attempts":1}]}"#,
        ),
    )
    .await;
    let worker = mount_response_once_match(
        &server,
        BodyContains("run slowly"),
        sse_response(worker_response("background-worker", "slow done"))
            .set_delay(Duration::from_secs(2)),
    )
    .await;
    let regular = mount_sse_once_match(
        &server,
        BodyContains("second ordinary turn"),
        worker_response("regular-turn", "regular done"),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    submit_workflow_start(&test.codex, "background").await?;
    let started_event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowStarted(_)
        )
    })
    .await;
    let codex_protocol::protocol::EventMsg::WorkflowStarted(started_event) = started_event else {
        unreachable!("workflow started event")
    };
    let workflow_id: codex_workflows::WorkflowId = serde_json::from_value(
        serde_json::Value::String(started_event.workflow_id().as_str().to_string()),
    )?;

    let manager = manager_for_thread(&test.codex);
    for _ in 0..100 {
        if manager.snapshot(workflow_id).await?.state == codex_workflows::WorkflowState::Running
            && !worker.requests().is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Running
    );
    assert_eq!(worker.requests().len(), 1);

    let started = std::time::Instant::now();
    test.codex
        .submit(codex_protocol::protocol::Op::UserInput {
            items: vec![codex_protocol::user_input::UserInput::Text {
                text: "second ordinary turn".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    for _ in 0..100 {
        if !regular.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(regular.requests().len(), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        manager_for_thread(&test.codex)
            .snapshot(workflow_id)
            .await?
            .state,
        codex_workflows::WorkflowState::Running
    );
    wait_for_event(&test.codex, |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;
    for _ in 0..300 {
        if manager.snapshot(workflow_id).await?.state.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        codex_workflows::WorkflowState::Completed
    );

    planner.single_request();
    worker.single_request();
    regular.single_request();
    Ok(())
}

#[tokio::test]
async fn workflow_pause_does_not_block_an_ordinary_turn_while_agent_is_stopping() -> Result<()> {
    let server = start_mock_server().await;
    let regular = mount_sse_once_match(
        &server,
        BodyContains("ordinary during pause"),
        worker_response("pause-ordinary", "ordinary done"),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;
    let manager = manager_for_thread(&test.codex);
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "pause ordinary",
                vec![task("slow", &[])],
                WorkflowLimits::default(),
            )?,
            test.session_configured.thread_id,
        )
        .await?;
    let adapter = Arc::new(FakeAgentControl {
        never_finish_wait: AtomicBool::new(true),
        ..Default::default()
    });
    let drive = manager.drive(workflow_id, adapter).await?;
    tokio::time::sleep(Duration::from_millis(20)).await;

    test.codex
        .submit(codex_protocol::protocol::Op::WorkflowPause(
            codex_protocol::workflow::WorkflowPauseParams {
                workflow_id: protocol_workflow_id(workflow_id)?,
            },
        ))
        .await?;
    test.codex
        .submit(codex_protocol::protocol::Op::UserInput {
            items: vec![codex_protocol::user_input::UserInput::Text {
                text: "ordinary during pause".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    for _ in 0..100 {
        if !regular.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let request_was_sent = regular.requests().len() == 1;
    let state_during_request = manager.snapshot(workflow_id).await?.state;
    drive.abort();
    assert!(request_was_sent);
    assert_eq!(
        state_during_request,
        codex_workflows::WorkflowState::Pausing
    );
    regular.single_request();
    Ok(())
}

#[tokio::test]
async fn workflow_session_teardown_closes_background_planner_without_registry() -> Result<()> {
    let server = start_mock_server().await;
    let planner = mount_response_once_match(
        &server,
        BodyContains("teardown planner"),
        sse_response(planner_response(
            "teardown-planner",
            r#"{"schema_version":1,"title":"teardown","objective":"teardown planner","tasks":[]}"#,
        ))
        .set_delay(Duration::from_secs(30)),
    )
    .await;
    let test = workflow_session(
        &server, /*workflows_enabled*/ true, /*agents_enabled*/ true,
    )
    .await?;

    submit_workflow_start(&test.codex, "teardown planner").await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            codex_protocol::protocol::EventMsg::WorkflowStarted(_)
        )
    })
    .await;
    for _ in 0..100 {
        if !planner.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    planner.single_request();

    tokio::time::timeout(Duration::from_secs(2), test.codex.shutdown_and_wait()).await??;
    Ok(())
}

struct BodyContains(&'static str);

impl wiremock::Match for BodyContains {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let body = zstd::stream::decode_all(std::io::Cursor::new(&request.body))
            .unwrap_or_else(|_| request.body.clone());
        String::from_utf8_lossy(&body).contains(self.0)
    }
}

struct LastUserInput(&'static str);

impl wiremock::Match for LastUserInput {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let body = zstd::stream::decode_all(std::io::Cursor::new(&request.body))
            .unwrap_or_else(|_| request.body.clone());
        let Ok(body) = serde_json::from_slice::<Value>(&body) else {
            return false;
        };
        body.get("input")
            .and_then(Value::as_array)
            .and_then(|input| {
                input
                    .iter()
                    .rev()
                    .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
            })
            .and_then(|item| item.get("content"))
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|item| item.get("text").and_then(Value::as_str) == Some(self.0))
            })
    }
}

async fn workflow_session(
    server: &wiremock::MockServer,
    workflows_enabled: bool,
    agents_enabled: bool,
) -> Result<core_test_support::test_codex::TestCodex> {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    test_codex()
        .with_auth(auth)
        .with_config(move |config| {
            if workflows_enabled {
                config
                    .features
                    .enable(Feature::DynamicWorkflows)
                    .expect("enable dynamic workflows");
            } else {
                config
                    .features
                    .disable(Feature::DynamicWorkflows)
                    .expect("disable dynamic workflows");
            }
            config.agents_enabled = agents_enabled;
            if agents_enabled {
                config
                    .features
                    .enable(Feature::Collab)
                    .expect("enable collab");
            } else {
                config
                    .features
                    .disable(Feature::Collab)
                    .expect("disable collab");
            }
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("disable multi-agent v2");
        })
        .build_with_auto_env(server)
        .await
}

async fn submit_workflow_start(codex: &codex_core::CodexThread, objective: &str) -> Result<()> {
    codex
        .submit(codex_protocol::protocol::Op::WorkflowStart(
            codex_protocol::workflow::WorkflowStartParams {
                objective: codex_protocol::workflow::WorkflowObjective::new(objective)?,
                policy: Some(codex_protocol::workflow::WorkflowPolicy::Ultracode),
            },
        ))
        .await?;
    Ok(())
}

fn planner_response(id: &str, message: &str) -> String {
    sse(vec![
        ev_response_created(id),
        ev_assistant_message(&format!("{id}-message"), message),
        ev_completed(id),
    ])
}

fn worker_response(id: &str, message: &str) -> String {
    planner_response(id, message)
}

fn protocol_workflow_id(
    workflow_id: codex_workflows::WorkflowId,
) -> Result<codex_protocol::workflow::WorkflowId> {
    Ok(codex_protocol::workflow::WorkflowId::new(
        workflow_id.to_string(),
    )?)
}

fn persisted_workflow_states(rollout: &str, workflow_id: &str) -> Result<Vec<String>> {
    Ok(persisted_workflow_snapshots(rollout, workflow_id)?
        .into_iter()
        .filter_map(|snapshot| {
            snapshot
                .get("state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect())
}

fn persisted_workflow_snapshots(rollout: &str, workflow_id: &str) -> Result<Vec<Value>> {
    let snapshots = rollout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .filter(|value| value.get("type").and_then(Value::as_str) == Some("workflow_state"))
        .filter(|value| {
            value
                .pointer("/payload/workflow_id")
                .and_then(Value::as_str)
                == Some(workflow_id)
        })
        .filter_map(|value| value.pointer("/payload/snapshot").cloned())
        .collect::<Vec<_>>();
    Ok(snapshots)
}

fn contains_state_subsequence(states: &[String], expected: &[&str]) -> bool {
    let mut expected = expected.iter();
    let mut next = expected.next();
    for state in states {
        if next.is_some_and(|expected| state == expected) {
            next = expected.next();
        }
    }
    next.is_none()
}
