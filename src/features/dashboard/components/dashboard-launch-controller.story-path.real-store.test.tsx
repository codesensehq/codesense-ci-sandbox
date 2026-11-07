// SPDX-License-Identifier: MIT
import { it, expect, vi } from "vitest";

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
  },
}));

vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
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
  readFile: vi.fn().mockResolvedValue("# Story\n"),
  probeWorktreeConfig: vi.fn(),
  getWorktreeConfig: vi.fn(),
  establishWorktreeStoryLink: vi.fn().mockResolvedValue(undefined),
  // SW-GW-8: a nav story open folds its session into the trusted registry (AC-1).
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/shared/lib/worktree", () => ({
  storyIdFromPath: vi.fn((p: string) => {
    const base = p.split("/").pop() ?? p;
    return base.replace(/\.md$/i, "");
  }),
  // R-18 Phase B (AC #28): identity-passthrough is faithful for clean ASCII
  // test paths.
  normalizeStoryFilePath: vi.fn((p: string) => p),
  parseStoryTitle: vi.fn(() => null),
  parseSourceRepoFromFrontmatter: vi.fn().mockReturnValue(null),
  // R-18: default to null (no story_id) for backward compat.
  parseStoryIdFromFrontmatter: vi.fn().mockReturnValue(null),
  resolveWorktreeParams: vi.fn().mockResolvedValue({ branchName: "feat/30-10", startPoint: null }),
  resolveWorktreePath: vi.fn().mockReturnValue("/wt/30-10"),
  setupWorktree: vi.fn(),
  // RF-perf-14 (decouple): the interactive open path (onCreating) now drives the
  // two phases directly — createWorktree (fast) then a backgrounded
  // runWorktreeSetupPhase — instead of the blocking setupWorktree. Default the
  // create to a fresh (non-rehydrate) success + the background setup to a no-op.
  createWorktree: vi.fn().mockResolvedValue({ worktreePath: "/wt/30-10", rehydrated: false }),
  runWorktreeSetupPhase: vi.fn().mockResolvedValue({ worktreePath: "/wt/30-10", results: [] }),
  findWorktreeOnDisk: vi.fn().mockResolvedValue(null),
}));

vi.mock("@/shared/lib/build-synthetic-repo", () => ({
  isSyntheticRepoPath: vi.fn((p: string) => p.startsWith("workspace:")),
  findWorkspaceRepoBySlug: vi.fn().mockReturnValue(undefined),
  // T.1/T.2/T.4: viewer-store imports parseSyntheticRepoKey (T.1 migration
  // helper + T.4 Phase 5 promptIdentityFromRepoPath / setupModalIdentity
  // derivation) and workspaceReposForTeam (T.2 ensureRootWorkspaces
  // filter). Strict parser rejects extra slash segments to match
  // production's invariant.
  parseSyntheticRepoKey: vi.fn((p: string) => {
    if (!p.startsWith("workspace:")) return null;
    const parts = p.slice("workspace:".length).split(":");
    if (parts.length !== 3) return null;
    const [orgSlug, team, ownerRepo] = parts;
    const slash = ownerRepo.split("/");
    if (slash.length !== 2) return null;
    return { orgSlug, team, owner: slash[0], repo: slash[1] };
  }),
  // T.2: returns a single fixture repo for the test's `repo` teamKey so
  // ensureRootWorkspaces(repo) mints exactly one root. Tests that need a
  // different shape can override the mock per-case.
  workspaceReposForTeam: vi.fn(() => [
    { repoPath: "workspace:test-org:default:test/repo", localPath: "/test/repo" },
  ]),
}));

import { makeDashboardLaunchController } from "./dashboard-launch-controller";
import { selectWorkspaceTabs, useViewerStore } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { useOrgStore } from "@/shared/stores/org-store";
import { probeWorktreeConfig, getWorktreeConfig } from "@/tauri/commands";
import {
  setupWorktree,
  createWorktree,
  runWorktreeSetupPhase,
  findWorktreeOnDisk,
  parseSourceRepoFromFrontmatter,
} from "@/shared/lib/worktree";
import { findWorkspaceRepoBySlug } from "@/shared/lib/build-synthetic-repo";
// RF-perf-14d: the real background-setup gate (imported by viewer-store from the
// deep path, so NOT mocked by the `@/shared/lib/worktree` barrel mock above).
import { __resetBackgroundSetupFanoutForTest } from "@/shared/lib/worktree/background-setup-fanout";

// SW-SO-2 (③): the open path reads status + config from ONE fused
// `probeWorktreeConfig`. `loadMock` (getWorktreeConfig) stays for any direct caller.
const probeMock = probeWorktreeConfig as unknown as ReturnType<typeof vi.fn>;
const loadMock = getWorktreeConfig as unknown as ReturnType<typeof vi.fn>;
const setupMock = setupWorktree as unknown as ReturnType<typeof vi.fn>;
const createMock = createWorktree as unknown as ReturnType<typeof vi.fn>;
const runSetupPhaseMock = runWorktreeSetupPhase as unknown as ReturnType<typeof vi.fn>;
const findOnDiskMock = findWorktreeOnDisk as unknown as ReturnType<typeof vi.fn>;
const parseSourceRepoMock = parseSourceRepoFromFrontmatter as unknown as ReturnType<typeof vi.fn>;
const findRepoBySlugMock = findWorkspaceRepoBySlug as unknown as ReturnType<typeof vi.fn>;

// T.5: `repo` is the teamKey passed as the first arg to
// `openOrFocusStoryWorkspace`/`ensureWorktree`. The action resolves the
// source repo via frontmatter against the team's tracked repos.
const repo = "test-org:default";
const storyFile = "/repo/stories/30-10-story-tab-workspaces.md";
const storyId = "30-10-story-tab-workspaces";

installTerminalLayoutHarness();

function reset() {
  useOrgStore.setState({
    activeOrgSlug: "test-org",
    activeTeamName: "default",
  });
  useViewerStore.setState({
    // SW-EW-1c: `ensureRootWorkspaces` (now a no-op) used to seed this bucket
    // with a root; seed an empty array directly so tests can spread it.
    workspaces: { [repo]: [] },
    activeWorkspace: {},
    terminalVisible: false,
    openedTeams: [repo],
  });
  probeMock.mockReset();
  loadMock.mockReset();
  setupMock.mockReset();
  // RF-perf-14: default the decoupled path's two phases to success each case.
  createMock.mockReset().mockResolvedValue({ worktreePath: "/wt/30-10", rehydrated: false });
  runSetupPhaseMock.mockReset().mockResolvedValue({ worktreePath: "/wt/30-10", results: [] });
  findOnDiskMock.mockReset().mockResolvedValue(null);
  parseSourceRepoMock.mockReset().mockReturnValue(null);
  findRepoBySlugMock.mockReset().mockReturnValue(undefined);
  // RF-perf-14d: the background-setup gate is a module-global — reset it so a cap
  // change / held slot in one case can't leak into the next (default cap 3).
  __resetBackgroundSetupFanoutForTest();
}

it("a story-path open with headed:false is rejected before any reveal, selection or tab", async () => {
  reset();
  probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
  await expect(
    makeDashboardLaunchController("/repo/panes/tree.html").openWorkspace({
      target: { kind: "story", storyPath: storyFile },
      headed: false,
    }),
  ).rejects.toThrow(/headed:false/);
  const state = useViewerStore.getState();
  expect(state.terminalVisible).toBe(false);
  expect(state.activeWorkspace[repo]).toBeUndefined();
  expect(state.workspaces[repo]).toEqual([]);
  expect(selectWorkspaceTabs(state, repo, storyId)).toHaveLength(0);
  expect(createMock).not.toHaveBeenCalled();
});
