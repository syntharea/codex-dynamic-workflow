mod agent_control;
mod api;
mod background;
mod events;
mod manager;
mod persistence;
mod planner;
mod prompts;
mod recovery;
mod runtime;
mod saved;
mod saved_fs;

pub use agent_control::WorkflowAgentControlAdapter;
pub use api::SavedWorkflowSummary;
pub use api::WorkflowApi;
pub use api::WorkflowList;
pub use api::api_for_thread;
pub use codex_workflows::WorkflowError;
pub use manager::WorkflowManager;
pub use manager::WorkflowManagerConfig;
pub use manager::WorkflowProgress;
pub use manager::WorkflowRuntimeEvent;
pub(crate) use recovery::WorkflowTerminalEvidence;
pub(crate) use runtime::handle_session_op;

pub async fn adapter_for_thread(
    thread: &crate::CodexThread,
) -> std::sync::Arc<WorkflowAgentControlAdapter> {
    let session_source = thread.session.thread_config_snapshot().await.session_source;
    std::sync::Arc::new(WorkflowAgentControlAdapter::new(
        thread.session.services.agent_control.clone(),
        thread.session.get_config().await,
        thread.session.thread_id(),
        session_source,
    ))
}

pub fn manager_for_thread(thread: &crate::CodexThread) -> std::sync::Arc<WorkflowManager> {
    thread.session.services.agent_control.workflow_manager()
}

pub async fn saved_workflows_for_thread(
    thread: &crate::CodexThread,
) -> anyhow::Result<Vec<codex_workflows::SavedWorkflowDefinition>> {
    use anyhow::Context;

    let limits = manager_for_thread(thread).config().workflow_limits();
    let config = thread.session.get_config().await;
    let store = saved::SavedWorkflowStore::from_config(config.as_ref(), limits)?;
    tokio::task::spawn_blocking(move || store.list())
        .await
        .context("saved workflow list task panicked")?
        .map_err(anyhow::Error::from)
}
