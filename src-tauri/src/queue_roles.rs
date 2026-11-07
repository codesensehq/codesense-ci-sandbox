// SPDX-License-Identifier: MIT
//! Shared identity for durable team queues, stored beside the team's avatar packs.
use std::path::{Path, PathBuf};

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub default_handler: Option<String>,
}

pub fn normalize_role(role: &str) -> Result<String, String> {
    let id = role.trim().strip_prefix("role:").unwrap_or(role.trim());
    if id.is_empty() || id == "human" {
        return Err("a role needs a non-blank queue id other than human".into());
    }
    Ok(id.to_string())
}

fn path(content: &Path, id: &str) -> PathBuf {
    // Encode queue ids into filenames without changing their addresses.
    let key: String = id
        .bytes()
        .map(|b| {
            if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    content.join("roles").join(format!("{key}.yaml"))
}

pub fn list(content: &Path) -> Result<Vec<RoleConfig>, String> {
    list_for(content, &Default::default())
}

pub(crate) fn list_for(
    content: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<RoleConfig>, String> {
    use crate::agents::content_reader::ContentReader;
    let dir = content.join("roles");
    let entries = match scope.list(&dir) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = dir.join(entry);
        if !scope.allows(&path) {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        let config: RoleConfig = serde_yaml::from_str(&scope.read_admitted(&path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(config);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

pub fn save(content: &Path, mut config: RoleConfig) -> Result<RoleConfig, String> {
    config.id = normalize_role(&config.id)?;
    config.name = config.name.trim().to_string();
    if config.name.is_empty() {
        return Err("give the role a display name".into());
    }
    if config
        .icon
        .as_deref()
        .is_some_and(|v| crate::avatars::parse_face_ref(v).is_none())
    {
        return Err("icon must be a pack:id reference, or null for the default piece".into());
    }
    let dest = path(content, &config.id);
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    let yaml = serde_yaml::to_string(&config).map_err(|e| e.to_string())?;
    crate::content_engine::atomic_io::write_tmp_fsync_rename(&dest, yaml.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_distinct_roles_keep_separate_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let lower = RoleConfig {
            id: "qa".into(),
            name: "Quality".into(),
            icon: Some("builtin-chess:rook".into()),
            default_handler: Some("acme/dev/qa-handler".into()),
        };
        let upper = RoleConfig {
            id: "QA".into(),
            name: "Quality Assurance".into(),
            icon: Some("builtin-chess:knight".into()),
            default_handler: Some("acme/dev/QA-handler".into()),
        };

        save(tmp.path(), lower.clone()).unwrap();
        save(tmp.path(), upper.clone()).unwrap();

        assert_eq!(list(tmp.path()).unwrap(), vec![upper, lower]);
        assert!(tmp.path().join("roles/qa.yaml").is_file());
        assert!(tmp.path().join("roles/%51%41.yaml").is_file());
    }

    #[test]
    fn rename_and_icon_changes_preserve_the_queue_and_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let mut role = RoleConfig {
            id: "reviewer".into(),
            name: "Reviewer".into(),
            icon: None,
            default_handler: Some("acme/dev/review-bot".into()),
        };
        save(tmp.path(), role.clone()).unwrap();
        role.name = "Review".into();
        role.icon = Some("builtin-chess:knight".into());
        save(tmp.path(), role.clone()).unwrap();
        assert_eq!(list(tmp.path()).unwrap(), vec![role]);
    }
}

/// Missing keeps a saved default; JSON null explicitly clears it.
pub fn nullable_role<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

pub fn effective_role(
    saved: Option<&str>,
    choice: Option<&Option<String>>,
) -> Result<Option<String>, String> {
    match choice {
        None => saved.map(normalize_role).transpose(),
        Some(None) => Ok(None),
        Some(Some(role)) => normalize_role(role).map(Some),
    }
}
