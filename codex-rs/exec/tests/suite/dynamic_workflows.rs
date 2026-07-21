use std::fs;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use assert_cmd::Command;
use core_test_support::responses;
use core_test_support::responses::ResponseMock;
use core_test_support::test_codex_exec::TestCodexExecBuilder;
use core_test_support::test_codex_exec::test_codex_exec;
use predicates::str::contains;
use serde_json::Value;
use serde_json::json;
use tokio::time::sleep;
use tokio::time::timeout;
use wiremock::MockServer;

const OBJECTIVE: &str = "verify the release";
const WORKER_SUMMARY: &str = "workflow complete";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ultracode_waits_for_workflow_completion_and_routes_human_output() -> Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let response_mock = completed_workflow(&server, OBJECTIVE, WORKER_SUMMARY).await;

    workflow_command(&test, &server)
        .arg(format!("ultracode {OBJECTIVE}"))
        .write_stdin("release context\n")
        .assert()
        .success()
        .stdout(format!("{WORKER_SUMMARY}\n"))
        .stderr(contains("workflow"));

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].body_contains_text(OBJECTIVE));
    assert!(requests[0].body_contains_text("release context"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_trigger_is_not_parsed_from_appended_stdin_context() -> Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        assistant_response("ordinary", "ordinary turn complete"),
    )
    .await;

    workflow_command(&test, &server)
        .arg("summarize the supplied context")
        .write_stdin(format!("ultracode {OBJECTIVE}\n"))
        .assert()
        .success()
        .stdout("ordinary turn complete\n");

    assert_eq!(response_mock.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effort_ultracode_and_deep_research_start_workflows() -> Result<()> {
    for (prompt, objective) in [
        ("/effort ultracode verify the release", "verify the release"),
        (
            "/deep-research compare release strategies",
            "compare release strategies",
        ),
    ] {
        let test = test_codex_exec();
        let server = responses::start_mock_server().await;
        let response_mock = completed_workflow(&server, objective, WORKER_SUMMARY).await;

        workflow_command(&test, &server)
            .arg(prompt)
            .assert()
            .success()
            .stdout(format!("{WORKER_SUMMARY}\n"));

        assert_eq!(response_mock.requests().len(), 2);
        assert!(response_mock.requests()[0].body_contains_text(objective));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_workflow_triggers_are_rejected_before_start() -> Result<()> {
    for (prompt, expected_error) in [
        (
            "/effort ultracode",
            "ultracode requires an objective in non-interactive exec",
        ),
        (
            "/deep-research",
            "deep-research requires a topic in non-interactive exec",
        ),
    ] {
        let test = test_codex_exec();
        let server = responses::start_mock_server().await;
        let response_mock =
            responses::mount_sse_once(&server, assistant_response("unexpected", "unexpected turn"))
                .await;

        workflow_command(&test, &server)
            .arg(prompt)
            .assert()
            .code(1)
            .stderr(contains(expected_error));

        assert!(response_mock.requests().is_empty());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_rejects_image_and_output_schema_inputs() -> Result<()> {
    let test = test_codex_exec();
    let image_path = test.home_path().join("input.png");
    fs::write(
        &image_path,
        b"not needed because workflow validation is earlier",
    )?;
    workflow_command(&test, &responses::start_mock_server().await)
        .arg(format!("ultracode {OBJECTIVE}"))
        .arg("--image")
        .arg(&image_path)
        .assert()
        .code(1)
        .stderr(contains(
            "workflow execution cannot be combined with --image",
        ));

    let test = test_codex_exec();
    let schema_path = test.home_path().join("schema.json");
    fs::write(&schema_path, r#"{"type":"object"}"#)?;
    workflow_command(&test, &responses::start_mock_server().await)
        .arg("--output-schema")
        .arg(&schema_path)
        .arg(format!("ultracode {OBJECTIVE}"))
        .assert()
        .code(1)
        .stderr(contains(
            "workflow execution cannot be combined with --output-schema",
        ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_workflow_exits_one_and_ignores_turn_events_in_jsonl() -> Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    responses::mount_sse_sequence(
        &server,
        vec![
            assistant_response("planner", &workflow_plan(OBJECTIVE)),
            responses::sse_failed("worker", "server_error", "worker failed"),
        ],
    )
    .await;

    let assert = workflow_command(&test, &server)
        .arg("--json")
        .arg(format!("ultracode {OBJECTIVE}"))
        .assert()
        .code(1);
    let events = jsonl_events(&assert.get_output().stdout)?;
    let event_types = event_types(&events);

    assert!(event_types.contains(&"workflow.started"));
    assert!(event_types.contains(&"workflow.failed"));
    assert!(
        !event_types
            .iter()
            .any(|event_type| event_type.starts_with("turn."))
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_cancels_target_workflow_and_waits_for_cancelled_event() -> Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(assistant_response("planner", &workflow_plan(OBJECTIVE))),
            responses::sse_response(assistant_response("worker", "too late"))
                .set_delay(Duration::from_secs(30)),
        ],
    )
    .await;
    let command = workflow_command(&test, &server);
    let mut command = std_command(&command);
    command
        .arg("--json")
        .arg(format!("ultracode {OBJECTIVE}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;

    wait_for_request_count(&response_mock, /*expected*/ 2).await?;
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(result, 0);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output()).await??;
    assert_eq!(output.status.code(), Some(1));
    let events = jsonl_events(&output.stdout)?;
    assert!(event_types(&events).contains(&"workflow.cancelled"));
    Ok(())
}

fn workflow_command(test: &TestCodexExecBuilder, server: &MockServer) -> Command {
    let mut command = test.cmd_with_server(server);
    command.args([
        "--skip-git-repo-check",
        "-c",
        "features.dynamic_workflows=true",
        "-c",
        "features.multi_agent=true",
        "-c",
        "features.multi_agent_v2=false",
        "-c",
        "features.responses_websockets=false",
        "-c",
        "features.responses_websockets_v2=false",
    ]);
    command
}

async fn completed_workflow(server: &MockServer, objective: &str, summary: &str) -> ResponseMock {
    responses::mount_sse_sequence(
        server,
        vec![
            assistant_response("planner", &workflow_plan(objective)),
            assistant_response("worker", summary),
        ],
    )
    .await
}

fn workflow_plan(objective: &str) -> String {
    json!({
        "schema_version": 1,
        "title": "Exec workflow",
        "objective": objective,
        "tasks": [{
            "id": "task",
            "title": "Exec task",
            "instructions": "complete the exec workflow",
            "dependencies": [],
            "agent_role": null,
            "intent": "read_only",
            "owned_paths": [],
            "verification": [],
            "max_attempts": 1,
        }],
    })
    .to_string()
}

fn assistant_response(id: &str, message: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_assistant_message(&format!("{id}-message"), message),
        responses::ev_completed(id),
    ])
}

fn jsonl_events(output: &[u8]) -> Result<Vec<Value>> {
    String::from_utf8(output.to_vec())?
        .lines()
        .map(|line| serde_json::from_str(line).with_context(|| format!("invalid JSONL: {line}")))
        .collect()
}

fn event_types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .collect()
}

fn std_command(command: &Command) -> std::process::Command {
    let mut std_command = std::process::Command::new(command.get_program());
    std_command.args(command.get_args());
    if let Some(current_dir) = command.get_current_dir() {
        std_command.current_dir(current_dir);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                std_command.env(key, value);
            }
            None => {
                std_command.env_remove(key);
            }
        }
    }
    std_command
}

async fn wait_for_request_count(response_mock: &ResponseMock, expected: usize) -> Result<()> {
    timeout(Duration::from_secs(20), async {
        while response_mock.requests().len() < expected {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {expected} Responses API requests"))?;
    Ok(())
}
