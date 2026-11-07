// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { ContextRepoStatus } from "@/tauri/commands";

import { conflictSideLine, elideMiddle, statusButtonModel } from "./context-repo-status";

const NOW = Date.parse("2026-10-06T12:00:00Z");

function status(over: Partial<ContextRepoStatus> = {}): ContextRepoStatus {
  return {
    branch: "main",
    headSha: "394fa6d",
    hasUpstream: true,
    upstream: "origin/main",
    remoteProvider: "github",
    ahead: 0,
    behind: 0,
    sync: { state: "ok", lastSyncedAt: NOW - 2 * 60_000 },
    conflicts: [],
    ...over,
  };
}

describe("statusButtonModel", () => {
  it("clean: nothing to say but the branch and when it last synced", () => {
    const m = statusButtonModel(status(), 0, NOW);
    expect(m).toMatchObject({ look: "main", clean: true, opensCard: false });
    expect(m.sentence).toBe("On main; last synced 2m ago.");
  });

  it("↑ counts uncommitted edits plus commits not pushed", () => {
    const m = statusButtonModel(status({ ahead: 2 }), 5, NOW);
    expect(m.up).toBe(7);
    expect(m.sentence).toBe(
      "On main; 7 changes not on GitHub (5 uncommitted edits + 2 commits not pushed); last synced 2m ago.",
    );
  });

  it("a branch off main, a detached HEAD and a branch with no upstream each say so", () => {
    expect(statusButtonModel(status({ branch: "gs/x" }), 0, NOW)).toMatchObject({
      look: "branch",
      sentence: expect.stringMatching(/^On gs\/x, not main;/),
    });
    expect(statusButtonModel(status({ branch: null }), 1, NOW)).toMatchObject({
      look: "detached",
      branchLabel: "394fa6d",
      sentence: expect.stringMatching(
        /^On no branch \(detached at 394fa6d\);.*nothing syncs until an agent puts it back on a branch\.$/,
      ),
    });
    expect(statusButtonModel(status({ branch: "feat", hasUpstream: false }), 0, NOW)).toMatchObject(
      { upIsNew: true, clean: false },
    );
  });

  it("a failure, a sign-out, no sender and a conflict open the card; retrying does not", () => {
    const failed = statusButtonModel(
      status({
        sync: { state: "failed", lastSyncedAt: NOW - 47 * 60_000, reason: "push refused" },
      }),
      3,
      NOW,
    );
    expect(failed).toMatchObject({ opensCard: true, stateWord: "failed" });
    expect(failed.sentence).toContain("sync failed (push refused), last synced 47m ago");
    expect(
      statusButtonModel(status({ sync: { state: "signin", lastSyncedAt: null } }), 0, NOW),
    ).toMatchObject({ opensCard: true, stateWord: "sign in" });
    expect(
      statusButtonModel(
        status({ sync: { state: "ok", lastSyncedAt: null, noSender: true } }),
        4,
        NOW,
      ).opensCard,
    ).toBe(true);
    // No remote and nothing to send: a local org's button opens Changes like any other.
    const local = statusButtonModel(
      status({ sync: { state: "ok", lastSyncedAt: null, noSender: true } }),
      0,
      NOW,
    );
    expect(local).toMatchObject({ opensCard: false, unsent: false });
    expect(local.sentence).not.toContain("will send");
    expect(
      statusButtonModel(
        status({
          conflicts: [
            {
              path: "a.md",
              mine: { who: null, subject: null, at: null, deleted: false },
              theirs: { who: null, subject: null, at: null, deleted: false },
              lastWriter: null,
              heldBack: [],
            },
          ],
        }),
        0,
        NOW,
      ),
    ).toMatchObject({ conflicts: 1, opensCard: true });
    const retrying = statusButtonModel(
      status({ sync: { state: "retrying", lastSyncedAt: null, attempt: 2, maxAttempts: 5 } }),
      3,
      NOW,
    );
    expect(retrying.opensCard).toBe(false);
    expect(retrying.sentence).toContain("retrying, attempt 2 of 5");
  });

  it("offline says when it last synced", () => {
    const m = statusButtonModel(
      status({ sync: { state: "offline", lastSyncedAt: NOW - 3 * 3_600_000 } }),
      3,
      NOW,
    );
    expect(m.sentence).toContain("offline, last synced 3h ago");
  });

  it("worktrees expose conflicts and unfinished Git operations without remote sync actions", () => {
    const worktree = {
      id: "cw_merge",
      name: "Merge",
      path: "/merge",
      mergeBase: "abc1234",
      uncommittedPaths: ["a.md"],
    };
    const conflict = {
      path: "a.md",
      mine: { who: null, subject: null, at: null, deleted: false },
      theirs: { who: null, subject: null, at: null, deleted: false },
      lastWriter: null,
      heldBack: [],
    };
    const conflicted = statusButtonModel(status({ worktree, conflicts: [conflict] }), 1, NOW);
    expect(conflicted).toMatchObject({
      conflicts: 1,
      clean: false,
      opensCard: true,
      unsent: false,
    });
    expect(conflicted.sentence).toContain("1 conflict to settle");
    const merging = statusButtonModel(
      status({ worktree, sync: { state: "syncing", progress: "Merging", lastSyncedAt: null } }),
      1,
      NOW,
    );
    expect(merging).toMatchObject({
      state: "syncing",
      clean: false,
      opensCard: true,
      unsent: false,
    });
    expect(merging.sentence).toContain("Merging");
    const stopped = statusButtonModel(
      status({
        worktree,
        sync: { state: "failed", reason: "merge stopped partway", lastSyncedAt: null },
      }),
      1,
      NOW,
    );
    expect(stopped).toMatchObject({ state: "failed", stateWord: "failed", clean: false });
    expect(stopped.sentence).toContain("merge stopped partway");
  });
});

describe("elideMiddle", () => {
  it("keeps a long branch's start and end", () => {
    expect(elideMiddle("gs/_bmad-output-d735ccb", 14)).toBe("gs/_bm…d735ccb");
    expect(elideMiddle("main", 14)).toBe("main");
  });
});

describe("conflictSideLine", () => {
  it("names who changed the file, what, and when; a deletion says so", () => {
    const at = NOW - 40 * 60_000;
    expect(
      conflictSideLine({ who: "claude", subject: "Q4 table", at, deleted: false }, "You", NOW),
    ).toBe("claude: Q4 table, 40m ago.");
    expect(
      conflictSideLine({ who: null, subject: null, at: null, deleted: false }, "You", NOW),
    ).toBe("You: changed it.");
    expect(conflictSideLine({ who: "maya", subject: "x", at, deleted: true }, "GitHub", NOW)).toBe(
      "maya deleted it, 40m ago.",
    );
  });
});
