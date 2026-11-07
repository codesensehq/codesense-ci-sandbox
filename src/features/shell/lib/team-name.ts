// SPDX-License-Identifier: MIT
/** Team names are lowercase slugs: first char alnum, rest alnum or hyphen.
 *  Mirrors the Rust `Team::new` / `is_slug_safe` rule so the frontend gives
 *  instant feedback and never lets the user submit something the backend
 *  would reject. The backend remains authoritative. */
const TEAM_NAME_RE = /^[a-z0-9][a-z0-9-]*$/;
const TEAM_NAME_MAX = 40;

/** Inline validation for the team-name field. Returns an error message for a
 *  NON-EMPTY invalid name, or `null` when the name is empty (submit stays
 *  disabled, no error shown yet) or valid. `existingNames` are the current
 *  org's team names, for the duplicate check. */
export function teamNameError(raw: string, existingNames: string[]): string | null {
  const name = raw.trim();
  if (!name) return null;
  if (!TEAM_NAME_RE.test(name)) {
    return "Use lowercase letters, numbers, and hyphens (e.g. platform).";
  }
  if (name.length > TEAM_NAME_MAX) {
    return `Team name must be ${TEAM_NAME_MAX} characters or fewer.`;
  }
  if (name.endsWith("-meta")) {
    return "Team names can’t end with “-meta” (reserved).";
  }
  if (existingNames.includes(name)) {
    return "A team with that name already exists.";
  }
  return null;
}
