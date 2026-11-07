// SPDX-License-Identifier: MIT
/**
 * The CLI workspace module's single door to the viewer store for IMPERATIVE
 * access. Files inside the terminal seam read one-shot state and call cohesive
 * store actions through `getViewerState()` instead of importing the global
 * `useViewerStore` singleton directly, so the seam depends on this adapter rather
 * than on the store implementation (NFR6). Reactive `useViewerStore((s) => …)`
 * subscriptions are deliberately NOT routed here — those surfaces are migrated
 * separately so their subscription identity and granularity are reviewed on their
 * own.
 *
 * This is behaviour-preserving: `getViewerState()` returns the exact same state
 * object `useViewerStore.getState()` did, so reads and action calls are
 * unchanged.
 */

import { useViewerStore } from "@/shared/stores/viewer-store";

/** One-shot snapshot of the viewer store for imperative reads and action calls.
 *  Never call this during render — it does not subscribe. */
export const getViewerState = (): ReturnType<typeof useViewerStore.getState> =>
  useViewerStore.getState();

/** Listen for every viewer-store change, imperatively (outside React); returns
 *  the unsubscribe. */
export const subscribeViewerState = (
  listener: (state: ReturnType<typeof useViewerStore.getState>) => void,
): (() => void) => useViewerStore.subscribe(listener);

// Pure store helpers the terminal seam composes with. Re-exported here so a seam
// file imports them from the module boundary, not from the store module.
export {
  selectTabBySessionId,
  selectWorkspaceTabs,
  selectActiveTerminalTab,
  selectRoleTab,
  selectHiddenTerminalTabs,
  createTerminalTab,
  promptIdentityFromRepoPath,
  effectiveMosaicRoot,
  // SW-WT-12 (S2): the checklist row reads a step's buffered output on demand.
  getBackgroundSetupStepOutput,
  // Which workspace fills the CLI area, and whether its tabs show as a grid.
  expandedWorkspaceId,
  // The tile expanded over the others (the focused one, while expanded is on).
  selectExpandedWorkspaceId,
  shouldRenderTabGrid,
  // The tiles sharing the CLI area right now, and their layout.
  activeForegroundTiles,
  activeForegroundLayout,
  workspacePanedTabIds,
  // Which sessions a tab still holds: shown, Hidden, or minimized.
  selectRunningTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
} from "@/shared/stores/viewer-store";

// The terminal-AI status vocabulary is framework-free and lives outside the store
// so it survives a partial store mock.
export {
  TERMINAL_AI_STATUSES,
  TERMINAL_WIRE_STATUSES,
  type TerminalAiStatus,
  type TerminalWireStatus,
} from "@/shared/lib/terminal-ai-status";

// Store-owned domain types the seam references. Type-only re-exports keep the
// seam's `import type` off the store module.
export type {
  Workspace,
  TerminalTab,
  TerminalTabOrigin,
  TerminalRole,
  NewTerminalTab,
  TerminalTabIndex,
} from "@/shared/stores/viewer-store";
