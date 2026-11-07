// SPDX-License-Identifier: MIT
import { useConfigStore } from "@/shared/stores/config-store";
import { useMemo } from "react";
import { parseContextId } from "@/shared/lib/context-model";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useResolvedRepo } from "@/shared/lib/use-resolved-repo";
import { useOrgStore } from "@/shared/stores/org-store";
import { resolveTeamContentContext, type ContentSource } from "@/shared/lib/team-content-context";

export function useContentSource(identity: string | null): ContentSource | undefined {
  const parsed = useMemo(() => parseContextId(identity ?? ""), [identity]);
  const org = useOrgStore((state) =>
    parsed?.kind === "team-all"
      ? state.orgs.find((item) => item.slug === parsed.orgSlug)
      : undefined,
  );
  const loaded = useOrgStore((state) => parsed?.kind === "team-all" && state.discoveryComplete);
  const teamKey =
    parsed?.orgSlug && parsed.teamName ? buildTeamKey(parsed.orgSlug, parsed.teamName) : "";
  const mainDoc = useConfigStore((state) =>
    parsed?.kind === "team-all" ? (state.teamMainDocPreferences?.[teamKey]?.mainDoc ?? "") : "",
  );
  const source = useResolvedRepo(parsed?.kind === "team-all" ? null : identity);
  return useMemo(() => {
    if (parsed?.kind !== "team-all") return source;
    const resolved = resolveTeamContentContext(
      buildTeamKey(parsed.orgSlug ?? "", parsed.teamName ?? null),
      org ? [org] : [],
      loaded,
      mainDoc,
    );
    return resolved.kind === "ready" ? resolved.source : undefined;
  }, [parsed, org, loaded, source, mainDoc]);
}
