// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};

use crate::context_repo::path_to_team_content_dir;

/// Returns `~/.codesense/` — the single source of truth for all CodeSense
/// user-scoped data (managed binaries, scratch notes, MCP socket, manifests).
///
/// NOT for `~/Library/Application Support/ai.codesense/` (Tauri's app data dir)
/// and NOT for `~/.claude/` (Claude CLI's own directory).
pub fn codesense_home_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codesense"))
}

/// Create `dir` (0700) if missing, refusing a symlink in its place.
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("could not inspect {}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{} must be a plain directory", dir.display()));
    }
    Ok(())
}

/// The warm-start frontmatter-cache file under `codesense_home`. Single Rust
/// source of truth for the `{home}/cache/frontmatter-cache.json` location
/// (mirrors the path the frontend hands to `init_watched_cache` at
/// `use-workspace-viewer-sync.ts`). Any Rust consumer that must read/drop the
/// persisted cache uses this rather than re-joining the literal.
pub fn frontmatter_cache_path(codesense_home: &std::path::Path) -> PathBuf {
    codesense_home.join("cache").join("frontmatter-cache.json")
}

/// Resolve a bundled resource path, preferring the Tauri resource resolver and
/// falling back to `src-tauri/{rel}` for `tauri dev`. `None` if neither exists.
///
/// Single home for the resolve-with-dev-fallback pattern used for the bundled
/// Starship binary/configs, the shell-integration script (PT-5), and the canvas
/// starter pack — callers pass a path relative to the bundle root (e.g.
/// `"resources/starship"`).
pub fn resolve_bundled_resource(app: &tauri::AppHandle, rel: &str) -> Option<PathBuf> {
    use tauri::path::BaseDirectory;
    use tauri::Manager;
    app.path()
        .resolve(rel, BaseDirectory::Resource)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| {
            let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
            dev.exists().then_some(dev)
        })
}

/// PTS-3: Tauri-exposed delegate to [`path_to_team_content_dir`].
///
/// Returns the absolute canonical per-team context-repo content dir
/// (`{org_root}/{team}/content/`). Pure path math — no filesystem
/// touch, matching the helper's contract.
///
/// The frontend uses this to populate `RepoState.teamContentPath` so the
/// TS layer never needs to know the on-disk layout. PTS-2 migrates every
/// consumer of `${contextRepoPath}/${viewerRoot}` to consume this field.
#[tauri::command]
pub async fn resolve_team_content_dir(org_root: String, team: String) -> Result<String, String> {
    let path = path_to_team_content_dir(&PathBuf::from(&org_root), &team);
    Ok(path.to_string_lossy().into_owned())
}

/// Sanitize a name into a safe slug: lowercase, dashes for spaces/underscores,
/// only alphanumeric, dash, and dot characters. Returns Err if the result is
/// empty or a reserved name (`.`, `..`).
pub fn sanitize_name(name: &str) -> Result<String, String> {
    let sanitized: String = name
        .to_lowercase()
        .replace([' ', '_'], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '.')
        .collect();

    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return Err(format!("Cannot sanitize name: '{}'", name));
    }

    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codesense_home_dir_returns_dotcodesense() {
        let dir = codesense_home_dir().expect("home dir should exist");
        assert!(dir.ends_with(".codesense"));
    }

    #[test]
    fn test_sanitize_name_basic() {
        assert_eq!(sanitize_name("My Project").unwrap(), "my-project");
    }

    #[test]
    fn test_sanitize_name_underscores() {
        assert_eq!(sanitize_name("mycorp_app").unwrap(), "mycorp-app");
    }

    #[test]
    fn test_sanitize_name_special_chars() {
        assert_eq!(sanitize_name("My Project!@#").unwrap(), "my-project");
    }

    #[test]
    fn test_sanitize_name_preserves_dots() {
        assert_eq!(sanitize_name("v2.0-beta").unwrap(), "v2.0-beta");
    }

    #[test]
    fn test_sanitize_name_empty_rejects() {
        assert!(sanitize_name("").is_err());
    }

    #[test]
    fn test_sanitize_name_dot_rejects() {
        assert!(sanitize_name(".").is_err());
    }

    #[test]
    fn test_sanitize_name_dotdot_rejects() {
        assert!(sanitize_name("..").is_err());
    }

    #[test]
    fn test_sanitize_name_all_special_chars_rejects() {
        assert!(sanitize_name("!@#$%^&*()").is_err());
    }

    #[tokio::test]
    async fn resolve_team_content_dir_returns_per_team_content_path() {
        let org_root = "/tmp/cs/users/42/orgs/acme/api";
        let team = "default";
        let result = resolve_team_content_dir(org_root.to_string(), team.to_string())
            .await
            .expect("delegate should not fail");

        let expected = path_to_team_content_dir(&PathBuf::from(org_root), team);
        assert_eq!(result, expected.to_string_lossy());
        assert!(result.ends_with("/default/content"));
    }
}
