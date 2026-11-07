// SPDX-License-Identifier: MIT
import type { Team } from "@/shared/types/domain";

/** Index teams by name for O(1) parent/child lookups. */
function byName(teams: Team[]): Map<string, Team> {
  const index = new Map<string, Team>();
  for (const team of teams) index.set(team.name, team);
  return index;
}

/**
 * `self` + every transitive descendant of `self` — the cycle-exclusion set for
 * the parent picker (AC-2). A team may never inherit from itself, nor from
 * anything that (directly or transitively) inherits from it, or a loop results.
 *
 * Built by inverting `parent_team` across the flat list with its own visited
 * guard, so a pre-existing cyclic `teams.yml` cannot hang the inversion.
 */
export function teamDescendants(teams: Team[], self: string): Set<string> {
  const childrenByParent = new Map<string, string[]>();
  for (const team of teams) {
    const parent = team.parent_team;
    if (parent != null) {
      const siblings = childrenByParent.get(parent);
      if (siblings) siblings.push(team.name);
      else childrenByParent.set(parent, [team.name]);
    }
  }

  const descendants = new Set<string>([self]);
  const stack: string[] = [self];
  while (stack.length > 0) {
    const name = stack.pop()!;
    for (const child of childrenByParent.get(name) ?? []) {
      if (!descendants.has(child)) {
        descendants.add(child);
        stack.push(child);
      }
    }
  }
  return descendants;
}

/** The DIRECT children of a team — teams whose `parent_team` names it. These
 *  are exactly the teams SLW-IN-3's reparent-on-delete re-points (grandchildren
 *  stay put, pointing at their unchanged direct parent), so this drives AC-5's
 *  "N sub-team(s)" disclosure count. */
export function teamChildren(teams: Team[], name: string): string[] {
  return teams.filter((t) => t.parent_team === name).map((t) => t.name);
}

/**
 * The team a deleted team's children should reparent to: the deleted team's own
 * parent (the grandparent), or `null` when the deleted team is a root (its
 * children become roots). MUST match SLW-IN-3's `apply_delete_team` reparent
 * rule so AC-5's disclosure copy can never disagree with the mutation. An
 * unknown team yields `null`.
 */
export function reparentTargetOnDelete(teams: Team[], name: string): string | null {
  const parent = byName(teams).get(name)?.parent_team ?? null;
  // Mirror IN-3's `apply_delete_team` exactly: a self-parented target (only
  // reachable via a hand-corrupted `teams.yml`) reparents its children to a
  // root (`None`), NOT back to itself — `grandparent.filter(|g| g != team_name)`.
  return parent === name ? null : parent;
}
