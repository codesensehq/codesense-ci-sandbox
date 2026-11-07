// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  findScopedSubtree,
  nearestExistingSubtree,
} from "@/shared/lib/pipeline-graph/pipeline-utils";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

/**
 * When the folder the nav is drilled into disappears from the tree (it was
 * deleted on disk or by a sync), drill back out to the
 * nearest folder that still exists instead of stranding the nav, and the
 * pipeline that follows it, on a scope with nothing in it.
 *
 * Acts only on a settled tree that the drill belongs to, so a tree still
 * loading, or one from another team, never resets the drill.
 */
export function useStrandedDrillFallback({
  tree,
  loading,
  scopePath,
  drillPath,
  onDrill,
}: {
  tree: DerivedPipelineTree | null | undefined;
  loading: boolean;
  scopePath: string;
  drillPath: string | undefined;
  onDrill: (absolutePath: string) => void;
}): void {
  useEffect(() => {
    if (!tree || loading || !scopePath || !drillPath) return;
    const root = tree.absoluteDirPath.replace(/\/+$/, "");
    if (!drillPath.startsWith(`${root}/`)) return;
    if (findScopedSubtree(tree, scopePath)) return;
    onDrill(nearestExistingSubtree(tree, scopePath).absoluteDirPath);
  }, [tree, loading, scopePath, drillPath, onDrill]);
}
