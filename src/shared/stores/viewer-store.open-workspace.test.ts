// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  useViewerStore,
  genericWorkspaceId,
  selectActiveTerminalTab,
  selectWorkspaceTabs,
  normalizeWorkspaceDir,
  shouldFocusOnOpen,
} from "./viewer-store";
import { storyIdFromPath } from "@/shared/lib/worktree";
import { dashboardRegisterSession } from "@/tauri/commands";
import {
  makeRealOpenWorkspaceTabDeps,
  makeRealSendToWorkspaceTabDeps,
  GENERIC_AGENT_ROLE,
} from "@/features/dashboard/components/workspace-tab-verbs";
import { useOrgStore } from "./org-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  // SW-GW-8: a nav-opened generic dir folds its session into the trusted registry.
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
}));

const TEAM = "test-org:default";
const WID = "ws-1";

installTerminalLayoutHarness();

const wsTabs = (teamKey: string, id: string) =>
  selectWorkspaceTabs(useViewerStore.getState(), teamKey, id);

beforeEach(() => {
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    mruWorkspaceStack: {},
    trayMode: {},
    selectedEpicDirName: {},
    railKeyByWorkspaceId: {},
  });
  // Reset the active team so the SW-GW-2 M1 test's org-store mutation can't leak.
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
});

// SLW-CO-6 (AC-1/AC-2/AC-7/AC-8): the unified sync open primitive that
// epic/git-sync/minimized route through. The story path is NOT covered here
// (its hardened async create lives in `ensureWorktree`).
describe("openWorkspace core (SLW-CO-6 AC-1/AC-2/AC-7)", () => {
  const live = () => useViewerStore.getState().workspaces[TEAM] ?? [];
  const shadow = (id: string) => useViewerStore.getState().softClosedWorkspaces[TEAM]?.[id];
  const tabs = (id: string) => wsTabs(TEAM, id);

  it("reveals an existing host workspace without creating another shell", () => {
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "generic",
      cwd: "/managed/worker",
      spawn: false,
    });
    expect(live().find((w) => w.id === WID)).toMatchObject({
      cwd: "/managed/worker",
      status: "ready",
    });
    expect(terminalHost().opened).toHaveLength(0);
  });

  it("resolved cwd ⇒ born 'ready', focused, first shell spawned (no overlay path)", () => {
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: WID, kind: "generic", cwd: "/root" });
    const ws = live().find((w) => w.id === WID);
    expect(ws).toMatchObject({ id: WID, kind: "generic", cwd: "/root", status: "ready" });
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(WID);
    expect(tabs(WID)).toHaveLength(1);
  });

  it("opened before the hosts' layout is known: the first shell waits for it, and is skipped when the host already holds terminals there", () => {
    useViewerStore.setState({
      terminalLayout: { hydrated: false, admission: {}, members: {}, byKey: {} },
    });
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: WID, kind: "generic", cwd: "/root" });
    expect(terminalHost().opened).toHaveLength(0);

    terminalHost().seed(
      `${TEAM}|${WID}`,
      { displayName: WID, kind: "generic" },
      {
        session: "survivor",
      },
    );
    useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [terminalHost().reply()] });
    expect(terminalHost().opened).toHaveLength(0);
    expect(tabs(WID).map((t) => t.sessionId)).toEqual(["survivor"]);
  });

  it("empty cwd ⇒ born 'creating' (overlay), NO shell spawned until the dir resolves", () => {
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: WID, kind: "epic", cwd: "" });
    expect(live().find((w) => w.id === WID)?.status).toBe("creating");
    // No `chdir("")`: the shell is deferred.
    expect(tabs(WID)).toHaveLength(0);

    // A later open with the resolved dir flips creating → ready and spawns.
    useViewerStore
      .getState()
      .openWorkspace({ teamKey: TEAM, workspaceId: WID, kind: "epic", cwd: "/epic/dir" });
    const ws = live().find((w) => w.id === WID);
    expect(ws).toMatchObject({ cwd: "/epic/dir", status: "ready" });
    expect(tabs(WID)).toHaveLength(1);
    // Still a singleton — the second open focused, didn't duplicate.
    expect(live().filter((w) => w.id === WID)).toHaveLength(1);
  });

  it("minimized:true ⇒ shadow entry only (no foreground tile, no xterm mount, no spawn)", () => {
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "generic",
      cwd: "/org",
      minimized: true,
    });
    expect(live().find((w) => w.id === WID)).toBeUndefined();
    expect(shadow(WID)?.workspace).toMatchObject({ id: WID, kind: "generic", cwd: "/org" });
    expect(terminalHost().opened).toHaveLength(0);
    expect(tabs(WID)).toHaveLength(0);
  });

  it("minimized on an already-LIVE workspace leaves it live (no yank-back), refreshing cwd", () => {
    const s = useViewerStore.getState();
    s.openWorkspace({ teamKey: TEAM, workspaceId: WID, kind: "generic", cwd: "/old" });
    s.openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "generic",
      cwd: "/new",
      minimized: true,
    });
    expect(live().find((w) => w.id === WID)?.cwd).toBe("/new");
    expect(shadow(WID)).toBeUndefined();
  });

  it("recordFields are merged onto the record (kind-specific fields)", () => {
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "epic",
      cwd: "/epic",
      recordFields: { epicDirPath: "epic-leaf", epicDocPath: "/docs/epic.md" },
    });
    expect(live().find((w) => w.id === WID)).toMatchObject({
      epicDirPath: "epic-leaf",
      epicDocPath: "/docs/epic.md",
    });
  });

  it("restoreSoftClosed reopens a minimized workspace (keeps its running session)", () => {
    const s = useViewerStore.getState();
    // Seed a soft-closed shadow with a surviving tab.
    s.openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "generic",
      cwd: "/root",
      minimized: true,
    });
    seedTerminalTabs(TEAM, WID, [{ session: "t1", name: "T", role: "notes" }]);
    // Open WITH restore → the shadow is reopened as a live workspace with its tab.
    s.openWorkspace({
      teamKey: TEAM,
      workspaceId: WID,
      kind: "generic",
      cwd: "/root",
      restoreSoftClosed: true,
    });
    expect(live().find((w) => w.id === WID)).toBeDefined();
    expect(shadow(WID)).toBeUndefined();
    expect(tabs(WID).map((t) => t.id)).toContain("t1");
  });
});

// SW-FX-69: open is additive + non-disruptive. The pure focus-policy guard and
// its store-level effect (default no-steal, opt-in focus, empty-mosaic guard).
describe("shouldFocusOnOpen (SW-FX-69 pure guard)", () => {
  it("empty mosaic (no other live tile) ⇒ focus regardless of the flag (AC-3)", () => {
    expect(shouldFocusOnOpen(undefined, false)).toBe(true);
    expect(shouldFocusOnOpen(false, false)).toBe(true);
    expect(shouldFocusOnOpen(true, false)).toBe(true);
  });

  it("another live tile present ⇒ focus only when explicitly requested (AC-1/AC-2)", () => {
    expect(shouldFocusOnOpen(undefined, true)).toBe(false); // default no-steal
    expect(shouldFocusOnOpen(false, true)).toBe(false); // explicit background
    expect(shouldFocusOnOpen(true, true)).toBe(true); // opt-in
  });
});

describe("openWorkspace focus policy (SW-FX-69)", () => {
  const live = () => useViewerStore.getState().workspaces[TEAM] ?? [];
  const active = () => useViewerStore.getState().activeWorkspace[TEAM];
  const mru = () => useViewerStore.getState().mruWorkspaceStack[TEAM] ?? [];
  const openWs = (id: string, focus?: boolean) =>
    useViewerStore.getState().openWorkspace({
      teamKey: TEAM,
      workspaceId: id,
      kind: "generic",
      cwd: "/root/" + id,
      focus,
    });

  it("AC-3: the FIRST open into an empty mosaic becomes active (nothing to preserve)", () => {
    openWs("w1");
    expect(active()).toBe("w1");
  });

  it("AC-1: opening a second workspace does NOT steal focus (default) — and no MRU push for it", () => {
    openWs("w1");
    openWs("w2"); // default: no focus flag
    expect(live().map((w) => w.id)).toEqual(["w1", "w2"]); // appended, not replaced
    expect(active()).toBe("w1"); // focus preserved on the tile we were in
    expect(mru()).not.toContain("w2"); // R3: no recency pollution for a background open
  });

  it("AC-2: focus:true on a second open jumps to it (opt-in) and pushes MRU", () => {
    openWs("w1");
    openWs("w2", true);
    expect(active()).toBe("w2");
    expect(mru()).toContain("w2");
  });

  it("AC-3: focus:false into an empty mosaic STILL focuses (empty-mosaic beats the flag)", () => {
    openWs("w1", false);
    expect(active()).toBe("w1");
  });

  // Code-review r1 (Blind Hunter): re-opening an ALREADY-LIVE background tile is the
  // "focus" half of openOrFocus — it must come to front even under the additive default.
  it("re-opening an already-live BACKGROUND tile focuses it (openOrFocus contract)", () => {
    openWs("w1");
    openWs("w2"); // w2 is background; active stays w1
    expect(active()).toBe("w1");
    openWs("w2"); // re-open the already-live w2 → focus it
    expect(active()).toBe("w2");
    expect(live().map((w) => w.id)).toEqual(["w1", "w2"]); // no duplicate, just focus
  });

  // Code-review r1 (Blind Hunter): the focus signal is the team's ACTIVE workspace,
  // not a rail-scoped tile count — preserve focus whenever a DIFFERENT live tile is active.
  it("a new background open preserves focus on whichever tile is active", () => {
    openWs("w1");
    openWs("w2", true); // explicitly focus w2 → active w2
    expect(active()).toBe("w2");
    openWs("w3"); // new background open → must not steal from the active w2
    expect(active()).toBe("w2");
    expect(mru()).not.toContain("w3");
  });

  // Code-review r2 (Blind Hunter): reveal-gesture wrappers (git-sync/notes/generic/
  // epic) opt into focus:true — clicking "open git-sync" into a live mosaic must jump.
  it("a reveal wrapper (openOrFocusGenericWorkspace) focuses even into a live mosaic", () => {
    openWs("w1"); // active w1, mosaic non-empty
    useViewerStore.getState().openOrFocusGenericWorkspace(TEAM, "/proj");
    expect(active()).toBe("generic:/proj"); // reveal jumped, did not open in background
  });

  // Code-review r2 (Blind/Edge): an EXPLICIT focus:false on a re-open is honored —
  // wasAlreadyOpen only supplies the UNSET default, it never overrides an explicit flag.
  it("focus:false on a re-open is honored (does not force a jump)", () => {
    openWs("w1");
    openWs("w2"); // background; active stays w1
    expect(active()).toBe("w1");
    openWs("w2", false); // explicit background re-open → must NOT steal
    expect(active()).toBe("w1");
  });

  // Code-review r3 (Verification Plan #1): drive the REAL story-open `onCreating`
  // closure (focus Site A) and assert it does not steal focus — the site that fires
  // at creation-record insert, the earliest point the yank was seen.
  it("story onCreating (Site A) does NOT steal focus from the active tile", async () => {
    openWs("w1"); // active w1, mosaic non-empty
    expect(active()).toBe("w1");
    const storyFile = "/ctx/epic/SW-XX-1-demo.md";
    const storyId = storyIdFromPath(storyFile);
    // Stub the async worktree driver to insert the creating record + fire the real
    // onCreating hook, the way ensureWorktree does mid-flight. Save/restore so the
    // action override never leaks to other tests.
    const originalEnsureWorktree = useViewerStore.getState().ensureWorktree;
    useViewerStore.setState({
      ensureWorktree: (async (
        tk: string,
        sf: string,
        _osp?: string,
        _es?: string,
        opts?: { onCreating?: () => void },
      ) => {
        useViewerStore.setState((s) => ({
          workspaces: {
            ...s.workspaces,
            [tk]: [
              ...(s.workspaces[tk] ?? []),
              { id: storyIdFromPath(sf), kind: "story", cwd: "/wt", status: "ready" },
            ],
          },
        }));
        opts?.onCreating?.(); // Site A runs the real applyOpenFocus
        return "/wt";
      }) as never,
    });
    try {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, storyFile);
    } finally {
      useViewerStore.setState({ ensureWorktree: originalEnsureWorktree });
    }
    expect(active()).toBe("w1"); // Site A + tail both additive → focus preserved
    expect(live().some((w) => w.id === storyId)).toBe(true); // the story tile did open
  });

  // Code-review r1/r2: the team-global active-workspace signal keeps the focus
  // decision correct in GROUPED (rail) mode, where a rail-scoped count would misread.
  it("grouped mode: a background open does not steal focus from the active tile", () => {
    useViewerStore.setState({
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: { w1: "alpha" } },
    });
    openWs("w1", true); // focus w1 in rail alpha
    expect(active()).toBe("w1");
    openWs("w2"); // new background open (different/unindexed rail) → keep w1 focused
    expect(active()).toBe("w1");
    expect(mru()).not.toContain("w2");
  });
});

// SW-GW-1: the generic "open a terminal at any dir" wrapper over openWorkspace.
describe("openOrFocusGenericWorkspace (SW-GW-1)", () => {
  const live = () => useViewerStore.getState().workspaces[TEAM] ?? [];
  const tabs = (id: string) => wsTabs(TEAM, id);

  it("id-minting is a normalized, per-dir id (trailing slashes stripped, root preserved)", () => {
    expect(normalizeWorkspaceDir("/Users/me/scratch/")).toBe("/Users/me/scratch");
    expect(normalizeWorkspaceDir("/Users/me/scratch///")).toBe("/Users/me/scratch");
    expect(normalizeWorkspaceDir("/")).toBe("/");
    expect(genericWorkspaceId("/Users/me/scratch")).toBe("generic:/Users/me/scratch");
    // The same dir with/without a trailing slash mints the SAME id (AC-5 dedupe).
    expect(genericWorkspaceId("/Users/me/scratch/")).toBe(genericWorkspaceId("/Users/me/scratch"));
  });

  it("opens a generic workspace rooted at the chosen dir; first tab spawns at that cwd (AC-2/AC-3)", () => {
    useViewerStore.getState().openOrFocusGenericWorkspace(TEAM, "/Users/me/scratch");
    const ws = live().find((w) => w.kind === "generic");
    expect(ws).toMatchObject({
      id: "generic:/Users/me/scratch",
      kind: "generic",
      cwd: "/Users/me/scratch",
      status: "ready",
    });
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("generic:/Users/me/scratch");
    expect(tabs("generic:/Users/me/scratch")).toHaveLength(1);
  });

  // SW-GW-8 (AC-1): a nav-opened generic dir folds its session into the trusted
  // registry under its `cwd` handle (the dashboard `dirHandle`) with "codesense"
  // provenance — so it shows in the Background agents panel like a nav-opened story.
  it("SW-GW-8 AC-1: registers the generic session under its cwd handle with codesense provenance", () => {
    const registerMock = dashboardRegisterSession as unknown as ReturnType<typeof vi.fn>;
    registerMock.mockClear();
    useViewerStore.getState().openOrFocusGenericWorkspace(TEAM, "/Users/me/scratch");
    const sessionId = tabs("generic:/Users/me/scratch")[0]?.sessionId;
    expect(sessionId).toBeTruthy();
    // The team it was opened in rides along, so the session's inbox stays in that team.
    expect(registerMock).toHaveBeenCalledWith("/Users/me/scratch", sessionId, "codesense", TEAM);
  });

  it("re-opening the same dir (even with a trailing slash) focuses, never duplicates (AC-5)", () => {
    const s = useViewerStore.getState();
    s.openOrFocusGenericWorkspace(TEAM, "/Users/me/scratch");
    s.openOrFocusGenericWorkspace(TEAM, "/Users/me/scratch/");
    expect(live().filter((w) => w.kind === "generic")).toHaveLength(1);
  });

  it("an empty dir is a quiet no-op (guards against a cancelled/empty pick)", () => {
    useViewerStore.getState().openOrFocusGenericWorkspace(TEAM, "");
    expect(live()).toHaveLength(0);
  });
});

describe("clearSyncOverlay (SLW-CO-7 — one-shot latch)", () => {
  it("drops the needsSyncOverlay latch for a workspace", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: WID, kind: "story", cwd: "/a", status: "ready", needsSyncOverlay: true }],
      },
    });
    useViewerStore.getState().clearSyncOverlay(TEAM, WID);
    expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBeUndefined();
  });

  it("is a no-op when the latch is already unset (referentially stable)", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WID, kind: "story", cwd: "/a", status: "ready" }] },
    });
    const before = useViewerStore.getState().workspaces;
    useViewerStore.getState().clearSyncOverlay(TEAM, WID);
    expect(useViewerStore.getState().workspaces).toBe(before);
  });
});

// SW-GW-2: the addressable "+ new tab" store primitive — mint a tab in an open
// workspace and return its sessionId (the handle `addTerminalTab`'s `void` return
// withholds), optionally role-tagging an agent tab.
describe("openTabInWorkspace (SW-GW-2)", () => {
  it("adds a tab to an open workspace and returns its sessionId", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WID, kind: "generic", cwd: "/proj", status: "ready" }] },
    });
    const sessionId = useViewerStore.getState().openTabInWorkspace(TEAM, WID);
    expect(sessionId).toMatch(/^shell-/);
    expect(wsTabs(TEAM, WID).some((t) => t.sessionId === sessionId)).toBe(true);
    // The returned tab is now the active tab of that workspace.
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM, WID)).toBe(sessionId);
  });

  it("tags an agent tab with the supplied role", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WID, kind: "generic", cwd: "/proj", status: "ready" }] },
    });
    const sessionId = useViewerStore
      .getState()
      .openTabInWorkspace(TEAM, WID, { role: "generic-agent" });
    expect(wsTabs(TEAM, WID).find((t) => t.sessionId === sessionId)?.role).toBe("generic-agent");
  });

  it("names the minted tab from the supplied name, else defaults to Terminal <n> (SW-WA-F6 AC-1)", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WID, kind: "generic", cwd: "/proj", status: "ready" }] },
    });
    // A supplied name becomes the tab's display name.
    const named = useViewerStore.getState().openTabInWorkspace(TEAM, WID, { name: "build watch" });
    expect(wsTabs(TEAM, WID).find((t) => t.sessionId === named)?.name).toBe("build watch");

    // Omitting the name keeps the default Terminal <n> (never blank).
    const unnamed = useViewerStore.getState().openTabInWorkspace(TEAM, WID);
    expect(wsTabs(TEAM, WID).find((t) => t.sessionId === unnamed)?.name).toMatch(/^Terminal \d+$/);
  });

  it("returns '' and mints NO tab when the workspace is not open (no orphan)", () => {
    useViewerStore.setState({ workspaces: { [TEAM]: [] } });
    const sessionId = useViewerStore.getState().openTabInWorkspace(TEAM, "generic:/gone");
    expect(sessionId).toBe("");
    expect(wsTabs(TEAM, "generic:/gone")).toHaveLength(0);
    expect(terminalHost().opened).toHaveLength(0);
  });

  it("successive tabs are distinct sessions", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WID, kind: "generic", cwd: "/proj", status: "ready" }] },
    });
    const a = useViewerStore.getState().openTabInWorkspace(TEAM, WID);
    const b = useViewerStore.getState().openTabInWorkspace(TEAM, WID);
    expect(a).not.toBe(b);
    expect(wsTabs(TEAM, WID).length).toBe(2);
  });
});

// SW-GW-2 code-review round 1: the real verb deps read the store — verify the
// cross-team (F4) and soft-close (F3) resolution the pure-core tests can't cover.
describe("workspace-tab-verbs real deps over the store (SW-GW-2 review)", () => {
  const OTHER = "other-org:default";

  it("F4: openTab resolves a generic workspace opened in a NON-active team", () => {
    // The workspace lives under OTHER, not TEAM — global resolution must still find it.
    useViewerStore.setState({
      workspaces: {
        [OTHER]: [{ id: "generic:/proj", kind: "generic", cwd: "/proj", status: "ready" }],
      },
    });
    const deps = makeRealOpenWorkspaceTabDeps();
    expect(deps.workspaceKindOf("generic:/proj")).toBe("generic");
    const sessionId = deps.openTab("generic:/proj");
    expect(sessionId).toMatch(/^shell-/);
    expect(wsTabs(OTHER, "generic:/proj").some((t) => t.sessionId === sessionId)).toBe(true);
  });

  it("r3-Major: closeTab rolls back the tab from the team it LIVES in, regardless of active team", async () => {
    // The tab was minted under OTHER; the active team is now TEAM (a mid-launch
    // switch). Rollback must still remove it from OTHER (not re-resolve by active
    // team, which would silently no-op and strand the orphan).
    useOrgStore.setState({ activeOrgSlug: "test-org", activeTeamName: "default" }); // active = TEAM
    useViewerStore.setState({
      workspaces: { [OTHER]: [{ id: "generic:/x", kind: "generic", cwd: "/x", status: "ready" }] },
    });
    seedTerminalTabs(OTHER, "generic:/x", [{ session: "shell-z", name: "T" }]);
    makeRealOpenWorkspaceTabDeps().closeTab("generic:/x", "shell-z");
    await new Promise((r) => setTimeout(r));
    expect(wsTabs(OTHER, "generic:/x").some((t) => t.sessionId === "shell-z")).toBe(false);
    expect(terminalHost().contains("shell-z")).toBe(false);
  });

  it("r4-Minor: closeTab also removes a tab that landed in a soft-close SHADOW mid-launch", async () => {
    // A concurrent displace/×-close during the awaited launch can move the tab into
    // a shadow; the rollback must reach it there too (dual scan, no stranded orphan).
    useViewerStore.setState({
      softClosedWorkspaces: {
        [OTHER]: {
          "generic:/x": {
            workspace: { id: "generic:/x", kind: "generic", cwd: "/x", status: "ready" },
          },
        },
      },
    });
    seedTerminalTabs(OTHER, "generic:/x", [
      { session: "shell-s", name: "T", role: GENERIC_AGENT_ROLE },
    ]);
    makeRealOpenWorkspaceTabDeps().closeTab("generic:/x", "shell-s");
    await new Promise((r) => setTimeout(r));
    expect(terminalHost().contains("shell-s")).toBe(false);
  });

  it("F4: workspaceKindOf returns null for an unknown workspace id", () => {
    useViewerStore.setState({ workspaces: {} });
    expect(makeRealOpenWorkspaceTabDeps().workspaceKindOf("generic:/gone")).toBeNull();
  });

  it("M1: when two teams hold the SAME generic workspaceId, openTab prefers the ACTIVE team", () => {
    // A generic id (`generic:<path>`) isn't team-namespaced — resolve to the
    // operator's active team, not whichever was inserted first.
    useOrgStore.setState({ activeOrgSlug: "test-org", activeTeamName: "default" }); // → TEAM
    useViewerStore.setState({
      workspaces: {
        [OTHER]: [{ id: "generic:/dup", kind: "generic", cwd: "/dup", status: "ready" }],
        [TEAM]: [{ id: "generic:/dup", kind: "generic", cwd: "/dup", status: "ready" }],
      },
    });
    const sessionId = makeRealOpenWorkspaceTabDeps().openTab("generic:/dup");
    expect(sessionId).toMatch(/^shell-/);
    // The tab landed under the ACTIVE team (TEAM), not OTHER.
    expect(wsTabs(TEAM, "generic:/dup")).toHaveLength(1);
    expect(wsTabs(OTHER, "generic:/dup")).toHaveLength(0);
  });

  it("F3: sendToWorkspaceTab tabInfo reaches a SOFT-CLOSED (keep-running) generic tab", () => {
    // A ×-closed generic tile keeps a live PTY in the soft-close shadow; its
    // sessionId must remain addressable (kind from the shadow's own record).
    useViewerStore.setState({
      workspaces: {},
      softClosedWorkspaces: {
        [TEAM]: {
          [WID]: { workspace: { id: WID, kind: "generic", cwd: "/proj", status: "ready" } },
        },
      },
    });
    seedTerminalTabs(TEAM, WID, [
      { session: "shell-a", name: "Terminal 1", role: GENERIC_AGENT_ROLE },
    ]);
    const info = makeRealSendToWorkspaceTabDeps().tabInfo("shell-a");
    expect(info).toEqual({ kind: "generic", isAgentTab: true });
  });

  it("F3: tabInfo returns null for a session that is nowhere (live or shadow)", () => {
    useViewerStore.setState({ softClosedWorkspaces: {} });
    expect(makeRealSendToWorkspaceTabDeps().tabInfo("shell-nope")).toBeNull();
  });
});
