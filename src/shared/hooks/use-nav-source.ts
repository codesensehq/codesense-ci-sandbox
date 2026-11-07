// SPDX-License-Identifier: MIT
import { useOrgRoot } from "@/features/git-sync/lib/use-org-root";
import { useEffect, useMemo, useRef, useState } from "react";
import { resolvePrivateNavSource, type PrivateNavSource } from "@/tauri/commands";
import { useContentSource } from "./use-content-source";
import { resolveNavSource } from "@/shared/lib/nav-source";
import { DEFAULT_NAV_LENS } from "@/shared/lib/nav-lens";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";

export function useNavSource(teamKey: string, navRepoPath: string) {
  const source = useViewerStore((s) => s.navLens[teamKey]?.source ?? DEFAULT_NAV_LENS.source);
  const clone = useContentSource(navRepoPath);
  const cloneRoot = useOrgRoot(navRepoPath);
  const profile = useLocalProfileStore((s) => s.status?.active?.principal);
  const initialized = useLocalProfileStore((s) => s.initialized);
  const orgs = useOrgStore((s) => s.orgs);
  const discovered = useOrgStore((s) => s.discoveryComplete);
  const { orgSlug } = parseTeamKey(teamKey);
  const [resolved, setResolved] = useState<{ key: string; value: PrivateNavSource } | null>(null);
  const [note, setNote] = useState("");
  const [retry, setRetry] = useState(0);
  const key = `${profile}:${orgSlug}:${retry}`;
  const fallingBack = useRef(false);
  useEffect(() => {
    if (fallingBack.current) fallingBack.current = false;
    else setNote("");
    if (source.kind !== "private") return;
    let live = true;
    const fallback = (message: string) => {
      if (!live) return;
      fallingBack.current = true;
      useViewerStore.getState().setNavLens(teamKey, { source: { kind: "clone" } });
      setNote(message);
    };
    if (initialized && !profile) {
      fallback("Private isn't available: you're not signed in to a profile. Showing your clone.");
    } else if (discovered && !orgs.some((o) => o.slug === orgSlug)) {
      fallback("Couldn't find your private repo, so this shows your clone.");
    } else {
      resolvePrivateNavSource(orgSlug).then(
        (value) => {
          if (live) setResolved({ key, value });
        },
        () => fallback("Couldn't find your private repo, so this shows your clone."),
      );
    }
    return () => {
      live = false;
    };
  }, [source, teamKey, key, initialized, profile, discovered, orgs, orgSlug]);
  const privateSource = resolved?.key === key ? resolved.value : null;
  const resolution = useMemo(
    () => resolveNavSource(source, clone, privateSource, cloneRoot),
    [source, clone, privateSource, cloneRoot],
  );
  return {
    ...resolution,
    cloneRoot,
    source,
    privateSource,
    note,
    retryBaseline: () => setRetry((n) => n + 1),
  };
}
export type ResolvedNavSource = ReturnType<typeof useNavSource>;
