// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import type { Org } from "@/shared/types/domain";
import { resolveTeamContentContext } from "./team-content-context";

const org: Org = {
  slug: "acme/context",
  display_name: "Acme",
  org_root: "/outside/attached/",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [{ name: "planning", source_repos: [] }],
};

describe("team content resolution", () => {
  it("resolves a zero-source team from its registered checkout", () => {
    expect(resolveTeamContentContext("acme/context:planning", [org], true)).toEqual({
      kind: "ready",
      source: {
        repoPath: "team:acme/context:planning",
        repoName: "Acme",
        contextRepoPath: "/outside/attached",
        teamContentPath: "/outside/attached/planning/content",
        mainDoc: "",
        teamKey: "acme/context:planning",
      },
    });
  });
  it("keeps the same content identity when sources change", () => {
    const before = resolveTeamContentContext("acme/context:planning", [org], true);
    const after = resolveTeamContentContext(
      "acme/context:planning",
      [
        {
          ...org,
          teams: [
            {
              ...org.teams[0],
              source_repos: [{ owner: "acme", repo: "api" }],
            },
          ],
        },
      ],
      true,
    );
    expect(after).toEqual(before);
  });
  it("distinguishes discovery from removed context and never resolves another team", () => {
    expect(resolveTeamContentContext("acme/context:planning", [], false).kind).toBe("loading");
    expect(resolveTeamContentContext("acme/context:planning", [], true).kind).toBe("unavailable");
    expect(resolveTeamContentContext("acme/context:other", [org], true).kind).toBe("unavailable");
    for (const key of [
      "_no_team:_none",
      "acme/context:_none",
      "malformed",
      "acme/context:../other",
    ]) {
      expect(resolveTeamContentContext(key, [org], true).kind).toBe("unavailable");
    }
  });
});
