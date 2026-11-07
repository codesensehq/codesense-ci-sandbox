// SPDX-License-Identifier: MIT
//! A team's landing page (its home page in the viewer): which file under the
//! team's content folder the app may open on its own when the team has nothing
//! open.
//!
//! The app opens this page without a click, so the check is strict: the page
//! must be a regular file whose REAL path (symlinks resolved) is inside the
//! team's content folder. A `README.md` symlinked to `~/.aws/credentials`, or a
//! declared page under a symlinked folder that leads out, never resolves.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::is_content_relative;

/// One page to try, relative to the content folder.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingCandidate {
    pub path: String,
    /// Match the file name in any letter case (the README/index fallbacks); a
    /// declared `landing_page` matches exactly.
    #[serde(default)]
    pub any_case: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LandingPage {
    /// `path` is `{content_root}/{relative path as spelled on disk}`.
    Found { path: String },
    /// The content folder is there and has none of the candidates.
    None,
    /// The content folder is missing, empty or unreadable (not cloned or synced
    /// yet): nothing can be decided now.
    Unavailable,
}

/// The first candidate that is a regular file really inside `content_root`.
///
/// `context_root` is the context root the caller admitted `content_root` under
/// (a lexical check): the content folder's real path must be inside that root's
/// real path too, so a team folder symlinked out of the context never resolves.
pub fn resolve(
    content_root: &str,
    candidates: &[LandingCandidate],
    context_root: &Path,
) -> LandingPage {
    let root_str = content_root.trim_end_matches('/');
    let root = Path::new(root_str);
    let Ok(real_root) = root.canonicalize() else {
        return LandingPage::Unavailable;
    };
    match context_root.canonicalize() {
        Ok(real_context) if real_root.starts_with(&real_context) => {}
        _ => return LandingPage::None,
    }
    // One listing per folder for the whole lookup: the content root is listed
    // once, however many candidates are tried there.
    let mut listings = Listings::default();
    // Missing, empty, or only placeholders (`.gitkeep`, `.DS_Store`) mid-sync:
    // nothing to decide yet.
    let has_content = listings
        .names(root)
        .is_some_and(|names| names.iter().any(|n| !n.starts_with('.')));
    if !has_content {
        return LandingPage::Unavailable;
    }
    for candidate in candidates {
        let rel = candidate.path.trim();
        if !is_content_relative(rel) {
            continue;
        }
        let segments: Vec<&str> = rel.split('/').collect();
        let Some((name, folders)) = segments.split_last() else {
            continue;
        };
        // Walk the folders by their exact on-disk names (a case-insensitive
        // filesystem would otherwise accept `Guide/` for `guide/`).
        let mut dir = root.to_path_buf();
        let mut shown = PathBuf::from(root_str);
        let mut walked = true;
        for folder in folders {
            match listings.on_disk_name(&dir, folder, false) {
                Some(on_disk) => {
                    dir.push(&on_disk);
                    shown.push(&on_disk);
                }
                None => {
                    walked = false;
                    break;
                }
            }
        }
        if !walked {
            continue;
        }
        let Some(on_disk) = listings.on_disk_name(&dir, name, candidate.any_case) else {
            continue;
        };
        if !is_file_inside(&dir.join(&on_disk), &real_root) {
            continue;
        }
        shown.push(on_disk);
        return LandingPage::Found {
            path: shown.to_string_lossy().into_owned(),
        };
    }
    LandingPage::None
}

/// Folder listings read during one lookup, each folder at most once.
#[derive(Default)]
struct Listings {
    by_dir: HashMap<PathBuf, Option<Vec<String>>>,
}

impl Listings {
    /// The entry names in `dir`, or `None` when it can't be listed.
    fn names(&mut self, dir: &Path) -> Option<&[String]> {
        self.by_dir
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                fs::read_dir(dir).ok().map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
            })
            .as_deref()
    }

    /// The directory entry named `name` in `dir` (exact first, then any case
    /// when allowed), as spelled on disk. Listing rather than `exists()` keeps an
    /// exact match exact on case-insensitive filesystems.
    fn on_disk_name(&mut self, dir: &Path, name: &str, any_case: bool) -> Option<String> {
        let names = self.names(dir)?;
        if names.iter().any(|n| n == name) {
            return Some(name.to_string());
        }
        if any_case {
            return names.iter().find(|n| n.eq_ignore_ascii_case(name)).cloned();
        }
        None
    }
}

/// `path` resolves (through every symlink) to a regular file under `real_root`
/// that is not hidden there: a visibly named symlink to `.env` or into
/// `.claude/` never resolves.
fn is_file_inside(path: &Path, real_root: &Path) -> bool {
    match path.canonicalize() {
        Ok(real) => {
            real.is_file()
                && real.strip_prefix(real_root).is_ok_and(|rel| {
                    rel.components()
                        .all(|c| !c.as_os_str().to_string_lossy().starts_with('.'))
                })
        }
        Err(_) => false,
    }
}

/// Whether `page` (relative to `content_root`) is a page the resolver would
/// open as a declared starter page: exact on-disk spelling, a regular file
/// really inside the content folder, nothing hidden. The registry edit checks
/// with this, so it never records a page the app would refuse to open.
pub fn declared_page_resolves(content_root: &Path, page: &str, context_root: &Path) -> bool {
    let candidate = LandingCandidate {
        path: page.to_string(),
        any_case: false,
    };
    matches!(
        resolve(&content_root.to_string_lossy(), &[candidate], context_root),
        LandingPage::Found { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fallbacks() -> Vec<LandingCandidate> {
        ["README.md", "README.html", "index.html"]
            .iter()
            .map(|p| LandingCandidate {
                path: (*p).into(),
                any_case: true,
            })
            .collect()
    }

    fn declared(path: &str) -> LandingCandidate {
        LandingCandidate {
            path: path.into(),
            any_case: false,
        }
    }

    fn found(root: &Path, rel: &str) -> LandingPage {
        LandingPage::Found {
            path: format!("{}/{rel}", root.to_string_lossy()),
        }
    }

    #[test]
    fn picks_the_first_candidate_present() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("index.html"), "i").unwrap();
        fs::write(tmp.path().join("README.html"), "r").unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            found(tmp.path(), "README.html")
        );
    }

    #[test]
    fn a_declared_page_wins_and_matches_exactly() {
        let tmp = tempdir().unwrap();
        fs::create_dir(tmp.path().join("guide")).unwrap();
        fs::write(tmp.path().join("guide/Start.md"), "s").unwrap();
        fs::write(tmp.path().join("README.md"), "r").unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        let mut cands = vec![declared("guide/Start.md")];
        cands.extend(fallbacks());
        assert_eq!(
            resolve(&root, &cands, tmp.path()),
            found(tmp.path(), "guide/Start.md")
        );
        let mut wrong_case = vec![declared("guide/start.md")];
        wrong_case.extend(fallbacks());
        assert_eq!(
            resolve(&root, &wrong_case, tmp.path()),
            found(tmp.path(), "README.md")
        );
    }

    #[test]
    fn fallbacks_match_any_case_and_keep_the_disk_spelling() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("readme.md"), "r").unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            found(tmp.path(), "readme.md")
        );
    }

    #[test]
    fn no_candidate_is_none_and_never_a_random_file() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("notes.md"), "n").unwrap();
        fs::create_dir(tmp.path().join("README.md")).unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(resolve(&root, &fallbacks(), tmp.path()), LandingPage::None);
    }

    #[test]
    fn a_missing_or_empty_content_folder_is_unavailable() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(
            resolve(&missing, &fallbacks(), tmp.path()),
            LandingPage::Unavailable
        );
        let empty = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&empty, &fallbacks(), tmp.path()),
            LandingPage::Unavailable
        );
    }

    #[test]
    fn unsafe_declared_paths_are_skipped() {
        let tmp = tempdir().unwrap();
        let content = tmp.path().join("content");
        fs::create_dir(&content).unwrap();
        fs::write(tmp.path().join("org.yml"), "o").unwrap();
        fs::write(content.join("README.md"), "r").unwrap();
        let root = content.to_string_lossy().into_owned();
        fs::write(content.join(".env"), "secret").unwrap();
        fs::create_dir(content.join(".claude")).unwrap();
        fs::write(content.join(".claude/notes.md"), "n").unwrap();
        for bad in [
            "../org.yml",
            "/etc/hosts",
            "a/../../org.yml",
            ".env",
            ".claude/notes.md",
        ] {
            let mut cands = vec![declared(bad)];
            cands.extend(fallbacks());
            assert_eq!(
                resolve(&root, &cands, tmp.path()),
                found(&content, "README.md"),
                "{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_readme_symlinked_out_of_the_content_never_resolves() {
        let tmp = tempdir().unwrap();
        let content = tmp.path().join("content");
        fs::create_dir(&content).unwrap();
        let secret = tmp.path().join("credentials");
        fs::write(&secret, "secret").unwrap();
        std::os::unix::fs::symlink(&secret, content.join("README.md")).unwrap();
        fs::write(content.join("index.html"), "i").unwrap();
        let root = content.to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            found(&content, "index.html")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_declared_page_under_a_symlinked_folder_out_never_resolves() {
        let tmp = tempdir().unwrap();
        let content = tmp.path().join("content");
        let outside = tmp.path().join("outside");
        fs::create_dir(&content).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("start.md"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, content.join("guide")).unwrap();
        let root = content.to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &[declared("guide/start.md")], tmp.path()),
            LandingPage::None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_visible_symlink_to_a_hidden_file_never_resolves() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".env"), "secret").unwrap();
        fs::write(tmp.path().join("index.html"), "i").unwrap();
        std::os::unix::fs::symlink(".env", tmp.path().join("README.md")).unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            found(tmp.path(), "index.html")
        );
    }

    #[test]
    fn declared_page_resolves_needs_the_exact_spelling() {
        let tmp = tempdir().unwrap();
        fs::create_dir(tmp.path().join("guide")).unwrap();
        fs::write(tmp.path().join("guide/start.md"), "s").unwrap();
        assert!(declared_page_resolves(
            tmp.path(),
            "guide/start.md",
            tmp.path()
        ));
        assert!(!declared_page_resolves(
            tmp.path(),
            "Guide/Start.md",
            tmp.path()
        ));
        assert!(!declared_page_resolves(tmp.path(), "guide", tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_content_is_fine() {
        let tmp = tempdir().unwrap();
        fs::create_dir(tmp.path().join("docs")).unwrap();
        fs::write(tmp.path().join("docs/home.md"), "h").unwrap();
        std::os::unix::fs::symlink("docs/home.md", tmp.path().join("README.md")).unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            found(tmp.path(), "README.md")
        );
    }

    #[test]
    fn a_folder_is_listed_once_per_lookup() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("README.md"), "r").unwrap();
        let mut listings = Listings::default();
        assert_eq!(
            listings.on_disk_name(tmp.path(), "readme.md", true),
            Some("README.md".into())
        );
        // A second probe of the same folder answers from the first listing.
        fs::remove_file(tmp.path().join("README.md")).unwrap();
        fs::write(tmp.path().join("index.html"), "i").unwrap();
        assert_eq!(
            listings.on_disk_name(tmp.path(), "README.md", false),
            Some("README.md".into())
        );
        assert_eq!(listings.on_disk_name(tmp.path(), "index.html", false), None);
        assert_eq!(listings.by_dir.len(), 1);
    }

    #[test]
    fn a_folder_with_only_placeholders_is_unavailable() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".gitkeep"), "").unwrap();
        fs::write(tmp.path().join(".DS_Store"), "").unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        assert_eq!(
            resolve(&root, &fallbacks(), tmp.path()),
            LandingPage::Unavailable
        );
    }

    #[test]
    fn a_declared_page_folder_must_match_its_case_exactly() {
        let tmp = tempdir().unwrap();
        fs::create_dir(tmp.path().join("Guide")).unwrap();
        fs::write(tmp.path().join("Guide/start.md"), "s").unwrap();
        fs::write(tmp.path().join("README.md"), "r").unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        let mut cands = vec![declared("guide/start.md")];
        cands.extend(fallbacks());
        assert_eq!(
            resolve(&root, &cands, tmp.path()),
            found(tmp.path(), "README.md")
        );
        let mut exact = vec![declared("Guide/start.md")];
        exact.extend(fallbacks());
        assert_eq!(
            resolve(&root, &exact, tmp.path()),
            found(tmp.path(), "Guide/start.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_content_folder_symlinked_out_of_the_context_never_resolves() {
        let tmp = tempdir().unwrap();
        let context = tmp.path().join("context");
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir_all(context.join("team")).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("README.md"), "secret").unwrap();
        std::os::unix::fs::symlink(&elsewhere, context.join("team/content")).unwrap();
        let root = context.join("team/content").to_string_lossy().into_owned();
        assert_eq!(resolve(&root, &fallbacks(), &context), LandingPage::None);
    }
}
