// SPDX-License-Identifier: MIT
import { useConfigStore } from "@/shared/stores/config-store";
import { logger } from "@/shared/lib/logger";
import { useEffect, useMemo } from "react";
import { useLoadingDeadline } from "./use-loading-deadline";
import { useOrgStore } from "@/shared/stores/org-store";
import { parseTeamKey } from "@/shared/lib/team-key";
import { resolveTeamContentContext } from "@/shared/lib/team-content-context";

export function useTeamContentContext(teamKey: string) {
  const { orgSlug } = parseTeamKey(teamKey);
  const org = useOrgStore((state) => state.orgs.find((item) => item.slug === orgSlug));
  const loaded = useOrgStore((state) => state.discoveryComplete);
  const attempt = useOrgStore((state) => state.discoveryAttempt);
  const preference = useConfigStore((state) => state.teamMainDocPreferences?.[teamKey]);
  const mainDoc = preference?.mainDoc ?? "";
  const hasPreference = preference !== undefined;
  const resolved = useMemo(
    () => resolveTeamContentContext(teamKey, org ? [org] : [], loaded, mainDoc),
    [teamKey, org, loaded, mainDoc],
  );
  const expired = useLoadingDeadline(resolved.kind === "loading", `${teamKey}:${attempt}`);
  const root = !expired && resolved.kind === "ready" ? resolved.source.teamContentPath : "";
  useEffect(() => {
    if (!root || hasPreference) return;
    void useConfigStore
      .getState()
      .ensureTeamMainDocPreference(teamKey)
      .catch((error) =>
        logger.warn("team-context", `Main doc preference unavailable: ${String(error)}`),
      );
  }, [teamKey, root, hasPreference]);
  return expired
    ? {
        kind: "unavailable" as const,
        error: "Loading team context timed out. Choose a team or refresh the workspace list.",
      }
    : resolved;
}
