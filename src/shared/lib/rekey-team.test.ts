// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { rekeyOrgStateForRename, type RekeyableOrgState } from "@/shared/lib/rekey-team";
import { buildTeamKey } from "@/shared/lib/team-key";
import type { Org } from "@/shared/types/domain";
import type { RepoState } from "@/shared/stores/config-store";

function team(name: string) {
  return { name, source_repos: [] };
}
function org(slug: string, names: string[]): Org {
  return { slug, display_name: slug, teams: names.map(team) } as unknown as Org;
}
const repo = (repoPath: string): RepoState => ({ repoPath }) as unknown as RepoState;

function baseState(): RekeyableOrgState {
  const oldKey = buildTeamKey("acme", "payments");
  return {
    orgs: [org("acme", ["payments", "keep"]), org("other", ["payments"])],
    activeOrgSlug: "acme",
    activeTeamName: "payments",
    lastTeamByOrgSlug: { acme: "payments", other: "payments" },
    shownTeamsByOrgSlug: { acme: ["payments", "keep"] },
    suspendedWorkspaceRepos: { [oldKey]: { r: repo("workspace:acme:payments:o/r") } },
    recentlyRemovedRepos: { [oldKey]: new Set(["o/r"]) },
  };
}

describe("rekeyOrgStateForRename", () => {
  it("rewrites every keyed field for the renamed team", () => {
    const next = rekeyOrgStateForRename(baseState(), "acme", "payments", "billing");
    const newKey = buildTeamKey("acme", "billing");
    const oldKey = buildTeamKey("acme", "payments");

    // orgs: only the acme team renamed; 'keep' and the other org untouched.
    const acme = next.orgs!.find((o) => o.slug === "acme")!;
    expect(acme.teams.map((t) => t.name)).toEqual(["billing", "keep"]);
    const other = next.orgs!.find((o) => o.slug === "other")!;
    expect(other.teams.map((t) => t.name)).toEqual(["payments"]);

    expect(next.activeTeamName).toBe("billing");
    expect(next.lastTeamByOrgSlug).toEqual({ acme: "billing", other: "payments" });
    expect(next.shownTeamsByOrgSlug).toEqual({ acme: ["billing", "keep"] });

    // recentlyRemovedRepos (team-agnostic slug values) moves old→new.
    expect([...next.recentlyRemovedRepos![newKey]]).toEqual(["o/r"]);
    expect(next.recentlyRemovedRepos![oldKey]).toBeUndefined();
    // suspendedWorkspaceRepos is DROPPED (stale inner synthetic
    // keys embed the old team) so activation re-mints cleanly — neither the old
    // NOR the new key survives.
    expect(next.suspendedWorkspaceRepos![oldKey]).toBeUndefined();
    expect(next.suspendedWorkspaceRepos![newKey]).toBeUndefined();
  });

  it("preserves the team's position in the org list", () => {
    const state = baseState();
    state.orgs = [org("acme", ["a", "payments", "z"])];
    const next = rekeyOrgStateForRename(state, "acme", "payments", "billing");
    expect(next.orgs![0].teams.map((t) => t.name)).toEqual(["a", "billing", "z"]);
  });

  it("leaves activeTeamName alone when a NON-active team is renamed", () => {
    const state = baseState();
    state.activeTeamName = "keep";
    const next = rekeyOrgStateForRename(state, "acme", "payments", "billing");
    expect(next.activeTeamName).toBe("keep");
  });

  it("leaves the active team alone when a same-named team in ANOTHER org is renamed", () => {
    // Active is acme:payments; an agent renames other:payments.
    const next = rekeyOrgStateForRename(baseState(), "other", "payments", "billing");
    expect(next.activeTeamName).toBe("payments");
    expect(next.lastTeamByOrgSlug).toEqual({ acme: "payments", other: "billing" });
    expect(next.orgs!.find((o) => o.slug === "acme")!.teams.map((t) => t.name)).toEqual([
      "payments",
      "keep",
    ]);
  });

  it("is a no-op when the names are equal", () => {
    expect(rekeyOrgStateForRename(baseState(), "acme", "payments", "payments")).toEqual({});
  });

  it("does not add an entry to shown-set when the org isn't curated", () => {
    const state = baseState();
    state.shownTeamsByOrgSlug = {};
    const next = rekeyOrgStateForRename(state, "acme", "payments", "billing");
    expect(next.shownTeamsByOrgSlug).toEqual({});
  });
});
