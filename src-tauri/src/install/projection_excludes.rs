// SPDX-License-Identifier: MIT
//! SW-CP-28: the per-team **projection-exclude list** (`projection-excludes.yml`).
//!
//! A team-synced list at `{org_root}/{team}/config/projection-excludes.yml` of
//! glob patterns for content paths that must **not** project (forward materialize)
//! and must **not** be adopted (reverse). It generalizes SLW-UD-6's hardcoded
//! `SECRET_GLOBS` / [`codesense_fs_core::filters::is_secret_path`] gate into a team knob:
//! the credential globs stay the single-source-of-truth DEFAULT set in code, and a
//! team's config layers on top — **add** a pattern, or **negate** a default
//! (gitignore-style leading `!`) to allow it. See
//! [`crate::content_engine::TeamExcludes`] for the two-globset resolution model.
//!
//! This lives beside [`super::projection_manifest`] (the `projections.yml` loader)
//! and mirrors its shape exactly: it owns the on-disk location (`config/`, NOT
//! `content/`, so it never self-projects), a lenient loader (a missing file is the
//! zero-config default; a malformed file degrades to defaults-only + `warn!`, never
//! a wave-blocking error), and a process-global, mtime-keyed cache of the RESOLVED
//! [`TeamExcludes`] so the projection engine resolves it **once per wave** and each
//! seam is a pure in-memory match.
//!
//! Readers: the projection engine (via [`load_team_excludes_cached`], once per
//! wave). Writers: CP-28's Settings section, via the config-dir read/write seam
//! ([`load_patterns_from`] / [`write_patterns_to`]) the Tauri commands wrap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::content_engine::TeamExcludes;
use crate::context_repo::team_config_dir_beside_content;
use crate::shared::yaml_io::{write_yaml_atomic, YamlIoError};

/// The single app-managed store filename under `{org_root}/{team}/config/`. `.yml`
/// (not `.yaml`) to match the sibling team configs (`projections.yml`,
/// `status-taxonomy.yml`, `field-schema.yml`).
pub const PROJECTION_EXCLUDES_FILENAME: &str = "projection-excludes.yml";

/// The store's on-disk home given a team `config/` dir directly — the sibling of
/// `content/` (resolved by callers via `team_config_dir_beside_content` or the
/// command's `path_to_team_config_dir`). It lives in `config/`, NOT `content/`, so
/// `content/` stays purely WYSIWYG and the file never self-projects (mirrors
/// [`super::projection_manifest`]).
fn excludes_path_in_config(config_dir: &Path) -> PathBuf {
    config_dir.join(PROJECTION_EXCLUDES_FILENAME)
}

/// Top-level `projection-excludes.yml` document: a flat list of glob patterns.
/// A leading `!` on a pattern negates a default (allow); every other entry is an
/// added exclude. Wrapping the list under a named `patterns` key leaves room for
/// future store-level keys without a breaking migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawProjectionExcludes {
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// Read + parse the raw team override patterns from a `config/` dir. Tolerant by
/// construction: a missing file is the zero-config case (empty list → defaults
/// only); a malformed file is `warn!`-logged and treated as empty rather than
/// wedging projection. This is the read seam the Settings command wraps, and the
/// resolve source below.
pub fn load_patterns_from(config_dir: &Path) -> Vec<String> {
    let path = excludes_path_in_config(config_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                target: "projection_excludes",
                "no exclude list at {} — defaults only", path.display()
            );
            return Vec::new();
        }
        Err(e) => {
            warn!(
                target: "projection_excludes",
                "cannot read {}: {e} — defaults only", path.display()
            );
            return Vec::new();
        }
    };
    match serde_yaml::from_str::<RawProjectionExcludes>(&raw) {
        Ok(doc) => doc.patterns,
        Err(e) => {
            warn!(
                target: "projection_excludes",
                "malformed {}: {e} — defaults only", path.display()
            );
            Vec::new()
        }
    }
}

/// Atomically write the team override patterns to a `config/` dir (the Settings
/// write seam). Deterministic YAML via [`write_yaml_atomic`]; the next projection
/// wave re-resolves from the new mtime (see [`load_team_excludes_cached`]).
pub fn write_patterns_to(config_dir: &Path, patterns: Vec<String>) -> Result<(), YamlIoError> {
    write_yaml_atomic(
        &excludes_path_in_config(config_dir),
        &RawProjectionExcludes { patterns },
    )
}

/// SW-CP-28: process-global, mtime-keyed cache of the RESOLVED [`TeamExcludes`].
/// Keyed by the store path → (last-seen `(mtime, len)`, resolved set). Mirrors
/// `projection_manifest::MANIFEST_CACHE`: the projection pass runs synchronously
/// per file save, so this replaces the read+parse+globset-compile on that hot path
/// with a single `stat`, re-resolving only when the file actually changes. Stored
/// as an `Arc` so a wave hoists ONE resolve and every seam clones the pointer.
#[allow(clippy::type_complexity)]
static EXCLUDES_CACHE: OnceLock<
    Mutex<HashMap<PathBuf, (Option<(SystemTime, u64)>, Arc<TeamExcludes>)>>,
> = OnceLock::new();

/// Resolve the team's effective [`TeamExcludes`] from its `content/` dir, with the
/// mtime cache in front. Resolve this ONCE per wave (before a subtree walk) and
/// pass `&*result` to [`crate::content_engine::is_projection_excluded`] at each
/// seam — the returned `Arc` keeps the compiled globsets alive with zero per-file
/// I/O. A missing/malformed store resolves to defaults-only against `mtime = None`.
pub fn load_team_excludes_cached(content_dir: &Path) -> Arc<TeamExcludes> {
    let config_dir = team_config_dir_beside_content(content_dir);
    let path = excludes_path_in_config(&config_dir);
    // Invalidate on (mtime, len) — same rationale as the manifest cache: bare mtime
    // has 1s granularity on some filesystems, so pair it with file length to catch
    // a same-second edit that changes size, from one `metadata()` stat.
    let stamp: Option<(SystemTime, u64)> = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (t, m.len())));

    let cache = EXCLUDES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some((cached_stamp, resolved)) = guard.get(&path) {
            if *cached_stamp == stamp {
                return Arc::clone(resolved);
            }
        }
    }

    // Miss or stale: re-read, re-resolve (compiles the globsets), refresh. The
    // resolve happens OUTSIDE the lock ON PURPOSE (mirrors
    // `projection_manifest::load_specs_cached`): holding the mutex across file I/O +
    // globset compilation would serialize every team's resolve. This is NOT a
    // check-then-act hazard — the entry is `stamp`-keyed and self-healing: two
    // concurrent cold callers of the same file resolve identical values (idempotent
    // insert), and if the file changes mid-race a stale entry is never *served*
    // because the next caller stats fresh and misses on the stamp mismatch. On a
    // poisoned lock we return a fresh correct resolve (just skip caching) rather than
    // dropping the team's config.
    let resolved = Arc::new(TeamExcludes::resolve(&load_patterns_from(&config_dir)));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path, (stamp, Arc::clone(&resolved)));
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_engine::is_projection_excluded;
    use tempfile::tempdir;

    /// A `config/` dir laid out beside a `content/` dir, so both the config-dir and
    /// content-dir resolvers land on the same file.
    fn team_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let team = tempdir().unwrap();
        let content_dir = team.path().join("content");
        let config_dir = team.path().join("config");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        (team, content_dir, config_dir)
    }

    #[test]
    fn missing_store_is_defaults_only() {
        // AC-1: no config → effective excludes equal the SECRET_GLOBS defaults.
        let (_team, content_dir, _config_dir) = team_dirs();
        let ex = load_team_excludes_cached(&content_dir);
        assert!(is_projection_excluded(".env", &ex));
        assert!(!is_projection_excluded("README.md", &ex));
    }

    #[test]
    fn write_then_resolve_round_trips_add_and_negate() {
        // AC-2 + AC-3: a written config with an add and a negate resolves through
        // the cache (fresh content_dir avoids a stale cache entry from a sibling test).
        let (_team, content_dir, config_dir) = team_dirs();
        write_patterns_to(
            &config_dir,
            vec!["**/*.tfstate".to_string(), "!**/.env".to_string()],
        )
        .unwrap();
        let ex = load_team_excludes_cached(&content_dir);
        assert!(is_projection_excluded("infra/prod.tfstate", &ex)); // added
        assert!(!is_projection_excluded(".env", &ex)); // negated default
        assert!(is_projection_excluded(".ssh/id_rsa", &ex)); // other defaults hold
    }

    #[test]
    fn load_patterns_round_trips_through_write() {
        let (_team, _content_dir, config_dir) = team_dirs();
        assert!(load_patterns_from(&config_dir).is_empty());
        write_patterns_to(&config_dir, vec!["**/*.log".to_string()]).unwrap();
        assert_eq!(
            load_patterns_from(&config_dir),
            vec!["**/*.log".to_string()]
        );
    }

    #[test]
    fn malformed_store_degrades_to_defaults_not_error() {
        let (_team, content_dir, config_dir) = team_dirs();
        std::fs::write(
            config_dir.join(PROJECTION_EXCLUDES_FILENAME),
            "patterns: [not, {a: map}\n",
        )
        .unwrap();
        // Malformed → defaults only, never a panic/error that wedges the wave.
        let ex = load_team_excludes_cached(&content_dir);
        assert!(is_projection_excluded(".env", &ex));
    }

    #[test]
    fn cache_re_resolves_after_edit() {
        // AC-6 mechanism: a rewrite lands a new (mtime,len) → next resolve reflects it.
        let (_team, content_dir, config_dir) = team_dirs();
        assert!(is_projection_excluded(
            ".env",
            &load_team_excludes_cached(&content_dir)
        ));
        write_patterns_to(&config_dir, vec!["!**/.env".to_string()]).unwrap();
        // A same-second edit changes length (empty list → one pattern), so the
        // (mtime,len) stamp differs and the cache re-resolves.
        assert!(!is_projection_excluded(
            ".env",
            &load_team_excludes_cached(&content_dir)
        ));
    }
}
