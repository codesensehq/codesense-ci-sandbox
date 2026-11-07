// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import {
  type NeedsYouKind,
  type SessionNeedsYou,
  useNeedsYouList,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { selectSessionHome, useViewerStore } from "@/shared/stores/viewer-store";
import type { SessionProgress, ViewerState, Workspace } from "@/shared/stores/viewer-store";
import { workspaceDisplayLabel } from "@/shared/lib/worktree";

/**
 * An open agent session that is waiting on the user, by the one shared definition
 * (`selectSessionNeedsYou`, anchored to the dot, seen or not): a pending
 * `ask_user` question, a crash, a failure, or a permission prompt. Includes Hidden
 * tabs and hidden agents (`hidden`) and tray-minimized workspaces.
 */
export interface SessionNeedingYou {
  sessionId: string;
  teamKey: string;
  workspaceId: string;
  tabId: string;
  /** Not on screen: a Hidden tab, or a hidden agent. */
  hidden: boolean;
  kind: NeedsYouKind;
  seen: boolean;
  /** The workspace's story id / title (what the HUD tile shows), else its id. */
  label: string;
  /** The terminal tab's name, for telling two sessions of one workspace apart. */
  tabName: string;
  /** Why it waits, in plain words (see {@link waitingReason}). */
  reason: string;
}

/**
 * Why a session waits, in the status marks' words: a question, a crash or an
 * error by name; for a permission, the agent's own blocked note, "Blocked" when it
 * reported blocked without one, else "Needs approval" (a prompt is open).
 */
export function waitingReason(kind: NeedsYouKind, progress: SessionProgress | undefined): string {
  if (kind === "question") return "Needs answer";
  if (kind === "crashed") return "Crashed";
  if (kind === "failed") return "Error";
  if (kind === "attention") return "Needs you";
  if (kind === "startup") return "Waiting on a startup screen";
  if (kind === "permission") return "Needs approval";
  const note = progress?.phase === "blocked" ? progress.note?.trim() : undefined;
  return note ? `Blocked: ${note}` : "Blocked";
}

/** A workspace's name as its tile shows it. */
export function workspaceLabel(ws: Workspace | undefined, workspaceId: string): string {
  // A plain folder workspace is named like its tile, never by its raw `generic:<dir>` id.
  if (ws?.kind === "generic") return workspaceDisplayLabel(ws);
  return ws?.storyId?.trim() || ws?.storyTitle?.trim() || workspaceId;
}

/** Name the canonical needs-you rows for display: workspace label and tab name. */
export function labelSessionsNeedingYou(
  s: ViewerState,
  rows: readonly SessionNeedsYou[],
): SessionNeedingYou[] {
  return rows.map((row) => {
    const home = selectSessionHome(s, row.sessionId);
    return {
      ...row,
      label: workspaceLabel(home?.workspace, row.workspaceId),
      tabName: home?.tab.name ?? row.tabId,
      reason: waitingReason(row.kind, s.progressBySession?.[row.sessionId]),
    };
  });
}

/**
 * The sessions waiting on the user, live, in `teamKey` (or every team when `null`),
 * oldest first — the canonical `selectSessionsNeedingYou` list, labelled.
 */
export function useSessionsNeedingYou(teamKey: string | null): SessionNeedingYou[] {
  const rows = useNeedsYouList(teamKey);
  const key = useViewerStore((s) => (s ? JSON.stringify(labelSessionsNeedingYou(s, rows)) : "[]"));
  return useMemo(() => (key ? (JSON.parse(key) as SessionNeedingYou[]) : []), [key]);
}
