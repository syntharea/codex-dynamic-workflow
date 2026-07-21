use pretty_assertions::assert_eq;

use crate::WORKFLOW_SCHEMA_VERSION;
use crate::WorkflowDefinition;
use crate::WorkflowLimits;
use crate::WorkflowState;
use crate::WorkflowTask;
use crate::WorkflowTaskId;
use crate::WorkflowTaskState;
use crate::WorkflowTransitionError;
use crate::WorkflowValidationError;

#[test]
fn workflow_definition_rejects_unknown_fields() {
    let value = serde_json::json!({
        "schema_version": 1,
        "title": "strict",
        "objective": "strict",
        "tasks": [],
        "unexpected": true,
    });

    assert!(serde_json::from_value::<WorkflowDefinition>(value).is_err());
}

#[test]
fn workflow_definition_rejects_unknown_schema_versions() {
    let value = serde_json::json!({
        "schema_version": WORKFLOW_SCHEMA_VERSION + 1,
        "title": "strict",
        "objective": "strict",
        "tasks": [],
    });

    assert!(serde_json::from_value::<WorkflowDefinition>(value).is_err());
}

#[test]
fn workflow_task_rejects_unknown_fields() {
    let value = serde_json::json!({
        "schema_version": WORKFLOW_SCHEMA_VERSION,
        "title": "strict",
        "objective": "strict",
        "tasks": [{
            "id": "task",
            "title": "Task",
            "instructions": "Run",
            "dependencies": [],
            "agent_role": null,
            "intent": "read_only",
            "owned_paths": [],
            "verification": [],
            "max_attempts": 1,
            "unexpected": true,
        }],
    });

    assert!(serde_json::from_value::<WorkflowDefinition>(value).is_err());
}

#[test]
fn rejects_duplicate_task_ids() {
    let error = WorkflowDefinition::try_new(
        "duplicate",
        vec![task("same", &[]), task("same", &[])],
        WorkflowLimits::default(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::DuplicateTaskId(WorkflowTaskId::from("same"))
    );
}

#[test]
fn rejects_missing_dependencies() {
    let error = WorkflowDefinition::try_new(
        "missing",
        vec![task("dependent", &["missing"])],
        WorkflowLimits::default(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::MissingDependency {
            task_id: WorkflowTaskId::from("dependent"),
            dependency: WorkflowTaskId::from("missing"),
        }
    );
}

#[test]
fn rejects_a_dependency_cycle() {
    let error = WorkflowDefinition::try_new(
        "cycle",
        vec![task("a", &["b"]), task("b", &["a"])],
        WorkflowLimits::default(),
    )
    .unwrap_err();

    assert_eq!(error, WorkflowValidationError::DependencyCycle);
}

#[test]
fn rejects_task_count_over_limit() {
    let limits = WorkflowLimits {
        max_tasks: 1,
        ..WorkflowLimits::default()
    };
    let error = WorkflowDefinition::try_new("large", vec![task("a", &[]), task("b", &[])], limits)
        .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::TooManyTasks { max_tasks: 1 }
    );
}

#[test]
fn enforces_structural_task_cap_above_configured_limits() {
    let limits = WorkflowLimits {
        max_tasks: usize::MAX,
        ..WorkflowLimits::default()
    };
    let definition =
        WorkflowDefinition::try_new("at-limit", numbered_tasks(/*count*/ 256), limits).unwrap();

    assert_eq!(definition.tasks.len(), 256);
    assert_eq!(
        WorkflowDefinition::try_new("over-limit", numbered_tasks(/*count*/ 257), limits)
            .unwrap_err(),
        WorkflowValidationError::TooManyTasks { max_tasks: 256 }
    );
}

#[test]
fn rejects_instructions_over_limit() {
    let limits = WorkflowLimits {
        max_instruction_bytes: 3,
        ..WorkflowLimits::default()
    };
    let error = WorkflowDefinition::try_new(
        "large instructions",
        vec![WorkflowTask {
            instructions: "four".to_string(),
            ..task("a", &[])
        }],
        limits,
    )
    .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::InstructionsTooLong {
            task_id: WorkflowTaskId::from("a"),
            max_bytes: 3,
        }
    );
}

#[test]
fn rejects_zero_attempt_limit() {
    let error = WorkflowDefinition::try_new(
        "attempts",
        vec![WorkflowTask {
            max_attempts: 0,
            ..task("a", &[])
        }],
        WorkflowLimits::default(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::InvalidMaxAttempts(WorkflowTaskId::from("a"))
    );
}

#[test]
fn rejects_task_attempts_over_configured_limit() {
    let limits = WorkflowLimits {
        max_task_attempts: 2,
        ..WorkflowLimits::default()
    };
    let error = WorkflowDefinition::try_new(
        "attempts",
        vec![WorkflowTask {
            max_attempts: 255,
            ..task("a", &[])
        }],
        limits,
    )
    .unwrap_err();

    assert_eq!(
        error,
        WorkflowValidationError::TooManyTaskAttempts {
            task_id: WorkflowTaskId::from("a"),
            max_attempts: 2,
        }
    );
}

#[test]
fn configured_task_attempt_override_is_honored() {
    let limits = WorkflowLimits {
        max_task_attempts: 255,
        ..WorkflowLimits::default()
    };
    let definition = WorkflowDefinition::try_new(
        "attempts",
        vec![WorkflowTask {
            max_attempts: 255,
            ..task("a", &[])
        }],
        limits,
    )
    .unwrap();

    assert_eq!(definition.tasks[0].max_attempts, 255);
}

#[test]
fn rejects_protocol_visible_strings_over_wire_limits() {
    let cases = [
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "t".repeat(257),
            objective: "objective".to_string(),
            tasks: vec![],
        },
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "title".to_string(),
            objective: "o".repeat(16 * 1024 + 1),
            tasks: vec![],
        },
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "title".to_string(),
            objective: "objective".to_string(),
            tasks: vec![WorkflowTask {
                title: "t".repeat(257),
                ..task("a", &[])
            }],
        },
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "title".to_string(),
            objective: "objective".to_string(),
            tasks: vec![WorkflowTask {
                agent_role: Some("r".repeat(257)),
                ..task("a", &[])
            }],
        },
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "title".to_string(),
            objective: "objective".to_string(),
            tasks: vec![WorkflowTask {
                owned_paths: vec!["p".repeat(4 * 1024 + 1)],
                ..task("a", &[])
            }],
        },
        WorkflowDefinition {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            title: "title".to_string(),
            objective: "objective".to_string(),
            tasks: vec![WorkflowTask {
                verification: vec!["v".repeat(4 * 1024 + 1)],
                ..task("a", &[])
            }],
        },
    ];

    for definition in cases {
        assert!(definition.validate(WorkflowLimits::default()).is_err());
    }
}

#[test]
fn returns_a_stable_topological_order() {
    let definition = WorkflowDefinition::try_new(
        "order",
        vec![
            task("root-b", &[]),
            task("root-a", &[]),
            task("join", &["root-a", "root-b"]),
        ],
        WorkflowLimits::default(),
    )
    .unwrap();

    assert_eq!(
        definition.topological_order(),
        vec![
            WorkflowTaskId::from("root-b"),
            WorkflowTaskId::from("root-a"),
            WorkflowTaskId::from("join"),
        ]
    );
}

#[test]
fn validates_workflow_state_transitions() {
    assert_eq!(
        WorkflowState::Planning.transition_to(WorkflowState::Running),
        Ok(WorkflowState::Running)
    );
    assert_eq!(
        WorkflowState::Running.transition_to(WorkflowState::Pausing),
        Ok(WorkflowState::Pausing)
    );
    assert_eq!(
        WorkflowState::Pausing.transition_to(WorkflowState::Paused),
        Ok(WorkflowState::Paused)
    );
    assert_eq!(
        WorkflowState::Paused.transition_to(WorkflowState::Running),
        Ok(WorkflowState::Running)
    );
    assert_eq!(
        WorkflowState::Running.transition_to(WorkflowState::Consolidating),
        Ok(WorkflowState::Consolidating)
    );
    assert_eq!(
        WorkflowState::Consolidating.transition_to(WorkflowState::Completed),
        Ok(WorkflowState::Completed)
    );
    assert_eq!(
        WorkflowState::Completed.transition_to(WorkflowState::Running),
        Err(WorkflowTransitionError::InvalidWorkflowTransition {
            from: WorkflowState::Completed,
            to: WorkflowState::Running,
        })
    );
    assert_eq!(
        WorkflowState::Failed.transition_to(WorkflowState::Planning),
        Err(WorkflowTransitionError::InvalidWorkflowTransition {
            from: WorkflowState::Failed,
            to: WorkflowState::Planning,
        })
    );
}

#[test]
fn workflow_transition_matrix_is_exhaustive() {
    let states = [
        WorkflowState::Planning,
        WorkflowState::Running,
        WorkflowState::Pausing,
        WorkflowState::Paused,
        WorkflowState::Consolidating,
        WorkflowState::Completed,
        WorkflowState::Failed,
        WorkflowState::Cancelled,
    ];

    for from in states {
        for to in states {
            let expected = matches!(
                (from, to),
                (
                    WorkflowState::Planning,
                    WorkflowState::Running | WorkflowState::Failed | WorkflowState::Cancelled
                ) | (
                    WorkflowState::Running,
                    WorkflowState::Pausing
                        | WorkflowState::Consolidating
                        | WorkflowState::Completed
                        | WorkflowState::Failed
                        | WorkflowState::Cancelled
                ) | (
                    WorkflowState::Pausing,
                    WorkflowState::Paused | WorkflowState::Failed | WorkflowState::Cancelled
                ) | (
                    WorkflowState::Paused,
                    WorkflowState::Running | WorkflowState::Failed | WorkflowState::Cancelled
                ) | (
                    WorkflowState::Consolidating,
                    WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
                )
            );
            assert_eq!(
                from.transition_to(to).is_ok(),
                expected,
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn validates_task_state_transitions() {
    assert_eq!(
        WorkflowTaskState::Queued.transition_to(WorkflowTaskState::Ready),
        Ok(WorkflowTaskState::Ready)
    );
    assert_eq!(
        WorkflowTaskState::Ready.transition_to(WorkflowTaskState::Running),
        Ok(WorkflowTaskState::Running)
    );
    assert_eq!(
        WorkflowTaskState::Running.transition_to(WorkflowTaskState::WaitingForApproval),
        Ok(WorkflowTaskState::WaitingForApproval)
    );
    assert_eq!(
        WorkflowTaskState::WaitingForApproval.transition_to(WorkflowTaskState::Running),
        Ok(WorkflowTaskState::Running)
    );
    assert_eq!(
        WorkflowTaskState::Running.transition_to(WorkflowTaskState::Succeeded),
        Ok(WorkflowTaskState::Succeeded)
    );
    assert_eq!(
        WorkflowTaskState::Succeeded.transition_to(WorkflowTaskState::Queued),
        Err(WorkflowTransitionError::InvalidTaskTransition {
            from: WorkflowTaskState::Succeeded,
            to: WorkflowTaskState::Queued,
        })
    );
}

#[test]
fn task_transition_matrix_is_exhaustive() {
    let states = [
        WorkflowTaskState::Queued,
        WorkflowTaskState::Ready,
        WorkflowTaskState::Running,
        WorkflowTaskState::WaitingForApproval,
        WorkflowTaskState::Succeeded,
        WorkflowTaskState::Failed,
        WorkflowTaskState::Blocked,
        WorkflowTaskState::Cancelled,
    ];

    for from in states {
        for to in states {
            let expected = matches!(
                (from, to),
                (
                    WorkflowTaskState::Queued,
                    WorkflowTaskState::Ready
                        | WorkflowTaskState::Blocked
                        | WorkflowTaskState::Cancelled
                ) | (
                    WorkflowTaskState::Ready,
                    WorkflowTaskState::Running
                        | WorkflowTaskState::Queued
                        | WorkflowTaskState::Failed
                        | WorkflowTaskState::Cancelled
                ) | (
                    WorkflowTaskState::Running,
                    WorkflowTaskState::WaitingForApproval
                        | WorkflowTaskState::Succeeded
                        | WorkflowTaskState::Failed
                        | WorkflowTaskState::Queued
                        | WorkflowTaskState::Cancelled
                ) | (
                    WorkflowTaskState::WaitingForApproval,
                    WorkflowTaskState::Running
                        | WorkflowTaskState::Failed
                        | WorkflowTaskState::Queued
                        | WorkflowTaskState::Cancelled
                ) | (WorkflowTaskState::Failed, WorkflowTaskState::Queued)
            );
            assert_eq!(
                from.transition_to(to).is_ok(),
                expected,
                "{from:?} -> {to:?}"
            );
        }
    }
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
        intent: crate::WorkflowTaskIntent::ReadOnly,
        owned_paths: Vec::new(),
        verification: Vec::new(),
        max_attempts: 2,
    }
}

fn numbered_tasks(count: usize) -> Vec<WorkflowTask> {
    (0..count)
        .map(|index| task(&format!("task-{index}"), &[]))
        .collect()
}
