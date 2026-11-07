// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { visibleTeams, activeTeams, archivedTeams, resolveActiveTeam } from "./user-teams";
import type { Org, Team } from "@/shared/types/domain";

const team = (name: string): Team => ({ name }) as Team;
const archivedTeam = (name: string): Team => ({ ...team(name), archived: true });
const org = (teams: Team[], slug = "acme"): Org => ({ slug, teams }) as Org;

describe("visibleTeams", () => {
  const teams = [team("a"), team("b"), team("c")];

  // SW-OB-9 D2: absent (no per-org entry) => show ALL teams. Non-regressive —
  // identical to SW-OB-7's open-access behavior. Membership never participates.
  describe("absent shown-set (never curated)", () => {
    it("returns ALL teams in the org, ignoring membership", () => {
      expect(visibleTeams(org(teams), {}).map((t) => t.name)).toEqual(["a", "b", "c"]);
    });
    it("returns a team the user is NOT a member of", () => {
      expect(visibleTeams(org([team("a")]), {}).map((t) => t.name)).toEqual(["a"]);
    });
    it("returns [] for an org with no teams", () => {
      expect(visibleTeams(org([]), {})).toEqual([]);
    });
  });

  // SW-OB-9 D2: the whole model rests on absent (undefined) !== empty ([]).
  describe("curated shown-set", () => {
    it("empty array => shows NONE (explicit curation to zero)", () => {
      expect(visibleTeams(org(teams), { acme: [] })).toEqual([]);
    });
    it("subset => shows only the named teams, in ORG order (not array order)", () => {
      expect(visibleTeams(org(teams), { acme: ["c", "a"] }).map((t) => t.name)).toEqual(["a", "c"]);
    });
    it("ignores names not present in the org", () => {
      expect(visibleTeams(org(teams), { acme: ["a", "ghost"] }).map((t) => t.name)).toEqual(["a"]);
    });
    it("keys on the org's OWN slug — another org's entry does not apply", () => {
      expect(visibleTeams(org(teams, "acme"), { other: ["a"] }).map((t) => t.name)).toEqual([
        "a",
        "b",
        "c",
      ]);
    });
  });
});

// SW-HB-7 (V-1): the archived filter is FOLDED into visibleTeams, before the
// personal shown-set filter. Archived is org-wide; the two predicates stay
// distinct (activeTeams / archivedTeams) but compose at this one funnel so no
// call site can leak an archived team.
describe("visibleTeams — archived fold (SW-HB-7 V-1)", () => {
  const mixed = [team("a"), archivedTeam("b"), team("c")];

  it("excludes archived teams under an absent shown-set", () => {
    expect(visibleTeams(org(mixed), {}).map((t) => t.name)).toEqual(["a", "c"]);
  });

  it("excludes archived teams even when the shown-set names them", () => {
    // Archive wins over personal curation — the filter applies BEFORE the
    // shown-set intersection (org truth beats personal view).
    expect(visibleTeams(org(mixed), { acme: ["a", "b", "c"] }).map((t) => t.name)).toEqual([
      "a",
      "c",
    ]);
  });

  it("back-compat: a team with no `archived` field reads as active", () => {
    expect(visibleTeams(org([team("a")]), {}).map((t) => t.name)).toEqual(["a"]);
  });
});

describe("activeTeams / archivedTeams (SW-HB-7 V-1)", () => {
  const mixed = [team("a"), archivedTeam("b"), team("c"), archivedTeam("d")];

  it("activeTeams returns only non-archived, in org order", () => {
    expect(activeTeams(org(mixed)).map((t) => t.name)).toEqual(["a", "c"]);
  });

  it("archivedTeams returns only archived (org-wide, no shown-set involved)", () => {
    expect(archivedTeams(org(mixed)).map((t) => t.name)).toEqual(["b", "d"]);
  });

  it("archivedTeams is empty when nothing is archived", () => {
    expect(archivedTeams(org([team("a")]))).toEqual([]);
  });
});

describe("resolveActiveTeam", () => {
  const teams = [team("a"), team("b"), team("c")];
  it("returns the team matching activeTeamName", () => {
    expect(resolveActiveTeam(teams, "b")?.name).toBe("b");
  });
  it("falls back to the first team when the name doesn't match", () => {
    expect(resolveActiveTeam(teams, "zzz")?.name).toBe("a");
  });
  it("falls back to the first team when activeTeamName is null", () => {
    expect(resolveActiveTeam(teams, null)?.name).toBe("a");
  });
  it("returns null for an empty list", () => {
    expect(resolveActiveTeam([], "a")).toBeNull();
  });
});
