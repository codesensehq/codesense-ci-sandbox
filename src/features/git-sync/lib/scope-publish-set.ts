// SPDX-License-Identifier: MIT
/**
 * Repo-relative content scoping shared by clone and private sources. The backend narrows
 * its diff to the same prefix; this filter also scopes fixture and cached publish sets.
 */

import { useMemo } from "react";

import { useContentSource } from "@/shared/hooks/use-content-source";

import type { PublishFile, PublishSet } from "./types";
import { useOrgRoot } from "./use-org-root";

/**
 * A content prefix relative to its git root (e.g.
 * `_team/default/content`), derived robustly from the resolved paths — never a
 * hardcoded layout. Returns `null` when it can't be derived (unhydrated repo:
 * `teamContentPath` is `""` or not a child of `orgRoot`); callers treat `null`
 * as "don't scope yet" so a transient hydration gap never blanks the UI.
 */
export function contentPrefix(
  orgRoot: string | null,
  teamContentPath: string | null | undefined,
): string | null {
  if (!orgRoot || !teamContentPath) return null;
  const root = orgRoot.replace(/\/+$/, "");
  const content = teamContentPath.replace(/\/+$/, "");
  if (content === root || !content.startsWith(`${root}/`)) return null;
  return content.slice(root.length + 1);
}

/** True when an org-root-relative path lives under the content prefix. */
function isUnderPrefix(path: string, prefix: string): boolean {
  return path === prefix || path.startsWith(`${prefix}/`);
}

/**
 * Filter a publish set to only the files under `prefix`, across BOTH the
 * `general` bucket and every workspace bucket, recomputing each `count` and
 * pruning emptied buckets. A `null` prefix returns the set UNCHANGED (the
 * hydration passthrough) — consumers gate fetching on a resolved root, so this
 * never surfaces as an over-count in practice.
 */
export function scopePublishSet(set: PublishSet, prefix: string | null): PublishSet {
  if (!prefix) return set;
  const keep = (f: PublishFile) => isUnderPrefix(f.path, prefix);
  const buckets = set.buckets
    .map((b) => {
      const files = b.files.filter(keep);
      return { ...b, files, count: files.length };
    })
    .filter((b) => b.files.length > 0);
  const generalFiles = set.general.files.filter(keep);
  return { buckets, general: { files: generalFiles, count: generalFiles.length } };
}

/**
 * Strip the content prefix from an org-root-relative path for DISPLAY only
 * (the open path stays absolute). A `null` prefix or a non-prefixed path is
 * returned unchanged, so Documentation / other-shaped paths aren't mangled.
 */
export function stripContentPrefix(path: string, prefix: string | null): string {
  if (!prefix || !isUnderPrefix(path, prefix) || path === prefix) return path;
  return path.slice(prefix.length + 1);
}

/**
 * Resolve the active team's content prefix for the repo backing `navRepoPath`
 * (org_root via {@link useOrgRoot} + `teamContentPath` from the resolved repo).
 * Single shared derivation for every consumer.
 */
export function useTeamContentPrefix(navRepoPath: string): string | null {
  const orgRoot = useOrgRoot(navRepoPath);
  const teamContentPath = useContentSource(navRepoPath)?.teamContentPath;
  return useMemo(() => contentPrefix(orgRoot, teamContentPath), [orgRoot, teamContentPath]);
}
