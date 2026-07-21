use std::collections::BTreeMap;

use codex_workflows::WorkflowAgentId;
use codex_workflows::WorkflowAgentOutcome;
use codex_workflows::WorkflowAgentOutcomeStatus;
use codex_workflows::WorkflowDispatchMarker;
use codex_workflows::WorkflowSnapshot;
use codex_workflows::WorkflowTaskState;

#[derive(Clone)]
pub(crate) struct WorkflowTerminalEvidence {
    marker: WorkflowDispatchMarker,
    agent_id: WorkflowAgentId,
    status: WorkflowTerminalStatus,
}

#[derive(Clone)]
enum WorkflowTerminalStatus {
    Succeeded { summary: String },
    Failed { summary: String, retryable: bool },
}

impl WorkflowTerminalEvidence {
    pub(crate) fn succeeded(
        marker: WorkflowDispatchMarker,
        agent_id: WorkflowAgentId,
        summary: String,
    ) -> Self {
        Self {
            marker,
            agent_id,
            status: WorkflowTerminalStatus::Succeeded { summary },
        }
    }

    pub(crate) fn failed(
        marker: WorkflowDispatchMarker,
        agent_id: WorkflowAgentId,
        summary: String,
        retryable: bool,
    ) -> Self {
        Self {
            marker,
            agent_id,
            status: WorkflowTerminalStatus::Failed { summary, retryable },
        }
    }
}

pub(super) fn trusted_terminal_outcomes(
    snapshot: &WorkflowSnapshot,
    evidence: &[WorkflowTerminalEvidence],
) -> Vec<WorkflowAgentOutcome> {
    let snapshot_agent_counts = counts(
        snapshot
            .tasks
            .iter()
            .filter_map(|task| task.agent_id.as_ref()),
    );
    let evidence_agent_counts = counts(evidence.iter().map(|item| &item.agent_id));
    let evidence_task_counts = counts(evidence.iter().map(|item| item.marker.task_id()));

    evidence
        .iter()
        .filter_map(|item| {
            let marker = &item.marker;
            if marker.workflow_id() != snapshot.workflow_id
                || marker.execution_attempt() != snapshot.execution_attempt
                || marker.agent_id() != Some(&item.agent_id)
                || snapshot_agent_counts.get(&item.agent_id) != Some(&1)
                || evidence_agent_counts.get(&item.agent_id) != Some(&1)
                || evidence_task_counts.get(marker.task_id()) != Some(&1)
            {
                return None;
            }
            let task = snapshot.task_by_id(marker.task_id())?;
            if !matches!(
                task.state,
                WorkflowTaskState::Ready | WorkflowTaskState::Running
            ) || task.attempts != marker.task_attempt()
                || task.agent_id.as_ref() != Some(&item.agent_id)
            {
                return None;
            }
            let status = match &item.status {
                WorkflowTerminalStatus::Succeeded { summary } => {
                    WorkflowAgentOutcomeStatus::Succeeded {
                        summary: summary.clone(),
                    }
                }
                WorkflowTerminalStatus::Failed { summary, retryable } => {
                    WorkflowAgentOutcomeStatus::Failed {
                        summary: summary.clone(),
                        retryable: *retryable,
                    }
                }
            };
            Some(WorkflowAgentOutcome {
                agent_id: item.agent_id.clone(),
                task_id: marker.task_id().clone(),
                status,
            })
        })
        .collect()
}

fn counts<'a, T>(values: impl Iterator<Item = &'a T>) -> BTreeMap<T, usize>
where
    T: Clone + Ord + 'a,
{
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value.clone()).or_default() += 1;
    }
    counts
}
