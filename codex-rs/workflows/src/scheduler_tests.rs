use pretty_assertions::assert_eq;

use crate::WorkflowAction;
use crate::WorkflowDefinition;
use crate::WorkflowError;
use crate::WorkflowId;
use crate::WorkflowLimits;
use crate::WorkflowScheduler;
use crate::WorkflowSnapshot;
use crate::WorkflowState;
use crate::WorkflowTask;
use crate::WorkflowTaskId;
use crate::WorkflowTaskIntent;
use crate::WorkflowTaskState;

#[test]
fn rejects_zero_concurrency() {
    assert_eq!(
        WorkflowScheduler::new(/*max_concurrent_tasks*/ 0).unwrap_err(),
        WorkflowError::InvalidConcurrency
    );
}

#[test]
fn schedules_runnable_tasks_deterministically_within_concurrency_limit() {
    let mut snapshot = running_snapshot(vec![task("b", &[]), task("a", &[])]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("b")),
            WorkflowAction::Persist
        ]
    );
    assert_eq!(snapshot.task("b").unwrap().state, WorkflowTaskState::Ready);
    assert_eq!(snapshot.task("b").unwrap().attempts, 1);
}

#[test]
fn waits_for_dependencies_before_scheduling() {
    let mut snapshot = running_snapshot(vec![task("root", &[]), task("child", &["root"])]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 2).unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("root")),
            WorkflowAction::Persist
        ]
    );
    snapshot
        .transition_task(&WorkflowTaskId::from("root"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("root"), WorkflowTaskState::Succeeded)
        .unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("child")),
            WorkflowAction::Persist
        ]
    );
}

#[test]
fn serializes_overlapping_known_writes() {
    let mut first = task("first", &[]);
    first.intent = WorkflowTaskIntent::Write;
    first.owned_paths = vec!["src/auth".to_string()];
    let mut second = task("second", &[]);
    second.intent = WorkflowTaskIntent::Write;
    second.owned_paths = vec!["src/auth/token.rs".to_string()];
    let mut snapshot = running_snapshot(vec![first, second]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 2).unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("first")),
            WorkflowAction::Persist
        ]
    );
}

#[test]
fn schedules_disjoint_known_writes_concurrently() {
    let mut first = task("first", &[]);
    first.intent = WorkflowTaskIntent::Write;
    first.owned_paths = vec!["src/auth".to_string()];
    let mut second = task("second", &[]);
    second.intent = WorkflowTaskIntent::Write;
    second.owned_paths = vec!["src/payments".to_string()];
    let mut snapshot = running_snapshot(vec![first, second]);

    assert_eq!(
        WorkflowScheduler::new(/*max_concurrent_tasks*/ 2)
            .unwrap()
            .next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("first")),
            WorkflowAction::Spawn(WorkflowTaskId::from("second")),
            WorkflowAction::Persist,
        ]
    );
}

#[test]
fn root_write_ownership_conflicts_with_every_write_path() {
    let mut root = task("root", &[]);
    root.intent = WorkflowTaskIntent::Write;
    root.owned_paths = vec!["./".to_string()];
    let mut nested = task("nested", &[]);
    nested.intent = WorkflowTaskIntent::Write;
    nested.owned_paths = vec!["src/lib.rs".to_string()];
    let mut snapshot = running_snapshot(vec![root, nested]);

    assert_eq!(
        WorkflowScheduler::new(/*max_concurrent_tasks*/ 2)
            .unwrap()
            .next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("root")),
            WorkflowAction::Persist,
        ]
    );
}

#[test]
fn serializes_unknown_writes_but_allows_reads() {
    let mut unknown_write = task("unknown", &[]);
    unknown_write.intent = WorkflowTaskIntent::Write;
    let mut known_write = task("known", &[]);
    known_write.intent = WorkflowTaskIntent::Write;
    known_write.owned_paths = vec!["src/lib.rs".to_string()];
    let read = task("read", &[]);
    let mut snapshot = running_snapshot(vec![unknown_write, known_write, read]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 3).unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("unknown")),
            WorkflowAction::Spawn(WorkflowTaskId::from("read")),
            WorkflowAction::Persist,
        ]
    );
}

#[test]
fn retries_retryable_failures_until_attempts_are_exhausted() {
    let mut snapshot = running_snapshot(vec![WorkflowTask {
        max_attempts: 2,
        ..task("retry", &[])
    }]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();

    scheduler.next_actions(&mut snapshot);
    snapshot
        .transition_task(&WorkflowTaskId::from("retry"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .record_task_failure(
            &WorkflowTaskId::from("retry"),
            /*retryable*/ true,
            "first",
        )
        .unwrap();
    assert_eq!(
        snapshot.task("retry").unwrap().state,
        WorkflowTaskState::Queued
    );

    scheduler.next_actions(&mut snapshot);
    snapshot
        .transition_task(&WorkflowTaskId::from("retry"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .record_task_failure(
            &WorkflowTaskId::from("retry"),
            /*retryable*/ true,
            "second",
        )
        .unwrap();
    assert_eq!(
        snapshot.task("retry").unwrap().state,
        WorkflowTaskState::Failed
    );
}

#[test]
fn blocks_dependents_but_continues_independent_branches() {
    let mut snapshot = running_snapshot(vec![
        WorkflowTask {
            max_attempts: 1,
            ..task("failed", &[])
        },
        task("blocked", &["failed"]),
        task("independent", &[]),
    ]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();

    scheduler.next_actions(&mut snapshot);
    snapshot
        .transition_task(&WorkflowTaskId::from("failed"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .record_task_failure(
            &WorkflowTaskId::from("failed"),
            /*retryable*/ true,
            "failed",
        )
        .unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("independent")),
            WorkflowAction::Persist,
        ]
    );
    assert_eq!(
        snapshot.task("blocked").unwrap().state,
        WorkflowTaskState::Blocked
    );
}

#[test]
fn completes_after_all_tasks_succeed() {
    let mut snapshot = running_snapshot(vec![task("done", &[])]);
    snapshot
        .transition_task(&WorkflowTaskId::from("done"), WorkflowTaskState::Ready)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("done"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("done"), WorkflowTaskState::Succeeded)
        .unwrap();

    assert_eq!(
        WorkflowScheduler::new(/*max_concurrent_tasks*/ 1)
            .unwrap()
            .next_actions(&mut snapshot),
        vec![
            WorkflowAction::Complete(WorkflowState::Completed),
            WorkflowAction::Persist
        ]
    );
}

#[test]
fn pausing_interrupts_active_tasks() {
    let mut snapshot = running_snapshot(vec![task("active", &[])]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();
    scheduler.next_actions(&mut snapshot);
    snapshot
        .transition_task(&WorkflowTaskId::from("active"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .transition_workflow(WorkflowState::Pausing)
        .unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Interrupt(WorkflowTaskId::from("active")),
            WorkflowAction::Persist,
        ]
    );
    assert_eq!(
        snapshot.task("active").unwrap().state,
        WorkflowTaskState::Running
    );
}

#[test]
fn cancellation_interrupts_active_tasks() {
    let mut snapshot = running_snapshot(vec![task("active", &[])]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();
    scheduler.next_actions(&mut snapshot);
    snapshot
        .transition_task(&WorkflowTaskId::from("active"), WorkflowTaskState::Running)
        .unwrap();
    snapshot
        .transition_workflow(WorkflowState::Cancelled)
        .unwrap();

    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Interrupt(WorkflowTaskId::from("active")),
            WorkflowAction::Persist,
        ]
    );
    assert_eq!(
        snapshot.task("active").unwrap().state,
        WorkflowTaskState::Running
    );
}

#[test]
fn cancellation_completes_when_no_agents_are_active() {
    let mut snapshot = running_snapshot(vec![task("queued", &[])]);
    snapshot
        .transition_workflow(WorkflowState::Cancelled)
        .unwrap();
    snapshot
        .transition_task(
            &WorkflowTaskId::from("queued"),
            WorkflowTaskState::Cancelled,
        )
        .unwrap();

    assert_eq!(
        WorkflowScheduler::new(/*max_concurrent_tasks*/ 1)
            .unwrap()
            .next_actions(&mut snapshot),
        vec![
            WorkflowAction::Complete(WorkflowState::Cancelled),
            WorkflowAction::Persist,
        ]
    );
}

#[test]
fn deferring_a_ready_task_rolls_back_its_attempt() {
    let mut snapshot = running_snapshot(vec![task("deferred", &[])]);
    let scheduler = WorkflowScheduler::new(/*max_concurrent_tasks*/ 1).unwrap();
    assert_eq!(
        scheduler.next_actions(&mut snapshot),
        vec![
            WorkflowAction::Spawn(WorkflowTaskId::from("deferred")),
            WorkflowAction::Persist,
        ]
    );
    snapshot
        .defer_ready_task(&WorkflowTaskId::from("deferred"))
        .unwrap();

    assert_eq!(
        snapshot.task("deferred").unwrap().state,
        WorkflowTaskState::Queued
    );
    assert_eq!(snapshot.task("deferred").unwrap().attempts, 0);
}

fn running_snapshot(tasks: Vec<WorkflowTask>) -> WorkflowSnapshot {
    let definition = WorkflowDefinition::try_new("test", tasks, WorkflowLimits::default()).unwrap();
    let mut snapshot = WorkflowSnapshot::new(WorkflowId::new(), definition);
    snapshot
        .transition_workflow(WorkflowState::Running)
        .unwrap();
    snapshot
}

fn task(id: &str, dependencies: &[&str]) -> WorkflowTask {
    WorkflowTask {
        id: WorkflowTaskId::from(id),
        title: id.to_string(),
        instructions: format!("Execute {id}"),
        dependencies: dependencies
            .iter()
            .copied()
            .map(WorkflowTaskId::from)
            .collect(),
        agent_role: None,
        intent: WorkflowTaskIntent::ReadOnly,
        owned_paths: Vec::new(),
        verification: Vec::new(),
        max_attempts: 2,
    }
}
