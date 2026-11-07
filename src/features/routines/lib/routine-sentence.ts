// SPDX-License-Identifier: MIT
import type {
  AgentRow,
  RoutineDef,
  RoutineForm,
  RoutineMode,
  RoutineMetadataCondition,
  RoutineEventDef,
  RoutineRunsOn,
  RoutineAfter,
  RoutineRow,
} from "@/tauri/commands";

import { runsOn } from "./routine-display";

/**
 * The routine as the editor's sentence: *Every [when] have [agent] do [task] in
 * [where]*. The when slot reads after the word "Every" ("5m", "weekday 09:00",
 * "mon 17:00") and is sent as typed: the backend's `parse_when` is the one
 * reader of it. This module only formats a stored cron back into the slot.
 */

const DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/** Interval steps the backend's `every Nm` / `every Nh` presets accept. */
const MINUTE_STEPS = [5, 10, 15, 20, 30];
const hourStep = (n: number) => n > 0 && 24 % n === 0;

const pad = (n: string) => n.padStart(2, "0");

/** A cron as the when slot shows it: the short form when there is one. */
export function slotFromCron(cron: string): string {
  const fields = cron.trim().split(/\s+/);
  if (fields.length !== 5) return cron;
  const [min, hour, dom, month, dow] = fields;
  if (dom !== "*" || month !== "*") return cron;
  const step = (f: string) => /^\*\/(\d+)$/.exec(f)?.[1];
  const num = (f: string) => /^\d+$/.test(f);
  // Only steps a preset accepts shorten; any other step stays a cron, so the
  // slot always reads back as the schedule it came from.
  const minutes = Number(step(min));
  const hours = Number(step(hour));
  if (MINUTE_STEPS.includes(minutes) && hour === "*" && dow === "*") return `${minutes}m`;
  if (min === "0" && hour === "*" && dow === "*") return "hour";
  if (min === "0" && hourStep(hours) && dow === "*") return `${hours}h`;
  if (num(min) && num(hour)) {
    const at = `${pad(hour)}:${pad(min)}`;
    if (dow === "*") return `day ${at}`;
    if (dow === "1-5") return `weekday ${at}`;
    if (/^[0-7]$/.test(dow)) return `${DAYS[Number(dow) % 7]} ${at}`;
  }
  return cron;
}

/** The routine's first schedule, if it has one. */
export function firstSchedule(def: RoutineDef): { cron: string; tz: string } | null {
  for (const trigger of def.triggers ?? []) {
    if ("schedule" in trigger) return trigger.schedule;
  }
  return null;
}

/** How many schedules the routine has. The sentence shows only the first, and
 *  a new `when` replaces them all, so with more than one the slot is read-only. */
export function scheduleCount(def: RoutineDef): number {
  return (def.triggers ?? []).filter((t) => "schedule" in t).length;
}

/** An event trigger in words, as the routine list says it (`schedule_in_words`). */
export function metadataTriggerWords(condition: RoutineMetadataCondition): string {
  const kinds = condition.selection.where?.kinds;
  const kind = kinds?.length === 1 ? kinds[0] : "document";
  const subject = `${kind === "epic" ? "an" : "a"} ${kind}`;
  const { target, previous } = condition;
  let words = condition.list
    ? `when ${subject} gains ${String(target.value ?? "")} in ${target.key}`
    : previous
      ? `when ${subject}'s ${target.key} moves from ${String(previous.value ?? "")} to ${String(target.value ?? "")}`
      : `when ${subject}'s ${target.key} becomes ${String(target.value ?? "")}`;
  if (condition.selection.where?.pathPrefix)
    words += `, under ${condition.selection.where.pathPrefix}`;
  return words;
}

export function eventTriggerWords(event: {
  kind: string;
  from_agent?: string;
  metadata?: RoutineMetadataCondition;
}): string {
  if (event.kind === "metadata_change" && event.metadata)
    return metadataTriggerWords(event.metadata);
  if (event.kind === "role_pending") return "when a message is queued for a role";
  if (event.kind === "inbox_idle") return "when an agent sits idle with unread mail";
  if (event.kind === "agent_done")
    return event.from_agent ? `when ${event.from_agent} finishes` : "when an agent finishes";
  return `on ${event.kind}`;
}

/** The sentence inputs and event conditions held by the editor draft. */
export interface SentenceDraft {
  action: "agent" | "command";
  command: string[];
  after?: RoutineAfter;
  events?: RoutineEventDef[];
  name: string;
  when: string;
  tz: string;
  agent: string;
  task: string;
  where: string;
  overlap: RoutineDef["policy"]["overlap"];
  missed: RoutineDef["policy"]["missed"];
  maxRuntime: string;
  maxRuntimeTouched?: boolean;
  visible: boolean;
  resultSchema?: unknown;
  /** A team routine's mode; a personal routine has none and never sends it. */
  runsOn: RoutineRunsOn;
  /** What a fire does, as picked; blank shows (and a create saves) the agent's
   *  default ({@link defaultMode}). */
  mode: RoutineMode | "";
  /** Where a wake looks for the agent (SW-AGR-5): `local` (This Mac), a team-shared
   *  machine id, or `any`. */
  machine: string;
  /** How long a wake waits for idle (`policy.max_wait`), shown; blank is the default. */
  maxWait?: string;
  /** SW-AGR-13's wake limits, as typed; blank is no limit. */
  maxWakes?: string;
  maxWakesPerDay?: string;
  minGap?: string;
}

export function metadataConditions(
  draft: Pick<SentenceDraft, "events">,
): RoutineMetadataCondition[] {
  return (draft.events ?? []).flatMap((e) =>
    e.kind === "metadata_change" && e.metadata ? [e.metadata] : [],
  );
}

export function metadataProblem(draft: Pick<SentenceDraft, "events" | "runsOn">): string | null {
  for (const condition of metadataConditions(draft)) {
    if (!condition.target.key.trim()) return "Pick a field and the value it changes to.";
    if (condition.target.value === undefined || String(condition.target.value).trim() === "")
      return `Say what ${condition.target.key} changes to.`;
    // Editor operands are text, whose eq comparison is exact in the query engine.
    if (
      typeof condition.previous?.value === "string" &&
      typeof condition.target.value === "string" &&
      condition.previous.value === condition.target.value
    )
      return `From and to are both ${condition.target.value}, so this never fires. Clear From, or pick another value.`;
  }
  if (metadataConditions(draft).length && draft.runsOn === "each_mac")
    return "Metadata triggers run once for the team.";
  return null;
}

/** A typed wake count as the form sends it: a whole number, or the text as typed so
 *  the save refuses it by name. Blank is null (no limit). */
function wakeCount(raw: string | undefined): number | string | null {
  const text = (raw ?? "").trim();
  if (!text) return null;
  return /^\d+$/.test(text) ? Number(text) : text;
}

/** What keeps the draft's limits from being saved, in the form's words, or null. The
 *  save checks the ranges (`min_gap` 1m to 7d) itself. */
export function limitsProblem(draft: SentenceDraft): string | null {
  const count = (raw: string | undefined) => {
    const text = (raw ?? "").trim();
    return text === "" || /^[1-9]\d*$/.test(text);
  };
  if (!count(draft.maxWakes)) return "Max wakes must be a whole number of 1 or more.";
  if (!count(draft.maxWakesPerDay)) return "Max wakes a day must be a whole number of 1 or more.";
  const gap = (draft.minGap ?? "").trim();
  if (gap && !/^\d+[mhd]$/.test(gap)) return "Min gap must be written like 30m, 2h or 1d.";
  return null;
}

/** The limits a draft sets, in words: `20 wakes in all · 3 a day · 4h apart`, or
 *  `none`. */
export function limitsSummary(draft: SentenceDraft): string {
  const parts = [
    draft.maxWakes?.trim() && `${draft.maxWakes.trim()} wakes in all`,
    draft.maxWakesPerDay?.trim() && `${draft.maxWakesPerDay.trim()} a day`,
    draft.minGap?.trim() && `${draft.minGap.trim()} apart`,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : "none";
}

/** Whether a routine on `agent` may wake it: a saved agent, never a built-in
 *  starter (its routines are fresh only, §9.1). */
export function canWake(agent: Pick<AgentRow, "key"> | undefined, ref: string): boolean {
  const key = agent?.key ?? ref.trim();
  return key !== "" && !key.startsWith("builtin/");
}

/** The mode a new routine gets when its author picks none, as the backend writes it:
 *  `wake` for a visible routine on a saved agent that launches in a tab, else `fresh`. */
export function defaultMode(
  agent: Pick<AgentRow, "key" | "launch"> | undefined,
  visible: boolean,
): RoutineMode {
  return agent && canWake(agent, agent.key) && agent.launch === "foreground" && visible
    ? "wake"
    : "fresh";
}

/** The mode the form shows: the pick, else the agent's default. */
export function shownMode(draft: SentenceDraft, agent: AgentRow | undefined): RoutineMode {
  if (draft.action === "command") return "fresh";
  if (!canWake(agent, draft.agent)) return "fresh";
  return draft.mode || defaultMode(agent, draft.visible);
}

/** A blank form for a new routine: no schedule, no agent yet, team workspace. */
export function emptyDraft(): SentenceDraft {
  return {
    action: "agent",
    command: [""],
    name: "",
    when: "",
    tz: "",
    agent: "",
    task: "",
    where: "team_workspace",
    overlap: "coalesce",
    missed: "skip",
    maxRuntime: "30m",
    visible: false,
    runsOn: "one_mac",
    mode: "",
    machine: "local",
  };
}

/** The form a routine opens with. */
export function draftFromDef(def: RoutineDef): SentenceDraft {
  const schedule = firstSchedule(def);
  return {
    action: def.action ?? "agent",
    command: def.command?.length ? def.command : [""],
    after: def.after,
    events: (def.triggers ?? []).flatMap((t) => ("event" in t ? [t.event] : [])),
    name: def.name,
    resultSchema: def.result_schema,
    when: schedule ? slotFromCron(schedule.cron) : "",
    tz: schedule?.tz ?? "",
    agent: def.agent ?? "",
    task: def.task ?? "",
    where: def.where.kind,
    overlap: def.policy.overlap,
    missed: def.policy.missed,
    maxRuntime: def.policy.max_runtime ?? "",
    visible: def.policy.visible,
    runsOn: runsOn(def),
    mode: def.mode ?? "",
    machine: def.where.machine?.trim() || "local",
    maxWait: def.policy.max_wait ?? "",
    maxWakes: def.policy.max_wakes?.toString() ?? "",
    maxWakesPerDay: def.policy.max_wakes_per_day?.toString() ?? "",
    minGap: def.policy.min_gap ?? "",
  };
}

/** What a create sends: every field, the schedule as typed, and the mode the form
 *  shows (`mode`, the agent's default when nothing was picked). */
export function createForm(draft: SentenceDraft, mode?: RoutineMode): RoutineForm {
  const picked = mode ?? (draft.mode || undefined);
  const command = draft.action === "command";
  return {
    ...(command
      ? { action: "command" as const, command: commandArgs(draft.command) }
      : {
          ...(picked ? { mode: picked } : {}),
          task: draft.task.trim(),
          agent: draft.agent.trim() || undefined,
        }),
    ...(draft.after ? { after: draft.after } : {}),
    ...(metadataConditions(draft).length ? { metadataTriggers: metadataConditions(draft) } : {}),
    name: draft.name.trim(),
    ...(!command ? { resultSchema: draft.resultSchema } : {}),
    when: draft.when.trim(),
    tz: draft.tz.trim() || undefined,
    where: draft.where.trim() || undefined,
    overlap: draft.overlap,
    missed: draft.missed,
    maxRuntime: draft.maxRuntime.trim() || undefined,
    ...(!command ? { visible: draft.visible, ...limitFields(draft) } : {}),
    machine: draft.machine.trim() || "local",
  };
}

/** The limits a create sends: only the ones set. */
function limitFields(
  draft: SentenceDraft,
): Pick<RoutineForm, "maxWakes" | "maxWakesPerDay" | "minGap"> {
  const out: Pick<RoutineForm, "maxWakes" | "maxWakesPerDay" | "minGap"> = {};
  const maxWakes = wakeCount(draft.maxWakes);
  const perDay = wakeCount(draft.maxWakesPerDay);
  const minGap = draft.minGap?.trim();
  if (maxWakes !== null) out.maxWakes = maxWakes;
  if (perDay !== null) out.maxWakesPerDay = perDay;
  if (minGap) out.minGap = minGap;
  return out;
}

/** What an edit sends: only the fields that changed from `base`, so an untouched
 *  schedule keeps every trigger the routine has. Null when nothing changed. */
export function editForm(base: SentenceDraft, draft: SentenceDraft): RoutineForm | null {
  const form: RoutineForm = {};
  const switched = draft.action !== base.action;
  if (switched) form.action = draft.action;
  if (
    draft.action === "command" &&
    (switched ||
      JSON.stringify(commandArgs(draft.command)) !== JSON.stringify(commandArgs(base.command)))
  )
    form.command = commandArgs(draft.command);
  if (JSON.stringify(draft.after) !== JSON.stringify(base.after)) form.after = draft.after ?? null;
  if (JSON.stringify(metadataConditions(base)) !== JSON.stringify(metadataConditions(draft)))
    form.metadataTriggers = metadataConditions(draft);
  if (JSON.stringify(draft.resultSchema) !== JSON.stringify(base.resultSchema))
    form.resultSchema = draft.resultSchema;
  if (draft.name.trim() !== base.name) form.name = draft.name.trim();
  if (draft.action !== "command" && (switched || draft.task.trim() !== base.task))
    form.task = draft.task.trim();
  if (draft.when.trim() !== base.when) {
    form.when = draft.when.trim();
    if (draft.tz.trim()) form.tz = draft.tz.trim();
  } else if (draft.tz.trim() !== base.tz && draft.tz.trim()) {
    // A zone alone moves every schedule into it and keeps each one.
    form.tz = draft.tz.trim();
  }
  // A blank agent or max runtime keeps what the routine has (the backend reads
  // blank as unset), so clearing one is not a change to save.
  const agent = draft.agent.trim();
  if (draft.action !== "command" && agent && (switched || agent !== base.agent)) form.agent = agent;
  if (draft.where.trim() !== base.where) form.where = draft.where.trim();
  if (draft.overlap !== base.overlap) form.overlap = draft.overlap;
  if (draft.missed !== base.missed) form.missed = draft.missed;
  const maxRuntime = draft.maxRuntime.trim();
  if (maxRuntime && maxRuntime !== base.maxRuntime) form.maxRuntime = maxRuntime;
  if (draft.action !== "command" && draft.visible !== base.visible) form.visible = draft.visible;
  if (draft.runsOn !== base.runsOn) form.runsOn = draft.runsOn;
  if (draft.action !== "command" && draft.mode && (switched || draft.mode !== base.mode))
    form.mode = draft.mode;
  if (draft.machine.trim() && draft.machine.trim() !== base.machine)
    form.machine = draft.machine.trim();
  // Limits: unlike the fields above, clearing one is an edit (null removes it).
  if (draft.action !== "command" && (draft.maxWakes ?? "").trim() !== (base.maxWakes ?? ""))
    form.maxWakes = wakeCount(draft.maxWakes);
  if (
    draft.action !== "command" &&
    (draft.maxWakesPerDay ?? "").trim() !== (base.maxWakesPerDay ?? "")
  )
    form.maxWakesPerDay = wakeCount(draft.maxWakesPerDay);
  if (draft.action !== "command" && (draft.minGap ?? "").trim() !== (base.minGap ?? ""))
    form.minGap = draft.minGap?.trim() || null;
  return Object.keys(form).length > 0 ? form : null;
}

/** One field a revision changed. */
export interface FieldChange {
  field: string;
  before: string;
  after: string;
}

function scheduleText(def: RoutineDef): string {
  return (def.triggers ?? [])
    .map((t) =>
      "schedule" in t ? `${t.schedule.cron} (${t.schedule.tz})` : eventTriggerWords(t.event),
    )
    .join(", ");
}

/** What `next` changed against `prev`, field by field, in the sentence's order. */
export function revisionDiff(
  prev: RoutineDef,
  next: RoutineDef,
  names?: ReadonlyMap<string, string>,
): FieldChange[] {
  const fields: [string, (d: RoutineDef) => string][] = [
    ["Name", (d) => d.name],
    [
      "Mode",
      (d) =>
        d.action === "command" ? "run the command" : d.mode === "wake" ? "wake" : "start a new",
    ],
    ["Command", (d) => d.command?.join(" ") ?? ""],
    ["After", (d) => (d.after ? afterWords(d.after, names) : "")],
    ["Triggers", scheduleText],
    ["Agent", (d) => d.agent ?? ""],
    ["Task", (d) => d.task ?? ""],
    ["Result schema", (d) => JSON.stringify(d.result_schema) ?? ""],
    ["Where", (d) => d.where.kind],
    ["Overlap", (d) => d.policy.overlap],
    ["Missed", (d) => d.policy.missed],
    ["Max runtime", (d) => d.policy.max_runtime ?? ""],
    ["Visible", (d) => (d.policy.visible ? "yes" : "no")],
    ["Max wakes", (d) => d.policy.max_wakes?.toString() ?? ""],
    ["Max wakes a day", (d) => d.policy.max_wakes_per_day?.toString() ?? ""],
    ["Min gap", (d) => d.policy.min_gap ?? ""],
    ["Runs on", (d) => (runsOn(d) === "each_mac" ? "each teammate's Mac" : "one Mac")],
    ["Machine", (d) => d.where.machine ?? "local"],
    ["Armed on", (d) => d.armed_host],
    ["Archived", (d) => (d.archived_at ? "yes" : "no")],
  ];
  return fields
    .map(([field, read]) => ({ field, before: read(prev), after: read(next) }))
    .filter((change) => change.before !== change.after);
}

/** Why the machine slot can't be saved as it is (design §4.6), or null. */
export type MachineProblem = "unshared" | "each_mac_remote";

/** What is wrong with the draft's machine for `mode`. `known` is the machines the team
 *  shares (with `local`), or null before they are read: nothing is "unshared" until then.
 *  A recipe each teammate's Mac runs only wakes that Mac's own agent (§9.9). A fresh run
 *  may start on another machine (SW-AGR-2b). */
export function machineProblem(
  draft: Pick<SentenceDraft, "machine" | "runsOn">,
  _mode: RoutineMode,
  known: readonly string[] | null,
  team: boolean,
): MachineProblem | null {
  const machine = draft.machine.trim() || "local";
  if (team && draft.runsOn === "each_mac" && machine !== "local") return "each_mac_remote";
  if (known && machine !== "local" && machine !== "any" && !known.includes(machine)) {
    return "unshared";
  }
  return null;
}

/** A machine problem in the design's words. */
export function machineProblemCopy(
  problem: MachineProblem,
  machine: string,
  _agent: string,
): string {
  switch (problem) {
    case "unshared":
      return `Pick a machine. ${machine} isn't shared with this team any more.`;
    case "each_mac_remote":
      return "each teammate's Mac only works with This Mac for wake.";
  }
}

/** The routine moved under its agent and nobody has saved it since: its form says why it
 *  starts a new session (§9.2). One migrated before the migration recorded its revision
 *  keeps the note while it still starts a new session. */
export function migratedUnsaved(def: Pick<RoutineDef, "migrated" | "revision">): boolean {
  if (!def.migrated) return false;
  return def.migrated.revision === undefined || def.migrated.revision === def.revision;
}

export function commandArgs(argv: string[]): string[] {
  const out = [...argv];
  while (out.length > 1 && out[out.length - 1] === "") out.pop();
  return out;
}

export function afterWords(after: RoutineAfter, names?: ReadonlyMap<string, string>): string {
  return `after ${names?.get(after.routine) ?? `${after.routine} (gone)`} ${after.on === "succeeded" ? "succeeds" : "finishes"}`;
}

export function needsUpstream(def: Pick<RoutineDef, "after" | "command" | "task">): boolean {
  return (
    !!def.after && [...(def.command ?? []), def.task ?? ""].some((s) => /\{\{trigger\./.test(s))
  );
}

export function upstreamRunReason(
  row: Pick<RoutineRow, "def" | "key">,
  rows: readonly RoutineRow[] | null,
): string | undefined {
  if (!needsUpstream(row.def)) return undefined;
  const space = row.key.slice(0, row.key.lastIndexOf("/") + 1);
  const name =
    rows?.find((r) => r.key === space + row.def.after?.routine)?.def.name ?? row.def.after?.routine;
  return `Runs only after ${name}: it needs that run's result.`;
}
