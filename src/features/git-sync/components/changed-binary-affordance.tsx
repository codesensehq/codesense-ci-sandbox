// SPDX-License-Identifier: MIT
import { FileWarning } from "lucide-react";

import { basename } from "@/shared/lib/basename";
import { useViewerStore } from "@/shared/stores/viewer-store";

import type { PublishFileStatus } from "../lib/types";
import { STATUS_LABEL } from "./file-status-chip";

/**
 * SW-GS-3 — the "changed — open both / pick-a-side" affordance shown in the
 * sync-view content area for a changed file that has no doc-aware redline:
 * binary files (`.excalidraw`, images) or non-markdown text. Rendering a real
 * redline for these would be empty/broken, so we show what changed and offer to
 * open the current version in the normal viewer instead.
 */
export interface ChangedBinaryAffordanceProps {
  absPath: string;
  status: PublishFileStatus;
  teamKey: string;
  navRepoPath: string;
}

export function ChangedBinaryAffordance({
  absPath,
  status,
  teamKey,
  navRepoPath,
}: ChangedBinaryAffordanceProps) {
  const openCurrent = () => {
    // Show the file itself instead of its changes.
    useViewerStore.getState().openFile(teamKey, navRepoPath, absPath);
    useViewerStore.getState().setTabDiff(teamKey, absPath, false);
  };

  return (
    <div
      className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center"
      data-testid="changed-binary-affordance"
    >
      <FileWarning className="size-8 text-muted-foreground" aria-hidden />
      <div className="space-y-1">
        <p className="text-sm font-medium">
          {STATUS_LABEL[status]} — no markdown redline for this file
        </p>
        <p className="font-mono text-xs text-muted-foreground">{basename(absPath)}</p>
        <p className="max-w-sm text-xs text-muted-foreground">
          Binary and non-markdown changes can’t be shown as a rendered redline. Open the current
          version to review it directly.
        </p>
      </div>
      {status !== "deleted" && (
        <button
          type="button"
          onClick={openCurrent}
          className="rounded border border-border px-3 py-1 text-xs font-medium hover:bg-accent"
        >
          Open current version
        </button>
      )}
    </div>
  );
}
