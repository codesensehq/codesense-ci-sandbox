// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useScopeActive, type ScopeKey } from "@/shared/lib/warm-scopes";
import { detachWorktreeWatch, lazyAttachWorktreeWatch } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

/**
 * SLW-WD-9 (RD-13): a REF-COUNTED worktree watcher shared by every consumer of a
 * workspace's `worktree-file-change` signal.
 *
 * The backend keeps ONE watch per `wsId` (token-matched detach). Before WD-9 only
 * `source-editor-inset.tsx` attached it — so the source-diff overlay saw no live
 * events unless the editor inset happened to be open, and a naive second attach
 * would replace the first's token, letting whichever consumer closed last tear
 * the watch out from under the other. This hook fixes that: a module-level
 * refcount attaches on 0→1 and detaches on 1→0, so the inset + the overlay (and
 * any future consumer) share exactly one OS watch with no strand.
 */

interface WatchEntry {
  count: number;
  cwd: string;
  /** The attach token once resolved; `null` while the attach is in flight. */
  token: number | null;
}

const watches = new Map<string, WatchEntry>();

function acquire(wsId: string, cwd: string): void {
  const existing = watches.get(wsId);
  // An entry released to 0 while its attach is in flight is dead: its `.then` detaches it, and a
  // new consumer (possibly another team's worktree under the same id) attaches its own.
  if (existing && existing.count > 0) {
    existing.count++;
    return;
  }
  const entry: WatchEntry = { count: 1, cwd, token: null };
  watches.set(wsId, entry);
  // Fire-and-forget so a watch failure never blocks the mount.
  lazyAttachWorktreeWatch(wsId, cwd)
    .then((token) => {
      if (entry.count === 0) {
        // Everyone released before the attach resolved → detach immediately.
        detachWorktreeWatch(wsId, token).catch(() => {});
        if (watches.get(wsId) === entry) watches.delete(wsId);
        return;
      }
      entry.token = token;
    })
    .catch((err) => {
      logger.warn("useWorktreeWatch", `worktree watch attach failed for ${wsId} — ${err}`);
    });
}

function release(wsId: string): void {
  const entry = watches.get(wsId);
  if (!entry) return;
  entry.count--;
  if (entry.count > 0) return;
  // Attach still in flight — its `.then` sees count 0 and detaches + removes.
  if (entry.token === null) return;
  detachWorktreeWatch(wsId, entry.token).catch(() => {});
  watches.delete(wsId);
}

/**
 * Attach the worktree watcher for `wsId` while `enabled` (and detach on cleanup),
 * ref-counted so multiple consumers share one watch. Idempotent per consumer —
 * each mount acquires exactly one reference. Workspace ids repeat across teams while the backend
 * keeps one watch per id, so a retained hidden team (`scope` not active) holds none: the shown
 * team's worktree is the one watched. When the team is shown again the hook signals "resumed"
 * (a change-nonce bump) so consumers catch up on what the missed window changed.
 */
export function useWorktreeWatch(
  wsId: string,
  cwd: string,
  enabled: boolean,
  scope?: ScopeKey | null,
): void {
  const ownsWatch = useScopeActive(scope);
  const suspendedRef = useRef(false);
  useEffect(() => {
    if (!enabled || !wsId || !cwd) return;
    if (!ownsWatch) {
      suspendedRef.current = true;
      return;
    }
    acquire(wsId, cwd);
    // The native attach emits no catch-up, so edits made while this team was hidden are
    // invisible. Resume signals the shared per-workspace change nonce once; every consumer
    // reconciles through its existing disk-change path (a dirty editor takes its conflict path).
    if (suspendedRef.current) {
      suspendedRef.current = false;
      useViewerStore.getState().bumpWorktreeChangedPath(wsId, "", true);
    }
    return () => release(wsId);
  }, [enabled, ownsWatch, wsId, cwd]);
}

/** Test-only: reset the shared refcount map between cases. */
export function __resetWorktreeWatch(): void {
  watches.clear();
}
