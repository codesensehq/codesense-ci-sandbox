// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { ContextRepoStatus, HistoryCommit } from "@/tauri/commands";

import { aheadBehindLine, commitWho, graphWidth, layoutHistory, refChips } from "./history-graph";

function c(sha: string, parents: string[], over: Partial<HistoryCommit> = {}): HistoryCommit {
  return {
    sha,
    shortSha: sha.slice(0, 7),
    parents,
    refs: [],
    subject: sha,
    author: "maya",
    byYou: false,
    committedAt: 0,
    lane: "main",
    localOnly: false,
    forkPoint: false,
    ...over,
  };
}

function status(over: Partial<ContextRepoStatus> = {}): ContextRepoStatus {
  return {
    branch: "main",
    headSha: "aaaaaaa",
    hasUpstream: true,
    upstream: "origin/main",
    remoteProvider: "github",
    ahead: 0,
    behind: 0,
    sync: { state: "ok", lastSyncedAt: null },
    conflicts: [],
    ...over,
  };
}

describe("layoutHistory", () => {
  // m2 ← merge(m1, b1) ← m0 ; b1 ← m0
  const page = [
    c("m2", ["mg"]),
    c("mg", ["m1", "b1"], { pr: 22 }),
    c("m1", ["m0"]),
    c("b1", ["m0"], { lane: "merged" }),
    c("m0", []),
  ];

  it("keeps main in lane 0 and gives a merged branch its own lane, drawn as its kind", () => {
    const rows = layoutHistory(page);
    expect(rows.map((r) => r.lane)).toEqual([0, 0, 0, 1, 0]);
    const merge = rows[1];
    expect(merge.outs).toEqual([
      { lane: 0, kind: "main" },
      { lane: 1, kind: "merged" },
    ]);
    // The branch's lane passes beside m1, then ends in m0 with main's.
    expect(rows[2].before[1]).toEqual({ sha: "b1", kind: "merged" });
    expect(rows[4].into).toEqual([0, 1]);
    expect(graphWidth(rows)).toBe(2);
  });

  it("is append-stable: a longer page never re-columns the rows above", () => {
    const short = layoutHistory(page.slice(0, 3)).map((r) => r.lane);
    const long = layoutHistory(page).map((r) => r.lane);
    expect(long.slice(0, 3)).toEqual(short);
  });
});

describe("refChips", () => {
  it("fills the checked-out branch, says 'not on GitHub' when ahead, and names an open PR", () => {
    const head = c("h", [], {
      refs: [
        { name: "main", kind: "local" },
        { name: "HEAD", kind: "head" },
      ],
      localOnly: true,
    });
    expect(refChips(head, status({ ahead: 1 }), { openPrs: [] })).toEqual([
      { text: "main", look: "head" },
      { text: "not on GitHub", look: "ahead" },
    ]);
    const pr = c("p", [], { refs: [{ name: "origin/gs/sync", kind: "remote" }], lane: "open" });
    expect(
      refChips(pr, status(), { openPrs: [{ number: 24, sha: "p", branch: "gs/sync" }] }),
    ).toEqual([
      { text: "#24 open", look: "pr" },
      { text: "gs/sync", look: "branch" },
    ]);
  });

  it("says how many commits GitHub has that this Mac lacks, on GitHub's copy", () => {
    const up = c("u", [], { refs: [{ name: "origin/main", kind: "remote" }] });
    expect(refChips(up, status({ behind: 2 }), { openPrs: [] })).toEqual([
      { text: "origin/main", look: "remote" },
      { text: "2 new on GitHub", look: "behind" },
    ]);
  });

  it("dashes a local branch GitHub has never seen", () => {
    const l = c("l", [], { refs: [{ name: "feat-x", kind: "local" }], lane: "local" });
    expect(refChips(l, status(), { openPrs: [] })).toEqual([{ text: "feat-x", look: "ahead" }]);
  });
});

describe("aheadBehindLine", () => {
  it("names the branch", () => {
    expect(aheadBehindLine(status({ ahead: 1 }))).toBe("main: 1 ahead");
    expect(aheadBehindLine(status({ ahead: 1, behind: 2 }))).toBe("main: 1 ahead, 2 behind");
    expect(aheadBehindLine(status())).toBe("main: up to date");
    expect(aheadBehindLine(status({ branch: null }))).toBe("detached at aaaaaaa");
    expect(aheadBehindLine(status({ branch: "feat", hasUpstream: false }))).toBe(
      "feat: not on GitHub",
    );
  });
});

describe("commitWho", () => {
  it("an agent works for the person who committed, a bot for no one", () => {
    expect(commitWho(c("a", [], { agent: { name: "claude" }, byYou: true }))).toEqual({
      who: "claude",
      forWhom: "you",
    });
    expect(commitWho(c("b", [], { agent: { name: "claude" }, author: "claude[bot]" }))).toEqual({
      who: "claude",
      forWhom: null,
    });
    expect(commitWho(c("m", []))).toEqual({ who: "maya", forWhom: null });
  });
});
