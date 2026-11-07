// SPDX-License-Identifier: MIT
import { Component, lazy, Suspense, useEffect, useRef, useState, type ReactNode } from "react";
import { type CommonViewerProps } from "@/shared/lib/viewer-file-type";
import { useEffectiveRefreshKey } from "@/shared/hooks/use-effective-refresh-key";
import { readFile } from "@/tauri/commands";
import { useReadyResolverContext } from "@/shared/hooks/use-resolver-context";
import { logger } from "@/shared/lib/logger";
import { toast } from "sonner";
import {
  BLANK_SCENE,
  parseExcalidrawScene,
  type ExcalidrawScene,
} from "@/features/dashboard/lib/excalidraw-scene";

// Scene parsing lives in the engine-free `excalidraw-scene.ts` leaf so the
// SC-13 export handler can reuse it without dragging the lazy engine chunk
// into the main bundle (Finding E). Re-exported here for existing importers
// (`excalidraw-canvas.tsx`, viewer tests).
export { parseExcalidrawScene, type ExcalidrawScene };

// The heavy `@excalidraw/excalidraw` bundle (and its CSS) live behind this
// lazy boundary so they are code-split out of the main app bundle and only
// fetched when a user actually opens a `.excalidraw` file (AC-6). The CSS
// import sits inside `excalidraw-canvas.tsx`, so it rides the same chunk.
const ExcalidrawCanvas = lazy(() => import("./excalidraw-canvas"));

/**
 * Guards the lazy canvas so a render-time throw inside Excalidraw (e.g. a
 * structurally-valid-JSON scene with bad element shapes) degrades to a
 * message instead of white-screening the pane (AC-3 "never crash"). Reset
 * per file by keying it on `filePath`.
 */
class CanvasErrorBoundary extends Component<
  { children: ReactNode; onError: () => void },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  componentDidCatch(error: unknown) {
    logger.error("viewer", `excalidraw canvas render error: ${String(error)}`);
    this.props.onError();
  }
  render() {
    if (this.state.failed) {
      return (
        <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
          Couldn't render this canvas.
        </div>
      );
    }
    return this.props.children;
  }
}

type ExcalidrawViewerProps = CommonViewerProps & {
  // SW-SC-3: the per-path changed set forwarded from `renderViewer`, mirroring
  // the html viewer's reload gate. Lets us advance the reload key only when
  // THIS file actually changed on disk.
  changedPaths?: Set<string> | null;
  /**
   * SW-GS-3: when provided, render THIS scene JSON (the publish-baseline
   * "before" side of the diff toggle) instead of reading `filePath` from disk.
   * `.excalidraw` is JSON text, so the baseline is carried in-band rather than
   * via the URL-served baseline route used for binary images.
   */
  sceneText?: string | null;
  /** SW-GS-3: render as a read-only review surface (the sync-view diff) — no
   *  editing, no persistence back to disk. */
  readOnly?: boolean;
};

/**
 * Renders a `.excalidraw` file as an editable Excalidraw canvas. The file is
 * read once per `filePath` for the INITIAL scene and handed to the (lazy)
 * canvas as `initialData`; edits persist back through the canvas's debounced
 * save.
 *
 * SW-SC-3: when this file changes on disk for an external reason, the per-repo
 * `refreshKey` is gated to a per-file key via `useEffectiveRefreshKey` (the
 * same seam `html-viewer.tsx` uses — NOT a canvas-local Tauri listener, which
 * would duplicate App.tsx's central one and leak). On advance we re-read the
 * file and hand the parsed scene to the canvas as `externalScene` + a bumped
 * `reloadNonce`; the canvas decides whether to live-apply or nudge. The
 * canvas is never remounted on reload — a self-save (filePath unchanged, key
 * suppressed) still can't clobber an in-progress drawing.
 */
export function ExcalidrawViewer({
  filePath,
  refreshKey,
  changedPaths,
  sceneText,
  readOnly,
}: ExcalidrawViewerProps) {
  // `null` = still reading the file; a scene object once resolved (blank on
  // empty / missing / malformed).
  const [scene, setScene] = useState<ExcalidrawScene | null>(null);
  // An external on-disk write of this file, parsed; bumped together with
  // `reloadNonce` so the canvas reacts once per genuine change.
  const [externalScene, setExternalScene] = useState<ExcalidrawScene | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);

  const effectiveRefreshKey = useEffectiveRefreshKey(refreshKey, changedPaths, filePath);

  // Active team context for the wireframe library (`null` until org/team/user
  // resolve — the canvas load/persist no-op then). Management is Excalidraw's
  // own native panel; CodeSense just hands the canvas the context to load and
  // persist the team bag (SW-SC-8).
  const libraryCtx = useReadyResolverContext();

  useEffect(() => {
    if (!filePath) return;
    let cancelled = false;
    // Reset to the loading state on every file switch so the previous
    // file's canvas can't flash through before the new scene is read.
    // Synchronous setState in an effect is intentional here — the scene is
    // async (depends on readFile) so it can't be a pure derived value, and
    // we want the reset committed before the read starts (mirrors
    // html-viewer.tsx's fileDeleted reset).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setScene(null);
    // Drop any external scene staged for the previous file.
    setExternalScene(null);

    // SW-GS-3 baseline ("before") side: render the supplied scene JSON instead
    // of reading the working-tree file.
    if (sceneText != null) {
      try {
        setScene(parseExcalidrawScene(sceneText));
      } catch {
        setScene(BLANK_SCENE);
      }
      return () => {
        cancelled = true;
      };
    }

    readFile(filePath)
      .then((text) => {
        if (cancelled) return;
        try {
          setScene(parseExcalidrawScene(text));
        } catch {
          // Malformed JSON → blank canvas + a non-fatal nudge (AC-3); never
          // crash the pane.
          toast.error("Couldn't parse this .excalidraw file — opened a blank canvas.");
          setScene(BLANK_SCENE);
        }
      })
      .catch((e) => {
        if (cancelled) return;
        // A new / not-yet-created `.excalidraw` is valid — open blank. Read
        // failures (ENOENT and otherwise) are a normal new-canvas path.
        logger.debug("viewer", `excalidraw read fell back to blank for ${filePath}: ${e}`);
        setScene(BLANK_SCENE);
      });
    return () => {
      cancelled = true;
    };
  }, [filePath, sceneText]);

  // SW-SC-3 reload path: re-read on an external change to THIS file. Refs gate
  // it so the initial mount and a file switch (the load effect above owns
  // those) never fire a reload — only a genuine `effectiveRefreshKey` advance
  // for the same file does.
  const reloadFileRef = useRef(filePath);
  const reloadKeyRef = useRef(effectiveRefreshKey);
  useEffect(() => {
    // SW-GS-3: a baseline ("before") render is static — never re-read on disk.
    if (sceneText != null) return;
    if (filePath !== reloadFileRef.current) {
      // Fresh file — the load effect owns it; just resync the gate.
      reloadFileRef.current = filePath;
      reloadKeyRef.current = effectiveRefreshKey;
      return;
    }
    if (effectiveRefreshKey === reloadKeyRef.current) return;
    reloadKeyRef.current = effectiveRefreshKey;
    if (!filePath) return;
    let cancelled = false;
    readFile(filePath)
      .then((text) => {
        if (cancelled) return;
        let parsed: ExcalidrawScene;
        try {
          parsed = parseExcalidrawScene(text);
        } catch {
          // SW-SC-4 (AC-7): a malformed external write must NOT clobber the open
          // canvas — keep the current scene. An agent writing via plain file
          // tools may not write atomically, so a watcher tick can land mid-write
          // and read a truncated file; the settle event re-reads the complete
          // file and applies it. Because this transient parse failure self-heals
          // (and never corrupts the canvas), it's logged, not toasted — surfacing
          // it would flash an error on every partial read of a perfectly good
          // write. A genuinely corrupt write simply doesn't reload; the user
          // re-triggers. (Initial open still toasts: see the load effect above.)
          logger.debug("viewer", `excalidraw reload skipped — unparsed write for ${filePath}`);
          return;
        }
        setExternalScene(parsed);
        setReloadNonce((n) => n + 1);
      })
      .catch((e) => {
        // Deleted / unreadable on reload is not a reload — leave the canvas as-is.
        if (cancelled) return;
        logger.debug("viewer", `excalidraw reload read skipped for ${filePath}: ${e}`);
      });
    return () => {
      cancelled = true;
    };
  }, [effectiveRefreshKey, filePath, sceneText]);

  if (!filePath) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Select a file to view
      </div>
    );
  }

  const loading = (
    <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
      Loading canvas…
    </div>
  );

  if (scene === null) return loading;

  return (
    // key on filePath: Excalidraw reads `initialData` once at mount, so a
    // file switch must remount with the new scene; the boundary also resets
    // per file. The same keying means a self-save (filePath unchanged) never
    // remounts — no mid-draw clobber (AC-4).
    <CanvasErrorBoundary
      key={filePath}
      onError={() => toast.error("Couldn't render this .excalidraw canvas.")}
    >
      {/* Relative wrapper preserves the canvas fill layout. Attach moved to
          the canvas file's right-click menu (SW-SC-12); library management is
          native to the canvas (SW-SC-8). */}
      <div className="relative h-full w-full">
        <Suspense fallback={loading}>
          <ExcalidrawCanvas
            filePath={filePath}
            scene={scene}
            externalScene={externalScene}
            reloadNonce={reloadNonce}
            // SW-GS-3: a read-only review surface persists NOTHING — withhold the
            // library context so the canvas no-ops library load/inject/persist
            // (it would otherwise write the shared team .excalidrawlib).
            libraryCtx={readOnly ? null : libraryCtx}
            readOnly={readOnly}
          />
        </Suspense>
      </div>
    </CanvasErrorBoundary>
  );
}
