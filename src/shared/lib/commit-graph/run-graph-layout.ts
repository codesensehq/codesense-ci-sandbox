// SPDX-License-Identifier: MIT
import {
  createWorkerLayoutRunner,
  type WorkerLayoutHandle,
} from "@/shared/lib/worker-layout/worker-layout-runner";
import { layoutCommitGraph, type CommitLaneInput, type CommitGraphLayout } from "./layout";
import type { GraphLayoutRequest, GraphLayoutResponse } from "./layout-types";
import GraphLayoutWorker from "./layout.worker.ts?worker";

/** A commit-graph layout request handle. */
export type GraphLayoutHandle = WorkerLayoutHandle<CommitGraphLayout>;

// SLW-WD-9 (RD-8): the commit-graph binding of the shared worker-layout runner —
// the exact off-thread lifecycle the pipeline uses (singleton, stale-drop,
// watchdog, error reset, sync fallback), just wired to this worker + the pure
// `layoutCommitGraph`.
const runner = createWorkerLayoutRunner<
  CommitLaneInput[],
  CommitGraphLayout,
  GraphLayoutRequest,
  GraphLayoutResponse
>({
  createWorker: () => new GraphLayoutWorker(),
  toMessage: (requestId, commits) => ({ requestId, commits }),
  fromMessage: (data) =>
    data.ok
      ? { requestId: data.requestId, ok: true, result: data.layout }
      : { requestId: data.requestId, ok: false, error: data.error },
  runSync: (commits) => layoutCommitGraph(commits),
  labels: {
    timeout: "commit-graph layout worker timed out",
    workerError: "commit-graph layout worker error",
    cloneError: "commit-graph layout worker: response could not be cloned",
  },
});

/**
 * Lay out the commit graph, off the main thread when possible. Falls back to a
 * synchronous main-thread layout when `Worker` is unavailable (tests / SSR), the
 * worker can't be constructed, or `postMessage` throws — identical output.
 */
export function runGraphLayout(commits: CommitLaneInput[]): GraphLayoutHandle {
  return runner.run(commits);
}
