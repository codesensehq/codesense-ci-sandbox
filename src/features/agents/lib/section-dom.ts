// SPDX-License-Identifier: MIT
/** The element of Edit section `id` inside `editor`. Several editors can be in the document at
 *  once (every retained team keeps its own), so a section is always looked up in one editor. */
export function sectionIn(editor: HTMLElement | null, id: string): HTMLElement | null {
  return editor?.querySelector<HTMLElement>(`#ed-${id}`) ?? null;
}

/** Whether `el` is on screen: no ancestor, itself included, is `hidden` or `display: none` (as a retained
 *  team's hidden tab is). */
export function isShown(el: HTMLElement | null): boolean {
  for (let node: HTMLElement | null = el; node; node = node.parentElement) {
    if (node.hidden || getComputedStyle(node).display === "none") return false;
  }
  return el !== null;
}
