// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { checkBranchExists, getWorktreeConfig, storyDefaultStartPoint } from "@/tauri/commands";
import { resolveWorktreeParams } from "@/shared/lib/worktree/resolve";
import { logger } from "@/shared/lib/logger";
import type { Workspace } from "@/shared/stores/viewer-store";
import { useOrgRoot } from "@/features/git-sync/lib/use-org-root";
import {
  forceChangeset,
  sourceCommitChangeset,
  sourceCommitFileRedline,
  sourceCommitGraph,
  sourceFileRedline,
  sourceRedline,
  subscribeChangeset,
  tickChangeset,
  type CadencePreset,
  type SourceRepo,
} from "@/features/git-sync/lib/source-redline-source";
import { pickCadencePreset } from "@/features/viewer/lib/use-remote-cadence";
import type {
  FileRedline,
  PublishFile,
  SourceCommitGraphEntry,
} from "@/features/git-sync/lib/types";
import { getCommitGraph, setCommitGraph } from "@/features/viewer/lib/commit-graph-cache";
import { describeDiffError } from "./source-diff-errors";

/** Commits fetched per page — first page paints fast, older pages on scroll (AC-7). */
const GRAPH_PAGE_SIZE = 60;

/**
 * SLW-WD-2 — the data hooks behind the source-diff review overlay. These live
 * OUTSIDE `git-sync` (so they MAY import `@/tauri/commands` for the config /
 * branch-existence reads), and reach the diff itself only through the sanctioned
 * `source-redline-source.ts` seam — never `invoke` directly.
 */

export interface SourceDiffBase {
  /** The resolved fork-parent baseRef (`undefined` = fork-from-default → WD-1's
   *  `origin/main` merge-base ladder; a branch name = a STACKED story). */
  baseRef: string | undefined;
  /** The worktree config's default start point (e.g. `"main"`) — the name the
   *  branch forks from, for the SLW-DV-7 comparison label when `baseRef` is
   *  `undefined`. Defaults to `DEFAULT_START_POINT` until the config resolves. */
  defaultStart: string;
}

/** The label for the branch's fork parent until it resolves, or when neither the
 *  worktree config nor the repo's refs name one. */
const DEFAULT_START_POINT = "main";

/** The branch name in a start point the backend resolved: `refs/remotes/origin/develop`
 *  → `origin/develop`, `refs/heads/main` → `main`. */
function startPointLabel(startPoint: string): string {
  return startPoint.replace(/^refs\/(?:remotes|heads)\//, "");
}

/** The name a story branch forks from by default: the configured `default_start_point`,
 *  else the repo's default branch as worktree creation resolves it
 *  (`storyDefaultStartPoint`), so the label matches the base a new branch got. */
async function defaultStartName(
  sourceRepoPath: string,
  configured: string | null | undefined,
): Promise<string> {
  // An empty configured value falls through too: the DV-7 label never names a blank ref.
  if (configured) return configured;
  try {
    return startPointLabel(await storyDefaultStartPoint(sourceRepoPath, null));
  } catch {
    return DEFAULT_START_POINT;
  }
}
const DEFAULT_SOURCE_DIFF_BASE: SourceDiffBase = {
  baseRef: undefined,
  defaultStart: DEFAULT_START_POINT,
};

/**
 * AC-10 — resolve the fork-parent `baseRef` for a story worktree by REUSING the
 * creation-time resolver, so the diff baseline is EXACTLY the start-point the
 * worktree was born from (the precedent is `viewer-store.ts:ensureWorktree`):
 *
 *  - fork-from-default (the common case): no start point, or `startPoint ===
 *    default_start_point` → `baseRef: undefined`, so WD-1 merge-bases against the
 *    default branch.
 *  - a STACKED story (non-empty `depends_on`): `startPoint` = the dependency's
 *    branch → thread that branch so the diff shows only this story's own work.
 *  - the dependency branch isn't created locally yet (the `checkBranchExists`
 *    F-29 edge) → fall back to `undefined` (a documented mild v1 over-report).
 *
 * SLW-DV-7: also returns `defaultStart` so the header can name the base without a
 * second config read. Off the render path (an effect + a config/doc read);
 * returns the default until resolved and on any failure.
 */
export function useSourceDiffBase(ws: Workspace): SourceDiffBase {
  // The context org_root for the story's SOURCE repo — the same root worktree
  // creation resolves the config against (`getWorktreeConfig(sourceRepo, orgRoot)`).
  const orgRoot = useOrgRoot(ws.sourceRepoPath ?? "");
  const sourceRepoPath = ws.sourceRepoPath;
  const storyFilePath = ws.storyFilePath;

  const [base, setBase] = useState<SourceDiffBase>(DEFAULT_SOURCE_DIFF_BASE);
  // Reset in the RENDER phase when the resolution inputs change (a workspace
  // switch) — the codebase's adjust-state-during-render idiom (cf.
  // `RedlineProse`/`BeforeAfterDiffToggle`), so the effect only ever setState()s
  // inside its async callback (never synchronously in its body).
  const inputKey = JSON.stringify([sourceRepoPath, storyFilePath, orgRoot]);
  const [seenKey, setSeenKey] = useState(inputKey);
  if (seenKey !== inputKey) {
    setSeenKey(inputKey);
    setBase(DEFAULT_SOURCE_DIFF_BASE);
  }

  useEffect(() => {
    if (!sourceRepoPath || !storyFilePath || !orgRoot) return;
    let cancelled = false;
    (async () => {
      try {
        const config = await getWorktreeConfig(sourceRepoPath, orgRoot);
        const { startPoint } = await resolveWorktreeParams(storyFilePath, config);
        const configuredStart = config?.default_start_point;
        const defaultStart = await defaultStartName(sourceRepoPath, configuredStart);
        // fork-from-default → undefined (WD-1's merge-base ladder). Otherwise the
        // start-point is a dependency branch — verify it exists locally before
        // threading it, else fall back to undefined (F-29 edge, not an error).
        // A falsy start-point can't be a real dependency branch, so treat it as
        // fork-from-default too (never thread an empty `baseRef`).
        if (!startPoint || startPoint === configuredStart) {
          if (!cancelled) setBase({ baseRef: undefined, defaultStart });
          return;
        }
        const exists = await checkBranchExists(sourceRepoPath, startPoint);
        if (!cancelled) setBase({ baseRef: exists ? startPoint : undefined, defaultStart });
      } catch {
        if (!cancelled) setBase(DEFAULT_SOURCE_DIFF_BASE);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sourceRepoPath, storyFilePath, orgRoot]);

  return base;
}

export interface SourceChangeset {
  files: PublishFile[] | null;
  loading: boolean;
  error: string | null;
  reload: () => void;
}

/**
 * AC-2 — the flat change-set for a source worktree, fetched LAZILY (only when
 * `enabled`). Threads the resolved `baseRef` (AC-10). Tracks loading/error and
 * exposes a `reload` (user-force bypass) for the error-row retry.
 *
 * DESIGN-remote-phase-2.md B-1 fix — this hook now routes through the seam's
 * `subscribeChangeset` so every consumer on `(repo, baseRef)` shares one
 * debounced fetch per window. Cold-fetch is immediate (first paint has no
 * delay). Subsequent `worktreeNonce` bumps are reported as `tickChangeset`
 * so the leading-edge coalesce runs. `reload` fires `forceChangeset` which
 * bypasses the debounce and cancels any armed timer.
 */
export function useSourceChangeset(
  repo: string | SourceRepo,
  baseRef: string | undefined,
  enabled: boolean,
  worktreeNonce = 0,
): SourceChangeset {
  // Back-compat: a plain `string` worktree path is a local repo. The remote
  // branch is reached by passing a `{kind: "remote", ...}` SourceRepo.
  const resolvedRepo: SourceRepo = typeof repo === "string" ? { kind: "local", cwd: repo } : repo;
  const identityKey =
    resolvedRepo.kind === "local"
      ? resolvedRepo.cwd
      : `${resolvedRepo.machineId}\x1f${resolvedRepo.handle}`;

  const [files, setFiles] = useState<PublishFile[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Latest files, readable inside the async catch without re-subscribing the
  // effect. Updated in an effect (not during render) so the render stays pure.
  const filesRef = useRef<PublishFile[] | null>(null);
  useEffect(() => {
    filesRef.current = files;
  }, [files]);

  // Render-phase HARD reset — only when WHAT we are diffing changes (worktree
  // switch, baseRef change, enable/disable): clear the prior worktree's files and
  // flip to the skeleton.
  const willFetch = enabled && !!identityKey;
  const resetKey = JSON.stringify([enabled, identityKey, baseRef]);
  const [seenKey, setSeenKey] = useState(resetKey);
  if (seenKey !== resetKey) {
    setSeenKey(resetKey);
    setFiles(null);
    setError(null);
    setLoading(willFetch);
  }
  const currentKeyRef = useRef(resetKey);
  // A layout effect: current before any promise settling after this commit.
  useLayoutEffect(() => {
    currentKeyRef.current = resetKey;
  }, [resetKey]);
  // Only the latest manual reload settles: an older one finishing late (a
  // rejection after a newer success) must not overwrite it.
  const reloadSeqRef = useRef(0);

  // DESIGN-remote-phase-2.md §1 — the subscribe loop. One subscription per
  // `(repo, baseRef, enabled)` identity; its onUpdate/onError feed the hook's
  // state. Leaving the hook (change of identity, unmount) tears down the
  // subscription — the seam cancels any pending timer.
  const subscribedCadenceRef = useRef<CadencePreset | null>(null);
  useEffect(() => {
    if (!enabled || !identityKey) return;
    const cadence = pickCadencePreset(resolvedRepo, null);
    subscribedCadenceRef.current = cadence;
    const unsubscribe = subscribeChangeset(
      resolvedRepo,
      baseRef,
      cadence,
      (result) => {
        setFiles([...result]); // own the array; the seam hands a readonly
        setError(null);
        setLoading(false);
      },
      (e) => {
        // A revalidation failure over a list we already have keeps the stale
        // list up (a transient git hiccup mid-edit must not blank a good
        // diff); only an empty pane surfaces the error row.
        if (filesRef.current == null) setError(describeDiffError(e, "changes"));
        else
          logger.warn(
            "useSourceChangeset",
            `stale changeset kept; revalidation failed — ${describeDiffError(e, "changes")}`,
          );
        setLoading(false);
      },
      { opClass: "diff" },
    );
    return unsubscribe;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identityKey, baseRef, enabled]);

  // DESIGN-remote-phase-2.md §1 — nonce bumps go through `tickChangeset`
  // (debounced, coalesced). The LOCAL path still receives OS-watcher ticks via
  // `worktreeNonce` from the viewer store; the REMOTE path's ticks come from
  // the per-handle event listener in `useRemoteWorktreeWatch` which already
  // calls `tickChangesetsForRepo` directly — this effect only matters for
  // local tabs, but is written repo-agnostically to keep the shape simple.
  useEffect(() => {
    if (!enabled || !identityKey || worktreeNonce === 0) return;
    tickChangeset(resolvedRepo, baseRef);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identityKey, baseRef, enabled, worktreeNonce]);

  // Manual retry (header "Refresh" / error-row retry) — bypass the debounce.
  const reload = useCallback(() => {
    if (!enabled || !identityKey) return;
    // Retrying an empty (errored) pane shows the loading state, so the click
    // visibly does something; a refresh over a good list keeps it up.
    if (filesRef.current == null) setLoading(true);
    // A refresh that settles after the basis changed (another worktree, `b`)
    // must not touch the new basis's list.
    const key = resetKey;
    const seq = ++reloadSeqRef.current;
    void forceChangeset(resolvedRepo, baseRef).then(
      (result) => {
        if (currentKeyRef.current !== key || reloadSeqRef.current !== seq) return;
        // The subscription normally delivers this too; a deduped or unchanged
        // result may not, and the skeleton must never stick.
        if (filesRef.current == null) setFiles([...result]);
        setError(null);
        setLoading(false);
      },
      (e: unknown) => {
        if (currentKeyRef.current !== key || reloadSeqRef.current !== seq) return;
        setLoading(false);
        if (filesRef.current == null) setError(describeDiffError(e, "changes"));
        else
          logger.warn(
            "useSourceChangeset",
            `stale changeset kept; forced refresh failed — ${describeDiffError(e, "changes")}`,
          );
      },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identityKey, baseRef, enabled, resetKey]);

  return { files, loading, error, reload };
}

export interface SourceRedline {
  redline: FileRedline | null;
  error: string | null;
}

/**
 * AC-3 — the before/after redline for one selected file, fetched via the same
 * `source-redline-source.ts` seam + `baseRef` so the list and the redline can
 * never disagree on the baseline. `filePath === null` = nothing selected (no
 * fetch). `oldPath` (worktree-relative) is set only for a rename.
 *
 * SLW-WD-9 (RD-5/AC-11): `worktreeNonce` is ADDED to the fetch key so the OPEN
 * redline re-fetches when the worktree changes — otherwise the pane you are
 * watching stays stale while only the file list refreshes.
 */
export function useSourceFileRedline(
  repo: string | SourceRepo,
  filePath: string | null,
  oldPath: string | undefined,
  baseRef: string | undefined,
  worktreeNonce = 0,
): SourceRedline {
  const resolvedRepo: SourceRepo = typeof repo === "string" ? { kind: "local", cwd: repo } : repo;
  const identityKey =
    resolvedRepo.kind === "local"
      ? resolvedRepo.cwd
      : `${resolvedRepo.machineId}\x1f${resolvedRepo.handle}`;

  const [redline, setRedline] = useState<FileRedline | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Latest redline, readable inside the async catch without re-subscribing.
  // Updated in an effect (not during render) so the render stays pure.
  const redlineRef = useRef<FileRedline | null>(null);
  useEffect(() => {
    redlineRef.current = redline;
  }, [redline]);

  // Render-phase HARD reset only on a FILE switch (or baseRef / worktree switch) —
  // clears the prior file's redline so a genuine change of subject shows a skeleton.
  // A live worktree change (`worktreeNonce`) is a SOFT revalidate (SLW-DV-5 AC-6a):
  // the open redline stays visible while the next one loads, so the pane never
  // blanks (and is never torn down + rebuilt) on an agent edit — AC-6.
  const resetKey = JSON.stringify([identityKey, filePath, oldPath, baseRef]);
  const [seenKey, setSeenKey] = useState(resetKey);
  if (seenKey !== resetKey) {
    setSeenKey(resetKey);
    setRedline(null);
    setError(null);
  }

  useEffect(() => {
    if (!filePath || !identityKey) return;
    let cancelled = false;
    const promise =
      resolvedRepo.kind === "local"
        ? sourceFileRedline(resolvedRepo.cwd, filePath, oldPath, baseRef)
        : sourceRedline(resolvedRepo, filePath, oldPath, baseRef);
    promise
      .then((result) => {
        if (cancelled) return;
        setRedline(result);
        setError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        // Keep the stale redline on a transient revalidation failure; only a
        // never-loaded pane surfaces the error (mirrors the changeset hook).
        if (redlineRef.current == null) setError(describeDiffError(e));
        else
          logger.warn(
            "useSourceFileRedline",
            `stale redline kept; revalidation failed — ${describeDiffError(e)}`,
          );
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identityKey, filePath, oldPath, baseRef, worktreeNonce]);

  return { redline, error };
}

export interface SourceCommitGraph {
  /** Accumulated commits across the pages fetched so far (newest → oldest). */
  entries: SourceCommitGraphEntry[];
  /** First page in flight (cold open) — caller shows a skeleton. */
  loading: boolean;
  /** An older page is being appended (scroll-to-bottom). */
  loadingMore: boolean;
  error: string | null;
  /** Another older page may exist. */
  hasMore: boolean;
  /** Fetch the next older page (AC-7). No-op while loading / exhausted. */
  loadMore: () => void;
}

/**
 * SLW-WD-9 (AC-6/AC-7/AC-8/AC-12) — the all-refs commit graph for a source
 * worktree, through the sanctioned `source-redline-source.ts` seam. Paged: page
 * one on open, older pages appended on `loadMore` (scroll). Cached per `wsId`
 * keyed by the invalidation `nonce` — refocusing a tile whose `(wsId, nonce)` is
 * unchanged serves the cache with NO re-walk; a bumped nonce (an edit / a commit)
 * is a miss → paging restarts from the tip (RD-5). Data only — the RAIL runs the
 * off-thread `useAsyncGraphLayout` over `entries`.
 */
export function useSourceCommitGraph(
  wsId: string,
  worktreePath: string,
  worktreeNonce: number,
  enabled: boolean,
): SourceCommitGraph {
  const [entries, setEntries] = useState<SourceCommitGraphEntry[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // `keyRef` guards `loadMore` against a stale generation writing over a newer
  // one (a page response landing after the nonce/workspace changed). Updated in an
  // effect (never during render), and declared BEFORE the fetch effects so it is
  // current when they run.
  const key = `${wsId}:${worktreeNonce}`;
  const keyRef = useRef(key);
  useEffect(() => {
    keyRef.current = key;
  }, [key]);

  // Render-phase reset on a (wsId, nonce) change: seed from the LRU cache (a hit
  // needs no fetch) or reset to empty and let the page-1 effect fetch.
  const [seenKey, setSeenKey] = useState<string | null>(null);
  if (seenKey !== key) {
    setSeenKey(key);
    const hit = getCommitGraph(wsId, worktreeNonce);
    if (hit) {
      setEntries(hit.entries);
      setCursor(hit.cursor);
      setHasMore(hit.hasMore);
    } else {
      setEntries([]);
      setCursor(null);
      setHasMore(true);
    }
    setLoadingMore(false);
    setError(null);
  }

  // Page 1 — a cache miss for the current (wsId, nonce).
  useEffect(() => {
    if (!enabled || !worktreePath) return;
    if (getCommitGraph(wsId, worktreeNonce)) return; // cache hit → served in render
    const myKey = key;
    let cancelled = false;
    sourceCommitGraph(worktreePath, GRAPH_PAGE_SIZE)
      .then((page) => {
        if (cancelled || keyRef.current !== myKey) return;
        const nextCursor = page.length > 0 ? page[page.length - 1].sha : null;
        const more = page.length === GRAPH_PAGE_SIZE;
        setEntries(page);
        setCursor(nextCursor);
        setHasMore(more);
        setError(null);
        setCommitGraph(wsId, {
          nonce: worktreeNonce,
          entries: page,
          cursor: nextCursor,
          hasMore: more,
        });
      })
      .catch((e: unknown) => {
        if (cancelled || keyRef.current !== myKey) return;
        setError(describeDiffError(e, "changes"));
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, worktreePath, wsId, worktreeNonce, key]);

  // Derived `loading` (a cold first page is in flight) — no setState needed, so
  // no synchronous set-state-in-effect and the enabled false→true transition is
  // handled for free: an empty, error-free, not-yet-exhausted graph that WOULD
  // fetch is "loading".
  const loading = enabled && !!worktreePath && entries.length === 0 && !error && hasMore;

  const loadMore = useCallback(() => {
    if (!enabled || !worktreePath || !hasMore || loading || loadingMore || cursor === null) return;
    const myKey = key;
    setLoadingMore(true);
    sourceCommitGraph(worktreePath, GRAPH_PAGE_SIZE, cursor)
      .then((page) => {
        if (keyRef.current !== myKey) return; // a nonce/workspace change superseded us
        const merged = [...entries, ...page];
        const nextCursor = page.length > 0 ? page[page.length - 1].sha : cursor;
        const more = page.length === GRAPH_PAGE_SIZE;
        setEntries(merged);
        setCursor(nextCursor);
        setHasMore(more);
        setLoadingMore(false);
        setCommitGraph(wsId, {
          nonce: worktreeNonce,
          entries: merged,
          cursor: nextCursor,
          hasMore: more,
        });
      })
      .catch((e: unknown) => {
        if (keyRef.current !== myKey) return;
        setError(describeDiffError(e, "changes"));
        setLoadingMore(false);
      });
  }, [
    enabled,
    worktreePath,
    hasMore,
    loading,
    loadingMore,
    cursor,
    entries,
    wsId,
    worktreeNonce,
    key,
  ]);

  return { entries, loading, loadingMore, error, hasMore, loadMore };
}

export interface SourceCommitChangeset {
  files: PublishFile[] | null;
  loading: boolean;
  error: string | null;
}

/**
 * SLW-WD-9 (AC-2/AC-8) — the flat change-set of ONE commit (its tree vs its
 * first-parent tree), fetched LAZILY only when a commit node is selected
 * (`sha !== null`). Immutable content — no worktree nonce needed (RD-11).
 */
export function useSourceCommitChangeset(
  worktreePath: string,
  sha: string | null,
): SourceCommitChangeset {
  const [files, setFiles] = useState<PublishFile[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchKey = JSON.stringify([worktreePath, sha]);
  const [seenKey, setSeenKey] = useState(fetchKey);
  if (seenKey !== fetchKey) {
    setSeenKey(fetchKey);
    setFiles(null);
    setError(null);
    setLoading(!!sha && !!worktreePath);
  }

  useEffect(() => {
    if (!sha || !worktreePath) return;
    let cancelled = false;
    sourceCommitChangeset(worktreePath, sha)
      .then((result) => {
        if (cancelled) return;
        setFiles(result);
        setError(null);
        setLoading(false);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(describeDiffError(e, "changes"));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [worktreePath, sha]);

  return { files, loading, error };
}

/**
 * SLW-WD-9 (AC-2/AC-8/RD-11) — the before/after redline for one file within a
 * specific commit, fetched LAZILY on sha + file selection. A committed blob is
 * immutable, so this is keyed by `sha` and is independent of paging /
 * virtualization / the worktree nonce (the selected sha keeps rendering even if
 * its row scrolled out of the virtual window). Renders through the SAME
 * `SourceDiffPane` as the working-tree redline (identical `FileRedline` shape).
 */
export function useSourceCommitRedline(
  worktreePath: string,
  sha: string | null,
  filePath: string | null,
  oldPath: string | undefined,
): SourceRedline {
  const [redline, setRedline] = useState<FileRedline | null>(null);
  const [error, setError] = useState<string | null>(null);

  const fetchKey = JSON.stringify([worktreePath, sha, filePath, oldPath]);
  const [seenKey, setSeenKey] = useState(fetchKey);
  if (seenKey !== fetchKey) {
    setSeenKey(fetchKey);
    setRedline(null);
    setError(null);
  }

  useEffect(() => {
    if (!sha || !filePath || !worktreePath) return;
    let cancelled = false;
    sourceCommitFileRedline(worktreePath, sha, filePath, oldPath)
      .then((result) => {
        if (cancelled) return;
        setRedline(result);
        setError(null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(describeDiffError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [worktreePath, sha, filePath, oldPath]);

  return { redline, error };
}
