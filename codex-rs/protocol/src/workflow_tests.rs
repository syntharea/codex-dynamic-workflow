use crate::protocol::EventMsg;
use crate::protocol::Op;
use crate::protocol::RolloutItem;
use crate::protocol::WorkflowCancelParams;
use crate::protocol::WorkflowCompletedEvent;
use crate::protocol::WorkflowDeleteSavedParams;
use crate::protocol::WorkflowFailedEvent;
use crate::protocol::WorkflowListParams;
use crate::protocol::WorkflowPauseParams;
use crate::protocol::WorkflowPolicy;
use crate::protocol::WorkflowProgressEvent;
use crate::protocol::WorkflowReadParams;
use crate::protocol::WorkflowResumeParams;
use crate::protocol::WorkflowRetryParams;
use crate::protocol::WorkflowRunSavedParams;
use crate::protocol::WorkflowSaveParams;
use crate::protocol::WorkflowStartParams;
use crate::protocol::WorkflowStartedEvent;
use crate::protocol::WorkflowStateItem;
use crate::protocol::WorkflowStatus;
use crate::protocol::WorkflowSummary;
use crate::protocol::WorkflowTaskOverride;
use crate::protocol::WorkflowTaskStatus;
use crate::protocol::WorkflowTaskSummary;
use crate::protocol::WorkflowText;
use crate::protocol::WorkflowTitle;
use crate::workflow::WORKFLOW_ID_MAX_BYTES;
use crate::workflow::WORKFLOW_MAX_TASKS;
use crate::workflow::WORKFLOW_NAME_MAX_BYTES;
use crate::workflow::WORKFLOW_OBJECTIVE_MAX_BYTES;
use crate::workflow::WorkflowId;
use crate::workflow::WorkflowName;
use crate::workflow::WorkflowObjective;
use crate::workflow::WorkflowTaskOverrides;
use serde_json::json;

#[test]
fn workflow_state_rollout_item_round_trips() {
    let item = RolloutItem::WorkflowState(
        WorkflowStateItem::try_new(
            /*schema_version*/ 1,
            "wf_123",
            json!({"state": "running"}),
        )
        .unwrap(),
    );

    let encoded = serde_json::to_value(&item).unwrap();
    let decoded: RolloutItem = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
}

#[test]
fn old_rollout_item_and_unknown_workflow_fields_decode() {
    let old = json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": []}});
    serde_json::from_value::<RolloutItem>(old).expect("old rollout item should decode");

    let event = json!({
        "type": "workflow_started",
        "workflow_id": "wf_123",
        "summary": {
            "workflow_id": "wf_123",
            "title": "demo",
            "status": "running",
            "completed_tasks": 0,
            "running_tasks": 1,
            "queued_tasks": 0,
            "blocked_tasks": 0,
            "failed_tasks": 0,
            "future_optional_field": true
        },
        "future_optional_event_field": "ignored"
    });

    let decoded: EventMsg = serde_json::from_value(event).expect("unknown fields are optional");
    assert!(matches!(decoded, EventMsg::WorkflowStarted(_)));
}

#[test]
fn bounded_workflow_text_rejects_oversized_values() {
    let oversized = "x".repeat(crate::workflow::WORKFLOW_TEXT_MAX_BYTES + 1);
    assert!(WorkflowText::try_new(oversized.clone()).is_err());
    assert!(serde_json::from_value::<WorkflowText>(json!(oversized)).is_err());
    assert!(
        WorkflowTitle::try_new("x".repeat(crate::workflow::WORKFLOW_TITLE_MAX_BYTES + 1)).is_err()
    );
}

#[test]
fn workflow_summary_rejects_oversized_task_lists() {
    let tasks = (0..=crate::workflow::WORKFLOW_MAX_TASKS)
        .map(|index| {
            json!({
                "task_id": index.to_string(),
                "title": "task",
                "status": "queued",
                "summary": "",
                "attempt": 0
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "workflow_id": "wf_123",
        "title": "demo",
        "status": "running",
        "tasks": tasks,
        "completed_tasks": 0,
        "running_tasks": 0,
        "queued_tasks": 0,
        "blocked_tasks": 0,
        "failed_tasks": 0,
        "summary": ""
    });

    assert!(serde_json::from_value::<WorkflowSummary>(value).is_err());
}

#[test]
fn workflow_state_rejects_unknown_schema_and_large_snapshots_but_accepts_unknown_inner_fields() {
    let unknown_schema = json!({
        "schema_version": 2,
        "workflow_id": "wf_123",
        "snapshot": {"state": "running"}
    });
    assert!(serde_json::from_value::<WorkflowStateItem>(unknown_schema).is_err());

    let unknown_inner = json!({
        "schema_version": 1,
        "workflow_id": "wf_123",
        "snapshot": {"state": "running", "future_field": {"nested": true}}
    });
    assert!(serde_json::from_value::<WorkflowStateItem>(unknown_inner).is_ok());

    let large_snapshot = json!({
        "schema_version": 1,
        "workflow_id": "wf_123",
        "snapshot": "x".repeat(crate::workflow::WORKFLOW_SNAPSHOT_MAX_BYTES + 1)
    });
    assert!(serde_json::from_value::<WorkflowStateItem>(large_snapshot).is_err());
}

#[test]
fn workflow_operations_cover_the_full_lifecycle() {
    let workflow_id = WorkflowId::try_new("wf_123").unwrap();
    let operations = [
        Op::WorkflowStart(WorkflowStartParams {
            objective: WorkflowObjective::try_new("ship it").unwrap(),
            policy: Some(WorkflowPolicy::Ultracode),
        }),
        Op::WorkflowRead(WorkflowReadParams {
            workflow_id: workflow_id.clone(),
        }),
        Op::WorkflowList(WorkflowListParams),
        Op::WorkflowPause(WorkflowPauseParams {
            workflow_id: workflow_id.clone(),
        }),
        Op::WorkflowResume(WorkflowResumeParams {
            workflow_id: workflow_id.clone(),
        }),
        Op::WorkflowCancel(WorkflowCancelParams {
            workflow_id: workflow_id.clone(),
        }),
        Op::WorkflowRetry(WorkflowRetryParams {
            workflow_id: workflow_id.clone(),
        }),
        Op::WorkflowSave(WorkflowSaveParams {
            workflow_id,
            name: WorkflowName::try_new("saved").unwrap(),
        }),
        Op::WorkflowRunSaved(WorkflowRunSavedParams {
            name: WorkflowName::try_new("saved").unwrap(),
            task_overrides: WorkflowTaskOverrides::try_new(vec![
                WorkflowTaskOverride::try_new(
                    "task-1",
                    Some(WorkflowTitle::try_new("override").unwrap()),
                    Some(WorkflowText::try_new("do this").unwrap()),
                )
                .unwrap(),
            ])
            .unwrap(),
        }),
        Op::WorkflowDeleteSaved(WorkflowDeleteSavedParams {
            name: WorkflowName::try_new("saved").unwrap(),
        }),
    ];

    assert_eq!(operations.len(), 10);
}

#[test]
fn workflow_start_policy_rejects_saved_definitions() {
    assert!(
        serde_json::from_value::<WorkflowPolicy>(json!({
            "type": "saved",
            "name": "release",
        }))
        .is_err(),
        "saved definitions must run through workflow_run_saved"
    );
}

#[test]
fn lifecycle_events_are_wire_serializable() {
    let workflow_id = WorkflowId::try_new("wf_123").unwrap();
    let summary = WorkflowSummary::try_new(
        workflow_id.clone(),
        WorkflowTitle::try_new("demo").unwrap(),
        WorkflowStatus::Running,
        Vec::new(),
        /*completed_tasks*/ 1,
        /*running_tasks*/ 1,
        /*queued_tasks*/ 0,
        /*blocked_tasks*/ 0,
        /*failed_tasks*/ 0,
        WorkflowText::try_new("working").unwrap(),
    )
    .unwrap();
    let events = [
        EventMsg::WorkflowStarted(
            WorkflowStartedEvent::try_new(workflow_id.clone(), summary.clone()).unwrap(),
        ),
        EventMsg::WorkflowProgress(WorkflowProgressEvent::new(
            workflow_id.clone(),
            WorkflowStatus::Running,
            /*completed_tasks*/ 1,
            /*running_tasks*/ 1,
            /*queued_tasks*/ 0,
            /*blocked_tasks*/ 0,
            /*failed_tasks*/ 0,
            WorkflowText::try_new("working").unwrap(),
        )),
        EventMsg::WorkflowPaused(
            crate::protocol::WorkflowPausedEvent::try_new(workflow_id.clone(), summary.clone())
                .unwrap(),
        ),
        EventMsg::WorkflowCompleted(
            WorkflowCompletedEvent::try_new(workflow_id.clone(), summary.clone()).unwrap(),
        ),
        EventMsg::WorkflowFailed(
            WorkflowFailedEvent::try_new(
                workflow_id.clone(),
                summary.clone(),
                WorkflowText::try_new("boom").unwrap(),
            )
            .unwrap(),
        ),
        EventMsg::WorkflowCancelled(
            crate::protocol::WorkflowCancelledEvent::try_new(workflow_id, summary).unwrap(),
        ),
    ];

    for event in events {
        serde_json::to_value(event).expect("workflow event should be serializable");
    }
}

#[test]
fn workflow_state_is_not_model_visible() {
    let item = RolloutItem::WorkflowState(
        WorkflowStateItem::try_new(
            /*schema_version*/ 1,
            "wf_123",
            json!({"state": "running"}),
        )
        .unwrap(),
    );

    assert!(!item.is_model_visible());
}

#[test]
fn validated_workflow_state_exposes_only_read_only_accessors() {
    let item = WorkflowStateItem::try_new(
        /*schema_version*/ 1,
        "wf_123",
        json!({"state": "running", "future_field": true}),
    )
    .unwrap();

    assert_eq!(item.schema_version(), 1);
    assert_eq!(item.workflow_id(), "wf_123");
    assert_eq!(
        item.snapshot(),
        &json!({"state": "running", "future_field": true})
    );
    serde_json::to_value(item).expect("validated workflow state should serialize");
}

#[test]
fn workflow_summary_constructor_validates_task_count_before_serialization() {
    let task = WorkflowTaskSummary::try_new(
        "task-1",
        WorkflowTitle::try_new("task").unwrap(),
        WorkflowTaskStatus::Queued,
        WorkflowText::default(),
        /*attempt*/ 0,
    )
    .unwrap();
    let summary = WorkflowSummary::try_new(
        WorkflowId::try_new("wf_123").unwrap(),
        WorkflowTitle::try_new("demo").unwrap(),
        WorkflowStatus::Running,
        vec![task],
        /*completed_tasks*/ 0,
        /*running_tasks*/ 0,
        /*queued_tasks*/ 1,
        /*blocked_tasks*/ 0,
        /*failed_tasks*/ 0,
        WorkflowText::default(),
    )
    .unwrap();

    assert_eq!(summary.workflow_id(), "wf_123");
    assert_eq!(summary.tasks().len(), 1);
    serde_json::to_value(summary).expect("validated workflow summary should serialize");
}

#[test]
fn workflow_task_overrides_and_user_strings_are_bounded() {
    assert!(WorkflowObjective::try_new("x".repeat(WORKFLOW_OBJECTIVE_MAX_BYTES + 1)).is_err());
    assert!(WorkflowName::try_new("x".repeat(WORKFLOW_NAME_MAX_BYTES + 1)).is_err());

    let override_item = WorkflowTaskOverride::try_new(
        "task-1",
        Some(WorkflowTitle::try_new("title").unwrap()),
        Some(WorkflowText::try_new("instructions").unwrap()),
    )
    .unwrap();
    let overrides = WorkflowTaskOverrides::try_new(vec![override_item]).unwrap();
    assert_eq!(overrides.len(), 1);
    assert!(
        WorkflowTaskOverrides::try_new(
            (0..=WORKFLOW_MAX_TASKS)
                .map(|index| {
                    WorkflowTaskOverride::try_new(
                        index.to_string(),
                        /*title*/ None,
                        /*instructions*/ None,
                    )
                    .unwrap()
                })
                .collect(),
        )
        .is_err()
    );
}

#[test]
fn workflow_id_is_bounded_on_all_wire_paths() {
    let oversized = "x".repeat(WORKFLOW_ID_MAX_BYTES + 1);
    assert!(serde_json::from_value::<WorkflowId>(json!(oversized)).is_err());
    assert!(
        serde_json::from_value::<WorkflowReadParams>(json!({
            "workflow_id": oversized
        }))
        .is_err()
    );
}

#[test]
fn lifecycle_event_rejects_mismatched_workflow_id_and_summary() {
    let summary = WorkflowSummary::try_new(
        WorkflowId::try_new("wf_summary").unwrap(),
        WorkflowTitle::try_new("demo").unwrap(),
        WorkflowStatus::Running,
        Vec::new(),
        /*completed_tasks*/ 0,
        /*running_tasks*/ 0,
        /*queued_tasks*/ 0,
        /*blocked_tasks*/ 0,
        /*failed_tasks*/ 0,
        WorkflowText::default(),
    )
    .unwrap();

    assert!(
        WorkflowStartedEvent::try_new(WorkflowId::try_new("wf_other").unwrap(), summary,).is_err()
    );
    assert!(
        serde_json::from_value::<EventMsg>(json!({
            "type": "workflow_started",
            "workflow_id": "wf_other",
            "summary": {
                "workflow_id": "wf_summary",
                "title": "demo",
                "status": "running",
                "completed_tasks": 0,
                "running_tasks": 0,
                "queued_tasks": 0,
                "blocked_tasks": 0,
                "failed_tasks": 0,
                "summary": ""
            }
        }))
        .is_err()
    );
}
