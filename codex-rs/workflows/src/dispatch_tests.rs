use pretty_assertions::assert_eq;

use super::*;

#[test]
fn dispatch_marker_round_trips_with_bounded_identity() {
    let marker = WorkflowDispatchMarker::try_new(
        WorkflowId::new(),
        WorkflowTaskId::from("task"),
        /*execution_attempt*/ 2,
        /*task_attempt*/ 3,
    )
    .expect("dispatch marker")
    .bind_agent(WorkflowAgentId::from("agent"))
    .expect("bound dispatch marker");

    let encoded = marker.encode().expect("encoded dispatch marker");
    let decoded = WorkflowDispatchMarker::decode(&encoded)
        .expect("decoded dispatch marker")
        .expect("recognized dispatch marker");

    assert!(encoded.len() <= WORKFLOW_DISPATCH_MARKER_MAX_BYTES);
    assert_eq!(decoded, marker);
}

#[test]
fn dispatch_marker_rejects_unbounded_or_ambiguous_identity() {
    let workflow_id = WorkflowId::new();
    assert!(matches!(
        WorkflowDispatchMarker::try_new(
            workflow_id,
            WorkflowTaskId::from("x".repeat(257)),
            /*execution_attempt*/ 1,
            /*task_attempt*/ 1,
        ),
        Err(WorkflowDispatchMarkerError::TaskIdTooLong { .. })
    ));
    assert!(matches!(
        WorkflowDispatchMarker::try_new(
            workflow_id,
            WorkflowTaskId::from("task"),
            /*execution_attempt*/ 0,
            /*task_attempt*/ 1,
        ),
        Err(WorkflowDispatchMarkerError::InvalidExecutionAttempt)
    ));
    assert!(matches!(
        WorkflowDispatchMarker::try_new(
            workflow_id,
            WorkflowTaskId::from("task"),
            /*execution_attempt*/ 1,
            /*task_attempt*/ 0,
        ),
        Err(WorkflowDispatchMarkerError::InvalidTaskAttempt)
    ));
    let oversized_agent = format!(
        "{WORKFLOW_DISPATCH_MARKER_PREFIX}{{\"schema_version\":1,\"workflow_id\":\"{workflow_id}\",\"task_id\":\"task\",\"execution_attempt\":1,\"task_attempt\":1,\"agent_id\":\"{}\"}}",
        "x".repeat(257)
    );
    assert!(matches!(
        WorkflowDispatchMarker::decode(&oversized_agent),
        Err(WorkflowDispatchMarkerError::InvalidJson(message))
            if message.contains("workflow dispatch agent id exceeds 256 bytes")
    ));
}

#[test]
fn dispatch_marker_decoder_ignores_other_custom_sources() {
    assert_eq!(WorkflowDispatchMarker::decode("integration:test"), Ok(None));
}

#[test]
fn dispatch_marker_cannot_encode_before_agent_binding() {
    let marker = WorkflowDispatchMarker::try_new(
        WorkflowId::new(),
        WorkflowTaskId::from("task"),
        /*execution_attempt*/ 1,
        /*task_attempt*/ 1,
    )
    .expect("dispatch marker");

    assert_eq!(
        marker.encode(),
        Err(WorkflowDispatchMarkerError::MissingAgentId)
    );
}

#[test]
fn dispatch_marker_rejects_duplicate_agent_binding() {
    let marker = WorkflowDispatchMarker::try_new(
        WorkflowId::new(),
        WorkflowTaskId::from("task"),
        /*execution_attempt*/ 1,
        /*task_attempt*/ 1,
    )
    .expect("dispatch marker")
    .bind_agent(WorkflowAgentId::from("agent"))
    .expect("bound dispatch marker");

    assert_eq!(
        marker.bind_agent(WorkflowAgentId::from("other")),
        Err(WorkflowDispatchMarkerError::AgentAlreadyBound)
    );
}
