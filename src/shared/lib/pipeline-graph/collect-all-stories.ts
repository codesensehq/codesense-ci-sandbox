// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree, StoryRef } from "./types";

/**
 * Recursively collect all `StoryRef` entries from a `DerivedPipelineTree`,
 * walking `stories` at each level and recursing into `subEpics`.
 */
export function collectAllStories(tree: DerivedPipelineTree): StoryRef[] {
  const result: StoryRef[] = [];

  function walk(node: DerivedPipelineTree): void {
    for (const story of node.stories) {
      result.push(story);
    }
    for (const sub of node.subEpics) {
      walk(sub);
    }
  }

  walk(tree);
  return result;
}

/**
 * Count all stories in a `DerivedPipelineTree` without allocating an array.
 */
export function countAllStories(tree: DerivedPipelineTree): number {
  let count = tree.stories.length;
  for (const sub of tree.subEpics) {
    count += countAllStories(sub);
  }
  return count;
}
