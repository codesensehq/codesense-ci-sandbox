// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-33 — tray keyboard/store logic: the shared activate state-rule, the
// per-team MRU stack, and the Cmd+Alt+; flip-to-last gesture.

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
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import {
  teamTileOrder,
  useViewerStore,
  type SoftClosedWorkspace,
  type Workspace,
} from "./viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const M = "e-1-min";

installTerminalLayoutHarness();

const ws = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });
const shadow = (id: string): SoftClosedWorkspace => ({ workspace: ws(id) });

beforeEach(() => {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    workspaces: { [TEAM]: [ws(A), ws(B)] },
    softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) } },
    mruWorkspaceStack: {},
  });
  for (const id of [A, B, M]) seedTerminalTabs(TEAM, id, [`sess-${id}`]);
  vi.clearAllMocks();
});

describe("MRU stack recording", () => {
  it("switchWorkspace pushes most-recent-first and de-dupes", () => {
    useViewerStore.getState().switchWorkspace(TEAM, B);
    useViewerStore.getState().switchWorkspace(TEAM, A);
    useViewerStore.getState().switchWorkspace(TEAM, B);
    // A was visited between the two B visits → de-duped, B on top, A next.
    expect(useViewerStore.getState().mruWorkspaceStack[TEAM]).toEqual([B, A]);
  });
});

describe("activateTrayWorkspace (shared state-rule)", () => {
  it("minimized target → replaces the focused leaf (becomes active + on-screen)", () => {
    useViewerStore.getState().activateTrayWorkspace(TEAM, M);
    const s = useViewerStore.getState();
    expect(s.activeWorkspace[TEAM]).toBe(M);
    expect(s.softClosedWorkspaces[TEAM]?.[M]).toBeUndefined(); // surfaced
  });
  it("on-screen, not focused → focuses (no mosaic mutation)", () => {
    useViewerStore.getState().activateTrayWorkspace(TEAM, B);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    expect(useViewerStore.getState().foregroundLayout[TEAM]?.root ?? null).toBeNull(); // untouched
  });
  it("already-focused → no-op", () => {
    useViewerStore.getState().activateTrayWorkspace(TEAM, A);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
  });
});

describe("flipLastWorkspace (Cmd+Alt+;)", () => {
  it("flips to the previous workspace and back (2-way toggle)", () => {
    useViewerStore.getState().switchWorkspace(TEAM, A); // MRU: [A]
    useViewerStore.getState().switchWorkspace(TEAM, B); // MRU: [B, A], current B
    useViewerStore.getState().flipLastWorkspace(TEAM); // → A
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
    useViewerStore.getState().flipLastWorkspace(TEAM); // → back to B
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
  });
  it("skips the current workspace and ids that no longer exist", () => {
    useViewerStore.setState({ mruWorkspaceStack: { [TEAM]: [A, "ghost", B] } });
    useViewerStore.setState({ activeWorkspace: { [TEAM]: A } });
    useViewerStore.getState().flipLastWorkspace(TEAM); // skip A (current) + ghost (gone) → B
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
  });
  it("no distinct prior workspace → no-op", () => {
    useViewerStore.setState({ mruWorkspaceStack: { [TEAM]: [A] } });
    useViewerStore.getState().flipLastWorkspace(TEAM);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
  });
});

describe("jumpToTrayIndex / setTrayOrder (Cmd+1..9)", () => {
  it("jumps to the Nth chip in the published rendered order via the state-rule", () => {
    useViewerStore.getState().setTrayOrder(TEAM, [A, B, M]);
    useViewerStore.getState().jumpToTrayIndex(TEAM, 1); // index 1 → B (on-screen → focus)
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    useViewerStore.getState().jumpToTrayIndex(TEAM, 2); // index 2 → M (minimized → replace)
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(M);
  });
  it("out-of-range index → no-op", () => {
    useViewerStore.getState().setTrayOrder(TEAM, [A, B]);
    useViewerStore.getState().jumpToTrayIndex(TEAM, 7);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A); // unchanged
  });

  it("reopens a minimized chip when NOTHING is focused (empty/all-minimized state)", () => {
    // Everything minimized → active is the no-repos sentinel, mosaic empty.
    // Cmd+1 must still reopen the chip (into center), not no-op.
    useViewerStore.setState({
      workspaces: { [TEAM]: [] },
      activeWorkspace: { [TEAM]: "root:_no_repos" },
      softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) } },
    });
    useViewerStore.getState().setTrayOrder(TEAM, [M]);
    useViewerStore.getState().jumpToTrayIndex(TEAM, 0);
    const s = useViewerStore.getState();
    expect(s.activeWorkspace[TEAM]).toBe(M); // reopened
    expect(s.workspaces[TEAM]?.some((w) => w.id === M)).toBe(true);
    expect(s.softClosedWorkspaces[TEAM]?.[M]).toBeUndefined(); // surfaced
  });
});

describe("markWorkspaceTrayCoachmarkSeen", () => {
  it("flips the seen flag", () => {
    expect(useViewerStore.getState().workspaceTraySeenCoachmark).toBe(false);
    useViewerStore.getState().markWorkspaceTrayCoachmarkSeen();
    expect(useViewerStore.getState().workspaceTraySeenCoachmark).toBe(true);
  });
});

describe("a move to a workspace of a team that is not active", () => {
  const OTHER = "acme:other";
  const O = "o-1";
  const OM = "o-min";

  beforeEach(() => {
    useViewerStore.setState({
      openedTeams: [TEAM, OTHER],
      workspaces: { [TEAM]: [ws(A), ws(B)], [OTHER]: [ws(O), ws("o-2")] },
      activeWorkspace: { [TEAM]: A, [OTHER]: "o-2" },
      softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) }, [OTHER]: { [OM]: shadow(OM) } },
    });
  });

  it("switchWorkspace sets that team's remembered focus and leaves the other team's alone", () => {
    useViewerStore.getState().switchWorkspace(OTHER, O);
    expect(useViewerStore.getState().activeWorkspace).toEqual({ [TEAM]: A, [OTHER]: O });
  });

  it("reopening a minimized workspace there puts it in that team only", () => {
    useViewerStore.getState().reopenSoftClosedWorkspace(OTHER, OM);
    const s = useViewerStore.getState();
    expect(s.workspaces[OTHER]?.map((w) => w.id)).toContain(OM);
    expect(s.workspaces[TEAM]?.map((w) => w.id)).toEqual([A, B]);
    expect(s.activeWorkspace[TEAM]).toBe(A);
  });

  it("a pill of the workspace already focused there changes nothing", () => {
    const before = useViewerStore.getState();
    expect(useViewerStore.getState().activateTrayWorkspace(OTHER, "o-2")).toBe(false);
    const after = useViewerStore.getState();
    expect(after.activeWorkspace).toBe(before.activeWorkspace);
    expect(after.workspaces).toBe(before.workspaces);
  });
});

describe("teamTileOrder", () => {
  it("is the same array until a slice it reads changes", () => {
    const first = teamTileOrder(useViewerStore.getState(), TEAM);
    useViewerStore.setState({ mruWorkspaceStack: { [TEAM]: [B] } });
    expect(teamTileOrder(useViewerStore.getState(), TEAM)).toBe(first);
    useViewerStore.setState((s) => ({
      workspaces: { ...s.workspaces, [TEAM]: [...s.workspaces[TEAM]!, ws("e-1-c")] },
    }));
    const next = teamTileOrder(useViewerStore.getState(), TEAM);
    expect(next).not.toBe(first);
    expect(next).toContain("e-1-c");
  });
});
