// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AgentRow, RoutineRow, RoutineRun } from "@/tauri/commands";

import {
  attentionCount,
  needsAttention,
  recentGroups,
  scheduleGroups,
  type ScheduleFilter,
} from "./schedule-model";

// SW-AGR-5 (design §4.8, SPEC §10): the Schedule view groups routines by agent, then by
// machine, Unassigned first when it isn't empty; never by routine.

const NOW = new Date(2026, 9, 6, 8, 42).getTime();
const MIN = 60_000;

function agent(key: string, name: string): AgentRow {
  return { key, name, runtime: "claude", scope: "team", org: "acme", team: "backend" } as AgentRow;
}
const BASIL = agent("acme/backend/basil", "Basil");
const LANCE = agent("acme/backend/lance", "Lance");

function row(
  name: string,
  agentKey: string | null,
  machine: string | undefined,
  overrides: Partial<RoutineRow> = {},
): RoutineRow {
  const id = name.toLowerCase().replace(/\s+/g, "-");
  return {
    key: `acme/backend/${id}`,
    scope: "team",
    armed_here: true,
    owner_display: "you",
    enabled: true,
    created_by: null,
    schedule_in_words: "weekdays at 9:00",
    next_run_at: NOW + 18 * MIN,
    last_run: null,
    needs_you: false,
    needs_you_run: null,
    agent_display: { name: agentKey ? "x" : "pockets", archived: false, resolvable: !!agentKey },
    agent_key: agentKey,
    unassigned: agentKey === null,
    duplicate: false,
    effective_mode: "wake",
    mine: true,
    def: {
      id,
      name,
      owner: "1",
      agent: "basil",
      mode: "wake",
      task: "t",
      where: { kind: "team_workspace", ...(machine ? { machine } : {}) },
      triggers: [{ schedule: { cron: "0 9 * * 1-5", tz: "UTC" } }],
      policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: true },
      revision: 1,
      armed_host: "this-mac",
    },
    ...overrides,
  };
}

const LABELS = new Map([["codesense-test-1", "codesense-test-1"]]);

describe("scheduleGroups", () => {
  const rows = [
    row("Inbox sweep", BASIL.key, "local", { next_run_at: NOW + 48 * MIN }),
    row("Morning triage", BASIL.key, "codesense-test-1"),
    row("Standup", BASIL.key, undefined, { next_run_at: NOW + 8 * MIN }),
    row("Review", LANCE.key, "any"),
    row("Weekly changelog", null, undefined),
  ];

  it("Unassigned first, then one group per agent by name, then machine, then next fire", () => {
    const { unassigned, groups } = scheduleGroups(rows, [LANCE, BASIL], LABELS, null);
    expect(unassigned.map((r) => r.def.name)).toEqual(["Weekly changelog"]);
    expect(groups.map((g) => g.name)).toEqual(["Basil", "Lance"]);
    expect(groups[0].machines.map((m) => [m.label, m.rows.map((r) => r.def.name)])).toEqual([
      ["This Mac", ["Standup", "Inbox sweep"]],
      ["codesense-test-1", ["Morning triage"]],
    ]);
    expect(groups[1].machines.map((m) => m.label)).toEqual(["any connected machine"]);
    expect(groups[0].count).toBe(3);
  });

  it("groups commands without an agent in Upcoming and Recent", () => {
    const command = row("Sync feed", null, "local", { unassigned: false });
    command.def = {
      ...command.def,
      action: "command",
      command: ["node", "sync.js"],
      agent: undefined,
      task: undefined,
      mode: undefined,
    };
    const upcoming = scheduleGroups([...rows, command], [BASIL, LANCE], LABELS, null);
    const group = upcoming.groups.find((g) => g.name === "Commands")!;
    expect(upcoming.groups.at(-1)?.name).toBe("Commands");
    expect(group.agentKey).toBeNull();
    expect(group.runtime).toBeNull();
    expect(group.machines[0].rows).toEqual([command]);
    expect(upcoming.unassigned).not.toContain(command);
    const run = {
      run_id: "command-run",
      phase: "succeeded",
      created_at: NOW,
      ended_at: NOW,
    } as RoutineRun;
    expect(
      recentGroups([command], { [command.key]: [run] }, [], LABELS, null, NOW).groups[0].name,
    ).toBe("Commands");
  });

  it("a filter shows one agent only; Needs attention shows Unassigned and failed wakes", () => {
    const basil: ScheduleFilter = { kind: "agent", agentKey: BASIL.key, name: "Basil" };
    const filtered = scheduleGroups(rows, [LANCE, BASIL], LABELS, basil);
    expect(filtered.unassigned).toEqual([]);
    expect(filtered.groups.map((g) => g.name)).toEqual(["Basil"]);

    const failed = row("Unknown one", LANCE.key, "local", {
      last_run: { run_id: "r1", phase: "unknown", reason: null, at: NOW, result_text: null },
    });
    const attention = scheduleGroups([...rows, failed], [LANCE, BASIL], LABELS, {
      kind: "attention",
    });
    expect(attention.unassigned).toHaveLength(1);
    expect(attention.groups.flatMap((g) => g.machines.flatMap((m) => m.rows))).toEqual([failed]);
  });

  it("an agent the list doesn't know still groups by its routine's agent name", () => {
    const { groups } = scheduleGroups(
      [row("Orphan", "acme/backend/ghost", "local")],
      [],
      LABELS,
      null,
    );
    expect(groups.map((g) => g.name)).toEqual(["x"]);
  });
});

describe("needs attention", () => {
  it("is Unassigned, or a latest run that is undelivered or unknown and was never opened", () => {
    const unknown = row("U", BASIL.key, "local", {
      last_run: { run_id: "r1", phase: "unknown", reason: null, at: NOW, result_text: null },
    });
    const undelivered = row("D", BASIL.key, "local", {
      last_run: {
        run_id: "r2",
        phase: "undelivered",
        reason: "busy_timeout",
        at: NOW,
        result_text: null,
      },
    });
    const fine = row("F", BASIL.key, "local");
    const orphan = row("O", null, undefined);
    expect([unknown, undelivered, fine, orphan].map((r) => needsAttention(r))).toEqual([
      true,
      true,
      false,
      true,
    ]);
    expect(attentionCount([unknown, undelivered, fine, orphan], new Set())).toBe(3);
    expect(attentionCount([unknown, undelivered, fine, orphan], new Set(["r1"]))).toBe(2);
  });
});

describe("recentGroups", () => {
  const run = (id: string, phase: RoutineRun["phase"], agoMin: number): RoutineRun => ({
    run_id: id,
    routine_key: "acme/backend/inbox-sweep",
    revision: 1,
    source: "schedule",
    occurrence_key: null,
    phase,
    reason: null,
    linked_run_id: null,
    session_id: null,
    created_at: NOW - agoMin * MIN,
    started_at: NOW - agoMin * MIN,
    ended_at: NOW - agoMin * MIN,
    result_text: null,
    agent: null,
  });

  it("lists the last 7 days newest first, folding each merged or skipped fire under the run before it", () => {
    const sweep = row("Inbox sweep", BASIL.key, "local");
    const runs = {
      [sweep.key]: [
        run("a", "coalesced", 5),
        run("b", "unknown", 12),
        run("c", "skipped", 20),
        run("d", "coalesced", 25),
        run("e", "delivered", 40),
        run("old", "delivered", 8 * 24 * 60),
      ],
    };
    const { groups } = recentGroups([sweep], runs, [BASIL], LABELS, null, NOW);
    const items = groups[0].machines[0].rows;
    expect(items.map((i) => [i.run.run_id, i.folded])).toEqual([
      ["b", 1],
      ["e", 2],
    ]);
    expect(groups[0].count).toBe(2);
  });
});
