// SPDX-License-Identifier: MIT
/**
 * The DOM half of the hidden-team rule (the hook half is `useScopeActive`). A warm team stays
 * mounted, hidden by an ancestor's inline `display:none`, and ids repeat across teams (workspace
 * ids are file stems), so a document-wide lookup by pane, viewer or HUD id can return a hidden
 * team's element. Every lookup that must land on what the user sees goes through here.
 */

/** Inside a subtree hidden by an inline `display:none` (an inactive team). */
export function isInHiddenTeam(el: Element): boolean {
  for (let n: Element | null = el; n; n = n.parentElement) {
    if ((n as HTMLElement).style?.display === "none") return true;
  }
  return false;
}

/** Every match of `selector` that is not inside a hidden team, in document order. */
export function queryAllShown<T extends Element = HTMLElement>(
  selector: string,
  root: ParentNode = document,
): T[] {
  return [...root.querySelectorAll<T>(selector)].filter((el) => !isInHiddenTeam(el));
}

/** The first match of `selector` that is not inside a hidden team, or null. */
export function queryShown<T extends Element = HTMLElement>(
  selector: string,
  root: ParentNode = document,
): T | null {
  for (const el of root.querySelectorAll<T>(selector)) {
    if (!isInHiddenTeam(el)) return el;
  }
  return null;
}
