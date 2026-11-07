// SPDX-License-Identifier: MIT
//! Layer identity and per-layer source-path resolution — SS.3.
//!
//! SW-XS-8 retired the install tiers (`TeamRepo` / `TeamWorkspace`) together
//! with the install-tier resolver that walked them: nothing delivers them, the
//! SW-XS-14 boot migration folds them into `content/`, and content reaches a
//! workspace only through the preparation engine (`agents::prepare`). The one
//! remaining layer is the team's own content ([`LayerId::TeamProjection`]).
//! [`ResolverContext`] carries the repo-agnostic identity (`codesense_home`,
//! `user_id`, `org_slug`, `team_name`) needed to find a team's
//! content root.

use std::path::{Path, PathBuf};

use codesense_fs_core::filters::{is_runtime_state, SECRET_GLOBS};
use log::warn;
use serde::{Deserialize, Serialize};

use crate::domain::Principal;

/// The content layer a file lives in.
///
/// **On-wire serialization** is by variant name. The retired install tiers are
/// gone. Inbound, `TeamWorkspace` (the old repo-agnostic default)
/// still loads as team content; `TeamRepo` (the per-repo tier) is refused
/// with a pointer to the repo tier's publish (`reverse_sync_worktree
/// {to:"repo"}`) — it must never land team-wide (review r1). The lockfile /
/// install-state readers go through [`super::lockfile::WireLayerId`], which
/// maps stored rows (and drains the retired tiers' wave rows).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
pub enum LayerId {
    /// The team's own content — `{org_root}/{team}/content/`.
    TeamProjection = 2,
}

/// The message an inbound retired `TeamRepo` gets.
pub const TEAM_REPO_RETIRED: &str = "TeamRepo (the per-repo install tier) is retired; to put a file in this repo's context tier, publish it with reverse_sync_worktree {to:\"repo\"}";

impl<'de> Deserialize<'de> for LayerId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        match name.as_str() {
            "TeamProjection" | "TeamWorkspace" => Ok(LayerId::TeamProjection),
            "TeamRepo" => Err(serde::de::Error::custom(TEAM_REPO_RETIRED)),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["TeamProjection"],
            )),
        }
    }
}

/// Every [`LayerId`], in order.
pub const PRIORITY_ORDER: &[LayerId] = &[LayerId::TeamProjection];

/// Repo-agnostic identity for a resolver call. Carries everything that
/// stays constant across a single team's worth of materialization
/// activity. Content roots do not depend on a source repository.
///
/// `user_id` stays the raw GitHub numeric id — the provider-id *attribute* of
/// the signed-in account (SW-FG-1 AC-2). Path-scope callers get the identity
/// key via [`ResolverContext::principal`], the single conversion point, rather
/// than re-deriving `ctx.principal()` at each site.
#[derive(Debug, Clone)]
pub struct ResolverContext {
    pub context_root: Option<crate::agents::manifest_resolve::ContextRoot>,
    /// Root of the codesense user-state tree on disk (typically
    /// `~/.codesense/`). Injected (not read from
    /// [`crate::commands::paths::codesense_home_dir`]) so the resolver
    /// stays pure per SS-3 AC-4.
    pub codesense_home: PathBuf,
    /// GitHub numeric user id — the per-user namespace key under
    /// `{codesense_home}/users/{user_id}/`.
    pub user_id: u64,
    /// Org slug — the per-org namespace key under
    /// `{codesense_home}/users/{user_id}/orgs/{org_slug}/`.
    pub org_slug: String,
    /// Active team name. `None` means no team layer: [`content_layer_root`]
    /// returns `None`.
    pub team_name: Option<String>,
}

impl ResolverContext {
    pub fn principal(&self) -> Principal {
        Principal::from(self.user_id)
    }

    pub(crate) fn view(
        &self,
    ) -> Option<std::sync::Arc<crate::context_repo::inheritance::TeamView>> {
        let team = self.team_name.as_deref()?;
        let root = crate::agents::roots::org_root(
            &self.codesense_home,
            &self.principal(),
            &self.org_slug,
            self.context_root.as_ref(),
        )
        .ok()?;
        Some(crate::context_repo::inheritance::team_view(
            &root,
            &self.org_slug,
            team,
        ))
    }
}

/// The team's content root, including an explicitly chosen context checkout.
/// Missing directories still resolve, so write destinations can create them.
pub fn content_layer_root(layer: LayerId, ctx: &ResolverContext) -> Option<PathBuf> {
    match layer {
        LayerId::TeamProjection => crate::agents::roots::team_content_dir_at(
            &ctx.codesense_home,
            &ctx.principal(),
            &ctx.org_slug,
            ctx.team_name.as_deref()?,
            ctx.context_root.as_ref(),
        )
        .ok(),
    }
}

/// Every logical file name under `layer`'s root, sorted, `/`-separated,
/// without ignored or runtime-state files. A missing root lists nothing.
/// (It replaces the install-tier resolver's enumeration, deleted with the
/// install tiers in SW-XS-8.)
pub fn list_layer(layer: LayerId, ctx: &ResolverContext) -> Vec<String> {
    let Some(root) = content_layer_root(layer, ctx) else {
        return Vec::new();
    };
    if root.symlink_metadata().is_ok_and(|m| m.is_symlink()) || !root.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<String> = walkdir::WalkDir::new(&root)
        .min_depth(1)
        .max_depth(64)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(&root).ok()?.to_str()?;
            let logical = rel.replace('\\', "/");
            (!codesense_fs_core::filters::is_ignored(&logical) && !is_runtime_state(&logical))
                .then_some(logical)
        })
        .collect();
    out.sort();
    out
}

/// Validate a logical name as safe to use as a relative path under any
/// layer root (filesystem or embedded). Rejects:
///
/// - Empty strings.
/// - Absolute paths (leading `/`, leading `\`, or Windows drive prefixes
///   like `C:\foo` / `C:/foo`).
/// - Backslashes anywhere (codesense uses forward-slash logical names;
///   a backslash on Unix would parse as part of a `Component::Normal`
///   and silently bypass `Path::components()`-based checks).
/// - Embedded NUL bytes.
/// - Any path segment equal to `.` or `..` (catches interior `/./` and
///   `/../` that `Path::components()` would silently normalize away).
/// - Empty segments (`//`, trailing `/`).
///
/// Used by every layer read and write. Needs this safety bar because
/// `PathBuf::join("/etc/passwd")` discards the layer root and would leak
/// files outside the install dir.
///
/// Operates on the raw string rather than via `Path::components()` to
/// catch interior `.` / `..` segments — `Path::components()` filters
/// out interior `.` segments as "redundant," so a component-match
/// approach would let `foo/./bar` pass while `include_dir::get_file`
/// (literal-key lookup) would not match.
pub fn is_safe_logical_name(logical_name: &str) -> bool {
    if logical_name.is_empty() {
        return false;
    }
    // P10 (iter-3): reject ALL control characters, not just NUL.
    // Parallels `validate_path_segment`'s `c.is_control()` sweep and
    // closes display-attack surface (RTL override `\u{202E}`, BOM
    // `\u{FEFF}`, other zero-width controls). `is_control()` subsumes
    // the earlier explicit NUL check.
    if logical_name.chars().any(|c| c.is_control()) {
        return false;
    }
    if logical_name.contains('\\') {
        return false;
    }
    if logical_name.starts_with('/') {
        return false;
    }
    // Windows drive prefix — letter + colon at the start of the string.
    // Tightened from the earlier "any byte == ':' at index 1" check, which
    // over-rejected legitimate POSIX filenames like `a:b/c.md` (per iter-2
    // review P5). Only `[A-Za-z]:` qualifies as a drive prefix.
    let bytes = logical_name.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    for seg in logical_name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
    }
    true
}

/// Validate `name` as safe to use as a single install-path segment
/// (org_slug / team / owner / repo under the user `installs/` root).
/// Returns `Some(reason)` if `name` is unsafe; returns `None` if accepted.
///
/// Shared by [`crate::content_engine::lockfile::lockfile_path`] (org_slug
/// and team_name validation) and by [`content_layer_root`] (defensive
/// validation of `ctx.org_slug` / `ctx.team_name` before path construction
/// — keeps the resolver from escaping `codesense_home` when handed an
/// unsafe context).
///
/// Rule set (iter-2 reshape, replaces the earlier `lockfile.rs`-local
/// validator):
///
/// - Empty string → reject.
/// - Embedded NUL → reject.
/// - Contains `/` or `\` → reject (path separator).
/// - `name != name.trim()` (leading or trailing whitespace) → reject.
///   This catches `"  acme  "` (which would otherwise create a distinct
///   path segment from `"acme"`) AND whitespace-only strings (which
///   trim to empty, so `name != name.trim()` is true). It also catches
///   trim-bypass attempts like `" .. "` and `" .foo"` — both surface
///   as "leading or trailing whitespace" before any traversal or
///   leading-dot rule fires.
/// - Equals `.` or `..` (exact match after the whitespace gate) →
///   reject (path traversal).
/// - Any `c.is_control()` character → reject.
///
/// **Not in the rule set (intentional):** leading-dot. Iter-1's
/// leading-dot rejection was reverted in iter-2 — `.github` is a
/// legitimate slug name (one of GitHub's own org/repo conventions),
/// and the original rationale ("hidden directories under
/// `_codesense/installs/`") was operationally weak. Hidden dirs under
/// `codesense_home` aren't user-visible during normal use, and
/// rejecting them creates an inconsistency with `SourceRepoSlug`
/// (which accepts `.github`-style repo names).
pub fn validate_path_segment(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty");
    }
    if name.contains('\0') {
        return Some("contains NUL");
    }
    if name.contains('/') || name.contains('\\') {
        return Some("contains path separator");
    }
    if name != name.trim() {
        return Some("leading or trailing whitespace");
    }
    if name == "." || name == ".." {
        return Some("path traversal");
    }
    if name.chars().any(|c| c.is_control()) {
        return Some("contains control character");
    }
    None
}

/// The org-slug path-safety gate. Its body lives in [`crate::context_repo::
/// org_slug`] alongside the rest of the slug ⇄ `(owner, repo)` logic (SS-FU-12
/// AC-8); re-exported here so the resolver gate and `content_engine`'s public
/// surface keep referring to it unchanged.
pub use crate::context_repo::org_slug::validate_org_slug;

/// SW-CP-28: compile a `GlobSet` from RUNTIME (team-supplied) patterns. Unlike
/// the builder behind the compile-time `IGNORE_GLOBS`/`SECRET_GLOBS` sets — which
/// panics on an invalid glob — a team's `projection-excludes.yml` is user input, so one bad
/// pattern is skipped with a `warn!` rather than panicking or wedging the whole
/// projection wave. An empty/all-invalid set builds to an empty `GlobSet` (matches
/// nothing), so a broken config degrades to "no team override", never to a crash.
///
/// SW-XS-12: `case_insensitive` builds a set that matches every case variant —
/// the exclude set, so `config/.ENV` (the same file as `config/.env` on macOS)
/// never projects. The allow set (a team's `!` negations) stays exact, so a
/// negation never opens a case variant it did not name.
fn build_globset_lenient(
    label: &str,
    patterns: &[&str],
    case_insensitive: bool,
) -> globset::GlobSet {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let glob = if case_insensitive {
            codesense_fs_core::filters::case_insensitive_glob(pattern)
        } else {
            globset::Glob::new(pattern)
        };
        match glob {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => warn!(
                target: "content_engine",
                "skipping invalid {label} glob {pattern:?}: {e}"
            ),
        }
    }
    builder
        .build()
        .unwrap_or_else(|_| globset::GlobSet::empty())
}

#[cfg(test)]
thread_local! {
    /// Test-only: behave like a build before SW-LN-B48 (runtime state projected), so
    /// a test can set up the stale worktree copy the fix must clean up.
    pub(crate) static LEGACY_RUNTIME_STATE_PROJECTION: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn legacy_runtime_state_projection() -> bool {
    LEGACY_RUNTIME_STATE_PROJECTION.with(|c| c.get())
}

#[cfg(not(test))]
fn legacy_runtime_state_projection() -> bool {
    false
}

/// SW-CP-28: the resolved, team-aware projection-exclude set — the configurable
/// generalization of `is_secret_path`. Two cached globsets over the layered
/// model:
/// - `exclude` = the [`SECRET_GLOBS`] defaults PLUS any team-added globs (runtime
///   state is excluded unconditionally ahead of both sets — see `is_excluded`);
/// - `allow` = the team's `!`-negations (gitignore-style) that opt a default (or
///   any path) back INTO projection.
///
/// A path is excluded iff it matches `exclude` and is NOT matched by `allow`, so a
/// team can EXTEND the defaults or NEGATE one, while the defaults remain the single
/// source of truth in code. Direction-agnostic: the same resolved set gates forward
/// materialize AND reverse adoption at every projection seam, so a negated default
/// is allowed symmetrically in both directions.
///
/// Resolve ONCE per wave (it compiles two globsets) and pass by reference to
/// [`is_projection_excluded`] at each seam — never rebuild per candidate file.
#[derive(Debug)]
pub struct TeamExcludes {
    exclude: globset::GlobSet,
    allow: globset::GlobSet,
}

impl TeamExcludes {
    /// Resolve from the raw team override patterns exactly as written in
    /// `projection-excludes.yml`. A leading `!` marks a negation (allow); every
    /// other entry is an added exclude. The [`SECRET_GLOBS`] defaults are always
    /// in the exclude set. Invalid team globs are skipped (lenient) so one typo
    /// never blocks a wave.
    pub fn resolve(patterns: &[String]) -> Self {
        let mut exclude: Vec<&str> = SECRET_GLOBS.to_vec();
        let mut allow: Vec<&str> = Vec::new();
        for p in patterns {
            // Trim incidental whitespace (a hand-edited `projection-excludes.yml`
            // can carry stray padding) and DROP a degenerate empty / bare-`!` entry
            // — otherwise `Glob::new("")` would compile an inert matcher or a padded
            // pattern would match literal-space paths, both silently wrong.
            let trimmed = p.trim();
            if let Some(negated) = trimmed.strip_prefix('!') {
                let negated = negated.trim();
                if !negated.is_empty() {
                    allow.push(negated);
                }
            } else if !trimmed.is_empty() {
                exclude.push(trimmed);
            }
        }
        // SW-XS-12: a behaviour change for team-added excludes. They used to
        // match case-sensitively (globset's default); now they match every case,
        // like the SECRET_GLOBS defaults they join, because on macOS
        // `Infra/Prod.TFSTATE` is the same file as `infra/prod.tfstate`. A team
        // exclude can only hide more, so this fails closed. The `!` negations
        // stay case-exact: a negation re-includes only the spelling it names.
        TeamExcludes {
            exclude: build_globset_lenient("projection-exclude", &exclude, true),
            allow: build_globset_lenient("projection-allow", &allow, false),
        }
    }

    /// The no-config default: [`SECRET_GLOBS`] with no team overrides (runtime state is
    /// always excluded on top). Used for the unconfigured team and for reverse vectors
    /// with no resolvable team.
    pub fn default_only() -> Self {
        Self::resolve(&[])
    }

    /// `true` when `logical_name` must NOT project (forward) or be adopted
    /// (reverse): it matches an exclude and is not negated by an allow.
    pub fn is_excluded(&self, logical_name: &str) -> bool {
        // SW-LN-B48: runtime state (SQLite DBs + sidecars, the Weft ledger) is never
        // team context — excluded in both directions and NOT negatable by a team.
        if is_runtime_state(logical_name) && !legacy_runtime_state_projection() {
            return true;
        }
        self.exclude.is_match(Path::new(logical_name))
            && !self.allow.is_match(Path::new(logical_name))
    }
}

/// SW-CP-28: the team-aware projection-exclude predicate that supersedes the
/// const-backed `is_secret_path` at every projection seam (forward top-level
/// file + dir, the nested subtree walk, merge projection, and reverse
/// adopt-refusal). Pure: it takes the PRE-RESOLVED [`TeamExcludes`] so the seams
/// stay I/O-free — resolve once per wave, match many times.
pub fn is_projection_excluded(logical_name: &str, excludes: &TeamExcludes) -> bool {
    excludes.is_excluded(logical_name)
}

/// SW-CP-28: best-effort validation of a single team-supplied exclude glob for the
/// Settings write path, so the UI can surface a bad pattern inline. Whitespace is
/// trimmed and a leading `!` (negation) is stripped before the parse. Unlike the
/// lenient resolve path (which silently drops a degenerate entry so a hand-edited
/// file never wedges a wave), the WRITE path is strict: an empty pattern (or a bare
/// `!`) is rejected with a clear error rather than persisted as an inert glob.
pub fn validate_exclude_glob(pattern: &str) -> Result<(), String> {
    let trimmed = pattern.trim();
    let bare = trimmed.strip_prefix('!').unwrap_or(trimmed).trim();
    if bare.is_empty() {
        return Err("empty glob pattern".to_string());
    }
    globset::Glob::new(bare)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// D-16: return a clone of `ctx` with `team_name` swapped. Pure data
/// helper for the team-switch teardown: synthesizes the OLD-team
/// ctx from the wave's NEW-team ctx by overriding `team_name`. Cheap
/// clone — `codesense_home` PathBuf is the only non-trivial field.
pub fn ctx_with_team(ctx: &ResolverContext, team_name: Option<String>) -> ResolverContext {
    ResolverContext {
        context_root: ctx.context_root.clone(),
        codesense_home: ctx.codesense_home.clone(),
        user_id: ctx.user_id,
        org_slug: ctx.org_slug.clone(),
        team_name,
    }
}

/// SLW-IN-2 / LT-3: the per-hop resolver context for `team` inside a chain
/// walk. Borrows `ctx` when `team` is already the active team — the common
/// length-1 chain, zero-clone — and clones a team-swapped copy only for genuine
/// parent hops. Centralizes the reuse-else-swap idiom every chain walker shares
/// (the wave engine's watch-root fan-out), so the
/// hot-path zero-clone optimization lives in exactly one place.
pub fn hop_ctx<'a>(ctx: &'a ResolverContext, team: &str) -> std::borrow::Cow<'a, ResolverContext> {
    if ctx.team_name.as_deref() == Some(team) {
        std::borrow::Cow::Borrowed(ctx)
    } else {
        std::borrow::Cow::Owned(ctx_with_team(ctx, Some(team.to_string())))
    }
}

/// SLW-MD-1 (Task 3.3): is `path` a **read-only projected / installed** file
/// rather than a writable team-workspace content-root original?
///
/// No reverse `path → is-installed-projection` classifier existed before MD-1
/// ([`content_layer_root`] is forward-only). Rather than enumerate every
/// installed-layer root (installs dir, org/personal repo trees, epic worktrees,
/// source clones, worktree branches) — an open-ended set — MD-1 uses the
/// **content-root gate** the story sanctions as the fallback: the *only*
/// writable location is a team-workspace content root, whose canonical flat
/// layout is `…/{team}/{TEAM_CONTENT_DIR}/…` (i.e. `{team}/content/…`, see
/// [`crate::context_repo::path_to_team_content_dir`]).
/// Anything NOT under such a segment sequence is treated as installed/projected
/// ⇒ read-only ⇒ the write is rejected.
///
/// This is intentionally conservative (reject-by-default): a caller-supplied
/// "read-only" flag is never trusted, and an unrecognized path is refused, not
/// waved through. `apply_metadata_edit` gates every write on `!is_installed_path`.
///
/// `codesense_home` is the absolute anchor (SLW-IN-11 hardening): a path is
/// writable ONLY at the exact layout offset under the home —
/// `{home}/users/{uid}/orgs/{owner}/{repo}/{team}/` followed by `content/…` (team
/// content, SLW-UD-8) or `repos/{o}/{r}/content/…` (per-repo content, SLW-IN-11).
/// Anchoring on the real root — rather than pattern-matching an `orgs/…/content`
/// window anywhere in the path — is what makes the guard sound: a path OUTSIDE the
/// home (a symlink `resolve_writable_target` canonicalized into some other tree,
/// or a crafted arbitrary path) fails `strip_prefix(home)` and is refused, and a
/// nested `orgs/…/content` look-alike buried inside a read-only `install/` subtree
/// can't masquerade as a content root because only the fixed offsets under the
/// home are inspected. Both `home` and `path` MUST be canonicalized by the caller
/// so the prefix comparison is exact.
///
/// Note (SLW-MD-1 scope): the writable gate is deliberately narrow to
/// **team-content** (`{team}/content`) only. Any `_`/`.`-prefixed segment at the
/// team slot (e.g. the surviving `_codesense/`) is NOT a valid team, so paths
/// under it fail the team check and stay read-only here. That is by design, not
/// an oversight.
pub fn is_installed_path(path: &Path, codesense_home: &Path) -> bool {
    use crate::context_repo::{ORGS_DIR, REPOS_DIR, TEAM_CONTENT_DIR, USERS_DIR};

    // SLW-MD-1 (write-guard bypass fix): a `..` (ParentDir) component makes the
    // path un-provable — the OS resolves it *after* this guard runs, so a path
    // like `…/{team}/content/../install/foo.md` would drop the `..` here
    // (component filter keeps only `Normal`), match the content-root anchor,
    // and be waved through as "writable" while the actual write lands in the
    // read-only `…/install/` tree. Reject any path bearing a `..` component:
    // if it can't be proven contained lexically, it is treated as installed
    // (read-only). Callers MUST feed the SAME path to the write, so what the
    // guard proves is what gets written.
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return true;
    }

    match crate::context_repo::context_attachments::path_is_registered_content(codesense_home, path)
    {
        Ok(true) => return false,
        Ok(false) => {}
        Err(error) => {
            log::warn!("{error}");
            return true;
        }
    }
    // Absolute anchor: everything else writable lives under the codesense home. A path
    // that isn't (a symlink-escaped canonicalization, a crafted arbitrary path) is
    // refused outright — no window-matching a look-alike could satisfy.
    let Ok(rel) = path.strip_prefix(codesense_home) else {
        return true;
    };
    let rel: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => os.to_str(),
            _ => None,
        })
        .collect();

    // rel = users / {uid} / orgs / {owner} / {repo} / {team} / {marker} / …
    if rel.first() != Some(&USERS_DIR) || rel.get(2) != Some(&ORGS_DIR) {
        return true;
    }
    let Some(team) = rel.get(5) else {
        return true;
    };
    if team.is_empty() || team.starts_with('_') || team.starts_with('.') {
        return true;
    }
    let is_content_root = match rel.get(6) {
        Some(&TEAM_CONTENT_DIR) => true,
        Some(&REPOS_DIR) => rel.get(9) == Some(&TEAM_CONTENT_DIR),
        _ => false,
    };

    !is_content_root
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_fs_core::filters::is_secret_path;

    fn ctx(team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/home/dev/.codesense"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: team.map(String::from),
        }
    }

    /// The canonical codesense-home prefix the `is_installed_path` test paths are
    /// rooted under (`{home}/users/{uid}/orgs/…`). A path outside this home
    /// (`/tmp/…`, a bare name) fails the `strip_prefix` anchor and is read-only.
    fn home() -> PathBuf {
        PathBuf::from("/home/dev/.codesense")
    }

    // ----- SLW-MD-1: is_installed_path (write-guard) -------------------------

    #[test]
    fn is_installed_path_content_root_is_writable() {
        // A team-workspace content-root file is writable (NOT installed).
        let p = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/content/_bmad-output/story.md",
        );
        assert!(
            !is_installed_path(&p, &home()),
            "content-root file must be writable"
        );
    }

    #[test]
    fn is_installed_path_install_tree_is_readonly() {
        // The team install tree is a projected/installed layer → read-only.
        let p = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/install/_bmad-output/story.md",
        );
        assert!(
            is_installed_path(&p, &home()),
            "install-tree file must be read-only"
        );
    }

    /// SLW-IN-11 AC-6: a per-repo `content/` file is writable (two-way), while the
    /// sibling per-repo `install/` file stays read-only. Both have `repos` as the
    /// first layer-marker — the segment three past `repos` is the discriminator.
    #[test]
    fn is_installed_path_per_repo_content_is_writable_install_is_readonly() {
        let content = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense/content/docs/d.md",
        );
        assert!(
            !is_installed_path(&content, &home()),
            "per-repo content file must be writable"
        );
        let install = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense/install/docs/d.md",
        );
        assert!(
            is_installed_path(&install, &home()),
            "per-repo install file must stay read-only"
        );
        // `_epic` pseudo-repo per-repo content is writable too (owner may be `_`).
        let epic = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/_epic/an-epic/content/docs/d.md",
        );
        assert!(
            !is_installed_path(&epic, &home()),
            "epic per-repo content file must be writable"
        );
    }

    /// SLW-IN-11 regression: an org slug segment literally named `repos` (a legal
    /// GitHub repo name) must NOT shadow the real team-adjacent marker. Scanning
    /// only the FIRST marker would match the org-slug `repos` (not team-anchored)
    /// and wrongly classify a genuine content root as read-only.
    #[test]
    fn is_installed_path_org_slug_named_repos_does_not_shadow_marker() {
        // org = someorg/repos → context path contains a `repos` segment before the team.
        let team_content = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/someorg/repos/default/content/docs/d.md",
        );
        assert!(
            !is_installed_path(&team_content, &home()),
            "team content under an org named `repos` must stay writable"
        );
        let per_repo_content = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/someorg/repos/default/repos/alderco/codesense/content/docs/d.md",
        );
        assert!(
            !is_installed_path(&per_repo_content, &home()),
            "per-repo content under an org named `repos` must stay writable"
        );
    }

    /// SLW-IN-11 regression: a `content/` dir NESTED under a per-repo `install/`
    /// tree must stay read-only — the positional team anchor must not let a deeper
    /// `content` segment masquerade as the per-repo content root.
    #[test]
    fn is_installed_path_content_nested_under_install_is_readonly() {
        let per_repo_install_content = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/repos/alderco/codesense/install/content/d.md",
        );
        assert!(
            is_installed_path(&per_repo_install_content, &home()),
            "content nested under a per-repo install tree must stay read-only"
        );
        let team_install_content = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/install/content/d.md",
        );
        assert!(
            is_installed_path(&team_install_content, &home()),
            "content nested under a team install tree must stay read-only"
        );
    }

    /// SLW-IN-11 integrity: a nested `orgs/{o}/{r}/{team}/content` look-alike buried
    /// INSIDE a read-only `install/` tree must NOT be waved through as writable. The
    /// first-`orgs` anchor lands on the outer (real) org root — whose own marker is
    /// `install` → read-only — and never descends to the nested look-alike.
    #[test]
    fn is_installed_path_nested_orgs_lookalike_under_install_is_readonly() {
        let nested = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/install/orgs/x/y/team2/content/s.md",
        );
        assert!(
            is_installed_path(&nested, &home()),
            "a nested orgs/.../content look-alike inside install must stay read-only"
        );
    }

    /// SLW-IN-11 hardening: a path OUTSIDE the codesense home — even one crafted
    /// with a perfectly-shaped `users/{uid}/orgs/{owner}/{repo}/{team}/content`
    /// window (e.g. what a symlink could canonicalize to) — is read-only, because
    /// the guard anchors on the real home via `strip_prefix`, not a window scan.
    #[test]
    fn is_installed_path_outside_home_is_readonly() {
        let crafted = PathBuf::from("/tmp/evil/users/42/orgs/acme/api/default/content/story.md");
        assert!(
            is_installed_path(&crafted, &home()),
            "a valid-looking content window outside the home must stay read-only"
        );
    }

    #[test]
    fn is_installed_path_source_clone_is_readonly() {
        // A materialized source-repo clone (epic worktree / sources) is read-only.
        let p = PathBuf::from(
            "/home/dev/.codesense/users/42/_epic/acme/EPIC-1/alderco/codesense/_bmad-output/story.md",
        );
        assert!(
            is_installed_path(&p, &home()),
            "source clone must be read-only"
        );
    }

    /// SLW-UD-8 AC-4: once folded under `{team}/content/`, the framework and
    /// skill files are writable (NOT installed) BY CONSTRUCTION — `is_installed_path`
    /// keys on the `{team}/content` window, so no guard edit is needed. A leftover
    /// team-level `install/` path stays read-only (negative control).
    #[test]
    fn ud8_folded_content_bmad_and_claude_are_writable() {
        let bmad = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/content/_bmad/bmm/config.yaml",
        );
        assert!(
            !is_installed_path(&bmad, &home()),
            "folded content/_bmad must be writable"
        );
        let skill = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/content/.claude/skills/bmad-x.md",
        );
        assert!(
            !is_installed_path(&skill, &home()),
            "folded content/.claude must be writable"
        );
        let leftover_install =
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/install/_bmad/x.md");
        assert!(
            is_installed_path(&leftover_install, &home()),
            "a leftover install-tree path stays read-only"
        );
    }

    #[test]
    fn is_installed_path_arbitrary_path_is_readonly() {
        // Reject-by-default: anything not under `{team}/content/` in an org tree.
        assert!(is_installed_path(
            &PathBuf::from("/tmp/whatever/story.md"),
            &home()
        ));
        assert!(is_installed_path(&PathBuf::from("story.md"), &home()));
    }

    #[test]
    fn is_installed_path_content_segment_must_be_in_org_tree() {
        // SLW-IN-7: a `content` dir NOT under an `orgs/{slug}/{team}/` tree is
        // not a team content root (the `orgs/` ancestor is the positional anchor
        // that replaced the old `_team` sentinel).
        let p = PathBuf::from("/home/dev/project/content/story.md");
        assert!(is_installed_path(&p, &home()));
    }

    #[test]
    fn is_installed_path_rejects_dotdot_escape_from_content_root() {
        // SLW-MD-1 write-guard bypass: a `..` component after the content root
        // resolves (at OS level) INTO the read-only install tree. The guard
        // must reject it rather than drop the `..` and see a clean content root.
        let escape = PathBuf::from(
            "/home/dev/.codesense/users/42/orgs/acme/api/default/content/../install/foo.md",
        );
        assert!(
            is_installed_path(&escape, &home()),
            "a `..`-bearing path that escapes the content root must be read-only"
        );

        // Sanity: the same path WITHOUT the `..` (the true content-root file) is
        // still writable — the reject is scoped to `..`, not over-broad.
        let clean =
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/content/foo.md");
        assert!(
            !is_installed_path(&clean, &home()),
            "clean content-root file writable"
        );
    }

    /// SS-7 AC-1: `TeamProjection` resolves to `_team/{team}/content/`
    /// when a team is set. This is the SOURCE root for story-linker's
    /// projection engine — it MUST be backed in `content_layer_root` so
    /// `MaterializedEntry` records keyed by `source_layer ==
    /// TeamProjection` have a meaningful associated source dir for
    /// debugging + future inspection tooling.
    #[test]
    fn content_layer_root_returns_team_content_path_for_team_projection_with_team() {
        let c = ctx(Some("default"));
        let p = content_layer_root(LayerId::TeamProjection, &c).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/content")
        );
    }

    /// SS-7 AC-1: `TeamProjection` requires `team_name` like the other
    /// Team-tier layers; `None` returns `None`.
    #[test]
    fn content_layer_root_returns_none_for_team_projection_when_team_is_none() {
        let c = ctx(None);
        assert!(content_layer_root(LayerId::TeamProjection, &c).is_none());
    }

    #[test]
    fn cp28_no_config_excludes_equal_secret_globs() {
        // AC-1: with no team config, the effective excludes are byte-for-byte the
        // current `SECRET_GLOBS` behavior — the whole `is_secret_path` corpus must
        // pass through the new predicate identically (both the refused secrets and
        // the kept-ordinary files).
        let ex = TeamExcludes::default_only();
        for secret in [
            ".env",
            "config/.env.production",
            "certs/server.pem",
            "tls/private.key",
            ".ssh/id_rsa",
            "nested/id_ed25519",
            "credentials",
            ".npmrc",
            "config/prod.env",
            ".git-credentials",
            "keys/AuthKey.p8",
            "android/release.jks",
            "putty/server.ppk",
            "gpg/private.asc",
        ] {
            assert!(
                is_projection_excluded(secret, &ex),
                "{secret} should be excluded by the default set"
            );
            // Parity with the pure default predicate it supersedes.
            assert_eq!(is_projection_excluded(secret, &ex), is_secret_path(secret));
        }
        for ordinary in [
            "README.md",
            "docs/guide.md",
            ".ssh/id_rsa.pub",
            "env.example.md",
        ] {
            assert!(!is_projection_excluded(ordinary, &ex));
            assert_eq!(
                is_projection_excluded(ordinary, &ex),
                is_secret_path(ordinary)
            );
        }
    }

    /// SW-XS-12: a secret in any case (`config/.ENV` is `config/.env` on
    /// macOS) is excluded by the defaults, and by a team-added exclude; a
    /// team's `!` negation is matched exactly as written, so it never opens a
    /// case variant it did not name (fail-closed).
    #[test]
    fn xs12_excludes_match_secret_case_variants_and_negations_stay_exact() {
        let ex = TeamExcludes::default_only();
        for secret in [
            "config/.ENV",
            ".Env.Local",
            "Certs/Server.PEM",
            ".SSH/ID_RSA",
            ".NPMRC",
        ] {
            assert!(is_projection_excluded(secret, &ex), "{secret}");
        }
        // Stays included: names that only look like a secret in another case.
        for kept in [
            "Docs/Guide.MD",
            "docs/Env.md",
            "notes/ENV",
            "Keys/ID_RSA.PUB",
        ] {
            assert!(!is_projection_excluded(kept, &ex), "{kept} stays included");
        }
        let team = TeamExcludes::resolve(&["**/*.tfstate".to_string(), "!**/.env".to_string()]);
        assert!(is_projection_excluded("infra/Prod.TFSTATE", &team));
        assert!(
            !is_projection_excluded("config/.env", &team),
            "the negation as written"
        );
        assert!(
            is_projection_excluded("config/.ENV", &team),
            "a negation does not open a case variant it did not name"
        );
    }

    #[test]
    fn swan27_bmad_payload_paths_are_not_excluded_by_default() {
        // SW-AN-27 (AC-2 caveat): the BMAD pack payload — the non-`.claude`
        // `_bmad/**` sibling tree (incl. the resolver `.py`) and the `bmad-*` skills —
        // must ride the whole-root projection under the no-config default. None of
        // these match SECRET_GLOBS; only a team-added broad glob (`**/*.py`,
        // `_bmad/**`) could drop them, which is a deliberate override, not the default.
        let ex = TeamExcludes::default_only();
        for payload in [
            "_bmad/scripts/resolve_customization.py",
            "_bmad/bmm/config.yaml",
            "_bmad/wds/skills/some-skill/SKILL.md",
            ".claude/skills/bmad-agent-dev/SKILL.md",
            ".claude/skills/wds-agent-freya-ux/SKILL.md",
        ] {
            assert!(
                !is_projection_excluded(payload, &ex),
                "{payload} must project under the no-config default"
            );
        }
    }

    #[test]
    fn cp28_team_can_negate_a_default_to_allow_it() {
        // AC-2: negating a default (gitignore-style `!`) makes that path project as
        // ordinary content — an engineer who wants `.env` in their worktree.
        let ex = TeamExcludes::resolve(&["!**/.env".to_string()]);
        assert!(!is_projection_excluded(".env", &ex));
        assert!(!is_projection_excluded("config/.env", &ex));
        // Negating `.env` does NOT open unrelated secrets — the allow is scoped.
        assert!(is_projection_excluded(".ssh/id_rsa", &ex));
        assert!(is_projection_excluded("tls/private.key", &ex));
    }

    #[test]
    fn cp28_team_can_add_an_exclude() {
        // AC-3: a team-added glob suppresses matching content in BOTH directions
        // (the predicate is direction-agnostic — one set gates forward + reverse).
        let ex = TeamExcludes::resolve(&["**/*.tfstate".to_string()]);
        assert!(is_projection_excluded("infra/prod.tfstate", &ex));
        assert!(is_projection_excluded("terraform.tfstate", &ex));
        // Defaults still hold alongside the addition.
        assert!(is_projection_excluded(".env", &ex));
        // Unrelated ordinary content still projects.
        assert!(!is_projection_excluded("infra/main.tf", &ex));
    }

    #[test]
    fn cp28_invalid_team_glob_is_skipped_not_fatal() {
        // A malformed team pattern must never wedge a wave: it is skipped, the rest
        // of the config still resolves, and the defaults remain in force.
        let ex = TeamExcludes::resolve(&["[".to_string(), "**/*.tfstate".to_string()]);
        assert!(is_projection_excluded("infra/prod.tfstate", &ex));
        assert!(is_projection_excluded(".env", &ex));
    }

    #[test]
    fn cp28_validate_exclude_glob_accepts_valid_rejects_bad() {
        assert!(validate_exclude_glob("**/*.tfstate").is_ok());
        assert!(validate_exclude_glob("!**/.env").is_ok()); // negation stripped first
        assert!(validate_exclude_glob("[").is_err());
        // Strict write path: a degenerate empty / bare-`!` / whitespace pattern is
        // rejected (surfaced to the UI) rather than persisted as an inert glob.
        assert!(validate_exclude_glob("").is_err());
        assert!(validate_exclude_glob("   ").is_err());
        assert!(validate_exclude_glob("!").is_err());
        assert!(validate_exclude_glob("!  ").is_err());
    }

    #[test]
    fn cp28_resolve_drops_degenerate_patterns_and_trims() {
        // Lenient resolve path: a hand-edited file with padding / empty / bare-`!`
        // entries must not wedge a wave or match literal-space paths — the entries
        // are trimmed and degenerate ones dropped, while the defaults still hold.
        let ex = TeamExcludes::resolve(&[
            "  **/*.tfstate  ".to_string(), // padded add → trimmed, matches
            "".to_string(),                 // empty → dropped
            "!".to_string(),                // bare negation → dropped
            "  !  **/.env  ".to_string(),   // padded negation → trimmed, negates .env
        ]);
        assert!(is_projection_excluded("infra/prod.tfstate", &ex)); // padded add works
        assert!(!is_projection_excluded(".env", &ex)); // padded negation works
        assert!(is_projection_excluded(".ssh/id_rsa", &ex)); // untouched default holds
                                                             // The dropped empties did not turn into an inert "match nothing / everything".
        assert!(!is_projection_excluded("README.md", &ex));
    }

    #[test]
    fn is_safe_logical_name_accepts_ordinary_relative_names() {
        assert!(is_safe_logical_name(
            ".claude/skills/sense-worktree-discovery/SKILL.md"
        ));
        assert!(is_safe_logical_name("README.md"));
        assert!(is_safe_logical_name("a/b/c/d.md"));
    }

    #[test]
    fn is_safe_logical_name_rejects_path_traversal_and_absolutes() {
        for hostile in [
            "",
            "/etc/passwd",
            "../escape",
            "../../etc",
            "a/../b",
            "a/./b",
            "./a",
            "a//b",
            "a/",
            "\0",
            "a\0b",
            "\\windows\\path",
            "a\\b",
            "C:/Windows",
            "C:\\Windows",
            "Z:/anything",
            "z:foo",
        ] {
            assert!(
                !is_safe_logical_name(hostile),
                "expected {:?} to be rejected",
                hostile
            );
        }
    }

    /// P5 (iter-2): a colon at byte 1 is only a Windows drive prefix
    /// when byte 0 is an ASCII letter. Names like `a:b/c.md` (a POSIX
    /// filename starting with a colon-bearing segment whose first byte
    /// is a digit or symbol — not a letter) must NOT be over-rejected.
    /// Actually `a` IS a letter, so this is treated as a drive prefix
    /// per the safer rule. Use a digit/underscore-prefix to test the
    /// over-rejection regression directly.
    #[test]
    fn is_safe_logical_name_accepts_colon_when_not_drive_prefix() {
        // First byte is a digit / underscore / symbol — not a drive prefix.
        for ok in ["1:foo", "_:foo", "9:bar/baz.md", ":foo"] {
            assert!(
                is_safe_logical_name(ok),
                "expected {:?} to be accepted (no Windows drive prefix)",
                ok
            );
        }
    }

    #[test]
    fn validate_path_segment_rejects_unsafe_inputs() {
        for hostile in [
            "",
            "evil/team",
            "ev\\il",
            "..",
            ".",
            "ev\0il",
            "ev\nil",
            // P2 (iter-2): leading whitespace was silently accepted; now rejected.
            " foo",
            "foo ",
            "  acme  ",
            // P3 (iter-2): whitespace-only.
            "   ",
            "\t",
            // P2 + earlier traversal: trim-bypass attempts caught by the
            // whitespace rule before the traversal rule fires.
            " .. ",
            " . ",
            " .foo",
        ] {
            assert!(
                validate_path_segment(hostile).is_some(),
                "expected {:?} to be rejected by validate_path_segment",
                hostile
            );
        }
    }

    /// P15 (iter-2 revert): leading-dot names are accepted again.
    /// `.github` is a legitimate slug name (GitHub's own metadata-repo
    /// convention); rejecting leading dots created an asymmetry with
    /// `SourceRepoSlug::new` and broke real-world inputs.
    #[test]
    fn validate_path_segment_accepts_leading_dot() {
        assert!(validate_path_segment(".github").is_none());
        assert!(validate_path_segment(".foo").is_none());
        assert!(validate_path_segment(".hidden").is_none());
    }

    #[test]
    fn validate_path_segment_accepts_ordinary_names() {
        for ok in ["default", "backend", "team-1", "team_1", "a.b.c", "v2.0"] {
            assert!(
                validate_path_segment(ok).is_none(),
                "expected {:?} to be accepted",
                ok
            );
        }
    }

    /// P6 (iter-2): `content_layer_root` defensively validates `ctx.org_slug`
    /// and `ctx.team_name` so an unsafe ResolverContext can't be used to
    /// escape codesense_home via `path_to_org_root`'s pure `PathBuf::join`.
    #[test]
    fn content_layer_root_rejects_unsafe_ctx_org_slug() {
        let c = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/home/dev/.codesense"),
            user_id: 42,
            org_slug: "../escape".to_string(),
            team_name: Some("default".to_string()),
        };
        assert!(content_layer_root(LayerId::TeamProjection, &c).is_none());
    }

    #[test]
    fn content_layer_root_rejects_unsafe_ctx_team_name() {
        let c = ResolverContext {
            context_root: None,
            codesense_home: PathBuf::from("/home/dev/.codesense"),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("ev/il".to_string()),
        };
        assert!(content_layer_root(LayerId::TeamProjection, &c).is_none());
    }

    #[test]
    fn ctx_with_team_overrides_team_only() {
        let c = ctx(Some("alpha"));
        let swapped = ctx_with_team(&c, Some("beta".to_string()));
        assert_eq!(swapped.team_name.as_deref(), Some("beta"));
        assert_eq!(swapped.codesense_home, c.codesense_home);
        assert_eq!(swapped.user_id, c.user_id);
        assert_eq!(swapped.org_slug, c.org_slug);
    }

    #[test]
    fn ctx_with_team_can_set_none() {
        let c = ctx(Some("alpha"));
        let swapped = ctx_with_team(&c, None);
        assert!(swapped.team_name.is_none());
    }

    /// SW-XS-8: the install tiers are gone; team content is the one layer;
    /// `TeamWorkspace` still loads, `TeamRepo` is refused (review r1).
    #[test]
    fn retired_install_tiers_load_as_team_content() {
        assert_eq!(PRIORITY_ORDER, &[LayerId::TeamProjection]);
        for name in ["TeamWorkspace", "TeamProjection"] {
            let layer: LayerId = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(layer, LayerId::TeamProjection, "{name}");
        }
        // Review r1 M1: the per-repo tier never lands team-wide.
        let error = serde_json::from_value::<LayerId>(serde_json::json!("TeamRepo")).unwrap_err();
        assert!(
            error.to_string().contains("reverse_sync_worktree"),
            "{error}"
        );
        assert_eq!(
            serde_json::to_value(LayerId::TeamProjection).unwrap(),
            serde_json::json!("TeamProjection")
        );
    }

    /// `list_layer` lists the team's content (no install tier), skipping
    /// ignored and runtime-state files; a missing root lists nothing.
    #[test]
    fn list_layer_lists_team_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut c = ctx(Some("default"));
        c.codesense_home = tmp.path().to_path_buf();
        assert!(list_layer(LayerId::TeamProjection, &c).is_empty());
        let root = content_layer_root(LayerId::TeamProjection, &c).unwrap();
        for (rel, bytes) in [
            ("docs/a.md", "a"),
            (".claude/skills/s/SKILL.md", "s"),
            (".DS_Store", "x"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let install = crate::context_repo::path_to_team_install_dir(
            &crate::context_repo::path_to_org_root(tmp.path(), &c.principal(), &c.org_slug)
                .unwrap(),
            "default",
        );
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("old.md"), "retired").unwrap();
        assert_eq!(
            list_layer(LayerId::TeamProjection, &c),
            vec![
                ".claude/skills/s/SKILL.md".to_string(),
                "docs/a.md".to_string()
            ]
        );
    }
}

#[cfg(test)]
mod context_root_tests {
    use super::*;

    #[test]
    fn write_destinations_use_the_chosen_checkout_even_before_the_folder_exists() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let ctx = ResolverContext {
            context_root: Some(crate::agents::manifest_resolve::ContextRoot {
                id: "cw_test".into(),
                root: root.path().into(),
            }),
            codesense_home: home.path().into(),
            user_id: 1,
            org_slug: "acme/context".into(),
            team_name: Some("team".into()),
        };
        let path = content_layer_root(LayerId::TeamProjection, &ctx).unwrap();
        assert_eq!(path, root.path().join("team/content"));
        assert!(!path.exists());
    }
}
