// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { useEffect, useLayoutEffect, useRef } from "react";
import { render, act, cleanup, fireEvent } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { DndContext } from "@dnd-kit/core";

// SW-EW-17 AC-6 (the crux): a mosaic rearrange (split / move / collapse) must
// relocate a terminal WITHOUT remounting it — same component instance, same DOM
// node — so the PTY never respawns and no `session restarted`/`reattached`
// marker fires (the marker only fires on a ShellTerminalPanel remount/reattach,
// so "never remounts" IS the negative-marker guarantee).

const rec = vi.hoisted(() => ({
  mounts: {} as Record<string, number>,
  unmounts: {} as Record<string, number>,
  keys: [] as string[],
  visibility: {} as Record<string, boolean[]>,
  contextLanding: {} as Record<string, boolean | undefined>,
  onContextHold: {} as Record<string, (() => void) | undefined>,
  onContextHoldTimeout: {} as Record<string, (() => void) | undefined>,
}));

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({
    sessionId,
    visible,
    autoFocus,
    contextLanding,
    onContextHold,
    onContextHoldTimeout,
  }: {
    sessionId: string;
    visible: boolean;
    autoFocus: boolean;
    contextLanding?: boolean;
    onContextHold?: () => void;
    onContextHoldTimeout?: () => void;
  }) => {
    rec.contextLanding[sessionId] = contextLanding;
    rec.onContextHold[sessionId] = onContextHold;
    rec.onContextHoldTimeout[sessionId] = onContextHoldTimeout;
    const nodeRef = useRef<HTMLDivElement>(null);
    const previousVisibleRef = useRef(false);
    useEffect(() => {
      rec.mounts[sessionId] = (rec.mounts[sessionId] ?? 0) + 1;
      return () => {
        rec.unmounts[sessionId] = (rec.unmounts[sessionId] ?? 0) + 1;
      };
    }, [sessionId]);
    useLayoutEffect(() => {
      (rec.visibility[sessionId] ??= []).push(visible);
      if (visible && !previousVisibleRef.current && autoFocus) nodeRef.current?.focus();
      previousVisibleRef.current = visible;
    }, [autoFocus, sessionId, visible]);
    return (
      <div
        ref={nodeRef}
        data-sid={sessionId}
        tabIndex={0}
        onKeyDown={(event) => rec.keys.push(event.key)}
      />
    );
  },
}));

vi.mock("@/tauri/commands", () => ({
  // Each team draws its agents with its own faces.
  avatarPacksListForTeam: vi.fn().mockResolvedValue({ packs: [], warnings: [] }),
  LOCAL_MACHINE_ID: "local",
  listMachines: vi.fn().mockResolvedValue([]),
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
  // Every shell sits at a bare prompt (idle) unless a test says otherwise.
  sessionsRunningProcesses: vi.fn((ids: string[]) =>
    Promise.resolve(ids.map(() => ({ foreground: null, background: [] }))),
  ),
  // SW-TP CAP-11: the per-tab caption's cwd slot calls this lazily; return
  // null so the caption falls back to the tab name in these layout tests.
  sessionCwd: vi.fn().mockResolvedValue(null),
  // This Mac's terminal server, asked while the app has lost it.
  terminalServerOverview: vi.fn().mockResolvedValue({ state: "unreachable" }),
}));

// SLW-DV-3: stub WorktreeEditorSurface to a marker so this test verifies the
// tile-mount WIRING (the surface is mounted per tile with the tile's focus state)
// without pulling the heavy editor leaf (CM6 / file tree / worktree IPC). If the
// `<WorktreeEditorSurface>` line in WorkspaceTileFrame is dropped, the marker
// disappears and the assertion turns red. The surface's own open/capability gate
// is covered in source-editor-inset.test.tsx.
vi.mock("./source-editor-inset", () => ({
  WorktreeEditorSurface: ({
    workspace,
    isFocused,
  }: {
    workspace: { id: string };
    isFocused: boolean;
  }) => (
    <div data-testid={`worktree-editor-surface-${workspace.id}`} data-focused={String(isFocused)} />
  ),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

// SW-EW-28: the expanded HUD reuses the real markdown viewer; stub it so these
// panel tests don't pull in the worker / mermaid / d2 render pipeline.
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({
  MarkdownViewer: ({ filePath }: { filePath: string }) => (
    <div data-testid="mock-markdown-viewer">{filePath}</div>
  ),
}));

// SW-EW-28: the compact card's story summary comes from this lazy hook; stub it
// so it doesn't reach for the (mocked-out) get_story_details IPC.
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "summary" }, loading: false })),
}));

import {
  selectExpandedWorkspaceId,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import type { WorkspaceKind } from "@/shared/lib/workspace-kind-capabilities";
import { contextBack } from "@/features/viewer/lib/context-switch";
import { useOrgStore } from "@/shared/stores/org-store";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { useEpicAssemblyStore } from "@/features/shell/stores/epic-assembly-store";
import { WorkspaceTerminalPanels, WorkspaceInitOverlay } from "./workspace-terminal-panels";
import { TERMINAL_THEMES } from "./terminal-constants";
import {
  EPIC_ASSEMBLY_REPORT_WATCHDOG_MS,
  MATERIALIZE_START_WATCHDOG_MS,
} from "./workspace-overlay-state";
import {
  seedTerminalTabs,
  terminalHost as layoutHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";

/** No terminal was ended: every seeded session still runs in its host. */
const allStillRunning = () => ["sid-a", "sid-b"].every((session) => layoutHost().contains(session));

function seedStore() {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    terminalVisible: true,
    workspaces: {
      [TEAM]: [
        { id: A, kind: "story", cwd: "/a", status: "ready" },
        { id: B, kind: "story", cwd: "/b", status: "ready" },
      ],
    },
  });
  seedTerminalTabs(TEAM, A, ["sid-a"]);
  seedTerminalTabs(TEAM, B, ["sid-b"]);
}

beforeEach(() => {
  useViewerStore.setState({ cliExpanded: false });
  rec.mounts = {};
  rec.unmounts = {};
  rec.keys = [];
  rec.visibility = {};
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  seedStore();
});

// Globals are off in this file (vitest imported explicitly), so RTL's
// auto-cleanup never registers — unmount each render between tests so accumulated
// trees can't leak handlers into a shared store spy (SW-EW-27 focus tests).
afterEach(() => {
  cleanup();
});

const renderPanels = () =>
  render(
    <DndContext>
      <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
    </DndContext>,
  );

describe("WorkspaceTerminalPanels — no-remount across a mosaic rearrange (AC-6)", () => {
  it("a directional move does not remount either terminal", () => {
    const { container } = renderPanels();
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    const nodeA = container.querySelector('[data-sid="sid-a"]');
    const nodeB = container.querySelector('[data-sid="sid-b"]');
    expect(nodeA).not.toBeNull();
    expect(nodeB).not.toBeNull();

    // Move B below A → a column (horizontal) split nesting both.
    act(() => {
      useViewerStore
        .getState()
        .placeWorkspaceTile(TEAM, B, { targetWorkspaceId: A, axis: "column", side: "after" });
    });

    // Same instances — never unmounted, never re-created.
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    expect(rec.unmounts).toEqual({});
    // The exact same DOM nodes survived the relocation.
    expect(container.querySelector('[data-sid="sid-a"]')).toBe(nodeA);
    expect(container.querySelector('[data-sid="sid-b"]')).toBe(nodeB);
    expect(allStillRunning()).toBe(true);
  });

  it("a preset change (columns→grid) does not remount terminals", () => {
    renderPanels();
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    act(() => {
      useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    });
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    expect(rec.unmounts).toEqual({});
  });

  it("moving the expanded zoom with the focus does not remount either terminal", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const { container } = renderPanels();
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    const nodeA = container.querySelector('[data-sid="sid-a"]');
    const nodeB = container.querySelector('[data-sid="sid-b"]');

    // Expand A, then select B → the zoom moves to B in place (no mosaic reveal).
    act(() => {
      useViewerStore.getState().expandWorkspace(TEAM, A);
    });
    act(() => {
      useViewerStore.getState().switchWorkspace(TEAM, B);
    });
    expect(selectExpandedWorkspaceId(useViewerStore.getState(), TEAM)).toBe(B);

    // Same instances + same DOM nodes — no remount, respawn, or resize-nudge repair.
    expect(rec.mounts).toEqual({ "sid-a": 1, "sid-b": 1 });
    expect(rec.unmounts).toEqual({});
    expect(container.querySelector('[data-sid="sid-a"]')).toBe(nodeA);
    expect(container.querySelector('[data-sid="sid-b"]')).toBe(nodeB);
    expect(allStillRunning()).toBe(true);
  });

  it("two-level zoom hides every off-path branch up the tree (AC-8)", () => {
    const C = "e-1-c";
    // A custom nested mosaic: column[ row[A, B], C ], zoomed on the deepest leaf A.
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [TEAM]: [...s.workspaces[TEAM], { id: C, kind: "story", cwd: "/c", status: "ready" }],
      },
      foregroundLayout: {
        [TEAM]: {
          sizes: [],
          direction: "row",
          preset: "custom",
          root: {
            kind: "split",
            axis: "column",
            sizes: [50, 50],
            children: [
              {
                kind: "split",
                axis: "row",
                sizes: [50, 50],
                children: [
                  { kind: "leaf", workspaceId: A },
                  { kind: "leaf", workspaceId: B },
                ],
              },
              { kind: "leaf", workspaceId: C },
            ],
          },
        },
      },
      activeWorkspace: { ...s.activeWorkspace, [TEAM]: A },
      cliExpanded: true,
    }));
    seedTerminalTabs(TEAM, C, ["sid-c"]);
    const { container } = renderPanels();

    // SW-EW-21: a collapsed branch is marked `data-zoom="collapsed"` on its
    // OUTER [data-panel] flex-item (the CSS then sets display:none). Walk
    // ancestors for that attribute rather than the old `hidden` className.
    const hasCollapsedAncestor = (el: Element | null): boolean => {
      for (let n = el; n; n = n.parentElement) {
        if (n.getAttribute?.("data-zoom") === "collapsed") return true;
      }
      return false;
    };
    // Zoomed leaf A is visible; its sibling B (inner split) AND C (outer split)
    // are both collapsed — zoom applied at every level on the path.
    expect(hasCollapsedAncestor(container.querySelector('[data-sid="sid-a"]'))).toBe(false);
    expect(hasCollapsedAncestor(container.querySelector('[data-sid="sid-b"]'))).toBe(true);
    expect(hasCollapsedAncestor(container.querySelector('[data-sid="sid-c"]'))).toBe(true);
    // No terminal remounted to achieve the zoom.
    expect(rec.unmounts).toEqual({});
  });

  it("Esc never restores a zoomed tile — from an xterm target OR document.body (SLW-FX-8)", () => {
    // A zoomed CLI tile is a live terminal; Esc is terminal input (vim/less/fzf/
    // any TUI), never a restore path. Restore lives only on Cmd+Shift+M, the ⛶
    // button, and the chrome double-click. This locks that Esc — from ANY origin,
    // including document.body where focus routinely drifts — leaves zoom intact.
    // Make this the active view: pre-SLW-FX-8 the chrome-level Esc handler would
    // have armed here, so this fails on baseline (body-Esc un-zoomed) and passes
    // after the handler is deleted — a real RED→GREEN.
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    useViewerStore.setState((s) => ({
      foregroundLayout: {
        ...s.foregroundLayout,
        [TEAM]: { sizes: [], direction: "row", preset: "columns", root: null },
      },
      activeWorkspace: { ...s.activeWorkspace, [TEAM]: A },
      cliExpanded: true,
    }));
    const { container } = renderPanels();
    const zoomed = () => selectExpandedWorkspaceId(useViewerStore.getState(), TEAM);
    expect(zoomed()).toBe(A);

    // Esc typed INTO a terminal must reach the shell, not collapse the zoom.
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    container.appendChild(xterm);
    act(() => {
      xterm.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(zoomed()).toBe(A);

    // Esc from document.body (focus drifted off the xterm) must ALSO leave the
    // zoom untouched — no global Esc-restore handler exists anymore.
    act(() => {
      document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(zoomed()).toBe(A);
  });

  it("collapsing to one tile (close B) keeps the survivor A shown and running", () => {
    const { container } = renderPanels();
    act(() => {
      // Remove B from the foreground set → reconcile prunes + collapses to A.
      useViewerStore.setState((s) => ({
        workspaces: { ...s.workspaces, [TEAM]: s.workspaces[TEAM].filter((w) => w.id !== B) },
      }));
    });
    // The leaf moved up the tree, so its slot remounted (free: the display lives
    // in the terminal manager); it is still shown, exactly once, and still running.
    expect(container.querySelectorAll('[data-sid="sid-a"]')).toHaveLength(1);
    expect(layoutHost().contains("sid-a")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// SW-EW-30 — Hide (–) and the default close (✕) are available at every N,
// including the lone tile (N=1), so the last workspace can be closed and the CLI
// drained to its empty state. Zoom stays split-only.
// ---------------------------------------------------------------------------
describe("WorkspaceTerminalPanels — Hide vs the default close at N=1", () => {
  function seedSingle() {
    useViewerStore.setState({
      foregroundLayout: {},
      softClosedWorkspaces: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: { [TEAM]: [{ id: A, kind: "story", cwd: "/a", status: "ready" }] },
      aiStatusBySession: {}, // no agent dots → the lone tab is a plain shell
    });
  }

  it("renders the Hide button at N=1 but NOT the zoom toggle", () => {
    seedSingle();
    const { queryByTestId } = renderPanels();
    expect(queryByTestId(`tile-hide-${A}`)).not.toBeNull();
    expect(queryByTestId(`tile-zoom-${A}`)).toBeNull();
  });

  it("✕ Ends a lone idle tile (removes it, no shadow)", async () => {
    seedSingle();
    const { getByTestId } = renderPanels();
    // The lone shell is idle → `✕` Ends it. The close is async (the running
    // probe), so flush it inside act.
    await act(async () => {
      fireEvent.click(getByTestId(`tile-close-${A}`));
    });
    expect(useViewerStore.getState().workspaces[TEAM]).toEqual([]);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[A]).toBeUndefined();
  });

  it("– Hides the lone tile into the tray (drains to empty, keeps running)", () => {
    seedSingle();
    const { getByTestId } = renderPanels();
    fireEvent.click(getByTestId(`tile-hide-${A}`));
    expect(useViewerStore.getState().workspaces[TEAM]).toEqual([]);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[A]).toBeDefined();
  });

  // Two tiles with A zoomed — `✕` on the zoomed idle tile removes it and the
  // store self-heals the zoom.
  function seedZoomed() {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    useViewerStore.setState({
      softClosedWorkspaces: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [
          { id: A, kind: "story", cwd: "/a", status: "ready" },
          { id: B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
      aiStatusBySession: {}, // no agent dots → plain shells
      foregroundLayout: {
        [TEAM]: { sizes: [], direction: "row", preset: "columns", root: null },
      },
      cliExpanded: true,
    });
  }

  it("✕ on the expanded idle tile Ends it, leaving one tile with nothing to zoom (AC-11)", async () => {
    seedZoomed();
    const { getByTestId } = renderPanels();
    expect(selectExpandedWorkspaceId(useViewerStore.getState(), TEAM)).toBe(A);
    await act(async () => {
      fireEvent.click(getByTestId(`tile-close-${A}`));
    });
    expect(useViewerStore.getState().workspaces[TEAM].map((w) => w.id)).toEqual([B]);
    expect(selectExpandedWorkspaceId(useViewerStore.getState(), TEAM)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// SW-EW-27 — focus a tile by clicking into its BODY (xterm area), not just the
// header chrome. The body is portaled into a host-layer sibling subtree, so the
// frame's onPointerDownCapture never sees its events (React events follow the
// fiber tree, not the DOM tree). The fix attaches the focus gesture inside the
// body's OWN subtree (WorkspaceTerminalBody). The mocked ShellTerminalPanel
// renders `<div data-sid=… />` inside that subtree, so a pointerdown on it
// exercises the real body handler.
// ---------------------------------------------------------------------------
describe("WorkspaceTerminalPanels — focus on body click (SW-EW-27)", () => {
  const bodyDown = (node: Element) =>
    node.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, cancelable: true }));

  it("AC-1/AC-4: pointerdown in tile B's body moves focus A → B and records history", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    useViewerStore.setState({
      openedTeams: [TEAM],
      contextHistory: { back: [], forward: [{ teamKey: TEAM, workspaceId: B }] },
    });
    const { container } = renderPanels();
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);

    const bodyB = container.querySelector('[data-sid="sid-b"]');
    expect(bodyB).not.toBeNull();
    act(() => {
      bodyDown(bodyB!);
    });

    // The store flip is the positive proof the body handler routes through the
    // real `switchWorkspace` (which makes AC-5's negative spy assertion meaningful).
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B);
    expect(useViewerStore.getState().contextHistory).toEqual({
      back: [{ teamKey: TEAM, workspaceId: A }],
      forward: [],
    });
    act(() => {
      contextBack();
    });
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
  });

  it("AC-3: the focus gesture does not preventDefault (xterm selection survives)", () => {
    const { container } = renderPanels();
    const bodyB = container.querySelector('[data-sid="sid-b"]');
    const ev = new MouseEvent("pointerdown", { bubbles: true, cancelable: true });
    act(() => {
      bodyB!.dispatchEvent(ev);
    });
    expect(ev.defaultPrevented).toBe(false);
  });

  it("AC-5: clicking the already-focused tile's body is a no-op", () => {
    const { container } = renderPanels();
    const spy = vi.spyOn(useViewerStore.getState(), "switchWorkspace");
    const bodyA = container.querySelector('[data-sid="sid-a"]');
    act(() => {
      bodyDown(bodyA!);
    });
    expect(spy).not.toHaveBeenCalled();
    // Independent of the spy: the focused tile stays focused (guards against an
    // inverted `!==` guard that would still skip the call but corrupt state).
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
    spy.mockRestore();
  });
});

// SW-EW-28 — the per-tile story HUD: visibility + size are per-workspace, and
// the show/hide toggle lives in the chrome's view-options kebab (OC-14). The
// component-level matrix lives in story-hud.test.tsx; this guards the parent's
// per-tile mount. The kebab-item → store wiring (view→store) is owned by
// terminal-tile-chrome.test.tsx; store→card (store→view) is guarded below.
describe("WorkspaceTerminalPanels — per-tile story HUD (SW-EW-28)", () => {
  function seedStoryTiles() {
    useViewerStore.setState({
      storyHudShown: {},
      storyHudExpanded: {},
      workspaces: {
        [TEAM]: [
          { id: A, kind: "story", cwd: "/a", status: "ready", storyFilePath: "/a/story.md" },
          { id: B, kind: "story", cwd: "/b", status: "ready", storyFilePath: "/b/story.md" },
        ],
      },
    });
  }

  it("hides the HUD by default; only the chrome toggle shows", () => {
    seedStoryTiles();
    const { container } = renderPanels();
    expect(container.querySelector(`[data-testid="story-hud-${A}"]`)).toBeNull();
    expect(container.querySelector(`[data-testid="story-hud-${B}"]`)).toBeNull();
    expect(container.querySelector('[data-testid="mock-markdown-viewer"]')).toBeNull();
    // The per-tile view-options kebab is present for both tiles — the HUD toggle
    // now lives inside it (OC-14).
    expect(container.querySelector(`[data-testid="tile-view-options-${A}"]`)).not.toBeNull();
    expect(container.querySelector(`[data-testid="tile-view-options-${B}"]`)).not.toBeNull();
  });

  it("shows the compact card only for the shown tile", () => {
    seedStoryTiles();
    act(() => {
      useViewerStore.setState({ storyHudShown: { [A]: true } });
    });
    const { container } = renderPanels();
    expect(container.querySelector(`[data-testid="story-hud-${A}"]`)).not.toBeNull(); // shown
    expect(container.querySelector(`[data-testid="story-hud-${B}"]`)).toBeNull(); // hidden
    expect(container.querySelector('[data-testid="mock-markdown-viewer"]')).toBeNull(); // compact
  });

  it("expands only the shown+expanded tile, over its own terminal", () => {
    seedStoryTiles();
    act(() => {
      useViewerStore.setState({
        storyHudShown: { [A]: true, [B]: true },
        storyHudExpanded: { [A]: true },
      });
    });
    const { container } = renderPanels();
    // A is expanded → its own story renders once; B stays a compact card.
    const viewers = container.querySelectorAll('[data-testid="mock-markdown-viewer"]');
    expect(viewers).toHaveLength(1);
    expect(viewers[0]).toHaveTextContent("/a/story.md");
    expect(container.querySelector(`[data-testid="story-hud-expanded-${A}"]`)).not.toBeNull();
    expect(container.querySelector(`[data-testid="story-hud-${A}"]`)).toBeNull(); // not compact
    expect(container.querySelector(`[data-testid="story-hud-${B}"]`)).not.toBeNull(); // compact
  });
});

// ---------------------------------------------------------------------------
// SW-EW-40 — per-epic foreground mosaic. In grouped tray mode the FRAME layer
// renders only the selected epic's tiles; the HOST layer spans the full open
// set so the other epics' terminals PARK off-DOM (no frame) rather than
// unmounting — a kill-free epic swap (AC-2/AC-4/AC-6/AC-7).
// ---------------------------------------------------------------------------
describe("WorkspaceTerminalPanels — per-epic mosaic (SW-EW-40)", () => {
  // A ∈ epic alpha, B ∈ epic beta (via the rail-key index the store caches).
  function seedGrouped(selected = "alpha") {
    useViewerStore.setState({
      foregroundLayout: {},
      foregroundLayoutByRail: {},
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: selected },
      railKeyByWorkspaceId: { [TEAM]: { [A]: "alpha", [B]: "beta" } },
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [
          { id: A, kind: "story", cwd: "/a", status: "ready" },
          { id: B, kind: "story", cwd: "/b", status: "ready" },
        ],
      },
    });
  }
  const chrome = (c: HTMLElement, id: string) =>
    c.querySelector(`[data-testid="tile-chrome-${id}"]`);

  it("renders ONLY the selected epic's tile; the other epic's terminal renders no slot and keeps running (AC-2/AC-6)", () => {
    seedGrouped("alpha");
    const { container } = renderPanels();
    expect(chrome(container, A)).not.toBeNull(); // alpha is framed
    expect(chrome(container, B)).toBeNull(); // beta is NOT framed
    // Beta renders no terminal: its display stays parked in the terminal manager.
    expect(rec.mounts).toEqual({ "sid-a": 1 });
    expect(allStillRunning()).toBe(true);
  });

  it("switching the selected epic swaps the framed tile with NO remount and NO PTY kill (AC-4/AC-6)", () => {
    seedGrouped("alpha");
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const { container } = renderPanels();
    expect(chrome(container, A)).not.toBeNull();
    const nodeA = container.querySelector<HTMLElement>('[data-sid="sid-a"]');
    expect(nodeA).not.toBeNull();
    nodeA!.focus();
    expect(document.activeElement).toBe(nodeA);
    // Measure ONLY what the epic switch triggers (isolate from any earlier
    // test's fire-and-forget close reaps landing in this window).
    act(() => {
      // SW-EW-38 setSelectedEpicGroup drives this in production.
      useViewerStore.getState().setSelectedEpicGroup(TEAM, "beta");
    });
    expect(chrome(container, A)).toBeNull(); // alpha left the mosaic
    expect(chrome(container, B)).not.toBeNull(); // beta entered
    // The swap moved slots only (alpha's slot out, beta's in) — no kill/close IPC.
    expect(container.querySelector('[data-sid="sid-a"]')).toBeNull();
    expect(rec.visibility["sid-b"]?.at(-1)).toBe(true);
    expect(allStillRunning()).toBe(true);

    act(() => {
      useViewerStore.getState().setSelectedEpicGroup(TEAM, "alpha");
    });
    const shownA = container.querySelectorAll<HTMLElement>('[data-sid="sid-a"]');
    expect(shownA).toHaveLength(1);
    expect(rec.visibility["sid-a"]?.at(-1)).toBe(true);
    expect(document.activeElement).toBe(shownA[0]);
    expect(allStillRunning()).toBe(true);
  });

  it("per-rail arrangement: setForegroundPreset writes the selected rail's slot, not the flat slice (AC-3/AC-5)", () => {
    seedGrouped("alpha");
    renderPanels();
    act(() => {
      useViewerStore.getState().setForegroundPreset(TEAM, "grid");
    });
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM]?.alpha?.preset).toBe("grid");
    expect(useViewerStore.getState().foregroundLayout[TEAM]).toBeUndefined();
  });

  it("a selected epic with zero open members renders no tile frames, terminals still parked (AC-7)", () => {
    seedGrouped("alpha");
    const { container } = renderPanels();
    act(() => {
      useViewerStore.getState().setSelectedEpicGroup(TEAM, "gamma"); // empty epic
    });
    expect(chrome(container, A)).toBeNull();
    expect(chrome(container, B)).toBeNull();
    // No crash, no kill — the empty state is handled by the parent view.
    expect(allStillRunning()).toBe(true);
  });

  it("cold start into grouped BEFORE the rail index is built renders all tiles (no empty flash)", () => {
    seedGrouped("alpha");
    // Simulate the pre-first-sync window: index not yet built for the team.
    useViewerStore.setState({ railKeyByWorkspaceId: {} });
    const { container } = renderPanels();
    // Both terminals framed (full set) instead of an empty mosaic — scopes once
    // syncWorkspaceFrontmatter lands the index.
    expect(chrome(container, A)).not.toBeNull();
    expect(chrome(container, B)).not.toBeNull();
  });

  it("flat mode renders ALL open tiles regardless of a stray rail index (AC-1 no-regression)", () => {
    seedGrouped("alpha");
    // Flip to flat — the comingled lens must ignore railKeyByWorkspaceId entirely.
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    const { container } = renderPanels();
    expect(chrome(container, A)).not.toBeNull();
    expect(chrome(container, B)).not.toBeNull(); // both epics tile together
  });
});

// ---------------------------------------------------------------------------
// SLW-FX-18 — a single-story full-screen (console) tile keeps its story title +
// status dropdown. `hideIdentity = isConsole && !split` dropped the whole
// identity block for a lone console tile on the premise that a console top
// switcher owns identity — but the switcher on main (`IdentityMenu`)
// carries org·team ONLY, never the story title/status. The fix narrows the gate
// with `&& !getKindCapabilities(kind).statusBadge` so a kind that owns a
// settable story identity (only `story`) keeps its block; identity-less kinds
// (epic/root/generic) stay hidden exactly as before.
// ---------------------------------------------------------------------------
describe("WorkspaceTerminalPanels — single-story console keeps title + status (SLW-FX-18)", () => {
  const chrome = (c: HTMLElement, id: string) =>
    c.querySelector(`[data-testid="tile-chrome-${id}"]`);
  const label = (c: HTMLElement, id: string) => c.querySelector(`[data-testid="tile-label-${id}"]`);
  const statusCtl = (c: HTMLElement, id: string) =>
    c.querySelector(`[data-testid="tile-story-status-${id}"]`);

  // Full-screen (console) + a lone tile. In grouped mode the rail filter
  // (alpha ⇒ only A) makes `tiles.length === 1` even with B open; in flat mode
  // only A is opened. A is a ready story w/ a storyStatus so the dropdown is
  // eligible. `kind` overrides A's kind for the non-story cases (AC-4/AC-6).
  function seedConsoleSingle(kind: WorkspaceKind = "story", grouped = true) {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    useViewerStore.setState({
      openedTeams: [TEAM],
      foregroundLayout: {},
      foregroundLayoutByRail: {},
      layoutMode: "console",
      trayMode: { [TEAM]: grouped ? "grouped" : "flat" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      railKeyByWorkspaceId: { [TEAM]: { [A]: "alpha", [B]: "beta" } },
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: grouped
          ? [
              { id: A, kind, cwd: "/a", status: "ready", storyStatus: "in-progress" },
              { id: B, kind: "story", cwd: "/b", status: "ready" },
            ]
          : [{ id: A, kind, cwd: "/a", status: "ready", storyStatus: "in-progress" }],
      },
    });
  }

  const renderWithStatus = (onStoryStatusChange = vi.fn()) =>
    render(
      <DndContext>
        <WorkspaceTerminalPanels
          teamKey={TEAM}
          repoPath="/repo"
          onStoryStatusChange={onStoryStatusChange}
        />
      </DndContext>,
    );

  it("AC-1/AC-2: a single-story console tile shows the story title AND the status dropdown", () => {
    seedConsoleSingle("story");
    const { container } = renderWithStatus();
    // Exactly one framed tile (grouped alpha ⇒ A only) — the lone-console corner.
    expect(chrome(container, A)).not.toBeNull();
    expect(chrome(container, B)).toBeNull();
    // AC-1: story-title label present (no blank top-left).
    expect(label(container, A)).not.toBeNull();
    // AC-2: the status dropdown control is present for a ready story w/ storyStatus.
    expect(statusCtl(container, A)).not.toBeNull();
  });

  it("AC-3: a single STORY tile in FLAT console mode also keeps title + status (grouping-agnostic)", () => {
    seedConsoleSingle("story", /* grouped */ false);
    const { container } = renderWithStatus();
    expect(chrome(container, A)).not.toBeNull();
    expect(chrome(container, B)).toBeNull();
    expect(label(container, A)).not.toBeNull();
    expect(statusCtl(container, A)).not.toBeNull();
  });

  it("AC-4: a NON-story lone console tile (generic, statusBadge=false) still hides identity", () => {
    seedConsoleSingle("generic", /* grouped */ false);
    const { container } = renderWithStatus();
    expect(chrome(container, A)).not.toBeNull(); // still a framed tile…
    // …but the identity block stays omitted (hideIdentity stays true).
    expect(label(container, A)).toBeNull();
    expect(statusCtl(container, A)).toBeNull();
  });

  it("AC-6: an epic lone console tile keeps identity hidden (statusBadge=false)", () => {
    seedConsoleSingle("epic", /* grouped */ false);
    const { container } = renderWithStatus();
    expect(chrome(container, A)).not.toBeNull();
    expect(label(container, A)).toBeNull();
    expect(container.querySelector(`[data-testid="tile-epic-icon-${A}"]`)).toBeNull();
  });

  it("AC-5a: split (N≥2) console still renders the story identity (unchanged)", () => {
    seedConsoleSingle("story");
    // Put BOTH tiles in the selected epic ⇒ tiles.length===2 ⇒ split ⇒ identity always shows.
    useViewerStore.setState({ railKeyByWorkspaceId: { [TEAM]: { [A]: "alpha", [B]: "alpha" } } });
    const { container } = renderWithStatus();
    expect(chrome(container, A)).not.toBeNull();
    expect(chrome(container, B)).not.toBeNull();
    expect(label(container, A)).not.toBeNull();
    expect(statusCtl(container, A)).not.toBeNull();
  });

  it("AC-5b: a single story tile in NON-console (shared) mode renders identity (unchanged)", () => {
    seedConsoleSingle("story", /* grouped */ false);
    useViewerStore.setState({ layoutMode: "shared" });
    const { container } = renderWithStatus();
    expect(chrome(container, A)).not.toBeNull();
    expect(label(container, A)).not.toBeNull();
    expect(statusCtl(container, A)).not.toBeNull();
  });
});

describe("WorkspaceInitOverlay — inline workspace-init (SLW-CO-5 → SLW-CO-6)", () => {
  // happy-dom does no layout/paint, so we assert DOM presence/text + gating only
  // (positioning over xterm + the visual handoff are manual/E2E — per the story's
  // test strategy). AC-2/AC-3.
  it("shows the overlay while a story workspace is 'creating', hides it on 'ready'", () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [
          {
            id: A,
            kind: "story",
            cwd: "/a",
            status: "creating",
            setupCancellable: true,
            setupState: {
              kind: "step1",
              phase: "copying",
              done: 41 * 1024 * 1024,
              total: 112 * 1024 * 1024,
              unit: "bytes",
            },
            storyTitle: "My Story",
          },
        ],
      },
    });
    const { getByTestId, getByText, queryByTestId } = renderPanels();

    // Overlay present with heading + live sub-label + identity (AC-2/AC-3).
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
    expect(getByText("Setting up worktree…")).toBeTruthy();
    // SW-XS-10: the setup line — phase left, determinate count right.
    expect(getByTestId("setup-line-text").textContent).toBe("Copying");
    expect(getByTestId("setup-line-count").textContent).toBe("41 / 112 MB");
    expect(getByText("My Story")).toBeTruthy();

    // Flip to "ready" → overlay gone (the terminal takes over — AC-7).
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          [TEAM]: [{ ...s.workspaces[TEAM][0], status: "ready", setupState: undefined }],
        },
      }));
    });
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  // SLW-CO-6 (AC-3): the overlay is now generalized to ANY `status==="creating"`
  // kind. An epic 'creating' shows the overlay with the epic heading and NO
  // Cancel (its dir-ensure create is not cancellable → `setupCancellable` unset).
  it("shows the overlay with the epic heading (no Cancel) for an epic 'creating' workspace", () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [{ id: A, kind: "epic", cwd: "/a", status: "creating" }],
      },
    });
    const { getByTestId, getByText, queryByText } = renderPanels();
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
    expect(getByText("Setting up epic workspace…")).toBeTruthy();
    expect(queryByText("Cancel")).toBeNull();
  });

  it("Cancel tears down the creating workspace via closeWorkspace (AC-8)", () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [
          {
            id: A,
            kind: "story",
            cwd: "/a",
            status: "creating",
            setupCancellable: true,
            setupState: { kind: "step1", phase: "starting" },
          },
        ],
      },
    });
    const { getByText } = renderPanels();
    act(() => {
      (getByText("Cancel") as HTMLButtonElement).click();
    });
    // The just-mounted "creating" workspace is gone (no orphan).
    expect(useViewerStore.getState().workspaces[TEAM]?.find((w) => w.id === A)).toBeUndefined();
  });
});

// SLW-CO-7: the overlay now owns the WHOLE sync (create → materialize). It rides
// the per-surface `surface-sync-store` wave state (keyed by `cwd`), so a
// `needsSyncOverlay` workspace stays covered after the ready-flip until its
// materialization wave settles — then hands off to the terminal (synced) or the
// in-pane failure placeholder (failed). happy-dom: DOM presence/gating only.
describe("WorkspaceInitOverlay — overlay owns the whole sync (SLW-CO-7)", () => {
  const CWD = "/a";
  const seedReadyMaterializing = (extra?: Partial<Workspace>) => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [
          {
            id: A,
            kind: "story",
            cwd: CWD,
            // Post dir-create: CO-6 flips ready immediately; CO-7 keeps the
            // overlay via `needsSyncOverlay` until the surface wave settles.
            status: "ready",
            needsSyncOverlay: true,
            storyTitle: "My Story",
            ...extra,
          },
        ],
      },
    });
  };

  beforeEach(() => {
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
  });

  it("AC-1/AC-7: keeps the overlay up in the PRE-WAVE gap (ready-flipped, wave not started yet)", () => {
    // The flicker guard: status is "ready" and the surface has NO wave state yet
    // (undefined) — a naive `creating || syncing` gate would flash the bare
    // terminal here. `needsSyncOverlay` keeps the overlay covering the pane.
    seedReadyMaterializing();
    const { getByTestId } = renderPanels();
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
  });

  it("AC-1: stays up while syncing (no materialize count: the setup line speaks)", () => {
    seedReadyMaterializing();
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
    });
    const { getByTestId, queryByText } = renderPanels();
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
    expect(queryByText(/Preparing context…|Materializing/)).toBeNull();
  });

  it("AC-1: dismisses the overlay once the surface is materialized (synced)", () => {
    seedReadyMaterializing();
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
      useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: true });
    });
    const { queryByTestId } = renderPanels();
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("AC-5/AC-7: no overlay for an already-materialized workspace (needsSyncOverlay unset)", () => {
    // A re-open / on-disk-rehydrate: born ready, no materialize wave fires — the
    // opt-out. Even in the pre-wave `undefined` surface state, no overlay shows.
    seedReadyMaterializing({ needsSyncOverlay: undefined });
    const { queryByTestId } = renderPanels();
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("AC-6: a materialize wave FAILURE hands off to the in-pane error placeholder (no Retry)", () => {
    seedReadyMaterializing();
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
      useSurfaceSyncStore
        .getState()
        .markCompleted(CWD, 1, { ok: false, errorMessage: "disk full" });
    });
    const { getByTestId, queryByTestId } = renderPanels();
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
    expect(getByTestId("workspace-materialize-failed")).toBeTruthy();
    expect(queryByTestId(`workspace-error-retry-${A}`)).toBeNull();
  });

  it("review: latch is ONE-SHOT — after synced, a LATER wave on the same cwd does NOT re-cover the terminal", () => {
    seedReadyMaterializing();
    const { queryByTestId } = renderPanels();
    // Materialize completes → overlay drops AND the clear-on-synced effect fires,
    // dropping the latch off the record.
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
      useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: true });
    });
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
    expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBeUndefined();
    // A later unrelated wave (reverse-sync/reconcile) on the same cwd flips the
    // surface back to syncing — the overlay must STAY hidden (no re-cover).
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 2);
    });
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("review: materialize-start watchdog drops a stuck overlay (no wave EVER fires — surface stays undefined)", () => {
    vi.useFakeTimers();
    try {
      // needsSyncOverlay true, surface undefined, no wave — the hang case.
      seedReadyMaterializing();
      const { queryByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      // Nothing advances the surface; the start watchdog fires and drops the latch.
      act(() => {
        vi.advanceTimersByTime(20_000);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeNull();
      expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });

  it("review: watchdog does NOT fire once a wave is syncing (a slow/engine-queued materialize keeps the overlay up)", () => {
    vi.useFakeTimers();
    try {
      seedReadyMaterializing();
      // A wave STARTED (surface=syncing) but emits no progress for a long time
      // (semaphore-queued / slow disk). The overlay must stay up — trust the wave.
      act(() => {
        useSurfaceSyncStore.getState().markStarted(CWD, 1);
      });
      const { queryByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      act(() => {
        vi.advanceTimersByTime(60_000);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});

// ---------------------------------------------------------------------------
// SLW-CO-8 — the epic overlay rides the whole parallel multi-repo assembly.
// The overlay reads the epic-scoped batch aggregate (inheritance wave + N
// per-repo dev-env installs), not a single wave.
// ---------------------------------------------------------------------------
describe("WorkspaceInitOverlay — epic assembly batch (SLW-CO-8)", () => {
  const CWD = "/epic";
  const runs = [
    { owner: "acme", repo: "api", runId: "epic-e-acme-api" },
    { owner: "acme", repo: "web", runId: "epic-e-acme-web" },
  ];
  const seedEpic = () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [{ id: A, kind: "epic", cwd: CWD, status: "ready", needsSyncOverlay: true }],
      },
    });
  };
  const inheritanceSynced = () => {
    useSurfaceSyncStore.getState().markStarted(CWD, 1);
    useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: true });
  };

  beforeEach(() => {
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
    useEpicAssemblyStore.setState({ byCwd: {}, runIdToCwd: {}, pendingByRunId: {} });
  });

  it("AC-2/AC-3: overlay stays up with an aggregate label while installs run", () => {
    seedEpic();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true); // 1 of 2 done, web still running
      inheritanceSynced();
    });
    const { getByTestId, getByText } = renderPanels();
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
    expect(getByText("Assembling 2 repos — 1 of 2 ready · installing acme/web…")).toBeTruthy();
  });

  it("AC-2: overlay clears once the whole batch settles (inheritance + all installs)", () => {
    seedEpic();
    const { queryByTestId } = renderPanels();
    expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", true);
      inheritanceSynced();
    });
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
    expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBeUndefined();
  });

  it("AC-2: readiness follows assembly and setup, not a background context wave", () => {
    seedEpic();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", true);
      useSurfaceSyncStore.getState().markStarted(CWD, 1); // an unrelated wave, still running
    });
    const { queryByTestId } = renderPanels();
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("AC-4: a partial failure clears the overlay to a usable epic + in-pane flag", () => {
    seedEpic();
    const { queryByTestId, getByText } = renderPanels();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", false); // one repo failed
      inheritanceSynced();
    });
    // Overlay is gone (epic usable), but the partial-failure flag shows in-pane.
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
    expect(queryByTestId("epic-partial-failure")).toBeTruthy();
    expect(getByText(/acme\/web couldn't set up — reopen to retry/)).toBeTruthy();
  });

  it("AC-4: a total assembly failure shows the in-pane error, no Retry", () => {
    seedEpic();
    const { getByTestId, queryByTestId } = renderPanels();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.markEpicAssemblyError(CWD, "backend blew up");
    });
    expect(getByTestId("workspace-materialize-failed")).toBeTruthy();
    expect(queryByTestId(`workspace-error-retry-${A}`)).toBeNull();
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("AC-6: a draft epic (no repos) clears as soon as inheritance syncs", () => {
    seedEpic();
    const { queryByTestId } = renderPanels();
    act(() => {
      const s = useEpicAssemblyStore.getState();
      s.beginEpicAssembly(CWD);
      s.seedEpicAssembly(CWD, [], []); // no repos → no installs
      inheritanceSynced();
    });
    expect(queryByTestId("workspace-init-overlay")).toBeNull();
  });

  it("watchdog: a product assembly that never reports fails in-pane after the report bound", () => {
    vi.useFakeTimers();
    try {
      seedEpic();
      act(() => {
        useEpicAssemblyStore.getState().beginEpicAssembly(CWD); // begun, never reports
      });
      const { queryByTestId, getByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      // The story start-watchdog window does not apply: assembly is legitimately slow.
      act(() => {
        vi.advanceTimersByTime(MATERIALIZE_START_WATCHDOG_MS);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      // Past the report bound the silent assembly is a failure, not a forever-cover.
      act(() => {
        vi.advanceTimersByTime(EPIC_ASSEMBLY_REPORT_WATCHDOG_MS);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeNull();
      expect(getByTestId("workspace-materialize-failed")).toBeTruthy();
      expect(queryByTestId(`workspace-error-retry-${A}`)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("watchdog: a product assembly whose begin never arrived also fails after the bound", () => {
    vi.useFakeTimers();
    try {
      seedEpic(); // latch set, no batch entry at all
      const { queryByTestId, getByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      act(() => {
        vi.advanceTimersByTime(EPIC_ASSEMBLY_REPORT_WATCHDOG_MS);
      });
      expect(getByTestId("workspace-materialize-failed")).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("watchdog: does NOT fire while an install is still running (long npm ci)", () => {
    vi.useFakeTimers();
    try {
      seedEpic();
      act(() => {
        const s = useEpicAssemblyStore.getState();
        s.beginEpicAssembly(CWD);
        s.seedEpicAssembly(CWD, runs, []); // both installs running
        inheritanceSynced();
      });
      const { queryByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      act(() => {
        vi.advanceTimersByTime(60_000);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("watchdog (review H2): does NOT fire on a slow un-seeded assembly whose inheritance already synced", () => {
    vi.useFakeTimers();
    try {
      seedEpic();
      // The batch has BEGUN but not seeded (assemble still doing worktree-adds),
      // and its inheritance wave already synced early. This is legit slow work —
      // the watchdog must NOT clear the overlay (would reveal an un-ready epic).
      act(() => {
        useEpicAssemblyStore.getState().beginEpicAssembly(CWD);
        inheritanceSynced();
      });
      const { queryByTestId } = renderPanels();
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      act(() => {
        vi.advanceTimersByTime(60_000);
      });
      expect(queryByTestId("workspace-init-overlay")).toBeTruthy();
      expect(useViewerStore.getState().workspaces[TEAM][0].needsSyncOverlay).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("WorkspaceSetupBadge — background-setup decouple (RF-perf-14)", () => {
  it("shows a NON-blocking running badge over the live terminal (shell not hidden)", () => {
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: { status: "running", label: "Running setup…" },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container } = renderPanels();
    const badge = container.querySelector(`[data-testid="workspace-setup-running-${A}"]`);
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toContain("Running setup…");
    expect(badge?.querySelector(".animate-spin")).toBeNull();
    // The terminal frame is still rendered underneath (shell NOT hidden).
    expect(container.querySelector(`[data-testid="workspace-error-${A}"]`)).toBeNull();
  });

  it("SW-WT-12: renders the per-command checklist with a row per step and the exit code on failure", () => {
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: {
                status: "running",
                label: "Running setup…",
                steps: [
                  { index: 0, command: "pnpm install", status: "done", exitCode: 0 },
                  { index: 1, command: "codegraph init", status: "running" },
                  { index: 2, command: "cargo build", status: "queued" },
                  { index: 3, command: "flaky", status: "failed", exitCode: 2 },
                ],
              },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container, getByLabelText } = renderPanels();
    const checklist = container.querySelector(`[data-testid="workspace-setup-checklist"]`);
    expect(checklist).not.toBeNull();
    // One row per configured step.
    expect(checklist?.querySelectorAll("li").length).toBe(4);
    // The running step spins; the failed row carries its exit code.
    expect(checklist?.querySelector(".animate-spin")).not.toBeNull();
    expect(checklist?.textContent).toContain("exit 2");
    // Rows are individually labelled for screen readers (status per row).
    expect(getByLabelText("step 1, pnpm install, done")).toBeTruthy();
    expect(getByLabelText("step 4, flaky, failed")).toBeTruthy();
  });

  it("SW-WT-12: renders no checklist when there are no configured steps", () => {
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: { status: "running", label: "Running setup…" },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container } = renderPanels();
    expect(container.querySelector(`[data-testid="workspace-setup-checklist"]`)).toBeNull();
  });

  it("SW-WT-12: a checklist row discloses its output on demand", () => {
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: {
                status: "running",
                label: "Running setup…",
                steps: [{ index: 0, command: "pnpm install", status: "running" }],
              },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container, getByLabelText } = renderPanels();
    // Output region is hidden until the row is expanded.
    expect(container.textContent).not.toContain("may contain secrets");
    act(() => {
      getByLabelText("Show output for pnpm install").click();
    });
    // Expanding shows the secrets warning + the (empty-buffer) graceful fallback.
    expect(container.textContent).toContain("may contain secrets");
    expect(container.textContent).toContain("No output captured.");
  });

  it("discloses structured failure details by keyboard without remounting the terminal", async () => {
    const retrySpy = vi.spyOn(useViewerStore.getState(), "retryWorktreeSetup");
    const diagnostics = {
      overallExitCode: 1,
      failedResult: {
        command: "pnpm install --frozen-lockfile",
        passed: false,
        stdout: "preparing\n",
        stderr: "ERR_PNPM_NO_LOCKFILE\n",
        exit_code: 1,
        phase: "setup" as const,
        step_index: 0,
      },
      streams: {
        stdout: { originalLength: 100_000, truncated: true },
        stderr: { originalLength: 23, truncated: false },
      },
    };
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: { status: "failed", error: "npm ci failed", diagnostics },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container } = renderPanels();
    const failed = container.querySelector(`[data-testid="workspace-setup-failed-${A}"]`);
    expect(failed).not.toBeNull();
    const terminalNode = container.querySelector('[data-sid="sid-a"]');
    const details = failed?.querySelector("button[aria-controls]") as HTMLButtonElement;
    expect(details.getAttribute("aria-expanded")).toBe("false");
    details.focus();
    await userEvent.keyboard("{Enter}");
    expect(details.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(details);
    const region = container.querySelector(
      `[data-testid="workspace-setup-details-${A}"]`,
    ) as HTMLElement;
    expect(region.textContent).toContain("setup · 1");
    expect(region.textContent).toContain("pnpm install --frozen-lockfile");
    expect(region.textContent).toContain("ERR_PNPM_NO_LOCKFILE");
    expect(region.textContent).toContain("Setup output may contain secrets");
    expect(region.textContent).toContain("Truncated from 100,000 characters");
    expect(container.querySelector('[data-sid="sid-a"]')).toBe(terminalNode);
    expect(rec.mounts["sid-a"]).toBe(1);
    fireEvent.keyDown(terminalNode!, { key: "x" });
    expect(rec.keys).toEqual(["x"]);
    expect(terminalNode?.textContent).not.toContain("ERR_PNPM_NO_LOCKFILE");
    expect(region.parentElement?.className).toContain("max-h-[calc(100%-1rem)]");
    expect(region.parentElement?.className).toContain("inset-x-2");

    await userEvent.keyboard("{Escape}");
    expect(details.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(details);

    await userEvent.keyboard(" ");
    expect(details.getAttribute("aria-expanded")).toBe("true");

    const retry = container.querySelector(
      `[data-testid="workspace-setup-retry-${A}"]`,
    ) as HTMLButtonElement;
    expect(retry).not.toBeNull();
    act(() => {
      fireEvent.click(retry);
    });
    expect(retrySpy).toHaveBeenCalledWith(TEAM, A);
    expect(retrySpy).toHaveBeenCalledTimes(1);
    expect(container.querySelector(`[data-testid="workspace-setup-details-${A}"]`)).toBeNull();
  });

  it("shows a visible legacy error fallback and static running status", () => {
    act(() => {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: { status: "failed", error: "legacy failure" },
            },
            s.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container } = renderPanels();
    fireEvent.click(container.querySelector("button[aria-controls]")!);
    expect(container.textContent).toContain("legacy failure");
    expect(container.querySelector(".animate-spin")).toBeNull();
  });

  it("explains a structured failure with no exit code or captured output", () => {
    act(() => {
      useViewerStore.setState((state) => ({
        workspaces: {
          ...state.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup: {
                status: "failed",
                error: "quiet failure",
                diagnostics: {
                  overallExitCode: -1,
                  failedResult: {
                    command: "quiet-command",
                    passed: false,
                    stdout: "",
                    stderr: "",
                    exit_code: null,
                    phase: "setup",
                    step_index: 0,
                  },
                  streams: {
                    stdout: { originalLength: 0, truncated: false },
                    stderr: { originalLength: 0, truncated: false },
                  },
                },
              },
            },
            state.workspaces[TEAM][1],
          ],
        },
      }));
    });
    const { container } = renderPanels();
    fireEvent.click(container.querySelector("button[aria-controls]")!);

    const region = container.querySelector(`[data-testid="workspace-setup-details-${A}"]`)!;
    expect(region.textContent).toContain("Exit codeUnavailable");
    expect(region.textContent).toContain("No captured output");
    expect(region.textContent).not.toContain("Standard output");
    expect(region.textContent).not.toContain("Standard error");
  });

  it("retries while collapsed and keeps a subsequent failure collapsed", () => {
    const retrySpy = vi.spyOn(useViewerStore.getState(), "retryWorktreeSetup");
    retrySpy.mockClear();
    const setBackgroundSetup = (backgroundSetup: Workspace["backgroundSetup"]) => {
      useViewerStore.setState((state) => ({
        workspaces: {
          ...state.workspaces,
          [TEAM]: [
            {
              id: A,
              kind: "story",
              cwd: "/a",
              status: "ready",
              backgroundSetup,
            },
            state.workspaces[TEAM][1],
          ],
        },
      }));
    };
    act(() => setBackgroundSetup({ status: "failed", error: "first failure" }));
    const { container } = renderPanels();

    expect(container.querySelector(`[data-testid="workspace-setup-details-${A}"]`)).toBeNull();
    act(() => {
      fireEvent.click(container.querySelector(`[data-testid="workspace-setup-retry-${A}"]`)!);
    });
    expect(retrySpy).toHaveBeenCalledOnce();

    act(() => setBackgroundSetup({ status: "running", label: "Running setup…" }));
    act(() => setBackgroundSetup({ status: "failed", error: "second failure" }));
    const details = container.querySelector("button[aria-controls]");
    expect(details?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(`[data-testid="workspace-setup-details-${A}"]`)).toBeNull();
  });

  it("renders NO badge when backgroundSetup is absent (the common case)", () => {
    const { container } = renderPanels();
    expect(container.querySelector(`[data-testid="workspace-setup-running-${A}"]`)).toBeNull();
    expect(container.querySelector(`[data-testid="workspace-setup-failed-${A}"]`)).toBeNull();
  });
});

describe("WorkspaceInitOverlay — Cancel and phase announcement", () => {
  const theme = TERMINAL_THEMES["github-dark"];

  afterEach(() => cleanup());

  it("Cancel calls onCancel", () => {
    const onCancel = vi.fn();
    const { getByRole } = render(
      <WorkspaceInitOverlay
        kind="story"
        themeConfig={theme}
        setupLabel="Preparing context… 1 / 4"
        onCancel={onCancel}
      />,
    );
    fireEvent.click(getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("announces the phase via a polite sr-only live region (AC-9)", () => {
    const { getByTestId, rerender } = render(
      <WorkspaceInitOverlay
        kind="story"
        themeConfig={theme}
        setupLabel="Checking worktree…"
        announce="Checking worktree…"
      />,
    );
    const live = getByTestId("workspace-init-announce");
    expect(live.getAttribute("aria-live")).toBe("polite");
    expect(live.className).toContain("sr-only");
    expect(live.textContent).toBe("Checking worktree…");

    // phase change → the announcement text swaps (one announcement per phase)
    rerender(
      <WorkspaceInitOverlay kind="story" themeConfig={theme} announce="Setting up worktree" />,
    );
    expect(getByTestId("workspace-init-announce").textContent).toBe("Setting up worktree");
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-DV-3 — the standalone worktree editor is actually mounted on the tile.
// ────────────────────────────────────────────────────────────────────────────

describe("WorkspaceTerminalPanels — worktree editor mount (SLW-DV-3)", () => {
  it("mounts WorktreeEditorSurface on every tile, threading each tile's focus state", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const view = renderPanels();
    // The surface is wired into the frame for each tile — deleting the mount line
    // makes these markers vanish. A is the active workspace → focused; B is not.
    const focused = view.getByTestId(`worktree-editor-surface-${A}`);
    const unfocused = view.getByTestId(`worktree-editor-surface-${B}`);
    expect(focused).toHaveAttribute("data-focused", "true");
    expect(unfocused).toHaveAttribute("data-focused", "false");
  });
});

// SW-LN-29 (M-28): the terminal body feeds the materialize state to each
// terminal (which holds a typed command until context lands) and to the chip.
describe("SW-LN-29 — context-landing chip + command hold wiring", () => {
  const CWD = "/a";
  const seed = () =>
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A },
      terminalVisible: true,
      workspaces: {
        [TEAM]: [{ id: A, kind: "story", cwd: CWD, status: "ready", needsSyncOverlay: true }],
      },
    });

  beforeEach(() => {
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
  });

  it("tells the terminal context is landing, then that it landed; the chip follows", () => {
    seed();
    const { getByTestId } = renderPanels();
    expect(rec.contextLanding["sid-a"]).toBe(true);
    expect(getByTestId("context-landing-chip")).toHaveTextContent("context landing…");
    act(() => rec.onContextHold["sid-a"]?.());
    expect(getByTestId("context-landing-chip")).toHaveTextContent(/your command runs/);
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
      useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: true });
    });
    expect(rec.contextLanding["sid-a"]).toBe(false);
    expect(getByTestId("context-landing-chip")).toHaveTextContent("context ready");
  });

  it("a hold timeout shows 'context not ready' until the context really lands", () => {
    seed();
    const { getByTestId } = renderPanels();
    act(() => rec.onContextHoldTimeout["sid-a"]?.());
    expect(getByTestId("context-landing-chip")).toHaveTextContent(
      "context not ready — your command ran without it",
    );
    act(() => {
      useSurfaceSyncStore.getState().markStarted(CWD, 1);
      useSurfaceSyncStore.getState().markCompleted(CWD, 1, { ok: true });
    });
    expect(getByTestId("context-landing-chip")).toHaveTextContent("context ready");
  });
});

/** This Mac's terminal server admitted live, or unreachable while the app reconnects. */
const setLocalSidecar = (state: "unavailable" | "live") =>
  useViewerStore.setState((s) => ({
    terminalLayout: {
      ...s.terminalLayout,
      admission: {
        ...s.terminalLayout.admission,
        "local/sidecar":
          state === "live"
            ? {
                member: { machineId: "local", slot: "sidecar" },
                revision: "9",
                state: "live",
                namespace: { serverId: "s", epoch: "e", generation: "9" },
                historical: null,
              }
            : {
                member: { machineId: "local", slot: "sidecar" },
                revision: "8",
                state: "unavailable",
                namespace: null,
                historical: null,
              },
      },
    },
  }));

// A workspace with no live terminals (a restored one whose terminals ended, or a
// fresh one) opens EMPTY: nothing auto-starts; the user starts a terminal, which
// opens through the workspace's host.
describe("WorkspaceTerminalPanels — empty workspace", () => {
  const E = "e-1-empty";
  const seedEmpty = (fields: Partial<Workspace> = {}) =>
    useViewerStore.setState({
      heldTerminalOpens: {},
      terminalHostsUnreachable: false,
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: E },
      terminalVisible: true,
      workspaces: { [TEAM]: [{ id: E, kind: "story", cwd: "/e", status: "ready", ...fields }] },
    });

  it("shows 'No terminals running' and starts nothing on its own", () => {
    seedEmpty();
    const { getByTestId } = renderPanels();
    expect(getByTestId("workspace-terminal-empty")).toHaveTextContent("No terminals running");
    expect(layoutHost().opened).toHaveLength(0);
    expect(rec.mounts).toEqual({});
  });

  it("its primary action is New terminal, with the live New Terminal Tab shortcut", () => {
    seedEmpty();
    const { getByTestId } = renderPanels();
    expect(getByTestId("workspace-terminal-empty-new")).toHaveTextContent(/^New terminal/);
    expect(getByTestId("workspace-terminal-empty-new-shortcut")).toHaveTextContent(
      formatKeystrokeDisplay("CmdOrCtrl+T"),
    );
    // Rebound, the hint follows.
    act(() =>
      useShortcutsStore.setState({ shortcuts: { "viewer.new-terminal-tab": "CmdOrCtrl+Shift+N" } }),
    );
    expect(getByTestId("workspace-terminal-empty-new-shortcut")).toHaveTextContent(
      formatKeystrokeDisplay("CmdOrCtrl+Shift+N"),
    );
    act(() => useShortcutsStore.setState({ shortcuts: {} }));
  });

  it("'New terminal' opens a tab through the host, which replaces the empty state", async () => {
    seedEmpty();
    const { getByTestId, queryByTestId } = renderPanels();
    await act(async () => {
      fireEvent.click(getByTestId("workspace-terminal-empty-new"));
    });
    expect(layoutHost().opened.map((r) => r.workspace)).toEqual([`${TEAM}|${E}`]);
    expect(queryByTestId("workspace-terminal-empty")).toBeNull();
    expect(Object.values(rec.mounts)).toEqual([1]);
  });

  it("while the terminal hosts have not answered, it says it is reconnecting — never a false empty", () => {
    seedEmpty();
    useViewerStore.setState({
      terminalLayout: { hydrated: false, admission: {}, members: {}, byKey: {} },
      terminalHostsUnreachable: true,
    });
    const { getByTestId, queryByTestId } = renderPanels();
    expect(getByTestId("terminal-hosts-reconnecting")).toHaveTextContent(
      "Reconnecting to the terminal server…",
    );
    expect(queryByTestId("workspace-terminal-empty")).toBeNull();
    expect(layoutHost().opened).toHaveLength(0);
  });

  it("a terminal held for a terminal-server restart shows over the pane", () => {
    seedEmpty();
    useViewerStore.setState({ heldTerminalOpens: { [`${TEAM}|${E}`]: 1 } });
    const { getByTestId } = renderPanels();
    expect(getByTestId("workspace-terminal-held")).toHaveTextContent(
      "This terminal will start once the terminal server restarts.",
    );
    expect(
      getByTestId("workspace-terminal-held").closest('[data-surface="terminal"]'),
    ).not.toBeNull();
  });

  it("is hidden while the workspace is still being set up", () => {
    seedEmpty({ status: "creating", setupState: { kind: "step1", phase: "starting" } });
    const { getByTestId, queryByTestId } = renderPanels();
    expect(getByTestId("workspace-init-overlay")).toBeTruthy();
    expect(queryByTestId("workspace-terminal-empty")).toBeNull();
  });
});

describe("WorkspaceTerminalPanels — reconnecting to This Mac's terminal server", () => {
  it("keeps the tabs, says the terminals keep running, escalates without a restart, and clears when live", async () => {
    vi.useFakeTimers();
    try {
      setLocalSidecar("unavailable");
      const { container, queryByText } = renderPanels();
      // Workspace A's pane, which runs a terminal on This Mac.
      const banner = () =>
        container.querySelector(
          `[data-testid="workspace-panels-${A}"] [data-testid="terminal-reconnecting-banner"]`,
        );
      // Nothing for a blip: the server is asked whether it still runs first.
      expect(banner()).toBeNull();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
      });
      expect(banner()).toHaveTextContent(
        "Reconnecting to the terminal server… Your terminals keep running there.",
      );
      expect(allStillRunning()).toBe(true);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(31_000);
      });
      expect(banner()).toHaveTextContent(
        "Still reconnecting to the terminal server (31s). Your terminals and agents keep running. If this doesn't clear, quit and reopen CodeSense; that reconnects without stopping them.",
      );
      expect(queryByText(/Restart terminal server/)).toBeNull();
      act(() => setLocalSidecar("live"));
      expect(banner()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("says nothing while the server was stopped on purpose, even over running tabs", async () => {
    const commands = await import("@/tauri/commands");
    vi.mocked(commands.terminalServerOverview).mockResolvedValue({
      state: "stopped",
    } as Awaited<ReturnType<typeof commands.terminalServerOverview>>);
    vi.useFakeTimers();
    try {
      setLocalSidecar("unavailable");
      const { queryByTestId } = renderPanels();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(20_000);
      });
      expect(queryByTestId("terminal-reconnecting-banner")).toBeNull();
    } finally {
      vi.useRealTimers();
      vi.mocked(commands.terminalServerOverview).mockResolvedValue({
        state: "unreachable",
      } as Awaited<ReturnType<typeof commands.terminalServerOverview>>);
    }
  });

  it("says nothing over a pane whose terminals here have all ended", async () => {
    const E = "e-1-ended";
    useViewerStore.setState((s) => ({
      activeWorkspace: { [TEAM]: E },
      workspaces: {
        [TEAM]: [...s.workspaces[TEAM]!, { id: E, kind: "story", cwd: "/e", status: "ready" }],
      },
    }));
    seedTerminalTabs(TEAM, E, [{ session: "sid-ended", exit: { code: 0, signal: null } }]);
    vi.useFakeTimers();
    try {
      setLocalSidecar("unavailable");
      const { container } = renderPanels();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
      });
      expect(
        container.querySelector(
          `[data-testid="workspace-panels-${E}"] [data-testid="terminal-reconnecting-banner"]`,
        ),
      ).toBeNull();
      // A pane still running terminals here does say it.
      expect(
        container.querySelector(
          `[data-testid="workspace-panels-${A}"] [data-testid="terminal-reconnecting-banner"]`,
        ),
      ).not.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("an empty pane says it is reconnecting while the server runs unanswered, not 'No terminals running'", async () => {
    const E = "e-1-empty-lost";
    useViewerStore.setState((s) => ({
      activeWorkspace: { [TEAM]: E },
      workspaces: {
        [TEAM]: [...s.workspaces[TEAM]!, { id: E, kind: "story", cwd: "/e", status: "ready" }],
      },
    }));
    vi.useFakeTimers();
    try {
      setLocalSidecar("unavailable");
      const { getByTestId, queryByTestId } = renderPanels();
      // Never a false "No terminals running" while the server is being asked about.
      expect(queryByTestId("workspace-terminal-empty")).toBeNull();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
      });
      expect(getByTestId("terminal-hosts-reconnecting")).toBeTruthy();
      expect(queryByTestId("workspace-terminal-empty")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("WorkspaceTerminalPanels — tiles follow the team's shared order", () => {
  it("a preset layout draws its tiles in the stored order, not by id", () => {
    const C = "e-1-c";
    useViewerStore.setState((s) => ({
      workspaces: {
        [TEAM]: [...s.workspaces[TEAM]!, { id: C, kind: "story", cwd: "/c", status: "ready" }],
      },
      foregroundLayout: {
        [TEAM]: { sizes: [], direction: "row", preset: "columns", root: null },
      },
      trayBaseOrderByTeam: { [TEAM]: [A, B, C] },
      workspaceOrderByTeam: { [TEAM]: [C, A, B] },
    }));
    seedTerminalTabs(TEAM, C, ["sid-c"]);
    const { container } = renderPanels();
    const tiles = Array.from(container.querySelectorAll('[data-testid^="workspace-panels-"]')).map(
      (el) => el.getAttribute("data-testid")!.replace("workspace-panels-", ""),
    );
    expect(tiles).toEqual([C, A, B]);
  });
});
