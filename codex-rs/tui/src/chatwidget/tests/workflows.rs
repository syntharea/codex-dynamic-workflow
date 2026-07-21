use super::*;
use crate::app_event::WorkflowManagerAction;
use codex_app_server_protocol::SavedWorkflow;
use codex_app_server_protocol::Workflow;
use codex_app_server_protocol::WorkflowListItem;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPolicy;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowStartedNotification;
use codex_app_server_protocol::WorkflowStatus;
use pretty_assertions::assert_eq;

fn next_workflow_start(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> (ThreadId, String, WorkflowPolicy) {
    loop {
        let event = rx.try_recv().expect("expected workflow start event");
        if let AppEvent::StartWorkflow {
            thread_id,
            objective,
            policy,
        } = event
        {
            return (thread_id, objective, policy);
        }
    }
}

fn assert_no_workflow_start(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, AppEvent::StartWorkflow { .. }),
            "ordinary prompt unexpectedly started a workflow"
        );
    }
}

fn workflow(id: &str, title: &str, status: WorkflowStatus) -> Workflow {
    Workflow {
        workflow_id: id.to_string(),
        title: title.to_string(),
        status,
        tasks: Vec::new(),
        completed_tasks: usize::from(status == WorkflowStatus::Completed),
        running_tasks: usize::from(status == WorkflowStatus::Running),
        queued_tasks: usize::from(status == WorkflowStatus::Planning),
        blocked_tasks: 0,
        failed_tasks: usize::from(status == WorkflowStatus::Failed),
        summary: format!("{title} summary"),
    }
}

fn saved_workflow(name: &str) -> SavedWorkflow {
    SavedWorkflow {
        name: name.to_string(),
        title: "Saved release workflow".to_string(),
        objective: "Prepare a release".to_string(),
        task_count: 4,
    }
}

fn next_manager_action(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> WorkflowManagerAction {
    loop {
        let event = rx.try_recv().expect("expected workflow manager action");
        if let AppEvent::WorkflowManagerAction { action, .. } = event {
            return action;
        }
    }
}

fn assert_workflow_manager_open_requested(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    loop {
        let event = rx.try_recv().expect("expected workflow manager request");
        if matches!(event, AppEvent::OpenWorkflowManager) {
            return;
        }
    }
}

#[tokio::test]
async fn ordinary_prompt_stays_on_the_regular_turn_path() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    chat.submit_user_message(UserMessage::from("Explain ultracoder behavior"));

    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected regular user turn");
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "Explain ultracoder behavior".to_string(),
            text_elements: Vec::new(),
        }]
    );
    assert_no_workflow_start(&mut rx);
}

#[tokio::test]
async fn standalone_ultracode_word_starts_a_workflow_instead_of_a_turn() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);

    chat.submit_user_message(UserMessage::from("Fix auth, ultracode."));

    assert_eq!(
        next_workflow_start(&mut rx),
        (
            thread_id,
            "Fix auth.".to_string(),
            WorkflowPolicy::Ultracode,
        )
    );
    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn effort_ultracode_arms_exactly_one_prompt_without_changing_reasoning() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    let original_effort = chat.current_reasoning_effort();

    chat.handle_slash_command_with_args_dispatch(
        SlashCommand::Effort,
        "ultracode".to_string(),
        Vec::new(),
    );

    assert_eq!(chat.current_reasoning_effort(), original_effort);
    chat.submit_user_message(UserMessage::from("Fix authentication"));
    assert_eq!(
        next_workflow_start(&mut rx),
        (
            thread_id,
            "Fix authentication".to_string(),
            WorkflowPolicy::Ultracode,
        )
    );
    assert_no_submit_op(&mut op_rx);

    chat.submit_user_message(UserMessage::from("Then explain the fix"));
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected second prompt to use the regular turn path");
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "Then explain the fix".to_string(),
            text_elements: Vec::new(),
        }]
    );
}

#[tokio::test]
async fn deep_research_supports_inline_and_one_shot_pending_objectives() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);

    chat.handle_slash_command_with_args_dispatch(
        SlashCommand::DeepResearch,
        "compare schedulers".to_string(),
        Vec::new(),
    );
    assert_eq!(
        next_workflow_start(&mut rx),
        (
            thread_id,
            "compare schedulers".to_string(),
            WorkflowPolicy::DeepResearch,
        )
    );

    chat.handle_slash_command_dispatch(SlashCommand::DeepResearch);
    chat.submit_user_message(UserMessage::from("map the Rust executor landscape"));
    assert_eq!(
        next_workflow_start(&mut rx),
        (
            thread_id,
            "map the Rust executor landscape".to_string(),
            WorkflowPolicy::DeepResearch,
        )
    );
    assert_no_submit_op(&mut op_rx);

    chat.submit_user_message(UserMessage::from("Summarize the methodology"));
    let Op::UserTurn { .. } = next_submit_op(&mut op_rx) else {
        panic!("expected pending mode to be consumed once");
    };
}

#[tokio::test]
async fn queued_ultracode_trigger_is_parsed_when_the_queue_drains() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.input_queue.user_turn_pending_start = true;
    chat.queue_user_message(UserMessage::from("ultracode harden queue draining"));
    chat.input_queue.user_turn_pending_start = false;

    assert!(chat.maybe_send_next_queued_input());

    assert_eq!(
        next_workflow_start(&mut rx),
        (
            thread_id,
            "harden queue draining".to_string(),
            WorkflowPolicy::Ultracode,
        )
    );
    assert_no_submit_op(&mut op_rx);
    assert!(chat.queued_user_message_texts().is_empty());
}

#[tokio::test]
async fn effort_picker_snapshot_includes_one_shot_ultracode() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    chat.handle_slash_command_dispatch(SlashCommand::Effort);

    assert_chatwidget_snapshot!(
        "dynamic_workflows_effort_picker",
        render_bottom_popup(&chat, /*width*/ 86),
    );
}

#[tokio::test]
async fn workflows_command_opens_manager_and_requests_current_runs() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    chat.handle_slash_command_dispatch(SlashCommand::Workflows);

    assert_workflow_manager_open_requested(&mut rx);
    assert_chatwidget_snapshot!(
        "dynamic_workflows_manager_loading",
        render_bottom_popup(&chat, /*width*/ 92),
    );
}

#[tokio::test]
async fn workflow_manager_empty_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    chat.show_workflow_manager(WorkflowListResponse {
        data: Vec::new(),
        next_cursor: None,
    });

    assert_chatwidget_snapshot!(
        "dynamic_workflows_manager_empty",
        render_bottom_popup(&chat, /*width*/ 92),
    );
}

#[tokio::test]
async fn workflow_manager_lifecycle_states_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let statuses = [
        ("planning", WorkflowStatus::Planning),
        ("running", WorkflowStatus::Running),
        ("paused", WorkflowStatus::Paused),
        ("failed", WorkflowStatus::Failed),
        ("completed", WorkflowStatus::Completed),
    ];

    chat.show_workflow_manager(WorkflowListResponse {
        data: statuses
            .into_iter()
            .map(|(label, status)| WorkflowListItem::Run {
                workflow: workflow(label, &format!("{label} workflow"), status),
            })
            .chain(std::iter::once(WorkflowListItem::Saved {
                workflow: saved_workflow("release"),
            }))
            .collect(),
        next_cursor: None,
    });

    assert_chatwidget_snapshot!(
        "dynamic_workflows_manager_states",
        render_bottom_popup(&chat, /*width*/ 100),
    );
}

#[tokio::test]
async fn workflow_notifications_refresh_the_open_manager() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.show_workflow_manager(WorkflowListResponse {
        data: Vec::new(),
        next_cursor: None,
    });

    chat.handle_server_notification(
        ServerNotification::WorkflowStarted(WorkflowStartedNotification {
            thread_id: thread_id.to_string(),
            workflow: workflow("wf-live", "Live workflow", WorkflowStatus::Running),
        }),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        ServerNotification::WorkflowProgress(WorkflowProgressNotification {
            thread_id: thread_id.to_string(),
            workflow_id: "wf-live".to_string(),
            status: WorkflowStatus::Paused,
            completed_tasks: 1,
            running_tasks: 0,
            queued_tasks: 2,
            blocked_tasks: 0,
            failed_tasks: 0,
            summary: "Paused after the first task".to_string(),
        }),
        /*replay_kind*/ None,
    );

    assert_chatwidget_snapshot!(
        "dynamic_workflows_notification_refreshes_manager",
        render_bottom_popup(&chat, /*width*/ 92),
    );
}

#[tokio::test]
async fn workflow_manager_exposes_every_supported_operation() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    let running = workflow("wf-running", "Running", WorkflowStatus::Running);

    chat.show_workflow_manager(WorkflowListResponse {
        data: vec![WorkflowListItem::Run {
            workflow: running.clone(),
        }],
        next_cursor: None,
    });
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let read = next_manager_action(&mut rx);

    chat.show_workflow_detail(running.clone());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let pause = next_manager_action(&mut rx);

    chat.show_workflow_detail(running);
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let cancel = next_manager_action(&mut rx);

    chat.show_workflow_detail(workflow("wf-paused", "Paused", WorkflowStatus::Paused));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let resume = next_manager_action(&mut rx);

    chat.show_workflow_detail(workflow("wf-failed", "Failed", WorkflowStatus::Failed));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let retry = next_manager_action(&mut rx);

    chat.show_workflow_detail(workflow(
        "wf-completed",
        "Completed",
        WorkflowStatus::Completed,
    ));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let save = next_manager_action(&mut rx);

    let saved = saved_workflow("release");
    chat.show_saved_workflow_detail(saved.clone());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let run = next_manager_action(&mut rx);

    chat.show_saved_workflow_detail(saved);
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let delete = next_manager_action(&mut rx);

    assert_eq!(
        vec![read, pause, cancel, resume, retry, save, run, delete],
        vec![
            WorkflowManagerAction::Read {
                workflow_id: "wf-running".to_string(),
            },
            WorkflowManagerAction::Pause {
                workflow_id: "wf-running".to_string(),
            },
            WorkflowManagerAction::Cancel {
                workflow_id: "wf-running".to_string(),
            },
            WorkflowManagerAction::Resume {
                workflow_id: "wf-paused".to_string(),
            },
            WorkflowManagerAction::Retry {
                workflow_id: "wf-failed".to_string(),
            },
            WorkflowManagerAction::Save {
                workflow_id: "wf-completed".to_string(),
                name: "completed".to_string(),
            },
            WorkflowManagerAction::Run {
                name: "release".to_string(),
            },
            WorkflowManagerAction::Delete {
                name: "release".to_string(),
            },
        ]
    );
}
