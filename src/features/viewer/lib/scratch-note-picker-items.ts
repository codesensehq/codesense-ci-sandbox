// SPDX-License-Identifier: MIT
import type { NoteMeta } from "@/tauri/commands";
import { partitionNotes } from "./note-ordering";
import type { QuickPickerItem } from "../components/quick-open-dialog";

/**
 * SW-SN-3: adapt the plural notes list to the generic {@link QuickOpenDialog}
 * item shape for the Cmd+O quick-switcher — the keyboard accelerator over the
 * SN-2 rail's list.
 *
 * Ordering REUSES SN-2's {@link partitionNotes} (pinned-first, then recency)
 * so the switcher's empty-state order is byte-for-byte the rail's visible order
 * — no re-derivation. The dialog preserves input order on an empty query and
 * ranks by fuzzy score once typing starts, so pinned-first applies to the empty
 * state; a typed query ranks purely by title match (AC-1/AC-2).
 *
 * `label` carries the title — the dialog fuzzy-searches `max(label, sublabel)`,
 * so the title in `label` is what's matched (do NOT duplicate it into
 * `sublabel`). The switcher is single-kind (every item is a note), so `id` is
 * the note's own id — no encode/decode multiplexing (that's only the workspace
 * picker's mixed-kind concern). `onSelect` receives the note id verbatim.
 */
export function buildScratchNotePickerItems(notes: NoteMeta[]): QuickPickerItem[] {
  const { pinned, recent } = partitionNotes(notes);
  return [...pinned, ...recent].map((n) => ({
    id: n.id,
    // Fall back to "Untitled" (the create-note default) so a blank-title note
    // still renders a real row and stays fuzzy-searchable — an empty `label`
    // would show a blank row and score 0 against any typed query, making the
    // note reachable only by scrolling the empty-query list.
    label: n.title.trim() || "Untitled",
    sublabel: "",
  }));
}
