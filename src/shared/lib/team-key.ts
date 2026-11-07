// SPDX-License-Identifier: MIT
/**
 * Team-key helpers — the single source of truth for the workspace-keying
 * contract introduced by sub-epic T (team-level workspaces).
 *
 * Pure-string helpers (`buildTeamKey`, `parseTeamKey`) and React hooks
 * (`useActiveTeamKey`) that read from `useOrgStore`.
 *
 * **Must not import from `viewer-store.ts`** — that would create a cycle
 * because viewer-store consumes these helpers. See story T-1 D-9.
 */
import { parseTeamKey } from "@/shared/lib/parse-team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import type { SourceRepoSlug } from "@/shared/types/domain";
import type { Workspace } from "@/shared/stores/viewer-store";
// Safe (no cycle): workspace-kind-capabilities imports nothing from viewer-store.
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";

/** Sentinel teamKey used when no org is active OR when migration cannot
 *  resolve a workspace to a team. UI consumers iterate workspaces by the
 *  active teamKey, so this sentinel is never rendered. */
export const NO_TEAM_SENTINEL = "_no_team:_none";

/** Sentinel root-workspace id returned by `firstRootIdForTeam` when a team
 *  has zero roots. No real workspace ever has this id — UI consumers (T.3)
 *  handle the zero-roots case explicitly before reading the active id. */
export const ROOT_NO_REPOS_SENTINEL = "root:_no_repos";

/** Build the canonical teamKey for a `(orgSlug, teamName)` pair. Null
 *  team maps to the literal `_none` segment so there's only one code
 *  path through downstream `Record<teamKey, …>` lookups. */
export function buildTeamKey(orgSlug: string, teamName: string | null): string {
  return `${orgSlug}:${teamName ?? "_none"}`;
}

export { parseTeamKey };

/** A team as text names it: its name, or `org › team` when another team of `among` (team keys)
 *  has the same name in another org. A key with no team reads as its org. */
export function teamLabelAmong(teamKey: string, among: Iterable<string>): string {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (!teamName) return orgSlug;
  for (const other of among) {
    const o = parseTeamKey(other);
    if (o.teamName === teamName && o.orgSlug !== orgSlug) return `${orgSlug} › ${teamName}`;
  }
  return teamName;
}

/** React hook returning the currently active team's teamKey. Reads
 *  `activeOrgSlug` + `activeTeamName` from `useOrgStore`. Returns the
 *  `NO_TEAM_SENTINEL` when no org is active (so consumers don't need a
 *  separate null branch — they read the sentinel bucket and find nothing,
 *  which is the correct "no workspaces visible" outcome).
 *
 *  Use this in components/hooks that need "the team for store reads
 *  RIGHT NOW." */
export function useActiveTeamKey(): string {
  const orgSlug = useOrgStore((s) => s.activeOrgSlug);
  const teamName = useOrgStore((s) => s.activeTeamName);
  if (orgSlug === null) return NO_TEAM_SENTINEL;
  return buildTeamKey(orgSlug, teamName);
}

/** Non-React variant of {@link useActiveTeamKey} — reads `useOrgStore`
 *  imperatively via `getState()`. Use in callbacks, effects, action
 *  bodies, or anywhere a hook cannot run. Same null-org → sentinel
 *  contract as the hook.
 *
 *  Why this exists: pre-existing code threaded
 *  `buildTeamKey(orgState.activeOrgSlug ?? "", orgState.activeTeamName)`
 *  inline at 20+ sites, producing the malformed bucket `":teamName"`
 *  (or `":_none"`) when `activeOrgSlug === null` instead of the canonical
 *  `NO_TEAM_SENTINEL`. Writes there are unreachable (no store path reads
 *  that key) — silent state loss in the no-team window. This helper is
 *  the canonical fix.
 */
export function getActiveTeamKey(): string {
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  if (activeOrgSlug === null) return NO_TEAM_SENTINEL;
  return buildTeamKey(activeOrgSlug, activeTeamName);
}

/** Reactive: whether a REAL (non-null) team is active. False in BOTH no-team
 *  windows — no org selected (teamKey `NO_TEAM_SENTINEL`) AND org-selected-but-
 *  team-null (teamKey `${orgSlug}:_none`). Team-scoped agent surfaces must not
 *  auto-bind when this is false: there is no team directory to bind an agent to,
 *  and the `${orgSlug}:_none` bucket is not navigable in the UI (SW-AW-6). */
export function useHasActiveTeam(): boolean {
  const orgSlug = useOrgStore((s) => s.activeOrgSlug);
  const teamName = useOrgStore((s) => s.activeTeamName);
  return orgSlug !== null && teamName !== null;
}

/** Non-React predicate: does `teamKey` denote a real team (vs either no-team
 *  form)? Both `NO_TEAM_SENTINEL` and `${orgSlug}:_none` parse to a null
 *  teamName. Connect closures gate on this so neither no-team window spawns an
 *  agent under a `_none` bucket. */
export function teamKeyHasTeam(teamKey: string): boolean {
  return parseTeamKey(teamKey).teamName !== null;
}

// ---------------------------------------------------------------------------
// T.2 helpers — root-workspace IDs are per-source-repo, not a constant.
// ---------------------------------------------------------------------------

/** Build a stable per-source-repo root workspace ID.
 *
 *  Accepts either the structured `SourceRepoSlug` (`{ owner, repo }`) or a
 *  pre-joined `"owner/repo"` string — both yield `"root:owner/repo"`. The
 *  id is stable across local-clone attachment changes (slug doesn't change
 *  when the user attaches/detaches a clone) so persisted terminal slices
 *  keyed by this id survive attachment churn. */
export function buildRootWorkspaceId(slug: SourceRepoSlug | string): string {
  if (typeof slug === "string") return `root:${slug}`;
  return `root:${slug.owner}/${slug.repo}`;
}

/** Round-trip a root workspace id back to its `"owner/repo"` slug.
 *
 *  Returns `null` for any string that isn't a real T.2 root id:
 *  - missing the `root:` prefix (story ids, arbitrary strings)
 *  - the legacy `"root"` literal (no slug suffix)
 *  - the `ROOT_NO_REPOS_SENTINEL` (no real slug) */
export function parseRootWorkspaceId(id: string): { slug: string } | null {
  if (!id.startsWith("root:")) return null;
  const slug = id.slice("root:".length);
  // Reject the no-repos sentinel and empty slugs.
  if (slug.length === 0 || slug === "_no_repos") return null;
  return { slug };
}

/** Predicate companion to `parseRootWorkspaceId`. Production code prefers
 *  `workspace.kind === "root"` — use this only in pure-string contexts
 *  (e.g. shortcuts/registry where only an id is available). */
export function isRootWorkspaceId(id: string): boolean {
  return parseRootWorkspaceId(id) !== null;
}

/** Active-workspace fallback: returns the id of the FIRST terminal-bearing
 *  workspace in the team's list (array order) — an `integration` or `story`
 *  workspace (or a transitional `root` during the v8→v9 migration window).
 *  Epic NODES are skipped (they hold no PTY surface).
 *
 *  SW-EW-1c: root workspaces are retired, so the old "first root" fallback
 *  becomes "first terminal workspace." Name kept to avoid churning the many
 *  call sites; the `ROOT_` sentinel name is likewise retained.
 *
 *  Returns `ROOT_NO_REPOS_SENTINEL` when the team has no terminal workspace
 *  (cold start / everything closed). UI consumers must handle that empty case
 *  before reading the active id; the sentinel is never a real workspace.
 *
 *  Structural type signature — takes the workspace map directly instead of
 *  `ViewerState` to avoid an import cycle with viewer-store (T.1 D-9). The
 *  `Workspace` type import is type-only and erased at runtime. */
export function firstRootIdForTeam(
  teamKey: string,
  workspacesByTeam: Record<string, Workspace[]>,
): string {
  const list = workspacesByTeam[teamKey];
  if (!list) return ROOT_NO_REPOS_SENTINEL;
  for (const ws of list) {
    if (getKindCapabilities(ws.kind).foregroundTile) return ws.id;
  }
  return ROOT_NO_REPOS_SENTINEL;
}

/** The workspace selection should land on after `closedId`'s tab is closed:
 *  the tab to its **right** (the next tab), falling to the **left** when the
 *  closed tab was the rightmost — the browser/editor convention. Right-bias is
 *  spatially stable: everything right of the closed tab shifts left into the
 *  vacated slot, so the right neighbor is the tab that now occupies the closed
 *  tab's screen position — the selection highlight stays put. Because an epic's
 *  children are contiguous, a middle-of-an-epic close lands on the next sibling,
 *  and closing an epic's last tab lands on the next epic's first tab (or, when it
 *  was the rightmost tab in the whole strip, the previous epic's tab).
 *
 *  `isEligible` gates which neighbors are selectable: the caller passes
 *  membership in the surviving (post-close) live workspace set, so parked
 *  (minimized/soft-closed) tiles are skipped — a close never resurrects one —
 *  AND a stale id in the published order (a workspace already gone via another
 *  path before the tray re-published) can never be chosen, so the returned id
 *  always backs a live workspace (never a dangling `activeWorkspace`). Falls
 *  back to `fallbackId` (the caller's first-root, derived from the live list)
 *  when no eligible neighbor remains anywhere, or the order isn't published yet
 *  — preserving the pre-SW-CW-4 first-root behavior for headless/test paths.
 *  Pure; the same right-biased clamped-neighbor rule as `right-pane-layout.ts`
 *  `reassignActive`, so tray-close and pane-close selection stay consistent. */
export function adjacentWorkspaceIdAfterClose(
  fullOrder: readonly string[] | undefined,
  closedId: string,
  isEligible: (id: string) => boolean,
  fallbackId: string,
): string {
  if (!fullOrder) return fallbackId;
  // Keep the closed (active) tab in the slice so its index is well-defined, then
  // pick the right neighbor from the eligible (live, non-parked) survivors —
  // after removal the id at `idx` is the old right neighbor; clamp to the last
  // survivor when the closed tab was rightmost.
  const onScreen = fullOrder.filter((id) => id === closedId || isEligible(id));
  const idx = onScreen.indexOf(closedId);
  if (idx === -1) return fallbackId;
  const remaining = onScreen.filter((id) => id !== closedId);
  return remaining.length > 0 ? remaining[Math.min(idx, remaining.length - 1)] : fallbackId;
}

/** The effective active workspace id for a team: the explicitly-focused
 *  workspace, or the team's first root as the fallback. Centralizes the
 *  `activeWorkspace[teamKey] ?? firstRootIdForTeam(...)` resolution that
 *  recurred across selectors, getters, and store actions, so the fallback
 *  rule lives in one place. Same structural-typed signature discipline as
 *  `firstRootIdForTeam` (no `ViewerState` import — avoids the cycle). */
export function activeWorkspaceIdForTeam(
  teamKey: string,
  activeWorkspaceByTeam: Record<string, string>,
  workspacesByTeam: Record<string, Workspace[]>,
): string {
  return activeWorkspaceByTeam[teamKey] ?? firstRootIdForTeam(teamKey, workspacesByTeam);
}

/** The active `Workspace` object for a team — resolves the active id via
 *  {@link activeWorkspaceIdForTeam} (focused workspace, else first root) and
 *  returns the matching object by reference, or `null` when none matches
 *  (e.g. the zero-roots `ROOT_NO_REPOS_SENTINEL`, which no real workspace
 *  carries). Centralizes the `activeWorkspaceIdForTeam(...)` + `.find(byId)`
 *  pair so the active-context selectors share the same resolution as the rest
 *  of the app. Returns the object by reference so a Zustand selector wrapping
 *  it stays `Object.is`-stable across mutations that leave the active
 *  workspace untouched (T-11). */
export function activeWorkspaceForTeam(
  teamKey: string,
  activeWorkspaceByTeam: Record<string, string>,
  workspacesByTeam: Record<string, Workspace[]>,
): Workspace | null {
  const wsId = activeWorkspaceIdForTeam(teamKey, activeWorkspaceByTeam, workspacesByTeam);
  return workspacesByTeam[teamKey]?.find((w) => w.id === wsId) ?? null;
}

/** Whether `teamKey` has the workspace `workspaceId`, open or minimized. */
export function workspaceInTeam(
  s: {
    workspaces: Record<string, readonly { id: string }[]>;
    softClosedWorkspaces: Record<string, Record<string, unknown>>;
  },
  teamKey: string,
  workspaceId: string,
): boolean {
  return (
    (s.workspaces[teamKey]?.some((w) => w.id === workspaceId) ?? false) ||
    s.softClosedWorkspaces[teamKey]?.[workspaceId] != null
  );
}

// ---------------------------------------------------------------------------
// T.3 helpers — source-repo glyph chip + canonical workspace sort.
// All pure functions; no React, no store dependency.
// ---------------------------------------------------------------------------

const GLYPH_CONSONANTS: ReadonlySet<string> = new Set("bcdfghjklmnpqrstvwxz".split(""));

/** Compute a stable 2-character glyph for one repo name (the portion after
 *  the `/` in a `${owner}/${repo}` slug). Used by `computeTeamGlyphMap` —
 *  callers prefer the map for collision handling.
 *
 *  Rules:
 *  - Empty input → `"??"` (defence-in-depth; never produced by valid slugs).
 *  - 1-char input → uppercased char + `"?"`.
 *  - 2+ char input → first char + the next consonant at index ≥ 1; if no
 *    consonant follows, fall back to `repoName[1]`. Result uppercased.
 *
 *  Examples: `"web" → "WB"` (first consonant after 'w' is 'b'),
 *  `"api" → "AP"`, `"ai" → "AI"` (no consonant after first char),
 *  `"auth" → "AT"`, `"wiki" → "WK"`. */
export function computeRepoGlyphBase(repoName: string): string {
  if (repoName.length === 0) return "??";
  if (repoName.length === 1) return `${repoName[0].toUpperCase()}?`;
  const first = repoName[0];
  let second: string | undefined;
  for (let i = 1; i < repoName.length; i++) {
    if (GLYPH_CONSONANTS.has(repoName[i].toLowerCase())) {
      second = repoName[i];
      break;
    }
  }
  if (second === undefined) second = repoName[1];
  return `${first}${second}`.toUpperCase();
}

/** Build the team-wide glyph map. Iterates `slugs` in input order so the
 *  collision suffix is deterministic — the first slug with a given base
 *  glyph keeps it un-suffixed; the 2nd collider gets `${base}2`, the 3rd
 *  `${base}3`, and so on.
 *
 *  Input: ordered list of `${owner}/${repo}` slugs (caller supplies them in
 *  the team's `source_repos` order — typically the result of
 *  `workspaceReposForTeam(...).map(r => r.sourceRepoSlug)`).
 *
 *  Output: `Map<slug, glyph>` keyed by the same slug strings.
 *
 *  Suffix numbering starts at 2 (not 1) so the un-suffixed form remains the
 *  canonical "first" glyph.
 *
 *  Uniqueness: every output glyph is unique within the map. Same-base
 *  colliders are disambiguated by the suffix counter. Cross-base collisions
 *  cannot occur because `computeRepoGlyphBase` always returns a 2-character
 *  string while suffix forms are 3+ characters — the value spaces are
 *  disjoint. */
export function computeTeamGlyphMap(slugs: readonly string[]): Map<string, string> {
  const out = new Map<string, string>();
  const baseCounts = new Map<string, number>();
  for (const slug of slugs) {
    const repoPart = slug.split("/")[1] ?? "";
    const base = computeRepoGlyphBase(repoPart);
    const prior = baseCounts.get(base) ?? 0;
    out.set(slug, prior === 0 ? base : `${base}${prior + 1}`);
    baseCounts.set(base, prior + 1);
  }
  return out;
}

/** Sort a team's workspaces into canonical render order:
 *  `(bucketIndex of sourceRepoSlug, kind === "root" ? 0 : 1)`, stable in
 *  in-block insertion order so the user's drag-reorder choices within a
 *  source repo are preserved.
 *
 *  `slugsInTeamOrder` is the team's source_repos order (typically the
 *  result of `workspaceReposForTeam(...).map(r => r.sourceRepoSlug)`).
 *  Workspaces whose `sourceRepoSlug` isn't in the list (orphans from a
 *  removed source repo, or workspaces missing the field entirely) sort
 *  AFTER all bucketed workspaces in a stable trailing group.
 *
 *  Returns a NEW array — the input is not mutated.
 *
 *  Pure function; safe to call from store reducers. Stable sort guarantee
 *  comes from Array.prototype.sort (TC39, stable since 2019). */
export function sortWorkspacesByCanonicalOrder(
  workspaces: readonly Workspace[],
  slugsInTeamOrder: readonly string[],
): Workspace[] {
  const bucketIndex = new Map<string, number>(slugsInTeamOrder.map((s, i) => [s, i]));
  return [...workspaces].sort((a, b) => {
    const ai = bucketIndex.get(a.sourceRepoSlug ?? "") ?? Number.MAX_SAFE_INTEGER;
    const bi = bucketIndex.get(b.sourceRepoSlug ?? "") ?? Number.MAX_SAFE_INTEGER;
    if (ai !== bi) return ai - bi;
    const aKind = a.kind === "root" ? 0 : 1;
    const bKind = b.kind === "root" ? 0 : 1;
    return aKind - bKind;
  });
}
