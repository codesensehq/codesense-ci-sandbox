// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { Org } from "@/shared/types/domain";
import { deriveWorkspacesTracking } from "./derive-workspaces-tracking";

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "acme",
    display_name: "Acme Corp",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: "https://github.com/acme/context.git",
    config_flags: {},
    people: [],
    teams: [
      {
        name: "default",
        source_repos: [{ owner: "alderco", repo: "codesense" }],
      },
    ],
    org_root: "/Users/dev/.codesense/users/123/orgs/acme",
    ...overrides,
  };
}

describe("deriveWorkspacesTracking", () => {
  it("returns single match when repo is tracked", () => {
    const orgs = [makeOrg()];
    const result = deriveWorkspacesTracking({ owner: "alderco", repo: "codesense" }, orgs);
    expect(result).toEqual([{ orgSlug: "acme", orgDisplayName: "Acme Corp", teamName: "default" }]);
  });

  it("returns matches across multiple orgs", () => {
    const orgs = [
      makeOrg(),
      makeOrg({
        slug: "other",
        display_name: "Other Org",
        teams: [
          {
            name: "backend",
            source_repos: [{ owner: "alderco", repo: "codesense" }],
          },
        ],
      }),
    ];
    const result = deriveWorkspacesTracking({ owner: "alderco", repo: "codesense" }, orgs);
    expect(result).toHaveLength(2);
    expect(result[0].orgSlug).toBe("acme");
    expect(result[1].orgSlug).toBe("other");
    expect(result[1].teamName).toBe("backend");
  });

  it("returns empty when no match", () => {
    const orgs = [makeOrg()];
    const result = deriveWorkspacesTracking({ owner: "unknown", repo: "repo" }, orgs);
    expect(result).toEqual([]);
  });

  it("returns empty for null input", () => {
    const orgs = [makeOrg()];
    const result = deriveWorkspacesTracking(null, orgs);
    expect(result).toEqual([]);
  });

  it("matching is case-insensitive", () => {
    const orgs = [makeOrg()];
    const result = deriveWorkspacesTracking({ owner: "AlderCo", repo: "CodeSense" }, orgs);
    expect(result).toHaveLength(1);
  });

  it("does not match the same owner/repo on a different host", () => {
    const orgs = [
      makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [
              {
                owner: "alderco",
                repo: "codesense",
                provider: "gitlab",
                host: "gitlab.com",
              },
            ],
          },
        ],
      }),
    ];

    expect(
      deriveWorkspacesTracking(
        { owner: "alderco", repo: "codesense", provider: "github", host: "github.com" },
        orgs,
      ),
    ).toEqual([]);
    expect(
      deriveWorkspacesTracking(
        { owner: "alderco", repo: "codesense", provider: "gitlab", host: "gitlab.com" },
        orgs,
      ),
    ).toHaveLength(1);
  });
});
