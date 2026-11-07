// SPDX-License-Identifier: MIT
import { switchContextToOrg } from "@/features/viewer/lib/context-switch";
import { useCallback, useMemo } from "react";

import { ContextMoveController } from "@/features/context-mobility/context-move-controller";
import { useTargetFacts } from "@/features/context-mobility/context-target-facts";
import { buildTargetPicker } from "@/features/context-mobility/target-picker";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import type { PendingCrossOrgOp } from "@/features/context-mobility/cross-org-resume";
import { teamAllContext } from "@/shared/lib/context-model";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useScopeActive } from "@/shared/lib/warm-scopes";
import { useUiStore } from "@/shared/stores/ui-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { basename } from "@/shared/lib/basename";

/**
 * SLW-PT-18 — the thin LIVE-WIRE layer for the content-mobility epic.
 *
 * The SLW-PT-9..17 stories built ready-to-mount components (`ContextMoveController`,
 * the sheets) but deferred the live mounting to this story. This host owns
 * exactly the integration glue — the SOURCE context (the team's own content,
 * which the nav browses: SW-XS-15) and the app-shell seams (org switch, create
 * team) — and renders
 * the merged components unchanged. It reimplements none of their behavior.
 */

/** The per-row move/copy affordance renderer, shared across every content file
 *  row in the nav. Returns `null` when the row can't carry the action (no user /
 *  no source context yet), so a row degrades cleanly. */
export type MoveActionSlotRenderer = (filePath: string, fileName: string) => React.ReactNode;

/**
 * The nav-scoped move/copy host. Builds ONE stable per-row renderer wired to the
 * team's content ("All of <Team>") as the SOURCE and the app-shell seams, so
 * each content file row can mount the ⋯ "Move to… / Copy to…" action without
 * re-deriving the source or the seams per row.
 */
export function useContextMoveRowSlot(
  teamKey: string,
  refreshRepoPath?: string,
): MoveActionSlotRenderer {
  // SW-XS-15: the nav always browses the team's own content (the pivot picks
  // whose context it overlays, never a tier), so that is every row's source.
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const selectedContext = useMemo(
    () => teamAllContext(orgSlug, teamName ?? ""),
    [orgSlug, teamName],
  );
  const orgs = useOrgStore((s) => s.orgs);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const setManageTeamsIntent = useUiStore((s) => s.setManageTeamsIntent);
  const user = useCurrentUser();
  const userId = user?.github_user_id ?? null;

  // SLW-PT-B6: pre-list the collision facts for the SAME targets the picker
  // offers — every selectable same-org target across EVERY team in the org, not
  // just the active team's own contexts. The active team's own enumeration
  // never listed a sibling team, so a cross-team
  // target's content was never listed: `targetCollision` read an unlisted set →
  // false, `planMove` omitted the collision step, and the §P Keep-both/Replace/
  // Cancel chooser was silently skipped (risking overwrite). `buildTargetPicker`
  // enumerates the cross-team same-org rows the picker actually lets the user
  // pick, so feeding its `sameOrg` contexts here lists every reachable target's
  // content root and `targetCollision`/`targetHas` resolve correctly.
  // The host's own org, not the global one: a warm team of another org keeps its own targets.
  const targets = useMemo(
    () =>
      buildTargetPicker({
        source: selectedContext,
        activeOrgSlug: orgSlug,
        orgs,
      }).sameOrg.map((r) => r.context),
    [orgSlug, orgs, selectedContext],
  );
  // SV-PBF-267: target roots are listed lazily, once a picker asks about a target,
  // and re-listed only when the index publishes a newer generation. Mounting this
  // host (or an fs bump) lists nothing.
  const facts = useTargetFacts(targets, useScopeActive(teamKey));

  // Bump the nav/file refresh after a completed move/copy/remove so the moved or
  // removed doc updates in place (the companion gap: the host passed no onMoved,
  // so the real nav never refreshed).
  const bumpRefresh = useCallback(() => {
    if (refreshRepoPath) {
      useFileRefreshStore.getState().bumpFileRefresh(refreshRepoPath);
    }
  }, [refreshRepoPath]);

  const onSwitchOrg = useCallback((pending: PendingCrossOrgOp) => {
    // §K carry-switch-resume: hand the target org to the shell; the controller
    // re-presents the picker in resume mode once the switch lands.
    switchContextToOrg(pending.targetOrgSlug, "nav");
  }, []);

  const onCreateTeam = useCallback(() => {
    // §E zero-teams "Move to Shared" → open the team-management surface focused
    // on the new-team field (the real create-team opener).
    setManageTeamsIntent("create");
  }, [setManageTeamsIntent]);

  return useCallback(
    (filePath: string, fileName: string): React.ReactNode => {
      if (userId === null) return null;
      // The item's content-root-relative logical name — best-effort from the
      // row's display name (the nav row carries a filesystem path, not the
      // content-root address the ops key on, so we key on the file's own name).
      const logicalName = fileName || basename(filePath);
      // The first click in the row (the ⋯ trigger, then its menu and picker) lists the
      // targets, once each; a mounted row lists nothing.
      return (
        <span className="contents" onClickCapture={facts.prefetch}>
          <ContextMoveController
            source={selectedContext}
            logicalName={logicalName}
            fileName={fileName}
            activeOrgSlug={activeOrgSlug ?? ""}
            orgs={orgs}
            userId={userId}
            targetCollision={(target) => facts.targetCollision(target, logicalName)}
            targetHas={(target, candidate) => facts.targetHas(target, candidate)}
            onSwitchOrg={onSwitchOrg}
            onCreateTeam={onCreateTeam}
            onMoved={bumpRefresh}
            onRemoved={bumpRefresh}
          />
        </span>
      );
    },
    [selectedContext, activeOrgSlug, orgs, userId, facts, bumpRefresh, onSwitchOrg, onCreateTeam],
  );
}
