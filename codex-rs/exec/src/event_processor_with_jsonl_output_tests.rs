use super::*;
use codex_app_server_protocol::WorkflowStatus;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn failed_turn_does_not_overwrite_output_last_message_file() {
    let tempdir = tempdir().expect("create tempdir");
    let output_path = tempdir.path().join("last-message.txt");
    std::fs::write(&output_path, "keep existing contents").expect("seed output file");

    let mut processor = EventProcessorWithJsonOutput::new(Some(output_path.clone()));

    let collected = processor.collect_thread_events(ServerNotification::ItemCompleted(
        codex_app_server_protocol::ItemCompletedNotification {
            item: ThreadItem::AgentMessage {
                id: "msg-1".to_string(),
                text: "partial answer".to_string(),
                phase: None,
                memory_citation: None,
            },
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
        },
    ));

    assert_eq!(collected.status, CodexStatus::Running);
    assert_eq!(processor.final_message(), Some("partial answer"));

    let status = processor.process_server_notification(ServerNotification::TurnCompleted(
        codex_app_server_protocol::TurnCompletedNotification {
            thread_id: "thread-1".to_string(),
            turn: codex_app_server_protocol::Turn {
                id: "turn-1".to_string(),
                items_view: codex_app_server_protocol::TurnItemsView::Full,
                items: Vec::new(),
                status: TurnStatus::Failed,
                error: Some(codex_app_server_protocol::TurnError {
                    message: "turn failed".to_string(),
                    additional_details: None,
                    codex_error_info: None,
                }),
                started_at: None,
                completed_at: Some(0),
                duration_ms: None,
            },
        },
    ));

    assert_eq!(status, CodexStatus::InitiateShutdown);
    assert_eq!(processor.final_message(), None);

    EventProcessor::print_final_output(&mut processor);

    assert_eq!(
        std::fs::read_to_string(&output_path).expect("read output file"),
        "keep existing contents"
    );
}

#[test]
fn runtime_warning_emits_a_non_fatal_error_item() {
    let mut processor = EventProcessorWithJsonOutput::new(/*last_message_path*/ None);

    let collected = processor.collect_thread_events(ServerNotification::Warning(
        codex_app_server_protocol::WarningNotification {
            thread_id: Some("thread-1".to_string()),
            message: "invalid global instructions".to_string(),
        },
    ));

    assert_eq!(
        collected,
        CollectedThreadEvents {
            events: vec![ThreadEvent::ItemCompleted(ItemCompletedEvent {
                item: ExecThreadItem {
                    id: "item_0".to_string(),
                    details: ThreadItemDetails::Error(ErrorItem {
                        message: "invalid global instructions".to_string(),
                    }),
                },
            })],
            status: CodexStatus::Running,
        }
    );
}

#[test]
fn mcp_tool_call_result_preserves_meta_in_jsonl_event() {
    let mut processor = EventProcessorWithJsonOutput::new(/*last_message_path*/ None);

    let collected = processor.collect_thread_events(ServerNotification::ItemCompleted(
        codex_app_server_protocol::ItemCompletedNotification {
            item: ThreadItem::McpToolCall {
                id: "mcp-1".to_string(),
                server: "search service".to_string(),
                tool: "web_run".to_string(),
                status: McpToolCallStatus::Completed,
                arguments: json!({"search_query": [{"q": "OpenAI Codex CLI documentation"}]}),
                app_context: None,
                mcp_app_resource_uri: None,
                plugin_id: None,
                result: Some(Box::new(codex_app_server_protocol::McpToolCallResult {
                    content: vec![json!({"type": "text", "text": "search result"})],
                    structured_content: None,
                    meta: Some(json!({"raw_messages": [{"ref_id": "turn0search0"}]})),
                })),
                error: None,
                duration_ms: Some(42),
            },
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
        },
    ));

    assert_eq!(collected.status, CodexStatus::Running);
    assert_eq!(collected.events.len(), 1);

    let ThreadEvent::ItemCompleted(ItemCompletedEvent { item }) = &collected.events[0] else {
        panic!("expected item.completed event");
    };
    let ThreadItemDetails::McpToolCall(item) = &item.details else {
        panic!("expected MCP tool call item");
    };
    let result = item.result.as_ref().expect("expected MCP tool result");
    assert_eq!(
        result.meta,
        Some(json!({"raw_messages": [{"ref_id": "turn0search0"}]}))
    );

    let serialized = serde_json::to_value(&collected.events[0]).expect("serialize event");
    assert_eq!(
        serialized["item"]["result"]["_meta"],
        json!({"raw_messages": [{"ref_id": "turn0search0"}]})
    );
    assert!(serialized["item"]["result"].get("meta").is_none());
}

#[test]
fn workflow_notifications_emit_jsonl_lifecycle_events() {
    let workflow = test_workflow(WorkflowStatus::Running, "running");
    let notifications = [
        ServerNotification::WorkflowStarted(
            codex_app_server_protocol::WorkflowStartedNotification {
                thread_id: "thread-1".to_string(),
                workflow: workflow,
            },
        ),
        ServerNotification::WorkflowProgress(
            codex_app_server_protocol::WorkflowProgressNotification {
                thread_id: "thread-1".to_string(),
                workflow_id: "workflow-1".to_string(),
                status: WorkflowStatus::Running,
                completed_tasks: 0,
                running_tasks: 1,
                queued_tasks: 0,
                blocked_tasks: 0,
                failed_tasks: 0,
                summary: "running".to_string(),
            },
        ),
        ServerNotification::WorkflowPaused(codex_app_server_protocol::WorkflowPausedNotification {
            thread_id: "thread-1".to_string(),
            workflow: test_workflow(WorkflowStatus::Paused, "paused"),
        }),
        ServerNotification::WorkflowCompleted(
            codex_app_server_protocol::WorkflowCompletedNotification {
                thread_id: "thread-1".to_string(),
                workflow: test_workflow(WorkflowStatus::Completed, "complete"),
            },
        ),
        ServerNotification::WorkflowFailed(codex_app_server_protocol::WorkflowFailedNotification {
            thread_id: "thread-1".to_string(),
            workflow: test_workflow(WorkflowStatus::Failed, "failed"),
            error: "worker failed".to_string(),
        }),
        ServerNotification::WorkflowCancelled(
            codex_app_server_protocol::WorkflowCancelledNotification {
                thread_id: "thread-1".to_string(),
                workflow: test_workflow(WorkflowStatus::Cancelled, "cancelled"),
            },
        ),
    ];
    let mut processor = EventProcessorWithJsonOutput::new(/*last_message_path*/ None);
    let collected = notifications.map(|notification| {
        let collected = processor.collect_thread_events(notification);
        (
            collected
                .events
                .into_iter()
                .map(|event| serde_json::to_value(event).expect("serialize event"))
                .collect::<Vec<_>>(),
            collected.status,
        )
    });

    assert_eq!(
        collected,
        [
            (
                vec![json!({
                    "type": "workflow.started",
                    "thread_id": "thread-1",
                    "workflow": test_workflow_json(WorkflowStatus::Running, "running"),
                })],
                CodexStatus::Running,
            ),
            (
                vec![json!({
                    "type": "workflow.progress",
                    "thread_id": "thread-1",
                    "workflow_id": "workflow-1",
                    "status": "running",
                    "completed_tasks": 0,
                    "running_tasks": 1,
                    "queued_tasks": 0,
                    "blocked_tasks": 0,
                    "failed_tasks": 0,
                    "summary": "running",
                })],
                CodexStatus::Running,
            ),
            (
                vec![json!({
                    "type": "workflow.paused",
                    "thread_id": "thread-1",
                    "workflow": test_workflow_json(WorkflowStatus::Paused, "paused"),
                })],
                CodexStatus::Running,
            ),
            (
                vec![json!({
                    "type": "workflow.completed",
                    "thread_id": "thread-1",
                    "workflow": test_workflow_json(WorkflowStatus::Completed, "complete"),
                })],
                CodexStatus::InitiateShutdown,
            ),
            (
                vec![json!({
                    "type": "workflow.failed",
                    "thread_id": "thread-1",
                    "workflow": test_workflow_json(WorkflowStatus::Failed, "failed"),
                    "error": "worker failed",
                })],
                CodexStatus::InitiateShutdown,
            ),
            (
                vec![json!({
                    "type": "workflow.cancelled",
                    "thread_id": "thread-1",
                    "workflow": test_workflow_json(WorkflowStatus::Cancelled, "cancelled"),
                })],
                CodexStatus::InitiateShutdown,
            ),
        ]
    );
}

#[test]
fn workflow_jsonl_fields_have_hard_limits() {
    const ID_MAX_BYTES: usize = 256;
    const TEXT_MAX_BYTES: usize = 4 * 1024;
    const TASKS_MAX: usize = 256;

    let long_id = "i".repeat(ID_MAX_BYTES + 1);
    let long_text = "é".repeat(TEXT_MAX_BYTES);
    let task = codex_app_server_protocol::WorkflowTask {
        task_id: long_id.clone(),
        title: long_text.clone(),
        status: codex_app_server_protocol::WorkflowTaskStatus::Running,
        summary: long_text.clone(),
        attempt: 1,
    };
    let workflow = codex_app_server_protocol::Workflow {
        workflow_id: long_id.clone(),
        title: long_text.clone(),
        status: WorkflowStatus::Running,
        tasks: vec![task; TASKS_MAX + 1],
        completed_tasks: 0,
        running_tasks: 1,
        queued_tasks: TASKS_MAX,
        blocked_tasks: 0,
        failed_tasks: 0,
        summary: long_text,
    };
    let mut processor = EventProcessorWithJsonOutput::new(/*last_message_path*/ None);
    let collected = processor.collect_thread_events(ServerNotification::WorkflowFailed(
        codex_app_server_protocol::WorkflowFailedNotification {
            thread_id: long_id,
            workflow,
            error: "e".repeat(TEXT_MAX_BYTES + 1),
        },
    ));
    let event = serde_json::to_value(&collected.events[0]).expect("serialize workflow event");
    let workflow = &event["workflow"];

    assert!(event["thread_id"].as_str().expect("thread id").len() <= ID_MAX_BYTES);
    assert!(event["error"].as_str().expect("error").len() <= TEXT_MAX_BYTES);
    assert!(workflow["workflow_id"].as_str().expect("workflow id").len() <= ID_MAX_BYTES);
    assert!(workflow["title"].as_str().expect("title").len() <= TEXT_MAX_BYTES);
    assert!(workflow["summary"].as_str().expect("summary").len() <= TEXT_MAX_BYTES);
    let tasks = workflow["tasks"].as_array().expect("workflow tasks");
    assert_eq!(tasks.len(), TASKS_MAX);
    assert!(tasks[0]["task_id"].as_str().expect("task id").len() <= ID_MAX_BYTES);
    assert!(tasks[0]["title"].as_str().expect("task title").len() <= TEXT_MAX_BYTES);
    assert!(tasks[0]["summary"].as_str().expect("task summary").len() <= TEXT_MAX_BYTES);
}

fn test_workflow(status: WorkflowStatus, summary: &str) -> codex_app_server_protocol::Workflow {
    codex_app_server_protocol::Workflow {
        workflow_id: "workflow-1".to_string(),
        title: "Test workflow".to_string(),
        status,
        tasks: vec![codex_app_server_protocol::WorkflowTask {
            task_id: "task-1".to_string(),
            title: "Test task".to_string(),
            status: codex_app_server_protocol::WorkflowTaskStatus::Running,
            summary: summary.to_string(),
            attempt: 1,
        }],
        completed_tasks: usize::from(status == WorkflowStatus::Completed),
        running_tasks: usize::from(status == WorkflowStatus::Running),
        queued_tasks: 0,
        blocked_tasks: 0,
        failed_tasks: usize::from(status == WorkflowStatus::Failed),
        summary: summary.to_string(),
    }
}

fn test_workflow_json(status: WorkflowStatus, summary: &str) -> serde_json::Value {
    json!({
        "workflow_id": "workflow-1",
        "title": "Test workflow",
        "status": status,
        "tasks": [{
            "task_id": "task-1",
            "title": "Test task",
            "status": "running",
            "summary": summary,
            "attempt": 1,
        }],
        "completed_tasks": usize::from(status == WorkflowStatus::Completed),
        "running_tasks": usize::from(status == WorkflowStatus::Running),
        "queued_tasks": 0,
        "blocked_tasks": 0,
        "failed_tasks": usize::from(status == WorkflowStatus::Failed),
        "summary": summary,
    })
}
