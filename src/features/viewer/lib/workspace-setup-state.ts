// SPDX-License-Identifier: MIT
// SW-XS-10 (SPEC §8.1, §8.6, X5): a workspace's setup, as typed state.
//
// Replaces the free-text `setupLabel`. Two slots per workspace:
// - `base`: Step 1 (the worktree: on This Mac, or the incremental bundle to a
//   machine), the wait for the first agent, Ready, or a Step 1 failure;
// - `runs`: each agent's Step 2 (the `agent-prepare-progress` stream without an
//   operation id), keyed by agent, so two agents preparing in one workspace
//   never overwrite each other.
// The caption, the tray pill and the screen-reader announcement all render the
// one line `displayedLine` derives from them, so they can never disagree.
//
// Rules this module owns (each is unit-tested):
// - "Ready" is painted only after Step 2 finished (X5), and only once no agent
//   is still preparing or failed — Step 1 alone never claims it.
// - A Step 2 that finishes within {@link NO_FLASH_MS} never paints "Preparing".
// - While several agents prepare, the line shows the busiest; a failure wins.
// - Counts are determinate where the backend has them (objects, bytes, files)
//   and the phase is indeterminate otherwise (diff, verify) — no spinner swarm.

import type { AgentPrepareState } from "@/features/agents/lib/agent-prepare-progress";
import type { Step1Phase, Step1Progress } from "@/tauri/events";

/** A Step 2 that settles within this window never shows "Preparing" (§8.1). */
export const NO_FLASH_MS = 400;

/** How long a settled "Ready" stays on the line before it goes quiet. */
export const READY_LINGER_MS = 4000;

/** How long the line waits for an expected agent's Step 2 to report before it
 *  goes quiet (the launch failed before preparing, and says so itself). It
 *  never turns into "Ready". */
export const AWAIT_AGENT_MS = 60_000;

/** The backend's typed code for a machine whose terminal server is too old
 *  (`TERMINAL_SERVER_OUTDATED`, `content_engine/destination.rs`). */
export const TERMINAL_SERVER_OUTDATED = "terminalServerOutdated";

/** Step 1's phases, as the setup line names them. `connecting` and `starting`
 *  are the desktop's own (before the machine reports); the rest mirror the
 *  Rust `Step1Phase`. */
export type SetupStep1Phase =
  | "connecting"
  | "starting"
  | "fetching"
  | "bundling"
  | "copying"
  | "verifying"
  | "creatingWorktree"
  | "updatingWorktree"
  | "runningSetup"
  | "cleaningUp";

/** The repo being copied and the machine it goes to, for the line's words. */
export interface Step1Names {
  repo?: string;
  machine?: string;
}

export type WorkspaceSetupState =
  /** Step 1 is running. `done`/`total` are objects or bytes (`unit`). */
  | ({
      kind: "step1";
      phase: SetupStep1Phase;
      done?: number;
      total?: number;
      unit?: "objects" | "bytes";
      /** The setup command running. */
      detail?: string;
    } & Step1Names)
  /** Step 1 finished and an agent launch is on its way: its Step 2 has not
   *  reported yet. Never "Ready" — the agent has no context yet. */
  | { kind: "awaitingAgent"; since: number }
  /** (Display only) the busiest agent's Step 2. */
  | {
      kind: "step2";
      agent: string;
      phase: "diff" | "transfer" | "apply";
      files: number;
      bytes: number;
      total: number;
      startedAt: number;
      /** Other agents preparing here at the same time. */
      alsoPreparing?: number;
    }
  /** Steps 1 and 2 finished. */
  | { kind: "ready"; at: number }
  /** A step failed; the line sticks until Retry (or dismiss). */
  | {
      kind: "failed";
      step: 1 | 2;
      /** The phase that stopped (Step 1), or `prepare` (Step 2). */
      phase: string;
      agent?: string;
      reason: string;
      /** The backend's typed reason, e.g. {@link TERMINAL_SERVER_OUTDATED}. */
      code?: string;
      /** Step 2: the `[path, reason]` entries that failed. */
      failed?: [string, string][];
      /** Step 2: the startup note the engine wrote. */
      note?: string;
    };

/** One agent's Step 2 in a workspace. */
export type AgentRun =
  | {
      kind: "running";
      agent: string;
      phase: "diff" | "transfer" | "apply";
      files: number;
      bytes: number;
      total: number;
      startedAt: number;
    }
  | {
      kind: "failed";
      agent: string;
      reason: string;
      code?: string;
      failed?: [string, string][];
      note?: string;
    }
  /** Finished; waits for the other agents before the line may say Ready.
   *  `painted`: its "Preparing" was on screen (so Ready follows it). */
  | { kind: "done"; agent: string; painted: boolean };

/** A workspace's setup: the base line and each agent's Step 2. */
export interface SetupSlots {
  base?: WorkspaceSetupState;
  runs?: Record<string, AgentRun>;
}

/** One Step 1 reading from a machine (`remote-workspace-progress`). */
export function step1StateFromProgress(
  progress: Step1Progress,
  names: Step1Names = {},
): WorkspaceSetupState {
  switch (progress.phase) {
    case "fetching":
      return {
        kind: "step1",
        phase: "fetching",
        ...names,
        ...(progress.objects !== null ? { done: progress.objects } : {}),
        ...(progress.total ? { total: progress.total } : {}),
        unit: "objects",
      };
    case "bundling":
      return {
        kind: "step1",
        phase: "bundling",
        ...names,
        ...(progress.objects ? { done: progress.objects } : {}),
        unit: "objects",
      };
    case "copying":
      return {
        kind: "step1",
        phase: "copying",
        ...names,
        done: progress.sent,
        total: progress.total,
        unit: "bytes",
      };
    case "verifying":
    case "creatingWorktree":
    case "updatingWorktree":
      return { kind: "step1", phase: progress.phase, ...names };
    case "failed":
      return { kind: "failed", step: 1, phase: progress.failedPhase, reason: progress.message };
  }
}

/** One local `setupWorktree` phase (`copy` / `setup` / `validate` / `cleanup`). */
export function step1StateFromLocalPhase(phase: string, command?: string): WorkspaceSetupState {
  switch (phase) {
    case "setup":
      return { kind: "step1", phase: "runningSetup", ...(command ? { detail: command } : {}) };
    case "validate":
      return { kind: "step1", phase: "verifying" };
    case "cleanup":
      return { kind: "step1", phase: "cleaningUp" };
    default:
      return { kind: "step1", phase: "creatingWorktree" };
  }
}

/** The plain-language reason a failed Step 2 gives. */
export function step2FailureReason(state: AgentPrepareState): string {
  const failed = state.summary?.failed ?? [];
  const count = state.summary?.failedCount ?? failed.length;
  if (failed.length > 0) {
    const [path, why] = failed[0];
    const lead =
      count === 1 ? `${path} couldn't be written` : `${count} files couldn't be written (${path})`;
    return `${lead}: ${why}`;
  }
  return state.note?.trim() || "the preparation stopped before it finished";
}

/** Whether the base line was in the middle of setting a workspace up. */
function wasSettingUp(state: WorkspaceSetupState | undefined): boolean {
  return state?.kind === "step1" || state?.kind === "awaitingAgent" || state?.kind === "failed";
}

/** Once no agent is running or failed, the finished runs settle the line:
 *  Ready when the workspace was being set up or a "Preparing" was on screen,
 *  otherwise the base line as it was (a quick re-launch stays quiet). */
function settle(slots: SetupSlots, now: number): SetupSlots {
  const runs = Object.values(slots.runs ?? {});
  if (runs.some((r) => r.kind !== "done")) return slots;
  const finished = runs.length > 0;
  const warranted = runs.some((r) => r.kind === "done" && r.painted) || wasSettingUp(slots.base);
  const base = finished && warranted ? ({ kind: "ready", at: now } as const) : slots.base;
  return base ? { base } : {};
}

/**
 * Fold one Step 2 state (the reducer's output for a launch: no operation id)
 * into a workspace's setup. Pure; `now` is injected. Each agent's run is its
 * own: one agent finishing, failing or being retried never touches another's.
 */
export function applyPrepare(slots: SetupSlots, state: AgentPrepareState, now: number): SetupSlots {
  const runs = { ...(slots.runs ?? {}) };
  const prev = runs[state.agent];
  if (!state.settled) {
    const phase = state.phase === "transfer" || state.phase === "apply" ? state.phase : "diff";
    const same = prev?.kind === "running";
    // Apply restarts its own counters at zero; the line keeps what was copied
    // so its count never runs backwards.
    const carry = same && phase === "apply";
    runs[state.agent] = {
      kind: "running",
      agent: state.agent,
      phase,
      files: carry ? Math.max(prev.files, state.files) : state.files,
      bytes: carry ? Math.max(prev.bytes, state.bytes) : state.bytes,
      total: state.total,
      startedAt: same ? prev.startedAt : now,
    };
    return { ...slots, runs };
  }
  switch (state.phase) {
    case "ready":
    case "skipped":
      runs[state.agent] = {
        kind: "done",
        agent: state.agent,
        painted: prev?.kind === "running" && now - prev.startedAt >= NO_FLASH_MS,
      };
      return settle({ ...slots, runs }, now);
    case "failed": {
      const { reason, code } = typedReason(step2FailureReason(state));
      runs[state.agent] = {
        kind: "failed",
        agent: state.agent,
        reason,
        ...(code ? { code } : {}),
        ...(state.summary?.failed.length ? { failed: state.summary.failed } : {}),
        ...(state.note ? { note: typedReason(state.note).reason } : {}),
      };
      return { ...slots, runs };
    }
    default:
      // Cancelled: as if it never ran.
      delete runs[state.agent];
      return settle({ ...slots, runs }, now);
  }
}

/** Mark `agent`'s Step 2 failed without an event (its launch refused before
 *  preparing, e.g. an outdated terminal server). */
export function failRun(
  slots: SetupSlots,
  names: string | readonly string[],
  reason: string,
  code?: string,
): SetupSlots {
  const all = (typeof names === "string" ? [names] : [...names]).filter(Boolean);
  // The launch knows its agent by key and display name; its Step 2 events by
  // the display name. Update the run the events already started, whichever
  // spelling it carries (`claude`, `Claude`, `builtin/claude`).
  const wanted = new Set(all.map((n) => n.toLowerCase()));
  const existing = Object.keys(slots.runs ?? {}).find((k) => wanted.has(k.toLowerCase()));
  const agent = existing ?? all[0] ?? "the agent";
  const parsed = typedReason(reason);
  const typed = code ?? parsed.code;
  return {
    ...slots,
    runs: {
      ...(slots.runs ?? {}),
      [agent]: { kind: "failed", agent, reason: parsed.reason, ...(typed ? { code: typed } : {}) },
    },
  };
}

/** The backend marks a typed failure in its text (`[terminalServerOutdated]
 *  Update the terminal server on …`): take the code out, keep the words. */
export function typedReason(text: string): { reason: string; code?: string } {
  const match = /\[([A-Za-z]+)\]\s*/.exec(text);
  if (match?.[1] !== TERMINAL_SERVER_OUTDATED) return { reason: text };
  return { reason: text.replace(match[0], "").trim(), code: match[1] };
}

/** Drop `agent`'s run (its failure dismissed, or a Retry about to run it
 *  again). Every other agent's run is kept. */
export function clearRun(slots: SetupSlots, agent: string, now: number): SetupSlots {
  if (!slots.runs?.[agent]) return slots;
  const runs = { ...slots.runs };
  delete runs[agent];
  const next = { ...slots, runs };
  return Object.keys(runs).length === 0
    ? next.base
      ? { base: next.base }
      : {}
    : settle(next, now);
}

/** How much a running Step 2 has left to move (the busiest has the most). */
function workLeft(run: Extract<AgentRun, { kind: "running" }>): number {
  if (run.phase === "diff") return 0;
  return Math.max(0, run.total - (run.phase === "transfer" ? run.bytes : run.files));
}

/** The base line as shown at `now`: a Ready older than {@link READY_LINGER_MS}
 *  and a wait older than {@link AWAIT_AGENT_MS} show nothing. */
function shownBase(base: WorkspaceSetupState | undefined, now: number) {
  if (base?.kind === "ready" && now - base.at >= READY_LINGER_MS) return undefined;
  if (base?.kind === "awaitingAgent" && now - base.since >= AWAIT_AGENT_MS) return undefined;
  return base;
}

/** The one line a workspace shows at `now`: a failed agent first; else the
 *  busiest agent whose Step 2 is older than {@link NO_FLASH_MS}; else the base
 *  line. */
export function displayedLine(slots: SetupSlots, now: number): WorkspaceSetupState | undefined {
  const runs = Object.values(slots.runs ?? {});
  const failed = runs.find((r) => r.kind === "failed");
  if (failed?.kind === "failed") {
    return {
      kind: "failed",
      step: 2,
      phase: "prepare",
      agent: failed.agent,
      reason: failed.reason,
      ...(failed.code ? { code: failed.code } : {}),
      ...(failed.failed ? { failed: failed.failed } : {}),
      ...(failed.note ? { note: failed.note } : {}),
    };
  }
  const running = runs.filter(
    (r): r is Extract<AgentRun, { kind: "running" }> => r.kind === "running",
  );
  const visible = running.filter((r) => now - r.startedAt >= NO_FLASH_MS);
  if (visible.length > 0) {
    const busiest = visible.reduce((a, b) => (workLeft(b) > workLeft(a) ? b : a));
    return {
      kind: "step2",
      agent: busiest.agent,
      phase: busiest.phase,
      files: busiest.files,
      bytes: busiest.bytes,
      total: busiest.total,
      startedAt: busiest.startedAt,
      ...(running.length > 1 ? { alsoPreparing: running.length - 1 } : {}),
    };
  }
  return shownBase(slots.base, now);
}

/** When the displayed line next changes on its own (a hold ending, a Ready
 *  going quiet, a wait expiring), or null. Drives one timer, never a clock. */
export function nextLineChangeAt(slots: SetupSlots, now: number): number | null {
  const at: number[] = [];
  for (const run of Object.values(slots.runs ?? {})) {
    if (run.kind === "running" && now - run.startedAt < NO_FLASH_MS)
      at.push(run.startedAt + NO_FLASH_MS);
  }
  const base = slots.base;
  if (base?.kind === "ready" && now - base.at < READY_LINGER_MS) at.push(base.at + READY_LINGER_MS);
  if (base?.kind === "awaitingAgent" && now - base.since < AWAIT_AGENT_MS)
    at.push(base.since + AWAIT_AGENT_MS);
  return at.length ? Math.min(...at) : null;
}

/** How the line reads. */
export interface SetupView {
  tone: "busy" | "ready" | "failed";
  /** The phase, on the left (no trailing ellipsis: the line animates one). */
  text: string;
  /** The determinate count, on the right, in tabular numerals. */
  count?: string;
  /** 0..1 when the phase is determinate (the ring's fill). */
  fraction?: number;
  /** The phase has no count: the line shows a moving ellipsis. */
  indeterminate: boolean;
  /** What a screen reader hears, once per phase change (never per count). */
  announce: string;
}

/** The words a Step 1 failure names its phase with. */
export const STEP1_FAILED_WHILE: Record<Step1Phase | SetupStep1Phase, string> = {
  connecting: "connecting",
  starting: "starting",
  fetching: "fetching",
  bundling: "bundling",
  copying: "copying",
  verifying: "verifying the copy",
  creatingWorktree: "creating the worktree",
  updatingWorktree: "moving the worktree",
  runningSetup: "running setup",
  cleaningUp: "cleaning up",
};

/** A Step 1 phase in words, naming the repo and machine when known
 *  (`Copying codesense to studio`). */
function step1Words(state: Extract<WorkspaceSetupState, { kind: "step1" }>): string {
  const { repo, machine } = state;
  const on = machine ? ` on ${machine}` : "";
  switch (state.phase) {
    case "connecting":
      return machine ? `Connecting to ${machine}` : "Connecting";
    case "starting":
      return "Starting";
    case "fetching":
      return repo ? `Fetching ${repo}` : "Fetching";
    case "bundling":
      return repo ? `Bundling ${repo}${machine ? ` for ${machine}` : ""}` : "Bundling";
    case "copying":
      return repo ? `Copying ${repo}${machine ? ` to ${machine}` : ""}` : "Copying";
    case "verifying":
      return machine ? `Verifying the copy${on}` : "Verifying";
    case "creatingWorktree":
      return `Creating worktree${on}`;
    case "updatingWorktree":
      return `Moving to the new base${on}`;
    case "runningSetup":
      return state.detail ? `Running setup (${state.detail})` : "Running setup";
    case "cleaningUp":
      return "Cleaning up";
  }
}

/** `41 / 112 MB`: both sides in the total's unit. */
export function formatByteProgress(done: number, total: number): string {
  const [div, unit] =
    total >= 1024 * 1024 ? [1024 * 1024, "MB"] : total >= 1024 ? [1024, "KB"] : [1, "B"];
  const d = Math.min(done, total);
  return `${Math.round(d / div)} / ${Math.max(1, Math.round(total / div))} ${unit}`;
}

/** `4.1 MB`, `820 KB`, `12 B`. */
export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) {
    const mb = bytes / (1024 * 1024);
    return `${mb >= 10 ? Math.round(mb) : mb.toFixed(1)} MB`;
  }
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

const nf = new Intl.NumberFormat("en-US");
const plural = (n: number, one: string, many = `${one}s`) =>
  `${nf.format(n)} ${n === 1 ? one : many}`;

/** The line for a displayed state (see {@link displayedLine}); null when there
 *  is nothing to say. */
export function describeSetup(state: WorkspaceSetupState | undefined): SetupView | null {
  if (!state) return null;
  switch (state.kind) {
    case "step1": {
      const text = step1Words(state);
      const { done, total } = state;
      if (state.unit === "bytes" && done !== undefined && total) {
        return {
          tone: "busy",
          text,
          count: formatByteProgress(done, total),
          fraction: Math.min(1, done / total),
          indeterminate: false,
          announce: text,
        };
      }
      if (state.unit === "objects" && done !== undefined) {
        return {
          tone: "busy",
          text,
          count: total ? `${nf.format(done)} / ${plural(total, "object")}` : plural(done, "object"),
          ...(total ? { fraction: Math.min(1, done / total) } : {}),
          indeterminate: !total,
          announce: text,
        };
      }
      return { tone: "busy", text, indeterminate: true, announce: text };
    }
    case "awaitingAgent":
      return {
        tone: "busy",
        text: "Preparing agent context",
        indeterminate: true,
        announce: "Preparing agent context",
      };
    case "step2": {
      const more = state.alsoPreparing ? ` (+${state.alsoPreparing})` : "";
      const text = `Preparing ${state.agent}${more}`;
      const announce = `Preparing ${state.agent}`;
      if (state.phase === "diff") return { tone: "busy", text, indeterminate: true, announce };
      const fraction =
        state.total > 0
          ? Math.min(1, (state.phase === "transfer" ? state.bytes : state.files) / state.total)
          : undefined;
      return {
        tone: "busy",
        text,
        count: `${plural(state.files, "file")} · ${formatBytes(state.bytes)}`,
        ...(fraction !== undefined ? { fraction } : {}),
        indeterminate: fraction === undefined,
        announce,
      };
    }
    case "ready":
      return { tone: "ready", text: "Ready", indeterminate: false, announce: "Ready" };
    case "failed": {
      const text =
        state.step === 2
          ? `Couldn't prepare ${state.agent ?? "the agent"} — ${afterDash(state.reason)}`
          : `Couldn't create the workspace while ${
              STEP1_FAILED_WHILE[state.phase as SetupStep1Phase] ?? state.phase
            } — ${afterDash(state.reason)}`;
      return { tone: "failed", text, indeterminate: false, announce: text };
    }
  }
}

/** The failure is the backend's typed "terminal server too old" (§11.13): the
 *  line offers "Update terminal server" as its one click. */
export function needsServerUpdate(state: { code?: string }): boolean {
  return state.code === TERMINAL_SERVER_OUTDATED;
}

/** A reason continuing a sentence after the dash: a capitalized first word
 *  (`Update the terminal server…`) is lowercased; a path or name
 *  (`CLAUDE.md`, `.claude/…`) keeps its case. */
function afterDash(reason: string): string {
  return /^[A-Z][a-z]/.test(reason) ? reason[0].toLowerCase() + reason.slice(1) : reason;
}
