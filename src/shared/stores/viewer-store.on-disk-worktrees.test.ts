// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore, composeWorktreeKey } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

// T.4 Phase 4: onDiskWorktrees is `Record<teamKey, Map<composite, …>>`
// where composite is `${sourceRepoSlug}::${storyFilePath}` (D-3).
//
// Two teams, two source repos per team to exercise the slug-preservation
// invariant of setOnDiskWorktrees (the prefix-scan replace).
const teamA = "org-a:default";
const teamB = "org-b:default";
const slugWeb = "acme/web";
const slugApi = "acme/api";
const story1 = "/teams/default/content/stories/story-1.md";
const story2 = "/teams/default/content/stories/story-2.md";
const story3 = "/teams/default/content/stories/story-3.md";

function reset() {
  useViewerStore.setState({ onDiskWorktrees: {} });
}

describe("onDiskWorktrees — setOnDiskWorktrees (T.4 D-3)", () => {
  beforeEach(reset);

  it("composes composite inner keys from raw storyFilePath input", () => {
    const map = new Map([[story1, { path: "/wt/story-1" }]]);
    useViewerStore.getState().setOnDiskWorktrees(teamA, slugWeb, map);

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket).toBeDefined();
    expect(bucket.has(composeWorktreeKey(slugWeb, story1))).toBe(true);
    expect(bucket.has(story1)).toBe(false); // raw key NOT present
  });

  it("preserves entries for OTHER source repos in the same team bucket", () => {
    // Seed: acme/web has story1, acme/api has story2 (same team).
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-web/1" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugApi, new Map([[story2, { path: "/wt-api/2" }]]));

    // Replace web's entries with a different story — api's entry untouched.
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story3, { path: "/wt-web/3" }]]));

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket.has(composeWorktreeKey(slugWeb, story1))).toBe(false);
    expect(bucket.has(composeWorktreeKey(slugWeb, story3))).toBe(true);
    // Critical D-3 invariant: api's entry survives the web replacement.
    expect(bucket.get(composeWorktreeKey(slugApi, story2))?.path).toBe("/wt-api/2");
  });

  it("prefix-scan replace uses the `::` terminator — adjacent-prefix slugs survive", () => {
    // T.4 Round 4 (Blind Hunter #4): D-3 collision tests cover `acme/web`
    // vs `acme/api` (disjoint prefixes). This test asserts the precise
    // boundary semantics — a regression to substring-prefix matching
    // (`startsWith("acme/web")` without the `::` terminator) would clobber
    // entries for adjacent slugs like `acme/web-staging`.
    const slugStaging = "acme/web-staging";
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-web/1" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugStaging, new Map([[story1, { path: "/wt-staging/1" }]]));

    // Now replace web's entries — staging must survive intact.
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story2, { path: "/wt-web/2" }]]));

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    // web's old entry is gone, new entry is present.
    expect(bucket.has(composeWorktreeKey(slugWeb, story1))).toBe(false);
    expect(bucket.get(composeWorktreeKey(slugWeb, story2))?.path).toBe("/wt-web/2");
    // staging's entry — whose composite key STARTS WITH "acme/web" but
    // continues with "-staging::…" — must NOT be clobbered.
    expect(bucket.get(composeWorktreeKey(slugStaging, story1))?.path).toBe("/wt-staging/1");
  });

  it("D-3 collision rule — same storyFilePath in two source repos coexists", () => {
    // Both source repos host a story at the same path (shared content via
    // _team/default/). Without composite keys, the second set would clobber
    // the first.
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-web/1" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugApi, new Map([[story1, { path: "/wt-api/1" }]]));

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket.get(composeWorktreeKey(slugWeb, story1))?.path).toBe("/wt-web/1");
    expect(bucket.get(composeWorktreeKey(slugApi, story1))?.path).toBe("/wt-api/1");
    expect(bucket.size).toBe(2);
  });

  it("sets an empty map (no on-disk worktrees for this source repo)", () => {
    useViewerStore.getState().setOnDiskWorktrees(teamA, slugWeb, new Map());
    expect(useViewerStore.getState().onDiskWorktrees[teamA].size).toBe(0);
  });

  it("empty Map replaces previous entries for the same slug (clears stale entries)", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt/1" }]]));
    useViewerStore.getState().setOnDiskWorktrees(teamA, slugWeb, new Map());
    expect(useViewerStore.getState().onDiskWorktrees[teamA].size).toBe(0);
  });

  it("lowercases the slug — case mismatch never causes a miss", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, "ACME/Web", new Map([[story1, { path: "/wt/1" }]]));
    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket.has(composeWorktreeKey("acme/web", story1))).toBe(true);
  });
});

describe("onDiskWorktrees — removeOnDiskWorktree (T.4 D-3)", () => {
  beforeEach(reset);

  it("removes a single composite-keyed entry", () => {
    const map = new Map([
      [story1, { path: "/wt/1" }],
      [story2, { path: "/wt/2" }],
    ]);
    useViewerStore.getState().setOnDiskWorktrees(teamA, slugWeb, map);
    useViewerStore.getState().removeOnDiskWorktree(teamA, slugWeb, story1);

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket.has(composeWorktreeKey(slugWeb, story1))).toBe(false);
    expect(bucket.get(composeWorktreeKey(slugWeb, story2))?.path).toBe("/wt/2");
  });

  it("is a no-op when the storyFilePath is not in the team bucket", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt/1" }]]));
    useViewerStore.getState().removeOnDiskWorktree(teamA, slugWeb, story2);
    expect(useViewerStore.getState().onDiskWorktrees[teamA].size).toBe(1);
  });

  it("is a no-op when the team bucket doesn't exist", () => {
    useViewerStore.getState().removeOnDiskWorktree(teamA, slugWeb, story1);
    expect(useViewerStore.getState().onDiskWorktrees[teamA]).toBeUndefined();
  });

  it("D-3 collision rule — removing one slug's entry doesn't touch the other slug's same-path entry", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-web/1" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugApi, new Map([[story1, { path: "/wt-api/1" }]]));

    useViewerStore.getState().removeOnDiskWorktree(teamA, slugWeb, story1);

    const bucket = useViewerStore.getState().onDiskWorktrees[teamA];
    expect(bucket.has(composeWorktreeKey(slugWeb, story1))).toBe(false);
    // The api entry for the SAME storyFilePath survives — D-3's whole point.
    expect(bucket.get(composeWorktreeKey(slugApi, story1))?.path).toBe("/wt-api/1");
  });
});

describe("onDiskWorktrees — isolation between teams", () => {
  beforeEach(reset);

  it("setting one team's bucket does not affect another team's bucket", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-a" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamB, slugWeb, new Map([[story3, { path: "/wt-b" }]]));

    expect(
      useViewerStore.getState().onDiskWorktrees[teamA].get(composeWorktreeKey(slugWeb, story1))
        ?.path,
    ).toBe("/wt-a");
    expect(
      useViewerStore.getState().onDiskWorktrees[teamB].get(composeWorktreeKey(slugWeb, story3))
        ?.path,
    ).toBe("/wt-b");
  });

  it("removing from one team does not affect another", () => {
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamA, slugWeb, new Map([[story1, { path: "/wt-a" }]]));
    useViewerStore
      .getState()
      .setOnDiskWorktrees(teamB, slugWeb, new Map([[story3, { path: "/wt-b" }]]));

    useViewerStore.getState().removeOnDiskWorktree(teamA, slugWeb, story1);

    expect(useViewerStore.getState().onDiskWorktrees[teamA].size).toBe(0);
    expect(useViewerStore.getState().onDiskWorktrees[teamB].size).toBe(1);
  });
});
