// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import { selectActiveTerminalTab, selectWorkspaceTabs, useViewerStore } from "./viewer-store";
import { useConfigStore, type RepoState } from "./config-store";
import { useOrgStore } from "./org-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

// T.2: tests treat REPO as a teamKey; seed a single source-repo so
// `ensureRootWorkspaces(REPO)` mints a root with this stable id.
const TEST_SLUG = "test-org/test-repo";
const ROOT_ID = buildRootWorkspaceId(TEST_SLUG);
const SYNTHETIC_KEY = `workspace:test-org:default:${TEST_SLUG}`;
const SYNTHETIC_KEY_2 = `workspace:test-org:default2:${TEST_SLUG}`;

vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  removeWorktree: vi.fn(() => Promise.resolve()),
  pushViewerState: vi.fn(() => Promise.resolve()),
  checkPathExists: vi.fn(() => Promise.resolve(true)),
}));

// T.1: REPO/REPO2 double as teamKey values so the team-keyed workspace
// slices' bucket key matches what `selectRepo` derives internally from
// active org/team. The beforeEach below sets `useOrgStore` to make the
// derivation produce these exact values.
const REPO = "test-org:default";
const REPO2 = "test-org:default2";

const DEFAULT_STATE = {
  openTabs: {},
  activeTab: {},
  terminalVisible: false,
  settings: {
    contentWidth: "normal" as const,
    sidebarWidth: 240,
    appTheme: "codesense" as const,
    viewerFontSize: "xl" as const,
    treeFontSize: "large" as const,
    terminal: "",
    terminalCustomCmd: "",
    editor: "",
    editorCustomCmd: "",
    terminalFontSize: "large" as const,
    codeEditorFontSize: "normal" as const,
    editorTheme: "default" as const,
    editorLineWrap: "auto" as const,
    editorFolding: "auto" as const,
    editorShowWhitespace: false,
    editorTabSize: 2 as const,
    editorProseFont: false,
    terminalTheme: "github-dark" as const,
    starshipPreset: "compact" as const,
  },
  workspaces: {},
  activeWorkspace: {},
  stateLoaded: false,
  openedTeams: [] as string[],
  navHistory: {},
  navIndex: {},
  navLens: {},
  openedViewerPaths: {},
};

installTerminalLayoutHarness();

describe("viewer-store coverage", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // T.1: viewer-store actions derive teamKey from active org/team.
    // T.2: seed workspaceRepos so ensureRootWorkspaces has a target slug.
    // T.2: also seed a non-empty `orgs` so `waitForOrgStoreReady` short-
    // circuits — vi.useFakeTimers freezes the 5s timeout fallback.
    useOrgStore.setState({
      orgs: [
        {
          slug: "test-org",
          display_name: "Test Org",
          created_at: "2026-01-01T00:00:00Z",
          remote_url: null,
          config_flags: {},
          people: [],
          // Review iter-1, finding #1: migrateV1toV2 reads slugs from here.
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "test-org", repo: "test-repo" }],
            },
            {
              name: "default2",
              source_repos: [{ owner: "test-org", repo: "test-repo" }],
            },
          ],
          org_root: "/orgs/test-org",
        },
      ],
      activeOrgSlug: "test-org",
      activeTeamName: "default",
      workspaceRepos: {
        [SYNTHETIC_KEY]: {
          repoPath: SYNTHETIC_KEY,
          localPath: "/test/path",
        } as unknown as RepoState,
        [SYNTHETIC_KEY_2]: {
          repoPath: SYNTHETIC_KEY_2,
          localPath: "/test/path2",
        } as unknown as RepoState,
      },
    });
    useViewerStore.setState(DEFAULT_STATE);
    // T.2: seed a root workspace per teamKey so terminal-tab tests' default
    // active workspace resolves to ROOT_ID (matches pre-T.2 behavior where
    // ROOT_WORKSPACE_ID = "root" was the implicit default).
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [REPO]: [{ id: ROOT_ID, kind: "root", cwd: REPO, status: "ready" }],
        [REPO2]: [{ id: ROOT_ID, kind: "root", cwd: REPO2, status: "ready" }],
      },
      activeWorkspace: { ...s.activeWorkspace, [REPO]: ROOT_ID, [REPO2]: ROOT_ID },
    }));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  // T.6: `selectRepo` / `goToPicker` / `closeRepo` retired. The repo-
  // selection surface is gone — workspace activation now flows through
  // `openTeam(teamKey)` + per-team `switchWorkspace`. See the T.6 sub-
  // epic README for the new contract.

  // ---------------------------------------------------------------------------
  // setTerminalVisible
  // ---------------------------------------------------------------------------
  describe("setTerminalVisible", () => {
    it("setTerminalVisible updates the global terminal visibility", () => {
      // When
      useViewerStore.getState().setTerminalVisible(true);

      // Then
      expect(useViewerStore.getState().terminalVisible).toBe(true);
    });
  });

  // ---------------------------------------------------------------------------
  // Terminal tab management
  // ---------------------------------------------------------------------------
  describe("terminal tab management", () => {
    const wsTabs = () => selectWorkspaceTabs(useViewerStore.getState(), REPO, ROOT_ID);
    const wsActiveTab = () => selectActiveTerminalTab(useViewerStore.getState(), REPO, ROOT_ID);
    const settle = () => vi.advanceTimersByTimeAsync(0);

    it("addTerminalTab opens a tab in the host and sets it active", async () => {
      await useViewerStore.getState().addTerminalTab(REPO, { sessionId: "t1" });

      expect(wsTabs().map((t) => t.id)).toEqual(["t1"]);
      expect(wsTabs()[0].name).toBe("Terminal 1");
      expect(wsActiveTab()).toBe("t1");
    });

    it("removeTerminalTab removes a tab and selects neighbor when active is removed", async () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1", "t2"], { active: "t2" });

      useViewerStore.getState().removeTerminalTab(REPO, "t2");
      await settle();

      expect(wsTabs().map((t) => t.id)).toEqual(["t1"]);
      expect(wsActiveTab()).toBe("t1");
    });

    it("removeTerminalTab opens a replacement when the last tab is removed", async () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1"]);

      useViewerStore.getState().removeTerminalTab(REPO, "t1");
      await settle();

      const tabs = wsTabs();
      expect(tabs).toHaveLength(1);
      expect(tabs[0].id).not.toBe("t1");
      expect(wsActiveTab()).toBe(tabs[0].id);
    });

    it("removeTerminalTab is a no-op for unknown tabId", async () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1"]);

      useViewerStore.getState().removeTerminalTab(REPO, "unknown");
      await settle();

      expect(wsTabs().map((t) => t.id)).toEqual(["t1"]);
    });

    it("removeTerminalTab preserves active when non-active tab is removed", async () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1", "t2"], { active: "t2" });

      useViewerStore.getState().removeTerminalTab(REPO, "t1");
      await settle();

      expect(wsActiveTab()).toBe("t2");
    });

    it("setActiveTerminalTab changes the active terminal tab", () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1", "t2"], { active: "t2" });

      useViewerStore.getState().setActiveTerminalTab(REPO, "t1");

      expect(wsActiveTab()).toBe("t1");
    });

    it("reorderTerminalTab moves a terminal tab", () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1", "t2", "t3"]);

      useViewerStore.getState().reorderTerminalTab(REPO, 0, 2);

      expect(wsTabs().map((t) => t.id)).toEqual(["t2", "t3", "t1"]);
    });

    // SLW-FX-20 AC-1: reorder moves only the tab, never the CLI behind it —
    // every session keeps running and keeps its unchanged tab entry.
    it("reorderTerminalTab keeps every session running and unchanged tabs identical (AC-1)", () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1", "t2", "t3"]);
      const t1Ref = wsTabs().find((t) => t.id === "t1")!;

      useViewerStore.getState().reorderTerminalTab(REPO, 0, 2);

      const after = wsTabs();
      expect(after.map((t) => t.sessionId)).toEqual(["t2", "t3", "t1"]);
      expect(after.find((t) => t.id === "t1")).toBe(t1Ref);
      expect(["t1", "t2", "t3"].every((id) => terminalHost().contains(id))).toBe(true);
    });

    it("renameTerminalTab updates the name of a specific tab", () => {
      seedTerminalTabs(REPO, ROOT_ID, ["t1"]);

      useViewerStore.getState().renameTerminalTab(REPO, "t1", "Renamed");

      expect(wsTabs()[0].name).toBe("Renamed");
    });
  });

  // ---------------------------------------------------------------------------
  // loadPersistedState
  // ---------------------------------------------------------------------------
  describe("loadPersistedState", () => {
    it("restores persisted team-keyed slices and prunes unknown teams", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      // Given: persisted state with a valid and invalid team
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO,
            repoName: "repo",
            contextRepoPath: "/ctx",
            // PTS-2: revealOpenedFile early-returns on empty teamContentPath;
            // mirror the pre-PTS-2 `${contextRepoPath}/${viewerRoot}` so file
            // fixtures under `/ctx/_bmad-output/...` are reachable.
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
      // T.4 AC-11: pruneTeamKeyed accepts outer keys present in
      // workspaceRepos (parsed via parseSyntheticRepoKey to teamKey) +
      // NO_TEAM_SENTINEL. Seed a synthetic for REPO's teamKey so its data
      // survives; omit REPO2 so its data is pruned.
      const { useOrgStore } = await import("./org-store");
      const SYNTHETIC = "workspace:test-org:default:test-org/repo";
      useOrgStore.setState({
        workspaceRepos: {
          [SYNTHETIC]: {
            repoPath: SYNTHETIC,
            repoName: "repo",
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
          // T.6 AC-7.4: schemaVersion 4 is the current shape — retired
          // fields (selectedRepoPath / viewMode / openedRepos /
          // workspaceViewerStates) are gone; `openedTeams` replaces them.
          schemaVersion: 4,
          openedTeams: [REPO],
          openTabs: {
            [REPO]: [{ path: "a.md", name: "a" }],
            [REPO2]: [{ path: "b.md", name: "b" }],
          },
          activeTab: { [REPO]: "a.md", [REPO2]: "b.md" },
          terminalVisible: { [REPO]: true, [REPO2]: false },
          settings: { contentWidth: "wide" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      // When
      await useViewerStore.getState().loadPersistedState();

      // Then: valid team's state is restored, unknown team is pruned.
      const s = useViewerStore.getState();
      expect(s.stateLoaded).toBe(true);
      expect(s.openedTeams).toEqual([REPO]);
      expect(s.openTabs[REPO]).toHaveLength(1);
      expect(s.openTabs[REPO2]).toBeUndefined();
      expect(s.settings.contentWidth).toBe("wide");
      // Verify DEFAULT_SETTINGS are merged in for missing keys
      expect(s.settings.appTheme).toBe("codesense");
      // SLW-WD-7 AC-3: an existing user's persisted settings lack
      // `defaultSourceDiffLayout` → the DEFAULT_SETTINGS spread defaults them to
      // `inline` on rehydrate (no bespoke migration).
      expect(s.settings.defaultSourceDiffLayout).toBe("inline");
    });

    it("sets stateLoaded when no viewer data exists", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      vi.mocked(loadAppState).mockResolvedValue("{}");

      // When
      await useViewerStore.getState().loadPersistedState();

      // Then
      expect(useViewerStore.getState().stateLoaded).toBe(true);
    });

    it("sets stateLoaded on parse error", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      vi.mocked(loadAppState).mockResolvedValue("NOT_JSON!!!");

      // When
      await useViewerStore.getState().loadPersistedState();

      // Then: gracefully recovers
      expect(useViewerStore.getState().stateLoaded).toBe(true);
    });

    it("clears unsupported editor values during restore", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO,
            repoName: "repo",
            contextRepoPath: "/ctx",
            // PTS-2: revealOpenedFile early-returns on empty teamContentPath;
            // mirror the pre-PTS-2 `${contextRepoPath}/${viewerRoot}` so file
            // fixtures under `/ctx/_bmad-output/...` are reachable.
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
      const persisted = {
        viewer: {
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone.
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: { editor: "vscode" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      // When
      await useViewerStore.getState().loadPersistedState();

      // Then: unsupported "vscode" is cleared to ""
      expect(useViewerStore.getState().settings.editor).toBe("");
    });

    it("coerces an unknown editorTheme to 'default' during restore (SLW-WDE-7)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          // A theme from a hypothetical newer build / corrupted state.
          settings: { editorTheme: "solarized-light" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      expect(useViewerStore.getState().settings.appTheme).toBe("codesense");
    });

    it("coerces corrupted editor power-config settings on restore (SW-VE-11)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: {
            editorFolding: "garbage",
            editorShowWhitespace: "yes", // non-boolean
            editorTabSize: 3, // not a valid size
            editorProseFont: 1, // non-boolean
          },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      const s = useViewerStore.getState().settings;
      expect(s.editorFolding).toBe("auto");
      expect(s.editorShowWhitespace).toBe(false);
      expect(s.editorTabSize).toBe(2);
      expect(s.editorProseFont).toBe(false);
    });

    it("preserves valid persisted editor power-config settings on restore (SW-VE-11)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: {
            editorFolding: "on",
            editorShowWhitespace: true,
            editorTabSize: 8,
            editorProseFont: true,
          },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      const s = useViewerStore.getState().settings;
      expect(s.editorFolding).toBe("on");
      expect(s.editorShowWhitespace).toBe(true);
      expect(s.editorTabSize).toBe(8);
      expect(s.editorProseFont).toBe(true);
    });

    it.each([
      ["large", "large"],
      ["small", "small"],
      [undefined, "normal"], // saved before the setting existed: the original, larger bar
      ["gigantic", "normal"], // corrupted
    ])("restores workspaceBarSize %s as %s", async (saved, expected) => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: saved === undefined ? {} : { workspaceBarSize: saved },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      expect(useViewerStore.getState().settings.workspaceBarSize).toBe(expected);
    });

    it("preserves a valid persisted editorTheme during restore (SLW-WDE-7)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: { editorTheme: "dracula" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      expect(useViewerStore.getState().settings.appTheme).toBe("dracula");
    });

    // SW-LN-35 (AC-3): the Starship prompt is opt-in for new installs, while an
    // existing install keeps the preset it saved.
    it.each([
      [{ editorTheme: "dracula" }, "plain"],
      [{ starshipPreset: "compact" }, "compact"],
      [{ starshipPreset: "plain" }, "plain"],
    ])("restores starshipPreset from %j as %s", async (settings, expected) => {
      const { loadAppState } = await import("@/tauri/commands");
      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings,
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      expect(useViewerStore.getState().settings.starshipPreset).toBe(expected);
    });

    it("expands ancestor folders for each restored active tab", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      // T.4 Phase 5: the startup reveal loop iterates teamKey-keyed
      // activeTab entries and looks up a representative repo via
      // `workspaceRepos` (synthetic-parseable). Seed both stores so the
      // legacy test path stays compatible — config-store carries the
      // teamContentPath, workspaceRepos provides a synthetic that parses
      // to teamKey "test-org:default" matching REPO.
      const SYNTHETIC = "workspace:test-org:default:test-org/repo";
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO,
            repoName: "repo",
            contextRepoPath: "/ctx",
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
      // Seed workspaceRepos for the startup reveal lookup. resolveRepo
      // (T.1 helper) checks both stores; the synthetic's resolution
      // returns the config-store data via the contextRepoPath chain.
      const { useOrgStore } = await import("./org-store");
      useOrgStore.setState({
        workspaceRepos: {
          [SYNTHETIC]: {
            repoPath: SYNTHETIC,
            repoName: "repo",
            contextRepoPath: "/ctx",
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        },
      });
      const activePath = "/ctx/_bmad-output/implementation-artifacts/epic-a/sub/story.md";
      const persisted = {
        viewer: {
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone.
          schemaVersion: 4,
          openTabs: { [REPO]: [{ path: activePath, name: "story.md" }] },
          activeTab: { [REPO]: activePath },
          terminalVisible: false,
          settings: {},
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      // Then: every ancestor between the file and the pipeline root is expanded
      // in statusNavExpandedPaths (not the deleted expandedFolders).
      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain("/ctx/_bmad-output/implementation-artifacts/epic-a");
      expect(expanded).toContain("/ctx/_bmad-output/implementation-artifacts/epic-a/sub");
      // R-14: viewer base is now `_bmad-output` (not `_bmad-output/implementation-artifacts`),
      // so the legacy pipeline-root dir is a regular ancestor that DOES get expanded.
      expect(expanded).toContain("/ctx/_bmad-output/implementation-artifacts");
    });

    it("subscribe-and-retry fallback expands ancestors when workspaceRepos arrives AFTER loadPersistedState (T.4 Round 5 TP3 P1 codepath)", async () => {
      // T.4 Round 5 TP3: the "expands ancestor folders" test above seeds
      // workspaceRepos BEFORE loadPersistedState, so the synchronous
      // tryReveal() succeeds and the subscribe + setTimeout fallback
      // (where Round 5 P1's wrapper-object TDZ fix lives) is never
      // exercised. This test seeds workspaceRepos = {} initially, lets
      // loadPersistedState run through to the reveal block (advancing
      // fake timers past waitForWorkspaceReposReady's 1000ms timeout),
      // then populates workspaceRepos to trigger the post-restore reveal
      // subscribe callback. Proves: (a) the subscribe-fallback codepath
      // actually executes, (b) the wrapper-object timer reference
      // doesn't throw, (c) reveal lands once workspaceRepos populates.
      const { loadAppState } = await import("@/tauri/commands");
      const { useOrgStore } = await import("./org-store");

      const SYNTHETIC = "workspace:test-org:default:test-org/repo";
      const REPO_TEAMKEY = "test-org:default";
      const activePath = "/ctx/_bmad-output/implementation-artifacts/epic-a/sub/story.md";

      // Config-store carries the repo data resolveRepo needs; team-keyed
      // synthetic in workspaceRepos starts EMPTY so first tryReveal misses.
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO_TEAMKEY,
            repoName: "repo",
            contextRepoPath: "/ctx",
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
      useOrgStore.setState({ workspaceRepos: {} });

      const persisted = {
        viewer: {
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone. Test
          // exercises the post-restore subscribe-and-retry reveal path,
          // not the migration step.
          schemaVersion: 4,
          openTabs: { [REPO_TEAMKEY]: [{ path: activePath, name: "story.md" }] },
          activeTab: { [REPO_TEAMKEY]: activePath },
          terminalVisible: false,
          settings: {},
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      // Start loadPersistedState. It will await
      // waitForWorkspaceReposReady's 1000ms timeout (workspaceRepos is
      // empty and activeOrgSlug is "test-org" so the no-active-org
      // short-circuit doesn't apply). Advance fake timers past the
      // timeout, then await the promise to completion.
      const loadPromise = useViewerStore.getState().loadPersistedState();
      await vi.advanceTimersByTimeAsync(1100);
      await loadPromise;

      // loadPersistedState has now run through prune (identity-prune,
      // workspaceRepos still empty) + restore + reveal block. The
      // synchronous tryReveal missed (no rep for REPO_TEAMKEY in empty
      // workspaceRepos), so the subscribe + setTimeout fallback was
      // registered. statusNavExpandedPaths[REPO_TEAMKEY] should not yet
      // contain ancestor paths.
      const beforeExpanded = useViewerStore.getState().statusNavExpandedPaths[REPO_TEAMKEY] ?? [];
      expect(beforeExpanded).not.toContain("/ctx/_bmad-output/implementation-artifacts/epic-a");
      expect(beforeExpanded).not.toContain("/ctx/_bmad-output/implementation-artifacts/epic-a/sub");

      // Populate workspaceRepos — the zustand subscriber fires
      // synchronously, the wrapper-bound timerHandle.current is
      // legally cleared (this is the codepath P1 protects), tryReveal
      // re-runs and finds the rep, revealOpenedFile lands.
      useOrgStore.setState({
        workspaceRepos: {
          [SYNTHETIC]: {
            repoPath: SYNTHETIC,
            repoName: "repo",
            contextRepoPath: "/ctx",
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        },
      });

      const afterExpanded = useViewerStore.getState().statusNavExpandedPaths[REPO_TEAMKEY] ?? [];
      expect(afterExpanded).toContain("/ctx/_bmad-output/implementation-artifacts/epic-a");
      expect(afterExpanded).toContain("/ctx/_bmad-output/implementation-artifacts/epic-a/sub");
    });

    it("restores terminalSplitRatio from persisted state", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO,
            repoName: "repo",
            contextRepoPath: "/ctx",
            // PTS-2: revealOpenedFile early-returns on empty teamContentPath;
            // mirror the pre-PTS-2 `${contextRepoPath}/${viewerRoot}` so file
            // fixtures under `/ctx/_bmad-output/...` are reachable.
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
      // T.4 AC-11: seed workspaceRepos with a synthetic matching REPO's
      // teamKey shape so `pruneTeamKeyed` accepts it.
      const { useOrgStore } = await import("./org-store");
      const SYNTHETIC = "workspace:test-org:default:test-org/repo";
      useOrgStore.setState({
        workspaceRepos: {
          [SYNTHETIC]: {
            repoPath: SYNTHETIC,
            repoName: "repo",
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
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone.
          schemaVersion: 4,
          openTabs: { [REPO]: [{ path: "a.md", name: "a" }] },
          activeTab: { [REPO]: "a.md" },
          terminalSplitRatio: { [REPO]: 0.65 },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      // A v4 blob still holds the old per-team map; the v15 step seeds the one
      // global ratio from it.
      expect(useViewerStore.getState().terminalSplitRatio).toBe(0.65);
    });
  });

  // ---------------------------------------------------------------------------
  // F-18: Persist and restore open workspaces
  // ---------------------------------------------------------------------------
  describe("F-18: workspace persist/restore", () => {
    // Persisting is a no-op until the saved state has loaded (SW-FU-B2).
    beforeEach(() => {
      useViewerStore.setState({ stateLoaded: true });
    });

    const REPO_CONFIG = {
      repoPath: REPO,
      repoName: "repo",
      contextRepoPath: "/ctx",
      teamContentPath: "/ctx/_bmad-output",
      hasRemote: false,
      remoteUrl: null,
      mainDoc: "",
    };

    function seedConfig() {
      useConfigStore.setState({ repos: [REPO_CONFIG] });
    }

    it("debouncedPersist serializes open workspaces and activeWorkspace (8.1)", async () => {
      const { saveAppState } = await import("@/tauri/commands");
      vi.mocked(saveAppState).mockClear();
      seedConfig();

      useViewerStore.setState({
        ...DEFAULT_STATE,
        stateLoaded: true,
        openedTeams: [REPO],
        workspaces: {
          [REPO]: [
            { id: ROOT_ID, kind: "root" as const, cwd: REPO, status: "ready" as const },
            {
              id: "F-18-test",
              kind: "story" as const,
              cwd: "/wt/F-18-test",
              storyFilePath: "/stories/F-18-test.md",
              storyId: "F-18-test",
              storyTitle: "Test Story",
              status: "ready" as const,
              statusError: "should-not-persist",
            },
          ],
        },
        activeWorkspace: { [REPO]: "F-18-test" },
      });

      await useViewerStore.getState().flushPersist();
      expect(saveAppState).toHaveBeenCalled();

      const saved = JSON.parse(vi.mocked(saveAppState).mock.calls.at(-1)![0]);
      // Workspaces and activeWorkspace are persisted
      expect(saved.viewer.workspaces[REPO]).toHaveLength(2);
      expect(saved.viewer.activeWorkspace[REPO]).toBe("F-18-test");
      // statusError is NOT persisted
      const storyWs = saved.viewer.workspaces[REPO].find(
        (w: { id: string }) => w.id === "F-18-test",
      );
      expect(storyWs).toBeDefined();
      expect(storyWs.statusError).toBeUndefined();
      expect(storyWs.storyTitle).toBe("Test Story");
    });

    it("loadPersistedState restores exactly the persisted workspace tabs (8.2)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      seedConfig();

      const persisted = {
        viewer: {
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone.
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          settings: {},
          workspaces: {
            [REPO]: [
              { id: ROOT_ID, kind: "root", cwd: REPO, status: "ready" },
              {
                id: "F-18-a",
                kind: "story",
                cwd: "/wt/F-18-a",
                storyFilePath: "/stories/F-18-a.md",
                storyId: "F-18-a",
                status: "ready",
              },
              {
                id: "F-18-b",
                kind: "story",
                cwd: "/wt/F-18-b",
                storyFilePath: "/stories/F-18-b.md",
                storyId: "F-18-b",
                status: "ready",
              },
            ],
          },
          activeWorkspace: { [REPO]: "F-18-b" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      // SW-EW-2: the legacy `kind:"root"` workspace is swept on load; the story
      // tabs restore unchanged and the active story is preserved.
      const ws = useViewerStore.getState().workspaces[REPO];
      expect(ws.map((w) => w.id)).toEqual(["F-18-a", "F-18-b"]);
      expect(useViewerStore.getState().activeWorkspace[REPO]).toBe("F-18-b");
    });

    it("post-load validation marks stale worktrees as error (8.3)", async () => {
      const { loadAppState, checkPathExists } = await import("@/tauri/commands");
      seedConfig();

      const persisted = {
        viewer: {
          // T.6 AC-7.4: schemaVersion 4 — retired fields gone.
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          settings: {},
          workspaces: {
            [REPO]: [
              { id: ROOT_ID, kind: "root", cwd: REPO, status: "ready" },
              {
                id: "stale-ws",
                kind: "story",
                cwd: "/wt/gone",
                storyFilePath: "/stories/stale.md",
                storyId: "stale-ws",
                status: "ready",
              },
            ],
          },
          activeWorkspace: { [REPO]: ROOT_ID },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));
      // The worktree path does not exist on disk
      vi.mocked(checkPathExists).mockResolvedValue(false);

      await useViewerStore.getState().loadPersistedState();
      // Wait for the async post-load validation pass
      await vi.advanceTimersByTimeAsync(0);

      const ws = useViewerStore.getState().workspaces[REPO];
      const stale = ws.find((w) => w.id === "stale-ws");
      expect(stale).toBeDefined();
      expect(stale!.status).toBe("error");
      expect(stale!.statusError).toBe("Worktree no longer exists on disk");
    });

    it("loadPersistedState restores persisted workspaces verbatim (8.4)", async () => {
      // T.1 / Task 5.5.b: the legacy auto-root-prepend in the restore loop
      // was removed. Restore leaves the persisted workspace list intact.
      // SW-EW-1c: `ensureRootWorkspaces` no longer seeds a root afterward —
      // root workspaces are retired and it is now a no-op — so the tail
      // assertion that it minted a root has been removed.
      const { loadAppState } = await import("@/tauri/commands");
      seedConfig();

      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          settings: {},
          workspaces: {
            [REPO]: [
              {
                id: "F-18-only",
                kind: "story",
                cwd: "/wt/F-18-only",
                storyFilePath: "/stories/F-18-only.md",
                storyId: "F-18-only",
                status: "ready",
              },
            ],
          },
          activeWorkspace: { [REPO]: "F-18-only" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      const ws = useViewerStore.getState().workspaces[REPO];
      expect(ws).toHaveLength(1);
      expect(ws[0].id).toBe("F-18-only");

      // SW-WV-A6: root minting is retired (`ensureRootWorkspaces` deleted). The
      // restored list is left exactly as-is — no root workspace ever appears.
      const wsAfter = useViewerStore.getState().workspaces[REPO];
      expect(wsAfter).toHaveLength(1);
      expect(wsAfter.find((w) => w.id === ROOT_ID)).toBeUndefined();
    });

    it("loadPersistedState falls back off a persisted activeWorkspace that is gone (8.5)", async () => {
      const { loadAppState } = await import("@/tauri/commands");
      seedConfig();

      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          settings: {},
          workspaces: {
            [REPO]: [
              {
                id: "kept-story",
                kind: "story",
                cwd: "/wt/kept",
                storyFilePath: "/stories/kept.md",
                storyId: "kept",
                status: "ready",
              },
            ],
          },
          activeWorkspace: { [REPO]: "deleted-workspace-id" },
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      // SW-EW-2: roots are swept, so the fallback resolves via firstRootIdForTeam
      // rather than the now-deleted active id.
      expect(useViewerStore.getState().activeWorkspace[REPO]).not.toBe("deleted-workspace-id");
    });

    it("upgrade path: first launch with no persisted workspaces leaves the team empty (8.2 edge)", async () => {
      // T.1 / Task 5.5.c: the legacy `restoredRepo`-based root seed was removed.
      // SW-EW-1c retired root workspaces and SW-WV-A6 deleted the no-op
      // `ensureRootWorkspaces`, so first launch leaves the team with no
      // workspaces at all — the controller CLI is ensured on demand (last-close
      // / dispatch), not seeded on load.
      const { loadAppState } = await import("@/tauri/commands");
      seedConfig();

      const persisted = {
        viewer: {
          schemaVersion: 4,
          openTabs: {},
          activeTab: {},
          settings: {},
          // No workspaces or activeWorkspace keys
        },
      };
      vi.mocked(loadAppState).mockResolvedValue(JSON.stringify(persisted));

      await useViewerStore.getState().loadPersistedState();

      // Restore leaves the gap; no workspaces bucket exists yet for REPO and
      // nothing seeds one on load (root minting is retired).
      expect(useViewerStore.getState().workspaces[REPO]).toBeUndefined();
      expect(useViewerStore.getState().activeWorkspace[REPO]).toBeUndefined();
    });
  });

  // ---------------------------------------------------------------------------
  // Reveal-on-activate — every action that moves activeTab re-expands the
  // file's ancestor folders, so a tab whose folder was collapsed comes back
  // into view. Mirrors what loadPersistedState does on app start.
  // ---------------------------------------------------------------------------
  describe("reveal-on-activate", () => {
    const PIPELINE_ROOT_ABS = "/ctx/_bmad-output/implementation-artifacts";
    const FILE_A = "/ctx/_bmad-output/implementation-artifacts/epic-a/sub/story.md";
    const FILE_A_ANCESTOR_DEEP = "/ctx/_bmad-output/implementation-artifacts/epic-a/sub";
    const FILE_A_ANCESTOR_EPIC = "/ctx/_bmad-output/implementation-artifacts/epic-a";
    const FILE_B = "/ctx/_bmad-output/docs/readme.md";
    const FILE_C = "/ctx/_bmad-output/implementation-artifacts/epic-b/story.md";

    function seedRepo() {
      useConfigStore.setState({
        repos: [
          {
            repoPath: REPO,
            repoName: "repo",
            contextRepoPath: "/ctx",
            // PTS-2: revealOpenedFile early-returns on empty teamContentPath;
            // mirror the pre-PTS-2 `${contextRepoPath}/${viewerRoot}` so file
            // fixtures under `/ctx/_bmad-output/...` are reachable.
            teamContentPath: "/ctx/_bmad-output",
            hasRemote: false,
            remoteUrl: null,
            mainDoc: "",
          },
        ],
      });
    }

    it("setActiveTab expands ancestor sub-epics in statusNavExpandedPaths", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: { [REPO]: [{ path: FILE_A, name: "story.md" }] },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().setActiveTab(REPO, REPO, FILE_A);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
      expect(expanded).toContain(FILE_A_ANCESTOR_EPIC);
      // R-14: legacy pipeline root is now a regular ancestor under viewerRoot
      // and gets expanded like any other dir between the file and viewerRoot.
      expect(expanded).toContain(PIPELINE_ROOT_ABS);
    });

    it("toggleStatusNavPinPersonal adds then removes a personal pin, per team", () => {
      const TEAM = "org:team-pin";
      useViewerStore.setState({ statusNavPinnedPaths: {} });

      useViewerStore.getState().toggleStatusNavPinPersonal(TEAM, "_bmad-output/epic-1");
      useViewerStore.getState().toggleStatusNavPinPersonal(TEAM, "_bmad-output/notes/a.md");
      expect(useViewerStore.getState().statusNavPinnedPaths[TEAM]).toEqual([
        "_bmad-output/epic-1",
        "_bmad-output/notes/a.md",
      ]);

      // Toggling an existing pin removes only that one (additive/idempotent toggle).
      useViewerStore.getState().toggleStatusNavPinPersonal(TEAM, "_bmad-output/epic-1");
      expect(useViewerStore.getState().statusNavPinnedPaths[TEAM]).toEqual([
        "_bmad-output/notes/a.md",
      ]);
      // A different team is untouched (per-team isolation).
      expect(useViewerStore.getState().statusNavPinnedPaths["org:other"]).toBeUndefined();
    });

    it("cycleTab (which routes through setActiveTab) reveals the next tab", () => {
      seedRepo();
      // T.6: cycleTab derives the repoPath arg as the **synthetic
      // workspace key** built from teamKey + the active workspace's
      // sourceRepoSlug (NOT sourceRepoPath, the M.2 on-disk clone path).
      // Mirror the synthetic-key contract in the test fixture by:
      //  - seeding `workspaceRepos[SYNTHETIC_KEY]` so resolveRepo's
      //    workspace-first lookup hits;
      //  - giving the active workspace a `sourceRepoSlug` so cycleTab
      //    can derive the synthetic.
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
          } as RepoState,
        },
      });
      useViewerStore.setState({
        workspaces: {
          [REPO]: [
            {
              id: ROOT_ID,
              kind: "root",
              cwd: REPO,
              sourceRepoPath: REPO,
              sourceRepoSlug: TEST_SLUG,
              status: "ready",
            },
          ],
        },
        activeWorkspace: { [REPO]: ROOT_ID },
        openTabs: {
          [REPO]: [
            { path: FILE_B, name: "readme.md" },
            { path: FILE_A, name: "story.md" },
          ],
        },
        activeTab: { [REPO]: FILE_B },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().cycleTab(REPO, "file", 1);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
    });

    // T.6: the cross-team `selectedRepoPath` "wrong team" guard test was
    // deleted because the predicate no longer exists. cycleTab now reads
    // the cycling team's own active workspace's sourceRepoPath directly,
    // so there is no cross-team ambiguity to guard against — the threading
    // is mechanical, no team comparison needed.

    it("navigateBack re-expands the previous file's ancestors", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: {
          [REPO]: [
            { path: FILE_A, name: "story.md" },
            { path: FILE_B, name: "readme.md" },
          ],
        },
        activeTab: { [REPO]: FILE_B },
        navHistory: { [REPO]: [FILE_A, FILE_B] },
        navIndex: { [REPO]: 1 },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().navigateBack(REPO, REPO);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
    });

    it("navigateForward re-expands the forward target's ancestors", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: {
          [REPO]: [
            { path: FILE_B, name: "readme.md" },
            { path: FILE_A, name: "story.md" },
          ],
        },
        activeTab: { [REPO]: FILE_B },
        navHistory: { [REPO]: [FILE_B, FILE_A] },
        navIndex: { [REPO]: 0 },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().navigateForward(REPO, REPO);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
    });

    it("closeTab reveals the new active tab when the closed one was active", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: {
          [REPO]: [
            { path: FILE_A, name: "story.md" },
            { path: FILE_B, name: "readme.md" },
          ],
        },
        activeTab: { [REPO]: FILE_B },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().closeTab(REPO, REPO, FILE_B);

      expect(useViewerStore.getState().activeTab[REPO]).toBe(FILE_A);
      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
    });

    it("closeOtherTabs reveals the kept tab", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: {
          [REPO]: [
            { path: FILE_A, name: "story.md" },
            { path: FILE_B, name: "readme.md" },
          ],
        },
        activeTab: { [REPO]: FILE_B },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().closeOtherTabs(REPO, REPO, FILE_A);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain(FILE_A_ANCESTOR_DEEP);
    });

    it.each(["clone", "private"] as const)(
      "activates a retained tab from the other source without changing the %s nav",
      (kind) => {
        seedRepo();
        const privateRoot = "/home/users/42/scratch/orgs/test-org/plans/content";
        const drill = kind === "clone" ? FILE_A_ANCESTOR_DEEP : `${privateRoot}/drafts`;
        const file = kind === "clone" ? `${privateRoot}/archive/plan.md` : FILE_A;
        useViewerStore.getState().setNavLens(REPO, { source: { kind } });
        useViewerStore.setState({
          openTabs: { [REPO]: [{ path: file, name: "plan.md" }] },
          drillPaths: { [REPO]: drill },
          pipelineScope: { [REPO]: "drafts" },
          statusNavExpandedPaths: { [REPO]: [drill] },
          revealCounter: { [REPO]: 7 },
        });

        useViewerStore.getState().setActiveTab(REPO, REPO, file);

        const s = useViewerStore.getState();
        expect(s.activeTab[REPO]).toBe(file);
        expect(s.drillPaths[REPO]).toBe(drill);
        expect(s.pipelineScope[REPO]).toBe("drafts");
        expect(s.statusNavExpandedPaths[REPO]).toEqual([drill]);
        expect(s.revealCounter[REPO]).toBe(7);
      },
    );

    it("resets both drillPaths and pipelineScope when activating a file outside pipeline root while drilled in", () => {
      seedRepo();
      const VIEWER_ROOT = "/ctx/_bmad-output";
      useViewerStore.setState({
        openTabs: { [REPO]: [{ path: FILE_B, name: "readme.md" }] },
        drillPaths: { [REPO]: FILE_A_ANCESTOR_DEEP },
        pipelineScope: { [REPO]: "epic-a/sub" },
      });

      useViewerStore.getState().setActiveTab(REPO, REPO, FILE_B);

      const s = useViewerStore.getState();
      expect(s.drillPaths[REPO]).toBe(VIEWER_ROOT);
      expect(s.pipelineScope[REPO]).toBe("");
    });

    it("preserves drillPaths and pipelineScope when activating a file inside the current drill path", () => {
      seedRepo();
      useViewerStore.setState({
        openTabs: { [REPO]: [{ path: FILE_A, name: "story.md" }] },
        drillPaths: { [REPO]: FILE_A_ANCESTOR_DEEP },
        pipelineScope: { [REPO]: "epic-a/sub" },
      });

      useViewerStore.getState().setActiveTab(REPO, REPO, FILE_A);

      const s = useViewerStore.getState();
      expect(s.drillPaths[REPO]).toBe(FILE_A_ANCESTOR_DEEP);
      expect(s.pipelineScope[REPO]).toBe("epic-a/sub");
    });

    it("resets both drillPaths and pipelineScope when activating a pipeline file in a different sub-epic", () => {
      seedRepo();
      const VIEWER_ROOT = "/ctx/_bmad-output";
      useViewerStore.setState({
        openTabs: { [REPO]: [{ path: FILE_C, name: "story.md" }] },
        drillPaths: { [REPO]: FILE_A_ANCESTOR_DEEP },
        pipelineScope: { [REPO]: "epic-a/sub" },
      });

      useViewerStore.getState().setActiveTab(REPO, REPO, FILE_C);

      const s = useViewerStore.getState();
      expect(s.drillPaths[REPO]).toBe(VIEWER_ROOT);
      expect(s.pipelineScope[REPO]).toBe("");
    });

    it("expands ancestor folders for files inside viewerRoot (post-R-14)", () => {
      // R-14: scope-collapse — viewer base is viewerRoot, so files in
      // sibling subtrees of the legacy pipeline-root (e.g. `_bmad-output/docs/`)
      // now have ancestors that get expanded. Pre-R-14 this test asserted
      // an empty expansion set because FILE_B was outside `implementation-artifacts/`.
      seedRepo();
      useViewerStore.setState({
        openTabs: { [REPO]: [{ path: FILE_B, name: "readme.md" }] },
        statusNavExpandedPaths: { [REPO]: [] },
      });

      useViewerStore.getState().setActiveTab(REPO, REPO, FILE_B);

      const expanded = useViewerStore.getState().statusNavExpandedPaths[REPO] ?? [];
      expect(expanded).toContain("/ctx/_bmad-output/docs");
    });
  });

  // ---------------------------------------------------------------------------
  // setRightPaneSplitRatio — pre-existing simple setter without a test
  // ---------------------------------------------------------------------------
  it("setRightPaneSplitRatio writes per-repo and persists", () => {
    useViewerStore.getState().setRightPaneSplitRatio(REPO, 0.42);
    expect(useViewerStore.getState().rightPaneSplitRatio[REPO]).toBe(0.42);
    // Subsequent writes overwrite, not merge.
    useViewerStore.getState().setRightPaneSplitRatio(REPO, 0.6);
    expect(useViewerStore.getState().rightPaneSplitRatio[REPO]).toBe(0.6);
  });

  // ---------------------------------------------------------------------------
  // setTerminalSplitRatio — the global terminal/viewer split ratio (D39)
  // ---------------------------------------------------------------------------
  it("setTerminalSplitRatio writes the global ratio", () => {
    useViewerStore.getState().setTerminalSplitRatio(0.55);
    expect(useViewerStore.getState().terminalSplitRatio).toBe(0.55);
    // Subsequent writes overwrite.
    useViewerStore.getState().setTerminalSplitRatio(0.7);
    expect(useViewerStore.getState().terminalSplitRatio).toBe(0.7);
  });

  it("terminalSplitRatio survives layout mode transitions", () => {
    useViewerStore.getState().setTerminalSplitRatio(0.35);
    // Switch to console and back to shared
    useViewerStore.getState().setLayoutMode("console");
    expect(useViewerStore.getState().terminalSplitRatio).toBe(0.35);
    useViewerStore.getState().setLayoutMode("shared");
    expect(useViewerStore.getState().terminalSplitRatio).toBe(0.35);
  });

  // ---------------------------------------------------------------------------
  // syncDrillPath — single source of truth for drill+scope sync
  // ---------------------------------------------------------------------------
  describe("syncDrillPath", () => {
    const CTX = "/ctx/repo";
    // PTS-2: 4th syncDrillPath arg is now `teamContentPath` (the Rust-
    // resolved per-team content dir), not the source-side `viewerRoot`
    // segment. The test fixtures use the pre-PTS-2 composition
    // `${CTX}/_bmad-output` as that absolute path so scope assertions
    // still match the legacy expected shape.
    const TEAM_CONTENT = "/ctx/repo/_bmad-output";
    // R-14: scope-collapse — syncDrillPath hardcodes "" for the pipeline root
    // when calling `computePipelineScopeUpdate`, regardless of the
    // PIPELINE_ROOT param. Scope assertions include the
    // `implementation-artifacts/` segment because it's a regular subdir
    // of the content root, not a special base.
    const PIPELINE_BASE = "/ctx/repo/_bmad-output/implementation-artifacts";

    beforeEach(() => {
      useViewerStore.setState({
        ...DEFAULT_STATE,
        drillPaths: {},
        pipelineScope: {},
      });
    });

    it("writes drillPath for the target absolute path", () => {
      useViewerStore.getState().syncDrillPath(REPO, `${PIPELINE_BASE}/epic-a`, CTX, TEAM_CONTENT);
      expect(useViewerStore.getState().drillPaths[REPO]).toBe(`${PIPELINE_BASE}/epic-a`);
    });

    it("derives pipelineScope from a target inside the content root (post-R-14)", () => {
      // R-14: scope is relative to the content root, so the
      // `implementation-artifacts/` segment is now part of the scope path.
      useViewerStore
        .getState()
        .syncDrillPath(REPO, `${PIPELINE_BASE}/epic-a/sub`, CTX, TEAM_CONTENT);
      expect(useViewerStore.getState().pipelineScope[REPO]).toBe(
        "implementation-artifacts/epic-a/sub",
      );
    });

    it("derives pipelineScope when target equals the legacy pipeline-root dir", () => {
      // R-14: previously this case reset scope to "" because the target was
      // the pipeline base. Post-collapse it's just another subdir.
      useViewerStore.setState({ pipelineScope: { [REPO]: "epic-a" } });
      useViewerStore.getState().syncDrillPath(REPO, PIPELINE_BASE, CTX, TEAM_CONTENT);
      expect(useViewerStore.getState().pipelineScope[REPO]).toBe("implementation-artifacts");
    });

    it("resets pipelineScope to '' when target is above the legacy pipeline-root dir but inside the content root", () => {
      // Drilling out to the content root → scope clears to "".
      useViewerStore.setState({ pipelineScope: { [REPO]: "epic-a" } });
      useViewerStore.getState().syncDrillPath(REPO, "/ctx/repo/_bmad-output", CTX, TEAM_CONTENT);
      expect(useViewerStore.getState().pipelineScope[REPO]).toBe("");
    });

    it("pins pipelineScope (no write) when target is outside the context repo", () => {
      // computePipelineScopeUpdate returns null in this case — store
      // should not touch pipelineScope.
      useViewerStore.setState({ pipelineScope: { [REPO]: "epic-a" } });
      useViewerStore.getState().syncDrillPath(REPO, "/totally/different/repo", CTX, TEAM_CONTENT);
      expect(useViewerStore.getState().pipelineScope[REPO]).toBe("epic-a");
    });
  });

  // ---------------------------------------------------------------------------
  // Per-repo state isolation
  // ---------------------------------------------------------------------------
  describe("per-repo state isolation", () => {
    it("actions on one repo do not affect another repo's state", () => {
      // Given: open files in two repos
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO2, REPO2, "b.md");

      // When: close tab in REPO
      useViewerStore.getState().closeTab(REPO, REPO, "a.md");

      // Then: REPO2 is unaffected
      expect(useViewerStore.getState().openTabs[REPO]).toEqual([]);
      expect(useViewerStore.getState().openTabs[REPO2]).toHaveLength(1);
    });
  });

  // ---------------------------------------------------------------------------
  // Preview / pinned tabs (Story 32.2 Part 1)
  // ---------------------------------------------------------------------------
  describe("openFilePreview / pinTab / preview semantics", () => {
    it("openFilePreview on an empty repo appends a tab with preview=true and sets active", () => {
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");

      const s = useViewerStore.getState();
      expect(s.openTabs[REPO]).toHaveLength(1);
      expect(s.openTabs[REPO]![0]).toMatchObject({ path: "a.md", preview: true });
      expect(s.activeTab[REPO]).toBe("a.md");
    });

    it("openFilePreview replaces the existing preview tab in place (no position change)", () => {
      // Given: pinned A, preview B
      useViewerStore.getState().openFile(REPO, REPO, "a.md"); // pinned
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md"); // preview at index 1

      // When: open C as preview
      useViewerStore.getState().openFilePreview(REPO, REPO, "c.md");

      // Then: A still pinned at index 0, C replaced B at index 1
      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs.map((t) => t.path)).toEqual(["a.md", "c.md"]);
      expect(tabs[0].preview).toBeFalsy();
      expect(tabs[1].preview).toBe(true);
    });

    it("openFilePreview pushes to navHistory identically to openFile", () => {
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md");

      const s = useViewerStore.getState();
      expect(s.navHistory[REPO]).toEqual(["a.md", "b.md"]);
      expect(s.navIndex[REPO]).toBe(1);
    });

    it("openFilePreview on an already-pinned tab leaves the preview flag false", () => {
      // Given: A is pinned
      useViewerStore.getState().openFile(REPO, REPO, "a.md");

      // When: single-click opens A via preview path
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");

      // Then: A stays pinned (preview flag not set)
      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs).toHaveLength(1);
      expect(tabs[0].preview).toBeFalsy();
      expect(useViewerStore.getState().activeTab[REPO]).toBe("a.md");
    });

    it("openFile on an existing preview tab pins it in place (dblclick / history-nav commit)", () => {
      // Given: A pinned at index 0, B as preview at index 1
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md");

      // When: openFile fires for B (tree-row dblclick second event)
      useViewerStore.getState().openFile(REPO, REPO, "b.md");

      // Then: B stays at index 1 but is now pinned
      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs.map((t) => t.path)).toEqual(["a.md", "b.md"]);
      expect(tabs[1].preview).toBeFalsy();
    });

    it("pinTab flips preview: true to preview: false", () => {
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");
      useViewerStore.getState().pinTab(REPO, "a.md");

      expect(useViewerStore.getState().openTabs[REPO]![0].preview).toBeFalsy();
    });

    it("pinTab on an already-pinned tab is a no-op", () => {
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      const before = useViewerStore.getState().openTabs[REPO];

      useViewerStore.getState().pinTab(REPO, "a.md");

      // Same reference — no set() fan-out
      expect(useViewerStore.getState().openTabs[REPO]).toBe(before);
    });

    it("closing a preview tab does not affect pinned siblings", () => {
      useViewerStore.getState().openFile(REPO, REPO, "a.md"); // pinned
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md"); // preview

      useViewerStore.getState().closeTab(REPO, REPO, "b.md");

      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs.map((t) => t.path)).toEqual(["a.md"]);
      expect(tabs[0].preview).toBeFalsy();
    });

    it("reorderTab with fromIdx === toIdx leaves the preview flag alone (no-op drag)", () => {
      useViewerStore.getState().openFile(REPO, REPO, "a.md"); // pinned
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md"); // preview at 1

      useViewerStore.getState().reorderTab(REPO, 1, 1);

      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs[1].preview).toBe(true);
    });

    it("reorderTab with fromIdx !== toIdx pins a preview tab (drag = commit)", () => {
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md");

      useViewerStore.getState().reorderTab(REPO, 1, 0);

      const tabs = useViewerStore.getState().openTabs[REPO]!;
      // b moved to index 0 and pinned
      expect(tabs.map((t) => t.path)).toEqual(["b.md", "a.md"]);
      expect(tabs[0].preview).toBeFalsy();
    });

    it("preview flag persists across set → re-set (rehydrate round-trip)", () => {
      // Given: one preview tab
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");
      const snapshot = useViewerStore.getState().openTabs;

      // When: simulate rehydrate — write the snapshot back
      useViewerStore.setState({ openTabs: snapshot });

      // Then: preview flag survives
      expect(useViewerStore.getState().openTabs[REPO]![0].preview).toBe(true);
    });

    it("at most one preview tab per repo", () => {
      useViewerStore.getState().openFilePreview(REPO, REPO, "a.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "c.md");

      const tabs = useViewerStore.getState().openTabs[REPO]!;
      expect(tabs.filter((t) => t.preview)).toHaveLength(1);
    });

    it("navigateBack pins an existing preview tab (AC-P6: history nav is a commit)", () => {
      // A (pinned) → B (preview) → navigate back to A, then forward to B.
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFilePreview(REPO, REPO, "b.md");

      expect(
        useViewerStore.getState().openTabs[REPO]!.find((t) => t.path === "b.md")?.preview,
      ).toBe(true);

      useViewerStore.getState().navigateBack(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("a.md");

      useViewerStore.getState().navigateForward(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");
      expect(
        useViewerStore.getState().openTabs[REPO]!.find((t) => t.path === "b.md")?.preview,
      ).toBeFalsy();
    });
  });

  // ---------------------------------------------------------------------------
  // Keep-alive viewer paths (Story 32.2 Part 2 — Task 2.4)
  // ---------------------------------------------------------------------------
  describe("touchViewerPath / openedViewerPaths", () => {
    it("prepends a path to the per-repo LRU", () => {
      useViewerStore.getState().touchViewerPath(REPO, "a.md");
      useViewerStore.getState().touchViewerPath(REPO, "b.md");

      expect(useViewerStore.getState().openedViewerPaths[REPO]).toEqual(["b.md", "a.md"]);
    });

    it("re-touching an existing path moves it to the front (LRU)", () => {
      const s = useViewerStore.getState();
      s.touchViewerPath(REPO, "a.md");
      s.touchViewerPath(REPO, "b.md");
      s.touchViewerPath(REPO, "c.md");
      s.touchViewerPath(REPO, "a.md"); // re-touch

      expect(useViewerStore.getState().openedViewerPaths[REPO]).toEqual(["a.md", "c.md", "b.md"]);
    });

    it("re-touching the already-front path is a no-op (same state reference)", () => {
      useViewerStore.getState().touchViewerPath(REPO, "a.md");
      const before = useViewerStore.getState().openedViewerPaths;

      useViewerStore.getState().touchViewerPath(REPO, "a.md");

      expect(useViewerStore.getState().openedViewerPaths).toBe(before);
    });

    it("caps the list at VIEWER_PERF.keepAliveMaxViewers, evicting the oldest", async () => {
      const { VIEWER_PERF } = await import("@/shared/lib/viewer-perf-config");
      const cap = VIEWER_PERF.keepAliveMaxViewers;

      // Push cap+2 distinct paths — oldest two should drop off.
      for (let i = 0; i < cap + 2; i++) {
        useViewerStore.getState().touchViewerPath(REPO, `file-${i}.md`);
      }

      const list = useViewerStore.getState().openedViewerPaths[REPO]!;
      expect(list).toHaveLength(cap);
      // Most recent first
      expect(list[0]).toBe(`file-${cap + 1}.md`);
      // Oldest (file-0.md and file-1.md) evicted
      expect(list).not.toContain("file-0.md");
      expect(list).not.toContain("file-1.md");
    });

    it("per-repo LRUs are isolated", () => {
      useViewerStore.getState().touchViewerPath(REPO, "a.md");
      useViewerStore.getState().touchViewerPath(REPO2, "x.md");

      expect(useViewerStore.getState().openedViewerPaths[REPO]).toEqual(["a.md"]);
      expect(useViewerStore.getState().openedViewerPaths[REPO2]).toEqual(["x.md"]);
    });

    it("ignores empty filePath (no-op)", () => {
      useViewerStore.getState().touchViewerPath(REPO, "");

      expect(useViewerStore.getState().openedViewerPaths[REPO]).toBeUndefined();
    });

    // T.6: closeRepo retired alongside selectRepo / goToPicker. The
    // analogous test for the new team-level cleanup lives below as the
    // closeAllTabs case; closeTeam itself is covered in the orchestrator
    // suite.

    it("closeAllTabs clears the repo's keep-alive list", () => {
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");
      useViewerStore.getState().touchViewerPath(REPO, "a.md");
      useViewerStore.getState().touchViewerPath(REPO, "b.md");

      useViewerStore.getState().closeAllTabs(REPO);

      expect(useViewerStore.getState().openedViewerPaths[REPO]).toBeUndefined();
    });
  });

  // ---------------------------------------------------------------------------
  // F-8: flushPersist
  // ---------------------------------------------------------------------------
  describe("F-8: flushPersist", () => {
    // Persisting is a no-op until the saved state has loaded (SW-FU-B2).
    beforeEach(() => {
      useViewerStore.setState({ stateLoaded: true });
    });

    it("flushPersist fires when pending debounced writes exist", async () => {
      const { saveAppState } = await import("@/tauri/commands");
      vi.mocked(saveAppState).mockClear();
      useViewerStore.setState({
        ...DEFAULT_STATE,
        stateLoaded: true,
        workspaces: {
          [REPO]: [{ id: ROOT_ID, kind: "root" as const, cwd: REPO, status: "ready" as const }],
        },
        activeWorkspace: { [REPO]: ROOT_ID },
      });

      // Trigger a debounced persist (non-critical path)
      useViewerStore.getState().persistState();
      expect(saveAppState).not.toHaveBeenCalled();

      // Flush — simulates the onCloseRequested handler
      await useViewerStore.getState().flushPersist();
      expect(saveAppState).toHaveBeenCalledTimes(1);

      // Original debounced timer should be cancelled — no double-write
      vi.advanceTimersByTime(500);
      await vi.advanceTimersByTimeAsync(0);
      expect(saveAppState).toHaveBeenCalledTimes(1);
    });
  });

  // ---------------------------------------------------------------------------
  // T.4 AC-15 / Task 9.3 + 9.4 — team-keyed slice isolation
  // ---------------------------------------------------------------------------
  describe("team isolation (T.4 AC-15)", () => {
    const TEAM_A = "acme:default";
    const TEAM_B = "acme:eng";

    it("9.3: actions on one team don't perturb sibling team's state", () => {
      // Pre-seed both teams across the spectrum of T.4 slices.
      useViewerStore.setState({
        openTabs: {
          [TEAM_A]: [{ path: "/a.md", name: "a" }],
          [TEAM_B]: [{ path: "/b.md", name: "b" }],
        },
        activeTab: { [TEAM_A]: "/a.md", [TEAM_B]: "/b.md" },
        drillPaths: { [TEAM_A]: "/drill-a", [TEAM_B]: "/drill-b" },
        pipelineScope: { [TEAM_A]: "scope-a", [TEAM_B]: "scope-b" },
      });

      // Mutate TEAM_A via the typical action surface.
      useViewerStore.getState().closeTab(TEAM_A, REPO, "/a.md");
      useViewerStore.getState().setDrillPath(TEAM_A, "/new-drill-a");
      useViewerStore.getState().setPipelineScope(TEAM_A, "new-scope-a");

      const s = useViewerStore.getState();
      // TEAM_B state is unchanged.
      expect(s.openTabs[TEAM_B]).toEqual([{ path: "/b.md", name: "b" }]);
      expect(s.activeTab[TEAM_B]).toBe("/b.md");
      expect(s.drillPaths[TEAM_B]).toBe("/drill-b");
      expect(s.pipelineScope[TEAM_B]).toBe("scope-b");
      // TEAM_A reflects the mutations.
      expect(s.openTabs[TEAM_A]).toEqual([]);
      expect(s.drillPaths[TEAM_A]).toBe("/new-drill-a");
      expect(s.pipelineScope[TEAM_A]).toBe("new-scope-a");
    });

    it("9.4: switching the active workspace within a team does NOT change layoutMode", () => {
      // layoutMode is one global setting (D39), not per-workspace. Switching
      // the active workspace must not mutate it.
      useViewerStore.setState({
        layoutMode: "console",
        workspaces: {
          [TEAM_A]: [
            { id: "root", kind: "root", cwd: "/r", status: "ready" },
            { id: "ws-1", kind: "story", cwd: "/wt-1", storyFilePath: "/s1.md", status: "ready" },
          ],
        },
        activeWorkspace: { [TEAM_A]: "root" },
      });

      useViewerStore.getState().switchWorkspace(TEAM_A, "ws-1");

      const s = useViewerStore.getState();
      expect(s.activeWorkspace[TEAM_A]).toBe("ws-1");
      // layoutMode is unchanged — it's global, not workspace-scoped.
      expect(s.layoutMode).toBe("console");
    });
  });
});
