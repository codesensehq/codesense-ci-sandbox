// SPDX-License-Identifier: MIT
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useState } from "react";

import { ExcalidrawViewer } from "@/features/dashboard/components/excalidraw-viewer";
import { HtmlViewer } from "@/features/dashboard/components/html-viewer";
import { ImageViewer } from "@/features/dashboard/components/image-viewer";
import { cn } from "@/lib/utils";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { getViewerFileType } from "@/shared/lib/viewer-file-type";

import { useFileRedline } from "../lib/use-file-redline";
import { STATUS_LABEL } from "./file-status-chip";

/**
 * SW-GS-3 — before/after diff for a changed NON-markdown file (excalidraw,
 * image, HTML) in sync view. These have no doc-aware redline, so we show the
 * actual file with a Before | After toggle (a toggle, not side-by-side, since
 * wide/tall files don't fit two-up). The "before" side is the publish baseline:
 * for images/HTML it's served from the git blob (`baselineSource`); for the
 * JSON-text excalidraw it's carried in-band as `sceneText`. Added files have
 * only an "after"; deleted files only a "before".
 */
export interface BeforeAfterDiffToggleProps {
  orgRoot: string;
  absPath: string;
  teamContentPath: string;
  /** Nav repo path — the reload signal is keyed on it, NOT identity repoPath
   *  (SW-FX-29). */
  navRepoPath: string;
  /** SW-GS-14: pre-rename org-root-relative path (a renamed doc's "before" reads
   *  from the old path), mirroring the markdown redline. */
  oldPath?: string;
  /** SW-GS-19: when set, the before/after text comes from the PR (merge-base vs
   *  PR-head). NOTE v1 limitation: the image/HTML "before" blob is still served
   *  from the working-tree baseline route (not PR-aware) — the doc-aware markdown
   *  redline is the PR-review target; binary before-images in a PR are a
   *  build-on-later. */
  prNumber?: number;
  /** SW-GS-13: when set (and no `prNumber`), the before/after text comes from the
   *  landed commit (first-parent vs commit). Same v1 binary limitation as the PR
   *  source above (image/HTML "before" still served from the working-tree route). */
  commitSha?: string;
  /** Whether this tab is the one on screen (default true). */
  active?: boolean;
}

type Side = "before" | "after";

export function BeforeAfterDiffToggle({
  orgRoot,
  absPath,
  teamContentPath,
  navRepoPath,
  oldPath,
  prNumber,
  commitSha,
  active,
}: BeforeAfterDiffToggleProps) {
  // The child viewers reload their own file on its writes; this reads the diff itself.
  const refreshKey = useFileRefreshStore((s) => s.fileRefreshKeys[navRepoPath] ?? 0);
  const changedPaths = useFileRefreshStore(
    (s): Set<string> | null => s.lastChangedContextFile[navRepoPath] ?? null,
  );
  const contextWorktree = useWorktreeRosterStore((s) =>
    s.rows?.find((row) => row.kind === "context" && absPath.startsWith(`${row.path}/`)),
  );
  const { relPath, read } = useFileRedline({
    orgRoot,
    absPath,
    navRepoPath,
    oldPath,
    prNumber,
    commitSha,
    active,
  });
  const redline = read && "redline" in read ? read.redline : null;
  const [side, setSide] = useState<Side>("after");

  if (relPath == null) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-sm text-muted-foreground">
        This file is outside the context repo.
      </div>
    );
  }
  if (read && "error" in read) {
    return (
      <div
        className="flex h-full items-center justify-center px-8 text-sm text-muted-foreground"
        role="alert"
      >
        Couldn't read the changes: {read.error}
      </div>
    );
  }
  if (!redline) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Loading…
      </div>
    );
  }

  const hasBefore = redline.status !== "added";
  const hasAfter = redline.status !== "deleted";
  const effectiveSide: Side = !hasAfter ? "before" : !hasBefore ? "after" : side;
  const fileType = getViewerFileType(absPath);
  const baselineSource = {
    orgRoot: contextWorktree?.path ?? orgRoot,
    relPath,
    tree: redline.baselineSha,
  };

  let body: React.ReactNode;
  if (fileType === "excalidraw") {
    // Read-only on BOTH sides: the sync diff is a review surface, and editing
    // would write to the working tree (the baseline side would clobber it).
    body =
      effectiveSide === "before" ? (
        <ExcalidrawViewer
          filePath={absPath}
          teamContentPath={teamContentPath}
          sceneText={redline.before}
          changedPaths={changedPaths}
          readOnly
        />
      ) : (
        <ExcalidrawViewer
          filePath={absPath}
          teamContentPath={teamContentPath}
          refreshKey={refreshKey}
          changedPaths={changedPaths}
          readOnly
        />
      );
  } else if (fileType === "image") {
    body = (
      <ImageViewer
        filePath={absPath}
        teamContentPath={teamContentPath}
        refreshKey={refreshKey}
        changedPaths={changedPaths}
        baselineSource={effectiveSide === "before" ? baselineSource : null}
      />
    );
  } else {
    body = (
      <HtmlViewer
        filePath={absPath}
        teamContentPath={teamContentPath}
        refreshKey={refreshKey}
        changedPaths={changedPaths}
        baselineSource={effectiveSide === "before" ? baselineSource : null}
      />
    );
  }

  return (
    <div className="flex h-full flex-col" data-testid="before-after-diff-toggle">
      <div className="flex items-center gap-2 border-b px-3 py-1.5 text-xs">
        <span className="font-medium">{STATUS_LABEL[redline.status]}</span>
        <span className="text-muted-foreground">
          {hasBefore && hasAfter
            ? `showing ${effectiveSide}`
            : hasAfter
              ? "current version"
              : "baseline version"}
        </span>
        {hasBefore && hasAfter && (
          <div className="ml-auto inline-flex overflow-hidden rounded border border-border">
            {(["before", "after"] as const).map((s) => (
              <button
                key={s}
                type="button"
                onClick={() => setSide(s)}
                className={cn(
                  "px-2 py-0.5 capitalize",
                  effectiveSide === s ? "bg-accent font-medium" : "hover:bg-accent/60",
                )}
              >
                {s === "before" ? (redline.beforeLabel ?? s) : (redline.afterLabel ?? s)}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="min-h-0 flex-1">{body}</div>
    </div>
  );
}
