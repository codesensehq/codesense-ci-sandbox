// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { privateFile } from "@/shared/lib/private-file";

interface FileRefreshState {
  fileRefreshKeys: Record<string, number>;
  /**
   * SF-3 review patch (Option B): per-repo set of file paths that changed
   * in the most recent debounce burst. `null` is the broad-refresh sentinel
   * — Layer 2 fast-path can't skip the read.
   *
   * Before this refactor the value was `string | null` (last-changed path).
   * That contract was incompatible with React 18 / Zustand batching: when
   * a burst contained N>1 paths, the debounce flush fired N synchronous
   * `set` calls, but React batched them into ONE re-render with the FINAL
   * path only. Viewers for the first N-1 changed files saw the wrong
   * `changedFilePath` and incorrectly took the Layer 2 fast-path, leaving
   * stale content displayed. Tracking a Set (single set call per burst)
   * resolves the batching gap.
   */
  lastChangedContextFile: Record<string, Set<string> | null>;
  /**
   * SR-9: per-repo viewer-facing content-version. Distinct from `fileRefreshKeys`
   * (the pipeline-scan counter): this advances on content-affecting bumps ONLY and
   * is **never** advanced by a `counterOnly` pipeline-scan bump. Content viewers gate
   * their `readFile` on this key (via `useEffectiveRefreshKey`), so a `ContentLayerEvent`
   * materialize storm — which counter-only bumps `fileRefreshKeys` ~1×/sec — can no
   * longer drive a redundant re-read/re-render of an open, unchanged file. Pipeline/scan
   * consumers keep reading `fileRefreshKeys` (they still need the counter-only tick);
   * only viewers read this slice. Kept coherent with the prefetch cache stamp in
   * `team-workspace-view` so the viewer's Layer-1 cache token still hits.
   */
  contentRefreshKeys: Record<string, number>;
  /**
   * @param repoPath  Repo to bump. Omit for broad bump-all.
   * @param changedPaths  Set of file paths that changed in this burst. `undefined`
   *   widens the change scope (broad refresh — viewers can't skip). One call per
   *   debounce burst (not per path) so React batches a single store update.
   * @param counterOnly  SW-SY-4: bump ONLY the `fileRefreshKeys` scan counter
   *   (pipeline/publish-set), leaving `lastChangedContextFile` AND (SR-9)
   *   `contentRefreshKeys` untouched. Used by a pure-forward `ContentLayerEvent` wave,
   *   which never changes a context file a viewer displays — so it must not overwrite a
   *   concurrent reverse wave's precise Set with the broad `null` sentinel, nor advance
   *   the viewer content-version. Ignored on the bump-all path.
   */
  bumpFileRefresh: (repoPath?: string, changedPaths?: Set<string>, counterOnly?: boolean) => void;
  /**
   * SW-FX-64 (AC-8): drop refresh-key entries for repos that were fully
   * evicted (F-19 workspace eviction). Both records grow add-only as distinct
   * repo keys are touched, so without this they accumulate one dead slot per
   * repo/worktree ever opened in a long session. Deletes each `repoPath` from
   * BOTH `fileRefreshKeys` and `lastChangedContextFile`.
   *
   * Callers pass the **synthetic** workspace keys returned by
   * `evictSuspendedWorkspace` (team-scoped: `workspace:{org}:{team}:...`), never
   * a shared source-disk path — a source path can back a still-mounted team, so
   * pruning it would be wrong. Safe by construction: an evicted workspace has no
   * mounted viewers, and a later re-resolve simply re-seeds the key from 0.
   *
   * SR-9: also drops the `contentRefreshKeys` entry — same add-only-growth argument.
   */
  pruneRepos: (repoPaths: string[]) => void;
}

export const useFileRefreshStore = create<FileRefreshState>((set) => ({
  fileRefreshKeys: {},
  lastChangedContextFile: {},
  contentRefreshKeys: {},

  bumpFileRefresh: (repoPath?: string, changedPaths?: Set<string>, counterOnly?: boolean) => {
    // Empty string is a bug indicator from a caller (the intentional
    // bump-all path uses `undefined`, not `""`). Without this guard `""`
    // falls into the falsy branch below and resets every repo's
    // lastChangedContextFile — destructive. Defensive return at the
    // truthful module boundary.
    if (repoPath === "") return;
    if (repoPath) {
      // SW-SY-4: counter-only — advance the scan counter without touching the
      // per-repo changed-paths Set (see `counterOnly` doc above). SR-9: also leaves
      // `contentRefreshKeys` untouched, so a `ContentLayerEvent` storm cannot drive a
      // viewer re-read of an open, unchanged file.
      if (counterOnly) {
        set((s) => ({
          fileRefreshKeys: {
            ...s.fileRefreshKeys,
            [repoPath]: (s.fileRefreshKeys[repoPath] ?? 0) + 1,
          },
        }));
        return;
      }
      // Content-affecting bump: advance BOTH the scan counter and the viewer
      // content-version (SR-9), and record the changed-paths Set.
      set((s) => ({
        fileRefreshKeys: {
          ...s.fileRefreshKeys,
          [repoPath]: (s.fileRefreshKeys[repoPath] ?? 0) + 1,
        },
        contentRefreshKeys: {
          ...s.contentRefreshKeys,
          [repoPath]: (s.contentRefreshKeys[repoPath] ?? 0) + 1,
        },
        lastChangedContextFile: {
          ...s.lastChangedContextFile,
          [repoPath]: changedPaths ?? null,
        },
      }));
    } else {
      // Bump all — a broad content refresh: advance both counters and reset
      // lastChangedContextFile for every repo. `contentRefreshKeys` keys are a
      // subset of `fileRefreshKeys` keys (every content bump touches the scan
      // counter first), so one pass over `fileRefreshKeys` covers both.
      set((s) => {
        const keys = { ...s.fileRefreshKeys };
        const contentKeys = { ...s.contentRefreshKeys };
        const changed = { ...s.lastChangedContextFile };
        for (const key of Object.keys(keys)) {
          keys[key] = (keys[key] ?? 0) + 1;
          contentKeys[key] = (contentKeys[key] ?? 0) + 1;
          changed[key] = null;
        }
        return {
          fileRefreshKeys: keys,
          contentRefreshKeys: contentKeys,
          lastChangedContextFile: changed,
        };
      });
    }
  },

  pruneRepos: (repoPaths: string[]) => {
    if (repoPaths.length === 0) return;
    set((s) => {
      const doomed = new Set(repoPaths);
      const keys = { ...s.fileRefreshKeys };
      const changed = { ...s.lastChangedContextFile };
      const contentKeys = { ...s.contentRefreshKeys };
      let mutated = false;
      for (const key of doomed) {
        if (key in keys) {
          delete keys[key];
          mutated = true;
        }
        if (key in changed) {
          delete changed[key];
          mutated = true;
        }
        if (key in contentKeys) {
          delete contentKeys[key];
          mutated = true;
        }
      }
      // Preserve referential identity when nothing matched — avoids a needless
      // store update (and re-render churn) on an evict that touched no keys.
      if (!mutated) return {};
      return {
        fileRefreshKeys: keys,
        lastChangedContextFile: changed,
        contentRefreshKeys: contentKeys,
      };
    });
  },
}));

/**
 * SW-FX-139: record that one content file was saved in the editor, so an open read
 * view re-reads it from disk. Each call is its own burst, carrying only that path:
 * the changed set is what the last burst changed, and it is never cleared after
 * viewers consume it, so merging into it would keep stale paths and broad bursts.
 * Private files use their own content root even when the nav shows another source.
 * An empty repo path is a no-op for other files.
 */
export function markContentPathChanged(repoPath: string, filePath: string): void {
  const root = privateFile(filePath)?.contentRoot ?? repoPath;
  if (!root) return;
  useFileRefreshStore.getState().bumpFileRefresh(root, new Set([filePath]));
}
