use super::*;
use codex_app_server_protocol::WorkflowListItem;
use codex_app_server_protocol::WorkflowPolicy;
use codex_workflows::WorkflowTrigger;
use codex_workflows::parse_workflow_trigger;

#[derive(Debug, Default)]
pub(super) struct WorkflowUiState {
    pub(super) pending_policy: Option<WorkflowPolicy>,
    pub(super) items: Vec<WorkflowListItem>,
    pub(super) detail_workflow_id: Option<String>,
}

impl ChatWidget {
    pub(crate) fn arm_workflow(&mut self, policy: WorkflowPolicy) {
        let message = match &policy {
            WorkflowPolicy::Ultracode => "Ultracode armed for your next prompt.",
            WorkflowPolicy::DeepResearch => "Deep research armed for your next prompt.",
        };
        self.workflow_ui.pending_policy = Some(policy);
        self.add_info_message(message.to_string(), /*hint*/ None);
    }

    pub(super) fn open_effort_popup(&mut self) {
        let mut items = vec![SelectionItem {
            name: "Ultracode (next prompt)".to_string(),
            description: Some(
                "Plan and execute a dynamic workflow once without changing reasoning defaults."
                    .to_string(),
            ),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::ArmWorkflow {
                    policy: WorkflowPolicy::Ultracode,
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        }];

        if let Some(preset) = self
            .model_catalog
            .try_list_models()
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == self.current_model())
            })
        {
            items.push(SelectionItem {
                name: "Regular reasoning effort…".to_string(),
                description: Some("Change the current model's reasoning level.".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenReasoningPopup {
                        model: preset.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Effort".to_string()),
            subtitle: Some(
                "Ultracode is one-shot; regular reasoning settings remain unchanged.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(super) fn handle_effort_args(&mut self, args: &str) {
        if args.eq_ignore_ascii_case("ultracode") {
            self.arm_workflow(WorkflowPolicy::Ultracode);
        } else {
            self.add_error_message("Usage: /effort ultracode".to_string());
        }
    }

    pub(super) fn handle_deep_research_args(&mut self, objective: &str) {
        if objective.trim().is_empty() {
            self.arm_workflow(WorkflowPolicy::DeepResearch);
            return;
        }
        let Some(thread_id) = self.thread_id else {
            self.add_error_message(
                "Deep research is unavailable before the session starts.".to_string(),
            );
            return;
        };
        self.app_event_tx.send(AppEvent::StartWorkflow {
            thread_id,
            objective: objective.to_string(),
            policy: WorkflowPolicy::DeepResearch,
        });
    }

    pub(super) fn handle_workflow_submission(
        &mut self,
        user_message: &UserMessage,
        history_record: &UserMessageHistoryRecord,
    ) -> bool {
        if !user_message.local_images.is_empty()
            || !user_message.remote_image_urls.is_empty()
            || user_message.text.trim_start().starts_with('!')
        {
            return false;
        }

        let explicit_trigger = parse_workflow_trigger(&user_message.text);
        let request = match explicit_trigger {
            Some(WorkflowTrigger::Ultracode { objective }) => {
                Some((WorkflowPolicy::Ultracode, objective))
            }
            Some(WorkflowTrigger::DeepResearch { topic }) => {
                Some((WorkflowPolicy::DeepResearch, topic))
            }
            Some(WorkflowTrigger::PendingUltracode) => {
                self.arm_workflow(WorkflowPolicy::Ultracode);
                return true;
            }
            Some(WorkflowTrigger::PendingDeepResearch) => {
                self.arm_workflow(WorkflowPolicy::DeepResearch);
                return true;
            }
            None if !user_message.text.trim_start().starts_with('/') => self
                .workflow_ui
                .pending_policy
                .take()
                .map(|policy| (policy, user_message.text.trim().to_string())),
            None => None,
        };
        let Some((policy, objective)) = request else {
            return false;
        };
        let Some(thread_id) = self.thread_id else {
            return false;
        };

        self.workflow_ui.pending_policy = None;
        self.on_user_message_display(user_message_display_for_history(
            user_message.clone(),
            history_record,
        ));
        if !user_message.text.is_empty() {
            self.append_message_history_entry(user_message.text.clone());
        }
        self.app_event_tx.send(AppEvent::StartWorkflow {
            thread_id,
            objective,
            policy,
        });
        true
    }
}
