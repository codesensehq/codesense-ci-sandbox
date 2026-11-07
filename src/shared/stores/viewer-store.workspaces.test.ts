// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  useViewerStore,
  defaultSourceDiffInline,
  resolveSourceDiffOption,
  selectActiveTerminalTab,
  selectMinimizedTerminalTabIndex,
  selectWorkspaceTabs,
} from "./viewer-store";
import type { Workspace } from "./viewer-store";
import { teamWorkspaceOrder } from "@/features/viewer/lib/workspace-order";
import { useOrgStore } from "./org-store";
import { resetPrunedAgentSessionsForTests, useAgentStateStore } from "./agent-state-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";
import type { RepoState } from "./config-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

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
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

// T.1: tests treat `repo` as a teamKey value (the bucket key for the
// re-keyed workspace + terminal slices). The legacy `selectRepo` action
// internally derives teamKey from `useOrgStore.{activeOrgSlug,activeTeamName}`
// — `resetState` configures org-store so the derived teamKey matches
// this `repo` constant.
const repo = "test-org:default";

installTerminalLayoutHarness();

const settle = () => new Promise((r) => setTimeout(r));
const tabIds = (teamKey: string, wsId: string) =>
  selectWorkspaceTabs(useViewerStore.getState(), teamKey, wsId).map((t) => t.id);

// T.2: tests seed a single source-repo per team so `ensureRootWorkspaces`
// has a target to mint. `ROOT_ID` is the matching per-source-repo root id
// (replaces T.1's literal `ROOT_ID = "root"`).
const TEST_SLUG = "test-org/test-repo";
const SYNTHETIC_KEY = `workspace:test-org:default:${TEST_SLUG}`;
const ROOT_ID = buildRootWorkspaceId(TEST_SLUG);

/** Build a minimal RepoState fixture for the synthetic repo. Set
 *  `localPath` non-empty to land the root in `status: "ready"`; omit it
 *  (or pass `""`) to land in `"creating"`. */
function seedRepoState(localPath?: string): RepoState {
  return {
    repoPath: SYNTHETIC_KEY,
    localPath: localPath ?? "",
    // The remaining RepoState fields aren't consumed by ensureRootWorkspaces;
    // cast through unknown for fixture brevity.
  } as unknown as RepoState;
}

function resetState(localPath?: string) {
  useOrgStore.setState({
    activeOrgSlug: "test-org",
    activeTeamName: "default",
    workspaceRepos: { [SYNTHETIC_KEY]: seedRepoState(localPath) },
  });
  useViewerStore.setState({
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
  });
}

const storyWorkspace = (id: string, cwd: string): Workspace => ({
  id,
  kind: "story",
  cwd,
  storyFilePath: `/repo/stories/${id}.md`,
  storyId: id,
  status: "ready",
});

// SW-EW-1c: `ensureRootWorkspaces` is now a no-op (root workspaces retired),
// so the per-source-repo root minting it used to perform no longer happens.
// The remaining store invariants that still treat a `kind:"root"` workspace
// specially (the close guard, the reorder root-anchor guard, jump/cycle
// fallbacks, per-workspace terminal slices) are still live for transitional
// migrated roots. These tests exercise those invariants by seeding the roots
// directly via `setState` instead of relying on the retired mint path.
function seedRoots(
  teamKey: string,
  roots: Array<{ slug: string; cwd?: string }>,
  setActiveToFirst = true,
) {
  const wsList: Workspace[] = roots.map(({ slug, cwd }) => ({
    id: buildRootWorkspaceId(slug),
    kind: "root",
    cwd: cwd ?? "",
    status: cwd ? "ready" : "creating",
    sourceRepoSlug: slug,
    sourceRepoPath: cwd || undefined,
  }));
  useViewerStore.setState((s) => ({
    workspaces: { ...s.workspaces, [teamKey]: wsList },
    activeWorkspace:
      setActiveToFirst && wsList.length > 0
        ? { ...s.activeWorkspace, [teamKey]: wsList[0].id }
        : s.activeWorkspace,
  }));
}

// SW-WV-A6: the "ensureRootWorkspaces — retired no-op" describe block was
// deleted here. The no-op action (SW-EW-1c) and its call sites are gone; the
// always-a-CLI guarantee is now owned by `openOrFocusTeamFolderWorkspace`
// (see viewer-store.team-folder-workspace.test.ts).

describe("closeWorkspace — bypassRootGuard", () => {
  beforeEach(() => resetState());

  it("closes root without bypassRootGuard now (root is user-closable — SW-GS-2a)", () => {
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    const rootId = useViewerStore.getState().workspaces[repo][0].id;
    useViewerStore.getState().closeWorkspace(repo, rootId);
    expect(useViewerStore.getState().workspaces[repo]).toHaveLength(0);
  });

  it("removes the root when called with bypassRootGuard", () => {
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    const rootId = useViewerStore.getState().workspaces[repo][0].id;
    useViewerStore.getState().closeWorkspace(repo, rootId, { bypassRootGuard: true });
    expect(useViewerStore.getState().workspaces[repo]).toHaveLength(0);
  });

  it("ends the removed root's terminals in their host", () => {
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    const rootId = useViewerStore.getState().workspaces[repo][0].id;
    seedTerminalTabs(repo, rootId, ["sess-x"]);
    useViewerStore.getState().closeWorkspace(repo, rootId, { bypassRootGuard: true });
    expect(terminalHost().contains("sess-x")).toBe(false);
  });
});

describe("jumpToRootWorkspace + activeWorkspace defaults — multi-root", () => {
  beforeEach(() => resetState());

  it("jumpToRootWorkspace lands on the first terminal workspace in array order", () => {
    // SW-EW-1c: jumpToRootWorkspace now resolves `firstRootIdForTeam` (first
    // non-epic workspace). Seed two roots directly (minting is retired).
    seedRoots(repo, [
      { slug: "owner/a", cwd: "/path/a" },
      { slug: "owner/b", cwd: "/path/b" },
    ]);
    // Move active to the second root, then jump back.
    useViewerStore.getState().switchWorkspace(repo, buildRootWorkspaceId("owner/b"));
    useViewerStore.getState().jumpToRootWorkspace(repo);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(buildRootWorkspaceId("owner/a"));
  });

  it("jumpToRootWorkspace jumps to the first workspace (epic now tiles — SW-GS-2a)", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          { id: "epic:e", kind: "epic", cwd: "", epicDirPath: "e", status: "ready" },
          { ...storyWorkspace("R-1", "/wt/R-1"), sourceRepoSlug: "owner/a" },
        ],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "R-1" },
    }));
    useViewerStore.getState().jumpToRootWorkspace(repo);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("epic:e");
  });
});

describe("workspaces — switching", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
  });

  it("switchWorkspace sets activeWorkspace to the target id", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
    }));

    useViewerStore.getState().switchWorkspace(repo, "30-10");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-10");
  });

  it("switchWorkspace is no-op for an unknown workspace id", () => {
    useViewerStore.getState().switchWorkspace(repo, "does-not-exist");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });

  it("jumpToRootWorkspace returns to root from any active workspace", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-10" },
    }));

    useViewerStore.getState().jumpToRootWorkspace(repo);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });
});

describe("OC-16 — nav commands report moved vs blocked (drives the visible cue)", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
  });

  it("jumpToRootWorkspace returns false when already at root, true otherwise", () => {
    // Seeding leaves ROOT_ID active → jumping to root is the already-there no-op.
    expect(useViewerStore.getState().jumpToRootWorkspace(repo)).toBe(false);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-10" },
    }));
    expect(useViewerStore.getState().jumpToRootWorkspace(repo)).toBe(true);
  });

  it("jumpToTrayIndex returns false out-of-range / already-active, true on a switch", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      trayOrderByTeam: { ...s.trayOrderByTeam, [repo]: [ROOT_ID, "30-10"] },
    }));
    expect(useViewerStore.getState().jumpToTrayIndex(repo, 5)).toBe(false); // out of range
    expect(useViewerStore.getState().jumpToTrayIndex(repo, 0)).toBe(false); // already active (ROOT_ID)
    expect(useViewerStore.getState().jumpToTrayIndex(repo, 1)).toBe(true); // → 30-10
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-10");
  });

  it("activateTrayWorkspace returns false for already-focused / unknown, true on a switch", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
    }));
    expect(useViewerStore.getState().activateTrayWorkspace(repo, ROOT_ID)).toBe(false); // already active
    expect(useViewerStore.getState().activateTrayWorkspace(repo, "ghost")).toBe(false); // unknown id
    expect(useViewerStore.getState().activateTrayWorkspace(repo, "30-10")).toBe(true);
  });

  it("cycleTerminalTab returns false at ≤1 tab, true when it moves", () => {
    // Active ROOT_ID starts with no tabs → ≤1 → blocked.
    expect(useViewerStore.getState().cycleTerminalTab(repo, "next")).toBe(false);
    seedTerminalTabs(repo, ROOT_ID, ["t1", "t2"], { active: "t1" });
    expect(useViewerStore.getState().cycleTerminalTab(repo, "next")).toBe(true);
    expect(selectActiveTerminalTab(useViewerStore.getState(), repo, ROOT_ID)).toBe("t2");
  });

  it("flipLastWorkspace returns false with no distinct prior, true on a flip", () => {
    // Only root exists and is active → no distinct prior workspace to flip to.
    expect(useViewerStore.getState().flipLastWorkspace(repo)).toBe(false);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-10" },
      mruWorkspaceStack: { ...s.mruWorkspaceStack, [repo]: [ROOT_ID, "30-10"] },
    }));
    expect(useViewerStore.getState().flipLastWorkspace(repo)).toBe(true); // → ROOT_ID
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });
});

describe("workspaces — cycle (flat mode: visible tray order)", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          storyWorkspace("30-10", "/wt/30-10"),
          storyWorkspace("30-9", "/wt/30-9"),
        ],
      },
      // OC-7: cycle traverses the published visible order, not the raw workspaces
      // array — seed it explicitly (flat mode = whole order; trayMode defaults flat).
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "30-10", "30-9"] },
    }));
  });

  it("cycleWorkspace next from root goes to 30-10", () => {
    // OC-16: a real move reports `true` (drives the blocked-nav cue at the seam).
    expect(useViewerStore.getState().cycleWorkspace(repo, "next")).toBe(true);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-10");
  });

  it("cycleWorkspace next wraps from last back to root", () => {
    useViewerStore.setState((s) => ({
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-9" },
    }));
    useViewerStore.getState().cycleWorkspace(repo, "next");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });

  it("cycleWorkspace prev from root wraps to last", () => {
    useViewerStore.getState().cycleWorkspace(repo, "prev");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-9");
  });

  it("cycleWorkspace is a no-op when only root exists", () => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID] },
    }));
    // OC-16: the ≤1-scope no-op reports `false` so the caller can flash the cue.
    expect(useViewerStore.getState().cycleWorkspace(repo, "next")).toBe(false);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });

  it("cycles onto a MINIMIZED member and restores it (activateTrayWorkspace path, not switchWorkspace no-op)", () => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    // "30-10" is on-screen; "30-9" is minimized (soft-closed) but still a visible
    // tray chip in trayFullOrderByTeam. The OLD switchWorkspace path would no-op on
    // it (not in `workspaces`); activateTrayWorkspace must restore it.
    const minimized = {
      id: "30-9",
      kind: "story" as const,
      cwd: "/wt/30-9",
      storyId: "30-9",
      status: "ready" as const,
      sourceRepoSlug: TEST_SLUG,
    };
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "30-10", "30-9"] },
      softClosedWorkspaces: {
        ...s.softClosedWorkspaces,
        [repo]: { "30-9": { workspace: minimized } },
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-10" },
    }));

    useViewerStore.getState().cycleWorkspace(repo, "next"); // 30-10 → 30-9 (minimized)

    // Restored: no longer soft-closed, now the active workspace.
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-9"]).toBeUndefined();
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-9");
  });

  it("falls back to the raw workspace order when trayFullOrderByTeam is unpublished", () => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      // trayFullOrderByTeam deliberately left empty (tray not yet published).
    }));
    useViewerStore.getState().cycleWorkspace(repo, "next");
    // Must still move (regression guard: reading only trayFullOrderByTeam would no-op).
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-10");
  });

  // OC-7 AC7: rapid repeats stay deterministic (ordering + wrap never desync).
  // The final focus race is the OC-5 coordinator's generation fence, inherited by
  // this path via the registry run()'s focusActiveWorkspaceTerminal; here we pin
  // the store-level determinism the fence sits on top of.
  const FOCUS_RACE_BURN_IN = 25;

  it("rapid next×N walks deterministically (order length 3 → active = order[N % 3])", () => {
    const order = [ROOT_ID, "30-10", "30-9"];
    for (let i = 1; i <= FOCUS_RACE_BURN_IN; i++) {
      useViewerStore.getState().cycleWorkspace(repo, "next");
      expect(useViewerStore.getState().activeWorkspace[repo]).toBe(order[i % order.length]);
    }
  });

  it("N nexts then N prevs returns to the start (no drift)", () => {
    const start = useViewerStore.getState().activeWorkspace[repo];
    for (let i = 0; i < FOCUS_RACE_BURN_IN; i++)
      useViewerStore.getState().cycleWorkspace(repo, "next");
    for (let i = 0; i < FOCUS_RACE_BURN_IN; i++)
      useViewerStore.getState().cycleWorkspace(repo, "prev");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(start);
  });
});

describe("workspaces — cycle (grouped mode: stays within the selected rail)", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          storyWorkspace("a1", "/wt/a1"),
          storyWorkspace("a2", "/wt/a2"),
          storyWorkspace("b1", "/wt/b1"),
        ],
      },
      trayMode: { ...s.trayMode, [repo]: "grouped" },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "a1", "a2", "b1"] },
      railKeyByWorkspaceId: {
        ...s.railKeyByWorkspaceId,
        [repo]: { [ROOT_ID]: " roots", a1: "epicA", a2: "epicA", b1: "epicB" },
      },
      selectedEpicDirName: { ...s.selectedEpicDirName, [repo]: "epicA" },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "a1" },
    }));
  });

  it("next stays within the selected rail (a1 → a2, not the other rail's b1)", () => {
    useViewerStore.getState().cycleWorkspace(repo, "next");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("a2");
  });

  it("wraps within the selected rail (a2 → a1)", () => {
    useViewerStore.setState((s) => ({ activeWorkspace: { ...s.activeWorkspace, [repo]: "a2" } }));
    useViewerStore.getState().cycleWorkspace(repo, "next");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("a1");
  });

  it("never crosses into another rail even when it holds members", () => {
    // From either end of epicA, next/prev must never land on epicB's b1.
    useViewerStore.getState().cycleWorkspace(repo, "prev");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("a2"); // prev from a1 wraps to a2
    expect(useViewerStore.getState().activeWorkspace[repo]).not.toBe("b1");
  });

  it("a single-member selected rail is a no-op", () => {
    useViewerStore.setState((s) => ({
      selectedEpicDirName: { ...s.selectedEpicDirName, [repo]: "epicB" },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "b1" },
    }));
    useViewerStore.getState().cycleWorkspace(repo, "next");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("b1");
  });
});

describe("workspaces — switchWorkspace does not end terminals", () => {
  const alive = () => ["sess-root", "sess-story"].every((id) => terminalHost().contains(id));

  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
    }));
    seedTerminalTabs(repo, ROOT_ID, ["sess-root"]);
    seedTerminalTabs(repo, "30-10", ["sess-story"]);
  });

  it("switchWorkspace keeps every terminal running", () => {
    useViewerStore.getState().switchWorkspace(repo, "30-10");
    expect(alive()).toBe(true);
  });

  it("rapid switching (A→B→A→B) keeps every terminal running", () => {
    useViewerStore.getState().switchWorkspace(repo, "30-10");
    useViewerStore.getState().switchWorkspace(repo, ROOT_ID);
    useViewerStore.getState().switchWorkspace(repo, "30-10");
    useViewerStore.getState().switchWorkspace(repo, ROOT_ID);
    expect(alive()).toBe(true);
    expect(tabIds(repo, "30-10")).toEqual(["sess-story"]);
  });
});

describe("workspaces — close", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "30-10" },
    }));
  });

  it("closeWorkspace removes the story workspace and falls back to root (no order published)", () => {
    // No trayFullOrder published (headless) → falls back to firstRoot. SW-CW-4
    // non-breaking path (adjacency only kicks in once the tray publishes an order).
    useViewerStore.getState().closeWorkspace(repo, "30-10");

    expect(useViewerStore.getState().workspaces[repo]).toHaveLength(1);
    expect(useViewerStore.getState().workspaces[repo][0].id).toBe(ROOT_ID);
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_ID);
  });

  it("SW-CW-4: closing the active tab selects the tab to its RIGHT (spatially stable)", () => {
    // [ROOT, A, B, C] all on-screen; close active B → C (slides into B's slot), not firstRoot.
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          storyWorkspace("A", "/wt/A"),
          storyWorkspace("B", "/wt/B"),
          storyWorkspace("C", "/wt/C"),
        ],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "B" },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "A", "B", "C"] },
    }));

    useViewerStore.getState().closeWorkspace(repo, "B");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("C");
  });

  it("SW-CW-4: closing a MIDDLE epic's last tab crosses RIGHT into the next epic", () => {
    // Strip [ROOT, eE, eF, eG]; close active eF (its epic's only tab, mid-strip) → eG (next epic).
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          storyWorkspace("eE", "/wt/E"),
          storyWorkspace("eF", "/wt/F"),
          storyWorkspace("eG", "/wt/G"),
        ],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "eF" },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "eE", "eF", "eG"] },
    }));

    useViewerStore.getState().closeWorkspace(repo, "eF");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("eG");
  });

  it("SW-CW-4: closing the RIGHTMOST tab of the strip falls left to the previous epic, not firstRoot", () => {
    // Full strip [ROOT, epicE-child, epicF-child]; close the rightmost (epicF-child,
    // the epic's only tab) → epicE-child to its left (no right neighbor), NOT the first-root jump.
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          storyWorkspace("epicE-child", "/wt/E"),
          storyWorkspace("epicF-child", "/wt/F"),
        ],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "epicF-child" },
      trayFullOrderByTeam: {
        ...s.trayFullOrderByTeam,
        [repo]: [ROOT_ID, "epicE-child", "epicF-child"],
      },
    }));

    useViewerStore.getState().closeWorkspace(repo, "epicF-child");
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("epicE-child");
  });

  it("SW-CW-4: never selects a STALE id in the published order (guards a dangling active id)", () => {
    // trayFullOrderByTeam still lists "gone" (a workspace closed via another path
    // before the tray re-published). Closing C must skip it and land on a live
    // survivor (A), never point activeWorkspace at the non-existent "gone".
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("A", "/wt/A"), storyWorkspace("C", "/wt/C")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "C" },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "A", "gone", "C"] },
    }));

    useViewerStore.getState().closeWorkspace(repo, "C");
    const active = useViewerStore.getState().activeWorkspace[repo];
    expect(active).toBe("A");
    expect(useViewerStore.getState().workspaces[repo].some((w) => w.id === active)).toBe(true);
  });

  it("SW-CW-4: closing a NON-active tab leaves the selection unchanged (AC-3)", () => {
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("A", "/wt/A"), storyWorkspace("B", "/wt/B")],
      },
      activeWorkspace: { ...s.activeWorkspace, [repo]: "B" },
      trayFullOrderByTeam: { ...s.trayFullOrderByTeam, [repo]: [ROOT_ID, "A", "B"] },
    }));

    useViewerStore.getState().closeWorkspace(repo, "A"); // close the non-active tab
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("B");
  });

  it("closeWorkspace closes root now (root is user-closable — SW-GS-2a)", () => {
    useViewerStore.getState().closeWorkspace(repo, ROOT_ID);
    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === ROOT_ID),
    ).toBeUndefined();
  });

  it("PT-10: closeWorkspace keeps the terminals running and minimizes the workspace (overrides SW-EW-1 AC-8)", () => {
    seedTerminalTabs(repo, "30-10", ["sess-a", "sess-b"]);

    useViewerStore.getState().closeWorkspace(repo, "30-10");

    // Nothing is ended — the panels detach (PT-1 unsubscribe) on unmount.
    expect(["sess-a", "sess-b"].every((id) => terminalHost().contains(id))).toBe(true);
    // The minimized workspace still reads its tabs from the host for reopen.
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeDefined();
    expect(
      selectMinimizedTerminalTabIndex(useViewerStore.getState())[repo]["30-10"].map((t) => t.id),
    ).toEqual(["sess-a", "sess-b"]);
  });

  it("closeWorkspace of a workspace with no terminals leaves no minimized shadow", () => {
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();
  });

  it("closeWorkspace cleans up the workspace's per-tile state", () => {
    useViewerStore.setState(() => ({
      // SW-EW-28: per-tile HUD state must be pruned on close (deterministic story
      // ids are reused, so a reopened tile must not inherit a stale shown/expanded).
      storyHudShown: { "30-10": true },
      storyHudExpanded: { "30-10": true },
      // SLW-WD-2/WD-6: per-tile source-diff state must also be pruned on close.
      sourceDiffShown: { "30-10": true },
      sourceDiffSelectedFile: { "30-10": "src/a.ts" },
      // SLW-DV-7: the comparison basis is session-memory (survives a hide) but
      // must be cleared on close so a reopened tile starts at branch-base.
      sourceDiffBaseMode: { "30-10": "head" },
      // RF-30 (AC-3): seed EVERY consolidated view option so the single-record
      // teardown is proven to prune the whole set, not just one key.
      sourceDiffViewOptions: {
        "30-10": {
          inline: true,
          wordWrap: true,
          expandAll: true,
          autoFollow: false,
        },
      },
      worktreeEditorOpen: { "30-10": true },
      worktreeEditorFile: { "30-10": "src/a.ts" },
      // SLW-DV-9: a preserved unsaved-edit draft must be pruned on close too, so a
      // reopened (reused) story id can't inherit a phantom dirty buffer.
      worktreeEditorDraft: {
        "30-10": { "src/a.ts": { relPath: "src/a.ts", buffer: "x", lastSaved: "y" } },
      },
      worktreeEditorTabs: { "30-10": ["src/a.ts"] },
    }));

    useViewerStore.getState().closeWorkspace(repo, "30-10");

    expect("30-10" in useViewerStore.getState().storyHudShown).toBe(false);
    expect("30-10" in useViewerStore.getState().storyHudExpanded).toBe(false);
    // Per-tab slices key on sessionId, so a workspace close drops the entries
    // for the workspace's tabs (not the wsId). Here the workspace was closed
    // with no live tabs, so visibility maps are empty.
    expect(useViewerStore.getState().sourceDiffShownBySession).toEqual({});
    expect(useViewerStore.getState().worktreeEditorOpenBySession).toEqual({});
    // SW-TP CAP-3: `sourceDiffSelectedFile` / `sourceDiffViewOptions` /
    // `sourceDiffBaseMode` are now session-keyed too — they are pruned on tab
    // close (pty-exit), not workspace close. The pre-seeded `"30-10"` entries
    // are workspace-named test fixtures (there are no live sessions here),
    // and the soft-close reducer's sidsForWs set is empty, so the entries
    // stay. A sibling test exercises the tab-close prune path.
    expect("30-10" in useViewerStore.getState().sourceDiffSelectedFile).toBe(true);
    expect("30-10" in useViewerStore.getState().sourceDiffViewOptions).toBe(true);
    expect("30-10" in useViewerStore.getState().sourceDiffBaseMode).toBe(true);
    expect("30-10" in useViewerStore.getState().worktreeEditorFile).toBe(false);
    expect("30-10" in useViewerStore.getState().worktreeEditorDraft).toBe(false);
    expect("30-10" in useViewerStore.getState().worktreeEditorTabs).toBe(false);
  });
});

describe("tile overlays — mutually exclusive within a tab (one work-surface per tab)", () => {
  // The two per-tab overlays (source + editor) key on sessionId; the HUD stays
  // per-workspace. These tests seed state directly so the mutual-exclusion
  // logic can run without needing a live terminal projection.

  const workspaceId = "mx-ws";
  const sessionId = "mx-sid";
  const teamKey = "team-a";

  const flagHud = (id: string) => useViewerStore.getState().storyHudShown[id] ?? false;
  const flagSource = (sid: string) =>
    useViewerStore.getState().sourceDiffShownBySession[sid] ?? false;
  const flagEditor = (sid: string) =>
    useViewerStore.getState().worktreeEditorOpenBySession[sid] ?? false;

  beforeEach(() => {
    useViewerStore.setState({
      storyHudShown: {},
      storyHudExpanded: {},
      sourceDiffShownBySession: {},
      worktreeEditorOpenBySession: {},
    });
  });

  it("opening source diff on a tab closes any editor on the SAME tab", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { [sessionId]: true } });
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId, workspaceId, teamKey, shown: true });
    expect(flagSource(sessionId)).toBe(true);
    expect(flagEditor(sessionId)).toBe(false);
  });

  it("opening editor on a tab closes any source diff on the SAME tab", () => {
    useViewerStore.setState({ sourceDiffShownBySession: { [sessionId]: true } });
    useViewerStore
      .getState()
      .toggleWorktreeEditorOpen({ sessionId, workspaceId, teamKey, open: true });
    expect(flagEditor(sessionId)).toBe(true);
    expect(flagSource(sessionId)).toBe(false);
  });

  it("closing an overlay never reopens a sibling", () => {
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId, workspaceId, teamKey, shown: true });
    useViewerStore
      .getState()
      .toggleWorktreeEditorOpen({ sessionId, workspaceId, teamKey, open: true });
    expect(flagSource(sessionId)).toBe(false);
    useViewerStore
      .getState()
      .toggleWorktreeEditorOpen({ sessionId, workspaceId, teamKey, open: false });
    expect(flagSource(sessionId)).toBe(false);
    expect(flagEditor(sessionId)).toBe(false);
  });

  it("opening a sibling closes the editor but preserves its file + unsaved draft (DV-9)", () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { [sessionId]: true },
      worktreeEditorFile: { [workspaceId]: "src/a.ts" },
      worktreeEditorDraft: {
        [workspaceId]: { "src/a.ts": { relPath: "src/a.ts", buffer: "wip", lastSaved: "base" } },
      },
    });
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId, workspaceId, teamKey, shown: true });
    expect(flagEditor(sessionId)).toBe(false);
    expect(flagSource(sessionId)).toBe(true);
    expect(useViewerStore.getState().worktreeEditorFile[workspaceId]).toBe("src/a.ts");
    expect(useViewerStore.getState().worktreeEditorDraft[workspaceId]?.["src/a.ts"]).toMatchObject({
      buffer: "wip",
    });
  });

  it("opening a sibling closes the diff but preserves its file / mode / commit", () => {
    useViewerStore.setState({
      sourceDiffShownBySession: { [sessionId]: true },
      sourceDiffSelectedFile: { [workspaceId]: "src/a.ts" },
      sourceDiffHistoryMode: { [workspaceId]: true },
      sourceDiffSelectedSha: { [workspaceId]: "abc123" },
    });
    useViewerStore
      .getState()
      .toggleWorktreeEditorOpen({ sessionId, workspaceId, teamKey, open: true });
    expect(flagSource(sessionId)).toBe(false);
    expect(flagEditor(sessionId)).toBe(true);
    const s = useViewerStore.getState();
    expect(s.sourceDiffSelectedFile[workspaceId]).toBe("src/a.ts");
    expect(s.sourceDiffHistoryMode[workspaceId]).toBe(true);
    expect(s.sourceDiffSelectedSha[workspaceId]).toBe("abc123");
  });

  it("source views across different tabs are independent (no instance-wide cap)", () => {
    // DESIGN-per-tab-primitives.md §5 — the single-source slot was dropped;
    // the data-source seam dedupes same-key fetches instead.
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId: "sid-a", workspaceId: "ws-a", teamKey, shown: true });
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId: "sid-b", workspaceId: "ws-b", teamKey, shown: true });
    expect(flagSource("sid-a")).toBe(true);
    expect(flagSource("sid-b")).toBe(true);
  });

  it("switching tabs within a tile reveals each tab's own state", () => {
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId: "sid-α", workspaceId, teamKey, shown: true });
    useViewerStore
      .getState()
      .toggleWorktreeEditorOpen({ sessionId: "sid-β", workspaceId, teamKey, open: true });
    // Both tabs of the same workspace keep their own primitive state.
    expect(flagSource("sid-α")).toBe(true);
    expect(flagEditor("sid-α")).toBe(false);
    expect(flagSource("sid-β")).toBe(false);
    expect(flagEditor("sid-β")).toBe(true);
  });

  it("closing a HUD by exclusion resets it to compact", () => {
    useViewerStore.setState({
      storyHudShown: { [workspaceId]: true },
      storyHudExpanded: { [workspaceId]: true },
    });
    useViewerStore
      .getState()
      .toggleSourceDiffShown({ sessionId, workspaceId, teamKey, shown: true });
    expect(flagHud(workspaceId)).toBe(false);
    expect(useViewerStore.getState().storyHudExpanded[workspaceId]).toBe(false);
  });
});

describe("SLW-WD-2 — source-diff overlay actions", () => {
  beforeEach(() => {
    useViewerStore.setState({
      sourceDiffShownBySession: {},
      sourceDiffSelectedFile: {},
      sourceDiffViewOptions: {},
      // SLW-WD-7: pin the default-layout setting (inline) so the toggle reconcile
      // has a known effective default and per-test mutations don't leak.
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "inline" },
    });
  });

  it("toggleSourceDiffShown flips visibility per tab (change-only)", () => {
    const before = useViewerStore.getState();
    const opener = (shown?: boolean) =>
      useViewerStore
        .getState()
        .toggleSourceDiffShown({ sessionId: "sid1", workspaceId: "w1", teamKey: "team-a", shown });
    opener();
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(true);
    opener(true); // no-op (already true)
    // Session memory: hiding PRESERVES the selected file + Working/ History
    // mode + selected commit + the per-tile view options (workspace-scoped),
    // so a re-open resumes where the user was.
    useViewerStore.setState({
      sourceDiffSelectedFile: { w1: "src/a.ts" },
      sourceDiffHistoryMode: { w1: true },
      sourceDiffSelectedSha: { w1: "abc123" },
      sourceDiffViewOptions: { w1: { inline: false, wordWrap: true } },
    });
    opener(false);
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(false);
    expect(useViewerStore.getState().sourceDiffSelectedFile.w1).toBe("src/a.ts");
    expect(useViewerStore.getState().sourceDiffHistoryMode.w1).toBe(true);
    expect(useViewerStore.getState().sourceDiffSelectedSha.w1).toBe("abc123");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1).toEqual({
      inline: false,
      wordWrap: true,
    });
    // change-only guard: no state churn re-hiding an already-hidden tab.
    const stable = useViewerStore.getState();
    opener(false);
    expect(useViewerStore.getState()).toBe(stable);
    void before;
  });

  it("setSourceDiffSelectedFile sets + clears (change-only)", () => {
    useViewerStore.getState().setSourceDiffSelectedFile("w1", "src/a.ts");
    expect(useViewerStore.getState().sourceDiffSelectedFile.w1).toBe("src/a.ts");
    const stable = useViewerStore.getState();
    useViewerStore.getState().setSourceDiffSelectedFile("w1", "src/a.ts"); // no-op
    expect(useViewerStore.getState()).toBe(stable);
    useViewerStore.getState().setSourceDiffSelectedFile("w1", null);
    expect(useViewerStore.getState().sourceDiffSelectedFile.w1).toBeNull();
  });

  // SLW-WD-6/WD-7: inline ↔ split code-diff view mode, reconciled against the
  // configurable default (RD-3/AC-6).
  it("toggleSourceDiffOption('inline') reconciles the first toggle against the effective default", () => {
    // Default = inline → an unset tile's FIRST toggle flips to split (false),
    // NOT to a hardcoded `true`.
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.inline).toBeUndefined();
    useViewerStore.getState().toggleSourceDiffOption("w1", "inline");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.inline).toBe(false);
    // change-only guard: re-asserting the effective default (inline) on an unset
    // sibling is a no-op (current === next).
    const stable = useViewerStore.getState();
    useViewerStore.getState().toggleSourceDiffOption("w2", "inline", true); // w2 unset, default inline → no write
    expect(useViewerStore.getState()).toBe(stable);
    // No-write means NO empty `{ w2: {} }` record is created either.
    expect("w2" in useViewerStore.getState().sourceDiffViewOptions).toBe(false);
    // explicit set still works
    useViewerStore.getState().toggleSourceDiffOption("w1", "inline", true);
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.inline).toBe(true);
  });

  it("toggleSourceDiffOption('inline') first toggle flips to inline when the default is split", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "split" },
    });
    // Default = split → an unset tile's first toggle flips to inline (true).
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.inline).toBeUndefined();
    useViewerStore.getState().toggleSourceDiffOption("w1", "inline");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.inline).toBe(true);
  });

  // SLW-DV-5 (AC-4): live follow-the-edit toggle. Default = on (absent), so the
  // FIRST toggle turns it off; explicit sets and the change-only guard hold.
  it("toggleSourceDiffOption('autoFollow') defaults on and the first toggle turns it off", () => {
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.autoFollow).toBeUndefined();
    useViewerStore.getState().toggleSourceDiffOption("w1", "autoFollow");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.autoFollow).toBe(false);
    useViewerStore.getState().toggleSourceDiffOption("w1", "autoFollow");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.autoFollow).toBe(true);
  });

  it("toggleSourceDiffOption('autoFollow') honors an explicit value and is change-only", () => {
    // Setting to the effective default (on) for an unset tile writes nothing.
    const stable = useViewerStore.getState();
    useViewerStore.getState().toggleSourceDiffOption("w2", "autoFollow", true);
    expect(useViewerStore.getState()).toBe(stable);
    expect(useViewerStore.getState().sourceDiffViewOptions.w2?.autoFollow).toBeUndefined();
    // Explicit off then on.
    useViewerStore.getState().toggleSourceDiffOption("w2", "autoFollow", false);
    expect(useViewerStore.getState().sourceDiffViewOptions.w2?.autoFollow).toBe(false);
    useViewerStore.getState().toggleSourceDiffOption("w2", "autoFollow", true);
    expect(useViewerStore.getState().sourceDiffViewOptions.w2?.autoFollow).toBe(true);
  });

  // AC-1/AC-2: the plain bi-state options (absent = false) flip independently
  // and each key round-trips inside the shared per-tile record without disturbing
  // its siblings. (ignore-whitespace is NOT one of these — SLW-DV-8b RD-3 models it
  // as a value of the mutually-exclusive `sourceDiffMode` enum, not a peer option.)
  it("toggleSourceDiffOption('wordWrap'/'expandAll') default off, flip, and coexist per tile", () => {
    useViewerStore.getState().toggleSourceDiffOption("w1", "wordWrap");
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.wordWrap).toBe(true);
    useViewerStore.getState().toggleSourceDiffOption("w1", "expandAll", true);
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.expandAll).toBe(true);
    // earlier key untouched by the later write (siblings share one record).
    expect(useViewerStore.getState().sourceDiffViewOptions.w1?.wordWrap).toBe(true);
    // change-only guard: re-setting expandAll to its current value is a no-op.
    const stable = useViewerStore.getState();
    useViewerStore.getState().toggleSourceDiffOption("w1", "expandAll", true);
    expect(useViewerStore.getState()).toBe(stable);
  });

  // AC-2/AC-3: the seam the consolidation introduces — an option key that is
  // ABSENT inside a PRESENT record must still resolve through its default rule
  // (nested absence == unset), and writing one key must not clobber a sibling.
  it("resolveSourceDiffOption keeps per-key defaults when a sibling key is present", () => {
    // A present record that only pins wordWrap — inline/expandAll/autoFollow are
    // genuinely absent inside it.
    useViewerStore.setState({ sourceDiffViewOptions: { w1: { wordWrap: true } } });
    const opts = () => useViewerStore.getState().sourceDiffViewOptions.w1;
    const settings = () => useViewerStore.getState().settings;

    // inline follows the Settings default (tri-state), NOT a flat false.
    expect(resolveSourceDiffOption(opts(), "inline", settings())).toBe(true); // default inline
    useViewerStore.setState({
      settings: { ...settings(), defaultSourceDiffLayout: "split" },
    });
    expect(resolveSourceDiffOption(opts(), "inline", settings())).toBe(false); // default split
    // autoFollow stays default-ON; expandAll stays default-OFF; wordWrap is the set value.
    expect(resolveSourceDiffOption(opts(), "autoFollow", settings())).toBe(true);
    expect(resolveSourceDiffOption(opts(), "expandAll", settings())).toBe(false);
    expect(resolveSourceDiffOption(opts(), "wordWrap", settings())).toBe(true);

    // Toggling the tri-state inline into the same record must not clobber wordWrap.
    useViewerStore.getState().toggleSourceDiffOption("w1", "inline");
    expect(opts()?.wordWrap).toBe(true);
    expect(opts()?.inline).toBeDefined();
  });

  // SLW-WD-7: the pure default-resolver + settings round-trip (AC-2/AC-3/RD-1).
  it("defaultSourceDiffInline resolves the setting to a boolean (inline when unset)", () => {
    expect(defaultSourceDiffInline({ defaultSourceDiffLayout: undefined })).toBe(true);
    expect(defaultSourceDiffInline({ defaultSourceDiffLayout: "inline" })).toBe(true);
    expect(defaultSourceDiffInline({ defaultSourceDiffLayout: "split" })).toBe(false);
  });

  it("defaultSourceDiffInline fails safe to inline on a corrupted/unrecognized value", () => {
    // Only an explicit "split" opts out of inline; a garbage persisted blob must
    // NOT silently open every diff side-by-side.
    expect(defaultSourceDiffInline({ defaultSourceDiffLayout: "sideBySide" as never })).toBe(true);
  });

  it("updateSettings round-trips defaultSourceDiffLayout", () => {
    expect(useViewerStore.getState().settings.defaultSourceDiffLayout).toBe("inline");
    useViewerStore.getState().updateSettings({ defaultSourceDiffLayout: "split" });
    expect(useViewerStore.getState().settings.defaultSourceDiffLayout).toBe("split");
    useViewerStore.getState().updateSettings({ defaultSourceDiffLayout: "inline" });
    expect(useViewerStore.getState().settings.defaultSourceDiffLayout).toBe("inline");
  });

  // Per-tab worktree code editor open/close.
  it("toggleWorktreeEditorOpen flips the editor per tab (change-only), closed by default", () => {
    const toggle = (open?: boolean) =>
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: "sid1",
        workspaceId: "w1",
        teamKey: "team-a",
        open,
      });
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1 ?? false).toBe(false);
    toggle();
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(true);
    const stable = useViewerStore.getState();
    toggle(true); // no-op (already open)
    expect(useViewerStore.getState()).toBe(stable);
    toggle(false);
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(false);
  });

  // Session memory: the editor's remembered file survives a close so a
  // re-open resumes it. `setWorktreeEditorFile` is change-only.
  it("setWorktreeEditorFile remembers the last file across an editor close/open", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: {}, worktreeEditorFile: {} });
    const toggle = (open?: boolean) =>
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: "sid1",
        workspaceId: "w1",
        teamKey: "team-a",
        open,
      });
    toggle(true);
    useViewerStore.getState().setWorktreeEditorFile("w1", "src/mod.ts");
    const stable = useViewerStore.getState();
    useViewerStore.getState().setWorktreeEditorFile("w1", "src/mod.ts"); // no-op
    expect(useViewerStore.getState()).toBe(stable);
    toggle(false);
    // Closing preserves the remembered file, so a re-open resumes it.
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/mod.ts");
  });

  // The worktree editor is its own tile surface — hiding the source diff must
  // NOT close it (an editor opened over the diff stays open).
  it("toggleSourceDiffShown(false) leaves the worktree editor open (decoupled)", () => {
    useViewerStore.setState({
      sourceDiffShownBySession: { sid1: true },
      worktreeEditorOpenBySession: { sid1: true },
    });
    useViewerStore.getState().toggleSourceDiffShown({
      sessionId: "sid1",
      workspaceId: "w1",
      teamKey: "team-a",
      shown: false,
    });
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(true);
  });
});

describe("workspaces — per-workspace terminal state", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG, cwd: "/repo" }]);
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [...s.workspaces[repo], storyWorkspace("30-10", "/wt/30-10")],
      },
    }));
  });

  it("addTerminalTab on root workspace opens only in root's host workspace", async () => {
    await useViewerStore.getState().addTerminalTab(repo, { sessionId: "s1" });

    expect(tabIds(repo, ROOT_ID)).toEqual(["s1"]);
    expect(tabIds(repo, "30-10")).toEqual([]);
  });

  it("tab names number per workspace (root tabs don't advance the story's numbering)", async () => {
    await useViewerStore.getState().addTerminalTab(repo, { sessionId: "s1" });
    await useViewerStore.getState().addTerminalTab(repo, { sessionId: "s2" });
    await useViewerStore.getState().addTerminalTab(repo, { sessionId: "s3" }, "30-10");

    const names = (wsId: string) =>
      selectWorkspaceTabs(useViewerStore.getState(), repo, wsId).map((t) => t.name);
    expect(names(ROOT_ID)).toEqual(["Terminal 1", "Terminal 2"]);
    expect(names("30-10")).toEqual(["Terminal 1"]);
  });

  it("per-workspace tabs are isolated: switching workspace changes which tabs are active", async () => {
    await useViewerStore.getState().addTerminalTab(repo, { sessionId: "s1" });

    expect(tabIds(repo, ROOT_ID)).toEqual(["s1"]);
    expect(selectActiveTerminalTab(useViewerStore.getState(), repo, ROOT_ID)).toBe("s1");

    useViewerStore.getState().switchWorkspace(repo, "30-10");
    expect(tabIds(repo, "30-10")).toEqual([]);
  });

  it("closing the last terminal in a workspace opens a replacement tab", async () => {
    seedTerminalTabs(repo, ROOT_ID, ["s1"]);
    useViewerStore.getState().removeTerminalTab(repo, "s1");
    await settle();

    const ws = useViewerStore.getState().workspaces[repo];
    expect(ws.map((w) => w.id)).toContain(ROOT_ID);
    const tabs = selectWorkspaceTabs(useViewerStore.getState(), repo, ROOT_ID);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].id).not.toBe("s1");
    expect(tabs[0].name).toMatch(/^Terminal \d+$/);
  });
});

describe("aiStatus indicator", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
  });

  it("setSessionAiStatus sets status keyed by sessionId (O(1), no teamKey)", () => {
    useViewerStore.getState().setSessionAiStatus("sess-1", "done");
    expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBe("done");
  });

  it("setSessionAiStatus with undefined clears (deletes) the entry", () => {
    useViewerStore.getState().setSessionAiStatus("sess-1", "done");
    useViewerStore.getState().setSessionAiStatus("sess-1", undefined);
    expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBeUndefined();
    expect("sess-1" in useViewerStore.getState().aiStatusBySession).toBe(false);
  });

  it("setSessionAiStatus supports all status values", () => {
    for (const status of ["running", "done", "error", "needs-permission"] as const) {
      useViewerStore.getState().setSessionAiStatus("sess-1", status);
      expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBe(status);
    }
  });

  it("AC#1: setSessionAiStatus never touches the terminal layout (no O(N) scan/rewrite)", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    const layoutBefore = useViewerStore.getState().terminalLayout;

    useViewerStore.getState().setSessionAiStatus("sess-1", "done");

    // Layout identity preserved → the status write did NOT rewrite any tab.
    expect(useViewerStore.getState().terminalLayout).toBe(layoutBefore);
  });

  it("AC#4 (PT-4): a minimized session's status lives in the same flat slice", () => {
    // A soft-closed (minimized) agent keeps emitting status for its background
    // PTY. The slice is location-agnostic (keyed by sessionId), so no shadow
    // branch is needed — the minimized card reads its status from here.
    useViewerStore.getState().setSessionAiStatus("sess-sc", "done");
    expect(useViewerStore.getState().aiStatusBySession["sess-sc"]).toBe("done");
  });

  it("setSessionAiStatus is a no-op when the status is unchanged (stable identity)", () => {
    useViewerStore.getState().setSessionAiStatus("sess-1", "running");
    const before = useViewerStore.getState().aiStatusBySession;

    useViewerStore.getState().setSessionAiStatus("sess-1", "running");

    // Reference identity preserved → no re-render churn for an unchanged status.
    expect(useViewerStore.getState().aiStatusBySession).toBe(before);
  });

  it("AC#5: setActiveTerminalTab does NOT clear status (live monitor, not notification)", () => {
    seedTerminalTabs(repo, ROOT_ID, ["other", "sess-1"], { active: "other" });
    useViewerStore.getState().setSessionAiStatus("sess-1", "done");

    useViewerStore.getState().setActiveTerminalTab(repo, "sess-1");

    expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBe("done");
  });

  it("status is never stored on the tab", () => {
    seedTerminalTabs(repo, ROOT_ID, ["s1"]);
    useViewerStore.getState().setSessionAiStatus("s1", "done");
    const json = JSON.stringify(selectWorkspaceTabs(useViewerStore.getState(), repo, ROOT_ID));
    expect(json).not.toContain("done");
  });

  it("no-leak: closing a tab prunes its status entry (PTY kept running → no pty-exit)", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    useViewerStore.getState().setSessionAiStatus("sess-1", "running");

    // Close it — the PTY is kept running (reap-only-if-idle), so no `pty-exit`
    // ever fires; the prune must clear the flat-slice entry itself.
    useViewerStore.getState().removeTerminalTab(repo, "sess-1");

    expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBeUndefined();
  });

  it("no-leak: closeTeam prunes status for all the team's sessions", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    useViewerStore.getState().setSessionAiStatus("sess-1", "running");

    useViewerStore.getState().closeTeam(repo);

    expect(useViewerStore.getState().aiStatusBySession["sess-1"]).toBeUndefined();
  });

  it("no-leak: killing a minimized card's terminals prunes its shadow sessions' status", () => {
    const minimized: Workspace = {
      id: "30-10",
      kind: "story",
      cwd: "/wt/30-10",
      storyId: "30-10",
      status: "ready",
      sourceRepoSlug: TEST_SLUG,
    };
    seedTerminalTabs(repo, "30-10", ["sess-sc"]);
    useViewerStore.setState({
      softClosedWorkspaces: { [repo]: { "30-10": { workspace: minimized } } },
    });
    useViewerStore.getState().setSessionAiStatus("sess-sc", "needs-permission");

    useViewerStore.getState().killTerminalsForSoftClosedWorkspace(repo, "30-10");

    expect(useViewerStore.getState().aiStatusBySession["sess-sc"]).toBeUndefined();
    expect(terminalHost().contains("sess-sc")).toBe(false);
  });

  describe("agent-state records are pruned on the same teardown paths", () => {
    const declare = (sessionId: string) =>
      useAgentStateStore.getState().recordSignal(sessionId, {
        tier: 1,
        signal: { kind: "claude-hook", event: "UserPromptSubmit" },
      });
    const hasRecord = (sessionId: string) => sessionId in useAgentStateStore.getState().bySession;

    beforeEach(() => {
      useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
      // Each case closes its sessions (tombstoning them) and the next reuses the ids.
      resetPrunedAgentSessionsForTests();
    });

    it("tab close", () => {
      seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
      declare("sess-1");
      declare("other");
      useViewerStore.getState().removeTerminalTab(repo, "sess-1");
      expect(hasRecord("sess-1")).toBe(false);
      expect(hasRecord("other")).toBe(true);
    });

    it("workspace reap (kill), but not a keep-running close", () => {
      seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
      declare("sess-1");
      useViewerStore.getState().closeWorkspace(repo, ROOT_ID);
      expect(hasRecord("sess-1")).toBe(true);

      resetState();
      seedRoots(repo, [{ slug: TEST_SLUG }]);
      seedTerminalTabs(repo, ROOT_ID, ["sess-2"]);
      declare("sess-2");
      useViewerStore.getState().closeWorkspace(repo, ROOT_ID, { killTerminals: true });
      expect(hasRecord("sess-2")).toBe(false);
    });

    it("team close", () => {
      seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
      declare("sess-1");
      useViewerStore.getState().closeTeam(repo);
      expect(hasRecord("sess-1")).toBe(false);
    });

    it("minimized-card shadow kill", () => {
      seedTerminalTabs(repo, "30-10", ["sess-sc"]);
      useViewerStore.setState({
        softClosedWorkspaces: {
          [repo]: {
            "30-10": {
              workspace: {
                id: "30-10",
                kind: "story",
                cwd: "/wt/30-10",
                storyId: "30-10",
                status: "ready",
                sourceRepoSlug: TEST_SLUG,
              },
            },
          },
        },
      });
      declare("sess-sc");
      useViewerStore.getState().killTerminalsForSoftClosedWorkspace(repo, "30-10");
      expect(hasRecord("sess-sc")).toBe(false);
    });
  });

  it("prune preserves slice identity when the closed session had no status (no churn)", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    useViewerStore.getState().setSessionAiStatus("other-session", "running");
    const before = useViewerStore.getState().aiStatusBySession;

    useViewerStore.getState().removeTerminalTab(repo, "sess-1");

    // Closing an unrelated statusless tab must not churn the slice identity.
    expect(useViewerStore.getState().aiStatusBySession).toBe(before);
  });

  it("SW-AM-4 no-leak: closing a tab prunes its progress entry in lockstep with status", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    useViewerStore.getState().setSessionProgress("sess-1", { phase: "dev", note: "wip" });

    useViewerStore.getState().removeTerminalTab(repo, "sess-1");

    expect(useViewerStore.getState().progressBySession["sess-1"]).toBeUndefined();
  });

  it("SW-AM-4 no-leak: closeTeam prunes progress for all the team's sessions", () => {
    seedTerminalTabs(repo, ROOT_ID, ["sess-1"]);
    useViewerStore
      .getState()
      .setSessionProgress("sess-1", { phase: "done", note: "x", settled: true });

    useViewerStore.getState().closeTeam(repo);

    expect(useViewerStore.getState().progressBySession["sess-1"]).toBeUndefined();
  });
});

describe("workspaces — reorderWorkspace (the shared pill and card order)", () => {
  beforeEach(() => {
    resetState();
    seedRoots(repo, [{ slug: TEST_SLUG }]);
    useViewerStore.setState((s) => ({
      workspaceOrderByTeam: {},
      // As the team view's tree sync leaves it.
      trayBaseOrderByTeam: { [repo]: [ROOT_ID, "30-10", "30-8", "30-9"] },
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          { ...storyWorkspace("30-10", "/wt/30-10"), sourceRepoSlug: TEST_SLUG },
          { ...storyWorkspace("30-9", "/wt/30-9"), sourceRepoSlug: TEST_SLUG },
          { ...storyWorkspace("30-8", "/wt/30-8"), sourceRepoSlug: TEST_SLUG },
        ],
      },
    }));
  });
  const shown = () => {
    const s = useViewerStore.getState();
    return teamWorkspaceOrder({
      base: s.trayBaseOrderByTeam[repo],
      workspaces: s.workspaces[repo],
      softClosed: s.softClosedWorkspaces[repo],
      stored: s.workspaceOrderByTeam[repo],
    });
  };

  it("with no drag yet, the order is the tray's computed one (roots, then ids)", () => {
    expect(shown()).toEqual([ROOT_ID, "30-10", "30-8", "30-9"]);
  });

  it("moves a workspace within its group and records the whole order; the workspace list is untouched", () => {
    const before = useViewerStore.getState().workspaces[repo].map((w) => w.id);
    expect(useViewerStore.getState().reorderWorkspace(repo, "30-9", { before: "30-10" })).toBe(
      true,
    );
    expect(shown()).toEqual([ROOT_ID, "30-9", "30-10", "30-8"]);
    expect(useViewerStore.getState().workspaceOrderByTeam[repo]).toEqual([
      ROOT_ID,
      "30-9",
      "30-10",
      "30-8",
    ]);
    expect(useViewerStore.getState().workspaces[repo].map((w) => w.id)).toEqual(before);
  });

  it("moves across tray groups: the base workspace (alone in its group) goes below the stories", () => {
    expect(useViewerStore.getState().reorderWorkspace(repo, ROOT_ID, { after: "30-8" })).toBe(true);
    expect(shown()).toEqual(["30-10", "30-8", ROOT_ID, "30-9"]);
    expect(useViewerStore.getState().reorderWorkspace(repo, "30-9", { before: "30-10" })).toBe(
      true,
    );
    expect(shown()).toEqual(["30-9", "30-10", "30-8", ROOT_ID]);
  });

  it("is a no-op for a move that lands where the workspace already is, or an unknown id", () => {
    expect(useViewerStore.getState().reorderWorkspace(repo, "30-8", { after: "30-10" })).toBe(
      false,
    );
    expect(useViewerStore.getState().reorderWorkspace(repo, "nope", { after: "30-10" })).toBe(
      false,
    );
  });

  it("minimising and restoring a workspace never moves it", () => {
    useViewerStore.getState().reorderWorkspace(repo, "30-8", { before: "30-10" });
    const order = shown();
    const ws = useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10")!;
    useViewerStore.setState((s) => ({
      workspaces: { ...s.workspaces, [repo]: s.workspaces[repo].filter((w) => w.id !== "30-10") },
      softClosedWorkspaces: {
        ...s.softClosedWorkspaces,
        [repo]: { "30-10": { workspace: ws, layoutMode: "shared" } },
      },
    }));
    expect(shown()).toEqual(order);
    useViewerStore.setState((s) => ({
      workspaces: { ...s.workspaces, [repo]: [...s.workspaces[repo], ws] },
      softClosedWorkspaces: { ...s.softClosedWorkspaces, [repo]: {} },
    }));
    expect(shown()).toEqual(order);
  });

  it("a newly opened workspace takes its computed place next to its neighbour", () => {
    useViewerStore.getState().reorderWorkspace(repo, "30-8", { before: "30-10" });
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [repo]: [
          ...s.workspaces[repo],
          { ...storyWorkspace("30-11", "/wt/30-11"), sourceRepoSlug: TEST_SLUG },
        ],
      },
    }));
    // Computed: 30-10, 30-11, 30-8, 30-9 → 30-11 follows 30-10 wherever 30-10 now is.
    expect(shown()).toEqual([ROOT_ID, "30-8", "30-10", "30-11", "30-9"]);
  });

  it("a workspace closed for good leaves the stored order and the tray's base order", () => {
    useViewerStore.getState().reorderWorkspace(repo, "30-9", { before: "30-10" });
    useViewerStore.getState().closeWorkspace(repo, "30-9", { killTerminals: true });
    const s = useViewerStore.getState();
    expect(s.workspaceOrderByTeam[repo]).not.toContain("30-9");
    expect(s.trayBaseOrderByTeam[repo]).toEqual([ROOT_ID, "30-10", "30-8"]);
  });

  it("a move over the order the tray drew lands there, though the saved base is behind it", () => {
    // The tray's tree now puts 30-9 first; the saved base has not caught up yet.
    const drawn = [ROOT_ID, "30-9", "30-10", "30-8"];
    expect(
      useViewerStore.getState().reorderWorkspace(repo, "30-8", { before: "30-10" }, drawn),
    ).toBe(true);
    // Where the line showed: between 30-9 and 30-10, not before 30-10 in the stale base.
    expect(useViewerStore.getState().workspaceOrderByTeam[repo]).toEqual([
      ROOT_ID,
      "30-9",
      "30-8",
      "30-10",
    ]);
  });

  it("closeTeam clears the team's stored order", () => {
    useViewerStore.getState().reorderWorkspace(repo, "30-9", { before: "30-10" });
    useViewerStore.getState().closeTeam(repo);
    expect(useViewerStore.getState().workspaceOrderByTeam[repo]).toBeUndefined();
  });
});

describe("addTerminalTab — idempotent on the session (SW-DX-17a)", () => {
  beforeEach(() => resetState());

  it("focuses an existing tab instead of opening its session twice", async () => {
    const teamKey = "default";
    seedRoots(teamKey, [{ slug: "owner/a", cwd: "/repo" }]);
    const wsId = buildRootWorkspaceId("owner/a");

    await useViewerStore.getState().addTerminalTab(teamKey, { sessionId: "sess-1" }, wsId);
    await useViewerStore.getState().addTerminalTab(teamKey, { sessionId: "sess-1" }, wsId);

    expect(terminalHost().opened).toHaveLength(1);
    expect(tabIds(teamKey, wsId)).toEqual(["sess-1"]);
  });
});

describe("cycleTerminalTab — order + wrap within the active workspace (OC-7 AC5)", () => {
  const teamKey = "default";
  let wsId: string;
  const active = () => selectActiveTerminalTab(useViewerStore.getState(), teamKey, wsId);

  beforeEach(() => {
    resetState();
    seedRoots(teamKey, [{ slug: "owner/a", cwd: "/repo" }]);
    wsId = buildRootWorkspaceId("owner/a");
    seedTerminalTabs(teamKey, wsId, ["t1", "t2", "t3"], { active: "t1" });
  });

  it("next advances the active tab by one", () => {
    useViewerStore.getState().cycleTerminalTab(teamKey, "next");
    expect(active()).toBe("t2");
  });

  it("next wraps last→first", () => {
    useViewerStore.getState().setActiveTerminalTab(teamKey, "t3", wsId);
    useViewerStore.getState().cycleTerminalTab(teamKey, "next");
    expect(active()).toBe("t1");
  });

  it("prev wraps first→last", () => {
    useViewerStore.getState().cycleTerminalTab(teamKey, "prev");
    expect(active()).toBe("t3");
  });

  it("is a no-op with a single tab", () => {
    resetState();
    seedRoots(teamKey, [{ slug: "owner/b", cwd: "/repo-b" }]);
    const only = buildRootWorkspaceId("owner/b");
    seedTerminalTabs(teamKey, only, ["solo"], { active: "solo" });
    expect(useViewerStore.getState().cycleTerminalTab(teamKey, "next")).toBe(false);
    expect(selectActiveTerminalTab(useViewerStore.getState(), teamKey, only)).toBe("solo");
  });
});
