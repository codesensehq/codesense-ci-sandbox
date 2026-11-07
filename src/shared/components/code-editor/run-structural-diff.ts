// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8b (AC-3) — the structural-diff binding of the shared worker-layout
 * runner (mirrors `commit-graph/run-graph-layout.ts`): the same off-thread
 * lifecycle (singleton worker, stale-request drop, watchdog timeout, error reset)
 * wired to `structural-diff.worker.ts`.
 *
 * The one deliberate divergence from the graph binding: there is NO synchronous
 * main-thread fallback. A structural parse is far too heavy to run on the render
 * thread, so when the worker is unavailable (SSR/tests), can't be constructed, or
 * `postMessage` throws, `runSync` REJECTS — the caller (`useStructuralDiff`) then
 * falls back to the plain bounded line diff. That keeps the "never blocks the
 * render thread / never spins the terminal tiles" guarantee intact (AC-3).
 */

import {
  createWorkerLayoutRunner,
  type WorkerLayoutHandle,
} from "@/shared/lib/worker-layout/worker-layout-runner";

import type { LanguageId } from "./cm-loader";
import type { OffsetChange } from "./whitespace-diff";
import type { StructuralDiffRequest, StructuralDiffResponse } from "./structural-diff";
import StructuralDiffWorker from "./structural-diff.worker.ts?worker";

export interface StructuralDiffInput {
  before: string;
  after: string;
  language: LanguageId;
}

export type StructuralDiffHandle = WorkerLayoutHandle<OffsetChange[]>;

const runner = createWorkerLayoutRunner<
  StructuralDiffInput,
  OffsetChange[],
  StructuralDiffRequest,
  StructuralDiffResponse
>({
  createWorker: () => new StructuralDiffWorker(),
  toMessage: (requestId, input) => ({
    requestId,
    before: input.before,
    after: input.after,
    language: input.language,
  }),
  fromMessage: (data) =>
    data.ok
      ? { requestId: data.requestId, ok: true, result: data.changes }
      : {
          requestId: data.requestId,
          ok: false,
          error: data.error || "structural diff worker: malformed response",
        },
  // No sync fallback: reject so the caller uses the line diff (never parse on the
  // render thread).
  runSync: () => {
    throw new Error("structural diff requires a worker");
  },
  labels: {
    timeout: "structural diff worker timed out",
    workerError: "structural diff worker error",
    cloneError: "structural diff worker: response could not be cloned",
  },
});

/**
 * Compute a structural diff off the main thread. The handle rejects when the
 * worker is unavailable / fails / times out — callers fall back to the line diff.
 */
export function runStructuralDiff(input: StructuralDiffInput): StructuralDiffHandle {
  return runner.run(input);
}
