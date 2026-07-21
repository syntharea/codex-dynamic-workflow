use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::WorkflowDefinition;
use crate::WorkflowLimits;
use crate::WorkflowTaskId;
use crate::WorkflowValidationError;
use crate::model::WORKFLOW_DEFINITION_MAX_BYTES;
use crate::model::WORKFLOW_DEFINITION_MAX_TASKS;
use crate::model::WORKFLOW_ID_MAX_BYTES;
use crate::model::WORKFLOW_OBJECTIVE_MAX_BYTES;
use crate::model::WORKFLOW_SCHEMA_VERSION;
use crate::model::WORKFLOW_TEXT_MAX_BYTES;
use crate::model::WORKFLOW_TITLE_MAX_BYTES;

impl WorkflowDefinition {
    pub fn validate(&self, limits: WorkflowLimits) -> Result<(), WorkflowValidationError> {
        if self.schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(WorkflowValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        validate_text("workflow title", &self.title, WORKFLOW_TITLE_MAX_BYTES)?;
        validate_text(
            "workflow objective",
            &self.objective,
            WORKFLOW_OBJECTIVE_MAX_BYTES,
        )?;
        let max_tasks = limits.max_tasks.min(WORKFLOW_DEFINITION_MAX_TASKS);
        if self.tasks.len() > max_tasks {
            return Err(WorkflowValidationError::TooManyTasks { max_tasks });
        }

        let mut ids = HashSet::with_capacity(self.tasks.len());
        for task in &self.tasks {
            validate_text("workflow task id", task.id.as_str(), WORKFLOW_ID_MAX_BYTES)?;
            validate_text("workflow task title", &task.title, WORKFLOW_TITLE_MAX_BYTES)?;
            if !ids.insert(task.id.clone()) {
                return Err(WorkflowValidationError::DuplicateTaskId(task.id.clone()));
            }
            if task.instructions.len() > limits.max_instruction_bytes.min(WORKFLOW_TEXT_MAX_BYTES) {
                return Err(WorkflowValidationError::InstructionsTooLong {
                    task_id: task.id.clone(),
                    max_bytes: limits.max_instruction_bytes,
                });
            }
            if task.max_attempts == 0 {
                return Err(WorkflowValidationError::InvalidMaxAttempts(task.id.clone()));
            }
            if task.max_attempts > limits.max_task_attempts {
                return Err(WorkflowValidationError::TooManyTaskAttempts {
                    task_id: task.id.clone(),
                    max_attempts: limits.max_task_attempts,
                });
            }
            if let Some(agent_role) = task.agent_role.as_deref() {
                validate_text("workflow agent role", agent_role, WORKFLOW_TITLE_MAX_BYTES)?;
            }
            for owned_path in &task.owned_paths {
                validate_text("workflow owned path", owned_path, WORKFLOW_TEXT_MAX_BYTES)?;
            }
            for verification in &task.verification {
                validate_text(
                    "workflow verification",
                    verification,
                    WORKFLOW_TEXT_MAX_BYTES,
                )?;
            }
        }

        if serde_json::to_vec(self)
            .map_err(|error| WorkflowValidationError::TextTooLong {
                field: "workflow definition serialization",
                max_bytes: error.to_string().len(),
            })?
            .len()
            > WORKFLOW_DEFINITION_MAX_BYTES
        {
            return Err(WorkflowValidationError::DefinitionTooLarge {
                max_bytes: WORKFLOW_DEFINITION_MAX_BYTES,
            });
        }

        for task in &self.tasks {
            for dependency in &task.dependencies {
                if !ids.contains(dependency) {
                    return Err(WorkflowValidationError::MissingDependency {
                        task_id: task.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        if self.topological_order().len() != self.tasks.len() {
            return Err(WorkflowValidationError::DependencyCycle);
        }
        Ok(())
    }

    pub fn topological_order(&self) -> Vec<WorkflowTaskId> {
        let mut indegrees: HashMap<&WorkflowTaskId, usize> = self
            .tasks
            .iter()
            .map(|task| (&task.id, task.dependencies.len()))
            .collect();
        let positions: HashMap<&WorkflowTaskId, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(position, task)| (&task.id, position))
            .collect();
        let mut dependents: HashMap<&WorkflowTaskId, Vec<&WorkflowTaskId>> = HashMap::new();
        for task in &self.tasks {
            for dependency in &task.dependencies {
                dependents.entry(dependency).or_default().push(&task.id);
            }
        }

        let mut ready: BTreeSet<(usize, &WorkflowTaskId)> = self
            .tasks
            .iter()
            .filter(|task| task.dependencies.is_empty())
            .map(|task| (positions[&task.id], &task.id))
            .collect();
        let mut order = Vec::with_capacity(self.tasks.len());
        while let Some((_, id)) = ready.pop_first() {
            order.push(id.clone());
            if let Some(task_dependents) = dependents.get(id) {
                for dependent in task_dependents {
                    let Some(indegree) = indegrees.get_mut(dependent) else {
                        continue;
                    };
                    *indegree -= 1;
                    if *indegree == 0 {
                        ready.insert((positions[dependent], dependent));
                    }
                }
            }
        }
        order
    }
}

fn validate_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), WorkflowValidationError> {
    if value.len() > max_bytes {
        return Err(WorkflowValidationError::TextTooLong { field, max_bytes });
    }
    Ok(())
}
