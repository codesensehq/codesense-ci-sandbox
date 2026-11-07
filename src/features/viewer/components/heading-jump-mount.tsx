// SPDX-License-Identifier: MIT
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { useCallback, useMemo } from "react";

import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { QuickOpenDialog } from "./quick-open-dialog";
import { buildHeadingPickerItems } from "../lib/heading-picker-items";

interface HeadingJumpMountProps {
  /** Only the active team's view renders the dialog (the open-flag is global,
   *  notes/headings are global per-install, but at most one dialog should mount). */
  isActiveView: boolean;
}

/**
 * SW-SN-6: the Cmd+Shift+O go-to-heading fuzzy jump — the keyboard sibling of the
 * SN-3 note switcher (`NoteSwitcherMount`), retargeted from notes to the ACTIVE
 * note's headings. Its own leaf so the `activeNoteHeadings` subscription (which
 * changes as the user edits headings) re-renders only this tiny mount, not the
 * 1700-line `TeamWorkspaceView`. Owns all heading-specific wiring; the generic
 * {@link QuickOpenDialog} stays item-source-agnostic (unforked, SN-3 AC-5).
 */
export function HeadingJumpMount({ isActiveView }: HeadingJumpMountProps) {
  const headingJumpOpen = useUiStore((s) => s.headingJumpOpen);
  const setHeadingJumpOpen = useUiStore((s) => s.setHeadingJumpOpen);
  // The live heading list published by the scratch panel (SN-5's `useNoteHeadings`
  // output — the single source; never re-enumerated). Reading it here means the
  // jump needs no access to the editor: building items on open does no work
  // beyond the adapter map, and never blocks the UI.
  const headings = useViewerStore((s) => s.activeNoteHeadings);
  const items = useMemo(() => buildHeadingPickerItems(headings), [headings]);

  // The picker id IS the heading slug. Reveal the notes surface FIRST so the
  // jump lands visibly even if the right pane was closed or on another tab, then
  // signal the panel (which owns the editor) to scroll. The panel recomputes the
  // target by slug from the live doc — so a slug that no longer resolves (heading
  // deleted while the modal was open) simply no-ops there.
  const handleSelect = useCallback((slug: string) => {
    const store = useViewerStore.getState();
    revealSlot("right-pane", { surface: "notes", intent: { heading: slug } });
    store.requestHeadingJump(slug);
  }, []);

  return (
    <QuickOpenDialog
      open={headingJumpOpen && isActiveView}
      onClose={() => setHeadingJumpOpen(false)}
      onSelect={handleSelect}
      items={items}
      placeholder="Go to heading…"
      emptyLabel="No headings in this note"
      // SW-FX-45: heading labels are freeform prose, not filenames — a heading
      // like `## Config.md` must render verbatim, never extension-stripped.
      stripExtension={false}
    />
  );
}
