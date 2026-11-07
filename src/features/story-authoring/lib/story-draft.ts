// SPDX-License-Identifier: MIT
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

/** An epic the New-story form can write into. */
export interface EpicOption {
  /** Absolute epic directory (the story file lands here). */
  absoluteDirPath: string;
  /** Tree-relative path, used as the picker label. */
  dirPath: string;
  /** Absolute path of the epic's main doc (README/index), for `repos:`. */
  mainDocPath: string;
}

/** Every directory in the tree that has an epic main doc, in tree order. */
export function listEpics(tree: DerivedPipelineTree): EpicOption[] {
  const out: EpicOption[] = [];
  const walk = (node: DerivedPipelineTree) => {
    if (node.mainDoc) {
      out.push({
        absoluteDirPath: node.absoluteDirPath,
        dirPath: node.dirPath || node.dirName,
        mainDocPath: node.mainDoc.filePath,
      });
    }
    node.subEpics.forEach(walk);
  };
  walk(tree);
  return out;
}

/** The subtree rooted at `absoluteDirPath`, or null. */
export function findEpicNode(
  tree: DerivedPipelineTree,
  absoluteDirPath: string,
): DerivedPipelineTree | null {
  if (tree.absoluteDirPath === absoluteDirPath) return tree;
  for (const sub of tree.subEpics) {
    const hit = findEpicNode(sub, absoluteDirPath);
    if (hit) return hit;
  }
  return null;
}

/** Every `story_id` in the tree (the depends_on picker's options). */
export function allStoryIds(tree: DerivedPipelineTree): string[] {
  const ids = new Set<string>();
  for (const story of collectAllStories(tree)) {
    if (story.storyId) ids.add(story.storyId);
  }
  return [...ids].sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
}

const NUMBERED_ID = /^(.+)-(\d+)$/;

/**
 * The id prefix an epic's stories share (`SW-PL` for `SW-PL-3`, `SW-PL-12`).
 * The most common prefix wins; ties go to the prefix seen first. Null when the
 * epic has no numbered story ids yet.
 */
export function epicStoryIdPrefix(epicStoryIds: readonly string[]): string | null {
  const counts = new Map<string, number>();
  for (const id of epicStoryIds) {
    const prefix = NUMBERED_ID.exec(id.trim())?.[1];
    if (prefix) counts.set(prefix, (counts.get(prefix) ?? 0) + 1);
  }
  let best: string | null = null;
  let bestCount = 0;
  for (const [prefix, count] of counts) {
    if (count > bestCount) {
      best = prefix;
      bestCount = count;
    }
  }
  return best;
}

/**
 * The next free id for `prefix`: one past the highest number used with that
 * prefix anywhere in `existingIds` (the whole org, so a minted id never
 * collides with a story in another epic).
 */
export function mintStoryId(prefix: string, existingIds: readonly string[]): string {
  let max = 0;
  for (const id of existingIds) {
    const m = NUMBERED_ID.exec(id.trim());
    if (m && m[1] === prefix) max = Math.max(max, Number(m[2]));
  }
  return `${prefix}-${max + 1}`;
}

/**
 * A prefix for an epic with no numbered stories: the uppercase initials of the
 * directory name's words, ignoring a leading ordering number
 * (`14-public-launch` → `PL`). Falls back to `STORY`.
 */
export function prefixFromEpicDirName(dirName: string): string {
  const words = dirName.split(/[^A-Za-z0-9]+/).filter((w) => w && !/^\d+$/.test(w));
  const initials = words.map((w) => w[0].toUpperCase()).join("");
  return initials || "STORY";
}

/** The id the form proposes for a new story in `epic`: the epic's own story-id prefix,
 *  or its directory's initials while it has no numbered stories yet. */
export function proposeStoryId(tree: DerivedPipelineTree, epicAbsoluteDirPath: string): string {
  const node = findEpicNode(tree, epicAbsoluteDirPath);
  const epicIds = node
    ? collectAllStories(node).flatMap((s) => (s.storyId ? [s.storyId] : []))
    : [];
  const prefix = epicStoryIdPrefix(epicIds) || prefixFromEpicDirName(node?.dirName ?? "");
  return mintStoryId(prefix, allStoryIds(tree));
}

/**
 * The repos a story's `source_repo` is picked from: the epic's `repos:` when it
 * names any, else the team's repos.
 */
export function repoChoices(epicRepos: readonly string[], teamRepos: readonly string[]): string[] {
  return [...new Set(epicRepos.length > 0 ? epicRepos : teamRepos)];
}

/**
 * The repo preselected for a story: the only choice when there is exactly one
 * (see {@link repoChoices}), else empty so the author must pick.
 */
export function defaultSourceRepo(
  epicRepos: readonly string[],
  teamRepos: readonly string[],
): string {
  const choices = repoChoices(epicRepos, teamRepos);
  return choices.length === 1 ? choices[0] : "";
}
