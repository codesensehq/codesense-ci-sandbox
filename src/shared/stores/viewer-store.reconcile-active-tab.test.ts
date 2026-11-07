// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

import { useViewerStore, reconcileActiveTabs } from "./viewer-store";
import { useOrgStore } from "./org-store";
import * as commands from "@/tauri/commands";

// SLW-FX-12: the viewer body renders off `activeTab[tk]` while the title-bar
// tab strip gates off `openTabs[tk]`. If those two team-keyed slices diverge
// (an `activeTab[tk]` with no backing `openTabs[tk]` member), the body shows a
// doc but the strip shows no tab — the "file open, no tab" cold-launch symptom.
// These tests pin the invariant `activeTab[tk]` MUST reference a member of
// `openTabs[tk]`, enforced at the writer (setActiveTab), on persist, and on
// restore. An unbacked REAL path is healed by RECONSTRUCTING its tab (keeping
// the user's doc + tab); a virtual/empty pointer is dropped.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
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
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
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

const TEAM = "acme:default";
const REAL = "/team/real.md";

async function persistAndRead(): Promise<Record<string, unknown>> {
  let written = "";
  saveAppStateMock.mockImplementation(async (payload: string) => {
    written = payload;
  });
  useViewerStore.getState().persistState();
  await new Promise((r) => setTimeout(r, 400));
  return JSON.parse(written).viewer;
}

beforeEach(() => {
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
  loadAppStateMock.mockReset().mockResolvedValue("{}");
  // Keep restore's readiness waits instant (no active org → no wait).
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null, workspaceRepos: {} });
  useViewerStore.setState({ openTabs: {}, activeTab: {} });
});

describe("SLW-FX-12 reconcileActiveTabs helper @P1", () => {
  it("keeps a backed activeTab untouched (Object.is-stable)", () => {
    const openTabs = { [TEAM]: [{ path: REAL, name: "real" }] };
    const activeTab = { [TEAM]: REAL };
    const out = reconcileActiveTabs(openTabs, activeTab);
    expect(out.openTabs).toBe(openTabs);
    expect(out.activeTab).toBe(activeTab);
  });

  it("reconstructs a tab for an unbacked REAL activeTab (keeps doc + tab)", () => {
    const out = reconcileActiveTabs({ [TEAM]: [] }, { [TEAM]: REAL });
    expect(out.activeTab[TEAM]).toBe(REAL);
    expect(out.openTabs[TEAM]).toEqual([{ path: REAL, name: "real" }]);
  });

  it("reconstructs into an absent bucket (no openTabs key at all)", () => {
    const out = reconcileActiveTabs({}, { [TEAM]: REAL });
    expect(out.openTabs[TEAM]?.map((t) => t.path)).toEqual([REAL]);
    expect(out.activeTab[TEAM]).toBe(REAL);
  });

  it("drops a virtual unbacked activeTab (no reconstruction)", () => {
    const out = reconcileActiveTabs({ [TEAM]: [] }, { [TEAM]: "virtual://notes" });
    expect(out.activeTab[TEAM]).toBeUndefined();
    expect(out.openTabs[TEAM] ?? []).toEqual([]);
  });
});

describe("SLW-FX-12 persist can't orphan @P1", () => {
  // AC-1: real activeTab + empty bucket → the written blob is self-consistent
  // (openTabs contains the active path), never a bare activeTab.
  it("reconstructs a real unbacked activeTab on persist (AC-1)", async () => {
    useViewerStore.setState({ openTabs: { [TEAM]: [] }, activeTab: { [TEAM]: REAL } });
    const saved = await persistAndRead();
    const openTabs = saved.openTabs as Record<string, Array<{ path: string }>>;
    const activeTab = saved.activeTab as Record<string, string>;
    expect(openTabs[TEAM]?.some((t) => t.path === REAL)).toBe(true);
    expect(activeTab[TEAM]).toBe(REAL);
  });

  // AC-2: the in-sync case is untouched.
  it("leaves an in-sync activeTab/openTabs pair unchanged (AC-2)", async () => {
    useViewerStore.setState({
      openTabs: { [TEAM]: [{ path: REAL, name: "real" }] },
      activeTab: { [TEAM]: REAL },
    });
    const saved = await persistAndRead();
    const openTabs = saved.openTabs as Record<string, Array<{ path: string }>>;
    const activeTab = saved.activeTab as Record<string, string>;
    expect(openTabs[TEAM]).toEqual([{ path: REAL, name: "real" }]);
    expect(activeTab[TEAM]).toBe(REAL);
  });
});

describe("SLW-FX-12 restore heals into the good state @P1", () => {
  // AC-4: a persisted orphan (activeTab set, openTabs empty/absent) restores as
  // doc + tab — recovered, not discarded.
  it("reconstructs the tab for an orphaned blob on load (AC-4)", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 9,
          openTabs: {},
          activeTab: { [TEAM]: REAL },
          openedTeams: [TEAM],
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]?.some((t) => t.path === REAL)).toBe(true);
    expect(s.activeTab[TEAM]).toBe(REAL);
  });

  // AC-5: an in-sync blob restores both slices intact.
  it("restores an in-sync blob unchanged (AC-5)", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 9,
          openTabs: { [TEAM]: [{ path: REAL, name: "real" }] },
          activeTab: { [TEAM]: REAL },
          openedTeams: [TEAM],
        },
      }),
    );
    await useViewerStore.getState().loadPersistedState();
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]).toEqual([{ path: REAL, name: "real" }]);
    expect(s.activeTab[TEAM]).toBe(REAL);
  });
});

describe("SLW-FX-12 setActiveTab ensures the tab @P1", () => {
  // AC-6: activating a not-yet-open path adds the tab so activeTab can't orphan.
  it("adds a tab when the target isn't open, and activates it (AC-6)", () => {
    useViewerStore.setState({ openTabs: { [TEAM]: [{ path: "/a.md", name: "a" }] } });
    useViewerStore.getState().setActiveTab(TEAM, null, "/not-open.md");
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]?.some((t) => t.path === "/not-open.md")).toBe(true);
    expect(s.activeTab[TEAM]).toBe("/not-open.md");
  });

  it("just activates when the target is already open (no duplicate) (AC-6)", () => {
    useViewerStore.setState({
      openTabs: {
        [TEAM]: [
          { path: "/a.md", name: "a" },
          { path: REAL, name: "real" },
        ],
      },
    });
    useViewerStore.getState().setActiveTab(TEAM, null, REAL);
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]).toHaveLength(2);
    expect(s.activeTab[TEAM]).toBe(REAL);
  });

  // AC-6b: enter-team resume shape — openTeam then setActiveTab for a file not
  // yet in openTabs must land on the file WITH a tab (SW-HB-2 AC-9).
  it("resume-to-file lands on the file with a tab (enter-team shape) (AC-6b)", () => {
    const viewer = useViewerStore.getState();
    viewer.openTeam(TEAM);
    viewer.setActiveTab(TEAM, null, REAL);
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]?.some((t) => t.path === REAL)).toBe(true);
    expect(s.activeTab[TEAM]).toBe(REAL);
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
