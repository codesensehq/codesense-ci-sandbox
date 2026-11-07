// SPDX-License-Identifier: MIT
//! The hidden/secret/VCS name filters — the single source of truth shared by
//! every file listing (context nav and worktree editor tree) and by the
//! content engine's resolver and projection seams.

use std::path::Path;

/// Built-in ignore globs: obvious filesystem noise that has no business being
/// listed or materialized into a code repo. Matched via `globset` on a logical
/// (repo-relative) name. User-configurable ignore lists are explicitly deferred.
pub const IGNORE_GLOBS: &[&str] = &[
    "**/.DS_Store",
    "**/*.swp",
    "**/.git/**",
    "**/node_modules/**",
    "**/.idea/**",
    // `.gitkeep` is a VCS-tracking placeholder with no business being
    // materialized. Team scaffolding seeds one directly inside the
    // `_team/{team}/install/` tier root, so without this filter a
    // freshly-scaffolded team would enumerate `.gitkeep` as resolver content.
    "**/.gitkeep",
];

/// Returns `true` if `logical_name` should be filtered out by
/// [`IGNORE_GLOBS`]. Built on `globset::GlobSet` for shared-matcher
/// efficiency across many candidate names. Static `OnceLock` caches the
/// compiled matcher; the cached value is derived from compile-time
/// constants only — every caller sees the same value, so the function stays
/// pure (no behavior switching on prior state).
pub fn is_ignored(logical_name: &str) -> bool {
    use std::sync::OnceLock;
    static MATCHER: OnceLock<globset::GlobSet> = OnceLock::new();
    let matcher = MATCHER.get_or_init(|| build_globset("IGNORE_GLOBS", IGNORE_GLOBS));
    matcher.is_match(Path::new(logical_name))
}

/// `true` when a DIRECTORY at repo-relative `dir_logical_name` has its ENTIRE
/// subtree filtered by [`IGNORE_GLOBS`] — every descendant matches
/// [`is_ignored`]. This lets a tree walk skip descending into a build/cruft dir
/// (`node_modules`, `.git`, `.idea`) using the SAME glob authority the per-file
/// [`is_ignored`] gate uses, so a walk-level prune and the per-file reject can
/// never disagree about which subtrees are ignorable.
///
/// The container globs are `**/<name>/**`, which match a DESCENDANT, not the
/// bare directory, so this probes a synthetic child. The probe name must itself
/// be non-ignored (it is a made-up sentinel, matching no glob) so the result
/// reflects only the directory. A bare-name file glob (e.g. `**/.gitkeep`,
/// `**/.DS_Store`) does NOT make a like-named directory a prunable subtree —
/// only the `**/<name>/**` container globs do — which is the intended behavior.
pub fn is_ignored_subtree(dir_logical_name: &str) -> bool {
    // A trailing component the `**` in `**/<name>/**` binds to. Deliberately a
    // name that matches no IGNORE_GLOB, so only the directory drives the result.
    let probe = format!(
        "{}/__codesense_walk_probe__",
        dir_logical_name.trim_end_matches('/')
    );
    is_ignored(&probe)
}

/// `true` when `name` is a version-control metadata directory that must never
/// appear in a file listing (context nav or worktree editor tree) or a
/// projection — browsing VCS plumbing is never useful and projecting one would
/// corrupt a repo. Matched on the bare name because `IGNORE_GLOBS`'s
/// `**/.git/**` deliberately requires a child and so can't catch the bare `.git`
/// entry itself. The single source of truth shared by the viewer scan and the
/// projection reserved-dir guard, so the two can never disagree about which
/// names are VCS dirs.
///
/// In any case — on macOS's case-insensitive filesystem `.GIT` is
/// `.git`.
pub fn is_vcs_metadata_dir(name: &str) -> bool {
    [".git", ".hg", ".svn"]
        .iter()
        .any(|vcs| name.eq_ignore_ascii_case(vcs))
}

/// Repo-relative filename globs that must NEVER be adopted into
/// the git-shared context repo — secrets / credentials / private keys. Matched on
/// the dest-relative `logical_name` exactly like [`IGNORE_GLOBS`] (so an ancestor
/// dir outside the repo can't spuriously match). Fail-CLOSED for safety: an
/// ambiguous `*.key` is refused rather than risk leaking a private key into
/// pulled-by-teammates content. Public keys (`*.pub`) are deliberately absent.
pub const SECRET_GLOBS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/*.env",
    "**/*.pem",
    "**/*.key",
    "**/*.p8",
    "**/*.p12",
    "**/*.pfx",
    "**/*.ppk",
    "**/*.jks",
    "**/*.keystore",
    "**/*.pkcs12",
    "**/*.asc",
    "**/*.gpg",
    "**/id_rsa",
    "**/id_dsa",
    "**/id_ecdsa",
    "**/id_ed25519",
    "**/credentials",
    "**/.git-credentials",
    "**/.npmrc",
    "**/.pypirc",
    "**/.netrc",
];

/// Runtime state files — SQLite databases and their write-ahead/shared-memory/
/// journal sidecars (the Weft ledger `weft.sqlite3` among them). They are an
/// application's live state, not team context: projecting one into a worktree
/// hands agents a database the app may be writing, and adopting one back would
/// ship it to teammates. Excluded unconditionally — a team `!` negation cannot opt
/// it back in — at every projection seam (forward materialize and reverse adoption)
/// and at the layer resolver. NOT hidden from file listings — a user's own repo
/// may keep a SQLite file they want to see.
pub const RUNTIME_STATE_GLOBS: &[&str] = &[
    "**/*.sqlite3",
    "**/*.sqlite3-wal",
    "**/*.sqlite3-shm",
    "**/*.sqlite3-journal",
    "**/*.sqlite",
    "**/*.sqlite-wal",
    "**/*.sqlite-shm",
    "**/*.sqlite-journal",
    // A `.db` file may be a checked-in fixture, but its WAL/SHM/journal sidecars
    // only exist while a process holds it open.
    "**/*.db-wal",
    "**/*.db-shm",
    "**/*.db-journal",
];

/// `true` when `logical_name` matches a [`RUNTIME_STATE_GLOBS`] pattern.
pub fn is_runtime_state(logical_name: &str) -> bool {
    use std::sync::OnceLock;
    static MATCHER: OnceLock<globset::GlobSet> = OnceLock::new();
    let matcher = MATCHER.get_or_init(|| build_globset("RUNTIME_STATE_GLOBS", RUNTIME_STATE_GLOBS));
    matcher.is_match(Path::new(logical_name))
}

/// `true` when `logical_name` matches a [`SECRET_GLOBS`] pattern
/// — the pure default-set predicate. The content engine's projection seams gate
/// on a team-aware generalization of it, but the read-through file listing
/// (`is_hidden_from_listing`) reuses this fixed set directly: a
/// non-configurable viewer scan must never turn a credential file into a
/// clickable, readable nav row.
pub fn is_secret_path(logical_name: &str) -> bool {
    use std::sync::OnceLock;
    static MATCHER: OnceLock<globset::GlobSet> = OnceLock::new();
    let matcher = MATCHER.get_or_init(|| build_globset("SECRET_GLOBS", SECRET_GLOBS));
    matcher.is_match(Path::new(logical_name))
}

/// The one authoritative "hide this entry from a file listing" contract, shared
/// by the context nav and the worktree editor tree (both scan via
/// [`crate::FsService::list`]). Allow-by-default transparency: every curated
/// entry shows — dot-directories like `.claude`/`.codex`/`.github` included —
/// except three targeted, individually-justified exclusions.
///
/// Heavy build/dependency dirs (`node_modules`/`target`) are deliberately NOT
/// hidden here: the worktree tree shows those de-emphasized rather than dropping
/// them, so their exclusion is a projection-only concern. Matched on the bare
/// entry name, so the decision is uniform at every level — the frontend expands
/// the tree by re-listing per directory (a listing covers a single level; it
/// does not recurse).
pub(crate) fn is_hidden_from_listing(name: &str) -> bool {
    is_ignored(name)            // .DS_Store, *.swp, .gitkeep
        || is_secret_path(name) // .env*, .npmrc, id_rsa, *.pem, credentials
        || is_vcs_metadata_dir(name) // .git, .hg, .svn
}

/// Compile a `GlobSet` from compile-time constant patterns. Shared by
/// [`is_ignored`] and [`is_secret_path`]. An invalid pattern is a build-time bug
/// (constants only) so it panics loudly during dev rather than degrading silently.
///
/// Case-insensitive. These sets keep secrets, VCS data and runtime
/// state out of listings and workspaces; on macOS's case-insensitive
/// filesystem `config/.ENV` and `.GIT/` are the same files as `config/.env`
/// and `.git/`, so a case-sensitive match would let them through. Matching
/// every case everywhere also fails closed on case-sensitive filesystems.
fn build_globset(label: &str, patterns: &[&str]) -> globset::GlobSet {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let glob = case_insensitive_glob(pattern)
            .unwrap_or_else(|e| panic!("invalid {label} pattern {pattern:?}: {e}"));
        builder.add(glob);
    }
    builder
        .build()
        .unwrap_or_else(|e| panic!("{label} GlobSet should build from valid patterns: {e}"))
}

/// A glob that matches every case variant of `pattern`. The one
/// spelling every exclude set (these constants and a team's added excludes in
/// the content engine) compiles with, so the two cannot disagree on case.
pub fn case_insensitive_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .case_insensitive(true)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_runtime_state_matches_sqlite_and_sidecars_only() {
        for state in [
            "weft.sqlite3",
            "weft.sqlite3-wal",
            "weft.sqlite3-shm",
            "weft.sqlite3-journal",
            "nested/app.sqlite",
            "nested/app.sqlite-wal",
        ] {
            assert!(is_runtime_state(state), "{state} is runtime state");
        }
        for ordinary in ["README.md", "docs/sqlite3-notes.md", "schema.sql"] {
            assert!(
                !is_runtime_state(ordinary),
                "{ordinary} is not runtime state"
            );
        }
        // Listings still show them (a user's own repo may keep one).
        assert!(!is_hidden_from_listing("weft.sqlite3"));
    }

    #[test]
    fn is_ignored_matches_ds_store() {
        assert!(is_ignored(".DS_Store"));
        assert!(is_ignored("nested/dir/.DS_Store"));
    }

    #[test]
    fn is_ignored_matches_swap_files() {
        assert!(is_ignored("foo.swp"));
        assert!(is_ignored("path/to/bar.swp"));
    }

    #[test]
    fn is_ignored_matches_inside_git_and_node_modules() {
        assert!(is_ignored(".git/config"));
        assert!(is_ignored("nested/.git/HEAD"));
        assert!(is_ignored("node_modules/foo/index.js"));
    }

    #[test]
    fn is_ignored_matches_gitkeep() {
        assert!(is_ignored(".gitkeep"));
        assert!(is_ignored("install/.gitkeep"));
        assert!(is_ignored("repos/.gitkeep"));
    }

    #[test]
    fn is_ignored_excludes_ordinary_files() {
        assert!(!is_ignored(
            ".claude/skills/sense-worktree-discovery/SKILL.md"
        ));
        assert!(!is_ignored("README.md"));
    }

    #[test]
    fn is_ignored_subtree_prunes_container_dirs_only() {
        // The `**/<name>/**` container globs make a whole subtree ignorable, so a
        // walk may prune the bare directory (even though `is_ignored` on the bare
        // name is false — it needs a child).
        assert!(is_ignored_subtree("node_modules"));
        assert!(is_ignored_subtree(".git"));
        assert!(is_ignored_subtree(".idea"));
        assert!(is_ignored_subtree("nested/node_modules"));
        assert!(is_ignored_subtree("a/b/.git"));
        // NOT prunable subtrees: `is_ignored` does not filter these, so pruning
        // them would drop real, adoptable files — the exact behavior-change trap.
        assert!(!is_ignored_subtree("target"));
        assert!(!is_ignored_subtree(".hg"));
        assert!(!is_ignored_subtree(".svn"));
        assert!(!is_ignored_subtree("docs"));
        assert!(!is_ignored_subtree(".claude"));
        // A bare-name FILE glob (`**/.gitkeep`) must not make a like-named DIR a
        // prunable subtree — only the container globs do.
        assert!(!is_ignored_subtree(".gitkeep"));
        // Invariant the probe relies on: the sentinel child must itself be
        // non-ignored, so the result reflects only the directory. If a future
        // IGNORE_GLOB ever matched the sentinel, EVERY directory would prune.
        assert!(
            !is_ignored("__codesense_walk_probe__"),
            "probe sentinel must match no IGNORE_GLOB, else every dir over-prunes"
        );
        // And it does not spuriously flip a non-container dir to prunable.
        assert!(!is_ignored("docs/__codesense_walk_probe__"));
    }

    #[test]
    fn is_vcs_metadata_dir_matches_bare_vcs_names_only() {
        assert!(is_vcs_metadata_dir(".git"));
        assert!(is_vcs_metadata_dir(".hg"));
        assert!(is_vcs_metadata_dir(".svn"));
        // The whole reason this guard exists: `IGNORE_GLOBS`'s `**/.git/**`
        // requires a child, so `is_ignored` does NOT catch the bare `.git` entry.
        assert!(!is_ignored(".git"));
        // Not a VCS dir: build/dep dirs and ordinary curated content.
        assert!(!is_vcs_metadata_dir("node_modules"));
        assert!(!is_vcs_metadata_dir("target"));
        assert!(!is_vcs_metadata_dir(".github"));
        assert!(!is_vcs_metadata_dir(".claude"));
    }

    /// macOS's filesystem is case-insensitive, so `config/.ENV` IS
    /// `config/.env` and `.GIT/` IS `.git/`. The secret, ignore (VCS cruft)
    /// and runtime-state sets, and the VCS-dir check, match every case variant
    /// on every path that consults them (listing, preparation, agent editor).
    #[test]
    fn secret_ignore_and_vcs_filters_match_every_case_variant() {
        for secret in [
            "config/.ENV",
            ".Env",
            "config/.ENV.production",
            "Config/Prod.ENV",
            "certs/SERVER.PEM",
            "tls/Private.Key",
            ".SSH/ID_RSA",
            "nested/Id_Ed25519",
            "CREDENTIALS",
            ".NPMRC",
            ".Git-Credentials",
        ] {
            assert!(is_secret_path(secret), "{secret} is a secret");
            assert!(
                is_hidden_from_listing(secret.rsplit('/').next().unwrap()),
                "{secret}"
            );
        }
        for ignored in [
            ".GIT/config",
            "nested/.Git/HEAD",
            "NODE_MODULES/foo/index.js",
            "x/.ds_store",
            "Foo.SWP",
            "install/.GITKEEP",
        ] {
            assert!(is_ignored(ignored), "{ignored} is ignored");
        }
        assert!(is_ignored_subtree("vendor/.GIT"));
        for state in ["Weft.SQLITE3", "nested/App.Sqlite-WAL", "x.DB-SHM"] {
            assert!(is_runtime_state(state), "{state} is runtime state");
        }
        for vcs in [".GIT", ".Git", ".HG", ".Svn"] {
            assert!(is_vcs_metadata_dir(vcs), "{vcs} is a VCS dir");
            assert!(is_hidden_from_listing(vcs), "{vcs}");
        }
        // Still precise: public keys, look-alikes and ordinary content stay.
        for ordinary in [
            ".SSH/ID_RSA.PUB",
            "ENV.md",
            "Readme.MD",
            ".GitHub",
            ".GitIgnore",
        ] {
            assert!(!is_secret_path(ordinary), "{ordinary}");
            assert!(!is_ignored(ordinary), "{ordinary}");
            assert!(!is_vcs_metadata_dir(ordinary), "{ordinary}");
        }
    }

    #[test]
    fn ud6_is_secret_path_refuses_credentials_keeps_ordinary() {
        // Secrets refused (any depth, via `**/`).
        assert!(is_secret_path(".env"));
        assert!(is_secret_path("config/.env.production"));
        assert!(is_secret_path("certs/server.pem"));
        assert!(is_secret_path("tls/private.key"));
        assert!(is_secret_path(".ssh/id_rsa"));
        assert!(is_secret_path("nested/id_ed25519"));
        assert!(is_secret_path("credentials"));
        assert!(is_secret_path(".npmrc"));
        // The suffix .env form and more key/credential formats.
        assert!(is_secret_path("config/prod.env"));
        assert!(is_secret_path(".git-credentials"));
        assert!(is_secret_path("keys/AuthKey.p8"));
        assert!(is_secret_path("android/release.jks"));
        assert!(is_secret_path("putty/server.ppk"));
        assert!(is_secret_path("gpg/private.asc"));
        // Ordinary content + PUBLIC keys are NOT secrets.
        assert!(!is_secret_path("README.md"));
        assert!(!is_secret_path("docs/guide.md"));
        assert!(!is_secret_path(".ssh/id_rsa.pub"));
        assert!(!is_secret_path("env.example.md"));
    }

    #[test]
    fn list_directory_visibility_hides_only_cruft_secrets_and_vcs_dirs() {
        // Curated content — including dot-directories — is shown.
        for shown in [
            ".claude",
            ".codex",
            ".agents",
            ".github",
            ".vscode",
            ".idea",
            "README.md",
            // Curated dot-config FILES the transparency change surfaces. These
            // pin that the `.git`/`.hg`/`.svn` VCS-dir guard does NOT over-match
            // the `.git*` config-file family (`.gitignore` != `.git`).
            ".gitignore",
            ".gitattributes",
            ".gitmodules",
            ".editorconfig",
            ".dockerignore",
            // node_modules/target reach the FE so the worktree tree can show
            // them de-emphasized — the viewer scan must NOT drop them.
            "node_modules",
            "target",
        ] {
            assert!(
                !is_hidden_from_listing(shown),
                "{shown} should be visible in a file listing"
            );
        }

        // OS/editor cruft, credentials, and VCS-metadata dirs stay hidden.
        for hidden in [
            ".DS_Store",
            "foo.swp",
            ".gitkeep",
            ".env",
            ".env.local",
            // `.env.example` is collateral of the shared `**/.env.*` secret glob.
            // Hidden by design — the viewer reuses projection's secret contract
            // verbatim rather than inventing a narrower one (documented tradeoff).
            ".env.example",
            ".npmrc",
            ".git-credentials",
            "id_rsa",
            "server.pem",
            ".git",
            ".hg",
            ".svn",
        ] {
            assert!(
                is_hidden_from_listing(hidden),
                "{hidden} should be hidden from a file listing"
            );
        }
    }
}
