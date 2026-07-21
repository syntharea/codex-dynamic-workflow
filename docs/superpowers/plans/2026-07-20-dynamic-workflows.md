# Dynamic Workflows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Claude Code-compatible Dynamic Workflows commands and user experience to Codex using a native Rust DAG scheduler and the existing subagent runtime.

**Architecture:** A new `codex-workflows` domain crate owns trigger parsing, validated workflow graphs, state transitions, scheduling, retries, snapshots, and saved declarations. `codex-core` adapts that engine to `AgentControl`, rollout persistence, and model planning; protocol, app-server, TUI, and exec expose the same lifecycle without duplicating orchestration logic.

**Tech Stack:** Rust 2024, Tokio, Serde, thiserror, Codex protocol/app-server v2, existing multi-agent `AgentControl`, rollout JSONL, ratatui, insta.

## Global Constraints

- Match Claude Code command semantics for `ultracode`, `/effort ultracode`, `/workflows`, and `/deep-research`.
- Do not execute JavaScript or add an embedded scripting runtime.
- Preserve existing approval, sandbox, model-provider, multi-agent depth, and concurrency enforcement.
- Support Linux, macOS, and Windows.
- Keep `codex-workflows` independent of `codex-core`.
- Keep all model-visible fragments bounded and below repository context-size limits.
- Add only app-server v2 APIs; do not change v1.
- Preserve old rollout deserialization and thread-history projection semantics.
- Do not overwrite the existing uncommitted concurrency or TUI changes.
- Do not commit or create branches unless the user explicitly requests it.
- After Rust changes, run scoped tests, then `just fix -p <crate>` where applicable, and finish with `just fmt` without rerunning tests afterward.

---

### Task 1: Native Workflow Domain Crate

**Files:**
- Modify: `codex-rs/Cargo.toml`
- Modify: `codex-rs/Cargo.lock`
- Create: `codex-rs/workflows/Cargo.toml`
- Create: `codex-rs/workflows/BUILD.bazel`
- Create: `codex-rs/workflows/src/lib.rs`
- Create: `codex-rs/workflows/src/id.rs`
- Create: `codex-rs/workflows/src/model.rs`
- Create: `codex-rs/workflows/src/trigger.rs`
- Create: `codex-rs/workflows/src/graph.rs`
- Create: `codex-rs/workflows/src/scheduler.rs`
- Create: `codex-rs/workflows/src/executor.rs`
- Create: `codex-rs/workflows/src/snapshot.rs`
- Create: `codex-rs/workflows/src/trigger_tests.rs`
- Create: `codex-rs/workflows/src/graph_tests.rs`
- Create: `codex-rs/workflows/src/scheduler_tests.rs`
- Create: `codex-rs/workflows/src/snapshot_tests.rs`

**Interfaces:**
- Produces: `WorkflowId`, `WorkflowTaskId`, `WorkflowDefinition`, `WorkflowTask`, `WorkflowState`, `WorkflowTaskState`, `WorkflowSnapshot`, `WorkflowScheduler`, `WorkflowAction`, `WorkflowAgentControl`, `parse_workflow_trigger`.
- Consumes: only workspace `serde`, `serde_json`, `thiserror`, `uuid`, and `pretty_assertions` dependencies.

- [ ] **Step 1: Register the crate and its dependencies**

Add `"workflows"` to workspace members and:

```toml
codex-workflows = { path = "workflows" }
```

Create a crate manifest with `serde`, `serde_json`, `thiserror`, and `uuid`, plus `pretty_assertions` as a dev dependency. Add a `rust_library` target using the repository's `codex_rust_crate` BUILD pattern.

- [ ] **Step 2: Write failing trigger parser tests**

Cover standalone case-insensitive `ultracode`, punctuation, removal from the objective, no match inside fenced code, `/effort ultracode <task>`, pending `/effort ultracode`, `/deep-research <topic>`, and ordinary prompts.

```rust
#[test]
fn parses_ultracode_outside_fenced_code() {
    assert_eq!(
        parse_workflow_trigger("Fix auth, ultracode.\n```text\nultracode\n```"),
        Some(WorkflowTrigger::Ultracode {
            objective: "Fix auth.\n```text\nultracode\n```".to_string(),
        })
    );
}
```

- [ ] **Step 3: Run trigger tests and confirm RED**

Run: `cd codex-rs && cargo test -p codex-workflows trigger_tests`

Expected: compilation fails because trigger types and parser do not exist.

- [ ] **Step 4: Implement trigger parsing**

Expose:

```rust
pub enum WorkflowTrigger {
    Ultracode { objective: String },
    PendingUltracode,
    DeepResearch { topic: String },
}

pub fn parse_workflow_trigger(input: &str) -> Option<WorkflowTrigger>;
```

Use a line scanner that tracks fenced code blocks and recognizes Unicode word boundaries without interpreting quoted workflow scripts.

- [ ] **Step 5: Write failing graph and transition tests**

Test unique IDs, missing dependencies, cycles, task-count limits, instruction-size limits, valid topological order, exhaustive workflow transitions, task transitions, retry exhaustion, independent-branch continuation, and write ownership conflicts.

```rust
#[test]
fn rejects_a_dependency_cycle() {
    let error = WorkflowDefinition::try_new(
        "cycle",
        vec![task("a", &["b"]), task("b", &["a"])],
        WorkflowLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error, WorkflowValidationError::DependencyCycle);
}
```

- [ ] **Step 6: Run graph tests and confirm RED**

Run: `cd codex-rs && cargo test -p codex-workflows graph_tests scheduler_tests`

Expected: compilation fails because domain and scheduler types do not exist.

- [ ] **Step 7: Implement domain types and scheduler**

Define opaque IDs and these stable shapes:

```rust
pub struct WorkflowDefinition {
    pub schema_version: u32,
    pub title: String,
    pub objective: String,
    pub tasks: Vec<WorkflowTask>,
}

pub struct WorkflowTask {
    pub id: WorkflowTaskId,
    pub title: String,
    pub instructions: String,
    pub dependencies: Vec<WorkflowTaskId>,
    pub agent_role: Option<String>,
    pub intent: WorkflowTaskIntent,
    pub owned_paths: Vec<String>,
    pub verification: Vec<String>,
    pub max_attempts: u8,
}

pub enum WorkflowAction {
    Spawn(WorkflowTaskId),
    Interrupt(WorkflowTaskId),
    Complete(WorkflowState),
    Persist,
}
```

`WorkflowScheduler::next_actions` must be deterministic, concurrency-bounded, dependency-aware, and conservative about concurrent unknown writes.

Define the executor boundary in the domain crate so core can implement it without reversing dependencies:

```rust
pub trait WorkflowAgentControl: Send + Sync {
    fn spawn(
        &self,
        request: WorkflowAgentRequest,
    ) -> impl Future<Output = Result<WorkflowAgentId, WorkflowError>> + Send;

    fn wait(
        &self,
        agents: Vec<WorkflowAgentId>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<WorkflowAgentOutcome>, WorkflowError>> + Send;

    fn interrupt(
        &self,
        agent: WorkflowAgentId,
    ) -> impl Future<Output = Result<(), WorkflowError>> + Send;
}
```

- [ ] **Step 8: Write snapshot round-trip tests**

Verify JSON round trips, unknown optional fields, `schema_version`, bounded summary truncation, and recovery of `Running` tasks as `Queued` unless a terminal agent outcome is supplied.

- [ ] **Step 9: Implement snapshots and saved declarations**

Expose:

```rust
pub struct WorkflowSnapshot {
    pub schema_version: u32,
    pub workflow_id: WorkflowId,
    pub definition: WorkflowDefinition,
    pub state: WorkflowState,
    pub tasks: Vec<WorkflowTaskSnapshot>,
}

pub struct SavedWorkflowDefinition {
    pub schema_version: u32,
    pub name: String,
    pub definition: WorkflowDefinition,
}
```

- [ ] **Step 10: Run the domain crate tests**

Run: `cd codex-rs && just test -p codex-workflows`

Expected: all trigger, graph, scheduler, state, and snapshot tests pass.

### Task 2: Configuration and Feature Gate

**Files:**
- Modify: `codex-rs/config/src/config_toml.rs`
- Modify: `codex-rs/config/src/schema.rs`
- Modify: `codex-rs/core/src/config/mod.rs`
- Modify: `codex-rs/core/src/config/config_tests.rs`
- Modify: `codex-rs/features/src/lib.rs`
- Modify: `codex-rs/core/config.schema.json`

**Interfaces:**
- Consumes: `codex_workflows::WorkflowLimits`.
- Produces: `WorkflowsToml`, effective `WorkflowConfig`, and `Feature::DynamicWorkflows`.

- [ ] **Step 1: Add failing configuration tests**

Parse the documented `[workflows]` table, verify defaults, reject zero limits, cap workflow concurrency by agent concurrency, and preserve the existing dynamic multi-agent default modifications already present in the worktree.

```rust
#[test]
fn workflow_config_defaults_are_bounded() {
    let config = load_config_toml("[workflows]\nmax_concurrent_tasks = 8");
    assert_eq!(config.workflows.max_concurrent_tasks, 8);
    assert_eq!(config.workflows.max_tasks_per_workflow, 256);
}
```

- [ ] **Step 2: Run configuration tests and confirm RED**

Run: `cd codex-rs && just test -p codex-config && just test -p codex-core 'config::tests::workflow'`

Expected: tests fail because workflow configuration fields do not exist.

- [ ] **Step 3: Implement configuration types**

Add:

```rust
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsToml {
    pub max_concurrent_workflows: Option<usize>,
    pub max_tasks_per_workflow: Option<usize>,
    pub max_concurrent_tasks: Option<usize>,
    pub max_task_attempts: Option<u8>,
    pub pause_timeout_seconds: Option<u64>,
}
```

Map it to an effective core `WorkflowConfig` with defaults `4`, `256`, `16`, `2`, and `30`. Register `Feature::DynamicWorkflows` as experimental and enabled for this fork's intended default experience.

- [ ] **Step 4: Regenerate and verify the config schema**

Run: `cd codex-rs && just write-config-schema`

Expected: `codex-rs/core/config.schema.json` contains the `workflows` object and its bounded integer fields.

- [ ] **Step 5: Run scoped tests**

Run: `cd codex-rs && just test -p codex-config && just test -p codex-core 'config::tests::workflow'`

Expected: all configuration and schema tests pass.

### Task 3: Protocol and Rollout Lifecycle

**Files:**
- Modify: `codex-rs/protocol/src/protocol.rs`
- Modify: `codex-rs/protocol/src/items.rs`
- Modify: `codex-rs/core/src/session/rollout_reconstruction.rs`
- Modify: `codex-rs/rollout/src/recorder_tests.rs`
- Create: `codex-rs/protocol/src/workflow.rs`
- Create: `codex-rs/protocol/src/workflow_tests.rs`

**Interfaces:**
- Consumes: serializable snapshots from `codex-workflows` through core conversion.
- Produces: workflow `Op` variants, `EventMsg` variants, `WorkflowStateItem`, and non-model-visible `RolloutItem::WorkflowState`.

- [ ] **Step 1: Write failing serialization compatibility tests**

Test all workflow lifecycle events, operation requests, old rollout decoding, new rollout round trips, unknown optional fields, and ensure workflow snapshots do not become `TurnItem` model context.

```rust
#[test]
fn workflow_state_rollout_item_round_trips() {
    let item = RolloutItem::WorkflowState(WorkflowStateItem {
        schema_version: 1,
        workflow_id: "wf_123".to_string(),
        snapshot: serde_json::json!({"state": "running"}),
    });
    assert_eq!(serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap(), item);
}
```

- [ ] **Step 2: Run protocol tests and confirm RED**

Run: `cd codex-rs && just test -p codex-protocol workflow`

Expected: compilation fails because workflow wire types do not exist.

- [ ] **Step 3: Implement protocol operations and events**

Add operation variants for start, read, list, pause, resume, cancel, retry, save, run-saved, and delete-saved. Add lifecycle events carrying bounded summaries rather than full task output.

```rust
pub struct WorkflowProgressEvent {
    pub workflow_id: String,
    pub state: WorkflowStatus,
    pub completed_tasks: usize,
    pub running_tasks: usize,
    pub queued_tasks: usize,
    pub blocked_tasks: usize,
    pub failed_tasks: usize,
    pub summary: Option<String>,
}
```

Add `RolloutItem::WorkflowState(WorkflowStateItem)` and explicitly exclude it from model-visible conversation item reconstruction.

- [ ] **Step 4: Extend exhaustive mappings and rollout reconstruction**

Update every exhaustive `Op`, `EventMsg`, and `RolloutItem` match. Restore workflow snapshots separately from turn history so existing `Thread.turns` semantics do not change.

- [ ] **Step 5: Run protocol and rollout tests**

Run: `cd codex-rs && just test -p codex-protocol && just test -p codex-rollout`

Expected: all protocol serialization, old-rollout compatibility, and recorder tests pass.

### Task 4: Core Workflow Runtime and Agent Adapter

**Files:**
- Modify: `codex-rs/core/Cargo.toml`
- Modify: `codex-rs/core/BUILD.bazel`
- Modify: `codex-rs/core/src/lib.rs`
- Modify: `codex-rs/core/src/agent/control.rs`
- Modify: `codex-rs/core/src/session/session.rs`
- Modify: `codex-rs/core/src/session/mod.rs`
- Create: `codex-rs/core/src/workflows/mod.rs`
- Create: `codex-rs/core/src/workflows/agent_control.rs`
- Create: `codex-rs/core/src/workflows/manager.rs`
- Create: `codex-rs/core/src/workflows/planner.rs`
- Create: `codex-rs/core/src/workflows/prompts.rs`
- Create: `codex-rs/core/src/workflows/persistence.rs`
- Create: `codex-rs/core/src/workflows/saved.rs`
- Create: `codex-rs/core/tests/suite/dynamic_workflows.rs`
- Modify: `codex-rs/core/tests/suite/mod.rs`

**Interfaces:**
- Consumes: `WorkflowDefinition`, scheduler actions, workflow protocol operations, existing `AgentControl` methods.
- Produces: `WorkflowManager`, `WorkflowAgentControl` adapter, planner repair loop, persistence callbacks, lifecycle events.

- [ ] **Step 1: Write failing core integration tests**

Use `TestCodexBuilder::build_with_auto_env()` and mocked Responses SSE to cover:

- valid planner graph starts two independent subagents concurrently;
- dependent task waits for its prerequisite;
- invalid graph receives one repair request and then succeeds;
- second invalid graph fails without spawning agents;
- worker failure retries once and blocks dependents after exhaustion;
- pause stops new dispatch; resume restarts dispatch;
- cancel interrupts running agents and cancels queued tasks;
- approval and sandbox settings are inherited;
- progress events contain counts and bounded summaries.
- the main thread accepts a second ordinary user turn while a background workflow remains running.

Hold all response mocks and assert outbound planner and worker requests through structured `ResponsesRequest` helpers.

- [ ] **Step 2: Run integration tests and confirm RED**

Run: `cd codex-rs && just test -p codex-core dynamic_workflows`

Expected: compilation fails because `WorkflowManager` and operations do not exist.

- [ ] **Step 3: Define the core execution adapter**

Implement the domain crate's `WorkflowAgentControl` trait without `async_trait`. Delegate to existing spawn, status subscription, interrupt, and close paths. Preserve existing capacity reservation and agent-depth enforcement.

- [ ] **Step 4: Implement `WorkflowManager`**

Use an `Arc<RwLock<BTreeMap<WorkflowId, WorkflowRun>>>` for registry state and per-workflow cancellation signals. `start` must validate the plan before registering runnable tasks; `drive` repeatedly consumes deterministic scheduler actions and emits protocol events.

```rust
pub struct WorkflowManager {
    config: WorkflowConfig,
    runs: RwLock<BTreeMap<WorkflowId, WorkflowRun>>,
    event_tx: broadcast::Sender<WorkflowEvent>,
}
```

Do not store `AgentControl` inside the manager. Pass the adapter to execution methods to avoid a reference cycle.

- [ ] **Step 5: Implement the planner and one-shot repair**

Create a bounded planner prompt requesting the versioned graph contract. Parse only the structured JSON payload. On validation failure, send the exact validation errors once for repair; fail after the second invalid result.

- [ ] **Step 6: Connect session operations and event persistence**

Handle workflow operations in the core session dispatch path. Persist a snapshot after each externally visible transition and emit progress only when counts/state/summary change.

- [ ] **Step 7: Run core workflow tests**

Run: `cd codex-rs && just test -p codex-core dynamic_workflows`

Expected: planner, dispatch, retry, pause, cancellation, and event tests pass.

### Task 5: Resume and Saved Workflow Definitions

**Files:**
- Modify: `codex-rs/core/src/thread_manager.rs`
- Modify: `codex-rs/core/src/agent/control/spawn.rs`
- Modify: `codex-rs/core/src/workflows/persistence.rs`
- Modify: `codex-rs/core/src/workflows/saved.rs`
- Modify: `codex-rs/core/tests/suite/multi_agent_resume.rs`
- Create: `codex-rs/core/tests/suite/workflow_resume.rs`
- Create: `codex-rs/core/tests/suite/saved_workflows.rs`

**Interfaces:**
- Consumes: `InitialHistory::Resumed`, workflow snapshots, Codex home, trusted project status.
- Produces: restored workflow registry and declarative global/project workflow stores.

- [ ] **Step 1: Write failing cold-resume tests**

Verify that completed tasks stay completed, unconfirmed running tasks return to queued, closed descendant agents are not revived, retry counters survive, and the first post-resume dispatch respects dependencies and concurrency.

- [ ] **Step 2: Write failing saved-definition tests**

Cover global save/list/run/delete, repository override precedence, rejection of untrusted project-local definitions, atomic writes, invalid schema versions, and creation of a fresh workflow ID on every run.

- [ ] **Step 3: Run resume and saved tests and confirm RED**

Run: `cd codex-rs && just test -p codex-core workflow_resume saved_workflows`

Expected: tests fail because restore and saved stores are not connected.

- [ ] **Step 4: Restore workflow snapshots during thread resume**

In `ThreadManager::resume_thread_with_history`, extract `WorkflowState` rollout items after agent metadata restoration and before accepting the first follow-up operation. Reconcile terminal descendant status before queueing recovery work.

- [ ] **Step 5: Implement declarative stores**

Use versioned JSON under `$CODEX_HOME/workflows/<name>.json` and trusted project `.codex/workflows/<name>.json`. Sanitize names to a conservative ASCII identifier, write through a temporary file plus atomic rename, and never follow a target path outside the selected workflows directory.

- [ ] **Step 6: Run resume and saved workflow tests**

Run: `cd codex-rs && just test -p codex-core workflow_resume saved_workflows multi_agent_resume`

Expected: all workflow and existing multi-agent resume tests pass.

### Task 6: App-Server v2 Workflow API

**Files:**
- Modify: `codex-rs/app-server-protocol/src/protocol/v2/mod.rs`
- Create: `codex-rs/app-server-protocol/src/protocol/v2/workflow.rs`
- Modify: `codex-rs/app-server-protocol/src/protocol/common.rs`
- Modify: `codex-rs/app-server-protocol/src/protocol/event_mapping.rs`
- Modify: `codex-rs/app-server/src/message_processor.rs`
- Modify: `codex-rs/app-server/src/request_processors/mod.rs`
- Create: `codex-rs/app-server/src/request_processors/workflow_processor.rs`
- Modify: `codex-rs/app-server/src/bespoke_event_handling.rs`
- Create: `codex-rs/app-server/tests/suite/v2/workflow.rs`
- Modify: `codex-rs/app-server/tests/suite/v2/mod.rs`
- Modify: generated app-server schema fixtures

**Interfaces:**
- Consumes: core workflow operations and lifecycle events.
- Produces: v2 `workflow/start`, `workflow/read`, `workflow/list`, `workflow/pause`, `workflow/resume`, `workflow/cancel`, `workflow/retry`, `workflow/save`, `workflow/run`, and `workflow/delete` APIs plus lifecycle notifications.

- [ ] **Step 1: Write failing public JSON-RPC tests**

Use `TestAppServer::new_with_auto_env()` and cover all methods, camelCase wire names, nullable optional request fields, cursor pagination for list, lifecycle notifications, error codes for invalid transitions, and thread resume visibility.

- [ ] **Step 2: Run protocol and app-server tests and confirm RED**

Run: `cd codex-rs && just test -p codex-app-server-protocol workflow && just test -p codex-app-server workflow`

Expected: compilation fails because workflow v2 types and request variants do not exist.

- [ ] **Step 3: Define v2 workflow payloads**

Use `#[serde(rename_all = "camelCase")]`, matching `#[ts(...)]`, `#[ts(export_to = "v2/")]`, `String` IDs, integer Unix-second timestamps, and `#[ts(optional = nullable)]` only on request option fields.

```rust
pub struct WorkflowStartParams {
    pub thread_id: String,
    pub objective: String,
    #[ts(optional = nullable)]
    pub policy: Option<WorkflowPolicy>,
}

pub struct WorkflowStartResponse {
    pub workflow: Workflow,
}
```

- [ ] **Step 4: Register and dispatch v2 requests**

Register singular resource method names and serialize mutations by workflow ID. Keep request processors thin and delegate to the thread/core workflow API.

- [ ] **Step 5: Map lifecycle notifications**

Map core events to v2 notifications without injecting workflow records into `Thread.turns`. Add started, progress, paused, completed, failed, and cancelled notifications.

- [ ] **Step 6: Regenerate schemas and run tests**

Run: `cd codex-rs && just write-app-server-schema && just write-app-server-schema --experimental && just test -p codex-app-server-protocol && just test -p codex-app-server`

Expected: schemas are current and all workflow API tests pass.

### Task 7: TUI Commands and Workflow Manager

**Files:**
- Modify: `codex-rs/tui/src/slash_command.rs`
- Modify: `codex-rs/tui/src/chatwidget/input_submission.rs`
- Modify: `codex-rs/tui/src/chatwidget/slash_dispatch.rs`
- Modify: `codex-rs/tui/src/chatwidget/mod.rs`
- Create: `codex-rs/tui/src/chatwidget/workflow_commands.rs`
- Create: `codex-rs/tui/src/workflow_manager.rs`
- Create: `codex-rs/tui/src/workflow_manager_tests.rs`
- Modify: `codex-rs/tui/src/chatwidget/tests/slash_commands.rs`
- Modify: `codex-rs/tui/src/bottom_pane/slash_commands.rs`
- Create: workflow manager snapshot files under `codex-rs/tui/src/snapshots/`

**Interfaces:**
- Consumes: workflow app-server/client operations and lifecycle notifications.
- Produces: trigger submission, pending ultracode effort, slash actions, manager rendering, workflow notifications.

- [ ] **Step 1: Re-read `codex-rs/tui/src/bottom_pane/AGENTS.md` before editing**

Confirm local TUI conventions and keep manager logic out of `chatwidget.rs` and large bottom-pane modules.

- [ ] **Step 2: Write failing slash and trigger tests**

Cover command listing, inline argument support, task-time availability, natural-language ultracode detection, fenced-code exclusion, pending `/effort ultracode`, one-time override consumption, `/deep-research` usage errors, and all `/workflows` subcommands.

Also verify that a running workflow does not disable ordinary message submission and that opening an active task switches to its existing subagent thread rather than creating a duplicate thread.

- [ ] **Step 3: Run focused tests and confirm RED**

Run: `cd codex-rs && just test -p codex-tui slash_command slash_commands workflow`

Expected: tests fail because commands and manager do not exist.

- [ ] **Step 4: Implement command parsing and submission**

Add `SlashCommand::Effort`, `SlashCommand::Workflows`, and `SlashCommand::DeepResearch`. Use `codex_workflows::parse_workflow_trigger` in `input_submission.rs`, and maintain `PendingWorkflowEffort::Ultracode` in the focused workflow command adapter rather than model reasoning settings.

- [ ] **Step 5: Implement workflow manager UI**

Render active and saved tabs, bounded summaries, task counts, active agents, elapsed time, and action availability. Reuse selection-view patterns but keep workflow state and rendering in `workflow_manager.rs`.

- [ ] **Step 6: Add six required snapshots**

Add snapshots for empty, planning, running, paused, failed, and completed states. Include saved definitions in at least the completed snapshot and verify narrow terminal widths do not overflow.

- [ ] **Step 7: Run TUI tests and review snapshots**

Run: `cd codex-rs && just test -p codex-tui && cargo insta pending-snapshots -p codex-tui`

Expected: tests pass and only intentional workflow snapshots are pending. Read each `.snap.new`, then run `cargo insta accept -p codex-tui` only after confirming the rendered content.

### Task 8: `codex exec` Workflow Execution and JSONL

**Files:**
- Modify: `codex-rs/exec/src/lib.rs`
- Modify: `codex-rs/exec/src/exec_events.rs`
- Modify: `codex-rs/exec/src/event_processor_with_jsonl_output.rs`
- Modify: `codex-rs/exec/src/event_processor_with_jsonl_output_tests.rs`
- Create: `codex-rs/exec/src/workflow_events.rs`
- Modify: `codex-rs/exec/tests/event_processor_with_json_output.rs`
- Create: `codex-rs/exec/tests/dynamic_workflows.rs`

**Interfaces:**
- Consumes: shared trigger parser and workflow app-server notifications.
- Produces: workflow initial operation, wait-for-terminal behavior, JSONL lifecycle events, terminal exit status.

- [ ] **Step 1: Write failing exec tests**

Cover the three trigger forms, ordinary prompts remaining ordinary turns, waiting for workflow terminal state, completed exit code `0`, failed/cancelled nonzero exit, and all JSONL workflow event shapes.

- [ ] **Step 2: Run exec tests and confirm RED**

Run: `cd codex-rs && just test -p codex-exec dynamic_workflows event_processor_with_jsonl_output`

Expected: tests fail because workflow initial operations and events do not exist.

- [ ] **Step 3: Add workflow initial operation**

```rust
enum InitialOperation {
    Turn(UserTurnRequest),
    Review(ReviewRequest),
    Workflow(WorkflowStartParams),
}
```

Parse through the shared trigger module and wait for workflow terminal notifications instead of treating the planner turn as completion.

- [ ] **Step 4: Add JSONL event variants**

Emit stable event types:

```text
workflow.started
workflow.progress
workflow.paused
workflow.completed
workflow.failed
workflow.cancelled
```

Extend notification filtering so these events cannot be silently dropped.

- [ ] **Step 5: Run exec tests**

Run: `cd codex-rs && just test -p codex-exec`

Expected: all exec workflow and existing event processor tests pass.

### Task 9: Cross-Surface Completion Audit

**Files:**
- Modify only files required by failures directly caused by Dynamic Workflows changes.

**Interfaces:**
- Consumes: all previous deliverables.
- Produces: verified implementation evidence against every design completion criterion.

- [ ] **Step 1: Re-read root `AGENTS.md` and review the complete diff**

Check for instruction drift, unrelated formatting, large modules, unbounded model context, missing Bazel data, public API growth, and changed rollout semantics.

- [ ] **Step 2: Run targeted crate tests in dependency order**

Run:

```bash
cd codex-rs
just test -p codex-workflows
just test -p codex-config
just test -p codex-protocol
just test -p codex-rollout
just test -p codex-core dynamic_workflows
just test -p codex-core workflow_resume
just test -p codex-app-server-protocol
just test -p codex-app-server workflow
just test -p codex-tui
just test -p codex-exec
```

Expected: all listed tests pass; unrelated environment failures are recorded separately and are not fixed in this change.

- [ ] **Step 3: Regenerate generated artifacts**

Run:

```bash
cd codex-rs
just write-config-schema
just write-app-server-schema
just write-app-server-schema --experimental
just bazel-lock-update
```

Expected: generated schemas and `MODULE.bazel.lock` match the current source and dependencies.

- [ ] **Step 4: Run scoped lint fixes**

Run:

```bash
cd codex-rs
just fix -p codex-workflows
just fix -p codex-protocol
just fix -p codex-core
just fix -p codex-app-server-protocol
just fix -p codex-app-server
just fix -p codex-tui
just fix -p codex-exec
```

Expected: no Dynamic Workflows clippy failures remain.

- [ ] **Step 5: Format last**

Run: `cd codex-rs && just fmt`

Expected: formatting succeeds. Do not rerun tests after this repository-required final formatting step.

- [ ] **Step 6: Audit every completion criterion**

Record direct evidence for both trigger forms, `/workflows` controls, `/deep-research`, concurrent execution, failure/retry/pause/resume/cancel, cold resume, saved definitions, TUI snapshots, app-server v2, exec JSONL, plan-mode regression, and multi-agent regression. Treat missing or indirect evidence as incomplete implementation.
