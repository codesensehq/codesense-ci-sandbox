// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, DocRef } from "./types";

/**
 * Recursively collect all plain-doc / binary-leaf `DocRef`s (`node.docs`) across
 * a `DerivedPipelineTree`, walking `docs` at each level and recursing into
 * `subEpics`. Sibling to `collectAllStories`.
 *
 * `node.docs` holds the non-story files the scanner surfaces: plain md/html docs
 * that fail the pipeline-type predicate (arch/README/notes) and supporting-doc
 * binaries (`.excalidraw`, images, pdf). The query layer's full-file
 * `supporting_docs` resolution (SW-DX-8) uses this so a `supporting_docs` ref to
 * a non-story file resolves as a real file instead of a dead ref.
 */
export function collectAllDocs(tree: DerivedPipelineTree): DocRef[] {
  const result: DocRef[] = [];

  function walk(node: DerivedPipelineTree): void {
    for (const doc of node.docs ?? []) {
      result.push(doc);
    }
    for (const sub of node.subEpics) {
      walk(sub);
    }
  }

  walk(tree);
  return result;
}
