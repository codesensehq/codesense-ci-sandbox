// SPDX-License-Identifier: MIT
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { create } from "zustand";
import { saveAppState, loadAppState, type RepoPermission } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { warmScopes } from "@/shared/lib/warm-scopes";
import {
  deepRewriteOrgSlugPaths,
  migrateOrgStoreOrgSlug,
} from "@/shared/lib/migrate-org-slug-keys";
import { createDebouncedAction } from "@/shared/lib/debounced-action";
import type { Org, Person, SourceRepoSlug, Team } from "@/shared/types/domain";
import { personStableId } from "@/shared/lib/person-reference";
import type { RepoState } from "@/shared/stores/config-store";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { buildTeamKey } from "@/shared/lib/team-key";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { visibleTeams, activeTeams } from "@/shared/lib/user-teams";
import { rekeyOrgStateForRename } from "@/shared/lib/rekey-team";
import { isCodesenseDocs } from "@/shared/lib/codesense-docs";
import type { OrgRegistryChangedPayload } from "@/tauri/events";

/** The authenticated user's tier on the active workspace's remote repo.
 *  `null` means either (a) no active workspace selected, or (b) the
 *  active workspace is local-only (`remote_url === null`). Per the
 *  Permission-Aware UI contract, `null` grants full-control.
 *
 *  SW-OB-8: the remote permission *fetch* is retired — this is now
 *  pinned at `null` for every workspace (full control), which is the
 *  local-first / open-access end state. The type keeps `RepoPermission`
 *  as the surface SW-OB-7 removes when it deletes the gate entirely. */
export type WorkspacePermission = RepoPermission | null;

interface OrgState {
  orgs: Org[];
  activeOrgSlug: string | null;
  activeTeamName: string | null;
  /** F-39: per-org memory of the last non-null team the user selected,
   *  keyed by org slug. Used by `setActiveOrg` to restore the last team
   *  on org switch, and by `setOrgs` when the active team is removed but
   *  a per-org memory exists. Append-or-update only: never cleared by
   *  `setActiveTeam(null)` / `setActiveOrg(null)` / `clearActive()` — a
   *  transient clear is not an explicit forget. */
  lastTeamByOrgSlug: Record<string, string>;
  /** SW-OB-9: per-org, per-user LOCAL "shown teams" set — which teams appear
   *  in the nav/switcher, keyed by org slug, valued by an array of team NAMES.
   *  A personal visibility preference only: never touches `teams.yml`, never a
   *  git commit. Persisted alongside `lastTeamByOrgSlug` (local app state only).
   *
   *  absent (no key)  => never curated  => `visibleTeams` shows ALL teams.
   *  `[]` (empty)     => curated to none => shows NONE (routes to no-team).
   *  `[names]`        => shows only those teams, in org order.
   *  Written ONLY by `setShownTeams`, which also enforces active ⊆ shown. */
  shownTeamsByOrgSlug: Record<string, string[]>;
  /** SW-UX (app-flow redesign): org slugs the user has DISABLED — hidden from the
   *  active workspace list but re-enable-able (a soft-remove, not a delete). Backs
   *  "disable org" in the workspace-switcher management (e.g. dropping the pinned
   *  CodeSense reference org once you have your own). A personal visibility
   *  preference only. Invariant enforced by `setOrgDisabled`: at least one org
   *  stays enabled (CodeSense can't leave you with zero orgs). */
  disabledOrgSlugs: string[];
  stateLoaded: boolean;
  /** Runtime org discovery completion, independent of persisted selection restoration. */
  discoveryComplete: boolean;
  /** Authenticated user's tier on the active workspace. SW-OB-8 pinned
   *  this at `null` (full control) for every workspace — the remote
   *  permission fetch that used to populate it is retired. Kept as the
   *  surface SW-OB-7 removes when it deletes the gate. Runtime-only;
   *  never persisted. */
  currentUserPermission: WorkspacePermission;
  // True once any setter has been called since module load. Used by
  // `loadPersistedState` to distinguish "never touched" (cold start —
  // safe to restore) from "explicitly set, possibly to null" (user
  // intent — must not be undone by a late-arriving rehydrate). Any
  // `setActiveOrg`, `setActiveTeam`, `clearActive`, etc. flips this
  // flag and causes rehydrate to drop the persisted block entirely.
  // Per-field merge would leak stale data (e.g., a persisted
  // `activeTeamName` from a prior session into a live `activeOrgSlug`
  // set by the user, violating the org/team invariant).
  hasUserIntent: boolean;

  /** Synthetic RepoState entries for workspace-tracked repos. Runtime-only,
   *  never persisted — keyed by synthetic repoPath (e.g.,
   *  "workspace:9north:default:acme/api"). */
  workspaceRepos: Record<string, RepoState>;

  /** Slugs (`owner/repo`, lowercased) the user explicitly removed from each
   *  team, keyed by `teamKey`. Lets a worktree open against a removed repo
   *  report `slug-stale-after-removal` instead of the generic
   *  `slug-not-in-team`. Populated ONLY by `removeTeamSourceRepo` — never by
   *  the wholesale `workspaceRepos` teardown/rebuild on team switch — so
   *  transient re-derivation can't be mistaken for a user removal. Runtime-
   *  only; an app restart clears it, collapsing stale-removal back to the
   *  generic reason (safe). */
  recentlyRemovedRepos: Record<string, Set<string>>;

  /** F-6: monotonic counter bumped after any successful
   *  `saveSourceRepoAttachment` (silent auto-attach or the empty-state
   *  CTA). `use-workspace-viewer-sync` subscribes to this so synthetic
   *  repos re-resolve their `localPath` without requiring a workspace
   *  switch. Runtime-only; never persisted. */
  attachmentsVersion: number;

  /** F-19: suspended workspace repos, keyed by workspace key
   *  (`${orgSlug}:${teamName ?? "_none"}`). Entries are moved here from
   *  `workspaceRepos` during org/team switch so `resolveRepo()` can
   *  still resolve synthetic paths for suspended repos. */
  suspendedWorkspaceRepos: Record<string, Record<string, RepoState>>;

  setWorkspaceRepo: (repo: RepoState) => void;
  updateWorkspaceRepo: (repoPath: string, partial: Partial<RepoState>) => void;
  removeWorkspaceRepo: (repoPath: string) => void;
  clearWorkspaceRepos: () => void;
  /** F-19: move current `workspaceRepos` into `suspendedWorkspaceRepos[key]`,
   *  then clear `workspaceRepos`. */
  suspendWorkspaceRepos: (workspaceKey: string) => void;
  /** SF-8: team-scoped variant of `suspendWorkspaceRepos`. Snapshots ONLY
   *  the supplied repos into `suspendedWorkspaceRepos[workspaceKey]` and
   *  removes those specific entries from `workspaceRepos`. Other teams'
   *  entries stay live — required during rapid-switch (A→B→A) when more
   *  than one team's synthetics may briefly coexist in `workspaceRepos`
   *  while a prior run is still mid-flight. */
  suspendTeamRepos: (workspaceKey: string, repos: RepoState[]) => void;
  /** F-19: move `suspendedWorkspaceRepos[key]` back to `workspaceRepos`,
   *  delete the suspended entry. No-op if key not found. */
  resumeWorkspaceRepos: (workspaceKey: string) => void;
  /** F-19: delete `suspendedWorkspaceRepos[key]` and return the repo
   *  paths for full cleanup. Returns empty array if key not found. */
  evictSuspendedWorkspace: (workspaceKey: string) => string[];
  /** F-6: increment `attachmentsVersion`. Call this after a successful
   *  `saveSourceRepoAttachment` to trigger synthetic-repo re-resolution. */
  bumpAttachmentsVersion: () => void;
  beginDiscovery: () => void;
  discoveryAttempt: number;
  setDiscoveryComplete: (complete: boolean) => void;
  setOrgs: (orgs: Org[]) => void;
  setActiveOrg: (slug: string | null) => void;
  setActiveTeam: (name: string | null) => void;
  /** Make `teamName` of `orgSlug` active in ONE write: a cross-org switch never passes through the
   *  target org's remembered team. */
  setActiveTeamIn: (orgSlug: string, teamName: string) => void;
  /** SW-OB-9: the ONLY writer of `shownTeamsByOrgSlug`. Replaces the org's
   *  shown set with `teamNames` (personal/local — no git, no membership).
   *  Centralizes AC-6 ("active team ⊆ shown set"): when writing the active
   *  org's set, it reconciles the active team in the SAME atomic `set` —
   *  keeping the active team if still shown, else falling back to the first
   *  remaining shown team (org order), else `null` (→ no-team state). */
  setShownTeams: (orgSlug: string, teamNames: string[]) => void;
  /** SW-UX (app-flow redesign): disable (soft-remove) or re-enable an org in the
   *  local workspace list. Enforces the ≥1-enabled-org invariant — disabling the
   *  last enabled org is a no-op. When disabling the ACTIVE org, switches the
   *  active org to the first remaining enabled one in the SAME atomic update. */
  setOrgDisabled: (slug: string, disabled: boolean) => void;
  clearActive: () => void;
  /** Push a new source repo slug into a team's `source_repos` in-memory.
   *  Ephemeral — NOT persisted. Re-derived from disk on next startup. */
  updateTeamSourceRepos: (orgSlug: string, teamName: string, slug: SourceRepoSlug) => void;
  /** Remove a source repo slug from a team's `source_repos` in-memory
   *  AND drop the corresponding synthetic entry from `workspaceRepos`.
   *  Ephemeral — NOT persisted. Re-derived from disk on next startup. */
  removeTeamSourceRepo: (orgSlug: string, teamName: string, slug: SourceRepoSlug) => void;
  /** SW-OB-1: append a newly created team to an org's `teams` in-memory.
   *  Ephemeral — NOT persisted; re-derived from disk on next startup. */
  addTeamToStore: (orgSlug: string, team: Team) => void;
  /** SW-ID-3: replace a single roster entry in an org's `people` in-memory (by
   *  `github_user_id`), used to reflect a refreshed name/avatar without a
   *  reload. No-op if the org/person is absent. Ephemeral — re-derived from
   *  disk on startup. Mirror of `addTeamToStore`. */
  updatePersonInStore: (orgSlug: string, person: Person) => void;
  addPersonToStore: (orgSlug: string, person: Person) => void;
  /** SW-OB-1: create a team locally — commit it to `teams.yml` (a local
   *  git commit via the SW-OB-8 seam), reflect it in-memory, and switch
   *  into it. Rejects (throws) on validation/commit failure so callers can
   *  surface an inline error. Reused by OB-2's choose-your-team state. */
  createTeam: (name: string) => Promise<void>;
  /** SW-HB-7: delete (de-register) a team locally — commit the `teams.yml`
   *  entry-strip via the SW-OB-8 seam, then reflect. AWAITs the commit (the UI
   *  guards it behind a name-echo confirm; git-recoverable). Throws on failure
   *  so callers can surface an inline error. */
  deleteTeam: (name: string) => Promise<void>;
  /** SW-HB-7: archive/unarchive a team ORG-WIDE (reversible retirement).
   *  Reflects OPTIMISTICALLY then commits (V-4: single-click + reversible →
   *  never block the UI on the local commit); rolls the in-memory flag back on
   *  commit failure and rethrows. */
  setTeamArchived: (name: string, archived: boolean) => Promise<void>;
  /** SW-HB-7: in-memory removal reflector (mirror of `addTeamToStore`). Drops
   *  the team from `orgs`, prunes the personal shown set (through the single
   *  `setShownTeams` writer) + a stale `lastTeamByOrgSlug` pointer, and
   *  reconciles the active team. Ephemeral — re-derived from disk on startup. */
  removeTeamFromStore: (orgSlug: string, name: string) => void;
  /** SW-HB-7: in-memory archived-flag reflector. Flips `archived` on the team
   *  and — only when archiving the CURRENTLY active team — reconciles the
   *  active team off it via `defaultTeamForOrg` (which excludes archived, V-1).
   *  Does NOT touch the shown set (V-2: reversibility). Ephemeral. */
  setTeamArchivedInStore: (orgSlug: string, name: string, archived: boolean) => void;
  /** SW-HB-8: rename a team in place — commit the registry + path move (local
   *  git commit via the SW-OB-8 seam / RenameTeam orchestrator), then re-key
   *  every in-memory field keyed by the old name across BOTH this store and the
   *  viewer store, so open tabs/terminals survive and no key dangles at the old
   *  name. Rejects (throws) on validation/commit failure for inline surfacing.
   *  The active-team watch/wave re-point rides the existing team-activation
   *  effect: changing `activeTeamName` old→new re-fires `setActiveTeamForWave`. */
  renameTeam: (oldName: string, newName: string) => Promise<void>;
  /** SLW-IN-5: set (or clear) a team's parent. `parent` is the parent team's
   *  name, or `null` to clear (the team becomes a root). Commits the
   *  `teams.yml` field flip via the SW-OB-8 seam (SetTeamParent variant — no new
   *  command), THEN reflects `parent_team` in-memory. Await-then-reflect (like
   *  `renameTeam`), so a rejected commit leaves state untouched — no rollback.
   *  No re-key (a parent pointer moves no directories) and no `persistState`
   *  (`orgs` is ephemeral, re-derived from disk). Throws on failure for inline
   *  surfacing; the picker (AC-2) already blocks cycles at write time. */
  setTeamParent: (teamName: string, parent: string | null) => Promise<void>;
  /** Set (or clear) a team's starter page in the active org: `page` is relative
   *  to the team's content folder, `null` falls back to README/index. Commits the
   *  `teams.yml` field flip (SetTeamLandingPage — a local working-tree edit,
   *  never pushed), THEN reflects `landing_page` in-memory. Throws on refusal
   *  (missing/hidden file, read-only org) for the caller to surface. */
  setTeamLandingPage: (teamName: string, page: string | null, orgSlug?: string) => Promise<void>;
  /** Move every in-memory field keyed by team `oldName` to `newName`, across
   *  this store and the viewer store. A no-op once the org no longer has
   *  `oldName`, so the UI's own rename and the registry-change reload can both
   *  apply it without double-moving state. */
  applyTeamRename: (orgSlug: string, oldName: string, newName: string) => Promise<void>;
  /** Reconcile `org` — freshly reloaded from disk after `change` — into the
   *  store: carry a rename/delete over to team-keyed state the way the UI's own
   *  rename/delete does, replace the org with disk truth (re-validating the
   *  active team), and keep a newly created team visible. `org` is null when the
   *  reload failed: the rename/delete mapping still applies, from the event
   *  alone. Idempotent, so an edit the UI already reflected applies cleanly a
   *  second time. */
  applyOrgRegistryChange: (org: Org | null, change: OrgRegistryChangedPayload) => Promise<void>;
  /** Make `teamName` in `orgSlug` the active team the way a team click does,
   *  switching org first when needed and making the team visible. Returns false
   *  when the store does not know that team. */
  switchToTeam: (orgSlug: string, teamName: string) => boolean;
  persistState: () => void;
  loadPersistedState: () => Promise<void>;
}

// Persisted subset of the store: session selection only.
// The `orgs` list is deliberately NOT persisted — source of truth is the
// cloned context repos on disk; later stories repopulate from disk.
// F-25 removed `currentUser` from persistence — the active local profile
// is the single source of truth for user identity.
interface PersistedOrgState {
  activeOrgSlug: string | null;
  activeTeamName: string | null;
  /** F-39: optional in pre-upgrade JSON; `loadPersistedState` defaults
   *  it to `{}` (or seeds from `{ [activeOrgSlug]: activeTeamName }`
   *  when the pre-upgrade pair is non-null). */
  lastTeamByOrgSlug?: Record<string, string>;
  /** SW-OB-9: per-org local team-visibility set. Optional in pre-upgrade JSON;
   *  `loadPersistedState` defaults it to `{}`. Restored/merged with the SAME
   *  per-entry, live-wins discipline as `lastTeamByOrgSlug` (see the merge in
   *  the `hasUserIntent` branch — dropping it would lose other orgs' curation). */
  shownTeamsByOrgSlug?: Record<string, string[]>;
  /** SW-UX (app-flow redesign): disabled (soft-removed) org slugs. Optional in
   *  pre-upgrade JSON; `loadPersistedState` defaults it to `[]`. */
  disabledOrgSlugs?: string[];
}

/** F-39: look up the user's last-active team for `org` from the per-org
 *  memory, validated against the org's current team list. Returns `null`
 *  when no memory exists or the remembered team is no longer in the org.
 *  Shared by `setActiveOrg` (org switch) and `setOrgs` (active team gone).
 *
 *  SW-OB-9: validated against the user's SHOWN teams, not just existence —
 *  the remembered team is only restorable if it's currently visible. A team
 *  the user has hidden must NOT be resurrected as active (AC-6: active team ⊆
 *  shown set). Absent shown-set ⇒ all teams shown ⇒ identical to pre-OB-9. */
export function resolveRestoredTeam(
  org: Org,
  lastTeamByOrgSlug: Record<string, string>,
  shownByOrg: Record<string, string[]>,
): string | null {
  const stored = lastTeamByOrgSlug[org.slug];
  if (stored === undefined) return null;
  return visibleTeams(org, shownByOrg).some((t) => t.name === stored) ? stored : null;
}

/**
 * SW-OB-7 (open team access): a sensible DEFAULT team to land on for an org —
 * the team named `"default"` if present, else the org's first team, else `null`
 * only for a genuinely team-less org.
 *
 * Why this exists: under open access every team in the org is a valid landing,
 * so the active team must never sit at `null` while the org HAS teams. A `null`
 * active team produces the teamKey `${orgSlug}:_none`, which does NOT match the
 * `${orgSlug}:${team}` key the viewer already opened (`openedTeams`) — and
 * `App.tsx` hides every opened team whose key isn't the active one, so the whole
 * content area goes `display:none` (a BLANK screen, no crash). Reconciliation
 * (`setOrgs`) / selection (`setActiveOrg`) fall back through here so the active
 * team is always a real, existing team when one is available.
 *
 * SW-OB-9: the landing team is chosen from the user's SHOWN teams, so the
 * "active team ⊆ shown set" invariant (AC-6) holds on EVERY active-team
 * resolution path (org switch, reconcile, restore), not just the `setShownTeams`
 * curation write. Absent shown-set ⇒ all teams shown ⇒ identical to SW-OB-7.
 * An empty shown-set ⇒ `null` ⇒ the no-team state (App.tsx now renders the
 * empty state on a null active team, so this no longer blanks the screen).
 */
export function defaultTeamForOrg(org: Org, shownByOrg: Record<string, string[]>): string | null {
  const shown = visibleTeams(org, shownByOrg);
  if (shown.length === 0) return null;
  return (shown.find((t) => t.name === "default") ?? shown[0]).name;
}

/** T-13: canonical key for the `recentlyRemovedRepos` set — `owner/repo`,
 *  lowercased. Both the record (removeTeamSourceRepo) and prune
 *  (updateTeamSourceRepos) seams MUST derive the key the same way or a
 *  remove→re-add cycle won't clear the flag; this helper guarantees they do. */
function removedRepoKey(slug: { owner: string; repo: string }): string {
  return `${slug.owner}/${slug.repo}`.toLowerCase();
}

/** Immutably patch a single team's fields within `orgs` — the shared shape
 *  behind the field-flip reflectors (`setTeamArchivedInStore`, `setTeamParent`).
 *  Untouched orgs and teams keep referential identity, so a flip only
 *  re-renders subscribers of the changed org/team. */
function patchTeamInOrgs(
  orgs: Org[],
  orgSlug: string,
  teamName: string,
  patch: Partial<Team>,
): Org[] {
  return orgs.map((org) =>
    org.slug === orgSlug
      ? { ...org, teams: org.teams.map((t) => (t.name === teamName ? { ...t, ...patch } : t)) }
      : org,
  );
}

const debouncedPersist = createDebouncedAction(
  () =>
    serializeAppStateWrite(async () => {
      try {
        const configState = await loadAppState();
        const existing = JSON.parse(configState || "{}");
        const {
          activeOrgSlug,
          activeTeamName,
          lastTeamByOrgSlug,
          shownTeamsByOrgSlug,
          disabledOrgSlugs,
        } = useOrgStore.getState();
        const persisted: PersistedOrgState = {
          activeOrgSlug,
          activeTeamName,
          lastTeamByOrgSlug,
          shownTeamsByOrgSlug,
          disabledOrgSlugs,
        };
        existing.org = persisted;
        await saveAppState(JSON.stringify(existing));
        logger.info("org-store", "persistState: success");
      } catch (e) {
        logger.error("org-store", `persistState: failed — ${e}`);
        throw e;
      }
    }),
  300,
  "persistState",
);

export const useOrgStore = create<OrgState>((set, get) => ({
  orgs: [],
  activeOrgSlug: null,
  activeTeamName: null,
  lastTeamByOrgSlug: {},
  shownTeamsByOrgSlug: {},
  disabledOrgSlugs: [],
  stateLoaded: false,
  discoveryComplete: false,
  hasUserIntent: false,
  workspaceRepos: {},
  recentlyRemovedRepos: {},
  suspendedWorkspaceRepos: {},
  attachmentsVersion: 0,
  currentUserPermission: null,

  setWorkspaceRepo: (repo) => {
    set((s) => ({ workspaceRepos: { ...s.workspaceRepos, [repo.repoPath]: repo } }));
  },

  updateWorkspaceRepo: (repoPath, partial) => {
    set((s) => {
      const existing = s.workspaceRepos[repoPath];
      if (!existing) return s;
      return { workspaceRepos: { ...s.workspaceRepos, [repoPath]: { ...existing, ...partial } } };
    });
  },

  removeWorkspaceRepo: (repoPath) => {
    set((s) => {
      const { [repoPath]: _, ...rest } = s.workspaceRepos;
      return { workspaceRepos: rest };
    });
  },

  clearWorkspaceRepos: () => {
    set({ workspaceRepos: {} });
  },

  suspendWorkspaceRepos: (workspaceKey) => {
    if (get().suspendedWorkspaceRepos[workspaceKey]) {
      logger.warn(
        "org-store",
        `suspendWorkspaceRepos: "${workspaceKey}" already suspended; skipping to avoid overwrite`,
      );
      return;
    }
    const repos = { ...get().workspaceRepos };
    set((s) => ({
      workspaceRepos: {},
      suspendedWorkspaceRepos: { ...s.suspendedWorkspaceRepos, [workspaceKey]: repos },
    }));
  },

  suspendTeamRepos: (workspaceKey, repos) => {
    if (repos.length === 0) return;
    set((s) => {
      const nextWorkspaceRepos = { ...s.workspaceRepos };
      for (const r of repos) delete nextWorkspaceRepos[r.repoPath];
      // SF-8 review iter-1 (BH#3): if the suspend slot is already occupied
      // (e.g. a prior run's bail snapshotted this team), DON'T overwrite —
      // the existing snapshot is the user's intended resume target. Still
      // drop the failing run's synthetics from live `workspaceRepos` so
      // they don't orphan a stale mount.
      if (s.suspendedWorkspaceRepos[workspaceKey]) {
        logger.warn(
          "org-store",
          `suspendTeamRepos: "${workspaceKey}" already suspended; dropping ${repos.length} orphan synthetics from live workspaceRepos (preserving existing snapshot)`,
        );
        return { workspaceRepos: nextWorkspaceRepos };
      }
      const snapshot: Record<string, RepoState> = {};
      for (const r of repos) snapshot[r.repoPath] = r;
      return {
        workspaceRepos: nextWorkspaceRepos,
        suspendedWorkspaceRepos: { ...s.suspendedWorkspaceRepos, [workspaceKey]: snapshot },
      };
    });
  },

  resumeWorkspaceRepos: (workspaceKey) => {
    const suspended = get().suspendedWorkspaceRepos[workspaceKey];
    if (!suspended) return;
    set((s) => {
      const { [workspaceKey]: _, ...restSuspended } = s.suspendedWorkspaceRepos;
      return { workspaceRepos: suspended, suspendedWorkspaceRepos: restSuspended };
    });
  },

  evictSuspendedWorkspace: (workspaceKey) => {
    const suspended = get().suspendedWorkspaceRepos[workspaceKey];
    const repoPaths = suspended ? Object.keys(suspended) : [];
    set((s) => {
      const { [workspaceKey]: _, ...restSuspended } = s.suspendedWorkspaceRepos;
      return { suspendedWorkspaceRepos: restSuspended };
    });
    return repoPaths;
  },

  bumpAttachmentsVersion: () => {
    set((s) => ({ attachmentsVersion: s.attachmentsVersion + 1 }));
  },

  discoveryAttempt: 0,
  beginDiscovery: () =>
    set((state) => ({ discoveryAttempt: state.discoveryAttempt + 1, discoveryComplete: false })),
  setDiscoveryComplete: (discoveryComplete) => set({ discoveryComplete }),

  setOrgs: (orgs) => {
    // NOTE: `setOrgs` deliberately does NOT flip `hasUserIntent`. It's a
    // system-driven refresh (disk → memory), not an expression of user
    // intent. Flipping the flag here would mean that a future
    // `setOrgs([...fromDisk])` during App init (the expected A.3+ flow)
    // would trip the `loadPersistedState` rehydrate-drop guard and
    // silently wipe `activeOrgSlug` / `activeTeamName`.
    const state = get();
    const nextActiveOrg =
      state.activeOrgSlug !== null
        ? (orgs.find((o) => o.slug === state.activeOrgSlug) ?? null)
        : null;
    if (nextActiveOrg === null) {
      set({ orgs, activeOrgSlug: null, activeTeamName: null });
      get().persistState();
      return;
    }
    // Active org still present; check whether the previously-active team is
    // still a valid landing. SW-OB-9: "valid" means still SHOWN (not merely
    // existing) — a hidden team must not stay active (AC-6). Absent shown-set
    // ⇒ all teams shown ⇒ identical to the pre-OB-9 existence check.
    const activeTeamStillValid =
      state.activeTeamName !== null &&
      visibleTeams(nextActiveOrg, state.shownTeamsByOrgSlug).some(
        (t) => t.name === state.activeTeamName,
      );
    // F-39 AC-6: when the previously-active team is gone (or now hidden), fall
    // back to the per-org memory. SW-OB-7: then fall back to a DEFAULT team
    // rather than `null` — under open access any team is a valid landing, and a
    // `null` active team while the org has SHOWN teams (and the viewer already
    // opened one) blanks the whole content area (see `defaultTeamForOrg`). Both
    // fallbacks are SW-OB-9 shown-set-aware. Only a genuinely team-less / all-
    // hidden org keeps a `null` active team (→ no-team state).
    const restoredTeam = activeTeamStillValid
      ? state.activeTeamName
      : (resolveRestoredTeam(nextActiveOrg, state.lastTeamByOrgSlug, state.shownTeamsByOrgSlug) ??
        defaultTeamForOrg(nextActiveOrg, state.shownTeamsByOrgSlug));
    set({
      orgs,
      activeOrgSlug: state.activeOrgSlug,
      activeTeamName: restoredTeam,
    });
    get().persistState();
  },

  setActiveOrg: (slug) => {
    const state = get();
    if (slug === null) {
      set({ activeOrgSlug: null, activeTeamName: null, hasUserIntent: true });
      get().persistState();
      return;
    }
    // Early return if the caller is re-selecting the already-active org —
    // prevents generic "set" flows (and StrictMode double-invoke) from
    // clobbering an already-selected team.
    //
    // F-39 review patch (P2): with the per-org memory introduced by AC-5,
    // the rehydrated state can legitimately be
    // `{ activeOrgSlug: A, activeTeamName: null, lastTeamByOrgSlug: { A: X } }` —
    // reachable when the user did `setActiveTeam(null)` in a prior
    // session (AC-5 keeps the memory across sessions). When the user
    // re-selects A in the switcher, we must consult the memory before
    // returning, otherwise the remembered team is never restored. We
    // still skip the path entirely when `activeTeamName` is non-null,
    // preserving the StrictMode-clobber guard.
    if (slug === state.activeOrgSlug) {
      if (state.activeTeamName === null) {
        const org = state.orgs.find((o) => o.slug === slug);
        if (org) {
          // SW-OB-7: restore the remembered team, else land on a default team
          // (open access) rather than staying at `null`. SW-OB-9: both honor
          // the shown set (AC-6 — never restore/land on a hidden team).
          const restoredTeam =
            resolveRestoredTeam(org, state.lastTeamByOrgSlug, state.shownTeamsByOrgSlug) ??
            defaultTeamForOrg(org, state.shownTeamsByOrgSlug);
          if (restoredTeam !== null) {
            set({ activeTeamName: restoredTeam, hasUserIntent: true });
            get().persistState();
          }
        }
      }
      return;
    }
    const org = state.orgs.find((o) => o.slug === slug);
    if (!org) {
      logger.warn("org-store", `setActiveOrg: slug "${slug}" not in orgs list`);
      return;
    }
    // F-39 AC-6: restore the user's last team for this org if the memory
    // exists and the team is still SHOWN. SW-OB-7: else land on a default team
    // (open access) instead of `null`. SW-OB-9: both fallbacks honor the shown
    // set, so switching into an org never lands active on a hidden team (AC-6);
    // an all-hidden org lands `null` → the no-team state. Atomic `set` so a
    // single watcher fire propagates both the slug and the team.
    const restoredTeam =
      resolveRestoredTeam(org, state.lastTeamByOrgSlug, state.shownTeamsByOrgSlug) ??
      defaultTeamForOrg(org, state.shownTeamsByOrgSlug);
    set({ activeOrgSlug: slug, activeTeamName: restoredTeam, hasUserIntent: true });
    get().persistState();
  },

  setActiveTeam: (name) => {
    const state = get();
    if (name === null) {
      // F-39 AC-5: clearing the active team does NOT touch
      // `lastTeamByOrgSlug` — a transient clear is not an explicit forget.
      set({ activeTeamName: null, hasUserIntent: true });
      get().persistState();
      return;
    }
    const org = state.orgs.find((o) => o.slug === state.activeOrgSlug);
    if (!org) {
      logger.warn("org-store", "setActiveTeam: no active org");
      return;
    }
    const team = org.teams.find((t) => t.name === name);
    if (!team) {
      logger.warn("org-store", `setActiveTeam: team "${name}" not in active org "${org.slug}"`);
      return;
    }
    // F-39 AC-5: write the per-org memory in the same atomic `set` so the
    // `[activeOrgSlug, activeTeamName]` watchers in App.tsx and
    // use-workspace-viewer-sync fire exactly once.
    set({
      activeTeamName: name,
      lastTeamByOrgSlug: { ...state.lastTeamByOrgSlug, [org.slug]: name },
      hasUserIntent: true,
    });
    get().persistState();
  },

  setActiveTeamIn: (orgSlug, teamName) => {
    const org = get().orgs.find((o) => o.slug === orgSlug);
    if (!org?.teams.some((t) => t.name === teamName)) {
      logger.warn("org-store", `setActiveTeamIn: team "${teamName}" not in org "${orgSlug}"`);
      return;
    }
    set((state) => ({
      activeOrgSlug: orgSlug,
      activeTeamName: teamName,
      lastTeamByOrgSlug: { ...state.lastTeamByOrgSlug, [orgSlug]: teamName },
      hasUserIntent: true,
    }));
    get().persistState();
  },

  setShownTeams: (orgSlug, teamNames) => {
    const state = get();
    const org = state.orgs.find((o) => o.slug === orgSlug);

    // SW-HB-7 (V-3): reconcile over ACTIVE (non-archived) teams only. An
    // archived team is org-wide-hidden, so it must neither block the collapse
    // to show-all nor be picked as a fallback active team below.
    const orgActiveTeams = org ? activeTeams(org) : [];

    // SW-OB-9: if the selection covers EVERY active team in the org, the user
    // hasn't curated a subset — store ABSENT (drop the entry) rather than an
    // explicit all-list, so the org stays in the show-all state and teams added
    // later (by anyone) remain visible by default. Absent === show-all today, so
    // this only changes FUTURE behavior, for the better; an explicit all-list
    // would silently hide a future team. Also collapses a no-op "select all →
    // Done".
    const coversAll =
      org !== undefined &&
      orgActiveTeams.length > 0 &&
      orgActiveTeams.every((t) => teamNames.includes(t.name));
    let nextMap: Record<string, string[]>;
    if (coversAll) {
      const { [orgSlug]: _dropped, ...rest } = state.shownTeamsByOrgSlug;
      nextMap = rest;
    } else {
      nextMap = { ...state.shownTeamsByOrgSlug, [orgSlug]: teamNames };
    }
    // The EFFECTIVE shown names after the collapse — used for AC-6
    // reconciliation so it's correct whether we stored explicit names or
    // collapsed to absent (absent ⇒ all ACTIVE org teams shown).
    const effectiveShown = coversAll ? orgActiveTeams.map((t) => t.name) : teamNames;

    // Curating a NON-active org: just write the map. No active team to
    // reconcile (the active org's shown set is unchanged). `hasUserIntent`
    // flips like every other setter so a pre-rehydrate curation isn't clobbered
    // by a late-arriving cold restore (consistency — `setShownTeams` is the one
    // writer of this map).
    if (orgSlug !== state.activeOrgSlug) {
      set({ shownTeamsByOrgSlug: nextMap, hasUserIntent: true });
      get().persistState();
      return;
    }

    // AC-6: the active team must stay inside the shown set. Keep it if still
    // shown; else fall back to the first remaining shown team in ORG order
    // (not the caller's array order); else `null` → the no-team state, whose
    // "Browse all teams" CTA opens the team-management tab to re-add (never a dead-end).
    const activeStillShown =
      state.activeTeamName !== null && effectiveShown.includes(state.activeTeamName);
    const desiredActive = activeStillShown
      ? state.activeTeamName
      : (orgActiveTeams.find((t) => effectiveShown.includes(t.name))?.name ?? null);

    // ONE atomic `set` (mirrors setActiveTeam's atomic write) so the
    // `[activeOrgSlug, activeTeamName]` watchers in App.tsx and
    // use-workspace-viewer-sync fire exactly once. Update the per-org memory
    // only when landing on a real team (a transient clear is not a forget —
    // symmetric with setActiveTeam).
    set({
      shownTeamsByOrgSlug: nextMap,
      activeTeamName: desiredActive,
      lastTeamByOrgSlug:
        desiredActive !== null
          ? { ...state.lastTeamByOrgSlug, [orgSlug]: desiredActive }
          : state.lastTeamByOrgSlug,
      hasUserIntent: true,
    });
    get().persistState();
  },

  clearActive: () => {
    set({ activeOrgSlug: null, activeTeamName: null, hasUserIntent: true });
    get().persistState();
  },

  setOrgDisabled: (slug, disabled) => {
    const state = get();
    const current = new Set(state.disabledOrgSlugs);
    // SW-OC-16: the CodeSense docs context repo is pinned and can't be disabled.
    if (disabled && isCodesenseDocs(slug)) return;
    if (disabled) {
      // Invariant: never disable the last enabled org — CodeSense can't leave you
      // with zero orgs. A disable that would empty the enabled set is a no-op.
      const enabledAfter = state.orgs.filter((o) => o.slug !== slug && !current.has(o.slug));
      if (enabledAfter.length === 0) {
        logger.warn("org-store", `setOrgDisabled: refusing to disable last enabled org (${slug})`);
        return;
      }
      current.add(slug);
      // If we just disabled the ACTIVE org, switch to the first remaining enabled
      // one (setActiveOrg resolves its landing team) before persisting the set.
      if (state.activeOrgSlug === slug) {
        set({ disabledOrgSlugs: [...current], hasUserIntent: true });
        get().setActiveOrg(enabledAfter[0].slug);
        get().persistState();
        return;
      }
    } else {
      current.delete(slug);
    }
    set({ disabledOrgSlugs: [...current], hasUserIntent: true });
    get().persistState();
  },

  updateTeamSourceRepos: (orgSlug, teamName, slug) => {
    const orgs = get().orgs.map((org) => {
      if (org.slug !== orgSlug) return org;
      return {
        ...org,
        teams: org.teams.map((team) => {
          if (team.name !== teamName) return team;
          // Idempotent: the registry-change reload may already list the repo.
          const key = sourceRepoRemoteKey(slug);
          if (team.source_repos.some((sr) => sourceRepoRemoteKey(sr) === key)) return team;
          return { ...team, source_repos: [...team.source_repos, slug] };
        }),
      };
    });
    set({ orgs });

    // T-13: re-adding a repo clears any stale-after-removal flag for it, so a
    // remove→re-add cycle doesn't keep reporting `slug-stale-after-removal`.
    // Not strictly required for correctness (the current-repos lookup runs
    // first), but keeps the set from accumulating dead entries.
    const teamKey = buildTeamKey(orgSlug, teamName);
    const removed = get().recentlyRemovedRepos[teamKey];
    const key = removedRepoKey(slug);
    if (removed?.has(key)) {
      const next = new Set(removed);
      next.delete(key);
      set((s) => ({ recentlyRemovedRepos: { ...s.recentlyRemovedRepos, [teamKey]: next } }));
    }
    // No persistState() — orgs are ephemeral, re-derived from disk on startup.
  },

  removeTeamSourceRepo: (orgSlug, teamName, slug) => {
    const orgs = get().orgs.map((org) => {
      if (org.slug !== orgSlug) return org;
      return {
        ...org,
        teams: org.teams.map((team) => {
          if (team.name !== teamName) return team;
          return {
            ...team,
            source_repos: team.source_repos.filter(
              (sr) => sourceRepoRemoteKey(sr) !== sourceRepoRemoteKey(slug),
            ),
          };
        }),
      };
    });
    set({ orgs });

    // Drop the synthetic workspace repo entry.
    const key = syntheticRepoKey(orgSlug, teamName, slug);
    get().removeWorkspaceRepo(key);

    // T-13: record the removal so a later worktree open against this slug
    // reports `slug-stale-after-removal` rather than the generic
    // `slug-not-in-team`. This is the single explicit removal seam — the
    // `recentlyRemovedRepos` set is intentionally NOT maintained by a
    // `workspaceRepos` diff-watcher (which would false-positive on the
    // wholesale teardown/rebuild that happens on every team switch).
    const teamKey = buildTeamKey(orgSlug, teamName);
    const removedKey = removedRepoKey(slug);
    set((s) => ({
      recentlyRemovedRepos: {
        ...s.recentlyRemovedRepos,
        [teamKey]: new Set(s.recentlyRemovedRepos[teamKey]).add(removedKey),
      },
    }));
    // No persistState() — orgs are ephemeral, re-derived from disk on startup.
  },

  addTeamToStore: (orgSlug, team) => {
    const orgs = get().orgs.map((org) => {
      if (org.slug !== orgSlug) return org;
      // Idempotent: never double-add a team already present (guards against a
      // store/disk divergence on a name the backend accepted).
      if (org.teams.some((t) => t.name === team.name)) return org;
      return { ...org, teams: [...org.teams, team] };
    });
    set({ orgs });
    // No persistState() — orgs are ephemeral, re-derived from disk on startup.
  },

  updatePersonInStore: (orgSlug, person) => {
    const orgs = get().orgs.map((org) => {
      if (org.slug !== orgSlug) return org;
      const id = personStableId(person);
      if (!org.people.some((p) => personStableId(p) === id)) return org;
      return {
        ...org,
        people: org.people.map((p) => (personStableId(p) === id ? person : p)),
      };
    });
    set({ orgs });
    // No persistState() — orgs are ephemeral, re-derived from disk on startup.
  },

  addPersonToStore: (orgSlug, person) => {
    const id = personStableId(person);
    set((state) => ({
      orgs: state.orgs.map((org) =>
        org.slug !== orgSlug || org.people.some((candidate) => personStableId(candidate) === id)
          ? org
          : { ...org, people: [...org.people, person] },
      ),
    }));
  },

  createTeam: async (name) => {
    const trimmed = name.trim();
    const currentUser = getCurrentUser();
    const orgSlug = get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to create a team.");
    if (!orgSlug) throw new Error("No active workspace to create a team in.");

    const creatorId = currentUser.github_user_id;
    // Local-only git commit appending the team to `teams.yml` (SW-OB-8
    // seam). SW-TM-3: teams no longer carry members. The creator profile is
    // still passed so the backend records the creator in people.yml if absent
    // (open access — the joiner isn't otherwise seeded). Throws on a
    // duplicate/invalid name (backend is authoritative); the dialog surfaces
    // it inline.
    await commitRegistryEdit(creatorId, orgSlug, {
      type: "CreateTeam",
      team_name: trimmed,
      creator_id: creatorId,
      creator_username: currentUser.github_username,
      creator_display_name: currentUser.display_name,
      creator_avatar_url: currentUser.avatar_url,
    });

    // Reflect disk-truth in-memory so the UI updates instantly. The
    // in-memory add MUST precede setActiveTeam, which rejects a team not
    // present in the active org.
    get().addTeamToStore(orgSlug, {
      name: trimmed,
      source_repos: [],
    });
    // SW-OB-9 (AC-6): a just-created team must be visible. If the org has an
    // EXPLICIT shown set (curated subset), add the new team to it so it appears
    // in the switcher and the switch below lands active on a SHOWN team. An
    // absent shown set already shows all teams (including this one) — leave it
    // absent so it also stays show-all for future teams. Routed through the one
    // `setShownTeams` writer (which keeps the current active team, still shown).
    showTeam(orgSlug, trimmed);
    // Only switch into the new team if we're still in the org it was created
    // in — the user could have switched orgs during the in-flight commit, in
    // which case setActiveTeam (scoped to the active org) would no-op.
    if (get().activeOrgSlug === orgSlug) {
      get().setActiveTeam(trimmed);
    }
  },

  deleteTeam: async (name) => {
    const currentUser = getCurrentUser();
    const orgSlug = get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to delete a team.");
    if (!orgSlug) throw new Error("No active workspace to delete a team from.");
    // AWAIT the commit before reflecting: delete is guarded by a UI name-echo
    // confirm (AC-6) and is git-recoverable-but-not-obvious, so correctness
    // (don't drop the card before the commit lands) outranks the few-ms optimism
    // a reversible action would warrant (V-4). Throws on failure (backend is
    // authoritative — e.g. NotFound); the panel surfaces it inline.
    await commitRegistryEdit(currentUser.github_user_id, orgSlug, {
      type: "DeleteTeam",
      team_name: name,
    });
    await forgetDeletedTeam(orgSlug, name);
  },

  setTeamArchived: async (name, archived) => {
    const currentUser = getCurrentUser();
    const orgSlug = get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to archive a team.");
    if (!orgSlug) throw new Error("No active workspace.");
    // V-4: archive/unarchive is single-click, frequent, and fully reversible —
    // reflect OPTIMISTICALLY so the card moves to/from the Archived section
    // instantly and the hub never waits on the local git commit; roll back on
    // failure. Capture the prior flag AND the prior active team for a FULL
    // revert: archiving the active team optimistically reconciles active off it
    // (possibly to null when it was the last team), and a failed commit must
    // undo that too — else the user is stranded on a fallback (or the no-team
    // state) after an operation that "didn't happen".
    const prior =
      get()
        .orgs.find((o) => o.slug === orgSlug)
        ?.teams.find((t) => t.name === name)?.archived ?? false;
    const priorActiveTeam = get().activeTeamName;
    get().setTeamArchivedInStore(orgSlug, name, archived);
    try {
      await commitRegistryEdit(currentUser.github_user_id, orgSlug, {
        type: "ArchiveTeam",
        team_name: name,
        archived,
      });
    } catch (err) {
      // Revert the optimistic flip; the team is valid-and-openable again. Then
      // restore the active pointer if the optimistic reconcile moved it — but
      // only while we're still in the same org (an org switch during the
      // in-flight commit owns the active team now). Rethrow so the caller toasts.
      get().setTeamArchivedInStore(orgSlug, name, prior);
      if (get().activeOrgSlug === orgSlug && get().activeTeamName !== priorActiveTeam) {
        get().setActiveTeam(priorActiveTeam);
      }
      throw err;
    }
  },

  removeTeamFromStore: (orgSlug, name) => {
    const state = get();
    // Drop the team from the in-memory org list (mirror of addTeamToStore).
    const orgs = state.orgs.map((org) =>
      org.slug === orgSlug ? { ...org, teams: org.teams.filter((t) => t.name !== name) } : org,
    );
    // V-2: delete is permanent — prune a stale last-team pointer for this org
    // so a future same-named team can't inherit it. (resolveRestoredTeam would
    // reject a gone team anyway; this keeps the map clean.)
    const { [orgSlug]: lastForOrg, ...restLast } = state.lastTeamByOrgSlug;
    set({
      orgs,
      lastTeamByOrgSlug: lastForOrg === name ? restLast : state.lastTeamByOrgSlug,
    });
    // Prune the personal shown set through the ONE writer (AC-5), which also
    // reconciles the active team when it drops out. Only an EXPLICIT shown set
    // needs pruning; an absent set already show-alls the remaining teams.
    const shown = get().shownTeamsByOrgSlug[orgSlug];
    if (shown !== undefined) {
      get().setShownTeams(
        orgSlug,
        shown.filter((n) => n !== name),
      );
      return;
    }
    // Absent shown set: nothing to prune, but if the deleted team was the active
    // one, reconcile it off the (now-removed) team via defaultTeamForOrg.
    if (state.activeOrgSlug === orgSlug && state.activeTeamName === name) {
      const org = orgs.find((o) => o.slug === orgSlug);
      if (org) get().setActiveTeam(defaultTeamForOrg(org, get().shownTeamsByOrgSlug));
    }
  },

  setTeamArchivedInStore: (orgSlug, name, archived) => {
    const state = get();
    const orgs = patchTeamInOrgs(state.orgs, orgSlug, name, { archived });
    set({ orgs });
    // V-2: archive does NOT touch the personal shown set (reversibility). Only
    // reconcile the active team when ARCHIVING the currently-active one —
    // defaultTeamForOrg now excludes archived (V-1 fold), so it lands on a live
    // team (or null → no-team state). Unarchive never needs reconcile.
    if (archived && state.activeOrgSlug === orgSlug && state.activeTeamName === name) {
      const org = orgs.find((o) => o.slug === orgSlug);
      if (org) get().setActiveTeam(defaultTeamForOrg(org, get().shownTeamsByOrgSlug));
    }
  },

  renameTeam: async (oldName, newName) => {
    const trimmedNew = newName.trim();
    const currentUser = getCurrentUser();
    const orgSlug = get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to rename a team.");
    if (!orgSlug) throw new Error("No active workspace to rename a team in.");
    if (oldName === trimmedNew) return;

    // Backend is authoritative: it validates the new name (slug + `-meta` +
    // length + duplicate), moves `_team/{old}→{new}` + `installs/…` atomically,
    // and rolls back on any failure. Throws on rejection → dialog shows it.
    await commitRegistryEdit(currentUser.github_user_id, orgSlug, {
      type: "RenameTeam",
      old_name: oldName,
      new_name: trimmedNew,
    });

    // Reflect the confirmed rename in-memory, atomically, once. Org-store half
    // (this store) + viewer-store half (open tabs/terminals/layout survive).
    // Order: rewrite the viewer slices FIRST while `activeTeamName` still holds
    // the OLD name, then flip the org-store keys. Setting `activeTeamName`
    // old→new re-fires the team-activation effect (setActiveTeamForWave) which
    // re-points the active-team watcher/wave to the moved `_team/{new}` path —
    // the AC-9 re-registration, via the proven switch seam (no new lock dance).
    await get().applyTeamRename(orgSlug, oldName, trimmedNew);
  },

  applyTeamRename: async (orgSlug, oldName, newName) => {
    if (oldName === newName) return;
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    // Checked after the await: the registry-change reload and the UI's own
    // rename both land here for one rename, and only the first may move state.
    const org = get().orgs.find((o) => o.slug === orgSlug);
    if (!org?.teams.some((t) => t.name === oldName)) return;
    // The scope owner moves first, so what is keyed to the team has moved before anything that
    // watches the viewer's open teams sees the old key leave, and before the active name flips.
    warmScopes.rekey(buildTeamKey(orgSlug, oldName), buildTeamKey(orgSlug, newName));
    useViewerStore.getState().rekeyTeam(orgSlug, oldName, newName);
    set((state) => rekeyOrgStateForRename(state, orgSlug, oldName, newName));
    get().persistState();
  },

  applyOrgRegistryChange: async (org, change) => {
    const orgSlug = change.orgSlug;
    if (!get().orgs.some((o) => o.slug === orgSlug)) return;
    // Team-keyed state moves while the store still holds the old team, i.e.
    // before disk truth replaces the org below.
    if (change.edit === "RenameTeam" && change.previousTeam && change.team) {
      await get().applyTeamRename(orgSlug, change.previousTeam, change.team);
    } else if (change.edit === "DeleteTeam" && change.team) {
      await forgetDeletedTeam(orgSlug, change.team);
    }
    if (!org) return;
    // Disk truth replaces the in-memory org; `setOrgs` falls the active team
    // back if it is gone, archived or hidden.
    const orgs = get().orgs;
    if (!orgs.some((o) => o.slug === orgSlug)) return;
    get().setOrgs(orgs.map((o) => (o.slug === orgSlug ? org : o)));
    if (change.edit === "CreateTeam" && change.team) showTeam(orgSlug, change.team);
  },

  switchToTeam: (orgSlug, teamName) => {
    const org = get().orgs.find((o) => o.slug === orgSlug);
    if (!org?.teams.some((t) => t.name === teamName)) {
      logger.warn("org-store", `switchToTeam: team "${teamName}" not in org "${orgSlug}"`);
      return false;
    }
    // A soft-removed org is still a local org the backend switched into.
    if (get().disabledOrgSlugs.includes(orgSlug)) get().setOrgDisabled(orgSlug, false);
    // A hidden team would be switched straight off again by the shown-set
    // reconcile, so the team is made visible first.
    showTeam(orgSlug, teamName);
    get().setActiveTeamIn(orgSlug, teamName);
    return true;
  },

  setTeamParent: async (teamName, parent) => {
    const currentUser = getCurrentUser();
    const orgSlug = get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to change a team's parent.");
    if (!orgSlug) throw new Error("No active workspace to change a team's parent in.");

    // No-op short-circuit: skip a useless commit when the parent is unchanged.
    const org = get().orgs.find((o) => o.slug === orgSlug);
    const current = org?.teams.find((t) => t.name === teamName)?.parent_team ?? null;
    if ((parent ?? null) === current) return;

    // Backend is authoritative (byte-stable field flip in `teams.yml`, no
    // directory moves). Throws on rejection → the inline editor surfaces it.
    await commitRegistryEdit(currentUser.github_user_id, orgSlug, {
      type: "SetTeamParent",
      team_name: teamName,
      parent_team: parent,
    });

    // Reflect the confirmed parent in-memory — a single immutable field flip on
    // the matching team (shared `patchTeamInOrgs`, like `setTeamArchivedInStore`).
    // No re-key (a parent pointer moves nothing) and no `persistState()` (`orgs`
    // is ephemeral).
    set((state) => ({
      orgs: patchTeamInOrgs(state.orgs, orgSlug, teamName, { parent_team: parent }),
    }));
  },

  setTeamLandingPage: async (teamName, page, targetOrg) => {
    const currentUser = getCurrentUser();
    const orgSlug = targetOrg ?? get().activeOrgSlug;
    if (!currentUser) throw new Error("You must be signed in to change a team's starter page.");
    if (!orgSlug) throw new Error("No active workspace to change a team's starter page in.");
    const landingPage = page?.trim() || null;
    const org = get().orgs.find((o) => o.slug === orgSlug);
    const current = org?.teams.find((t) => t.name === teamName)?.landing_page ?? null;
    if (landingPage === current) return;

    await commitRegistryEdit(currentUser.github_user_id, orgSlug, {
      type: "SetTeamLandingPage",
      team_name: teamName,
      landing_page: landingPage,
    });
    set((state) => ({
      orgs: patchTeamInOrgs(state.orgs, orgSlug, teamName, { landing_page: landingPage }),
    }));
  },

  persistState: () => {
    // Suppress writes until rehydrate has completed. Pre-rehydrate
    // setters (system-driven `setOrgs` disk-loads in particular) must
    // NOT schedule a debounced save, because the save's 300ms timer
    // can race the async `loadAppState()` round-trip and write
    // in-memory defaults back to disk — clobbering the persisted
    // block before `loadPersistedState` reads it. Once `stateLoaded`
    // flips true, `loadPersistedState` itself triggers a flush to
    // capture any intervening setter writes.
    if (!get().stateLoaded) {
      return;
    }
    debouncedPersist.trigger();
  },

  loadPersistedState: async () => {
    logger.info("org-store", "loadPersistedState: loading");
    try {
      const raw = await loadAppState();
      const data = JSON.parse(raw || "{}");
      // SS-FU-12: rewrite the legacy `owner--repo` org slug to `owner/repo` in
      // the persisted org block (`activeOrgSlug` + `lastTeamByOrgSlug` keys)
      // before either restore branch reads it, so the restored selection
      // matches the now-`owner/repo` slugs the backend reports. Also run the
      // deep `/orgs/{owner}--{repo}/...` path rewrite as cheap insurance in
      // case any future field embeds such a path. Narrow + idempotent.
      if (data.org && typeof data.org === "object") {
        migrateOrgStoreOrgSlug(data.org as Record<string, unknown>);
        deepRewriteOrgSlugPaths(data.org);
      }
      const state = get();
      // If any setter fired before this async load resolved, user intent
      // is authoritative: drop the persisted block entirely. Per-field
      // merge is unsafe because `activeTeamName` is only meaningful in
      // the context of its `activeOrgSlug` — restoring a stale team into
      // a live org selection would leak cross-session state.
      if (state.hasUserIntent) {
        // A setter fired before rehydrate resolved — live state wins.
        // Do NOT flush to disk here: the setter may have only touched
        // a subset of fields, leaving others at null defaults. Writing
        // those defaults back to disk would destroy the persisted
        // `activeOrgSlug`/`activeTeamName` that the all-or-nothing
        // design deliberately kept out of live state. Any subsequent
        // user action will persist normally via the post-rehydrate
        // `persistState` path.
        //
        // F-39 review patch (P1): the drop-on-conflict rule above
        // applies to the slug/team PAIR (whose cross-field invariant
        // makes per-field merge unsafe). It does NOT apply to
        // `lastTeamByOrgSlug` — that map is independent of any single
        // live setter, and each entry stands on its own. Discarding it
        // here would cause data loss: the next `setActiveTeam` would
        // persist an in-memory map containing only the just-set entry,
        // overwriting every other org's remembered team on disk. Merge
        // persisted entries in, with live entries winning on key
        // collision.
        // F-39 review iter 1 (P1+P4): cold-start race coverage.
        //   1. `persistedMap` — explicit map from the persisted block.
        //   2. Migration seed — if no map but a non-null pre-upgrade
        //      pair exists, treat the pair as a one-entry seed so users
        //      upgrading mid-race don't lose their pre-upgrade team
        //      memory.
        //   3. After merging the map, try to restore the team for the
        //      live `activeOrgSlug` if a setter already auto-selected
        //      one (e.g., `useOrgDiscovery` picked `loadedOrgs[0]`
        //      before rehydrate resolved) but left `activeTeamName`
        //      null — without this the user's remembered team for that
        //      org is silently dropped because the merged map arrived
        //      after the selection.
        const persistedOrg = data.org as PersistedOrgState | undefined;
        const persistedMap = persistedOrg?.lastTeamByOrgSlug;
        const persistedSlug = persistedOrg?.activeOrgSlug ?? null;
        const persistedTeam = persistedOrg?.activeTeamName ?? null;
        const seededMap: Record<string, string> =
          persistedMap ??
          (persistedSlug && persistedTeam ? { [persistedSlug]: persistedTeam } : {});
        // SW-OB-9: the shown-visibility map is independent per-entry, exactly
        // like `lastTeamByOrgSlug` — merge persisted entries with live winning
        // on collision. It must be handled BEFORE the empty-seededMap early
        // return below: a user may have curated visibility (shown map non-empty)
        // even with no remembered-team memory, and dropping it here would lose
        // every org's curation on the next persist (mirror of the F-39 P1 trap).
        const mergedShownMap = {
          ...(persistedOrg?.shownTeamsByOrgSlug ?? {}),
          ...state.shownTeamsByOrgSlug,
        };
        // SW-UX: disabled-org set — union persisted + live (a simple flat list,
        // independent of the org/team invariant, so a plain union is correct).
        const mergedDisabled = Array.from(
          new Set([...(persistedOrg?.disabledOrgSlugs ?? []), ...state.disabledOrgSlugs]),
        );
        const mergedMap = { ...seededMap, ...state.lastTeamByOrgSlug };
        // SW-OB-9 (AC-6): a pre-rehydrate setter may have set a NON-null active
        // team using the then-empty live shown map (e.g. `setActiveOrg` landing
        // on the org default). If the just-merged persisted shown map HIDES that
        // team, active would sit outside the shown set. Reconcile it here
        // (mirrors `setOrgs`). `undefined` = leave active as-is; otherwise
        // override. Mutually exclusive with the null-active restoration below.
        let reconciledHiddenActive: string | null | undefined = undefined;
        if (state.activeTeamName !== null && state.activeOrgSlug !== null) {
          const liveOrg = state.orgs.find((o) => o.slug === state.activeOrgSlug);
          if (
            liveOrg &&
            !visibleTeams(liveOrg, mergedShownMap).some((t) => t.name === state.activeTeamName)
          ) {
            reconciledHiddenActive =
              resolveRestoredTeam(liveOrg, mergedMap, mergedShownMap) ??
              defaultTeamForOrg(liveOrg, mergedShownMap);
          }
        }

        if (Object.keys(seededMap).length === 0) {
          logger.info(
            "org-store",
            "loadPersistedState: setter already fired; keeping live state (merged shown-teams)",
          );
          set({
            stateLoaded: true,
            shownTeamsByOrgSlug: mergedShownMap,
            disabledOrgSlugs: mergedDisabled,
            ...(reconciledHiddenActive !== undefined
              ? { activeTeamName: reconciledHiddenActive }
              : {}),
          });
          return;
        }
        // Cold-start team-restoration: if a setter chose a slug but
        // left team null, see if the merged map has a remembered team
        // for that slug and the team is still in the org's team list.
        let restoredTeamForLiveSlug: string | null = null;
        if (state.activeOrgSlug !== null && state.activeTeamName === null) {
          const liveOrg = state.orgs.find((o) => o.slug === state.activeOrgSlug);
          if (liveOrg) {
            // SW-OB-9: restore only a SHOWN remembered team (AC-6) — use the
            // just-merged shown map so a hidden team isn't resurrected.
            restoredTeamForLiveSlug = resolveRestoredTeam(liveOrg, mergedMap, mergedShownMap);
          }
        }
        logger.info(
          "org-store",
          `loadPersistedState: setter already fired; merging persisted lastTeamByOrgSlug${
            restoredTeamForLiveSlug ? ` + restoring team "${restoredTeamForLiveSlug}"` : ""
          }`,
        );
        set({
          stateLoaded: true,
          lastTeamByOrgSlug: mergedMap,
          shownTeamsByOrgSlug: mergedShownMap,
          disabledOrgSlugs: mergedDisabled,
          // Null-active restoration and hidden-active reconciliation are
          // mutually exclusive (one needs active === null, the other !== null).
          ...(restoredTeamForLiveSlug !== null ? { activeTeamName: restoredTeamForLiveSlug } : {}),
          ...(reconciledHiddenActive !== undefined
            ? { activeTeamName: reconciledHiddenActive }
            : {}),
        });
        return;
      }

      if (data.org) {
        const {
          activeOrgSlug,
          activeTeamName,
          lastTeamByOrgSlug: persistedMap,
          shownTeamsByOrgSlug: persistedShownMap,
          disabledOrgSlugs: persistedDisabled,
        } = data.org as PersistedOrgState;

        // F-39 AC-7: capture raw persisted values BEFORE the slug/team
        // validation block — so the migration seed below sees the
        // pre-upgrade pair even when validation is about to invalidate
        // it. An invalid slug/team in the live `orgs` list is harmless
        // in the map (AC-6's `teamExists` filter handles validity at
        // restore time).
        //
        // F-39 review iter2 (P1): use truthy (`&&`) guards, NOT
        // `!== null`. A bug-written persisted JSON with `activeOrgSlug:
        // ""` would otherwise slip an empty-string slug into the map
        // and persist forever. Symmetric with the hasUserIntent merge
        // branch above.
        const persistedSlugRaw = activeOrgSlug ?? null;
        const persistedTeamRaw = activeTeamName ?? null;
        const nextLastTeamByOrgSlug: Record<string, string> =
          persistedMap ??
          (persistedSlugRaw && persistedTeamRaw ? { [persistedSlugRaw]: persistedTeamRaw } : {});

        // Validate the restored slug/team against the live `orgs` list
        // if it's already populated (e.g., a prior `setOrgs` call from
        // a disk-driven A.3+ loader). If `orgs` is empty — the common
        // cold-start case — trust the persisted slug; the first setter
        // that touches `orgs` will run its own validity check.
        let nextOrgSlug = activeOrgSlug ?? null;
        let nextTeamName = activeTeamName ?? null;
        if (nextOrgSlug !== null && state.orgs.length > 0) {
          const org = state.orgs.find((o) => o.slug === nextOrgSlug);
          if (!org) {
            logger.warn(
              "org-store",
              `loadPersistedState: persisted activeOrgSlug "${nextOrgSlug}" not in current orgs list; clearing`,
            );
            nextOrgSlug = null;
            nextTeamName = null;
          } else {
            // SW-OB-9: validate the persisted active team against the SHOWN
            // teams (the shown map being restored), not just existence — a
            // persisted team the user has since hidden must not reopen as active
            // (AC-6). Absent shown-set ⇒ all shown ⇒ same as the pre-OB-9 check.
            const coldShownMap = persistedShownMap ?? {};
            if (
              nextTeamName !== null &&
              !visibleTeams(org, coldShownMap).some((t) => t.name === nextTeamName)
            ) {
              logger.warn(
                "org-store",
                `loadPersistedState: persisted activeTeamName "${nextTeamName}" not shown in org "${nextOrgSlug}"; clearing team`,
              );
              nextTeamName = null;
            }
            // SW-OB-7: `loadPersistedState` is the THIRD writer of `activeTeamName`
            // (besides setOrgs/setActiveOrg). When `orgs` was already populated by
            // discovery BEFORE rehydrate AND the persisted active org still matches,
            // no later setOrgs/setActiveOrg runs to reconcile — so a persisted (or
            // just-invalidated) `null` team would be written straight through and
            // re-blank the screen (`${org}:_none` ≠ the viewer's restored
            // `openedTeams` entry). Land on the remembered-or-default team here too,
            // mirroring setOrgs. SW-OB-9: shown-set-aware, so an all-hidden org
            // stays `null` → the no-team state (App.tsx renders the empty state on
            // a null active team); only a genuinely team-less org keeps `null`.
            if (nextTeamName === null) {
              nextTeamName =
                resolveRestoredTeam(org, nextLastTeamByOrgSlug, coldShownMap) ??
                defaultTeamForOrg(org, coldShownMap);
            }
          }
        }

        // Cross-field invariant: a team is meaningless without an org.
        // Handles incoherent persisted blocks (null slug + non-null
        // team, e.g., from an interrupted write or prior bug).
        if (nextOrgSlug === null) {
          nextTeamName = null;
        }

        set({
          activeOrgSlug: nextOrgSlug,
          activeTeamName: nextTeamName,
          lastTeamByOrgSlug: nextLastTeamByOrgSlug,
          // SW-OB-9: restore the local visibility map verbatim (default `{}`);
          // no validation against `orgs` needed — `visibleTeams` filters names
          // to existing teams at read time, so a stale name is harmless.
          shownTeamsByOrgSlug: persistedShownMap ?? {},
          disabledOrgSlugs: persistedDisabled ?? [],
          stateLoaded: true,
        });
      } else {
        set({ stateLoaded: true });
      }
    } catch (e) {
      logger.error("org-store", `loadPersistedState: failed — ${e}`);
      set({ stateLoaded: true });
    }
  },
}));

/** The one writer of the warm scopes. Every way the active team changes (a context switch, the
 *  MCP, the picker, team create/delete fallbacks, a restored session) is a `set` of these two
 *  fields, so the scope is activated, and its team opened, by the store's own first subscriber: in
 *  the same synchronous tick as the write and before React's listeners schedule a render. No
 *  commit sees an active team that is not open. */
useOrgStore.subscribe((state, prev) => {
  if (state.activeOrgSlug === prev.activeOrgSlug && state.activeTeamName === prev.activeTeamName) {
    return;
  }
  warmScopes.activate(
    // The same key the workbench and the viewer sync use, so an org without a team is its `_none` scope.
    state.activeOrgSlug !== null ? buildTeamKey(state.activeOrgSlug, state.activeTeamName) : null,
  );
});

/** Persist the old profile before native switches its namespace. */
export async function flushOrgForProfileSwitch(): Promise<void> {
  if (!useOrgStore.getState().stateLoaded) return;
  debouncedPersist.trigger();
  await debouncedPersist.flushStrict();
}

/** Keep `teamName` visible in `orgSlug`. Only an EXPLICIT shown set can hide
 *  it; an absent set already shows every team (and stays absent so it also
 *  shows future ones). Routed through the one `setShownTeams` writer. */
function showTeam(orgSlug: string, teamName: string): void {
  const store = useOrgStore.getState();
  const shown = store.shownTeamsByOrgSlug[orgSlug];
  if (shown !== undefined && !shown.includes(teamName)) {
    store.setShownTeams(orgSlug, [...shown, teamName]);
  }
}

/** Drop a deleted team from in-memory state: the team itself (with its active
 *  fallback) and its maximized panel, so a future same-named team (same
 *  teamKey) can't resurface already-maximized. A no-op once the team is gone,
 *  so the UI's own delete and the registry-change reload can both run it. */
async function forgetDeletedTeam(orgSlug: string, teamName: string): Promise<void> {
  const org = useOrgStore.getState().orgs.find((o) => o.slug === orgSlug);
  if (!org?.teams.some((t) => t.name === teamName)) return;
  // Dynamic import avoids the org↔viewer static import cycle.
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  // A team may have been age-evicted already, leaving only its retained
  // workspace and terminal state. Deletion is destructive, so use closeTeam's
  // default cleanup before removing the team from the org store.
  useViewerStore.getState().closeTeam(buildTeamKey(orgSlug, teamName));
  warmScopes.forget(buildTeamKey(orgSlug, teamName));
  useOrgStore.getState().removeTeamFromStore(orgSlug, teamName);
  useViewerStore.getState().setMaximized(buildTeamKey(orgSlug, teamName), null);
}
