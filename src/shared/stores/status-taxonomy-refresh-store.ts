// SPDX-License-Identifier: MIT
import { createTeamRefreshStore } from "./create-team-refresh-store";

/**
 * SW-GT-3 · Per-team refresh nonces for the status taxonomy. `use-status-taxonomy`
 * only re-loads on `teamKey`/`orgRoot` change, so an in-session taxonomy edit
 * (GT-6's editor, once it lands) bumps this to force the active team's snapshot to
 * re-hydrate without an app restart (AC-3). GT-3 provides the plumbing; wiring a
 * writer to `bump()` is that writer's story.
 */
export const useStatusTaxonomyRefresh = createTeamRefreshStore();
