use codex_protocol::workflow::WorkflowObjective;
use codex_protocol::workflow::WorkflowPolicy;
use codex_workflows::WorkflowError;

const PLANNER_PROMPT_MAX_BYTES: usize = 24 * 1024;
const REPAIR_PROMPT_MAX_BYTES: usize = 8 * 1024;

pub(super) fn planner_prompt(
    objective: &WorkflowObjective,
    policy: Option<&WorkflowPolicy>,
) -> String {
    let policy = policy_instructions(policy);
    let prompt = format!(
        "Create a native Codex workflow for this objective:\n\n{}{policy}\n\nReturn only one strict JSON object. Do not use markdown fences or commentary. The object must have schema_version=1, title, objective, and tasks. Every task must have id, title, instructions, dependencies, agent_role, intent (read_only or write), owned_paths, verification, and max_attempts. Dependencies must reference existing task ids and form an acyclic graph. Keep all text concise and bounded.",
        objective.as_str(),
    );
    truncate(prompt, PLANNER_PROMPT_MAX_BYTES)
}

pub(super) fn repair_prompt(
    objective: &WorkflowObjective,
    policy: Option<&WorkflowPolicy>,
    invalid_payload: &str,
    errors: &str,
) -> Result<String, WorkflowError> {
    let policy_section = policy_instructions(policy);
    let error_section = format!(
        "{policy_section}\n\nReturn only a corrected complete JSON object using schema_version=1. Fix these exact errors:\n{errors}"
    );
    if error_section.len() > REPAIR_PROMPT_MAX_BYTES {
        return Err(WorkflowError::InvalidSnapshot(format!(
            "planner validation errors exceed the repair prompt limit of {REPAIR_PROMPT_MAX_BYTES} bytes"
        )));
    }

    let remaining = REPAIR_PROMPT_MAX_BYTES - error_section.len();
    let objective_budget = remaining / 2;
    let payload_budget = remaining - objective_budget;
    let objective_section = truncate(
        format!(
            "Create a corrected native Codex workflow for this original objective:\n\n{}",
            objective.as_str()
        ),
        objective_budget,
    );
    let payload_section = truncate(
        format!("\n\nThe first planner response was invalid:\n{invalid_payload}"),
        payload_budget,
    );
    Ok(format!(
        "{objective_section}{payload_section}{error_section}"
    ))
}

fn policy_instructions(policy: Option<&WorkflowPolicy>) -> String {
    match policy {
        Some(WorkflowPolicy::Ultracode) => "\n\nUltracode policy: decompose the objective aggressively, maximize safe parallelism between independent tasks, preserve dependency and write-ownership constraints, and require explicit verification before completion.".to_string(),
        Some(WorkflowPolicy::DeepResearch) => "\n\nDeep research policy: decompose the topic into independent research dimensions. Prefer read-only tasks. Require source or repository evidence for conclusions. Include tasks that surface disagreements and known unknowns, then synthesize one final report for the initiating conversation.".to_string(),
        None => String::new(),
    }
}

fn truncate(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod tests;
