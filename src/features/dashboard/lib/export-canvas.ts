// SPDX-License-Identifier: MIT
import { toast as sonnerToast } from "sonner";

import { readFile, writeBinaryFile } from "@/tauri/commands";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import { blobToBase64 } from "@/shared/lib/blob-to-base64";
import {
  parseExcalidrawScene,
  sceneForLoad,
  type ExcalidrawScene,
} from "@/features/dashboard/lib/excalidraw-scene";

/**
 * SW-SC-13 — Tauri-aware canvas export (PNG + SVG).
 *
 * Excalidraw's native "Export image" saves via a browser download, a silent
 * no-op in the Tauri webview (Finding A). This uses the Tauri save flow —
 * `@tauri-apps/plugin-dialog` `save()` → `writeBinaryFile` (Finding B) — driven from the canvas node's
 * right-click menu, and renders the scene with `exportToBlob` / `exportToSvg`.
 *
 * Code-split (AC-6 / Finding D): the heavy `@excalidraw/excalidraw` engine is
 * reached only through `loadEngine`, whose default does a dynamic `import()`,
 * so this lib carries no static engine import even though the eagerly-bundled
 * nav calls it. Scene parsing comes from the engine-free `excalidraw-scene.ts`
 * leaf (Finding E), never a re-implemented parser.
 *
 * Responsiveness (AC-8): the save dialog is shown before the engine is loaded,
 * so cancelling never pays the engine-chunk cost; the post-confirm read →
 * render → write runs under a `toast.loading` so the cold first export gives
 * feedback. Deps are injected (defaults wire the real rails) for unit tests.
 */

export type ExportCanvasFormat = "png" | "svg";

/** The two scene→image renderers, loaded lazily so the engine stays code-split. */
export type CanvasExportEngine = {
  exportToBlob: (opts: {
    elements: readonly unknown[];
    appState?: Record<string, unknown>;
    files: Record<string, unknown> | null;
    mimeType?: string;
    [extra: string]: unknown;
  }) => Promise<Blob>;
  exportToSvg: (opts: {
    elements: readonly unknown[];
    appState?: Record<string, unknown>;
    files: Record<string, unknown> | null;
    [extra: string]: unknown;
  }) => Promise<SVGSVGElement>;
};

type SaveDialogOptions = {
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
};

type ToastLike = {
  loading: (message: string) => string | number;
  success: (message: string, opts?: { id?: string | number }) => void;
  error: (message: string, opts?: { id?: string | number }) => void;
};

export type ExportCanvasDeps = {
  readFile: (path: string) => Promise<string>;
  /** Native save dialog; returns the chosen path or `null` on cancel. */
  save: (opts: SaveDialogOptions) => Promise<string | null>;
  /** Lazily resolves the Excalidraw render fns — keeps the engine out of the main chunk. */
  loadEngine: () => Promise<CanvasExportEngine>;
  writeBinaryFile: (base64Data: string, outputPath: string) => Promise<void>;
  toast: ToastLike;
};

const defaultDeps: ExportCanvasDeps = {
  // Wrap the command imports rather than referencing them in this literal so
  // the binding is read at call time, not module-eval — otherwise any test
  // that partially mocks `@/tauri/commands` (e.g. App.test) throws on import.
  readFile: (path) => readFile(path),
  save: async (opts) => (await import("@tauri-apps/plugin-dialog")).save(opts),
  // Dynamic import is the code-split boundary (AC-6); the loose engine type
  // above intentionally widens Excalidraw's params, so cast through the module.
  loadEngine: async () => (await import("@excalidraw/excalidraw")) as unknown as CanvasExportEngine,
  writeBinaryFile: (base64Data, outputPath) => writeBinaryFile(base64Data, outputPath),
  toast: sonnerToast as unknown as ToastLike,
};

function basenameOf(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash >= 0 ? path.slice(slash + 1) : path;
}

/**
 * AC-4: the canvas basename with its `.excalidraw` extension swapped for the
 * export format — `…/design.excalidraw` → `design.png` / `design.svg`.
 */
export function exportDefaultFilename(canvasPath: string, format: ExportCanvasFormat): string {
  return `${stripViewerExtension(basenameOf(canvasPath))}.${format}`;
}

async function renderBase64(
  engine: CanvasExportEngine,
  scene: ExcalidrawScene,
  format: ExportCanvasFormat,
): Promise<string> {
  // The same filter the canvas loads through, so an export fetches nothing either.
  const { elements, appState, files } = sceneForLoad(scene);
  if (format === "png") {
    const blob = await engine.exportToBlob({ elements, appState, files, mimeType: "image/png" });
    return blobToBase64(blob);
  }
  const svg = await engine.exportToSvg({ elements, appState, files });
  const markup = new XMLSerializer().serializeToString(svg);
  // Route SVG markup through the same `blobToBase64`: a Blob UTF-8-encodes the
  // string (correct for non-ASCII, unlike bare `btoa`) and avoids a per-byte
  // main-thread concat — one base64 path for both formats.
  return blobToBase64(new Blob([markup], { type: "image/svg+xml" }));
}

/**
 * SW-SC-14: render a `.excalidraw` file's CURRENT on-disk scene to a PNG
 * base64, through the SAME `renderBase64` core as the right-click export — so
 * the agent's read-back is byte-for-byte the render the user sees. Headless
 * (read file → parse → engine → blob); needs no mounted canvas, never touches
 * the live-editing path. The render bridge (`use-canvas-render-bridge`) calls
 * this, writes the result to the PNG sidecar, and hands the path back to MCP.
 */
export async function renderCanvasToPngBase64(
  canvasPath: string,
  deps: Pick<ExportCanvasDeps, "readFile" | "loadEngine"> = defaultDeps,
  sceneJson?: string,
): Promise<string> {
  const text = sceneJson ?? (await deps.readFile(canvasPath));
  const scene = parseExcalidrawScene(text);
  const engine = await deps.loadEngine();
  const base64 = await renderBase64(engine, scene, "png");
  // A degenerate render (empty payload) would write a 0-byte sidecar behind a
  // success path — surface it as a failure so the bridge resolves with an error.
  if (!base64) throw new Error("render produced no image data");
  return base64;
}

/**
 * Export a `.excalidraw` canvas to a user-chosen PNG/SVG file. Fire-and-forget
 * from the nav context menu — never awaited in render (AC-8). A cancelled save
 * dialog is a clean no-op that never loads the engine.
 */
export async function exportCanvas(
  canvasPath: string,
  format: ExportCanvasFormat,
  deps: ExportCanvasDeps = defaultDeps,
): Promise<void> {
  const label = format.toUpperCase();
  const savePath = await deps.save({
    defaultPath: exportDefaultFilename(canvasPath, format),
    filters: [{ name: `${label} Image`, extensions: [format] }],
  });
  if (savePath === null) return; // AC-3: cancel → no file, no error, engine never loaded.

  const toastId = deps.toast.loading(`Exporting ${label}…`);
  try {
    const text = await deps.readFile(canvasPath);
    let scene: ExcalidrawScene;
    try {
      scene = parseExcalidrawScene(text);
    } catch {
      deps.toast.error("Couldn't export — the canvas file is malformed.", { id: toastId });
      return;
    }
    const engine = await deps.loadEngine();
    const base64 = await renderBase64(engine, scene, format);
    // Guard against a degenerate render: writing an empty payload would leave a
    // 0-byte file behind a success toast. Surface it as a failure instead.
    if (!base64) throw new Error("export produced no image data");
    await deps.writeBinaryFile(base64, savePath);
    deps.toast.success(`Exported ${label}`, { id: toastId });
  } catch (err) {
    deps.toast.error(`Export failed: ${err instanceof Error ? err.message : String(err)}`, {
      id: toastId,
    });
  }
}
