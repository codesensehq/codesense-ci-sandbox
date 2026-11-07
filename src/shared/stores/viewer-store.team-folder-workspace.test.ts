// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore, genericWorkspaceId } from "./viewer-store";
import { useOrgStore } from "./org-store";
import { ROOT_NO_REPOS_SENTINEL } from "@/shared/lib/team-key";
import * as commands from "@/tauri/commands";
import type { Org } from "@/shared/types/domain";

// SW-WV-A6: the team-folder workspace is the ever-present root CLI — a
// `kind:"generic"` terminal rooted at the team's context-repo content dir
// (the attached organization’s team content root). It guarantees the operator is never left in a
// dead-end with no CLI: closing the last foreground tile re-ensures it
// synchronously so `activeWorkspace[teamKey]` is never `ROOT_NO_REPOS_SENTINEL`
// once Weft is initialized for the team.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  // SW-GW-8: a nav-opened generic dir folds its session into the trusted registry.
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
  // AC-4: kind:"generic" opts into the load-time staleness sweep, which
  // validates the cwd on disk — the context-repo dir is real, so it passes.
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

const TEAM = "test-org:default";
const CONTENT_PATH = "/attached/plans/default/content";
const TEAM_FOLDER_ID = genericWorkspaceId(CONTENT_PATH);

function seedTeamContent(orgRoot = "/attached/plans"): void {
  const org: Org = {
    slug: "test-org",
    display_name: "Test",
    org_root: orgRoot,
    created_at: "",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [{ name: "default", source_repos: [] }],
  };
  useOrgStore.setState({
    activeOrgSlug: "test-org",
    activeTeamName: "default",
    orgs: [org],
    stateLoaded: true,
    discoveryComplete: true,
    workspaceRepos: {},
  });
}

function resetViewer(): void {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    mruWorkspaceStack: {},
    trayMode: {},
    selectedEpicDirName: {},
    railKeyByWorkspaceId: {},
  });
}

beforeEach(() => {
  resetViewer();
  vi.clearAllMocks();
  useOrgStore.setState({
    orgs: [],
    discoveryComplete: false,
    activeOrgSlug: null,
    activeTeamName: null,
    workspaceRepos: {},
  });
});

// ── Task 1 / AC-1, AC-3, AC-6 ───────────────────────────────────────────────
describe("openOrFocusTeamFolderWorkspace (SW-WV-A6 Task 1)", () => {
  const live = () => useViewerStore.getState().workspaces[TEAM] ?? [];

  it("SV-1: bootstraps a source-free team with no previous workspace at its canonical root", () => {
    seedTeamContent();
    useViewerStore.getState().openOrFocusTeamFolderWorkspace(TEAM);
    const ws = live().find((w) => w.id === TEAM_FOLDER_ID);
    expect(ws).toMatchObject({
      id: TEAM_FOLDER_ID,
      kind: "generic",
      cwd: CONTENT_PATH,
      status: "ready",
    });
    // The team's active workspace is the team folder.
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(TEAM_FOLDER_ID);
  });

  it("AC-3: a second ensure focuses the existing record, never duplicates it", () => {
    seedTeamContent();
    useViewerStore.getState().openOrFocusTeamFolderWorkspace(TEAM);
    useViewerStore.getState().openOrFocusTeamFolderWorkspace(TEAM);
    expect(live().filter((w) => w.id === TEAM_FOLDER_ID)).toHaveLength(1);
    expect(live().filter((w) => w.kind === "generic")).toHaveLength(1);
  });

  it("AC-7: an attached team with an unavailable organization root is a no-op", () => {
    seedTeamContent("");
    useViewerStore.getState().openOrFocusTeamFolderWorkspace(TEAM);
    expect(live()).toEqual([]);
  });

  it("AC-7: a team still loading organization discovery is a no-op", () => {
    // No registered organization or team content root.
    useViewerStore.getState().openOrFocusTeamFolderWorkspace(TEAM);
    expect(useViewerStore.getState().workspaces[TEAM] ?? []).toEqual([]);
  });
});

// ── Task 2 / AC-2, AC-5, AC-7 ───────────────────────────────────────────────
describe("ensure-on-last-close (SW-WV-A6 Task 2)", () => {
  const live = () => useViewerStore.getState().workspaces[TEAM] ?? [];

  it("AC-2/AC-5: closing the last foreground tile restores the team folder SYNCHRONOUSLY", () => {
    seedTeamContent();
    // Open a single story workspace as the only foreground tile.
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: "story-1", kind: "story", cwd: "/wt/story-1" });
    expect(live().some((w) => w.id === "story-1")).toBe(true);

    // Close it via the user-facing path. NO await — AC-5 is structural.
    useViewerStore.getState().requestCloseWorkspace(TEAM, "story-1");

    const state = useViewerStore.getState();
    // The team is never empty: the team folder is present…
    const teamFolder = (state.workspaces[TEAM] ?? []).find((w) => w.id === TEAM_FOLDER_ID);
    expect(teamFolder).toMatchObject({ kind: "generic", cwd: CONTENT_PATH });
    // …and the active workspace is a real id, never the empty-UI sentinel.
    expect(state.activeWorkspace[TEAM]).not.toBe(ROOT_NO_REPOS_SENTINEL);
    expect(state.activeWorkspace[TEAM]).toBe(TEAM_FOLDER_ID);
  });

  it("AC-7: closing a NON-last workspace does not mint a team-folder record", () => {
    seedTeamContent();
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: "story-1", kind: "story", cwd: "/wt/story-1" });
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: "story-2", kind: "story", cwd: "/wt/story-2" });

    useViewerStore.getState().requestCloseWorkspace(TEAM, "story-1");

    // story-2 remains a foreground tile → no team folder was ensured.
    const list = useViewerStore.getState().workspaces[TEAM] ?? [];
    expect(list.some((w) => w.id === "story-2")).toBe(true);
    expect(list.some((w) => w.kind === "generic")).toBe(false);
  });

  it("AC-7: last-close for a team with no teamContentPath leaves it empty at the sentinel", () => {
    seedTeamContent("");
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: "story-1", kind: "story", cwd: "/wt/story-1" });

    useViewerStore.getState().requestCloseWorkspace(TEAM, "story-1");

    const state = useViewerStore.getState();
    // No team folder minted — the existing TeamWorkspaceEmptyState branch holds.
    expect(state.workspaces[TEAM] ?? []).toEqual([]);
    expect(state.activeWorkspace[TEAM]).toBe(ROOT_NO_REPOS_SENTINEL);
  });
});

// ── Task 3 / AC-4 ───────────────────────────────────────────────────────────
describe("team-folder persistence round-trip (SW-WV-A6 Task 3)", () => {
  const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
  const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

  beforeEach(() => {
    loadAppStateMock.mockReset().mockResolvedValue("{}");
    saveAppStateMock.mockReset().mockResolvedValue(undefined);
  });

  it("AC-4: kind:generic team-folder survives persist → reload (loadMigration:keep, not swept)", async () => {
    seedTeamContent();
    // Seed the team-folder workspace as the only tile for the team.
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: TEAM_FOLDER_ID, kind: "generic", cwd: CONTENT_PATH, status: "ready" }],
      },
      activeWorkspace: { [TEAM]: TEAM_FOLDER_ID },
    });

    let capturedPayload: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      capturedPayload = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(capturedPayload).not.toBeNull();

    // Reset and reload from the captured payload.
    resetViewer();
    seedTeamContent();
    loadAppStateMock.mockResolvedValue(capturedPayload!);
    await useViewerStore.getState().loadPersistedState();

    const state = useViewerStore.getState();
    const restored = state.workspaces[TEAM] ?? [];
    // The generic team-folder record survives — NOT swept by the SW-EW-2
    // kind:"root" migration or any other load-time sweep.
    expect(restored).toHaveLength(1);
    expect(restored[0]).toMatchObject({ id: TEAM_FOLDER_ID, kind: "generic", cwd: CONTENT_PATH });
    // Active resolves back to the team folder, not the empty-UI sentinel.
    expect(state.activeWorkspace[TEAM]).toBe(TEAM_FOLDER_ID);
    expect(state.activeWorkspace[TEAM]).not.toBe(ROOT_NO_REPOS_SENTINEL);
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
