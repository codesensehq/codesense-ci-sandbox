// SPDX-License-Identifier: MIT
//! Resolve scanned sources independently of the guard crate's location.
use std::path::{Path, PathBuf};

/// The app directory, or an explicitly selected checkout for A/B checks.
pub fn app_root() -> PathBuf {
    std::env::var_os("CODESENSE_GUARD_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .canonicalize()
        .expect("guard app root must exist")
}

/// Application sources and every sibling crate's sources, in stable order.
pub fn source_roots() -> Vec<PathBuf> {
    roots_at(&app_root())
}

fn roots_at(app: &Path) -> Vec<PathBuf> {
    let mut roots = vec![app.join("src")];
    let mut crates: Vec<_> = std::fs::read_dir(app.join("crates"))
        .expect("read source crates")
        .map(|entry| entry.expect("read crate entry").path().join("src"))
        .filter(|path| path.is_dir())
        .collect();
    crates.sort();
    roots.extend(crates);
    roots
}

/// Find exactly one source by its path relative to a source root.
pub fn locate(relative: &str) -> PathBuf {
    locate_in(&source_roots(), relative)
}

fn locate_in(roots: &[PathBuf], relative: &str) -> PathBuf {
    let matches: Vec<_> = roots
        .iter()
        .map(|root| root.join(relative))
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "locate({relative:?}): expected exactly one source, found {}: {matches:?}",
        matches.len()
    );
    matches.into_iter().next().unwrap()
}

/// A retired source must be absent from every source root.
pub fn absent(relative: &str) -> bool {
    absent_in(&source_roots(), relative)
}

fn absent_in(roots: &[PathBuf], relative: &str) -> bool {
    roots.iter().all(|root| !root.join(relative).exists())
}

/// Keep app allowlist labels stable and qualify other crates by their path.
pub fn source_label(file: &Path) -> String {
    label_at(&app_root(), file)
}

fn label_at(app: &Path, file: &Path) -> String {
    file.strip_prefix(app.join("src"))
        .or_else(|_| file.strip_prefix(app))
        .unwrap_or_else(|_| panic!("no source root for {}", file.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

/// Resolve legacy app-relative reads, frontend reads, and guard fixtures.
pub fn resolve(relative: &str) -> PathBuf {
    if relative == "src/lib.rs" {
        app_root().join(relative)
    } else if let Some(source) = relative.strip_prefix("src/") {
        locate(source)
    } else if relative.starts_with("tests/") {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
    } else {
        app_root().join(relative)
    }
}

/// Read a source or guard fixture with a useful failing path.
pub fn read(relative: &str) -> String {
    let path = resolve(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_resolution_case_table() {
        let temporary =
            std::env::temp_dir().join(format!("codesense-guards-{}", std::process::id()));
        let app = temporary.join("src-tauri");
        for dir in [
            "src/domain",
            "crates/one/src/domain",
            "crates/two/src/domain",
        ] {
            std::fs::create_dir_all(app.join(dir)).unwrap();
        }
        let roots = roots_at(&app);
        assert_eq!(roots.len(), 3);
        assert_eq!(
            label_at(&app, &roots[0].join("domain/probe.rs")),
            "domain/probe.rs"
        );
        assert_eq!(
            label_at(&app, &roots[1].join("domain/probe.rs")),
            "crates/one/src/domain/probe.rs"
        );
        for copies in 0..=3 {
            if copies > 0 {
                std::fs::write(roots[copies - 1].join("domain/probe.rs"), "").unwrap();
            }
            assert_eq!(absent_in(&roots, "domain/probe.rs"), copies == 0);
            let result = std::panic::catch_unwind(|| locate_in(&roots, "domain/probe.rs"));
            if copies == 1 {
                assert_eq!(result.unwrap(), roots[0].join("domain/probe.rs"));
            } else {
                let panic = result.unwrap_err();
                let message = panic.downcast_ref::<String>().unwrap();
                assert!(message.contains(&format!("found {copies}")), "{message}");
            }
        }
        // A retired file reintroduced only in another crate must still fail.
        std::fs::write(roots[2].join("domain/retired.rs"), "").unwrap();
        assert!(!absent_in(&roots, "domain/retired.rs"));
        std::fs::remove_dir_all(temporary).unwrap();
    }
}
