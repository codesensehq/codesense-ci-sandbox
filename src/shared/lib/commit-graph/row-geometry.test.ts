// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { commitRowDraw } from "./row-geometry";
import { layoutCommitGraph, type CommitLaneInput } from "./layout";

const c = (sha: string, ...parents: string[]): CommitLaneInput => ({ sha, parents });

describe("commitRowDraw", () => {
  it("draws a straight lane for a linear chain middle commit", () => {
    // A → B → C; the row for B has lane 0, incoming [B], one parent edge to 0.
    const { rows } = layoutCommitGraph([c("A", "B"), c("B", "C"), c("C")]);
    const b = commitRowDraw(rows[1]);
    expect(b.nodeLane).toBe(0);
    // Top: lane 0 converges to node (0→0); bottom: node→parent lane 0.
    expect(b.segments).toContainEqual({ x1: 0, y1: 0, x2: 0, y2: 0.5 });
    expect(b.segments).toContainEqual({ x1: 0, y1: 0.5, x2: 0, y2: 1 });
  });

  it("draws two outgoing edges for a merge node", () => {
    const { rows } = layoutCommitGraph([c("M", "P1", "P2"), c("P1", "R"), c("P2", "R"), c("R")]);
    const m = commitRowDraw(rows[0]);
    // Node at lane 0; parent edges to lanes 0 and 1.
    expect(m.segments).toContainEqual({ x1: 0, y1: 0.5, x2: 0, y2: 1 });
    expect(m.segments).toContainEqual({ x1: 0, y1: 0.5, x2: 1, y2: 1 });
  });

  it("draws a converging (merge-in) edge when another lane expected this commit", () => {
    // Two tips A, B both point to shared base Z: at Z's row lane 1 converges to 0.
    const { rows } = layoutCommitGraph([c("A", "Z"), c("B", "Z"), c("Z")]);
    const z = commitRowDraw(rows[2]);
    expect(z.nodeLane).toBe(0);
    // lane 1 (which also expected Z) angles into the node at lane 0.
    expect(z.segments).toContainEqual({ x1: 1, y1: 0, x2: 0, y2: 0.5 });
  });

  it("passes an unrelated branch straight through the row", () => {
    // While A→Z resolves, a side lane holding an unrelated commit passes through.
    const { rows } = layoutCommitGraph([c("A", "Z"), c("S1", "S2"), c("Z"), c("S2")]);
    // Row for Z (index 2): lane 1 holds S2 (unrelated) → straight pass-through.
    const z = commitRowDraw(rows[2]);
    const passThroughTop = z.segments.some((s) => s.x1 === 1 && s.x2 === 1 && s.y1 === 0);
    const passThroughBottom = z.segments.some((s) => s.x1 === 1 && s.x2 === 1 && s.y2 === 1);
    expect(passThroughTop && passThroughBottom).toBe(true);
  });

  it("a root commit has no outgoing parent edges", () => {
    const { rows } = layoutCommitGraph([c("A")]);
    const a = commitRowDraw(rows[0]);
    expect(a.segments.filter((s) => s.y2 === 1)).toHaveLength(0);
  });
});
