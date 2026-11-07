// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DevEnvRun } from "@/tauri/commands";
import { WORKTREE_RUN_COMPLETED_EVENT } from "@/tauri/events";

import { useEpicAssemblyListener } from "./use-epic-assembly-listener";
import { useEpicAssemblyStore } from "../stores/epic-assembly-store";
import { useSurfaceSyncStore } from "../stores/surface-sync-store";

// Capture the listener registered for the completed-event topic so tests can
// drive payloads through it.
type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((topic: string, cb: Handler) => {
    handlers.set(topic, cb);
    return Promise.resolve(() => handlers.delete(topic));
  }),
}));

function emitCompleted(runId: string, ok: boolean) {
  handlers.get(WORKTREE_RUN_COMPLETED_EVENT)?.({
    payload: { run_id: runId, ok, exit_code: ok ? 0 : 1, error: ok ? null : "boom" },
  });
}

const CWD = "/epic/dir";
const runs: DevEnvRun[] = [
  { owner: "acme", repo: "api", runId: "epic-e-acme-api" },
  { owner: "acme", repo: "web", runId: "epic-e-acme-web" },
];

beforeEach(() => {
  handlers.clear();
  useEpicAssemblyStore.setState({ byCwd: {}, runIdToCwd: {}, pendingByRunId: {} });
  useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
});

describe("useEpicAssemblyListener", () => {
  it("routes a completion into the batch by run_id", async () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, []);
    renderHook(() => useEpicAssemblyListener());
    await Promise.resolve();

    emitCompleted("epic-e-acme-api", true);
    expect(useEpicAssemblyStore.getState().byCwd[CWD].installs["epic-e-acme-api"].state).toBe(
      "done",
    );
  });

  it("mirrors the settled failure set onto the chip echo (install failure)", async () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, []);
    renderHook(() => useEpicAssemblyListener());
    await Promise.resolve();

    emitCompleted("epic-e-acme-api", true);
    // Not all terminal yet → chip not marked.
    expect(useSurfaceSyncStore.getState().assemblyFailure[CWD]).toBeUndefined();

    emitCompleted("epic-e-acme-web", false);
    // All terminal, one failed → chip echoes the combined summary.
    expect(useSurfaceSyncStore.getState().assemblyFailure[CWD]).toBe(
      "1 repo couldn't set up: acme/web",
    );
  });

  it("clears the chip when everything settles clean", async () => {
    // Pre-seed a stale chip failure (e.g. from a create-time surface) …
    useSurfaceSyncStore.getState().markAssemblyFailure(CWD, "stale");
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, []);
    renderHook(() => useEpicAssemblyListener());
    await Promise.resolve();

    emitCompleted("epic-e-acme-api", true);
    emitCompleted("epic-e-acme-web", true);
    expect(useSurfaceSyncStore.getState().assemblyFailure[CWD]).toBeUndefined();
  });

  it("folds create failures into the settled chip echo", async () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, [{ owner: "acme", repo: "cli", error: "no clone" }]);
    renderHook(() => useEpicAssemblyListener());
    await Promise.resolve();

    emitCompleted("epic-e-acme-api", true);
    emitCompleted("epic-e-acme-web", true);
    // Create failure persists even though both installs succeeded.
    expect(useSurfaceSyncStore.getState().assemblyFailure[CWD]).toBe(
      "1 repo couldn't set up: acme/cli",
    );
  });

  it("ignores a completion whose run_id isn't a tracked epic install", async () => {
    renderHook(() => useEpicAssemblyListener());
    await Promise.resolve();
    // Should not throw, should not create any chip failure, and (non-epic id)
    // must NOT be buffered — the shared channel is guarded by the `epic-` prefix.
    emitCompleted("story-worktree-run-123", false);
    expect(useSurfaceSyncStore.getState().assemblyFailure).toEqual({});
    expect(useEpicAssemblyStore.getState().pendingByRunId).toEqual({});
  });
});
