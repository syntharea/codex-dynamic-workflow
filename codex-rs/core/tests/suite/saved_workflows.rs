use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use codex_core::workflows::manager_for_thread;
use codex_core::workflows::saved_workflows_for_thread;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::ThreadId;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::workflow::WorkflowCancelParams;
use codex_protocol::workflow::WorkflowDeleteSavedParams;
use codex_protocol::workflow::WorkflowId as ProtocolWorkflowId;
use codex_protocol::workflow::WorkflowName;
use codex_protocol::workflow::WorkflowReadParams;
use codex_protocol::workflow::WorkflowRunSavedParams;
use codex_protocol::workflow::WorkflowSaveParams;
use codex_protocol::workflow::WorkflowStartParams;
use codex_protocol::workflow::WorkflowTaskOverride;
use codex_protocol::workflow::WorkflowTaskOverrides;
use codex_protocol::workflow::WorkflowText;
use codex_protocol::workflow::WorkflowTitle;
use codex_workflows::SavedWorkflowDefinition;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::matchers::method;

#[tokio::test]
async fn global_save_run_delete_uses_fresh_ids_and_preserves_saved_definition() -> Result<()> {
    let server = start_mock_server().await;
    let workers = mount_sse_sequence(
        &server,
        vec![
            worker_response("overridden-worker", "overridden done"),
            worker_response("original-worker", "original done"),
        ],
    )
    .await;
    let test = workflow_session(&server, ProjectTrust::Unknown).await?;
    let manager = manager_for_thread(&test.codex);
    let source_id = manager
        .start(
            definition("global source", "original instructions")?,
            test.session_configured.thread_id,
        )
        .await?;

    submit_save(&test, &source_id.to_string(), "global").await?;
    submit_read_and_wait(&test, &source_id.to_string()).await?;
    let global_path = test.codex_home_path().join("workflows/global.json");
    let definition_before_run = fs::read(&global_path)?;

    let first_id = run_saved(
        &test,
        "global",
        WorkflowTaskOverrides::try_new(vec![WorkflowTaskOverride::try_new(
            "task",
            Some(WorkflowTitle::new("overridden title")?),
            Some(WorkflowText::new("overridden instructions")?),
        )?])?,
    )
    .await?;
    assert_eq!(fs::read(&global_path)?, definition_before_run);

    let second_id = run_saved(&test, "global", WorkflowTaskOverrides::default()).await?;
    assert_ne!(first_id, second_id);
    assert_eq!(fs::read(&global_path)?, definition_before_run);

    let requests = workers.requests();
    assert_eq!(requests.len(), 2);
    let overridden_request = requests[0].body_json().to_string();
    assert!(overridden_request.contains("overridden instructions"));
    assert!(!overridden_request.contains("original instructions"));
    let original_request = requests[1].body_json().to_string();
    assert!(original_request.contains("original instructions"));

    submit_save(&test, &first_id, "completed-copy").await?;
    submit_read_and_wait(&test, &first_id).await?;
    assert!(
        test.codex_home_path()
            .join("workflows/completed-copy.json")
            .is_file()
    );

    test.codex
        .submit(Op::WorkflowDeleteSaved(WorkflowDeleteSavedParams {
            name: WorkflowName::new("global")?,
        }))
        .await?;
    let error = run_saved_expect_error(&test, "global", WorkflowTaskOverrides::default()).await?;
    assert!(error.contains("not found"), "unexpected error: {error}");
    assert!(!global_path.exists());
    Ok(())
}

#[tokio::test]
async fn project_definition_overrides_global_only_when_project_is_trusted() -> Result<()> {
    let server = start_mock_server().await;

    let trusted = workflow_session(&server, ProjectTrust::Trusted).await?;
    write_saved(
        &trusted.codex_home_path().join("workflows"),
        "shared",
        definition("global", "global instructions")?,
    )?;
    write_saved(
        &trusted.config.cwd.join(".codex/workflows"),
        "shared",
        definition("project", "project instructions")?,
    )?;
    let trusted_saved = saved_workflows_for_thread(&trusted.codex).await?;
    assert_eq!(
        trusted_saved
            .iter()
            .map(|saved| (saved.name.as_str(), saved.definition.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("shared", "project")]
    );
    let trusted_worker = mount_sse_once(
        &server,
        worker_response("trusted-project-worker", "project done"),
    )
    .await;
    run_saved(&trusted, "shared", WorkflowTaskOverrides::default()).await?;
    let trusted_request = trusted_worker.single_request().body_json().to_string();
    assert!(trusted_request.contains("project instructions"));
    assert!(!trusted_request.contains("global instructions"));

    let untrusted = workflow_session(&server, ProjectTrust::Untrusted).await?;
    write_saved(
        &untrusted.codex_home_path().join("workflows"),
        "shared",
        definition("global", "global instructions")?,
    )?;
    write_saved(
        &untrusted.config.cwd.join(".codex/workflows"),
        "shared",
        definition("project", "project instructions")?,
    )?;
    let untrusted_saved = saved_workflows_for_thread(&untrusted.codex).await?;
    assert_eq!(
        untrusted_saved
            .iter()
            .map(|saved| (saved.name.as_str(), saved.definition.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("shared", "global")]
    );
    let untrusted_worker = mount_sse_once(
        &server,
        worker_response("untrusted-global-worker", "global done"),
    )
    .await;
    run_saved(&untrusted, "shared", WorkflowTaskOverrides::default()).await?;
    let untrusted_request = untrusted_worker.single_request().body_json().to_string();
    assert!(untrusted_request.contains("global instructions"));
    assert!(!untrusted_request.contains("project instructions"));
    Ok(())
}

#[tokio::test]
async fn saved_run_rejects_invalid_overrides_and_propagates_store_errors() -> Result<()> {
    let server = start_mock_server().await;
    let test = workflow_session(&server, ProjectTrust::Unknown).await?;
    write_saved(
        &test.codex_home_path().join("workflows"),
        "limits",
        definition("limits", "ok")?,
    )?;
    let manager = manager_for_thread(&test.codex);
    let mut manager_config = manager.config();
    manager_config.max_instruction_bytes = 4;
    manager.configure(manager_config).await?;

    let unknown = run_saved_expect_error(
        &test,
        "limits",
        WorkflowTaskOverrides::try_new(vec![WorkflowTaskOverride::try_new(
            "missing",
            Some(WorkflowTitle::new("missing")?),
            Some(WorkflowText::new("ok")?),
        )?])?,
    )
    .await?;
    assert!(
        unknown.contains("unknown task"),
        "unexpected error: {unknown}"
    );

    let duplicate = run_saved_expect_error(
        &test,
        "limits",
        WorkflowTaskOverrides::try_new(vec![
            WorkflowTaskOverride::try_new(
                "task",
                Some(WorkflowTitle::new("first")?),
                Some(WorkflowText::new("ok")?),
            )?,
            WorkflowTaskOverride::try_new(
                "task",
                Some(WorkflowTitle::new("second")?),
                Some(WorkflowText::new("ok")?),
            )?,
        ])?,
    )
    .await?;
    assert!(
        duplicate.contains("duplicate task"),
        "unexpected error: {duplicate}"
    );

    let over_limit = run_saved_expect_error(
        &test,
        "limits",
        WorkflowTaskOverrides::try_new(vec![WorkflowTaskOverride::try_new(
            "task",
            Some(WorkflowTitle::new("within title limit")?),
            Some(WorkflowText::new("too long")?),
        )?])?,
    )
    .await?;
    assert!(
        over_limit.contains("instruction limit of 4"),
        "unexpected error: {over_limit}"
    );

    let workflows_root = test.codex_home_path().join("workflows");
    fs::write(
        workflows_root.join("future.json"),
        r#"{"schema_version":999}"#,
    )?;
    let schema_error =
        run_saved_expect_error(&test, "future", WorkflowTaskOverrides::default()).await?;
    assert!(
        schema_error.contains("unsupported schema version 999"),
        "unexpected error: {schema_error}"
    );
    Ok(())
}

#[tokio::test]
async fn save_checks_owner_and_rejects_planning_workflows() -> Result<()> {
    let server = start_mock_server().await;
    let _delayed_planner = mount_response_once_match(
        &server,
        method("POST"),
        sse_response(planner_response(
            "delayed-planner",
            r#"{"schema_version":1,"title":"planned","objective":"still planning","tasks":[]}"#,
        ))
        .set_delay(Duration::from_secs(30)),
    )
    .await;
    let test = workflow_session(&server, ProjectTrust::Unknown).await?;
    let manager = manager_for_thread(&test.codex);
    let foreign_id = manager
        .start(
            definition("foreign", "foreign instructions")?,
            ThreadId::new(),
        )
        .await?;

    submit_save(&test, &foreign_id.to_string(), "foreign").await?;
    let owner_error = wait_for_error(&test).await?;
    assert!(
        owner_error.contains("belongs to another root thread"),
        "unexpected error: {owner_error}"
    );

    test.codex
        .submit(Op::WorkflowStart(WorkflowStartParams {
            objective: codex_protocol::workflow::WorkflowObjective::new("still planning")?,
            policy: None,
        }))
        .await?;
    let planning_id = wait_for_started(&test).await?;
    submit_save(&test, &planning_id, "planning").await?;
    let planning_error = wait_for_error(&test).await?;
    assert!(
        planning_error.contains("accepted definition"),
        "unexpected error: {planning_error}"
    );

    test.codex
        .submit(Op::WorkflowCancel(WorkflowCancelParams {
            workflow_id: ProtocolWorkflowId::new(&planning_id)?,
        }))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::WorkflowCancelled(cancelled)
                if cancelled.workflow_id().as_str() == planning_id
        )
    })
    .await;
    submit_save(&test, &planning_id, "cancelled-planning").await?;
    let cancelled_error = tokio::time::timeout(Duration::from_secs(2), wait_for_error(&test))
        .await
        .expect("cancelled planning workflow save must fail")?;
    assert!(
        cancelled_error.contains("accepted definition"),
        "unexpected error: {cancelled_error}"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum ProjectTrust {
    Unknown,
    Trusted,
    Untrusted,
}

async fn workflow_session(
    server: &wiremock::MockServer,
    project_trust: ProjectTrust,
) -> Result<TestCodex> {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    test_codex()
        .with_auth(auth)
        .with_config(move |config| {
            config
                .features
                .enable(Feature::DynamicWorkflows)
                .expect("enable dynamic workflows");
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collab");
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("disable multi-agent v2");
            config.agents_enabled = true;
            config.active_project.trust_level = match project_trust {
                ProjectTrust::Unknown => None,
                ProjectTrust::Trusted => Some(TrustLevel::Trusted),
                ProjectTrust::Untrusted => Some(TrustLevel::Untrusted),
            };
            fs::create_dir_all(config.cwd.join(".git")).expect("create git root");
            config.git_checkout_root = Some(config.cwd.clone());
            config.git_checkout_root_is_trusted = matches!(project_trust, ProjectTrust::Trusted);
        })
        .build(server)
        .await
}

async fn submit_save(test: &TestCodex, workflow_id: &str, name: &str) -> Result<()> {
    test.codex
        .submit(Op::WorkflowSave(WorkflowSaveParams {
            workflow_id: ProtocolWorkflowId::new(workflow_id)?,
            name: WorkflowName::new(name)?,
        }))
        .await?;
    Ok(())
}

async fn submit_read_and_wait(test: &TestCodex, workflow_id: &str) -> Result<()> {
    test.codex
        .submit(Op::WorkflowRead(WorkflowReadParams {
            workflow_id: ProtocolWorkflowId::new(workflow_id)?,
        }))
        .await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::WorkflowProgress(progress)
                if progress.workflow_id().as_str() == workflow_id
        ) || matches!(event, EventMsg::Error(_))
    })
    .await;
    if let EventMsg::Error(error) = event {
        bail!(error.message);
    }
    Ok(())
}

async fn run_saved(
    test: &TestCodex,
    name: &str,
    task_overrides: WorkflowTaskOverrides,
) -> Result<String> {
    test.codex
        .submit(Op::WorkflowRunSaved(WorkflowRunSavedParams {
            name: WorkflowName::new(name)?,
            task_overrides,
        }))
        .await?;
    let workflow_id = wait_for_started(test).await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::WorkflowCompleted(completed)
                if completed.workflow_id().as_str() == workflow_id
        ) || matches!(event, EventMsg::Error(_))
    })
    .await;
    if let EventMsg::Error(error) = event {
        bail!(error.message);
    }
    Ok(workflow_id)
}

async fn run_saved_expect_error(
    test: &TestCodex,
    name: &str,
    task_overrides: WorkflowTaskOverrides,
) -> Result<String> {
    test.codex
        .submit(Op::WorkflowRunSaved(WorkflowRunSavedParams {
            name: WorkflowName::new(name)?,
            task_overrides,
        }))
        .await?;
    wait_for_error(test).await
}

async fn wait_for_started(test: &TestCodex) -> Result<String> {
    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::WorkflowStarted(_) | EventMsg::Error(_))
    })
    .await;
    match event {
        EventMsg::WorkflowStarted(started) => Ok(started.workflow_id().as_str().to_string()),
        EventMsg::Error(error) => bail!(error.message),
        _ => unreachable!("workflow start or error event"),
    }
}

async fn wait_for_error(test: &TestCodex) -> Result<String> {
    let event = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = event else {
        unreachable!("workflow error event")
    };
    Ok(error.message)
}

fn write_saved(root: &Path, name: &str, definition: WorkflowDefinition) -> Result<()> {
    fs::create_dir_all(root)?;
    fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&SavedWorkflowDefinition::new(name, definition))?,
    )?;
    Ok(())
}

fn definition(title: &str, instructions: &str) -> Result<WorkflowDefinition> {
    Ok(WorkflowDefinition::try_new_with_objective(
        title,
        format!("{title} objective"),
        vec![WorkflowTask {
            id: WorkflowTaskId::from("task"),
            title: "task".to_string(),
            instructions: instructions.to_string(),
            dependencies: Vec::new(),
            agent_role: None,
            intent: WorkflowTaskIntent::ReadOnly,
            owned_paths: Vec::new(),
            verification: Vec::new(),
            max_attempts: 1,
        }],
        WorkflowLimits::default(),
    )?)
}

fn planner_response(id: &str, message: &str) -> String {
    sse(vec![
        ev_response_created(id),
        ev_assistant_message(&format!("{id}-message"), message),
        ev_completed(id),
    ])
}

fn worker_response(id: &str, message: &str) -> String {
    planner_response(id, message)
}
