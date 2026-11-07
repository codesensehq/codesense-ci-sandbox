// SPDX-License-Identifier: MIT
import { fixtureTeamNodes } from "./agent-list-test-fixture";
import { describe, expect, it } from "vitest";

import { type AgentTreeNode } from "@/features/viewer/lib/agent-list-view";
import {
  buildPaneRows,
  paneRowPlaceWords,
  paneRowSemantics,
  type PaneRowsInput,
} from "@/features/viewer/lib/agents-pane-rows";
import type { PlannedTeam } from "@/features/viewer/lib/agents-pane-plan";
import { agentRowKey } from "@/shared/lib/agent-pins";
import { managerFoldKey, teamFoldKey, workspaceFoldKey } from "@/shared/lib/workspace-key";
import type { AgentSessionRow } from "@/tauri/commands";
import { flattenNodes } from "@/features/agents/lib/agent-list-layout";

function row(handle: string, over: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle,
    sessionId: `sid-${handle}`,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: 1_000,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

function teamOf(
  teamKey: string,
  rows: AgentSessionRow[],
  wsOf: Record<string, string>,
): PlannedTeam {
  const nodes: AgentTreeNode[] = fixtureTeamNodes(teamKey, rows, wsOf);
  const launchers: Record<string, string> = {};
  const walk = (ns: readonly AgentTreeNode[]) =>
    ns.forEach((n) => {
      for (const c of n.children) launchers[c.row.handle] = n.row.handle;
      walk(n.children);
    });
  walk(nodes);
  return { teamKey, rows: nodes, launchers };
}

/** Each agent row's handle, indent and the launcher it is drawn under. */
function agentDepths(rows: ReturnType<typeof buildPaneRows>) {
  return rows.flatMap((r) =>
    r.kind === "agent" ? [[r.node.row.handle, r.depth, r.parentHandle] as const] : [],
  );
}

function input(over: Partial<PaneRowsInput> = {}): PaneRowsInput {
  return {
    pinned: [],
    pinnedExpanded: false,
    pinnedVisibleMax: 5,
    teams: [],
    arrange: "teams",
    statusRows: [],
    collapsed: undefined,
    needs: new Set(),
    arrowFilter: null,
    filtering: false,
    ...over,
  };
}

describe("buildPaneRows", () => {
  it("lists each team, then its workspaces, then their agents, in tree order", () => {
    const team = teamOf(
      "team-1",
      [row("lead"), row("worker", { launchedBy: "lead" }), row("solo")],
      { lead: "ws-a", worker: "ws-a", solo: "ws-b" },
    );
    const keys = buildPaneRows(input({ teams: [team] })).map((r) => r.key);
    expect(keys).toEqual([
      "team:team-1",
      "ws:team-1|ws-a",
      `agent:${agentRowKey("team-1", "lead")}`,
      `agent:${agentRowKey("team-1", "worker")}`,
      "ws:team-1|ws-b",
      `agent:${agentRowKey("team-1", "solo")}`,
    ]);
  });

  it("indents a launched agent one level below its launcher", () => {
    const team = teamOf("team-1", [row("lead"), row("worker", { launchedBy: "lead" })], {
      lead: "ws-a",
      worker: "ws-a",
    });
    expect(agentDepths(buildPaneRows(input({ teams: [team] })))).toEqual([
      ["lead", 0, null],
      ["worker", 1, "lead"],
    ]);
  });

  it("keeps a worker whose launcher works in another card at its own card's top level", () => {
    const team = teamOf(
      "team-1",
      [row("lead"), row("mid", { launchedBy: "lead" }), row("leaf", { launchedBy: "mid" })],
      { lead: "ws-a", mid: "ws-b", leaf: "ws-a" },
    );
    expect(agentDepths(buildPaneRows(input({ teams: [team] })))).toEqual([
      ["lead", 0, null],
      ["leaf", 0, null],
      ["mid", 0, null],
    ]);
  });

  it("keeps a worker at the top level when the filters leave its launcher out", () => {
    const team = teamOf(
      "team-1",
      [row("lead"), row("mid", { launchedBy: "lead" }), row("leaf", { launchedBy: "mid" })],
      { lead: "ws-a", mid: "ws-a", leaf: "ws-a" },
    );
    // The plan kept only the lead and the leaf: the leaf is not drawn under the lead.
    const kept = {
      ...team,
      rows: flattenNodes(team.rows)
        .filter((n) => n.row.handle !== "mid")
        .map((n) => ({ ...n, children: [] })),
    };
    expect(agentDepths(buildPaneRows(input({ teams: [kept] })))).toEqual([
      ["lead", 0, null],
      ["leaf", 0, null],
    ]);
  });

  it("drops a folded team's and a folded workspace's rows, but keeps their headers", () => {
    const team = teamOf("team-1", [row("a"), row("b")], { a: "ws-a", b: "ws-b" });
    const folded = buildPaneRows(
      input({ teams: [team], collapsed: { [teamFoldKey("team-1")]: true } }),
    );
    expect(folded.map((r) => r.key)).toEqual(["team:team-1"]);

    const wsFolded = buildPaneRows(
      input({ teams: [team], collapsed: { [workspaceFoldKey("team-1", "ws-a")]: true } }),
    );
    expect(wsFolded.map((r) => r.key)).toEqual([
      "team:team-1",
      "ws:team-1|ws-a",
      "ws:team-1|ws-b",
      `agent:${agentRowKey("team-1", "b")}`,
    ]);
  });

  it("opens a folded team during a search, and leaves its saved fold alone", () => {
    const team = teamOf("team-1", [row("a"), row("b")], { a: "ws-a", b: "ws-a" });
    const rows = buildPaneRows(
      input({ teams: [team], filtering: true, collapsed: { [teamFoldKey("team-1")]: true } }),
    );
    expect(rows[0]).toMatchObject({ kind: "team", folded: false });
    expect(rows.some((r) => r.key === `agent:${agentRowKey("team-1", "a")}`)).toBe(true);
  });

  it("with the status arrangement, lists sections and their agents with no team headers", () => {
    const node = {
      row: row("busy"),
      workspace: null,
      children: [],
      story: null,
    } as unknown as AgentTreeNode;
    const rows = buildPaneRows(
      input({ arrange: "status", statusRows: [{ node, teamKey: "team-1", rank: 1 }] }),
    );
    expect(rows.map((r) => r.key)).toEqual([
      "status:working",
      `status-agent:${agentRowKey("team-1", "busy")}`,
    ]);
  });

  it("keys a status row by its team, so one handle in two teams gives two rows", () => {
    const node = (h: string) =>
      ({ row: row(h), workspace: null, children: [], story: null }) as unknown as AgentTreeNode;
    const rows = buildPaneRows(
      input({
        arrange: "status",
        statusRows: [
          { node: node("twin"), teamKey: "team-1", rank: 1 },
          { node: node("twin"), teamKey: "acme:ops", rank: 1 },
        ],
      }),
    );
    const keys = rows.filter((r) => r.kind === "status-agent").map((r) => r.key);
    expect(keys).toEqual([
      `status-agent:${agentRowKey("team-1", "twin")}`,
      `status-agent:${agentRowKey("acme:ops", "twin")}`,
    ]);
    expect(new Set(keys).size).toBe(2);
  });

  it('keeps team "a:b" with handle "c" apart from team "a" with handle "b:c"', () => {
    const left = teamOf("a:b", [row("c")], { c: "ws-a" });
    const right = teamOf("a", [row("b:c")], { "b:c": "ws-a" });
    const agents = buildPaneRows(input({ teams: [left, right] })).filter((r) => r.kind === "agent");
    expect(agents.map((r) => r.key)).toEqual([
      `agent:${agentRowKey("a:b", "c")}`,
      `agent:${agentRowKey("a", "b:c")}`,
    ]);
    expect(new Set(agents.map((r) => r.key)).size).toBe(2);
  });

  it("puts the Pinned group first as one row, cut short with a '+n more' count", () => {
    const team = teamOf("team-1", [row("a")], { a: "ws-a" });
    const entries = Array.from({ length: 7 }, (_, i) => ({
      teamKey: "team-1",
      id: `e${i}`,
      pin: {
        kind: "session" as const,
        teamKey: "team-1",
        handle: `p${i}`,
        sessionId: null,
        name: `n${i}`,
      },
      nodes: [
        {
          row: row(`p${i}`),
          workspace: null,
          children: [],
          story: null,
        } as unknown as AgentTreeNode,
      ],
    }));
    const rows = buildPaneRows(input({ pinned: entries, teams: [team], pinnedVisibleMax: 5 }));
    expect(rows[0]).toMatchObject({ kind: "pinned", key: "pinned", more: 2, expanded: false });
    expect(rows[0].kind === "pinned" && rows[0].entries).toHaveLength(5);
  });
});

describe("buildPaneRows while searching", () => {
  it("keeps a folded workspace closed when no filter is on", () => {
    const team = teamOf("team-1", [row("a")], { a: "ws-a" });
    const keys = buildPaneRows(
      input({
        teams: [team],
        collapsed: { [workspaceFoldKey("team-1", "ws-a")]: true },
      }),
    ).map((r) => r.key);
    expect(keys).not.toContain(`agent:${agentRowKey("team-1", "a")}`);
    expect(keys).toContain("ws:team-1|ws-a");
  });

  it("opens a folded workspace during a search, so its matches show under it", () => {
    const team = teamOf("team-1", [row("a")], { a: "ws-a" });
    const keys = buildPaneRows(
      input({
        teams: [team],
        filtering: true,
        collapsed: { [workspaceFoldKey("team-1", "ws-a")]: true },
      }),
    ).map((r) => r.key);
    expect(keys).toContain(`agent:${agentRowKey("team-1", "a")}`);
  });

  it("keeps every row's key its own when one handle is listed twice in a team, by its session", () => {
    const node = (sid: string): AgentTreeNode => ({
      row: row("twin", { sessionId: sid }),
      story: null,
      workspace: { id: "ws-a", label: "ws-a", kind: "folder" },
      children: [],
    });
    const team: PlannedTeam = { teamKey: "team-1", rows: [node("s1"), node("s2")], launchers: {} };
    const agents = buildPaneRows(input({ teams: [team] })).filter((r) => r.kind === "agent");
    const base = `agent:${agentRowKey("team-1", "twin")}`;
    // The first keeps the plain key; the second names its session.
    expect(agents.map((r) => r.key)).toEqual([base, `${base}|s2`]);
    const statusRows = [node("s1"), node("s2")].map((n) => ({
      node: n,
      teamKey: "team-1",
      rank: 1 as const,
    }));
    const status = buildPaneRows(input({ arrange: "status", statusRows }))
      .filter((r) => r.kind === "status-agent")
      .map((r) => r.key);
    expect(new Set(status).size).toBe(2);
  });
});

describe("paneRowSemantics", () => {
  it("gives every row its place in the whole list and a header its heading level", () => {
    const one = teamOf("team-1", [row("lead"), row("worker", { launchedBy: "lead" })], {
      lead: "ws-a",
      worker: "ws-a",
    });
    const two = teamOf("acme:ops", [row("solo"), row("loose")], { solo: "ws-x" });
    const rows = buildPaneRows(input({ teams: [one, two] }));
    const sem = paneRowSemantics(rows);
    expect(sem.map((s) => [s.posInSet, s.setSize])).toEqual(
      rows.map((_, i) => [i + 1, rows.length]),
    );
    expect(rows.map((r, i) => [r.kind, sem[i]!.headingLevel])).toEqual([
      ["team", 3],
      ["ws", 4],
      ["agent", null],
      ["agent", null],
      ["team", 3],
      ["ws", 4],
      ["agent", null],
      ["ws", 4],
      ["agent", null],
    ]);
    expect(sem.map((s) => s.places)).toEqual([
      [],
      [],
      [{ teamKey: "team-1", workspace: "ws-a", underHandle: null, group: null }],
      [{ teamKey: "team-1", workspace: "ws-a", underHandle: "lead", group: null }],
      [],
      [],
      [{ teamKey: "acme:ops", workspace: "ws-x", underHandle: null, group: null }],
      [],
      [{ teamKey: "acme:ops", workspace: null, underHandle: null, group: null }],
    ]);
  });

  it("reads the place of a row far below what a virtual list draws from the rows alone", () => {
    const handles = Array.from({ length: 150 }, (_, i) => `a-${String(i).padStart(3, "0")}`);
    const wsOf = Object.fromEntries(handles.map((h, i) => [h, i < 75 ? "ws-a" : "ws-b"]));
    const team = teamOf(
      "team-1",
      handles.map((h) => row(h)),
      wsOf,
    );
    const rows = buildPaneRows(input({ teams: [team] }));
    const sem = paneRowSemantics(rows);
    // Team, card, 75 agents, card, 75 agents.
    expect(rows).toHaveLength(153);
    const last = rows.length - 1;
    expect(rows[last]!.kind).toBe("agent");
    expect(sem[last]).toEqual({
      posInSet: 153,
      setSize: 153,
      headingLevel: null,
      places: [{ teamKey: "team-1", workspace: "ws-b", underHandle: null, group: null }],
    });
    const secondCard = rows.findIndex((r) => r.key === "ws:team-1|ws-b");
    expect(sem[secondCard]).toMatchObject({ posInSet: 78, headingLevel: 4 });
  });

  it("names a status row's section and a pinned row's group, with each one's own team", () => {
    const node = (h: string, ws: string | null) =>
      ({
        row: row(h),
        workspace: ws ? { id: ws, label: ws, kind: "folder" } : null,
        children: [],
        story: null,
      }) as unknown as AgentTreeNode;
    const status = buildPaneRows(
      input({
        arrange: "status",
        statusRows: [
          { node: node("ask", "ws-a"), teamKey: "team-1", rank: 0 },
          { node: node("busy", null), teamKey: "acme:ops", rank: 1 },
        ],
      }),
    );
    const statusSem = paneRowSemantics(status);
    expect(status.map((r, i) => [r.kind, statusSem[i]!.headingLevel])).toEqual([
      ["status-head", 3],
      ["status-agent", null],
      ["status-head", 3],
      ["status-agent", null],
    ]);
    expect(statusSem[1]!.places).toEqual([
      { teamKey: "team-1", workspace: "ws-a", underHandle: null, group: "Needs you" },
    ]);
    expect(statusSem[3]!.places).toEqual([
      { teamKey: "acme:ops", workspace: null, underHandle: null, group: "Working" },
    ]);

    const pinned = buildPaneRows(
      input({
        pinned: [
          {
            teamKey: "acme:ops",
            id: "e0",
            pin: {
              kind: "session" as const,
              teamKey: "acme:ops",
              handle: "p0",
              sessionId: null,
              name: "p0",
            },
            nodes: [node("p0", "ws-x")],
          },
        ],
      }),
    );
    expect(paneRowSemantics(pinned)[0]).toEqual({
      posInSet: 1,
      setSize: 1,
      headingLevel: 3,
      places: [{ teamKey: "acme:ops", workspace: "ws-x", underHandle: null, group: "Pinned" }],
    });
  });
});

describe("paneRowPlaceWords", () => {
  it("says the group, the team, the workspace and the launcher, leaving out what is not there", () => {
    expect(
      paneRowPlaceWords(
        { teamKey: "team-1", workspace: "ws-a", underHandle: "lead", group: null },
        { teamLabel: "Alpha", underName: "Lead" },
      ),
    ).toBe("Team Alpha · Workspace ws-a · Under Lead");
    expect(
      paneRowPlaceWords(
        { teamKey: "team-1", workspace: null, underHandle: null, group: "Pinned" },
        { teamLabel: "Acme › Ops" },
      ),
    ).toBe("Pinned · Team Acme › Ops · No workspace");
    expect(
      paneRowPlaceWords(
        { teamKey: "team-1", workspace: "ws-a", underHandle: "lead", group: null },
        { teamLabel: "Alpha" },
      ),
    ).toBe("Team Alpha · Workspace ws-a · Under lead");
  });
});

describe("manager arrangement", () => {
  const team = teamOf(
    "team-1",
    [
      row("lead"),
      row("mid", { launchedBy: "lead" }),
      row("leaf", { launchedBy: "mid" }),
      row("solo"),
    ],
    { lead: "ws-a", mid: "ws-b", leaf: "ws-c", solo: "ws-c" },
  );
  it("keeps a nested crew across three workspaces and separates No manager", () => {
    const rows = buildPaneRows(input({ teams: [team], arrange: "manager" }));
    expect(agentDepths(rows)).toEqual([
      ["lead", 0, null],
      ["mid", 1, "lead"],
      ["leaf", 2, "mid"],
      ["solo", 0, null],
    ]);
    expect(rows.some((r) => r.kind === "ws")).toBe(false);
    expect(rows.some((r) => r.kind === "manager-head" && r.label === "No manager")).toBe(true);
    const agents = rows.filter((r) => r.kind === "agent");
    expect(agents.slice(0, 3).map((r) => r.treeHandle)).toEqual(["lead", "lead", "lead"]);
    expect(agents[3].treeHandle).toBeUndefined();
  });
  it("folds managers independently of workspace folds", () => {
    const collapsed = {
      [managerFoldKey("team-1", "mid")]: true,
      [workspaceFoldKey("team-1", "ws-a")]: true,
    };
    expect(
      agentDepths(buildPaneRows(input({ teams: [team], arrange: "manager", collapsed }))),
    ).toEqual([
      ["lead", 0, null],
      ["mid", 1, "lead"],
      ["solo", 0, null],
    ]);
    expect(
      agentDepths(
        buildPaneRows(input({ teams: [team], arrange: "manager", collapsed, filtering: true })),
      ),
    ).toHaveLength(4);
  });
});
