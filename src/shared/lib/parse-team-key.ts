// SPDX-License-Identifier: MIT
/** Dependency-free leaf, so hooks on hot render paths parse a teamKey without importing
 *  `team-key` (and its store imports). Re-exported from `team-key`. */
/** Parse a teamKey back to its `(orgSlug, teamName)` components. Splits at
 *  the FIRST colon so the orgSlug is unambiguous even if a team name
 *  itself contains a colon. The `_none` teamName segment maps back to
 *  `null`. */
export function parseTeamKey(key: string): { orgSlug: string; teamName: string | null } {
  const idx = key.indexOf(":");
  if (idx === -1) {
    // Malformed input: treat the whole string as orgSlug, null team.
    return { orgSlug: key, teamName: null };
  }
  const orgSlug = key.slice(0, idx);
  const tail = key.slice(idx + 1);
  return { orgSlug, teamName: tail === "_none" ? null : tail };
}
