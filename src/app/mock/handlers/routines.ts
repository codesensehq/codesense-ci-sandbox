// SPDX-License-Identifier: MIT
import { eventTriggerWords } from "@/features/routines/lib/routine-sentence";
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import type {
  AgentLaunchWarning,
  RoutineAdmission,
  RoutineAgentDisplay,
  RoutineDef,
  RoutineDetail,
  RoutineForm,
  RoutineRevision,
  RoutineRow,
  RoutineRun,
  RoutineRunDetail,
  RoutineSaveReply,
  RoutineSchedulePreview,
  SavedRoutine,
} from "@/tauri/commands";
import type { RunOutcomeRead } from "@/shared/lib/run-outcome";
import { ROUTINES_CHANGED_EVENT } from "@/tauri/events";

/**
 * The Routines mock backend: a handful of routines (the user's own, one an agent
 * created, one armed on another Mac, a teammate's shared job on another Mac, a
 * teammate's recipe each Mac runs, every one editable) with a run
 * history that covers every row kind the Runs view renders — succeeded, skipped
 * with reasons, merged, needs-you — plus a missed count. Every mutation updates
 * the in-memory rows and emits `routines:changed`, so the tab moves the way it
 * does against the real backend: Run now admits a running run that succeeds a
 * few seconds later, and Stop ends one `cancelled`.
 */

const ME = "1";
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** How long a mock run runs before it succeeds. */
const MOCK_RUN_MS = 4_000;

function def(
  id: string,
  name: string,
  agent: string,
  owner: string,
  cron: string | null,
  task: string,
): RoutineDef {
  return {
    id,
    name,
    owner,
    agent,
    task,
    where: { kind: "team_workspace" },
    triggers: cron ? [{ schedule: { cron, tz: "America/Denver" } }] : [],
    policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: false },
    revision: 1,
    armed_host: owner === ME ? "this-mac" : "sams-mac",
  };
}

export interface MockRoutine {
  row: Omit<RoutineRow, "last_run" | "needs_you" | "needs_you_run" | "agent_display">;

  runs: RoutineRun[];
  /** Oldest first; revision 1 is synthesized from the row when absent. */
  revisions?: RoutineRevision[];
}

const DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const DAY_NAMES = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

/** The mock's `parse_when`: the backend's presets, the sentence slot forms
 *  after an optional "every" ("5m", "hour", "day 09:00", "weekday 09:00",
 *  "mon 17:00"), or a 5-field cron. */
export function mockParseWhen(text: string): string {
  const all = text.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const words = all.length > 1 && all[0] === "every" ? all.slice(1) : all;
  const clock = (at: string | undefined): [number, number] | null => {
    const m = /^(\d{1,2}):(\d{2})$/.exec(at ?? "");
    if (!m) return null;
    const [h, min] = [Number(m[1]), Number(m[2])];
    return h < 24 && min < 60 ? [h, min] : null;
  };
  const dayOf = (word: string) =>
    DAYS.findIndex((d) => word === d || (word.length > 3 && DAY_NAMES[DAYS.indexOf(d)] === word));
  const bad = () => new Error(`unrecognized schedule ${JSON.stringify(text.trim())}`);
  const [first, second, third] = words;
  if (words.length === 1 && (first === "hourly" || first === "hour")) return "0 * * * *";
  if (words.length === 1) {
    const m = /^(\d+)([mh])$/.exec(first ?? "");
    if (!m) throw bad();
    const n = Number(m[1]);
    if (m[2] === "m" && [5, 10, 15, 20, 30].includes(n)) return `*/${n} * * * *`;
    if (m[2] === "h" && n > 0 && 24 % n === 0) return `0 */${n} * * *`;
    throw bad();
  }
  if (words.length === 2 && ["daily", "day", "weekdays", "weekday"].includes(first)) {
    const at = clock(second);
    if (!at) throw bad();
    return `${at[1]} ${at[0]} * * ${first.startsWith("week") ? "1-5" : "*"}`;
  }
  if ((words.length === 3 && first === "weekly") || words.length === 2) {
    const [dayWord, atWord] = words.length === 3 ? [second, third] : [first, second];
    const day = dayOf(dayWord);
    const at = clock(atWord);
    if (day < 0 || !at) throw bad();
    return `${at[1]} ${at[0]} * * ${day}`;
  }
  if (words.length === 5) return words.join(" ");
  throw bad();
}

/** The next `n` fires of the crons the mock writes (no general cron engine). */
function mockNextFires(cron: string, from: number, n: number): number[] {
  const [min, hour, , , dow] = cron.split(" ");
  const out: number[] = [];
  const t = new Date(from);
  t.setSeconds(0, 0);
  for (let i = 0; out.length < n && i < 60 * 24 * 14; i += 1) {
    t.setMinutes(t.getMinutes() + 1);
    const matches = (field: string, value: number) => {
      if (field === "*") return true;
      const step = /^\*\/(\d+)$/.exec(field);
      if (step) return value % Number(step[1]) === 0;
      const range = /^(\d+)-(\d+)$/.exec(field);
      if (range) return value >= Number(range[1]) && value <= Number(range[2]);
      return Number(field) === value;
    };
    if (matches(min, t.getMinutes()) && matches(hour, t.getHours()) && matches(dow, t.getDay())) {
      out.push(t.getTime());
    }
  }
  return out;
}

function mockInWords(cron: string): string {
  const [min, hour, , , dow] = cron.split(" ");
  const every = /^\*\/(\d+)$/;
  if (every.test(min)) return `every ${every.exec(min)![1]} minutes`;
  if (min === "0" && hour === "*") return "every hour";
  if (min === "0" && every.test(hour)) return `every ${every.exec(hour)![1]} hours`;
  const at = `${hour}:${min.padStart(2, "0")}`;
  if (dow === "1-5") return `weekdays at ${at}`;
  if (dow === "*") return `daily at ${at}`;
  return `${DAYS[Number(dow)] ?? dow} at ${at}`;
}

function seed(now: number): MockRoutine[] {
  const run = (
    key: string,
    n: number,
    phase: RoutineRun["phase"],
    endedAgo: number,
    extra: Partial<RoutineRun> = {},
  ): RoutineRun => ({
    run_id: `${key.split("/").pop()}-run-${n}`,
    routine_key: key,
    revision: 1,
    source: "schedule",
    occurrence_key: null,
    phase,
    reason: null,
    linked_run_id: null,
    session_id: phase === "skipped" || phase === "coalesced" ? null : `sess-${n}`,
    created_at: now - endedAgo - 2 * MINUTE,
    started_at: phase === "skipped" || phase === "coalesced" ? null : now - endedAgo - MINUTE,
    ended_at: now - endedAgo,
    result_text: null,
    detail: null,
    agent: null,
    ...extra,
  });
  const triage = "_personal/1/personal/morning-triage";
  const digest = "acme/backend/weekly-digest";
  const sweep = "_personal/1/personal/pr-sweeper";
  const standup = "acme/backend/standup-notes";
  const release = "acme/backend/release-notes";
  const inbox = "acme/backend/my-inbox-digest";
  return [
    {
      row: {
        key: triage,
        scope: "personal",
        armed_here: true,
        owner_display: "you",
        enabled: true,
        created_by: { kind: "agent", name: "triage-bot" },
        schedule_in_words: "weekdays at 9:00 MDT",
        next_run_at: now + 3 * HOUR,
        def: def(
          "morning-triage",
          "Morning triage",
          "triage",
          ME,
          "0 9 * * 1-5",
          "Sweep the support channel and post what needs an owner.",
        ),
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: true,
      },
      runs: [
        run(triage, 4, "skipped", 2 * HOUR, { reason: "missed:1" }),
        run(triage, 3, "coalesced", 26 * HOUR, { linked_run_id: "morning-triage-run-2" }),
        run(triage, 2, "succeeded", 26 * HOUR, {
          result_text: "3 threads need an owner; posted the list to #support.",
        }),
        run(triage, 1, "skipped", 50 * HOUR, { reason: "already_running" }),
      ],
    },
    {
      row: {
        key: digest,
        scope: "team",
        armed_here: true,
        owner_display: "you",
        enabled: true,
        created_by: { kind: "user", name: "octocat" },
        schedule_in_words: "Mondays at 8:30 MDT",
        next_run_at: now + 50 * HOUR,
        def: def(
          "weekly-digest",
          "Weekly digest",
          "writer",
          ME,
          "30 8 * * 1",
          "Write the weekly digest of merged work.",
        ),
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: true,
      },
      runs: [
        run(digest, 2, "needs_you", 5 * HOUR, {
          result_text: "Which repos belong in the digest? Two new ones appeared.",
        }),
        run(digest, 1, "succeeded", 7 * 24 * HOUR, { result_text: "Posted the digest." }),
      ],
    },
    {
      row: {
        key: sweep,
        scope: "personal",
        armed_here: false,
        owner_display: "you",
        enabled: true,
        created_by: { kind: "user", name: "octocat" },
        schedule_in_words: "every 2 hours",
        next_run_at: now + 40 * MINUTE,
        def: def("pr-sweeper", "PR sweeper", "reviewer", ME, "0 */2 * * *", "Nudge stale PRs."),
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: true,
      },
      // Its agent exits without calling signal_done: the run simply ends.
      runs: [run(sweep, 1, "ended", 80 * MINUTE)],
    },
    {
      row: {
        key: standup,
        scope: "team",
        armed_here: false,
        owner_display: "Sam",
        enabled: true,
        created_by: { kind: "user", name: "sam" },
        schedule_in_words: "weekdays at 8:00 MDT",
        next_run_at: now + 2 * HOUR,
        def: def(
          "standup-notes",
          "Standup notes",
          "scribe",
          "2",
          "0 8 * * 1-5",
          "Collect yesterday's merges into standup notes.",
        ),
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: false,
      },
      runs: [run(standup, 1, "succeeded", 22 * HOUR, { result_text: "Notes posted." })],
    },
    {
      // Written by hand with no Mac named: it runs nowhere until one is picked.
      row: {
        key: release,
        scope: "team",
        armed_here: false,
        owner_display: "Sam",
        enabled: true,
        created_by: { kind: "user", name: "sam" },
        schedule_in_words: "Fridays at 16:00 MDT",
        next_run_at: null,
        def: {
          ...def(
            "release-notes",
            "Release notes",
            "writer",
            "2",
            "0 16 * * 5",
            "Draft release notes from this week's merged PRs.",
          ),
          armed_host: "",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: false,
      },
      runs: [],
    },
    {
      // A recipe each teammate's Mac runs: off here until turned on.
      row: {
        key: inbox,
        scope: "team",
        armed_here: true,
        owner_display: "Sam",
        enabled: false,
        created_by: { kind: "user", name: "sam" },
        schedule_in_words: "weekdays at 8:15 MDT",
        next_run_at: null,
        def: {
          ...def(
            "my-inbox-digest",
            "My inbox digest",
            "scribe",
            "2",
            "15 8 * * 1-5",
            "Summarize the PRs and issues waiting on me and post them to my DMs.",
          ),
          armed_host: "",
          runs_on: "each_mac",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: false,
      },
      runs: [],
    },
  ];
}

const TERMINAL = new Set(["succeeded", "failed", "needs_you", "ended", "timed_out", "cancelled"]);
/** The runs a list row's last run reads: everything but a fire that never ran. */
const STARTED = new Set([...TERMINAL, "running", "queued", "delivered", "undelivered", "unknown"]);

/**
 * SW-AGR-5: the routines world `pnpm dev:mock` boots with — routines that belong to the
 * agents the agents mock lists (Triage, Scribe, Reviewer, Dev), covering every state the
 * Agents tab shows: a wake queued on a busy session (`⏰ due`), an `unknown` wake, an
 * undelivered one, a fresh routine that is off, an event-only routine, a recipe each Mac
 * runs, a personal one, and one Unassigned whose agent is gone.
 */
export function agentRoutinesSeed(now: number, remote = "gpu-box-2"): MockRoutine[] {
  const TRIAGE = "acme/backend/triage";
  const SCRIBE = "acme/backend/scribe";
  const REVIEWER = "acme/backend/reviewer";
  const DEV = "_personal/1/personal/dev";
  const at = (minutes: number) => now + minutes * MINUTE;
  const run = (
    key: string,
    n: number,
    phase: RoutineRun["phase"],
    endedAgoMin: number,
    extra: Partial<RoutineRun> = {},
  ): RoutineRun => ({
    run_id: `run_${key.split("/").pop()!.slice(0, 4)}${n}`,
    routine_key: key,
    revision: 1,
    source: "schedule",
    occurrence_key: null,
    phase,
    reason: null,
    linked_run_id: null,
    session_id: phase === "skipped" || phase === "coalesced" ? null : "s-triage",
    created_at: at(-endedAgoMin - 1),
    started_at: phase === "skipped" || phase === "coalesced" ? null : at(-endedAgoMin - 1),
    ended_at: phase === "running" || phase === "queued" ? null : at(-endedAgoMin),
    result_text: null,
    detail: null,
    agent: null,
    ...extra,
  });
  const routine = (
    key: string,
    agentKey: string | null,
    d: RoutineDef,
    words: string,
    nextAt: number | null,
    extra: Partial<MockRoutine["row"]> = {},
  ): MockRoutine["row"] => ({
    key,
    scope: key.startsWith("_personal/") ? "personal" : "team",
    armed_here: true,
    owner_display: "you",
    enabled: true,
    created_by: { kind: "user", name: "alice" },
    schedule_in_words: words,
    next_run_at: nextAt,
    def: d,
    agent_key: agentKey,
    unassigned: false,
    duplicate: false,
    effective_mode: d.mode ?? "fresh",
    mine: true,
    ...extra,
  });
  const wake = (d: RoutineDef, machine: string): RoutineDef => ({
    ...d,
    mode: "wake",
    where: { kind: "team_workspace", machine },
    policy: { ...d.policy, visible: true },
  });
  const triage = "_personal/1/personal/morning-triage";
  const sweep = "acme/backend/inbox-sweep";
  const retro = "acme/backend/weekly-retro";
  const standup = "acme/backend/standup-notes";
  const inbox = "acme/backend/my-inbox-digest";
  const review = "acme/backend/review-after-triage";
  const prs = "_personal/1/personal/pr-sweeper";
  const changelog = "acme/backend/weekly-changelog";
  return [
    {
      row: routine(
        triage,
        TRIAGE,
        wake(
          def(
            "morning-triage",
            "Morning triage",
            "triage",
            ME,
            "0 9 * * 1-5",
            "Triage new issues and post what needs an owner.",
          ),
          remote,
        ),
        "weekdays at 9:00",
        at(18),
        {
          queued: {
            runId: "run_morn3",
            machineId: remote,
            sessionId: "s-triage",
            marker: "Morning triage",
            deadlineMs: at(48),
            unconfirmed: false,
          },
        },
      ),
      runs: [
        run(triage, 3, "queued", 0, { wake_machine: remote }),
        run(triage, 2, "succeeded", 24 * 60, {
          delivered_at: at(-24 * 60 - 1),
          wake_machine: remote,
          result_text: "3 issues need an owner; posted the list to #support.",
        }),
        run(triage, 1, "undelivered", 3 * 24 * 60, {
          reason: "busy_timeout",
          wake_machine: remote,
        }),
      ],
    },
    {
      row: routine(
        sweep,
        TRIAGE,
        wake(
          def(
            "inbox-sweep",
            "Inbox sweep",
            "triage",
            ME,
            "*/30 * * * *",
            "Sweep the team inbox and answer anything addressed to you.",
          ),
          "local",
        ),
        "every 30 minutes",
        at(18),
      ),
      runs: [
        run(sweep, 4, "unknown", 12, { wake_machine: "local" }),
        run(sweep, 3, "running", 42, { delivered_at: at(-43), wake_machine: "local" }),
        run(sweep, 2, "coalesced", 57, { linked_run_id: "run_inbo3" }),
        run(sweep, 1, "undelivered", 9 * 60, { reason: "session_ended", wake_machine: "local" }),
      ],
    },
    {
      row: routine(
        retro,
        TRIAGE,
        {
          ...def(
            "weekly-retro",
            "Weekly retro",
            "triage",
            ME,
            "0 16 * * 5",
            "Draft the retro from this week's merged work.",
          ),
          mode: "fresh",
        },
        "Fridays at 16:00",
        null,
        { enabled: false },
      ),
      // A fresh run that started its agent on the remote machine (SW-AGR-2b).
      runs: [
        run(retro, 1, "succeeded", 5 * 24 * 60, {
          result_text: "Retro drafted.",
          launch_machine: remote,
        }),
      ],
    },
    {
      row: routine(
        standup,
        SCRIBE,
        {
          ...def(
            "standup-notes",
            "Standup notes",
            "scribe",
            "2",
            "0 8 * * 1-5",
            "Collect yesterday's merges into standup notes.",
          ),
          mode: "fresh",
          migrated: { from: "routines/standup-notes", revision: 1 },
        },
        "weekdays at 8:00",
        at(23 * 60),
        { owner_display: "Sam", mine: false },
      ),
      runs: [run(standup, 1, "succeeded", 40, { result_text: "Notes posted." })],
    },
    {
      row: routine(
        inbox,
        SCRIBE,
        {
          ...def(
            "my-inbox-digest",
            "My inbox digest",
            "scribe",
            "2",
            "15 8 * * 1-5",
            "Summarize the PRs and issues waiting on me and post them to my DMs.",
          ),
          mode: "fresh",
          armed_host: "",
          runs_on: "each_mac",
        },
        "weekdays at 8:15",
        null,
        { owner_display: "Sam", mine: false, enabled: false },
      ),
      runs: [],
    },
    {
      row: routine(
        review,
        REVIEWER,
        {
          ...wake(
            def(
              "review-after-triage",
              "Review after triage",
              "reviewer",
              ME,
              null,
              "Review the issues Triage just labelled.",
            ),
            remote,
          ),
          triggers: [{ event: { kind: "agent_done", from_agent: "Triage" } }],
        },
        "when Triage finishes",
        null,
      ),
      runs: [
        run(review, 1, "undelivered", 2 * 60, {
          reason: "needs_preparation",
          wake_machine: remote,
        }),
      ],
    },
    {
      row: routine(
        prs,
        DEV,
        {
          ...def("pr-sweeper", "PR sweeper", "dev", ME, "0 */2 * * *", "Nudge stale PRs."),
          mode: "fresh",
        },
        "every 2 hours",
        at(78),
      ),
      runs: [run(prs, 1, "ended", 42)],
    },
    {
      row: routine(
        changelog,
        null,
        {
          ...def(
            "weekly-changelog",
            "Weekly changelog",
            "pockets",
            ME,
            "0 17 * * 5",
            "Write the weekly changelog.",
          ),
          mode: "fresh",
        },
        "Fridays at 17:00",
        null,
        { unassigned: true },
      ),
      runs: [],
    },
  ];
}

/** An agent the mock knows, by id. */
export interface MockAgent {
  name: string;
  archived?: boolean;
  /** The `/name`s it starts with. */
  commands?: string[];
}

/** The demo agents. `reviewer` is missing on purpose: the PR sweeper names it, so its
 *  row shows the unresolvable-agent chip. */
const AGENTS: Record<string, MockAgent> = {
  default: { name: "Default" },
  dev: { name: "Dev" },
  scribe: { name: "Scribe" },
  triage: { name: "Triage", commands: ["triage-slack"] },
  writer: { name: "Writer", archived: true },
};

/** The starters every Mac has, by key, as the agents mock lists them. */
const STARTERS: Record<string, MockAgent> = {
  "builtin/claude": { name: "Claude" },
  "builtin/codex": { name: "Codex" },
  "builtin/claude-unattended": { name: "Claude (unattended)" },
  "builtin/codex-unattended": { name: "Codex (unattended)" },
};

export interface MockRoutinesOptions {
  now?: () => number;
  /** The agents routines may name; the demo set by default. Personal and team agents
   *  share one table here. */
  agents?: Record<string, MockAgent>;
  /** The routines the world starts with; the demo set by default. */
  seed?: (now: number) => MockRoutine[];
  missed?: number;
  /** How long a Run now runs before it succeeds; `null` leaves it running. */
  runMs?: number | null;
}

/** A mock routines world: its command handlers, and a way to end a run the way
 *  the agent running it would. */
export interface MockRoutines {
  handlers: HandlerRegistry;
  finishRun: (runId: string, phase: RoutineRun["phase"], resultText: string | null) => void;
  /** Edit a routine as an agent would through `routine_update`. */
  agentEdit: (key: string, agent: string, form: RoutineForm) => void;
  /** The definition of the routine named `name`, as saved. */
  defOf: (name: string) => RoutineDef | undefined;
}

export function createMockRoutines(
  events: MockEventBus,
  options: MockRoutinesOptions = {},
): MockRoutines {
  const now = options.now ?? Date.now;
  const routines = (options.seed ?? seed)(now());
  let missed = options.missed ?? 1;
  let nextRun = 1;
  const runMs = options.runMs === undefined ? MOCK_RUN_MS : options.runMs;
  const agents = options.agents ?? AGENTS;
  /** The agent a reference names: a starter's key, a bare id, or the id at the end of an
   *  agent key. */
  const agentOf = (ref = ""): MockAgent | undefined =>
    STARTERS[ref] ?? (/:/.test(ref) ? undefined : agents[ref.split("/").pop() ?? ""]);
  const agentDisplay = (ref = ""): RoutineAgentDisplay => {
    const agent = agentOf(ref);
    return agent
      ? { name: agent.name, archived: agent.archived === true, resolvable: true }
      : { name: ref, archived: false, resolvable: false };
  };
  /** As the backend checks a save's agent: one you can see, else name agent_list. */
  const checkAgent = (ref: string | undefined) => {
    if (!ref?.trim()) throw new Error("name an agent (see agent_list)");
    if (/:/.test(ref)) {
      throw new Error(`agent ${JSON.stringify(ref)} is not an agent id or key; see agent_list`);
    }
    if (!agentOf(ref.trim())) {
      throw new Error(`no agent ${JSON.stringify(ref.trim())} that you can see; see agent_list`);
    }
  };
  /** The backend's leading-command warning. */
  const warningsFor = (def: RoutineDef): AgentLaunchWarning[] => {
    const name = /^\/([A-Za-z0-9_-]+)/.exec((def.task ?? "").trim())?.[1];
    const agent = agentOf(def.agent);
    if (!name || !agent || agent.commands?.includes(name)) return [];
    return [
      {
        code: "command_not_in_manifest",
        message: `this agent doesn't start with /${name} — add it to the agent or the run won't find it`,
      },
    ];
  };

  const find = (key: unknown): MockRoutine => {
    const found = routines.find((r) => r.row.key === key);
    if (!found) throw new Error(`routine ${String(key)} not found`);
    return found;
  };
  const changed = (routineKey: string, runId: string | null = null) =>
    events.emit(ROUTINES_CHANGED_EVENT, { routine_key: routineKey, run_id: runId });
  const findRun = (runId: unknown): [MockRoutine, RoutineRun] => {
    for (const r of routines) {
      const run = r.runs.find((x) => x.run_id === runId);
      if (run) return [r, run];
    }
    throw new Error(`routine run ${String(runId)} not found`);
  };
  const toRow = (r: MockRoutine): RoutineRow => {
    const lastRun = r.runs.find((x) => STARTED.has(x.phase));
    // A run that simply ended reported nothing: it neither raises nor answers a needs-you.
    const lastEnded = r.runs.find((x) => TERMINAL.has(x.phase) && x.phase !== "ended");
    // A run's question is for the Mac that ran it, whoever created the routine.
    const needs = lastEnded?.phase === "needs_you" ? lastEnded : null;
    const asLast = (x: RoutineRun) => ({
      run_id: x.run_id,
      phase: x.phase,
      reason: x.reason,
      at: x.ended_at ?? x.started_at ?? x.created_at,
      result_text: x.result_text,
      detail: x.detail,
      ...(x.delivered_at !== undefined ? { delivered_at: x.delivered_at } : {}),
      ...(x.wake_machine !== undefined ? { wake_machine: x.wake_machine } : {}),
      ...(x.launch_machine !== undefined ? { launch_machine: x.launch_machine } : {}),
      session_id: x.session_id,
    });
    return {
      ...r.row,
      agent_display: agentDisplay(r.row.def.agent),
      last_run: lastRun ? asLast(lastRun) : null,
      needs_you: needs !== null,
      needs_you_run: needs ? asLast(needs) : null,
    };
  };
  const saved = (r: MockRoutine): SavedRoutine => ({
    key: r.row.key,
    def: r.row.def,
    warnings: [],
  });
  const revisionsOf = (r: MockRoutine): RoutineRevision[] => {
    r.revisions ??= [
      {
        revision: r.row.def.revision,
        author: r.row.created_by ?? { kind: "user", name: "octocat" },
        at: new Date(now() - 10 * 24 * HOUR).toISOString(),
        routine: r.row.def,
      },
    ];
    return r.revisions;
  };
  /** Write `next` as a new revision by `author` and refresh what the row shows. */
  const commit = (r: MockRoutine, next: RoutineDef, author: RoutineRevision["author"]) => {
    const revisions = revisionsOf(r);
    // As the backend places it: a recipe each Mac runs keeps the Mac it names;
    // one becoming a shared job runs on this Mac, the one that changed it; a
    // shared job naming no Mac is placed on this one.
    const wasRecipe = r.row.def.runs_on === "each_mac";
    const recipe = next.runs_on === "each_mac";
    const armedHost = recipe
      ? next.armed_host
      : wasRecipe || next.armed_host.trim() === ""
        ? "this-mac"
        : next.armed_host;
    const def = { ...next, armed_host: armedHost, revision: r.row.def.revision + 1 };
    revisions.push({
      revision: def.revision,
      author,
      at: new Date(now()).toISOString(),
      routine: def,
    });
    const cron = def.triggers?.find((t) => "schedule" in t);
    const schedule = cron && "schedule" in cron ? cron.schedule.cron : null;
    // A change of mode starts this Mac's switch from the new mode's default,
    // except that a shared job paused here stays off when it becomes a recipe.
    const enabled =
      wasRecipe === recipe ? r.row.enabled : !recipe || (armedHost === "this-mac" && r.row.enabled);
    r.row = {
      ...r.row,
      def,
      effective_mode: def.mode ?? r.row.effective_mode,
      // As the backend files a routine under its agent: a bare id is in its own space.
      ...(next.agent !== r.row.def.agent
        ? {
            agent_key: (next.agent ?? "").includes("/")
              ? next.agent
              : `${r.row.key.slice(0, r.row.key.lastIndexOf("/"))}/${next.agent}`,
            unassigned: false,
          }
        : {}),
      enabled,
      armed_here: recipe || armedHost === "this-mac",
      schedule_in_words:
        (def.triggers ?? [])
          .map((t) => ("schedule" in t ? mockInWords(t.schedule.cron) : eventTriggerWords(t.event)))
          .join("; ") || "manual only",
      next_run_at: schedule && enabled ? (mockNextFires(schedule, now(), 1)[0] ?? null) : null,
    };
    changed(r.row.key);
  };
  const applyForm = (def: RoutineDef, form: RoutineForm): RoutineDef => {
    const policy = { ...def.policy };
    if (form.overlap) policy.overlap = form.overlap;
    if (form.missed) policy.missed = form.missed;
    if (form.maxRuntime !== undefined) policy.max_runtime = form.maxRuntime;
    if (form.visible !== undefined) policy.visible = form.visible;
    // SW-AGR-13: a value sets a limit, null clears it, absent keeps it.
    const limits = [
      ["maxWakes", "max_wakes"],
      ["maxWakesPerDay", "max_wakes_per_day"],
      ["minGap", "min_gap"],
    ] as const;
    for (const [field, key] of limits) {
      const value = form[field];
      if (value === null) delete policy[key];
      else if (value !== undefined) (policy as Record<string, unknown>)[key] = value;
    }
    // As the backend: a schedule without a zone is read in the routine's zone.
    const current = def.triggers?.find((t) => "schedule" in t);
    const currentTz = current && "schedule" in current ? current.schedule.tz : "America/Denver";
    const tz = form.tz?.trim() || currentTz;
    let triggers = [...(def.triggers ?? [])];
    if (form.when !== undefined) {
      triggers = triggers.filter((t) => !("schedule" in t));
      if (form.when.trim()) triggers.unshift({ schedule: { cron: mockParseWhen(form.when), tz } });
    } else if (form.tz?.trim()) {
      triggers = triggers.map((t) => ("schedule" in t ? { schedule: { ...t.schedule, tz } } : t));
    }
    if (form.metadataTriggers) {
      const replacements = form.metadataTriggers.map((metadata) => ({
        event: { kind: "metadata_change", metadata },
      }));
      triggers = triggers.flatMap((t) =>
        "event" in t && t.event.kind === "metadata_change"
          ? replacements.length
            ? [replacements.shift()!]
            : []
          : [t],
      );
      triggers.push(...replacements);
    }
    const command = (form.action ?? def.action) === "command";
    return {
      ...def,
      ...(form.action ? { action: form.action } : {}),
      ...(form.command ? { command: form.command } : {}),
      ...(form.after !== undefined ? { after: form.after ?? undefined } : {}),
      ...(command
        ? { agent: "", task: "", mode: undefined }
        : form.action === "agent"
          ? { command: undefined }
          : {}),
      ...(form.name !== undefined ? { name: form.name.trim() } : {}),
      ...(form.task !== undefined ? { task: form.task.trim() } : {}),
      ...(form.agent?.trim() ? { agent: form.agent.trim() } : {}),
      ...(form.where?.trim() || form.machine?.trim()
        ? {
            where: {
              kind: form.where?.trim() || def.where.kind,
              ...(form.machine?.trim() || def.where.machine
                ? { machine: form.machine?.trim() || def.where.machine }
                : {}),
            },
          }
        : {}),
      ...(form.runsOn ? { runs_on: form.runsOn } : {}),
      ...(form.mode ? { mode: form.mode } : {}),
      ...(form.resultSchema !== undefined ? { result_schema: form.resultSchema } : {}),
      triggers,
      policy,
    };
  };
  /** An agent editing a routine through its MCP tool, for demos and tests. */
  const agentEdit: MockRoutines["agentEdit"] = (key, agent, form) => {
    const r = find(key);
    commit(r, applyForm(r.row.def, form), { kind: "agent", name: agent });
  };
  const finishRun: MockRoutines["finishRun"] = (runId, phase, resultText) => {
    const [r, run] = findRun(runId);
    if (run.phase !== "running" && run.phase !== "queued") return;
    run.phase = phase;
    run.ended_at = now();
    run.result_text = resultText;
    changed(r.row.key, run.run_id);
  };

  /** As the run store records a routine run that launched and ended: under the routine's
   *  handle, with its transcript persisted beside it. */
  const outcomeOf = (run: RoutineRun): RunOutcomeRead | null => {
    if (!TERMINAL.has(run.phase) || run.started_at === null || run.ended_at === null) return null;
    const failed = run.phase === "failed" || run.phase === "cancelled";
    const attention = run.phase === "needs_you" || run.phase === "timed_out";
    return {
      outcome: {
        story_id: `routine:${run.routine_key}@${run.run_id}`,
        run_id: run.run_id,
        trigger_source: "routine",
        status: failed ? "failed" : attention ? "needs-attention" : "success",
        summary: run.result_text,
        started_at: new Date(run.started_at).toISOString(),
        ended_at: new Date(run.ended_at).toISOString(),
        artifact_ref: `${run.run_id}.log`,
      },
      validated: failed ? "failed" : attention ? "needs-attention" : "completed",
    };
  };
  const outcomes = (): RunOutcomeRead[] =>
    routines.flatMap((r) => r.runs.map(outcomeOf).filter((o) => o !== null));
  const transcriptOf = (run: RoutineRun, name: string): string =>
    [
      `$ ${name}`,
      "Reading the team's recent activity…",
      run.result_text ?? "Nothing to report.",
      `[run ${run.phase}]`,
    ].join("\n");

  const handlers: HandlerRegistry = {
    routine_list: () => routines.map(toRow),

    routine_get: (args): RoutineDetail => {
      const r = find(args.key);
      return {
        agent_key: r.row.agent_key,
        unassigned: r.row.unassigned,
        effective_mode: r.row.effective_mode,
        key: r.row.key,
        scope: r.row.scope,
        armed_here: r.row.armed_here,
        owner_display: r.row.owner_display,
        enabled: r.row.enabled,
        agent_display: agentDisplay(r.row.def.agent),
        def: r.row.def,
        revisions: [...revisionsOf(r)],
      };
    },

    routine_save: (args): RoutineSaveReply => {
      const form = (args.form ?? {}) as RoutineForm;
      if (typeof args.key === "string") {
        const r = find(args.key);
        if (args.baseRevision !== r.row.def.revision) {
          return { outcome: "conflict", current_revision: r.row.def.revision };
        }
        if (form.agent?.trim()) checkAgent(form.agent);
        commit(r, applyForm(r.row.def, form), { kind: "user", name: "octocat" });
        return {
          outcome: "saved",
          key: r.row.key,
          revision: r.row.def.revision,
          warnings: warningsFor(r.row.def),
        };
      }
      if (
        !form.name?.trim() ||
        (form.action === "command" ? !form.command?.[0] : !form.task?.trim()) ||
        (!form.when?.trim() && !form.metadataTriggers?.length && !form.after)
      ) {
        throw new Error("a routine needs a name, a task and a schedule");
      }
      const base = form.name
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-|-$/g, "");
      let id = base;
      for (let n = 2; routines.some((x) => x.row.def.id === id); n += 1) id = `${base}-${n}`;
      const blank = def(id, "", "", ME, null, "");
      const created = applyForm(blank, {
        ...form,
        maxRuntime: form.maxRuntime || (form.action === "command" ? "5m" : "30m"),
      });
      if (form.action !== "command") checkAgent(form.agent);
      const enabled = args.enabled !== false;
      const cron = form.when?.trim() ? mockParseWhen(form.when) : null;
      // "Who can use it": a team when given, else your scratch space.
      const team = args.team as { org: string; team: string } | null | undefined;
      const fresh: MockRoutine = {
        row: {
          key: team ? `${team.org}/${team.team}/${id}` : `_personal/${ME}/personal/${id}`,
          scope: team ? "team" : "personal",
          armed_here: true,
          owner_display: "you",
          enabled,
          created_by: { kind: "user", name: "octocat" },
          schedule_in_words: (created.triggers ?? [])
            .map((t) =>
              "schedule" in t ? mockInWords(t.schedule.cron) : eventTriggerWords(t.event),
            )
            .join("; "),
          next_run_at: enabled && cron ? (mockNextFires(cron, now(), 1)[0] ?? null) : null,
          def: created,
          // As the backend reads it from the dir the routine lands in: the agent's key.
          agent_key: form.agent?.includes("/") ? form.agent.trim() : null,
          unassigned: false,
          duplicate: false,
          effective_mode: created.mode ?? "fresh",
          mine: true,
        },
        runs: [],
        revisions: [
          {
            revision: 1,
            author: { kind: "user", name: "octocat" },
            at: new Date(now()).toISOString(),
            routine: created,
          },
        ],
      };
      routines.push(fresh);
      changed(fresh.row.key);
      return { outcome: "saved", key: fresh.row.key, revision: 1, warnings: warningsFor(created) };
    },

    routine_preview_command: (args) => ({
      argv: (args.command as string[]).map((arg) =>
        arg.split("{{date}}").join(new Date(now()).toISOString().slice(0, 10)),
      ),
      error: (args.command as string[])[0]
        ? null
        : "Name a program, like python3, or a script in the team's files.",
      warnings: [],
    }),
    routine_preview_schedule: (args): RoutineSchedulePreview => {
      const cron = mockParseWhen(String(args.when ?? ""));
      return {
        cron,
        tz: typeof args.tz === "string" && args.tz ? args.tz : "America/Denver",
        in_words: mockInWords(cron),
        next_fires: mockNextFires(cron, now(), 5),
      };
    },

    routine_restore_revision: (args): RoutineSaveReply => {
      const r = find(args.key);
      if (args.baseRevision !== r.row.def.revision) {
        return { outcome: "conflict", current_revision: r.row.def.revision };
      }
      const old = revisionsOf(r).find((x) => x.revision === args.revision);
      if (!old) throw new Error(`routine ${r.row.key} has no revision ${String(args.revision)}`);
      commit(
        r,
        {
          ...old.routine,
          id: r.row.def.id,
          owner: r.row.def.owner,
          armed_host: r.row.def.armed_host,
          archived_at: r.row.def.archived_at,
        },
        {
          kind: "user",
          name: "octocat",
        },
      );
      return {
        outcome: "saved",
        key: r.row.key,
        revision: r.row.def.revision,
        warnings: warningsFor(r.row.def),
      };
    },

    routine_open_authoring_agent: () => ({ opened: true, message: "Opened Routine author." }),

    /** SW-XS-25: prepared on its machine from now: no longer stale. */
    routine_refresh_on_machine: (args) => {
      const r = find(args.key);
      const machine = r.row.content_stale?.machine ?? r.row.def.armed_machine ?? "";
      r.row = { ...r.row, content_stale: null };
      changed(r.row.key);
      return machine;
    },

    routine_set_enabled: (args) => {
      const r = find(args.key);
      const enabled = Boolean(args.enabled);
      const cron = r.row.def.triggers?.find((t) => "schedule" in t);
      const schedule = cron && "schedule" in cron ? cron.schedule.cron : null;
      r.row = {
        ...r.row,
        enabled,
        next_run_at: schedule && enabled ? (mockNextFires(schedule, now(), 1)[0] ?? null) : null,
      };
      changed(r.row.key);
      return undefined;
    },

    routine_run_now: (args): RoutineAdmission => {
      const r = find(args.key);
      const at = now();
      const id = `manual-${nextRun++}`;
      const active = r.runs.find((x) => x.phase === "running" || x.phase === "queued");
      const base: RoutineRun = {
        run_id: id,
        routine_key: r.row.key,
        revision: r.row.def.revision,
        source: "manual",
        occurrence_key: null,
        phase: "running",
        reason: null,
        linked_run_id: null,
        session_id: null,
        created_at: at,
        started_at: null,
        ended_at: null,
        result_text: null,
        detail: null,
        // As the backend records the agent revision a run launched.
        agent: `${r.row.def.agent}@1`,
      };
      if (!r.row.enabled) {
        const reason = r.row.def.runs_on === "each_mac" ? "off_here" : "paused";
        r.runs.unshift({ ...base, phase: "skipped", reason, ended_at: at });
        changed(r.row.key, id);
        return { run_id: id, outcome: "skipped", reason };
      }
      if (active) {
        r.runs.unshift({ ...base, phase: "coalesced", linked_run_id: active.run_id, ended_at: at });
        changed(r.row.key, id);
        return { run_id: id, outcome: "coalesced", active_run_id: active.run_id };
      }
      const running: RoutineRun = { ...base, session_id: `sess-${id}`, started_at: at };
      r.runs.unshift(running);
      changed(r.row.key, id);
      if (runMs !== null) {
        setTimeout(
          () => finishRun(id, "succeeded", `${r.row.def.name}: done, nothing new.`),
          runMs,
        );
      }
      return { run_id: id, outcome: "queued" };
    },

    routine_runs_since: (args) => {
      // As the backend: every routine, at most `perRoutine` of each one's runs and as many
      // of its merged and skipped fires, counted apart.
      const since = typeof args.sinceMs === "number" ? args.sinceMs : 0;
      const cap = typeof args.perRoutine === "number" ? args.perRoutine : 50;
      const folds = (run: RoutineRun) => run.phase === "coalesced" || run.phase === "skipped";
      return routines
        .flatMap((r) => {
          const recent = r.runs.filter(
            (run) => (run.ended_at ?? run.started_at ?? run.created_at) >= since,
          );
          return [
            ...recent.filter((x) => !folds(x)).slice(0, cap),
            ...recent.filter(folds).slice(0, cap),
          ];
        })
        .sort((a, b) => b.created_at - a.created_at);
    },

    routine_runs: (args) => {
      const r = find(args.key);
      const limit = typeof args.limit === "number" ? args.limit : 50;
      const start = args.before ? r.runs.findIndex((x) => x.run_id === args.before) + 1 : 0;
      const page = r.runs.slice(start, start + limit);
      const more = start + limit < r.runs.length;
      return { runs: page, next_before: more ? (page[page.length - 1]?.run_id ?? null) : null };
    },

    routine_stop_run: (args) => {
      const [r, run] = findRun(args.runId);
      if (run.phase !== "running" && run.phase !== "queued") return false;
      run.phase = "cancelled";
      run.ended_at = now();
      // A queued wake is withdrawn from its host, as the backend does (Cancel wake).
      if (r.row.queued?.runId === run.run_id) r.row = { ...r.row, queued: null };
      changed(r.row.key, run.run_id);
      return true;
    },

    routine_archive: (args) => {
      const r = find(args.key);
      routines.splice(routines.indexOf(r), 1);
      changed(r.row.key);
      return saved(r);
    },

    routine_copy_to_mine: (args) => {
      const source = find(args.key);
      const id = `${source.row.def.id}-copy`;
      const copy: MockRoutine = {
        row: {
          ...source.row,
          key: `_personal/${ME}/personal/${id}`,
          scope: "personal",
          armed_here: true,
          owner_display: "you",
          created_by: { kind: "user", name: "octocat" },
          enabled: true,
          // A personal copy runs on its own Mac, whichever way the original runs.
          def: { ...source.row.def, id, owner: ME, armed_host: "this-mac", runs_on: undefined },
          agent_key: null,
          unassigned: false,
          duplicate: false,
          effective_mode: "fresh",
          mine: true,
        },
        runs: [],
      };
      routines.push(copy);
      changed(copy.row.key);
      return saved(copy);
    },

    routine_arm_here: (args) => {
      const r = find(args.key);
      // A recipe each Mac runs is turned on here instead of moved.
      if (r.row.def.runs_on === "each_mac") {
        r.row = { ...r.row, enabled: true };
        changed(r.row.key);
        return saved(r);
      }
      if (r.row.def.armed_host === "this-mac") return saved(r);
      commit(r, { ...r.row.def, armed_host: "this-mac" }, { kind: "user", name: "octocat" });
      return saved(r);
    },

    routine_run_detail: (args): RoutineRunDetail => {
      const [r, run] = findRun(args.runId);
      return { ...run, routine_name: r.row.def.name };
    },

    list_run_outcomes: () => outcomes(),
    read_run_outcome: (args) => outcomes().find((o) => o.outcome.run_id === args.runId) ?? null,
    read_run_artifact: (args) => {
      for (const r of routines) {
        const run = r.runs.find((x) => x.run_id === args.runId);
        if (run) return outcomeOf(run) ? transcriptOf(run, r.row.def.name) : null;
      }
      return null;
    },
    read_run_transcript: () => null,
    read_run_work_product: () => null,
    has_raw_terminal_snapshot: () => false,

    routine_needs_you_count: () => routines.map(toRow).filter((row) => row.needs_you).length,

    routine_scheduler_status: () => ({ missed_count: missed }),

    routine_acknowledge_missed: () => {
      missed = 0;
      for (const r of routines) changed(r.row.key);
      return undefined;
    },
  };
  const defOf = (name: string) => routines.find((r) => r.row.def.name === name)?.row.def;
  return { handlers, finishRun, agentEdit, defOf };
}

/** The demo routines world `pnpm dev:mock` boots with. */
export function routineHandlers(events: MockEventBus): HandlerRegistry {
  return createMockRoutines(events, { seed: agentRoutinesSeed, agents: DEMO_AGENTS }).handlers;
}

/** The agents the dev:mock routines name, by id (as the agents mock lists them). */
const DEMO_AGENTS: Record<string, MockAgent> = {
  triage: { name: "Triage", commands: ["triage-slack"] },
  scribe: { name: "Scribe" },
  reviewer: { name: "Reviewer" },
  dev: { name: "Dev" },
  writer: { name: "Writer", archived: true },
};
