// SPDX-License-Identifier: MIT
import { parseContextId } from "./context-model";
import { parseSyntheticRepoKey } from "./build-synthetic-repo";
import { buildTeamKey } from "./team-key";

/** Team ownership for content surfaces, without inventing a source-repo slug. */
export function contentTeamKey(identity: string): string | null {
  const context = parseContextId(identity);
  if (context?.orgSlug && context.teamName && context.teamName !== "_none") {
    return buildTeamKey(context.orgSlug, context.teamName);
  }
  const source = parseSyntheticRepoKey(identity);
  return source ? buildTeamKey(source.orgSlug, source.team) : null;
}
