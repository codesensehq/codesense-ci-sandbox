// SPDX-License-Identifier: MIT
import type { Person } from "@/shared/types/domain";
import { personByLogin, type PeopleByLogin } from "@/shared/lib/people-by-login";

const PERSON_PREFIX = "person:";

export function personStableId(person: Person): string {
  return person.id ?? String(person.github_user_id);
}

export function personReference(person: Person): string {
  return `${PERSON_PREFIX}${personStableId(person)}`;
}

export function personIdFromReference(value: string | null): string | null {
  if (!value?.startsWith(PERSON_PREFIX)) return null;
  const id = value.slice(PERSON_PREFIX.length).trim();
  return id || null;
}

export function buildPeopleById(people: readonly Person[]): ReadonlyMap<string, Person> {
  return new Map(people.map((person) => [personStableId(person), person]));
}

export function resolvePersonReference(
  value: string | null,
  byId: ReadonlyMap<string, Person>,
  byLogin: PeopleByLogin,
): Person | null {
  const id = personIdFromReference(value);
  return id ? (byId.get(id) ?? null) : value ? personByLogin(byLogin, value) : null;
}
