// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { detectedAgentCli } from "@/features/viewer/lib/hand-agents";
import { RELEASE_NOT_SUPPORTED } from "@/features/viewer/lib/session-states";
import { StatusMark, type StatusMarkProps } from "@/shared/components/status-mark";
import { activityQualifier } from "@/shared/lib/agent-activity-words";
import { useRemoteServerUpdate } from "./remote-server-update";
import { isQuietMark } from "@/shared/lib/status-mark";
import { sessionEndInfo, type SessionEndInfo } from "@/shared/lib/session-end";
import { type DotScope, useScopeMark } from "../hooks/use-active-workspace-terminal-tabs";
import {
  selectAgentCrashStands,
  selectSessionExit,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  selectTabBySessionId,
  useViewerStore,
  type TerminalTab,
} from "@/shared/stores/viewer-store";

type MarkProps = Omit<StatusMarkProps, "mark" | "label">;

/** {@link sessionEndInfo} for one session, from both stores (or the tab's
 *  own layout exit, when the caller holds its tab). */
export function useSessionEnd(
  sessionId: string,
  tabExit?: TerminalTab["exit"],
): SessionEndInfo | null {
  const exit = useAgentStateStore(selectSessionExit(sessionId));
  useAgentListProjectionStore(
    (s) => s.sessions?.find((row) => row.sessionId === sessionId)?.effectiveStatus,
  );
  const agentCrashed = useAgentStateStore(selectAgentCrashStands(sessionId));
  const layoutExit = useViewerStore((s) => selectTabBySessionId(s, sessionId)?.exit);
  return sessionEndInfo(exit, tabExit ?? layoutExit, agentCrashed);
}

/** Why a session's activity cannot be read right now, in words; null when it can. */
function useActivityFreshness(sessionId: string, hasStatus: boolean): string | null {
  const activity = useAgentStateStore((s) => s.activityBySession[sessionId]);
  // A host reports activity only for agent invocations, so a session it has no
  // snapshot for is usually a plain shell: there is nothing to be stale about
  // unless CodeSense otherwise knows an agent runs there.
  const knownAgent = useAgentStateStore(
    (s) =>
      detectedAgentCli(
        s.bySession[sessionId],
        s.launchBySession[sessionId]?.agent,
        s.processAgentBySession[sessionId],
      ) !== null,
  );
  // A remote tab's server is updated from its machine, not the local panel.
  const remote = useRemoteServerUpdate(sessionId);
  if (!activity?.snapshot && !hasStatus && !knownAgent) return null;
  if (activity?.freshness === "upgrade-required") {
    if (remote) {
      return `The terminal server on ${remote.label} needs an update for agent activity. Use Update terminal server on the tab or in Machines.`;
    }
    return "Terminal server update required for agent activity. Review the update in the terminal panel; existing terminals can keep running.";
  }
  if (activity?.freshness === "recovering") return "Restoring agent activity";
  if (activity?.freshness === "unavailable") {
    return "Agent activity unavailable; last observation may be stale";
  }
  const snapshot = activity?.snapshot;
  if (snapshot?.exit == null && snapshot?.reason === "inbox-update-required") {
    return "Codex is out of date for inbox wake. Update to 0.159.3+; mail stays stored and readable.";
  }
  if (snapshot && (!snapshot.observed || snapshot.limited)) {
    const what = snapshot.observed ? "Limited agent activity reporting" : "Hooks not connected";
    return snapshot.provider === "codex"
      ? `${what}. In Codex, run /hooks to review CodeSense hooks; approval is optional.`
      : what;
  }
  return null;
}

/**
 * One session's status mark, subscribed to just that session, so a tick for
 * another session re-renders nothing here. Its mark is the session's own
 * (`useScopeMark`): a crash or a clean exit once its process has ended, else
 * its live status. During connected-host recovery the shared words qualify the
 * retained mark as restoring/stale. Without an observation it shows Unknown;
 * disconnected hosts show Offline. Renders nothing for a plain shell.
 *
 * With `wordsId`, the state in words (no age) is also rendered hidden under
 * that id, for a host's `aria-labelledby`.
 */
export function SessionStatusMark({
  sessionId,
  exit,
  wordsId,
  ...rest
}: {
  sessionId: string;
  /** The tab's exit as its layout has it, when the caller holds the tab. */
  exit?: TerminalTab["exit"];
  wordsId?: string;
} & MarkProps) {
  const { mark, label, words } = useScopeMark(
    exit ? { members: [{ sessionId, exit }] } : { sessionIds: [sessionId] },
  );
  const ended = mark === "crashed" || mark === "ended";
  const fresh = useActivityFreshness(sessionId, mark !== undefined);
  // A live session whose CLI release is outside the qualified range says so after its status.
  const release = useAgentListProjectionStore(
    (s) => s.sessions?.find((row) => row.sessionId === sessionId)?.releaseQualified === false,
  );
  const qualified = useAgentStateStore((s) => {
    const activity = s.activityBySession[sessionId];
    return activity?.freshness === "upgrade-required"
      ? undefined
      : activityQualifier(activity, mark);
  });
  type Shown = { mark: StatusMarkProps["mark"]; label: string; words: string };
  let shown: Shown | null = null;
  if (mark && (ended || mark === "offline")) {
    shown = { mark, label, words };
  } else if (mark) {
    // Shared scope words already carry recovery qualifiers on every surface.
    shown =
      fresh && !qualified
        ? { mark, label: `${label} · ${fresh}`, words: `${words}, ${fresh}` }
        : { mark, label, words };
  } else if (fresh) {
    shown = {
      mark: "unknown",
      label: `Activity unknown · ${fresh}`,
      words: `Activity unknown, ${fresh}`,
    };
  }
  if (shown && release && !ended) {
    shown = {
      ...shown,
      label: `${shown.label} · ${RELEASE_NOT_SUPPORTED}`,
      words: `${shown.words} · ${RELEASE_NOT_SUPPORTED}`,
    };
  }
  return (
    <>
      {shown && <StatusMark {...rest} mark={shown.mark} label={shown.label} />}
      {wordsId && (
        <span id={wordsId} hidden>
          {shown?.words ?? ""}
        </span>
      )}
    </>
  );
}

/**
 * A status mark over a scope: a session, a workspace's tabs, an epic's
 * workspaces, a tray "+N", a tile's Hidden tabs. Shows the most urgent member's
 * mark and names what it stands for — one session's state and age, or the
 * members by state ("2 need you · 1 working"). `quiet="hide"` renders nothing
 * while every member is quiet (idle, unknown, ended), for badges that only
 * speak when something happens; the default draws the quiet mark (an idle ring
 * when nothing reports).
 */
export function ScopedStatusMark({
  scope,
  context,
  wordsId,
  quiet = "show",
  ...rest
}: {
  scope: DotScope;
  /** What the mark stands for, when its place does not say ("Hidden tiles"). */
  context?: string;
  wordsId?: string;
  quiet?: "show" | "hide";
} & MarkProps) {
  const { mark, label, words } = useScopeMark(scope);
  const hidden = quiet === "hide" && isQuietMark(mark);
  return (
    <>
      {!hidden && (
        <StatusMark
          {...rest}
          mark={mark ?? "ready"}
          label={context ? `${context}: ${label}` : label}
        />
      )}
      {wordsId && (
        <span id={wordsId} hidden>
          {hidden ? "" : words}
        </span>
      )}
    </>
  );
}
