// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { patchWorkspaceSetupList, setWorkspaceBackgroundSetup } from "./viewer-store";
import type { Workspace } from "./viewer-store";
import type { WorkspaceSetupState } from "@/features/viewer/lib/workspace-setup-state";

const CLONING: WorkspaceSetupState = { kind: "step1", phase: "creatingWorktree" };
const INSTALLING: WorkspaceSetupState = { kind: "step1", phase: "runningSetup" };

// RF-perf-14c Prong 3: `patchWorkspaceSetupList` is the pure core of
// `updateWorkspaceSetup`. The worktree-setup stream fires `onProgress` per
// output LINE (npm/cargo emit hundreds), each landing in a Tauri `listen()`
// callback that ESCAPES React 18 batching — so a same-value write must return
// the SAME array reference to hit Zustand's `Object.is` no-op short-circuit and
// notify zero subscribers. These tests pin that reference-identity contract.

function creatingWorkspace(over: Partial<Workspace> = {}): Workspace {
  return {
    id: "story-1",
    kind: "story",
    cwd: "/wt/story-1",
    status: "creating",
    setupState: CLONING,
    ...over,
  } as Workspace;
}

describe("patchWorkspaceSetupList (RF-perf-14c Prong 3)", () => {
  it("returns the SAME array ref when the resolved setup state is unchanged (an equal value) (same-phase line)", () => {
    const list = [creatingWorkspace({ setupState: { ...INSTALLING } })];
    const next = patchWorkspaceSetupList(list, "story-1", { setupState: { ...INSTALLING } });
    // No-op write ⇒ identical reference ⇒ Zustand notifies nobody.
    expect(next).toBe(list);
  });

  it("returns a NEW array with the updated workspace on a real setup-state change (phase transition)", () => {
    const list = [creatingWorkspace({ setupState: CLONING })];
    const next = patchWorkspaceSetupList(list, "story-1", { setupState: { ...INSTALLING } });
    expect(next).not.toBe(list);
    expect(next![0]).not.toBe(list[0]);
    expect(next![0].setupState).toEqual(INSTALLING);
    expect(next![0].status).toBe("creating");
  });

  it("leaves sibling workspaces untouched (same element refs) on a real change", () => {
    const target = creatingWorkspace({ id: "story-1", setupState: CLONING });
    const sibling = creatingWorkspace({ id: "story-2", setupState: CLONING });
    const list = [target, sibling];
    const next = patchWorkspaceSetupList(list, "story-1", { setupState: { ...INSTALLING } });
    expect(next).not.toBe(list);
    expect(next![1]).toBe(sibling); // untouched element keeps its ref
    expect(next![0]).not.toBe(target);
  });

  it("status→ready clears the Step 1 line (Ready waits for Step 2) and returns a new array", () => {
    const list = [creatingWorkspace({ setupState: { ...INSTALLING } })];
    const next = patchWorkspaceSetupList(list, "story-1", { status: "ready" });
    expect(next).not.toBe(list);
    expect(next![0].status).toBe("ready");
    expect(next![0].setupState).toBeUndefined();
  });

  it("SW-XS-10: status→ready takes the wait for an expected agent, never Ready", () => {
    const list = [creatingWorkspace({ setupState: CLONING })];
    const waiting: WorkspaceSetupState = { kind: "awaitingAgent", since: 1 };
    const next = patchWorkspaceSetupList(list, "story-1", { status: "ready", setupState: waiting });
    expect(next![0].setupState).toEqual(waiting);
  });

  it("SW-XS-10: status→error leaves a Step 1 failure line naming the phase it stopped in", () => {
    const list = [creatingWorkspace({ setupState: { ...INSTALLING } })];
    const next = patchWorkspaceSetupList(list, "story-1", { status: "error", statusError: "boom" });
    expect(next![0].setupState).toEqual({
      kind: "failed",
      step: 1,
      phase: "runningSetup",
      reason: "boom",
    });
  });

  it("status→error sets statusError and returns a new array", () => {
    const list = [creatingWorkspace()];
    const next = patchWorkspaceSetupList(list, "story-1", { status: "error", statusError: "boom" });
    expect(next).not.toBe(list);
    expect(next![0].status).toBe("error");
    expect(next![0].statusError).toBe("boom");
  });

  it("no-ops (same ref) a status→ready when the line is already cleared and status matches", () => {
    // Idempotent re-delivery of the ready flip must not churn.
    const list = [creatingWorkspace({ status: "creating", setupState: undefined })];
    const first = patchWorkspaceSetupList(list, "story-1", { status: "ready" })!;
    const second = patchWorkspaceSetupList(first, "story-1", { status: "ready" });
    // The second delivery targets a now-"ready" (non-creating) workspace → skipped.
    expect(second).toBe(first);
  });

  it("ignores workspaces that are not in 'creating' status (returns same ref)", () => {
    const list = [creatingWorkspace({ status: "ready", setupState: undefined })];
    const next = patchWorkspaceSetupList(list, "story-1", { setupState: { ...INSTALLING } });
    expect(next).toBe(list);
  });

  it("returns the input unchanged when the team list is undefined", () => {
    expect(
      patchWorkspaceSetupList(undefined, "story-1", { setupState: INSTALLING }),
    ).toBeUndefined();
  });
});

describe("setWorkspaceBackgroundSetup (RF-perf-14 decouple)", () => {
  it("sets the background-setup badge on a READY workspace (terminal stays mounted)", () => {
    const list = [creatingWorkspace({ status: "ready", setupState: undefined })];
    const next = setWorkspaceBackgroundSetup(list, "story-1", {
      status: "running",
      label: "Running setup…",
    });
    expect(next).not.toBe(list);
    expect(next![0].status).toBe("ready"); // never downgrades the workspace
    expect(next![0].backgroundSetup).toEqual({ status: "running", label: "Running setup…" });
  });

  it("returns the SAME array ref on a no-op (same-value badge update)", () => {
    const list = [
      creatingWorkspace({
        status: "ready",
        backgroundSetup: { status: "running", label: "Running setup…" },
      }),
    ];
    const next = setWorkspaceBackgroundSetup(list, "story-1", {
      status: "running",
      label: "Running setup…",
    });
    expect(next).toBe(list); // value-equality guard ⇒ no churn during the setup stream
  });

  it("clears the badge (undefined) on setup success", () => {
    const list = [creatingWorkspace({ status: "ready", backgroundSetup: { status: "running" } })];
    const next = setWorkspaceBackgroundSetup(list, "story-1", undefined);
    expect(next).not.toBe(list);
    expect(next![0].backgroundSetup).toBeUndefined();
  });

  it("RF-perf-14d: flips queued → running as a REAL change (new array), no-ops a same-value queued", () => {
    const queued = [creatingWorkspace({ status: "ready", setupState: undefined })];
    // Queued → running is a real status change: new array + updated badge.
    const running = setWorkspaceBackgroundSetup(queued, "story-1", {
      status: "queued",
      label: "Running setup…",
    });
    expect(running).not.toBe(queued);
    expect(running![0].backgroundSetup).toEqual({ status: "queued", label: "Running setup…" });
    // A same-value queued write is churn-safe (same array ref) during the wait.
    const again = setWorkspaceBackgroundSetup(running, "story-1", {
      status: "queued",
      label: "Running setup…",
    });
    expect(again).toBe(running);
    // Acquiring a slot flips to running — a real change (different status).
    const nowRunning = setWorkspaceBackgroundSetup(running, "story-1", {
      status: "running",
      label: "Running setup…",
    });
    expect(nowRunning).not.toBe(running);
    expect(nowRunning![0].backgroundSetup?.status).toBe("running");
  });

  it("sets a retryable failed badge WITHOUT touching status (shell stays usable)", () => {
    const list = [creatingWorkspace({ status: "ready", setupState: undefined })];
    const next = setWorkspaceBackgroundSetup(list, "story-1", {
      status: "failed",
      error: "npm ci failed",
    });
    expect(next![0].status).toBe("ready"); // NOT "error" — the terminal is not hidden
    expect(next![0].backgroundSetup).toEqual({ status: "failed", error: "npm ci failed" });
  });

  it("retains structured diagnostics and treats a new attempt as a real change", () => {
    const diagnostics = {
      overallExitCode: 1,
      failedResult: {
        command: "npm ci",
        passed: false,
        stdout: "",
        stderr: "boom",
        exit_code: 1,
        phase: "setup" as const,
        step_index: 0,
      },
      streams: {
        stdout: { originalLength: 0, truncated: false },
        stderr: { originalLength: 4, truncated: false },
      },
    };
    const list = [creatingWorkspace({ status: "ready" })];
    const failed = setWorkspaceBackgroundSetup(list, "story-1", {
      status: "failed",
      error: "npm ci failed",
      diagnostics,
    })!;
    expect(failed[0].backgroundSetup).toEqual({
      status: "failed",
      error: "npm ci failed",
      diagnostics,
    });
    expect(
      setWorkspaceBackgroundSetup(failed, "story-1", {
        status: "failed",
        error: "npm ci failed",
        diagnostics,
      }),
    ).toBe(failed);
    expect(
      setWorkspaceBackgroundSetup(failed, "story-1", {
        status: "failed",
        error: "npm ci failed",
        diagnostics: { ...diagnostics },
      }),
    ).not.toBe(failed);
  });

  it("no-ops (same ref) clearing an already-clear badge", () => {
    const list = [creatingWorkspace({ status: "ready", setupState: undefined })];
    expect(setWorkspaceBackgroundSetup(list, "story-1", undefined)).toBe(list);
  });

  it("leaves sibling workspaces untouched on a real change", () => {
    const target = creatingWorkspace({ id: "story-1", status: "ready" });
    const sibling = creatingWorkspace({ id: "story-2", status: "ready" });
    const list = [target, sibling];
    const next = setWorkspaceBackgroundSetup(list, "story-1", { status: "running" });
    expect(next![1]).toBe(sibling);
    expect(next![0]).not.toBe(target);
  });

  it("returns the input unchanged when the team list is undefined", () => {
    expect(
      setWorkspaceBackgroundSetup(undefined, "story-1", { status: "running" }),
    ).toBeUndefined();
  });
});

describe("setWorkspaceBackgroundSetup — SW-WT-12 checklist steps (S2)", () => {
  const steps = (
    rows: Array<{ index: number; command: string; status: string; exitCode?: number | null }>,
  ) => rows as never;

  it("treats a step-status transition as a REAL change (new array → re-render)", () => {
    const running = [
      creatingWorkspace({
        status: "ready",
        backgroundSetup: {
          status: "running",
          label: "Running setup…",
          steps: steps([{ index: 0, command: "npm ci", status: "running" }]),
        },
      }),
    ];
    const done = setWorkspaceBackgroundSetup(running, "story-1", {
      status: "running",
      label: "Running setup…",
      steps: steps([{ index: 0, command: "npm ci", status: "done" }]),
    });
    expect(done).not.toBe(running); // a ✓ must render
  });

  it("dedups an identical step list (same array ref → no churn on line events)", () => {
    const running = [
      creatingWorkspace({
        status: "ready",
        backgroundSetup: {
          status: "running",
          label: "Running setup…",
          steps: steps([{ index: 0, command: "npm ci", status: "running" }]),
        },
      }),
    ];
    const again = setWorkspaceBackgroundSetup(running, "story-1", {
      status: "running",
      label: "Running setup…",
      steps: steps([{ index: 0, command: "npm ci", status: "running" }]),
    });
    expect(again).toBe(running); // unchanged checklist ⇒ RF-perf-14c no-churn preserved
  });
});
