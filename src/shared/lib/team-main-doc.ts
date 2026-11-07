// SPDX-License-Identifier: MIT
import { teamAllContextId } from "./context-model";
import { parseTeamKey } from "./team-key";
import { parseSyntheticRepoKey } from "./build-synthetic-repo";
import { normalizeMainDoc } from "./pipeline-graph/normalize";
import { setRepoOverrides, type WorkspaceConfig } from "./workspace-config";

export type TeamMainDocPreference =
  | { kind: "ready"; mainDoc: string }
  | { kind: "conflict"; mainDoc: ""; candidates: string[] }
  | { kind: "unavailable"; mainDoc: ""; error: string };

/** One-time compatibility belongs to the qualified team, never its current source list. */
export function resolveTeamMainDocPreference(teamKey: string, config: WorkspaceConfig) {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (!orgSlug || !teamName) throw new Error("Choose an available team.");
  const id = teamAllContextId(orgSlug, teamName);
  const explicit = config.repos[id]?.mainDoc;
  if (explicit !== undefined)
    return {
      config,
      preference: { kind: "ready", mainDoc: normalizeMainDoc(explicit) } as TeamMainDocPreference,
    };
  const candidates = new Set<string>();
  for (const [key, override] of Object.entries(config.repos)) {
    const owner = parseSyntheticRepoKey(key);
    if (owner?.orgSlug !== orgSlug || owner.team !== teamName || !override.mainDoc) continue;
    try {
      const value = normalizeMainDoc(override.mainDoc);
      if (value) candidates.add(value);
    } catch {
      /* An invalid legacy convention cannot become a team choice. */
    }
  }
  if (candidates.size > 1)
    return {
      config,
      preference: {
        kind: "conflict",
        mainDoc: "",
        candidates: [...candidates].sort(),
      } as TeamMainDocPreference,
    };
  const mainDoc = [...candidates][0] ?? "";
  return {
    config: setRepoOverrides(config, id, { mainDoc }),
    preference: { kind: "ready", mainDoc } as TeamMainDocPreference,
  };
}
