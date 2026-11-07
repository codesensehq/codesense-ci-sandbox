// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";
import { useOrgStore } from "./org-store";
import * as commands from "@/tauri/commands";

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
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const SYNTHETIC_KEY = "workspace:acme:default:owner/repo";
const TEAM_KEY = "acme:default";

function resetState() {
  useOrgStore.setState({
    // T.1 D-1: `loadPersistedState` waits for `useOrgStore.orgs.length > 0`
    // when the persisted blob contains legacy (non-synthetic) outer keys,
    // matching the production cold-start await semantics. Seed a minimal
    // org so tests resolve the wait immediately instead of hitting the 5s
    // timeout. The org's `teams` list is irrelevant for sentinel-path
    // tests — what matters is that orgs is non-empty.
    // T.2 (review iter-1, finding #1): seed `orgs[*].teams[*].source_repos`
    // — the migration now reads slugs from here, not from `workspaceRepos`
    // (which is hydrated async post-boot and would race the migration).
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        created_at: "2026-01-01T00:00:00Z",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "owner", repo: "repo" }],
          },
        ],
        org_root: "/orgs/acme",
      },
    ],
    workspaceRepos: {},
    activeOrgSlug: null,
    activeTeamName: null,
  });
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    openedTeams: [],
  });
  loadAppStateMock.mockReset();
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
}

describe("T.1 migration — synthetic outer key parse-first path", () => {
  beforeEach(resetState);

  it("re-buckets a workspaces entry whose outer key is a synthetic repoPath", async () => {
    const persisted = {
      viewer: {
        // No schemaVersion → migration runs.
        selectedRepoPath: SYNTHETIC_KEY,
        workspaces: {
          [SYNTHETIC_KEY]: [
            {
              id: "F-1",
              kind: "story",
              cwd: "/wt/F-1",
              storyFilePath: "/stories/F-1.md",
              storyId: "F-1",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { [SYNTHETIC_KEY]: "F-1" },
        terminalTabs: { [SYNTHETIC_KEY]: [{ id: "t1", name: "Shell 1", sessionId: "sess1" }] },
        activeTerminalTab: { [SYNTHETIC_KEY]: "t1" },
        shellTabCounter: { [SYNTHETIC_KEY]: 1 },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    // Workspaces re-bucketed under the team-keyed slot.
    expect(state.workspaces[TEAM_KEY]).toBeDefined();
    expect(state.workspaces[TEAM_KEY]).toHaveLength(1);
    expect(state.workspaces[TEAM_KEY][0].id).toBe("F-1");
    // Active workspace re-bucketed too.
    expect(state.activeWorkspace[TEAM_KEY]).toBe("F-1");
    // Legacy persisted terminal slices are dropped: terminals live in their host.
    expect(state).not.toHaveProperty("terminalTabsByWorkspace");
    expect(state).not.toHaveProperty("terminalTabs");
    // Original synthetic-keyed buckets are gone.
    expect(state.workspaces[SYNTHETIC_KEY]).toBeUndefined();
  });

  it("does not require org-store readiness for the synthetic parse path", async () => {
    // org-store is intentionally empty (set in resetState).
    const persisted = {
      viewer: {
        workspaces: {
          [SYNTHETIC_KEY]: [
            {
              id: "X-1",
              kind: "story",
              cwd: "/wt/X-1",
              storyFilePath: "/X-1.md",
              storyId: "X-1",
              status: "ready",
            },
          ],
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().workspaces[TEAM_KEY]).toBeDefined();
  });
});

describe("T.1 migration — sentinel bucket fallback", () => {
  beforeEach(resetState);

  it("buckets legacy real-path entries with no sourceRepoSlug into the sentinel", async () => {
    const persisted = {
      viewer: {
        workspaces: {
          // Non-synthetic outer key + workspace without sourceRepoSlug.
          "/Users/dev/repo": [
            {
              id: "L-1",
              kind: "story",
              cwd: "/wt/L-1",
              storyFilePath: "/stories/L-1.md",
              storyId: "L-1",
              status: "ready",
            },
          ],
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const sentinel = useViewerStore.getState().workspaces["_no_team:_none"];
    expect(sentinel).toBeDefined();
    expect(sentinel).toHaveLength(1);
    expect(sentinel[0].id).toBe("L-1");
  });
});

describe("T.1 migration — schemaVersion behavior", () => {
  beforeEach(resetState);

  it("skips migration entirely when schemaVersion >= 1", async () => {
    const persisted = {
      viewer: {
        schemaVersion: 1,
        // Outer key is already a teamKey; migration should pass through.
        workspaces: {
          [TEAM_KEY]: [
            {
              id: "ready-1",
              kind: "story",
              cwd: "/wt/ready-1",
              storyFilePath: "/stories/ready-1.md",
              storyId: "ready-1",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { [TEAM_KEY]: "ready-1" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().workspaces[TEAM_KEY]).toHaveLength(1);
    expect(useViewerStore.getState().activeWorkspace[TEAM_KEY]).toBe("ready-1");
  });
});

describe("SW-EW-2 — drops persisted kind:'epic' workspaces on load", () => {
  beforeEach(resetState);

  it("filters out epic workspaces, keeps stories, and falls active back off a dropped epic", async () => {
    const persisted = {
      viewer: {
        schemaVersion: 9,
        workspaces: {
          [TEAM_KEY]: [
            {
              id: "story-keep",
              kind: "story",
              cwd: "/wt/story-keep",
              storyFilePath: "/stories/keep.md",
              storyId: "keep",
              status: "ready",
            },
            // Legacy epic-as-workspace node — must be dropped (terminal-less,
            // strands no PTY).
            { id: "epic:alpha", kind: "epic", cwd: "", epicDirPath: "alpha", status: "ready" },
          ],
        },
        // Active workspace pointed at the dropped epic → must fall back.
        activeWorkspace: { [TEAM_KEY]: "epic:alpha" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    const list = state.workspaces[TEAM_KEY] ?? [];
    expect(list.some((w) => w.kind === "epic")).toBe(false);
    expect(list.map((w) => w.id)).toEqual(["story-keep"]);
    // The active workspace no longer resolves to the dropped epic.
    expect(state.activeWorkspace[TEAM_KEY]).not.toBe("epic:alpha");
  });

  it("sweeps legacy roots + epic-less integrations, keeps stories + real epic integrations", async () => {
    const persisted = {
      viewer: {
        schemaVersion: 9,
        workspaces: {
          [TEAM_KEY]: [
            // Legacy per-repo root — swept (codesense is story-organized).
            { id: "root:owner/repo", kind: "root", cwd: "/repo", status: "ready" },
            // Already re-homed root (epic-less integration) — swept.
            {
              id: "integration:bare:owner/repo",
              kind: "integration",
              cwd: "/repo",
              epicDirPath: "",
              status: "ready",
            },
            // Real epic integration terminal (the planning surface) — KEPT.
            // SW-EW-15: a kept epic-level integration row now carries
            // `epicDocPath` (the doc-path-keyed shape); a stale pre-EW-15 row
            // without it is swept (see viewer-store.persistence-v2.test.ts).
            {
              id: "integration:epic-x:owner/repo",
              kind: "integration",
              cwd: "/repo",
              epicDirPath: "epic-x",
              epicDocPath: "/ctx/epics/epic-x/epic.md",
              status: "ready",
            },
            {
              id: "story-keep",
              kind: "story",
              cwd: "/wt/keep",
              storyFilePath: "/stories/keep.md",
              storyId: "keep",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { [TEAM_KEY]: "root:owner/repo" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const list = useViewerStore.getState().workspaces[TEAM_KEY] ?? [];
    expect(list.map((w) => w.id).sort()).toEqual(
      ["integration:epic-x:owner/repo", "story-keep"].sort(),
    );
    expect(list.some((w) => w.kind === "root")).toBe(false);
    // Active fell back off the swept root.
    expect(useViewerStore.getState().activeWorkspace[TEAM_KEY]).not.toBe("root:owner/repo");
  });
});

describe("T.1 migration — cross-slice consistency via shared map", () => {
  beforeEach(resetState);

  it("re-buckets the workspace slices under the same teamKey and drops the legacy terminal slices", async () => {
    const persisted = {
      viewer: {
        workspaces: {
          [SYNTHETIC_KEY]: [
            {
              id: "S-1",
              kind: "story",
              cwd: "/wt/S-1",
              storyFilePath: "/stories/S-1.md",
              storyId: "S-1",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { [SYNTHETIC_KEY]: "S-1" },
        terminalTabs: { [SYNTHETIC_KEY]: [{ id: "t1", name: "Shell 1", sessionId: "sess1" }] },
        activeTerminalTab: { [SYNTHETIC_KEY]: "t1" },
        shellTabCounter: { [SYNTHETIC_KEY]: 2 },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    expect(state.workspaces[TEAM_KEY]).toBeDefined();
    expect(state.activeWorkspace[TEAM_KEY]).toBe("S-1");
    for (const legacy of [
      "terminalTabsByWorkspace",
      "activeTerminalTabByWorkspace",
      "shellTabCounterByWorkspace",
    ]) {
      expect(state).not.toHaveProperty(legacy);
    }
  });
});

// ---------------------------------------------------------------------------
// T.4 D-11: schemaVersion 1 → 2 migration. Re-buckets the persisted T.4
// team-keyed slices from repoPath-keyed to teamKey-keyed shape via the same
// synthetic-parse strategy as T.1. Applies per-slice merge rules from D-4.
// The CLI chrome is no longer among them: v15 seeds it into one value (D39).
// ---------------------------------------------------------------------------

function seedWorkspaceRepo() {
  // Seed workspaceRepos so `pruneTeamKeyed` accepts TEAM_KEY post-migration.
  // T.4 D-4: workspaceRepos is also the source-of-truth for `slugToTeamKey`,
  // but T.4 slices use only Pass 1 (synthetic parse) — no slug fallback —
  // so an empty map would still work for parse-resolvable outer keys.
  useOrgStore.setState({
    workspaceRepos: {
      [SYNTHETIC_KEY]: {
        repoPath: SYNTHETIC_KEY,
        repoName: "repo",
        contextRepoPath: "/ctx",
        teamContentPath: "/ctx/_bmad-output",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      },
    },
  });
}

describe("T.4 migration — schemaVersion 1 → 2 slice re-bucketing", () => {
  beforeEach(() => {
    resetState();
    seedWorkspaceRepo();
  });

  it("re-buckets the T.4 team-keyed slices keyed by synthetic repoPath and seeds the global CLI chrome", async () => {
    const persisted = {
      viewer: {
        // T.1 already ran (schemaVersion: 1). T.4 migration kicks in.
        schemaVersion: 1,
        openTabs: { [SYNTHETIC_KEY]: [{ path: "a.md", name: "a" }] },
        activeTab: { [SYNTHETIC_KEY]: "a.md" },
        drillPaths: { [SYNTHETIC_KEY]: "/sub/dir" },
        terminalVisible: { [SYNTHETIC_KEY]: true },
        layoutMode: { [SYNTHETIC_KEY]: "shared" },
        lastTerminalMode: { [SYNTHETIC_KEY]: "console" },
        rightPaneSplitRatio: { [SYNTHETIC_KEY]: 0.6 },
        terminalSplitRatio: { [SYNTHETIC_KEY]: 0.45 },
        statusNavExpandedPaths: { [SYNTHETIC_KEY]: ["/a", "/b"] },
        pipelineScope: { [SYNTHETIC_KEY]: "features" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM_KEY]).toHaveLength(1);
    expect(s.openTabs[TEAM_KEY]![0].path).toBe("a.md");
    expect(s.activeTab[TEAM_KEY]).toBe("a.md");
    expect(s.drillPaths[TEAM_KEY]).toBe("/sub/dir");
    expect(s.rightPaneSplitRatio[TEAM_KEY]).toBe(0.6);
    expect(s.statusNavExpandedPaths[TEAM_KEY]).toEqual(["/a", "/b"]);
    expect(s.pipelineScope[TEAM_KEY]).toBe("features");
    // D39: the CLI chrome is one global value, seeded from the only saved bucket.
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).toBe("shared");
    expect(s.lastTerminalMode).toBe("console");
    expect(s.terminalSplitRatio).toBe(0.45);
    // Original repoPath bucket is gone.
    expect(s.openTabs[SYNTHETIC_KEY]).toBeUndefined();
  });

  it("concat+dedup arrays when two repoPath buckets collapse onto one teamKey", async () => {
    // Two synthetic repos in the same team.
    const SYN_A = "workspace:acme:default:owner/repo-a";
    const SYN_B = "workspace:acme:default:owner/repo-b";
    useOrgStore.setState({
      workspaceRepos: {
        [SYN_A]: {
          repoPath: SYN_A,
          repoName: "a",
          contextRepoPath: "/ctx",
          teamContentPath: "/ctx/_bmad-output",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
        [SYN_B]: {
          repoPath: SYN_B,
          repoName: "b",
          contextRepoPath: "/ctx",
          teamContentPath: "/ctx/_bmad-output",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      },
    });
    const persisted = {
      viewer: {
        schemaVersion: 1,
        openTabs: {
          [SYN_A]: [
            { path: "a.md", name: "a" },
            { path: "shared.md", name: "shared" },
          ],
          [SYN_B]: [
            { path: "shared.md", name: "shared" },
            { path: "b.md", name: "b" },
          ],
        },
        statusNavExpandedPaths: {
          [SYN_A]: ["/x", "/y"],
          [SYN_B]: ["/y", "/z"],
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    // openTabs: deduped by path; collation order preserves SYN_A first
    // (Object.entries iteration order).
    const tabs = s.openTabs[TEAM_KEY] ?? [];
    expect(tabs.map((t) => t.path)).toEqual(["a.md", "shared.md", "b.md"]);
    // statusNavExpandedPaths: dedup by string value.
    expect(s.statusNavExpandedPaths[TEAM_KEY]).toEqual(["/x", "/y", "/z"]);
  });

  it("scalar collisions resolve last-write-wins when buckets merge", async () => {
    const SYN_A = "workspace:acme:default:owner/repo-a";
    const SYN_B = "workspace:acme:default:owner/repo-b";
    useOrgStore.setState({
      workspaceRepos: {
        [SYN_A]: {
          repoPath: SYN_A,
          repoName: "a",
          contextRepoPath: "/ctx",
          teamContentPath: "/ctx/_bmad-output",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
        [SYN_B]: {
          repoPath: SYN_B,
          repoName: "b",
          contextRepoPath: "/ctx",
          teamContentPath: "/ctx/_bmad-output",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      },
    });
    const persisted = {
      viewer: {
        schemaVersion: 1,
        // SYN_A then SYN_B in entry order → SYN_B wins.
        activeTab: { [SYN_A]: "from-a.md", [SYN_B]: "from-b.md" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    expect(s.activeTab[TEAM_KEY]).toBe("from-b.md");
  });

  // T.6 D-9: workspaceViewerStates retired entirely. A v3 blob may carry
  // the slice (with any payload shape — legacy drillPath, selectedRepoPath,
  // pipelineScope, etc. that pre-T.4 versions packed inside); v4 load
  // ignores it and the field is absent from the loaded state.
  it("v3 → v4: workspaceViewerStates slice is dropped on load (no slice in state)", async () => {
    seedWorkspaceRepo();
    const persisted = {
      viewer: {
        schemaVersion: 3,
        workspaces: { [TEAM_KEY]: [] },
        // Legacy payload from when the snapshot stored drill/scope/active-tab
        // — should not surface anywhere on the loaded state.
        workspaceViewerStates: {
          [TEAM_KEY]: {
            orgSlug: "acme",
            teamName: "default",
            selectedRepoPath: SYNTHETIC_KEY,
            drillPath: "/old/drill",
            activeTabPath: "/old/active.md",
            pipelineScope: "old-scope",
          },
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    // `workspaceViewerStates` is no longer in the state shape — the only
    // visible signal is that it doesn't appear when keys are enumerated.
    const s = useViewerStore.getState() as unknown as Record<string, unknown>;
    expect(s.workspaceViewerStates).toBeUndefined();
  });

  // T.6 AC-7.4: schemaVersion bumped 3 → 4. v4 drops `selectedRepoPath`,
  // `viewMode`, `openedRepos`, `workspaceViewerStates` from the persisted
  // shape and introduces `openedTeams`. v3 blobs containing the retired
  // fields load cleanly — JS destructuring just doesn't bind them.
  it("v3 → v4: retired top-level fields (selectedRepoPath/viewMode/openedRepos) are dropped on load", async () => {
    seedWorkspaceRepo();
    const persisted = {
      viewer: {
        schemaVersion: 3,
        selectedRepoPath: SYNTHETIC_KEY,
        viewMode: "viewer",
        openedRepos: [SYNTHETIC_KEY],
        // No openedTeams in v3 → falls back to [] in v4 state.
        workspaces: { [TEAM_KEY]: [] },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState() as unknown as Record<string, unknown>;
    expect(s.selectedRepoPath).toBeUndefined();
    expect(s.viewMode).toBeUndefined();
    expect(s.openedRepos).toBeUndefined();
    // openedTeams was absent in the v3 blob → restore coerces to [].
    expect(useViewerStore.getState().openedTeams).toEqual([]);
  });

  // T.6 AC-7.4 round-trip: a v4 blob persists openedTeams (the v3 trio
  // replacement) and team-keyed slices, reload restores them verbatim; the
  // old per-team layoutMode map becomes the one global value (D39).
  it("v4 round-trip: openedTeams + team-keyed slices survive persist+reload", async () => {
    seedWorkspaceRepo();
    const persisted = {
      viewer: {
        schemaVersion: 4,
        openedTeams: [TEAM_KEY],
        openTabs: { [TEAM_KEY]: [{ path: "a.md", name: "a" }] },
        activeTab: { [TEAM_KEY]: "a.md" },
        drillPaths: { [TEAM_KEY]: "/drill" },
        layoutMode: { [TEAM_KEY]: "console" },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    expect(s.openedTeams).toEqual([TEAM_KEY]);
    expect(s.openTabs[TEAM_KEY]).toHaveLength(1);
    expect(s.activeTab[TEAM_KEY]).toBe("a.md");
    expect(s.drillPaths[TEAM_KEY]).toBe("/drill");
    expect(s.layoutMode).toBe("console");
  });

  it("pruneTeamKeyed drops unknown outer keys when workspaceRepos is non-empty", async () => {
    // T.4 Phase 10 review (Round 2 MED #4): when workspaceRepos is empty,
    // pruneTeamKeyed identity-prunes (preserves everything) to avoid data
    // loss from a cold-start race. Test the prune behavior by seeding a
    // valid workspaceRepo for a DIFFERENT team — then the bucket that
    // doesn't match any known teamKey gets pruned, while the sentinel +
    // the known one survive.
    const ANOTHER_SYN = "workspace:other-org:eng:foo/bar";
    const ANOTHER_TEAM_KEY = "other-org:eng";
    useOrgStore.setState({
      workspaceRepos: {
        [ANOTHER_SYN]: {
          repoPath: ANOTHER_SYN,
          repoName: "bar",
          contextRepoPath: "/ctx2",
          teamContentPath: "/ctx2/_bmad-output",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        },
      },
    });
    const persisted = {
      viewer: {
        schemaVersion: 3,
        openTabs: {
          [TEAM_KEY]: [{ path: "stale.md", name: "stale" }],
          [ANOTHER_TEAM_KEY]: [{ path: "kept.md", name: "kept" }],
          "_no_team:_none": [{ path: "sentinel.md", name: "sentinel" }],
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    // TEAM_KEY ("acme:default") isn't in workspaceRepos → pruned.
    expect(s.openTabs[TEAM_KEY]).toBeUndefined();
    // ANOTHER_TEAM_KEY matches the seeded workspaceRepo → preserved.
    expect(s.openTabs[ANOTHER_TEAM_KEY]).toHaveLength(1);
    // Sentinel bucket is always valid.
    expect(s.openTabs["_no_team:_none"]).toHaveLength(1);
  });

  it("pruneTeamKeyed preserves all entries when workspaceRepos is empty (Phase 10 MED #4)", async () => {
    // T.4 Phase 10 review: if workspaceRepos is still empty after the wait
    // (cold-start race OR active team has no source repos), preserve every
    // persisted entry instead of dropping them. Wiping silently destroys
    // user data; keeping stale entries is recoverable on next session.
    useOrgStore.setState({ workspaceRepos: {} });
    const persisted = {
      viewer: {
        // schemaVersion 3 → post-T.4 pass-through so the test exercises
        // pruneTeamKeyed's empty-workspaceRepos branch without re-running
        // T.4's migration (which would sentinel-collapse TEAM_KEY via
        // Pass 3 before pruning).
        schemaVersion: 3,
        openTabs: {
          [TEAM_KEY]: [{ path: "preserved.md", name: "preserved" }],
          "_no_team:_none": [{ path: "sentinel.md", name: "sentinel" }],
        },
      },
    };
    loadAppStateMock.mockResolvedValue(JSON.stringify(persisted));

    await useViewerStore.getState().loadPersistedState();

    const s = useViewerStore.getState();
    // Both entries preserved — pruning is skipped when workspaceRepos is empty.
    expect(s.openTabs[TEAM_KEY]).toHaveLength(1);
    expect(s.openTabs["_no_team:_none"]).toHaveLength(1);
  });
});

describe("RF-perf-14d — worktreeSetupParallelism load coercion", () => {
  beforeEach(resetState);

  async function loadSettings(worktreeSetupParallelism: unknown) {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 9, settings: { worktreeSetupParallelism } } }),
    );
    await useViewerStore.getState().loadPersistedState();
    return useViewerStore.getState().settings.worktreeSetupParallelism;
  }

  it('round-trips the "auto" sentinel', async () => {
    expect(await loadSettings("auto")).toBe("auto");
  });

  it('round-trips the "off" sentinel (RF-perf-23 AC-1/AC-5 — unbounded persists)', async () => {
    expect(await loadSettings("off")).toBe("off");
  });

  it("round-trips any valid numeric override — presets AND large custom values (RF-perf-23)", async () => {
    // The old 1–4 clamp is gone: setup is a cheap clone, so a high custom value
    // is legitimate and must survive a reload (AC-5 "a huge number persists").
    expect(await loadSettings(1)).toBe(1);
    expect(await loadSettings(4)).toBe(4);
    expect(await loadSettings(8)).toBe(8); // a raised preset
    expect(await loadSettings(12)).toBe(12);
    expect(await loadSettings(10)).toBe(10); // a custom value
    expect(await loadSettings(999)).toBe(999); // MAX_BACKGROUND_SETUP_CAP — still valid
  });

  it('coerces a corrupted/legacy value back to "auto" (radio always has a selection)', async () => {
    expect(await loadSettings("garbage")).toBe("auto");
    // A non-number, non-sentinel value (e.g. a stray boolean) also coerces.
    expect(await loadSettings(true)).toBe("auto");
  });

  it('coerces a non-integer / sub-1 / above-MAX number back to "auto" (no wedged radio)', async () => {
    // These aren't representable as a selected option (the custom input floors +
    // clamps to [1, MAX] on entry), so a persisted one would leave the group with
    // no checked option AND no tab-stop. Above-MAX would also render as
    // sci-notation in the number input — "off" is the unbounded path, not 1e21.
    expect(await loadSettings(0)).toBe("auto");
    expect(await loadSettings(-1)).toBe("auto");
    expect(await loadSettings(2.9)).toBe("auto"); // non-integer
    expect(await loadSettings(1000)).toBe("auto"); // above MAX (999)
    expect(await loadSettings(1e21)).toBe("auto"); // absurd → not a valid cap
  });
});

describe("SW-XS-3 — v11 drops the automatic forward-sync settings", () => {
  beforeEach(resetState);

  const RETIRED = [
    "forwardSyncDefault",
    "forwardSyncOverrides",
    "hydrationMode",
    "autoPullEnabled",
  ];

  async function loadAt(schemaVersion: number) {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion,
          settings: {
            forwardSyncDefault: "live",
            forwardSyncOverrides: { "/wt/a": "on" },
            hydrationMode: "manual",
            autoPullEnabled: false,
            reverseSyncDefault: "interval",
            reverseSyncOverrides: { "/wt/a": "on" },
            maxWorktrees: 7,
            quitOnClose: true,
          },
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    return useViewerStore.getState().settings as unknown as Record<string, unknown>;
  }

  it.each([9, 10])("a v%i blob loads with the retired fields gone", async (version) => {
    const settings = await loadAt(version);
    for (const key of RETIRED) expect(settings).not.toHaveProperty(key);
    // Unrelated settings survive the migration.
    expect(settings.quitOnClose).toBe(true);
  });

  // SW-XS-4: the chain continues to v12, which drops the automatic reverse-sync
  // settings — a v10 (pre-XS-3) and a v11 (XS-3) blob both step through it.
  it.each([10, 11])("a v%i blob loads with the reverse-sync settings gone", async (version) => {
    const settings = await loadAt(version);
    expect(settings).not.toHaveProperty("reverseSyncDefault");
    expect(settings).not.toHaveProperty("reverseSyncOverrides");
    expect(settings.quitOnClose).toBe(true);
  });

  // SW-XS-17: the chain continues to v13, which drops the worktree cap / idle reaper
  // settings — every older blob steps through it.
  it.each([10, 11, 12])("a v%i blob loads with the worktree cap gone", async (version) => {
    const settings = await loadAt(version);
    expect(settings).not.toHaveProperty("maxWorktrees");
    expect(settings.quitOnClose).toBe(true);
  });
});

describe("retained context worktrees", () => {
  beforeEach(resetState);
  it("restores a context workspace by its stable registry id and retains its name and branch", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 13,
          workspaces: {
            [TEAM_KEY]: [
              {
                id: "cw_one",
                kind: "context",
                contextWorktreeId: "cw_one",
                contextWorktreeName: "Sprint 1",
                contextWorktreeBranch: "default/sprint-1",
                cwd: "/w/default/content",
                status: "ready",
              },
            ],
          },
          activeWorkspace: { [TEAM_KEY]: "cw_one" },
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().workspaces[TEAM_KEY]).toEqual([
      expect.objectContaining({
        kind: "context",
        id: "cw_one",
        contextWorktreeName: "Sprint 1",
        contextWorktreeBranch: "default/sprint-1",
      }),
    ]);
  });
});
