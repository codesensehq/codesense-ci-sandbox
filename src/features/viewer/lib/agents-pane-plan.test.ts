// SPDX-License-Identifier: MIT
import { fixtureTeamNodes } from "./agent-list-test-fixture";
import { describe, expect, it } from "vitest";

import { type AgentTreeNode } from "@/features/viewer/lib/agent-list-view";
import {
  planAgentsPane,
  type PaneRules,
  type PaneTeam,
} from "@/features/viewer/lib/agents-pane-plan";
import { agentRowKey } from "@/shared/lib/agent-pins";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

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

/** One team's launch tree over the given rows, with a workspace per row when given. */
function teamOf(
  teamKey: string,
  rows: AgentSessionRow[],
  wsOf: Record<string, string> = {},
): PaneTeam {
  const nodes = fixtureTeamNodes(teamKey, rows, wsOf);
  return { teamKey, nodes };
}

const statuses: Record<string, AgentStatus> = {};
const rules: PaneRules = {
  statusOf: (r) => statuses[r.handle] ?? "running",
  textOf: (n: AgentTreeNode) => `${n.row.handle} ${n.workspace?.label ?? ""}`,
};
const NO_FILTER = { query: "", status: "all" as const, workspace: null };

describe("planAgentsPane", () => {
  const teams = [
    teamOf("team-1", [row("lead"), row("worker", { launchedBy: "lead" })], {
      lead: "ws-a",
      worker: "ws-a",
    }),
    teamOf("acme:ops", [row("elsewhere")], { elsewhere: "ws-x" }),
  ];

  it("shows every team with its agents, and counts them all when nothing filters", () => {
    const plan = planAgentsPane(teams, [], NO_FILTER, rules);
    expect(plan.teams.map((t) => t.teamKey)).toEqual(["team-1", "acme:ops"]);
    expect(plan.total).toBe(3);
    expect(plan.visible).toBe(3);
    expect(plan.filtering).toBe(false);
    expect(plan.keep).toBeNull();
  });

  it("hiding a team leaves the folds of the rest working", () => {
    const plan = planAgentsPane(teams, ["acme:ops"], NO_FILTER, rules);
    expect(plan.freezesFolds).toBe(false);
  });

  it("drops a hidden team whole, and counts only the teams in view, as the menu's All does", () => {
    const plan = planAgentsPane(teams, ["acme:ops"], NO_FILTER, rules);
    expect(plan.teams.map((t) => t.teamKey)).toEqual(["team-1"]);
    expect(plan.visible).toBe(2);
    expect(plan.total).toBe(2);
    expect(plan.counts.all).toBe(plan.total);
    // Hiding a team is not a filter of the teams in view: the header reads a plain total.
    expect(plan.filtering).toBe(false);
  });

  it("the agent chip keeps its agent's sessions and counts as a filter, so the header reads 1/3", () => {
    const chipTeams = [
      teamOf("team-1", [row("lead", { agentRevision: "reviewer@3" }), row("other")]),
      teamOf("acme:ops", [row("elsewhere")]),
    ];
    const plan = planAgentsPane(chipTeams, [], { ...NO_FILTER, agentKey: "reviewer" }, rules);
    expect(plan.teams.flatMap((t) => t.rows.map((n) => n.row.handle))).toEqual(["lead"]);
    expect(plan.visible).toBe(1);
    expect(plan.total).toBe(3);
    // The Show menu counts what the chip leaves: All shows the one row the list does.
    expect(plan.counts.all).toBe(1);
    expect(plan.filtering).toBe(true);
    expect(plan.chipHiddenTeams).toEqual([]);
  });

  it("names the hidden teams the chip's agent runs in, when it runs in none shown", () => {
    const chipTeams = [
      teamOf("team-1", [row("other")]),
      teamOf("acme:ops", [row("lead", { agentRevision: "reviewer@3" })]),
    ];
    const plan = planAgentsPane(
      chipTeams,
      ["acme:ops"],
      { ...NO_FILTER, agentKey: "reviewer" },
      rules,
    );
    expect(plan.visible).toBe(0);
    expect(plan.chipHiddenTeams).toEqual(["acme:ops"]);
  });

  it("counts each Show bucket after the chip and the arrow, so the bucket in use counts the rows shown", () => {
    statuses.worker = "awaiting-answer";
    statuses.elsewhere = "awaiting-answer";
    try {
      const arrow = { teamKey: "team-1", workspaceId: "ws-a" };
      const all = planAgentsPane(teams, [], { ...NO_FILTER, workspace: arrow }, rules);
      expect(all.counts).toEqual({ all: 2, needs: 1, working: 1, idle: 0 });
      expect(all.counts.all).toBe(all.visible);
      const needs = planAgentsPane(
        teams,
        [],
        { ...NO_FILTER, status: "needs", workspace: arrow },
        rules,
      );
      // The other team's agent waits too, but the arrow leaves only team-1's card.
      expect(needs.visible).toBe(1);
      expect(needs.counts.needs).toBe(needs.visible);
      expect(needs.counts.all).toBe(2);
      // The header reads visible/total over the teams in view.
      expect(needs.total).toBe(3);
    } finally {
      delete statuses.worker;
      delete statuses.elsewhere;
    }
  });

  it("counts after the chip and the arrow together, while the total stays the teams in view", () => {
    const both = [
      teamOf(
        "team-1",
        [
          row("rev-a", { agentRevision: "reviewer@3" }),
          row("rev-b", { agentRevision: "reviewer@3" }),
          row("other"),
        ],
        { "rev-a": "ws-a", "rev-b": "ws-b", other: "ws-a" },
      ),
      teamOf("acme:ops", [row("rev-x", { agentRevision: "reviewer@3" })], { "rev-x": "ws-x" }),
      teamOf("acme:hid", [row("rev-h", { agentRevision: "reviewer@3" })]),
    ];
    const plan = planAgentsPane(
      both,
      ["acme:hid"],
      { ...NO_FILTER, agentKey: "reviewer", workspace: { teamKey: "team-1", workspaceId: "ws-a" } },
      rules,
    );
    expect(plan.teams.flatMap((t) => t.rows.map((n) => n.row.handle))).toEqual(["rev-a"]);
    expect(plan.counts).toEqual({ all: 1, working: 1, needs: 0, idle: 0 });
    expect(plan.total).toBe(4);
    expect(plan.counts.all).not.toBe(plan.total);
    // The chip's agent runs in a shown team too; the hidden team it also runs in is still named.
    expect(plan.chipHiddenTeams).toEqual(["acme:hid"]);
    // With no chip, no hidden team is named.
    expect(planAgentsPane(both, ["acme:hid"], NO_FILTER, rules).chipHiddenTeams).toEqual([]);
  });

  it("freezes folds only for a typed search or the workspace chip, never for a Show filter", () => {
    expect(planAgentsPane(teams, [], { ...NO_FILTER, status: "needs" }, rules).freezesFolds).toBe(
      false,
    );
    expect(planAgentsPane(teams, [], { ...NO_FILTER, query: "lead" }, rules).freezesFolds).toBe(
      true,
    );
    const chip = { teamKey: "team-1", workspaceId: "ws-a" };
    expect(planAgentsPane(teams, [], { ...NO_FILTER, workspace: chip }, rules).freezesFolds).toBe(
      true,
    );
  });

  it("flattens each team to its rows, so the view draws team › workspace › agent", () => {
    const plan = planAgentsPane(teams, [], NO_FILTER, rules);
    const mine = plan.teams[0]!.rows;
    expect(mine.map((n) => n.row.handle)).toEqual(["lead", "worker"]);
    expect(mine.every((n) => n.children.length === 0)).toBe(true);
  });

  it("keeps only the workspace the arrow filter names, and only in its own team", () => {
    const plan = planAgentsPane(
      teams,
      [],
      { ...NO_FILTER, workspace: { teamKey: "team-1", workspaceId: "ws-a" } },
      rules,
    );
    expect(plan.teams.map((t) => t.teamKey)).toEqual(["team-1"]);
    expect(plan.visible).toBe(2);
    expect(plan.filtering).toBe(true);
  });

  it("applies the search and Show filter, and counts what is left", () => {
    statuses.worker = "awaiting-answer";
    try {
      const search = planAgentsPane(teams, [], { ...NO_FILTER, query: "elsewhere" }, rules);
      expect(search.visible).toBe(1);
      expect(search.keep?.has(agentRowKey("acme:ops", "elsewhere"))).toBe(true);
      const needs = planAgentsPane(teams, [], { ...NO_FILTER, status: "needs" }, rules);
      expect(needs.visible).toBe(1);
      expect(needs.counts.needs).toBe(1);
      expect(needs.counts.all).toBe(3);
    } finally {
      delete statuses.worker;
    }
  });

  it("returns no visible rows when nothing matches, with the team list still complete", () => {
    const plan = planAgentsPane(teams, [], { ...NO_FILTER, query: "zzz-nothing" }, rules);
    expect(plan.visible).toBe(0);
    expect(plan.teams).toEqual([]);
    expect(plan.total).toBe(3);
  });
});

describe("planAgentsPane twin handles", () => {
  it("a search or Show match in one team keeps only that team's row of a shared handle", () => {
    const twins = [
      teamOf("team-1", [row("twin", { sessionId: "sid-twin-1" })], { twin: "ws-a" }),
      teamOf("acme:ops", [row("twin", { sessionId: "sid-twin-2" })], { twin: "ws-x" }),
    ];
    const byWs: PaneRules = {
      statusOf: (r) => (r.sessionId === "sid-twin-2" ? "awaiting-answer" : "running"),
      textOf: (n) => `${n.row.handle} ${n.workspace?.label ?? ""}`,
    };
    const search = planAgentsPane(twins, [], { ...NO_FILTER, query: "ws-x" }, byWs);
    expect(search.teams.map((t) => t.teamKey)).toEqual(["acme:ops"]);
    expect(search.visible).toBe(1);
    expect(search.keep).toEqual(new Set([agentRowKey("acme:ops", "twin")]));
    const needs = planAgentsPane(twins, [], { ...NO_FILTER, status: "needs" }, byWs);
    expect(needs.teams.map((t) => [t.teamKey, t.rows.length])).toEqual([["acme:ops", 1]]);
    expect(needs.visible).toBe(1);
    expect(needs.counts.needs).toBe(needs.visible);
    expect(needs.total).toBe(2);
  });
});

describe("planAgentsPane hidden teams", () => {
  it("treats a hidden team with no agents left as not hidden, so the chip and filtering clear", () => {
    const team = teamOf("team-1", [row("a")]);
    const plan = planAgentsPane(
      [team],
      ["team-gone"],
      { query: "", status: "all", workspace: null },
      rules,
    );
    expect(plan.filtering).toBe(false);
    expect(plan.visible).toBe(1);
  });
});

describe("planAgentsPane launchers", () => {
  it("names each launched agent's launcher in the team's tree, whatever the filters keep", () => {
    const team = teamOf("team-1", [row("lead"), row("worker", { launchedBy: "lead" })], {
      lead: "ws-a",
      worker: "ws-a",
    });
    const plan = planAgentsPane(
      [team],
      [],
      { query: "worker", status: "all", workspace: null },
      rules,
    );
    expect(plan.teams[0]!.rows.map((n) => n.row.handle)).toEqual(["worker"]);
    expect(plan.teams[0]!.launchers).toEqual({ worker: "lead" });
  });
});
