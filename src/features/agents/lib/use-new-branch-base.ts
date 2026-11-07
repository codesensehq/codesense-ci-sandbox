// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { AGENTS_LAUNCHER } from "./agent-launch";

/** Look up the branch a new worktree in `repo` (`owner/repo`) would start from, once the
 *  typing settles. `null` while there is no repo, no local clone, or the lookup fails. */
export function useNewBranchBase(repo: string): string | null {
  const [base, setBase] = useState<{ repo: string; branch: string | null } | null>(null);
  const slug = repo.trim();
  useEffect(() => {
    if (!/^[^/\s]+\/[^/\s]+$/.test(slug)) return;
    let live = true;
    const timer = setTimeout(() => {
      void import("@/features/dashboard/components/launch-open-real")
        .then((m) => m.resolveNewBranchBase(AGENTS_LAUNCHER, slug))
        .catch(() => null)
        .then((branch) => {
          if (live) setBase({ repo: slug, branch });
        });
    }, 300);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [slug]);
  return base?.repo === slug ? base.branch : null;
}
