use super::*;
use crate::app_event::WorkflowManagerAction;
use crate::app_event_sender::AppEventSender;
use codex_app_server_protocol::SavedWorkflow;
use codex_app_server_protocol::Workflow;
use codex_app_server_protocol::WorkflowListItem;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowStatus;
use ratatui::text::Span;

const WORKFLOW_MANAGER_VIEW_ID: &str = "workflow-manager";
const WORKFLOW_DETAIL_VIEW_ID: &str = "workflow-detail";

impl ChatWidget {
    pub(crate) fn open_workflow_manager_loading(&mut self) {
        self.workflow_ui.detail_workflow_id = None;
        self.show_or_replace_workflow_view(|| {
            workflow_message_params(WORKFLOW_MANAGER_VIEW_ID, "Workflows", "Loading workflows…")
        });
    }

    pub(crate) fn show_workflow_manager(&mut self, response: WorkflowListResponse) {
        self.workflow_ui.items = response.data;
        self.show_cached_workflow_manager();
    }

    pub(crate) fn show_cached_workflow_manager(&mut self) {
        self.workflow_ui.detail_workflow_id = None;
        let thread_id = self.thread_id;
        let items = self.workflow_ui.items.clone();
        self.show_or_replace_workflow_view(|| workflow_manager_params(thread_id, &items));
    }

    pub(crate) fn show_workflow_manager_error(&mut self, message: String) {
        self.workflow_ui.detail_workflow_id = None;
        self.show_or_replace_workflow_view(|| {
            workflow_message_params(WORKFLOW_MANAGER_VIEW_ID, "Workflows", &message)
        });
    }

    pub(crate) fn show_workflow_detail(&mut self, workflow: Workflow) {
        upsert_workflow(&mut self.workflow_ui.items, workflow.clone());
        self.workflow_ui.detail_workflow_id = Some(workflow.workflow_id.clone());
        let thread_id = self.thread_id;
        self.show_or_replace_workflow_view(|| workflow_detail_params(thread_id, &workflow));
    }

    pub(crate) fn show_saved_workflow_detail(&mut self, workflow: SavedWorkflow) {
        self.workflow_ui.detail_workflow_id = None;
        let thread_id = self.thread_id;
        self.show_or_replace_workflow_view(|| saved_workflow_detail_params(thread_id, &workflow));
    }

    pub(crate) fn on_workflow_loaded(&mut self, workflow: Workflow) {
        upsert_workflow(&mut self.workflow_ui.items, workflow.clone());
        self.refresh_open_workflow_view(Some(&workflow));
        self.request_redraw();
    }

    pub(crate) fn on_workflow_progress(&mut self, notification: WorkflowProgressNotification) {
        let updated_workflow = self
            .workflow_ui
            .items
            .iter_mut()
            .find_map(|item| match item {
                WorkflowListItem::Run { workflow }
                    if workflow.workflow_id == notification.workflow_id =>
                {
                    workflow.status = notification.status;
                    workflow.completed_tasks = notification.completed_tasks;
                    workflow.running_tasks = notification.running_tasks;
                    workflow.queued_tasks = notification.queued_tasks;
                    workflow.blocked_tasks = notification.blocked_tasks;
                    workflow.failed_tasks = notification.failed_tasks;
                    workflow.summary = notification.summary.clone();
                    Some(workflow.clone())
                }
                WorkflowListItem::Run { .. } | WorkflowListItem::Saved { .. } => None,
            });
        self.refresh_open_workflow_view(updated_workflow.as_ref());
        self.request_redraw();
    }

    pub(crate) fn on_workflow_saved(&mut self, saved_workflow: SavedWorkflow) {
        if let Some(item) = self.workflow_ui.items.iter_mut().find(|item| {
            matches!(
                item,
                WorkflowListItem::Saved { workflow }
                    if workflow.name == saved_workflow.name
            )
        }) {
            *item = WorkflowListItem::Saved {
                workflow: saved_workflow,
            };
        } else {
            self.workflow_ui.items.push(WorkflowListItem::Saved {
                workflow: saved_workflow,
            });
        }
    }

    pub(crate) fn on_workflow_deleted(&mut self, name: &str) {
        self.workflow_ui.items.retain(|item| {
            !matches!(
                item,
                WorkflowListItem::Saved { workflow } if workflow.name == name
            )
        });
    }

    fn refresh_open_workflow_view(&mut self, workflow: Option<&Workflow>) {
        let detail_replaced = workflow.is_some_and(|workflow| {
            self.workflow_ui.detail_workflow_id.as_deref() == Some(workflow.workflow_id.as_str())
                && self.bottom_pane.replace_selection_view_if_active(
                    WORKFLOW_DETAIL_VIEW_ID,
                    workflow_detail_params(self.thread_id, workflow),
                )
        });
        if !detail_replaced {
            let _ = self.bottom_pane.replace_selection_view_if_active(
                WORKFLOW_MANAGER_VIEW_ID,
                workflow_manager_params(self.thread_id, &self.workflow_ui.items),
            );
        }
    }

    fn show_or_replace_workflow_view(&mut self, params: impl Fn() -> SelectionViewParams) {
        if !self.bottom_pane.replace_active_views_with_selection_view(
            &[WORKFLOW_MANAGER_VIEW_ID, WORKFLOW_DETAIL_VIEW_ID],
            params(),
        ) {
            self.bottom_pane.show_selection_view(params());
        }
        self.request_redraw();
    }
}

fn workflow_manager_params(
    thread_id: Option<ThreadId>,
    workflow_items: &[WorkflowListItem],
) -> SelectionViewParams {
    let items = if workflow_items.is_empty() {
        vec![SelectionItem {
            name: "No workflows yet".to_string(),
            description: Some(
                "Trigger ultracode or deep research, then return here to manage it.".to_string(),
            ),
            is_disabled: true,
            ..Default::default()
        }]
    } else {
        workflow_items
            .iter()
            .map(|item| workflow_list_selection_item(thread_id, item))
            .collect()
    };

    SelectionViewParams {
        view_id: Some(WORKFLOW_MANAGER_VIEW_ID),
        title: Some("Workflows".to_string()),
        subtitle: Some("Open a run or saved workflow to manage it.".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

fn workflow_list_selection_item(
    thread_id: Option<ThreadId>,
    item: &WorkflowListItem,
) -> SelectionItem {
    match item {
        WorkflowListItem::Run { workflow } => {
            let actions: Vec<SelectionAction> = thread_id.map_or_else(Vec::new, |thread_id| {
                let workflow_id = workflow.workflow_id.clone();
                let action: SelectionAction = Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::WorkflowManagerAction {
                        thread_id,
                        action: WorkflowManagerAction::Read {
                            workflow_id: workflow_id.clone(),
                        },
                    });
                });
                vec![action]
            });
            SelectionItem {
                name: workflow.title.clone(),
                name_prefix_spans: workflow_status_prefix(workflow.status),
                description: Some(workflow_counts_description(workflow)),
                actions,
                is_disabled: thread_id.is_none(),
                ..Default::default()
            }
        }
        WorkflowListItem::Saved { workflow } => {
            let workflow = workflow.clone();
            SelectionItem {
                name: workflow.title.clone(),
                name_prefix_spans: vec!["[".dim(), "saved".magenta(), "] ".dim()],
                description: Some(format!(
                    "{} tasks · {}",
                    workflow.task_count, workflow.objective
                )),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenSavedWorkflowDetail {
                        workflow: workflow.clone(),
                    });
                })],
                ..Default::default()
            }
        }
    }
}

fn workflow_detail_params(thread_id: Option<ThreadId>, workflow: &Workflow) -> SelectionViewParams {
    let workflow_id = workflow.workflow_id.clone();
    let mut items = Vec::new();
    match workflow.status {
        WorkflowStatus::Planning | WorkflowStatus::Running | WorkflowStatus::Consolidating => {
            items.push(workflow_action_item(
                thread_id,
                "Pause",
                "Pause after currently running work reaches a safe point.",
                WorkflowManagerAction::Pause {
                    workflow_id: workflow_id.clone(),
                },
            ));
            items.push(workflow_action_item(
                thread_id,
                "Cancel",
                "Cancel this workflow and stop scheduling new tasks.",
                WorkflowManagerAction::Cancel {
                    workflow_id: workflow_id.clone(),
                },
            ));
        }
        WorkflowStatus::Pausing => {
            items.push(workflow_action_item(
                thread_id,
                "Cancel",
                "Cancel this workflow instead of waiting for it to pause.",
                WorkflowManagerAction::Cancel {
                    workflow_id: workflow_id.clone(),
                },
            ));
        }
        WorkflowStatus::Paused => {
            items.push(workflow_action_item(
                thread_id,
                "Resume",
                "Continue scheduling tasks from the paused state.",
                WorkflowManagerAction::Resume {
                    workflow_id: workflow_id.clone(),
                },
            ));
            items.push(workflow_action_item(
                thread_id,
                "Cancel",
                "Cancel this paused workflow.",
                WorkflowManagerAction::Cancel {
                    workflow_id: workflow_id.clone(),
                },
            ));
        }
        WorkflowStatus::Failed => {
            items.push(workflow_action_item(
                thread_id,
                "Retry",
                "Retry failed tasks while keeping completed work.",
                WorkflowManagerAction::Retry {
                    workflow_id: workflow_id.clone(),
                },
            ));
        }
        WorkflowStatus::Completed | WorkflowStatus::Cancelled => {}
    }
    items.push(workflow_action_item(
        thread_id,
        "Save",
        "Save this workflow so it can be run again.",
        WorkflowManagerAction::Save {
            workflow_id,
            name: workflow_save_name(&workflow.title),
        },
    ));

    SelectionViewParams {
        view_id: Some(WORKFLOW_DETAIL_VIEW_ID),
        title: Some(workflow.title.clone()),
        subtitle: Some(format!(
            "{} · {}",
            workflow_status_name(workflow.status),
            workflow_counts_description(workflow)
        )),
        footer_note: (!workflow.summary.is_empty())
            .then(|| Line::from(workflow.summary.clone().dim())),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

fn saved_workflow_detail_params(
    thread_id: Option<ThreadId>,
    workflow: &SavedWorkflow,
) -> SelectionViewParams {
    let name = workflow.name.clone();
    let items = vec![
        workflow_action_item(
            thread_id,
            "Run",
            "Start a new run from this saved workflow.",
            WorkflowManagerAction::Run { name: name.clone() },
        ),
        workflow_action_item(
            thread_id,
            "Delete",
            "Delete this saved workflow.",
            WorkflowManagerAction::Delete { name },
        ),
    ];

    SelectionViewParams {
        view_id: Some(WORKFLOW_DETAIL_VIEW_ID),
        title: Some(workflow.title.clone()),
        subtitle: Some(format!("Saved workflow · {} tasks", workflow.task_count)),
        footer_note: Some(Line::from(workflow.objective.clone().dim())),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

fn workflow_action_item(
    thread_id: Option<ThreadId>,
    name: &str,
    description: &str,
    action: WorkflowManagerAction,
) -> SelectionItem {
    let actions: Vec<SelectionAction> = thread_id.map_or_else(Vec::new, |thread_id| {
        let action: SelectionAction = Box::new(move |tx: &AppEventSender| {
            tx.send(AppEvent::WorkflowManagerAction {
                thread_id,
                action: action.clone(),
            });
        });
        vec![action]
    });
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions,
        is_disabled: thread_id.is_none(),
        ..Default::default()
    }
}

fn workflow_message_params(
    view_id: &'static str,
    title: &str,
    message: &str,
) -> SelectionViewParams {
    SelectionViewParams {
        view_id: Some(view_id),
        title: Some(title.to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![SelectionItem {
            name: message.to_string(),
            is_disabled: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn upsert_workflow(items: &mut Vec<WorkflowListItem>, updated_workflow: Workflow) {
    if let Some(item) = items.iter_mut().find(|item| {
        matches!(
            item,
            WorkflowListItem::Run { workflow }
                if workflow.workflow_id == updated_workflow.workflow_id
        )
    }) {
        *item = WorkflowListItem::Run {
            workflow: updated_workflow,
        };
    } else {
        items.insert(
            0,
            WorkflowListItem::Run {
                workflow: updated_workflow,
            },
        );
    }
}

fn workflow_status_prefix(status: WorkflowStatus) -> Vec<Span<'static>> {
    vec!["[".dim(), workflow_status_span(status), "] ".dim()]
}

fn workflow_status_span(status: WorkflowStatus) -> Span<'static> {
    match status {
        WorkflowStatus::Planning => "planning".cyan(),
        WorkflowStatus::Running => "running".cyan(),
        WorkflowStatus::Pausing => "pausing".cyan(),
        WorkflowStatus::Paused => "paused".dim(),
        WorkflowStatus::Consolidating => "consolidating".cyan(),
        WorkflowStatus::Completed => "completed".green(),
        WorkflowStatus::Failed => "failed".red(),
        WorkflowStatus::Cancelled => "cancelled".dim(),
    }
}

fn workflow_status_name(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Planning => "Planning",
        WorkflowStatus::Running => "Running",
        WorkflowStatus::Pausing => "Pausing",
        WorkflowStatus::Paused => "Paused",
        WorkflowStatus::Consolidating => "Consolidating",
        WorkflowStatus::Completed => "Completed",
        WorkflowStatus::Failed => "Failed",
        WorkflowStatus::Cancelled => "Cancelled",
    }
}

fn workflow_counts_description(workflow: &Workflow) -> String {
    format!(
        "{} completed · {} running · {} queued · {} blocked · {} failed",
        workflow.completed_tasks,
        workflow.running_tasks,
        workflow.queued_tasks,
        workflow.blocked_tasks,
        workflow.failed_tasks
    )
}

fn workflow_save_name(title: &str) -> String {
    let mut name = String::new();
    let mut pending_separator = false;
    for character in title.chars() {
        if character.is_alphanumeric() {
            if pending_separator && !name.is_empty() {
                name.push('-');
            }
            name.extend(character.to_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if name.is_empty() {
        "workflow".to_string()
    } else {
        name
    }
}
