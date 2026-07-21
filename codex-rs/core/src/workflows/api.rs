use std::sync::Arc;

use anyhow::Result;
use codex_protocol::workflow::WorkflowCancelParams;
use codex_protocol::workflow::WorkflowDeleteSavedParams;
use codex_protocol::workflow::WorkflowPauseParams;
use codex_protocol::workflow::WorkflowReadParams;
use codex_protocol::workflow::WorkflowResumeParams;
use codex_protocol::workflow::WorkflowRetryParams;
use codex_protocol::workflow::WorkflowRunSavedParams;
use codex_protocol::workflow::WorkflowSaveParams;
use codex_protocol::workflow::WorkflowStartParams;
use codex_protocol::workflow::WorkflowSummary;
use uuid::Uuid;

use crate::CodexThread;
use crate::session::session::Session;

use super::events;
use super::runtime;

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowList {
    pub workflows: Vec<WorkflowSummary>,
    pub saved_workflows: Vec<SavedWorkflowSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedWorkflowSummary {
    pub name: String,
    pub title: String,
    pub objective: String,
    pub task_count: usize,
}

#[derive(Clone)]
pub struct WorkflowApi {
    session: Arc<Session>,
}

impl WorkflowApi {
    pub async fn start(&self, params: WorkflowStartParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::start(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn read(&self, params: WorkflowReadParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let snapshot = runtime::control::read(&self.session, &self.manager(), params).await?;
        summary(&snapshot)
    }

    pub async fn list(&self) -> Result<WorkflowList> {
        runtime::ensure_available(&self.session)?;
        let manager = self.manager();
        let snapshots = runtime::control::list(&self.session, &manager).await;
        let workflows = snapshots.iter().map(summary).collect::<Result<Vec<_>>>()?;
        let saved_workflows = runtime::saved::list_saved_workflows(&self.session, &manager)
            .await?
            .into_iter()
            .map(SavedWorkflowSummary::from)
            .collect();
        Ok(WorkflowList {
            workflows,
            saved_workflows,
        })
    }

    pub async fn pause(&self, params: WorkflowPauseParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::control::pause(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn resume(&self, params: WorkflowResumeParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::control::resume(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn cancel(&self, params: WorkflowCancelParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::control::cancel(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn retry(&self, params: WorkflowRetryParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::control::retry(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn save(&self, params: WorkflowSaveParams) -> Result<SavedWorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        runtime::saved::save_workflow(Arc::clone(&self.session), self.manager(), params)
            .await
            .map(SavedWorkflowSummary::from)
    }

    pub async fn run_saved(&self, params: WorkflowRunSavedParams) -> Result<WorkflowSummary> {
        runtime::ensure_available(&self.session)?;
        let submission_id = submission_id();
        let snapshot = runtime::saved::run_saved_workflow(
            Arc::clone(&self.session),
            &submission_id,
            self.manager(),
            params,
        )
        .await?;
        summary(&snapshot)
    }

    pub async fn delete_saved(&self, params: WorkflowDeleteSavedParams) -> Result<bool> {
        runtime::ensure_available(&self.session)?;
        runtime::saved::delete_saved_workflow(Arc::clone(&self.session), self.manager(), params)
            .await
    }

    fn manager(&self) -> Arc<super::WorkflowManager> {
        self.session.services.agent_control.workflow_manager()
    }
}

impl From<codex_workflows::SavedWorkflowDefinition> for SavedWorkflowSummary {
    fn from(saved: codex_workflows::SavedWorkflowDefinition) -> Self {
        Self {
            name: saved.name,
            title: saved.definition.title,
            objective: saved.definition.objective,
            task_count: saved.definition.tasks.len(),
        }
    }
}

pub fn api_for_thread(thread: &CodexThread) -> WorkflowApi {
    WorkflowApi {
        session: Arc::clone(&thread.session),
    }
}

fn summary(snapshot: &codex_workflows::WorkflowSnapshot) -> Result<WorkflowSummary> {
    events::summary(snapshot)
        .map(|(_, summary)| summary)
        .map_err(anyhow::Error::from)
}

fn submission_id() -> String {
    Uuid::now_v7().to_string()
}
