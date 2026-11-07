// SPDX-License-Identifier: MIT
// SF-8 / SLW-IN-7: TS-side mirror of Rust's `src-tauri/src/context_repo/path.rs`
// layout constants + `path_to_team_content_dir` pure path math. The Rust
// function performs no filesystem touch and no existence check, so an IPC call
// to resolve it is gratuitous — duplicating the formula synchronously here lets
// `buildSyntheticRepo` and the hard-switch hoisting (SF-8 Piece 1) run on the
// main thread without awaiting Tauri. Constants must stay in sync with the Rust
// side; the unit test guards the formula against drift.
//
// SLW-IN-7 flattened teams to the repo root: `{org_root}/{team}/…` (the
// `_team/` wrapper is gone). The parsers anchor on the first layer-marker child
// (`install`/`content`/`repos`/`libraries`) after the org slug.

export const TEAM_CONTENT_DIR = "content";

/** The four layer-marker directory names a team directory directly contains.
 *  Mirror of Rust `LAYER_MARKER_DIRS`. A team is the segment immediately before
 *  the first of these after the org slug. */
export const LAYER_MARKER_DIRS = ["install", "content", "repos", "libraries"] as const;

/** Synchronous mirror of Rust `path_to_team_content_dir(org_root, team)`.
 *  Returns `{org_root}/{team}/content`. */
export function pathToTeamContentDirSync(orgRoot: string, team: string): string {
  return `${orgRoot}/${team}/${TEAM_CONTENT_DIR}`;
}

/** SLW-FX-14: inverse of {@link pathToTeamContentDirSync} for an epic
 *  workspace's persisted `epicDocPath`.
 *
 *  An epic-shell workspace persists its own `epicDocPath` (a `recordField`),
 *  which is a full absolute path of shape `{org_root}/{team}/content/…/epic.md`.
 *  That is the ONLY synchronous cold-open source of the epic's `org_root` +
 *  `team`: both `org-store.orgs` and `org-store.workspaceRepos` are unpersisted
 *  and populate only after the async org-discovery crawl, but `epicDocPath`
 *  rides persisted viewer state and is present at first paint (mirrors how a
 *  STORY workspace recovers its identity from its own persisted
 *  `sourceRepoSlug`).
 *
 *  Returns `{ orgRoot, team, teamContentPath }` when the path's first
 *  layer-marker after the org slug is `content` (with a non-empty `org_root`
 *  prefix and team segment), else `null` — a non-content path (e.g. the
 *  per-source-repo `/{team}/repos/…` context path whose first marker is
 *  `repos`), a marker at index 0/1 (no `org_root`), or empty/nullish input all
 *  degrade to `null` (caller falls back to the genuine no-anchor state). Pure —
 *  no store reads, no filesystem touch. */
export function teamContentPathFromEpicDocPath(
  epicDocPath: string | null | undefined,
): { orgRoot: string; team: string; teamContentPath: string } | null {
  if (!epicDocPath) return null;
  const parts = epicDocPath.split("/");
  // Anchor on the FIRST layer-marker after the org slug; the team is the
  // segment immediately before it. The marker must be `content` (an epic doc
  // lives under `content/`, never `install`/`repos`). Mirrors the Rust flat
  // parsers so it can't drift from `pathToTeamContentDirSync`.
  const markerIdx = parts.findIndex((p) => (LAYER_MARKER_DIRS as readonly string[]).includes(p));
  if (markerIdx < 2 || parts[markerIdx] !== TEAM_CONTENT_DIR) return null;
  const team = parts[markerIdx - 1];
  const orgRoot = parts.slice(0, markerIdx - 1).join("/");
  if (orgRoot === "" || team === "") return null;
  return { orgRoot, team, teamContentPath: pathToTeamContentDirSync(orgRoot, team) };
}
