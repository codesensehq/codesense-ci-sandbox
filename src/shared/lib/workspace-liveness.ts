// SPDX-License-Identifier: MIT
import type { TerminalTab, TerminalTabIndex, Workspace } from "@/shared/stores/viewer-store";

/**
 * SW-GW-3 — the open-state oracle for a story's workspace, resolved from the
 * viewer-store open list (NOT the dashboard-launch registry).
 *
 * The origin bug: a story worktree opened through the normal CodeSense viewer UI
 * never enters `DashboardLaunchRegistry` (only dashboard launches `begin`/`bind`;
 * only epic opens `dashboard_register_session`). So the launch registry can't
 * answer "is this story open?" for the exact case the OBSERVE half exists for.
 * The `workspaces[teamKey]` open list IS launch-source-agnostic, so we resolve
 * against it: a story is open iff a `kind:"story"` workspace with a matching
 * `storyId` exists in ANY team bucket. `sessionId` is that workspace's first
 * terminal tab (the one a liveness probe checks); `null` when it has no tab yet.
 *
 * Pure — takes the two store slices, so it is unit-testable with no store/React.
 * Liveness itself is NOT decided here (a tab existing is NOT liveness — the
 * SW-DX-17a rule): the caller probes the returned `sessionId` with the real
 * `try_wait` backend.
 *
 * `preferTeamKey` disambiguates a cross-org collision: a `story_id` is unique
 * WITHIN an org (validate_story blocks a duplicate), but the viewer can hold two
 * orgs at once, each with a same-id story open. The launch side (`resolveStoryPath`)
 * resolves org-scoped in the ACTIVE org; passing the active teamKey here makes the
 * observe side agree — an exact-team match wins over a foreign one, so both halves
 * resolve the same session. Absent/no preferred match → the first insertion-order
 * match (unchanged single-team behavior). Synchronous — no await-separated ambient
 * read, so it is not the SW-GW-2 active-team race.
 */
export function findOpenStoryWorkspace(
  workspaces: Record<string, Workspace[]>,
  tabIndex: TerminalTabIndex,
  storyId: string,
  preferTeamKey?: string | null,
): { teamKey: string; workspaceId: string; sessionId: string | null } | null {
  if (!storyId) return null;
  let firstMatch: { teamKey: string; workspaceId: string; sessionId: string | null } | null = null;
  for (const [teamKey, teamWs] of Object.entries(workspaces)) {
    for (const ws of teamWs) {
      if (ws.kind === "story" && ws.storyId === storyId) {
        const sessionId = pickStorySessionId(tabIndex[teamKey]?.[ws.id]);
        const hit = { teamKey, workspaceId: ws.id, sessionId };
        if (preferTeamKey && teamKey === preferTeamKey) return hit; // exact team wins
        firstMatch ??= hit;
      }
    }
  }
  return firstMatch;
}

/**
 * Pick the session id a story workspace's status/liveness/drill should target.
 *
 * Prefer the story's AGENT tab (role-tagged — `story-dev:<id>`) over the plain
 * shell at `[0]`. The agent is the session that calls `signal_done` AND whose
 * child exit means "work stopped", so the liveness probe, the story-scoped
 * subscription, AND the story→agent drill (SW-AV-16) must all target it — NOT the
 * shell, which stays alive while the panel is open (a false `live:true`) and never
 * carries the agent's status delta. Fall back to the first tab only when there is
 * no agent (a manual open with just a shell — the shell being open is the honest
 * "live" answer, and there is nothing else to reach).
 *
 * SW-GW-6: a story can host N sessions in one bucket — the MAIN session (role
 * `story-dev:<id>`) plus NAMED ones (`story-dev:<id>#<label>`). The story-keyed
 * read tracks the MAIN session, so prefer a role tag WITHOUT the `#` separator;
 * only if there is no main tab (e.g. only named sessions are open) fall back to any
 * role-tagged tab, then the bare shell. Deterministic instead of latching onto
 * whichever named session happens to sort first.
 */
function pickStorySessionId(tabs: TerminalTab[] | undefined): string | null {
  return (
    (tabs?.find((t) => t.role && !t.role.includes("#")) ?? tabs?.find((t) => t.role) ?? tabs?.[0])
      ?.sessionId ?? null
  );
}
