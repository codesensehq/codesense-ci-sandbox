// SPDX-License-Identifier: MIT
import { expect } from "vitest";
import type { Person } from "@/shared/types/domain";
import { presentPerson } from "@/shared/lib/person-presentation";

function person(identities: NonNullable<Person["identities"]>): Person {
  return {
    id: "p-1",
    identities,
    github_user_id: 0,
    github_username: null,
    display_name: "Grace Hopper",
    avatar_url: null,
  };
}

describe("presentPerson", () => {
  const labels = new Map([
    ["github", "GitHub"],
    ["gitlab", "GitLab"],
  ]);

  it("marks a handle-less person without disabling them", () => {
    expect(presentPerson(person([]), labels)).toMatchObject({
      label: "Grace Hopper",
      linked: false,
      chips: [],
    });
  });

  it("hides the default-only chip and shows every provider for multiple identities", () => {
    const github = { provider: "github", handle: "grace", provider_user_id: "1" };
    expect(presentPerson(person([github]), labels).chips).toEqual([]);
    expect(
      presentPerson(
        person([github, { provider: "gitlab", handle: "grace-gl", provider_user_id: "9" }]),
        labels,
      ).chips,
    ).toEqual(["GitHub", "GitLab"]);
  });

  it("normalizes provider keys for matching, de-duplication, and labels", () => {
    expect(
      presentPerson(
        person([
          { provider: " GitHub ", handle: "grace" },
          { provider: "GITHUB", handle: "duplicate-provider" },
          { provider: "GitLab", handle: "grace-gl" },
        ]),
        labels,
        "GITHUB",
      ),
    ).toMatchObject({ handle: "grace", chips: ["GitHub", "GitLab"] });
  });
});
