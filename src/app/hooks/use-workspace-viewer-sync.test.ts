// SPDX-License-Identifier: MIT
import { useBackendActiveTeamStore } from "@/shared/lib/backend-active-team";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { setCodesenseHomeForTests } from "@/shared/lib/codesense-home";
import { toast } from "sonner";
import { applyWorkspaceConfig, resetWorkspaceConfigView } from "@/shared/stores/config-store";
import { loadWorkspaceConfig, removeWatch } from "@/tauri/commands";
import { useWorkspaceViewerSync } from "./use-workspace-viewer-sync";
import { useAttentionBoard } from "@/features/weft/hooks/use-attention-board";
import { switchContext } from "@/features/viewer/lib/context-switch";
import { useOrgStore } from "@/shared/stores/org-store";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { selectWorkspaceTabs, useViewerStore } from "@/shared/stores/viewer-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";
import type { Attachment, CurrentUser, Org } from "@/shared/types/domain";
import {
  initWatchedCache,
  listAttachmentsByRemote,
  listAttentionBoard,
  reconcileStuckWorktrees,
  reconcileWorktreesOnLaunch,
  setActiveTeamForWave,
} from "@/tauri/commands";

// SW-TS-2: the hook now gates team-switch capture on the TeamSwitch
// `sync-wave-completed` event. Mock `@tauri-apps/api/event` `listen` with a
// controllable registry so tests can register handlers and emit the event.
const eventMock = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      let set = listeners.get(event);
      if (!set) {
        set = new Set();
        listeners.set(event, set);
      }
      set.add(handler);
      return () => {
        set!.delete(handler);
      };
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));

vi.mock("sonner", () => ({
  toast: {
    info: vi.fn(),
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
    loading: vi.fn().mockReturnValue("toast-id"),
    dismiss: vi.fn(),
  },
}));

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    addWatch: vi.fn().mockResolvedValue(undefined),
    removeWatch: vi.fn().mockResolvedValue(undefined),
    saveAppState: vi.fn().mockResolvedValue(undefined),
    loadAppState: vi.fn().mockResolvedValue("{}"),
    pushViewerState: vi.fn().mockResolvedValue(undefined),
    listNotes: vi.fn().mockResolvedValue([]),
    readNote: vi.fn().mockResolvedValue({
      id: "n1",
      title: "Scratch",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    }),
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
    startWatching: vi.fn().mockResolvedValue(undefined),
    stopWatching: vi.fn().mockResolvedValue(undefined),
    checkPathExists: vi.fn().mockResolvedValue(true),
    // F-6: workspace-sync calls this once per hard switch / soft refresh.
    listAttachmentsByRemote: vi.fn().mockResolvedValue(new Map()),
    // SS-4c AC-13/AC-14/AC-16: wave-engine triggers.
    setActiveTeamForWave: vi.fn().mockResolvedValue(undefined),
    // Activity's board read (the backend resolves it from its active team).
    listAttentionBoard: vi.fn().mockResolvedValue({
      youOwe: [],
      owedToYou: [],
      onAgent: [],
      parked: [],
      doneUnacked: [],
    }),
    // SW-TS-5 §4.4: team-agnostic worktree launch reconcile.
    reconcileWorktreesOnLaunch: vi.fn().mockResolvedValue(undefined),
    // SLW-FX-3 (AC-1): in-session stuck-only sweep (window focus / visibility).
    reconcileStuckWorktrees: vi.fn().mockResolvedValue(undefined),
    // R-3: atomic cache init + capture commands.
    initWatchedCache: vi.fn().mockResolvedValue(0),
    // R-10/R-1: explicit default mock — overridden per-test when a test
    // needs workspace-config overrides to thread through.
    loadWorkspaceConfig: vi.fn().mockResolvedValue({ repos: {} }),
    // SF-8: `resolveTeamContentDir` retired from this hook — buildSyntheticRepo
    // now resolves teamContentPath synchronously via `pathToTeamContentDirSync`.
    // Kept as an export-shaped stub in case other consumers import it.
    resolveTeamContentDir: vi.fn(
      async (orgRoot: string, team: string) => `${orgRoot}/_team/${team}/content`,
    ),
  };
});

// F-25: user identity is read from `useCurrentUser` (the active local
// profile). Mock the helper directly so tests can flip identity
// without standing up a local-profile fixture.
let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

installTerminalLayoutHarness();

/** The session ids a team's workspace shows, from the host's layout. */
const sessionsOf = (teamKey: string, workspaceId: string) =>
  selectWorkspaceTabs(useViewerStore.getState(), teamKey, workspaceId).map((t) => t.sessionId);

const mockedListAttachmentsByRemote = vi.mocked(listAttachmentsByRemote);
const mockedInitWatchedCache = vi.mocked(initWatchedCache);
const mockedSetActiveTeamForWave = vi.mocked(setActiveTeamForWave);
/** The teams committed through `setActiveTeamForWave`, in call order. */
const committedTeams = () => mockedSetActiveTeamForWave.mock.calls.map((c) => c[0].team);
const mockedReconcileWorktreesOnLaunch = vi.mocked(reconcileWorktreesOnLaunch);
const mockedReconcileStuckWorktrees = vi.mocked(reconcileStuckWorktrees);

function makeUser(githubUserId = 1234): CurrentUser {
  return {
    github_user_id: githubUserId,
    github_username: "testuser",
    display_name: "Test User",
    avatar_url: null,
  };
}

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "9north",
    display_name: "9North Studio",
    created_at: "2025-01-01T00:00:00Z",
    remote_url: "https://github.com/9north/context",
    config_flags: {},
    people: [],
    teams: [
      {
        name: "default",
        source_repos: [
          { owner: "acme", repo: "api" },
          { owner: "acme", repo: "web" },
        ],
      },
    ],
    org_root: "/home/user/.codesense/users/1234/orgs/9north",
    ...overrides,
  };
}

function makeOrgSingle(slug = "solo"): Org {
  return makeOrg({
    slug,
    display_name: "Solo Org",
    org_root: `/home/user/.codesense/users/1234/orgs/${slug}`,
    teams: [
      {
        name: "default",
        source_repos: [{ owner: "solo", repo: "only" }],
      },
    ],
  });
}

describe("useWorkspaceViewerSync", () => {
  beforeEach(() => {
    for (const scope of Object.keys(warmScopes.debugCounts().tiers)) warmScopes.forget(scope);
    resetWorkspaceConfigView();
    setCodesenseHomeForTests("/home/user/.codesense");
    currentUserValue = null;
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
      workspaceRepos: {},
      suspendedWorkspaceRepos: {},
      attachmentsVersion: 0,
    });
    useViewerStore.setState({
      openedTeams: [],
      workspaces: {},
      activeWorkspace: {},
    });
    vi.clearAllMocks();
    eventMock.listeners.clear();
  });

  it("does nothing when no org is active", () => {
    renderHook(() => useWorkspaceViewerSync());
    expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(0);
  });

  it("does nothing when activeOrgSlug is set but orgs list is empty (cold-start race)", () => {
    useOrgStore.setState({ activeOrgSlug: "9north" });
    renderHook(() => useWorkspaceViewerSync());
    expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(0);
  });

  it("builds synthetic repos when org becomes available", async () => {
    const org = makeOrg();
    useOrgStore.setState({ activeOrgSlug: "9north", orgs: [org] });

    renderHook(() => useWorkspaceViewerSync());

    // F-6: workspace-sync awaits listAttachmentsByRemote before building synthetics.
    await vi.waitFor(() => {
      const wsRepos = useOrgStore.getState().workspaceRepos;
      expect(Object.keys(wsRepos)).toHaveLength(2);
      expect(wsRepos["workspace:9north:default:acme/api"]).toBeDefined();
      expect(wsRepos["workspace:9north:default:acme/web"]).toBeDefined();
    });
  });

  it("tears down outgoing repos on workspace switch", async () => {
    const org1 = makeOrgSingle("org1");
    const org2 = makeOrgSingle("org2");
    useOrgStore.setState({ activeOrgSlug: "org1", orgs: [org1, org2] });

    const { rerender } = renderHook(() => useWorkspaceViewerSync());

    // Verify org1 repos are set up
    await vi.waitFor(() => {
      expect(
        useOrgStore.getState().workspaceRepos["workspace:org1:default:solo/only"],
      ).toBeDefined();
    });

    // Switch to org2
    act(() => {
      useOrgStore.setState({ activeOrgSlug: "org2" });
    });
    rerender();

    // org1 repos should be gone, org2 repos should be present
    await vi.waitFor(() => {
      const wsRepos = useOrgStore.getState().workspaceRepos;
      expect(wsRepos["workspace:org1:default:solo/only"]).toBeUndefined();
      expect(wsRepos["workspace:org2:default:solo/only"]).toBeDefined();
    });
  });

  it("skips teardown/rebuild on no-op org refresh (same source_repos)", () => {
    const org = makeOrgSingle();
    useOrgStore.setState({ activeOrgSlug: "solo", orgs: [org] });

    const { rerender } = renderHook(() => useWorkspaceViewerSync());

    const reposBefore = useOrgStore.getState().workspaceRepos;

    // Simulate org refresh with same data
    act(() => {
      useOrgStore.setState({ orgs: [{ ...org }] });
    });
    rerender();

    // workspaceRepos should be the same object (not rebuilt)
    expect(useOrgStore.getState().workspaceRepos).toBe(reposBefore);
  });

  it("rebuilds on team switch", async () => {
    const org = makeOrg({
      teams: [
        {
          name: "default",
          source_repos: [{ owner: "acme", repo: "api" }],
        },
        {
          name: "platform",
          source_repos: [{ owner: "acme", repo: "infra" }],
        },
      ],
    });
    useOrgStore.setState({ activeOrgSlug: "9north", orgs: [org] });

    const { rerender } = renderHook(() => useWorkspaceViewerSync());

    await vi.waitFor(() => {
      expect(
        useOrgStore.getState().workspaceRepos["workspace:9north:default:acme/api"],
      ).toBeDefined();
    });

    // Switch team
    act(() => {
      useOrgStore.setState({ activeTeamName: "platform" });
    });
    rerender();

    await vi.waitFor(() => {
      const wsRepos = useOrgStore.getState().workspaceRepos;
      expect(wsRepos["workspace:9north:default:acme/api"]).toBeUndefined();
      expect(wsRepos["workspace:9north:platform:acme/infra"]).toBeDefined();
    });
  });

  it("clears synthetic repos when activeOrgSlug becomes null", async () => {
    const org = makeOrgSingle();
    useOrgStore.setState({ activeOrgSlug: "solo", orgs: [org] });

    const { rerender } = renderHook(() => useWorkspaceViewerSync());

    await vi.waitFor(() => {
      expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(1);
    });

    // Clear active org
    act(() => {
      useOrgStore.setState({ activeOrgSlug: null });
    });
    rerender();

    expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(0);
  });

  it("fires when orgs load after slug is already set (cold-start race)", async () => {
    // Slug restored from persistence before org discovery
    useOrgStore.setState({ activeOrgSlug: "9north", orgs: [] });

    const { rerender } = renderHook(() => useWorkspaceViewerSync());
    expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(0);

    // Orgs arrive from disk discovery
    act(() => {
      useOrgStore.setState({ orgs: [makeOrg()] });
    });
    rerender();

    await vi.waitFor(() => {
      expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(2);
    });
  });

  // ---- F-6 AC-17: re-trigger semantics (R3-P3 patch) ----

  describe("F-6 AC-17: re-trigger semantics", () => {
    it("re-runs when currentUser transitions null → set (login completes)", async () => {
      const org = makeOrgSingle();
      // Pre-login: org is active but no user yet.
      currentUserValue = null;
      useOrgStore.setState({
        activeOrgSlug: "solo",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      // Initial run: localPath null (empty attachments map for null user).
      await vi.waitFor(() => {
        const repo = useOrgStore.getState().workspaceRepos["workspace:solo:default:solo/only"];
        expect(repo).toBeDefined();
        expect(repo!.localPath).toBeNull();
      });

      // R4-3: the pre-login render did NOT call the wrapper —
      // `loadAttachmentsMap` short-circuits to `new Map()` when
      // `githubUserId === null` (no IPC fires). The `mockClear()` is
      // defensive (e.g., against StrictMode double-invoke) and makes the
      // call-count assertion below unambiguous.
      mockedListAttachmentsByRemote.mockClear();
      // Subsequent calls return a Map with a matching attachment so the
      // re-run can be observed via localPath populating. Use
      // `mockResolvedValue` (not `mockResolvedValueOnce`) so any unexpected
      // extra invocation (StrictMode, re-render churn) keeps returning the
      // same value rather than falling back to the default empty Map.
      mockedListAttachmentsByRemote.mockResolvedValue(
        new Map([
          [
            "github.com/solo/only",
            {
              repo_path: "/Users/dev/solo-only",
              remote_url: "https://github.com/solo/only.git",
              attached_at: null,
              tracking_prompt_dismissed_at: null,
              credential_ref: null,
            },
          ],
        ]),
      );

      // Login completes — flip the mocked CurrentUser, then re-render
      // so the hook re-evaluates useCurrentUser() and the effect dep
      // `githubUserId` transitions null → 1234. The assignment is a
      // plain JS mutation (not a React state update), so no act() wrap.
      currentUserValue = makeUser(1234);
      rerender();

      // Re-run must fire: listAttachmentsByRemote called again, localPath
      // resolves to the attached clone path. The no-op guard must NOT
      // short-circuit even though orgSlug/team/sourceReposKey are unchanged.
      // R4-3: assert "called" rather than "called exactly once" so the test
      // is robust to harmless re-renders that also pass the guard.
      await vi.waitFor(() => {
        expect(mockedListAttachmentsByRemote).toHaveBeenCalled();
        const repo = useOrgStore.getState().workspaceRepos["workspace:solo:default:solo/only"];
        expect(repo!.localPath).toBe("/Users/dev/solo-only");
      });
    });

    it("re-runs when attachmentsVersion is bumped", async () => {
      const org = makeOrgSingle();
      currentUserValue = makeUser(1234);
      useOrgStore.setState({
        activeOrgSlug: "solo",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(
          useOrgStore.getState().workspaceRepos["workspace:solo:default:solo/only"],
        ).toBeDefined();
      });

      mockedListAttachmentsByRemote.mockClear();
      // R4-3: see comment on the prior test — prefer `mockResolvedValue`
      // over `mockResolvedValueOnce` so any unexpected re-render returns
      // the same value rather than falling back to the empty default.
      mockedListAttachmentsByRemote.mockResolvedValue(
        new Map([
          [
            "github.com/solo/only",
            {
              repo_path: "/Users/dev/freshly-attached",
              remote_url: "https://github.com/solo/only.git",
              attached_at: null,
              tracking_prompt_dismissed_at: null,
              credential_ref: null,
            },
          ],
        ]),
      );

      // User completes an attach via AttachLocalCloneDialog — bumps version.
      act(() => {
        useOrgStore.getState().bumpAttachmentsVersion();
      });
      rerender();

      // Soft-refresh fires: attachments reloaded, localPath populates.
      await vi.waitFor(() => {
        expect(mockedListAttachmentsByRemote).toHaveBeenCalled();
        const repo = useOrgStore.getState().workspaceRepos["workspace:solo:default:solo/only"];
        expect(repo!.localPath).toBe("/Users/dev/freshly-attached");
      });
    });

    it("no-op guard short-circuits when repos + user + version unchanged", async () => {
      const org = makeOrgSingle();
      currentUserValue = makeUser(1234);
      useOrgStore.setState({
        activeOrgSlug: "solo",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      // Wait for initial sync to settle.
      await vi.waitFor(() => {
        expect(
          useOrgStore.getState().workspaceRepos["workspace:solo:default:solo/only"],
        ).toBeDefined();
      });

      // Clear the initial-run call so we can assert zero additional calls.
      mockedListAttachmentsByRemote.mockClear();

      // Irrelevant state mutation (e.g., a different org-store field
      // updating). The hook's effect re-runs but the no-op guard should
      // short-circuit before kicking off any async work.
      act(() => {
        // Simulate an org-list refresh with identical content (same source_repos).
        useOrgStore.setState({ orgs: [{ ...org }] });
      });
      rerender();

      // Give any spurious async work a chance to surface.
      await new Promise((resolve) => setTimeout(resolve, 20));

      expect(mockedListAttachmentsByRemote).not.toHaveBeenCalled();
    });
  });

  // ---- F-19 / F-20 — Terminal persistence across org/team switches ----

  describe("F-19/F-20: suspend/resume", () => {
    it("F-19: org switch suspends outgoing team (openedTeams + terminal state preserved)", async () => {
      const orgA = makeOrgSingle("orgA");
      const orgB = makeOrgSingle("orgB");
      useOrgStore.setState({ activeOrgSlug: "orgA", orgs: [orgA, orgB] });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      const repoA = "workspace:orgA:default:solo/only";
      const teamA = "orgA:_none";

      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined();
        expect(useViewerStore.getState().openedTeams).toContain(teamA);
      });

      // Seed a running terminal under orgA's team key (keys are team-scoped).
      seedTerminalTabs(teamA, "root", ["sess-1"]);

      // Switch to orgB
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "orgB" });
      });
      rerender();

      await vi.waitFor(() => {
        const wsRepos = useOrgStore.getState().workspaceRepos;
        expect(wsRepos["workspace:orgB:default:solo/only"]).toBeDefined();
      });

      // orgA's team key should still be in openedTeams (suspended, not closed).
      const vs = useViewerStore.getState();
      expect(vs.openedTeams).toContain(teamA);
      expect(sessionsOf(teamA, "root")).toEqual(["sess-1"]);
      expect(terminalHost().contains("sess-1")).toBe(true);

      // orgA's workspaceRepos should be in suspendedWorkspaceRepos
      const os = useOrgStore.getState();
      expect(os.suspendedWorkspaceRepos[teamA]).toBeDefined();
      expect(os.suspendedWorkspaceRepos[teamA][repoA]).toBeDefined();
    });

    it("F-19: A→B→A round-trip resumes suspended workspace (terminals alive)", async () => {
      const orgA = makeOrgSingle("orgA");
      const orgB = makeOrgSingle("orgB");
      useOrgStore.setState({ activeOrgSlug: "orgA", orgs: [orgA, orgB] });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      const repoA = "workspace:orgA:default:solo/only";
      const teamA = "orgA:_none";

      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined();
      });

      // Seed a running terminal under orgA's team key (keys are team-scoped).
      seedTerminalTabs(teamA, "root", ["sess-1"]);

      // Switch A → B
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "orgB" });
      });
      rerender();

      await vi.waitFor(() => {
        expect(
          useOrgStore.getState().workspaceRepos["workspace:orgB:default:solo/only"],
        ).toBeDefined();
      });

      // Switch B → A (resume)
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "orgA" });
      });
      rerender();

      // orgA's workspaceRepos should be restored from suspended
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined();
      });

      // Suspended entry should be removed
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamA]).toBeUndefined();

      // Terminal state should still be intact
      const vs = useViewerStore.getState();
      expect(vs.openedTeams).toContain(teamA);
      expect(sessionsOf(teamA, "root")).toEqual(["sess-1"]);
      expect(terminalHost().contains("sess-1")).toBe(true);
      // The hook never attached a watch (SW-WL-3), so suspending a team removes none.
      expect(removeWatch).not.toHaveBeenCalled();
    });

    it("reconciles resumed mainDoc from the current owned view without reading or updating suspended repos", async () => {
      const orgA = makeOrgSingle("orgA");
      const orgB = makeOrgSingle("orgB");
      const repoA = "workspace:orgA:default:solo/only";
      useOrgStore.setState({ activeOrgSlug: "orgA", orgs: [orgA, orgB] });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await waitFor(() => expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined());
      act(() => applyWorkspaceConfig({ repos: { [repoA]: { mainDoc: "Old.md" } } }));
      act(() => useOrgStore.setState({ activeOrgSlug: "orgB" }));
      rerender();
      await waitFor(() =>
        expect(useOrgStore.getState().suspendedWorkspaceRepos["orgA:_none"]?.[repoA]?.mainDoc).toBe(
          "Old.md",
        ),
      );
      const hidden = useOrgStore.getState().suspendedWorkspaceRepos;
      act(() => applyWorkspaceConfig({ repos: { [repoA]: { mainDoc: "Latest.md" } } }));
      expect(useOrgStore.getState().suspendedWorkspaceRepos).toBe(hidden);
      vi.mocked(loadWorkspaceConfig).mockClear();
      act(() => useOrgStore.setState({ activeOrgSlug: "orgA" }));
      rerender();
      await waitFor(() =>
        expect(useOrgStore.getState().workspaceRepos[repoA]?.mainDoc).toBe("Latest.md"),
      );
      expect(loadWorkspaceConfig).not.toHaveBeenCalled();
    });

    it("uses the owned config for rebuilt repos without a separate full read", async () => {
      currentUserValue = makeUser();
      const org = makeOrgSingle();
      const repo = "workspace:solo:default:solo/only";
      applyWorkspaceConfig({ repos: { [repo]: { mainDoc: "Owned.md" } } });
      useOrgStore.setState({ activeOrgSlug: "solo", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await waitFor(() =>
        expect(useOrgStore.getState().workspaceRepos[repo]?.mainDoc).toBe("Owned.md"),
      );
      expect(loadWorkspaceConfig).not.toHaveBeenCalled();
      act(() =>
        useOrgStore.setState({ attachmentsVersion: useOrgStore.getState().attachmentsVersion + 1 }),
      );
      await waitFor(() => expect(listAttachmentsByRemote).toHaveBeenCalledTimes(2));
      expect(useOrgStore.getState().workspaceRepos[repo]?.mainDoc).toBe("Owned.md");
      expect(loadWorkspaceConfig).not.toHaveBeenCalled();
    });

    it("F-19: team switch within same org suspends outgoing team's repos", async () => {
      const org = makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
          {
            name: "platform",
            source_repos: [{ owner: "acme", repo: "infra" }],
          },
        ],
      });
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      const repoDefault = "workspace:9north:default:acme/api";
      const teamDefault = "9north:default";
      const teamPlatform = "9north:platform";

      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoDefault]).toBeDefined();
        expect(useViewerStore.getState().openedTeams).toContain(teamDefault);
      });

      // Seed a running terminal under the outgoing team's key.
      seedTerminalTabs(teamDefault, "root", ["sess-team"]);

      // Switch team
      act(() => {
        useOrgStore.setState({ activeTeamName: "platform" });
      });
      rerender();

      await vi.waitFor(() => {
        expect(
          useOrgStore.getState().workspaceRepos["workspace:9north:platform:acme/infra"],
        ).toBeDefined();
        expect(useViewerStore.getState().openedTeams).toContain(teamPlatform);
      });

      // Default team's key should still be in openedTeams (suspended).
      const vs = useViewerStore.getState();
      expect(vs.openedTeams).toContain(teamDefault);
      expect(sessionsOf(teamDefault, "root")).toEqual(["sess-team"]);
    });

    // PTS-5 AC 11b: workspace-resume staleness fix. The watcher was torn down
    // while suspended, so any files written during suspension never bumped
    // fileRefreshKeys. On resume the cache rebuilds via mtime_diff
    // (initWatchedCache) but TS pipeline views won't re-derive unless their
    // refresh key bumps. Verify the resume path bumps every resumed repo,
    // strictly above the pre-resume baseline (so the bump can't be confused
    // with one from the initial mount or a sibling fan-out).
    it("PTS-5: resume bumps fileRefreshKeys for every repo in the resumed workspace", async () => {
      // Logged-in path — currentUserValue must be set before render or the
      // hook short-circuits without exercising the resume code path.
      currentUserValue = makeUser();

      const orgA = makeOrg({
        slug: "9north",
        org_root: "/home/user/.codesense/users/1234/orgs/9north",
        teams: [
          {
            name: "smart-web",
            source_repos: [
              { owner: "mycorp", repo: "mycorp_app" },
              { owner: "AllWidgets", repo: "AllWidgets" },
            ],
          },
        ],
      });
      const orgB = makeOrgSingle("orgB");
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "smart-web",
        orgs: [orgA, orgB],
      });

      const repoMycorp = "workspace:9north:smart-web:mycorp/mycorp_app";
      const repoAllWidgets = "workspace:9north:smart-web:AllWidgets/AllWidgets";

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoMycorp]).toBeDefined();
        expect(useOrgStore.getState().workspaceRepos[repoAllWidgets]).toBeDefined();
      });

      // Switch away to orgB — 9north smart-web gets suspended.
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "orgB", activeTeamName: null });
      });
      rerender();
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoMycorp]).toBeUndefined();
        expect(useOrgStore.getState().suspendedWorkspaceRepos["9north:smart-web"]).toBeDefined();
      });

      // Snapshot baselines AFTER the suspend transition settles (orgB's own
      // initWatchedCache has already fired) and BEFORE the resume switch.
      // Anything that grows from here must have originated from the resume
      // path under test — the spy can no longer be satisfied by orgB's init.
      const baselineInitCount = mockedInitWatchedCache.mock.calls.length;
      const baselineMycorp = useFileRefreshStore.getState().fileRefreshKeys[repoMycorp] ?? 0;
      const baselineAllWidgets =
        useFileRefreshStore.getState().fileRefreshKeys[repoAllWidgets] ?? 0;

      // Switch back — 9north smart-web gets RESUMED.
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "smart-web" });
      });
      rerender();

      // Wait for the resume IIFE to reach initWatchedCache — proof that the
      // suspended-then-resumed branch (not just any rebuild) actually ran.
      await vi.waitFor(() => {
        expect(mockedInitWatchedCache.mock.calls.length).toBeGreaterThan(baselineInitCount);
      });

      // Both repos must bump strictly ABOVE baseline. `>=` would pass even if
      // the bump never fires (baseline is already 0).
      await vi.waitFor(() => {
        const keys = useFileRefreshStore.getState().fileRefreshKeys;
        expect(keys[repoMycorp] ?? 0).toBeGreaterThan(baselineMycorp);
        expect(keys[repoAllWidgets] ?? 0).toBeGreaterThan(baselineAllWidgets);
      });
    });

    /** Visit teams the owner did not see before. A team the owner saw first is suspended once three
     *  newer teams have been active, and dropped once more than the threshold of switches passed. */
    const visitOtherTeams = (...keys: string[]) =>
      act(() => {
        for (const key of keys) warmScopes.activate(key);
      });

    it("F-19: a suspended workspace the owner drops past the switch threshold is released, PTYs kept (AC-6)", async () => {
      const orgA = makeOrgSingle("orgA");
      const orgB = makeOrgSingle("orgB");
      const orgStale = makeOrgSingle("stale");
      const repoA = "workspace:orgA:default:solo/only";
      const repoStale = "workspace:stale:default:solo/only";
      const teamStale = "stale:default";
      const workspaceStale = {
        id: "SW-9",
        kind: "story" as const,
        cwd: "/wt/SW-9",
        storyFilePath: "/stories/SW-9.md",
        storyId: "SW-9",
        status: "ready" as const,
      };
      const teamRecent = "recent:_none";

      // A workspace that was suspended long ago: the owner saw it first, then lost it.
      useOrgStore.setState({
        activeOrgSlug: "orgA",
        orgs: [orgA, orgB, orgStale],
        suspendedWorkspaceRepos: {
          [teamStale]: {
            [repoStale]: {
              repoPath: repoStale,
              repoName: "stale",
              contextRepoPath: "/fake",
              teamContentPath: "",
              hasRemote: false,
              remoteUrl: null,
              mainDoc: "",
              localPath: null,
            },
          },
          [teamRecent]: {
            "workspace:recent:default:solo/only": {
              repoPath: "workspace:recent:default:solo/only",
              repoName: "recent",
              contextRepoPath: "/fake2",
              teamContentPath: "",
              hasRemote: false,
              remoteUrl: null,
              mainDoc: "",
              localPath: null,
            },
          },
        },
      });

      // Seed viewer state for the stale team so eviction can unmount it and a
      // later switch can prove the same workspace terminal is reattached.
      useViewerStore.setState({
        openedTeams: [teamStale],
        workspaces: { [teamStale]: [workspaceStale] },
      });
      seedTerminalTabs(teamStale, workspaceStale.id, ["sess-evict"]);
      useViewerStore.getState().setSessionAiStatus("sess-evict", "running");
      useViewerStore.getState().setSessionProgress("sess-evict", {
        phase: "dev",
        note: "working",
      });

      visitOtherTeams(teamStale);

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined();
      });

      // Three newer teams: stale is suspended, but only just (4 switches since it was active).
      visitOtherTeams("x:1", "x:2", "x:3");
      expect(warmScopes.tierOf(teamStale)).toBe("suspended");
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamStale]).toBeDefined();
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamRecent]).toBeDefined();

      // Exactly the threshold of switches is still alive; one more is past it and the owner drops it.
      visitOtherTeams("x:4");
      expect(warmScopes.tierOf(teamStale)).toBe("suspended");
      visitOtherTeams("x:5");
      expect(warmScopes.tierOf(teamStale)).toBe("gone");
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamStale]).toBeUndefined();

      // Eviction should release renderer state while leaving the global
      // terminal session and its team/workspace attachment available to re-open.
      expect(useViewerStore.getState().openedTeams).not.toContain(teamStale);
      expect(terminalHost().contains("sess-evict")).toBe(true);
      expect(sessionsOf(teamStale, workspaceStale.id)).toEqual(["sess-evict"]);
      expect(useViewerStore.getState().aiStatusBySession["sess-evict"]).toBe("running");
      expect(useViewerStore.getState().progressBySession["sess-evict"]).toEqual({
        phase: "dev",
        note: "working",
      });
      expect(useViewerStore.getState().progressTickBySession["sess-evict"]).toBeDefined();

      // Return through the app's context switch path. The renderer/team view
      // mounts again with its workspace tile still attached to the live PTY.
      act(() => {
        expect(
          switchContext({ teamKey: teamStale, workspaceId: workspaceStale.id }, "switcher"),
        ).toBe(true);
      });
      rerender();
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoStale]).toBeDefined();
        expect(useViewerStore.getState().openedTeams).toContain(teamStale);
      });
      expect(useViewerStore.getState().workspaces[teamStale]).toContainEqual(workspaceStale);
      expect(sessionsOf(teamStale, workspaceStale.id)).toEqual(["sess-evict"]);
      expect(terminalHost().contains("sess-evict")).toBe(true);

      // recent (delta=3) should still be suspended
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamRecent]).toBeDefined();
    });

    it("RF-perf-10: does NOT release a suspended team pinned for unsaved editor buffers", async () => {
      // A team past the switch threshold is normally closed (renderer state freed). A team with
      // unsaved edits is pinned warm by the render gate, since the live CodeMirror buffer exists
      // only in its mounted subtree: it must be spared until the edit is committed or discarded.
      const orgA = makeOrgSingle("orgA");
      const repoA = "workspace:orgA:default:solo/only";
      const repoStale = "workspace:stale:default:solo/only";
      const teamStale = "stale:_none";

      useOrgStore.setState({
        activeOrgSlug: "orgA",
        orgs: [orgA],
        suspendedWorkspaceRepos: {
          [teamStale]: {
            [repoStale]: {
              repoPath: repoStale,
              repoName: "stale",
              contextRepoPath: "/fake",
              teamContentPath: "",
              hasRemote: false,
              remoteUrl: null,
              mainDoc: "",
              localPath: null,
            },
          },
        },
      });
      useViewerStore.setState({
        openedTeams: [teamStale],
        editingDirtyPaths: { [teamStale]: ["/fake/story.md"] },
      });
      seedTerminalTabs(teamStale, "root", ["sess-dirty"]);
      visitOtherTeams(teamStale);
      let unpin = () => {};
      act(() => {
        unpin = warmScopes.pin(teamStale, "unsaved-edits");
      });

      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useOrgStore.getState().workspaceRepos[repoA]).toBeDefined();
      });
      visitOtherTeams("x:1", "x:2", "x:3", "x:4", "x:5");

      // Dirty: spared. Still warm, still suspended-with-repos, still opened, terminals running.
      expect(warmScopes.tierOf(teamStale)).toBe("warm");
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamStale]).toBeDefined();
      expect(useViewerStore.getState().openedTeams).toContain(teamStale);
      expect(sessionsOf(teamStale, "root")).toEqual(["sess-dirty"]);

      // Once the edit is committed the pin goes and the owner drops it as usual.
      act(() => unpin());
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamStale]).toBeUndefined();
      expect(useViewerStore.getState().openedTeams).not.toContain(teamStale);
    });

    it("F-20: after release, useResolvedRepo returns undefined for the released repo", async () => {
      // Verifies that release makes the repo unresolvable, so
      // RepoViewerView unmounts and term.dispose() runs correctly.
      const { useResolvedRepo } = await import("@/shared/lib/use-resolved-repo");
      const orgA = makeOrgSingle("orgA");
      const repoStale = "workspace:stale:default:solo/only";

      useOrgStore.setState({
        activeOrgSlug: "orgA",
        orgs: [orgA],
        suspendedWorkspaceRepos: {
          "stale:_none": {
            [repoStale]: {
              repoPath: repoStale,
              repoName: "stale",
              contextRepoPath: "/fake",
              teamContentPath: "",
              hasRemote: false,
              remoteUrl: null,
              mainDoc: "",
              localPath: null,
            },
          },
        },
      });
      visitOtherTeams("stale:_none");

      // Before release: repo is resolvable via suspended fallback (F-20 fix)
      const { result: resolveResult } = renderHook(() => useResolvedRepo(repoStale));
      expect(resolveResult.current).toBeDefined();

      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(
          useOrgStore.getState().workspaceRepos["workspace:orgA:default:solo/only"],
        ).toBeDefined();
      });

      visitOtherTeams("x:1", "x:2", "x:3", "x:4", "x:5");
      await vi.waitFor(() => {
        expect(useOrgStore.getState().suspendedWorkspaceRepos["stale:_none"]).toBeUndefined();
      });

      // After release: repo is no longer resolvable
      expect(resolveResult.current).toBeUndefined();
    });

    it("SW-FX-65: a repo-less backgrounded team stays opened while it is warm, and is released when the owner drops it", async () => {
      // A team with zero source_repos still mounts full-fat heavyweight surfaces behind
      // display:none. It ages out like any team: the owner tracks its scope, no repo snapshot.
      const reader = makeOrg({
        slug: "reader",
        display_name: "Reader Org",
        org_root: "/home/user/.codesense/users/1234/orgs/reader",
        teams: [{ name: "default", source_repos: [] }],
      });
      const orgB = makeOrgSingle("orgB");
      useOrgStore.setState({ activeOrgSlug: "reader", orgs: [reader, orgB] });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      const teamReader = "reader:_none";

      // Repo-less team opens (no synthetics, but the team pane mounts) and is the active scope.
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain(teamReader);
      });
      expect(warmScopes.tierOf(teamReader)).toBe("active");

      // Switch away: it stays opened (warm), not closed.
      act(() => {
        useOrgStore.setState({ activeOrgSlug: "orgB" });
      });
      rerender();
      await vi.waitFor(() => {
        expect(warmScopes.tierOf(teamReader)).toBe("warm");
      });
      expect(useViewerStore.getState().openedTeams).toContain(teamReader);

      // Visit enough other teams and it is released with its terminal sessions kept.
      visitOtherTeams("x:1", "x:2", "x:3", "x:4", "x:5");
      expect(warmScopes.tierOf(teamReader)).toBe("gone");
      expect(useViewerStore.getState().openedTeams).not.toContain(teamReader);
    });
  });

  // =========================================================================
  // B-3: Team switch install
  // =========================================================================
  describe("B-3: team switch install", () => {
    function makeOrgWithTeams(): Org {
      return makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
          {
            name: "beta",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
        ],
      });
    }

    it("commits the new team (and writes nothing) when the team changes", async () => {
      const org = makeOrgWithTeams();
      currentUserValue = makeUser();

      // Provide attachment for acme/api so it passes the filter.
      const attachments = new Map([
        ["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as any],
      ]);
      mockedListAttachmentsByRemote.mockResolvedValue(attachments);

      // Initial render with team "default".
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(Object.keys(useOrgStore.getState().workspaceRepos).length).toBeGreaterThan(0);
      });

      // Switch to team "beta".
      act(() => {
        useOrgStore.setState({ activeTeamName: "beta" });
      });
      rerender();

      await vi.waitFor(() => {
        expect(committedTeams()).toContain("beta");
      });

      const call = mockedSetActiveTeamForWave.mock.calls.find((c) => c[0].team === "beta")![0];
      expect(call.orgSlug).toBe("9north");
      expect(call.sourceRepos).toEqual([{ owner: "acme", repo: "api" }]);
    });
  });

  // ---- R-3 — Atomic init_watched_cache ----

  describe("R-3: initWatchedCache on startup", () => {
    it("calls initWatchedCache during hard switch", async () => {
      const org = makeOrgSingle();
      useOrgStore.setState({ activeOrgSlug: "solo", orgs: [org] });

      renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(mockedInitWatchedCache).toHaveBeenCalledTimes(1);
        const [orgRoot, cachePath] = mockedInitWatchedCache.mock.calls[0];
        expect(orgRoot).toBe(org.org_root);
        expect(cachePath).toContain("/cache/frontmatter-cache.json");
      });
    });

    // SW-WL-3 (AC-12): the per-repo eager watcher attach was removed from
    // init_watched_cache — attach is now surface-bound (driven by
    // WorkspaceOpenSet per open workspace), so initWatchedCache no longer
    // receives per-repo watch configs. The "passes watch configs for each
    // tracked repo" test that asserted that behavior was retired here.
  });

  // T.2: auto-sync fires an orphan-warning toast when a tracked repo is removed
  // while story workspaces under it remain open, and never mints a root
  // workspace. Review iter-1, Task 5.5.
  describe("T.2 auto-sync", () => {
    const TEAM_KEY = "solo:default";

    it("does NOT mint any root on first activation (SW-EW-1c retired)", async () => {
      // SW-EW-1c retired root workspaces; SW-WV-A6 deleted `ensureRootWorkspaces`
      // outright — the auto-sync effect never mints a per-source-repo root on
      // activation. The controller CLI is ensured on demand instead.
      const org = makeOrgSingle("solo");
      useOrgStore.setState({ activeOrgSlug: "solo", activeTeamName: "default", orgs: [org] });

      renderHook(() => useWorkspaceViewerSync());

      // Let the activation effect settle, then assert no root was minted.
      await vi.waitFor(() => {
        expect(mockedInitWatchedCache).toHaveBeenCalled();
      });
      await new Promise((resolve) => setTimeout(resolve, 20));
      const ws = useViewerStore.getState().workspaces[TEAM_KEY] ?? [];
      expect(ws.filter((w) => w.kind === "root")).toHaveLength(0);
    });

    it("fires the orphan-warning toast once per removed slug when its story workspaces remain", async () => {
      // Start with a two-repo team so we can drop one of them.
      const org = makeOrg(); // teams[0].source_repos = [acme/api, acme/web]
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      // SW-EW-1c: no roots are minted anymore. Wait for the initial activation
      // effect to settle (so prevRef captures the two-repo source_repos key)
      // before mutating source_repos below.
      await vi.waitFor(() => {
        expect(mockedInitWatchedCache).toHaveBeenCalled();
      });

      // Seed an open story workspace under `acme/web` so it counts as orphaned
      // when `acme/web` drops from source_repos.
      act(() => {
        useViewerStore.setState((s) => ({
          workspaces: {
            ...s.workspaces,
            "9north:default": [
              ...(s.workspaces["9north:default"] ?? []),
              {
                id: "R-1",
                kind: "story",
                cwd: "/wt/R-1",
                storyFilePath: "/repo/R-1.md",
                storyId: "R-1",
                status: "ready",
                sourceRepoSlug: "acme/web",
              },
            ],
          },
        }));
      });

      // Mutate source_repos: drop acme/web. The hook's effect should
      // detect the team-internal mutation and fire the orphan toast.
      const trimmedOrg = makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
        ],
      });
      act(() => {
        useOrgStore.setState({ orgs: [trimmedOrg] });
      });
      rerender();

      await vi.waitFor(() => {
        expect(toast.warning).toHaveBeenCalledWith(
          expect.stringContaining("Removed source repo acme/web"),
        );
      });
    });

    // SW-EW-1c: the "flips a root's status from creating → ready when its
    // attachment lands" test was removed. It exercised `ensureRootWorkspaces`'
    // attach-state-change root-flip (AC-5), which is retired now that the action
    // is a no-op and roots are no longer minted. The attachment soft-refresh
    // that drives workspaceRepos.localPath is still covered by the org-store /
    // soft-refresh tests; there is no longer a root workspace whose cwd/status
    // it would flip.
  });

  // SS-4c AC-16: the first mount with a team commits it once (no Startup
  // wave since SW-XS-3); a later team switch commits the new team.
  describe("SS-4c AC-16: first-mount team activation", () => {
    it("commits the restored team once on first mount", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1);
      });

      const startupCall = mockedSetActiveTeamForWave.mock.calls[0]?.[0];
      expect(startupCall?.orgSlug).toBe("solo");
      expect(startupCall?.team).toBe("default");
      expect(startupCall?.githubUserId).toBe(1234);
      expect(startupCall?.sourceRepos).toEqual([{ owner: "solo", repo: "only" }]);
    });

    it("does not re-commit the first-mount team on a later team switch", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
          {
            name: "platform",
            source_repos: [{ owner: "acme", repo: "infra" }],
          },
        ],
      });
      // Provide attachments for both source repos so the team-switch path
      // engages.
      mockedListAttachmentsByRemote.mockResolvedValue(
        new Map([
          ["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as Attachment],
          ["github.com/acme/infra", { repo_path: "/Users/dev/acme-infra" } as Attachment],
        ]),
      );
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      // First mount commits the restored team once.
      await vi.waitFor(() => {
        expect(committedTeams()).toEqual(["default"]);
      });

      // Switch to platform team.
      act(() => {
        useOrgStore.setState({ activeTeamName: "platform" });
      });
      rerender();

      await vi.waitFor(() => {
        expect(committedTeams()).toContain("platform");
      });

      // The first-mount activation stays fire-once.
      expect(committedTeams().filter((t) => t === "default")).toHaveLength(1);
    });

    it("does not fire when no team is active (gate stays closed)", async () => {
      currentUserValue = makeUser(1234);
      // Render with no active org — Startup gate is closed.
      renderHook(() => useWorkspaceViewerSync());

      // Settle: nothing should fire because there's no org+team to converge.
      await vi.waitFor(() => {
        expect(Object.keys(useOrgStore.getState().workspaceRepos)).toHaveLength(0);
      });
      expect(mockedSetActiveTeamForWave).not.toHaveBeenCalled();
    });
  });

  // SW-TS-5 §4.4: the team-agnostic worktree launch reconcile fires once per
  // session, un-awaited, independent of the team-switch orchestration.
  describe("SW-TS-5 §4.4: worktree launch reconcile", () => {
    it("fires reconcileWorktreesOnLaunch once on first mount", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
      });
      // Passed the derived codesenseHome + the current github user id; never
      // a team (team-agnostic).
      const [home, userId] = mockedReconcileWorktreesOnLaunch.mock.calls[0] ?? [];
      expect(typeof home).toBe("string");
      expect(home).not.toBe("");
      expect(userId).toBe(1234);
    });

    it("fires even when login resolves after the workspace is established", async () => {
      // M1 regression: when githubUserId is null at first establishment, the
      // workspace is built via the hard-switch path (no reconcile yet). Login
      // then changes ONLY the user id, routing the next run through the
      // soft-refresh branch. The reconcile (fired above the branch split) must
      // still fire — a hard-switch-only call would be stranded here.
      currentUserValue = null;
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      // No authenticated user yet → reconcile must not fire.
      await vi.waitFor(() => {
        expect(useOrgStore.getState()).toBeDefined();
      });
      expect(mockedReconcileWorktreesOnLaunch).not.toHaveBeenCalled();

      // Login resolves — only the user id changes (soft-refresh path).
      act(() => {
        currentUserValue = makeUser(1234);
      });
      rerender();

      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
      });
      const [, userId] = mockedReconcileWorktreesOnLaunch.mock.calls[0] ?? [];
      expect(userId).toBe(1234);
    });

    it("re-fires after logout and re-login (clears the one-shot gate)", async () => {
      // Logout is a session boundary: the one-shot ref must reset so a
      // re-login — especially as a DIFFERENT github user (a different
      // users/{uid}/ worktree tree) — reconciles again.
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
      });

      // Drop the active profile without nulling activeOrgSlug. The gate reset
      // must hinge on the user id going null, not on the no-active-org branch.
      act(() => {
        currentUserValue = null;
      });
      rerender();

      // Select a different profile (different users/{uid}/ tree).
      act(() => {
        currentUserValue = makeUser(5678);
      });
      rerender();

      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(2);
      });
      const [, secondUserId] = mockedReconcileWorktreesOnLaunch.mock.calls[1] ?? [];
      expect(secondUserId).toBe(5678);
    });

    it("re-fires on a direct account switch with no logout hop", async () => {
      // Keying on the user id (not a bare boolean) means a direct A→B user
      // change — without an intervening null — still reconciles user B's
      // worktree tree.
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
      });

      // Switch directly to a different user — org/team unchanged, no null hop.
      act(() => {
        currentUserValue = makeUser(5678);
      });
      rerender();

      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(2);
      });
      const [, secondUserId] = mockedReconcileWorktreesOnLaunch.mock.calls[1] ?? [];
      expect(secondUserId).toBe(5678);
    });

    it("does not re-fire on subsequent team switches in the same session", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
          {
            name: "platform",
            source_repos: [{ owner: "acme", repo: "infra" }],
          },
        ],
      });
      mockedListAttachmentsByRemote.mockResolvedValue(
        new Map([
          ["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as Attachment],
          ["github.com/acme/infra", { repo_path: "/Users/dev/acme-infra" } as Attachment],
        ]),
      );
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });

      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
      });

      act(() => {
        useOrgStore.setState({ activeTeamName: "platform" });
      });
      rerender();

      await vi.waitFor(() => {
        expect(committedTeams()).toContain("platform");
      });

      // Reconcile is fire-once-per-session — a team switch must not re-fire it.
      expect(mockedReconcileWorktreesOnLaunch).toHaveBeenCalledTimes(1);
    });
  });

  // SLW-FX-3 (AC-1): the in-session stuck-only sweep re-drives a worktree
  // stranded mid-session (record written, derive wave never enqueued, no FE
  // remount) on window focus / tab-visible — the launch reconcile can't, being
  // once-per-uid on mount.
  describe("SLW-FX-3: in-session stuck-worktree sweep", () => {
    it("does not derive story context on foreground focus", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());
      // Listeners are registered synchronously on mount; the sweep itself only
      // fires on the event (never on mount — that's the launch reconcile's job).
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();

      act(() => {
        window.dispatchEvent(new Event("focus"));
      });

      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();
    });

    it("does not fire while logged out", async () => {
      currentUserValue = null;
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());
      // Dispatch focus and flush microtasks so an errant async sweep would have
      // resolved before we assert — a real barrier, not a trivially-true wait.
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
        await Promise.resolve();
      });

      // No authenticated user → nothing to reconcile.
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();
    });
  });

  // SW-FX-66 (D2.1): the post-establish verify beat closes the gap SLW-FX-3's
  // focus/visibility sweep leaves — a burst-created, unopened, never-refocused
  // worktree whose create-time derive wave was lost. A bounded timer after each
  // establish fires ONE stuck-sweep (a no-op if everything materialized).
  describe("SW-FX-66: post-establish verify beat", () => {
    beforeEach(() => {
      vi.useFakeTimers();
      useViewerStore.setState({ worktreeEstablishTick: 0 });
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it("does not schedule hydration after registration", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());
      // No establish yet (tick 0) → the beat is disarmed.
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();

      // Simulate a worktree link being established.
      act(() => {
        useViewerStore.setState((s) => ({
          worktreeEstablishTick: s.worktreeEstablishTick + 1,
        }));
      });
      // Armed but MUST NOT fire before the window elapses.
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(20_000);
      });
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();
    });

    it("does not fire when no worktree has been established (tick 0)", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());
      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();
    });

    it("keeps burst registrations free of automatic hydration", async () => {
      currentUserValue = makeUser(1234);
      const org = makeOrgSingle("solo");
      useOrgStore.setState({
        activeOrgSlug: "solo",
        activeTeamName: "default",
        orgs: [org],
      });

      renderHook(() => useWorkspaceViewerSync());

      // Three establishes in quick succession — each re-arms the timer.
      for (let i = 0; i < 3; i++) {
        act(() => {
          useViewerStore.setState((s) => ({
            worktreeEstablishTick: s.worktreeEstablishTick + 1,
          }));
        });
        await act(async () => {
          await vi.advanceTimersByTimeAsync(5_000); // < window, so no fire yet
        });
      }
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(20_000);
      });
      // One sweep total — the burst coalesced (re-arm cleared the prior timer).
      expect(mockedReconcileStuckWorktrees).not.toHaveBeenCalled();
    });
  });

  // ============================================================================
  // SF-8 — Non-blocking org/team switch
  // ============================================================================
  describe("same-team hard switch superseded by an attachments-only change", () => {
    it("keeps the active team open and mounted (seeding repos + attach bump)", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("9north:default");
      });
      await new Promise((r) => setTimeout(r, 20));

      // The repo-set change starts a hard switch that parks in loadAttachmentsMap.
      let releaseHardSwitch!: (v: Map<string, Attachment>) => void;
      mockedListAttachmentsByRemote.mockImplementationOnce(
        () =>
          new Promise<Map<string, Attachment>>((res) => {
            releaseHardSwitch = res;
          }),
      );
      act(() => {
        useOrgStore.setState({
          orgs: [
            makeOrg({
              teams: [
                {
                  name: "default",
                  source_repos: [
                    { owner: "acme", repo: "api" },
                    { owner: "acme", repo: "web" },
                  ],
                },
              ],
            }),
          ],
        });
      });
      rerender();
      await vi.waitFor(() => expect(releaseHardSwitch).toBeDefined());

      // An attachment lands while it is in flight, superseding it.
      act(() => {
        useOrgStore.getState().bumpAttachmentsVersion();
      });
      rerender();
      await new Promise((r) => setTimeout(r, 20));

      releaseHardSwitch(new Map());
      await new Promise((r) => setTimeout(r, 50));

      expect(useViewerStore.getState().openedTeams).toContain("9north:default");
      const repos = useOrgStore.getState().workspaceRepos;
      expect(repos["workspace:9north:default:acme/api"]).toBeDefined();
      expect(repos["workspace:9north:default:acme/web"]).toBeDefined();
    });

    // SW-LN-B57: the backend only resolves active-team reads (Activity) once this commit
    // lands, so the commit announces the team for those reads to retry at once.
    it("announces the backend's active team once the wave commit lands", async () => {
      useBackendActiveTeamStore.setState({ teamKey: null, epoch: 0 });
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));
      await vi.waitFor(() =>
        expect(useBackendActiveTeamStore.getState()).toEqual({
          teamKey: "9north:default",
          epoch: 1,
        }),
      );
    });

    // SW-LN-B51: the wave engine must learn the new repo set even though the run that saw
    // the repo-set change was superseded — otherwise worktrees of the added repo never
    // receive team context edits.
    it("sends the new repo set to the wave engine (setActiveTeamForWave)", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));
      await new Promise((r) => setTimeout(r, 20));

      let releaseHardSwitch!: (v: Map<string, Attachment>) => void;
      mockedListAttachmentsByRemote.mockImplementationOnce(
        () =>
          new Promise<Map<string, Attachment>>((res) => {
            releaseHardSwitch = res;
          }),
      );
      act(() => {
        useOrgStore.setState({
          orgs: [
            makeOrg({
              teams: [
                {
                  name: "default",
                  source_repos: [
                    { owner: "acme", repo: "api" },
                    { owner: "acme", repo: "web" },
                  ],
                },
              ],
            }),
          ],
        });
      });
      rerender();
      await vi.waitFor(() => expect(releaseHardSwitch).toBeDefined());
      act(() => {
        useOrgStore.getState().bumpAttachmentsVersion();
      });
      rerender();
      await new Promise((r) => setTimeout(r, 20));
      releaseHardSwitch(new Map());

      await vi.waitFor(() => {
        const last = mockedSetActiveTeamForWave.mock.calls.at(-1)?.[0];
        expect(last?.sourceRepos).toEqual([
          { owner: "acme", repo: "api" },
          { owner: "acme", repo: "web" },
        ]);
      });
    });
  });

  // SW-LN-B68: a signed-in team with no repo yet still has to be the backend's active
  // team — every active-team read (Activity) fails until it is.
  describe("team with no repos", () => {
    const EMPTY_BOARD = { youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
    beforeEach(() => {
      useBackendActiveTeamStore.setState({ teamKey: null, epoch: 0 });
    });
    afterEach(() => {
      // clearAllMocks keeps implementations: restore the defaults these tests override.
      mockedSetActiveTeamForWave.mockReset().mockResolvedValue(undefined);
      vi.mocked(listAttentionBoard).mockReset().mockResolvedValue(EMPTY_BOARD);
      mockedListAttachmentsByRemote.mockReset().mockResolvedValue(new Map());
      vi.useRealTimers();
    });

    const noRepoOrg = () =>
      makeOrg({
        teams: [
          { name: "general", source_repos: [] },
          { name: "default", source_repos: [{ owner: "acme", repo: "api" }] },
        ],
      });

    it("commits the active team to the backend without waiting for a repo", async () => {
      useBackendActiveTeamStore.setState({ teamKey: null, epoch: 0 });
      currentUserValue = makeUser();
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "general",
        orgs: [noRepoOrg()],
      });
      renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));
      expect(mockedSetActiveTeamForWave.mock.calls[0]?.[0]).toMatchObject({
        orgSlug: "9north",
        team: "general",
        sourceRepos: [],
        githubUserId: 1234,
      });
      await vi.waitFor(() =>
        expect(useBackendActiveTeamStore.getState().teamKey).toBe("9north:general"),
      );
    });

    it("recommits the team with its repos once the first repo is attached", async () => {
      currentUserValue = makeUser();
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "general",
        orgs: [noRepoOrg()],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));

      act(() => {
        useOrgStore.setState({
          orgs: [
            makeOrg({
              teams: [{ name: "general", source_repos: [{ owner: "acme", repo: "api" }] }],
            }),
          ],
        });
      });
      rerender();

      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(2));
      expect(mockedSetActiveTeamForWave.mock.calls[1]?.[0].sourceRepos).toEqual([
        { owner: "acme", repo: "api" },
      ]);
    });

    it("commits a switched-to team that has no repos", async () => {
      currentUserValue = makeUser();
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [noRepoOrg()],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));

      act(() => {
        useOrgStore.setState({ activeTeamName: "general" });
      });
      rerender();

      await vi.waitFor(() =>
        expect(mockedSetActiveTeamForWave.mock.calls.at(-1)?.[0]).toMatchObject({
          team: "general",
          sourceRepos: [],
        }),
      );
      expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(2);
    });

    it("commits a switched-to team whose repos aren't attached on this machine", async () => {
      currentUserValue = makeUser();
      mockedListAttachmentsByRemote.mockResolvedValue(new Map());
      const org = makeOrg({
        teams: [
          { name: "default", source_repos: [{ owner: "acme", repo: "api" }] },
          { name: "platform", source_repos: [{ owner: "acme", repo: "infra" }] },
        ],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));

      act(() => {
        useOrgStore.setState({ activeTeamName: "platform" });
      });
      rerender();

      await vi.waitFor(() =>
        expect(mockedSetActiveTeamForWave.mock.calls.at(-1)?.[0]).toMatchObject({
          team: "platform",
          sourceRepos: [{ owner: "acme", repo: "infra" }],
        }),
      );
    });

    it("recommits the team you go back to after a repo-less one (resume path)", async () => {
      currentUserValue = makeUser();
      mockedListAttachmentsByRemote.mockResolvedValue(
        new Map([["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as Attachment]]),
      );
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [noRepoOrg()],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));
      await new Promise((r) => setTimeout(r, 20));

      act(() => useOrgStore.setState({ activeTeamName: "general" }));
      rerender();
      await vi.waitFor(() =>
        expect(useBackendActiveTeamStore.getState().teamKey).toBe("9north:general"),
      );
      await new Promise((r) => setTimeout(r, 20));

      act(() => useOrgStore.setState({ activeTeamName: "default" }));
      rerender();
      await vi.waitFor(() =>
        expect(useBackendActiveTeamStore.getState().teamKey).toBe("9north:default"),
      );
      expect(mockedSetActiveTeamForWave.mock.calls.at(-1)?.[0]).toMatchObject({
        team: "default",
        sourceRepos: [{ owner: "acme", repo: "api" }],
      });
    });

    it("commits a repo-less team when sign-in resolves after the workspace mounted", async () => {
      currentUserValue = null;
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "general",
        orgs: [noRepoOrg()],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() =>
        expect(useViewerStore.getState().openedTeams).toContain("9north:general"),
      );
      await new Promise((r) => setTimeout(r, 20));
      expect(mockedSetActiveTeamForWave).not.toHaveBeenCalled();

      currentUserValue = makeUser();
      rerender();
      await vi.waitFor(() =>
        expect(mockedSetActiveTeamForWave.mock.calls.at(-1)?.[0]).toMatchObject({
          team: "general",
          sourceRepos: [],
        }),
      );
    });

    it("retries a failed repo-less commit on its own", async () => {
      vi.useFakeTimers({ shouldAdvanceTime: true });
      mockedSetActiveTeamForWave.mockRejectedValueOnce(new Error("ipc down"));
      currentUserValue = makeUser();
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "general",
        orgs: [noRepoOrg()],
      });
      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(1));
      await vi.advanceTimersByTimeAsync(1100);
      await vi.waitFor(() => expect(mockedSetActiveTeamForWave).toHaveBeenCalledTimes(2));
      await vi.waitFor(() =>
        expect(useBackendActiveTeamStore.getState().teamKey).toBe("9north:general"),
      );
    });

    it("does not commit a team before one is selected", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({ teams: [{ name: "default", source_repos: [] }] });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: null, orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await new Promise((r) => setTimeout(r, 50));
      expect(mockedSetActiveTeamForWave).not.toHaveBeenCalled();
    });

    // The seam: the backend refuses the board read until it holds an active team, as the
    // real `list_attention_board` does. Activity must land on its board (the empty
    // state), never on "Couldn't load Activity".
    it("Activity loads its board, not an error, before any repo is attached", async () => {
      useBackendActiveTeamStore.setState({ teamKey: null, epoch: 0 });
      let backendTeam: string | null = null;
      mockedSetActiveTeamForWave.mockImplementation(async (p) => {
        backendTeam = `${p.orgSlug}:${p.team}`;
      });
      const readBoard = vi.mocked(listAttentionBoard);
      const empty = EMPTY_BOARD;
      readBoard.mockImplementation(async () => {
        if (backendTeam === null) {
          throw new Error("no active team selected — open codesense and pick a team first");
        }
        return empty;
      });
      currentUserValue = makeUser();
      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "general",
        orgs: [noRepoOrg()],
      });

      const activity = renderHook(() => useAttentionBoard());
      renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(
        () => {
          expect(activity.result.current.board).toEqual(empty);
        },
        { timeout: 3000 },
      );
      expect(activity.result.current.error).toBeNull();
      expect(activity.result.current.degraded).toBeNull();
      activity.unmount();
    });
  });

  describe("active team dropped from openedTeams", () => {
    it("re-opens the active team whose workspace is mounted (e.g. a persisted empty list)", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("9north:default");
      });
      await new Promise((r) => setTimeout(r, 20));

      // The persisted viewer state loads over the fresh mount.
      act(() => {
        useViewerStore.setState({ stateLoaded: true, openedTeams: [] });
      });

      expect(useViewerStore.getState().openedTeams).toEqual(["9north:default"]);
    });

    it("SW-FU-B2: re-opens a repo-less active team (v0.0.11 general team) the persisted list dropped", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({ teams: [{ name: "general", source_repos: [] }] });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "general", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("9north:general");
      });
      await new Promise((r) => setTimeout(r, 20));

      // A Home-Base-era persisted list (another team) loads over the fresh mount.
      act(() => {
        useViewerStore.setState({ stateLoaded: true, openedTeams: ["9north:old"] });
      });

      expect(useViewerStore.getState().openedTeams).toContain("9north:general");
    });

    it("does not re-open a team closed by the org-removal teardown", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("9north:default");
      });
      await new Promise((r) => setTimeout(r, 20));

      act(() => {
        useViewerStore.setState({ stateLoaded: true });
        useOrgStore.setState({ orgs: [makeOrgSingle("other")] });
      });
      rerender();

      expect(useViewerStore.getState().openedTeams).not.toContain("9north:default");
      expect(useOrgStore.getState().workspaceRepos).toEqual({});
    });

    it("leaves a team closed once its workspace repos are gone", async () => {
      currentUserValue = makeUser();
      const org = makeOrg({
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "9north", activeTeamName: "default", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await vi.waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("9north:default");
      });
      await new Promise((r) => setTimeout(r, 20));

      act(() => {
        useOrgStore.setState({ workspaceRepos: {} });
        useViewerStore.setState({ stateLoaded: true, openedTeams: [] });
      });

      expect(useViewerStore.getState().openedTeams).toEqual([]);
    });
  });

  describe("SF-8: synchronous early mount + bail cleanup", () => {
    function makeOrgWithTeams(): Org {
      return makeOrg({
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
          {
            name: "beta",
            source_repos: [{ owner: "acme", repo: "api" }],
          },
        ],
      });
    }

    it("AC-1: workspaceRepos + openedTeams populated SYNCHRONOUSLY before any await resolves", () => {
      // Hang `initWatchedCache` so the IIFE blocks AFTER the synchronous
      // early-mount block but BEFORE any visible await microtask resolves.
      // This makes the assertion below specifically about the same-frame
      // chrome flip — no microtask processing has happened yet.
      mockedInitWatchedCache.mockImplementation(() => new Promise<number>(() => {}));
      const org = makeOrgSingle("solo");
      useOrgStore.setState({ activeOrgSlug: "solo", activeTeamName: null, orgs: [org] });

      // renderHook fires the effect SYNCHRONOUSLY. Inside the IIFE,
      // Phase 1 + Phase 2a + openTeam all run synchronously before any
      // await is reached. Assert state immediately after render — no
      // `await`, no `waitFor`.
      renderHook(() => useWorkspaceViewerSync());

      const wsRepos = useOrgStore.getState().workspaceRepos;
      expect(wsRepos["workspace:solo:default:solo/only"]).toBeDefined();
      expect(useViewerStore.getState().openedTeams).toContain("solo:_none");
    });

    it("a connected context repo outside users/{id}/orgs still switches in", async () => {
      mockedInitWatchedCache.mockReset();
      mockedInitWatchedCache.mockResolvedValue(0);
      const org = makeOrg({
        slug: "acme/ctx",
        org_root: "/home/user/work/acme-ctx",
        teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
      });
      useOrgStore.setState({ activeOrgSlug: "acme/ctx", activeTeamName: "default", orgs: [org] });
      renderHook(() => useWorkspaceViewerSync());
      await waitFor(() => {
        expect(useViewerStore.getState().openedTeams).toContain("acme/ctx:default");
        expect(mockedInitWatchedCache).toHaveBeenCalled();
      });
      expect(mockedInitWatchedCache.mock.calls[0]).toContain(
        "/home/user/.codesense/cache/frontmatter-cache.json",
      );
    });

    it("AC-5: hard-switch fires bumpFileRefresh per incoming repo after the awaits resolve", async () => {
      mockedInitWatchedCache.mockReset();
      mockedInitWatchedCache.mockResolvedValue(0);
      const org = makeOrgWithTeams();
      currentUserValue = makeUser();
      const attachments = new Map([
        ["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as any],
      ]);
      mockedListAttachmentsByRemote.mockResolvedValue(attachments);

      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(Object.keys(useOrgStore.getState().workspaceRepos).length).toBeGreaterThan(0);
      });

      // Snapshot the incoming repo's fileRefreshKey before the team-switch
      // catch-up fires.
      const repoKey = "workspace:9north:beta:acme/api";
      const keyBefore = useFileRefreshStore.getState().fileRefreshKeys[repoKey] ?? 0;

      // Trigger team change: default → beta. The IIFE awaits initWatchedCache
      // + the active-team commit, then fires `bumpFileRefresh` per incoming repo
      // for the SF-8 catch-up.
      act(() => {
        useOrgStore.setState({ activeTeamName: "beta" });
      });
      rerender();

      await vi.waitFor(() => {
        // Catch-up bump landed: the repo's fileRefreshKey incremented.
        const keyAfter = useFileRefreshStore.getState().fileRefreshKeys[repoKey] ?? 0;
        expect(keyAfter).toBeGreaterThan(keyBefore);
      });
    });

    it("AC-6: A→B→A rapid switch — bailed Run 1 (A) suspends without orphaning, Run 3 resumes A", async () => {
      // Hang beta's active-team commit so Run 1 (default→beta) gets superseded
      // by Run 2 (beta→default) BEFORE Run 1's switch resolves. Run 1's
      // post-await bail must SUSPEND beta (not close) so that on Run 2's
      // resume path, beta stays in suspendedWorkspaceRepos until needed.
      mockedInitWatchedCache.mockReset();
      mockedInitWatchedCache.mockResolvedValue(0);
      const org = makeOrgWithTeams();
      currentUserValue = makeUser();
      const attachments = new Map([
        ["github.com/acme/api", { repo_path: "/Users/dev/acme-api" } as any],
      ]);
      mockedListAttachmentsByRemote.mockResolvedValue(attachments);

      let resolveBetaSwitch!: () => void;
      const betaSwitch = new Promise<void>((res) => {
        resolveBetaSwitch = res;
      });
      mockedSetActiveTeamForWave.mockImplementation((params) =>
        params.team === "beta" ? betaSwitch : Promise.resolve(),
      );

      useOrgStore.setState({
        activeOrgSlug: "9north",
        activeTeamName: "default",
        orgs: [org],
      });
      const { rerender } = renderHook(() => useWorkspaceViewerSync());

      await vi.waitFor(() => {
        expect(Object.keys(useOrgStore.getState().workspaceRepos).length).toBeGreaterThan(0);
      });

      // Trigger Run 2: default → beta (hard-switch path).
      act(() => {
        useOrgStore.setState({ activeTeamName: "beta" });
      });
      rerender();
      await vi.waitFor(() => {
        expect(committedTeams()).toContain("beta");
      });

      // Trigger Run 3: beta → default (rapid switch; supersedes Run 2).
      act(() => {
        useOrgStore.setState({ activeTeamName: "default" });
      });
      rerender();
      await new Promise((r) => setTimeout(r, 20));

      // Resolve the pending beta switch → Run 2's IIFE wakes up, runId is
      // stale → bailHardSwitchEarlyMount fires for beta.
      resolveBetaSwitch();
      await new Promise((r) => setTimeout(r, 50));

      // Invariant: openedTeams is a stable superset; no team gets orphaned
      // out of the per-team stack by a mid-flight bail. Specifically, the
      // currently-active team ("9north:default") must be present.
      expect(useViewerStore.getState().openedTeams).toContain("9north:default");

      // SF-8 review iter-1 (BH#11): tighten the bail-suspend invariant.
      // Beta's synthetics must be in suspendedWorkspaceRepos (so a
      // subsequent beta re-click resumes them), and live workspaceRepos
      // must NOT carry the beta-keyed synthetic anymore.
      const teamBKey = "9north:beta";
      const teamBSyntheticKey = "workspace:9north:beta:acme/api";
      expect(useOrgStore.getState().suspendedWorkspaceRepos[teamBKey]).toBeDefined();
      expect(useOrgStore.getState().workspaceRepos[teamBSyntheticKey]).toBeUndefined();
    });
  });
});
