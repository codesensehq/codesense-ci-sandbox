// SPDX-License-Identifier: MIT
import { splitStoryHandle } from "@/features/dashboard/components/dashboard-launch";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { showsStoryStatus } from "@/shared/lib/workspace-kind-capabilities";
import type { Workspace } from "@/shared/stores/viewer-store";

/**
 * SW-AM-1 (AC-3): the story a background-agent session is attached to — the
 * session→story join the Agents panel renders and SW-AM-2 (open story) / SW-AM-4
 * (live activity) reuse.
 *
 * `teamKey`/`workspaceId` always point at the backing workspace record (live tile
 * or soft-closed/minimized shadow) the story resolved from — so a caller reuses
 * the shipped `changeStoryStatus(teamKey, workspaceId, …)` write path, and
 * `storyFilePath` gives a workspace-independent disk anchor for SW-AM-2.
 *
 * NOTE (follow-up, `../UX-REVIEW.md` durability): resolution is keyed off OPEN
 * workspace records, so a session whose workspace has been fully closed (its
 * soft-closed shadow evicted) resolves to `null` → the row degrades gracefully
 * (AC-6). Re-seating this on the durable pipeline story index
 * (`buildGroupStoryIdIndex`) so it survives workspace eviction is tracked as a
 * SW-AM follow-up; the `(handle, snapshot)` signature stays stable when it lands.
 */
export interface SessionStory {
  /** Frontmatter `story_id` (e.g. "SW-AM-1"). */
  storyId: string;
  title?: string;
  status: PipelineStatus;
  storyFilePath: string;
  teamKey: string;
  workspaceId: string;
}

/** The store slices the resolver reads — passed in as primitives so the resolver
 *  is pure and unit-tested without the store (mirrors `resolveAgentShow`). */
export interface WorkspaceSnapshot {
  workspaces: Record<string, Workspace[]>;
  softClosedWorkspaces: Record<string, Record<string, { workspace: Workspace }>>;
}

/** The `storyId` stem of a STORY session handle, or `null` for a `::`-bearing
 *  branch/team handle or a `/`-bearing epic/path handle. Shares only the `::`/`/`
 *  exclusion with `formatSessionHandle`; resolution is deliberately MORE permissive
 *  than display — a bare `SW-AM-1` (which `formatSessionHandle` renders opaquely)
 *  and a `SW-AM-1#fork` both key on the `SW-AM-1` stem, since they are the same
 *  story. Malformed handles don't occur (the label mint validates upstream). */
export function storyIdFromHandle(handle: string): string | null {
  if (handle.includes("::") || handle.includes("/")) return null;
  return splitStoryHandle(handle).storyId;
}

/** Build a `SessionStory` from a workspace record, or `null` if it can't own a
 *  status chip. Reuses the shared `showsStoryStatus` gate (story kind + `ready` +
 *  non-null `storyStatus`) so the Agents panel shows a chip exactly where the pill
 *  / tile / tray would — one source of truth, no drift. `showsStoryStatus` narrows
 *  `storyStatus` to non-null for us. */
export function sessionStoryOfWorkspace(
  ws: Workspace,
  teamKey: string,
  workspaceId: string,
): SessionStory | null {
  if (!ws.storyId || !ws.storyFilePath || !showsStoryStatus(ws)) return null;
  return {
    storyId: ws.storyId,
    title: ws.storyTitle,
    status: ws.storyStatus,
    storyFilePath: ws.storyFilePath,
    teamKey,
    workspaceId,
  };
}

/**
 * Index every resolvable story (across all teams) by `storyId`, built once per
 * workspace snapshot so a panel of N rows costs O(1) per row instead of
 * re-scanning every workspace per row per render. Live tiles are inserted before
 * soft-closed shadows and win ties (`!index.has`), so a minimized shadow never
 * shadows the live record of the same story.
 *
 * `preferTeamKey` (the panel's host team) is visited first WITHIN each liveness
 * tier, so among records of the SAME tier the host team wins a same-`storyId` tie.
 * Liveness still outranks team: a live tile in another team beats a host-team
 * soft-closed shadow (the whole live pass precedes the shadow pass), which is the
 * intended "live is more authoritative than minimized" precedence.
 */
export function buildSessionStoryIndex(
  snapshot: WorkspaceSnapshot,
  preferTeamKey?: string | null,
): Map<string, SessionStory> {
  const index = new Map<string, SessionStory>();
  const consider = (ws: Workspace, teamKey: string, workspaceId: string) => {
    const story = sessionStoryOfWorkspace(ws, teamKey, workspaceId);
    if (story && !index.has(story.storyId)) index.set(story.storyId, story);
  };
  const hostFirst = (teamKeys: string[]) =>
    preferTeamKey && teamKeys.includes(preferTeamKey)
      ? [preferTeamKey, ...teamKeys.filter((t) => t !== preferTeamKey)]
      : teamKeys;
  for (const teamKey of hostFirst(Object.keys(snapshot.workspaces))) {
    for (const ws of snapshot.workspaces[teamKey]) consider(ws, teamKey, ws.id);
  }
  for (const teamKey of hostFirst(Object.keys(snapshot.softClosedWorkspaces))) {
    for (const [workspaceId, { workspace }] of Object.entries(
      snapshot.softClosedWorkspaces[teamKey],
    )) {
      consider(workspace, teamKey, workspaceId);
    }
  }
  return index;
}

/** Resolve a session `handle` against a prebuilt index (the per-row hot path). */
export function sessionStoryFromIndex(
  handle: string,
  index: Map<string, SessionStory>,
): SessionStory | null {
  const storyId = storyIdFromHandle(handle);
  return storyId ? (index.get(storyId) ?? null) : null;
}
