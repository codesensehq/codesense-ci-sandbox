// SPDX-License-Identifier: MIT
// SW-XS-10: the one consumer of `agent-prepare-progress`. A launch's Step 2
// reaches its workspace's setup line; an explicit operation (an operationId)
// reaches the pill's chip; count ticks within a phase are coalesced (never
// per-file noise) while phase changes and settles land at once.

import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { AgentPrepareProgressPayload } from "@/tauri/events";

import { useContextOperationsStore } from "../stores/context-operations-store";

type Handler = (e: { payload: AgentPrepareProgressPayload }) => void;
const handlers = vi.hoisted(() => [] as Handler[]);
vi.mock("@tauri-apps/api/event", () => ({
  listen: (_: string, cb: Handler) => {
    handlers.push(cb);
    return Promise.resolve(() => {});
  },
}));

const { useContextProgressListener, CONTEXT_PROGRESS_THROTTLE_MS } =
  await import("./use-context-progress-listener");

const TEAM = "acme:default";
const emit = (payload: Partial<AgentPrepareProgressPayload>) =>
  handlers.at(-1)!({
    payload: {
      workspace: "/wt/SW-1",
      agent: "Barb",
      phase: "diff",
      files: 0,
      bytes: 0,
      total: 10,
      ...payload,
    },
  });

beforeEach(async () => {
  vi.useFakeTimers({ now: 1_000, toFake: ["setTimeout", "clearTimeout", "Date"] });
  handlers.length = 0;
  useContextOperationsStore.getState().reset();
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        { id: "SW-1", kind: "story", cwd: "/wt/SW-1", status: "ready" } as Workspace,
        { id: "SW-2", kind: "story", cwd: "/wt/SW-2", status: "ready" } as Workspace,
      ],
    },
  });
  renderHook(() => useContextProgressListener());
  await vi.waitFor(() => expect(handlers.length).toBeGreaterThan(0));
});
afterEach(() => vi.useRealTimers());

const setup = (id: string) =>
  useViewerStore.getState().workspaces[TEAM].find((w) => w.id === id)?.agentRuns?.Barb;

describe("SW-XS-10 progress routing", () => {
  it("a launch's Step 2 drives the named workspace's setup line, and only that one", () => {
    emit({ phase: "diff" });
    expect(setup("SW-1")).toMatchObject({ kind: "running", agent: "Barb", phase: "diff" });
    expect(setup("SW-2")).toBeUndefined();
    expect(Object.keys(useContextOperationsStore.getState().byId)).toHaveLength(0);
  });

  it("an explicit operation goes to the chip's store, never the setup line", () => {
    emit({ operationId: "op-9", operationKind: "hydrate", phase: "diff" });
    expect(useContextOperationsStore.getState().byId["op-9"]).toBeDefined();
    expect(setup("SW-1")).toBeUndefined();
  });

  it("a machine workspace is matched by its handle", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          {
            id: "SW-3",
            kind: "story",
            cwd: "",
            status: "ready",
            location: { kind: "machine", machineId: "m", requestId: "r", handle: "h-3", cwd: "/x" },
          } as Workspace,
        ],
      },
    });
    emit({ workspace: "h-3", phase: "diff" });
    expect(setup("SW-3")).toMatchObject({ kind: "running" });
  });

  it("coalesces count ticks within a phase; phase changes and the settle land at once", () => {
    const spy = vi.spyOn(useViewerStore.getState(), "applyAgentPrepareProgress");
    useViewerStore.setState({ applyAgentPrepareProgress: spy });
    emit({ phase: "diff" });
    emit({ phase: "transfer", files: 0, bytes: 0, total: 1000 });
    for (let i = 1; i <= 200; i++) emit({ phase: "transfer", files: i, bytes: i * 5, total: 1000 });
    // diff + transfer's first reading land at once; 200 ticks are held back.
    expect(spy.mock.calls.length).toBeLessThanOrEqual(3);
    vi.advanceTimersByTime(CONTEXT_PROGRESS_THROTTLE_MS);
    const afterWindow = spy.mock.calls.length;
    expect(afterWindow).toBeLessThanOrEqual(4);
    expect(spy.mock.calls.at(-1)![0]).toMatchObject({ files: 200 });
    emit({ phase: "ready", files: 200, bytes: 1000, total: 10 });
    expect(spy.mock.calls.at(-1)![0]).toMatchObject({ phase: "ready", settled: true });
  });
});
