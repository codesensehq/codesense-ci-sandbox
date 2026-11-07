// SPDX-License-Identifier: MIT
import { dashboardPublishingFallback } from "@/features/dashboard-publishing/dashboards-virtual-tab";
import type { ReactElement } from "react";

import { HtmlViewer } from "@/features/dashboard/components/html-viewer";
import { ExcalidrawViewer } from "@/features/dashboard/components/excalidraw-viewer";
import { ImageViewer } from "@/features/dashboard/components/image-viewer";
import { CodeFileViewer } from "@/features/viewer/components/code-file-viewer";
import {
  MarkdownViewer,
  type MarkdownExtras,
} from "@/features/dashboard/components/markdown-viewer";
import { MarkdownRedlineViewer } from "@/features/git-sync/components/markdown-redline-viewer";
import { BeforeAfterDiffToggle } from "@/features/git-sync/components/before-after-diff-toggle";
import { getViewerFileType, type CommonViewerProps } from "@/shared/lib/viewer-file-type";
import { resolveVirtualTab } from "@/shared/lib/virtual-viewer-registry";

/**
 * Extension-based viewer dispatch — the single way content is rendered the
 * way the CodeSense viewer renders it (markdown→rendered, images shown,
 * PDF→HtmlViewer iframe, excalidraw→canvas).
 *
 * Extracted here (SW-GS-1) from `team-workspace-view.tsx` so V4's conflict
 * preview (AC-8/9) composes the SAME dispatch instead of forking it — the
 * function was already pure (closes over no component state), so this is a
 * DRY move, not a behavior change. The team-workspace view imports it back.
 *
 * `common` carries the shared `CommonViewerProps` contract (file path,
 * refresh key, team content root, nav callback). `md` carries markdown-only
 * extras (post-commit signal, anchor scroll, watcher invalidation) — formats
 * with their own extras should mirror this split. The `never` exhaustiveness
 * guard fails the TS build if a `VIEWER_EXTENSIONS` type is added without a
 * case here.
 */
export function renderViewer(
  common: CommonViewerProps & { filePath: string },
  md: MarkdownExtras,
): ReactElement {
  // SW-GS-2b (C): synthetic "virtual" tabs (e.g. the Git Sync Workbench) render
  // a registered React surface instead of disk content. Short-circuit BEFORE
  // extension dispatch — a sentinel path has no extension, so `getViewerFileType`
  // would default it to "markdown" and try to `readFile` a non-existent file.
  const VirtualTab =
    resolveVirtualTab(common.filePath) ?? dashboardPublishingFallback(common.filePath);
  if (VirtualTab) return <VirtualTab {...common} />;

  const fileType = getViewerFileType(common.filePath);
  // SW-GS-3: in sync view a changed NON-markdown file (image/html/excalidraw)
  // shows the actual file with a Before | After toggle instead of a redline.
  const redlineDiff = md.redline ? (
    <BeforeAfterDiffToggle
      orgRoot={md.redline.orgRoot}
      absPath={common.filePath}
      teamContentPath={common.teamContentPath}
      navRepoPath={md.redline.navRepoPath}
      oldPath={md.redline.oldPath}
      prNumber={md.redline.prNumber}
      commitSha={md.redline.commitSha}
      active={md.redline.active}
    />
  ) : null;
  switch (fileType) {
    case "html":
      if (redlineDiff) return redlineDiff;
      // SW-FX-5: HTML viewer reuses markdown's `changedPaths` reload gate so
      // a per-repo refreshKey bump only reloads the iframe whose file changed.
      return (
        <HtmlViewer
          {...common}
          changedPaths={md.changedPaths}
          viewStateKey={md.viewStateKey}
          pendingHash={md.pendingHash}
          onPendingHashConsumed={md.onPendingHashConsumed}
        />
      );
    case "markdown":
      // SW-GS-3: in sync view a changed markdown file renders as a redline
      // (working-tree vs the publish baseline) instead of plain content.
      if (md.redline) {
        return (
          <MarkdownRedlineViewer
            orgRoot={md.redline.orgRoot}
            absPath={common.filePath}
            teamKey={md.redline.teamKey}
            navRepoPath={md.redline.navRepoPath}
            oldPath={md.redline.oldPath}
            prNumber={md.redline.prNumber}
            commitSha={md.redline.commitSha}
            active={md.redline.active}
          />
        );
      }
      return <MarkdownViewer {...common} {...md} />;
    case "excalidraw":
      if (redlineDiff) return redlineDiff;
      // SW-SC-3: the canvas hot-reloads on an external write to its own file.
      // It reuses the same per-file `changedPaths` gate as html/markdown so a
      // per-repo refreshKey bump only reloads the canvas whose file changed.
      return <ExcalidrawViewer {...common} changedPaths={md.changedPaths} />;
    case "image":
      if (redlineDiff) return redlineDiff;
      // SW-SC-7: URL-served `<img>`. Reuses the same per-file `changedPaths`
      // reload gate as html/excalidraw so a watcher tick reloads only the
      // changed image's tab.
      return <ImageViewer {...common} changedPaths={md.changedPaths} />;
    case "code":
      // SW-VE-2: in sync/redline view a changed code file keeps the EXACT
      // pre-VE-2 behavior — before VE-2 these extensions defaulted to "markdown"
      // and rendered a MarkdownRedlineViewer line-diff, so routing the redline
      // branch there is byte-identical (no regression, and it preserves the
      // line-change info a PR review needs). It deliberately does NOT use the
      // `BeforeAfterDiffToggle`: that path URL-serves the file through HtmlViewer,
      // and the HTTP MIME for `.ts`/`.sh`/`.env` (video/x-sh/octet-stream) would
      // render a blank iframe. A syntax-highlighted code LINE-diff is VE-4
      // (concurrent-write) territory. Outside sync view (`md.redline` unset —
      // normal viewing) it renders the new
      // read-only, syntax-highlighted `CodeFileViewer`.
      if (md.redline) {
        return (
          <MarkdownRedlineViewer
            orgRoot={md.redline.orgRoot}
            absPath={common.filePath}
            teamKey={md.redline.teamKey}
            navRepoPath={md.redline.navRepoPath}
            oldPath={md.redline.oldPath}
            prNumber={md.redline.prNumber}
            commitSha={md.redline.commitSha}
            active={md.redline.active}
          />
        );
      }
      return <CodeFileViewer {...common} changedPaths={md.changedPaths} />;
    default: {
      const _exhaustive: never = fileType;
      void _exhaustive;
      return <MarkdownViewer {...common} {...md} />;
    }
  }
}
