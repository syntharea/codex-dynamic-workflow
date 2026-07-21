use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::ThreadId;
use codex_workflows::WorkflowAgentControl;
use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowAgentOutcome;
use codex_workflows::WorkflowAgentRequest;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use codex_workflows::WorkflowTaskState;
use tokio::sync::mpsc;

use super::super::WorkflowManager;
use super::super::WorkflowRuntimeEvent;
use super::commit_or_fail_action;
use super::supervise_drive;

#[derive(Default)]
struct BlockingAgentControl {
    spawned: Mutex<Vec<WorkflowAgentId>>,
    interrupted: Mutex<Vec<WorkflowAgentId>>,
    closed: Mutex<Vec<WorkflowAgentId>>,
}

struct CleanupBlockingAgentControl {
    interrupt_started: tokio::sync::Notify,
    interrupt_release: tokio::sync::Notify,
    close_started: tokio::sync::Notify,
    close_release: tokio::sync::Notify,
    interrupted: Mutex<Vec<WorkflowAgentId>>,
    closed: Mutex<Vec<WorkflowAgentId>>,
}

impl Default for CleanupBlockingAgentControl {
    fn default() -> Self {
        Self {
            interrupt_started: tokio::sync::Notify::new(),
            interrupt_release: tokio::sync::Notify::new(),
            close_started: tokio::sync::Notify::new(),
            close_release: tokio::sync::Notify::new(),
            interrupted: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
        }
    }
}

impl WorkflowAgentControl for CleanupBlockingAgentControl {
    async fn spawn(
        &self,
        _request: WorkflowAgentRequest,
    ) -> Result<WorkflowAgentId, WorkflowError> {
        Ok(WorkflowAgentId::from("cleanup-agent"))
    }

    async fn wait(
        &self,
        _agents: Vec<WorkflowAgentId>,
        _timeout: Duration,
    ) -> Result<Vec<WorkflowAgentOutcome>, WorkflowError> {
        std::future::pending().await
    }

    async fn interrupt(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.interrupted.lock().unwrap().push(agent);
        self.interrupt_started.notify_waiters();
        self.interrupt_release.notified().await;
        Ok(())
    }

    async fn close(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.closed.lock().unwrap().push(agent);
        self.close_started.notify_waiters();
        self.close_release.notified().await;
        Ok(())
    }
}

impl WorkflowAgentControl for BlockingAgentControl {
    async fn spawn(
        &self,
        _request: WorkflowAgentRequest,
    ) -> Result<WorkflowAgentId, WorkflowError> {
        let agent = WorkflowAgentId::from("agent-1");
        self.spawned.lock().unwrap().push(agent.clone());
        Ok(agent)
    }

    async fn wait(
        &self,
        _agents: Vec<WorkflowAgentId>,
        _timeout: Duration,
    ) -> Result<Vec<WorkflowAgentOutcome>, WorkflowError> {
        std::future::pending().await
    }

    async fn interrupt(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.interrupted.lock().unwrap().push(agent);
        Ok(())
    }

    async fn close(&self, agent: WorkflowAgentId) -> Result<(), WorkflowError> {
        self.closed.lock().unwrap().push(agent);
        Ok(())
    }
}

#[tokio::test]
async fn dynamic_workflows_consumer_failure_aborts_driver_and_agents() -> Result<()> {
    let manager = Arc::new(WorkflowManager::new(/*max_concurrent_tasks*/ 1)?);
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "consumer failure",
                vec![WorkflowTask {
                    id: WorkflowTaskId::from("task"),
                    title: "task".to_string(),
                    instructions: "run task".to_string(),
                    dependencies: Vec::new(),
                    agent_role: None,
                    intent: WorkflowTaskIntent::ReadOnly,
                    owned_paths: Vec::new(),
                    verification: Vec::new(),
                    max_attempts: 1,
                }],
                WorkflowLimits::default(),
            )?,
            ThreadId::default(),
        )
        .await?;
    let adapter = Arc::new(CleanupBlockingAgentControl::default());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(move |event| {
        event_tx
            .send(event)
            .map_err(|_| WorkflowError::Agent("workflow event consumer is unavailable".to_string()))
    });
    let drive_task = manager
        .drive_with_events(workflow_id, Arc::clone(&adapter), sink)
        .await?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&received);
    let event_task = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            captured.lock().unwrap().push(event.clone());
            if matches!(event, WorkflowRuntimeEvent::Persist(_)) {
                anyhow::bail!("checked workflow persistence failed");
            }
        }
        Ok(())
    });

    let supervision = tokio::spawn(supervise_drive(
        Arc::clone(&manager),
        workflow_id,
        Arc::clone(&adapter),
        drive_task,
        event_task,
    ));
    adapter.interrupt_started.notified().await;
    assert_eq!(
        manager.retry_transition(workflow_id).await,
        Err(WorkflowError::AlreadyRunning)
    );
    adapter.interrupt_release.notify_waiters();
    adapter.close_started.notified().await;
    assert_eq!(
        manager.retry_transition(workflow_id).await,
        Err(WorkflowError::AlreadyRunning)
    );
    adapter.close_release.notify_waiters();
    let result = supervision.await?;

    assert!(result.is_err());
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        WorkflowState::Failed
    );
    assert!(
        !received
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, WorkflowRuntimeEvent::Completed(_)))
    );
    assert_eq!(adapter.interrupted.lock().unwrap().len(), 1);
    assert_eq!(adapter.closed.lock().unwrap().len(), 1);
    Ok(())
}

#[tokio::test]
async fn dynamic_workflows_cancellation_signal_survives_check_wait_gap() -> Result<()> {
    let manager = WorkflowManager::new(/*max_concurrent_tasks*/ 1)?;
    let snapshot = manager
        .begin_planning("planner cancellation race".to_string(), ThreadId::default())
        .await?;
    let cancellation = manager.cancellation_signal(snapshot.workflow_id).await?;
    assert!(!manager.is_cancelled(snapshot.workflow_id).await?);

    manager.cancel(snapshot.workflow_id).await?;
    tokio::time::timeout(Duration::from_millis(50), cancellation.notified())
        .await
        .context("cancellation notification was lost between check and wait")?;
    Ok(())
}

#[tokio::test]
async fn dynamic_workflows_control_commit_failure_stops_active_agents() -> Result<()> {
    let manager = Arc::new(WorkflowManager::new(/*max_concurrent_tasks*/ 1)?);
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "control failure",
                vec![WorkflowTask {
                    id: WorkflowTaskId::from("task"),
                    title: "task".to_string(),
                    instructions: "run task".to_string(),
                    dependencies: Vec::new(),
                    agent_role: None,
                    intent: WorkflowTaskIntent::ReadOnly,
                    owned_paths: Vec::new(),
                    verification: Vec::new(),
                    max_attempts: 1,
                }],
                WorkflowLimits::default(),
            )?,
            ThreadId::default(),
        )
        .await?;
    let adapter = Arc::new(BlockingAgentControl::default());
    let drive = manager.drive(workflow_id, Arc::clone(&adapter)).await?;
    for _ in 0..100 {
        if !adapter.spawned.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let result = commit_or_fail_action(
        &manager,
        workflow_id,
        Arc::clone(&adapter),
        Err(anyhow::anyhow!("resume snapshot persistence failed")),
    )
    .await;

    assert!(result.is_err());
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        WorkflowState::Failed
    );
    assert_eq!(adapter.interrupted.lock().unwrap().len(), 1);
    assert_eq!(adapter.closed.lock().unwrap().len(), 1);

    assert_eq!(
        manager.retry_transition(workflow_id).await,
        Err(WorkflowError::AlreadyRunning)
    );
    tokio::time::timeout(Duration::from_secs(1), drive)
        .await
        .context("workflow driver did not finish after persistence failure")?
        .context("workflow driver task panicked")??;
    manager.retry_transition(workflow_id).await?;
    Ok(())
}

#[tokio::test]
async fn dynamic_workflows_control_failure_blocks_retry_during_agent_cleanup() -> Result<()> {
    let manager = Arc::new(WorkflowManager::new(/*max_concurrent_tasks*/ 1)?);
    let workflow_id = manager
        .start(
            WorkflowDefinition::try_new(
                "cleanup gate",
                vec![WorkflowTask {
                    id: WorkflowTaskId::from("task"),
                    title: "task".to_string(),
                    instructions: "run task".to_string(),
                    dependencies: Vec::new(),
                    agent_role: None,
                    intent: WorkflowTaskIntent::ReadOnly,
                    owned_paths: Vec::new(),
                    verification: Vec::new(),
                    max_attempts: 1,
                }],
                WorkflowLimits::default(),
            )?,
            ThreadId::default(),
        )
        .await?;
    let adapter = Arc::new(CleanupBlockingAgentControl::default());
    let drive = manager.drive(workflow_id, Arc::clone(&adapter)).await?;
    for _ in 0..100 {
        if manager
            .snapshot(workflow_id)
            .await?
            .task("task")
            .is_some_and(|task| task.state == WorkflowTaskState::Running)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let failure_manager = Arc::clone(&manager);
    let failure_adapter = Arc::clone(&adapter);
    let failure = tokio::spawn(async move {
        commit_or_fail_action(
            &failure_manager,
            workflow_id,
            failure_adapter,
            Err(anyhow::anyhow!("control snapshot persistence failed")),
        )
        .await
    });
    adapter.interrupt_started.notified().await;
    assert_eq!(
        manager.retry_transition(workflow_id).await,
        Err(WorkflowError::AlreadyRunning)
    );

    adapter.interrupt_release.notify_waiters();
    adapter.close_started.notified().await;
    assert_eq!(
        manager.retry_transition(workflow_id).await,
        Err(WorkflowError::AlreadyRunning)
    );

    adapter.close_release.notify_waiters();
    let failure_result = failure.await?;
    assert!(failure_result.is_err());
    drive.await??;
    manager.retry_transition(workflow_id).await?;
    Ok(())
}
