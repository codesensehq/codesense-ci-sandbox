// SPDX-License-Identifier: MIT
import type { Person } from "@/shared/types/domain";
import { usernameKey } from "@/shared/lib/github-username";
import { logger } from "@/shared/lib/logger";

/**
 * SW-ID-1: the login → Person index. `people.yml` keys by numeric
 * `github_user_id`, but `assigned_to` carries a bare login, so the epic's
 * assignee surfaces (SW-ID-5 picker, SW-ID-6 avatars/filter) need to resolve a
 * login to its stable `Person` (id + display_name + avatar_url). This is the
 * ONE seam they consume — no linear scans over `org.people` at call sites.
 *
 * The by-login twin of the Rust `Org::person_by_username`; the two share the
 * same case-insensitive, first-wins contract.
 */
export type PeopleByLogin = Map<string, Person>;

/**
 * Build the login index from a roster. Keyed by the case-folded login
 * (`usernameKey`), so lookups are case-insensitive. **First-wins** on a
 * duplicate case-folded login — `people.yml` enforces uniqueness on id, not
 * login, so a collision is possible; keeping the first entry makes the index
 * deterministic in roster order and warns once so the duplicate is visible.
 */
export function buildPeopleByLogin(people: Person[]): PeopleByLogin {
  const index: PeopleByLogin = new Map();
  for (const person of people) {
    // SW-FG-4b (AC-7): a handle-less GitHub teammate has a null login — present/
    // assignable by id but not correlatable, so they never enter the login index.
    // Guarding here also keeps every index consumer (e.g. the assignee picker)
    // null-safe without its own guard — `usernameKey` throws on a null argument.
    if (person.github_username == null) continue;
    const key = usernameKey(person.github_username);
    // Skip a blank key (a malformed roster entry) so it can't shadow real
    // lookups — `usernameKey("")` is "", which no real login normalizes to.
    if (key === "") continue;
    const existing = index.get(key);
    if (existing !== undefined) {
      // Include the raw stored usernames (not just the folded key + ids) so a
      // maintainer can grep people.yml for the literal colliding entries.
      logger.warn(
        "people-by-login",
        `duplicate login "${key}" — keeping "${existing.github_username}" (id ${existing.github_user_id}), ignoring "${person.github_username}" (id ${person.github_user_id})`,
      );
      continue;
    }
    index.set(key, person);
  }
  return index;
}

/**
 * Resolve a login to its `Person`, or `null` when the roster has no match —
 * the caller falls back to initials. Normalizes the lookup the same way the
 * index was built (`@`-strip + trim + case-fold via `usernameKey`), so a
 * user-typed `@Login` and a stored `login` resolve identically.
 */
export function personByLogin(index: PeopleByLogin, login: string): Person | null {
  return index.get(usernameKey(login)) ?? null;
}
