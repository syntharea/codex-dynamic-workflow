use super::*;
use codex_app_server_protocol::WorkflowCancelParams;
use codex_app_server_protocol::WorkflowCancelResponse;
use codex_app_server_protocol::WorkflowDeleteParams;
use codex_app_server_protocol::WorkflowDeleteResponse;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPauseParams;
use codex_app_server_protocol::WorkflowPauseResponse;
use codex_app_server_protocol::WorkflowPolicy;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowResumeParams;
use codex_app_server_protocol::WorkflowResumeResponse;
use codex_app_server_protocol::WorkflowRetryParams;
use codex_app_server_protocol::WorkflowRetryResponse;
use codex_app_server_protocol::WorkflowRunParams;
use codex_app_server_protocol::WorkflowRunResponse;
use codex_app_server_protocol::WorkflowSaveParams;
use codex_app_server_protocol::WorkflowSaveResponse;
use codex_app_server_protocol::WorkflowStartParams;
use codex_app_server_protocol::WorkflowStartResponse;
use serde::de::DeserializeOwned;

impl AppServerSession {
    async fn workflow_request<T>(
        &mut self,
        build_request: impl FnOnce(RequestId) -> ClientRequest,
        context: &'static str,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let request_id = self.next_request_id();
        self.client
            .request_typed(build_request(request_id))
            .await
            .wrap_err(context)
    }

    pub(crate) async fn workflow_start(
        &mut self,
        thread_id: ThreadId,
        objective: String,
        policy: WorkflowPolicy,
    ) -> Result<WorkflowStartResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowStart {
                request_id,
                params: WorkflowStartParams {
                    thread_id: thread_id.to_string(),
                    objective,
                    policy: Some(policy),
                },
            },
            "workflow/start failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_list(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<WorkflowListResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowList {
                request_id,
                params: WorkflowListParams {
                    thread_id: thread_id.to_string(),
                    cursor: None,
                    limit: None,
                },
            },
            "workflow/list failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_read(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
    ) -> Result<WorkflowReadResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowRead {
                request_id,
                params: WorkflowReadParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                },
            },
            "workflow/read failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_pause(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
    ) -> Result<WorkflowPauseResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowPause {
                request_id,
                params: WorkflowPauseParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                },
            },
            "workflow/pause failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_resume(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
    ) -> Result<WorkflowResumeResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowResume {
                request_id,
                params: WorkflowResumeParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                },
            },
            "workflow/resume failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_cancel(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
    ) -> Result<WorkflowCancelResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowCancel {
                request_id,
                params: WorkflowCancelParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                },
            },
            "workflow/cancel failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_retry(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
    ) -> Result<WorkflowRetryResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowRetry {
                request_id,
                params: WorkflowRetryParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                },
            },
            "workflow/retry failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_save(
        &mut self,
        thread_id: ThreadId,
        workflow_id: String,
        name: String,
    ) -> Result<WorkflowSaveResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowSave {
                request_id,
                params: WorkflowSaveParams {
                    thread_id: thread_id.to_string(),
                    workflow_id,
                    name,
                },
            },
            "workflow/save failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_run(
        &mut self,
        thread_id: ThreadId,
        name: String,
    ) -> Result<WorkflowRunResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowRun {
                request_id,
                params: WorkflowRunParams {
                    thread_id: thread_id.to_string(),
                    name,
                    task_overrides: None,
                },
            },
            "workflow/run failed in TUI",
        )
        .await
    }

    pub(crate) async fn workflow_delete(
        &mut self,
        thread_id: ThreadId,
        name: String,
    ) -> Result<WorkflowDeleteResponse> {
        self.workflow_request(
            |request_id| ClientRequest::WorkflowDelete {
                request_id,
                params: WorkflowDeleteParams {
                    thread_id: thread_id.to_string(),
                    name,
                },
            },
            "workflow/delete failed in TUI",
        )
        .await
    }
}
