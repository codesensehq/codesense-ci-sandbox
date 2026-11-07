// SPDX-License-Identifier: MIT
// RF-perf-12: session-scoped view-position snapshots (scroll + editor cursor).
//
// RF-perf-10 unmounts backgrounded teams / evicts cold file viewers to reclaim
// renderer memory. A plain React unmount destroys component-local view state
// (nav scroll, markdown scroll, CodeMirror cursor/scroll) that was never lifted
// out of component lifetime, so a team switch drops the user back at scroll-top
// with a reset cursor. This module defines the tiny, PLAIN-DATA snapshot shape
// and the composed keys the viewer-store `viewStateByKey` slice is keyed by.
//
// AC-8 (memory-neutral): a snapshot is plain scalar values only — never a CodeMirror
// `EditorState`, doc `Text`, or undo history — so persisting it across an
// unmount CANNOT re-pin the megabytes RF-perf-10 unmounts to free.

/** Separator between the team key and the per-view suffix in a composed key.
 *  A team key is `${orgSlug}:${team}` (one colon); the doubled `::` is an
 *  unambiguous boundary so a prefix prune can't collide sibling teams
 *  (`a:b::` never prefixes `a:bc::…`). */
export const VIEW_KEY_SEP = "::";

/** RF-perf-12: a lightweight, serializable view-position snapshot. All fields
 *  optional so nav (scroll only) and editors (scroll + selection) share one
 *  shape. Deliberately holds NO document text / EditorState / history (AC-8) —
 *  plain scalar values only. */
export interface ViewSnapshot {
  /** HTML document route, retained across file-viewer eviction. */
  hash?: string;
  /** Scroll container `scrollTop` (nav tree, markdown prose, or editor scrollDOM). */
  scrollTop?: number;
  /** Editor cursor/selection as document offsets (anchor..head). */
  selection?: { anchor: number; head: number };
}

/** Composed key for a team's nav/tree scroll (one per team). */
export function navViewKey(teamKey: string): string {
  return `${teamKey}${VIEW_KEY_SEP}nav`;
}

/** Composed key for a file's markdown/editor view state (one per open file). */
export function fileViewKey(teamKey: string, filePath: string): string {
  return `${teamKey}${VIEW_KEY_SEP}${filePath}`;
}

/**
 * RF-perf-12: drop every snapshot belonging to a team — its `${teamKey}::nav`
 * key and every `${teamKey}::${filePath}` key — in one pass. Reference-stable:
 * returns the SAME map instance when the team had no snapshots, mirroring
 * `pruneSessionMap` / `removeTeamKeyed` in the viewer store so a `closeTeam` for
 * an unrelated team never churns slice identity (no needless re-renders). This
 * is the GC that keeps `viewStateByKey` bounded across a long session (AC-5).
 */
export function pruneViewStateForTeam<V>(
  map: Record<string, V>,
  teamKey: string,
): Record<string, V> {
  const prefix = `${teamKey}${VIEW_KEY_SEP}`;
  let next: Record<string, V> | null = null;
  for (const key of Object.keys(map)) {
    if (key.startsWith(prefix)) {
      next ??= { ...map };
      delete next[key];
    }
  }
  return next ?? map;
}
