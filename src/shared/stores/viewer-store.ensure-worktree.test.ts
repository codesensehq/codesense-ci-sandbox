// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { awaitWorktreeEstablished } from "@/shared/lib/worktree/pending-establish";

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
    loading: vi.fn().mockReturnValue("toast-loading-id"),
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
  readFile: vi.fn().mockResolvedValue("# Story Title\n"),
  removeWorktree: vi.fn().mockResolvedValue(undefined),
  getWorktreeUnpublishedContext: vi.fn().mockResolvedValue({ count: 0, files: [] }),
  probeWorktreeConfig: vi.fn(),
  getWorktreeConfig: vi.fn(),
  checkBranchExists: vi.fn().mockResolvedValue(true),
  establishWorktreeStoryLink: vi.fn().mockResolvedValue(undefined),
  // SW-GW-8: a nav story open folds its session into the trusted registry (AC-1).
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
  // SW-WB-1: reconcile-path commands.
  listWorktrees: vi.fn().mockResolvedValue([]),
  checkWorktreeDirty: vi.fn().mockResolvedValue({ dirty: false }),
  switchWorktreeBranch: vi
    .fn()
    .mockResolvedValue({ worktree_path: "/wt/feat-x", from_remote: false }),
}));

vi.mock("@/shared/lib/worktree", () => ({
  storyIdFromPath: vi.fn((p: string) => {
    const base = p.split("/").pop() ?? p;
    return base.replace(/\.md$/i, "");
  }),
  // R-18 Phase B (AC #28): defence-in-depth normaliser. Test paths are
  // already clean ASCII so identity-passthrough is a faithful mock.
  normalizeStoryFilePath: vi.fn((p: string) => p),
  parseStoryTitle: vi.fn((content: string) => {
    const match = content.match(/^#\s+(.+)/m);
    return match ? match[1].trim() : null;
  }),
  // M.2: default to null (no source_repo) for backward compat.
  parseSourceRepoFromFrontmatter: vi.fn().mockReturnValue(null),
  // R-18: default to null (no story_id) for backward compat — workspaces
  // without frontmatter story_id fall back to path-derived display labels.
  parseStoryIdFromFrontmatter: vi.fn().mockReturnValue(null),
  // SW-WB-1: shared branch-ref matcher used by the reconcile action.
  branchRefMatches: (live: string | null | undefined, want: string) =>
    !!live && (live === want || live.endsWith(`/${want}`)),
  resolveWorktreeParams: vi
    .fn()
    .mockResolvedValue({ branchName: "feat/34-9-worktree-gate", startPoint: null }),
  resolveWorktreePath: vi.fn().mockReturnValue("/wt/feat-34-9-worktree-gate"),
  setupWorktree: vi.fn(),
  // RF-perf-14 (decouple): the interactive open path drives createWorktree +
  // a backgrounded runWorktreeSetupPhase instead of the blocking setupWorktree.
  createWorktree: vi
    .fn()
    .mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false }),
  runWorktreeSetupPhase: vi
    .fn()
    .mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] }),
  findWorktreeOnDisk: vi.fn(),
}));

// F-10: mock resolveRepo so we can test synthetic-path resolution in
// cleanupStoryWorktree. Default returns undefined (legacy-path passthrough).
vi.mock("@/shared/lib/resolve-repo", () => ({
  resolveRepo: vi.fn().mockReturnValue(undefined),
}));

// M.2: mock build-synthetic-repo for findWorkspaceRepoBySlug.
// isSyntheticRepoPath is re-exported because resolve-source-repo-path.ts
// imports it from this module — omitting it breaks resolveRealRepoPath.
// T.4: parseSyntheticRepoKey is needed by deriveSourceRepoSlug (called from
// composite-key construction in ensureWorktree). Returns null for non-synthetic paths so the
// fallback branch (slug = repoPath verbatim) kicks in for legacy real-paths
// — matches the production semantics tested here.
vi.mock("@/shared/lib/build-synthetic-repo", () => ({
  isSyntheticRepoPath: vi.fn((p: string) => p.startsWith("workspace:")),
  findWorkspaceRepoBySlug: vi.fn().mockReturnValue(undefined),
  // T.1/T.2/T.4: viewer-store imports parseSyntheticRepoKey (used by T.4
  // AC-3 throwNoClone identity derivation + T.1 migration) and
  // workspaceReposForTeam (T.2 ensureRootWorkspaces). Strict parser
  // rejects extra slash segments to match production's invariant.
  parseSyntheticRepoKey: vi.fn((p: string) => {
    if (!p.startsWith("workspace:")) return null;
    const parts = p.slice("workspace:".length).split(":");
    if (parts.length !== 3) return null;
    const [orgSlug, team, ownerRepo] = parts;
    const slash = ownerRepo.split("/");
    if (slash.length !== 2) return null;
    return { orgSlug, team, owner: slash[0], repo: slash[1] };
  }),
  // T.2: returns a single fixture repo for the test's `repo` teamKey.
  workspaceReposForTeam: vi.fn(() => [
    { repoPath: "workspace:test-org:default:test/repo", localPath: "/test/repo" },
  ]),
}));

import { toast } from "sonner";
import {
  useViewerStore,
  selectWorkspaceTabs,
  composeWorktreeKey,
  deriveSourceRepoSlug,
  resolveOpeningWorkspaceId,
  getBackgroundSetupStepOutput,
  type Workspace,
} from "./viewer-store";
import { useOrgStore } from "./org-store";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import {
  probeWorktreeConfig,
  getWorktreeConfig,
  removeWorktree,
  getWorktreeUnpublishedContext,
  readFile,
  checkBranchExists,
  establishWorktreeStoryLink,
  dashboardRegisterSession,
  listWorktrees,
  checkWorktreeDirty,
  switchWorktreeBranch,
} from "@/tauri/commands";
import { resolveRepo } from "@/shared/lib/resolve-repo";
import {
  setupWorktree,
  createWorktree,
  runWorktreeSetupPhase,
  findWorktreeOnDisk,
  parseSourceRepoFromFrontmatter,
  parseStoryIdFromFrontmatter,
  resolveWorktreeParams,
  resolveWorktreePath,
} from "@/shared/lib/worktree";
import { findWorkspaceRepoBySlug } from "@/shared/lib/build-synthetic-repo";

const removeWorktreeMock = removeWorktree as unknown as ReturnType<typeof vi.fn>;
const unpublishedMock = getWorktreeUnpublishedContext as unknown as ReturnType<typeof vi.fn>;
const resolveRepoMock = resolveRepo as unknown as ReturnType<typeof vi.fn>;
const createMock = createWorktree as unknown as ReturnType<typeof vi.fn>;
const runSetupPhaseMock = runWorktreeSetupPhase as unknown as ReturnType<typeof vi.fn>;

// SW-SO-2 (③): the open path now reads status + config from ONE fused
// `probeWorktreeConfig`. `loadMock` (getWorktreeConfig) is still exercised by the
// non-open-path callers below.
const probeMock = probeWorktreeConfig as unknown as ReturnType<typeof vi.fn>;
const loadMock = getWorktreeConfig as unknown as ReturnType<typeof vi.fn>;
const setupMock = setupWorktree as unknown as ReturnType<typeof vi.fn>;
const findOnDiskMock = findWorktreeOnDisk as unknown as ReturnType<typeof vi.fn>;
const parseSourceRepoMock = parseSourceRepoFromFrontmatter as unknown as ReturnType<typeof vi.fn>;
const parseStoryIdMock = parseStoryIdFromFrontmatter as unknown as ReturnType<typeof vi.fn>;
const findRepoBySlugMock = findWorkspaceRepoBySlug as unknown as ReturnType<typeof vi.fn>;
const readFileMock = readFile as unknown as ReturnType<typeof vi.fn>;
const checkBranchExistsMock = checkBranchExists as unknown as ReturnType<typeof vi.fn>;
const resolveWorktreeParamsMock = resolveWorktreeParams as unknown as ReturnType<typeof vi.fn>;
const establishLinkMock = establishWorktreeStoryLink as unknown as ReturnType<typeof vi.fn>;
const registerSessionMock = dashboardRegisterSession as unknown as ReturnType<typeof vi.fn>;

// T.5: `repo` is now a proper teamKey passed as the first arg to
// `ensureWorktree`/`openOrFocusStoryWorkspace`. The action derives the source repo from the
// story's frontmatter (or falls back to the team's first source repo via
// `workspaceReposForTeam`).
const repo = "test-org:default";
const storyFile = "/repo/stories/34-9-worktree-gate.md";
const storyId = "34-9-worktree-gate";

installTerminalLayoutHarness();

// T.5: when no frontmatter source_repo is declared, ensureWorktree falls
// back to the team's first source repo. The `workspaceReposForTeam` mock
// returns `{ repoPath: "workspace:test-org:default:test/repo", localPath:
// "/test/repo" }`, so the resolved slug is `"test/repo"` and the
// resolved source clone path is `/test/repo`. Tests compose against these
// values for the no-frontmatter fallback path.
const teamFallbackSlug = "test/repo";
const teamFallbackSourcePath = "/test/repo";

const baseConfig = {
  version: 1,
  setup: ["npm install"],
};

function reset() {
  useOrgStore.setState({
    activeOrgSlug: "test-org",
    activeTeamName: "default",
    recentlyRemovedRepos: {},
  });
  useViewerStore.setState({
    // SW-EW-1c: `ensureRootWorkspaces` used to seed `workspaces[repo]` with a
    // root here; it is now a no-op (roots retired). Seed an empty bucket so the
    // tests that spread `...s.workspaces[repo]` have an array to extend.
    workspaces: { [repo]: [] },
    activeWorkspace: {},
    onDiskWorktrees: {},
    terminalVisible: false,
    openingWorkspaces: {},
    openedTeams: [repo],
  });
  probeMock.mockReset();
  loadMock.mockReset();
  setupMock.mockReset();
  // RF-perf-14: default the decoupled path's two phases to success each case.
  createMock
    .mockReset()
    .mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false });
  runSetupPhaseMock
    .mockReset()
    .mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });
  findOnDiskMock.mockReset().mockResolvedValue(null);
  resolveRepoMock.mockReset().mockReturnValue(undefined);
  parseSourceRepoMock.mockReset().mockReturnValue(null);
  parseStoryIdMock.mockReset().mockReturnValue(null);
  findRepoBySlugMock.mockReset().mockReturnValue(undefined);
  checkBranchExistsMock.mockReset().mockResolvedValue(true);
  resolveWorktreeParamsMock
    .mockReset()
    .mockResolvedValue({ branchName: "feat/34-9-worktree-gate", startPoint: null });
  establishLinkMock.mockReset().mockResolvedValue(true);
}

describe("ensureWorktree — store hit", () => {
  beforeEach(reset);

  it("resolves the canonical tracked slug for a legacy ready workspace without source_repo", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/existing",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/existing");
    expect(establishLinkMock).toHaveBeenCalledWith("/wt/existing", storyFile, "", teamFallbackSlug);
    expect(findOnDiskMock).not.toHaveBeenCalled();
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("skips workspaces in 'creating' status (not ready)", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/creating",
            storyFilePath: storyFile,
            storyId,
            status: "creating" as const,
          },
        ],
      },
    }));

    // Should proceed to on-disk check since store entry is not "ready"
    findOnDiskMock.mockResolvedValue({
      path: "/wt/on-disk",
      branch: "refs/heads/feat/34-9-worktree-gate",
      head_sha: "abc",
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/on-disk");
    expect(findOnDiskMock).toHaveBeenCalled();
  });
});

describe("ensureWorktree — on-disk detection", () => {
  beforeEach(reset);

  it("keeps failed registration retryable instead of caching a ready workspace", async () => {
    findOnDiskMock.mockResolvedValue({ path: "/wt/retry-register", branch: "feat/retry" });
    establishLinkMock
      .mockRejectedValueOnce(new Error("registration unavailable"))
      .mockResolvedValueOnce(true);
    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "registration unavailable",
    );
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === storyId)?.status).toBe(
      "error",
    );
    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).resolves.toBe(
      "/wt/retry-register",
    );
    expect(establishLinkMock).toHaveBeenCalledTimes(2);
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
  });

  it("reconciles persisted ready workspaces whose durable setup is incomplete", async () => {
    findOnDiskMock.mockResolvedValue({ path: "/wt/persisted", branch: "feat/retry" });
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/persisted",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
            sourceRepoPath: repo,
          },
        ],
      },
    }));
    establishLinkMock.mockResolvedValue(false);
    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).resolves.toBe(
      "/wt/persisted",
    );
    expect(runSetupPhaseMock).toHaveBeenCalledTimes(1);
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
  });

  it("does not reopen a workspace closed during ready reconciliation", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/persisted",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
            sourceRepoPath: repo,
            sourceRepoSlug: teamFallbackSlug,
          },
        ],
      },
    }));
    let release!: (complete: boolean) => void;
    establishLinkMock.mockReturnValue(
      new Promise((resolve) => {
        release = resolve;
      }),
    );
    const opening = useViewerStore.getState().ensureWorktree(repo, storyFile);
    useViewerStore.getState().closeWorkspace(repo, storyId);
    release(false);
    await expect(opening).rejects.toThrow("cancelled");
    expect(runSetupPhaseMock).not.toHaveBeenCalled();
    expect(useViewerStore.getState().workspaces[repo].some((w) => w.id === storyId)).toBe(false);
  });

  it("recovers incomplete setup on an existing worktree without recreating it", async () => {
    findOnDiskMock.mockResolvedValue({ path: "/wt/retry-setup", branch: "feat/retry" });
    establishLinkMock.mockResolvedValue(false);
    runSetupPhaseMock
      .mockRejectedValueOnce(new Error("setup failed"))
      .mockResolvedValueOnce({ worktreePath: "/wt/retry-setup", results: [] });
    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "setup failed",
    );
    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).resolves.toBe(
      "/wt/retry-setup",
    );
    expect(runSetupPhaseMock).toHaveBeenCalledTimes(2);
    expect(createMock).not.toHaveBeenCalled();
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
  });

  it("adopts an on-disk worktree and returns its path", async () => {
    findOnDiskMock.mockResolvedValue({
      path: "/wt/on-disk",
      branch: "refs/heads/feat/34-9-worktree-gate",
      head_sha: "abc",
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/on-disk");
    expect(setupMock).not.toHaveBeenCalled();

    // Verify adoption: workspace registered in store
    const ws = useViewerStore.getState().workspaces[repo];
    const adopted = ws.find((w) => w.id === storyId);
    expect(adopted).toBeDefined();
    expect(adopted!.cwd).toBe("/wt/on-disk");
    expect(adopted!.status).toBe("ready");
    expect(adopted!.storyFilePath).toBe(storyFile);
  });
});

describe("ensureWorktree — config optional (SW-WT-1)", () => {
  beforeEach(reset);

  it("PROCEEDS bare when config is missing — no throw, setup still runs", async () => {
    // SW-WT-1 AC-6: a missing worktree.yaml is a calm state. ensureWorktree
    // proceeds to create the worktree (setup materializes via the wave) — it
    // no longer rejects or raises a setup-required modal.
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "missing" }, config: null });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();
  });

  it("rejects with failedStep 'config' and toasts when config is invalid", async () => {
    // SW-WT-1: an invalid worktree.yaml is loud — the user expressed intent
    // and it's broken. Surface the error via toast and throw a `config`-step
    // error; setup must not run.
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "invalid", error: "bad yaml" }, config: null });

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile)
      .catch((e: unknown) => e);

    expect((err as { failedStep?: string }).failedStep).toBe("config");
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("bad yaml"));
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("PROCEEDS bare when the config probe IPC fails (degrades to missing)", async () => {
    // A transient backend hiccup on the fused probe degrades to `missing` so
    // creation is never blocked (AC-6).
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockRejectedValue(new Error("IPC error"));
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();
  });

  it("PROCEEDS with defaults when the probe reports present but carries a null config", async () => {
    // SW-SO-2 (③): a `present` status with a null `config` (defensive) → proceed
    // with null-defaulted config (resolve.ts null-defaults). No throw, no second
    // read.
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: null });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();
  });

  it("DOES NOT swallow a present-but-broken config to defaults (surfaces invalid)", async () => {
    // SW-SO-2 (③): the fuse structurally eliminates the old "present, then a
    // second load fails" mode — presence + content come from ONE read. The
    // "don't silently degrade a real config to defaults" guarantee (SW-WT-1
    // review hardening) is now the `invalid` path: a config that exists but is
    // malformed surfaces loudly and blocks creation, never falls through to
    // defaults.
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "invalid", error: "IPC boom" }, config: null });

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "IPC boom",
    );
    expect(setupMock).not.toHaveBeenCalled();
  });
});

describe("ensureWorktree — creation flow", () => {
  beforeEach(reset);

  it("creates a worktree, shows toast, adopts, and returns the cwd", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({
      worktreePath: "/wt/feat-34-9-worktree-gate",
      results: [],
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();

    // SLW-CO-5: the workspace flips "creating" → "ready" on success (no toast).
    const ws = useViewerStore.getState().workspaces[repo];
    const adopted = ws.find((w) => w.id === storyId);
    expect(adopted).toBeDefined();
    expect(adopted!.status).toBe("ready");
    // SW-XS-10: Step 1 alone never paints Ready; with no agent expected the line clears.
    expect(adopted!.setupState).toBeUndefined();
    expect(adopted!.cwd).toBe("/wt/feat-34-9-worktree-gate");
  });

  // SW-AM-7: the re-home cold-open attaches the agent's OWN PTY as the tab, so it
  // must suppress the default first-shell spawn (which would also hijack the
  // storyId nav-registration). `awaitSetup:true` forces the blocking path so the
  // flip+spawn is observable synchronously after the await.
  it("SW-AM-7: spawnFirstTerminal:false suppresses the default first shell on create", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    await useViewerStore.getState().ensureWorktree(repo, storyFile, undefined, undefined, {
      onCreating: () => {},
      awaitSetup: true,
      spawnFirstTerminal: false,
    });

    expect(selectWorkspaceTabs(useViewerStore.getState(), repo, storyId)).toEqual([]);
  });

  it("SW-AM-7: default (spawnFirstTerminal unset) still spawns the first shell on create", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    await useViewerStore.getState().ensureWorktree(repo, storyFile, undefined, undefined, {
      onCreating: () => {},
      awaitSetup: true,
    });

    expect(selectWorkspaceTabs(useViewerStore.getState(), repo, storyId)).toHaveLength(1);
  });

  it("adds the workspace as 'creating' BEFORE setup resolves (SLW-CO-5 AC-1)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    // Assert the record exists as "creating" WHILE setup is still pending — the
    // terminal panel + inline overlay mount before the (potentially slow) setup.
    let statusDuringSetup: string | undefined;
    setupMock.mockImplementation(async () => {
      statusDuringSetup = useViewerStore
        .getState()
        .workspaces[repo]?.find((w) => w.id === storyId)?.status;
      return { worktreePath: "/wt/feat-34-9-worktree-gate", results: [] };
    });

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(statusDuringSetup).toBe("creating");
  });

  it("flips the workspace to 'error' + statusError when setup fails (SLW-CO-5 AC-6)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    const setupError = Object.assign(new Error("npm install failed"), {
      failedStep: "setup",
      recoverable: true,
    });
    setupMock.mockRejectedValue(setupError);

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "npm install failed",
    );

    // The error is surfaced on the record (frame swaps to WorkspaceErrorPlaceholder).
    const failed = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(failed?.status).toBe("error");
    expect(failed?.statusError).toBe("npm install failed");
  });

  it("tears down the 'creating' workspace when setup is cancelled (SLW-CO-5 AC-8)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    const cancelError = Object.assign(new Error("cancelled"), {
      failedStep: "cancelled",
      recoverable: true,
    });
    setupMock.mockRejectedValue(cancelError);

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "cancelled",
    );

    // No phantom "creating" workspace left behind.
    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.find((w) => w.id === storyId)).toBeUndefined();
  });

  it("clears a stale 'error' record so a retry re-creates the workspace fresh (SLW-CO-5)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Seed a prior FAILED open (the error trap the fix removes).
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          {
            id: storyId,
            kind: "story",
            cwd: "/wt/feat-34-9-worktree-gate",
            storyFilePath: storyFile,
            storyId,
            status: "error",
            statusError: "npm install failed",
          },
        ],
      },
    }));

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    // The retry re-created the workspace fresh — now ready, error cleared.
    const after = useViewerStore.getState().workspaces[repo]?.filter((w) => w.id === storyId) ?? [];
    expect(after).toHaveLength(1);
    expect(after[0].status).toBe("ready");
    expect(after[0].statusError).toBeUndefined();
  });

  it("isMidFlightClose guard: closeTeam during setupWorktree skips addStoryWorkspace (Round 4 Patch 1)", async () => {
    // T.4 Round 4 (Edge Case Hunter #1 / Round 3 MED → Round 4 sweep):
    // `teamCloseEpoch` is bumped by `closeTeam`; `ensureWorktree` captures
    // the epoch at entry and checks at every post-await write site. If
    // `closeTeam(teamKey)` fires during `setupWorktree`'s multi-second
    // window, the post-await `addStoryWorkspace` is skipped (else the
    // workspace would resurrect a torn-down team).
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    // Drive setupWorktree to a deferred promise so we can fire closeTeam
    // BETWEEN the await and the resolution. Use a `setupCalled` signal so
    // we wait for the IIFE to ACTUALLY reach setupMock before bumping the
    // epoch (otherwise `resolveSetup` would still be undefined).
    let resolveSetup!: (v: { worktreePath: string; results: never[] }) => void;
    let signalSetupCalled!: () => void;
    const setupCalledPromise = new Promise<void>((res) => {
      signalSetupCalled = res;
    });
    setupMock.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSetup = res;
          signalSetupCalled();
        }),
    );

    const promise = useViewerStore.getState().ensureWorktree(repo, storyFile);

    // Wait for `setupWorktree` to actually be called — pre-setup awaits
    // (readFile, parseSourceRepo, etc.) must drain first.
    await setupCalledPromise;

    // closeTeam fires while ensureWorktree is awaiting setupWorktree.
    useViewerStore.getState().closeTeam(repo);

    // Now resolve setupWorktree — the post-await guard should detect the
    // epoch bump and skip the addStoryWorkspace write.
    resolveSetup({ worktreePath: "/wt/raced", results: [] });
    await promise;

    expect(setupMock).toHaveBeenCalledTimes(1);

    // The workspace must NOT be in the store — guard tripped. Pre-patch,
    // `addStoryWorkspace` would have written into a torn-down team's bucket,
    // resurrecting the team in `workspaces`.
    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws?.find((w) => w.id === storyId)).toBeUndefined();

    // SW-TS-4: the link/derive is inside the same guard, so a mid-flight
    // close also skips firing a team-derive into the torn-down team. A
    // re-open re-links it idempotently (symmetric with the on-disk exit).
    expect(establishLinkMock).not.toHaveBeenCalled();
  });
});

describe("ensureWorktree — SW-TS-4 establish link", () => {
  beforeEach(reset);

  it("links + derives the freshly-created worktree (worktreePath, story path, slug)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });
    // Story id comes from the story's frontmatter, not the path-derived label.
    parseStoryIdMock.mockReturnValue("SW-TS-4");

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(establishLinkMock).toHaveBeenCalledTimes(1);
    expect(establishLinkMock).toHaveBeenCalledWith(
      "/wt/feat-34-9-worktree-gate",
      storyFile,
      "SW-TS-4",
      teamFallbackSlug,
    );
  });

  it("re-asserts the link + re-derives for an already-on-disk worktree (AC-4)", async () => {
    findOnDiskMock.mockResolvedValue({
      path: "/wt/on-disk",
      branch: "refs/heads/feat/34-9-worktree-gate",
      head_sha: "abc",
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/on-disk");
    expect(setupMock).not.toHaveBeenCalled();
    expect(establishLinkMock).toHaveBeenCalledTimes(1);
    expect(establishLinkMock).toHaveBeenCalledWith(
      "/wt/on-disk",
      storyFile,
      // No frontmatter story_id in the default mock → empty string passed.
      "",
      teamFallbackSlug,
    );
  });

  it("tracks the in-flight establish so a launch can await it before spawn", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });
    let release!: () => void;
    establishLinkMock.mockReturnValue(new Promise<void>((r) => (release = r)));

    const opening = useViewerStore.getState().ensureWorktree(repo, storyFile);
    await vi.waitFor(() => expect(establishLinkMock).toHaveBeenCalledOnce());
    const cwd = "/wt/feat-34-9-worktree-gate";

    let settled = false;
    const waiting = awaitWorktreeEstablished(cwd).then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    release();
    await waiting;
    await opening;
    expect(settled).toBe(true);
  });

  it("propagates a failed registration without exposing ready", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });
    // Fire-and-forget: the derive command rejects, but the user has already
    // landed — ensureWorktree must still resolve to the cwd.
    establishLinkMock.mockRejectedValue(new Error("derive enqueue failed"));

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      "derive enqueue failed",
    );
  });
});

describe("ensureWorktree — deduplication", () => {
  beforeEach(reset);

  it("concurrent calls for the same story share a single promise", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({
      worktreePath: "/wt/feat-34-9-worktree-gate",
      results: [],
    });

    const store = useViewerStore.getState();
    const p1 = store.ensureWorktree(repo, storyFile);
    const p2 = store.ensureWorktree(repo, storyFile);

    const [cwd1, cwd2] = await Promise.all([p1, p2]);

    expect(cwd1).toBe("/wt/feat-34-9-worktree-gate");
    expect(cwd2).toBe("/wt/feat-34-9-worktree-gate");
    // Setup only called once — second call piggybacked on the first's promise
    expect(setupMock).toHaveBeenCalledTimes(1);
  });
});

// ===========================================================================
// SW-SO-1 — pre-record "opening" overlay flag + hoisted abort
// ===========================================================================

describe("resolveOpeningWorkspaceId (pure — AC-7 handoff)", () => {
  const rec = (id: string): Workspace =>
    ({ id, kind: "story", cwd: "", status: "creating" }) as Workspace;

  it("returns undefined for no opening set / empty set", () => {
    expect(resolveOpeningWorkspaceId(undefined, [])).toBeUndefined();
    expect(resolveOpeningWorkspaceId(new Set(), [])).toBeUndefined();
  });

  it("returns the opening id while NO record exists yet", () => {
    expect(resolveOpeningWorkspaceId(new Set(["x"]), [])).toBe("x");
  });

  it("returns undefined once the record lands (handoff to the in-tile overlay)", () => {
    expect(resolveOpeningWorkspaceId(new Set(["x"]), [rec("x")])).toBeUndefined();
  });

  it("skips ids that already have a record, returns the still-pending one", () => {
    expect(resolveOpeningWorkspaceId(new Set(["x", "y"]), [rec("x")])).toBe("y");
  });
});

describe("beginWorkspaceOpen / endWorkspaceOpen (idempotency)", () => {
  beforeEach(reset);
  const opening = () => useViewerStore.getState().openingWorkspaces[repo];

  it("adds idempotently and drops the per-team set when it empties", () => {
    const s = useViewerStore.getState();
    s.beginWorkspaceOpen(repo, "a");
    s.beginWorkspaceOpen(repo, "a"); // idempotent — no duplicate
    expect([...(opening() ?? [])]).toEqual(["a"]);
    s.beginWorkspaceOpen(repo, "b");
    expect(opening()?.size).toBe(2);
    s.endWorkspaceOpen(repo, "a");
    expect([...(opening() ?? [])]).toEqual(["b"]);
    s.endWorkspaceOpen(repo, "missing"); // no-op on unknown id
    expect([...(opening() ?? [])]).toEqual(["b"]);
    s.endWorkspaceOpen(repo, "b");
    // Empty set is removed entirely (kept clean).
    expect(useViewerStore.getState().openingWorkspaces[repo]).toBeUndefined();
  });
});

describe("ensureWorktree — SW-SO-1 opening overlay flag", () => {
  beforeEach(reset);
  const opening = () => useViewerStore.getState().openingWorkspaces[repo];

  it("AC-1: raises the flag SYNCHRONOUSLY on a foreground open, before any await, and clears it on success", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Call WITHOUT awaiting — the flag must already be set (raised before the
    // first probe await, within the click's synchronous task → paints instantly).
    const p = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    expect(opening()?.has(storyId)).toBe(true);

    await p;
    // Cleared on the single `finally`.
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("AC-3: the fast path (ready workspace) NEVER raises the flag — no flash", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/existing",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));

    const p = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    // Set after the fast-path early-return, so it is never raised here.
    expect(opening()?.has(storyId) ?? false).toBe(false);
    await p;
    expect(opening()?.has(storyId) ?? false).toBe(false);
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("is NOT raised for a background (no-onCreating) ensureWorktree call", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    const p = useViewerStore.getState().ensureWorktree(repo, storyFile);
    expect(opening()?.has(storyId) ?? false).toBe(false);
    await p;
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("AC-7: while the 'creating' record exists, the overlay hands off (resolveOpeningWorkspaceId → undefined) even though the flag is still set", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    // Hold Phase A (create) pending so the record sits in "creating" while we
    // inspect (RF-perf-14: the pre-ready window is now createWorktree).
    let resolveSetup!: (v: { worktreePath: string; rehydrated: boolean }) => void;
    let signalSetupCalled!: () => void;
    const setupCalled = new Promise<void>((res) => (signalSetupCalled = res));
    createMock.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSetup = res;
          signalSetupCalled();
        }),
    );

    const p = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    await setupCalled;

    const st = useViewerStore.getState();
    // Record now exists as "creating" AND the flag is still set (finally hasn't run)…
    expect(st.workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe("creating");
    expect(st.openingWorkspaces[repo]?.has(storyId)).toBe(true);
    // …but the pre-record overlay has ALREADY handed off (record present → undefined).
    expect(
      resolveOpeningWorkspaceId(st.openingWorkspaces[repo], st.workspaces[repo] ?? []),
    ).toBeUndefined();

    resolveSetup({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false });
    await p;
    expect(useViewerStore.getState().openingWorkspaces[repo]?.has(storyId) ?? false).toBe(false);
  });

  it("clears the flag on an error exit (config invalid)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "invalid", error: "bad yaml" }, config: null });

    await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch(() => undefined);

    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r1 (AC-1): reveals the terminal region synchronously when raising the flag, so the overlay isn't clipped in viewer/collapsed mode", async () => {
    // The overlay is hosted inside the terminal flex region, which is 0px-tall
    // while `terminalVisible` is false. Raising the flag must also reveal it — at
    // intent time, not the post-probe `onCreating` — or there's no visible
    // spinner during the exact pre-flight stall AC-1 targets.
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Terminal starts hidden (viewer/collapsed mode).
    expect(useViewerStore.getState().terminalVisible).toBe(false);

    const p = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    // Revealed synchronously (before any probe await), so the overlay is visible.
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    expect(useViewerStore.getState().layoutMode).toBe("shared");

    await p;
  });

  it("review r1: the fast path does NOT reveal the terminal (no flag → no side effects)", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/existing",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));

    await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });

    expect(useViewerStore.getState().terminalVisible).toBe(false);
  });

  it("review r2 (AC-5): on-disk adopt clears the opening flag and lands the right cwd", async () => {
    findOnDiskMock.mockResolvedValue({
      path: "/wt/on-disk",
      branch: "refs/heads/feat/34-9-worktree-gate",
      head_sha: "abc",
    });

    const cwd = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });

    expect(cwd).toBe("/wt/on-disk");
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.cwd).toBe(
      "/wt/on-disk",
    );
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r2 (AC-6): a blocked-dependency exit clears the opening flag", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    // startPoint (dep branch) differs from the default → the pre-flight branch
    // guard runs; the dep branch is missing → blocked-dep throw.
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "dep-branch",
    });
    checkBranchExistsMock.mockResolvedValue(false);

    const err = (await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch((e: unknown) => e)) as { failedStep?: string };

    expect(err.failedStep).toBe("blocked-dep");
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r2 (AC-6): closeTeam sweeps openingWorkspaces (mid-flight team close)", () => {
    useViewerStore.getState().beginWorkspaceOpen(repo, storyId);
    expect(useViewerStore.getState().openingWorkspaces[repo]?.has(storyId)).toBe(true);

    useViewerStore.getState().closeTeam(repo);
    expect(useViewerStore.getState().openingWorkspaces[repo]).toBeUndefined();
  });
});

describe("cancelWorkspaceOpen — SW-SO-1 (AC-2)", () => {
  beforeEach(reset);
  const opening = () => useViewerStore.getState().openingWorkspaces[repo];

  it("aborts the pre-flight open: drops the overlay immediately, creates no record, never runs setup", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Hold findWorktreeOnDisk pending to sit in the pre-flight probe window.
    let resolveFind!: (v: unknown) => void;
    findOnDiskMock.mockReset().mockReturnValue(
      new Promise((res) => {
        resolveFind = res;
      }),
    );

    const onCreating = vi.fn();
    const result = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating })
      .catch((e: unknown) => e);

    // Overlay is up during the probe window.
    expect(opening()?.has(storyId)).toBe(true);

    // Cancel: overlay clears IMMEDIATELY (even while findWorktreeOnDisk is still pending).
    useViewerStore.getState().cancelWorkspaceOpen(repo, storyId);
    expect(opening()?.has(storyId) ?? false).toBe(false);

    // Let the held probe resolve — the post-await checkpoint bails as "cancelled".
    resolveFind(null);
    const err = (await result) as { failedStep?: string };
    expect(err.failedStep).toBe("cancelled");

    // No worktree/record was created; setup never ran; the record overlay never took over.
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId),
    ).toBeUndefined();
    expect(setupMock).not.toHaveBeenCalled();
    expect(onCreating).not.toHaveBeenCalled();
  });

  it("review r1: an immediate reopen of the SAME story after Cancel runs a FRESH open (no dead click)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Open 1 sits in the pre-flight window on a held findWorktreeOnDisk (the slow
    // git-spawn a user would Cancel). Only the FIRST probe call is held.
    let resolveFind1!: (v: unknown) => void;
    findOnDiskMock.mockReset().mockReturnValueOnce(
      new Promise((res) => {
        resolveFind1 = res;
      }),
    );
    const p1 = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch((e: unknown) => e);
    expect(opening()?.has(storyId)).toBe(true);

    // Let open 1 actually REACH + hold findWorktreeOnDisk before we cancel/reopen —
    // in production there's a human-scale delay, so open 1 is already holding its
    // git spawn when the user cancels. (Without this, both opens race the shared
    // mock synchronously and open 2 grabs the held probe — a test artifact.)
    await new Promise((r) => setTimeout(r, 0));
    expect(findOnDiskMock).toHaveBeenCalledTimes(1);
    findOnDiskMock.mockResolvedValue(null); // open 2's probe resolves cleanly

    // Cancel, then IMMEDIATELY reopen the same story while open 1 is still aborting.
    useViewerStore.getState().cancelWorkspaceOpen(repo, storyId);
    const onCreating2 = vi.fn();
    const p2 = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: onCreating2 });

    // Fresh open re-raised the flag synchronously (a piggyback would skip
    // beginWorkspaceOpen and leave it cleared).
    expect(opening()?.has(storyId)).toBe(true);

    // The reopen is a FRESH open — it does NOT piggyback open 1's doomed promise:
    // it actually creates the worktree (resolves to the cwd, not a swallowed "").
    await expect(p2).resolves.toBe("/wt/feat-34-9-worktree-gate");
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
    expect(onCreating2).toHaveBeenCalled();

    // Draining open 1's doomed promise must NOT clobber open 2's now-owned state:
    // the ready record + cleared flag survive.
    resolveFind1(null);
    const err1 = (await p1) as { failedStep?: string };
    expect(err1.failedStep).toBe("cancelled");
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r1: closing an in-flight 'creating' record clears the opening flag (no resurrected overlay)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    // Hold Phase A (create) so the record sits in "creating" with the flag set.
    let resolveSetup!: (v: { worktreePath: string; rehydrated: boolean }) => void;
    let signalSetupCalled!: () => void;
    const setupCalled = new Promise<void>((res) => (signalSetupCalled = res));
    createMock.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSetup = res;
          signalSetupCalled();
        }),
    );

    const p = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch((e: unknown) => e);
    await setupCalled;
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "creating",
    );

    // Close the creating record (in-tile Cancel). This deletes the abort handle,
    // so the aborted open's ownership-guarded finally will skip its flag clear —
    // closeWorkspace must clear it, else the flag outlives the record and
    // resolveOpeningWorkspaceId resurrects the overlay.
    useViewerStore.getState().closeWorkspace(repo, storyId);
    expect(opening()?.has(storyId) ?? false).toBe(false);
    expect(
      resolveOpeningWorkspaceId(
        useViewerStore.getState().openingWorkspaces[repo],
        useViewerStore.getState().workspaces[repo] ?? [],
      ),
    ).toBeUndefined();

    resolveSetup({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false });
    await p;
    // Still clear after the doomed open drains.
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r2: in-tile Cancel (closeWorkspace) of a creating record + immediate reopen runs a FRESH open, and the doomed open can't nuke the reopen", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    // Open 1's Phase A (create) is held (and later rejected) to model a slow
    // git-abort; open 2 resolves cleanly. (RF-perf-14: the in-flight, cancellable
    // call for the interactive path is now createWorktree.)
    let setupCall = 0;
    let rejectSetup1!: (e: unknown) => void;
    let signalSetup1!: () => void;
    const setup1Called = new Promise<void>((res) => (signalSetup1 = res));
    createMock.mockImplementation(() => {
      setupCall += 1;
      if (setupCall === 1) {
        return new Promise((_res, rej) => {
          rejectSetup1 = rej;
          signalSetup1();
        });
      }
      return Promise.resolve({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false });
    });

    // Open 1: probe passes → "creating" record lands → setup in-flight.
    const p1 = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch((e: unknown) => e);
    await setup1Called;
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "creating",
    );

    // In-tile Cancel → closeWorkspace: removes the record, aborts, evicts the dedup entry.
    useViewerStore.getState().closeWorkspace(repo, storyId);
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId),
    ).toBeUndefined();

    // Immediate reopen must be FRESH (not piggyback the doomed p1) → actually opens.
    const onCreating2 = vi.fn();
    const cwd2 = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: onCreating2 });
    expect(cwd2).toBe("/wt/feat-34-9-worktree-gate");
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
    expect(onCreating2).toHaveBeenCalled();

    // The doomed open 1's setup finally rejects `cancelled` (the slow git-abort).
    // Its ownership-guarded cancelled-catch must NOT tear down the live reopen.
    rejectSetup1(Object.assign(new Error("aborted"), { failedStep: "cancelled" }));
    await p1;
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });

  it("review r3: a reopen after a mid-probe closeTeam runs a FRESH open (no dead-click piggyback on the doomed promise)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    // Open 1 held at findWorktreeOnDisk (the multi-second probe window).
    let resolveFind1!: (v: unknown) => void;
    findOnDiskMock.mockReset().mockReturnValueOnce(
      new Promise((res) => {
        resolveFind1 = res;
      }),
    );
    const p1 = useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() })
      .catch((e: unknown) => e);
    await new Promise((r) => setTimeout(r, 0));
    expect(findOnDiskMock).toHaveBeenCalledTimes(1);
    findOnDiskMock.mockResolvedValue(null);

    // Team switch / eviction mid-probe. Reseed the team bucket closeTeam swept so
    // the reopen has somewhere to land (a switch-back re-opens the team).
    useViewerStore.getState().closeTeam(repo);
    useViewerStore.setState((s) => ({
      workspaces: { ...s.workspaces, [repo]: [] },
      openedTeams: [repo],
    }));

    // Reopen the same story — must be FRESH, not piggyback the doomed p1.
    const cwd2 = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    expect(cwd2).toBe("/wt/feat-34-9-worktree-gate");
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );

    // Drain the doomed p1 — its ownership-guarded teardown leaves the reopen intact.
    resolveFind1(null);
    await p1;
    expect(useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe(
      "ready",
    );
  });

  it("review r2 (AC-4): two concurrent opens share ONE opening-flag entry (dedup, no double-set)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    let resolveSetup!: (v: { worktreePath: string; rehydrated: boolean }) => void;
    let signalSetup!: () => void;
    const setupCalled = new Promise<void>((res) => (signalSetup = res));
    createMock.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSetup = res;
          signalSetup();
        }),
    );

    const store = useViewerStore.getState();
    const p1 = store.ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    const p2 = store.ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: vi.fn() });
    await setupCalled;

    // Exactly one flag entry (second caller piggybacked before the flag point) and
    // exactly one create (dedup).
    expect([...(opening() ?? [])]).toEqual([storyId]);
    expect(createMock).toHaveBeenCalledTimes(1);

    resolveSetup({ worktreePath: "/wt/feat-34-9-worktree-gate", rehydrated: false });
    await Promise.all([p1, p2]);
    expect(opening()?.has(storyId) ?? false).toBe(false);
  });
});

const syntheticKey = "workspace:codesense:default:alderco/codesense";
const syntheticTeamKey = "codesense:default";
const syntheticStoryFile = "/ctx/stories/F-3-pipeline-doc-types.md";
const syntheticStoryId = "F-3-pipeline-doc-types";

describe("cleanupStoryWorktree — F-10 synthetic path resolution", () => {
  const toastErrorMock = toast.error as unknown as ReturnType<typeof vi.fn>;

  function seedSyntheticWorkspace() {
    // T.1: cleanupStoryWorktree's internal teamKey derivation reads
    // active org/team. Set org-store to match the synthetic key's
    // encoded `(codesense, default)` tuple so the workspace lookup hits.
    useOrgStore.setState({
      activeOrgSlug: "codesense",
      activeTeamName: "default",
    });
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [syntheticTeamKey]: [
          {
            id: "root",
            kind: "root" as const,
            cwd: "/wt/root",
            status: "ready" as const,
          },
          {
            id: syntheticStoryId,
            kind: "story" as const,
            cwd: "/wt/F-3",
            storyFilePath: syntheticStoryFile,
            storyId: syntheticStoryId,
            status: "ready" as const,
          },
        ],
      },
      openedTeams: [syntheticKey],
    }));
  }

  beforeEach(() => {
    reset();
    removeWorktreeMock.mockClear().mockResolvedValue(undefined);
    unpublishedMock.mockReset().mockResolvedValue({ count: 0, files: [] });
    resolveRepoMock.mockReset().mockReturnValue(undefined);
    toastErrorMock.mockClear();
  });

  // SW-XS-4 (M4): a removal that would discard unpublished context edits states
  // the count and waits for the explicit "Remove anyway" (then forces).
  it("SW-XS-4: holds a removal that would discard unpublished context edits until Remove anyway", async () => {
    const legacyRepoPath = "/repo";
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));
    unpublishedMock.mockResolvedValue({ count: 3, files: ["a.md", "b.md", "c.md"] });
    const warning = vi.mocked(toast.warning);
    warning.mockClear();

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    expect(removeWorktreeMock).not.toHaveBeenCalled();
    const [message, opts] = warning.mock.calls[0] as unknown as [
      string,
      { action: { label: string; onClick: () => void } },
    ];
    expect(message).toBe(
      "This worktree holds 3 unpublished context edits; removing it discards them.",
    );
    expect(opts.action.label).toBe("Remove anyway");
    opts.action.onClick();
    // A git-clean worktree: discards the context edits only — never `--force`.
    await vi.waitFor(() =>
      expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/34-9", false, true),
    );
  });

  it("SW-XS-4: Remove anyway on a worktree with uncommitted code asks again and only then forces", async () => {
    const legacyRepoPath = "/repo";
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));
    unpublishedMock.mockResolvedValue({ count: 1, files: ["a.md"] });
    vi.mocked(checkWorktreeDirty).mockResolvedValueOnce({ dirty: true });
    const warning = vi.mocked(toast.warning);
    warning.mockClear();

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);
    const first = warning.mock.calls[0][1] as unknown as { action: { onClick: () => void } };
    first.action.onClick();
    await vi.waitFor(() => expect(warning).toHaveBeenCalledTimes(2));
    expect(removeWorktreeMock).not.toHaveBeenCalled();
    const [message, second] = warning.mock.calls[1] as unknown as [
      string,
      { action: { label: string; onClick: () => void } },
    ];
    expect(message).toContain("uncommitted code changes");
    expect(second.action.label).toBe("Discard code changes too");
    second.action.onClick();
    await vi.waitFor(() =>
      expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/34-9", true, true),
    );
  });

  it("SW-XS-4: a failed unpublished check holds the removal and says it couldn't check", async () => {
    const legacyRepoPath = "/repo";
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));
    unpublishedMock.mockRejectedValue("install store unavailable");
    const warning = vi.mocked(toast.warning);
    warning.mockClear();

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    expect(removeWorktreeMock).not.toHaveBeenCalled();
    expect(warning.mock.calls[0][0]).toContain("couldn't check for unpublished context edits");
  });

  it("resolves synthetic repoPath to real path before calling removeWorktree (AC-1)", async () => {
    seedSyntheticWorkspace();
    resolveRepoMock.mockReturnValue({
      repoPath: syntheticKey,
      repoName: "codesense",
      contextRepoPath: "/ctx",
      hasRemote: true,
      remoteUrl: "https://github.com/alderco/codesense",
      mainDoc: "",
      localPath: "/Users/dev/codesense",
    });

    await useViewerStore.getState().cleanupStoryWorktree(syntheticKey, syntheticStoryFile);

    expect(removeWorktreeMock).toHaveBeenCalledWith(
      "/Users/dev/codesense",
      "/wt/F-3",
      false,
      false,
    );
  });

  it("shows toast error and skips removeWorktree when no local clone attached (AC-3)", async () => {
    seedSyntheticWorkspace();
    resolveRepoMock.mockReturnValue({
      repoPath: syntheticKey,
      repoName: "codesense",
      contextRepoPath: "/ctx",
      hasRemote: true,
      remoteUrl: "https://github.com/alderco/codesense",
      mainDoc: "",
      localPath: null,
    });

    await useViewerStore.getState().cleanupStoryWorktree(syntheticKey, syntheticStoryFile);

    expect(removeWorktreeMock).not.toHaveBeenCalled();
    expect(toastErrorMock).toHaveBeenCalledWith("No local clone attached — cannot remove worktree");
  });

  it("passes legacy repoPath through unchanged (AC-2)", async () => {
    // T.1: cleanupStoryWorktree's first arg stays a legacy repoPath used
    // by `resolveRealRepoPath` to find the source clone. The workspace
    // lookup is team-keyed (derived from active org/team).
    const legacyRepoPath = "/repo";
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    }));
    resolveRepoMock.mockReturnValue(undefined);

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/34-9", false, false);
  });
});

describe("ensureWorktree — progress callback", () => {
  beforeEach(reset);

  it("feeds setup progress phases into the workspace setupState (SLW-CO-5 AC-3 / SW-XS-10)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });

    const labelOf = () =>
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.setupState;
    const labelsSeen: unknown[] = [];
    setupMock.mockImplementation(async (opts: { onProgress?: (e: unknown) => void }) => {
      opts.onProgress?.({ phase: "copy", stepIndex: 0, stream: "stdout", line: "", tsMs: 1 });
      labelsSeen.push(labelOf());
      opts.onProgress?.({ phase: "setup", stepIndex: 0, stream: "stdout", line: "", tsMs: 2 });
      labelsSeen.push(labelOf());
      return { worktreePath: "/wt/feat-34-9-worktree-gate", results: [] };
    });

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    // The overlay's sub-label tracked the live phase while "creating"…
    expect(labelsSeen).toEqual([
      { kind: "step1", phase: "creatingWorktree" },
      { kind: "step1", phase: "runningSetup" },
    ]);
    // …then cleared once the workspace flipped to "ready".
    expect(labelOf()).toBeUndefined();
  });

  it("SW-WT-12 (S2): onStep drives the backgroundSetup checklist (seed → running → failed+exitCode)", async () => {
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({
      status: { kind: "present" },
      config: { version: 1, setup: ["npm ci", "cargo build"] },
    });

    // Hold the background setup open (never resolve) so the running-state badge
    // + checklist stay observable while we drive step events into the store.
    let captured: { onStep?: (e: unknown) => void } = {};
    runSetupPhaseMock.mockImplementation(
      (opts: { onStep?: (e: unknown) => void }) =>
        new Promise(() => {
          captured = opts;
        }),
    );

    // Decoupled path (onCreating, no awaitSetup) → backgrounded runWorktreeSetupPhase.
    await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: () => {} });

    // Narrow off the failed union member — this run stays in the running state
    // (the mock never resolves), so label + steps are always present here.
    const bg = () =>
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup as
        | {
            status: string;
            label?: string;
            steps?: Array<{
              index: number;
              command: string;
              status: string;
              exitCode?: number | null;
            }>;
          }
        | undefined;

    // The slot runs the backgrounded setup on a microtask; wait for onStep to arm.
    await vi.waitFor(() => expect(captured.onStep).toBeDefined());
    await vi.waitFor(() => expect(bg()?.steps?.length).toBe(2));

    // Rows are seeded queued from the probed setup: list; label reflects the S1 gate.
    expect(bg()?.steps?.[0]).toMatchObject({ index: 0, command: "npm ci", status: "queued" });
    expect(bg()?.steps?.[1]).toMatchObject({ index: 1, command: "cargo build", status: "queued" });
    expect(bg()?.label).toBe("Running setup…");

    // A started event flips the row to running…
    captured.onStep!({ kind: "started", phase: "setup", stepIndex: 0, command: "npm ci" });
    expect(bg()?.steps?.[0]?.status).toBe("running");

    // …and a failed finished event flips it to ✗ carrying the exit code.
    captured.onStep!({
      kind: "finished",
      phase: "setup",
      stepIndex: 0,
      passed: false,
      exitCode: 1,
    });
    expect(bg()?.steps?.[0]).toMatchObject({ status: "failed", exitCode: 1 });

    // A passing finished event flips its row to ✓ (the done branch).
    captured.onStep!({
      kind: "finished",
      phase: "setup",
      stepIndex: 1,
      passed: true,
      exitCode: 0,
    });
    expect(bg()?.steps?.[1]).toMatchObject({ status: "done", exitCode: 0 });

    // SW-WT-12 (S2): onProgress buffers per-step output for on-demand disclosure.
    (captured as { onProgress?: (e: unknown) => void }).onProgress?.({
      phase: "setup",
      stepIndex: 0,
      stream: "stdout",
      line: "installing deps",
      tsMs: 1,
    });
    expect(getBackgroundSetupStepOutput(storyId, 0)).toContain("installing deps");
  });
});

// ---------------------------------------------------------------------------
// F-17: cleanupStoryWorktree — on-disk fallback (no workspace entry)
// ---------------------------------------------------------------------------
describe("cleanupStoryWorktree — F-17 on-disk fallback", () => {
  const toastSuccessMock = toast.success as unknown as ReturnType<typeof vi.fn>;

  beforeEach(() => {
    reset();
    removeWorktreeMock.mockReset().mockResolvedValue(undefined);
    resolveRepoMock.mockReturnValue(undefined);
    toastSuccessMock.mockClear();
  });

  it("uses onDiskWorktrees cwd when no workspace entry exists (AC-2)", async () => {
    // T.4 D-3: cleanupStoryWorktree first arg is still legacy repoPath
    // (T.5 re-signs the entry point). Internally it derives teamKey from
    // useOrgStore and composes the composite inner key from slug + path.
    // For non-synthetic repoPath, slug === repoPath (fallback branch).
    const legacyRepoPath = "/repo";
    const composite = composeWorktreeKey(deriveSourceRepoSlug(legacyRepoPath), storyFile);
    useViewerStore.setState({
      onDiskWorktrees: {
        [repo]: new Map([[composite, { path: "/wt/orphan-34-9" }]]),
      },
    });

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/orphan-34-9", false, false);
    expect(toastSuccessMock).toHaveBeenCalledWith("Worktree removed", { id: "toast-loading-id" });
  });

  it("clears onDiskWorktrees entry after successful removal (AC-2)", async () => {
    const legacyRepoPath = "/repo";
    const composite = composeWorktreeKey(deriveSourceRepoSlug(legacyRepoPath), storyFile);
    useViewerStore.setState({
      onDiskWorktrees: {
        [repo]: new Map([[composite, { path: "/wt/orphan-34-9" }]]),
      },
    });

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    const map = useViewerStore.getState().onDiskWorktrees[repo];
    expect(map.has(composite)).toBe(false);
  });

  it("clears onDiskWorktrees entry when workspace entry EXISTS (AC-3)", async () => {
    const legacyRepoPath = "/repo";
    const composite = composeWorktreeKey(deriveSourceRepoSlug(legacyRepoPath), storyFile);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
      onDiskWorktrees: {
        [repo]: new Map([[composite, { path: "/wt/34-9" }]]),
      },
    }));

    await useViewerStore.getState().cleanupStoryWorktree(legacyRepoPath, storyFile);

    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/34-9", false, false);
    // T.4 D-3: on-disk entry cleared via composite key in the team-keyed bucket.
    const map = useViewerStore.getState().onDiskWorktrees[repo];
    expect(map.has(composite)).toBe(false);
    // Workspace entry also closed (team-keyed bucket).
    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.find((w) => w.id === storyId)).toBeUndefined();
  });

  it("cross-team: synthetic repoPath derives a teamKey different from active and cleanup hits THAT bucket (Round 4 Patch 5)", async () => {
    // T.4 Round 4 (Blind Hunter #2): the AC-3 legacy-path test above passes
    // for the wrong reason — it relies on the active-team fallback
    // (`getActiveTeamKey()`) coincidentally matching the seeded bucket. This
    // test exercises the actual synthetic-parsing path: the repoPath arg
    // parses to a teamKey ≠ active team, and cleanup must hit that derived
    // bucket, not the active one.
    const activeTeam = repo; // "test-org:default" (active per `reset()`)
    const otherSynthetic = "workspace:other-org:design:owner/repo";
    const otherTeamKey = "other-org:design";
    const otherSlug = deriveSourceRepoSlug(otherSynthetic); // "owner/repo"
    const otherStoryFile = "/ctx/stories/cross-team-story.md";
    const otherStoryId = "cross-team-story";
    const otherComposite = composeWorktreeKey(otherSlug, otherStoryFile);

    // Seed an active-team bucket with a sentinel entry that MUST survive —
    // a regression that derived teamKey from active state instead of from
    // the synthetic arg would hit this bucket and wipe the sentinel.
    const sentinelComposite = composeWorktreeKey(
      deriveSourceRepoSlug("/sentinel-repo"),
      "/ctx/stories/sentinel.md",
    );

    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [activeTeam]: [
          ...s.workspaces[activeTeam],
          {
            id: "sentinel-active",
            kind: "story" as const,
            cwd: "/wt/sentinel-active",
            storyFilePath: "/ctx/stories/sentinel.md",
            storyId: "sentinel-active",
            status: "ready" as const,
          },
        ],
        [otherTeamKey]: [
          {
            id: otherStoryId,
            kind: "story" as const,
            cwd: "/wt/other-team",
            storyFilePath: otherStoryFile,
            storyId: otherStoryId,
            status: "ready" as const,
            sourceRepoSlug: otherSlug,
            // Seed sourceRepoPath so realPath resolution doesn't fall
            // through to resolveRealRepoPath (which would return null
            // without a workspaceRepos entry for the synthetic).
            sourceRepoPath: "/clones/owner-repo",
          },
        ],
      },
      onDiskWorktrees: {
        [activeTeam]: new Map([[sentinelComposite, { path: "/wt/sentinel-active" }]]),
        [otherTeamKey]: new Map([[otherComposite, { path: "/wt/other-team" }]]),
      },
    }));

    await useViewerStore.getState().cleanupStoryWorktree(otherSynthetic, otherStoryFile);

    // The OTHER team's bucket is cleaned.
    expect(useViewerStore.getState().onDiskWorktrees[otherTeamKey]?.has(otherComposite)).toBe(
      false,
    );
    expect(
      useViewerStore.getState().workspaces[otherTeamKey]?.find((w) => w.id === otherStoryId),
    ).toBeUndefined();
    // The active team's bucket is UNTOUCHED — proves the cleanup did NOT
    // fall back to active-team for the bucket lookup.
    expect(
      useViewerStore.getState().onDiskWorktrees[activeTeam]?.get(sentinelComposite)?.path,
    ).toBe("/wt/sentinel-active");
    expect(
      useViewerStore.getState().workspaces[activeTeam]?.find((w) => w.id === "sentinel-active"),
    ).toBeDefined();
  });

  it("bails silently when neither workspace nor on-disk entry exists (AC-4)", async () => {
    await useViewerStore.getState().cleanupStoryWorktree(repo, storyFile);

    expect(removeWorktreeMock).not.toHaveBeenCalled();
    expect(toastSuccessMock).not.toHaveBeenCalled();
  });
});

// ===========================================================================
// M.2 — source_repo resolution from frontmatter
// ===========================================================================

const crossRepoSlug = "acme/api";
const crossRepoKey = "workspace:9north:default:acme/api";
const crossRepoLocalPath = "/clones/acme-api";
const crossRepoState = {
  repoPath: crossRepoKey,
  repoName: "api (9North Studio)",
  contextRepoPath: "/ctx/acme/api",
  hasRemote: true,
  remoteUrl: null,
  mainDoc: "",
  localPath: crossRepoLocalPath,
};

describe("ensureWorktree — M.2 source_repo resolution", () => {
  beforeEach(() => {
    reset();
    // Standard create-path mocks for the slow path.
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue(undefined);
  });

  it("AC-1: creates worktree in source_repo's clone when frontmatter declares source_repo", async () => {
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    // setupWorktree should use the source repo's localPath, not the context repo.
    expect(setupMock).toHaveBeenCalledWith(
      expect.objectContaining({ repoPath: crossRepoLocalPath }),
    );
    // T.4 Round 5 TP2: assert the slug lookup is scoped to the active
    // team. parseTeamKey("test-org:default") → {orgSlug: "test-org",
    // teamName: "default"}; production threads that through to
    // findWorkspaceRepoBySlug as the second arg so cross-team slug
    // collisions can't resolve to a foreign team's clone. Without this
    // assertion, a regression that dropped the second arg (or passed
    // the wrong team) would silently land here — the mock ignores args
    // and still returns crossRepoState.
    expect(findRepoBySlugMock).toHaveBeenCalledWith(crossRepoSlug, {
      orgSlug: "test-org",
      teamName: "default",
    });
  });

  it("AC-2 (T.5 fallback): falls back to team's first source repo when no source_repo frontmatter", async () => {
    // parseSourceRepoMock defaults to null (no source_repo). Post-T.5 the
    // fallback resolves via the team's first tracked repo, not the panel
    // repoPath. The workspaceReposForTeam mock returns
    // `{ repoPath: "workspace:test-org:default:test/repo", localPath:
    // "/test/repo" }`, so setup runs against `/test/repo`.
    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalledWith(
      expect.objectContaining({ repoPath: teamFallbackSourcePath }),
    );
    // No frontmatter slug → findWorkspaceRepoBySlug is not consulted.
    expect(findRepoBySlugMock).not.toHaveBeenCalled();
  });

  it("T.5 D-1 (iter-1 #F1): throws WorktreeTeamMismatchError(slug-not-in-team) and toasts at the throw site", async () => {
    parseSourceRepoMock.mockReturnValue("unknown/repo");
    // findRepoBySlugMock defaults to undefined (no match).

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile)
      .catch((e: unknown) => e);
    expect(err).toMatchObject({
      failedStep: "team-mismatch",
      reason: "slug-not-in-team",
      slug: "unknown/repo",
      teamKey: repo,
    });
    expect((err as Error).name).toBe("WorktreeTeamMismatchError");
    expect(setupMock).not.toHaveBeenCalled();
    // iter-1 #F1: toast.error is emitted at the throw site (HANDLED_FAILED_STEPS
    // swallows the error, so no caller-side toast fires — the toast MUST
    // come from the action itself).
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("unknown/repo"));
  });

  it("T.5 D-1 (iter-1 #F20): findWorkspaceRepoBySlug is invoked with parseTeamKey(teamKey), not the active team", async () => {
    // Round 5 P3 regression guard — ensure the lookup is scoped to the
    // teamKey arg, not whatever getActiveTeamKey returns. Set the active
    // team to a DIFFERENT team than the one we're passing.
    parseSourceRepoMock.mockReturnValue("acme/api");
    findRepoBySlugMock.mockReturnValue(undefined);
    useOrgStore.setState({
      activeOrgSlug: "wrong-org",
      activeTeamName: "wrong-team",
    });

    await useViewerStore
      .getState()
      .ensureWorktree("test-org:default", storyFile)
      .catch(() => undefined);

    expect(findRepoBySlugMock).toHaveBeenCalledWith("acme/api", {
      orgSlug: "test-org",
      teamName: "default",
    });
  });

  it("T.5 D-1 (iter-1 #F1): throws WorktreeTeamMismatchError(missing-source-repo) and toasts at the throw site", async () => {
    // parseSourceRepoMock defaults to null (no source_repo). Override the
    // team-repos lookup to return an empty list — the fallback path has
    // nothing to land on and must throw the typed mismatch error.
    const { workspaceReposForTeam } = await import("@/shared/lib/build-synthetic-repo");
    const workspaceReposForTeamMock = workspaceReposForTeam as unknown as ReturnType<typeof vi.fn>;
    workspaceReposForTeamMock.mockReturnValueOnce([]);

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile)
      .catch((e: unknown) => e);
    expect(err).toMatchObject({
      failedStep: "team-mismatch",
      reason: "missing-source-repo",
      slug: null,
      teamKey: repo,
    });
    expect((err as Error).name).toBe("WorktreeTeamMismatchError");
    expect(setupMock).not.toHaveBeenCalled();
    // iter-1 #F1: toast.error fires at the throw site with actionable text.
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("no source repos"));
  });

  it("SW-WT-1: missing config with frontmatter source_repo PROCEEDS against that repo's clone", async () => {
    // The config-optional tests above exercise the no-frontmatter fallback.
    // This sibling covers the frontmatter-present branch: a missing config
    // is calm and setup runs against the resolved storyRepo's clone, not a
    // setup-required modal.
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "missing" }, config: null });
    setupMock.mockResolvedValue({ worktreePath: "/wt/feat-34-9-worktree-gate", results: [] });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalledWith(
      expect.objectContaining({ repoPath: crossRepoLocalPath }),
    );
  });

  it("T.5 D-1: WorktreeTeamMismatchError is included in HANDLED_FAILED_STEPS (openOrFocusStoryWorkspace swallow path)", async () => {
    // The orchestrator's catch path checks failedStep against
    // HANDLED_FAILED_STEPS; mismatch errors should be swallowed AT THE
    // ORCHESTRATOR LAYER because the action itself already toasted at
    // the throw site (iter-1 #F1). This keeps the user-feedback path
    // single-emit while preserving the orchestrator's "don't double-toast"
    // contract.
    parseSourceRepoMock.mockReturnValue("unknown/repo");
    // Clear the cross-test mock state so the "exactly one toast" assertion
    // below counts only calls from THIS invocation.
    (toast.error as unknown as ReturnType<typeof vi.fn>).mockClear();

    // Should NOT throw — openOrFocusStoryWorkspace swallows team-mismatch.
    // SW-FX-41: the action now returns the resolved worktree cwd (Promise<string>);
    // a swallowed pre-create error resolves to "" (no cwd resolved), not undefined.
    await expect(
      useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile),
    ).resolves.toBe("");
    expect(setupMock).not.toHaveBeenCalled();
    // iter-1 #F1: action-emitted toast still fires once (not zero, not twice).
    expect(toast.error).toHaveBeenCalledTimes(1);
  });

  it("AC-4: shows no-clone modal for tracked source_repo with no local clone", async () => {
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue({ ...crossRepoState, localPath: null });

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toMatchObject({
      failedStep: "no-clone",
    });

    // T.4 AC-3: noClonePromptRepo is `{teamKey, sourceRepoSlug}`.
    // crossRepoKey = `workspace:9north:default:acme/api` → teamKey "9north:default",
    // sourceRepoSlug "acme/api".
    expect(useViewerStore.getState().noClonePromptRepo).toEqual({
      teamKey: "9north:default",
      sourceRepoSlug: "acme/api",
    });
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("AC-5: stores sourceRepoPath on workspace for downstream cleanup", async () => {
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws?.sourceRepoPath).toBe(crossRepoLocalPath);
  });

  it("M.3: stores sourceRepoSlug on workspace when frontmatter declares source_repo", async () => {
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws?.sourceRepoSlug).toBe(crossRepoSlug);
  });

  it("M.3: sourceRepoSlug is undefined when no source_repo declared", async () => {
    // parseSourceRepoMock defaults to null (no source_repo).
    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws?.sourceRepoSlug).toBe(teamFallbackSlug);
  });

  it("AC-7: cleanupStoryWorktree uses stored sourceRepoPath", async () => {
    // Manually seed a workspace with sourceRepoPath.
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/34-9",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
            sourceRepoPath: crossRepoLocalPath,
          },
        ],
      },
    }));

    await useViewerStore.getState().cleanupStoryWorktree(repo, storyFile);

    // removeWorktree should be called with the stored sourceRepoPath.
    expect(removeWorktreeMock).toHaveBeenCalledWith(crossRepoLocalPath, "/wt/34-9", false, false);
  });

  it("AC-8: reads story file exactly once in the slow path", async () => {
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);
    readFileMock.mockClear();

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    // readFile should be called exactly once for the story file.
    const storyFileReads = readFileMock.mock.calls.filter(
      (args: unknown[]) => args[0] === storyFile,
    );
    expect(storyFileReads).toHaveLength(1);
  });

  it("T.4 Round 5 TP2 / T.5: cross-team slug collision — same slug in two teams resolves to the caller's team", async () => {
    // Per Round 5 P3, teams can independently track the same source
    // repository (e.g. `acme/api` is in both team A and team B). The
    // re-signed findWorkspaceRepoBySlug filters by team, so two
    // ensureWorktree calls with the SAME sourceRepoSlug but DIFFERENT
    // teamKeys must resolve to each team's own clone.
    //
    // T.5: callers now pass `teamKey` directly (e.g. "9north:team-a") instead
    // of a synthetic repoPath; the action threads teamKey into
    // findWorkspaceRepoBySlug as the second arg.
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);

    const teamARepoState = {
      ...crossRepoState,
      repoPath: "workspace:9north:team-a:acme/api",
      localPath: "/clones/team-a-api",
    };
    const teamBRepoState = {
      ...crossRepoState,
      repoPath: "workspace:9north:team-b:acme/api",
      localPath: "/clones/team-b-api",
    };

    findRepoBySlugMock.mockImplementation(
      (_slug: string, team: { orgSlug: string; teamName: string | null }) => {
        if (team.orgSlug === "9north" && team.teamName === "team-a") return teamARepoState;
        if (team.orgSlug === "9north" && team.teamName === "team-b") return teamBRepoState;
        return undefined;
      },
    );

    // Team A call — uses team A's clone.
    setupMock.mockClear();
    await useViewerStore.getState().ensureWorktree("9north:team-a", "/stories/shared-story.md");
    expect(setupMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ repoPath: "/clones/team-a-api" }),
    );

    // Team B call — uses team B's clone, NOT team A's. Without P3's
    // team-scope filter, both calls would resolve to whichever insertion
    // order put first in workspaceRepos.
    setupMock.mockClear();
    await useViewerStore.getState().ensureWorktree("9north:team-b", "/stories/shared-story.md");
    expect(setupMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ repoPath: "/clones/team-b-api" }),
    );

    // Both calls threaded the correct team into findWorkspaceRepoBySlug.
    expect(findRepoBySlugMock).toHaveBeenCalledWith(crossRepoSlug, {
      orgSlug: "9north",
      teamName: "team-a",
    });
    expect(findRepoBySlugMock).toHaveBeenCalledWith(crossRepoSlug, {
      orgSlug: "9north",
      teamName: "team-b",
    });
  });

  // T-13: reserved reasons (slug-stale-after-removal, frontmatter-rewritten)
  // disambiguated inside the single lookup-failure throw site (D-2 ladder).

  it("T-13 AC-1: throws slug-stale-after-removal when the slug was recently removed from the team", async () => {
    parseSourceRepoMock.mockReturnValue("removed/repo");
    findRepoBySlugMock.mockReturnValue(undefined);
    useOrgStore.setState({
      recentlyRemovedRepos: { "test-org:default": new Set(["removed/repo"]) },
    });

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile)
      .catch((e: unknown) => e);
    expect(err).toMatchObject({
      failedStep: "team-mismatch",
      reason: "slug-stale-after-removal",
      slug: "removed/repo",
      teamKey: repo,
    });
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("was removed from this team"));
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("T-13 AC-2: throws frontmatter-rewritten when expectedSlug differs from the current slug", async () => {
    parseSourceRepoMock.mockReturnValue("new/slug");
    findRepoBySlugMock.mockReturnValue(undefined);

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, "old/slug")
      .catch((e: unknown) => e);
    expect(err).toMatchObject({
      failedStep: "team-mismatch",
      reason: "frontmatter-rewritten",
      teamKey: repo,
    });
    // D-3: the rewritten toast carries a single "Reload pipeline" action.
    expect(toast.error).toHaveBeenCalledWith(
      expect.stringContaining("was rewritten"),
      expect.objectContaining({
        action: expect.objectContaining({ label: "Reload pipeline" }),
      }),
    );
  });

  it("T-13 D-2: frontmatter-rewritten takes precedence over stale-after-removal", async () => {
    parseSourceRepoMock.mockReturnValue("new/slug");
    findRepoBySlugMock.mockReturnValue(undefined);
    // Even with the current slug flagged as recently-removed, a rewrite
    // (expectedSlug !== current) is the more specific cause and wins.
    useOrgStore.setState({
      recentlyRemovedRepos: { "test-org:default": new Set(["new/slug"]) },
    });

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, "old/slug")
      .catch((e: unknown) => e);
    expect(err).toMatchObject({ reason: "frontmatter-rewritten" });
  });

  it("T-13 D-2: stale-after-removal fires when expectedSlug matches current (no rewrite)", async () => {
    parseSourceRepoMock.mockReturnValue("removed/repo");
    findRepoBySlugMock.mockReturnValue(undefined);
    useOrgStore.setState({
      recentlyRemovedRepos: { "test-org:default": new Set(["removed/repo"]) },
    });

    // expectedSlug equals current → no rewrite; removal disambiguation wins
    // over the generic slug-not-in-team.
    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, "removed/repo")
      .catch((e: unknown) => e);
    expect(err).toMatchObject({ reason: "slug-stale-after-removal" });
  });

  it("T-13 AC-2: does NOT throw when the (rewritten) current slug is still tracked", async () => {
    // current slug differs from expectedSlug, but the current slug resolves —
    // the action worked, so it must open silently against the current repo.
    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);

    const cwd = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, "old/slug");

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();
  });

  it("T-13 AC-3: still throws slug-not-in-team when no rewrite and not recently removed", async () => {
    parseSourceRepoMock.mockReturnValue("unknown/repo");
    findRepoBySlugMock.mockReturnValue(undefined);

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile)
      .catch((e: unknown) => e);
    expect(err).toMatchObject({ reason: "slug-not-in-team", slug: "unknown/repo" });
  });

  it("T-13 D-2: rewrite check is case-insensitive (case-only diff is not a rewrite)", async () => {
    // The frontmatter slug differs from expectedSlug only in case — the
    // mapping is unchanged, so this must NOT report frontmatter-rewritten.
    parseSourceRepoMock.mockReturnValue("Acme/Api");
    findRepoBySlugMock.mockReturnValue(undefined);

    const err = await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, "acme/api")
      .catch((e: unknown) => e);
    expect(err).toMatchObject({ reason: "slug-not-in-team" });
  });
});

// ===========================================================================
// F-29 — Pre-flight branch guard
// ===========================================================================

describe("ensureWorktree — F-29 pre-flight branch guard", () => {
  beforeEach(() => {
    reset();
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue({
      worktreePath: "/wt/feat-34-9-worktree-gate",
      results: [],
    });
  });

  it("throws Blocked (naming the dep branch) and skips setup when dep branch doesn't exist", async () => {
    // resolveWorktreeParams returns a dep-derived start point
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "feat/dep-branch",
    });
    checkBranchExistsMock.mockResolvedValue(false);

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      'Blocked — dependency branch "feat/dep-branch" hasn\'t been created yet',
    );
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("proceeds normally when dep branch exists", async () => {
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "feat/dep-branch",
    });
    checkBranchExistsMock.mockResolvedValue(true);

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(setupMock).toHaveBeenCalled();
  });

  it("skips pre-flight check when start point equals default (no deps)", async () => {
    // Default resolveWorktreeParams mock returns startPoint: null (no configured base)
    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    // checkBranchExists should NOT be called when startPoint === defaultStart
    expect(checkBranchExistsMock).not.toHaveBeenCalled();
  });

  // #2840: with no worktree.yaml the open sends no start point and no default (the
  // backend resolves origin/HEAD for a new branch), never an assumed `main`, and the
  // F-29 dependency guard does not mistake the missing base for a dependency branch.
  it("#2840: with no configured base, opens with no start point and no default", async () => {
    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(checkBranchExistsMock).not.toHaveBeenCalled();
    expect(setupMock).toHaveBeenCalledWith(
      expect.objectContaining({ startPoint: null, defaultStartPoint: null }),
    );
  });

  // #2840: a configured `develop` / `master` that exists only as `origin/<name>` (or
  // whose local branch is stale) goes to the backend as the configured default, which
  // prefers the remote ref; sent as the start point it was used verbatim and failed
  // with "invalid reference".
  it.each(["develop", "master"])(
    "#2840: a configured default of %s goes to the backend to resolve, not as the start point",
    async (configured) => {
      probeMock.mockResolvedValue({
        status: { kind: "present" },
        config: { ...baseConfig, default_start_point: configured },
      });
      resolveWorktreeParamsMock.mockResolvedValue({
        branchName: "feat/34-9-worktree-gate",
        startPoint: configured,
      });

      await useViewerStore.getState().ensureWorktree(repo, storyFile);

      expect(checkBranchExistsMock).not.toHaveBeenCalled();
      expect(setupMock).toHaveBeenCalledWith(
        expect.objectContaining({ startPoint: null, defaultStartPoint: configured }),
      );
    },
  );

  it("#2840: the backend's refusal, naming every ref it tried, is the open's error", async () => {
    const refusal =
      "unavailable: can't start a new story branch in /repo: no default branch found. Tried refs/remotes/origin/HEAD (→ develop), refs/remotes/origin/develop, refs/heads/develop. Run `git remote set-head origin --auto` in /repo, or set `default_start_point` in the repo's worktree.yaml (an agent can use write_worktree_config), then retry.";
    setupMock.mockRejectedValue(
      Object.assign(new Error(refusal), { failedStep: "create", recoverable: false }),
    );

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      refusal,
    );
  });

  it("skips pre-flight check when overrideStartPoint is provided", async () => {
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "feat/dep-branch",
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile, "main");

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    expect(checkBranchExistsMock).not.toHaveBeenCalled();
    // setupWorktree should be called with the override start point
    expect(setupMock).toHaveBeenCalledWith(expect.objectContaining({ startPoint: "main" }));
  });

  // SW-FX-41 (AC-1/AC-2): "Create from default" must OPEN the worktree it makes,
  // not just create it. It now DELEGATES to `openOrFocusStoryWorkspace` (passing
  // the default branch as `overrideStartPoint`), so the new workspace is FOCUSED
  // (`activeWorkspace`) and a terminal SPAWNS in it — via the shared `onCreating`
  // hook + `finishForegroundOpen` tail, the SAME path a normal open uses (no
  // second branch that can forget it). RED before this fix: the old create-only body
  // called `ensureWorktree` directly and returned the cwd UNOPENED — neither
  // `activeWorkspace` nor a terminal tab was set, so the user landed in `$HOME`.
  it("an open with a default-branch override focuses the new workspace and spawns a terminal in it", async () => {
    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile, undefined, "main");

    // AC-1: the created worktree is now the ACTIVE (focused) workspace.
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(storyId);
    // AC-1: a terminal was spawned in it (auto-launched first tab).
    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, storyId);
    expect(tabs.length).toBeGreaterThan(0);
    // The workspace record carries the RESOLVED worktree cwd — so the spawned
    // shell chdir's there, never "" / `$HOME`.
    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws?.cwd).toBe("/wt/feat-34-9-worktree-gate");
  });

  // SW-GW-8 (AC-1): opening the new worktree folds its session into the SAME
  // trusted registry a dashboard launch does — under the storyId handle, CAP-FREE,
  // with "codesense" provenance — so a nav-opened story appears in the Background
  // agents panel (closing the SW-GW-3 split; mirrors the epic register path).
  it("SW-GW-8 AC-1: the nav open registers its session under storyId with codesense provenance", async () => {
    registerSessionMock.mockClear();

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile, undefined, "main");

    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, storyId);
    const sessionId = (tabs.find((t) => t.role) ?? tabs[0])?.sessionId;
    expect(sessionId).toBeTruthy();
    // storyId handle = the SAME key a dashboard `openWorkspace({storyId})` main
    // session binds → the two collapse to one panel row.
    expect(registerSessionMock).toHaveBeenCalledWith(storyId, sessionId, "codesense", repo);
  });

  // SW-FX-41 (review, Edge Case Hunter MAJOR): delegating the open means a
  // HANDLED create/setup failure is SWALLOWED by openOrFocusStoryWorkspace
  // (already toasted at the throw site) → it resolves "". The Create-from-default caller
  // must propagate that "" (not a real cwd, not a throw) so its handler
  // suppresses the green "Started from …" success toast — otherwise that toast
  // fires OVER a workspace showing an error state (contradictory UX).
  it("an open with a default-branch override resolves '' when it swallows a handled create failure", async () => {
    // RF-perf-14: a `setup` failure now backgrounds (doesn't fail the open), so
    // the failure that makes the delegated open fail + swallow is a Phase A
    // (create) failure — still in HANDLED_FAILED_STEPS, still resolves "".
    createMock.mockRejectedValue(
      Object.assign(new Error("git worktree add failed"), { failedStep: "create" }),
    );

    await expect(
      useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile, undefined, "main"),
    ).resolves.toBe("");
  });

  // SW-FX-41 (review r2, Edge Case Hunter MAJOR): the early-delegate branch
  // (story's source_repo NOT tracked by the team) now routes through
  // `openOrFocusStoryWorkspace` too — the ONE focus+spawn path (AC-2) — instead
  // of a bare `ensureWorktree`. On the normal mismatch it swallows the handled
  // `team-mismatch` error and resolves "" (mismatch toast still fires at the
  // throw site), so the handler suppresses its success toast; on the rare
  // slug-re-added race the worktree would be FOCUSED + spawned, not created
  // unopened (the exact SW-FX-41 defect the bare call left on this path).
  it("an open with a default-branch override (slug-not-in-team) resolves '' and surfaces the mismatch, no success", async () => {
    parseSourceRepoMock.mockReturnValue("unknown/repo");
    findRepoBySlugMock.mockReturnValue(undefined);
    (toast.error as unknown as ReturnType<typeof vi.fn>).mockClear();

    await expect(
      useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile, undefined, "main"),
    ).resolves.toBe("");
    // The mismatch error is surfaced (toast at the throw site), and NO worktree
    // setup ran — so the handler's cwd is "" and no "Started from …" fires.
    expect(setupMock).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalled();
  });

  it("blocked error has failedStep 'blocked-dep' for error handling", async () => {
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "feat/dep-branch",
    });
    checkBranchExistsMock.mockResolvedValue(false);

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toMatchObject({
      failedStep: "blocked-dep",
    });
  });

  // SW-FX-3 AC-4: the legacy worktree.yaml `default_start_point` (e.g. "dev"
  // for a repo whose default branch isn't "main") must plumb from
  // getWorktreeConfig through viewer-store into `defaultStartPoint`. The
  // resolver itself is already proven to honor the field (resolve.test.ts);
  // this locks the store-side link the live AllWidgets failure exposed,
  // WITHOUT depending on that external repo/machine.
  it("AC-4: config.default_start_point plumbs into defaultStartPoint (F-29 guard compares against it)", async () => {
    probeMock.mockResolvedValue({
      status: { kind: "present" },
      config: { ...baseConfig, default_start_point: "dev" },
    });
    // A start point of "main" is NOT the configured default ("dev"), so the F-29
    // guard must fire — it would be skipped if the default were the hardcoded "main".
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "main",
    });
    checkBranchExistsMock.mockResolvedValue(false);

    await expect(useViewerStore.getState().ensureWorktree(repo, storyFile)).rejects.toThrow(
      'Blocked — dependency branch "main"',
    );
    expect(checkBranchExistsMock).toHaveBeenCalledTimes(1);
  });

  it("AC-4: no-deps path uses default_start_point as the worktree start point", async () => {
    probeMock.mockResolvedValue({
      status: { kind: "present" },
      config: { ...baseConfig, default_start_point: "dev" },
    });
    // Deps absent → resolver returns startPoint === defaultStart ("dev").
    resolveWorktreeParamsMock.mockResolvedValue({
      branchName: "feat/34-9-worktree-gate",
      startPoint: "dev",
    });

    const cwd = await useViewerStore.getState().ensureWorktree(repo, storyFile);

    expect(cwd).toBe("/wt/feat-34-9-worktree-gate");
    // startPoint === defaultStartPoint ("dev") → pre-flight guard skipped, and "dev"
    // goes to the backend as the configured default (resolved like the MCP open, its
    // cached `origin/dev` first), not as a verbatim start point.
    expect(checkBranchExistsMock).not.toHaveBeenCalled();
    expect(setupMock).toHaveBeenCalledWith(
      expect.objectContaining({ startPoint: null, defaultStartPoint: "dev" }),
    );
  });
});

describe("ensureWorktree — R-18 storyId from frontmatter", () => {
  beforeEach(() => {
    reset();
    parseStoryIdMock.mockReset();
    findOnDiskMock.mockReset();
    setupMock.mockReset();
  });

  it("AC #1/#2: sets Workspace.storyId from frontmatter when present", async () => {
    parseStoryIdMock.mockReturnValue("R-18");
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue(undefined);

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws!.id).toBe(storyId); // identity = path-derived workspaceId
    expect(ws!.storyId).toBe("R-18"); // display = frontmatter value
  });

  it("AC #21: falls back to path-derived storyId when frontmatter is absent", async () => {
    parseStoryIdMock.mockReturnValue(null);
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue(undefined);

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws!.id).toBe(storyId);
    // Path-derived fallback: storyId === workspaceId when no frontmatter.
    expect(ws!.storyId).toBe(storyId);
  });

  it("AC #22: two distinct files with same story_id create distinct workspaces", async () => {
    // Both files declare `story_id: R-100` in frontmatter but live at
    // different paths — identity (`id`) is path-derived so they must NOT
    // collide. Display label (`storyId`) is shared and intentional.
    parseStoryIdMock.mockReturnValue("R-100");
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue(undefined);

    const fileA = "/repo/stories/R-100-alpha.md";
    const fileB = "/repo/stories/R-100-beta.md";

    await useViewerStore.getState().ensureWorktree(repo, fileA);
    await useViewerStore.getState().ensureWorktree(repo, fileB);

    const ws = useViewerStore.getState().workspaces[repo] ?? [];
    const a = ws.find((w) => w.id === "R-100-alpha");
    const b = ws.find((w) => w.id === "R-100-beta");
    expect(a).toBeDefined();
    expect(b).toBeDefined();
    expect(a!.id).not.toBe(b!.id);
    expect(a!.storyId).toBe("R-100");
    expect(b!.storyId).toBe("R-100");
  });

  it("AC #2: on-disk adoption path also picks up frontmatter storyId", async () => {
    parseStoryIdMock.mockReturnValue("R-18");
    findOnDiskMock.mockResolvedValue({
      path: "/wt/on-disk",
      branch: "refs/heads/feat/34-9-worktree-gate",
      head_sha: "abc",
    });

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws!.cwd).toBe("/wt/on-disk");
    expect(ws!.storyId).toBe("R-18");
  });

  // Code-review P12 (round-2 R7 rename): parser-returned storyId flows
  // through `ensureWorktree`'s non-on-disk branch into Workspace.storyId.
  // The parser itself is mocked at module scope (line ~52), so the test
  // exercises the wiring: parser-output → addStoryWorkspace param →
  // Workspace.storyId. Real HTML/YAML content handling is covered by
  // the `parseStoryIdFromFrontmatter` unit tests in `resolve.test.ts`.
  it("P12: parser-returned storyId flows into Workspace.storyId on the non-on-disk path", async () => {
    parseStoryIdMock.mockReturnValue("R-18-html");
    findOnDiskMock.mockResolvedValue(null);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    setupMock.mockResolvedValue(undefined);

    await useViewerStore.getState().ensureWorktree(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws!.storyId).toBe("R-18-html");
  });
});

// T.5 (Phase 10 review iter-2): regression tests for the team-keyed
// entry points. The first arg is now a teamKey directly — verify the
// workspace work lands in the team named by the arg, independent of the
// active team. `cleanupStoryWorktree` still takes a legacy synthetic
// repoPath (out of T.5 scope) so that branch keeps the SYN_* fixture.
describe("ensureWorktree / cleanupStoryWorktree — team bucket isolation (Phase 10 MED #6)", () => {
  const TEAM_A_KEY = "test-org:default";
  const TEAM_B_KEY = "test-org:other";
  const SYN_TEAM_B_REPO = "workspace:test-org:other:acme/api";

  beforeEach(() => {
    reset();
    // Active team is "default" (set by reset()); TEAM_B_KEY is a
    // different team. Uses the store-hit pattern (pre-seeded ready
    // workspace) so we don't have to wire the full no-clone /
    // source-repo resolution stack.
  });

  it("ensureWorktree(teamKey for team B): store-hit read uses team B's bucket, not active team A", async () => {
    // Pre-seed a ready workspace under team B's teamKey. ensureWorktree's
    // store-hit path reads `workspaces[teamKey]`; if the bucket key were
    // somehow defaulted to the active team, the lookup would miss and
    // the slow path would throw.
    useViewerStore.setState({
      workspaces: {
        [TEAM_B_KEY]: [
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/team-b-existing",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    });

    const cwd = await useViewerStore.getState().ensureWorktree(TEAM_B_KEY, storyFile);

    // Store hit returned team B's cwd — proves the teamKey arg drove
    // the bucket lookup, not the active team A.
    expect(cwd).toBe("/wt/team-b-existing");
    expect(findOnDiskMock).not.toHaveBeenCalled();
    expect(setupMock).not.toHaveBeenCalled();
  });

  it("ensureWorktree(teamKey for active team A): store-hit read uses team A's bucket — regression sanity", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM_A_KEY]: [
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/team-a-existing",
            storyFilePath: storyFile,
            storyId,
            status: "ready" as const,
          },
        ],
      },
    });

    const cwd = await useViewerStore.getState().ensureWorktree(TEAM_A_KEY, storyFile);
    expect(cwd).toBe("/wt/team-a-existing");
  });

  it("cleanupStoryWorktree(synthetic for team B): reads team B's bucket, not active team A", async () => {
    // cleanupStoryWorktree still takes a legacy synthetic repoPath
    // (T.5 scope only covers the four worktree entry points). Verifies
    // the team-derivation inside cleanupStoryWorktree still threads the
    // right bucket.
    useViewerStore.setState({
      workspaces: {
        [TEAM_A_KEY]: [
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/team-a-keep",
            storyFilePath: storyFile,
            status: "ready" as const,
            sourceRepoPath: "/real/team-a-clone",
          },
        ],
        [TEAM_B_KEY]: [
          {
            id: storyId,
            kind: "story" as const,
            cwd: "/wt/team-b-remove",
            storyFilePath: storyFile,
            status: "ready" as const,
            sourceRepoPath: "/real/team-b-clone",
          },
        ],
      },
    });

    await useViewerStore.getState().cleanupStoryWorktree(SYN_TEAM_B_REPO, storyFile);

    // removeWorktree was called with team B's stored paths.
    expect(removeWorktreeMock).toHaveBeenCalledWith(
      "/real/team-b-clone",
      "/wt/team-b-remove",
      false,
      false,
    );
    const s = useViewerStore.getState();
    // Team B's workspace was closed; team A's survives.
    expect(s.workspaces[TEAM_B_KEY]?.find((w) => w.id === storyId)).toBeUndefined();
    expect(s.workspaces[TEAM_A_KEY]?.find((w) => w.id === storyId)).toBeDefined();
  });
});

// ===========================================================================
// SW-WB-1: reconcileWorktreeBranch — reconcile a story's worktree onto its
// intended (`branch_name`) branch. Clean → recreate; dirty → migrate in place;
// aligned → noop. Reuses the ensure-worktree harness mocks.
// ===========================================================================
describe("reconcileWorktreeBranch (SW-WB-1)", () => {
  const listWorktreesMock = listWorktrees as unknown as ReturnType<typeof vi.fn>;
  const checkDirtyMock = checkWorktreeDirty as unknown as ReturnType<typeof vi.fn>;
  const switchBranchMock = switchWorktreeBranch as unknown as ReturnType<typeof vi.fn>;
  const resolvePathMock = resolveWorktreePath as unknown as ReturnType<typeof vi.fn>;

  const CLONE = "/real/clone";
  const OLD_PATH = "/wt/feat-old";
  const INTENT = "feat/34-9-worktree-gate"; // resolveWorktreeParams mock default

  function seedWorkspace() {
    useViewerStore.setState({
      workspaces: {
        [repo]: [
          {
            id: storyId,
            kind: "story" as const,
            cwd: OLD_PATH,
            storyFilePath: storyFile,
            status: "ready" as const,
            sourceRepoPath: CLONE,
            sourceRepoSlug: "test/repo",
          },
        ],
      },
    });
  }

  beforeEach(() => {
    reset();
    loadMock.mockResolvedValue(baseConfig);
    resolveWorktreeParamsMock.mockResolvedValue({ branchName: INTENT, startPoint: null });
    resolvePathMock.mockReturnValue("/wt/feat-34-9-worktree-gate");
    listWorktreesMock.mockReset();
    checkDirtyMock.mockReset();
    checkDirtyMock.mockResolvedValue({ dirty: false });
    switchBranchMock.mockReset();
    switchBranchMock.mockResolvedValue({ worktree_path: OLD_PATH, from_remote: false });
    establishLinkMock.mockClear();
    removeWorktreeMock.mockClear();
  });

  it("noop when the worktree is already on the intended branch", async () => {
    seedWorkspace();
    // Observed branch == intent.
    listWorktreesMock.mockResolvedValue([{ path: OLD_PATH, branch: INTENT, head_sha: "abc" }]);

    const res = await useViewerStore.getState().reconcileWorktreeBranch(repo, storyFile);

    expect(res.mode).toBe("noop");
    expect(switchBranchMock).not.toHaveBeenCalled();
    expect(removeWorktreeMock).not.toHaveBeenCalled();
    // Re-stamps dev_branch even on noop so a stale frontmatter value (e.g. an
    // out-of-band `git switch`) is corrected and the chip clears.
    expect(establishLinkMock).toHaveBeenCalled();
  });

  it("migrates in place (git switch, WIP preserved) when the tree is dirty", async () => {
    seedWorkspace();
    listWorktreesMock.mockResolvedValue([{ path: OLD_PATH, branch: "feat/old", head_sha: "abc" }]);
    checkDirtyMock.mockResolvedValue({ dirty: true });

    const res = await useViewerStore.getState().reconcileWorktreeBranch(repo, storyFile);

    expect(res.mode).toBe("migrate");
    // Same path preserved (session kept); switch called with repo, old path, intent.
    expect(switchBranchMock).toHaveBeenCalledWith(CLONE, OLD_PATH, INTENT);
    expect(res.path).toBe(OLD_PATH);
    // Re-link fires so dev_branch re-stamps and the chip clears.
    expect(establishLinkMock).toHaveBeenCalled();
    // Never prunes the worktree on a migrate.
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("recreates + cleans up the orphan when the tree is clean", async () => {
    seedWorkspace();
    listWorktreesMock.mockResolvedValue([{ path: OLD_PATH, branch: "feat/old", head_sha: "abc" }]);
    checkDirtyMock.mockResolvedValue({ dirty: false });

    // Isolate reconcile from the full ensureWorktree/closeWorkspace machinery.
    const ensureSpy = vi.fn().mockResolvedValue("/wt/feat-34-9-worktree-gate");
    const closeSpy = vi.fn();
    useViewerStore.setState({ ensureWorktree: ensureSpy, closeWorkspace: closeSpy });

    const res = await useViewerStore.getState().reconcileWorktreeBranch(repo, storyFile);

    expect(res.mode).toBe("recreate");
    // Old workspace hard-closed (clean ⇒ no WIP), old worktree pruned, fresh create.
    expect(closeSpy).toHaveBeenCalledWith(repo, storyId, { killTerminals: true });
    expect(removeWorktreeMock).toHaveBeenCalledWith(CLONE, OLD_PATH, false);
    expect(ensureSpy).toHaveBeenCalledWith(repo, storyFile);
    expect(switchBranchMock).not.toHaveBeenCalled();
    // Re-stamps dev_branch on the fresh worktree so the chip clears.
    expect(establishLinkMock).toHaveBeenCalled();
  });

  it("forceRecreate discards WIP: recreates even on a dirty tree (AC-7)", async () => {
    seedWorkspace();
    listWorktreesMock.mockResolvedValue([{ path: OLD_PATH, branch: "feat/old", head_sha: "abc" }]);
    checkDirtyMock.mockResolvedValue({ dirty: true }); // dirty — would normally migrate
    const ensureSpy = vi.fn().mockResolvedValue("/wt/feat-34-9-worktree-gate");
    const closeSpy = vi.fn();
    useViewerStore.setState({ ensureWorktree: ensureSpy, closeWorkspace: closeSpy });

    const res = await useViewerStore
      .getState()
      .reconcileWorktreeBranch(repo, storyFile, { forceRecreate: true });

    // forceRecreate skips the dirty→migrate branch entirely.
    expect(res.mode).toBe("recreate");
    expect(switchBranchMock).not.toHaveBeenCalled();
    expect(closeSpy).toHaveBeenCalledWith(repo, storyId, { killTerminals: true });
    expect(removeWorktreeMock).toHaveBeenCalledWith(CLONE, OLD_PATH, false);
    expect(ensureSpy).toHaveBeenCalledWith(repo, storyFile);
  });

  it("falls back to a fresh create when no worktree record is known", async () => {
    // No seeded workspace → nothing to reconcile in place.
    const ensureSpy = vi.fn().mockResolvedValue("/wt/fresh");
    useViewerStore.setState({ ensureWorktree: ensureSpy });

    const res = await useViewerStore.getState().reconcileWorktreeBranch(repo, storyFile);

    expect(res.mode).toBe("recreate");
    expect(ensureSpy).toHaveBeenCalledWith(repo, storyFile);
    expect(switchBranchMock).not.toHaveBeenCalled();
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });
});

// The store's own actions, before any test swaps them for spies.
const realActions = (() => {
  const s = useViewerStore.getState();
  return { ensureWorktree: s.ensureWorktree, closeWorkspace: s.closeWorkspace };
})();

describe("SW-XS-10 review r1: Retry of a failed local create, through the real ensureWorktree", () => {
  beforeEach(() => {
    reset();
    useViewerStore.setState(realActions);
  });

  const failedRecord = (): Workspace => ({
    id: storyId,
    kind: "story",
    cwd: "/wt/feat-34-9-worktree-gate",
    storyFilePath: storyFile,
    storyId,
    status: "error",
    statusError: "git worktree add failed",
    setupState: { kind: "failed", step: 1, phase: "creatingWorktree", reason: "git failed" },
  });
  const record = () => useViewerStore.getState().workspaces[repo].find((w) => w.id === storyId);

  it("re-runs the create into the same record and lands ready", async () => {
    useViewerStore.setState({ workspaces: { [repo]: [failedRecord()] } });
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    await useViewerStore.getState().retryWorkspaceCreate(repo, storyId);
    expect(createMock).toHaveBeenCalled();
    expect(record()).toMatchObject({ status: "ready" });
    expect(record()?.statusError).toBeUndefined();
  });

  it("a failure before the record is reached (invalid config) lands on its line, never a vanish", async () => {
    useViewerStore.setState({ workspaces: { [repo]: [failedRecord()] } });
    probeMock.mockResolvedValue({ status: { kind: "invalid", error: "bad yaml" }, config: null });
    await useViewerStore.getState().retryWorkspaceCreate(repo, storyId);
    expect(record()).toMatchObject({
      status: "error",
      statusError: "Worktree config is invalid: bad yaml",
      setupState: { kind: "failed", step: 1 },
    });
  });

  it("a cancelled retry returns the record to its prior error state", async () => {
    const prior = failedRecord();
    useViewerStore.setState({ workspaces: { [repo]: [prior] } });
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: baseConfig });
    createMock.mockRejectedValue(
      Object.assign(new Error("cancelled"), { failedStep: "cancelled" }),
    );
    await useViewerStore.getState().retryWorkspaceCreate(repo, storyId);
    // Either the open tore the record down or the retry restored it — never
    // stuck "creating".
    expect(record()).toMatchObject({ status: "error", statusError: "git worktree add failed" });
  });
});
