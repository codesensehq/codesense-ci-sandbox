// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import {
  admitRenderedDocument,
  resolveRenderedDocument,
  type RenderedDocumentCapture,
} from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

export type RenderedDocumentFormat = "text" | "html";
type Capture = (format: RenderedDocumentFormat) => Promise<RenderedDocumentCapture>;
type Viewer = { capture: Capture; frameId?: string };
const viewers = new Map<string, Map<symbol, Viewer>>();

// Match the backend's lexical context-path validation (without resolving symlinks).
export function renderedDocumentPath(path: string): string {
  const parts: string[] = [];
  for (const part of path.split("/")) {
    if (part === "..") parts.pop();
    else if (part && part !== ".") parts.push(part);
  }
  return `/${parts.join("/")}`;
}

export function registerRenderedDocument(
  path: string,
  capture: Capture,
  frameId?: string,
): () => void {
  const key = renderedDocumentPath(path);
  const id = Symbol();
  const entries = viewers.get(key) ?? new Map<symbol, Viewer>();
  entries.set(id, { capture, frameId });
  viewers.set(key, entries);
  return () => {
    entries.delete(id);
    if (!entries.size) viewers.delete(key);
  };
}

export async function runRenderedDocumentRequest(
  request: {
    requestId: string;
    path: string;
    format: RenderedDocumentFormat;
    requiresAuthority?: boolean;
  },
  resolve = resolveRenderedDocument,
  admit = admitRenderedDocument,
): Promise<void> {
  let capture: RenderedDocumentCapture | null = null;
  let error: string | null = null;
  try {
    const entries = viewers.get(renderedDocumentPath(request.path));
    if (!entries?.size)
      throw new Error(
        "Open the current HTML document in CodeSense, then select its tab and retry. For a pane, call pane_open and show its panel; a closed or rejected pane has no rendered frame.",
      );
    if (entries.size !== 1)
      throw new Error(
        "Multiple current viewers match this document; close duplicate views and retry.",
      );
    const viewer = entries.values().next().value!;
    if (request.requiresAuthority) await admit(request.requestId, viewer.frameId);
    capture = await viewer.capture(request.format);
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
  try {
    await resolve(request.requestId, capture, error);
  } catch (e) {
    logger.error("mcp", `rendered document reply failed: ${String(e)}`);
  }
}

export function useRenderedDocumentBridge() {
  useEffect(() => {
    const unlisten = getCurrentWebviewWindow().listen<
      Parameters<typeof runRenderedDocumentRequest>[0]
    >("document:read-rendered-request", (event) => {
      void runRenderedDocumentRequest(event.payload);
    });
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
