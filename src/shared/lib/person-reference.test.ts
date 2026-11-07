// SPDX-License-Identifier: MIT
import { expect } from "vitest";
import type { Person } from "@/shared/types/domain";
import { buildPeopleByLogin } from "@/shared/lib/people-by-login";
import {
  buildPeopleById,
  personIdFromReference,
  personReference,
  resolvePersonReference,
} from "@/shared/lib/person-reference";

const people: Person[] = [
  {
    id: "p-1",
    identities: [],
    github_user_id: 0,
    github_username: null,
    display_name: "Grace",
    avatar_url: null,
  },
  {
    id: "2",
    identities: [{ provider: "github", handle: "octocat", provider_user_id: "2" }],
    github_user_id: 2,
    github_username: "octocat",
    display_name: "Octo Cat",
    avatar_url: null,
  },
];

describe("person reference", () => {
  it("round-trips a stable person id", () => {
    expect(personReference(people[0])).toBe("person:p-1");
    expect(personIdFromReference("person:p-1")).toBe("p-1");
  });

  it("resolves canonical ids before legacy logins", () => {
    const byId = buildPeopleById(people);
    const byLogin = buildPeopleByLogin(people);
    expect(resolvePersonReference("person:p-1", byId, byLogin)).toBe(people[0]);
    expect(resolvePersonReference("OCTOCAT", byId, byLogin)).toBe(people[1]);
    expect(resolvePersonReference("person:missing", byId, byLogin)).toBeNull();
  });
});
