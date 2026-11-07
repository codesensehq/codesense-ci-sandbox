// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";

// Capture the event handlers the hook registers so we can drive them directly.
const h = vi.hoisted(() => ({
  handlers: new Map<string, (e: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    h.handlers.set(name, cb);
    return () => h.handlers.delete(name);
  }),
}));

import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { TERMINAL_AI_STATUSES, TERMINAL_WIRE_STATUSES } from "@/shared/lib/terminal-ai-status";

function emit(name: string, payload: unknown) {
  const cb = h.handlers.get(name);
  if (!cb) throw new Error(`no handler registered for ${name}`);
  cb({ payload });
}

beforeEach(() => {
  h.handlers.clear();
  useViewerStore.setState({ aiStatusBySession: {}, progressBySession: {} });
});

describe("useTerminalStatusEvents (SLW-FX-17 AC-5/AC-6 listener contract)", () => {
  it("a valid terminal-status-update sets exactly that session's dot", () => {
    renderHook(() => useTerminalStatusEvents());
    emit("terminal-status-update", { tab_session_id: "shell-1", status: "running" });
    expect(useViewerStore.getState().aiStatusBySession["shell-1"]).toBe("running");
    // A different, untouched session's dot is unchanged (string-equality correlation).
    expect(useViewerStore.getState().aiStatusBySession["shell-2"]).toBeUndefined();
  });

  it("status 'idle' clears the dot (AC-6)", () => {
    useViewerStore.setState({ aiStatusBySession: { "shell-1": "running" } });
    renderHook(() => useTerminalStatusEvents());
    emit("terminal-status-update", { tab_session_id: "shell-1", status: "idle" });
    expect(useViewerStore.getState().aiStatusBySession["shell-1"]).toBeUndefined();
  });

  it("an unrecognized status is a no-op", () => {
    useViewerStore.setState({ aiStatusBySession: { "shell-1": "running" } });
    renderHook(() => useTerminalStatusEvents());
    emit("terminal-status-update", { tab_session_id: "shell-1", status: "bogus" });
    // Unchanged — the enum guard rejects it.
    expect(useViewerStore.getState().aiStatusBySession["shell-1"]).toBe("running");
  });

  it("pty-exit clears the session's dot (AC-6)", () => {
    useViewerStore.setState({ aiStatusBySession: { "shell-1": "done" } });
    renderHook(() => useTerminalStatusEvents());
    emit("pty-exit", { session_id: "shell-1" });
    expect(useViewerStore.getState().aiStatusBySession["shell-1"]).toBeUndefined();
  });

  it("SW-AM-4: pty-exit drops a stale IN-PROGRESS progress entry", () => {
    useViewerStore.setState({ progressBySession: { "shell-1": { phase: "dev", note: "wip" } } });
    renderHook(() => useTerminalStatusEvents());
    emit("pty-exit", { session_id: "shell-1" });
    expect(useViewerStore.getState().progressBySession["shell-1"]).toBeUndefined();
  });

  it("SW-AM-4 (AC-4): pty-exit KEEPS a settled summary so it survives to teardown", () => {
    useViewerStore.setState({
      progressBySession: { "shell-1": { phase: "done", note: "shipped", settled: true } },
    });
    renderHook(() => useTerminalStatusEvents());
    emit("pty-exit", { session_id: "shell-1" });
    expect(useViewerStore.getState().progressBySession["shell-1"]).toEqual({
      phase: "done",
      note: "shipped",
      settled: true,
    });
  });

  // The wire validator's accepted set is derived from `TERMINAL_WIRE_STATUSES`,
  // so the two lists cannot drift. Proven observably: every wire status sets the
  // stored status, `"idle"` clears it (accepted but not stored), and a status
  // outside the derived set is ignored — including the record-only dot states,
  // which reach the dot through the agent record, never the wire.
  it("accepts exactly the stored statuses plus 'idle' off the wire (status-union agreement)", () => {
    renderHook(() => useTerminalStatusEvents());
    for (const status of TERMINAL_WIRE_STATUSES) {
      emit("terminal-status-update", { tab_session_id: "s1", status });
      expect(useViewerStore.getState().aiStatusBySession["s1"]).toBe(status);
    }
    const recordOnly = TERMINAL_AI_STATUSES.filter(
      (s) => !(TERMINAL_WIRE_STATUSES as readonly string[]).includes(s),
    );
    expect(recordOnly).toEqual(["thinking", "crashed"]);
    for (const status of recordOnly) {
      emit("terminal-status-update", { tab_session_id: "s1", status });
      expect(useViewerStore.getState().aiStatusBySession["s1"]).toBe("needs-permission");
    }
    emit("terminal-status-update", { tab_session_id: "s1", status: "idle" });
    expect(useViewerStore.getState().aiStatusBySession["s1"]).toBeUndefined();
    useViewerStore.getState().setSessionAiStatus("s1", "running");
    emit("terminal-status-update", { tab_session_id: "s1", status: "bogus" });
    expect(useViewerStore.getState().aiStatusBySession["s1"]).toBe("running");
  });
});
