// SPDX-License-Identifier: MIT
//! Whole-file UTF-8 text read.

use std::fs;
use std::path::Path;

use log::{debug, error};

/// Read `path` as UTF-8 text — ungated; [`crate::FsService::read`] is the only
/// caller and admits `path` first.
pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| {
        let path = path.display();
        // ENOENT is a normal probe condition (e.g. extension-probing in
        // resolveWorktreeParams) — log at debug, not error.
        if e.kind() == std::io::ErrorKind::NotFound {
            debug!(target: "files", "read_file: not found {}", path);
        } else {
            error!(target: "files", "read_file failed for {}: {}", path, e);
        }
        format!("failed to read file {}: {}", path, e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_utf8_text_verbatim() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("main.rs");
        let source = "fn main() {\n    println!(\"héllo, 世界\");\n}\n";
        fs::write(&file, source).unwrap();
        assert_eq!(read_text(&file).unwrap(), source);
    }

    #[test]
    fn a_missing_file_errors_with_the_path() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope.md");
        let err = read_text(&missing).unwrap_err();
        assert!(
            err.starts_with(&format!("failed to read file {}: ", missing.display())),
            "{err}"
        );
    }

    #[test]
    fn a_non_utf8_file_errors() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("blob.bin");
        fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_text(&file).is_err());
    }
}
