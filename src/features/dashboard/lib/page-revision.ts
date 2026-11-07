// SPDX-License-Identifier: MIT
import type { PaneSourceRow } from "@/tauri/commands";

/**
 * The version of the page a source row serves: the document revision, which survives the
 * pane-source view moving between teams, else the per-reader revision. Rust renumbers the per-reader
 * `sourceRevision` whenever a team's source is re-created after the view left it, so keying a page
 * reload on it re-mints every warm return.
 */
export function pageRevision(row: Pick<PaneSourceRow, "documentRevision" | "sourceRevision">) {
  return row.documentRevision ?? row.sourceRevision;
}
