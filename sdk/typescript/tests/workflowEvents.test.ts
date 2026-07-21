import { describe, expect, it } from "@jest/globals";

import type { ThreadEvent, WorkflowEventSnapshot } from "../src/index";

const workflow: WorkflowEventSnapshot = {
  workflow_id: "workflow-1",
  title: "Verify release",
  status: "running",
  tasks: [
    {
      task_id: "task-1",
      title: "Run checks",
      status: "waiting_for_approval",
      summary: "Waiting",
      attempt: 1,
    },
  ],
  completed_tasks: 0,
  running_tasks: 1,
  queued_tasks: 0,
  blocked_tasks: 0,
  failed_tasks: 0,
  summary: "Running checks",
};

describe("workflow events", () => {
  it("exposes every codex exec workflow JSONL event shape", () => {
    const events: ThreadEvent[] = [
      { type: "workflow.started", thread_id: "thread-1", workflow },
      {
        type: "workflow.progress",
        thread_id: "thread-1",
        workflow_id: "workflow-1",
        status: "running",
        completed_tasks: 0,
        running_tasks: 1,
        queued_tasks: 0,
        blocked_tasks: 0,
        failed_tasks: 0,
        summary: "Running checks",
      },
      { type: "workflow.paused", thread_id: "thread-1", workflow },
      { type: "workflow.completed", thread_id: "thread-1", workflow },
      {
        type: "workflow.failed",
        thread_id: "thread-1",
        workflow,
        error: "check failed",
      },
      { type: "workflow.cancelled", thread_id: "thread-1", workflow },
    ];

    expect(events.map((event) => event.type)).toEqual([
      "workflow.started",
      "workflow.progress",
      "workflow.paused",
      "workflow.completed",
      "workflow.failed",
      "workflow.cancelled",
    ]);
  });
});
