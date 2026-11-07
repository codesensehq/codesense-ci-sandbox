// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

import type { AgentRoutineItem, AgentRow, RoutineRow } from "@/tauri/commands";

/** The golden `agent_get.routines[]` contract the Rust read model pins. */
const golden = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../../src-tauri/src/fixtures/wake/agent-routines-queued.json"),
    "utf-8",
  ),
) as { routines: AgentRoutineItem[] };

import {
  chipOfItem,
  chipOfRow,
  chipTime,
  machineWords,
  modeWord,
  nextRunChip,
  queuedWakeFor,
  routineSentence,
  routinesLabel,
  routinesOf,
} from "./agent-routines";

// SW-AGR-5 (design §4.2, §4.4): an agent's routines as its row chip and its Routines
// section read them.

const NOW = new Date(2026, 9, 6, 8, 42).getTime(); // Tue Oct 6, 8:42
const at = (day: number, h: number, m = 0) => new Date(2026, 9, day, h, m).getTime();
const LOCALE = "en-GB";

function row(name: string, overrides: Partial<RoutineRow> = {}): RoutineRow {
  const id = name.toLowerCase().replace(/\s+/g, "-");
  return {
    key: `acme/backend/${id}`,
    scope: "team",
    armed_here: true,
    owner_display: "Alice",
    enabled: true,
    created_by: null,
    schedule_in_words: "weekdays at 9:00",
    next_run_at: at(6, 9),
    last_run: null,
    needs_you: false,
    needs_you_run: null,
    agent_display: { name: "Basil", archived: false, resolvable: true },
    agent_key: "acme/backend/basil",
    unassigned: false,
    duplicate: false,
    effective_mode: "wake",
    mine: true,
    def: {
      id,
      name,
      owner: "1",
      agent: "basil",
      mode: "wake",
      task: "triage new issues",
      where: { kind: "team_workspace", machine: "codesense-test-1" },
      triggers: [{ schedule: { cron: "0 9 * * 1-5", tz: "America/Denver" } }],
      policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: true },
      revision: 1,
      armed_host: "this-mac",
    },
    ...overrides,
  };
}

const LABELS = new Map([["codesense-test-1", "codesense-test-1"]]);

describe("routinesOf", () => {
  it("lists only the agent's routines: enabled first, then next fire, then name", () => {
    const rows = [
      row("Weekly retro", { enabled: false, next_run_at: null }),
      row("Inbox sweep", { next_run_at: at(6, 9) }),
      row("Morning triage", { next_run_at: at(6, 9) }),
      row("Standup", { next_run_at: at(6, 8, 50) }),
      row("Other", { agent_key: "acme/backend/lance" }),
    ];
    expect(routinesOf(rows, "acme/backend/basil").map((r) => r.def.name)).toEqual([
      "Standup",
      "Inbox sweep",
      "Morning triage",
      "Weekly retro",
    ]);
  });
});

describe("chipTime", () => {
  it("today: the time; within 6 days: the weekday and time; later: the date", () => {
    expect(chipTime(at(6, 9), NOW, LOCALE)).toBe("9:00");
    expect(chipTime(at(8, 9), NOW, LOCALE)).toBe("Thu 9:00");
    expect(chipTime(at(14, 9), NOW, LOCALE)).toBe("14 Oct");
  });
});

describe("nextRunChip", () => {
  it("names the next fire of the agent's enabled routines", () => {
    const chip = nextRunChip(
      [
        chipOfRow(row("Inbox sweep", { next_run_at: at(6, 9, 30) })),
        chipOfRow(row("Morning triage")),
      ],
      NOW,
      LOCALE,
    );
    expect(chip).toMatchObject({ state: "next", label: "9:00" });
    expect(chip?.tooltip).toBe("Next: Morning triage, weekdays at 9:00 (in 18m)");
  });

  it("reads due while a wake is queued for a busy session", () => {
    const queued = {
      runId: "run-42",
      machineId: "codesense-test-1",
      sessionId: "s-basil",
      marker: "Morning triage",
      deadlineMs: at(6, 9, 30),
      unconfirmed: false,
    };
    const chip = nextRunChip(
      [chipOfRow(row("Morning triage", { queued })), chipOfRow(row("Inbox sweep"))],
      NOW,
      LOCALE,
      "Basil",
    );
    expect(chip).toMatchObject({ state: "due", label: "due" });
    expect(chip?.tooltip).toBe("Morning triage is waiting for Basil to be idle (until 9:30)");
  });

  it("an event-only routine reads after its trigger", () => {
    const after = row("Review after Basil", {
      next_run_at: null,
      def: {
        ...row("x").def,
        triggers: [{ event: { kind: "agent_done", from_agent: "Basil" } }],
      },
    });
    expect(nextRunChip([chipOfRow(after)], NOW, LOCALE)).toMatchObject({
      state: "after",
      label: "after Basil",
    });
  });

  it("no chip when every routine is off, or there are none", () => {
    expect(nextRunChip([chipOfRow(row("Off", { enabled: false }))], NOW, LOCALE)).toBeNull();
    expect(nextRunChip([], NOW, LOCALE)).toBeNull();
  });

  it("reads `queued` from the golden agent_get.routines[] contract", () => {
    const items = golden.routines;
    const chip = nextRunChip(items.map(chipOfItem), NOW, LOCALE, "Basil");
    expect(chip).toMatchObject({ state: "due", label: "due" });
    expect(chip?.tooltip).toContain("Triage is waiting for Basil to be idle");
  });
});

describe("the routine sentence", () => {
  it("reads schedule · mode · task · machine", () => {
    expect(routineSentence(row("Morning triage"), LABELS)).toEqual({
      when: "Weekdays at 9:00",
      mode: "wake",
      task: "“triage new issues”",
      machine: "on codesense-test-1",
    });
  });

  it("a fresh routine reads start a new, on This Mac when no machine is named", () => {
    const fresh = row("Weekly retro", {
      effective_mode: "fresh",
      def: { ...row("x").def, mode: "fresh", where: { kind: "team_workspace" } },
    });
    expect(routineSentence(fresh, LABELS)).toMatchObject({
      mode: "start a new",
      machine: "on This Mac",
    });
  });

  it("an event trigger leads the sentence; a long task is cut at 60 characters", () => {
    const long = "x".repeat(80);
    const after = row("Review", {
      def: {
        ...row("x").def,
        task: `${long}\nsecond line`,
        triggers: [{ event: { kind: "agent_done", from_agent: "Basil" } }],
      },
    });
    const s = routineSentence(after, LABELS);
    expect(s.when).toBe("When Basil finishes");
    expect(s.task).toBe(`“${"x".repeat(59)}…”`);
  });

  it("names every machine slot", () => {
    expect(machineWords(undefined, LABELS)).toBe("on This Mac");
    expect(machineWords("local", LABELS)).toBe("on This Mac");
    expect(machineWords("any", LABELS)).toBe("on any connected machine");
    expect(machineWords("codesense-test-1", LABELS)).toBe("on codesense-test-1");
    expect(machineWords("gpu-box-2", new Map())).toBe("on gpu-box-2");
    expect(machineWords("local", LABELS, "each_mac")).toBe("on each teammate's Mac");
  });

  it("mode words", () => {
    expect(modeWord("wake")).toBe("wake");
    expect(modeWord("fresh")).toBe("start a new");
  });
});

describe("routinesLabel", () => {
  const agent = (n: number, archived = false) =>
    ({
      routines: Array.from({ length: n }, (_, i) => ({ key: `k${i}`, name: `R${i}` })),
      archived_routines: archived
        ? Array.from({ length: n }, (_, i) => ({ key: `k${i}`, name: `R${i}` }))
        : [],
      archived,
    }) as Pick<AgentRow, "routines" | "archived_routines" | "archived">;
  it("counts the agent's own routines; none means no link", () => {
    expect(routinesLabel(agent(2))).toBe("2 routines");
    expect(routinesLabel(agent(1))).toBe("1 routine");
    expect(routinesLabel(agent(0))).toBeNull();
  });
  it("an archived agent says its routines were archived with it", () => {
    expect(routinesLabel(agent(2, true))).toBe("archived · 2 routines archived");
    expect(
      routinesLabel({ archived: true, archived_routines: [], routines: [{ key: "k", name: "R" }] }),
    ).toBe("archived · 1 routine");
  });
});

describe("queuedWakeFor (the sidebar's due marker)", () => {
  const queued = (machineId: string) => ({
    runId: "r",
    machineId,
    sessionId: "s-basil",
    marker: "Morning triage",
    deadlineMs: null,
    unconfirmed: false,
  });
  it("marks the remote session the wake is queued on, and not a local one with its id", () => {
    const rows = [row("Morning triage", { queued: queued("codesense-test-1") })];
    expect(queuedWakeFor(rows, "s-basil", "codesense-test-1")?.def.name).toBe("Morning triage");
    expect(queuedWakeFor(rows, "s-basil", "local")).toBeNull();
    expect(queuedWakeFor(rows, "s-other", "codesense-test-1")).toBeNull();
  });
  it("marks a local session for a wake queued on This Mac", () => {
    const rows = [row("Inbox sweep", { queued: queued("local") })];
    expect(queuedWakeFor(rows, "s-basil", "local")?.def.name).toBe("Inbox sweep");
  });
});
