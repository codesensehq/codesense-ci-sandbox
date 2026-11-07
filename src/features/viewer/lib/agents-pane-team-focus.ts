// SPDX-License-Identifier: MIT
/**
 * A team header's funnel: show only that team by hiding every other one in the pane's "By team"
 * hide list, the same list the filter menu's Teams checklist edits. Whether the funnel is on is
 * read from that list, never stored beside it, so the funnel and the menu cannot disagree; what
 * was hidden before the funnel is kept so a second click puts exactly that back.
 */

/** What the pane had hidden when a team's funnel went on, so turning it off restores it. */
export interface TeamFocusRestore {
  teamKey: string;
  hidden: readonly string[];
}

/** The team the hide list narrows the pane to: of `shownTeams` (the teams with agents on the
 *  pane), the one left showing when there are at least two. Null when more than one shows, or none.
 *  Teams with no agents yet are not counted, as the "hidden" chip does not count them. */
export function focusedTeam(
  shownTeams: readonly string[],
  hidden: readonly string[],
): string | null {
  if (shownTeams.length < 2) return null;
  const shown = shownTeams.filter((key) => !hidden.includes(key));
  return shown.length === 1 ? shown[0]! : null;
}

/** The teams a funnel reads and hides: `shownTeams`, the teams with agents on the pane (whether it
 *  is on), and `allTeams`, every team the Teams checklist lists (what it hides). */
export interface FunnelTeams {
  shownTeams: readonly string[];
  allTeams: readonly string[];
}

/** The hide list and restore point a click on `teamKey`'s funnel makes. On: every other team
 *  hidden (open teams with no agents yet too, so none appears under it), keeping what was hidden
 *  before. Off (it was the focused team): the list as it was before the funnel, or every team shown
 *  when that is not known (the checklist narrowed the pane to it). */
export function toggleTeamFocus(
  teamKey: string,
  { shownTeams, allTeams }: FunnelTeams,
  hidden: readonly string[],
  restore: TeamFocusRestore | null,
): { hidden: string[]; restore: TeamFocusRestore | null } {
  if (focusedTeam(shownTeams, hidden) === teamKey) {
    const before = restore?.teamKey === teamKey ? restore.hidden : [];
    return { hidden: before.filter((key) => key !== teamKey), restore: null };
  }
  // Teams hidden that the checklist no longer lists stay hidden, as they were.
  const others = [...new Set([...allTeams, ...shownTeams])].filter((key) => key !== teamKey);
  const kept = hidden.filter((key) => key !== teamKey && !others.includes(key));
  return {
    hidden: [...kept, ...others],
    restore: { teamKey, hidden: [...hidden] },
  };
}
