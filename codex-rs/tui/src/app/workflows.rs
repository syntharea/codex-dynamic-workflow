use super::*;
use crate::app_event::WorkflowManagerAction;
use codex_app_server_protocol::WorkflowPolicy;

impl App {
    pub(super) async fn open_workflow_manager(&mut self, app_server: &mut AppServerSession) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.show_workflow_manager_error(
                "Workflows are unavailable before the session starts.".to_string(),
            );
            return;
        };
        match app_server.workflow_list(thread_id).await {
            Ok(response) => self.chat_widget.show_workflow_manager(response),
            Err(err) => {
                tracing::warn!(error = %err, "workflow/list failed from TUI");
                self.chat_widget
                    .show_workflow_manager_error(format!("Failed to load workflows: {err}"));
            }
        }
    }

    pub(super) async fn start_workflow(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        objective: String,
        policy: WorkflowPolicy,
    ) {
        match app_server
            .workflow_start(thread_id, objective, policy)
            .await
        {
            Ok(response) => self.chat_widget.on_workflow_loaded(response.workflow),
            Err(err) => {
                tracing::warn!(error = %err, "workflow/start failed from TUI");
                self.chat_widget
                    .add_error_message(format!("Failed to start workflow: {err}"));
            }
        }
    }

    pub(super) async fn handle_workflow_manager_action(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        action: WorkflowManagerAction,
    ) {
        match action {
            WorkflowManagerAction::Read { workflow_id } => {
                match app_server.workflow_read(thread_id, workflow_id).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to read workflow: {err}")),
                }
            }
            WorkflowManagerAction::Pause { workflow_id } => {
                match app_server.workflow_pause(thread_id, workflow_id).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to pause workflow: {err}")),
                }
            }
            WorkflowManagerAction::Resume { workflow_id } => {
                match app_server.workflow_resume(thread_id, workflow_id).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to resume workflow: {err}")),
                }
            }
            WorkflowManagerAction::Cancel { workflow_id } => {
                match app_server.workflow_cancel(thread_id, workflow_id).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to cancel workflow: {err}")),
                }
            }
            WorkflowManagerAction::Retry { workflow_id } => {
                match app_server.workflow_retry(thread_id, workflow_id).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to retry workflow: {err}")),
                }
            }
            WorkflowManagerAction::Save { workflow_id, name } => {
                match app_server.workflow_save(thread_id, workflow_id, name).await {
                    Ok(response) => {
                        let name = response.saved_workflow.name.clone();
                        self.chat_widget.on_workflow_saved(response.saved_workflow);
                        self.chat_widget.add_info_message(
                            format!("Saved workflow as '{name}'."),
                            /*hint*/ None,
                        );
                        self.chat_widget.show_cached_workflow_manager();
                    }
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to save workflow: {err}")),
                }
            }
            WorkflowManagerAction::Run { name } => {
                match app_server.workflow_run(thread_id, name).await {
                    Ok(response) => self.chat_widget.show_workflow_detail(response.workflow),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to run workflow: {err}")),
                }
            }
            WorkflowManagerAction::Delete { name } => {
                match app_server.workflow_delete(thread_id, name.clone()).await {
                    Ok(response) if response.deleted => {
                        self.chat_widget.on_workflow_deleted(&name);
                        self.chat_widget.add_info_message(
                            format!("Deleted saved workflow '{name}'."),
                            /*hint*/ None,
                        );
                        self.chat_widget.show_cached_workflow_manager();
                    }
                    Ok(_) => self
                        .chat_widget
                        .add_error_message(format!("Saved workflow '{name}' was not deleted.")),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to delete workflow: {err}")),
                }
            }
        }
    }
}
