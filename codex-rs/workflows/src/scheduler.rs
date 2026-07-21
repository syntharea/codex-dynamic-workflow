use crate::WorkflowError;
use crate::WorkflowSnapshot;
use crate::WorkflowState;
use crate::WorkflowTask;
use crate::WorkflowTaskId;
use crate::WorkflowTaskIntent;
use crate::WorkflowTaskState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowAction {
    Spawn(WorkflowTaskId),
    Interrupt(WorkflowTaskId),
    Complete(WorkflowState),
    Persist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowScheduler {
    max_concurrent_tasks: usize,
}

impl WorkflowScheduler {
    pub fn new(max_concurrent_tasks: usize) -> Result<Self, WorkflowError> {
        if max_concurrent_tasks == 0 {
            return Err(WorkflowError::InvalidConcurrency);
        }
        Ok(Self {
            max_concurrent_tasks,
        })
    }

    pub fn next_actions(&self, snapshot: &mut WorkflowSnapshot) -> Vec<WorkflowAction> {
        match snapshot.state {
            WorkflowState::Pausing => {
                return interrupt_active_tasks(snapshot, WorkflowTaskState::Queued);
            }
            WorkflowState::Cancelled => {
                let actions = interrupt_active_tasks(snapshot, WorkflowTaskState::Cancelled);
                if actions.is_empty() {
                    return vec![
                        WorkflowAction::Complete(WorkflowState::Cancelled),
                        WorkflowAction::Persist,
                    ];
                }
                return actions;
            }
            WorkflowState::Running => {}
            WorkflowState::Planning
            | WorkflowState::Paused
            | WorkflowState::Consolidating
            | WorkflowState::Completed
            | WorkflowState::Failed => return Vec::new(),
        }

        let mut changed = block_failed_dependencies(snapshot);
        let mut actions = Vec::new();
        let mut active_ids: Vec<WorkflowTaskId> = snapshot
            .tasks
            .iter()
            .filter(|task| task.state.is_active())
            .map(|task| task.task_id.clone())
            .collect();

        for task in snapshot.definition.tasks.clone() {
            if active_ids.len() >= self.max_concurrent_tasks {
                break;
            }
            let Some(task_snapshot) = snapshot.task_by_id(&task.id) else {
                continue;
            };
            if task_snapshot.state != WorkflowTaskState::Queued
                || !dependencies_succeeded(snapshot, &task)
                || has_write_conflict(snapshot, &task, &active_ids)
            {
                continue;
            }

            let Ok(task_snapshot) = snapshot.task_mut(&task.id) else {
                continue;
            };
            let Ok(state) = task_snapshot.state.transition_to(WorkflowTaskState::Ready) else {
                continue;
            };
            task_snapshot.state = state;
            task_snapshot.attempts = task_snapshot.attempts.saturating_add(1);
            active_ids.push(task.id.clone());
            actions.push(WorkflowAction::Spawn(task.id));
            changed = true;
        }

        if actions.is_empty() && !snapshot.tasks.iter().any(|task| task.state.is_active()) {
            let completion = if snapshot
                .tasks
                .iter()
                .all(|task| task.state == WorkflowTaskState::Succeeded)
            {
                Some(WorkflowState::Completed)
            } else if snapshot.tasks.iter().all(|task| task.state.is_terminal()) {
                Some(WorkflowState::Failed)
            } else {
                None
            };
            if let Some(state) = completion
                && let Ok(next_state) = snapshot.state.transition_to(state)
            {
                snapshot.state = next_state;
                actions.push(WorkflowAction::Complete(state));
                changed = true;
            }
        }

        if changed {
            actions.push(WorkflowAction::Persist);
        }
        actions
    }
}

fn block_failed_dependencies(snapshot: &mut WorkflowSnapshot) -> bool {
    let mut changed = false;
    loop {
        let blocked_ids: Vec<WorkflowTaskId> = snapshot
            .definition
            .tasks
            .iter()
            .filter(|task| {
                snapshot.task_by_id(&task.id).is_some_and(|snapshot_task| {
                    snapshot_task.state == WorkflowTaskState::Queued
                        && task.dependencies.iter().any(|dependency| {
                            snapshot.task_by_id(dependency).is_some_and(|dependency| {
                                matches!(
                                    dependency.state,
                                    WorkflowTaskState::Failed
                                        | WorkflowTaskState::Blocked
                                        | WorkflowTaskState::Cancelled
                                )
                            })
                        })
                })
            })
            .map(|task| task.id.clone())
            .collect();
        if blocked_ids.is_empty() {
            break;
        }
        for task_id in blocked_ids {
            if let Ok(task) = snapshot.task_mut(&task_id)
                && let Ok(state) = task.state.transition_to(WorkflowTaskState::Blocked)
            {
                task.state = state;
                changed = true;
            }
        }
    }
    changed
}

fn dependencies_succeeded(snapshot: &WorkflowSnapshot, task: &WorkflowTask) -> bool {
    task.dependencies.iter().all(|dependency| {
        snapshot
            .task_by_id(dependency)
            .is_some_and(|task| task.state == WorkflowTaskState::Succeeded)
    })
}

fn has_write_conflict(
    snapshot: &WorkflowSnapshot,
    candidate: &WorkflowTask,
    active_ids: &[WorkflowTaskId],
) -> bool {
    active_ids.iter().any(|active_id| {
        snapshot
            .definition
            .task(active_id)
            .is_some_and(|active| tasks_conflict(candidate, active))
    })
}

fn tasks_conflict(first: &WorkflowTask, second: &WorkflowTask) -> bool {
    if first.intent == WorkflowTaskIntent::ReadOnly || second.intent == WorkflowTaskIntent::ReadOnly
    {
        return false;
    }
    if first.owned_paths.is_empty() || second.owned_paths.is_empty() {
        return true;
    }
    first.owned_paths.iter().any(|first_path| {
        second
            .owned_paths
            .iter()
            .any(|second_path| paths_overlap(first_path, second_path))
    })
}

fn paths_overlap(first: &str, second: &str) -> bool {
    let first = normalize_path(first);
    let second = normalize_path(second);
    first == "/"
        || second == "/"
        || first == second
        || first
            .strip_prefix(&second)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || second
            .strip_prefix(&first)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn normalize_path(path: &str) -> String {
    let normalized = path
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string();
    if normalized.is_empty() || normalized == "." {
        "/".to_string()
    } else {
        normalized
    }
}

fn interrupt_active_tasks(
    snapshot: &mut WorkflowSnapshot,
    _target_state: WorkflowTaskState,
) -> Vec<WorkflowAction> {
    let active_ids: Vec<WorkflowTaskId> = snapshot
        .tasks
        .iter()
        .filter(|task| task.state.is_active())
        .map(|task| task.task_id.clone())
        .collect();
    let mut actions = Vec::with_capacity(active_ids.len() + 1);
    actions.extend(active_ids.into_iter().map(WorkflowAction::Interrupt));
    if !actions.is_empty() {
        actions.push(WorkflowAction::Persist);
    }
    actions
}
