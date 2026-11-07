// SPDX-License-Identifier: MIT
import { type TerminalFontSize, useViewerStore } from "@/shared/stores/viewer-store";
import type { XtermVisualState } from "@/shared/lib/xterm-visual-state";
// Type-only: this module is loaded by the Node-side specs too.
import type {} from "@/shared/terminal/terminal-manager";
import {
  disposeWorkspaceFixture,
  fixtureHydrationSettled,
  focusWorkspacePane,
  hideAndResumeWindow,
  readWorkspaceLayout,
  resetWorkspaceFixture,
  seedWorkspaceFixture,
  snapshotWorkspaceFixture,
  switchWorkspaceTab,
  type WorkspaceFixtureIdentity,
  type WorkspaceFixturePlan,
  type WorkspaceFixtureSnapshot,
  type WorkspaceLayoutReading,
} from "./e2e-cli-workspace-control";

/**
 * WebDriver-only fixture control for QH-6's terminal-webgl suite: six
 * same-config CLI panes — the geometry SW-FX-82's shared-atlas sibling
 * corruption reproduces at. Reuses the generic workspace-fixture core in
 * `e2e-cli-workspace-control.ts`; adds only the six-pane, one-shared-epic plan.
 * Behind the `webdriver` build mode, so it is tree-shaken from and proven absent
 * (`scripts/verify-webdriver-absence.ts`) from the shipped app.
 */

export const TERMINAL_WEBGL_ORG_SLUG = "qh6";
export const TERMINAL_WEBGL_TEAM_NAME = "webgl";
export const TERMINAL_WEBGL_TEAM_KEY = `${TERMINAL_WEBGL_ORG_SLUG}:${TERMINAL_WEBGL_TEAM_NAME}`;
export const TERMINAL_WEBGL_WORKSPACE_IDS = [
  "qh6-pane-a",
  "qh6-pane-b",
  "qh6-pane-c",
  "qh6-pane-d",
  "qh6-pane-e",
  "qh6-pane-f",
] as const;
export const TERMINAL_WEBGL_PANE_COUNT = TERMINAL_WEBGL_WORKSPACE_IDS.length;
// One shared parent epic across all six panes ⇒ one shared xterm glyph atlas —
// the precondition SW-FX-82's sibling corruption needs.
const TERMINAL_WEBGL_SHARED_EPIC = "qh6-epic";

export interface TerminalWebglFixtureInput {
  root: string;
  /** Exactly {@link TERMINAL_WEBGL_PANE_COUNT} isolated working directories. */
  cwds: string[];
}

export function createTerminalWebglFixturePlan(
  input: TerminalWebglFixtureInput,
): WorkspaceFixturePlan {
  if (input.cwds.length !== TERMINAL_WEBGL_PANE_COUNT) {
    throw new Error(
      `terminal-webgl fixture needs exactly ${TERMINAL_WEBGL_PANE_COUNT} cwds, got ${input.cwds.length}`,
    );
  }
  return {
    orgSlug: TERMINAL_WEBGL_ORG_SLUG,
    teamName: TERMINAL_WEBGL_TEAM_NAME,
    teamKey: TERMINAL_WEBGL_TEAM_KEY,
    sourceRepo: { owner: "fixture", repo: "webgl", provider: "github" },
    root: input.root,
    displayName: "QH6 WebGL",
    // Terminal-primary (console) is this fixture's resting state: the six panes
    // need a real pixel budget for the atlas oracle, and it matches the epic's
    // CLI-primary intent. `shared` stays reachable as a T6 transition target, not
    // the resting layout — under `shared` the panes are squished under the file
    // viewer and read as blank (the SW-CL-2e blank-panes defect).
    layoutMode: "console",
    workspaces: TERMINAL_WEBGL_WORKSPACE_IDS.map((id, index) => ({
      id,
      storyId: id.toUpperCase(),
      parentEpic: TERMINAL_WEBGL_SHARED_EPIC,
      cwd: input.cwds[index],
    })),
  };
}

const TERMINAL_WEBGL_IDENTITY: WorkspaceFixtureIdentity = {
  teamKey: TERMINAL_WEBGL_TEAM_KEY,
  teamName: TERMINAL_WEBGL_TEAM_NAME,
  workspaceIds: TERMINAL_WEBGL_WORKSPACE_IDS,
  // A reset restores the same console resting layout the seed established.
  layoutMode: "console",
};

function seed(input: TerminalWebglFixtureInput): Promise<WorkspaceFixtureSnapshot> {
  return seedWorkspaceFixture(createTerminalWebglFixturePlan(input));
}

/** The generic fixture snapshot plus each pane's live xterm visual state, read
 *  from the panel-registered readers (QH-6b). Composed only here — the generic
 *  `snapshotWorkspaceFixture` stays untouched so other suites are unaffected. */
export interface TerminalWebglSnapshot extends WorkspaceFixtureSnapshot {
  visualState: Record<string, XtermVisualState>;
}

function readAllVisualState(): Record<string, XtermVisualState> {
  const readers = window.__CODESENSE_TERMINAL_STATE__ ?? {};
  const visualState: Record<string, XtermVisualState> = {};
  for (const id of TERMINAL_WEBGL_WORKSPACE_IDS) {
    const state = readers[id]?.();
    if (state) visualState[id] = state;
  }
  return visualState;
}

/** Every registered pane's visual state keyed by its harness pane key — the six
 *  fixture panes AND any extra tab a layout journey opened. */
function readAllPaneVisualState(): Record<string, XtermVisualState> {
  const visualState: Record<string, XtermVisualState> = {};
  for (const [key, read] of Object.entries(window.__CODESENSE_TERMINAL_STATE__ ?? {})) {
    const pane = document.querySelector<HTMLElement>(
      `[data-qh1-workspace-id="${CSS.escape(key)}"]`,
    );
    if (
      !pane ||
      pane.closest("[inert], [aria-hidden='true']") ||
      !pane.getBoundingClientRect().width
    )
      continue;
    const state = read();
    if (state) visualState[key] = state;
  }
  return visualState;
}

function terminalWebglSnapshot(): TerminalWebglSnapshot {
  return {
    ...snapshotWorkspaceFixture(TERMINAL_WEBGL_TEAM_KEY),
    visualState: readAllVisualState(),
  };
}

export interface TerminalWebglE2eControl {
  hydrationSettled: () => boolean;
  seed: (input: TerminalWebglFixtureInput) => Promise<WorkspaceFixtureSnapshot>;
  reset: () => WorkspaceFixtureSnapshot;
  dispose: () => Promise<void>;
  snapshot: () => TerminalWebglSnapshot;
  hideAndResume: (durationMs: number) => Promise<void>;
  /** Drive the production terminal-font setting (the same `updateSettings` the
   *  settings UI commits) to trigger the `document.fonts.load` → atlas-recovery
   *  wave (AC-4d). Returns the resulting preset. */
  setTerminalFontPreset: (preset: TerminalFontSize) => TerminalFontSize;
  /** SW-CL-12: switch the active terminal to `workspaceId` through the real
   *  `setActiveTerminalTab` + `switchWorkspace` + focus path (the tab-switch that
   *  reveals a background pane against a mutated shared atlas). Returns the focused
   *  workspace id, or null when it has no live tab to activate. */
  switchTab: (workspaceId: string) => string | null;
  /** SW-CL-12 (SW-FX-98): focus-click a visible sibling pane without changing the
   *  active workspace. Returns the focused workspace id, or null. */
  focusPane: (workspaceId: string) => string | null;
  /** Read-only: the fixture team's layout state (mode, preset, zoom, tabs, tab
   *  grid). Layout journeys drive transitions through real UI clicks and use this
   *  only to check what the click did. */
  layout: () => WorkspaceLayoutReading;
  /** Read-only: every registered pane's visual state keyed by harness pane key. */
  paneVisualState: () => Record<string, XtermVisualState>;
  /** Force the session display's WebGL context lost (`WEBGL_lose_context`), as
   *  a GPU reset would. False when the display holds no WebGL canvas. */
  loseContext: (sessionId: string) => boolean;
}

function loseContext(sessionId: string): boolean {
  return window.__csTerminals?.[sessionId]?.loseContext() ?? false;
}

export function registerTerminalWebglE2eControl(): void {
  window.__CODESENSE_TERMINAL_WEBGL_E2E__ = {
    hydrationSettled: fixtureHydrationSettled,
    seed,
    reset: () => resetWorkspaceFixture(TERMINAL_WEBGL_IDENTITY),
    dispose: () => disposeWorkspaceFixture(TERMINAL_WEBGL_TEAM_KEY),
    snapshot: terminalWebglSnapshot,
    hideAndResume: hideAndResumeWindow,
    setTerminalFontPreset: (preset: TerminalFontSize) => {
      useViewerStore.getState().updateSettings({ terminalFontSize: preset });
      return preset;
    },
    switchTab: (workspaceId: string) => switchWorkspaceTab(TERMINAL_WEBGL_TEAM_KEY, workspaceId),
    focusPane: (workspaceId: string) => focusWorkspacePane(workspaceId),
    layout: () => readWorkspaceLayout(TERMINAL_WEBGL_TEAM_KEY),
    paneVisualState: readAllPaneVisualState,
    loseContext,
  };
}

declare global {
  interface Window {
    __CODESENSE_TERMINAL_WEBGL_E2E__?: TerminalWebglE2eControl;
  }
}
