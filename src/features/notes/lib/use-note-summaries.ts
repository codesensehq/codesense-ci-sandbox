// SPDX-License-Identifier: MIT
import { useDeferredValue, useMemo } from "react";

import type { NoteMeta } from "@/tauri/commands";

import { summarize, type NoteSummary } from "./note-summary";

/**
 * Each note's preview for the list, from the records projection. The active note's
 * follows the editor's live content.
 */
export function useNoteListSummaries(
  notes: readonly NoteMeta[],
  activeId: string | null,
  activeContent: string,
  enabled: boolean,
): ReadonlyMap<string, NoteSummary> {
  const content = useDeferredValue(activeContent);
  const active = useMemo(() => (enabled ? summarize(content) : null), [content, enabled]);
  const listed = useMemo(
    () =>
      new Map<string, NoteSummary>(
        notes.map((n) => [n.id, { snippet: n.preview ?? "", refs: n.refs ?? [] }]),
      ),
    [notes],
  );
  return useMemo(() => {
    if (!active || !activeId) return listed;
    const out = new Map(listed);
    out.set(activeId, active);
    return out;
  }, [listed, active, activeId]);
}
