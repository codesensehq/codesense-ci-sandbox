// SPDX-License-Identifier: MIT
use crate::domain::{Attachment, Attachments};
use crate::shared::yaml_io::{write_yaml_atomic, YamlIoError};
use std::path::Path;

#[derive(Debug)]
pub enum AttachmentsIoError {
    Io(std::io::Error),
    Parse(serde_yaml::Error),
    Write(YamlIoError),
}

impl std::fmt::Display for AttachmentsIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachmentsIoError::Io(e) => write!(f, "attachments io: {e}"),
            AttachmentsIoError::Parse(e) => write!(f, "attachments parse: {e}"),
            AttachmentsIoError::Write(e) => write!(f, "attachments write: {e}"),
        }
    }
}

impl std::error::Error for AttachmentsIoError {}

/// Read attachments from YAML. Missing file → empty `Attachments`. Malformed → `Err(Parse)`.
pub fn read_attachments(path: &Path) -> Result<Attachments, AttachmentsIoError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents).map_err(AttachmentsIoError::Parse),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Attachments::default()),
        Err(e) => Err(AttachmentsIoError::Io(e)),
    }
}

/// Thin wrapper over `write_yaml_atomic<Attachments>`.
pub fn write_attachments_atomic(path: &Path, data: &Attachments) -> Result<(), AttachmentsIoError> {
    write_yaml_atomic(path, data).map_err(AttachmentsIoError::Write)
}

/// Match canonical checkout identity while retaining exact matches for missing paths.
/// Multiple historical aliases are ambiguous: never silently choose or erase a row.
pub fn resolve_by_repo_path(
    attachments: &[Attachment],
    repo_path: &str,
) -> Result<Option<usize>, String> {
    let canonical = Path::new(repo_path).canonicalize().ok();
    let mut matches = attachments
        .iter()
        .enumerate()
        .filter(|(_, attachment)| {
            attachment.repo_path == repo_path
                || canonical.as_ref().is_some_and(|root| {
                    Path::new(&attachment.repo_path)
                        .canonicalize()
                        .ok()
                        .as_ref()
                        == Some(root)
                })
        })
        .map(|(index, _)| index);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!("Multiple source attachment records refer to {repo_path}. Repair the conflicting records in attachments.yml before continuing; no records were changed."));
    }
    Ok(first)
}

#[cfg(test)]
pub fn find_by_repo_path(attachments: &[Attachment], repo_path: &str) -> Option<usize> {
    resolve_by_repo_path(attachments, repo_path).expect("unambiguous test fixture")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_file_returns_empty() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nonexistent.yml");
        let result = read_attachments(&path).unwrap();
        assert!(result.attachments.is_empty());
    }

    #[test]
    fn parse_error_on_malformed_yaml() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bad.yml");
        std::fs::write(&path, "not: valid: yaml: [[[").unwrap();
        let result = read_attachments(&path);
        assert!(
            matches!(result, Err(AttachmentsIoError::Parse(_))),
            "should be a parse error"
        );
    }

    #[test]
    fn write_and_read_roundtrip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("attachments.yml");

        let data = Attachments {
            attachments: vec![
                Attachment {
                    repo_path: "/Users/dev/repo-a".into(),
                    remote_url: Some("https://github.com/org/repo-a.git".into()),
                    attached_at: Some("2025-03-01T12:00:00Z".parse().unwrap()),
                    tracking_prompt_dismissed_at: None,
                    credential_ref: None,
                    managed: false,
                },
                Attachment {
                    repo_path: "/Users/dev/repo-b".into(),
                    remote_url: None,
                    attached_at: None,
                    tracking_prompt_dismissed_at: None,
                    credential_ref: None,
                    managed: false,
                },
            ],
        };

        write_attachments_atomic(&path, &data).unwrap();
        let roundtripped = read_attachments(&path).unwrap();
        assert_eq!(roundtripped.attachments.len(), 2);
        assert_eq!(roundtripped.attachments[0].repo_path, "/Users/dev/repo-a");
        assert_eq!(roundtripped.attachments[1].repo_path, "/Users/dev/repo-b");
    }

    #[test]
    fn find_by_repo_path_exact_match() {
        let attachments = vec![
            Attachment {
                repo_path: "/Users/dev/repo-a".into(),
                remote_url: None,
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
            Attachment {
                repo_path: "/Users/dev/repo-b".into(),
                remote_url: None,
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
        ];

        assert_eq!(
            find_by_repo_path(&attachments, "/Users/dev/repo-b"),
            Some(1)
        );
        assert_eq!(find_by_repo_path(&attachments, "/Users/dev/REPO-B"), None);
        assert_eq!(find_by_repo_path(&attachments, "/Users/dev/repo-c"), None);
    }
}
