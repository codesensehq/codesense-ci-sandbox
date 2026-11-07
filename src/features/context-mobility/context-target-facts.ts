// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import type { Context } from "@/shared/lib/context-model";
import type { Org } from "@/shared/types/domain";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { useOrgStore } from "@/shared/stores/org-store";
import { logger } from "@/shared/lib/logger";
import {
  fileListErrorMessage,
  isNotIndexedError,
  onContextIndexGeneration,
  requestContentFileIndex,
} from "@/shared/lib/pipeline-graph/content-scan-queue";
import { toastError } from "@/shared/lib/with-error-toast";

/**
 * SLW-PT-B3 (companion) — supply the §P collision gate its target facts.
 *
 * `ContextMoveController` needs two SYNCHRONOUS predicates about the chosen move
 * TARGET: does an item already live there under this logical name
 * (`targetCollision`), and does a candidate keep-both name collide there
 * (`targetHas`). Without them the collision "keep both" step + keep-both rename
 * can never fire and the picker's chosen target silently reject-on-collides in
 * the backend with no user-facing chooser. The host passed them as `undefined`.
 *
 * The move/copy backend keys on the SAME `content/` root the nav scans and the
 * (now-fixed) team-all endpoint resolves — `path_to_team_content_dir` (mirrored
 * synchronously by {@link pathToTeamContentDirSync}). So a TEAM target's content
 * root is pure path math off the org's `org_root`; we list it once and read the
 * predicates from the cached set. A Personal target has no synchronous FE
 * content-root helper, so it degrades to "no collision known" (the prior
 * behavior) rather than block — never a regression.
 */

/** Resolve a move-TARGET {@link Context} to its absolute `content/` root, using
 *  the loaded orgs. Team targets (`team-all`/`team-repo`) resolve to the team's
 *  content dir — the exact dir the backend keys on. Personal + view-only return
 *  `null` (no synchronous content root available here). Pure. */
export function resolveTeamTargetContentRoot(target: Context, orgs: readonly Org[]): string | null {
  if (target.kind !== "team-all" && target.kind !== "team-repo") return null;
  if (!target.teamName) return null;
  // The org id is carried by the context id (`team:<orgSlug>:…`); find the loaded
  // org by matching a team of that name whose org owns it. The picker only offers
  // same-org targets, so resolving by team name within the active orgs is exact.
  const orgSlug = orgSlugOf(target.id);
  const org = orgs.find((o) => o.slug === orgSlug);
  if (!org?.org_root) return null;
  // Both team-all and team-repo team content lives under the ONE team content
  // dir (the per-repo scope is a subtree filter, not a distinct root) — the same
  // dir the nav scans and the backend keys on.
  return pathToTeamContentDirSync(org.org_root, target.teamName);
}

/** The `<orgSlug>` field of a `team:`/`team-repo:` context id, else `null`. */
function orgSlugOf(id: string): string | null {
  const rest = id.startsWith("team-repo:")
    ? id.slice("team-repo:".length)
    : id.startsWith("team:")
      ? id.slice("team:".length)
      : null;
  if (rest === null) return null;
  const idx = rest.indexOf(":");
  if (idx <= 0) return null;
  return rest.slice(0, idx);
}

/** The predicates {@link resolveTeamTargetContentRoot}'s listing feeds into the
 *  controller. Both default to `false` (no known collision) for a target whose
 *  content root can't be resolved synchronously. */
export interface TargetFacts {
  /** Whether an item already lives at `target` under `logicalName`. */
  targetCollision: (target: Context, logicalName: string) => boolean;
  /** Whether a candidate keep-both name already lives at `target`. */
  targetHas: (target: Context, candidate: string) => boolean;
  /** List every distinct target root now (once each). Called when a move/copy picker is
   *  about to open, so the facts are there before a target is chosen. */
  prefetch: () => void;
}

/**
 * SV-PBF-267: read the (team) target content listings LAZILY, so the controller's
 * synchronous collision predicates read live facts without every mounted team view
 * listing every team on mount. Nothing is listed until a move/copy picker is about to
 * open ({@link TargetFacts.prefetch}) or a predicate is asked about a target; then
 * each distinct content root is listed once, through the shared per-root
 * cache (`requestContentFileIndex`). Roots are deduped: a team with N source repos
 * has ONE content root. When the index publishes a newer generation, the facts are
 * dropped and re-listed on the next ask — an fs bump alone never lists anything.
 * Personal/unresolvable targets read as no-collision. A root the index refuses is
 * reported to the user (toast), never read as an empty list.
 */
export function useTargetFacts(targets: readonly Context[], active = true): TargetFacts {
  const orgsRef = useOrgsSnapshot();
  // rootPath -> Set<relativePath>. A ref so the predicates are stable + sync.
  const listingsRef = useRef<Map<string, Set<string>>>(new Map());
  // Roots asked for at this generation (listed, or listing).
  const requestedRef = useRef<Set<string>>(new Set());
  const liveRef = useRef(true);
  const [, force] = useState(0);

  useEffect(() => {
    liveRef.current = true;
    const stop = onContextIndexGeneration(() => {
      // Facts at the old generation are stale. Only re-render if something was asked,
      // so an idle team view does no work on an index change.
      if (requestedRef.current.size === 0) return;
      listingsRef.current = new Map();
      requestedRef.current = new Set();
      force((n) => n + 1);
    });
    return () => {
      liveRef.current = false;
      stop();
    };
  }, []);

  const ensure = useCallback(
    (root: string) => {
      if (!active || requestedRef.current.has(root)) return;
      requestedRef.current.add(root);
      const requested = requestedRef.current;
      void requestContentFileIndex(root).then(
        (files) => {
          if (!liveRef.current || requestedRef.current !== requested) return;
          listingsRef.current.set(root, new Set(files.map((f) => f.relativePath)));
          force((n) => n + 1);
        },
        (error: unknown) => {
          if (!liveRef.current || requestedRef.current !== requested) return;
          listingsRef.current.set(root, new Set());
          if (isNotIndexedError(error))
            toastError("Couldn't check for name clashes: ", new Error(fileListErrorMessage(error)));
          else logger.warn("context-mobility", `target listing failed for ${root}: ${error}`);
        },
      );
    },
    [active],
  );

  const has = useCallback(
    (target: Context, name: string): boolean => {
      const root = resolveTeamTargetContentRoot(target, orgsRef);
      if (!root) return false;
      ensure(root);
      return listingsRef.current.get(root)?.has(name) ?? false;
    },
    [orgsRef, ensure],
  );

  const prefetch = useCallback(() => {
    for (const root of new Set(targets.map((t) => resolveTeamTargetContentRoot(t, orgsRef))))
      if (root) ensure(root);
  }, [targets, orgsRef, ensure]);

  return { targetCollision: has, targetHas: has, prefetch };
}

function useOrgsSnapshot(): readonly Org[] {
  return useOrgStore((s) => s.orgs);
}
