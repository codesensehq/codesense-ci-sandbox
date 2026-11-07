// SPDX-License-Identifier: MIT
import { dismissAgentMark } from "@/features/viewer/lib/cli-workspace/transport/terminal-activity-source";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { teamSessionLaunchRequest } from "@/features/agents/lib/reopen-team-launch";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import {
  useSessionTabName,
  useSessionTabTeamKey,
  useSessionTabWorkspace,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

import { agentCrashSeq } from "@/shared/lib/agent-state";
import {
  realRelaunchDeps,
  relaunchAgentInLiveShell,
  relaunchAgentSession,
  restartRequest,
  type RelaunchDeps,
  type RelaunchResult,
  type RespawnTab,
} from "@/shared/lib/agent-session-launcher";
import {
  selectSessionExit,
  useAgentStateStore,
  type RelaunchState,
} from "@/shared/stores/agent-state-store";
import type { TerminalTab } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { exitWords } from "@/shared/lib/terminal-dot-words";
import type { TerminalEndedBarProps } from "../components/terminal-ended-bar";
import { runRestart, type RestartAction } from "../lib/terminal-restart";

export interface UseTerminalExitOptions {
  sessionId: string;
  /** The tab's exit as its host's layout records it (set once its process
   *  exited; survives a reload, unlike the exit event). */
  exit: TerminalTab["exit"];
  /** Set while the tab's shell is gone. The panel's input and resize gates read it. */
  exitedRef: RefObject<boolean>;
  /** The pane's element: whether it holds keyboard focus when the process ends. */
  containerRef: RefObject<HTMLElement | null>;
  /** Hand keyboard focus to the terminal. */
  focusTerminal: () => void;
  /** Start a new process in this ended tab (`terminal_respawn_tab`). On success
   *  the pane remounts on the new process. */
  respawn: RespawnTab;
  /** End the tab. `dead`: its process is gone, so there is nothing to ask about. */
  endTab: (opts: { dead: boolean }) => void;
  /** The launcher's line-injection seam (real by default). */
  relaunchDeps?: RelaunchDeps;
}

export interface TerminalExitControl {
  /** The tab's shell exited (`pty-exit`). */
  markExited: () => void;
  /** Keyboard focus is headed for this pane (an attach or a reveal): while the
   *  shell is gone it goes to the bar instead. */
  claimFocus: () => void;
  /** Props for the pane's ended bar, or null when there is nothing to show. */
  bar: TerminalEndedBarProps | null;
}

const IDLE: RelaunchState = { busy: false, failure: null };

/**
 * The pane side of a process ending. When the tab's shell exits, the pane shows
 * the ended bar (how it ended; Restart, Close, and Relaunch when CodeSense
 * launched the tab's agent) and forwards nothing to the dead session: a new
 * process starts only from the bar, in the same tab (`terminal_respawn_tab`).
 * An exit CodeSense asked for (End) shows nothing — its host removes the tab.
 * When an agent crashes inside a shell that still runs, the bar offers to
 * relaunch it without taking focus from the live shell.
 *
 * How it ended comes from the agent-state store (AS-6), else from the tab's
 * layout entry (after a reload). The launch spec, a dismissal and a restart or
 * relaunch in flight (or failed) are kept per session in the same store, so
 * they outlive this pane, which remounts on the new process.
 */
export function useTerminalExit({
  sessionId,
  exit: layoutExit,
  exitedRef,
  containerRef,
  focusTerminal,
  respawn,
  endTab,
  relaunchDeps = realRelaunchDeps,
}: UseTerminalExitOptions): TerminalExitControl {
  /** Bumped each time the bar should take keyboard focus (0: it never has). */
  const [ended, setEnded] = useState<{ focusToken: number } | null>(null);
  const exit = useAgentStateStore(selectSessionExit(sessionId));
  // A primitive, so the pane re-renders when an agent crash lands, not on every
  // hook event of the session's agent.
  const crashSeq = useAgentStateStore((s) => agentCrashSeq(s.bySession[sessionId]));
  const launch = useAgentStateStore((s) => s.launchBySession[sessionId]);
  const row = useAgentListProjectionStore((s) =>
    s.sessions?.find((r) => r.sessionId === sessionId),
  );
  const teamKey = useSessionTabTeamKey(sessionId);
  const workspace = useSessionTabWorkspace(sessionId);
  const tabName = useSessionTabName(sessionId);
  const teamLaunch =
    teamKey && workspace
      ? teamSessionLaunchRequest(teamKey, workspace, row, tabName ?? "Agent")
      : null;
  const relaunch = useAgentStateStore((s) => s.relaunchBySession[sessionId]) ?? IDLE;

  const endTabRef = useRef(endTab);
  endTabRef.current = endTab;

  // A focus claim that came before the bar did.
  const focusClaimedRef = useRef(false);

  const markExited = useCallback(() => {
    exitedRef.current = true;
    const active = typeof document === "undefined" ? null : document.activeElement;
    const focusHere =
      focusClaimedRef.current || (!!active && !!containerRef.current?.contains(active));
    focusClaimedRef.current = false;
    setEnded((e) => e ?? { focusToken: focusHere ? 1 : 0 });
  }, [exitedRef, containerRef]);

  const claimFocus = useCallback(() => {
    if (!exitedRef.current) return;
    setEnded((e) => {
      if (e) return { focusToken: e.focusToken + 1 };
      focusClaimedRef.current = true;
      return e;
    });
  }, [exitedRef]);

  // A pane mounted on a tab whose process already ended (a restored tab, a
  // remount, a reload) gets the bar too: that exit came and went before it.
  const exitKnown = exit !== undefined || layoutExit !== undefined;
  useEffect(() => {
    if (exitKnown && !exitedRef.current) markExited();
  }, [exitKnown, exitedRef, markExited]);

  // A restart or relaunch: one at a time per session, shared with the tray.
  const run = useCallback(
    (action: RestartAction, go: () => Promise<RelaunchResult>) => {
      runRestart(sessionId, action, go);
    },
    [sessionId],
  );

  const restart = useCallback(() => {
    if (teamLaunch) {
      openLaunchSheet(teamLaunch);
      return;
    }
    if (!exitedRef.current) return;
    run("Restart", () => respawn(restartRequest(launch?.spec)).then(() => ({ ok: true }) as const));
  }, [teamLaunch, exitedRef, run, respawn, launch]);

  const relaunchAfterExit = useCallback(() => {
    if (teamLaunch) {
      openLaunchSheet(teamLaunch);
      return;
    }
    if (!launch || !exitedRef.current) return;
    run("Relaunch", () => relaunchAgentSession(sessionId, launch.spec, respawn, relaunchDeps));
  }, [teamLaunch, launch, exitedRef, run, sessionId, respawn, relaunchDeps]);

  const relaunchInShell = useCallback(() => {
    if (teamLaunch) {
      openLaunchSheet(teamLaunch);
      return;
    }
    if (!launch) return;
    run("Relaunch", () => relaunchAgentInLiveShell(sessionId, launch.line, relaunchDeps));
    focusTerminal();
  }, [teamLaunch, launch, run, sessionId, relaunchDeps, focusTerminal]);

  const crashStands =
    crashSeq !== null &&
    (!row?.crashId || row.effectiveStatus === "crashed" || row.effectiveStatus === "failed");
  const crashExit =
    !ended && crashStands
      ? useAgentStateStore.getState().bySession[sessionId]?.t2?.signal
      : undefined;
  const agentCrash =
    crashSeq !== null && crashExit?.kind === "exited"
      ? { seq: crashSeq, code: crashExit.code, signal: crashExit.signal ?? null }
      : null;

  // An exit CodeSense asked for: the tab is on its way out, no bar.
  const requested = exit?.requested === true;
  const failure = relaunch.failure;
  const code = exit?.code ?? layoutExit?.code ?? null;
  const signal = exit?.signal ?? layoutExit?.signal ?? null;
  const known = exit !== undefined || layoutExit !== undefined;
  // After a reload only the layout's exit is left: a signal death crashed. An
  // agent crash that still stands crashed the tab too (the tab's marker agrees).
  const crashed = (exit ? exit.outcome === "crashed" : !!layoutExit?.signal) || crashStands;

  let bar: TerminalEndedBarProps | null = null;
  if (ended && !requested) {
    bar = {
      kind: "session",
      outcome: failure || crashed ? "crashed" : "ended",
      failedAction: failure?.action ?? null,
      detail: failure?.reason ?? row?.endReason ?? (known ? exitWords({ code, signal }) : null),
      agent: launch?.agent ?? teamLaunch?.name ?? null,
      focusToken: ended.focusToken,
      busy: relaunch.busy,
      onRestart: restart,
      onRelaunchAgent: launch || teamLaunch ? relaunchAfterExit : undefined,
      onClose: () => endTabRef.current({ dead: true }),
      onEscape: focusTerminal,
    };
  } else if (!ended && failure) {
    // The shell runs, but the agent did not start in it.
    bar = {
      kind: "agent",
      outcome: "crashed",
      failedAction: failure.action,
      detail: failure.reason,
      agent: launch?.agent ?? teamLaunch?.name ?? null,
      focusToken: 0,
      busy: relaunch.busy,
      onRelaunchAgent: launch || teamLaunch ? relaunchInShell : undefined,
      onClose: () => endTabRef.current({ dead: false }),
      onDismiss: () => useAgentStateStore.getState().setRelaunch(sessionId, null),
      onEscape: focusTerminal,
    };
  } else if (agentCrash) {
    bar = {
      kind: "agent",
      outcome: "crashed",
      failedAction: null,
      detail: row?.endReason ?? exitWords(agentCrash),
      agent: launch?.agent ?? teamLaunch?.name ?? null,
      focusToken: 0,
      busy: relaunch.busy,
      onRelaunchAgent: launch || teamLaunch ? relaunchInShell : undefined,
      onClose: () => endTabRef.current({ dead: false }),
      onDismiss: () => {
        if (row?.crashId) void dismissAgentMark(sessionId, row.crashId);
      },
      onEscape: focusTerminal,
    };
  }

  return { markExited, claimFocus, bar };
}
