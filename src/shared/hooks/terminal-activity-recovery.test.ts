// SPDX-License-Identifier: MIT
import {
  openAgentInboxRail,
  useAgentInboxRailStore,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { isRunningAgent } from "@/features/viewer/lib/hand-agents";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ActivitySnapshot,
  ActivitySnapshotReply,
  RuntimeMemberTarget,
} from "@/tauri/commands";
import {
  EMPTY_TERMINAL_LAYOUT,
  type TerminalLayoutProjection,
} from "@/shared/lib/terminal-layout-projection";
import { createActivityRecovery } from "./terminal-activity-recovery";
import {
  agentInputSeq,
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
const target: RuntimeMemberTarget = {
  member: { machineId: "local", slot: "sidecar" },
  admissionRevision: "1",
  namespace: { serverId: "server", epoch: "connection", generation: "1" },
};
const snap = (revision = 2, state: ActivitySnapshot["state"] = "done"): ActivitySnapshot => ({
  session: "s",
  identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
  revision,
  turnGeneration: 1,
  provider: "claude",
  state,
  observed: true,
  limited: true,
  exit: null,
  asks: [],
});
function setup() {
  let layout: TerminalLayoutProjection = {
    ...EMPTY_TERMINAL_LAYOUT,
    hydrated: true,
    members: {
      "local/sidecar": {
        target,
        rev: 1,
        dismissed: [],
        active: {},
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
  };
  const requests: Array<(reply: ActivitySnapshotReply) => void> = [];
  const fetch = vi.fn(
    () => new Promise<ActivitySnapshotReply>((resolve) => requests.push(resolve)),
  );
  const apply = vi.fn();
  const unavailable = vi.fn();
  const recovery = createActivityRecovery({
    layout: () => layout,
    acceptsAuthority: () => true,
    fetch,
    sequence: () => 1,
    apply,
    unavailable,
  });
  return {
    recovery,
    fetch,
    apply,
    unavailable,
    requests,
    setLayout: (next: TerminalLayoutProjection) => {
      layout = next;
    },
    getLayout: () => layout,
    event: (snapshot: ActivitySnapshot, t = target) =>
      recovery.event({ target: t, event: { Activity: { snapshot } } }),
  };
}
afterEach(() => vi.useRealTimers());
describe("activity recovery admission", () => {
  it("keeps an open inbox through another tab's host refresh and closes on confirmed exit", async () => {
    resetPrunedAgentSessionsForTests();
    useAgentStateStore.setState({
      bySession: {},
      activityBySession: {},
      exitBySession: {},
      processAgentBySession: {},
    });
    useAgentInboxRailStore.setState({ bySession: {} });
    const h = setup();
    h.apply.mockImplementation((snapshot: ActivitySnapshot) =>
      useAgentStateStore.getState().recordActivity(snapshot, agentInputSeq()),
    );
    h.unavailable.mockImplementation((ids, state) =>
      useAgentStateStore.getState().activityUnavailable(ids, state),
    );
    h.recovery.reconcile();
    h.requests[0]({ target, snapshots: [snap(2, "idle")] });
    await Promise.resolve();
    openAgentInboxRail("s");
    const open = useAgentInboxRailStore.getState().bySession.s;
    expect(open).toBeDefined();
    const layout = h.getLayout();
    const member = layout.members["local/sidecar"];
    const ws = member.workspaces.w;
    h.setLayout({
      ...layout,
      members: {
        "local/sidecar": {
          ...member,
          workspaces: { w: { ...ws, tabs: [...ws.tabs, { ...ws.tabs[0], session: "second" }] } },
        },
      },
    });
    h.recovery.reconcile();
    expect(isRunningAgent(useAgentStateStore.getState(), "s", h.getLayout())).toBe(false);
    expect(useAgentInboxRailStore.getState().bySession.s).toBe(open);
    h.requests[1]({ target, snapshots: [snap(2, "idle")] });
    await Promise.resolve();
    expect(isRunningAgent(useAgentStateStore.getState(), "s", h.getLayout())).toBe(true);
    expect(useAgentInboxRailStore.getState().bySession.s).toBe(open);
    h.event({ ...snap(3), exit: { code: 0, signal: null, requested: false } });
    expect(useAgentInboxRailStore.getState().bySession.s).toBeUndefined();
    h.recovery.dispose();
  });

  it("uses a positive process probe on a connected uninstrumented host, never a recovering or disconnected host", () => {
    const h = setup();
    const state = {
      bySession: {},
      processAgentBySession: { s: "codex" },
      activityBySession: { s: { freshness: "unavailable" as const, snapshot: null } },
    };
    expect(isRunningAgent(state, "s", h.getLayout())).toBe(true);
    expect(
      isRunningAgent(
        { ...state, activityBySession: { s: { freshness: "recovering", snapshot: null } } },
        "s",
        h.getLayout(),
      ),
    ).toBe(false);
    const layout = h.getLayout();
    const offline = {
      ...layout,
      admission: {
        "local/sidecar": {
          member: target.member,
          revision: "2",
          state: "unavailable" as const,
          namespace: null,
          historical: null,
        },
      },
    };
    expect(isRunningAgent(state, "s", offline)).toBe(false);
    h.recovery.dispose();
  });
  it("projects queued activity from its original receipt and replay cannot rejuvenate it", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    resetPrunedAgentSessionsForTests();
    useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
    const h = setup();
    h.apply.mockImplementation((snapshot: ActivitySnapshot, _sentAt: number, receivedAt: number) =>
      useAgentStateStore.getState().recordActivity(snapshot, agentInputSeq(), receivedAt),
    );
    h.recovery.reconcile();
    const queued = { ...snap(3, "thinking"), activityAgeMs: 10_000 };
    h.event(queued);
    await vi.advanceTimersByTimeAsync(2_000);
    h.requests[0]({ target, snapshots: [{ ...snap(2, "thinking"), activityAgeMs: 20_000 }] });
    await vi.advanceTimersByTimeAsync(0);
    expect(h.apply).toHaveBeenLastCalledWith(queued, 1, 1_000_000);
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(990_000);
    await vi.advanceTimersByTimeAsync(1_000);
    h.event(queued);
    h.recovery.resync();
    h.requests[1]({ target, snapshots: [queued] });
    await vi.advanceTimersByTimeAsync(0);
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(990_000);
    h.recovery.dispose();
  });

  it("distinguishes a connected old owner from outage and recovers after an explicit upgrade resync", async () => {
    vi.useFakeTimers();
    const h = setup();
    h.recovery.reconcile();
    h.requests[0]({ target, availability: "upgrade-required", snapshots: [] });
    await vi.advanceTimersByTimeAsync(0);
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "upgrade-required");
    expect(h.apply).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(9000);
    expect(h.fetch).toHaveBeenCalledTimes(1);
    h.recovery.resync();
    expect(h.fetch).toHaveBeenCalledTimes(2);
    h.requests[1]({ target, availability: "available", snapshots: [snap()] });
    await vi.advanceTimersByTimeAsync(0);
    expect(h.apply).toHaveBeenCalledExactlyOnceWith(snap(), 1, Date.now());
    h.recovery.dispose();
  });

  it("never paints an old snapshot contradicted by a buffered replacement invocation", async () => {
    vi.useFakeTimers();
    const h = setup();
    h.recovery.reconcile();
    const replacement = {
      ...snap(1, "needs-permission"),
      identity: { ...snap().identity, invocationId: "replacement" },
    };
    h.event(replacement);
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    expect(h.apply).not.toHaveBeenCalled();
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "unavailable");
    await vi.advanceTimersByTimeAsync(3000);
    expect(h.fetch).toHaveBeenCalledTimes(2);
    h.requests[1]({ target, snapshots: [replacement] });
    await Promise.resolve();
    expect(h.apply).toHaveBeenCalledExactlyOnceWith(replacement, 1, Date.now());
    h.recovery.dispose();
  });
  it("recovers another host while one snapshot is still pending", async () => {
    const h = setup();
    const local = h.getLayout().members["local/sidecar"];
    const remote = { ...target, member: { machineId: "box", slot: "remote" as const } };
    h.setLayout({
      ...h.getLayout(),
      members: {
        ...h.getLayout().members,
        "box/remote": {
          ...local,
          target: remote,
          workspaces: {
            w: {
              ...local.workspaces.w,
              tabs: local.workspaces.w.tabs.map((tab) => ({ ...tab, session: "remote" })),
            },
          },
        },
      },
    });
    h.recovery.reconcile();
    expect(h.fetch).toHaveBeenCalledTimes(2);
    h.requests[1]({ target: remote, snapshots: [{ ...snap(), session: "remote" }] });
    await Promise.resolve();
    expect(h.apply.mock.calls.map((call) => call[0].session)).toEqual(["remote"]);
    h.recovery.dispose();
  });
  it("reconnect establishes new route before accepting retained invocation", async () => {
    const h = setup();
    h.recovery.reconcile();
    const next = {
      ...target,
      admissionRevision: "2",
      namespace: { ...target.namespace, generation: "2" },
    };
    const prior = h.getLayout();
    h.setLayout({
      ...prior,
      members: { "local/sidecar": { ...prior.members["local/sidecar"], target: next } },
    });
    h.recovery.reconcile();
    h.requests[0]({ target, snapshots: [snap(8)] });
    h.requests[1]({ target: next, snapshots: [snap(2)] });
    await Promise.resolve();
    h.event(snap(9), target);
    expect(h.apply.mock.calls.map((call) => call[0].revision)).toEqual([2]);
    h.event(snap(3, "needs-permission"), next);
    expect(h.apply.mock.calls.map((call) => call[0].revision)).toEqual([2, 3]);
    h.recovery.dispose();
  });
  it("buffers event racing snapshot, restores done then newer waiting, drops duplicates", async () => {
    const h = setup();
    h.recovery.reconcile();
    h.event(snap(3, "needs-permission"));
    expect(h.apply).not.toHaveBeenCalled();
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    expect(h.apply.mock.calls.map((call) => call[0].state)).toEqual(["done", "needs-permission"]);
    h.event(snap(2));
    h.event(snap(3));
    expect(h.apply).toHaveBeenCalledTimes(2);
    h.recovery.dispose();
  });
  it("disconnection ignores pending snapshot and delayed old connection events", async () => {
    const h = setup();
    h.recovery.reconcile();
    h.setLayout({
      ...h.getLayout(),
      admission: {
        "local/sidecar": {
          member: target.member,
          revision: "2",
          state: "unavailable",
          namespace: null,
          historical: null,
        },
      },
    });
    h.recovery.reconcile();
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    h.event(snap(3));
    expect(h.apply).not.toHaveBeenCalled();
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "unavailable");
    h.recovery.dispose();
  });
  it("unknown incarnation and sequence gaps resnapshot instead of overwriting", async () => {
    const h = setup();
    h.recovery.reconcile();
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    h.event(snap(5, "working"));
    expect(h.apply).toHaveBeenCalledTimes(1);
    expect(h.fetch).toHaveBeenCalledTimes(2);
    h.requests[1]({ target, snapshots: [snap(5, "working")] });
    await Promise.resolve();
    expect(h.apply).toHaveBeenCalledTimes(2);
    h.event({ ...snap(6), identity: { ...snap().identity, invocationId: "other" } });
    expect(h.fetch).toHaveBeenCalledTimes(3);
    expect(h.apply).toHaveBeenCalledTimes(2);
    h.recovery.dispose();
  });
  it("coalesces resync during a pending read and observes a subsequently unavailable owner", async () => {
    vi.useFakeTimers();
    const h = setup();
    h.recovery.reconcile();
    h.recovery.resync();
    h.recovery.resync();
    expect(h.fetch).toHaveBeenCalledTimes(1);
    // This reply was captured before the resync; it cannot satisfy that request.
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    await Promise.resolve();
    expect(h.fetch).toHaveBeenCalledTimes(2);
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "recovering");
    vi.advanceTimersByTime(3000);
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "unavailable");
    expect(h.apply).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(9000);
    expect(h.fetch).toHaveBeenCalledTimes(2);
    h.recovery.dispose();
  });
  it("does not dispatch a queued resync after disposal", async () => {
    const h = setup();
    h.recovery.reconcile();
    h.recovery.resync();
    h.recovery.dispose();
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    await Promise.resolve();
    expect(h.fetch).toHaveBeenCalledTimes(1);
    expect(h.apply).not.toHaveBeenCalled();
  });
  it("times out unavailable without treating silence as exit; late reply is rejected", async () => {
    vi.useFakeTimers();
    const h = setup();
    h.recovery.reconcile();
    vi.advanceTimersByTime(3000);
    expect(h.unavailable).toHaveBeenLastCalledWith(["s"], "unavailable");
    vi.advanceTimersByTime(9000);
    expect(h.fetch).toHaveBeenCalledTimes(1);
    h.requests[0]({ target, snapshots: [snap()] });
    await Promise.resolve();
    expect(h.apply).not.toHaveBeenCalled();
    h.recovery.dispose();
  });
});
