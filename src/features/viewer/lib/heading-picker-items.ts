// SPDX-License-Identifier: MIT
import type { Heading } from "./note-headings";
import type { QuickPickerItem } from "../components/quick-open-dialog";

/**
 * SW-SN-6: adapt the active note's live heading list (SW-SN-5's `useNoteHeadings`
 * output — the single heading source; never re-enumerated here) to the generic
 * {@link QuickOpenDialog} item shape for the Cmd+Shift+O go-to-heading jump.
 *
 * The sibling of SW-SN-3's `buildScratchNotePickerItems`: same dialog, different
 * item source (headings, not notes). `label` carries the heading text — the
 * dialog fuzzy-searches `max(label, sublabel)`, so the text in `label` is what's
 * matched; `sublabel` is left empty (do NOT duplicate the text into it). `id` is
 * the heading's `slug`, which `enumerateHeadings` already de-duplicates by
 * occurrence ordinal, so it's unique-within-note and safe as the picker id /
 * React key; `onSelect` receives it verbatim and re-matches by slug against a
 * freshly recomputed doc (positions go stale — see the panel's jump effect).
 */
export function buildHeadingPickerItems(headings: Heading[]): QuickPickerItem[] {
  return headings.map((h) => ({
    id: h.slug,
    label: h.text,
    sublabel: "",
  }));
}
