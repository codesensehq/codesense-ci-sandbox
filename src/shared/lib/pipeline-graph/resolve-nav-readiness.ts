// SPDX-License-Identifier: MIT
import type { StoryRef } from "./types";
import type {
  GroupIndexEntry,
  GroupIndexFile,
  FullGroupIndex,
  StoryIdIndex,
} from "./build-group-index";
import type { StatusIndex } from "@/shared/stores/status-taxonomy-store";
import { resolveStoryIdRef } from "./story-id-index";
import {
  isDoneStatus,
  isNotStartedStatus,
  normalizeStatus,
  stripDotSlash,
  stripPipelineExtension,
  resolveFileInGroup,
  resolveScopeRelativePath,
} from "./pipeline-utils";

/**
 * A dependency that prevents a story from being ready.
 *
 * - `unmet` — the dep resolved to a real story whose status ≠ `done`.
 * - `unresolved` — the dep could not be resolved against the supplied index:
 *   a missing sibling, a missing cross-epic file, or a target group absent
 *   from the index. ALL unresolved deps still block (strict resolver) — the
 *   reason exists only so the UI can word the blocker. The out-of-scope case
 *   must never *reach* this resolver in app code: readiness resolves against
 *   the full graph via `PipelineGraph` (SW-FX-1).
 */
export type BlockedDep =
  | { ref: string; reason: "unmet"; status: string }
  | { ref: string; reason: "unresolved" };

export interface ReadinessResult {
  /**
   * `true` iff the story's own status is *ready-eligible* (see
   * `isReadyEligibleStatus`) AND it has no blocking deps.
   */
  ready: boolean;
  /** Every dep that prevents readiness. Empty when all deps are met. */
  blockedBy: BlockedDep[];
}

/**
 * Is `status` eligible for the "ready" affordance (green left-nav row + pipeline
 * node glow + board readiness badge) once all `dependsOn` deps are met? SW-GT-3:
 * this is now role-keyed — a state whose role is `not_started` is the
 * *workable-next* state (deps met → ready to plan/build). Every later role
 * (`active`, `review`, `blocked`, `done`) is already in-flight or finished, so it
 * never lights up. Keyed off role (not literal `planning`/`todo`) so it survives a
 * team's replaced state set; single source of truth so the nav row, pipeline glow,
 * board badge, and blocked-title wording stay in lockstep.
 */
export function isReadyEligibleStatus(status: string, byId?: StatusIndex): boolean {
  return isNotStartedStatus(status, byId);
}

/**
 * Collect every `dependsOn` ref that prevents this story from being ready.
 * Returns `[]` for null/empty deps (trivially met). Self-references are
 * skipped. EVERY resolution failure is a blocker — nothing is softened; the
 * only fix for out-of-scope refs is to resolve against the full graph.
 *
 * Resolution algorithm (shares its precedence with the React Flow edge
 * transform's bare-ref resolution in `derivedTreeToReactFlow` — both resolve
 * story_ids through the same `buildGroupStoryIdIndex` / `resolveStoryIdRef`
 * primitive, so nav edges and readiness can never disagree on any ref):
 * 1. Strip `./` prefix
 * 2. Bare ref (no `/`) → **exact filename → stem** sibling lookup in the owner
 *    group, then **story_id** (`storyIdIndex`, the SW-SB-9 location-independent
 *    alias) resolved against the FULL index — the tier SW-SB-18's codemod made
 *    load-bearing (SW-FX-70)
 * 3. Path with `/` → cross-epic lookup via `resolveScopeRelativePath`
 * 4. Check the resolved story's status is a `done`-role state (role-keyed)
 *
 * `storyIdIndex` is built ONCE by the caller (`buildGroupStoryIdIndex`), never
 * inside this per-story call (AC-7). A poisoned (duplicate) id resolves to
 * nothing → `unresolved`, and a story_id that resolves back to this same story
 * is skipped as a self-reference (AC-4).
 */
export function collectUnmetDeps(
  story: StoryRef,
  ownerScopeRel: string,
  groupIndex: Map<string, GroupIndexEntry>,
  storyIdIndex: StoryIdIndex,
  byId?: StatusIndex,
): BlockedDep[] {
  const deps = story.dependsOn;
  if (deps == null || deps.length === 0) return [];

  const ownerGroup = groupIndex.get(ownerScopeRel);
  const blocked: BlockedDep[] = [];

  // A dep that resolved to a real story file blocks unless it's `done` or is
  // this same story — a story can't block itself, in ANY ref form (bare stem,
  // story_id, or `../path`). The single self-ref + status gate for all tiers.
  const evaluateResolved = (targetFile: GroupIndexFile, rawRef: string): void => {
    if (targetFile.filePath === story.filePath) return;
    const status = normalizeStatus(targetFile.status, byId);
    // SW-GT-3: a dependency is "met" when its status resolves to a `done`-role
    // state (role-keyed, not the literal id `"done"`) — so readiness (and the
    // board's per-card ready badge) survives a team's custom terminal state.
    if (!isDoneStatus(status, byId)) blocked.push({ ref: rawRef, reason: "unmet", status });
  };

  for (const rawRef of deps) {
    const ref = stripDotSlash(rawRef);

    // Bare ref (no path separator): sibling stem first, then global story_id.
    if (!ref.includes("/")) {
      // Skip a bare-stem self-reference up front — catches the ambiguous-stem
      // case where sibling resolution returns null (so `evaluateResolved`'s
      // identity check can't fire). The story_id and `../path` self-ref forms
      // are caught post-resolution by that identity check.
      if (stripPipelineExtension(story.fileName) === stripPipelineExtension(ref)) continue;

      // 1. Exact filename → stem sibling lookup in the owner group.
      const fileName = ownerGroup ? resolveFileInGroup(ownerGroup.files, ref) : null;
      if (fileName) {
        evaluateResolved(ownerGroup!.files.get(fileName)!, rawRef);
        continue;
      }

      // 2. Global story_id (SW-FX-70). Resolves against the full index, so an
      // in-scope and a cross-scope target collapse to the same lookup. Poisoned
      // / absent → undefined → falls through to `unresolved` (AC-4).
      const loc = resolveStoryIdRef(storyIdIndex, ref);
      const targetFile = loc ? groupIndex.get(loc.scopeRel)?.files.get(loc.fileName) : undefined;
      if (!targetFile) {
        blocked.push({ ref: rawRef, reason: "unresolved" });
        continue;
      }
      evaluateResolved(targetFile, rawRef);
      continue;
    }

    // Cross-epic: ref contains "/"
    const lastSlash = ref.lastIndexOf("/");
    const dirPortion = ref.substring(0, lastSlash);
    const filePortion = ref.substring(lastSlash + 1);
    if (!filePortion) {
      blocked.push({ ref: rawRef, reason: "unresolved" });
      continue;
    }

    const targetScopeRel = resolveScopeRelativePath(ownerScopeRel, dirPortion);
    if (targetScopeRel === null) {
      blocked.push({ ref: rawRef, reason: "unresolved" });
      continue;
    }

    const targetGroup = groupIndex.get(targetScopeRel);
    if (!targetGroup) {
      blocked.push({ ref: rawRef, reason: "unresolved" });
      continue;
    }

    const targetFileName = resolveFileInGroup(targetGroup.files, filePortion);
    if (!targetFileName) {
      blocked.push({ ref: rawRef, reason: "unresolved" });
      continue;
    }

    evaluateResolved(targetGroup.files.get(targetFileName)!, rawRef);
  }

  return blocked;
}

/**
 * The app-facing readiness query: `ready` plus the blocking deps that explain a
 * `false`. Requires a `FullGroupIndex` (constructible only via
 * `buildFullGroupIndex`), so a scoped/drilled index is a **compile-time error**
 * here — readiness can never silently regress to drill-dependence (SW-FX-1
 * AC-6). App code reaches this through `PipelineGraph.readiness`.
 */
export function resolveReadiness(
  story: StoryRef,
  ownerScopeRel: string,
  groupIndex: FullGroupIndex,
  storyIdIndex: StoryIdIndex,
  byId?: StatusIndex,
): ReadinessResult {
  const blockedBy = collectUnmetDeps(story, ownerScopeRel, groupIndex, storyIdIndex, byId);
  const ready =
    isReadyEligibleStatus(normalizeStatus(story.status, byId), byId) && blockedBy.length === 0;
  return { ready, blockedBy };
}

/**
 * How many distinct stories `story` depends on: its `dependsOn` refs without repeats or a
 * reference to itself, so a ready story's count is the dependencies readiness checked.
 */
export function countDependencies(story: StoryRef): number {
  const self = new Set([stripPipelineExtension(story.fileName), story.storyId ?? ""]);
  const refs = new Set(
    (story.dependsOn ?? [])
      .map((r) => stripPipelineExtension(stripDotSlash(r)))
      .filter((r) => r && !self.has(r)),
  );
  return refs.size;
}

/**
 * What a ready story's rail and row say: ready to start, and how many dependencies that
 * rests on. One wording so every surface that names readiness agrees.
 */
export function readyTitle(dependencyCount: number): string {
  if (dependencyCount <= 0) return "Ready to start · no dependencies";
  if (dependencyCount === 1) return "Ready to start · its dependency is done";
  return `Ready to start · all ${dependencyCount} dependencies done`;
}

/** What a not-started story held back by dependencies says: the ones it waits on. */
export function waitingTitle(blockedBy: readonly BlockedDep[]): string {
  return `Waiting on ${blockedBy.map(formatBlockedDep).join(", ")}`;
}

/** Human-readable label for a blocking dep, e.g. `"../seamless-sync/SS-3 (in-progress)"`. */
export function formatBlockedDep(dep: BlockedDep): string {
  return dep.reason === "unmet" ? `${dep.ref} (${dep.status})` : `${dep.ref} (unresolved)`;
}
