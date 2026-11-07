// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import { notesHaveProjection, notesScopeKey, type NotesScope } from "@/shared/lib/notes-scope";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { recordsPrepareNativeView } from "@/tauri/commands";

/** Every note of the org, not the first page a pane loads. */
const ALL_NOTES: [number, number] = [0, 100_000];

const watches = new Map<string, { count: number; stop: () => void }>();

/**
 * Keep the notes the viewer holds on `scope`'s records projection, the same view a
 * Notes pane subscribes to: an agent's change arrives as one delta. Ref-counted per
 * scope. Notes kept on this Mac with no org have no projection (only the person
 * writes them), so they are read once per load instead.
 */
export function watchNotesProjection(scope: NotesScope): () => void {
  const org = scope.orgSlug;
  if (!org || !notesHaveProjection(scope)) return () => {};
  const key = notesScopeKey(scope);
  let watch = watches.get(key);
  if (!watch) {
    let stopped = false;
    let unwatch = () => {};
    watch = {
      count: 0,
      stop: () => {
        stopped = true;
        unwatch();
      },
    };
    watches.set(key, watch);
    void (async () => {
      try {
        const viewKey = await recordsPrepareNativeView(org, "notes");
        if (stopped || !viewKey) return;
        unwatch = liveHub.watch(
          "records",
          viewKey,
          (view, change) =>
            useViewerStore.getState().applyNotesProjection(scope, view.snapshot, change),
          { window: ALL_NOTES },
        );
      } catch (e) {
        logger.error("notes", `Cannot watch ${org}'s notes: ${String(e)}`);
      }
    })();
  }
  watch.count++;
  const current = watch;
  return () => {
    if (--current.count > 0) return;
    current.stop();
    if (watches.get(key) === current) watches.delete(key);
  };
}

/** App-root bridge: the viewer's notes follow the current profile and org's projection. */
export function useNotesProjection(): void {
  const githubUserId = useCurrentUser()?.github_user_id ?? 0;
  const orgSlug = useOrgStore((s) => s.activeOrgSlug) ?? null;
  useEffect(() => watchNotesProjection({ githubUserId, orgSlug }), [githubUserId, orgSlug]);
}
