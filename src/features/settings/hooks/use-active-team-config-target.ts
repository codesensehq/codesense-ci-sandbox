// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { getActiveTeamKey } from "@/shared/lib/team-key";

/**
 * SW-GT-6 · Resolve the `(orgRoot, team, teamKey)` triple the phase-config
 * editors write against — the ACTIVE org + active team. `team` is `null` (and
 * `orgRoot` may be `""`) until an org/team is selected; the panels render a
 * "pick a team" empty state in that window and never call `saveTeamConfig`.
 *
 * `teamKey` is the canonical key the snapshot stores + refresh nonces are keyed
 * by, so a save can `bump(teamKey)` to re-hydrate exactly this team.
 */
export interface TeamConfigTarget {
  orgRoot: string;
  team: string | null;
  teamKey: string;
}

export function useActiveTeamConfigTarget(): TeamConfigTarget {
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const orgRoot = useOrgStore((s) =>
    s.activeOrgSlug !== null
      ? (s.orgs.find((o) => o.slug === s.activeOrgSlug)?.org_root ?? "")
      : "",
  );
  // `getActiveTeamKey` reads the same `useOrgStore` fields imperatively; the
  // reactive `activeTeamName`/`orgRoot` subscriptions above keep the component
  // re-rendering when the active team changes, so the returned key stays fresh.
  return { orgRoot, team: activeTeamName, teamKey: getActiveTeamKey() };
}
