// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { getProjectionExcludes, type ProjectionExcludesView } from "@/tauri/commands";

/**
 * SW-CP-28: load a team's projection-exclude view (the in-code default globs +
 * the team's raw override patterns) ONCE per (orgRoot, team). Mirrors
 * `useMergeMarks`' load-once posture: fail-open (no active team / a failed read →
 * empty view), never blocks first paint. `nonce` forces a reload after a save so
 * the panel reflects what was persisted.
 *
 * The defaults come from THIS backend read (never an FE literal) so a change to
 * `SECRET_GLOBS` surfaces in the UI automatically (AC-7).
 */
const EMPTY_VIEW: ProjectionExcludesView = { defaults: [], patterns: [] };

export interface UseProjectionExcludes {
  /** The in-code default globs, rendered as toggles. */
  defaults: string[];
  /** The team's stored override patterns (adds + `!`-negations). */
  patterns: string[];
  /** True until the first backend read resolves. */
  loading: boolean;
}

export function useProjectionExcludes(
  orgRoot: string,
  team: string | null,
  nonce: number,
): UseProjectionExcludes {
  const [view, setView] = useState<ProjectionExcludesView>(EMPTY_VIEW);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    // Funnel BOTH the no-team case and the real read through a promise so the state
    // writes happen in `.then`/`.catch` (async), never synchronously in the effect
    // body — and a reload (nonce bump) keeps the prior data on screen with no
    // "Loading…" flash. `loading` starts true and flips once, on first resolve.
    const load =
      orgRoot && team ? getProjectionExcludes(orgRoot, team) : Promise.resolve(EMPTY_VIEW);
    load
      .then((v) => {
        if (!cancelled) {
          setView(v);
          setLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setView(EMPTY_VIEW);
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [orgRoot, team, nonce]);

  return { defaults: view.defaults, patterns: view.patterns, loading };
}
