// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The action resolves the focused workspace via getActiveWorkspaceContext (the
// shared workspace-scoped idiom); mock it to drive the capability gate + guards.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

import { useViewerStore } from "@/shared/stores/viewer-store";
import { SHORTCUT_ACTIONS, findConflict } from "./registry";
import { isReservedKeystroke } from "./keystroke";

// The hotkey now resolves the active tab's sessionId via `selectActiveTerminalTab`,
// which reads the terminal projection. Mock it so the test can supply a sid.
const selectActiveTerminalTabMock = vi.fn();
vi.mock("@/shared/stores/viewer-store", async () => {
  const actual = await vi.importActual<typeof import("@/shared/stores/viewer-store")>(
    "@/shared/stores/viewer-store",
  );
  return {
    ...actual,
    selectActiveTerminalTab: (...args: unknown[]) => selectActiveTerminalTabMock(...args),
  };
});

function runToggleCodeEditor() {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-code-editor");
  if (!action) throw new Error("viewer.toggle-code-editor not registered");
  action.run();
}

// SW-VE-12: the hotkey now fires on every folder-rooted kind (gate flipped
// from `sourceDiffToggle` → `sourceCapable`). `story` + `root` are both
// capable, so the test uses them to exercise the open/close path.
const storyWs = { id: "w1", kind: "story" as const };
const rootWs = { id: "r1", kind: "root" as const };
// SW-VE-12 R3-M2: `epic` is `sourceCapable` so the Code Editor hotkey
// fires on it (the editor opens on its cwd); the Source Diff hotkey
// (separate test block below) does NOT fire there because `diffCapable`
// is false. A future flip of the diff hotkey's gate to `sourceCapable`
// would fail loudly.
const epicWs = { id: "e1", kind: "epic" as const };

describe("SLW-DV-3 viewer.toggle-code-editor shortcut action", () => {
  beforeEach(() => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: {},
      worktreeEditorFile: {},
      sourceDiffSelectedFile: {},
    });
    vi.clearAllMocks();
    selectActiveTerminalTabMock.mockReturnValue("sid1");
  });

  it("is registered as a rebindable, menu-visible ⌘⇧C workspace toggle", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-code-editor");
    expect(action).toBeDefined();
    expect(action?.defaultKeystroke).toBe("CmdOrCtrl+Shift+C");
    expect(action?.menuPath).toEqual(["View", "Toggle Code Editor"]);
    expect(action?.scope).toBe("workspace");
  });

  it("⌘⇧C is collision-free and not OS-reserved", () => {
    expect(findConflict("CmdOrCtrl+Shift+C", {}, "viewer.toggle-code-editor")).toBeNull();
    // A Shift chord is never OS-reserved (reserved list is un-shifted only).
    expect(isReservedKeystroke("CmdOrCtrl+Shift+C")).toBeNull();
  });

  it("run() toggles the editor open, then closed (a view toggle like the diff / HUD)", () => {
    // Session memory: the toggle resumes the tile's remembered file — it does NOT
    // touch the editor's file memory (`worktreeEditorFile`), only visibility.
    useViewerStore.setState({ worktreeEditorFile: { w1: "src/mod.ts" } });
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: storyWs });
    runToggleCodeEditor(); // open
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(true);
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/mod.ts");
    runToggleCodeEditor(); // close
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(false);
  });

  it("SW-VE-12: run() fires on `root` too — every folder-rooted kind qualifies", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: rootWs });
    runToggleCodeEditor();
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(true);
  });

  it("run() is a no-op when no workspace is focused", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: null });
    runToggleCodeEditor();
    expect(useViewerStore.getState().worktreeEditorOpenBySession).toEqual({});
  });

  it("run() is a no-op when the focused tile has no active tab (idle tile)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: storyWs });
    selectActiveTerminalTabMock.mockReturnValueOnce(undefined);
    runToggleCodeEditor();
    expect(useViewerStore.getState().worktreeEditorOpenBySession).toEqual({});
  });

  it("does NOT collide with export-pdf (⌘⇧E stays with export)", () => {
    const exportPdf = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.export-pdf");
    expect(exportPdf?.defaultKeystroke).toBe("CmdOrCtrl+Shift+E");
    const toggleEditor = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-code-editor");
    expect(toggleEditor?.defaultKeystroke).not.toBe(exportPdf?.defaultKeystroke);
  });

  // SW-VE-12 R3-M2: Code Editor hotkey fires on every `sourceCapable` kind,
  // including `epic` (`sourceCapable=true` even though `diffCapable=false`).
  it("SW-VE-12 R3-M2: run() fires on `epic` (sourceCapable)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: epicWs });
    runToggleCodeEditor();
    expect(useViewerStore.getState().worktreeEditorOpenBySession.sid1).toBe(true);
  });
});

// SW-VE-12 R3-M2: the Source Diff hotkey gates on `diffCapable`, NOT
// `sourceCapable`. `epic` is deliberately inert so the overlay never
// opens on its cwd (an enclosing-repo diff would mislead). A future flip
// of this gate back to `sourceCapable` would fail these assertions.
describe("SW-VE-12 R3-M2: viewer.toggle-source-diff kind gating", () => {
  function runToggleSourceDiff() {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-source-diff");
    if (!action) throw new Error("viewer.toggle-source-diff not registered");
    action.run();
  }

  beforeEach(() => {
    useViewerStore.setState({ sourceDiffShownBySession: {} });
    vi.clearAllMocks();
    selectActiveTerminalTabMock.mockReturnValue("sid1");
  });

  it("fires on `story` (diffCapable)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: storyWs });
    runToggleSourceDiff();
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(true);
  });

  it("fires on `root` + `generic` (diffCapable)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: rootWs });
    runToggleSourceDiff();
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(true);
    const genericWs = { id: "g1", kind: "generic" as const };
    useViewerStore.setState({ sourceDiffShownBySession: {} });
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: genericWs });
    runToggleSourceDiff();
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1).toBe(true);
  });

  it("is INERT on `epic` (not diffCapable)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "team-a", workspace: epicWs });
    runToggleSourceDiff();
    expect(useViewerStore.getState().sourceDiffShownBySession.sid1 ?? false).toBe(false);
  });
});
