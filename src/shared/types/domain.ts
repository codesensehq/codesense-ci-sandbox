// SPDX-License-Identifier: MIT
// NOTE ON NUMERIC USER IDs:
// GitHub numeric user IDs (github_user_id) are typed as `number` here.
// GitHub's current max numeric ID is ~9 digits, well inside
// Number.MAX_SAFE_INTEGER (2^53-1 ≈ 16 digits). No BigInt needed for the
// foreseeable future. Do NOT switch to string/BigInt without an explicit
// migration plan — callers throughout the app assume `number`.

/** SW-FG-8 (AC-8): the provider-neutral binding to a signed-in connection — the FE
 *  mirror of the Rust `CredentialRef`. `account` is the provider's stable id
 *  (GitHub's numeric `user_id` rendered to string). Replaces the GitHub-only
 *  `github_account_id`. */
export interface CredentialRef {
  provider: string;
  account: string;
}

export interface Attachment {
  repo_path: string;
  remote_url: string | null;
  attached_at: string | null;
  tracking_prompt_dismissed_at: string | null;
  // SW-FG-8 (AC-8): the connection this source repo is bound to, as a
  // provider-neutral `credential_ref (provider, account)`. `null` = unbound
  // (legacy record or never set) → the active connection is used. Mirrors Rust
  // `Attachment.credential_ref: Option<CredentialRef>`.
  credential_ref: CredentialRef | null;
  // SW-MC-1: ownership marker. `true` ⇒ codesense cloned and manages this dir
  // (may pull/GC it); absent/`false` ⇒ user-attached, hands-off. Optional so
  // legacy records and attach-existing writes that omit it default to false
  // (mirrors Rust `#[serde(default)] managed: bool`).
  managed?: boolean;
}

export interface Person {
  /** Stable on current payloads. Optional only at the compatibility boundary for
   *  persisted/test fixtures created before roster IDs were introduced. Code that
   *  needs a key must use `personStableId` rather than reading this directly. */
  id?: string;
  /** Present on current payloads; absent only on legacy fixtures/persisted data. */
  identities?: Identity[];
  github_user_id: number | null;
  // SW-FG-4b (AC-7): a handle-less GitHub teammate (added, not yet linked)
  // composes into the roster with a null handle — so login-keying read-sites
  // (`buildPeopleByLogin`, the members add-dup check) must tolerate null. The FE
  // reshape onto a provider identity list is FG-4c.
  github_username: string | null;
  display_name: string;
  avatar_url: string | null;
}

/** The current roster payload contract after legacy data has been composed. */
export type StablePerson = Person & Required<Pick<Person, "id" | "identities">>;

export interface Identity {
  provider: string;
  handle?: string | null;
  provider_user_id?: string | null;
}

export interface SourceRepoSlug {
  owner: string;
  repo: string;
  /** Canonical code-host provider id composed from teams.yml. */
  provider?: string;
  /** Canonical git-remote host composed by the backend. */
  host?: string;
}

export interface Team {
  name: string;
  source_repos: SourceRepoSlug[];
  /** SW-HB-7: org-wide, reversible retirement. Absent ⇔ active (mirrors the
   *  Rust `Team.archived` serde default). Archived teams are excluded from
   *  every default surface via the `visibleTeams` fold (V-1) — distinct from
   *  the per-user `shownTeamsByOrgSlug` hide. */
  archived?: boolean;
  /** SLW-IN-1: the single parent team this team inherits from, or absent/null
   *  for a root team (mirrors the Rust `Team.parent_team` serde default).
   *  Content resolves self → parent → … → root, nearer-wins (SLW-IN-2).
   *  The parent picker + chain breadcrumb that surface this are SLW-IN-5. */
  parent_team?: string | null;
  /** SLW-PT-1: DTO-only marker (mirrors the Rust `Team.is_personal` serde
   *  default) set only on a synthesized **provider** (personal-team) row, so
   *  the Home Base card can render the flagship `Local`/private legibility.
   *  Never persisted to `teams.yml`; absent ⇔ an ordinary team. */
  is_personal?: boolean;
  /** The team's home page, opened whenever it has nothing open; relative to the team's content
   *  root (`{org_root}/{team}/content`). Mirrors the Rust `Team.landing_page`
   *  (`teams.yml` `landing_page:`). Absent ⇒ README.md / README.html /
   *  index.html at the content root, else nothing. */
  landing_page?: string | null;
}

// Per-org config flags, mirroring Rust `OrgConfigFlags` (serialized from
// `org.yml::config_flags`). Every field is optional so a legacy org without
// the key deserializes clean.
export interface OrgConfigFlags {
  // Where codesense lands managed source clones for this org. `null`/absent ⇒
  // the hidden default under `~/.codesense/users/{uid}/sources/`; a string ⇒ a
  // visible workspace root (clones go to `{root}/{owner}/{repo}/`).
  workspace_root_dir?: string | null;
}

export interface Org {
  /** User-owned checkout; Git operations belong to the user. */
  is_attached?: boolean;
  slug: string;
  display_name: string;
  // ISO-8601 UTC, serialized from chrono::DateTime<Utc> on the Rust side.
  created_at: string;
  remote_url: string | null;
  /** Canonical provider derived by the backend from the context remote. */
  remote_provider?: string | null;
  config_flags: OrgConfigFlags;
  people: Person[];
  teams: Team[];
  /** Machine-local connection bound to this context repo. */
  credential_ref?: CredentialRef | null;
  org_root: string;
  /** Runtime-only backend-resolved physical base paths, keyed by
   *  `team\ncanonicalRemoteKey`. Never authored or persisted by the renderer. */
  resolved_repository_paths?: Record<string, string>;
}

export interface OrgSummary {
  is_attached?: boolean;
  availability_error?: string | null;
  slug: string;
  display_name: string;
  has_remote: boolean;
  // F-38: origin URL from the context repo's .git/config. `null` when no
  // remote exists (or the URL is non-UTF-8). Snake_case mirrors the Rust
  // struct (no serde camelCase rename on OrgSummary).
  remote_url: string | null;
  org_root: string;
}

export interface CurrentUser {
  /** Stable CodeSense identity. Optional only for legacy persisted/test snapshots. */
  principal?: string;
  github_user_id: number;
  github_username: string;
  display_name: string;
  avatar_url: string | null;
  /** The viewer's chosen face, either `pack:id` or a `data:` URL. Null when
   *  nothing is chosen — renderers fall back to initials of `display_name`. */
  identity_avatar?: string | null;
}
