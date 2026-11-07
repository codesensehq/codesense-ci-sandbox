// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { layoutCommitGraph, type CommitLaneInput } from "./layout";

// Helper: sha "A" with the given parent shas.
const c = (sha: string, ...parents: string[]): CommitLaneInput => ({ sha, parents });

// Map sha → lane for terse assertions.
function laneBySha(commits: CommitLaneInput[]): Record<string, number> {
  const { rows } = layoutCommitGraph(commits);
  return Object.fromEntries(rows.map((r) => [r.sha, r.lane]));
}

describe("layoutCommitGraph — lane assignment", () => {
  it("keeps a linear chain in a single lane", () => {
    // A → B → C (newest first), each parent is the next.
    const commits = [c("A", "B"), c("B", "C"), c("C")];
    const lanes = laneBySha(commits);
    expect(lanes).toEqual({ A: 0, B: 0, C: 0 });
    // The root ends its lane; the tip's parent edge stays in lane 0.
    const { rows, width } = layoutCommitGraph(commits);
    expect(rows[0].parentLanes).toEqual([0]);
    expect(rows[2].parentLanes).toEqual([]); // root has no parents
    expect(width).toBe(1);
  });

  it("puts two diverging branches on distinct lanes", () => {
    // Tips A and B both descend to shared base Z.
    //   A → Z, B → Z, Z (root)
    const commits = [c("A", "Z"), c("B", "Z"), c("Z")];
    const lanes = laneBySha(commits);
    expect(lanes.A).toBe(0);
    expect(lanes.B).toBe(1); // second tip opens a new lane
    expect(lanes.Z).toBe(0); // shared base lands in the leftmost expecting lane
    const { rows } = layoutCommitGraph(commits);
    // At Z's row, lane 1 also expected Z → it converges into lane 0.
    const zRow = rows.find((r) => r.sha === "Z")!;
    expect(zRow.incoming[0]).toBe("Z");
    expect(zRow.incoming[1]).toBe("Z");
  });

  it("draws multiple join edges for a merge commit (2 parents)", () => {
    // M is a merge of P1 and P2; P1/P2 share root R.
    //   M → (P1, P2), P1 → R, P2 → R, R
    const commits = [c("M", "P1", "P2"), c("P1", "R"), c("P2", "R"), c("R")];
    const { rows } = layoutCommitGraph(commits);
    const m = rows.find((r) => r.sha === "M")!;
    expect(m.lane).toBe(0);
    // First parent continues in M's lane; the second parent opens a new lane.
    expect(m.parentLanes).toHaveLength(2);
    expect(m.parentLanes[0]).toBe(0);
    expect(m.parentLanes[1]).toBe(1);
    expect(m.parentLanes[0]).not.toBe(m.parentLanes[1]);
  });

  it("frees a lane when a branch ends and reuses it for a later tip", () => {
    // A short side branch S1→S2(root) ends, freeing its lane; a later
    // independent tip T should reuse the freed (leftmost) lane.
    //   Main:  A → B, B → C, C (root)
    //   Side:  S1 → S2, S2 (root) diverging under A
    // Order (newest first): A, S1, S2, B, T, C ... construct so a lane frees then reuses.
    const commits = [
      c("A", "B"), // lane 0
      c("S1", "S2"), // lane 1 (new tip)
      c("S2"), // lane 1 ends (root) → lane 1 freed
      c("B", "C"), // lane 0 continues
      c("T", "C"), // new tip → reuses freed leftmost lane 1
      c("C"), // root, lands leftmost expecting (lane 0)
    ];
    const lanes = laneBySha(commits);
    expect(lanes.S1).toBe(1);
    expect(lanes.S2).toBe(1);
    expect(lanes.T).toBe(1); // freed lane 1 reused
  });
});

describe("layoutCommitGraph — append stability (RD-10)", () => {
  it("layout(page1) lane assignment is a byte-identical prefix of layout(page1 ++ page2)", () => {
    // A realistic graph split across two pages (newest first).
    const page1 = [c("A", "B"), c("B", "C", "D"), c("C", "E")];
    const page2 = [c("D", "E"), c("E")];

    const first = layoutCommitGraph(page1);
    const combined = layoutCommitGraph([...page1, ...page2]);

    // Every page-1 row (sha, lane, parentLanes, incoming) is identical in the
    // combined layout — appending older history never re-columns visible rows.
    expect(combined.rows.slice(0, page1.length)).toEqual(first.rows);
  });

  it("is deterministic — same input yields identical output", () => {
    const commits = [c("A", "B", "C"), c("B", "D"), c("C", "D"), c("D")];
    expect(layoutCommitGraph(commits)).toEqual(layoutCommitGraph(commits));
  });
});

describe("layoutCommitGraph — edge cases", () => {
  it("returns an empty layout for no commits", () => {
    expect(layoutCommitGraph([])).toEqual({ rows: [], width: 0 });
  });

  it("handles a lone root commit", () => {
    const { rows, width } = layoutCommitGraph([c("A")]);
    expect(rows).toHaveLength(1);
    expect(rows[0].lane).toBe(0);
    expect(rows[0].parentLanes).toEqual([]);
    expect(width).toBe(1);
  });

  it("holds a lane open when a parent is not in the loaded page", () => {
    // B's parent Z is not present (older, unloaded) → its lane stays reserved.
    const { rows } = layoutCommitGraph([c("A", "B"), c("B", "Z")]);
    const bRow = rows.find((r) => r.sha === "B")!;
    expect(bRow.parentLanes).toEqual([0]); // edge routes down lane 0 toward Z
  });
});
