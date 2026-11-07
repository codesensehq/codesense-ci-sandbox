// SPDX-License-Identifier: MIT
import { useState } from "react";

import { cn } from "@/lib/utils";
import { basename } from "@/shared/lib/basename";
import { canMergeMark } from "@/features/dashboard/lib/merge-mark";
import { useMergeMarks } from "@/features/dashboard/lib/use-merge-marks";
import { useReadyResolverContext } from "@/shared/hooks/use-resolver-context";
import { useActiveOrg } from "@/shared/stores/use-active-org";

import { MergePreviewPanel } from "./merge-preview-panel";

/**
 * SW-CP-29: wraps the read-only content view of a merge-marked config file with
 * an "Authored | Merged preview" toggle. Authored (default) shows the fragment
 * as written in team content; Merged preview shows the repo-scoped deep-merge —
 * what the file BECOMES in a chosen repo (see {@link MergePreviewPanel}).
 *
 * Self-hiding: for any file that is not a merge-marked structured config (or when
 * there is no resolved team context / the surface is a sync redline where a
 * merged snapshot is meaningless), it renders `children` verbatim and adds no
 * chrome — so the caller can wrap every read-only tab unconditionally.
 *
 * The toggle only swaps which view mounts; it never triggers a projection wave
 * or a write. Merge marks are loaded once per (orgRoot, team) via the shared
 * {@link useMergeMarks} seam (same source the nav badge reads), so the toggle and
 * the badge can never disagree about whether a file is marked.
 */
type MergeMarkedViewerProps = {
  /** Absolute path of the open file. */
  absPath: string;
  /** The team content root, to derive the content-relative logical path. */
  teamContentPath: string;
  /** False on surfaces where a merged snapshot is meaningless (sync redline). */
  enabled: boolean;
  /** The normal read-only viewer for this file (the "authored" view). */
  children: React.ReactNode;
};

/** Content-relative, forward-slashed logical path — the merge-marks-store key.
 *  Mirrors the inline relativizer in `viewer-file-type.ts`. Returns "" when the
 *  file is NOT under the content root (so a stray absolute path can never be
 *  used as a marks key), which the caller treats as "not applicable". */
function toContentRelative(absPath: string, contentRoot: string): string {
  const root = contentRoot.endsWith("/") ? contentRoot : `${contentRoot}/`;
  if (!absPath.startsWith(root)) return "";
  return absPath.slice(root.length).replace(/\\/g, "/");
}

export function MergeMarkedViewer({
  absPath,
  teamContentPath,
  enabled,
  children,
}: MergeMarkedViewerProps): React.ReactElement {
  const ctx = useReadyResolverContext();
  const org = useActiveOrg();
  const orgRoot = org?.org_root ?? "";
  const { isMarked } = useMergeMarks(orgRoot, ctx?.teamName ?? "");
  const [showMerged, setShowMerged] = useState(false);

  const logicalPath = teamContentPath && enabled ? toContentRelative(absPath, teamContentPath) : "";
  const applicable =
    enabled &&
    ctx != null &&
    logicalPath !== "" &&
    canMergeMark(logicalPath) &&
    isMarked(logicalPath);

  // Not a previewable merge-marked file (or no team context) → pass through with
  // zero added chrome.
  if (!applicable || ctx == null) {
    return <>{children}</>;
  }

  return (
    <div className="flex h-full flex-col">
      <div
        className="flex items-center gap-1 border-b px-2 py-1"
        style={{ borderColor: "var(--border)" }}
        data-testid="merge-marked-view-toggle"
      >
        <ToggleButton active={!showMerged} onClick={() => setShowMerged(false)}>
          Authored
        </ToggleButton>
        <ToggleButton active={showMerged} onClick={() => setShowMerged(true)}>
          Merged preview
        </ToggleButton>
      </div>
      <div className="relative flex-1 overflow-hidden">
        {showMerged ? (
          <MergePreviewPanel ctx={ctx} logicalPath={logicalPath} filename={basename(absPath)} />
        ) : (
          children
        )}
      </div>
    </div>
  );
}

function ToggleButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}): React.ReactElement {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "rounded px-2 py-0.5 text-xs",
        active ? "bg-[var(--accent)] text-[var(--accent-foreground)]" : "text-muted-foreground",
      )}
    >
      {children}
    </button>
  );
}
