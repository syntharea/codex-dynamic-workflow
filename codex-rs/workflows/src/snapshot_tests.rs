use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use crate::SavedWorkflowDefinition;
use crate::WORKFLOW_SCHEMA_VERSION;
use crate::WorkflowAgentId;
use crate::WorkflowAgentOutcome;
use crate::WorkflowAgentOutcomeStatus;
use crate::WorkflowDefinition;
use crate::WorkflowId;
use crate::WorkflowLimits;
use crate::WorkflowSnapshot;
use crate::WorkflowState;
use crate::WorkflowTask;
use crate::WorkflowTaskId;
use crate::WorkflowTaskIntent;
use crate::WorkflowTaskState;

#[test]
fn snapshot_round_trips_through_json() {
    let mut snapshot = snapshot();
    snapshot.accept_definition();
    snapshot.resume_after_pause = true;
    let json = serde_json::to_string(&snapshot).unwrap();

    assert_eq!(
        serde_json::from_str::<WorkflowSnapshot>(&json).unwrap(),
        snapshot
    );
}

#[test]
fn snapshots_without_definition_state_restore_as_placeholders() {
    let mut snapshot = snapshot();
    snapshot.accept_definition();
    let mut value = serde_json::to_value(snapshot).unwrap();
    value.as_object_mut().unwrap().remove("definition_state");

    let restored: WorkflowSnapshot = serde_json::from_value(value).unwrap();

    assert!(!restored.is_definition_accepted());
}

#[test]
fn terminal_planning_placeholders_remain_unaccepted() {
    for state in [WorkflowState::Failed, WorkflowState::Cancelled] {
        let mut snapshot = snapshot();
        snapshot.transition_workflow(state).unwrap();
        let restored: WorkflowSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();

        assert!(!restored.is_definition_accepted(), "{state:?}");
    }
}

#[test]
fn snapshots_without_resume_intent_default_to_false() {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value.as_object_mut().unwrap().remove("resume_after_pause");

    let restored: WorkflowSnapshot = serde_json::from_value(value).unwrap();

    assert!(!restored.resume_after_pause);
}

#[test]
fn persistence_failure_can_fail_an_unpersisted_completed_snapshot() {
    let mut snapshot = running_snapshot();
    snapshot
        .record_task_success(
            &WorkflowTaskId::from("task"),
            "done",
            /*max_summary_bytes*/ 128,
        )
        .unwrap();
    snapshot
        .transition_workflow(WorkflowState::Completed)
        .unwrap();
    snapshot.resume_after_pause = true;

    snapshot.mark_persistence_failed();

    assert_eq!(snapshot.state, WorkflowState::Failed);
    assert!(!snapshot.resume_after_pause);
}

#[test]
fn ignores_unknown_optional_snapshot_fields() {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value["futureField"] = json!({ "enabled": true });
    value["tasks"][0]["futureTaskField"] = json!("future");

    let restored: WorkflowSnapshot = serde_json::from_value(value).unwrap();

    assert_eq!(restored.schema_version, WORKFLOW_SCHEMA_VERSION);
    assert_eq!(restored.tasks.len(), 1);
}

#[test]
fn bounds_utf8_task_summaries_without_splitting_characters() {
    let mut snapshot = running_snapshot();
    snapshot
        .record_task_success(
            &WorkflowTaskId::from("task"),
            "ééé",
            /*max_summary_bytes*/ 5,
        )
        .unwrap();

    assert_eq!(
        snapshot.task("task").unwrap().summary.as_deref(),
        Some("éé")
    );
}

#[test]
fn recovery_requeues_running_tasks_without_terminal_outcomes() {
    let mut snapshot = running_snapshot();

    snapshot.recover(&[], /*max_summary_bytes*/ 128).unwrap();

    assert_eq!(
        snapshot.task("task").unwrap().state,
        WorkflowTaskState::Queued
    );
    assert_eq!(snapshot.task("task").unwrap().agent_id, None);
}

#[test]
fn recovery_requeues_ready_tasks_without_terminal_outcomes() {
    let mut snapshot = snapshot();
    snapshot
        .transition_workflow(WorkflowState::Running)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("task"), WorkflowTaskState::Ready)
        .unwrap();
    snapshot.tasks[0].attempts = 1;
    snapshot.tasks[0].agent_id = Some(WorkflowAgentId::from("agent-1"));

    snapshot.recover(&[], /*max_summary_bytes*/ 128).unwrap();

    assert_eq!(
        snapshot.task("task").unwrap().state,
        WorkflowTaskState::Queued
    );
    assert_eq!(snapshot.task("task").unwrap().agent_id, None);
}

#[test]
fn recovery_applies_confirmed_terminal_agent_outcomes() {
    let mut snapshot = running_snapshot();
    let outcome = WorkflowAgentOutcome {
        agent_id: WorkflowAgentId::from("agent-1"),
        task_id: WorkflowTaskId::from("task"),
        status: WorkflowAgentOutcomeStatus::Succeeded {
            summary: "done".to_string(),
        },
    };

    snapshot
        .recover(&[outcome], /*max_summary_bytes*/ 128)
        .unwrap();

    assert_eq!(
        snapshot.task("task").unwrap().state,
        WorkflowTaskState::Succeeded
    );
    assert_eq!(
        snapshot.task("task").unwrap().summary.as_deref(),
        Some("done")
    );
}

#[test]
fn saved_declarations_round_trip_and_create_fresh_runs() {
    let saved = SavedWorkflowDefinition::new("auth-review", definition());
    let json = serde_json::to_string(&saved).unwrap();
    let restored: SavedWorkflowDefinition = serde_json::from_str(&json).unwrap();

    assert_eq!(restored, saved);
    assert_eq!(restored.schema_version, WORKFLOW_SCHEMA_VERSION);
    let first = restored.instantiate();
    let second = restored.instantiate();
    assert_ne!(first.workflow_id, second.workflow_id);
    assert!(first.is_definition_accepted());
    assert!(second.is_definition_accepted());
    assert_eq!(first.definition, restored.definition);
    assert_eq!(second.definition, restored.definition);
}

#[test]
fn workflow_definition_deserialization_enforces_structural_task_cap() {
    let definition: WorkflowDefinition =
        serde_json::from_value(definition_value(/*task_count*/ 256)).expect("256 task definition");

    assert_eq!(definition.tasks.len(), 256);
    assert!(
        serde_json::from_value::<WorkflowDefinition>(definition_value(/*task_count*/ 257)).is_err()
    );
}

#[test]
fn workflow_snapshot_deserialization_enforces_structural_task_cap() {
    let snapshot: WorkflowSnapshot =
        serde_json::from_value(snapshot_value(/*task_count*/ 256)).expect("256 task snapshot");

    assert_eq!(snapshot.tasks.len(), 256);
    assert!(
        serde_json::from_value::<WorkflowSnapshot>(snapshot_value(/*task_count*/ 257)).is_err()
    );
}

#[test]
fn workflow_snapshot_deserialization_revalidates_definition() {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value["definition"]["tasks"][0]["max_attempts"] = json!(0);

    assert!(serde_json::from_value::<WorkflowSnapshot>(value).is_err());
}

#[test]
fn workflow_snapshot_rejects_unknown_schema_versions() {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value["schema_version"] = json!(WORKFLOW_SCHEMA_VERSION + 1);

    assert!(serde_json::from_value::<WorkflowSnapshot>(value).is_err());
}

#[test]
fn failed_workflows_retry_through_explicit_attempt_history() {
    let mut snapshot = snapshot();
    snapshot.transition_workflow(WorkflowState::Failed).unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("task"), WorkflowTaskState::Ready)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("task"), WorkflowTaskState::Failed)
        .unwrap();
    snapshot.tasks[0].attempts = 1;
    snapshot.tasks[0].summary = Some("failed".to_string());
    let previous_tasks = snapshot.tasks.clone();

    snapshot.retry_failed_workflow().unwrap();

    assert_eq!(snapshot.state, WorkflowState::Planning);
    assert_eq!(snapshot.execution_attempt, 2);
    assert_eq!(snapshot.attempt_history.len(), 1);
    assert_eq!(snapshot.attempt_history[0].execution_attempt, 1);
    assert_eq!(snapshot.attempt_history[0].state, WorkflowState::Failed);
    assert_eq!(snapshot.attempt_history[0].tasks, previous_tasks);
    assert_eq!(
        snapshot.task("task").unwrap().state,
        WorkflowTaskState::Queued
    );
    assert_eq!(snapshot.task("task").unwrap().attempts, 0);
}

fn snapshot() -> WorkflowSnapshot {
    WorkflowSnapshot::new(WorkflowId::new(), definition())
}

fn running_snapshot() -> WorkflowSnapshot {
    let mut snapshot = snapshot();
    snapshot
        .transition_workflow(WorkflowState::Running)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("task"), WorkflowTaskState::Ready)
        .unwrap();
    snapshot
        .transition_task(&WorkflowTaskId::from("task"), WorkflowTaskState::Running)
        .unwrap();
    snapshot.tasks[0].attempts = 1;
    snapshot.tasks[0].agent_id = Some(WorkflowAgentId::from("agent-1"));
    snapshot
}

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::try_new_with_objective(
        "review",
        "Review authentication",
        vec![WorkflowTask {
            id: WorkflowTaskId::from("task"),
            title: "Review".to_string(),
            instructions: "Review authentication".to_string(),
            dependencies: Vec::new(),
            agent_role: Some("explorer".to_string()),
            intent: WorkflowTaskIntent::ReadOnly,
            owned_paths: Vec::new(),
            verification: vec!["Report evidence".to_string()],
            max_attempts: 2,
        }],
        WorkflowLimits::default(),
    )
    .unwrap()
}

fn definition_value(task_count: usize) -> Value {
    let tasks = (0..task_count).map(task_value).collect::<Vec<_>>();
    json!({
        "schema_version": WORKFLOW_SCHEMA_VERSION,
        "title": "structural limit",
        "objective": "structural limit",
        "tasks": tasks,
    })
}

fn snapshot_value(task_count: usize) -> Value {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value["definition"] = definition_value(task_count);
    value["tasks"] = Value::Array((0..task_count).map(task_snapshot_value).collect());
    value
}

fn task_value(index: usize) -> Value {
    json!({
        "id": format!("task-{index}"),
        "title": "Task",
        "instructions": "Run",
        "dependencies": [],
        "agent_role": null,
        "intent": "read_only",
        "owned_paths": [],
        "verification": [],
        "max_attempts": 1,
    })
}

fn task_snapshot_value(index: usize) -> Value {
    json!({
        "task_id": format!("task-{index}"),
        "state": "queued",
        "attempts": 0,
        "agent_id": null,
        "summary": null,
    })
}
