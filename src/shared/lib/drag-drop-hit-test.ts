// SPDX-License-Identifier: MIT
/**
 * Pure hit-testing for Tauri webview drag-drop. Tauri's `onDragDropEvent`
 * delivers a `position` (not a DOM target), so a handler that wants to route a
 * drop to a specific on-screen element must compare that position to the
 * element's rect itself. Shared by every webview-drop consumer (the terminal
 * tile drop and the SW-VE-7 nav file-drop) so the coordinate-space handling
 * lives in exactly one place.
 */

export type DragPosition = { x: number; y: number };

/**
 * Hit-test a drag-drop position against an on-screen rect.
 *
 * Tauri's `onDragDropEvent` `position` is in CSS (logical) pixels relative to
 * the webview — the same coordinate space `getBoundingClientRect()` returns —
 * so it is compared directly. (The payload's TS type is `PhysicalPosition`, but
 * on macOS the value is logical: verified empirically at dpr=2 the raw position
 * lands in the tile actually dropped on, whereas dividing by `devicePixelRatio`
 * shifts every drop toward the top-left.)
 *
 * Half-open on the right/bottom edges so a drop landing exactly on the seam
 * shared by two adjacent rects resolves to exactly one (the one whose inclusive
 * left/top owns the seam) — never both, never a double write.
 */
export function dropPositionInRect(
  position: DragPosition,
  rect: { left: number; top: number; right: number; bottom: number },
): boolean {
  return (
    position.x >= rect.left &&
    position.x < rect.right &&
    position.y >= rect.top &&
    position.y < rect.bottom
  );
}

/** The topmost element owns the drop. Native containers yield to a nested frame,
 * whose document decides what to do with the paths. */
export function dropPositionHitsElement(position: DragPosition, element: HTMLElement): boolean {
  if (!dropPositionInRect(position, element.getBoundingClientRect())) return false;
  const topmost = element.ownerDocument.elementFromPoint(position.x, position.y);
  return (
    topmost !== null &&
    (topmost === element || (topmost.tagName !== "IFRAME" && element.contains(topmost)))
  );
}
