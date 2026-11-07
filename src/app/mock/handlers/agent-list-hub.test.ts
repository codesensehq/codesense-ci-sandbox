// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";
import { MockEventBus } from "../mock-events";
import { agentListHub, agentListRows, agentListStatusRows } from "./agent-list-hub";

const session = (
  handle: string,
  launchedBy?: string,
  extra: Partial<AgentSessionRow> = {},
): AgentSessionRow => ({
  handle,
  sessionId: `sess-${handle}`,
  kind: "dashboard-launched",
  alive: true,
  status: "running",
  backgrounded: false,
  startedWith: null,
  startedAtMs: 0,
  launcher: "codesense",
  agentRevision: null,
  lastOutputMs: null,
  launchedBy,
  ...extra,
});
const story: Workspace = { id: "S-1", kind: "story", cwd: "/s1", status: "ready", storyId: "S-1" };
const other: Workspace = { id: "S-0", kind: "story", cwd: "/s0", status: "ready", storyId: "S-0" };

describe("the mock agent list projection", () => {
  const rows = agentListRows(
    [session("lead"), session("worker", "lead"), session("solo")],
    new Map([
      ["sess-lead", { team: "o:t", workspace: story }],
      ["sess-worker", { team: "o:t", workspace: story }],
      ["sess-solo", { team: "o:t", workspace: other }],
    ]),
    ["o:t"],
    { "o:t": ["S-0", "S-1"] },
  );
  const byRef = (ref: string) => rows.find((r) => r.sessionRef === `sess-${ref}`)!;

  it("leads a workspace's crew with the launcher that has the most agents under it", () => {
    expect([byRef("lead").crewLead, byRef("worker").crewLead]).toEqual([true, false]);
    expect(byRef("worker").crewParentId).toBe(byRef("lead").id);
    expect([byRef("lead").crewOrder, byRef("worker").crewOrder]).toEqual([0, 1]);
  });

  it("ranks workspace cards in the order the viewer resolved", () => {
    expect([byRef("solo").workspaceRank, byRef("lead").workspaceRank]).toEqual([0, 1]);
  });
});

describe("the mock agent list ordering and membership", () => {
  const place = (sessions: AgentSessionRow[], ws: Workspace = story) =>
    new Map(sessions.map((s) => [s.sessionId!, { team: "o:t", workspace: ws }]));
  const project = (sessions: AgentSessionRow[]) => {
    const rows = agentListRows(sessions, place(sessions), ["o:t"]);
    return (ref: string) => rows.find((r) => r.sessionRef === `sess-${ref}`);
  };

  it("leaves ended sessions out of the list and its crews", () => {
    const get = project([
      session("lead", undefined, { startedAtMs: 3 }),
      session("gone", "lead", { alive: false, startedAtMs: 2 }),
      session("worker", "lead", { startedAtMs: 1 }),
    ]);
    expect(get("gone")).toBeUndefined();
    expect([get("lead")!.crewOrder, get("worker")!.crewOrder]).toEqual([0, 1]);
  });

  it("orders newest first, roots with children before childless roots", () => {
    const rows = agentListRows(
      [
        session("newest", undefined, { startedAtMs: 9 }),
        session("old-lead", undefined, { startedAtMs: 1 }),
        session("old-kid", "old-lead", { startedAtMs: 2 }),
        session("new-kid", "old-lead", { startedAtMs: 5 }),
      ],
      new Map([
        ["sess-newest", { team: "o:t", workspace: story }],
        ["sess-old-lead", { team: "o:t", workspace: story }],
        ["sess-old-kid", { team: "o:t", workspace: story }],
        ["sess-new-kid", { team: "o:t", workspace: story }],
      ]),
      ["o:t"],
    );
    expect(rows.map((r) => r.sessionRef)).toEqual([
      "sess-old-lead",
      "sess-new-kid",
      "sess-old-kid",
      "sess-newest",
    ]);
  });

  it("breaks an equal-size lead tie toward the newer root", () => {
    const get = project([
      session("older", undefined, { startedAtMs: 1 }),
      session("newer", undefined, { startedAtMs: 2 }),
    ]);
    expect([get("newer")!.crewLead, get("older")!.crewLead]).toEqual([true, false]);
    expect([get("newer")!.crewOrder, get("older")!.crewOrder]).toEqual([0, 1]);
  });
});

describe("the mock Status view", () => {
  const sessions = [
    session("idle-b", undefined, { effectiveStatus: "idle" }),
    session("work", undefined, { effectiveStatus: "running" }),
    session("ask", undefined, { effectiveStatus: "awaiting-answer" }),
    session("idle-a", undefined, { effectiveStatus: "completed" }),
  ];
  const all = agentListRows(
    sessions,
    new Map(sessions.map((s) => [s.sessionId!, { team: "o:t", workspace: story }])),
    ["o:t"],
  );
  const rows = agentListStatusRows(all, sessions);

  it("ranks Needs answer, then Working, then Ready", () => {
    expect(rows.map((r) => [r.sessionRef, r.statusGroup])).toEqual([
      ["sess-ask", 0],
      ["sess-work", 1],
      ["sess-idle-a", 2],
      ["sess-idle-b", 2],
    ]);
  });

  it("numbers the rows in that order", () => {
    expect(rows.map((r) => r.order)).toEqual([0, 1, 2, 3]);
  });
});

describe("the mock Status view and unread mail", () => {
  const sessions = [
    session("dev", undefined, { effectiveStatus: "running" }),
    session("lead", undefined, { effectiveStatus: "tool-running" }),
    session("ask", undefined, { effectiveStatus: "awaiting-answer" }),
    session("zed", undefined, { effectiveStatus: "running" }),
  ];
  const placements = new Map(
    sessions.map((s) => [s.sessionId!, { team: "o:t", workspace: story }]),
  );
  const all = agentListRows(sessions, placements, ["o:t"]);

  it("puts a Working agent with unread mail ahead of its zero-unread peers", () => {
    const rows = agentListStatusRows(all, sessions, { lead: 1 });
    expect(rows.map((r) => r.sessionRef)).toEqual([
      "sess-ask",
      "sess-lead",
      "sess-dev",
      "sess-zed",
    ]);
  });

  it("ranks more unread first, then falls back to the handle", () => {
    const rows = agentListStatusRows(all, sessions, { zed: 2, lead: 2, dev: 1 });
    expect(rows.map((r) => r.sessionRef)).toEqual([
      "sess-ask",
      "sess-lead",
      "sess-zed",
      "sess-dev",
    ]);
  });

  it("never lets unread mail cross a status group", () => {
    const rows = agentListStatusRows(all, sessions, { ask: 0, dev: 9 });
    expect(rows[0].sessionRef).toBe("sess-ask");
  });

  it("re-projects the view when the inbox changes", () => {
    const bus = new MockEventBus();
    const deltas: Array<{ topic: string; viewKey: string; upserts: Array<{ id: string }> }> = [];
    bus.bind((_callback, payload) =>
      deltas.push((payload as { payload: (typeof deltas)[number] }).payload),
    );
    bus.handlers()["plugin:event|listen"]({ event: "codesense:delta", handler: 1 });
    let unread: Record<string, number> = {};
    const hub = agentListHub(bus, {
      sessions: () => sessions,
      placements: new Map(
        sessions.map((s) => [s.sessionId!, { team: "o:t", workspaceId: story.id }]),
      ),
      workspaces: { "o:t": [story] },
      teamOrder: ["o:t"],
      statusFields: () => ({}),
      unread: () => unread,
    });
    const refs = () =>
      (hub.snapshot("agentList", "all|status")!.snapshot as Array<{ sessionRef?: string }>)
        .map((r) => r.sessionRef)
        .filter(Boolean);
    expect(refs()).toEqual(["sess-ask", "sess-dev", "sess-lead", "sess-zed"]);
    unread = { lead: 1 };
    bus.emit("agent-inbox-changed", null);
    expect(refs()).toEqual(["sess-ask", "sess-lead", "sess-dev", "sess-zed"]);
    expect(deltas.filter((d) => d.viewKey === "all|status").length).toBe(1);
    expect(deltas.some((d) => d.viewKey === "all")).toBe(false);
  });
});

describe("the mock Status view ranks every status like Rust `AgentStatus::rank`", () => {
  const expected: Record<string, 0 | 1 | 2> = {
    "awaiting-answer": 0,
    paused: 0,
    blocked: 0,
    dropped: 0,
    failed: 0,
    crashed: 0,
    errored: 0,
    running: 1,
    thinking: 1,
    "tool-running": 1,
    idle: 2,
    stalled: 2,
    unknown: 2,
    completed: 2,
    done: 2,
  };

  it.each(Object.entries(expected))("%s sits in group %i", (status, group) => {
    const sessions = [session("a", undefined, { effectiveStatus: status as never })];
    const rows = agentListRows(sessions, new Map([["sess-a", { team: "o:t", workspace: story }]]), [
      "o:t",
    ]);
    expect(agentListStatusRows(rows, sessions)[0].statusGroup).toBe(group);
  });
});

describe("the mock agentList view's spend rows", () => {
  it("serves each scope's spend as a `spend` row, as the backend projects it", () => {
    const hub = agentListHub(new MockEventBus(), {
      sessions: () => [],
      placements: new Map(),
      workspaces: {},
      teamOrder: ["o:t"],
      statusFields: () => ({}),
      spend: () => ({
        "agent:o/t/triage": {
          today: 40,
          week: 80,
          cap: { perDay: 50, perWeek: 200 },
          state: "warn",
          pending: false,
          requestedCap: null,
          unpricedModels: [],
        },
      }),
    });
    const rows = hub.snapshot("agentList", "all")!.snapshot as Array<Record<string, unknown>>;
    const spend = rows.filter((r) => r.rowType === "spend");
    expect(spend).toHaveLength(1);
    expect(spend[0]).toMatchObject({ scopeKind: "agent", key: "o/t/triage" });
    expect((spend[0].spend as { today: number }).today).toBe(40);
  });
});
