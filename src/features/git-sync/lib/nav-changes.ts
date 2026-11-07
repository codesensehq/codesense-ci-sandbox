// SPDX-License-Identifier: MIT
import type { ContextConflict } from "@/tauri/commands";

import { joinOrgPath } from "./group-changes-by-node";
import type { PublishFile, PublishSet } from "./types";

/** How a changed file's name is tinted: green new, blue modified (and renamed), dusty-rose
 *  struck deleted, violet conflict. */
export type NavChangeKind = "added" | "modified" | "renamed" | "deleted" | "conflict";

/** One file the context repo has that GitHub doesn't (or that conflicts with GitHub). */
export interface NavChange {
  /** Absolute path (what the nav and the tabs key on). */
  absPath: string;
  /** Org-root-relative path (what the backend keys on). */
  relPath: string;
  kind: NavChangeKind;
  /** Renamed: the old org-root-relative path (the redline reads "before" from it). */
  renamedFrom?: string;
  /** Line counts vs the baseline; absent for a binary or synthesized change. */
  added?: number;
  removed?: number;
}

function kindOf(f: PublishFile): NavChangeKind {
  if (f.status === "deleted") return "deleted";
  if (f.renamedFrom) return "renamed";
  return f.status;
}

/** Every changed file of a (team-scoped) publish set plus the conflicts, one entry per path.
 *  A conflicted path reads as a conflict whatever else the set says about it. */
export function toNavChanges(
  set: PublishSet,
  orgRoot: string,
  conflicts: readonly ContextConflict[] = [],
): NavChange[] {
  const out = new Map<string, NavChange>();
  for (const f of [...set.buckets.flatMap((b) => b.files), ...set.general.files]) {
    out.set(f.path, {
      absPath: joinOrgPath(orgRoot, f.path),
      relPath: f.path,
      kind: kindOf(f),
      renamedFrom: f.renamedFrom,
      added: f.added,
      removed: f.removed,
    });
  }
  for (const c of conflicts) {
    const prior = out.get(c.path);
    out.set(c.path, {
      ...prior,
      absPath: joinOrgPath(orgRoot, c.path),
      relPath: c.path,
      kind: "conflict",
    });
  }
  return [...out.values()];
}

function sameChange(a: NavChange, b: NavChange): boolean {
  return (
    a.absPath === b.absPath &&
    a.relPath === b.relPath &&
    a.kind === b.kind &&
    a.renamedFrom === b.renamedFrom &&
    a.added === b.added &&
    a.removed === b.removed
  );
}

/**
 * A re-read's changes laid over the last ones without moving anything: a file still changed
 * keeps its place (and its object, when nothing about it moved, so its row doesn't
 * re-render), newly changed files follow, and the last array comes back as-is when the
 * answer is the same.
 */
export function keepUnchangedChanges(prev: readonly NavChange[], next: NavChange[]): NavChange[] {
  if (prev.length === 0) return next;
  const nextByPath = new Map(next.map((c) => [c.absPath, c]));
  const out: NavChange[] = [];
  let same = prev.length === next.length;
  for (const old of prev) {
    const now = nextByPath.get(old.absPath);
    if (!now) {
      same = false;
      continue;
    }
    nextByPath.delete(old.absPath);
    if (sameChange(old, now)) out.push(old);
    else {
      same = false;
      out.push(now);
    }
  }
  for (const added of nextByPath.values()) {
    same = false;
    out.push(added);
  }
  return same ? (prev as NavChange[]) : out;
}

/** Words for a change kind, as the accessible name and the card say it. */
export const CHANGE_KIND_WORD: Record<NavChangeKind, string> = {
  added: "Added",
  modified: "Modified",
  renamed: "Renamed",
  deleted: "Deleted",
  conflict: "Conflict",
};

/** What the agent-context overlay knows of a nav path (a subset of the nav's agent context). */
export interface ChangeScope {
  relOf: (absPath: string) => string | null;
  membership: (rel: string) => { state: "in" | "partial" | "out"; tier?: string };
}

/** The changes inside an agent's context, and how many fall outside it. No scope (Team) keeps
 *  every change. */
export function scopeChanges(
  changes: readonly NavChange[],
  scope: ChangeScope | undefined,
  cloneRoot?: string | null,
): { inScope: NavChange[]; outside: number } {
  if (!scope) return { inScope: [...changes], outside: 0 };
  const inScope = changes.filter((c) => {
    const rel = scope.relOf(cloneRoot ? joinOrgPath(cloneRoot, c.relPath) : c.absPath);
    return rel !== null && scope.membership(rel).state !== "out";
  });
  return { inScope, outside: changes.length - inScope.length };
}
