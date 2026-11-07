// SPDX-License-Identifier: MIT
import type { OpenDiffParams, OpenDiffResult } from "./open-diff-types";
import { readOpenDiff } from "./source-redline-source";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { OpenDiffTab } from "../components/open-diff-tab";
import { DIFF_TAB_PREFIX, diffTabAddress, openDiffTab } from "./open-diff-route";

export function registerOpenDiffTab(): void {
  registerVirtualTab(
    DIFF_TAB_PREFIX,
    (path) => {
      const address = diffTabAddress(path);
      return address ? address.params.path.split("/").pop() + " · Diff" : "Diff";
    },
    OpenDiffTab,
  );
}
export async function openDiff(
  params: OpenDiffParams,
  origin?: { frameId?: string; page?: string; team?: { org: string; team: string } },
): Promise<OpenDiffResult> {
  const result = await readOpenDiff(params, origin?.frameId, origin?.page);
  if (result.status !== "opened") return result;
  registerOpenDiffTab();
  openDiffTab(params, origin?.team);
  return { status: "opened" };
}
