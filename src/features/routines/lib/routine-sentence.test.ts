// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { mockParseWhen } from "@/app/mock/handlers/routines";
import type { RoutineDef } from "@/tauri/commands";

import {
  createForm,
  defaultMode,
  draftFromDef,
  editForm,
  emptyDraft,
  eventTriggerWords,
  limitsProblem,
  limitsSummary,
  machineProblem,
  machineProblemCopy,
  migratedUnsaved,
  revisionDiff,
  shownMode,
  slotFromCron,
} from "./routine-sentence";

function def(overrides: Partial<RoutineDef> = {}): RoutineDef {
  return {
    id: "echo",
    name: "Echo",
    owner: "1",
    agent: "default",
    task: "echo hi",
    where: { kind: "team_workspace" },
    triggers: [{ schedule: { cron: "*/5 * * * *", tz: "UTC" } }],
    policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: false },
    revision: 1,
    armed_host: "this-mac",
    ...overrides,
  };
}

describe("the when slot", () => {
  it.each([
    ["*/5 * * * *", "5m"],
    ["0 * * * *", "hour"],
    ["0 */2 * * *", "2h"],
    ["0 9 * * *", "day 09:00"],
    ["30 8 * * 1-5", "weekday 08:30"],
    ["0 17 * * 1", "mon 17:00"],
    ["0 9 1 * *", "0 9 1 * *"],
    ["*/7 * * * *", "*/7 * * * *"],
    ["0 */5 * * *", "0 */5 * * *"],
  ])("%s shows as %s and round-trips", (cron, slot) => {
    expect(slotFromCron(cron)).toBe(slot);
    // The slot is sent as shown; the mock reads it as the backend's parse_when does.
    expect(mockParseWhen(slot)).toBe(cron);
  });
});

describe("the sentence form", () => {
  it("creates with every field, the schedule as typed", () => {
    const form = createForm({ ...emptyDraft(), name: " Echo ", task: "echo", when: "5m" });
    expect(form).toMatchObject({
      name: "Echo",
      task: "echo",
      when: "5m",
      where: "team_workspace",
      overlap: "coalesce",
      missed: "skip",
      maxRuntime: "30m",
      visible: false,
    });
    expect(form.agent).toBeUndefined();
    expect(form.tz).toBeUndefined();
  });

  it("edits send only what changed; an untouched schedule is not sent", () => {
    const base = draftFromDef(def());
    expect(base.when).toBe("5m");
    expect(editForm(base, base)).toBeNull();
    expect(editForm(base, { ...base, task: "echo there" })).toEqual({ task: "echo there" });
    expect(editForm(base, { ...base, when: "10m" })).toEqual({ when: "10m", tz: "UTC" });
    // A zone alone keeps every schedule: no `when` is sent.
    expect(editForm(base, { ...base, tz: "Europe/London" })).toEqual({ tz: "Europe/London" });
    expect(editForm(base, { ...base, missed: "run_once", visible: true })).toEqual({
      missed: "run_once",
      visible: true,
    });
    // Blank keeps what the routine has, so clearing is not an edit to save.
    expect(editForm(base, { ...base, agent: " ", maxRuntime: "" })).toBeNull();
    expect(editForm(base, { ...base, agent: "triage" })).toEqual({ agent: "triage" });
  });
});

describe("wake limits (SW-AGR-13)", () => {
  const limited = def({
    policy: {
      overlap: "skip",
      missed: "skip",
      max_runtime: "30m",
      visible: false,
      max_wakes: 20,
      max_wakes_per_day: 3,
      min_gap: "4h",
    },
  });

  it("opens with the routine's limits and summarises them", () => {
    const draft = draftFromDef(limited);
    expect([draft.maxWakes, draft.maxWakesPerDay, draft.minGap]).toEqual(["20", "3", "4h"]);
    expect(limitsSummary(draft)).toBe("20 wakes in all · 3 a day · 4h apart");
    expect(limitsSummary(draftFromDef(def()))).toBe("none");
  });

  it("a create sends only the limits that are set", () => {
    const form = createForm({ ...emptyDraft(), name: "Heavy", minGap: " 2h ", maxWakes: "5" });
    expect(form).toMatchObject({ minGap: "2h", maxWakes: 5 });
    expect("maxWakesPerDay" in form).toBe(false);
    expect("maxWakes" in createForm(emptyDraft())).toBe(false);
  });

  it("an edit sends a changed limit, and null for one cleared", () => {
    const base = draftFromDef(limited);
    expect(editForm(base, { ...base, maxWakes: "40" })).toEqual({ maxWakes: 40 });
    expect(editForm(base, { ...base, minGap: "", maxWakesPerDay: " " })).toEqual({
      minGap: null,
      maxWakesPerDay: null,
    });
    // Not a whole number: sent as typed, so the save refuses it by name.
    expect(editForm(base, { ...base, maxWakes: "lots" })).toEqual({ maxWakes: "lots" });
    expect(editForm(base, base)).toBeNull();
  });

  it("says what is wrong with a limit before it is saved", () => {
    const base = draftFromDef(limited);
    expect(limitsProblem(base)).toBeNull();
    expect(limitsProblem({ ...base, maxWakes: "lots" })).toBe(
      "Max wakes must be a whole number of 1 or more.",
    );
    expect(limitsProblem({ ...base, maxWakesPerDay: "0" })).toBe(
      "Max wakes a day must be a whole number of 1 or more.",
    );
    expect(limitsProblem({ ...base, minGap: "soon" })).toBe(
      "Min gap must be written like 30m, 2h or 1d.",
    );
    expect(limitsProblem({ ...base, maxWakes: "", minGap: "" })).toBeNull();
  });

  it("a revision diff names the limits", () => {
    expect(revisionDiff(def(), limited).map((c) => c.field)).toEqual(
      expect.arrayContaining(["Max wakes", "Max wakes a day", "Min gap"]),
    );
  });
});

describe("revision diffs", () => {
  it("lists each changed field with before and after", () => {
    const prev = def();
    const next = def({
      task: "echo there",
      revision: 2,
      triggers: [{ schedule: { cron: "0 * * * *", tz: "UTC" } }],
    });
    expect(revisionDiff(prev, next)).toEqual([
      { field: "Triggers", before: "*/5 * * * * (UTC)", after: "0 * * * * (UTC)" },
      { field: "Task", before: "echo hi", after: "echo there" },
    ]);
    expect(revisionDiff(prev, def({ revision: 2 }))).toEqual([]);
  });
});

describe("structured result editor roundtrip", () => {
  it("keeps the schema through loading and form construction and preserves omitted edits", () => {
    const schema = { type: "object", properties: { count: { type: "integer" } } };
    const original = def({ result_schema: schema, triggers: [], runs_on: "each_mac" });
    const draft = draftFromDef(original);
    expect(createForm(draft).resultSchema).toEqual(schema);
    expect(draft.runsOn).toBe("each_mac");
    expect(editForm(draft, { ...draft, runsOn: "one_mac" })).toEqual({ runsOn: "one_mac" });
    const edit = editForm(draft, { ...draft, name: "Renamed" });
    expect(edit).toEqual({ name: "Renamed" }); // Backend patches preserve omitted schema.
    expect(editForm(draft, { ...draft, resultSchema: null })).toEqual({ resultSchema: null });
    const replacement = { type: "array" };
    expect(editForm(draft, { ...draft, resultSchema: replacement })).toEqual({
      resultSchema: replacement,
    });
    expect(revisionDiff(original, { ...original, result_schema: replacement })).toEqual([
      {
        field: "Result schema",
        before: JSON.stringify(schema),
        after: JSON.stringify(replacement),
      },
    ]);
  });
});

describe("event triggers", () => {
  it("words the existing event kinds", () => {
    expect(
      [
        { kind: "inbox_idle" },
        { kind: "agent_done", from_agent: "reviewer" },
        { kind: "agent_done" },
        { kind: "role_pending" },
      ].map(eventTriggerWords),
    ).toEqual([
      "when an agent sits idle with unread mail",
      "when reviewer finishes",
      "when an agent finishes",
      "when a message is queued for a role",
    ]);
  });

  it("names an inbox_idle trigger in a revision diff", () => {
    const prev = def({ triggers: [] });
    const next = def({ revision: 2, triggers: [{ event: { kind: "inbox_idle" } }] });
    expect(revisionDiff(prev, next)).toEqual([
      { field: "Triggers", before: "", after: "when an agent sits idle with unread mail" },
    ]);
  });
});

describe("the mode word (SW-AGR-2)", () => {
  const barb = { key: "acme/backend/barb", name: "Barb", launch: "foreground" as const };
  const batch = { key: "acme/backend/batch", name: "Batch", launch: "headless" as const };
  const starter = { key: "builtin/claude", name: "Claude", launch: "foreground" as const };

  it("defaults a new routine to wake only when visible, on a saved agent that launches in a tab", () => {
    expect(defaultMode(barb, true)).toBe("wake");
    expect(defaultMode(barb, false)).toBe("fresh");
    expect(defaultMode(batch, true)).toBe("fresh");
    expect(defaultMode(starter, true)).toBe("fresh");
    expect(defaultMode(undefined, true)).toBe("fresh");
    const opencode = { ...barb, runtime: "opencode" as const };
    expect(defaultMode(opencode, true)).toBe("wake");
    expect(
      shownMode({ ...emptyDraft(), agent: opencode.key, mode: "wake" }, opencode as never),
    ).toBe("wake");
  });

  it("shows the pick, else the default, and never wake for a starter", () => {
    const draft = { ...emptyDraft(), agent: barb.key, visible: true };
    expect(shownMode(draft, barb as never)).toBe("wake");
    expect(shownMode({ ...draft, mode: "fresh" }, barb as never)).toBe("fresh");
    expect(shownMode({ ...draft, agent: starter.key, mode: "wake" }, starter as never)).toBe(
      "fresh",
    );
  });

  it("a create saves the mode the form shows; an edit sends a changed pick only", () => {
    const draft = { ...emptyDraft(), name: "T", task: "t", when: "5m", agent: barb.key };
    expect(createForm(draft, "wake").mode).toBe("wake");
    expect(createForm(draft)).not.toHaveProperty("mode");
    const base = draftFromDef(def({ mode: "fresh" }));
    expect(base.mode).toBe("fresh");
    expect(editForm(base, { ...base, mode: "wake" })).toEqual({ mode: "wake" });
    expect(editForm(base, base)).toBeNull();
  });
});

describe("the machine slot (SW-AGR-5)", () => {
  it("opens with the routine's machine; none named reads as This Mac", () => {
    expect(draftFromDef(def()).machine).toBe("local");
    expect(
      draftFromDef(def({ where: { kind: "team_workspace", machine: "gpu-box-2" } })).machine,
    ).toBe("gpu-box-2");
  });

  it("a create sends the machine; an edit sends it only when it changed", () => {
    const draft = { ...emptyDraft(), name: "N", task: "t", when: "5m", agent: "barb" };
    expect(createForm({ ...draft, machine: "gpu-box-2" }, "wake").machine).toBe("gpu-box-2");
    expect(createForm(draft, "fresh").machine).toBe("local");
    const base = draftFromDef(def());
    expect(editForm(base, { ...base, machine: "any" })).toEqual({ machine: "any" });
    expect(editForm(base, { ...base })).toBeNull();
  });

  it("a migrated routine opens on start a new: the mode it always had", () => {
    const migrated = def({ migrated: { from: "config/routines/echo" } });
    expect(draftFromDef(migrated).mode).toBe("");
    expect(shownMode(draftFromDef(migrated), undefined)).toBe("fresh");
  });
});

describe("machine problems block a save (SW-AGR-5 r1)", () => {
  const draft = { ...emptyDraft(), name: "N", task: "t", when: "5m", agent: "barb" };
  const known = ["local", "codesense-test-1"];
  it("names each problem: unshared, each teammate's Mac elsewhere; a fresh run may start elsewhere (SW-AGR-2b)", () => {
    expect(machineProblem({ ...draft, machine: "gone-box" }, "wake", known, false)).toBe(
      "unshared",
    );
    expect(
      machineProblem({ ...draft, machine: "codesense-test-1" }, "fresh", known, false),
    ).toBeNull();
    expect(
      machineProblem(
        { ...draft, machine: "codesense-test-1", runsOn: "each_mac" },
        "wake",
        known,
        true,
      ),
    ).toBe("each_mac_remote");
    expect(machineProblem({ ...draft, machine: "any" }, "wake", known, false)).toBeNull();
    expect(
      machineProblem({ ...draft, machine: "codesense-test-1" }, "wake", known, false),
    ).toBeNull();
  });
  it("can't call a machine unshared before the list is read", () => {
    expect(machineProblem({ ...draft, machine: "gone-box" }, "wake", null, false)).toBeNull();
  });
  it("says each problem in the design's words", () => {
    expect(machineProblemCopy("unshared", "gone-box", "Barb")).toBe(
      "Pick a machine. gone-box isn't shared with this team any more.",
    );
    expect(machineProblemCopy("each_mac_remote", "codesense-test-1", "Barb")).toBe(
      "each teammate's Mac only works with This Mac for wake.",
    );
  });
});

describe("the migrated note (SW-AGR-5 r1)", () => {
  it("shows until the routine is saved once after moving under its agent", () => {
    expect(
      migratedUnsaved(def({ revision: 3, migrated: { from: "routines/echo", revision: 3 } })),
    ).toBe(true);
    expect(
      migratedUnsaved(def({ revision: 4, migrated: { from: "routines/echo", revision: 3 } })),
    ).toBe(false);
    expect(migratedUnsaved(def({ revision: 4 }))).toBe(false);
  });
});

it("words metadata target, exact transition, list gains and folder restrictions", () => {
  const target = { key: "status", op: "eq" as const, value: "in-progress" };
  const metadata = { selection: { where: { kinds: ["story" as const] } }, target };
  expect(eventTriggerWords({ kind: "metadata_change", metadata })).toBe(
    "when a story's status becomes in-progress",
  );
  expect(
    eventTriggerWords({
      kind: "metadata_change",
      metadata: {
        ...metadata,
        previous: { ...target, value: "planning" },
        selection: { where: { ...metadata.selection.where, pathPrefix: "stories/" } },
      },
    }),
  ).toBe("when a story's status moves from planning to in-progress, under stories/");
  expect(
    eventTriggerWords({
      kind: "metadata_change",
      metadata: {
        ...metadata,
        target: { key: "tags", op: "eq", value: "blocked" },
        list: true,
      },
    }),
  ).toBe("when a story gains blocked in tags");
});

it("sends only metadata replacements, preserves typed numeric operands until edited, and clears schedules", () => {
  const metadata = {
    selection: { where: { kinds: ["story" as const] } },
    target: { key: "points", op: "eq" as const, value: 1 },
  };
  const base = draftFromDef(
    def({
      triggers: [
        { schedule: { cron: "*/5 * * * *", tz: "UTC" } },
        { event: { kind: "metadata_change", metadata } },
        { event: { kind: "inbox_idle" } },
      ],
    }),
  );
  expect(editForm(base, { ...base, task: "new" })).toEqual({ task: "new" });
  expect(editForm(base, { ...base, when: "" })).toEqual({ when: "", tz: "UTC" });
  expect(createForm(base).metadataTriggers?.[0].target.value).toBe(1);
  expect(editForm(base, { ...base, events: [{ kind: "inbox_idle" }] })).toEqual({
    metadataTriggers: [],
  });
});

describe("command and completion trigger forms", () => {
  it("keeps a wake draft's fields while command options use ordinary run policies", () => {
    const base = { ...emptyDraft(), mode: "wake" as const, agent: "scribe", maxWakes: "3" };
    const draft = {
      ...base,
      action: "command" as const,
      command: ["printf", "ok"],
      maxWakes: "invalid hidden draft",
      visible: true,
    };
    expect(shownMode(draft, undefined)).toBe("fresh");
    const form = editForm(base, draft);
    expect(form).not.toHaveProperty("maxWakes");
    expect(form).not.toHaveProperty("visible");
    expect(draft.mode).toBe("wake");
  });

  it("sends argv separately and omits the saved agent draft", () => {
    const draft = {
      ...emptyDraft(),
      action: "command" as const,
      command: ["printf", "a | $HOME", "", ""],
      name: "Print",
      when: "5m",
      agent: "saved-agent",
      task: "Saved task",
    };
    const form = createForm(draft, "wake");
    expect(form).toMatchObject({ action: "command", command: ["printf", "a | $HOME"] });
    expect(form).not.toHaveProperty("agent");
    expect(form).not.toHaveProperty("task");
    expect(form).not.toHaveProperty("mode");
  });
  it("sends action switches and after removal on edits", () => {
    const base = { ...emptyDraft(), after: { routine: "feed", on: "succeeded" as const } };
    const next = {
      ...base,
      action: "command" as const,
      command: ["printf", "ok"],
      after: undefined,
    };
    expect(editForm(base, next)).toMatchObject({
      action: "command",
      command: ["printf", "ok"],
      after: null,
    });
  });
});
