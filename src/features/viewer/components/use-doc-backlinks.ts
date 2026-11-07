// SPDX-License-Identifier: MIT
import { useLayoutEffect, useState } from "react";

import { listNotes, readNote, type NoteMeta } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { currentNotesScope } from "@/shared/lib/notes-scope";

import { noteBodyLinksDoc } from "../lib/doc-relations";
import type { CsLinkResolveContext } from "../lib/note-links";

interface UseDocBacklinksArgs {
  /** Gate the scan — only runs while the popover is open (on-demand, SW-SN-10 AC-3). */
  enabled: boolean;
  activeTabPath: string;
  /** Stable per (activeTabPath, tree, teamContentPath) — memoized by the caller. */
  resolveCtx: CsLinkResolveContext;
}

interface DocBacklinksState {
  backlinks: NoteMeta[];
  loading: boolean;
}

const EMPTY: DocBacklinksState = { backlinks: [], loading: false };

/** SW-SN-10 Task 3: derive the notes that link the open doc.
 *
 *  The scan is N Tauri/disk round-trips (`list_notes` + one `read_note` per note —
 *  note bodies are NOT in the store), so it runs in an effect OFF the render path,
 *  never blocking the chip or its Supporting-docs section. It re-runs each time the
 *  popover opens (`enabled` false→true), the doc changes, or the tree changes — so
 *  an in-place note edit is always reflected on the next open (no stale cache). The
 *  bodies are read CONCURRENTLY, so a re-scan is ~one round-trip of latency. Nothing
 *  is persisted and the repo doc is never touched (AC-3 / AC-6).
 */
export function useDocBacklinks({
  enabled,
  activeTabPath,
  resolveCtx,
}: UseDocBacklinksArgs): DocBacklinksState {
  const [state, setState] = useState<DocBacklinksState>(EMPTY);

  // Layout effect (pre-paint) so the loading reset commits before the browser
  // paints the opening popover — otherwise the enabling render shows the empty
  // (first open) or previous-scan (reopen) state for one frame before the scan
  // flips `loading`. The reset is per-open, so state can't persist across opens.
  useLayoutEffect(() => {
    if (!enabled || !activeTabPath) return;
    let cancelled = false;
    // A cancelled scan never leaves `loading` stuck true (fresh state each run).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setState({ backlinks: [], loading: true });

    void (async () => {
      try {
        // SLW-PT-7: notes resolve to the caller's personal team (identity-scoped).
        const scope = currentNotesScope();
        const notes = await listNotes(scope);
        if (cancelled) return;
        // Read bodies CONCURRENTLY (each is a Tauri/disk round-trip) so N notes
        // cost ~one round-trip of latency, not N in series. A note that vanished
        // mid-scan resolves to null and just doesn't contribute a backlink.
        const bodies = await Promise.all(
          notes.map((meta) =>
            readNote(scope, meta.id)
              .then((n) => n.content)
              .catch((e) => {
                logger.error("viewer", `useDocBacklinks: readNote(${meta.id}) failed: ${e}`);
                return null;
              }),
          ),
        );
        if (cancelled) return;
        const matches = notes.filter(
          (_, i) => bodies[i] !== null && noteBodyLinksDoc(bodies[i]!, activeTabPath, resolveCtx),
        );
        setState({ backlinks: matches, loading: false });
      } catch (e) {
        if (cancelled) return;
        logger.error("viewer", `useDocBacklinks: scan failed: ${e}`);
        setState({ backlinks: [], loading: false });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [enabled, activeTabPath, resolveCtx]);

  // While disabled (popover closed / no doc) report empty without a reset render;
  // the effect re-scans from scratch on the next open.
  return enabled && activeTabPath ? state : EMPTY;
}
