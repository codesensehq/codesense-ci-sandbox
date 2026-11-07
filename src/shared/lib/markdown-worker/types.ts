// SPDX-License-Identifier: MIT
import type { Root as HastRoot } from "hast";

// Code fences never travel as their own Block: the worker emits their
// full <pre><code> markup inline inside the containing `html` block,
// wrapped with a <button class="copy-btn"> that the main-thread container
// dispatches via event delegation (see markdown-viewer.tsx onClick).
// Zero createRoot per code fence — whether top-level or nested at any
// depth. Only island blocks (mermaid, d2, csquery) remain as React-island
// mounts via `data-island-id` placeholders: diagrams need async SVG
// rendering, and SW-DX-5's `csquery` needs to run a live query + subscribe
// to the status stream — neither can live as inert DOM.
export type Block =
  | { id: number; kind: "html"; html: string }
  | { id: number; kind: "mermaid"; code: string }
  | { id: number; kind: "d2"; code: string }
  // SW-DX-5: an inline `csquery` fence — the fence body (a tiny query DSL) is
  // carried verbatim; the main thread parses + renders it as a live island.
  | { id: number; kind: "csquery"; code: string }
  | { id: number; kind: "table"; html: string };

export type ParseRequest =
  | { requestId: number; mode: "hast"; text: string }
  | {
      requestId: number;
      mode: "blocks";
      text: string;
      filePath: string;
      /**
       * PTS-2: per-team content dir, used as the fallback root for
       * `resolveViewerLinkPath` when an absolute href can't be anchored
       * against `CONTENT_DIR_SEGMENT` in `filePath`.
       */
      teamContentPath: string;
    }
  | { requestId: number; mode: "export"; text: string }
  // SW-GS-3: render a doc-aware redline of `before` → `after` (block-aligned
  // ins/del). Returns annotated HAST, same shape as `hast` mode.
  | { requestId: number; mode: "redline"; before: string; after: string };

export interface WorkerPerfHast {
  parseMs: number;
  runMs: number;
  // Absolute time (performance.timeOrigin + performance.now()) so the
  // main thread can diff against its own receive timestamp safely —
  // `performance.now()` alone has a per-realm time origin and can't be
  // compared across the worker boundary.
  postAt: number;
}

export interface WorkerPerfBlocks extends WorkerPerfHast {
  splitMs: number;
}

export type ParseResponse =
  | {
      requestId: number;
      ok: true;
      mode: "hast";
      hast: HastRoot;
      perf: WorkerPerfHast;
    }
  | {
      requestId: number;
      ok: true;
      mode: "blocks";
      blocks: Block[];
      perf: WorkerPerfBlocks;
    }
  | {
      requestId: number;
      ok: true;
      mode: "export";
      html: string;
      perf: WorkerPerfHast;
    }
  | {
      requestId: number;
      ok: true;
      mode: "redline";
      hast: HastRoot;
      perf: WorkerPerfHast;
    }
  | { requestId: number; ok: false; error: string };
