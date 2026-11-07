// SPDX-License-Identifier: MIT
/**
 * The canonical per-session agent state — ONE vocabulary every surface projects
 * from (the terminal dot, the Agents-panel phase, any future surface), plus the
 * pure three-tier precedence resolver that produces it.
 *
 * Why this exists: the dot (`TerminalAiStatus`), the Agents-panel `Phase`, and
 * the headless outcome classifier (`classify_headless_outcome`) are three
 * independent vocabularies that never reconcile. Every writer now maps into
 * `AgentState` and every surface is a projection of it, so the surfaces cannot
 * drift.
 *
 * Lives in its own framework-free module (mirrors `terminal-ai-status.ts`) so the
 * literal set, the mapping tables, and the resolver have one home and cannot be
 * mocked away by a partial store mock. Nothing here touches the store, the
 * liveness monitor, or any transport: the resolver consumes tier INPUTS
 * wherever the PTY child runs (local, session-server sidecar, remote), and the
 * wiring of each live source is left to the consumers.
 *
 * Design note: `_bmad-output/03-terminal/36-agent-state-detection/agent-state-model.md`.
 */

import type { Phase } from "./roles";
import type { TerminalAiStatus, TerminalWireStatus } from "./terminal-ai-status";

// ---------------------------------------------------------------------------
// The state set
// ---------------------------------------------------------------------------

/**
 * Every canonical agent state. `errored` (a DECLARED failure, process still up)
 * and `crashed` (the process DIED abnormally) are deliberately distinct: both
 * are non-benign but carry different next actions.
 */
export const AGENT_STATES = [
  "working",
  "thinking",
  "needs-permission",
  "blocked",
  "done",
  "errored",
  "crashed",
  "idle",
] as const;

export type AgentState = (typeof AGENT_STATES)[number];

/** Narrow an untrusted string to an `AgentState` (null when off-enum). */
export function asAgentState(s: string): AgentState | null {
  return (AGENT_STATES as readonly string[]).includes(s) ? (s as AgentState) : null;
}

// ---------------------------------------------------------------------------
// Tier inputs
// ---------------------------------------------------------------------------

/** The Claude hook event names the status receiver (`resolve_status`) maps today. */
export const CLAUDE_HOOK_EVENTS = [
  "Stop",
  "StopFailure",
  "PermissionRequest",
  "UserPromptSubmit",
  "PreToolUse",
  "PostToolUse",
  "PostToolUseFailure",
  "SessionEnd",
] as const;

export type ClaudeHookEvent = (typeof CLAUDE_HOOK_EVENTS)[number];

/** The MCP `signal_done` status values (validated server-side in `signal_done`). */
export const SIGNAL_DONE_OUTCOMES = ["success", "failed", "needs-attention"] as const;

export type SignalDoneOutcome = (typeof SIGNAL_DONE_OUTCOMES)[number];

/** Tier-1 — the agent DECLARED its state (authoritative). */
export type ExplicitSignal =
  | { kind: "claude-hook"; event: ClaudeHookEvent }
  | { kind: "signal-done"; outcome: SignalDoneOutcome }
  /** A blocking `ask_user` elicitation is open. */
  | { kind: "ask-user" }
  /** The last open `ask_user` resolved; the agent is back inside its turn. */
  | { kind: "ask-resolved" }
  /** `signal_progress`; the phase is the fixed lifecycle enum shared with the Agents panel. */
  | { kind: "signal-progress"; phase: Phase };

/** Tier-2 — the OS truth about the agent process, from whichever locus owns it. */
export type ProcessSignal =
  | { kind: "alive" }
  /**
   * `code` is the exit code when the owning locus knows it, else null — the local
   * process walk only sees alive/gone, so a vanished process arrives code-less.
   * A killed-by-signal exit sets `signal`. An unknown code with no signal is not
   * evidence of a crash on its own.
   *
   * `requested` is set when CodeSense itself ended the process (a tab close, a
   * stop): it resolves like a clean exit, never `crashed`.
   *
   * `unannounced` is never set by a producer: `advanceSessionAgent` stamps it on
   * a code-less exit that ends an open turn of an agent that announces its own
   * end (a hook-established Claude sends `SessionEnd` before a normal exit) and
   * that was seen running — the process vanished mid-turn without saying so,
   * which reads `crashed`.
   */
  | {
      kind: "exited";
      code: number | null;
      signal?: string | null;
      requested?: boolean;
      unannounced?: boolean;
    };

/** Tier-3 — scrape-fallback classification of the PTY for non-signaling agents. */
export const SCRAPE_SIGNALS = ["known-approval-prompt", "byte-quiet", "unknown"] as const;

export type ScrapeSignal = (typeof SCRAPE_SIGNALS)[number];

/**
 * One tier's latest input. `seq` is a monotonic sequence (or timestamp) shared
 * across the three tiers for one session, so the resolver can tell which input
 * is fresher. Larger = newer. `seq` must be finite — a NaN compares false against
 * everything and would silently drop that input.
 */
export interface TierInput<T> {
  signal: T;
  seq: number;
}

export type Tier1Input = TierInput<ExplicitSignal>;
export type Tier2Input = TierInput<ProcessSignal>;
export type Tier3Input = TierInput<ScrapeSignal>;

// ---------------------------------------------------------------------------
// The writer mapping table, as data
// ---------------------------------------------------------------------------

/** Tier-1 Claude hook event → state (mirror of the receiver's `resolve_status`). */
export const CLAUDE_HOOK_STATE: Readonly<Record<ClaudeHookEvent, AgentState>> = {
  Stop: "done",
  StopFailure: "errored",
  PermissionRequest: "needs-permission",
  // A turn is open from UserPromptSubmit to Stop. Inside it the model is
  // reasoning, except while a tool runs (PreToolUse → PostToolUse*).
  UserPromptSubmit: "thinking",
  PreToolUse: "working",
  PostToolUse: "thinking",
  PostToolUseFailure: "thinking",
  // The Claude session ended; no live agent for this slot.
  SessionEnd: "idle",
};

/** Tier-1 MCP `signal_done` outcome → state. */
export const SIGNAL_DONE_STATE: Readonly<Record<SignalDoneOutcome, AgentState>> = {
  success: "done",
  failed: "errored",
  // A completed outcome does not imply a permission prompt.
  "needs-attention": "done",
};

/** Tier-1 MCP `ask_user` (blocking elicitation open) → state. */
export const ASK_USER_STATE: AgentState = "needs-permission";

/** Tier-1 MCP `ask_user` resolved: it is a tool call inside a live turn. */
export const ASK_RESOLVED_STATE: AgentState = "working";

/**
 * Tier-1 MCP `signal_progress` phase → state. Only `blocked` means "needs a
 * human"; every other phase is the agent making progress (including `done`,
 * which is a progress milestone, not the completion contract — that is
 * `signal_done`). `blocked` is its own state, distinct from a Claude
 * `PermissionRequest`'s `needs-permission`: the agent's own self-report that it
 * is held, carrying a reason that surfaces as `Blocked: <note>` instead of
 * `Waiting for your approval`.
 */
export const SIGNAL_PROGRESS_STATE: Readonly<Record<Phase, AgentState>> = {
  planning: "working",
  dev: "working",
  review: "working",
  pr: "working",
  blocked: "blocked",
  done: "working",
};

/** Tier-2 floor for a live process when no higher truth is known. */
export const PROCESS_ALIVE_FLOOR: AgentState = "working";
/** Tier-2 clean exit (code 0, no signal). */
export const PROCESS_CLEAN_EXIT_STATE: AgentState = "done";
/** Tier-2 abnormal exit (nonzero code, or killed by a signal). */
export const PROCESS_ABNORMAL_EXIT_STATE: AgentState = "crashed";

/**
 * Tier-3 scrape classification → state, with the strict-blocked + idle-fallback
 * bias: only a KNOWN approval UI claims `needs-permission`; an unknown prompt
 * falls back to `idle`, never a false "needs you".
 */
export const SCRAPE_STATE: Readonly<Record<ScrapeSignal, AgentState>> = {
  "known-approval-prompt": "needs-permission",
  "byte-quiet": "idle",
  unknown: "idle",
};

/** Map one Tier-1 signal to its state via the tables above. */
export function explicitSignalState(signal: ExplicitSignal): AgentState {
  switch (signal.kind) {
    case "claude-hook":
      return CLAUDE_HOOK_STATE[signal.event];
    case "signal-done":
      return SIGNAL_DONE_STATE[signal.outcome];
    case "ask-user":
      return ASK_USER_STATE;
    case "ask-resolved":
      return ASK_RESOLVED_STATE;
    case "signal-progress":
      return SIGNAL_PROGRESS_STATE[signal.phase];
  }
}

/** How a process ended, as far as its exit status says (Rust `ExitClass`). */
export const EXIT_CLASSES = ["clean", "abnormal", "unknown"] as const;

export type ExitClass = (typeof EXIT_CLASSES)[number];

/**
 * Classify an exit status — the one classifier every exit path shares (Rust
 * `classify_exit`, which the headless run outcome and the `pty-exit` payload
 * use). A signal wins over any code; an empty signal name counts as none.
 */
export function classifyExit(code: number | null, signal?: string | null): ExitClass {
  if (signal != null && signal !== "") return "abnormal";
  if (code == null) return "unknown";
  return code === 0 ? "clean" : "abnormal";
}

/**
 * True when a Tier-2 exit is abnormal: killed by a signal, or a KNOWN nonzero
 * code — unless CodeSense asked for it. An unknown code (null, no signal) is not
 * abnormal — otherwise a normal `/exit` observed by the code-less local walk
 * would read `crashed`.
 */
export function isAbnormalExit(p: Extract<ProcessSignal, { kind: "exited" }>): boolean {
  return p.requested !== true && classifyExit(p.code, p.signal) === "abnormal";
}

/**
 * How a session's own process ended, for its ended/crashed marks (the pane's
 * bar, the tab marker, the tray). OS notifications do not read it: they leave
 * plain shells out.
 *
 * A plain shell crashed only when a signal killed it. Any status it exited
 * with — a plain `false; exit`, or `exit` after a Ctrl-C'd command (130) — is
 * the shell's own decision to end. When the exit is the one that crashed the
 * agent running in the session (`agentCrashed`, AS-6's classification), it
 * crashed. An exit CodeSense asked for is never a crash.
 */
export function sessionExitOutcome(
  exit: { code: number | null; signal: string | null; requested: boolean },
  agentCrashed: boolean,
): "done" | "crashed" {
  if (exit.requested) return "done";
  if (exit.signal != null && exit.signal !== "") return "crashed";
  return agentCrashed ? "crashed" : "done";
}

/** The ingestion seq of the exit that crashed the session's agent, while that
 *  crash is the record's latest word; else null. */
export function agentCrashSeq(rec: SessionAgentRecord | undefined): number | null {
  if (rec === undefined || rec.state !== "crashed" || !endedByExit(rec)) return null;
  return rec.t2?.seq ?? null;
}

/** Map one Tier-2 signal to its state in isolation (alive → the floor). */
export function processSignalState(p: ProcessSignal): AgentState {
  if (p.kind === "alive") return PROCESS_ALIVE_FLOOR;
  return isAbnormalExit(p) ? PROCESS_ABNORMAL_EXIT_STATE : PROCESS_CLEAN_EXIT_STATE;
}

// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/** Tier-1 states that describe an in-flight turn — superseded by a later process exit. */
const IN_FLIGHT: ReadonlySet<AgentState> = new Set<AgentState>([
  "working",
  "thinking",
  "needs-permission",
  "blocked",
]);

/** States the alive floor preserves from `prev` (at-least-working; `thinking` is a peer). */
const BUSY: ReadonlySet<AgentState> = new Set<AgentState>(["working", "thinking"]);

/** True when `input` is strictly fresher than a (cleared) Tier-1 input, or Tier-1 is absent. */
function postDates(input: { seq: number }, t1: Tier1Input | null | undefined): boolean {
  return t1 == null || input.seq > t1.seq;
}

/**
 * Resolve the canonical state from the three tiers' latest inputs (each may be
 * absent) plus the previously resolved state. Pure: same inputs → same output.
 *
 * Precedence is signal > process > scrape. A lower tier may only FILL an
 * unknown; it never overrides a higher-tier truth — with exactly one exception,
 * process death:
 *
 *  1. **Process-death is terminal.** A Tier-2 exit at least as fresh as the
 *     Tier-1 input (`t2.seq >= t1.seq`) is the end of that agent's life:
 *     - after a clear (`SessionEnd` → `idle`) the agent had already ended, so
 *       the exit is its host's and the state stays `idle` whatever the code;
 *     - abnormal (killed by a signal, or a known nonzero code) and not
 *       requested → `crashed`, over any other Tier-1 state (a `kill -9` mid-turn
 *       — or after `Stop` — is dead, not `working`/`done`); a requested exit (a
 *       tab close, a stop) is treated as clean;
 *     - an unknown code with no signal is clean, except when it is
 *       `unannounced` over an in-flight Tier-1 state: the agent vanished
 *       mid-turn without its end hook → `crashed`;
 *     - clean → `done`, superseding an in-flight Tier-1 state
 *       (`working`/`thinking`/`needs-permission`), but a declared settled Tier-1
 *       state (`done`/`errored`) holds — it is the more specific truth.
 *     A Tier-1 input strictly fresher than the exit is a new declaration (a new
 *     agent life in the same session) and wins by precedence.
 *  2. **Tier-1 is authoritative.** Any Tier-1 input other than a cleared one
 *     (`idle`, e.g. `SessionEnd`) is the answer. A later Tier-2 "alive" tick
 *     never overwrites it — the process is briefly alive at the prompt after
 *     `Stop`, exactly the `floorDotStatus = current ?? "running"` rule.
 *  3. **Tier-2 alive is a floor** used when Tier-1 is absent, or cleared and the
 *     alive tick is fresher than the clear: at-least-`working`, keeping `prev`
 *     when it was already `thinking`/`working`. A Tier-3 known approval prompt
 *     refines the floor to `needs-permission`; byte-quiet / unknown never pull a
 *     live process below the floor.
 *  4. **Tier-3 alone** (no CURRENT process knowledge — Tier-2 absent, or older
 *     than a Tier-1 clear; Tier-1 absent, or cleared before the observation)
 *     maps through `SCRAPE_STATE`: unknown → `idle`, never `needs-permission`.
 *  5. A cleared Tier-1 (`idle`) with nothing fresher, or no input at all → `idle`.
 */
export function resolveAgentState(
  t1: Tier1Input | null | undefined,
  t2: Tier2Input | null | undefined,
  t3: Tier3Input | null | undefined,
  prev: AgentState | null | undefined,
): AgentState {
  const t1State = t1 ? explicitSignalState(t1.signal) : null;

  // 1. Process death is terminal (when not older than the Tier-1 declaration).
  if (t2 && t2.signal.kind === "exited" && (t1 == null || t2.seq >= t1.seq)) {
    // The agent already ended (a clear such as `SessionEnd`): this exit is its
    // host's — a shell exiting later — and says nothing about the agent.
    if (t1State === "idle") return "idle";
    const inFlight = t1State == null || IN_FLIGHT.has(t1State);
    if (isAbnormalExit(t2.signal)) return PROCESS_ABNORMAL_EXIT_STATE;
    if (t2.signal.unannounced === true && t1State != null && inFlight) {
      return PROCESS_ABNORMAL_EXIT_STATE;
    }
    if (inFlight) return PROCESS_CLEAN_EXIT_STATE;
    return t1State;
  }

  // 2. An uncleared Tier-1 declaration is authoritative.
  if (t1State != null && t1State !== "idle") return t1State;

  // From here Tier-1 is absent or cleared. An exit older than a clear belongs to
  // a previous agent life and says nothing about now.

  // 3. Tier-2 alive floor — fills an absent Tier-1, or a cleared one it post-dates.
  if (t2 && t2.signal.kind === "alive" && postDates(t2, t1)) {
    if (t3 && t3.signal === "known-approval-prompt") return SCRAPE_STATE[t3.signal];
    return prev != null && BUSY.has(prev) ? prev : PROCESS_ALIVE_FLOOR;
  }

  // 4. Scrape fills when Tier-1 is absent or was cleared before the scrape
  //    observation. Any Tier-2 input that reaches this point does not post-date
  //    the clear (steps 1 and 3 already took every current one; an alive tick at
  //    the same seq counts as the clear), so it does not block a fresher scrape.
  if (t3 && postDates(t3, t1)) return SCRAPE_STATE[t3.signal];

  // 5. Cleared, or nothing known.
  return "idle";
}

// ---------------------------------------------------------------------------
// Live per-session record
// ---------------------------------------------------------------------------

/**
 * One session's live agent state: the latest input of each tier the app
 * ingests today (Claude hooks and MCP signals, the process-liveness probe), the
 * state the resolver derives from them, and the agent the probe looks for.
 */
export interface SessionAgentRecord {
  /** Last positively observed host turn, retained through its Stop/exit for reordered completion. */
  hostTurn?: import("@/tauri/commands").ActivityCompletion;
  /** Ingestion sequence of an explicitly observed new turn, not a trailing tool hook. */
  turnStartedSeq?: number;
  /** Legacy hooks have no tool ids; retain unmatched starts across progress reports. */
  openTools?: number;
  /** Last admitted hook, MCP signal, host revision or live PTY output (epoch ms).
   * Liveness probes and repeated snapshot polls do not refresh this clock. */
  activityAt?: number;
  t1: Tier1Input | null;
  t2: Tier2Input | null;
  state: AgentState;
  /**
   * The agent process established in this session (the liveness probe's
   * target), or null when unknown. A Claude hook establishes `"claude"`; later
   * inputs from other sources (an MCP signal the same agent sent) keep it. It
   * is dropped when that agent ends (`SessionEnd`, a process exit), so a
   * different agent's later signals are not probed as Claude.
   */
  agent: string | null;
}

/**
 * Where a Tier-2 observation came from:
 *  - `probe` — the periodic process walk (alive/gone, never a code);
 *  - `command-end` — the shell's OSC 133 command-end status for the agent it
 *    ran in the foreground (the agent's own exit code; a signal death reads as
 *    `128 + n`);
 *  - `pty` — the session's own process exited (`pty-exit`, with the status its
 *    host — in-process or the session-server sidecar — reaped).
 */
export type ProcessSource = "probe" | "command-end" | "pty";

/** One ingested observation for a session. */
export type SessionAgentInput =
  | { tier: 1; signal: ExplicitSignal }
  | { tier: 2; signal: ProcessSignal; source?: ProcessSource };

/**
 * Whether a Claude hook may replace a `needs-permission` on display. A
 * `PreToolUse` never does: in a turn the order is `PreToolUse` →
 * `PermissionRequest` → (approval) → tool → `PostToolUse`, so a `PreToolUse`
 * that arrives while blocked is a sibling tool call (a parallel tool or a
 * subagent), not the approval. Every other hook, and a process exit, still
 * clears it.
 */
export function hookClearsPermission(event: ClaudeHookEvent): boolean {
  return event !== "PreToolUse";
}

/** True while the record's turn is open (the agent process should be running). */
export function isTurnOpen(rec: SessionAgentRecord): boolean {
  return IN_FLIGHT.has(rec.state);
}

/** The process name the liveness probe looks for: the agent established in the
 *  session; `null` when unknown. */
export function agentProcessName(rec: SessionAgentRecord): string | null {
  return rec.agent;
}

/** The session's established agent after `input` (see `SessionAgentRecord.agent`). */
function nextAgent(prev: SessionAgentRecord | undefined, input: SessionAgentInput): string | null {
  if (input.tier === 1 && input.signal.kind === "claude-hook") {
    return input.signal.event === "SessionEnd" ? null : "claude";
  }
  if (input.tier === 2 && input.signal.kind === "exited") return null;
  return prev?.agent ?? null;
}

/** Same observation (the `unannounced` stamp is derived, not observed). */
function sameProcessSignal(a: ProcessSignal, b: ProcessSignal): boolean {
  if (a.kind === "alive" || b.kind === "alive") return a.kind === b.kind;
  return (
    a.code === b.code &&
    (a.signal ?? null) === (b.signal ?? null) &&
    (a.requested === true) === (b.requested === true)
  );
}

/** True when the record's latest input is a process exit: its agent's life ended there. */
export function endedByExit(rec: SessionAgentRecord): boolean {
  return rec.t2?.signal.kind === "exited" && (rec.t1 == null || rec.t2.seq >= rec.t1.seq);
}

/** Display-only escape from a historical task result, requiring ordered turn evidence. */
export function hasTurnAfterCompletion(
  rec: SessionAgentRecord | undefined,
  completionSeq: number | undefined,
): boolean {
  return (
    completionSeq !== undefined &&
    rec?.turnStartedSeq !== undefined &&
    rec.turnStartedSeq > completionSeq
  );
}

/**
 * The stop signals, by host OS, in the order STOP, TSTP, TTIN, TTOU. A job a
 * stop signal suspended (Ctrl-Z) leaves its shell a status of 128 + the
 * signal's number: it is not an exit.
 */
export const DARWIN_STOP_SIGNALS = [17, 18, 21, 22] as const;
export const LINUX_STOP_SIGNALS = [19, 20, 21, 22] as const;

const DARWIN_STOPPED = new Set(DARWIN_STOP_SIGNALS.map((n) => 128 + n));
const LINUX_STOPPED = new Set(LINUX_STOP_SIGNALS.map((n) => 128 + n));

/** The shell statuses that mean "suspended" on a macOS or a Linux host. */
export function stoppedStatuses(mac: boolean): ReadonlySet<number> {
  return mac ? DARWIN_STOPPED : LINUX_STOPPED;
}

/** Whether this app runs on macOS (read per call, so tests can stub it). */
function hostIsMac(): boolean {
  return typeof navigator !== "undefined" && /Mac/i.test(navigator.platform ?? "");
}

/**
 * Whether a shell's command-end `code` in this session is the agent's own
 * exit: an agent is established and running in the foreground, or it just
 * vanished `unannounced` and this is the status that says how. A stop status
 * (the agent was suspended, not ended) never is.
 */
export function commandEndIsAgentExit(rec: SessionAgentRecord, code: number): boolean {
  // The shells already report no command-end for a suspended job; this is
  // the backstop, for this host's stop signals.
  if (stoppedStatuses(hostIsMac()).has(code)) return false;
  if (rec.agent !== null) return true;
  const t2 = rec.t2?.signal;
  return endedByExit(rec) && t2?.kind === "exited" && t2.unannounced === true && t2.code === null;
}

/**
 * Whether the session's own process exiting (`pty-exit`) is its agent's exit.
 * Agents run inside the tab's login shell, so the PTY child is that shell: its
 * exit is the agent's only while the agent is still established with its turn
 * open (the shell died under it). Once the agent has finished (a settled
 * state, `SessionEnd`, or an MCP-only agent that never established itself),
 * the shell's own exit status — `false; exit`, Ctrl-C then Ctrl-D — says
 * nothing about the agent. (No launch path starts an agent as the PTY child
 * itself today; such a launch would count too.)
 */
export function ptyExitIsAgentExit(rec: SessionAgentRecord): boolean {
  return rec.agent !== null && IN_FLIGHT.has(rec.state);
}

/**
 * Stamp `unannounced` on a code-less exit that ends an open turn of an agent
 * that announces its own end, and that was seen running (the session's own
 * process exiting counts: it hosted the agent).
 */
function stampExit(
  prev: SessionAgentRecord | undefined,
  signal: ProcessSignal,
  source: ProcessSource | undefined,
): ProcessSignal {
  if (
    signal.kind !== "exited" ||
    prev == null ||
    classifyExit(signal.code, signal.signal) !== "unknown" ||
    signal.requested === true ||
    prev.agent === null ||
    !IN_FLIGHT.has(prev.state)
  ) {
    return signal;
  }
  const seenRunning = source === "pty" || prev.t2?.signal.kind === "alive";
  return seenRunning ? { ...signal, unannounced: true } : signal;
}

/**
 * Fold one observation into a session's record. `seq` must be larger than every
 * seq already in `prev`.
 *
 * A Claude turn is bracketed by hooks: `UserPromptSubmit` opens it
 * (`thinking`), `PreToolUse`/`PostToolUse` move it between `working` and
 * `thinking`, `Stop` closes it (`done`). A long silent turn fires no hook at
 * all, so the liveness probe's periodic `alive` tick is its heartbeat: while
 * the turn is open each tick confirms the process is still there and the
 * resolver keeps the declared `thinking` (a Tier-2 tick never overrides a
 * Tier-1 declaration). When the process is gone the tick is an exit, which ends
 * the turn instead of leaving a dead session reading `thinking`.
 *
 * A `PreToolUse` that arrives while the record reads `needs-permission` keeps
 * that display state while retaining the sibling tool's open-call evidence.
 *
 * A process exit ends the turn: `done` when it is clean, `crashed` when it is
 * abnormal, or when a code-less exit is `unannounced` (see `ProcessSignal`).
 *
 * A code-less exit after the record already ended on an exit is dropped: the
 * earlier exit's status stands.
 *
 * A repeated Tier-2 observation that changes nothing returns `prev` itself, so
 * a steady poll does not churn the store. Dropping it is lossless: the stored
 * tick already post-dates the Tier-1 input, which is the only ordering the
 * resolver reads, the resolved state is a function of those same inputs, and
 * every later Tier-1 input post-dates both.
 */
export function advanceSessionAgent(
  prev: SessionAgentRecord | undefined,
  input: SessionAgentInput,
  seq: number,
): SessionAgentRecord {
  let openTools =
    prev?.openTools ??
    (prev?.t1?.signal.kind === "claude-hook" && prev.t1.signal.event === "PreToolUse" ? 1 : 0);
  if (input.tier === 2 && input.signal.kind === "exited") openTools = 0;
  if (input.tier === 1) {
    const signal = input.signal;
    if (signal.kind === "signal-done") openTools = 0;
    if (signal.kind === "claude-hook") {
      if (signal.event === "PreToolUse") openTools++;
      else if (signal.event === "PostToolUse" || signal.event === "PostToolUseFailure")
        openTools = Math.max(0, openTools - 1);
      else if (["Stop", "StopFailure", "SessionEnd", "UserPromptSubmit"].includes(signal.event))
        openTools = 0;
    }
  }
  if (
    prev?.state === "needs-permission" &&
    input.tier === 1 &&
    input.signal.kind === "claude-hook" &&
    !hookClearsPermission(input.signal.event)
  ) {
    return { ...prev, openTools };
  }
  if (
    input.tier === 2 &&
    prev?.t2 != null &&
    sameProcessSignal(prev.t2.signal, input.signal) &&
    (prev.t1 == null || prev.t2.seq > prev.t1.seq)
  ) {
    return prev;
  }
  // A code-less "gone" after the agent already exited says nothing new, and
  // must not blur the status that exit carried.
  if (
    input.tier === 2 &&
    input.signal.kind === "exited" &&
    input.signal.requested !== true &&
    classifyExit(input.signal.code, input.signal.signal) === "unknown" &&
    prev != null &&
    endedByExit(prev)
  ) {
    return prev;
  }
  const t1 = input.tier === 1 ? { signal: input.signal, seq } : (prev?.t1 ?? null);
  const t2 =
    input.tier === 2
      ? { signal: stampExit(prev, input.signal, input.source), seq }
      : (prev?.t2 ?? null);
  let state = resolveAgentState(t1, t2, null, prev?.state);
  if (state === "thinking" && openTools > 0) state = "working";
  return { ...prev, t1, t2, state, openTools, agent: nextAgent(prev, input) };
}

// ---------------------------------------------------------------------------
// Projections
// ---------------------------------------------------------------------------

/**
 * `AgentState` → the status dot. Total over every state, and every non-idle
 * state paints its own swatch: `thinking` (a model turn, no tool) is not
 * `running` (a tool in flight), and `crashed` (the process died) is not `error`
 * (a declared failure, process still up). `undefined` is the clear (no dot).
 */
export const AGENT_STATE_DOT: Readonly<Record<AgentState, TerminalAiStatus | undefined>> = {
  working: "running",
  thinking: "thinking",
  "needs-permission": "needs-permission",
  // A self-reported block reads as `needs-permission` on the coarse terminal dot
  // vocabulary (the dot's palette has no finer "blocked" swatch yet); the richer
  // `NeedsYouKind.blocked` surfaces the distinction on the agents list and the
  // Needs-you lane.
  blocked: "needs-permission",
  done: "done",
  errored: "error",
  crashed: "crashed",
  idle: undefined,
};

/**
 * `AgentState` → the coarse wire status: what the `terminal-status-update`
 * event carries (the Rust receiver's `terminal_dot_wire`, checked against this
 * table) and what the stored per-session status holds. It folds `thinking`
 * into `running` and `crashed` into `error`; the dot gets the finer states from
 * the record (`agentRecordDot`). `undefined` is the store's clear: the status-event
 * hook stores nothing for the wire's transient `"idle"` and
 * `setSessionAiStatus(id, undefined)` removes the entry.
 */
export const AGENT_STATE_TERMINAL_DOT: Readonly<
  Record<AgentState, TerminalWireStatus | undefined>
> = {
  working: "running",
  thinking: "running",
  "needs-permission": "needs-permission",
  blocked: "needs-permission",
  done: "done",
  errored: "error",
  crashed: "error",
  idle: undefined,
};

export function agentStateToTerminalDot(state: AgentState): TerminalWireStatus | undefined {
  return AGENT_STATE_TERMINAL_DOT[state];
}

/**
 * `AgentState` → the Agents-panel live-progress `Phase`.
 *
 * `Phase` is a run-lifecycle vocabulary, not a liveness one, so a few states
 * have no faithful counterpart:
 *  - `working`/`thinking` → `dev` (the "in progress / active" phase).
 *  - `errored`/`crashed` → `blocked`: `Phase` has no failure value, and a
 *    failed or dead agent needs a human exactly like a blocked one (role
 *    `blocked`). The dot still distinguishes them.
 *  - `idle` → `undefined`: an empty slot has no live phase (no chip), matching
 *    the dot's clear.
 */
export const AGENT_STATE_PHASE: Readonly<Record<AgentState, Phase | undefined>> = {
  working: "dev",
  thinking: "dev",
  "needs-permission": "blocked",
  blocked: "blocked",
  done: "done",
  errored: "blocked",
  crashed: "blocked",
  idle: undefined,
};

export function agentStateToPhase(state: AgentState): Phase | undefined {
  return AGENT_STATE_PHASE[state];
}

/**
 * The stored status a session keeps once its agent's process is gone, or
 * `undefined` to clear it: an agent that ended on an exit keeps its state when
 * that exit crashed it or carried a known status; a code-less clean vanish
 * clears, as the liveness floor always has (it also heals a dot a misrouted
 * hook left behind), and so does a record whose agent did not end on an exit —
 * its tab's shell exiting clears the dot as it always did.
 */
export function deathDot(rec: SessionAgentRecord | undefined): TerminalWireStatus | undefined {
  if (rec == null || !endedByExit(rec)) return undefined;
  const t2 = rec.t2?.signal;
  if (t2?.kind !== "exited") return undefined;
  if (rec.state === "crashed" || t2.code !== null) return agentStateToTerminalDot(rec.state);
  return undefined;
}

/**
 * The dot the agent record dictates, or `null` when it has no say and the
 * session's stored status stands:
 *  - no record (a plain shell, an agent that never declared itself);
 *  - the agent ended cleanly (`SessionEnd` → `idle`);
 *  - the agent ended on an exit that leaves no dot (a code-less clean vanish),
 *    where every path that folds the exit also clears the stored status.
 * Otherwise the record's state projects through `AGENT_STATE_DOT`, so a live
 * turn reads `thinking` / `running` and a crash holds `crashed`. A session's
 * dot is this, else its stored status — so a session with no record renders
 * exactly as before.
 */
export function agentRecordDot(rec: SessionAgentRecord | undefined): TerminalAiStatus | null {
  if (rec == null || rec.state === "idle") return null;
  if (endedByExit(rec) && deathDot(rec) === undefined) return null;
  return AGENT_STATE_DOT[rec.state] ?? null;
}
