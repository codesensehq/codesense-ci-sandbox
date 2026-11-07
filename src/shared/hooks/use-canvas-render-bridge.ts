// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { resolveCanvasRender } from "@/tauri/commands";
import { renderCanvasToPngBase64 } from "@/features/dashboard/lib/export-canvas";
import { logger } from "@/shared/lib/logger";

/**
 * SW-SC-14 — frontend leg of the `render_canvas` MCP render bridge.
 *
 * Excalidraw's exporter is DOM-bound, so the Rust MCP verb can't render on its
 * own: it emits a `canvas:render-request` and waits. This listener renders the
 * canvas's CURRENT on-disk scene through the same exporter the user sees (the
 * shared `renderCanvasToPngBase64` core) and resolves the bridge with the
 * base64 PNG bytes directly — the Rust verb embeds those bytes inline in its
 * MCP tool response as an `image` content block.
 *
 * MCP architectural reframe (2026-10): previously wrote an `.excalidraw.png`
 * sidecar via `writeBinaryContentFile` and resolved with the sidecar path.
 * Dropping the sidecar means (a) no PNG lands on disk per render, so the docs
 * tree stays source-only, and (b) the agent sees the render inline without an
 * extra read round-trip.
 */

export type CanvasRenderRequest = { requestId: string; path: string; sceneJson?: string };

export type CanvasRenderDeps = {
  render: (path: string, sceneJson?: string) => Promise<string>;
  resolve: (requestId: string, pngBase64: string | null, error: string | null) => Promise<void>;
};

const defaultDeps: CanvasRenderDeps = {
  render: (path, sceneJson) => renderCanvasToPngBase64(path, undefined, sceneJson),
  resolve: (requestId, pngBase64, error) => resolveCanvasRender(requestId, pngBase64, error),
};

/**
 * Handle one render request end-to-end: render → resolve with the base64, or
 * resolve with an error if any step throws (so the Rust verb sees a real
 * failure instead of timing out). Pure + dependency-injected so the round-trip
 * is unit-testable without React or Tauri.
 */
export async function runCanvasRenderRequest(
  req: CanvasRenderRequest,
  deps: CanvasRenderDeps = defaultDeps,
): Promise<void> {
  let pngBase64: string | null = null;
  let error: string | null = null;
  try {
    pngBase64 = await (req.sceneJson === undefined
      ? deps.render(req.path)
      : deps.render(req.path, req.sceneJson));
  } catch (e) {
    pngBase64 = null;
    error = e instanceof Error ? e.message : String(e);
    logger.error("mcp", `canvas render request failed for ${req.path}: ${error}`);
  }
  // Reply exactly once. A failed reply (e.g. the IPC itself rejects) can't be
  // recovered — the Rust verb falls back to its timeout — so swallow-and-log it
  // rather than let `void runCanvasRenderRequest(...)` become an unhandled
  // rejection.
  try {
    await deps.resolve(req.requestId, pngBase64, error);
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    logger.error("mcp", `canvas render resolve failed for ${req.requestId}: ${message}`);
  }
}

/**
 * Mount the render bridge once at app root (App.tsx), beside `useMcpActions`.
 */
export function useCanvasRenderBridge() {
  useEffect(() => {
    const unlisten = listen<CanvasRenderRequest>("canvas:render-request", (event) => {
      void runCanvasRenderRequest(event.payload);
    });
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
