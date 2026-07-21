use codex_protocol::workflow::WorkflowObjective;
use codex_protocol::workflow::WorkflowPolicy;

use super::repair_prompt;

#[test]
fn repair_prompt_preserves_exact_errors_after_a_maximum_length_objective() {
    let objective = WorkflowObjective::new("o".repeat(16 * 1024)).expect("bounded objective");
    let errors = "task dependent depends on missing task missing";

    let prompt = repair_prompt(&objective, /*policy*/ None, "invalid payload", errors)
        .expect("repair prompt");

    assert!(prompt.contains(errors));
}

#[test]
fn repair_prompt_rejects_validation_errors_over_its_hard_limit() {
    let objective = WorkflowObjective::new("objective").expect("bounded objective");

    let error = repair_prompt(
        &objective,
        /*policy*/ None,
        "invalid payload",
        &"e".repeat(8 * 1024),
    )
    .expect_err("oversized validation errors should fail");

    assert!(error.to_string().contains("validation errors exceed"));
}

#[test]
fn repair_prompt_preserves_deep_research_policy() {
    let objective = WorkflowObjective::new("compare workflow engines").expect("bounded objective");

    let prompt = repair_prompt(
        &objective,
        Some(&WorkflowPolicy::DeepResearch),
        "invalid payload",
        "missing evidence task",
    )
    .expect("repair prompt");

    assert!(prompt.contains("Deep research policy"));
    assert!(prompt.contains("source or repository evidence"));
    assert!(prompt.contains("known unknowns"));
}
