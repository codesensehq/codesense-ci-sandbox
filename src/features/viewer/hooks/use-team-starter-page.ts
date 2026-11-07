// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";

import {
  landingPagesVersion,
  lookupTeamLandingPage,
  peekTeamLandingPage,
  subscribeLandingPages,
  teamContentRoot,
} from "@/features/viewer/lib/team-landing-page";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org, Team } from "@/shared/types/domain";

export interface ActiveTeamStarterPage {
  org: Org;
  team: Team;
  contentRoot: string;
  /** The page the team opens when nothing is open (declared or by convention),
   *  as spelled on disk; `null` when it has none; `undefined` while it is being
   *  looked up. */
  effectivePath: string | null | undefined;
}

/**
 * The active team's starter page, kept current: it is looked up again when the
 * team's content changes or a registry edit lands (both forget the memoised
 * page, see `invalidateLandingPages`). Read-only — it never opens anything.
 */
export function useActiveTeamStarterPage(owningTeamKey?: string): ActiveTeamStarterPage | null {
  const activeOrg = useActiveOrg();
  const owningOrg = useOrgStore((s) =>
    owningTeamKey
      ? s.orgs.find((org) => org.slug === owningTeamKey.slice(0, owningTeamKey.lastIndexOf(":")))
      : undefined,
  );
  const org = owningTeamKey ? owningOrg : activeOrg;
  const teamName = useOrgStore((s) =>
    owningTeamKey ? owningTeamKey.slice(owningTeamKey.lastIndexOf(":") + 1) : s.activeTeamName,
  );
  const team = org?.teams.find((t) => t.name === teamName) ?? null;
  const version = useSyncExternalStore(subscribeLandingPages, landingPagesVersion);
  const orgRoot = org?.org_root;
  const name = team?.name;
  const declared = team?.landing_page ?? null;
  const key = orgRoot && name ? `${orgRoot}\u0000${name}\u0000${declared ?? ""}` : null;
  const [resolved, setResolved] = useState<{ key: string; path: string | null } | null>(null);

  useEffect(() => {
    if (!key || !orgRoot || !name) return;
    let live = true;
    lookupTeamLandingPage({ org_root: orgRoot }, { name, landing_page: declared })
      .then((r) => {
        if (live) setResolved({ key, path: r.kind === "found" ? r.path : null });
      })
      .catch(() => {
        if (live) setResolved({ key, path: null });
      });
    return () => {
      live = false;
    };
  }, [key, orgRoot, name, declared, version]);

  // A page already looked up (by the nav, an earlier menu) is read at once, so a
  // freshly opened menu never shows a stale "Set" for the default page.
  const peeked =
    orgRoot && name
      ? peekTeamLandingPage({ org_root: orgRoot }, { name, landing_page: declared })
      : undefined;
  const effectivePath =
    resolved?.key === key
      ? resolved.path
      : peeked
        ? peeked.kind === "found"
          ? peeked.path
          : null
        : undefined;
  // Stable while nothing changes, so the nav's row callbacks stay memoised.
  return useMemo(
    () =>
      org && team
        ? { org, team, contentRoot: teamContentRoot(org.org_root, team.name), effectivePath }
        : null,
    [org, team, effectivePath],
  );
}
