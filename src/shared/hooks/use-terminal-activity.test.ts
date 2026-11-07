// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ActivitySnapshot, ActivitySnapshotReply } from "@/tauri/commands";
const h = vi.hoisted(() => ({
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
  fetch: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    h.handlers.set(name, handler);
    return () => {
      h.handlers.delete(name);
    };
  }),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  terminalActivitySnapshot: h.fetch,
}));
vi.mock("@/shared/hooks/use-agent-liveness-monitor", () => ({
  watchOpenTurns: () => () => {},
  watchTerminalTabAgents: () => () => {},
}));
import { useTerminalStatusEvents } from "./use-terminal-status-events";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { EMPTY_TERMINAL_LAYOUT } from "@/shared/lib/terminal-layout-projection";
const target = {
  member: { machineId: "local" as const, slot: "sidecar" as const },
  admissionRevision: "1",
  namespace: { serverId: "server", epoch: "epoch", generation: "1" },
};
const snapshot: ActivitySnapshot = {
  session: "s",
  identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
  revision: 2,
  turnGeneration: 1,
  provider: "claude",
  state: "done",
  observed: true,
  limited: true,
  exit: null,
  asks: [],
};
beforeEach(() => {
  h.handlers.clear();
  h.fetch.mockReset();
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
  useViewerStore.setState({
    aiStatusBySession: { s: "running" },
    terminalLayout: {
      ...EMPTY_TERMINAL_LAYOUT,
      hydrated: true,
      members: {
        "local/sidecar": {
          target,
          rev: 1,
          active: {},
          dismissed: [],
          workspaces: {
            w: {
              descriptor: { displayName: "w", kind: "generic" },
              tabs: [
                {
                  session: "s",
                  name: "s",
                  role: null,
                  placement: "tab",
                  state: { kind: "live" },
                  tags: {},
                },
              ],
              lastActive: null,
              grid: { paned: [], preset: "columns", collapsed: false },
              nextOrdinal: 2,
            },
          },
        },
      },
    },
  });
});
afterEach(() => {
  useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT });
});
describe("mounted ActivityV1 recovery", () => {
  it("subscribes before snapshot, restores state, and rejects subsequent legacy writes", async () => {
    let resolve!: (reply: ActivitySnapshotReply) => void;
    h.fetch.mockImplementation(() => {
      expect(h.handlers.has("terminal-runtime-event")).toBe(true);
      return new Promise<ActivitySnapshotReply>((r) => {
        resolve = r;
      });
    });
    const hook = renderHook(() => useTerminalStatusEvents());
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      h.handlers.get("terminal-runtime-event")!({
        payload: {
          target,
          event: {
            Activity: { snapshot: { ...snapshot, revision: 3, state: "needs-permission" } },
          },
        },
      });
      resolve({ target, snapshots: [snapshot] });
      await Promise.resolve();
    });
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
    act(() =>
      h.handlers.get("terminal-status-update")!({
        payload: { tab_session_id: "s", status: "done", hook_event: "Stop" },
      }),
    );
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
    expect(useViewerStore.getState().aiStatusBySession.s).toBeUndefined();
    hook.unmount();
    expect(h.handlers.size).toBe(0);
  });
  it("a snapshot that observed nothing lets the legacy hooks speak, and keeps their dot", async () => {
    h.fetch.mockResolvedValue({
      target,
      snapshots: [{ ...snapshot, state: "idle", observed: false }],
    });
    const hook = renderHook(() => useTerminalStatusEvents());
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(useAgentStateStore.getState().activityBySession.s?.snapshot?.observed).toBe(false);
    // The floor's stored dot is not erased by a snapshot that observed nothing.
    expect(useViewerStore.getState().aiStatusBySession.s).toBe("running");
    act(() =>
      h.handlers.get("terminal-status-update")!({
        payload: {
          tab_session_id: "s",
          status: "needs-permission",
          hook_event: "PermissionRequest",
        },
      }),
    );
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
    hook.unmount();
  });

  it("unmount rejects a late snapshot without recreating a record", async () => {
    let resolve!: (reply: ActivitySnapshotReply) => void;
    h.fetch.mockImplementation(
      () =>
        new Promise<ActivitySnapshotReply>((r) => {
          resolve = r;
        }),
    );
    const hook = renderHook(() => useTerminalStatusEvents());
    await act(async () => {
      await Promise.resolve();
    });
    hook.unmount();
    resolve({ target, snapshots: [snapshot] });
    await Promise.resolve();
    expect(useAgentStateStore.getState().bySession.s).toBeUndefined();
  });
});
