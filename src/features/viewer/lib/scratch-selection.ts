// SPDX-License-Identifier: MIT
// SW-SN-8 AC-4/AC-9: pure decision logic for the scratch-note selection popover.
//
// Extracted as plain functions (mirroring `handleScratchNoteTab`) so the
// show/hide + active-mark decisions are unit-testable without a live editor or
// layout — the panel's component test mocks the whole Tiptap stack, and
// happy-dom has no geometry. Keeping this logic off the editor's per-keystroke
// path is also what makes AC-9 (non-blocking) hold: the panel only re-renders on
// `selectionUpdate`, and while typing the selection is collapsed so this returns
// null and no popover state churns.

/** Minimal editor shape this module reads — avoids coupling to the Editor class. */
export interface SelectionReadableEditor {
  state: {
    selection: { from: number; to: number };
    doc: { textBetween(from: number, to: number, blockSeparator?: string): string };
  };
  isActive(name: string): boolean;
}

export interface SelectionMenuTarget {
  from: number;
  to: number;
  text: string;
}

/**
 * Decide whether the selection popover should show, and for what text.
 * Returns null for a collapsed selection or a whitespace-only selection
 * (the same guards the retired right-click menu used).
 */
export function computeSelectionMenu(editor: SelectionReadableEditor): SelectionMenuTarget | null {
  const { from, to } = editor.state.selection;
  if (from === to) return null;
  const text = editor.state.doc.textBetween(from, to, "\n");
  if (!text.trim()) return null;
  return { from, to, text };
}

export interface ActiveMarks {
  bold: boolean;
  italic: boolean;
  code: boolean;
}

/** Read the inline marks active at the current selection, for button highlight. */
export function readActiveMarks(editor: Pick<SelectionReadableEditor, "isActive">): ActiveMarks {
  return {
    bold: editor.isActive("bold"),
    italic: editor.isActive("italic"),
    code: editor.isActive("code"),
  };
}
