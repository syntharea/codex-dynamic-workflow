use std::sync::Arc;

use anyhow::Error as AnyhowError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowCancelParams;
use codex_app_server_protocol::WorkflowCancelResponse;
use codex_app_server_protocol::WorkflowDeleteParams;
use codex_app_server_protocol::WorkflowDeleteResponse;
use codex_app_server_protocol::WorkflowListItem;
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
use codex_core::ThreadManager;
use codex_core::workflows::WorkflowApi;
use codex_core::workflows::WorkflowError;
use codex_core::workflows::api_for_thread;
use codex_protocol::ThreadId;
use codex_protocol::workflow::WorkflowCancelParams as CoreWorkflowCancelParams;
use codex_protocol::workflow::WorkflowDeleteSavedParams as CoreWorkflowDeleteSavedParams;
use codex_protocol::workflow::WorkflowId as CoreWorkflowId;
use codex_protocol::workflow::WorkflowName as CoreWorkflowName;
use codex_protocol::workflow::WorkflowObjective as CoreWorkflowObjective;
use codex_protocol::workflow::WorkflowPauseParams as CoreWorkflowPauseParams;
use codex_protocol::workflow::WorkflowPolicy as CoreWorkflowPolicy;
use codex_protocol::workflow::WorkflowReadParams as CoreWorkflowReadParams;
use codex_protocol::workflow::WorkflowResumeParams as CoreWorkflowResumeParams;
use codex_protocol::workflow::WorkflowRetryParams as CoreWorkflowRetryParams;
use codex_protocol::workflow::WorkflowRunSavedParams as CoreWorkflowRunSavedParams;
use codex_protocol::workflow::WorkflowSaveParams as CoreWorkflowSaveParams;
use codex_protocol::workflow::WorkflowStartParams as CoreWorkflowStartParams;
use codex_protocol::workflow::WorkflowTaskOverride as CoreWorkflowTaskOverride;
use codex_protocol::workflow::WorkflowTaskOverrides as CoreWorkflowTaskOverrides;
use codex_protocol::workflow::WorkflowText as CoreWorkflowText;
use codex_protocol::workflow::WorkflowTitle as CoreWorkflowTitle;
use uuid::Uuid;

use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::error_code::invalid_request;
use crate::workflow_mapping::saved_workflow_from_core;
use crate::workflow_mapping::workflow_from_core;

use super::thread_processor::core_thread_write_error;

pub(crate) struct WorkflowRequestProcessor {
    thread_manager: Arc<ThreadManager>,
}

impl WorkflowRequestProcessor {
    pub(crate) fn new(thread_manager: Arc<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    pub(crate) async fn workflow_start(
        &self,
        params: WorkflowStartParams,
    ) -> Result<WorkflowStartResponse, JSONRPCErrorError> {
        let WorkflowStartParams {
            thread_id,
            objective,
            policy,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let objective = CoreWorkflowObjective::new(objective).map_err(workflow_params_error)?;
        let policy = policy.map(policy_to_core).transpose()?;
        let workflow = api
            .start(CoreWorkflowStartParams { objective, policy })
            .await
            .map_err(|error| workflow_api_error("start", error))?;
        Ok(WorkflowStartResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_read(
        &self,
        params: WorkflowReadParams,
    ) -> Result<WorkflowReadResponse, JSONRPCErrorError> {
        let WorkflowReadParams {
            thread_id,
            workflow_id,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let workflow = api
            .read(CoreWorkflowReadParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
            })
            .await
            .map_err(|error| workflow_api_error("read", error))?;
        Ok(WorkflowReadResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_list(
        &self,
        params: WorkflowListParams,
    ) -> Result<WorkflowListResponse, JSONRPCErrorError> {
        let WorkflowListParams {
            thread_id,
            cursor,
            limit,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let listed = api
            .list()
            .await
            .map_err(|error| workflow_api_error("list", error))?;
        let mut items = listed
            .workflows
            .iter()
            .map(|workflow| {
                (
                    format!("run:{}", workflow.workflow_id()),
                    WorkflowListItem::Run {
                        workflow: workflow_from_core(workflow),
                    },
                )
            })
            .collect::<Vec<_>>();
        items.extend(listed.saved_workflows.into_iter().map(|saved| {
            let name = saved.name.clone();
            (
                format!("saved:{name}"),
                WorkflowListItem::Saved {
                    workflow: saved_workflow_from_core(saved),
                },
            )
        }));
        items.sort_by(|left, right| left.0.cmp(&right.0));
        paginate_workflows(items, cursor, limit)
    }

    pub(crate) async fn workflow_pause(
        &self,
        params: WorkflowPauseParams,
    ) -> Result<WorkflowPauseResponse, JSONRPCErrorError> {
        let WorkflowPauseParams {
            thread_id,
            workflow_id,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let workflow = api
            .pause(CoreWorkflowPauseParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
            })
            .await
            .map_err(|error| workflow_api_error("pause", error))?;
        Ok(WorkflowPauseResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_resume(
        &self,
        params: WorkflowResumeParams,
    ) -> Result<WorkflowResumeResponse, JSONRPCErrorError> {
        let WorkflowResumeParams {
            thread_id,
            workflow_id,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let workflow = api
            .resume(CoreWorkflowResumeParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
            })
            .await
            .map_err(|error| workflow_api_error("resume", error))?;
        Ok(WorkflowResumeResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_cancel(
        &self,
        params: WorkflowCancelParams,
    ) -> Result<WorkflowCancelResponse, JSONRPCErrorError> {
        let WorkflowCancelParams {
            thread_id,
            workflow_id,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let workflow = api
            .cancel(CoreWorkflowCancelParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
            })
            .await
            .map_err(|error| workflow_api_error("cancel", error))?;
        Ok(WorkflowCancelResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_retry(
        &self,
        params: WorkflowRetryParams,
    ) -> Result<WorkflowRetryResponse, JSONRPCErrorError> {
        let WorkflowRetryParams {
            thread_id,
            workflow_id,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let workflow = api
            .retry(CoreWorkflowRetryParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
            })
            .await
            .map_err(|error| workflow_api_error("retry", error))?;
        Ok(WorkflowRetryResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_save(
        &self,
        params: WorkflowSaveParams,
    ) -> Result<WorkflowSaveResponse, JSONRPCErrorError> {
        let WorkflowSaveParams {
            thread_id,
            workflow_id,
            name,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let saved = api
            .save(CoreWorkflowSaveParams {
                workflow_id: workflow_id_to_core(workflow_id)?,
                name: workflow_name_to_core(name)?,
            })
            .await
            .map_err(|error| workflow_api_error("save", error))?;
        Ok(WorkflowSaveResponse {
            saved_workflow: saved_workflow_from_core(saved),
        })
    }

    pub(crate) async fn workflow_run(
        &self,
        params: WorkflowRunParams,
    ) -> Result<WorkflowRunResponse, JSONRPCErrorError> {
        let WorkflowRunParams {
            thread_id,
            name,
            task_overrides,
        } = params;
        let api = self.workflow_api(&thread_id).await?;
        let task_overrides = task_overrides
            .unwrap_or_default()
            .into_iter()
            .map(|task_override| {
                let title = task_override
                    .title
                    .map(CoreWorkflowTitle::new)
                    .transpose()
                    .map_err(workflow_params_error)?;
                let instructions = task_override
                    .instructions
                    .map(CoreWorkflowText::new)
                    .transpose()
                    .map_err(workflow_params_error)?;
                CoreWorkflowTaskOverride::try_new(task_override.task_id, title, instructions)
                    .map_err(workflow_params_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let workflow = api
            .run_saved(CoreWorkflowRunSavedParams {
                name: workflow_name_to_core(name)?,
                task_overrides: CoreWorkflowTaskOverrides::try_new(task_overrides)
                    .map_err(workflow_params_error)?,
            })
            .await
            .map_err(|error| workflow_api_error("run saved", error))?;
        Ok(WorkflowRunResponse {
            workflow: workflow_from_core(&workflow),
        })
    }

    pub(crate) async fn workflow_delete(
        &self,
        params: WorkflowDeleteParams,
    ) -> Result<WorkflowDeleteResponse, JSONRPCErrorError> {
        let WorkflowDeleteParams { thread_id, name } = params;
        let api = self.workflow_api(&thread_id).await?;
        let deleted = api
            .delete_saved(CoreWorkflowDeleteSavedParams {
                name: workflow_name_to_core(name)?,
            })
            .await
            .map_err(|error| workflow_api_error("delete saved", error))?;
        Ok(WorkflowDeleteResponse { deleted })
    }

    async fn workflow_api(&self, thread_id: &str) -> Result<WorkflowApi, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(thread_id)
            .map_err(|error| invalid_params(format!("invalid threadId: {error}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|error| core_thread_write_error("load workflow thread", error))?;
        Ok(api_for_thread(&thread))
    }
}

fn policy_to_core(policy: WorkflowPolicy) -> Result<CoreWorkflowPolicy, JSONRPCErrorError> {
    match policy {
        WorkflowPolicy::Ultracode => Ok(CoreWorkflowPolicy::Ultracode),
        WorkflowPolicy::DeepResearch => Ok(CoreWorkflowPolicy::DeepResearch),
    }
}

fn workflow_id_to_core(workflow_id: String) -> Result<CoreWorkflowId, JSONRPCErrorError> {
    Uuid::parse_str(&workflow_id)
        .map_err(|error| invalid_params(format!("invalid workflowId: {error}")))?;
    CoreWorkflowId::new(workflow_id).map_err(workflow_params_error)
}

fn workflow_name_to_core(name: String) -> Result<CoreWorkflowName, JSONRPCErrorError> {
    CoreWorkflowName::new(name).map_err(workflow_params_error)
}

fn paginate_workflows(
    items: Vec<(String, WorkflowListItem)>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<WorkflowListResponse, JSONRPCErrorError> {
    let total = items.len();
    let start = match cursor {
        Some(cursor) => cursor
            .parse::<usize>()
            .map_err(|_| invalid_request(format!("invalid cursor: {cursor}")))?,
        None => 0,
    };
    if start > total {
        return Err(invalid_request(format!(
            "cursor {start} exceeds total workflows {total}"
        )));
    }

    let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
    let end = start.saturating_add(effective_limit).min(total);
    let data = items[start..end]
        .iter()
        .map(|(_, item)| item.clone())
        .collect();
    let next_cursor = (end < total).then(|| end.to_string());
    Ok(WorkflowListResponse { data, next_cursor })
}

fn workflow_params_error(error: impl std::fmt::Display) -> JSONRPCErrorError {
    invalid_params(format!("invalid workflow parameters: {error}"))
}

fn workflow_api_error(operation: &str, error: AnyhowError) -> JSONRPCErrorError {
    if workflow_api_error_is_invalid_request(&error) {
        return invalid_request(format!("failed to {operation} workflow: {error:#}"));
    }

    tracing::warn!(operation, error = %error, "workflow API operation failed internally");
    internal_error(format!("failed to {operation} workflow"))
}

fn workflow_api_error_is_invalid_request(error: &AnyhowError) -> bool {
    if let Some(error) = error.downcast_ref::<WorkflowError>() {
        return match error {
            WorkflowError::Validation(_)
            | WorkflowError::Transition(_)
            | WorkflowError::TaskNotFound(_)
            | WorkflowError::InvalidConcurrency
            | WorkflowError::RetryRequiresFailedWorkflow
            | WorkflowError::InvalidSnapshot(_)
            | WorkflowError::WorkflowsDisabled
            | WorkflowError::AlreadyRunning => true,
            WorkflowError::ExecutionAttemptOverflow
            | WorkflowError::Agent(_)
            | WorkflowError::ExecutorCapacity(_)
            | WorkflowError::ExecutorFailure(_)
            | WorkflowError::ConfigurationLocked => false,
        };
    }

    matches!(
        error.to_string().as_str(),
        "dynamic workflows feature is disabled" | "dynamic workflows require multi-agent support"
    )
}

#[cfg(test)]
#[path = "workflow_processor_tests.rs"]
mod tests;
