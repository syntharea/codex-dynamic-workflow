# Dynamic Workflows Design

## Status

Approved direction: match Claude Code Dynamic Workflows command semantics and user experience while implementing orchestration with a native Rust engine. JavaScript workflow files and JavaScript execution are explicitly out of scope.

## Goals

- Start a dynamic workflow when a user includes the standalone word `ultracode` in a prompt.
- Support `/effort ultracode <task>` as an explicit equivalent trigger.
- Support `/workflows` as the workflow management surface.
- Support `/deep-research <topic>` as a built-in research workflow.
- Plan, execute, monitor, pause, resume, cancel, and persist workflows.
- Run independent workflow tasks concurrently through the existing subagent infrastructure.
- Keep the main conversation usable while workflows execute in the background.
- Expose equivalent behavior in the TUI, app-server, IDE clients, and `codex exec` where the surface supports it.
- Preserve existing plan mode, multi-agent behavior, approval policy, sandboxing, and session resume semantics.

## Non-Goals

- Running Claude JavaScript workflow files.
- Providing a general-purpose embedded scripting runtime.
- Bypassing existing tool approvals, sandbox restrictions, or agent concurrency limits.
- Guaranteeing that every workflow task can execute concurrently; dependencies and write conflicts may serialize work.
- Replacing skills, custom agents, hooks, or collaboration modes.

## User Experience

### Natural-language trigger

The standalone word `ultracode`, matched case-insensitively in user text, requests a dynamic workflow:

```text
ultracode migrate this service to the new authentication API
```

```text
Migrate the service, ultracode. Preserve the public API and run integration tests.
```

The trigger is removed before deriving the workflow objective. Trigger-looking text inside fenced code blocks does not activate a workflow.

### Explicit effort command

```text
/effort ultracode migrate this service to the new authentication API
```

`/effort ultracode` without a task activates ultracode effort for the next submitted prompt. Submitting another `/effort` value or starting a workflow consumes and clears the pending override.

### Workflow manager

`/workflows` opens a workflow manager showing:

- workflow ID and title;
- current state and elapsed time;
- completed, running, queued, blocked, and failed task counts;
- active agents and their current task;
- the latest progress summary;
- whether the workflow can be paused, resumed, cancelled, retried, or opened.

The command also accepts non-interactive subcommands:

```text
/workflows list
/workflows status <workflow-id>
/workflows pause <workflow-id>
/workflows resume <workflow-id>
/workflows cancel <workflow-id>
/workflows retry <workflow-id>
/workflows open <workflow-id>
/workflows save <workflow-id> <name>
/workflows run <name> [task-overrides]
/workflows delete <name>
```

In the TUI, the manager provides the same actions through keyboard shortcuts. Unsupported actions are disabled rather than failing after selection.

The manager has separate views for active workflow runs and saved workflow definitions. Saving a completed or accepted workflow stores its validated graph as a reusable declaration. Running a saved definition creates a new workflow ID and preserves the saved definition unchanged.

### Deep research

```text
/deep-research compare the current authentication implementations and recommend a migration plan
```

This creates a dynamic workflow using a built-in research policy:

- decompose the question into independent research dimensions;
- prefer read-only agents;
- require source or repository evidence for conclusions;
- synthesize disagreements and known unknowns;
- return one final report to the initiating conversation.

### Background behavior

Starting a workflow returns control to the main conversation after planning has been accepted by the engine. Progress arrives as bounded workflow events rather than unbounded worker logs. The user may continue chatting while the workflow runs.

Completion, failure, cancellation, and requests for user input appear as high-priority notifications. Opening a workflow shows its task graph, progress, and bounded task summaries.

## Architecture

### New `codex-workflows` crate

Create a workspace crate that owns workflow-domain behavior without depending on `codex-core`:

- workflow and task identifiers;
- workflow definitions and validated task graphs;
- states and transitions;
- dependency scheduling;
- retry policy;
- progress aggregation;
- persistence DTOs;
- saved workflow definition DTOs;
- trigger parsing;
- deterministic unit tests.

The crate must not execute tools, spawn agents, access the network, or depend on TUI types. Execution is provided through a narrow adapter implemented by `codex-core`.

### Core integration

`codex-core` owns a workflow runtime associated with the thread manager. It provides:

- a planner adapter that asks the model for a structured workflow graph;
- an executor adapter backed by the existing `AgentControl` APIs;
- workflow lifecycle management;
- rollout persistence and restoration;
- protocol event emission;
- approval and sandbox propagation;
- bounded summaries returned from worker threads.

The runtime reuses existing subagent spawn, wait, steer, interrupt, and close behavior. It does not create a second agent implementation.

### Planning contract

The planner produces a versioned structured graph containing:

- workflow title and objective;
- task ID, title, instructions, and expected output;
- dependencies;
- suggested agent role and model class;
- read/write intent;
- owned file paths or responsibility boundaries when known;
- verification requirements;
- maximum attempts;
- optional consolidation tasks.

The Rust engine validates:

- unique IDs;
- references to existing dependencies;
- acyclic dependencies;
- bounded task count and instruction size;
- valid attempt limits;
- non-overlapping concurrent write ownership where declared;
- availability of requested agent roles;
- configured concurrency and depth limits.

Invalid plans are returned to the planner for one repair attempt. A second invalid plan fails the workflow with an actionable error.

### Scheduler

The scheduler is event-driven. A task is runnable when all dependencies succeeded and workflow state is `Running`. Runnable tasks are queued and dispatched subject to:

- the existing global and per-session subagent limits;
- the workflow-specific concurrency limit;
- write ownership conflicts;
- agent depth limits;
- pause, cancellation, and approval state.

Task failure behavior:

1. Retry when attempts remain and the failure is retryable.
2. Mark dependent tasks blocked when attempts are exhausted.
3. Continue independent branches when safe.
4. Run final consolidation only when its required inputs are available.
5. Finish as `Failed` when required tasks cannot complete.

### State model

Workflow states:

- `Planning`
- `Running`
- `Pausing`
- `Paused`
- `Consolidating`
- `Completed`
- `Failed`
- `Cancelled`

Task states:

- `Queued`
- `Ready`
- `Running`
- `WaitingForApproval`
- `Succeeded`
- `Failed`
- `Blocked`
- `Cancelled`

All transitions are checked by the domain crate. Terminal states cannot transition back except `Failed` workflows through the explicit retry action, which creates a new execution attempt while retaining history.

### Persistence and resume

Workflow creation, validated plans, transitions, task attempts, bounded summaries, and terminal outcomes are recorded as versioned rollout items. Large command output and full subagent history remain in their agent threads and are not copied into the parent workflow state.

On resume, the runtime reconstructs workflow state from rollout items. Tasks that were running when the process stopped become queued recovery tasks unless their subagent thread has a confirmed terminal outcome. The scheduler never assumes that an incomplete task succeeded.

Reusable workflow definitions are stored under the Codex home and may be overridden by repository-local definitions in `.codex/workflows/`. The persisted format is versioned and declarative; it contains task templates, dependencies, role hints, ownership rules, and verification requirements, but no executable code. Repository-local definitions only load in trusted projects.

### Protocol

Add versioned protocol types for:

- workflow summary;
- task summary;
- lifecycle events;
- progress updates;
- action requests and responses;
- workflow-created thread references.
- saved workflow definition summaries and management operations.

App-server additions use v2 only and follow existing camelCase and schema-generation requirements. Raw response item compatibility is preserved by adding new variants rather than changing existing variants.

### TUI

Add slash commands and a dedicated workflow manager module instead of extending central modules with substantial logic. TUI responsibilities are limited to:

- parsing user commands and ultracode triggers;
- sending workflow operations;
- rendering workflow notifications and the manager;
- switching to associated agent threads;
- showing errors and available actions.

The manager receives snapshot coverage for empty, planning, running, paused, failed, and completed states.

### Non-interactive execution

`codex exec` recognizes `ultracode`, `/effort ultracode`, and `/deep-research`. It waits for the initiated workflow by default and exits according to the workflow terminal state. JSON output emits structured workflow events. A future detached execution option may return immediately, but detachment is not required for the first complete implementation because there is no persistent daemon guarantee in all CLI environments.

## Configuration

Add a `dynamic_workflows` feature flag during stabilization. Configuration uses a dedicated table with self-documenting fields:

```toml
[workflows]
max_concurrent_workflows = 4
max_tasks_per_workflow = 256
max_concurrent_tasks = 16
max_task_attempts = 2
pause_timeout_seconds = 30
```

Effective task concurrency is always capped by the existing agent limits. Configuration schema generation is required.

## Safety and Invariants

- Workflow agents inherit the initiating thread's approval and sandbox policy.
- A workflow cannot elevate permissions or silently change its model provider.
- Cancellation stops queued work immediately and requests interruption of running agents.
- Pause prevents new dispatch and waits for running tasks to reach a safe boundary or timeout.
- Concurrent write tasks require disjoint ownership; unknown ownership is serialized by default.
- All model-visible workflow fragments are bounded and represented by contextual fragment structs.
- No workflow item injected into model context may exceed repository context-size limits.
- Workflow events and summaries exclude secrets and raw environment values.

## Testing Strategy

### Domain crate

- trigger recognition and fenced-code exclusion;
- DAG validation and cycle rejection;
- dependency readiness;
- concurrency and write-conflict scheduling;
- pause, resume, cancellation, retry, and terminal transitions;
- persistence round trips and version handling.

### Core integration

- planner request and valid graph execution;
- invalid graph repair and failure;
- parallel subagent execution with bounded summaries;
- retry and blocked dependencies;
- approval propagation;
- cancellation and pause behavior;
- session resume without false task success;
- model and multi-agent version selection.

### TUI

- slash command parsing;
- pending `/effort ultracode` behavior;
- natural-language trigger behavior;
- workflow manager actions;
- saved workflow creation, reuse, and deletion;
- snapshots for all major states;
- plan mode and existing multi-agent regressions.

### App-server and exec

- public v2 RPC behavior and generated schemas;
- workflow lifecycle notifications;
- resume behavior;
- JSONL output and exit status for `codex exec`.

## Delivery Sequence

1. Add the domain crate, trigger parser, graph validation, scheduler, and unit tests.
2. Add protocol and rollout representations with serialization tests.
3. Integrate the core planner and existing subagent executor.
4. Add persistence, pause, resume, cancellation, and retry.
5. Add declarative saved workflow definitions and reuse.
6. Add TUI commands, manager, notifications, and snapshots.
7. Add app-server v2 and `codex exec` support.
8. Run scoped formatting, linting, tests, schema generation, and compatibility review.

## Completion Criteria

The feature is complete only when all of the following are demonstrated from the current worktree:

- Both `ultracode` trigger forms start equivalent workflows.
- `/workflows` can inspect and control live and restored workflows.
- Completed workflows can be saved, listed, rerun, and deleted without executing scripts.
- `/deep-research` runs through the native workflow engine.
- Independent tasks execute concurrently through existing subagents.
- Pause, resume, retry, cancel, failure, and completion behavior is covered by tests.
- Workflow state survives process/thread resume without claiming unconfirmed work succeeded.
- TUI, app-server v2, and `codex exec` expose consistent lifecycle behavior.
- Existing plan mode and multi-agent tests remain green.
- Required schemas, snapshots, formatting, and scoped linting are current.
