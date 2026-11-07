// SPDX-License-Identifier: MIT
//! The file service a host exposes: the host injects its gate, fs-core owns the
//! gated list/read.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use log::warn;

use crate::filters::is_hidden_from_listing;
use crate::gate::{normalize_path_components, path_within_any_root};
use crate::listing::{list_dir, FileEntry};
use crate::read::read_text;

/// Which paths a host may read. The only thing a host implements: every gate is
/// an [`FsService`] through the blanket impl, so list/read have exactly one
/// implementation.
pub trait FsGate {
    /// Admit `path` or refuse it with the host's access-denied message.
    fn admit(&self, path: &Path) -> Result<(), String>;
}

/// The gated worktree file service. Implemented for every [`FsGate`] and for
/// nothing else, so no host can carry its own reader.
pub trait FsService: FsGate {
    /// List one level of `dir` (filtered, symlink-resolved, dirs first in natural
    /// order) once the gate admits it. A missing directory lists as empty.
    fn list(&self, dir: &Path) -> Result<Vec<FileEntry>, String> {
        self.admit(dir)?;
        list_dir(dir)
    }

    /// Read `path` as UTF-8 text once the gate admits it.
    fn read(&self, path: &Path) -> Result<String, String> {
        self.admit(path)?;
        read_text(path)
    }
}

impl<G: FsGate + ?Sized> FsService for G {}

/// A gate over an explicit root set. A path is admitted only when:
/// - it lies lexically inside an absolute root ([`path_within_any_root`]) — each
///   root is accepted as given and in its canonical spelling;
/// - where it resolves lies inside a canonical root. A path that does not exist
///   yet is judged by its nearest existing ancestor, so neither `..` nor a
///   symlink under a root reaches anything outside every root. A path that
///   cannot be resolved for any other reason is refused;
/// - no component below its root — as requested or as resolved — names an entry
///   the listing hides (a secret or VCS path), compared case-insensitively, so
///   such a file is never served by name or through an alias either.
///
/// Roots are resolved once, at construction.
pub struct RootSetFs {
    lexical_roots: Vec<PathBuf>,
    real_roots: Vec<PathBuf>,
}

impl RootSetFs {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let lexical: Vec<PathBuf> = roots
            .into_iter()
            .filter(|root| root.is_absolute())
            .map(|root| normalize_path_components(&root))
            .collect();
        let mut real_roots: Vec<PathBuf> = lexical
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        real_roots.dedup();
        let mut lexical_roots = lexical;
        for real in &real_roots {
            if !lexical_roots.contains(real) {
                lexical_roots.push(real.clone());
            }
        }
        Self {
            lexical_roots,
            real_roots,
        }
    }

    /// Where `path` resolves, walking up to the nearest existing ancestor when
    /// the leaf is missing (`link/..` leaves through the link, as the OS does).
    fn resolve(path: &Path) -> Option<PathBuf> {
        let mut probe = path.to_path_buf();
        loop {
            match probe.canonicalize() {
                Ok(real) => return Some(real),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    if !probe.pop() {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }

    fn names_a_hidden_entry(path: &Path, roots: &[PathBuf]) -> bool {
        roots
            .iter()
            .filter_map(|root| path.strip_prefix(root).ok())
            .any(|rel| {
                rel.components().any(|c| {
                    let name = c.as_os_str().to_string_lossy();
                    is_hidden_from_listing(&name) || is_hidden_from_listing(&name.to_lowercase())
                })
            })
    }
}

impl FsGate for RootSetFs {
    fn admit(&self, path: &Path) -> Result<(), String> {
        let lexical = normalize_path_components(path);
        let admitted = path_within_any_root(&lexical, &self.lexical_roots)
            && !Self::names_a_hidden_entry(&lexical, &self.lexical_roots)
            && Self::resolve(path).is_some_and(|real| {
                path_within_any_root(&real, &self.real_roots)
                    && !Self::names_a_hidden_entry(&real, &self.real_roots)
            });
        if admitted {
            return Ok(());
        }
        warn!(target: "files", "path rejected (outside every allowed root or hidden): {}", path.display());
        Err("Access denied: path is outside every allowed root".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    const DENIED: &str = "Access denied: path is outside every allowed root";

    #[test]
    fn an_allowed_root_lists_and_reads() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("a.md"), "alpha").unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        let entries = service.list(root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.md");
        assert_eq!(service.read(&root.path().join("a.md")).unwrap(), "alpha");
    }

    #[test]
    fn a_path_outside_every_root_is_denied_before_touching_disk() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.md"), "nope").unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(service.list(outside.path()).unwrap_err(), DENIED);
        assert_eq!(
            service.read(&outside.path().join("secret.md")).unwrap_err(),
            DENIED
        );
    }

    #[test]
    fn an_empty_root_set_denies_everything() {
        let root = tempdir().unwrap();
        let service = RootSetFs::new(Vec::new());
        assert_eq!(service.list(root.path()).unwrap_err(), DENIED);
    }

    #[test]
    fn a_missing_path_inside_a_root_is_admitted_and_lists_empty() {
        let root = tempdir().unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);
        assert!(service.list(&root.path().join("gone")).unwrap().is_empty());
        assert!(service.read(&root.path().join("gone.md")).is_err());
    }

    #[test]
    fn dot_dot_traversal_out_of_a_root_is_denied() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("wt");
        fs::create_dir_all(&root).unwrap();
        fs::write(parent.path().join("outside.md"), "nope").unwrap();
        let service = RootSetFs::new(vec![root.clone()]);

        assert_eq!(
            service.read(&root.join("../outside.md")).unwrap_err(),
            DENIED
        );
        assert_eq!(service.list(&root.join("..")).unwrap_err(), DENIED);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_outside_the_roots_pointing_in_is_denied() {
        // The requested path is judged too, not only where it resolves.
        let root = tempdir().unwrap();
        fs::write(root.path().join("a.md"), "alpha").unwrap();
        let elsewhere = tempdir().unwrap();
        let link = elsewhere.path().join("into-root");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(service.list(&link).unwrap_err(), DENIED);
        assert_eq!(service.read(&link.join("a.md")).unwrap_err(), DENIED);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_under_a_root_escaping_it_is_denied() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("b.md"), "beta").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        // The link itself still lists as a directory entry of the root…
        let entries = service.list(root.path()).unwrap();
        assert!(entries.iter().any(|e| e.name == "escape" && e.is_dir));
        // …but nothing is served through it.
        assert_eq!(
            service.list(&root.path().join("escape")).unwrap_err(),
            DENIED
        );
        assert_eq!(
            service.read(&root.path().join("escape/b.md")).unwrap_err(),
            DENIED
        );
        // Nor through a lexically-inside `link/..` that physically leaves the root.
        assert_eq!(
            service.list(&root.path().join("escape/..")).unwrap_err(),
            DENIED
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_under_a_root_staying_inside_it_is_followed() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("real")).unwrap();
        fs::write(root.path().join("real/c.md"), "gamma").unwrap();
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("alias")).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(
            service.read(&root.path().join("alias/c.md")).unwrap(),
            "gamma"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_leaf_under_an_escaping_symlink_is_denied() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(
            service
                .admit(&root.path().join("escape/not-yet.md"))
                .unwrap_err(),
            DENIED
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_canonically_spelled_path_under_an_aliased_root_is_admitted() {
        let real = tempdir().unwrap();
        fs::write(real.path().join("a.md"), "alpha").unwrap();
        let aliases = tempdir().unwrap();
        let alias = aliases.path().join("alias");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();
        let service = RootSetFs::new(vec![alias.clone()]);

        let canonical = real.path().canonicalize().unwrap().join("a.md");
        assert_eq!(service.read(&canonical).unwrap(), "alpha");
        assert_eq!(service.read(&alias.join("a.md")).unwrap(), "alpha");
    }

    #[test]
    fn secret_and_vcs_paths_are_not_served_by_name() {
        let root = tempdir().unwrap();
        fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/config"), "[core]").unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(service.read(&root.path().join(".env")).unwrap_err(), DENIED);
        assert_eq!(
            service.read(&root.path().join(".git/config")).unwrap_err(),
            DENIED
        );
        assert_eq!(service.list(&root.path().join(".git")).unwrap_err(), DENIED);
    }

    #[test]
    fn a_relative_root_admits_nothing() {
        let root = tempdir().unwrap();
        let service = RootSetFs::new(vec![PathBuf::from("."), PathBuf::from("")]);
        assert_eq!(service.list(root.path()).unwrap_err(), DENIED);
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(service.list(&cwd).unwrap_err(), DENIED);
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_reached_through_an_innocent_alias_is_denied() {
        let root = tempdir().unwrap();
        fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/config"), "[core]").unwrap();
        std::os::unix::fs::symlink(root.path().join(".env"), root.path().join("notes.md")).unwrap();
        std::os::unix::fs::symlink(root.path().join(".git"), root.path().join("meta")).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        assert_eq!(
            service.read(&root.path().join("notes.md")).unwrap_err(),
            DENIED
        );
        assert_eq!(service.list(&root.path().join("meta")).unwrap_err(), DENIED);
        assert_eq!(
            service.read(&root.path().join("meta/config")).unwrap_err(),
            DENIED
        );
    }

    #[test]
    fn a_hidden_name_in_another_case_is_denied() {
        let root = tempdir().unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);
        assert_eq!(
            service.admit(&root.path().join(".ENV")).unwrap_err(),
            DENIED
        );
        assert_eq!(
            service.admit(&root.path().join(".GIT/config")).unwrap_err(),
            DENIED
        );
    }

    #[test]
    fn an_unresolvable_path_inside_a_root_is_denied() {
        // ENOTDIR: a path "through" a regular file cannot be resolved.
        let root = tempdir().unwrap();
        fs::write(root.path().join("file.md"), "x").unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);
        assert_eq!(
            service
                .admit(&root.path().join("file.md/inner"))
                .unwrap_err(),
            DENIED
        );
    }

    #[test]
    fn a_deeply_missing_path_inside_a_root_is_admitted() {
        let root = tempdir().unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);
        assert!(service.list(&root.path().join("a/b/c")).unwrap().is_empty());
    }

    #[test]
    fn the_filters_run_inside_the_gated_listing() {
        let root = tempdir().unwrap();
        for name in [".env", ".DS_Store", "id_rsa", "keep.md"] {
            fs::write(root.path().join(name), "x").unwrap();
        }
        fs::create_dir_all(root.path().join(".git")).unwrap();
        let service = RootSetFs::new(vec![root.path().to_path_buf()]);

        let names: Vec<String> = service
            .list(root.path())
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["keep.md"]);
    }
}
