use std::fs;

use anyhow::Result;
use codex_core::workflows::SavedWorkflowSummary;
use codex_core::workflows::api_for_thread;
use codex_core::workflows::manager_for_thread;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::workflow::WorkflowCancelParams;
use codex_protocol::workflow::WorkflowDeleteSavedParams;
use codex_protocol::workflow::WorkflowId as ProtocolWorkflowId;
use codex_protocol::workflow::WorkflowName;
use codex_protocol::workflow::WorkflowPauseParams;
use codex_protocol::workflow::WorkflowReadParams;
use codex_protocol::workflow::WorkflowSaveParams;
use codex_protocol::workflow::WorkflowStatus;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowState;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn synchronous_api_reads_lists_and_controls_workflows() -> Result<()> {
    let server = start_mock_server().await;
    let test = workflow_session(&server).await?;
    let manager = manager_for_thread(&test.codex);
    let workflow_id = manager
        .start(definition("release")?, test.session_configured.thread_id)
        .await?;
    let protocol_id = ProtocolWorkflowId::new(workflow_id.to_string())?;
    let api = api_for_thread(&test.codex);

    let read = api
        .read(WorkflowReadParams {
            workflow_id: protocol_id.clone(),
        })
        .await?;
    assert_eq!(read.workflow_id(), workflow_id.to_string());
    assert_eq!(read.status(), WorkflowStatus::Running);

    let listed = api.list().await?;
    assert_eq!(listed.workflows, vec![read]);
    assert!(listed.saved_workflows.is_empty());

    let pausing = api
        .pause(WorkflowPauseParams {
            workflow_id: protocol_id.clone(),
        })
        .await?;
    assert_eq!(pausing.status(), WorkflowStatus::Pausing);

    let cancelled = api
        .cancel(WorkflowCancelParams {
            workflow_id: protocol_id,
        })
        .await?;
    assert_eq!(cancelled.status(), WorkflowStatus::Cancelled);
    assert_eq!(
        manager.snapshot(workflow_id).await?.state,
        WorkflowState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn synchronous_api_returns_saved_workflow_results() -> Result<()> {
    let server = start_mock_server().await;
    let test = workflow_session(&server).await?;
    let manager = manager_for_thread(&test.codex);
    let workflow_id = manager
        .start(
            definition("saved release")?,
            test.session_configured.thread_id,
        )
        .await?;
    let api = api_for_thread(&test.codex);

    let saved = api
        .save(WorkflowSaveParams {
            workflow_id: ProtocolWorkflowId::new(workflow_id.to_string())?,
            name: WorkflowName::new("release")?,
        })
        .await?;
    assert_eq!(
        saved,
        SavedWorkflowSummary {
            name: "release".to_string(),
            title: "saved release".to_string(),
            objective: "saved release objective".to_string(),
            task_count: 1,
        }
    );

    let listed = api.list().await?;
    assert_eq!(listed.saved_workflows, vec![saved]);

    assert!(
        api.delete_saved(WorkflowDeleteSavedParams {
            name: WorkflowName::new("release")?,
        })
        .await?
    );
    assert!(
        !api.delete_saved(WorkflowDeleteSavedParams {
            name: WorkflowName::new("release")?,
        })
        .await?
    );
    Ok(())
}

async fn workflow_session(
    server: &wiremock::MockServer,
) -> Result<core_test_support::test_codex::TestCodex> {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    test_codex()
        .with_auth(auth)
        .with_config(|config| {
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
            fs::create_dir_all(config.cwd.join(".git")).expect("create git root");
            config.git_checkout_root = Some(config.cwd.clone());
        })
        .build(server)
        .await
}

fn definition(title: &str) -> Result<WorkflowDefinition> {
    Ok(WorkflowDefinition::try_new_with_objective(
        title,
        format!("{title} objective"),
        vec![WorkflowTask {
            id: WorkflowTaskId::from("task"),
            title: "task".to_string(),
            instructions: "execute task".to_string(),
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
