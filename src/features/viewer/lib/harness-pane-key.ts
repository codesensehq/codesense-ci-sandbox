// SPDX-License-Identifier: MIT
/**
 * The key a terminal body registers its webdriver-only harness seams under (the
 * `__CODESENSE_*` readers and the `data-qh1-workspace-id` container attribute).
 *
 * A workspace's first tab keeps the bare workspace id, so single-tab fixtures and
 * every existing harness call site read exactly what they did before. Every
 * further tab gets its own `workspaceId::tabId` key, so a workspace showing
 * several tabs at once (a tab grid) never has two terminals fighting over one
 * registry slot — each grid cell is scored against its OWN buffer.
 *
 * The key is POSITIONAL: it follows the tab's index, not its identity. If the
 * first tab closes, the tab that moves into position 0 re-registers under the
 * bare workspace id (the registry effect re-runs on a key change). Harness code
 * must therefore re-read pane keys after any tab close rather than cache them.
 */
export const HARNESS_PANE_KEY_SEPARATOR = "::";

export function harnessPaneKey(workspaceId: string, tabIndex: number, tabId: string): string {
  return tabIndex === 0 ? workspaceId : `${workspaceId}${HARNESS_PANE_KEY_SEPARATOR}${tabId}`;
}
