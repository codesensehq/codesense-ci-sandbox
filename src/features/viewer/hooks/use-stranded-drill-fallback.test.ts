// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { nearestExistingSubtree } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { useStrandedDrillFallback } from "./use-stranded-drill-fallback";

const ROOT = "/org/team/content";

function dir(dirPath: string, subEpics: DerivedPipelineTree[] = []): DerivedPipelineTree {
  return {
    dirPath,
    dirName: dirPath.split("/").pop() ?? "",
    absoluteDirPath: dirPath ? `${ROOT}/${dirPath}` : ROOT,
    stories: [],
    subEpics,
    orderError: null,
    mainDoc: null,
  };
}

const installed = dir("", [dir("dashboards", [dir("dashboards/starter")]), dir("notes")]);
const afterUninstall = dir("", [dir("notes")]);
const starterGoneOnly = dir("", [dir("dashboards"), dir("notes")]);

describe("nearestExistingSubtree", () => {
  it("returns the scope itself, else its nearest surviving ancestor, else the root", () => {
    expect(nearestExistingSubtree(installed, "dashboards/starter").dirPath).toBe(
      "dashboards/starter",
    );
    expect(nearestExistingSubtree(starterGoneOnly, "dashboards/starter").dirPath).toBe(
      "dashboards",
    );
    expect(nearestExistingSubtree(afterUninstall, "dashboards/starter").dirPath).toBe("");
  });
});

describe("useStrandedDrillFallback", () => {
  const drilled = `${ROOT}/dashboards/starter`;

  it("drills back to the nearest existing folder once the drilled folder is removed", () => {
    const onDrill = vi.fn();
    const { rerender } = renderHook(
      (tree: DerivedPipelineTree) =>
        useStrandedDrillFallback({
          tree,
          loading: false,
          scopePath: "dashboards/starter",
          drillPath: drilled,
          onDrill,
        }),
      { initialProps: installed },
    );
    expect(onDrill).not.toHaveBeenCalled();

    rerender(starterGoneOnly);
    expect(onDrill).toHaveBeenLastCalledWith(`${ROOT}/dashboards`);

    rerender(afterUninstall);
    expect(onDrill).toHaveBeenLastCalledWith(ROOT);
  });

  it("leaves the drill alone while the tree loads or when the tree is another team's", () => {
    const onDrill = vi.fn();
    renderHook(() =>
      useStrandedDrillFallback({
        tree: afterUninstall,
        loading: true,
        scopePath: "dashboards/starter",
        drillPath: drilled,
        onDrill,
      }),
    );
    renderHook(() =>
      useStrandedDrillFallback({
        tree: afterUninstall,
        loading: false,
        scopePath: "dashboards/starter",
        drillPath: "/org/other-team/content/dashboards/starter",
        onDrill,
      }),
    );
    expect(onDrill).not.toHaveBeenCalled();
  });
});
