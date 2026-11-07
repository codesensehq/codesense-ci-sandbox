// SPDX-License-Identifier: MIT
import { useCallback, useState } from "react";

import { useLiveCliSession } from "@/features/viewer/stores/worktree-roster-store";

import { getPublishSet } from "./publish-set-source";
import { useScheduledRead } from "./refresh-scheduler";
import { EMPTY_PUBLISH_SET, type PublishSet } from "./types";

/**
 * While an agent's CLI run is live it may write without pause, and every settled burst would
 * be a full diff against the publish baseline. Then reads are at least this far apart.
 */
export const PUBLISH_SET_LIVE_MIN_INTERVAL_MS = 10_000;

export interface UsePublishSetResult {
  set: PublishSet;
  loading: boolean;
  refresh: () => void;
}

/**
 * The context repo's changed files (working tree vs the publish baseline), scoped to the
 * team's subtree server-side by `pathPrefix`. Read at once for a new root or prefix; after
 * that each change of `trigger` (the repo's file-change counter) re-reads once its burst
 * settles, each change of `urgentTrigger` (git moved: a commit, checkout, staging) re-reads
 * straight away, and only while `active`; while an agent's CLI run is live (`liveCliPath`), at most
 * one read per {@link PUBLISH_SET_LIVE_MIN_INTERVAL_MS}. The previous set stays rendered while
 * a read is in flight, and a failed read keeps it.
 */
export function usePublishSet(
  orgRoot: string | null,
  pathPrefix: string | null,
  options: {
    active: boolean;
    trigger: unknown;
    urgentTrigger?: unknown;
    liveCliPath: string;
    worktree?: string;
  },
): UsePublishSetResult {
  const agentLive = useLiveCliSession(options.liveCliPath);
  const [settled, setSettled] = useState<{ key: string | null; set: PublishSet } | null>(null);
  // Teams in one org share the root, so the prefix is part of the source.
  const sourceKey =
    orgRoot == null ? null : `${orgRoot}|${pathPrefix ?? ""}|${options.worktree ?? ""}`;

  const read = useCallback(
    (isCurrent: () => boolean) =>
      getPublishSet(orgRoot, undefined, undefined, pathPrefix, options.worktree)
        .then((result) => {
          // A missing answer counts as a failed read: keep the prior set.
          if (result && isCurrent()) setSettled({ key: sourceKey, set: result });
        })
        .catch(() => {
          // Keep the prior set on error — don't blank the list.
        }),
    [orgRoot, pathPrefix, sourceKey, options.worktree],
  );

  const { loading, refresh } = useScheduledRead(sourceKey, read, {
    active: options.active,
    trigger: options.trigger,
    urgentTrigger: options.urgentTrigger,
    refreshOnFocus: true,
    minIntervalMs: agentLive ? PUBLISH_SET_LIVE_MIN_INTERVAL_MS : 0,
  });
  return { set: settled?.key === sourceKey ? settled.set : EMPTY_PUBLISH_SET, loading, refresh };
}
