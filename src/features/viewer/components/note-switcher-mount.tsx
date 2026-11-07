// SPDX-License-Identifier: MIT
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { useCallback, useEffect, useMemo, useState } from "react";

import { createDebouncedAction } from "@/shared/lib/debounced-action";
import { fuzzyScore } from "@/shared/lib/fuzzy-match";
import { logger } from "@/shared/lib/logger";
import { currentNotesScope } from "@/shared/lib/notes-scope";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { searchNotes, type NoteHit } from "@/tauri/commands";

import { QuickOpenDialog, type QuickPickerItem } from "./quick-open-dialog";
import { buildScratchNotePickerItems } from "../lib/scratch-note-picker-items";

interface NoteSwitcherMountProps {
  /** Only the active team's view renders the dialog (the open-flag is global,
   *  notes are global per-install, but at most one dialog should mount). */
  isActiveView: boolean;
}

/** SW-SN-11: how long typing must idle before the backend body scan fires.
 *  Short enough to feel live, long enough that a fast typist triggers one scan
 *  for the settled query, not one per keystroke (AC-4 "debounced"). */
const SEARCH_DEBOUNCE_MS = 150;

/**
 * SW-SN-3: the Cmd+O note quick-switcher — the keyboard accelerator over the
 * SN-2 rail's list. Extracted into its own leaf so its `notes` subscription
 * (which changes on every note open/create/rename — `setActiveNote` bumps
 * `lastOpenedAt`) re-renders only this tiny mount, not the whole 1700-line
 * `TeamWorkspaceView`. Owns all note-specific wiring; the generic
 * {@link QuickOpenDialog} stays item-source-agnostic (SW-SN-6 heading-jump reuse).
 *
 * SW-SN-11: also matches note **bodies**. The dialog surfaces its live query via
 * `onQueryChange`; this mount debounces a backend `search_notes` scan and merges
 * the body matches UNDER the title matches (via the dialog's generic `tier`):
 * title items tier 0, body items tier 1. All note-specific logic (scan, merge,
 * dedup) stays here — the dialog gains only two generic, opt-in props.
 */
export function NoteSwitcherMount({ isActiveView }: NoteSwitcherMountProps) {
  const noteSwitcherOpen = useUiStore((s) => s.noteSwitcherOpen);
  const setNoteSwitcherOpen = useUiStore((s) => s.setNoteSwitcherOpen);
  // In-memory notes (loaded by `loadNotes` at boot) — building items on open
  // does no disk I/O. Order reuses SN-2's `partitionNotes` inside the adapter.
  const notes = useViewerStore((s) => s.notes);

  // SW-SN-11 body-search state: the live query (trimmed, mirrored from the dialog
  // for the AC-2 title-dedup) and the most recent backend scan result — TAGGED
  // with the query it ran for. Tagging makes both staleness and the loading state
  // pure derivations: a result is "current" only when its query still matches, so
  // a slow earlier scan resolving after a newer one can never apply stale hits or
  // wrongly clear the pending state (runs are serialized+chained by the debouncer).
  const [query, setQuery] = useState("");
  const [bodyResult, setBodyResult] = useState<{ query: string; hits: NoteHit[] }>({
    query: "",
    hits: [],
  });

  // One debounced searcher for the mount's lifetime (lazy `useState` initializer
  // so it's created exactly once and stays stable across re-renders).
  // `createDebouncedAction` coalesces to the latest query, so a fast typist
  // triggers a single scan for the settled query. The result is applied as a
  // side-effect (the action is fire-and-forget by contract), tagged with its
  // query so a superseded scan is ignored by the derivations below.
  const [searcher] = useState(() =>
    createDebouncedAction(
      async (...args: unknown[]) => {
        const q = args[0] as string;
        // Always stamp a result tagged with `q` — even on failure. The backend
        // scan can reject (corrupt index, dir error); without stamping, the
        // derived `searching` flag would never clear and the switcher would hang
        // on "Searching…". On error we fall back to title-only (empty hits).
        try {
          // SLW-PT-7: resolve at scan time (snapshot) so the search hits the caller's
          // personal-team notes; the searcher is created once, so a closure-captured
          // id would go stale.
          setBodyResult({ query: q, hits: await searchNotes(currentNotesScope(), q) });
        } catch (e) {
          logger.error("viewer", `search_notes failed: ${e}`);
          setBodyResult({ query: q, hits: [] });
        }
      },
      SEARCH_DEBOUNCE_MS,
      "search_notes",
    ),
  );

  // Cancel a not-yet-fired scan on unmount. (A scan already awaiting IPC can't be
  // aborted here; its resolve is a harmless no-op set-state on an unmounted tree.)
  useEffect(() => () => searcher.cancel(), [searcher]);

  // The dialog owns the query; this mirrors the TRIMMED query here (so the dedup
  // agrees with the dialog's trimmed matching + the backend's trimmed scan) and
  // gates the scan. Empty query = no scan and no body rows (AC-4 — opening stays
  // instant).
  const handleQueryChange = useCallback(
    (raw: string) => {
      const q = raw.trim();
      setQuery(q);
      if (q === "") {
        searcher.cancel();
        setBodyResult({ query: "", hits: [] });
      } else {
        searcher.trigger(q);
      }
    },
    [searcher],
  );

  // A scan is in flight whenever the current non-empty query has no MATCHING
  // result yet — drives the dialog's "Searching…" affordance so a body-only query
  // doesn't flash "No notes found" during the debounce+IPC gap. Derived, so a
  // stale result (whose `query` differs) never clears it.
  const searching = query !== "" && bodyResult.query !== query;

  // Merge: title items (tier 0, dialog fuzzy-filters them by title) followed by
  // body items (tier 1). AC-2: a note matching by BOTH title and body appears
  // once, as a title match — so drop a body hit whose title already matches the
  // query. AC-3: body row shows the title as label, the matched line as sublabel.
  const items = useMemo<QuickPickerItem[]>(() => {
    const titleItems = buildScratchNotePickerItems(notes);
    // Only merge hits that belong to the CURRENT query — an empty query, a
    // superseded/late scan, or a no-match scan all fall back to titles only.
    if (query === "" || bodyResult.query !== query || bodyResult.hits.length === 0) {
      return titleItems;
    }
    // Reuse the title items the adapter already built (so a body row's label is
    // byte-for-byte its title row's — no re-deriving the "Untitled" fallback, no
    // drift). A hit with no matching title item means the note isn't in the
    // in-memory list (deleted since the scan, or not yet loaded) → skip it.
    const titleById = new Map(titleItems.map((i) => [i.id, i] as const));
    const bodyItems: QuickPickerItem[] = [];
    for (const hit of bodyResult.hits) {
      // Title-only hits are already covered by the local fuzzy title tier.
      if (hit.matched === "title") continue;
      const titleItem = titleById.get(hit.id);
      if (titleItem === undefined) continue;
      // Dedup exactly as the dialog decides a title match: it keeps a title item
      // when max(fuzzy(label), fuzzy(sublabel)) > 0, so mirror BOTH fields (title
      // items carry sublabel "" today, but this stays correct if a hint is added).
      const titleMatches =
        fuzzyScore(query, titleItem.label, { filenameFromPath: true }) > 0 ||
        fuzzyScore(query, titleItem.sublabel, { filenameFromPath: true }) > 0;
      if (titleMatches) continue;
      bodyItems.push({ id: hit.id, label: titleItem.label, sublabel: hit.snippet ?? "", tier: 1 });
    }
    return [...titleItems, ...bodyItems];
  }, [notes, bodyResult, query]);

  // The picker id IS the note id (single-kind list — no decode). Reveal the
  // notes surface first so the jump lands visibly even if the right pane was
  // closed or on another tab, then activate the note (SN-1 loads it; SN-2 AC-9
  // reveals it in the rail). `setActiveNote`'s body-read is async + off the
  // interaction path, so the dialog closes instantly. Body-match rows route
  // through this identical path (SW-SN-11 AC-5 — no new open path).
  const handleSelect = useCallback((id: string) => {
    const store = useViewerStore.getState();
    // Guard the render→select gap: if the note was deleted while the dialog was
    // open (e.g. by an agent over MCP), do nothing rather than reveal the
    // pane and fire a doomed `setActiveNote` that would strand the surface on
    // the previous note. `getState()` reads the freshest list at click time.
    if (!store.notes.some((n) => n.id === id)) return;
    revealSlot("right-pane", { surface: "notes", intent: { noteId: id } });
    void store.setActiveNote(id);
  }, []);

  return (
    <QuickOpenDialog
      open={noteSwitcherOpen && isActiveView}
      onClose={() => setNoteSwitcherOpen(false)}
      onSelect={handleSelect}
      onQueryChange={handleQueryChange}
      loading={searching}
      items={items}
      placeholder="Search notes…"
      emptyLabel="No notes found"
      // SW-FX-45: note labels are freeform titles, not filenames — a note titled
      // `Release.md` must render verbatim (agree with the SN-2 rail), not stripped.
      stripExtension={false}
    />
  );
}
