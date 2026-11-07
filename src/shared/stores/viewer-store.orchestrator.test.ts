// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

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

import { selectWorkspaceTabs, useViewerStore } from "./viewer-store";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { useOrgStore } from "./org-store";
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
import {
  __resetBackgroundSetupFanoutForTest,
  setBackgroundSetupCap,
  withBackgroundSetupSlot,
} from "@/shared/lib/worktree/background-setup-fanout";

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

describe("openOrFocusStoryWorkspace — focus existing", () => {
  beforeEach(reset);

  it("switches to the existing workspace and expands terminal panel — no recreation", async () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story",
            cwd: "/wt/30-10",
            storyFilePath: storyFile,
            storyId,
            status: "ready",
          },
        ],
      },
    }));

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(storyId);
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    expect(probeMock).not.toHaveBeenCalled();
    expect(setupMock).not.toHaveBeenCalled();
  });
});

describe("openOrFocusStoryWorkspace — config optional (SW-WT-1)", () => {
  beforeEach(reset);

  it("PROCEEDS to create the worktree when probe returns 'missing'", async () => {
    // SW-WT-1 AC-6: a missing worktree.yaml is calm — setup runs and the
    // story workspace is adopted (no setup-required modal, no rejection).
    probeMock.mockResolvedValue({ status: { kind: "missing" }, config: null });
    loadMock.mockResolvedValue(null);
    setupMock.mockResolvedValue({ worktreePath: "/wt/30-10", results: [] });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.find((w) => w.id === storyId)?.status).toBe("ready");
    // RF-perf-14: the interactive open now creates the worktree (Phase A) then
    // backgrounds setup, so the create — not the blocking setupWorktree — runs.
    expect(createMock).toHaveBeenCalled();
  });

  it("PROCEEDS when the config probe throws (degrades to missing)", async () => {
    probeMock.mockRejectedValue("network down");
    loadMock.mockResolvedValue(null);
    setupMock.mockResolvedValue({ worktreePath: "/wt/30-10", results: [] });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.find((w) => w.id === storyId)?.status).toBe("ready");
    expect(createMock).toHaveBeenCalled();
  });
});

describe("openOrFocusStoryWorkspace — happy path", () => {
  beforeEach(reset);

  it("ensures worktree, adopts workspace, focuses it, auto-launches a shell", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({
      version: 1,
      setup: [],
    });
    setupMock.mockResolvedValue({ worktreePath: "/wt/30-10", results: [] });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo];
    const story = ws.find((w) => w.id === storyId);
    expect(story).toBeDefined();
    expect(story?.status).toBe("ready");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(storyId);
    expect(useViewerStore.getState().terminalVisible).toBe(true);

    // Auto-launched shell sits under the new workspace.
    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, storyId);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].name).toBe("Terminal 1");
    expect(tabs[0].sessionId).toBeDefined();
  });

  it("mounts the panel as 'creating' with NO shell before setup resolves, spawns it on 'ready' (SLW-CO-5 AC-1)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });

    // Snapshot the store state from INSIDE the (still-pending) setup call — this
    // is the moment the terminal is on screen but setup hasn't finished. Robust
    // vs microtask counting: the mock runs after the record + onCreating switch.
    // RF-perf-14 (decouple): the pre-ready window is now Phase A (createWorktree)
    // — the terminal is on screen as "creating" while the git worktree is being
    // added, and the shell is DEFERRED until the dir exists. Snapshot INSIDE the
    // (still-pending) create; the ready-flip + shell now land the instant Phase A
    // resolves (setup runs in the background AFTER, not before, the shell).
    let snap: { status?: string; tabs: number; active?: string; visible?: boolean } | null = null;
    createMock.mockImplementation(async () => {
      const s = useViewerStore.getState();
      snap = {
        status: s.workspaces[repo]?.find((w) => w.id === storyId)?.status,
        tabs: selectWorkspaceTabs(s, repo, storyId).length,
        active: s.activeWorkspace[repo],
        visible: s.terminalVisible,
      };
      return { worktreePath: "/wt/30-10", rehydrated: false };
    });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    // Mid-create: panel mounted (switched + terminal visible + "creating"), but the
    // shell is DEFERRED — never spawned into a not-yet-created worktree dir.
    expect(snap).not.toBeNull();
    expect(snap!.active).toBe(storyId);
    expect(snap!.visible).toBe(true);
    expect(snap!.status).toBe("creating");
    expect(snap!.tabs).toBe(0);

    // On dir-exists (Phase A resolve): flips to "ready" and the shell is spawned.
    const after = useViewerStore.getState();
    expect(after.workspaces[repo]?.find((w) => w.id === storyId)?.status).toBe("ready");
    expect(selectWorkspaceTabs(after, repo, storyId).length).toBe(1);
  });

  it("skips the shell spawn if a concurrent close removed the workspace mid-setup (SLW-CO-5 abort race)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });

    // Simulate Cancel removing the record in the window where create still
    // resolves successfully (the abort lost the race) — the ready-flip + shell
    // side-effects must NOT run on the ghost workspace (no orphan tab / PTY).
    // RF-perf-14: this race is now around Phase A (create), the pre-ready window.
    createMock.mockImplementation(async () => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [repo]: (s.workspaces[repo] ?? []).filter((w) => w.id !== storyId),
        },
      }));
      return { worktreePath: "/wt/30-10", rehydrated: false };
    });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId),
    ).toBeUndefined();
    expect(selectWorkspaceTabs(useViewerStore.getState(), repo, storyId).length).toBe(0);
  });
});

describe("openOrFocusStoryWorkspace — SW-FX-35 tray pair coherence", () => {
  beforeEach(reset);

  // Seed an already-adopted workspace so we hit the synchronous focus branch
  // (no worktree setup), then open it. The bug lives in the tail shared by
  // every branch: `switchWorkspace` + show-the-terminal.
  async function focusExisting() {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          {
            id: storyId,
            kind: "story",
            cwd: "/wt/30-10",
            storyFilePath: storyFile,
            storyId,
            status: "ready",
          },
        ],
      },
    }));
    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
  }

  it("AC-1: from a cold viewer layoutMode, the tray pair is satisfied (visible + non-viewer)", async () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    await focusExisting();
    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).not.toBe("viewer");
  });

  it("AC-4: restores lastTerminalMode === 'console'", async () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    await focusExisting();
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("AC-4: unset lastTerminalMode falls back to 'shared'", async () => {
    useViewerStore.setState({ layoutMode: "viewer", lastTerminalMode: "shared" });
    await focusExisting();
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });

  it("AC-5: no downgrade when a CLI workspace is already open (layoutMode 'shared')", async () => {
    useViewerStore.setState({
      layoutMode: "shared",
      lastTerminalMode: "shared",
    });
    await focusExisting();
    const s = useViewerStore.getState();
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });
});

describe("openOrFocusStoryWorkspace — failure path", () => {
  beforeEach(reset);

  it("RF-perf-14 (AC-4): a BACKGROUND setup failure keeps the terminal mounted + shows a retryable badge", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({
      version: 1,
      setup: [],
    });
    // Phase A (create) succeeds → workspace flips ready + the shell mounts. Phase
    // B (setup) fails IN THE BACKGROUND — the decouple's whole point is that this
    // no longer hides the shell or aborts the open; it becomes a retry badge.
    const diagnostics = {
      overallExitCode: 1,
      failedResult: {
        command: "npm ci",
        passed: false,
        stdout: "",
        stderr: "boom",
        exit_code: 1,
        phase: "setup" as const,
        step_index: 0,
      },
      streams: {
        stdout: { originalLength: 0, truncated: false },
        stderr: { originalLength: 4, truncated: false },
      },
    };
    const err = Object.assign(new Error("boom"), {
      failedStep: "setup",
      recoverable: true,
      diagnostics,
    });
    runSetupPhaseMock.mockRejectedValue(err);

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    // Let the backgrounded setup's rejection settle (the .catch sets the badge).
    await new Promise((r) => setTimeout(r, 0));

    const story = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    // The open resolved and the shell IS spawned — the shell is not hidden.
    expect(story?.status).toBe("ready");
    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, storyId);
    expect(tabs).toHaveLength(1);
    // The failure is a retryable badge over the LIVE terminal, not an error swap.
    expect(story?.backgroundSetup).toEqual({ status: "failed", error: "boom", diagnostics });

    // Age eviction retains the workspace and badge, so its retry owner must survive too.
    useViewerStore.getState().closeTeam(repo, { preserveTerminalSessions: true });
    useViewerStore.getState().openTeam(repo);

    let finishRetry!: () => void;
    runSetupPhaseMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          finishRetry = () => resolve({ worktreePath: "/wt/30-10", results: [] });
        }),
    );
    useViewerStore.getState().retryWorktreeSetup(repo, storyId);
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup
        ?.status,
    ).toBe("running");
    expect(runSetupPhaseMock).toHaveBeenCalledTimes(2);
    finishRetry();
    await new Promise((r) => setTimeout(r, 0));
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup,
    ).toBeUndefined();
  });

  it("lets a running background setup settle after renderer eviction", async () => {
    probeMock.mockResolvedValue({
      status: { kind: "present" },
      config: { version: 1, setup: ["npm ci"] },
    });

    let finishSetup!: () => void;
    let setupSignal: AbortSignal | undefined;
    runSetupPhaseMock.mockImplementation(
      (opts: { signal?: AbortSignal }) =>
        new Promise((resolve) => {
          setupSignal = opts.signal;
          finishSetup = () => resolve({ worktreePath: "/wt/30-10", results: [] });
        }),
    );

    await useViewerStore
      .getState()
      .ensureWorktree(repo, storyFile, undefined, undefined, { onCreating: () => {} });
    await vi.waitFor(() => expect(setupSignal).toBeDefined());
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup
        ?.status,
    ).toBe("running");

    useViewerStore.getState().closeTeam(repo, { preserveTerminalSessions: true });
    useViewerStore.getState().openTeam(repo);
    expect(setupSignal?.aborted).toBe(false);

    finishSetup();
    await vi.waitFor(() => {
      expect(
        useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup,
      ).toBeUndefined();
    });
  });

  it("swallows an invalid-config failure (failedStep 'config' is handled)", async () => {
    // SW-WT-1: an invalid worktree.yaml throws a `config`-step error from
    // ensureWorktree (after toasting at the throw site). `config` is in
    // HANDLED_FAILED_STEPS, so the orchestrator swallows it — resolves
    // without auto-launching a shell.
    probeMock.mockResolvedValue({ status: { kind: "invalid", error: "bad yaml" }, config: null });

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    expect(setupMock).not.toHaveBeenCalled();
    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, storyId);
    expect(tabs).toHaveLength(0);
  });
});

describe("openOrFocusStoryWorkspace — cancel path", () => {
  beforeEach(reset);

  it("resolves (swallows) on cancel — no phantom workspace entry", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({
      version: 1,
      setup: [],
    });
    const err = Object.assign(new Error("aborted"), {
      failedStep: "cancelled",
      recoverable: true,
    });
    // RF-perf-14: a cancel now lands during Phase A (create), which rejects
    // "cancelled" → the outer catch tears down the just-mounted "creating" record.
    createMock.mockRejectedValue(err);

    // Cancelled errors are swallowed — resolves instead of rejecting
    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.find((w) => w.id === storyId)).toBeUndefined();
  });

  it("overlay Cancel (closeWorkspace) aborts the in-flight setupWorktree (SLW-CO-5 AC-8)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });

    // RF-perf-14: a create (Phase A) that stays in-flight until its signal
    // aborts, then rejects "cancelled" (mirroring the real createWorktree
    // contract) so the promise settles and the `ensureWorktreeInFlight` dedup
    // entry clears. The abort wiring AC-8 guards is the SAME — Cancel still cancels.
    let capturedSignal: AbortSignal | undefined;
    createMock.mockImplementation(
      (opts: { signal?: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          capturedSignal = opts.signal;
          opts.signal?.addEventListener("abort", () =>
            reject(
              Object.assign(new Error("cancelled"), {
                failedStep: "cancelled",
                recoverable: true,
              }),
            ),
          );
        }),
    );

    // Kick off the open (do NOT await yet — create is in flight); flush the
    // pre-create awaits until `createWorktree` is reached.
    const openPromise = useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    for (let i = 0; i < 30 && !capturedSignal; i++) await Promise.resolve();

    expect(capturedSignal).toBeDefined();
    expect(capturedSignal!.aborted).toBe(false);

    // The overlay's Cancel calls closeWorkspace, which MUST abort the in-flight
    // setup (this is what makes AC-8's "Cancel still cancels" real) and remove
    // the just-mounted "creating" workspace.
    useViewerStore.getState().closeWorkspace(repo, storyId);
    expect(capturedSignal!.aborted).toBe(true);
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId),
    ).toBeUndefined();

    // Let the (now-rejecting) setup settle so the dedup map clears for later tests.
    await openPromise;
  });

  it("concurrent calls share a single ensureWorktree invocation", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });
    setupMock.mockResolvedValue({ worktreePath: "/wt/30-10", results: [] });

    const p1 = useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    const p2 = useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    await Promise.all([p1, p2]);

    // Create only called once — dedup in ensureWorktree (RF-perf-14: the
    // interactive open runs createWorktree, not the blocking setupWorktree).
    expect(createMock).toHaveBeenCalledTimes(1);

    // Only ONE story workspace exists.
    const matching = useViewerStore.getState().workspaces[repo].filter((w) => w.id === storyId);
    expect(matching).toHaveLength(1);
  });
});

// T.6: `closeRepo` retired in Task 10 along with its dismiss-toast
// wiring. The team-level teardown lives in `closeTeam(teamKey)` (see
// viewer-store.ts:2460), which atomically clears every team-keyed
// slice; there is no per-repo toast dismiss site left to assert.
// closeTeam's own contract is covered indirectly by the F-19 eviction
// tests in use-workspace-viewer-sync.test.ts.

describe("openOrFocusStoryWorkspace — M.2 cross-repo", () => {
  beforeEach(reset);

  it("cross-repo workspace appears in context repo's tab bar, not source repo's", async () => {
    const crossRepoSlug = "acme/api";
    const crossRepoLocalPath = "/clones/acme-api";
    const crossRepoState = {
      repoPath: "workspace:9north:default:acme/api",
      repoName: "api (9North Studio)",
      contextRepoPath: "/ctx/acme/api",
      hasRemote: true,
      remoteUrl: null,
      mainDoc: "",
      localPath: crossRepoLocalPath,
    };

    parseSourceRepoMock.mockReturnValue(crossRepoSlug);
    findRepoBySlugMock.mockReturnValue(crossRepoState);
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });
    setupMock.mockResolvedValue(undefined);

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);

    // Workspace should be in the CONTEXT repo's tab bar (repo), not the source repo's.
    const contextWs = useViewerStore.getState().workspaces[repo] ?? [];
    const sourceWs = useViewerStore.getState().workspaces[crossRepoState.repoPath] ?? [];
    expect(contextWs.find((w) => w.id === storyId)).toBeDefined();
    expect(sourceWs.find((w) => w.id === storyId)).toBeUndefined();
  });
});

describe("openOrFocusStoryWorkspace — background-setup lifecycle (RF-perf-14)", () => {
  beforeEach(reset);

  const flush = () => new Promise((r) => setTimeout(r, 0));

  it("closing the workspace ABORTS the in-flight background setup (no orphaned npm ci)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });
    // The background setup stays in-flight, capturing its signal, until aborted.
    let capturedSignal: AbortSignal | undefined;
    runSetupPhaseMock.mockImplementation(
      (opts: { signal?: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          capturedSignal = opts.signal;
          opts.signal?.addEventListener("abort", () =>
            reject(
              Object.assign(new Error("cancelled"), {
                failedStep: "cancelled",
                recoverable: true,
              }),
            ),
          );
        }),
    );

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    await flush(); // let the background setup start + capture its signal

    // The open resolved (Phase A) and the background setup is running.
    expect(capturedSignal).toBeDefined();
    expect(capturedSignal!.aborted).toBe(false);
    expect(
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup
        ?.status,
    ).toBe("running");

    // Closing the workspace must abort the background setup (kills the subprocess).
    useViewerStore.getState().closeWorkspace(repo, storyId);
    expect(capturedSignal!.aborted).toBe(true);
  });

  it("a background setup superseded by close→reopen does NOT stamp the reopened workspace", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });
    // Open 1's background setup is held so we can fail it AFTER a reopen.
    let rejectSetup1!: (e: unknown) => void;
    runSetupPhaseMock.mockImplementationOnce(
      () =>
        new Promise((_resolve, reject) => {
          rejectSetup1 = reject;
        }),
    );

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    await flush();

    // Close (aborts + de-registers open 1's background setup) then reopen.
    useViewerStore.getState().closeWorkspace(repo, storyId);
    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    await flush();

    // Open 1's stale setup now fails — it must NOT stamp the REOPENED workspace
    // (ownership guard): the reopened record's badge reflects open 2, not open 1.
    rejectSetup1(Object.assign(new Error("boom"), { failedStep: "setup", recoverable: true }));
    await flush();

    const ws = useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(ws).toBeDefined();
    expect(ws?.backgroundSetup?.status).not.toBe("failed"); // open 1's failure didn't bleed through
  });

  it("reads QUEUED behind a saturated gate, then flips to RUNNING when a slot frees (RF-perf-14d)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });

    // Saturate the background-setup gate: cap 1, hold the only slot with an
    // external task so the story's setup can't get in.
    setBackgroundSetupCap(1);
    let releaseHolder!: () => void;
    const holder = withBackgroundSetupSlot(() => new Promise<void>((r) => (releaseHolder = r)));
    await flush();

    // Hold the story's own setup once it actually runs, so we can observe RUNNING
    // before it settles + clears the badge.
    let resolveSetup!: () => void;
    runSetupPhaseMock.mockImplementation(
      () =>
        new Promise((res) => {
          resolveSetup = () => res({ worktreePath: "/wt/30-10", results: [] });
        }),
    );

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    await flush();

    const badge = () =>
      useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId)?.backgroundSetup;

    // No slot yet → the badge reads QUEUED and the setup subprocess has NOT started.
    expect(badge()?.status).toBe("queued");
    expect(runSetupPhaseMock).not.toHaveBeenCalled();

    // Free the slot → the queued setup acquires it → flips to RUNNING + starts.
    releaseHolder();
    await holder;
    await flush();
    expect(badge()?.status).toBe("running");
    expect(runSetupPhaseMock).toHaveBeenCalledTimes(1);

    // Finish → the badge clears (shell was usable throughout).
    resolveSetup();
    await flush();
    expect(badge()).toBeUndefined();
  });

  it("closing a QUEUED workspace tears it down without resurrecting it or stamping a stale badge (RF-perf-14d)", async () => {
    probeMock.mockResolvedValue({ status: { kind: "present" }, config: { version: 1, setup: [] } });
    loadMock.mockResolvedValue({ version: 1, setup: [] });

    // Saturate the gate so the story's setup parks in "queued".
    setBackgroundSetupCap(1);
    let releaseHolder!: () => void;
    const holder = withBackgroundSetupSlot(() => new Promise<void>((r) => (releaseHolder = r)));
    await flush();

    // Faithful to the real setup phase: if the signal is already aborted when the
    // slot frees, it bails "cancelled" without spawning a subprocess.
    runSetupPhaseMock.mockImplementation((opts: { signal?: AbortSignal }) =>
      opts.signal?.aborted
        ? Promise.reject(
            Object.assign(new Error("cancelled"), { failedStep: "cancelled", recoverable: true }),
          )
        : Promise.resolve({ worktreePath: "/wt/30-10", results: [] }),
    );

    await useViewerStore.getState().openOrFocusStoryWorkspace(repo, storyFile);
    await flush();

    const find = () => useViewerStore.getState().workspaces[repo]?.find((w) => w.id === storyId);
    expect(find()?.backgroundSetup?.status).toBe("queued");

    // Close while still queued → the workspace record is torn down; the "queued"
    // badge goes with it (no orphaned "Queued…" chip).
    useViewerStore.getState().closeWorkspace(repo, storyId);
    expect(find()).toBeUndefined();

    // Free the slot → the queued fn wakes late, sees the aborted signal, bails
    // cancelled, and — no longer owning the handle — writes nothing back. The
    // closed workspace is NOT resurrected and no stale badge reappears.
    releaseHolder();
    await holder;
    await flush();
    await flush();
    expect(find()).toBeUndefined();
  });
});
