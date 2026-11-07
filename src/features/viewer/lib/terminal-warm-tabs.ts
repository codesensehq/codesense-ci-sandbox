// SPDX-License-Identifier: MIT
interface WarmTab {
  id: string;
  origin?: { offlineSince?: number };
}

/** Keep the selected pane, its immediate neighbours, and the last selection.
 * Stable strip order lets React retain each keyed pane when selection moves. */
export function warmTerminalTabIds(
  tabs: readonly WarmTab[],
  activeId: string | undefined,
  previousId: string | undefined,
  shown: boolean,
): string[] {
  if (!shown) return [];
  const index = tabs.findIndex((tab) => tab.id === activeId);
  if (index === -1) return [];
  const ids = new Set([activeId, tabs[index - 1]?.id, tabs[index + 1]?.id, previousId]);
  return tabs
    .filter(
      (tab) => ids.has(tab.id) && (tab.id === activeId || tab.origin?.offlineSince === undefined),
    )
    .map((tab) => tab.id);
}
