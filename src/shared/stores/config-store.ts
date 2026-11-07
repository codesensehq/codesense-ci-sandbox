// SPDX-License-Identifier: MIT
import {
  resolveTeamMainDocPreference,
  type TeamMainDocPreference,
} from "@/shared/lib/team-main-doc";
import { resolveTeamContentContext } from "@/shared/lib/team-content-context";
import { teamAllContextId } from "@/shared/lib/context-model";
import { parseTeamKey } from "@/shared/lib/team-key";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { create } from "zustand";
import {
  saveAppState,
  loadAppState,
  createContextRepo,
  loadWorkspaceConfig,
  saveWorkspaceConfig,
} from "@/tauri/commands";
import type { ContextRepoInfo } from "@/shared/types";
import { normalizeMainDoc } from "@/shared/lib/pipeline-graph/normalize";
import { isSyntheticRepoPath } from "@/shared/lib/build-synthetic-repo";
import { getCodesenseHome } from "@/shared/lib/codesense-home";
import { type WorkspaceRepoOverrides } from "@/shared/lib/workspace-config";
import { useOrgStore } from "@/shared/stores/org-store";
import { logger } from "@/shared/lib/logger";

/** SW-WT-9: a remembered one-time approval to run a team-synced worktree
 *  setup. Keyed by repo + config-content-hash so a config change re-prompts. */
export interface WorktreeSetupApproval {
  configHash: string;
  approvedAtMs: number;
}

/** SW-WT-9: approval key = repo path + config-content-hash. The hash alone
 *  identifies content; the repo path namespaces it per-repo (AC-3). */
export function setupApprovalKey(repoPath: string, configHash: string): string {
  return `${repoPath}::${configHash}`;
}

export interface RepoState {
  repoPath: string;
  repoName: string;
  contextRepoPath: string;
  /** PTS-3: canonical per-team context-repo content dir resolved by Rust
   *  (`path_to_team_content_dir`). Empty string when the repo isn't fully
   *  hydrated. PTS-2 migrates every consumer of the legacy
   *  `${contextRepoPath}/${viewerRoot}` composition to this field. */
  teamContentPath: string;
  hasRemote: boolean;
  remoteUrl: string | null;
  /** Global main doc filename convention (P.3). Empty string = built-in
   *  fallback chain (README.md → index.md). A non-empty value like "Epic.md"
   *  is passed to the scanner as the configured main doc name. */
  mainDoc: string;
  /** Real on-disk source-code path (F-6). Populated on synthetic
   *  workspace repos from `attachments.yml`. Absent or null on legacy
   *  `config-store.repos` entries, where `repoPath` is itself the real
   *  path. Use `resolveSourceRepoPath(repo)` rather than reading this
   *  directly. */
  localPath?: string | null;
}

/** The per-repo settings flowing through `updateRepo`. Equal to
 *  `Partial<WorkspaceRepoOverrides>`; every key of `WorkspaceRepoOverrides`
 *  must also exist on `RepoState` so `snapshotKeys` can read from a
 *  `RepoState` source — enforced by the assertion below. */
type RepoSettingPartial = Partial<WorkspaceRepoOverrides>;

/** Compile-time guard: every key of `WorkspaceRepoOverrides` must exist on
 *  `RepoState`. Breaks the build if a future override field is added
 *  without a matching `RepoState` field, which would make `snapshotKeys`
 *  silently read `undefined` and the revert path silently delete fields. */
type _AssertOverridesSubsetOfRepoState = keyof WorkspaceRepoOverrides extends keyof RepoState
  ? true
  : never;
const _assertOverridesSubsetOfRepoState: _AssertOverridesSubsetOfRepoState = true;
void _assertOverridesSubsetOfRepoState;

interface ConfigState {
  initialized: boolean;
  teamMainDocPreferences: Record<string, TeamMainDocPreference>;
  ensureTeamMainDocPreference: (teamKey: string) => Promise<void>;
  setTeamMainDoc: (teamKey: string, value: string) => Promise<void>;
  repos: RepoState[];
  addRepo: (repoPath: string) => Promise<void>;
  removeRepo: (repoPath: string) => Promise<void>;
  updateRepoRemote: (
    repoPath: string,
    hasRemote: boolean,
    remoteUrl: string | null,
  ) => Promise<void>;
  setMainDoc: (repoPath: string, value: string) => Promise<void>;
  /** SW-WT-9: remembered approvals to run team-synced setup, keyed via
   *  `setupApprovalKey(repoPath, configHash)`. */
  worktreeSetupApprovals: Record<string, WorktreeSetupApproval>;
  /** SW-WT-9: has this repo+config-hash been approved already? */
  isSetupApproved: (repoPath: string, configHash: string) => boolean;
  /** SW-WT-9: remember an approval (RMW to app-state, preserves other keys). */
  recordSetupApproval: (repoPath: string, configHash: string) => Promise<void>;
  loadState: () => Promise<void>;
}

export const useConfigStore = create<ConfigState>((set, get) => {
  /** Shared helper for the four per-repo setters. Routes synthetic vs legacy
   *  through the same optimistic-with-revert contract:
   *
   *    1. Pre-flight guard (synthetic only): warn-and-skip for unknown keys.
   *    2. Snapshot previous values for keys in `partial`.
   *    3. Optimistically write to the relevant store.
   *    4. Await disk save; on rejection, log + snapshot-compare revert + rethrow.
   *
   *  Snapshot-compare revert: only restore a key if the current in-memory
   *  value still equals the value we optimistically set. A newer overlapping
   *  write that already replaced the slot keeps ownership; its own rejection
   *  path will revert if needed. Prevents stale revert from clobbering newer
   *  user input for setters without a `useFieldDraft` commitInFlight guard. */
  async function updateRepo(repoPath: string, partial: RepoSettingPartial): Promise<void> {
    const keys = Object.keys(partial) as (keyof RepoSettingPartial)[];
    if (keys.length === 0) return;

    if (isSyntheticRepoPath(repoPath)) {
      const existing = useOrgStore.getState().workspaceRepos[repoPath];
      if (!existing) {
        logger.warn(
          "config-store",
          `updateRepo: synthetic repo ${repoPath} not in workspaceRepos; skipping write of fields [${keys.join(",")}]`,
        );
        return;
      }
      const previous = snapshotKeys(existing, keys);
      useOrgStore.getState().updateWorkspaceRepo(repoPath, partial);
      try {
        await persistWorkspaceOverride(repoPath, partial);
      } catch (e) {
        logger.error("config-store", `Failed to save workspace override: ${e}`);
        const revert = buildRevertPartial(
          useOrgStore.getState().workspaceRepos[repoPath],
          partial,
          previous,
        );
        if (revert) {
          useOrgStore.getState().updateWorkspaceRepo(repoPath, revert);
        }
        throw e;
      }
      return;
    }

    // Legacy branch
    const previousRepos = get().repos;
    const previousRepo = previousRepos.find((r) => r.repoPath === repoPath);
    const previous = previousRepo ? snapshotKeys(previousRepo, keys) : ({} as RepoSettingPartial);
    set({
      repos: previousRepos.map((r) => (r.repoPath === repoPath ? { ...r, ...partial } : r)),
    });
    try {
      await persist(get());
    } catch (e) {
      logger.error("config-store", `Failed to persist app state: ${e}`);
      const current = get().repos.find((r) => r.repoPath === repoPath);
      const revert = buildRevertPartial(current, partial, previous);
      if (revert) {
        set({
          repos: get().repos.map((r) => (r.repoPath === repoPath ? { ...r, ...revert } : r)),
        });
      }
      throw e;
    }
  }

  return {
    initialized: false,
    teamMainDocPreferences: {},
    ensureTeamMainDocPreference: (teamKey) => {
      const generation = workspaceGeneration;
      const pending = teamPreferenceLoads.get(teamKey);
      if (pending) return pending;
      const request = queueConfigWrite(async () => {
        if (generation !== workspaceGeneration) return { ok: true as const };
        try {
          validatedTeamPreferenceId(teamKey);
          let preference!: TeamMainDocPreference;
          await mutateWorkspaceConfig((config) => {
            validatedTeamPreferenceId(teamKey);
            const resolved = resolveTeamMainDocPreference(teamKey, config);
            preference = resolved.preference;
            return resolved.config;
          });
          if (generation === workspaceGeneration) {
            if (workspaceConfiguration)
              preference = resolveTeamMainDocPreference(teamKey, workspaceConfiguration).preference;
            set((state) => ({
              teamMainDocPreferences: { ...state.teamMainDocPreferences, [teamKey]: preference },
            }));
          }
          return { ok: true as const };
        } catch (error: unknown) {
          const message = error instanceof Error ? error.message : String(error);
          if (generation === workspaceGeneration)
            set((state) => ({
              teamMainDocPreferences: {
                ...state.teamMainDocPreferences,
                [teamKey]: { kind: "unavailable", mainDoc: "", error: message },
              },
            }));
          // Optional preference discovery is not an unsaved human edit. Keep
          // it out of the source/profile write-failure ledger.
          return { ok: false as const, error };
        }
      })
        .then((result) => {
          if (!result.ok) throw result.error;
        })
        .finally(() => {
          if (generation === workspaceGeneration) teamPreferenceLoads.delete(teamKey);
        });
      teamPreferenceLoads.set(teamKey, request);
      return request;
    },
    setTeamMainDoc: async (teamKey, value) => {
      const generation = workspaceGeneration;
      const id = validatedTeamPreferenceId(teamKey);
      const mainDoc = normalizeMainDoc(value);
      await queueConfigWrite(async () => {
        if (generation !== workspaceGeneration) return;
        validatedTeamPreferenceId(teamKey);
        await saveWorkspaceConfig(requiredConfigHome(), { repos: { [id]: { mainDoc } } });
        if (!workspaceConfiguration)
          set((state) => ({
            teamMainDocPreferences: {
              ...state.teamMainDocPreferences,
              [teamKey]: { kind: "ready", mainDoc },
            },
          }));
      });
    },
    repos: [],
    worktreeSetupApprovals: {},

    isSetupApproved: (repoPath, configHash) => {
      const key = setupApprovalKey(repoPath, configHash);
      return get().worktreeSetupApprovals[key]?.configHash === configHash;
    },

    recordSetupApproval: async (repoPath, configHash) => {
      const key = setupApprovalKey(repoPath, configHash);
      const previous = get().worktreeSetupApprovals;
      if (previous[key]?.configHash === configHash) return;
      // Optimistic in-memory update, then RMW persist. On failure, revert
      // only if our entry is still the one in place (mirrors updateRepo).
      const next = { ...previous, [key]: { configHash, approvedAtMs: Date.now() } };
      set({ worktreeSetupApprovals: next });
      try {
        await persist(get());
      } catch (e) {
        logger.error("config-store", `Failed to persist setup approval: ${e}`);
        const current = get().worktreeSetupApprovals;
        // Surgically drop only our optimistic entry (if it's still ours). A
        // wholesale restore to `previous` would clobber any approval recorded
        // by a concurrent setup between our set() and this failure.
        if (current[key]?.approvedAtMs === next[key].approvedAtMs) {
          const reverted = { ...current };
          delete reverted[key];
          set({ worktreeSetupApprovals: reverted });
        }
        throw e;
      }
    },

    addRepo: async (repoPath) => {
      let contextInfo: ContextRepoInfo;
      try {
        contextInfo = await createContextRepo(repoPath);
      } catch (e) {
        throw new Error(`Failed to create context repo: ${e}`);
      }

      const repo: RepoState = {
        repoPath,
        repoName: contextInfo.repo_name,
        contextRepoPath: contextInfo.path,
        teamContentPath: "",
        hasRemote: contextInfo.has_remote,
        remoteUrl: contextInfo.remote_url,
        mainDoc: "",
      };

      set({ repos: [...get().repos, repo] });
      try {
        await persist(get());
      } catch (e) {
        logger.error("config-store", `Failed to persist app state after addRepo: ${e}`);
      }
    },

    removeRepo: async (repoPath) => {
      set({ repos: get().repos.filter((r) => r.repoPath !== repoPath) });
      try {
        await persist(get());
      } catch (e) {
        logger.error("config-store", `Failed to persist app state after removeRepo: ${e}`);
      }
    },

    updateRepoRemote: async (repoPath, hasRemote, remoteUrl) => {
      set({
        repos: get().repos.map((r) =>
          r.repoPath === repoPath ? { ...r, hasRemote, remoteUrl } : r,
        ),
      });
      try {
        await persist(get());
      } catch (e) {
        logger.error("config-store", `Failed to persist app state after updateRepoRemote: ${e}`);
      }
    },

    setMainDoc: async (repoPath, value) => {
      const normalized = normalizeMainDoc(value);
      await updateRepo(repoPath, { mainDoc: normalized });
    },

    loadState: async () => {
      try {
        const raw = await loadAppState();
        const parsed = JSON.parse(raw);
        const repos = migrateRepos(parsed.repos ?? []);
        set({
          initialized: true,
          repos,
          worktreeSetupApprovals: coerceApprovals(parsed.worktreeSetupApprovals),
        });
      } catch {
        set({ initialized: true, repos: [], worktreeSetupApprovals: {} });
      }
    },
  };
});

/** Defensively coerce the persisted approvals map — any malformed entry is
 *  dropped rather than crashing the whole load (mirrors `migrateRepos`). */
function coerceApprovals(raw: unknown): Record<string, WorktreeSetupApproval> {
  if (!raw || typeof raw !== "object") return {};
  const out: Record<string, WorktreeSetupApproval> = {};
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (!value || typeof value !== "object") continue;
    const v = value as Record<string, unknown>;
    if (typeof v.configHash === "string" && typeof v.approvedAtMs === "number") {
      out[key] = { configHash: v.configHash, approvedAtMs: v.approvedAtMs };
    }
  }
  return out;
}

/** Copy the values of `keys` from `source` into a fresh `RepoSettingPartial`.
 *  Used to capture the pre-optimistic-write state for snapshot-compare revert. */
function snapshotKeys(
  source: Pick<RepoState, keyof WorkspaceRepoOverrides>,
  keys: (keyof RepoSettingPartial)[],
): RepoSettingPartial {
  const out: RepoSettingPartial = {};
  for (const key of keys) {
    assignOverrideKey(out, key, source[key]);
  }
  return out;
}

/** Snapshot-compare revert: for each key we optimistically wrote, only revert
 *  if the current in-memory value still equals our optimistic value. Returns
 *  a partial with just the keys that should be reverted, or `null` if nothing
 *  needs reverting (either source is gone, or every slot has been overwritten
 *  by a newer write that now owns it). */
function buildRevertPartial(
  current: Pick<RepoState, keyof WorkspaceRepoOverrides> | undefined,
  optimistic: RepoSettingPartial,
  previous: RepoSettingPartial,
): RepoSettingPartial | null {
  if (!current) return null;
  const revert: RepoSettingPartial = {};
  let any = false;
  for (const key of Object.keys(optimistic) as (keyof RepoSettingPartial)[]) {
    if (current[key] === optimistic[key]) {
      assignOverrideKey(revert, key, previous[key]);
      any = true;
    }
  }
  return any ? revert : null;
}

/** Exported for tests only. Public API is the 4 setters → `updateRepo`. */
export const __test__ = { buildRevertPartial };

/** Per-key assignment that satisfies TS's per-key value-type narrowing. */
function assignOverrideKey<K extends keyof WorkspaceRepoOverrides>(
  target: RepoSettingPartial,
  key: K,
  value: WorkspaceRepoOverrides[K] | undefined,
): void {
  target[key] = value;
}

/**
 * Type-coerce helpers for migrating persisted state. The on-disk JSON
 * is `unknown` — every field could be missing, the wrong type, or a
 * hand-edited garbage value. Use these instead of bare `as` casts so a
 * corrupted state file degrades to defaults rather than crashing far
 * from the migration.
 */
function coerceString(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function coerceStringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function coerceBoolean(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/// Migrate from old formats (string[] or RepoConfig[]) to RepoState[]
function migrateRepos(rawRepos: unknown[]): RepoState[] {
  return rawRepos
    .map((r: unknown): RepoState | null => {
      if (typeof r === "string") {
        // Old format: plain string path
        const name = r.split("/").pop() ?? r;
        return {
          repoPath: r,
          repoName: name.toLowerCase().replace(/[_ ]/g, "-"),
          contextRepoPath: "",
          teamContentPath: "",
          hasRemote: false,
          remoteUrl: null,
          mainDoc: "",
        };
      }
      if (typeof r !== "object" || r === null) return null;

      const obj = r as Record<string, unknown>;
      // Could be old RepoConfig (snake_case) or new RepoState (camelCase).
      const repoPath = coerceString(obj.repoPath, coerceString(obj.repo_path, ""));
      if (!repoPath) return null;

      const fallbackName = repoPath.split("/").pop()?.toLowerCase().replace(/[_ ]/g, "-") ?? "";
      return {
        repoPath,
        repoName: coerceString(obj.repoName, fallbackName),
        contextRepoPath: coerceString(obj.contextRepoPath, ""),
        teamContentPath: coerceString(obj.teamContentPath, ""),
        hasRemote: coerceBoolean(obj.hasRemote, false),
        remoteUrl: coerceStringOrNull(obj.remoteUrl),
        mainDoc: coerceString(obj.mainDoc, ""),
      };
    })
    .filter((r): r is RepoState => r !== null && r.repoPath.length > 0);
}

/** Read-modify-write the on-disk state so we don't clobber other top-level
 *  keys (notably `viewer`, owned by viewer-store). A straight overwrite was
 *  wiping the viewer key on every config mutation; viewer-store's debounced
 *  persist usually restored it on the next write, but the gap window meant
 *  a crash or app close right after a config change lost tabs/drill/expanded/
 *  pipelineScope state. Mirror viewer-store's RMW pattern.
 *
 *  Errors propagate to the caller. `updateRepo` uses this to drive the
 *  optimistic-revert path; `addRepo`/`removeRepo`/`updateRepoRemote` catch
 *  locally to preserve their pre-R-11 swallow-with-log behavior. */
function persist(state: ConfigState): Promise<void> {
  return queueConfigWrite(() =>
    serializeAppStateWrite(async () => {
      const raw = await loadAppState();
      const existing = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
      existing.repos = state.repos;
      existing.worktreeSetupApprovals = state.worktreeSetupApprovals;
      await saveAppState(JSON.stringify(existing));
    }),
  );
}

/** Serialize config-owned writes and retain failures for the profile-switch boundary. */
let configWriteTail: Promise<void> = Promise.resolve();
let configWriteFailures: unknown[] = [];
let retryConfigSnapshot = false;
function queueConfigWrite<T>(write: () => Promise<T>): Promise<T> {
  const next = configWriteTail.then(write);
  configWriteTail = next
    .then(() => undefined)
    .catch((error: unknown) => {
      configWriteFailures.push(error);
    });
  return next;
}
export async function flushConfigForProfileSwitch(): Promise<void> {
  if (retryConfigSnapshot) {
    retryConfigSnapshot = false;
    // Some legacy mutations retain their optimistic state after a failed save.
    // A deliberate retry must persist that current state before it may be retired.
    void persist(useConfigStore.getState()).catch(() => undefined);
  }
  let draining: Promise<void>;
  do {
    draining = configWriteTail;
    await draining;
  } while (draining !== configWriteTail);
  // Abort this attempt on failure. The next explicit attempt saves the latest
  // state, including either caller rollback or retained legacy optimistic edits.
  const failures = configWriteFailures;
  configWriteFailures = [];
  if (failures.length) {
    retryConfigSnapshot = true;
    throw failures[0];
  }
}

/** Module-level Promise chain that serializes workspace-config disk writes.
 *  Two rapid setter calls (e.g. two quick mainDoc edits)
 *  would otherwise race: A loads → B loads → A saves → B saves clobbers A.
 *  Chaining ensures each disk load-modify-save observes the previous save.
 *
 *  Note: this only serializes the DISK path. `updateRepo` performs the
 *  optimistic in-memory write BEFORE queuing onto this chain (so the UI
 *  reflects the latest user input immediately), then the snapshot-compare
 *  guard in `updateRepo`'s revert path resolves any cross-call ordering
 *  hazards between in-memory state and disk-write outcomes. */

async function persistWorkspaceOverride(
  repoPath: string,
  partial: Partial<WorkspaceRepoOverrides>,
): Promise<void> {
  const generation = workspaceGeneration;
  return queueConfigWrite(async () => {
    if (generation !== workspaceGeneration) return;
    const repo = useOrgStore.getState().workspaceRepos[repoPath];
    if (!repo) {
      // Defense-in-depth: `updateRepo` guards too, but a future direct caller
      // could bypass it. Keeping this no-op avoids crashing on a missing
      // contextRepoPath access.
      logger.warn(
        "config-store",
        `persistWorkspaceOverride: skipping for unknown synthetic key ${repoPath}`,
      );
      return;
    }
    // The home is null only if the OS home could not be read. THROW (don't silently return) so the
    // caller's catch in `updateRepo` reverts the optimistic in-memory write and
    // the failure surfaces — a silent return would leave the UI showing an
    // override as saved that never reaches disk and is lost on restart.
    const home = getCodesenseHome();
    if (home === null) {
      throw new Error(
        `persistWorkspaceOverride: the CodeSense home is unavailable for ${repoPath}`,
      );
    }
    await saveWorkspaceConfig(home, { repos: { [repoPath]: partial } });
  });
}

const teamPreferenceLoads = new Map<string, Promise<void>>();
function requiredConfigHome(): string {
  const home = getCodesenseHome();
  if (!home) throw new Error("The CodeSense settings home is unavailable.");
  return home;
}
function validatedTeamPreferenceId(teamKey: string): string {
  const state = useOrgStore.getState();
  if (resolveTeamContentContext(teamKey, state.orgs, state.discoveryComplete).kind !== "ready")
    throw new Error("This team is unavailable.");
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  return teamAllContextId(orgSlug, teamName!);
}

/** Callers hold configWriteTail; source and team writes share one lossless RMW. */
async function mutateWorkspaceConfig(
  change: (
    config: import("@/shared/lib/workspace-config").WorkspaceConfig,
  ) => import("@/shared/lib/workspace-config").WorkspaceConfig,
  home = requiredConfigHome(),
) {
  const config = workspaceConfiguration ?? (await loadWorkspaceConfig(home));
  const next = change(config);
  const repos: import("@/tauri/commands").WorkspaceConfig["repos"] = {};
  for (const [id, fields] of Object.entries(next.repos)) {
    if (JSON.stringify(config.repos[id]) !== JSON.stringify(fields)) repos[id] = fields;
  }
  if (Object.keys(repos).length) await saveWorkspaceConfig(home, { repos });
}

let workspacePrincipal: string | null = null;
let workspaceGeneration = 0;
export function bindWorkspaceConfigPrincipal(principal: string): void {
  if (workspacePrincipal && workspacePrincipal !== principal) resetWorkspaceConfigView();
  workspacePrincipal = principal;
}
let workspaceConfiguration: import("@/tauri/commands").WorkspaceConfig | null = null;
/** Current owned view only: rebuilding or resuming consumers never rereads the source. */
export function workspaceRepoOverrides(repoPath: string): WorkspaceRepoOverrides {
  return workspaceConfiguration?.repos[repoPath] ?? {};
}
export function resetWorkspaceConfigView(): void {
  workspaceGeneration++;
  workspacePrincipal = null;
  teamPreferenceLoads.clear();
  workspaceConfiguration = null;
  useConfigStore.setState({ teamMainDocPreferences: {} });
}
/** Only persisted conventions change here; navigation and local input drafts retain ownership. */
export function applyWorkspaceConfig(config: import("@/tauri/commands").WorkspaceConfig): void {
  workspaceConfiguration = config;
  useConfigStore.setState((state) => {
    const preferences = Object.fromEntries(
      Object.keys(state.teamMainDocPreferences).map((teamKey) => [
        teamKey,
        resolveTeamMainDocPreference(teamKey, config).preference,
      ]),
    );
    return JSON.stringify(preferences) === JSON.stringify(state.teamMainDocPreferences)
      ? state
      : { teamMainDocPreferences: preferences };
  });
  useOrgStore.setState((state) => {
    let changed = false;
    const workspaceRepos = Object.fromEntries(
      Object.entries(state.workspaceRepos).map(([id, repo]) => {
        const mainDoc = config.repos[id]?.mainDoc ?? "";
        if (repo.mainDoc === mainDoc) return [id, repo];
        changed = true;
        return [id, { ...repo, mainDoc }];
      }),
    );
    return changed ? { workspaceRepos } : state;
  });
}
