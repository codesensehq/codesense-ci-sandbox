// SPDX-License-Identifier: MIT
import { useState, useEffect, useCallback } from "react";
import { getHttpApiPort } from "@/tauri/commands";
import {
  buildBaselineViewerUrl,
  buildViewerUrl,
  type CommonViewerProps,
} from "@/shared/lib/viewer-file-type";
import { FileDeletedPlaceholder } from "@/shared/components/file-deleted-placeholder";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useFrameRecord } from "./use-frame-record";
import { useEffectiveRefreshKey } from "@/shared/hooks/use-effective-refresh-key";

// SW-SC-7: image viewer. Mirrors `html-viewer.tsx` — the bytes are served by
// the local static server (`viewer.rs`, correct Content-Type via mime_guess),
// so this is a URL-served `<img>`, never a `readFile`/base64 round-trip. No
// Rust-side viewer changes. `changedPaths` gates hot-reload to the file that
// actually changed (same per-file reload contract the HTML viewer uses).
type ImageViewerProps = CommonViewerProps & {
  changedPaths?: Set<string> | null;
  /**
   * SW-GS-3: when set, render the file's PUBLISH-BASELINE version (the "before"
   * side of the diff toggle) from the git blob instead of the working tree.
   */
  baselineSource?: { orgRoot: string; relPath: string; tree?: string } | null;
};

/**
 * Port resolution state:
 * - `undefined` → still resolving
 * - `null`      → server disabled
 * - `number`    → resolved port
 */
type PortState = number | null | undefined;

export function ImageViewer({
  filePath,
  refreshKey,
  teamContentPath,
  changedPaths,
  baselineSource,
}: ImageViewerProps) {
  const [port, setPort] = useState<PortState>(undefined);
  const [failed, setFailed] = useState(false);
  const frameDocumentPath = baselineSource
    ? `${baselineSource.orgRoot.replace(/\/$/, "")}/${baselineSource.relPath.replace(/^\/+/, "")}`
    : filePath;
  const { frameRecord: frame, frameError } = useFrameRecord(
    port,
    filePath && frameDocumentPath
      ? {
          documentPath: frameDocumentPath,
          contentRoot: teamContentPath,
          slot: baselineSource ? "baseline" : "image",
        }
      : null,
  );

  // SW-SC-7 (AC-7): gate the per-repo `refreshKey` to a per-file key so a
  // watcher tick only re-fetches the image whose file changed — unrelated
  // tabs hold their prior key and don't reload. Mirrors the HTML viewer.
  const effectiveRefreshKey = useEffectiveRefreshKey(refreshKey, changedPaths, filePath);

  // Resolve the HTTP API port once.
  useEffect(() => {
    let cancelled = false;
    getHttpApiPort()
      .then((p) => {
        if (!cancelled) setPort(p);
      })
      .catch(() => {
        if (!cancelled) setPort(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Reset the broken-image state whenever the file or its effective refresh
  // key changes, so a recovered file (or a new tab) gets a fresh load attempt
  // rather than being stuck on a stale placeholder. The flag is driven by the
  // async `<img onError>` callback, so it can't be a pure derived value.
  // SW-GS-3: a STABLE key for the baseline side so toggling Before/After (same
  // filePath + key, different URL) clears a stale broken-image state — keyed on
  // a derived string, NOT the per-render `baselineSource` object, so the reset
  // fires once per real change, not every parent render (mirrors html-viewer's
  // `viewerUrl`-keyed reset).
  const baselineKey = baselineSource ? `${baselineSource.orgRoot}\0${baselineSource.relPath}` : "";
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setFailed(false);
  }, [filePath, effectiveRefreshKey, baselineKey]);

  const handleClose = useCallback(() => {
    // Mirror html-viewer: close via the active workspace's synthetic key.
    const { teamKey, repoPath } = getActiveWorkspaceContext();
    if (repoPath && filePath) {
      useViewerStore.getState().closeTab(teamKey, repoPath, filePath);
    }
  }, [filePath]);

  if (!filePath) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Select a file to view
      </div>
    );
  }

  if (
    port === undefined ||
    (port !== null &&
      !failed &&
      !frameError &&
      (!frame || frame.grant.dashboard !== frameDocumentPath))
  ) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Loading…
      </div>
    );
  }

  if (failed || frameError) {
    // `||` (not `??`) so a trailing-slash path whose `pop()` is "" still
    // falls back to a label rather than rendering empty.
    const fileName = filePath.split("/").pop() || "Image";
    return <FileDeletedPlaceholder fileName={fileName} onClose={handleClose} />;
  }

  const url =
    port == null || !frame
      ? null
      : baselineSource
        ? buildBaselineViewerUrl(frame.origin, baselineSource.orgRoot, baselineSource.relPath, {
            tree: baselineSource.tree,
            v: effectiveRefreshKey,
          })
        : buildViewerUrl(port, teamContentPath, filePath, {
            v: effectiveRefreshKey,
            origin: frame?.origin,
          });
  if (!url) {
    return (
      <div className="flex items-center justify-center h-full text-red-500 text-sm">
        Image server is not available
      </div>
    );
  }

  return (
    // AC-3: centered both axes on the themed viewer backdrop, with padding so
    // the image isn't edge-to-edge. `max-w/max-h-full` + `object-contain`
    // scales large images DOWN to fit (aspect preserved, no overflow); small
    // images keep their natural size (no `w/h-full`, so a 64px icon renders
    // crisp at 64px, never upscaled-blurry).
    <div
      className="flex h-full w-full items-center justify-center overflow-hidden p-4"
      style={{ backgroundColor: "var(--cs-viewer-bg, transparent)" }}
    >
      <img
        src={url}
        alt={filePath.split("/").pop() || "image"}
        onError={() => setFailed(true)}
        className="max-h-full max-w-full object-contain rounded-sm shadow-sm"
      />
    </div>
  );
}
