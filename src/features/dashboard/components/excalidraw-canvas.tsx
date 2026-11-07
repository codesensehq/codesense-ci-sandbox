// SPDX-License-Identifier: MIT
import "@excalidraw/excalidraw/index.css";

import { useCallback, useEffect, useMemo, useRef, useState, type ComponentProps } from "react";
import {
  Excalidraw,
  serializeAsJSON,
  hashElementsVersion,
  CaptureUpdateAction,
  exportToBlob,
} from "@excalidraw/excalidraw";
import { useTheme } from "next-themes";
import { toast } from "sonner";

import {
  readCanvasLibrary,
  writeCanvasLibrary,
  writeBinaryContentFile,
  writeContentFile,
} from "@/tauri/commands";
import { createDebouncedAction } from "@/shared/lib/debounced-action";
import { blobToBase64 } from "@/shared/lib/blob-to-base64";
import { canvasPngPath } from "@/shared/lib/viewer-file-type";
import type { ResolverContext } from "@/shared/hooks/use-resolver-context";
import { logger } from "@/shared/lib/logger";
import { libraryEnvelope, packBlob } from "@/features/dashboard/lib/canvas-library-merge";
import type { ExcalidrawScene } from "./excalidraw-viewer";
import { sceneForLoad } from "@/features/dashboard/lib/excalidraw-scene";
import { decideCanvasReload } from "./excalidraw-reload-policy";
import { EXCALIDRAW_CANVAS_ATTR } from "@/shared/hooks/use-gesture-pane-scope";

// Matches the scratch-note save cadence (viewer-store.ts). Excalidraw's
// `onChange` is chatty (fires on selection / scroll / every pointer move),
// so the write is debounced and gated on a content signature below.
const SAVE_DEBOUNCE_MS = 1000;
// Library persistence cadence — Excalidraw's `onLibraryChange` is chatty
// during injection and drag, so coalesce before writing the whole bag.
const LIBRARY_PERSIST_DEBOUNCE_MS = 800;

type ExcalidrawProps = ComponentProps<typeof Excalidraw>;
type OnChange = NonNullable<ExcalidrawProps["onChange"]>;
type OnLibraryChange = NonNullable<ExcalidrawProps["onLibraryChange"]>;
type SceneElements = Parameters<typeof hashElementsVersion>[0];
// The imperative API, derived from the prop so no named type-import is needed.
type ExcalidrawAPI = Parameters<NonNullable<ExcalidrawProps["excalidrawAPI"]>>[0];

// The appState keys `serializeAsJSON(…, "local")` actually persists (its
// export cleaner keeps exactly these four). This is the single source of truth
// for the persisted appState set: `sceneSignature` hashes exactly these keys,
// and an external reload applies ONLY these via `updateScene` — never
// scroll/zoom/selection — so the agent's shapes appear in place while the
// user's viewport is preserved (AC-2). Add a key here and both follow — but
// only primitive-valued keys: `sceneSignature` stringifies each, so an
// object/array value would collapse to "[object Object]" and break change
// detection (the four below are a string/bool/number).
const PERSISTED_APPSTATE_KEYS = [
  "viewBackgroundColor",
  "gridModeEnabled",
  "gridSize",
  "gridStep",
] as const;

/**
 * A cheap signature of everything `serializeAsJSON(…)` actually writes to a
 * `.excalidraw` file, used both to decide whether an `onChange` is a real edit
 * worth persisting and to detect divergence between the live canvas and disk
 * (SW-SC-3): the element hash, the persisted appState keys, and the embedded-
 * file count. Transient view state (scroll/zoom/selection, tool defaults,
 * canvas name) is `export:false` and never written, so it is excluded —
 * panning never moves the signature.
 */
function sceneSignature(
  elements: readonly unknown[],
  appState: unknown,
  files: object | null | undefined,
): string {
  const a = appState as Record<string, unknown>;
  return [
    hashElementsVersion(elements as SceneElements),
    ...PERSISTED_APPSTATE_KEYS.map((k) => String(a?.[k] ?? "")),
    Object.keys(files ?? {}).length,
  ].join("|");
}

function persistedAppState(appState: unknown): Record<string, unknown> {
  const a = appState as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const k of PERSISTED_APPSTATE_KEYS) {
    if (a?.[k] !== undefined) out[k] = a[k];
  }
  return out;
}

type Props = {
  filePath: string;
  scene: ExcalidrawScene;
  /**
   * SW-GS-3: read-only review surface (the sync-view diff). Renders the canvas
   * in Excalidraw view mode AND disables the persist path, so a stray edit can
   * never write back to disk — critical for the "before" (baseline) side, where
   * a save would clobber the working file with baseline content.
   */
  readOnly?: boolean;
  // SW-SC-3: an external on-disk write of this file, parsed by the wrapper.
  // `reloadNonce` increments on each genuine external change so the canvas
  // reacts even when two writes parse to the same `externalScene` object.
  externalScene?: ExcalidrawScene | null;
  reloadNonce?: number;
  /** Active team context for the wireframe library (SW-SC-5/SW-SC-8).
   *  `null`/absent until an org/team/user is selected — load/persist no-op
   *  then (D5). */
  libraryCtx?: ResolverContext | null;
};

/** Excalidraw's `validateEmbeddable`: no link may be embedded, so a canvas loads no outside frame. */
function rejectEmbeddable(_link: string): boolean {
  return false;
}

/** Excalidraw's library menu offers "Publish selected", which posts the items and the author's
 *  details to Excalidraw's cloud function. It has no option to turn that off, so the canvas hides
 *  the menu item; its `data-testid` is the one Excalidraw gives it (not "remove", whatever it
 *  says). `excalidraw-canvas.test.tsx` checks the installed package still uses it. */
const LIBRARY_PUBLISH_ITEM_SELECTOR = '.library-menu [data-testid="lib-dropdown--remove"]';

/**
 * The embedded Excalidraw canvas (lazy-loaded by `excalidraw-viewer.tsx`).
 * Mounts from `scene`, follows the app's light/dark theme, persists edits back
 * to `filePath` as valid Excalidraw scene JSON, hot-reloads in place when the
 * file changes on disk for an external reason (SW-SC-3), and provisions
 * wireframe libraries into the panel (SW-SC-5).
 *
 * This module owns every `@excalidraw/excalidraw` import (component, CSS,
 * serialize/version helpers, imperative API) so they stay inside the
 * code-split chunk.
 */
export default function ExcalidrawCanvas({
  filePath,
  scene: fileScene,
  externalScene: fileExternalScene,
  reloadNonce,
  libraryCtx = null,
  readOnly = false,
}: Props) {
  const { resolvedTheme } = useTheme();

  // Both load seams (mount via `initialData`, reload via `updateScene`/`addFiles`)
  // take the scene through `sceneForLoad`, so a canvas loads nothing from the
  // network. Signatures are taken after it too, so a dropped part is no change.
  const scene = useMemo(() => sceneForLoad(fileScene), [fileScene]);
  const externalScene = useMemo(
    () => (fileExternalScene ? sceneForLoad(fileExternalScene) : fileExternalScene),
    [fileExternalScene],
  );

  // The imperative handle — captured once on mount via the `excalidrawAPI`
  // callback. Used to apply external reloads in place (no remount), preserving
  // the user's pan/zoom (AC-2), and to inject wireframe libraries (SW-SC-5).
  const apiRef = useRef<ExcalidrawAPI | null>(null);
  // Flips true once the API is captured, so the library-injection effect
  // re-runs the moment it's ready (the reload paths read `apiRef` directly).
  const [apiReady, setApiReady] = useState(false);

  // Two reads of one signature definition (DRY): what we believe is currently
  // on disk vs. what's live on the canvas. `diskSig` advances only on a
  // SUCCESSFUL save or an applied reload; `liveSig` tracks every edit. The
  // canvas is "dirty" (has unsaved local edits) when they differ. Both seed
  // from the same first-render value (one ref must not read another during
  // render). `scene` is stable for the component's lifetime (the parent keys
  // on filePath), so the seed is memoized — not recomputed on every re-render
  // (theme toggle, nudge open/close, reload).
  const initialSig = useMemo(
    () => sceneSignature(scene.elements, scene.appState, scene.files),
    [scene],
  );
  const diskSigRef = useRef<string>(initialSig);
  const liveSigRef = useRef<string>(initialSig);

  // One debounced writer per mounted file (the parent keys this component on
  // filePath, so `filePath` is stable for the component's lifetime). Saving is
  // fire-and-forget — drawing is never blocked on disk I/O (AC-6). `diskSig`
  // advances only here, on a confirmed write, so a failed save leaves the
  // canvas dirty and a later edit retries. A failed write surfaces a toast
  // keyed by a stable id (sonner de-dupes) instead of spamming every second.
  // The diskSig read/write below runs inside the debounce timer, never during
  // render.
  const save = useMemo(
    () =>
      createDebouncedAction(
        async (...args: unknown[]) => {
          const writtenSig = args[1] as string;
          try {
            await writeContentFile(filePath, args[0] as string);
            // Advance the disk baseline only if the canvas still holds exactly
            // what we just wrote. If a reload (applyExternal) landed while this
            // write was in flight — cancel() can't stop an already-fired write
            // — `liveSig` has moved on, so skipping here prevents a stale write
            // from corrupting dirty/echo tracking for the rest of the session.
            if (liveSigRef.current === writtenSig) diskSigRef.current = writtenSig;
          } catch (e) {
            toast.error("Couldn't save canvas changes to disk.", {
              id: `excalidraw-save-error:${filePath}`,
            });
            throw e; // let createDebouncedAction log it
          }
        },
        SAVE_DEBOUNCE_MS,
        "excalidrawSave",
      ),
    [filePath],
  );

  // SW-SC-2 (AC-3/AC-4/AC-6): regenerate the PNG render alongside the JSON
  // save so the agent's read path always has a current picture (for freehand /
  // handwriting the scene JSON can't convey). The scene snapshot arrives as the
  // trigger arg (latest-wins, like the JSON save). Fire-and-forget and
  // debounced — drawing is never blocked on the export or the binary write —
  // and the write is watcher-suppressed (writeBinaryContentFile) so it adds no
  // backend churn. A render/write failure is non-fatal (logged, no toast): the
  // PNG is a derived artifact and the JSON save is the source of truth.
  const exportPng = useMemo(
    () =>
      createDebouncedAction(
        async (...args: unknown[]) => {
          const s = args[0] as Parameters<typeof exportToBlob>[0] | undefined;
          if (!s) return;
          try {
            const blob = await exportToBlob({ ...s, mimeType: "image/png" });
            const base64 = await blobToBase64(blob);
            await writeBinaryContentFile(canvasPngPath(filePath), base64);
          } catch (e) {
            logger.error("viewer", `excalidraw PNG render failed for ${filePath}: ${e}`);
          }
        },
        SAVE_DEBOUNCE_MS,
        "excalidrawPng",
      ),
    [filePath],
  );

  // SW-SC-8 library state. True while the mount injection is mutating the
  // panel. `updateLibrary` fires `onLibraryChange`, so without this gate the
  // injection's own event would write the just-read bag straight back — a
  // wasted disk round-trip, and a slow read that resolved after a user edit
  // would `merge:false`-wipe it and then persist the loss. Only user-initiated
  // changes after injection settles are persisted.
  const injectingRef = useRef(false);
  // Serializes injections so two rapid re-runs (e.g. a team switch) can't
  // interleave their `updateLibrary` calls against the one imperative API.
  const injectChainRef = useRef<Promise<void>>(Promise.resolve());
  // Serialized form of the bag we last persisted. Excalidraw can emit
  // `onLibraryChange` more than once for a single change, so a fire that
  // serializes to the same bag we just wrote is a redundant no-op and is
  // skipped — independent of `injectingRef`'s timing. Mirrors the scene save's
  // diskSig/liveSig gate.
  const lastBagSigRef = useRef<string | null>(null);

  // Persist the WHOLE library bag to the one team file (SW-SC-8 D3). There's a
  // single tier now, so no provisioned/personal filtering — the native panel's
  // exact contents are written, which makes a per-item delete stick. The
  // pre-serialized envelope arrives as the trigger arg (also the signature, so
  // it's never serialized twice). Debounced + fire-and-forget so the library
  // UI is never blocked on disk (AC-6).
  const persistTeam = useMemo(
    () =>
      createDebouncedAction(
        async (...args: unknown[]) => {
          if (!libraryCtx) return;
          const content = args[0] as string;
          await writeCanvasLibrary({ ...libraryCtx, content });
        },
        LIBRARY_PERSIST_DEBOUNCE_MS,
        "excalidrawTeamLib",
      ),
    [libraryCtx],
  );

  const initialData = useMemo<ExcalidrawProps["initialData"]>(
    () =>
      ({
        elements: scene.elements,
        files: scene.files,
        // `collaborators` is a Map at runtime; a JSON-loaded value would be
        // the wrong shape. Drop it on load — `serializeAsJSON(…, "local")`
        // also strips transient state on the way out.
        appState: { ...scene.appState, collaborators: undefined },
      }) as ExcalidrawProps["initialData"],
    [scene],
  );

  // Flush any pending write when the canvas unmounts (file switch / tab
  // close / app quit) so an edit made inside the debounce window isn't lost.
  // Flush the PNG and the library write too. Also runs when `persistTeam`
  // changes (a team switch), flushing the old team's pending bag before the
  // new context takes over.
  useEffect(
    () => () => {
      void save.flush();
      void exportPng.flush();
      void persistTeam.flush();
    },
    [save, exportPng, persistTeam],
  );

  const handleChange = useCallback<OnChange>(
    (elements, appState, files) => {
      // SW-GS-3: never persist from a read-only review surface (the sync diff).
      // viewModeEnabled blocks UI edits, but onChange still fires for view-state
      // changes (pan/zoom) — bail before the writer so nothing reaches disk.
      if (readOnly) return;
      const sig = sceneSignature(elements, appState, files);
      const prevSig = liveSigRef.current;
      // Gate on the PREVIOUS live signature, not on disk: a scene that returns
      // to the on-disk content (an undo/revert) while a different write is
      // still queued must re-trigger so it supersedes that stale pending write
      // — otherwise the abandoned edit lands and the revert is lost. Comparing
      // to the prior signature still skips selection/scroll/hover noise (same
      // content twice) and absorbs the echo `onChange` that `updateScene`
      // fires after a reload (applyExternal pre-sets liveSig to the applied
      // signature, so the echo equals prevSig).
      if (sig === prevSig) return;
      let json: string;
      try {
        json = serializeAsJSON(elements, appState, files, "local");
      } catch (e) {
        // Leave liveSig at prevSig so an identical later onChange re-attempts
        // the write (a serialize failure is usually transient); only advance
        // it once we have a serializable scene to schedule.
        logger.error("viewer", `excalidraw serialize failed for ${filePath}: ${e}`);
        return;
      }
      liveSigRef.current = sig;
      save.trigger(json, sig);
      // SW-SC-2: same signature gate drives the PNG — schedule a render of the
      // current scene (exportToBlob wants a non-null files map).
      exportPng.trigger({ elements, appState, files: files ?? {} });
    },
    [save, exportPng, filePath, readOnly],
  );

  const handleLibraryChange = useCallback<OnLibraryChange>(
    (items) => {
      // Ignore the mount injection's own synchronous `onLibraryChange` event
      // (fast path). Only user-initiated changes are persisted.
      if (injectingRef.current) return;
      // Content gate: a fire whose serialized bag equals the one we last
      // persisted is a redundant no-op and is skipped, so identical content is
      // never written twice (Excalidraw can re-emit onLibraryChange for one
      // change). A real add/save/delete changes the bag and persists.
      const content = libraryEnvelope(items);
      if (content === lastBagSigRef.current) return;
      lastBagSigRef.current = content;
      persistTeam.trigger(content);
    },
    [persistTeam],
  );

  // Apply an external scene in place: viewport-preserving, non-undoable
  // (a remote write shouldn't become a user undo step). Returns false if the
  // imperative API isn't captured yet, so the caller can fall back to a nudge
  // rather than silently drop the change.
  const applyExternal = useCallback(
    (s: ExcalidrawScene): boolean => {
      const api = apiRef.current;
      if (!api) return false;
      // Discard any debounced local save still in flight: the user took the
      // disk state (turn-based), so a queued write of their now-superseded
      // edits must not land afterward and clobber the scene we just applied.
      save.cancel();
      api.updateScene({
        elements: s.elements,
        appState: persistedAppState(s.appState),
        captureUpdate: CaptureUpdateAction.NEVER,
      } as Parameters<ExcalidrawAPI["updateScene"]>[0]);
      const files = Object.values(s.files);
      if (files.length) {
        api.addFiles(files as Parameters<ExcalidrawAPI["addFiles"]>[0]);
      }
      // Seed the baseline from what the canvas ACTUALLY holds now, not from the
      // input scene: `updateScene` merges appState and `addFiles` unions files,
      // so the live state can differ from `s` (e.g. an omitted persisted key is
      // retained, files accumulate). Reading the committed state makes the
      // onChange this triggers a true no-op — no spurious write-back to disk.
      const appliedSig = sceneSignature(api.getSceneElements(), api.getAppState(), api.getFiles());
      diskSigRef.current = appliedSig;
      liveSigRef.current = appliedSig;
      return true;
    },
    [save],
  );

  // A reload the user must opt into because they have unsaved local edits
  // (AC-3). Holds the latest external scene; a newer write while the nudge is
  // open replaces it so "reload" always takes the freshest disk state.
  const [pendingReload, setPendingReload] = useState<ExcalidrawScene | null>(null);

  // React to an external write parsed by the wrapper. Gated on `reloadNonce`
  // so it fires once per genuine change and never on the initial mount.
  const seenNonceRef = useRef<number | undefined>(reloadNonce);
  useEffect(() => {
    if (reloadNonce === seenNonceRef.current) return;
    seenNonceRef.current = reloadNonce;
    if (!externalScene) return;
    const externalSig = sceneSignature(
      externalScene.elements,
      externalScene.appState,
      externalScene.files,
    );
    const dirty = liveSigRef.current !== diskSigRef.current;
    const decision = decideCanvasReload({ externalSig, diskSig: diskSigRef.current, dirty });
    if (decision === "noop") {
      // Disk matches what we already hold — clear any open nudge that was
      // pointing at a now-superseded intermediate scene.
      setPendingReload(null);
      return;
    }
    if (decision === "apply") {
      // Fall back to a nudge if the API isn't captured yet, so a clean-canvas
      // reload is surfaced (user-recoverable) rather than silently dropped.
      setPendingReload(applyExternal(externalScene) ? null : externalScene);
      return;
    }
    setPendingReload(externalScene);
  }, [reloadNonce, externalScene, applyExternal]);

  const acceptPendingReload = useCallback(() => {
    if (!pendingReload) return;
    // Keep the banner up if the apply couldn't run (API not ready) so the
    // reload isn't lost; by click time the API is captured in practice.
    if (applyExternal(pendingReload)) setPendingReload(null);
  }, [pendingReload, applyExternal]);

  // Load the team library into the panel — fully off the mount path (AC-6):
  // the canvas has already rendered by the time this effect runs. Re-runs when
  // the API/context resolve (a team switch swaps `libraryCtx`); runs are
  // chained so they can't interleave their `updateLibrary` calls. All
  // management after this is Excalidraw's own native panel (SW-SC-8).
  useEffect(() => {
    const api = apiRef.current;
    if (!apiReady || !api || !libraryCtx) return;
    let cancelled = false;
    const run = async () => {
      if (cancelled) return;
      // Drain any in-flight/pending write BEFORE the read so the `merge: false`
      // inject can't drop a just-saved shape (AC-3). `flush()` awaits the whole
      // outstanding write chain, so the read can't race it. The flush + read
      // legs stay UNGATED, so a user edit during them is queued, not dropped.
      await persistTeam.flush();
      if (cancelled) return;
      let content: string | null;
      try {
        content = await readCanvasLibrary(libraryCtx);
      } catch (e) {
        logger.error("viewer", `excalidraw library read failed: ${e}`);
        return;
      }
      // No team bag yet (fresh team, or seed unavailable) → nothing to inject.
      if (cancelled || content == null) return;
      // Gate the injection's own `onLibraryChange` so it doesn't persist the
      // just-read bag straight back (idempotent, but a wasted write — and a
      // slow read would otherwise wipe-then-persist an in-session edit).
      injectingRef.current = true;
      try {
        await api.updateLibrary({
          libraryItems: packBlob(content),
          merge: false,
          openLibraryMenu: false,
        });
      } catch (e) {
        logger.error("viewer", `excalidraw library injection failed: ${e}`);
        toast.error("Couldn't load wireframe libraries.", { id: "excalidraw-lib-error" });
      } finally {
        injectingRef.current = false;
      }
    };
    injectChainRef.current = injectChainRef.current.then(run, run);
    return () => {
      cancelled = true;
    };
  }, [apiReady, libraryCtx, persistTeam]);

  return (
    // SW-SC-10: marks this subtree as a canvas so the document-level gesture
    // guard (useGesturePaneScope) lets an over-canvas pinch through while
    // suppressing pinches that began over the pipeline pane.
    <div className="relative h-full w-full bg-background" {...{ [EXCALIDRAW_CANVAS_ATTR]: "" }}>
      <style>{`${LIBRARY_PUBLISH_ITEM_SELECTOR} { display: none; }`}</style>
      {pendingReload && (
        // Non-modal: positioned over the top of the canvas; pointer events are
        // scoped to the banner so drawing continues underneath (AC-3).
        <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center">
          <div className="pointer-events-auto flex items-center gap-3 rounded-md border bg-popover px-3 py-1.5 text-sm text-popover-foreground shadow-md">
            <span>Canvas updated on disk</span>
            <button
              type="button"
              onClick={acceptPendingReload}
              className="rounded bg-primary px-2 py-0.5 text-xs font-medium text-primary-foreground hover:opacity-90"
            >
              Reload
            </button>
            <button
              type="button"
              aria-label="Dismiss"
              onClick={() => setPendingReload(null)}
              className="rounded px-1 text-xs text-muted-foreground hover:text-foreground"
            >
              ✕
            </button>
          </div>
        </div>
      )}
      <Excalidraw
        excalidrawAPI={(api) => {
          apiRef.current = api;
          setApiReady(true);
        }}
        initialData={initialData}
        theme={resolvedTheme === "dark" ? "dark" : "light"}
        viewModeEnabled={readOnly}
        onChange={handleChange}
        onLibraryChange={handleLibraryChange}
        // SW-SC-13 (AC-5): Excalidraw's native "Export image" / "Save as image"
        // save via a browser download — a silent no-op in the Tauri webview.
        // Hide them so the only export affordance is the working Tauri-aware
        // right-click "Export as PNG/SVG" on the canvas's nav node.
        UIOptions={{ canvasActions: { export: false, saveAsImage: false } }}
        // Web embeds are off: Excalidraw would otherwise load every allowlisted embed in the
        // scene (YouTube, Figma, X…) as a third-party frame as soon as the canvas opens.
        validateEmbeddable={rejectEmbeddable}
      />
    </div>
  );
}
