// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import type { Editor } from "@tiptap/react";
import { enumerateHeadings, headingSignature, type Heading } from "../lib/note-headings";

/**
 * SW-SN-5: the live heading source for the outline (and SW-SN-6's go-to-heading
 * jump — both import THIS hook; do not re-enumerate elsewhere).
 *
 * Rebuilds the heading list from the live ProseMirror doc on two signals:
 *  1. `editor.on("update")` — every doc change while typing/formatting. (NOT
 *     `"transaction"`, which also fires on pure caret moves and would rebuild the
 *     outline on every cursor move.)
 *  2. `noteKey` (the active note id) changing — a note switch loads content via
 *     `setContent(…, { emitUpdate: false })`, which suppresses the `update` event,
 *     so `update` alone would leave the outline showing the PREVIOUS note's
 *     headings. This effect is placed (at the call site) AFTER the panel's
 *     store→editor sync effect, so the doc already holds the new note when read.
 *
 * Signature-gated: `setHeadings` only fires when the heading list actually
 * changes, so body-only edits (which keep the same headings) trigger no
 * re-render of the outline or its host rail.
 */
export function useNoteHeadings(editor: Editor | null, noteKey: string | null): Heading[] {
  const [headings, setHeadings] = useState<Heading[]>([]);
  const sigRef = useRef<string | null>(null);

  const recompute = useCallback(() => {
    const next = editor ? enumerateHeadings(editor.state.doc) : [];
    const sig = headingSignature(next);
    if (sig !== sigRef.current) {
      sigRef.current = sig;
      setHeadings(next);
    }
  }, [editor]);

  // Live edits: the editor's own doc-change event. (The initial/editor-attach
  // recompute is handled by the note-load effect below — its `recompute` dep
  // changes when the editor attaches.)
  useEffect(() => {
    if (!editor) return;
    editor.on("update", recompute);
    return () => {
      editor.off("update", recompute);
    };
  }, [editor, recompute]);

  // Note-load path (emitUpdate:false setContent) + first mount. Must run AFTER
  // the panel's store→editor sync effect applies the new note (post-commit), so
  // it reads the fresh doc — `useSyncExternalStore` can't serve this (its
  // snapshot is read during render, before that setContent runs → stale). The
  // recompute is signature-gated, so it's a no-op setState unless the heading
  // list actually changed.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- external-source sync: the note-load setContent is emitUpdate:false, so this is the outline's only rebuild signal on a switch
    recompute();
  }, [recompute, noteKey]);

  return headings;
}
