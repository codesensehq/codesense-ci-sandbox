// SPDX-License-Identifier: MIT
import { useCallback, useState } from "react";
import { toast } from "sonner";

import { readFile, writeContentFile } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import type { StoryRef } from "@/shared/lib/pipeline-graph/types";
import { attachSupportingDoc } from "@/features/dashboard/lib/attach-supporting-doc";

/** An open story-picker session: the canvas being attached + the candidate
 *  stories (its enclosing epic, already resolved). */
export type CanvasAttachPicker = { canvasPath: string; stories: StoryRef[] };

/**
 * SW-SC-12: the nav-local attach flow behind the `.excalidraw` "Attach to
 * story…" context-menu item.
 *
 * The candidate-story *resolution* is a pure, in-memory tree walk done by the
 * caller (`collectAllStories(findEnclosingEpic(...))`) — this hook owns only
 * the side-effecting half: the idempotent write and the picker open-state.
 * The only disk I/O — one `readFile` + one atomic `writeContentFile` — happens
 * on an explicit story pick, never on menu open (AC-6). Attach behaviour is
 * SC-2's `attach-supporting-doc` logic, reused unchanged and retargeted from
 * "folder-local sibling" to "the picked story".
 */
export function useAttachCanvasToStory() {
  const [picker, setPicker] = useState<CanvasAttachPicker | null>(null);

  /** Write the canvas as a supporting doc of `story` (idempotent). */
  const attachTo = useCallback(async (canvasPath: string, story: StoryRef) => {
    const label = stripViewerExtension(story.fileName);
    try {
      const { changed } = await attachSupportingDoc(story.filePath, canvasPath, {
        readFile,
        writeContentFile,
      });
      if (changed) toast.success(`Attached to ${label}`);
      else toast(`Already attached to ${label}`);
    } catch (err) {
      logger.error("viewer", `attach ${canvasPath} to ${story.filePath} failed: ${err}`);
      toast.error("Couldn't attach the canvas to that story.");
    }
  }, []);

  /**
   * Drive the menu-item click given the pre-resolved candidate set: exactly
   * one story attaches immediately (no modal); two or more open the picker.
   * Zero is guarded upstream — the menu item renders disabled — so this is a
   * defensive no-op for an empty set.
   */
  const open = useCallback(
    (canvasPath: string, stories: StoryRef[]) => {
      if (stories.length === 1) {
        void attachTo(canvasPath, stories[0]);
      } else if (stories.length > 1) {
        setPicker({ canvasPath, stories });
      }
    },
    [attachTo],
  );

  /** Resolve a picker selection (the chosen story's `filePath`) and attach.
   *  The write is fired outside the state updater (updaters must stay pure —
   *  React double-invokes them under StrictMode). */
  const selectFromPicker = useCallback(
    (filePath: string) => {
      if (!picker) return;
      const story = picker.stories.find((s) => s.filePath === filePath);
      setPicker(null);
      if (story) void attachTo(picker.canvasPath, story);
    },
    [picker, attachTo],
  );

  /** Dismiss the picker without attaching. */
  const closePicker = useCallback(() => setPicker(null), []);

  return { picker, open, selectFromPicker, closePicker };
}
