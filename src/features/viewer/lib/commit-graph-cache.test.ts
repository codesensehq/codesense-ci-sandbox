// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import {
  getCommitGraph,
  setCommitGraph,
  evictCommitGraph,
  __resetCommitGraphCache,
  type CachedCommitGraph,
} from "./commit-graph-cache";
import type { SourceCommitGraphEntry } from "@/features/git-sync/lib/types";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

const entry = (sha: string): SourceCommitGraphEntry => ({
  sha,
  shortSha: sha.slice(0, 7),
  parents: [],
  refs: [],
  subject: sha,
  author: "t",
  committedAt: 0,
});

const snap = (
  nonce: number,
  shas: string[],
  cursor: string | null,
  hasMore = false,
): CachedCommitGraph => ({
  nonce,
  entries: shas.map(entry),
  cursor,
  hasMore,
});

beforeEach(() => __resetCommitGraphCache());

describe("commit-graph-cache", () => {
  it("returns null on a cold miss", () => {
    expect(getCommitGraph("ws1", 0)).toBeNull();
  });

  it("serves a hit for the same wsId + nonce (RD-6: no re-walk on refocus)", () => {
    setCommitGraph("ws1", snap(3, ["A", "B"], "B", true));
    const hit = getCommitGraph("ws1", 3);
    expect(hit?.entries.map((e) => e.sha)).toEqual(["A", "B"]);
    expect(hit?.cursor).toBe("B");
    expect(hit?.hasMore).toBe(true);
  });

  it("is a MISS when the nonce differs (RD-5: bumped nonce restarts from tip)", () => {
    setCommitGraph("ws1", snap(3, ["A"], "A"));
    expect(getCommitGraph("ws1", 4)).toBeNull();
  });

  it("evicts a workspace on demand", () => {
    setCommitGraph("ws1", snap(0, ["A"], "A"));
    evictCommitGraph("ws1");
    expect(getCommitGraph("ws1", 0)).toBeNull();
  });

  it("evicts a workspace's cache when it CLOSES (leaves the workspace list) — RD-6, review r1", () => {
    const wsX = { id: "wsX" } as Workspace;
    // Open wsX (in the list) and populate its cache.
    useViewerStore.setState({ workspaces: { team: [wsX] } });
    setCommitGraph("wsX", snap(0, ["A"], "A"));
    expect(getCommitGraph("wsX", 0)).not.toBeNull();
    // Close it (remove from every team's list) → the store subscription evicts.
    useViewerStore.setState({ workspaces: { team: [] } });
    expect(getCommitGraph("wsX", 0)).toBeNull();
  });

  it("does NOT evict on a mere focus switch (the workspace stays in the list) — AC-6", () => {
    const a = { id: "wsA" } as Workspace;
    const b = { id: "wsB" } as Workspace;
    useViewerStore.setState({ workspaces: { team: [a, b] } });
    setCommitGraph("wsA", snap(0, ["A"], "A"));
    // A store change that keeps both workspaces present (e.g. focus/other state).
    useViewerStore.setState({ workspaces: { team: [b, a] } });
    expect(getCommitGraph("wsA", 0)).not.toBeNull();
  });

  it("bounds memory: evicts the least-recently-used beyond the cap of 8", () => {
    for (let i = 0; i < 9; i++) setCommitGraph(`ws${i}`, snap(0, [`C${i}`], `C${i}`));
    // ws0 was the oldest insert and never touched → evicted; ws1..ws8 remain.
    expect(getCommitGraph("ws0", 0)).toBeNull();
    expect(getCommitGraph("ws8", 0)).not.toBeNull();
  });

  it("touch-on-read keeps a recently-used entry alive across evictions", () => {
    for (let i = 0; i < 8; i++) setCommitGraph(`ws${i}`, snap(0, [`C${i}`], `C${i}`));
    // Touch ws0 (would otherwise be the next LRU victim), then insert a 9th.
    expect(getCommitGraph("ws0", 0)).not.toBeNull();
    setCommitGraph("ws8", snap(0, ["C8"], "C8"));
    // ws1 (now oldest) is evicted; the touched ws0 survives.
    expect(getCommitGraph("ws0", 0)).not.toBeNull();
    expect(getCommitGraph("ws1", 0)).toBeNull();
  });
});
