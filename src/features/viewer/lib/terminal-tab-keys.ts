// SPDX-License-Identifier: MIT
import { beginContextNavigation } from "@/shared/lib/context-navigation";
import { create } from "zustand";
import { toast } from "sonner";

import { CLI_HOST_SELECTOR } from "./cli-host";
import { recordContextChange } from "./context-switch";

import {
  activeForegroundTiles,
  createTerminalTab,
  expandedWorkspaceId,
  selectExpandedWorkspaceId,
  getViewerState,
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  shouldRenderTabGrid,
  subscribeViewerState,
  workspacePanedTabIds,
  type TerminalTab,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import {
  getNeedsYouState,
  isNeedsYouNews,
  selectSessionsNeedingYou,
  type SessionNeedsYou,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { getPendingAttentionTarget } from "@/features/viewer/lib/attention-alerts-controller";
import { popClosedTab, type ClosedTabEntry } from "@/features/viewer/lib/closed-tab-history";
import {
  activateAndFocus,
  focusActiveWorkspaceTerminal,
} from "@/features/viewer/lib/focus-cascade";
import {
  focusQuestionIn,
  focusTileQuestion,
  isFocusInTileQuestion,
} from "@/features/viewer/lib/tile-question-focus";
import { activeWorkspaceIdForTeam } from "@/shared/lib/team-key";
import { flashBlockedNavForWorkspace } from "@/features/viewer/lib/blocked-nav-cue";
import { gridFitsCells } from "@/features/viewer/lib/tab-grid-layout";
import { isSplitActive } from "@/features/viewer/lib/foreground-tiles";
import { isRemoteWorkspace, remoteWorkspaceDriver } from "@/shared/lib/workspace-location";
import { queryShown } from "@/shared/lib/warm-scopes";

/**
 * The keyboard verbs on terminal tabs: new, split, rename, find, clear, jump to
 * tab N, reopen the last closed tab, the expand key, and "jump to
 * what needs me". Each shortcut-registry action is a thin call into here, so the
 * registry, the capture-phase terminal intercept and the tests share one
 * implementation.
 *
 * Tabs are the terminal host's layout: every read goes through the layout
 * selectors, every change through a layout verb of the viewer store.
 *
 * Every verb acts on the focused tile (the team's active workspace) and its active
 * tab, except where keyboard focus sits on a specific terminal — then that one is
 * the target. Rename acts only on the tab pill that holds focus. Each returns
 * whether it did anything, so the registry can cue a blocked key.
 */

/** Marks a tab pill with its tab id, so a keyboard verb can target the pill that holds focus. */
export const TERMINAL_TAB_ID_ATTR = "data-terminal-tab-id";
/** Marks a terminal body with its session id, for the same reason. */
export const TERMINAL_SESSION_ID_ATTR = "data-terminal-session-id";
/** The CLI's terminal area (tile chrome, tab bars and terminal bodies). */
const TERMINAL_AREA_SELECTOR = CLI_HOST_SELECTOR;

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

interface ActiveTab {
  workspaceId: string;
  tabs: TerminalTab[];
  tab: TerminalTab | undefined;
}

/** The focused tile — an open workspace — with its tabs and active tab. */
function activeTabOf(teamKey: string): ActiveTab | null {
  const s = getViewerState();
  const workspaceId = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
  if (!s.workspaces[teamKey]?.some((w) => w.id === workspaceId)) return null;
  const tabs = selectWorkspaceTabs(s, teamKey, workspaceId);
  const activeId = selectActiveTerminalTab(s, teamKey, workspaceId);
  return { workspaceId, tabs, tab: tabs.find((t) => t.id === activeId) ?? tabs[0] };
}

function focusedElement(): Element | null {
  return typeof document === "undefined" ? null : document.activeElement;
}

/** Keyboard focus is somewhere in the terminal area (a terminal, a tab pill, a find bar). */
export function isFocusInTerminalArea(): boolean {
  return focusedElement()?.closest(TERMINAL_AREA_SELECTOR) != null;
}

/** The tab whose pill holds keyboard focus. */
function focusedPillTabId(): string | undefined {
  return (
    focusedElement()?.closest(`[${TERMINAL_TAB_ID_ATTR}]`)?.getAttribute(TERMINAL_TAB_ID_ATTR) ??
    undefined
  );
}

/** The session a verb acts on: the focused terminal's, else the focused tile's active tab's. */
function targetSessionId(teamKey: string): string | undefined {
  const body = focusedElement()?.closest(`[${TERMINAL_SESSION_ID_ATTR}]`);
  return body?.getAttribute(TERMINAL_SESSION_ID_ATTR) ?? activeTabOf(teamKey)?.tab?.sessionId;
}

// ---------------------------------------------------------------------------
// Showing a tab — wherever it is
// ---------------------------------------------------------------------------

type Where = "open" | "minimized" | "gone";

/** Where a workspace is: an open tile, a tile minimized in the tray, or gone. */
function whereIs(teamKey: string, workspaceId: string): Where {
  const s = getViewerState();
  if (s.workspaces[teamKey]?.some((w) => w.id === workspaceId)) return "open";
  if (s.softClosedWorkspaces[teamKey]?.[workspaceId]) return "minimized";
  return "gone";
}

/**
 * Put `workspaceId` on screen as the focused tile, keeping its sessions: a
 * minimized tile is RESTORED from the tray (never its session adopted into
 * another tile). Resolves false when it is gone.
 */
async function bringOnScreen(teamKey: string, workspaceId: string): Promise<boolean> {
  const s = getViewerState();
  const where = whereIs(teamKey, workspaceId);
  if (where === "gone") return false;
  recordContextChange("nav", () => {
    if (where === "open") {
      if (s.activeWorkspace[teamKey] !== workspaceId) s.switchWorkspace(teamKey, workspaceId);
    } else {
      s.activateTrayWorkspace(teamKey, workspaceId);
    }
  });
  return whereIs(teamKey, workspaceId) === "open";
}

type Landing = "terminal" | "question";

/** Focus a tab just brought on screen: its tile's question banner (falling back
 *  to the terminal if the banner doesn't come up), or its terminal. */
function land(teamKey: string, workspaceId: string, focus: Landing): void {
  getViewerState().expandTerminalOnOpen(teamKey);
  if (focus === "terminal") {
    activateAndFocus({ teamKey, workspaceId });
    return;
  }
  // Mint the activation without a terminal focus, so a pending one can't land on
  // top of the banner.
  activateAndFocus({ teamKey, workspaceId, focus: "focus-elsewhere" });
  void focusQuestionIn(workspaceId).then((ok) => {
    if (!ok) activateAndFocus({ teamKey, workspaceId });
  });
}

/**
 * Show one running tab, focused — wherever it is: on an open tile, on a tile
 * minimized in the tray, or Hidden under a tile's chip. Resolves false when the
 * tab no longer exists.
 */
export async function showTab(
  teamKey: string,
  workspaceId: string,
  tabId: string,
  focus: Landing = "terminal",
  stillHere: () => boolean = () => true,
): Promise<boolean> {
  const has = (tabs: TerminalTab[]) => tabs.some((t) => t.id === tabId);
  const s = getViewerState();
  const hidden = has(selectHiddenTerminalTabs(s, teamKey, workspaceId));
  if (!hidden && !has(selectWorkspaceTabs(s, teamKey, workspaceId))) return false;
  if (!(await bringOnScreen(teamKey, workspaceId))) return false;
  if (!stillHere()) return false;
  if (hidden) getViewerState().restoreHiddenTerminalTab(teamKey, workspaceId, tabId);
  else getViewerState().setActiveTerminalTab(teamKey, tabId, workspaceId);
  land(teamKey, workspaceId, focus);
  return true;
}

// ---------------------------------------------------------------------------
// New tab
// ---------------------------------------------------------------------------

/**
 * A snapshot of where the user is when a verb starts, so a focus that has to wait
 * (a held open, a door opening) lands only if they are still there: the same tile
 * active, and keyboard focus still in the terminal area (or nowhere).
 */
export function whereTheUserIs(teamKey: string): () => boolean {
  const workspaceId = getViewerState().activeWorkspace[teamKey];
  return () => {
    if (getViewerState().activeWorkspace[teamKey] !== workspaceId) return false;
    const el = focusedElement();
    return !el || el === document.body || isFocusInTerminalArea();
  };
}

/** Open a new terminal tab in `workspaceId` (by default the focused tile) and put
 *  the cursor in it — unless, by the time the host has opened it, the user has
 *  moved on. A workspace showing its tabs side by side gets the new terminal as
 *  another cell: a tab outside the grid would be active yet never drawn. */
export function newTerminalTab(teamKey: string, workspaceId?: string): boolean {
  const target = workspaceId ?? activeTabOf(teamKey)?.workspaceId;
  if (!target) return false;
  beginContextNavigation();
  void openNewTab(teamKey, target, whereTheUserIs(teamKey));
  return true;
}

async function openNewTab(
  teamKey: string,
  workspaceId: string,
  stillHere: () => boolean,
): Promise<void> {
  // A workspace on a machine gets its new tab there, at its worktree.
  const record = getViewerState().workspaces[teamKey]?.find((w) => w.id === workspaceId);
  if (isRemoteWorkspace(record)) {
    if (record.status === "ready")
      remoteWorkspaceDriver()?.openTerminal({ teamKey, workspaceId, location: record.location });
    return;
  }
  const tab = createTerminalTab();
  try {
    await getViewerState().addTerminalTab(teamKey, tab, workspaceId);
  } catch {
    return; // refused — the host's refusal is already logged
  }
  await showNewTab(teamKey, workspaceId, tab.sessionId, stillHere);
}

/** Said when a new terminal can't join the side-by-side cells readably. */
export const NEW_TAB_NO_ROOM = "No room to show another tab at once; showing one tab at a time";

/**
 * Show a just-opened tab — local or on a remote Machine — and put the cursor
 * in it, unless the user has moved on. A workspace showing its tabs side by
 * side gets it as another cell: a tab outside the grid would be active yet
 * never drawn. Only while every cell stays readable; otherwise the workspace
 * goes back to tabs, where the new (active) tab is the one shown, and says so.
 */
export async function showNewTab(
  teamKey: string,
  workspaceId: string,
  session: string,
  stillHere: () => boolean,
): Promise<void> {
  if (shouldRenderTabGrid(getViewerState(), teamKey, workspaceId)) {
    const tabsNow = () => selectWorkspaceTabs(getViewerState(), teamKey, workspaceId);
    if (!(await whenViewer(() => tabsNow().some((t) => t.id === session)))) return;
    if (roomForSplit(teamKey, workspaceId)) {
      getViewerState().paneOutTerminalTab(teamKey, workspaceId, session);
    } else {
      forgetZoomedCells(teamKey, workspaceId);
      rememberHeadingOut(teamKey, workspaceId, true);
      getViewerState().collapseTabGridToTabs(teamKey, workspaceId);
      getViewerState().setActiveTerminalTab(teamKey, session, workspaceId);
      toast(NEW_TAB_NO_ROOM);
    }
    await whenViewer(
      () => selectActiveTerminalTab(getViewerState(), teamKey, workspaceId) === session,
    );
  }
  if (stillHere()) focusActiveWorkspaceTerminal(teamKey);
}

// ---------------------------------------------------------------------------
// Split — a new terminal beside the focused one, side by side
// ---------------------------------------------------------------------------

/** How long a split waits for its host to announce the new tab. */
const SPLIT_TAB_WAIT_MS = 10_000;

/** Resolves true once `holds()` is true of the viewer store, false if it isn't
 *  within `timeoutMs`. */
function whenViewer(holds: () => boolean, timeoutMs = SPLIT_TAB_WAIT_MS): Promise<boolean> {
  if (holds()) return Promise.resolve(true);
  return new Promise((resolve) => {
    const stop = (result: boolean) => {
      clearTimeout(timer);
      unsubscribe();
      resolve(result);
    };
    const timer = setTimeout(() => stop(false), timeoutMs);
    const unsubscribe = subscribeViewerState(() => {
      if (holds()) stop(true);
    });
  });
}

/** The tab a keyboard verb acts on in `workspaceId`: the one whose terminal holds
 *  focus there, else the workspace's active tab. */
function focusedTabIn(teamKey: string, workspaceId: string): string | undefined {
  const tabs = selectWorkspaceTabs(getViewerState(), teamKey, workspaceId);
  const sessionId = targetSessionId(teamKey);
  return (
    tabs.find((t) => t.sessionId === sessionId)?.id ??
    selectActiveTerminalTab(getViewerState(), teamKey, workspaceId) ??
    tabs[0]?.id
  );
}

/** Keyboard focus is in a text field or editor outside the terminal area — where
 *  ⌘D is that editor's own key (a code editor's "select next occurrence"). */
export function isFocusInOtherEditor(): boolean {
  const el = focusedElement();
  // A terminal's own input is an xterm textarea: ⌘D there is the split.
  if (!el || isFocusInTerminalArea() || el.closest(".xterm")) return false;
  return (
    el.closest(
      "input, textarea, select, [contenteditable=''], [contenteditable='true'], .cm-editor",
    ) != null
  );
}

/** Said when a split is refused: the tile can't fit another readable cell. */
export const SPLIT_NO_ROOM = "No room for another terminal here";
/** Said when the host opened the split's terminal but never announced it. */
export const SPLIT_NOT_PLACED = "New terminal opened but couldn't be placed";

/** The on-screen size of the workspace's terminal pane (0×0 when not shown). */
function paneSize(workspaceId: string): { width: number; height: number } {
  if (typeof document === "undefined") return { width: 0, height: 0 };
  const pane = queryShown(`[data-testid="workspace-panels-${CSS.escape(workspaceId)}"]`);
  const rect = pane?.getBoundingClientRect();
  return { width: rect?.width ?? 0, height: rect?.height ?? 0 };
}

/** Whether splitting in `workspaceId` leaves every cell above the readable
 *  floor. A tile not yet expanded splits into two cells of the expanded area. */
function roomForSplit(teamKey: string, workspaceId: string): boolean {
  const s = getViewerState();
  if (expandedWorkspaceId(s, teamKey) !== workspaceId) return true;
  const cells = shouldRenderTabGrid(s, teamKey, workspaceId)
    ? workspacePanedTabIds(s, teamKey, workspaceId).length + 1
    : 2;
  const { width, height } = paneSize(workspaceId);
  return gridFitsCells(width, height, cells);
}

/** The splits of each team, one after another: a ⌘D pressed while one is still
 *  opening waits for it, and splits beside the terminal it opened. */
const splitChains = new Map<string, Promise<string | undefined>>();

/** Test-only reset. */
export function resetSplitChainsForTests(): void {
  splitChains.clear();
}

/** One split, run in its team's turn. Resolves with the new tab, if placed. */
async function runSplit(
  teamKey: string,
  workspaceId: string,
  anchorTabId: string,
): Promise<string | undefined> {
  const tabsNow = () => selectWorkspaceTabs(getViewerState(), teamKey, workspaceId);
  if (!tabsNow().some((t) => t.id === anchorTabId)) return undefined;
  if (!roomForSplit(teamKey, workspaceId)) {
    flashBlockedNavForWorkspace(workspaceId, "pulse");
    toast(SPLIT_NO_ROOM);
    return undefined;
  }
  forgetZoomedCells(teamKey, workspaceId);
  const s = getViewerState();
  if (expandedWorkspaceId(s, teamKey) !== workspaceId) s.expandWorkspace(teamKey, workspaceId);
  const stillHere = whereTheUserIs(teamKey);
  const tab = createTerminalTab();
  try {
    await getViewerState().addTerminalTab(teamKey, tab, workspaceId);
  } catch {
    return undefined; // refused — the host's refusal is already logged
  }
  // The host announces the new tab (and, below, its focus) after it answers.
  if (!(await whenViewer(() => tabsNow().some((t) => t.id === tab.sessionId)))) {
    toast(SPLIT_NOT_PLACED);
    return undefined;
  }
  if (!getViewerState().splitTerminalBeside(teamKey, workspaceId, anchorTabId, tab.sessionId))
    return undefined;
  const focused = await whenViewer(
    () => selectActiveTerminalTab(getViewerState(), teamKey, workspaceId) === tab.sessionId,
  );
  if (focused && stillHere()) focusActiveWorkspaceTerminal(teamKey);
  return tab.sessionId;
}

/**
 * Split (⌘D, or Split on a tab's or tile's menu): open a new terminal beside
 * `target` (by default the focused terminal of the focused tile), show the two
 * side by side — the new one joins the cells already shown — and put the cursor
 * in the new one, unless the user has moved on by the time its host opened it.
 * Only an expanded tile can show a grid, so in a split the tile is expanded
 * first. Refused, with a pulse and a word, when another cell wouldn't fit above
 * the readable floor. Splits queue per team: a second ⌘D while the first is
 * still opening splits beside the terminal the first one opened.
 */
export function splitTerminal(
  teamKey: string,
  target?: { workspaceId: string; tabId: string },
): boolean {
  const active = activeTabOf(teamKey);
  const workspaceId = target?.workspaceId ?? active?.workspaceId;
  if (!workspaceId) return false;
  const anchorTabId = target?.tabId ?? focusedTabIn(teamKey, workspaceId);
  if (
    !anchorTabId ||
    !selectWorkspaceTabs(getViewerState(), teamKey, workspaceId).some((t) => t.id === anchorTabId)
  )
    return false;
  beginContextNavigation();
  const pending = splitChains.get(teamKey);
  const turn = (pending ?? Promise.resolve(undefined)).then((opened) => {
    // Pressed again while the last split was opening: beside what it opened.
    const anchor =
      pending && !target && opened && workspaceId === activeTabOf(teamKey)?.workspaceId
        ? opened
        : anchorTabId;
    return runSplit(teamKey, workspaceId, anchor).catch(() => undefined);
  });
  splitChains.set(teamKey, turn);
  void turn.then(() => {
    if (splitChains.get(teamKey) === turn) splitChains.delete(teamKey);
  });
  return true;
}

// ---------------------------------------------------------------------------
// Zoom one cell, and back
// ---------------------------------------------------------------------------

/** The cells a zoomed cell was taken from, per workspace, to show again. */
const zoomedFrom = new Map<string, readonly string[]>();
const zoomKey = (teamKey: string, workspaceId: string) => `${teamKey}\n${workspaceId}`;

function forgetZoomedCells(teamKey: string, workspaceId: string): void {
  zoomedFrom.delete(zoomKey(teamKey, workspaceId));
}

/** Test-only reset. */
export function resetZoomedCellsForTests(): void {
  zoomedFrom.clear();
}

/** Zoom one side-by-side cell (a caption's double-click): it fills the tile as
 *  its active tab, the other cells kept running; showing side by side again
 *  brings back exactly the cells it was zoomed from. */
export function zoomTerminalCell(teamKey: string, workspaceId: string, tabId: string): void {
  beginContextNavigation();
  const s = getViewerState();
  if (shouldRenderTabGrid(s, teamKey, workspaceId)) {
    zoomedFrom.set(zoomKey(teamKey, workspaceId), workspacePanedTabIds(s, teamKey, workspaceId));
  }
  s.setActiveTerminalTab(teamKey, tabId, workspaceId);
  s.collapseTabGridToTabs(teamKey, workspaceId);
}

/** Show all the workspace's tabs at once: the cells a zoom was taken from when
 *  there are any left, else every tab. */
export function showAllTabs(teamKey: string, workspaceId: string): void {
  beginContextNavigation();
  const key = zoomKey(teamKey, workspaceId);
  rememberHeadingOut(teamKey, workspaceId, false);
  const remembered = zoomedFrom.get(key);
  zoomedFrom.delete(key);
  if (remembered && getViewerState().showTerminalCells(teamKey, workspaceId, remembered)) return;
  getViewerState().gridAllTerminalTabs(teamKey, workspaceId);
}

/** Back to one tab at a time: a zoom's cells are forgotten (the user chose it). */
export function showOneTab(teamKey: string, workspaceId: string): void {
  beginContextNavigation();
  forgetZoomedCells(teamKey, workspaceId);
  rememberHeadingOut(teamKey, workspaceId, true);
  getViewerState().collapseTabGridToTabs(teamKey, workspaceId);
}

// ---------------------------------------------------------------------------
// Rename — the tab bar that shows the tab enters its inline edit
// ---------------------------------------------------------------------------

export interface TabRenameRequest {
  tabId: string;
  nonce: number;
}

export const useTabRenameRequestStore = create<{ request: TabRenameRequest | null }>(() => ({
  request: null,
}));

// Monotonic across requests (the store is cleared once a tile takes a request),
// so a repeat request on the same tab always re-arms the editor.
let renameNonce = 0;

/** Ask whichever tab bar shows `tabId` to open its inline rename. */
export function requestTabRename(tabId: string): void {
  renameNonce += 1;
  useTabRenameRequestStore.setState({ request: { tabId, nonce: renameNonce } });
}

/**
 * Rename the tab whose pill has keyboard focus (F2). Only from a pill: in the
 * terminal itself F2 belongs to the program running there (htop, mc, nano).
 */
export function renameFocusedTab(): boolean {
  const tabId = focusedPillTabId();
  if (!tabId) return false;
  requestTabRename(tabId);
  return true;
}

// ---------------------------------------------------------------------------
// Find / clear — through a handle each mounted terminal registers
// ---------------------------------------------------------------------------

export interface TerminalKeyHandle {
  /** Open (or re-focus) the terminal's find bar. */
  openFind: () => void;
  /** Clear the terminal's screen and scrollback. False when it didn't (a
   *  full-screen program owns the screen). */
  clear: () => boolean;
}

const handles = new Map<string, TerminalKeyHandle>();

/** Register a mounted terminal's handle. Returns the unregister function. */
export function registerTerminalKeyHandle(
  sessionId: string,
  handle: TerminalKeyHandle,
): () => void {
  handles.set(sessionId, handle);
  return () => {
    if (handles.get(sessionId) === handle) handles.delete(sessionId);
  };
}

function focusedTerminalHandle(teamKey: string): TerminalKeyHandle | undefined {
  if (!isFocusInTerminalArea()) return undefined;
  const sessionId = targetSessionId(teamKey);
  return sessionId ? handles.get(sessionId) : undefined;
}

/** Find in the focused terminal (⌘F from the terminal area; the doc viewer keeps its own ⌘F). */
export function findInFocusedTerminal(teamKey: string): boolean {
  const handle = focusedTerminalHandle(teamKey);
  handle?.openFind();
  return handle !== undefined;
}

/** Clear the focused terminal's screen and scrollback — not while a full-screen
 *  program (vim, htop, an agent's TUI) owns the screen, which it would blank. */
export function clearFocusedTerminal(teamKey: string): boolean {
  return focusedTerminalHandle(teamKey)?.clear() ?? false;
}

// ---------------------------------------------------------------------------
// Jump to tab N
// ---------------------------------------------------------------------------

/** Jump to the focused tile's tab `n` (1-based). As in browsers, 9 is always the last tab. */
export function jumpToTerminalTab(teamKey: string, n: number): boolean {
  const active = activeTabOf(teamKey);
  if (!active) return false;
  const idx = n === 9 ? active.tabs.length - 1 : n - 1;
  const tab = active.tabs[idx];
  if (!tab) return false;
  beginContextNavigation();
  getViewerState().setActiveTerminalTab(teamKey, tab.id, active.workspaceId);
  if (tab.id === active.tab?.id) return false;
  focusActiveWorkspaceTerminal(teamKey);
  return true;
}

// ---------------------------------------------------------------------------
// Reopen the last closed tab
// ---------------------------------------------------------------------------

/** Whether the closed entry can still come back. A Hidden tab must still be
 *  Hidden (not restored by hand since, not ended); an Ended tab's workspace must
 *  still exist. */
function reopenable(entry: ClosedTabEntry): boolean {
  const { teamKey, workspaceId, tab } = entry;
  if (entry.kind === "hidden") {
    const s = getViewerState();
    // Hidden as a tab (a Background entry) or with its whole tile (in the tray).
    return (
      selectHiddenTerminalTabs(s, teamKey, workspaceId).some((t) => t.id === tab.id) ||
      (whereIs(teamKey, workspaceId) !== "open" &&
        selectWorkspaceTabs(s, teamKey, workspaceId).some((t) => t.id === tab.id))
    );
  }
  return whereIs(teamKey, workspaceId) !== "gone";
}

async function reopen(entry: ClosedTabEntry): Promise<void> {
  const { teamKey, workspaceId, tab } = entry;
  const stillHere = whereTheUserIs(teamKey);
  if (entry.kind === "hidden") {
    await showTab(teamKey, workspaceId, tab.id, "terminal", stillHere);
    return;
  }
  // Ended: the process is gone, so a fresh shell in the same workspace (whose cwd
  // every new tab there starts in), under the same name — unless an agent ran
  // there: the name was the agent's, and this is a plain shell.
  if (!(await bringOnScreen(teamKey, workspaceId))) return;
  const name = entry.wasAgent ? undefined : tab.name;
  const tabs = selectWorkspaceTabs(getViewerState(), teamKey, workspaceId);
  const replacement = entry.wasLastTab && tabs.length === 1 ? tabs[0] : undefined;
  if (replacement) {
    // Ending a tile's last tab already opened a fresh shell in its place: that is
    // the reopened shell, not a second one beside it.
    if (name) getViewerState().renameTerminalTab(teamKey, replacement.id, name, workspaceId);
    getViewerState().setActiveTerminalTab(teamKey, replacement.id, workspaceId);
  } else {
    await getViewerState()
      .addTerminalTab(teamKey, createTerminalTab(name), workspaceId)
      .catch(() => {});
  }
  if (stillHere()) land(teamKey, workspaceId, "terminal");
}

/** Reopen the most recently Hidden or Ended tab, skipping any that can't come back. */
export function reopenClosedTab(teamKey: string): boolean {
  for (let entry = popClosedTab(teamKey); entry; entry = popClosedTab(teamKey)) {
    if (!reopenable(entry)) continue;
    void reopen(entry);
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// One key in and out: workspaces → Expanded → Expanded · all tabs, and back
// ---------------------------------------------------------------------------

/** What the expand key names after each step. */
export const EXPAND_STEP_LABELS = {
  workspaces: "Workspaces",
  expanded: "Expanded",
  allTabs: "Expanded · all tabs",
} as const;
/** One toast for the key, so quick presses replace it rather than stack. */
const EXPAND_STEP_TOAST_ID = "expand-step";
const EXPAND_STEP_TOAST_MS = 1200;

/**
 * Expanded workspaces that came back out of all tabs: from one tab, the key keeps
 * going out. Expanded reached any other way heads in. Forgotten as soon as the
 * workspace stops being expanded, so a later expand starts heading in again.
 */
const headingOut = new Set<string>();
let watchingHeadingOut = false;

function rememberHeadingOut(teamKey: string, workspaceId: string, out: boolean): void {
  const key = zoomKey(teamKey, workspaceId);
  if (!out) {
    headingOut.delete(key);
    return;
  }
  headingOut.add(key);
  if (watchingHeadingOut) return;
  watchingHeadingOut = true;
  subscribeViewerState((s) => {
    for (const k of headingOut) {
      const [team, ws] = k.split("\n");
      if (expandedWorkspaceId(s, team) !== ws) headingOut.delete(k);
    }
  });
}

/** Test-only reset. */
export function resetExpandStepForTests(): void {
  headingOut.clear();
}

/**
 * The expand key (⌘⇧M) bounces the focused tile between three states, turning
 * round at each end: the workspace view → Expanded (its tile fills the area, one
 * tab at a time) → Expanded · all tabs → Expanded → the workspace view. The ends
 * are read from the layout itself; only the middle needs to know which way it
 * was going. A tile with one tab skips all tabs (expand and restore); a tile
 * already alone in the area has no workspace view to go back to (one tab ⇄ all
 * tabs). Each step names the state it reached in a brief toast.
 */
export function stepExpand(teamKey: string): boolean {
  const active = activeTabOf(teamKey);
  if (!active) return false;
  const { workspaceId } = active;
  const key = zoomKey(teamKey, workspaceId);
  const s = getViewerState();
  const zoomed = selectExpandedWorkspaceId(s, teamKey) === workspaceId;
  // A cell zoomed out of all tabs can always go back to them.
  const canShowAll = active.tabs.length >= 2 || zoomedFrom.has(key);
  let reached: string;
  if (shouldRenderTabGrid(s, teamKey, workspaceId)) {
    showOneTab(teamKey, workspaceId);
    reached = EXPAND_STEP_LABELS.expanded;
  } else if (expandedWorkspaceId(s, teamKey) !== workspaceId) {
    // Alone in the area already, expanding shows nothing new: go on to all tabs.
    const alone = !isSplitActive(activeForegroundTiles(s, teamKey).length);
    if (alone && !canShowAll) return false;
    s.expandWorkspace(teamKey, workspaceId);
    if (alone) showAllTabs(teamKey, workspaceId);
    reached = alone ? EXPAND_STEP_LABELS.allTabs : EXPAND_STEP_LABELS.expanded;
  } else if (canShowAll && !(zoomed && headingOut.has(key))) {
    showAllTabs(teamKey, workspaceId);
    reached = EXPAND_STEP_LABELS.allTabs;
  } else if (zoomed) {
    s.setCliExpanded(false);
    reached = EXPAND_STEP_LABELS.workspaces;
  } else {
    return false;
  }
  toast(reached, { id: EXPAND_STEP_TOAST_ID, duration: EXPAND_STEP_TOAST_MS });
  focusActiveWorkspaceTerminal(teamKey);
  return true;
}

// ---------------------------------------------------------------------------
// Jump to what needs me
// ---------------------------------------------------------------------------

/** The session the jump last landed on, and when its dot began: the cycle moves on
 *  from it even after it leaves the list (landing on a crash marks it seen). */
let lastLanded: { teamKey: string; sessionId: string; since: number } | null = null;

/** Test-only reset. */
export function resetWhatNeedsYouCycleForTests(): void {
  lastLanded = null;
}

function landingFor(row: SessionNeedsYou): Landing {
  return row.kind === "question" ? "question" : "terminal";
}

/**
 * The one "take me to what needs me" verb, over the shared needs-you list
 * (`selectSessionsNeedingYou`, less what is no longer news — `isNeedsYouNews`:
 * a question, a permission prompt, an unseen crash or failure — across open
 * tabs, Hidden tabs, and tray tiles, oldest first):
 *
 * 1. an alert the user hasn't acted on yet names where to start;
 * 2. when the focused tile has a question waiting and focus isn't in its banner
 *    yet, go to that banner;
 * 3. otherwise the next session in the list after the focused tab (wrapping),
 *    brought on screen — a minimized tile restored, a Hidden tab revealed —
 *    landing in its question banner if it asks, else its terminal.
 *
 * Returns false when nothing needs you.
 */
export function jumpToWhatNeedsYou(teamKey: string): boolean {
  const state = getNeedsYouState();
  const rows = selectSessionsNeedingYou(state, teamKey).filter(isNeedsYouNews);
  if (rows.length === 0) return false;
  const since = (sessionId: string) => state.seen.stateSince[sessionId] ?? Number.POSITIVE_INFINITY;
  const go = (row: SessionNeedsYou) => {
    lastLanded = { teamKey, sessionId: row.sessionId, since: since(row.sessionId) };
    void showTab(teamKey, row.workspaceId, row.tabId, landingFor(row));
  };

  // The session the Jump pill is offering (an alert the user hasn't acted on yet).
  const alerted = getPendingAttentionTarget();
  const alertedRow = alerted ? rows.find((r) => r.sessionId === alerted) : undefined;
  if (alertedRow) {
    go(alertedRow);
    return true;
  }

  const current = activeTabOf(teamKey);
  if (
    current &&
    rows.some((r) => r.workspaceId === current.workspaceId && r.kind === "question" && !r.hidden) &&
    !isFocusInTileQuestion(current.workspaceId)
  ) {
    // The banner of the tile you're in; a zoom-collapsed one is revealed first, and
    // a collapsed terminal pane is opened first, so focus never lands in a banner
    // the user can't see.
    getViewerState().expandTerminalOnOpen(teamKey);
    void focusTileQuestion(current.workspaceId, (wsId) =>
      getViewerState().switchWorkspace(teamKey, wsId),
    );
    return true;
  }

  const at = rows.findIndex(
    (r) => r.workspaceId === current?.workspaceId && r.tabId === current.tab?.id,
  );
  if (at !== -1) {
    go(rows[(at + 1) % rows.length]);
    return true;
  }
  // The focused tab isn't in the list — typically the crash just landed on, now
  // seen. Move on from where the last jump landed (the list is ordered by when
  // each dot began), not back to the oldest.
  const anchor =
    lastLanded?.teamKey === teamKey && !rows.some((r) => r.sessionId === lastLanded?.sessionId)
      ? lastLanded
      : null;
  const next = anchor ? rows.find((r) => since(r.sessionId) > anchor.since) : undefined;
  go(next ?? rows[0]);
  return true;
}
