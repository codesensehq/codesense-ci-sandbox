// SPDX-License-Identifier: MIT
/**
 * The context repo's seam: one read of the clone on disk (branch, upstream, ahead/behind,
 * an operation git has in progress, conflicts, what the syncing agent last reported), the
 * card's actions, and History's pages of commits. Offline is read from the browser: the clone cannot
 * know it.
 */

import { parseTeamKey } from "@/shared/lib/team-key";
import {
  contextRepoHistory,
  sendInboxAsHuman,
  contextRepoStatus,
  handContextConflictToAgent,
  resolveContextConflict,
  runContextSyncAction,
  type ContextConflictResolution,
  type ContextRepoStatus,
  type ContextSyncAction,
  type HistoryBranches,
  type HistoryPage,
  type HumanSendResult,
  type InboxTeam,
} from "@/tauri/commands";

function isOffline(): boolean {
  return typeof navigator !== "undefined" && navigator.onLine === false;
}

/** The context repo's status, with offline laid over a quiet state. */
export async function readContextRepoStatus(
  orgRoot: string,
  worktree?: string,
): Promise<ContextRepoStatus> {
  const status = await (worktree
    ? contextRepoStatus(orgRoot, worktree)
    : contextRepoStatus(orgRoot));
  if (!worktree && isOffline() && status.sync.state === "ok") {
    return { ...status, sync: { ...status.sync, state: "offline" } };
  }
  return status;
}

export function settleConflict(
  orgRoot: string,
  relPath: string,
  resolution: ContextConflictResolution,
): Promise<void> {
  return resolveContextConflict(orgRoot, relPath, resolution);
}

function inboxTeam(teamKey: string): InboxTeam {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  return { org: orgSlug, team: teamName ?? "" };
}

/** A send the inbox refused reads as a failure, with the inbox's own words. */
async function sent(result: Promise<HumanSendResult>): Promise<void> {
  const r = await result;
  if (r.status === "invalid") throw new Error(r.message);
}

export function handConflict(
  teamKey: string,
  orgRoot: string,
  relPath: string,
  agentName: string,
): Promise<void> {
  return sent(handContextConflictToAgent(inboxTeam(teamKey), orgRoot, relPath, agentName));
}

export function runSyncAction(
  teamKey: string,
  orgRoot: string,
  action: ContextSyncAction,
  agentName: string,
  reason: string | null,
): Promise<void> {
  return sent(runContextSyncAction(inboxTeam(teamKey), orgRoot, action, agentName, reason));
}

/** One page of the context repo's commits, newest first. */
export function readContextHistory(
  orgRoot: string,
  branches: HistoryBranches,
  paths: string[] | null,
  limit: number,
  cursor: string | null,
): Promise<HistoryPage> {
  return contextRepoHistory(orgRoot, branches, paths, limit, cursor);
}

export function handWorktreeChanges(teamKey: string, handle: string, text: string): Promise<void> {
  return sent(sendInboxAsHuman(inboxTeam(teamKey), `@${handle}`, text, "plain", null, null));
}
