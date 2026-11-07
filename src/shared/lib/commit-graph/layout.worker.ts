// SPDX-License-Identifier: MIT
/// <reference lib="webworker" />
import { layoutCommitGraph } from "./layout";
import type { GraphLayoutRequest, GraphLayoutResponse } from "./layout-types";

// SLW-WD-9: run the commit-graph lane sweep off the main thread (mirrors
// `pipeline-layout.worker`). `layout.ts` is pure (no DOM), so it runs unchanged.
self.addEventListener("message", (ev: MessageEvent<GraphLayoutRequest>) => {
  const { requestId, commits } = ev.data;
  try {
    const layout = layoutCommitGraph(commits);
    const resp: GraphLayoutResponse = { requestId, ok: true, layout };
    self.postMessage(resp);
  } catch (e) {
    const resp: GraphLayoutResponse = {
      requestId,
      ok: false,
      error: e instanceof Error ? e.message : String(e),
    };
    self.postMessage(resp);
  }
});

export {};
