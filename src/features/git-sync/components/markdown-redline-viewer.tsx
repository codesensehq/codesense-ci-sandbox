// SPDX-License-Identifier: MIT
import { privateFile } from "@/shared/lib/private-file";
import { useRef } from "react";

import { useHighlightTheme } from "@/features/dashboard/components/markdown-viewer";
import {
  SCROLL_CONTAINER_STYLE,
  useViewerChrome,
  useViewerScrollKeys,
} from "@/features/dashboard/components/viewer-chrome";

import type { PublishFileStatus } from "../lib/types";
import { useFileRedline } from "../lib/use-file-redline";
import { ChangedBinaryAffordance } from "./changed-binary-affordance";
import { RedlineProse } from "./redline-prose";

/**
 * SW-GS-3 — rendered-markdown redline for one changed file in sync view.
 *
 * Fetches the before/after content (`get_file_redline`, off-thread), block-aligns
 * it into annotated ins/del HAST in the markdown web worker, and renders it via
 * `hast-util-to-jsx-runtime`. Reuses the markdown viewer's CHROME — scroll
 * keyboard shortcuts, content-width / theme / font settings, and the hljs theme
 * — so it behaves identically to the normal viewer (just with redline marks).
 * Against GitHub the redline follows THIS file's writes live (debounced, only while the tab
 * and window are on screen) and never re-reads on other files' writes.
 */
export interface MarkdownRedlineViewerProps {
  /** Context-repo root (`org_root`). */
  orgRoot: string;
  /** Absolute working-tree path of the selected changed file. */
  absPath: string;
  /** Active team + nav repo path (the reload signal is keyed on navRepoPath,
   *  NOT identity repoPath — SW-FX-29). */
  teamKey: string;
  navRepoPath: string;
  /** SW-GS-14: pre-rename org-root-relative path; set only when the active file
   *  is a rename, so "before" is read from the old baseline path (the moved
   *  doc's content lives there) instead of reading as all-added. */
  oldPath?: string;
  /** SW-GS-19: when set, redline the file across THIS PR (merge-base blob vs
   *  PR-head blob) instead of baseline-vs-working-tree. */
  prNumber?: number;
  /** SW-GS-13: when set (and no `prNumber`), redline the file across THIS landed
   *  commit (first-parent blob vs commit blob). */
  commitSha?: string;
  /** Whether this tab is the one on screen (default true). */
  active?: boolean;
}

// The FETCH state; parse → render is delegated to the shared `RedlineProse`.
type FetchState =
  | { phase: "error"; message: string }
  | { phase: "binary"; status: PublishFileStatus }
  | { phase: "text"; before: string; after: string };

export function MarkdownRedlineViewer({
  orgRoot,
  absPath,
  teamKey,
  navRepoPath,
  oldPath,
  prNumber,
  commitSha,
  active,
}: MarkdownRedlineViewerProps) {
  const { relPath, read } = useFileRedline({
    orgRoot,
    absPath,
    navRepoPath,
    oldPath,
    prNumber,
    commitSha,
    active,
  });
  const scrollRef = useRef<HTMLDivElement>(null);
  const proseRef = useRef<HTMLDivElement>(null);

  const { widthClass, hasTheme, fontSize } = useViewerChrome();
  useHighlightTheme();
  useViewerScrollKeys(scrollRef, proseRef);

  const fetched: FetchState | null = !read
    ? null
    : "error" in read
      ? { phase: "error", message: read.error }
      : read.redline.binary
        ? { phase: "binary", status: read.redline.status }
        : { phase: "text", before: read.redline.before, after: read.redline.after };

  if (relPath == null) {
    return (
      <div className="flex h-full items-center justify-center px-8 text-sm text-muted-foreground">
        This file is outside the context repo.
      </div>
    );
  }

  if (fetched?.phase === "binary") {
    return (
      <ChangedBinaryAffordance
        absPath={absPath}
        status={fetched.status}
        teamKey={teamKey}
        navRepoPath={navRepoPath}
      />
    );
  }

  return (
    <div
      ref={scrollRef}
      className={`h-full overflow-y-auto ${hasTheme ? "rounded-none" : ""}`}
      style={SCROLL_CONTAINER_STYLE}
      data-testid="markdown-redline-viewer"
    >
      <div className={`mx-auto px-8 py-6 ${widthClass}`}>
        {privateFile(absPath) && (
          <div className="mb-4 flex flex-wrap justify-between gap-2 text-xs text-muted-foreground">
            <span>
              Private ·{" "}
              {read && "redline" in read
                ? (read.redline.baselineSha ?? "last commit")
                : "last commit"}
            </span>
            <span>Private · {commitSha ? commitSha.slice(0, 7) : "uncommitted"}</span>
          </div>
        )}
        {read && "redline" in read && read.redline.beforeLabel && (
          <div
            className="mb-4 flex flex-wrap justify-between gap-x-4 gap-y-1 text-xs text-muted-foreground"
            data-testid="context-redline-labels"
          >
            <span>{read.redline.beforeLabel}</span>
            <span>{read.redline.afterLabel}</span>
          </div>
        )}
        <RedlineProse
          before={fetched?.phase === "text" ? fetched.before : null}
          after={fetched?.phase === "text" ? fetched.after : null}
          error={fetched?.phase === "error" ? fetched.message : null}
          resetKey={absPath}
          proseRef={proseRef}
          className="md-viewer-prose cs-redline-prose prose prose-sm max-w-none"
          style={{ fontSize }}
        />
      </div>
    </div>
  );
}
