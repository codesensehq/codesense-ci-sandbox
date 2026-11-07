// SPDX-License-Identifier: MIT
import { useCallback, useState } from "react";
import { toast } from "sonner";

import { readFile, writeContentFile } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { attachSupportingDoc } from "@/features/dashboard/lib/attach-supporting-doc";

/** An open attach-to-node session: the Documentation doc being attached (its
 *  absolute path). The picker's candidate nodes are supplied by the caller. */
export type DocAttachPicker = { docPath: string };

/**
 * SW-GS-11: the side-effecting half of the sync-view "Attach to a story/epic"
 * flow. Sibling to {@link useAttachCanvasToStory} (SW-SC-12) — both drive a node
 * picker over the same `attachSupportingDoc` write, retargeted here from a canvas
 * to a loose Documentation file and from `StoryRef`s to pipeline nodes.
 *
 * Lives in `dashboard/lib` (not `git-sync/lib`) because it touches
 * `@/tauri/commands`, which the `no-direct-invoke` guard forbids in the
 * `git-sync` feature. The hook deals only in primitive `(mdPath, label)`
 * strings, so it carries no feature-specific type coupling — the caller maps
 * its nodes to those.
 *
 * The only disk I/O — one `readFile` + one atomic `writeContentFile` — happens on
 * an explicit node pick, never on picker open. The `.md` write is
 * watcher-suppressed but self-nudges `context-file-change` for pipeline files, so
 * the sync list re-groups automatically (no manual refresh here).
 */
export function useAttachDocToNode() {
  const [picker, setPicker] = useState<DocAttachPicker | null>(null);

  /** Record `docPath` into `mdPath`'s `supporting_docs` (idempotent). `label` is
   *  the human node name for the toast. */
  const attachTo = useCallback(async (docPath: string, mdPath: string, label: string) => {
    try {
      const { changed } = await attachSupportingDoc(mdPath, docPath, {
        readFile,
        writeContentFile,
      });
      if (changed) toast.success(`Attached to ${label}`);
      else toast(`Already attached to ${label}`);
    } catch (err) {
      logger.error("attach-doc", `attach ${docPath} to ${mdPath} failed: ${err}`);
      toast.error("Couldn't attach that doc.");
    }
  }, []);

  /** Open the node picker for a Documentation doc. */
  const open = useCallback((docPath: string) => setPicker({ docPath }), []);

  /** Dismiss the picker without attaching. */
  const closePicker = useCallback(() => setPicker(null), []);

  /** Resolve a picker selection (the chosen node's own `.md` path + label) and
   *  attach. The write fires outside the state updater (updaters must stay pure —
   *  React double-invokes them under StrictMode). */
  const selectNode = useCallback(
    (mdPath: string, label: string) => {
      if (!picker) return;
      const { docPath } = picker;
      setPicker(null);
      void attachTo(docPath, mdPath, label);
    },
    [picker, attachTo],
  );

  return { picker, open, closePicker, selectNode };
}
