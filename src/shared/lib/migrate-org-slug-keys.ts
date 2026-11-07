// SPDX-License-Identifier: MIT
/**
 * SS-FU-12 persisted-state migration: rewrite the legacy collapsed org slug
 * `owner--repo` to the nested `owner/repo` form wherever it is embedded as a
 * key (or key prefix) in persisted UI state.
 *
 * The org slug appears in two shapes across the two app-state stores:
 *
 * - **viewer-store** — as the org-slug prefix of every `teamKey`
 *   (`${orgSlug}:${team}`) that keys the ~17 team-keyed slices.
 * - **org-store** — as the bare `activeOrgSlug` scalar and as the keys of
 *   `lastTeamByOrgSlug`.
 *
 * Both rewrites are narrow and idempotent: a key already in `owner/repo` form
 * (it contains `/`) or with no `--` is returned unchanged, so re-running the
 * migration — or running it against a half-migrated blob — is safe. The viewer
 * caller gates on `schemaVersion < 6` so the pass runs once in the common
 * case; org-store has no schema counter and relies on the narrow match alone.
 *
 * v5 originally only rewrote the *outer* `teamKey` of each persisted slice
 * (and the org-store fields), missing absolute paths inside slice values that
 * happened to embed `/orgs/{owner}--{repo}/...`. v6 adds
 * [`deepRewriteOrgSlugPaths`], a recursive string rewrite that catches those.
 * The schema bump 5→6 forces every existing v5 user to re-run the migration so
 * stale paths in `statusNavExpandedPaths`, `workspaces`, `openedTeams`, etc.
 * are healed automatically.
 */

import { deepRewriteStrings } from "./deep-rewrite-strings";

/**
 * Persisted viewer-store slices whose outer key is a `teamKey`
 * (`${orgSlug}:${team}`). Source of truth is the serializer in
 * `viewer-store.ts` (`existing.viewer = { ... }`) — these are exactly the
 * `Record`-shaped, team-keyed properties it writes, plus legacy pre-v15 chrome
 * maps that must be rewritten before the global cliChrome seed. Runtime-only team-keyed
 * slices (e.g. `navHistory`) are intentionally absent: they
 * are never persisted, so there is nothing to rewrite.
 */
export const TEAM_KEYED_PERSISTED_SLICES = [
  "openTabs",
  "activeTab",
  "drillPaths",
  "rightPaneSplitRatio",
  "statusNavExpandedPaths",
  "pipelineScope",
  "workspaces",
  "activeWorkspace",
  "softClosedWorkspaces",
  // Legacy pre-v15 chrome maps, read only by the one-time global chrome seed.
  "terminalVisible",
  "layoutMode",
  "lastTerminalMode",
  "terminalSplitRatio",
  "previousSplitRatio",
  "workspacePanelCollapsed",
  "workspacePanelWidth",
] as const;

/**
 * Rewrite a legacy `owner--repo` org slug to `owner/repo`, splitting on the
 * FIRST `--` (a GitHub login can't contain `--`, so the first occurrence is
 * always the owner/repo boundary). Returns the input unchanged when it is
 * already nested (`/`) or has no usable `--` boundary.
 */
export function rewriteLegacyOrgSlug(slug: string): string {
  if (slug.includes("/")) return slug;
  const i = slug.indexOf("--");
  // `i <= 0` rejects an empty owner (`--repo`); `i + 2 >= length` rejects an
  // empty repo (`owner--`). Neither is a real org slug.
  if (i <= 0 || i + 2 >= slug.length) return slug;
  return `${slug.slice(0, i)}/${slug.slice(i + 2)}`;
}

/**
 * Rewrite the org-slug prefix of a `teamKey` (`owner--repo:team` →
 * `owner/repo:team`). The team suffix and any non-teamKey string pass through
 * unchanged.
 */
export function rewriteLegacyTeamKey(key: string): string {
  const colon = key.indexOf(":");
  const orgSlug = colon < 0 ? key : key.slice(0, colon);
  const rewritten = rewriteLegacyOrgSlug(orgSlug);
  if (rewritten === orgSlug) return key;
  return colon < 0 ? rewritten : `${rewritten}${key.slice(colon)}`;
}

/** Return a copy of `record` with each key passed through `rewriteKey`. */
function rewriteRecordKeys<T>(
  record: Record<string, T>,
  rewriteKey: (key: string) => string,
): Record<string, T> {
  const out: Record<string, T> = {};
  for (const [key, value] of Object.entries(record)) {
    out[rewriteKey(key)] = value;
  }
  return out;
}

/**
 * In-place rewrite of a persisted viewer block's `teamKey`-shaped strings:
 * the outer keys of every team-keyed `Record` slice **and** the bare-string
 * elements of `viewer.openedTeams` (the only slice that stores teamKeys as
 * array values rather than as outer keys).
 */
export function migrateViewerOrgSlugKeys(viewer: Record<string, unknown>): void {
  for (const slice of TEAM_KEYED_PERSISTED_SLICES) {
    const container = viewer[slice];
    if (container && typeof container === "object" && !Array.isArray(container)) {
      viewer[slice] = rewriteRecordKeys(container as Record<string, unknown>, rewriteLegacyTeamKey);
    }
  }
  if (Array.isArray(viewer.openedTeams)) {
    // Dedupe after the rewrite: a v6 session that added a live
    // `owner/repo:team` entry while a legacy `owner--repo:team` entry was
    // still in the array ends up with both — and on v7's rewrite they
    // collapse to the SAME teamKey, producing two duplicate panels in the
    // UI. First-occurrence order is preserved.
    const seen = new Set<string>();
    viewer.openedTeams = (viewer.openedTeams as unknown[])
      .map((entry) => (typeof entry === "string" ? rewriteLegacyTeamKey(entry) : entry))
      .filter((entry) => {
        if (typeof entry !== "string") return true;
        if (seen.has(entry)) return false;
        seen.add(entry);
        return true;
      });
  }
}

/**
 * Match `/orgs/{owner}--{repo}` segments inside any absolute path. The owner
 * is lazy and the repo is lazy with a `/` or end-of-string lookahead, so the
 * boundary is always the FIRST `--` even when the repo half itself contains
 * `--` (`/orgs/alderco--my--repo/` rewrites to `/orgs/alderco/my--repo/`).
 */
const ORGS_PATH_LEGACY_RE = /\/orgs\/([^/]+?)--([^/]+?)(?=\/|$)/g;

/**
 * SS-FU-12 v5→v6: in-place recursive rewrite of every string value under
 * `node` whose substring `/orgs/{owner}--{repo}` carries the legacy slug. v5
 * rewrote outer teamKey prefixes but missed the *absolute paths* embedded
 * inside slice values (tab paths, drill paths, status-nav expanded folders,
 * workspace `storyFilePath`s, `openedTeams` metadata), leaving them pointed
 * at the now-renamed org root. Narrow + idempotent: a path already in
 * `/orgs/{owner}/{repo}/` form has no `--` after `/orgs/` and is skipped.
 */
export function deepRewriteOrgSlugPaths(node: unknown): void {
  deepRewriteStrings(node, (value) => value.replace(ORGS_PATH_LEGACY_RE, "/orgs/$1/$2"));
}

/**
 * In-place rewrite of a persisted org block: the `activeOrgSlug` scalar and the
 * keys of `lastTeamByOrgSlug` get the legacy `owner--repo` → `owner/repo`
 * rewrite.
 */
export function migrateOrgStoreOrgSlug(org: Record<string, unknown>): void {
  if (typeof org.activeOrgSlug === "string") {
    org.activeOrgSlug = rewriteLegacyOrgSlug(org.activeOrgSlug);
  }
  const lastTeam = org.lastTeamByOrgSlug;
  if (lastTeam && typeof lastTeam === "object" && !Array.isArray(lastTeam)) {
    org.lastTeamByOrgSlug = rewriteRecordKeys(
      lastTeam as Record<string, unknown>,
      rewriteLegacyOrgSlug,
    );
  }
}
