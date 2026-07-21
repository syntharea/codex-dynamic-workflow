use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::workflow::WorkflowDeleteSavedParams;
use codex_protocol::workflow::WorkflowRunSavedParams;
use codex_protocol::workflow::WorkflowSaveParams;
use codex_protocol::workflow::WorkflowTaskOverrides;
use codex_workflows::SavedWorkflowDefinition;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowError;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowSnapshot;

use crate::session::session::Session;

use super::WorkflowManager;
use super::commit_or_fail;
use super::events;
use super::parse_workflow_id;
use super::spawn_drive;
use super::workflow_adapter;
use crate::workflows::saved::SavedWorkflowStore;
use crate::workflows::saved::SavedWorkflowStoreError;

pub(in crate::workflows) async fn save_workflow(
    session: Arc<Session>,
    manager: Arc<WorkflowManager>,
    params: WorkflowSaveParams,
) -> Result<SavedWorkflowDefinition> {
    let workflow_id = parse_workflow_id(params.workflow_id.as_str())?;
    manager
        .ensure_owner(workflow_id, session.thread_id())
        .await?;
    let snapshot = manager.snapshot(workflow_id).await?;
    if !snapshot.is_definition_accepted() {
        return Err(WorkflowError::InvalidSnapshot(format!(
            "workflow {workflow_id} has no accepted definition and cannot be saved"
        ))
        .into());
    }
    let limits = manager.config().workflow_limits();
    snapshot
        .definition
        .validate(limits)
        .map_err(WorkflowError::from)?;
    let saved = SavedWorkflowDefinition::new(params.name.as_str(), snapshot.definition);
    let store = saved_workflow_store(&session, limits).await?;
    let saved_result = saved.clone();
    saved_store_io(move || store.save(&saved)).await?;
    Ok(saved_result)
}

pub(in crate::workflows) async fn run_saved_workflow(
    session: Arc<Session>,
    submission_id: &str,
    manager: Arc<WorkflowManager>,
    params: WorkflowRunSavedParams,
) -> Result<WorkflowSnapshot> {
    let limits = manager.config().workflow_limits();
    let store = saved_workflow_store(&session, limits).await?;
    let name = params.name.into_inner();
    let read_name = name.clone();
    let saved = saved_store_io(move || store.read(&read_name))
        .await?
        .ok_or_else(|| {
            WorkflowError::InvalidSnapshot(format!("saved workflow {name:?} not found"))
        })?;
    let definition = apply_task_overrides(saved.definition, &params.task_overrides, limits)?;
    let workflow_id = manager.start(definition, session.thread_id()).await?;
    let snapshot = manager.snapshot(workflow_id).await?;
    let adapter = workflow_adapter(&session).await;
    commit_or_fail(
        &session,
        submission_id,
        &manager,
        workflow_id,
        adapter,
        snapshot.clone(),
        events::started(&snapshot).map_err(anyhow::Error::from),
    )
    .await?;
    spawn_drive(session, submission_id.to_string(), manager, workflow_id).await;
    Ok(snapshot)
}

pub(in crate::workflows) async fn delete_saved_workflow(
    session: Arc<Session>,
    manager: Arc<WorkflowManager>,
    params: WorkflowDeleteSavedParams,
) -> Result<bool> {
    let store = saved_workflow_store(&session, manager.config().workflow_limits()).await?;
    let name = params.name.into_inner();
    saved_store_io(move || store.delete(&name)).await
}

pub(in crate::workflows) async fn list_saved_workflows(
    session: &Session,
    manager: &WorkflowManager,
) -> Result<Vec<SavedWorkflowDefinition>> {
    let store = saved_workflow_store(session, manager.config().workflow_limits()).await?;
    saved_store_io(move || store.list()).await
}

fn apply_task_overrides(
    mut definition: WorkflowDefinition,
    task_overrides: &WorkflowTaskOverrides,
    limits: WorkflowLimits,
) -> Result<WorkflowDefinition> {
    let mut overridden_task_ids = HashSet::with_capacity(task_overrides.len());
    for task_override in task_overrides.as_slice() {
        let task_id = task_override.task_id();
        if !overridden_task_ids.insert(task_id) {
            return Err(WorkflowError::InvalidSnapshot(format!(
                "duplicate task override for workflow task {task_id:?}"
            ))
            .into());
        }
        let task = definition
            .tasks
            .iter_mut()
            .find(|task| task.id.as_str() == task_id)
            .ok_or_else(|| {
                WorkflowError::InvalidSnapshot(format!(
                    "unknown task id {task_id:?} in saved workflow override"
                ))
            })?;
        if let Some(title) = task_override.title() {
            task.title = title.as_str().to_string();
        }
        if let Some(instructions) = task_override.instructions() {
            task.instructions = instructions.as_str().to_string();
        }
    }
    definition.validate(limits).map_err(WorkflowError::from)?;
    Ok(definition)
}

async fn saved_workflow_store(
    session: &Session,
    limits: WorkflowLimits,
) -> Result<SavedWorkflowStore> {
    let config = session.get_config().await;
    SavedWorkflowStore::from_config(config.as_ref(), limits).map_err(anyhow::Error::from)
}

async fn saved_store_io<T>(
    operation: impl FnOnce() -> std::result::Result<T, SavedWorkflowStoreError> + Send + 'static,
) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("saved workflow I/O task panicked")?
        .map_err(anyhow::Error::from)
}
