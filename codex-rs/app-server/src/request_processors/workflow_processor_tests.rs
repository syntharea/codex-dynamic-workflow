use std::io;

use crate::error_code::INTERNAL_ERROR_CODE;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use codex_core::workflows::WorkflowError;

use super::workflow_api_error;

#[test]
fn workflow_api_io_failures_are_internal_errors() {
    let error = workflow_api_error(
        "save",
        anyhow::Error::new(io::Error::new(io::ErrorKind::PermissionDenied, "read only")),
    );

    assert_eq!(error.code, INTERNAL_ERROR_CODE);
}

#[test]
fn workflow_api_state_errors_are_invalid_requests() {
    let error = workflow_api_error(
        "retry",
        anyhow::Error::new(WorkflowError::RetryRequiresFailedWorkflow),
    );

    assert_eq!(error.code, INVALID_REQUEST_ERROR_CODE);
}

#[test]
fn workflow_api_agent_failures_are_internal_errors() {
    let error = workflow_api_error(
        "pause",
        anyhow::Error::new(WorkflowError::Agent("interrupt failed".to_string())),
    );

    assert_eq!(error.code, INTERNAL_ERROR_CODE);
}
