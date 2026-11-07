// SPDX-License-Identifier: MIT
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useDashboardStreamStore } from "@/shared/stores/dashboard-stream-store";
import packets from "../../../../tests/fixtures/effective-status-seam.json";
import { describe, expect, it, vi } from "vitest";
const transport = vi.hoisted(() => ({
  callbacks: new Map<string, (value: any) => void>(),
  watch: vi.fn(),
  stop: vi.fn(),
}));
vi.mock("@/features/dashboard/lib/live-hub", () => ({
  liveHub: {
    watch: (topic: string, _key: string, callback: (value: any) => void) => {
      transport.watch(topic);
      transport.callbacks.set(topic, callback);
      return transport.stop;
    },
  },
}));
import {
  joinAgentList,
  watchAgentList,
  useAgentListProjectionStore,
} from "./agent-list-projection";
import type { AgentSessionRow } from "@/tauri/commands";

const session = (handle: string): AgentSessionRow => ({
  handle,
  sessionId: `sid-${handle}`,
  kind: "registered",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: 1,
  launcher: null,
  lastOutputMs: null,
});
describe("agentList owner joins", () => {
  it("follows server parent references and order even when names and unread change", () => {
    const refs = [
      {
        id: "worker",
        sessionRef: "worker",
        inboxRef: "counts:team:acme/dev",
        teamKey: "acme:dev",
        parentId: "lead",
        parentSource: "chosen",
        order: 1,
        workspaceRef: null,
        storyRef: null,
        workspaceRank: 0,
      },
      {
        id: "lead",
        sessionRef: "lead",
        inboxRef: "counts:team:acme/dev",
        teamKey: "acme:dev",
        parentId: null,
        parentSource: null,
        order: 0,
        workspaceRef: null,
        storyRef: null,
        workspaceRank: 0,
      },
    ];
    const rows = [session("worker"), session("lead")];
    const first = joinAgentList(refs, rows, {}, {});
    expect(first.get("acme:dev")?.[0].row.handle).toBe("lead");
    expect(first.get("acme:dev")?.[0].children[0].row.handle).toBe("worker");
    const next = joinAgentList(refs, [{ ...rows[0], displayName: "Changed" }, rows[1]], {}, {});
    expect(next.get("acme:dev")?.[0].children[0].row.displayName).toBe("Changed");
    expect(next.get("acme:dev")?.[0].children[0].parentSource).toBe("chosen");
  });
  it("joins a workspace by the server reference rather than guessing from the tab", () => {
    const refs = [
      {
        id: "row",
        sessionRef: "worker",
        inboxRef: "counts:team:acme/dev",
        teamKey: "acme:dev",
        parentId: null,
        parentSource: null,
        order: 0,
        workspaceRef: "canonical",
        storyRef: null,
        workspaceRank: 0,
      },
    ];
    const nodes = joinAgentList(
      refs,
      [session("worker")],
      { "acme:dev": [{ id: "canonical", kind: "generic", cwd: "/wt/right", status: "ready" }] },
      {},
    );
    expect(nodes.get("acme:dev")?.[0].workspace?.id).toBe("canonical");
  });
});

it("joins two teams' same handle by distinct process references", () => {
  const refs = [
    {
      id: "a",
      sessionRef: "sid-a",
      inboxRef: "a",
      teamKey: "org:a",
      parentId: null,
      parentSource: null,
      order: 0,
      workspaceRef: null,
      storyRef: null,
      workspaceRank: 0,
    },
    {
      id: "b",
      sessionRef: "sid-b",
      inboxRef: "b",
      teamKey: "org:b",
      parentId: null,
      parentSource: null,
      order: 1,
      workspaceRef: null,
      storyRef: null,
      workspaceRank: 0,
    },
  ];
  const owners = [
    { ...session("twin"), sessionId: "sid-a", displayName: "Alpha" },
    { ...session("twin"), sessionId: "sid-b", displayName: "Beta" },
  ];
  const joined = joinAgentList(refs, owners, {}, {});
  expect(joined.get("org:a")?.[0].row.displayName).toBe("Alpha");
  expect(joined.get("org:b")?.[0].row.displayName).toBe("Beta");
});

it("shares subscriptions and joins inbox changes into the existing process owners", () => {
  transport.watch.mockClear();
  transport.stop.mockClear();
  useAgentListProjectionStore.setState({ sessions: null, references: [], partial: true });
  const first = watchAgentList();
  const second = watchAgentList();
  try {
    expect(transport.watch).toHaveBeenCalledTimes(3);
    transport.callbacks.get("sessions")!({
      snapshot: [{ ...session("worker"), id: "sid-worker", team: { org: "org", team: "a" } }],
    });
    const original = useAgentListProjectionStore.getState().sessions![0];
    transport.callbacks.get("inbox")!({
      snapshot: [
        {
          id: "counts:team:org/a",
          rowType: "counts",
          team: { org: "org", team: "a" },
          humanUnread: 0,
          recipients: { worker: 3 },
          needsYouBySender: {},
          sessions: {
            worker: { team: { org: "org", team: "a" }, unread: 3, groupUnread: 2, groupReadId: 7 },
          },
        },
      ],
    });
    const changed = useAgentListProjectionStore.getState().sessions![0];
    expect(changed).not.toBe(original);
    expect(changed).toMatchObject({
      id: "sid-worker",
      handle: "worker",
      unread: 3,
      groupUnread: 2,
      groupReadId: 7,
    });
    transport.callbacks.get("sessions")!({ snapshot: [{ ...original }] });
    expect(useAgentListProjectionStore.getState().sessions![0]).toBe(changed);
    first();
    expect(transport.stop).not.toHaveBeenCalled();
  } finally {
    second();
  }
  expect(transport.stop).toHaveBeenCalledTimes(3);
});

it("receives Rust supervision outcomes into existing managed consumers and dashboard subscribers", () => {
  const owner = packets.steps[0].sessionDeltas
    .flatMap((delta) => delta.upserts as unknown as AgentSessionRow[])
    .find((row) => row.id === "scale-49")!;
  expect(owner.managedState).toBe("needs-attention");
  useManagedSessionStore.setState({ states: { "scale-49": "running" } });
  const receive = vi.fn((..._args: unknown[]) => {
    expect(useManagedSessionStore.getState().states["scale-49"]).toBe("needs-attention");
  });
  const stop = useDashboardStreamStore
    .getState()
    .subscribe({ kind: "session", sessionId: "scale-49" }, receive);
  useAgentListProjectionStore.setState({ sessions: [owner] });
  expect(useManagedSessionStore.getState().states["scale-49"]).toBe("needs-attention");
  expect(receive).toHaveBeenCalledOnce();
  expect(receive.mock.calls[0][0]).toEqual([
    { origin: "session", kind: "session", sessionId: "scale-49", status: "needs-attention" },
  ]);
  useAgentListProjectionStore.setState({ sessions: [{ ...owner, lastOutputMs: 123 }] });
  expect(receive).toHaveBeenCalledOnce();
  useManagedSessionStore
    .getState()
    .addPendingAsk({ sessionId: "scale-49", correlationId: "new-ask", question: "Which target?" });
  useAgentListProjectionStore.setState({ sessions: [{ ...owner, unread: 2 }] });
  expect(useManagedSessionStore.getState().states["scale-49"]).toBe("awaiting-answer");
  expect(receive).toHaveBeenCalledOnce();
  stop();
});
