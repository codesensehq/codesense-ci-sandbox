// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { useDashboardStreamStore } from "@/shared/stores/dashboard-stream-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { StatusCallback } from "@/shared/lib/dashboard-stream/types";
import { useDashboardStream } from "./use-dashboard-stream";

// Controllable `listen` registry so the test can emit events + track unlisten.
const eventMock = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  const unlisten = vi.fn();
  return {
    listeners,
    unlisten,
    listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      let set = listeners.get(event);
      if (!set) {
        set = new Set();
        listeners.set(event, set);
      }
      set.add(handler);
      return () => {
        set!.delete(handler);
        unlisten();
      };
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));

vi.mock("@/shared/lib/team-key", () => ({ getActiveTeamKey: vi.fn(() => "alderco:default") }));
vi.mock("@/shared/stores/org-store", () => ({ useOrgStore: { getState: () => ({ orgs: [] }) } }));

function emit(event: string, payload: unknown) {
  for (const h of [...(eventMock.listeners.get(event) ?? [])]) h({ payload });
}

beforeEach(() => {
  vi.useFakeTimers();
  eventMock.listeners.clear();
  eventMock.unlisten.mockClear();
  useDashboardStreamStore.setState({ subs: new Map(), fresh: {} });
  useViewerStore.setState({ progressBySession: {} });
});
afterEach(() => {
  vi.useRealTimers();
});

describe("useDashboardStream", () => {
  it("routes no doc events: context-file changes are not listened to (SW-OC-26)", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {}); // flush the `listen` registrations
    expect(eventMock.listeners.has("context-file-change")).toBe(false);
  });

  it("a hidden webview marks the hub stale", async () => {
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, vi.fn());
    const subId = [...useDashboardStreamStore.getState().subs.keys()][0];
    renderHook(() => useDashboardStream());
    await act(async () => {});
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(useDashboardStreamStore.getState().getFresh(subId)).toBe(false);
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
  });

  it("maps mcp:agent-done into a session-scoped delta (AC-5)", async () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);

    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "needs-attention", summary: "help" });
    });

    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb.mock.calls[0][0][0]).toMatchObject({
      origin: "session",
      kind: "session",
      sessionId: "s1",
      status: "needs-attention",
    });
  });

  it("maps mcp:agent-progress into a story-visible progress delta (SW-GW-4)", async () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);

    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      // Optional fields arrive as `null` from the emit; the bridge collapses them.
      emit("mcp:agent-progress", {
        sessionId: "s1",
        phase: "dev",
        step: 3,
        total: 8,
        note: null,
      });
    });

    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb.mock.calls[0][0][0]).toMatchObject({
      origin: "session",
      kind: "progress",
      sessionId: "s1",
      phase: "dev",
      step: 3,
      total: 8,
    });
    expect(cb.mock.calls[0][0][0]).not.toHaveProperty("note", null);
  });

  it("drops an off-enum mcp:agent-progress phase (defence-in-depth)", async () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);

    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-progress", { sessionId: "s1", phase: "shipped" });
    });

    expect(cb).not.toHaveBeenCalled();
  });

  it("SW-AM-4: mcp:agent-progress retains the latest progress in progressBySession", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-progress", { sessionId: "s1", phase: "dev", step: 3, total: 8, note: "wip" });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toEqual({
      phase: "dev",
      step: 3,
      total: 8,
      note: "wip",
    });
  });

  it("SW-AM-4 (AC-4): mcp:agent-done settles the retained progress with the summary", async () => {
    useViewerStore.setState({ progressBySession: { s1: { phase: "dev", note: "wip" } } });
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: "shipped it" });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toEqual({
      phase: "done",
      note: "shipped it",
      settled: true,
    });
  });

  it("SW-AM-4 (AC-4): needs-attention done settles to the blocked phase", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "needs-attention", summary: "help" });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toMatchObject({
      phase: "blocked",
      settled: true,
    });
  });

  it("SW-AM-4 (AC-5): a done for a session that never reported progress stays absent", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: null });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toBeUndefined();
  });

  it("SW-AM-4 (AC-5): an empty/whitespace summary writes no phantom entry", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: "   " });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toBeUndefined();
  });

  it("SW-AM-4 (AC-4): a needs-attention done with no summary settles a live entry to blocked", async () => {
    useViewerStore.setState({ progressBySession: { s1: { phase: "dev", note: "wip" } } });
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "needs-attention", summary: null });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toEqual({
      phase: "blocked",
      settled: true,
    });
  });

  it("SW-AM-4 (AC-4): a repeat done with no summary does NOT blank the first summary", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: "shipped it" });
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: null });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toEqual({
      phase: "done",
      note: "shipped it",
      settled: true,
    });
  });

  it("SW-AM-4 (AC-4): a late progress tick does NOT resurrect a settled entry", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-done", { sessionId: "s1", status: "success", summary: "shipped it" });
      // A stray/reordered progress after done must be ignored (else pty-exit
      // would later delete the summary).
      emit("mcp:agent-progress", { sessionId: "s1", phase: "dev", step: 4, total: 8, note: "wip" });
    });

    expect(useViewerStore.getState().progressBySession["s1"]).toEqual({
      phase: "done",
      note: "shipped it",
      settled: true,
    });
  });

  it("SW-AM-4: a progress/done event with an empty sessionId writes nothing", async () => {
    renderHook(() => useDashboardStream());
    await act(async () => {});

    await act(async () => {
      emit("mcp:agent-progress", { sessionId: "", phase: "dev", note: "wip" });
      emit("mcp:agent-done", { sessionId: "", status: "success", summary: "done" });
    });

    expect(useViewerStore.getState().progressBySession[""]).toBeUndefined();
  });

  it("tears down all listeners on unmount (no leak, AC-9)", async () => {
    const { unmount } = renderHook(() => useDashboardStream());
    await act(async () => {}); // registrations resolve (3 listeners)

    unmount();
    // Each tracked `listen` promise's unlisten fired (done, message, progress).
    expect(eventMock.unlisten).toHaveBeenCalledTimes(3);
  });
});
