// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { teamChildren, teamDescendants, reparentTargetOnDelete } from "./team-chain";
import type { Team } from "@/shared/types/domain";

// Inline builder mirroring user-teams.test.ts: only the fields these pure
// walks read (name + parent_team) matter.
const team = (name: string, parent?: string | null): Team =>
  ({ name, source_repos: [], parent_team: parent }) as Team;

// @P1
describe("teamDescendants (cycle-exclusion set for the picker)", () => {
  it("leaf team → just {self}", () => {
    const teams = [team("root"), team("a", "root")];
    expect([...teamDescendants(teams, "a")]).toEqual(["a"]);
  });

  it("self + all transitive descendants", () => {
    // root ← a ← b ,  root ← c  (a,c are children of root; b child of a)
    const teams = [team("root"), team("a", "root"), team("b", "a"), team("c", "root")];
    expect(teamDescendants(teams, "root")).toEqual(new Set(["root", "a", "b", "c"]));
    expect(teamDescendants(teams, "a")).toEqual(new Set(["a", "b"]));
  });

  it("always contains self even for an unknown team", () => {
    expect(teamDescendants([team("a")], "ghost")).toEqual(new Set(["ghost"]));
  });

  it("terminates on a cyclic graph (a↔b)", () => {
    const teams = [team("a", "b"), team("b", "a")];
    expect(teamDescendants(teams, "a")).toEqual(new Set(["a", "b"]));
  });
});

// @P1
describe("teamChildren (direct children — drives AC-5 disclosure count)", () => {
  it("returns only DIRECT children, not grandchildren", () => {
    const teams = [team("root"), team("a", "root"), team("b", "a"), team("c", "root")];
    expect(teamChildren(teams, "root").sort()).toEqual(["a", "c"]);
    expect(teamChildren(teams, "a")).toEqual(["b"]);
  });

  it("returns [] for a childless team", () => {
    expect(teamChildren([team("root"), team("a", "root")], "a")).toEqual([]);
  });
});

// @P1
describe("reparentTargetOnDelete (must match IN-3 apply_delete_team rule)", () => {
  it("child reparents to the deleted team's parent (grandparent)", () => {
    const teams = [team("root"), team("mid", "root"), team("leaf", "mid")];
    // deleting "mid" → its children inherit from "root"
    expect(reparentTargetOnDelete(teams, "mid")).toBe("root");
  });

  it("deleting a root team → children become roots (null)", () => {
    const teams = [team("root"), team("mid", "root")];
    expect(reparentTargetOnDelete(teams, "root")).toBeNull();
  });

  it("unknown team → null", () => {
    expect(reparentTargetOnDelete([team("a")], "ghost")).toBeNull();
  });

  it("self-parented target → null (children become roots, matching IN-3's filter)", () => {
    const teams = [{ ...team("mid", "mid") }, team("leaf", "mid")];
    expect(reparentTargetOnDelete(teams, "mid")).toBeNull();
  });
});
