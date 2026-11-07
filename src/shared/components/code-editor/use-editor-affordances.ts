// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useViewerStore } from "@/shared/stores/viewer-store";

import {
  resolveEditorAffordances,
  type ResolvedEditorAffordances,
  DEFAULT_EDITOR_FOLDING,
  DEFAULT_EDITOR_TAB_SIZE,
} from "./editor-affordances";
import { DEFAULT_EDITOR_LINE_WRAP } from "./line-wrap";

/**
 * SW-VE-11: resolve every per-type editor affordance for `path` from the global
 * viewer settings — the ONE call each editable/viewer `<CodeEditor>` mount makes
 * so the affordances can't drift between the context inset, the source-worktree
 * inset, and the read-only viewer.
 *
 * Each setting is read via its own primitive selector (not a composite object)
 * so a store update to an UNRELATED setting doesn't churn this hook, and the
 * resolved object is memoized — a settings change is one cheap recompute + one
 * live CM6 reconfigure, never a remount. Fields default defensively so a
 * partial-state test store (or a pre-seed render) never throws.
 */
export function useEditorAffordances(path: string): ResolvedEditorAffordances {
  const editorLineWrap = useViewerStore(
    (s) => s.settings?.editorLineWrap ?? DEFAULT_EDITOR_LINE_WRAP,
  );
  const editorFolding = useViewerStore((s) => s.settings?.editorFolding ?? DEFAULT_EDITOR_FOLDING);
  const editorShowWhitespace = useViewerStore((s) => s.settings?.editorShowWhitespace ?? false);
  const editorTabSize = useViewerStore((s) => s.settings?.editorTabSize ?? DEFAULT_EDITOR_TAB_SIZE);
  const editorProseFont = useViewerStore((s) => s.settings?.editorProseFont ?? false);

  return useMemo(
    () =>
      resolveEditorAffordances(
        { editorLineWrap, editorFolding, editorShowWhitespace, editorTabSize, editorProseFont },
        path,
      ),
    [editorLineWrap, editorFolding, editorShowWhitespace, editorTabSize, editorProseFont, path],
  );
}
