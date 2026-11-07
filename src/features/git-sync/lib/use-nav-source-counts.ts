// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { readPrivateSourceListing } from "./publish-set-source";
import { parseTeamKey } from "@/shared/lib/team-key";
import { getPublishSet } from "./publish-set-source";
import { useOrgRoot } from "./use-org-root";
import { useTeamContentPrefix } from "./scope-publish-set";

/** Menu counts are read-only: listing a private source must never create its baseline. */
export function useNavSourceCounts(open: boolean, repoPath: string, teamKey: string) {
  const root = useOrgRoot(repoPath);
  const prefix = useTeamContentPrefix(repoPath);
  const [counts, setCounts] = useState<{ key: string; clone?: number; private?: number } | null>(
    null,
  );
  useEffect(() => {
    if (!open) return;
    let live = true;
    getPublishSet(root, undefined, undefined, prefix)
      .then((set) => {
        if (live)
          setCounts((c) => ({
            ...(c?.key === teamKey ? c : {}),
            key: teamKey,
            clone: set.general.count + set.buckets.reduce((n, b) => n + b.count, 0),
          }));
      })
      .catch(() => {});
    readPrivateSourceListing(parseTeamKey(teamKey).orgSlug)
      .then((listing) => {
        const org = listing.management?.orgs.find((o) => o.org === parseTeamKey(teamKey).orgSlug);
        if (live)
          setCounts((c) => ({
            ...(c?.key === teamKey ? c : {}),
            key: teamKey,
            private: org?.tracked ? org.uncommitted : undefined,
          }));
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [open, root, prefix, teamKey]);
  return counts?.key === teamKey ? counts : null;
}
