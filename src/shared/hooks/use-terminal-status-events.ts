// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  getViewerState,
  TERMINAL_WIRE_STATUSES,
  type TerminalWireStatus,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { watchOpenTurns, watchTerminalTabAgents } from "@/shared/hooks/use-agent-liveness-monitor";
import {
  CLAUDE_HOOK_EVENTS,
  type ClaudeHookEvent,
  deathDot,
  type ExplicitSignal,
  hookClearsPermission,
  SIGNAL_DONE_OUTCOMES,
  type SignalDoneOutcome,
} from "@/shared/lib/agent-state";
import { asPhase } from "@/shared/lib/roles";
import { admitsAgentUpdate, useAgentStateStore } from "@/shared/stores/agent-state-store";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { logger } from "@/shared/lib/logger";
import { useTerminalActivity } from "./use-terminal-activity";

interface TerminalStatusPayload {
  tab_session_id: string;
  status: string;
  /** The Claude hook behind `status`; feeds the agent-state record. */
  hook_event?: string;
  /** `"mcp"` for a write from the MCP explicit-signal bridge (`agent_dot.rs`). */
  source?: string;
  /** With `source: "mcp"`: the Tier-1 input behind `status`. */
  agent_signal?: unknown;
}

function asClaudeHookEvent(e: string | undefined): ClaudeHookEvent | null {
  return e !== undefined && (CLAUDE_HOOK_EVENTS as readonly string[]).includes(e)
    ? (e as ClaudeHookEvent)
    : null;
}

function asSignalDoneOutcome(v: unknown): SignalDoneOutcome | null {
  return SIGNAL_DONE_OUTCOMES.find((o) => o === v) ?? null;
}

/** The MCP bridge's Tier-1 input, or null for anything off-shape. A hook is
 *  never accepted from this path; hooks arrive as `hook_event`. */
function asMcpSignal(payload: TerminalStatusPayload): ExplicitSignal | null {
  if (payload.source !== "mcp") return null;
  const s = payload.agent_signal as Record<string, unknown> | null | undefined;
  switch (s?.kind) {
    case "ask-user":
    case "ask-resolved":
      return { kind: s.kind };
    case "signal-done": {
      const outcome = asSignalDoneOutcome(s.outcome);
      return outcome ? { kind: "signal-done", outcome } : null;
    }
    case "signal-progress": {
      const phase = typeof s.phase === "string" ? asPhase(s.phase) : null;
      return phase ? { kind: "signal-progress", phase } : null;
    }
    default:
      return null;
  }
}

/** `pty-exit`: the session's own process exited, with the status its host reaped. */
interface PtyExitPayload {
  session_id: string;
  exit_code?: number | null;
  signal?: string | null;
  /** CodeSense ended the process (a tab close, a stop). */
  requested?: boolean;
}

/** `pty-osc133`: a shell's semantic prompt marker (`osc_dispatch.rs`). */
interface PtyOsc133Payload {
  session_id: string;
  kind: string;
  /** For `command-end`: the finished foreground command's exit status. */
  exit_code?: number | null;
}

function numberOrNull(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

// The `terminal-status-update` wire accepts every stored (wire) status plus the
// transient `"idle"` signal (which clears status rather than being stored).
// Derived from `TERMINAL_WIRE_STATUSES` so the two never drift.
const VALID_STATUSES = new Set<string>([...TERMINAL_WIRE_STATUSES, "idle"]);

/**
 * Listens for terminal lifecycle events:
 *
 * - `terminal-status-update` — emitted by the HTTP API when Claude Code hooks fire,
 *   and by the MCP explicit-signal bridge (`source: "mcp"`). Each new status
 *   replaces the previous one (live status monitor), and the Tier-1 input behind
 *   it (the hook event, or the MCP `agent_signal`) is folded into the session's
 *   agent-state record. A hook the Rust dot ledger suppresses is never emitted,
 *   so it touches neither.
 *
 * - `pty-exit` — emitted when a terminal process exits, with its exit status.
 *   The exit is recorded for the session. It is folded into the agent record
 *   only while the agent is established with its turn open (the shell died
 *   under it); a finished agent's shell exiting leaves the record alone. The
 *   dot then keeps a crash or an agent exit's known status, else clears as
 *   before.
 *
 * - `pty-osc133` `command-end` — the exit status of a shell's foreground
 *   command. When the session's agent was that command (it runs inside the
 *   tab's shell, so its end is not a `pty-exit`), it is the agent's own exit
 *   code: a nonzero status (`137` for a `kill -9`) resolves `crashed`.
 *
 * While mounted it also has the liveness poller probe every session whose agent
 * turn is open (`watchOpenTurns`), so a plain CLI tab gets the heartbeat too, and
 * every terminal tab for each installed agent (`watchTerminalTabAgents`).
 */
export function useTerminalStatusEvents() {
  useTerminalActivity();
  useEffect(() => {
    // Guard against React StrictMode double-mount: the first mount's cleanup
    // runs synchronously before the listen() promise resolves. The `cancelled`
    // flag ensures the stale listener from the first mount is a no-op.
    let cancelled = false;
    const group = createListenerGroup();
    const unwatchOpenTurns = watchOpenTurns();
    // Every terminal tab gets the dot floor for any installed agent running in it.
    const unwatchTabAgents = watchTerminalTabAgents();

    group.track(
      listen<TerminalStatusPayload>("terminal-status-update", (event) => {
        if (cancelled) return;
        const { tab_session_id, status, hook_event } = event.payload;
        if (!admitsAgentUpdate(tab_session_id)) return;
        logger.debug("http", `terminal-status-update: session=${tab_session_id} status=${status}`);

        const hookEvent = asClaudeHookEvent(hook_event);
        const signal: ExplicitSignal | null = hookEvent
          ? { kind: "claude-hook", event: hookEvent }
          : asMcpSignal(event.payload);
        const wasBlocked =
          useAgentStateStore.getState().bySession[tab_session_id]?.state === "blocked";
        if (signal) {
          useAgentStateStore.getState().recordSignal(tab_session_id, { tier: 1, signal });
        }

        if (!VALID_STATUSES.has(status)) return;

        const store = getViewerState();
        // A sibling tool call's PreToolUse must not clear a pending permission.
        if (
          hookEvent &&
          !hookClearsPermission(hookEvent) &&
          !wasBlocked &&
          store.aiStatusBySession[tab_session_id] === "needs-permission"
        ) {
          return;
        }

        // SW-FX-52: status is keyed by the globally-unique PTY `sessionId` in the
        // flat `aiStatusBySession` slice, so no team gating is needed — the write
        // is O(1) and team-agnostic. Readers only render the active team's tiles,
        // so the observable result is unchanged from the prior active-team scope.
        store.setSessionAiStatus(
          tab_session_id,
          status === "idle" ? undefined : (status as TerminalWireStatus),
        );
      }),
    );

    group.track(
      listen<PtyExitPayload>("pty-exit", (event) => {
        if (cancelled) return;
        const { session_id } = event.payload;
        const store = getViewerState();
        const record = useAgentStateStore.getState().recordPtyExit(session_id, {
          code: numberOrNull(event.payload.exit_code),
          signal: typeof event.payload.signal === "string" ? event.payload.signal : null,
          requested: event.payload.requested === true,
        });
        // A pruned session (its tab closed) is ignored: its dot is already gone.
        if (record === undefined) return;
        store.setSessionAiStatus(session_id, deathDot(record ?? undefined));
        // SW-AM-4: drop a stale in-progress note when the PTY exits, but KEEP a
        // `settled` summary (written by `signal_done`, which fires just before
        // exit) so AC-4's terminal summary survives until the row is torn down.
        const prog = store.progressBySession[session_id];
        if (prog && !prog.settled) store.setSessionProgress(session_id, undefined);
      }),
    );

    group.track(
      listen<PtyOsc133Payload>("pty-osc133", (event) => {
        if (cancelled || event.payload.kind !== "command-end") return;
        const code = numberOrNull(event.payload.exit_code);
        if (code === null) return;
        const { session_id } = event.payload;
        const record = useAgentStateStore.getState().recordCommandEnd(session_id, code);
        if (record === undefined) return;
        getViewerState().setSessionAiStatus(session_id, deathDot(record));
      }),
    );

    return () => {
      cancelled = true;
      unwatchOpenTurns();
      unwatchTabAgents();
      group.dispose();
    };
  }, []);
}
