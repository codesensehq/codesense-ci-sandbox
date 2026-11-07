// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useState } from "react";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import type { ContextWorktreeInfo } from "@/tauri/commands";
import { getPublishSet } from "./publish-set-source";

export function useContextWorktrees(teamKey: string) {
  const rows = useWorktreeRosterStore((s) => s.rows);
  return useMemo(
    () =>
      rows?.filter(
        (r): r is ContextWorktreeInfo =>
          r.kind === "context" && `${r.orgSlug}:${r.team}` === teamKey,
      ) ?? [],
    [rows, teamKey],
  );
}

export function useContextSourceOptions(
  teamKey: string,
  orgRoot: string | null,
  prefix: string | null,
) {
  const worktrees = useContextWorktrees(teamKey);
  const [counts, setCounts] = useState<Record<string, number>>({});
  const refresh = useCallback(() => {
    if (!orgRoot) return;
    void Promise.all(
      worktrees.map(async (row) => {
        const set = await getPublishSet(orgRoot, undefined, undefined, prefix, row.id);
        return [row.id, set.general.count + set.buckets.reduce((n, b) => n + b.count, 0)] as const;
      }),
    )
      .then((entries) => setCounts(Object.fromEntries(entries)))
      .catch(() => {});
  }, [orgRoot, prefix, worktrees]);
  return {
    sources: worktrees.map((w) => ({
      id: w.id,
      name: w.name,
      branch: w.branch,
      count: counts[w.id],
    })),
    refresh,
  };
}
