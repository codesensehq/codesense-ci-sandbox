// SPDX-License-Identifier: MIT
import type { CommitLaneInput, CommitGraphLayout } from "./layout";

/**
 * SLW-WD-9: message contract between the main thread and the commit-graph layout
 * Web Worker (mirrors `pipeline-layout-types`). `layoutCommitGraph` is pure and
 * operates on plain `{ sha, parents }` — both directions structured-clone cleanly.
 */
export interface GraphLayoutRequest {
  requestId: number;
  commits: CommitLaneInput[];
}

export type GraphLayoutResponse =
  | { requestId: number; ok: true; layout: CommitGraphLayout }
  | { requestId: number; ok: false; error: string };
