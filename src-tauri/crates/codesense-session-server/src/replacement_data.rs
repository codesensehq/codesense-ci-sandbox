// SPDX-License-Identifier: MIT
//! Pure parsing of bounded authorization text and installed file manifests.

use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn authorization_fields(input: &str) -> Result<BTreeMap<&str, &str>, String> {
    let mut fields = std::collections::BTreeMap::new();
    for line in input.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| "malformed replacement authorization".to_string())?;
        if value.is_empty() || fields.insert(key, value).is_some() {
            return Err("malformed replacement authorization".into());
        }
    }
    Ok(fields)
}

pub(crate) fn manifest_matches(manifest: &str, executable_hash: &str, executable: &Path) -> bool {
    manifest.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(executable_hash)
            && fields
                .next()
                .is_some_and(|path| Path::new(path).file_name() == executable.file_name())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_fields_preserve_validation_and_crlf_behavior() {
        let fields = authorization_fields("schema=v2\r\nnonce=abc\n").unwrap();
        assert_eq!(fields.get("schema"), Some(&"v2"));
        assert_eq!(fields.get("nonce"), Some(&"abc"));
        for bad in ["a=", "a=1\na=2", "missing-separator", "a=1\n\n"] {
            assert_eq!(
                authorization_fields(bad).unwrap_err(),
                "malformed replacement authorization"
            );
        }
    }

    #[test]
    fn manifest_match_requires_hash_and_executable_filename() {
        let manifest = "abc ./other\ndef ./codesense-session-server\n";
        let executable = Path::new("/installed/codesense-session-server");
        assert!(manifest_matches(manifest, "def", executable));
        assert!(!manifest_matches(manifest, "abc", executable));
        assert!(!manifest_matches("def", "def", executable));
    }
}
