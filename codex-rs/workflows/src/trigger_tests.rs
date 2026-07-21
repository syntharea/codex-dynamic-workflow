use pretty_assertions::assert_eq;

use crate::WorkflowTrigger;
use crate::parse_workflow_trigger;

#[test]
fn parses_case_insensitive_standalone_ultracode() {
    assert_eq!(
        parse_workflow_trigger("ULTRACODE fix authentication"),
        Some(WorkflowTrigger::Ultracode {
            objective: "fix authentication".to_string(),
        })
    );
}

#[test]
fn removes_punctuated_ultracode_from_objective() {
    assert_eq!(
        parse_workflow_trigger("Fix auth, ultracode."),
        Some(WorkflowTrigger::Ultracode {
            objective: "Fix auth.".to_string(),
        })
    );
}

#[test]
fn removes_trigger_punctuation_after_ultracode() {
    assert_eq!(
        parse_workflow_trigger("ultracode: fix authentication"),
        Some(WorkflowTrigger::Ultracode {
            objective: "fix authentication".to_string(),
        })
    );
}

#[test]
fn parses_ultracode_outside_fenced_code() {
    assert_eq!(
        parse_workflow_trigger("Fix auth, ultracode.\n```text\nultracode\n```"),
        Some(WorkflowTrigger::Ultracode {
            objective: "Fix auth.\n```text\nultracode\n```".to_string(),
        })
    );
}

#[test]
fn ignores_ultracode_inside_fenced_code() {
    assert_eq!(
        parse_workflow_trigger("Explain this:\n```text\nultracode\n```"),
        None
    );
}

#[test]
fn matches_markdown_fence_character_and_opener_length() {
    assert_eq!(
        parse_workflow_trigger(
            "Explain this:\n````text\n```\nultracode\n~~~~\nstill fenced\n````\n"
        ),
        None
    );
}

#[test]
fn does_not_match_ultracode_inside_another_word() {
    assert_eq!(parse_workflow_trigger("Explain ultracoder behavior"), None);
}

#[test]
fn does_not_match_ultracode_before_a_combining_mark() {
    assert_eq!(
        parse_workflow_trigger("Explain ultracode\u{301} behavior"),
        None
    );
}

#[test]
fn does_not_match_ultracode_before_an_arabic_combining_mark() {
    assert_eq!(parse_workflow_trigger("ultracode\u{0651}"), None);
}

#[test]
fn parses_effort_ultracode_with_objective() {
    assert_eq!(
        parse_workflow_trigger("/effort ultracode fix authentication"),
        Some(WorkflowTrigger::Ultracode {
            objective: "fix authentication".to_string(),
        })
    );
}

#[test]
fn parses_pending_effort_ultracode() {
    assert_eq!(
        parse_workflow_trigger("/effort ultracode"),
        Some(WorkflowTrigger::PendingUltracode)
    );
}

#[test]
fn parses_deep_research_topic() {
    assert_eq!(
        parse_workflow_trigger("/deep-research compare schedulers"),
        Some(WorkflowTrigger::DeepResearch {
            topic: "compare schedulers".to_string(),
        })
    );
}

#[test]
fn parses_pending_deep_research() {
    assert_eq!(
        parse_workflow_trigger("/deep-research"),
        Some(WorkflowTrigger::PendingDeepResearch)
    );
}

#[test]
fn ignores_ordinary_prompts() {
    assert_eq!(parse_workflow_trigger("Fix authentication"), None);
}
