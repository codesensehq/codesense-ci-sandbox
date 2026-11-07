// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import type { HistoryBranches, HistoryPage } from "@/tauri/commands";

import { useWindowVisible } from "@/shared/hooks/use-window-focused";

import { readContextHistory } from "./repo-status-source";
import { useScheduledRead } from "./refresh-scheduler";

/** How many commits a page asks for. */
export const HISTORY_PAGE = 60;

const EMPTY: HistoryPage = { commits: [], nextCursor: null, openPrs: [], branches: ["main"] };

export interface ContextHistory {
  page: HistoryPage;
  loading: boolean;
  /** An older page is on its way. */
  loadingMore: boolean;
  error: string | null;
  loadMore: () => void;
}

/**
 * The context repo's commits for the History view: the first page, re-read straight away
 * each time `moved` changes (git moved HEAD, a ref, or fetched), plus older pages on demand.
 * The page on screen stays while it is re-read. Reads only while the window is on screen; a
 * move while hidden is read on coming back. `paths` narrows to commits that touched a file
 * under one of them. Pass `branches` and `paths` memoized: a new object is a new source.
 */
export function useContextHistory(
  orgRoot: string | null,
  branches: HistoryBranches,
  paths: string[] | null,
  moved: unknown,
): ContextHistory {
  const windowVisible = useWindowVisible();
  const [settled, setSettled] = useState<{ root: string; page: HistoryPage } | null>(null);
  const page = settled?.root === orgRoot ? settled.page : EMPTY;
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const branchKey = JSON.stringify(branches);
  const pathKey = paths ? paths.join("\n") : "";
  const sourceKey = orgRoot ? `${orgRoot}|${branchKey}|${pathKey}` : null;
  // Older pages belong to the source they were asked for.
  const sourceRef = useRef(sourceKey);
  useEffect(() => {
    sourceRef.current = sourceKey;
  }, [sourceKey]);

  const doFetch = useCallback(
    (isCurrent: () => boolean) =>
      orgRoot
        ? readContextHistory(orgRoot, branches, paths, HISTORY_PAGE, null)
            .then((p) => {
              if (!isCurrent()) return;
              setSettled({ root: orgRoot, page: p });
              setError(null);
            })
            .catch((e) => {
              if (isCurrent()) setError(String(e));
            })
        : Promise.resolve(),
    [orgRoot, branches, paths],
  );
  // A file write is not a new commit: History re-reads when `moved` does, not on every write.
  const { loading } = useScheduledRead(sourceKey, doFetch, {
    active: windowVisible,
    urgentTrigger: moved,
  });

  const loadMore = useCallback(() => {
    if (!orgRoot || !page.nextCursor || loadingMore) return;
    const asked = sourceKey;
    setLoadingMore(true);
    readContextHistory(orgRoot, branches, paths, HISTORY_PAGE, page.nextCursor)
      .then((next) => {
        if (sourceRef.current !== asked) return;
        setSettled((previous) => ({
          root: orgRoot,
          page: {
            ...next,
            commits: [
              ...(previous?.root === orgRoot ? previous.page.commits : []),
              ...next.commits,
            ],
          },
        }));
      })
      .catch((e) => {
        if (sourceRef.current === asked) setError(String(e));
      })
      .finally(() => setLoadingMore(false));
  }, [orgRoot, page.nextCursor, loadingMore, sourceKey, branches, paths]);

  return { page: orgRoot ? page : EMPTY, loading, loadingMore, error, loadMore };
}
