// SPDX-License-Identifier: MIT
import { useConfigStore } from "@/shared/stores/config-store";
import { parseContextId } from "./context-model";
import { buildTeamKey } from "./team-key";
import { resolveTeamContentContext, type ContentSource } from "./team-content-context";
import { resolveRepo } from "./resolve-repo";
import { useOrgStore } from "@/shared/stores/org-store";

/** Compatibility boundary for content surfaces that can also browse a source scope. */
export function resolveContentSource(identity: string): ContentSource | undefined {
  const context = parseContextId(identity);
  if (context?.kind === "team-all") {
    const state = useOrgStore.getState();
    const resolved = resolveTeamContentContext(
      buildTeamKey(context.orgSlug ?? "", context.teamName ?? null),
      state.orgs,
      state.discoveryComplete,
      useConfigStore.getState().teamMainDocPreferences[
        buildTeamKey(context.orgSlug ?? "", context.teamName ?? null)
      ]?.mainDoc ?? "",
    );
    return resolved.kind === "ready" ? resolved.source : undefined;
  }
  return resolveRepo(identity);
}
