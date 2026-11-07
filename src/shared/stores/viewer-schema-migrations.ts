// SPDX-License-Identifier: MIT
/**
 * The persisted viewer blob's schema-migration chain.
 *
 * Each step names the version it migrates TO and mutates the raw persisted
 * `viewer` object in place. On load, {@link runViewerSchemaMigrations} applies
 * every step whose `to` is above the saved `schemaVersion`, in ascending order,
 * so a blob saved several versions ago steps through each migration in turn
 * (v9 → v10 → v11 → …). A new migration appends one step and bumps
 * {@link VIEWER_SCHEMA_VERSION}; it never edits an earlier step.
 *
 * Only the passes that run on the raw blob before the load destructures it live
 * here. The older T.1 / T.4 / T.6 reshapes run later in `loadPersistedState`
 * on the destructured slices and keep their own `< N` gates.
 */
import { deepRewriteFlattenTeamPaths } from "@/shared/lib/migrate-flatten-team-paths";
import {
  deepRewriteOrgSlugPaths,
  migrateViewerOrgSlugKeys,
} from "@/shared/lib/migrate-org-slug-keys";
import { seedCliChrome } from "./cli-chrome";

/** What a step may need from outside the `viewer` blob. */
export interface ViewerMigrationContext {
  /** The team that was open when the app last closed (from the saved org selection), if any. */
  readonly lastTeamKey?: string | null;
  /** The loader seeds after T.1/T.4 resolve legacy repo buckets. */
  readonly deferCliChromeSeed?: boolean;
}

export interface ViewerSchemaMigration {
  /** The schema version this step produces. */
  readonly to: number;
  readonly migrate: (viewer: Record<string, unknown>, context: ViewerMigrationContext) => void;
}

/**
 * SW-XS-3: the persisted settings that configured automatic forward sync. The
 * app no longer syncs content into workspaces on its own, so these have no
 * meaning and are dropped from saved state.
 */
export const RETIRED_FORWARD_SYNC_SETTINGS = [
  "forwardSyncDefault",
  "forwardSyncOverrides",
  "hydrationMode",
  "autoPullEnabled",
] as const;

/**
 * SW-XS-4: the persisted settings that configured automatic reverse sync. Edits
 * are published only by an explicit operation now, so these are dropped too.
 */
export const RETIRED_REVERSE_SYNC_SETTINGS = [
  "reverseSyncDefault",
  "reverseSyncOverrides",
] as const;

/**
 * SW-XS-17: the persisted settings that configured the worktree cap, the idle
 * reaper and the merged-worktree auto-remove. A worktree is now removed only by
 * an explicit Remove, so these are dropped from saved state.
 */
export const RETIRED_WORKTREE_REAP_SETTINGS = [
  "maxWorktrees",
  "idleThresholdMinutes",
  "worktreePins",
  "autoRemoveMergedWorktrees",
] as const;

function dropSettings(viewer: Record<string, unknown>, keys: readonly string[]): void {
  const settings = viewer.settings;
  if (!settings || typeof settings !== "object" || Array.isArray(settings)) return;
  for (const key of keys) {
    delete (settings as Record<string, unknown>)[key];
  }
}

/** SW-XS-15: the per-team selection of the left-nav context pivot, as persisted. */
export interface PersistedAgentContextSelection {
  agentKey: string;
  repoSlug?: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

/**
 * SW-XS-15: keep only well-formed `{agentKey, repoSlug?}` selections (a hand-edited or partial
 * blob must never crash the nav); anything else reads as Team (everything).
 */
export function sanitizeAgentContextSelections(
  raw: unknown,
): Record<string, PersistedAgentContextSelection> {
  if (!isRecord(raw)) return {};
  const out: Record<string, PersistedAgentContextSelection> = {};
  for (const [teamKey, value] of Object.entries(raw)) {
    if (!isRecord(value)) continue;
    const agentKey = value.agentKey;
    if (typeof agentKey !== "string" || agentKey === "") continue;
    const repoSlug = value.repoSlug;
    out[teamKey] =
      typeof repoSlug === "string" && /^(?:[^/\s]+\/)?[^/\s]+\/[^/\s]+$/.test(repoSlug)
        ? { agentKey, repoSlug }
        : { agentKey };
  }
  return out;
}

/**
 * SW-XS-15 v13 → v14: the left-nav pivot maps to an AGENT. The retired install-tier pivot
 * (`selectedSourceRepo` slug filter + `selectedContextId` "‹Team› · ‹Repo›" id) named no agent,
 * so it carries over as Team (everything): both slots are dropped and `selectedAgentContext`
 * starts empty (an existing one is kept, sanitized).
 */
export function migrateAgentContextPivot(viewer: Record<string, unknown>): void {
  delete viewer.selectedSourceRepo;
  delete viewer.selectedContextId;
  viewer.selectedAgentContext = sanitizeAgentContextSelections(viewer.selectedAgentContext);
}

export const VIEWER_SCHEMA_MIGRATIONS: readonly ViewerSchemaMigration[] = [
  {
    // SS-FU-12 v5–v8: rewrite the org-slug prefix (`owner--repo` → `owner/repo`)
    // of every team key, the deep absolute paths, and the `openedTeams` array
    // (deduped). Narrow and idempotent.
    to: 8,
    migrate: (viewer) => {
      migrateViewerOrgSlugKeys(viewer);
      deepRewriteOrgSlugPaths(viewer);
    },
  },
  {
    // SLW-IN-8 v9 → v10: repoint persisted paths that embed the retired
    // `_team/{team}/` nesting to the flat `{team}/` layout. A structural no-op
    // on already-flat paths.
    to: 10,
    migrate: (viewer) => {
      deepRewriteFlattenTeamPaths(viewer);
    },
  },
  {
    // SW-XS-3 v10 → v11: drop the automatic forward-sync settings.
    to: 11,
    migrate: (viewer) => dropSettings(viewer, RETIRED_FORWARD_SYNC_SETTINGS),
  },
  {
    // SW-XS-4 v11 → v12: drop the automatic reverse-sync settings.
    to: 12,
    migrate: (viewer) => dropSettings(viewer, RETIRED_REVERSE_SYNC_SETTINGS),
  },
  {
    // SW-XS-17 v12 → v13: drop the worktree cap, idle-threshold, pin and merged
    // auto-remove settings.
    to: 13,
    migrate: (viewer) => dropSettings(viewer, RETIRED_WORKTREE_REAP_SETTINGS),
  },
  {
    // SW-XS-15 v13 → v14: the context pivot maps to an agent; the install-tier
    // pivot's two slots are retired.
    to: 14,
    migrate: migrateAgentContextPivot,
  },
  {
    // D39 v14 → v15: the per-team CLI chrome (and each layout's zoom) is seeded into one
    // global `cliChrome` value.
    to: 15,
    migrate: (viewer, context) => {
      if (!context.deferCliChromeSeed) seedCliChrome(viewer, context.lastTeamKey);
    },
  },
];

/** The schema version the persist site writes: the last step's `to`. */
export const VIEWER_SCHEMA_VERSION =
  VIEWER_SCHEMA_MIGRATIONS[VIEWER_SCHEMA_MIGRATIONS.length - 1].to;

/**
 * Apply, in order, every migration newer than `savedVersion`. A missing or
 * non-numeric version runs the whole chain. Returns the versions applied.
 */
export function runViewerSchemaMigrations(
  viewer: Record<string, unknown>,
  savedVersion: unknown,
  context: ViewerMigrationContext = {},
): number[] {
  const from = typeof savedVersion === "number" ? savedVersion : 0;
  const applied: number[] = [];
  for (const step of VIEWER_SCHEMA_MIGRATIONS) {
    if (step.to > from) {
      step.migrate(viewer, context);
      applied.push(step.to);
    }
  }
  return applied;
}
