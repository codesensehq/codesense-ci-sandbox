// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// RF-12: shared types for the pipeline-status-nav sub-modules (context menu,
// dialogs). The orchestrator owns the state; these types let the extracted
// presentational components consume it without re-declaring the shape.
// ---------------------------------------------------------------------------
import type { PinnedNavItem } from "@/features/dashboard/lib/nav-pins";

/** Right-click context-menu state (position + target + variant flags). `pin` is
 *  set only when the right-clicked row IS a pinned row (SLW-NC-7). */
export interface NavContextMenuState {
  owner?: object;
  x: number;
  y: number;
  path: string;
  epicDirPath?: string;
  /** SW-EW-6: absolute path to the epic's mainDoc (epic headers only). */
  epicFilePath?: string;
  /** SW-EW-1 AC-7c: true only for epic folder headers. */
  isEpic?: boolean;
  /** SLW-NC-7: set when the right-clicked row is a pinned row. */
  pin?: PinnedNavItem;
}
