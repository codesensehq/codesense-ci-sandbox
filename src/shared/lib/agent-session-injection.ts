// SPDX-License-Identifier: MIT
import {
  sessionIsIdle,
  writePty,
  submitPtyLine,
  terminalLaunchActivityLine,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";
import {
  getViewerState,
  selectTabBySessionId,
  type TerminalAiStatus,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { sessionTarget } from "@/shared/lib/terminal-layout-projection";

/**
 * SW-GS-2a (AC-8/AC-9): the guarded auto-injection wrapper over `writePty`.
 *
 * CodeSense drives an agent session by *typing for it* — the `claude` launch
 * line, the SW-GS-2 recipe trigger, and the `/cs-status` re-orient on reconnect
 * are all auto-sent. Raw `writePty` is unsafe for that: it sends no newline, has
 * no idle gate, and returns `Ok(())` SILENTLY when the session is gone. This
 * wrapper adds the three guards the auto path needs while leaving the raw
 * `writePty` / `sendTextToSession` contracts untouched (passthrough user text in
 * SW-GS-1's chat box stays ungated — AC-9).
 *
 * This is a REFERENCE-PATTERN primitive (see story banner): nothing here is
 * agent-specific, so every story/epic agent surface reuses it verbatim.
 */

/** Whether the agent session is ready to receive an auto-sent line. */
export type AtPromptDecision = "at-prompt" | "busy";

/**
 * Gate mode (AC-8b) — which readiness signal applies to the line being sent:
 *  - `"shell"`: the target is a bare `zsh` (the agent launch / relaunch line).
 *    Gate on tree-idle only; `aiStatus` from a now-dead agent is irrelevant and
 *    must NOT wedge the relaunch (a Ctrl+C/crash leaves a stale `aiStatus` with
 *    no SessionEnd hook to clear it).
 *  - `"none"`: the target is a CONFIRMED-LIVE agent (caller already waited on the
 *    `sessionHasLiveAgent` liveness probe). Send unconditionally — `session_is_idle`
 *    is always false while the agent process is alive, so no idle gate can pass,
 *    and the agent buffers stdin until it is ready. Used for the post-launch
 *    recipe trigger and the reconnect `/cs-status`.
 *  - `"auto"` (default): prefer the hook-driven `aiStatus` when present (the
 *    attention signal for SW-GS-1's chip); fall back to tree-idle. Best-effort —
 *    the notify hook is not guaranteed at `org_root`, so callers that MUST inject
 *    use `"shell"`/`"none"` with the liveness probe instead of trusting `aiStatus`.
 */
export type GateMode = "shell" | "none" | "auto";

export function decideAtPrompt(
  aiStatus: TerminalAiStatus | undefined,
  shellIdle: boolean,
  gate: GateMode = "auto",
): AtPromptDecision {
  if (gate === "none") return "at-prompt";
  if (gate === "shell") return shellIdle ? "at-prompt" : "busy";
  // auto: prefer the hook-driven status, fall back to tree-idle.
  if (aiStatus === "done") return "at-prompt";
  if (aiStatus === "running" || aiStatus === "needs-permission" || aiStatus === "error") {
    return "busy";
  }
  return shellIdle ? "at-prompt" : "busy";
}

/**
 * AC-8a — how we "press Enter" after a line.
 *
 * Submit EXACTLY like the chat box (the proven-working path): write the line and
 * the Enter in ONE buffer — `text\r`. The Enter is a CARRIAGE RETURN (`\r`), the
 * real Enter keystroke; it works in both the shell (line discipline maps CR→NL)
 * and a raw-mode TUI.
 *
 * Two things that do NOT work, learned the hard way:
 *  - `\n` (line feed): a raw-mode TUI keeps it as a newline INSIDE the input box
 *    and never submits (the original "typed but not sent" bug).
 *  - a SEPARATE `\r` write after the text: a lone `\r` byte arriving in its own
 *    write (after the agent already ingested the text) gets dropped / not treated
 *    as a submit. The chat box works because the `\r` rides in the SAME write as
 *    the text and is processed atomically — so we do the same here.
 */
export const ENTER_KEY = "\r";

/** Injected seams so the wrapper unit-tests without a real PTY. */
export interface InjectDeps {
  /** True iff a terminal tab with this sessionId currently exists. */
  sessionExists: (sessionId: string) => boolean;
  /** Live hook-driven status for the session, if any. */
  getAiStatus: (sessionId: string) => TerminalAiStatus | undefined;
  /** Process-tree idle (the `session_is_idle` command). */
  isShellIdle: (sessionId: string) => Promise<boolean>;
  /** Raw write (the `writePty` command). */
  write: (sessionId: string, text: string) => Promise<void>;
  /** Managed shell launch only, after its idle gate has admitted the write. */
  writeShell?: (sessionId: string, text: string) => Promise<void>;
  /**
   * SW-GS-30 — reliable submit (the `submit_pty_line` command): settle → write →
   * confirm-from-output → discrete-Enter nudge. Passed the RAW line (it owns the
   * submit key). Resolves `{ submitted }` — `false` ⇒ the line stayed held after
   * every nudge. Used only by the opt-in `{ submit:true }` path.
   */
  submit: (sessionId: string, line: string) => Promise<{ submitted: boolean }>;
  submitShell?: (sessionId: string, line: string) => Promise<{ submitted: boolean }>;
}

export type InjectOutcome =
  | { ok: true }
  // SW-GS-30 `not-submitted`: the `{ submit:true }` path wrote the line but the
  // agent never left its prompt after the bounded nudges (paste-held / unresponsive)
  // — distinct from `agent-busy` (the line was never written, agent mid-turn).
  | { ok: false; reason: "agent-busy" | "not-submitted" };

/**
 * SW-GS-22: is this session's process still around (its tab shown, Hidden, or
 * in a minimized workspace, and not exited)? The SINGLE source of truth for
 * "the session is still around", shared by the guarded injector below
 * (`defaultInjectDeps`) AND the reopen-reconnect gate in
 * `useAgentWorkspaceSession` — so the mount effect and the injector can never
 * disagree about liveness. SW-GS-2d: a MINIMIZED or Hidden agent's PTY is
 * alive, so its tab counts; an exited tab (kept read-only) does not.
 */
export function sessionExists(sessionId: string): boolean {
  const tab = selectTabBySessionId(getViewerState(), sessionId);
  return tab !== undefined && tab.exit === undefined;
}

function managedLaunchTarget(
  sessionId: string,
  text: string,
):
  | { target: NonNullable<ReturnType<typeof sessionTarget>>; provider: "claude" | "codex" }
  | undefined {
  const first = /^(?:'([^']*)'|([^\s"'`;|&<>]+))(?:\s|$)/.exec(text.trimStart());
  const provider = (first?.[1] ?? first?.[2])?.split("/").at(-1);
  const target = sessionTarget(getViewerState().terminalLayout, sessionId);
  return target && (provider === "claude" || provider === "codex")
    ? { target, provider }
    : undefined;
}

export const defaultInjectDeps: InjectDeps = {
  sessionExists,
  // SW-FX-52: status lives in the flat `aiStatusBySession` slice keyed by
  // sessionId — a direct O(1) read, no tab scan.
  getAiStatus: (sessionId) => getViewerState().aiStatusBySession[sessionId],
  isShellIdle: (sessionId) => sessionIsIdle(sessionId),
  write: (sessionId, text) => writePty(sessionId, text),
  writeShell: async (sessionId, text) => {
    const launch = managedLaunchTarget(sessionId, text);
    if (launch) {
      await terminalLaunchActivityLine(launch.target, launch.provider, text);
    } else {
      await writePty(sessionId, text);
    }
  },
  submitShell: (sessionId, line) => {
    const launch = managedLaunchTarget(sessionId, line);
    return launch
      ? terminalLaunchActivityLine(launch.target, launch.provider, line, true)
      : submitPtyLine(sessionId, line);
  },
  submit: (sessionId, line) => submitPtyLine(sessionId, line),
};

/**
 * Guarded auto-injection (AC-8):
 *  (a) appends a trailing `\n` if absent;
 *  (b) gates on agent-at-prompt — returns `{ok:false, reason:"agent-busy"}`
 *      without writing when the agent is mid-turn;
 *  (c) THROWS loudly when the target session is gone (raw `writePty` would have
 *      returned `Ok(())` and silently dropped the line).
 *
 * The shell-idle IPC is only issued when `aiStatus` is non-decisive, so a
 * known-`done` agent injects with zero extra round-trips.
 *
 * SW-GS-30 — `opts.submit`: for a line the agent MUST run, pass `submit:true` to
 * route the write through `submit_pty_line` (the `submit` seam) instead of a raw
 * one-write `text\r`. That settles, writes, confirms the submit from the agent's
 * own output, and nudges a discrete Enter if the line was paste-held — then
 * reports whether it actually submitted. A held line ⇒ `{ok:false,
 * reason:"not-submitted"}` (distinct from `agent-busy`). Short lines confirm on
 * the first output resumption with zero nudges (no extra latency). The default
 * (`submit` omitted) keeps the proven raw `text\r` fast path unchanged.
 */
export async function injectAgentLine(
  sessionId: string,
  line: string,
  deps: InjectDeps = defaultInjectDeps,
  opts: { gate?: GateMode; submit?: boolean } = {},
): Promise<InjectOutcome> {
  if (!deps.sessionExists(sessionId)) {
    // AC-8c: loud — the owned tab died/closed under us. Never a silent no-op.
    throw new Error(
      `Guarded injection target session "${sessionId}" not found — the agent session is gone.`,
    );
  }

  const gate = opts.gate ?? "auto";
  const aiStatus = gate === "auto" ? deps.getAiStatus(sessionId) : undefined;
  // Only the `shell` gate (and an `auto` gate with a non-decisive aiStatus)
  // needs the idle IPC; `none` never does, and a decisive aiStatus short-circuits.
  const needsIdleProbe =
    gate === "shell" ||
    (gate === "auto" &&
      aiStatus !== "done" &&
      aiStatus !== "running" &&
      aiStatus !== "needs-permission" &&
      aiStatus !== "error");
  const shellIdle = needsIdleProbe ? await deps.isShellIdle(sessionId) : false;

  if (decideAtPrompt(aiStatus, shellIdle, gate) === "busy") {
    return { ok: false, reason: "agent-busy" };
  }

  // Strip a trailing newline once — both submit paths pass the RAW line (the Enter
  // is owned by `submit_pty_line` / the atomic `+ ENTER_KEY` write), so the
  // normalization must not diverge between them.
  const rawLine = line.replace(/[\r\n]+$/, "");

  // gate "shell" = the line is TYPED INTO THE BARE zsh (agent launch / relaunch /
  // dashboard tab-verb command). Prefix it with a LEADING SPACE so the managed
  // .zshrc's `setopt HIST_IGNORE_SPACE` keeps codesense plumbing out of the
  // user's ↑ history. This lives at THE chokepoint (every shell-gate caller —
  // `injectWhenReady`, `workspace-tab-verbs` `submitLine`) funnels through here,
  // so no direct caller can bypass it. gate "none"/"auto" (text bound for the
  // live agent TUI, not the shell) is untouched.
  const shellLine = gate === "shell" ? ` ${rawLine}` : rawLine;

  // SW-GS-30: a MUST-run line routes through `submit_pty_line` — it owns the
  // submit key (pass the RAW line, no hand-appended `\r`), confirms from output,
  // and nudges a held (paste-batched) line. A line that never submits after the
  // bounded nudges is legible, not a false `{ok:true}`.
  if (opts.submit) {
    const { submitted } = await (
      gate === "shell" && deps.submitShell ? deps.submitShell : deps.submit
    )(sessionId, shellLine);
    return submitted ? { ok: true } : { ok: false, reason: "not-submitted" };
  }

  // Default fast path: the line + Enter (`\r`) in ONE write — same as the chat box
  // (the proven submit path). See ENTER_KEY docs for why \n and a separate \r both fail.
  await (gate === "shell" && deps.writeShell ? deps.writeShell : deps.write)(
    sessionId,
    shellLine + ENTER_KEY,
  );
  return { ok: true };
}

/**
 * SW-GS-2d — a bare Enter "nudge" to a CONFIRMED-LIVE agent session. The agent
 * TUI batches a long line as a PASTE and holds the trailing Enter as content; a
 * DISCRETE Enter sent a moment later (outside the paste burst) submits it. Used
 * by the handshake to nudge a prime that landed but didn't submit, retried until
 * the pong confirms. No gating/throwing — fire-and-forget against a live session.
 */
export function pressEnter(
  sessionId: string,
  write: (sessionId: string, text: string) => Promise<void> = writePty,
): Promise<void> {
  return write(sessionId, ENTER_KEY);
}

/**
 * SW-GS-2a (AC-11): inject `line`, retrying while the agent is still busy until
 * it reaches its prompt (or `attempts` is exhausted). Used by Reconnect to
 * auto-run `/cs-status` once a freshly-relaunched agent has come up. A
 * session-not-found still THROWS immediately (it never becomes ready). `sleep`
 * is injectable so tests don't wait on real timers.
 *
 * SW-GS-30 — `opts.submit` threads through to `injectAgentLine` (route MUST-run
 * lines through `submit_pty_line`). Only `agent-busy` is retry-eligible: a
 * `not-submitted` outcome means the line WAS written and `submit_pty_line` already
 * spent its full settle/confirm/nudge budget — re-attempting would just re-run
 * that costly cycle, so it returns immediately (the caller records
 * `send-not-submitted`).
 */
export async function injectWhenReady(
  sessionId: string,
  line: string,
  opts: {
    attempts?: number;
    sleepMs?: number;
    sleep?: (ms: number) => Promise<void>;
    gate?: GateMode;
    submit?: boolean;
  } = {},
  deps: InjectDeps = defaultInjectDeps,
): Promise<InjectOutcome> {
  const attempts = opts.attempts ?? 30;
  const sleepMs = opts.sleepMs ?? 200;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  let last: InjectOutcome = { ok: false, reason: "agent-busy" };
  for (let i = 0; i < attempts; i++) {
    // The shell-gate leading space (HIST_IGNORE_SPACE) is applied inside
    // `injectAgentLine` — THE chokepoint — so every shell-gate caller shares it.
    last = await injectAgentLine(sessionId, line, deps, {
      gate: opts.gate,
      submit: opts.submit,
    });
    if (last.ok) return last;
    // A written-but-unsubmitted line is terminal here — only a mid-turn (`agent-busy`,
    // never-written) line is worth waiting out and retrying.
    if (last.reason === "not-submitted") return last;
    // Don't sleep after the final attempt (L3).
    if (i < attempts - 1) await sleep(sleepMs);
  }
  return last;
}
