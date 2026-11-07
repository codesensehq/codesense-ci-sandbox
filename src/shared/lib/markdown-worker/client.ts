// SPDX-License-Identifier: MIT
import type { Root as HastRoot } from "hast";
import type { Block, ParseRequest, ParseResponse, WorkerPerfBlocks, WorkerPerfHast } from "./types";
import ParseWorker from "./parse.worker.ts?worker";

export interface HastResult {
  hast: HastRoot;
  perf: WorkerPerfHast;
  // Absolute-time timestamp at which the main thread received the
  // message; used alongside `perf.postAt` to derive postMessage
  // transfer cost when diagnosing.
  receiveAt: number;
}

export interface BlocksResult {
  blocks: Block[];
  perf: WorkerPerfBlocks;
  receiveAt: number;
}

type PendingHast = {
  mode: "hast";
  resolve: (r: HastResult) => void;
  reject: (e: Error) => void;
};
type PendingBlocks = {
  mode: "blocks";
  resolve: (r: BlocksResult) => void;
  reject: (e: Error) => void;
};
type PendingExport = {
  mode: "export";
  resolve: (r: string) => void;
  reject: (e: Error) => void;
};
type PendingRedline = {
  mode: "redline";
  resolve: (r: HastResult) => void;
  reject: (e: Error) => void;
};
type PendingEntry = PendingHast | PendingBlocks | PendingExport | PendingRedline;

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, PendingEntry>();

function ensureWorker(): Worker {
  if (worker) return worker;
  worker = new ParseWorker();
  worker.addEventListener("message", (ev: MessageEvent<ParseResponse>) => {
    const receiveAt = performance.timeOrigin + performance.now();
    const { requestId } = ev.data;
    const entry = pending.get(requestId);
    if (!entry) return; // stale / cancelled
    pending.delete(requestId);

    if (!ev.data.ok) {
      entry.reject(new Error(ev.data.error));
      return;
    }

    if (ev.data.mode === "hast" && entry.mode === "hast") {
      entry.resolve({ hast: ev.data.hast, perf: ev.data.perf, receiveAt });
      return;
    }
    if (ev.data.mode === "blocks" && entry.mode === "blocks") {
      entry.resolve({
        blocks: ev.data.blocks,
        perf: ev.data.perf,
        receiveAt,
      });
      return;
    }
    if (ev.data.mode === "export" && entry.mode === "export") {
      entry.resolve(ev.data.html);
      return;
    }
    if (ev.data.mode === "redline" && entry.mode === "redline") {
      entry.resolve({ hast: ev.data.hast, perf: ev.data.perf, receiveAt });
      return;
    }
    entry.reject(
      new Error(`markdown worker mode mismatch: expected ${entry.mode}, got ${ev.data.mode}`),
    );
  });
  // Worker-level failures come in two flavours:
  //   - `error`        — uncaught exception in the worker's event loop.
  //   - `messageerror` — a response value couldn't be structured-cloned
  //                      across the boundary. The response is dropped
  //                      by the browser; the pending entry would orphan
  //                      into a silent hang without this handler.
  // In both cases the worker is likely in an unknowable state, so we
  // reject every pending entry AND null the singleton reference. The
  // next `ensureWorker()` call spawns a fresh instance; without the
  // null-out, a wedged worker stays cached and every subsequent parse
  // hangs forever.
  const failAllAndReset = (reason: string) => {
    for (const [, entry] of pending) entry.reject(new Error(reason));
    pending.clear();
    // RF-perf-15 (F-6): terminate the wedged worker before dropping the ref so its
    // thread + heap + WASM are freed deterministically — a live Worker isn't GC'd
    // while it can still post. Mirrors the tested worker-layout-runner sibling.
    if (worker) {
      worker.terminate();
      worker = null;
    }
  };
  worker.addEventListener("error", (ev) => failAllAndReset(ev.message));
  worker.addEventListener("messageerror", () =>
    failAllAndReset("markdown worker: response could not be cloned"),
  );
  return worker;
}

export interface ParseHandle<T> {
  promise: Promise<T>;
  cancel: () => void;
}

/**
 * Legacy hast-mode entry point — kept so the `VIEWER_PERF.renderMode =
 * "jsx"` rollback branch behaves exactly like the 32.4 code did.
 * Returns the full hast root; caller renders via
 * `hast-util-to-jsx-runtime` on the main thread.
 */
export function parseMarkdown(text: string): ParseHandle<HastRoot> {
  const w = ensureWorker();
  const requestId = nextId++;
  const promise = new Promise<HastResult>((resolve, reject) => {
    pending.set(requestId, { mode: "hast", resolve, reject });
  }).then((r) => r.hast);
  const req: ParseRequest = { requestId, mode: "hast", text };
  w.postMessage(req);
  return { promise, cancel: () => pending.delete(requestId) };
}

/**
 * Block-mode entry point introduced by story 32.6. Worker runs the same
 * remark/rehype pipeline, rewrites internal .md links into
 * `data-internal-href` attributes, then deep-walks the hast and
 * extracts every `<pre><code>` and `language-mermaid` fence into a
 * `Block` entry — replacing each nested-island in-tree with a
 * `<span data-island-id="N">` placeholder so main can mount the React
 * island at the right DOM location.
 *
 * The resulting `Block[]` is the full render payload for the block
 * path — main needs only map it to JSX, no `hast-util-to-jsx-runtime`
 * walk, no H2 cost.
 */
export function parseToBlocks(
  text: string,
  filePath: string,
  teamContentPath: string,
): ParseHandle<BlocksResult> {
  const w = ensureWorker();
  const requestId = nextId++;
  const promise = new Promise<BlocksResult>((resolve, reject) => {
    pending.set(requestId, { mode: "blocks", resolve, reject });
  });
  const req: ParseRequest = {
    requestId,
    mode: "blocks",
    text,
    filePath,
    teamContentPath,
  };
  w.postMessage(req);
  return { promise, cancel: () => pending.delete(requestId) };
}

/**
 * Export-mode entry point for PDF export (R.4). Worker runs the same
 * remark/rehype pipeline, then stringifies the full HAST root to an
 * HTML string via rehype-stringify — no walkAndTransform, no block
 * splitting, no copy-button wrappers. Mermaid/d2 fences remain as
 * raw `<pre><code class="language-mermaid">`.
 *
 * Returns just the HTML fragment string; the caller wraps it in a
 * full document with inlined print stylesheet.
 */
export function parseToExportHtml(text: string): ParseHandle<string> {
  const w = ensureWorker();
  const requestId = nextId++;
  const promise = new Promise<string>((resolve, reject) => {
    pending.set(requestId, { mode: "export", resolve, reject });
  });
  const req: ParseRequest = { requestId, mode: "export", text };
  w.postMessage(req);
  return { promise, cancel: () => pending.delete(requestId) };
}

/**
 * SW-GS-3 redline entry point — parse `before` + `after` and block-align them
 * into one annotated ins/del HAST root (the shared SW-DR-2 spine). Returns the
 * full hast root; the caller renders it via `hast-util-to-jsx-runtime`, same as
 * the legacy `hast` path.
 */
export function parseToRedline(before: string, after: string): ParseHandle<HastResult> {
  const w = ensureWorker();
  const requestId = nextId++;
  const promise = new Promise<HastResult>((resolve, reject) => {
    pending.set(requestId, { mode: "redline", resolve, reject });
  });
  const req: ParseRequest = { requestId, mode: "redline", before, after };
  w.postMessage(req);
  return { promise, cancel: () => pending.delete(requestId) };
}
