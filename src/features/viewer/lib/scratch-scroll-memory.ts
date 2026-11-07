// SPDX-License-Identifier: MIT
// Scratch-note scroll memory across right-pane minimize/restore.
//
// The right pane keeps panels MOUNTED and toggles `display:none` when a section
// is collapsed/minimized (see use-reparenting-panel-bodies). A scroll container
// hidden with `display:none` loses its `scrollTop` when shown again, so the
// notes editor jumps back to the top on every restore. React can't observe that
// ancestor display flip, so the panel watches the scroll container's rendered
// size (a ResizeObserver): size → 0 means hidden, size → positive means shown.
//
// This module is the pure decision core, kept DOM-free so it's unit-testable.
// The panel wires DOM events (scroll + ResizeObserver) to these transitions.

export interface ScrollMemory {
  /** The last scrollTop seen while the container was visible. */
  saved: number;
  /** Whether the container is currently hidden (display:none / zero size). */
  hidden: boolean;
}

export const initScrollMemory = (visible: boolean): ScrollMemory => ({
  saved: 0,
  hidden: !visible,
});

/**
 * Record a scroll position. Only visible scrolls are trusted — a `display:none`
 * container can report a spurious scrollTop of 0, which must not clobber the
 * saved value we intend to restore.
 */
export function recordScroll(mem: ScrollMemory, visible: boolean, scrollTop: number): ScrollMemory {
  if (!visible || scrollTop === mem.saved) return mem;
  return { ...mem, saved: scrollTop };
}

/**
 * Fold a visibility change into the memory. Returns the next memory and, when
 * the container just went hidden → visible, the scrollTop to restore (else null).
 * Restoring only on the hidden→visible edge means normal in-view resizes (the
 * pane widening, content growing) never fight the user's current scroll.
 */
export function onVisibilityChange(
  mem: ScrollMemory,
  visible: boolean,
): [ScrollMemory, number | null] {
  if (!visible) return [{ ...mem, hidden: true }, null];
  if (mem.hidden) return [{ ...mem, hidden: false }, mem.saved];
  return [mem, null];
}

/** Reset saved scroll (e.g. when the active note changes — a different note
 *  should not restore the previous note's position). Preserves hidden state. */
export function resetScroll(mem: ScrollMemory): ScrollMemory {
  return { ...mem, saved: 0 };
}
