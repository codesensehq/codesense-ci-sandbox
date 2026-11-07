// SPDX-License-Identifier: MIT
import type { HubSnapshot, HubDelta } from "@/features/dashboard/lib/live-hub";
import statusPackets from "../../../tests/fixtures/effective-status-seam.json";
import type { HandlerRegistry } from "./mock-router";
import type { MockEventBus } from "./mock-events";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import type { SessionListRow, AgentSessionRow } from "@/tauri/commands";

export function liveHubFixture(events: MockEventBus): HandlerRegistry {
  const sessions = Array.from({ length: 50 }, (_, i) => ({
    handle: `scale-${i}`,
    displayName: `Session ${i}`,
    state: "idle",
    launchedBy: null,
    chatWorkspaceId: null,
    groupUnread: 0,
    groupReadId: 0,
    queueRole: null,
    roleJoinError: null,
    unread: 100,
    oldestUnreadAt: null,
    mailNotice: "pending",
    notifiedAt: null,
    inboxWaiter: true,
    machine: i < 25 ? "This Mac" : "Emulator",
    machineId: i < 25 ? undefined : "emulator",
  })) as SessionListRow[];
  const messages = Array.from({ length: 5000 }, (_, i) => ({
    id: i + 1,
    to: `scale-${i % 50}`,
    text: `Message ${i}`,
  }));
  const counts = {
    id: "counts:team",
    rowType: "counts",
    team: { org: "acme/widgets", team: "default" },
    humanUnread: 0,
    recipients: Object.fromEntries(sessions.map((s) => [s.handle, 100])),
    needsYouBySender: {},
    sessions: Object.fromEntries(
      sessions.map((s) => [s.handle, { unread: 100, groupUnread: 0, groupReadId: 0 }]),
    ),
  };
  const inboxRows = () => [
    counts,
    ...messages
      .slice()
      .reverse()
      .map((m) => ({
        id: `message:${m.id}`,
        messageId: m.id,
        rowType: "message",
        text: m.text,
        to: m.to,
      })),
  ];
  const subscriptions = new Map<
    string,
    {
      topic: string;
      viewKey: string;
      window?: [number, number];
      loaded: Map<string, Record<string, unknown>>;
      rev: number;
    }
  >();
  let statusMode = false;
  let statusSessions: HubSnapshot = statusPackets.initial.sessions;
  let statusOrder: HubSnapshot = statusPackets.initial.order;
  let inboxRev = 0;
  let reads = 0;
  let subscribeReads = 0;
  let rev = 0;
  const roster = () =>
    sessions.map((s) => ({
      handle: s.handle,
      displayName: s.displayName,
      sessionId: s.handle,
      alive: true,
    })) as AgentSessionRow[];
  let agentPane = false;
  let agentRev = 0;
  let pinnedRefs: string[] = [];
  const agents = sessions.map((session, index) => ({
    id: JSON.stringify(["acme/widgets:default", session.handle]),
    sessionRef: session.handle,
    inboxRef: "counts:team:acme/widgets/default",
    teamKey: "acme/widgets:default",
    parentId: null,
    parentSource: null,
    order: index,
    workspaceRef: null as string | null,
    workspace: null as { id: string; label: string } | null,
    storyRef: null,
    workspaceRank: 0,
    crewParentId: null,
    crewOrder: 0,
    crewLead: false,
  }));
  const api = {
    sessions,
    startAgentPane: (options: { pinned?: number[] } = {}) => {
      agentPane = true;
      pinnedRefs = (options.pinned || []).map((index) => agents[index].id);
      sessions.forEach((session, index) =>
        Object.assign(session, {
          id: session.handle,
          sessionId: session.handle,
          alive: true,
          effectiveStatus: "idle",
          team: { org: "acme/widgets", team: "default" },
          rowNames: { primary: `Session ${index}`, full: `Session ${index}` },
        }),
      );
      agents.forEach((row) =>
        Object.assign(row, {
          workspaceRef: "scale-work",
          workspace: { id: "scale-work", label: "Scale work 0" },
        }),
      );
    },
    startStatus: () => {
      statusMode = true;
      statusSessions = statusPackets.initial.sessions;
      statusOrder = statusPackets.initial.order;
    },
    fireStatusStep: (index: number) => {
      const step = statusPackets.steps[index];
      const apply = (snapshot: HubSnapshot, deltas: readonly HubDelta[]): HubSnapshot => {
        const rows = new Map(snapshot.snapshot.map((row) => [row.id, row]));
        let rev = snapshot.rev;
        for (const delta of deltas) {
          rev = Math.max(rev + 1, delta.rev);
          for (const id of delta.removes) rows.delete(id);
          for (const row of delta.upserts) rows.set(row.id, row);
          events.emit("codesense:delta", { ...delta, rev });
        }
        return { ...snapshot, rev, snapshot: [...rows.values()] };
      };
      statusSessions = apply(statusSessions, step.sessionDeltas);
      statusOrder = apply(statusOrder, step.orderDeltas);
      return step.reorderBytes;
    },
    messages,
    hosts: ["local", "emulator"],
    counters: () => ({ reads, subscribeReads }),
    reset: () => {
      reads = 0;
      subscribeReads = 0;
    },
    fireAgentList: () => {
      agents[0] = agentPane
        ? {
            ...agents[0],
            workspace: { id: "scale-work", label: `Scale work ${(agentRev + 1) % 2}` },
          }
        : { ...agents[0], workspaceRank: agents[0].workspaceRank === 0 ? 1 : 0 };
      const delta = {
        topic: "agentList",
        viewKey: agentPane ? "all" : "team:acme/widgets/default",
        rev: ++agentRev,
        upserts: [agents[0]],
        removes: [],
      };
      events.emit("codesense:delta", delta);
      for (const [id, subscription] of subscriptions) {
        if (subscription.topic !== "agentList") continue;
        const selected = subscription.window ? agents.slice(...subscription.window) : agents;
        const upserts = selected.filter(
          (row) => JSON.stringify(row) !== JSON.stringify(subscription.loaded.get(row.id)),
        );
        if (!upserts.length) continue;
        events.emit("codesense:subscription-delta", {
          subscriptionId: id,
          baseRev: subscription.rev,
          delta: { ...delta, viewKey: subscription.viewKey, upserts, total: agents.length },
        });
        subscription.loaded = new Map(selected.map((row) => [row.id, structuredClone(row)]));
        subscription.rev = agentRev;
      }
      return new TextEncoder().encode(JSON.stringify(delta)).length;
    },
    fireInbox: () => {
      const message = messages[messages.length - 1]!;
      message.text = `Changed ${Date.now()}`;
      inboxRev++;
      for (const [id, subscription] of subscriptions) {
        if (subscription.topic !== "inbox") continue;
        const all = inboxRows();
        const selected = subscription.window
          ? all.slice(...subscription.window)
          : all.filter((r) => subscription.loaded.has(r.id));
        const next = new Map(
          selected.map((r) => [r.id, structuredClone(r) as Record<string, unknown>]),
        );
        const upserts = selected.filter(
          (r) => JSON.stringify(r) !== JSON.stringify(subscription.loaded.get(r.id)),
        );
        const removes = [...subscription.loaded.keys()].filter((id) => !next.has(id));
        if (!upserts.length && !removes.length) continue;
        events.emit("codesense:subscription-delta", {
          subscriptionId: id,
          baseRev: subscription.rev,
          delta: {
            topic: "inbox",
            viewKey: subscription.viewKey,
            rev: inboxRev,
            upserts,
            removes,
            total: all.length,
          },
        });
        subscription.loaded = next;
        subscription.rev = inboxRev;
      }
    },
    fire: (idleHeartbeat = false) => {
      sessions[0] = { ...sessions[0], displayName: `Session ${Date.now()}` };
      if (idleHeartbeat) Object.assign(sessions[1], { heartbeatAtMs: Date.now() });
      useAgentListProjectionStore.setState({ sessions: roster() });
      events.emit("session-state-changed", { sessionId: "scale-0" });
      const delta = {
        topic: "sessions",
        viewKey: "all",
        rev: ++rev,
        upserts: (idleHeartbeat ? sessions.slice(0, 2) : sessions.slice(0, 1)).map((row) => ({
          id: row.handle,
          ...row,
        })),
        removes: [],
      };
      events.emit("codesense:delta", delta);
      if (idleHeartbeat)
        for (const [id, subscription] of subscriptions) {
          if (subscription.topic !== "sessions") continue;
          const all = sessions.map((row) => ({ id: row.handle, ...row }));
          const selected = subscription.window ? all.slice(...subscription.window) : all;
          const upserts = selected.filter(
            (row) => JSON.stringify(row) !== JSON.stringify(subscription.loaded.get(row.id)),
          );
          if (!upserts.length) continue;
          events.emit("codesense:subscription-delta", {
            subscriptionId: id,
            baseRev: subscription.rev,
            delta: {
              topic: "sessions",
              viewKey: subscription.viewKey,
              rev,
              upserts,
              removes: [],
              total: all.length,
            },
          });
          subscription.loaded = new Map(selected.map((row) => [row.id, structuredClone(row)]));
          subscription.rev = rev;
        }
      return new TextEncoder().encode(JSON.stringify(delta)).length;
    },
    fireBurst: (count = 4) => {
      const changed = sessions.slice(0, count).map((row, index) => {
        const next = { ...row, displayName: `${row.displayName} burst ${rev + index + 1}` };
        sessions[index] = next;
        return {
          topic: "sessions",
          viewKey: "all",
          rev: ++rev,
          upserts: [{ id: next.handle, ...next }],
          removes: [],
        };
      });
      useAgentListProjectionStore.setState({ sessions: roster() });
      for (const delta of changed) {
        events.emit("session-state-changed", { sessionId: String(delta.upserts[0].id) });
        events.emit("codesense:delta", delta);
      }
      return changed.length;
    },
  };
  Object.assign(window, { __liveHubFixture: api });
  useAgentListProjectionStore.setState({ sessions: roster() });
  return {
    live_hub_subscribe: (args) => {
      reads++;
      subscribeReads++;
      const topic = String(args.topic);
      const window = args.window as [number, number] | undefined;
      const rows =
        statusMode && topic === "sessions"
          ? statusSessions.snapshot
          : statusMode && topic === "agentList"
            ? statusOrder.snapshot
            : topic === "inbox"
              ? inboxRows()
              : topic === "ui:dock"
                ? [{ id: "ui:dock", arrange: "teams", folds: {}, pins: {} }]
                : topic === "agentList"
                  ? agentPane
                    ? [
                        ...agents,
                        {
                          id: "agentList:state",
                          rowType: "state",
                          pane: { pinnedRefs },
                          ready: true,
                          order: Number.MAX_VALUE,
                          teamOrder: ["acme/widgets:default"],
                          teamNames: {
                            "acme/widgets:default": { org: "acme/widgets", team: "default" },
                          },
                        },
                      ]
                    : agents
                  : sessions.map((s) => ({ id: s.handle, ...s }));
      const snapshot = window
        ? rows.slice(...window)
        : rows.slice(0, topic === "inbox" ? 200 : rows.length);
      const currentRev =
        statusMode && topic === "sessions"
          ? statusSessions.rev
          : statusMode && topic === "agentList"
            ? statusOrder.rev
            : topic === "inbox"
              ? inboxRev
              : topic === "agentList"
                ? agentRev
                : rev;
      if (args.subscriptionId)
        subscriptions.set(String(args.subscriptionId), {
          topic,
          viewKey: String(args.viewKey),
          window,
          loaded: new Map(snapshot.map((r) => [r.id, structuredClone(r)])),
          rev: currentRev,
        });
      return {
        topic,
        viewKey: String(args.viewKey),
        rev: currentRev,
        snapshot,
        total: rows.length,
        cursor: topic === "inbox" && !window ? String(snapshot.length) : undefined,
      };
    },
    live_hub_unsubscribe: (args) => {
      subscriptions.delete(String(args.subscriptionId));
    },
    live_hub_window: (args) => {
      const subscription = subscriptions.get(String(args.subscriptionId))!;
      const window = args.window as [number, number];
      const all =
        subscription.topic === "inbox"
          ? inboxRows()
          : subscription.topic === "agentList"
            ? agents
            : sessions.map((row) => ({ id: row.handle, ...row }));
      const selected = all.slice(...window);
      const next = new Map(selected.map((r) => [r.id, structuredClone(r)]));
      const upserts = selected.filter((r) => !subscription.loaded.has(r.id));
      const removes = [...subscription.loaded.keys()].filter((id) => !next.has(id));
      subscription.loaded = next;
      subscription.window = window;
      return {
        topic: subscription.topic,
        viewKey: subscription.viewKey,
        rev:
          subscription.topic === "inbox"
            ? inboxRev
            : subscription.topic === "agentList"
              ? agentRev
              : rev,
        upserts,
        removes,
        total: all.length,
      };
    },
    live_hub_page: (args) => {
      const subscription = subscriptions.get(String(args.subscriptionId))!;
      const offset = Number(args.cursor);
      const all = inboxRows();
      const snapshot = all.slice(offset, offset + 200);
      for (const row of snapshot) subscription.loaded.set(row.id, structuredClone(row));
      return {
        topic: "inbox",
        viewKey: subscription.viewKey,
        rev: inboxRev,
        snapshot,
        total: all.length,
        cursor: offset + 200 < all.length ? String(offset + 200) : undefined,
      };
    },
    agent_read_sessions: () => {
      reads++;
      return structuredClone(sessions);
    },
    dashboard_list_sessions: () => roster(),
    wv_list_roster: () => [],
  };
}
