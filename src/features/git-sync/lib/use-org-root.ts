// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import { useOrgStore } from "@/shared/stores/org-store";

import { deriveOrgRoot } from "@/shared/lib/codesense-home";
import { useContentSource } from "@/shared/hooks/use-content-source";

/**
 * SW-GS-2 — resolve the context-repo org_root (`…/orgs/{org}/codesense-context`)
 * for the repo backing `navRepoPath`, the SAME repo whose pipeline tree the view
 * scanned. A synthetic source-repo workspace's `contextRepoPath` carries a
 * `/_team/{team}/repos/{owner}/{repo}` suffix, which `deriveOrgRoot` strips
 * (no-op when it's already an org root). Returns `null` when unresolved
 * (unhydrated / no repo) so callers short-circuit instead of fetching a wrong
 * or empty root.
 */
export function useOrgRoot(navRepoPath: string): string | null {
  const contextPath = useContentSource(navRepoPath)?.contextRepoPath;
  const registeredRoot = useOrgStore(
    (s) =>
      s.orgs.find(
        (org) => contextPath === org.org_root || contextPath?.startsWith(`${org.org_root}/`),
      )?.org_root,
  );
  return useMemo(
    () => registeredRoot ?? (contextPath ? deriveOrgRoot(contextPath) : null),
    [contextPath, registeredRoot],
  );
}
