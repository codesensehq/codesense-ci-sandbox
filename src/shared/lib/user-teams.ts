// SPDX-License-Identifier: MIT
import type { Org, Team } from "@/shared/types/domain";

/**
 * The teams visible and selectable to a user within an org.
 *
 * SW-OB-7 opened team access: org access alone grants every team (membership is
 * not a gate — SW-TM-3 removed it entirely). The rule lives here ONCE so every
 * "which teams show" read (identity menu team section, identity menu trigger, org-switcher
 * default pick, cycle-team shortcut) shares one definition and can't drift.
 *
 * SW-OB-9 (local team visibility) narrows it with a per-org, local "shown
 * teams" set (`shownTeamsByOrgSlug`, org-store). The semantics hinge on
 * absent !== empty:
 *   - `shown[org.slug] === undefined` (never curated) => ALL `org.teams`
 *     (non-regressive; identical to SW-OB-7).
 *   - `shown[org.slug] === []`        (curated to none) => NONE.
 *   - `shown[org.slug] === [names]`   => intersection, in ORG order.
 *
 * Kept PURE (the shown-map is passed in, not read from the store) so the two
 * rendering call sites can subscribe to it via a zustand selector and
 * re-render on curation — a `getState()` read here would be invisible to
 * React and leave the nav stale. Imperative call sites pass a `getState()`
 * snapshot.
 *
 * SW-HB-7 (V-1): the org-wide `archived` filter is composed HERE, before the
 * personal shown-set filter. `visibleTeams` is the single funnel every default
 * surface (identity menu team section, identity menu trigger, org-switcher default,
 * cycle-team, Home Base panel, `defaultTeamForOrg`/`resolveRestoredTeam`)
 * already routes through, so folding archived-exclusion in once means NO call
 * site can leak an archived team — the same "one definition, can't drift" rule
 * this function already embodies. The two predicates (`archived` = org truth,
 * shown-set = personal view) stay conceptually distinct — see `activeTeams`.
 */
export function visibleTeams(org: Org, shownByOrg: Record<string, string[]>): Team[] {
  // Archived (org-wide) filter first, then the shown-set (personal) filter.
  const active = activeTeams(org);
  const shown = shownByOrg[org.slug];
  if (shown === undefined) return active;
  return active.filter((t) => shown.includes(t.name));
}

/**
 * SW-HB-7 (V-1): the org's ACTIVE (non-archived) teams — the org-wide pre-filter
 * composed inside `visibleTeams`. Exported as its own pure predicate so it can
 * be unit-tested and reused independently of the personal
 * shown-set. Absent `archived` ⇔ active.
 */
export function activeTeams(org: Org): Team[] {
  return org.teams.filter((t) => !t.archived);
}

/**
 * SW-HB-7 (V-1): the org's ARCHIVED teams — the complement of `activeTeams`.
 * Deliberately org-wide (NOT intersected with any `shownTeamsByOrgSlug`): the
 * Home Base "Archived (N)" disclosure must surface every retired team so it can
 * be unarchived or deleted even when it isn't in this user's shown set.
 */
export function archivedTeams(org: Org): Team[] {
  return org.teams.filter((t) => t.archived);
}

/**
 * The active team within a user's team list: the one matching `activeTeamName`,
 * else the first (array-order primary, the same convention the rest of the app
 * uses for source_repos[0]). Returns `null` only when the list is empty. Pure.
 */
export function resolveActiveTeam(userTeams: Team[], activeTeamName: string | null): Team | null {
  return userTeams.find((t) => t.name === activeTeamName) ?? userTeams[0] ?? null;
}
