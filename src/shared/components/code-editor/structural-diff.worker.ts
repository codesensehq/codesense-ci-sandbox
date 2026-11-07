// SPDX-License-Identifier: MIT
/// <reference lib="webworker" />
import { diffArrays } from "diff";

import {
  structuralOffsetChanges,
  treeToStructuralTokens,
  type StructuralDiffRequest,
  type StructuralDiffResponse,
} from "./structural-diff";
import { parseStructural } from "./structural-diff-parsers";

// SLW-DV-8b (AC-3): parse both sides and diff their token streams OFF the render
// thread (Lezer trees can't cross the worker boundary, so parsing happens here),
// posting back only the serializable `OffsetChange[]`. Mirrors the pure
// parse-and-return shape of `commit-graph/layout.worker.ts`.
self.addEventListener("message", (ev: MessageEvent<StructuralDiffRequest>) => {
  const { requestId, before, after, language } = ev.data;
  try {
    const treeA = parseStructural(before, language);
    const treeB = parseStructural(after, language);
    if (!treeA || !treeB) throw new Error(`no structural parser for language "${language}"`);
    const tokensA = treeToStructuralTokens(treeA, (from, to) => before.slice(from, to));
    const tokensB = treeToStructuralTokens(treeB, (from, to) => after.slice(from, to));
    const changes = structuralOffsetChanges(tokensA, tokensB, (a, b) => diffArrays(a, b));
    const resp: StructuralDiffResponse = { requestId, ok: true, changes };
    self.postMessage(resp);
  } catch (e) {
    const resp: StructuralDiffResponse = {
      requestId,
      ok: false,
      error: e instanceof Error ? e.message : String(e),
    };
    self.postMessage(resp);
  }
});

export {};
