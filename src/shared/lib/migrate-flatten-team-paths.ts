// SPDX-License-Identifier: MIT
/**
 * SLW-IN-8 persisted-viewer-state migration: repoint absolute paths that still
 * embed the retired `_team/{team}/` nesting to the flat `{team}/` layout
 * SLW-IN-7 moved content to on disk.
 *
 * SLW-IN-7 flattened `{org_root}/_team/{team}/… → {org_root}/{team}/…` and
 * repointed the on-disk dirs, install lockfiles, and projection symlinks — but
 * NOT the persisted viewer blob. So `Workspace.storyFilePath` / `epicDocPath`,
 * open-tab paths, drill paths, and nav history still point at the vanished
 * `_team/…` locations, breaking story→workspace reconnection (AC-4), the
 * chrome-title→doc gesture, and epic tray-grouping (AC-4b).
 *
 * The obvious reuse target — `rewriteTeamContentPaths` (viewer-store) — does NOT
 * fit: it skips the `workspaces` slice entirely (never touching
 * `storyFilePath`/`epicDocPath`) and is rename-shaped (one `oldName`→`newName`),
 * whereas a flatten drops the container for EVERY team at once. Instead this is
 * the structural twin of `deepRewriteOrgSlugPaths`: a whole-blob recursive
 * rewrite (shared [`deepRewriteStrings`] walk) that covers every path-bearing
 * string in one pass.
 *
 * Double-anchored on the `_team/` literal AND the following layer marker, so it
 * is a structural no-op on already-flat paths (no live-tree "flatten detection"
 * probe needed) and idempotent over workspaces a user already self-healed by
 * close/reopen.
 */

import { deepRewriteStrings } from "./deep-rewrite-strings";

/**
 * Match `/_team/{team}/{marker}` inside any absolute path and drop the `_team/`
 * container segment. Anchored on BOTH the `_team/` literal and the following
 * layer-marker (`content|install|repos|libraries`) so a coincidental
 * `/_team/{something-else}` never matches. The team slug obeys the backend
 * `is_slug_safe` shape (`^[a-z0-9][a-z0-9-]*$`).
 *
 * The trailing `/`-or-end boundary is a NON-consuming lookahead (`(?=\/|$)`),
 * exactly mirroring the twin `ORGS_PATH_LEGACY_RE`. Consuming it would swallow
 * the delimiter a second, immediately-adjacent `/_team/…` match needs, so the
 * global rewrite could skip the second of two boundary-sharing occurrences.
 * The lookahead makes the global pass rewrite every occurrence in a string.
 */
const FLATTEN_TEAM_PATH_RE =
  /\/_team\/([a-z0-9][a-z0-9-]*)\/(content|install|repos|libraries)(?=\/|$)/g;

/**
 * SLW-IN-8: in-place recursive rewrite of every string under `node`, dropping
 * the retired `_team/` container from `/_team/{team}/{marker}` path segments.
 * Structural twin of `deepRewriteOrgSlugPaths`; invoked in `loadPersistedState`
 * gated on `schemaVersion < 10`.
 */
export function deepRewriteFlattenTeamPaths(node: unknown): void {
  deepRewriteStrings(node, (value) => value.replace(FLATTEN_TEAM_PATH_RE, "/$1/$2"));
}
