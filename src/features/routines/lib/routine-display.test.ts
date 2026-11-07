// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { RoutineRow, RoutineRun } from "@/tauri/commands";

import {
  agentName,
  createdByLabel,
  groupRoutines,
  lastRunSummary,
  machineLabel,
  phaseLabel,
  phaseTone,
  reasonCopy,
  relativeTime,
  runsOn,
  scheduleLabel,
  routineListLabel,
  switchLabel,
  timeAgo,
  whereLabel,
} from "./routine-display";

function row(name: string, overrides: Partial<RoutineRow> = {}): RoutineRow {
  return {
    key: `acme/backend/${name.toLowerCase().replace(/\s+/g, "-")}`,
    scope: "team",
    armed_here: true,
    owner_display: "Alice",
    enabled: true,
    created_by: { kind: "user", name: "alice" },
    schedule_in_words: "every 5 minutes",
    next_run_at: null,
    last_run: null,
    needs_you: false,
    needs_you_run: null,
    agent_display: { name: "Dev", archived: false, resolvable: true },
    agent_key: null,
    unassigned: false,
    duplicate: false,
    effective_mode: "fresh",
    mine: true,
    def: {
      id: name,
      name,
      owner: "1",
      agent: "dev",
      task: "t",
      where: { kind: "team_workspace" },
      policy: { overlap: "coalesce", missed: "skip", visible: false },
      revision: 1,
      armed_host: "h",
    },
    ...overrides,
  };
}

describe("groupRoutines", () => {
  it("puts routines whose agent is gone in Unassigned, first and out of Mine and Team", () => {
    const rows = [
      row("kept"),
      row("orphan", { unassigned: true }),
      row("Team orphan", { unassigned: true, mine: false }),
      row("theirs", { mine: false }),
      row("twin", { duplicate: true }),
    ];
    const { unassigned, mine, team } = groupRoutines(rows);
    expect(unassigned.map((r) => r.def.name)).toEqual(["orphan", "Team orphan", "twin"]);
    expect(mine.map((r) => r.def.name)).toEqual(["kept"]);
    expect(team.map((r) => r.def.name)).toEqual(["theirs"]);
  });

  it("splits Mine from Team and orders each group by name, whatever order rows arrive in", () => {
    const rows = [
      row("zeta"),
      row("Alpha", { mine: false }),
      row("beta"),
      row("Gamma", { mine: false }),
      row("alpha"),
    ];
    const { mine, team } = groupRoutines(rows);
    expect(mine.map((r) => r.def.name)).toEqual(["alpha", "beta", "zeta"]);
    expect(team.map((r) => r.def.name)).toEqual(["Alpha", "Gamma"]);
    // A re-read in another order (a run ended, a row was selected) changes nothing.
    const again = groupRoutines([...rows].reverse());
    expect(again.mine.map((r) => r.key)).toEqual(mine.map((r) => r.key));
    expect(again.team.map((r) => r.key)).toEqual(team.map((r) => r.key));
  });

  it("breaks a name tie by key, so two same-named routines never swap", () => {
    const a = row("Digest", { key: "acme/a/digest" });
    const b = row("Digest", { key: "acme/b/digest" });
    expect(groupRoutines([b, a]).mine.map((r) => r.key)).toEqual([a.key, b.key]);
  });
});

describe("reasonCopy", () => {
  const skipped = (reason: string) =>
    ({ phase: "skipped", reason }) as Pick<RoutineRun, "phase" | "reason">;

  it.each([
    ["missed:1", "Skipped — 1 missed while CodeSense was closed or asleep"],
    ["missed:12", "Skipped — 12 missed while CodeSense was closed or asleep"],
    ["already_running", "Skipped — previous run still running"],
    ["target_unavailable", "Skipped — workspace unavailable"],
    ["loop_guard", "Skipped — loop guard"],
    ["paused", "Skipped — paused"],
    ["queue_full", "Skipped — queue full"],
  ])("%s reads as human copy", (reason, copy) => {
    expect(reasonCopy(skipped(reason))).toBe(copy);
  });

  it("names the fill-in a command could not start without, from its detail", () => {
    expect(
      reasonCopy({
        phase: "failed",
        reason: "launch_failed",
        action: "command",
        detail: "unresolved:itemId",
      }),
    ).toBe("Failed — {{itemId}} had no value");
  });

  it("reads an interrupted run as the app quitting mid-run", () => {
    expect(reasonCopy({ phase: "failed", reason: "interrupted" })).toBe(
      "Interrupted — CodeSense quit mid-run",
    );
  });

  it("reads a run whose agent exited with an error as failed", () => {
    expect(reasonCopy({ phase: "failed", reason: "exited_with_error" })).toBe(
      "Failed — the agent exited with an error",
    );
  });

  it("reads a run that simply ended as Ended, with nothing to add", () => {
    expect(reasonCopy({ phase: "ended", reason: null })).toBeNull();
    expect(phaseLabel("ended")).toBe("Ended");
    expect(phaseTone("ended")).toBe("quiet");
    expect(
      lastRunSummary({ run_id: "r", phase: "ended", reason: null, at: 0, result_text: null }),
    ).toBe("Ended");
  });

  it("says nothing extra when there is no reason, and never drops an unknown one", () => {
    expect(reasonCopy({ phase: "succeeded", reason: null })).toBeNull();
    expect(reasonCopy({ phase: "failed", reason: "something_new" })).toBe("Error — something_new");
  });
});

describe("row labels", () => {
  it("credits an agent by name and the caller as you", () => {
    expect(createdByLabel(row("a", { created_by: { kind: "agent", name: "triage-bot" } }))).toBe(
      "triage-bot",
    );
    expect(createdByLabel(row("a"))).toBe("you");
    expect(createdByLabel(row("a", { mine: false, owner_display: "Sam" }))).toBe("Sam");
  });

  it("says where a personal routine fires when that is not this Mac", () => {
    const mine = (overrides: Partial<RoutineRow> = {}) =>
      row("a", { scope: "personal", ...overrides });
    expect(machineLabel(mine())).toBeNull();
    expect(machineLabel(mine({ armed_here: false }))).toBe("Runs on another Mac");
    // A hand-written routine that names no Mac runs on none.
    const unplaced = mine({ armed_here: false });
    unplaced.def = { ...unplaced.def, armed_host: " " };
    expect(machineLabel(unplaced)).toBe("Not running anywhere");
  });

  it("says a team routine's mode and where it runs", () => {
    // A shared job runs once for the team, on one Mac.
    expect(machineLabel(row("a"))).toBe("Runs once for the team · on this Mac");
    // A Mac is a host, not a person: another Mac is never named after anyone,
    // whoever created or placed the routine.
    expect(machineLabel(row("a", { armed_here: false }))).toBe(
      "Runs once for the team · on another Mac",
    );
    expect(machineLabel(row("a", { mine: false, owner_display: "Sam", armed_here: false }))).toBe(
      "Runs once for the team · on another Mac",
    );
    const unplaced = row("a", { armed_here: false });
    unplaced.def = { ...unplaced.def, armed_host: "" };
    expect(machineLabel(unplaced)).toBe("Runs once for the team · not running anywhere");
    // A recipe each teammate's Mac runs says whether it is on here.
    const recipe = (enabled: boolean) => {
      const r = row("a", { enabled });
      r.def = { ...r.def, runs_on: "each_mac", armed_host: "" };
      return r;
    };
    expect(machineLabel(recipe(true))).toBe("Each teammate's Mac · on here");
    expect(machineLabel(recipe(false))).toBe("Each teammate's Mac · off here");
    expect(switchLabel(recipe(false))).toBe("Run on my Mac");
    expect(switchLabel(row("Digest"))).toBe("Digest enabled");
    expect(reasonCopy({ phase: "skipped", reason: "off_here" })).toBe(
      "Skipped — off on this Mac. Turn on for this Mac to run here",
    );
    expect(runsOn(recipe(true).def)).toBe("each_mac");
    expect(runsOn(row("a").def)).toBe("one_mac");
  });

  it("names the agent and the place", () => {
    expect(agentName({ name: "Triage", archived: false, resolvable: true })).toBe("Triage");
    expect(agentName({ name: "Triage", archived: true, resolvable: true })).toBe(
      "Triage (archived)",
    );
    expect(agentName({ name: "acme/api/backend/gone", archived: false, resolvable: false })).toBe(
      "acme/api/backend/gone",
    );
    expect(whereLabel("team_workspace")).toBe("Team workspace");
    expect(whereLabel("repo:acme/api")).toBe("acme/api");
  });

  it("puts times relative to now", () => {
    const now = 1_000_000_000;
    expect(relativeTime(now + 3 * 3_600_000 + 5, now)).toBe("in 3h");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5m ago");
    expect(relativeTime(now + 2 * 86_400_000, now)).toBe("in 2d");
    expect(timeAgo(now - 3 * 3_600_000, now)).toBe("3h ago");
    expect(timeAgo(now + 2_000, now)).toBe("just now");
  });
});

describe("scheduleLabel", () => {
  it("opens a line in sentence case whatever case the schedule was described in", () => {
    expect(scheduleLabel({ schedule_in_words: "weekdays at 9:00 MDT" })).toBe(
      "Weekdays at 9:00 MDT",
    );
    expect(scheduleLabel({ schedule_in_words: "every 2 hours" })).toBe("Every 2 hours");
    expect(scheduleLabel({ schedule_in_words: "Mondays at 8:30" })).toBe("Mondays at 8:30");
    expect(scheduleLabel({ schedule_in_words: "" })).toBe("");
  });
});

it("keeps multiple triggers compact in lists and complete in the detail label", () => {
  const value = row("Metadata");
  value.schedule_in_words =
    "daily at 9:00; when a message is queued for a role; when an agent finishes";
  value.def.triggers = [
    { schedule: { cron: "0 9 * * *", tz: "UTC" } },
    { event: { kind: "role_pending" } },
    { event: { kind: "agent_done" } },
  ];
  expect(routineListLabel(value)).toBe("Daily at 9:00 + 2 more");
  expect(scheduleLabel(value)).toContain("when an agent finishes");
});
