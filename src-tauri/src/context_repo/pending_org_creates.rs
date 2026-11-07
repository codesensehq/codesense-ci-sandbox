// SPDX-License-Identifier: MIT
//! Principal-local, non-secret recovery journal for shared org creation.

use crate::domain::CredentialRef;
use crate::shared::yaml_io::{read_yaml_or_default, YamlIoError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PendingCreateMode {
    Create,
    Select,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum PendingCreateStage {
    Journaled,
    Scaffolded,
    CancellingPreRemote,
    RemoteConfirmed,
    PushedAndVerified,
    BindingPersisted,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingRemoteIdentity {
    pub provider: String,
    pub host: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingOrgCreate {
    pub schema_version: u32,
    pub generation: String,
    pub remote: PendingRemoteIdentity,
    pub scaffold: PathBuf,
    pub scaffold_fingerprint: String,
    pub workspace_name: String,
    pub credential_ref: CredentialRef,
    pub mode: PendingCreateMode,
    pub private: Option<bool>,
    pub stage: PendingCreateStage,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingOrgCreates {
    #[serde(default)]
    entries: BTreeMap<String, PendingOrgCreate>,
}

#[derive(Debug)]
pub enum PendingCreateError {
    Io(YamlIoError),
    InvalidSlug,
    InvalidRecord,
}

impl std::fmt::Display for PendingCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "pending create journal I/O failed: {error}"),
            Self::InvalidSlug => f.write_str("invalid normalized org slug"),
            Self::InvalidRecord => f.write_str("invalid pending create record"),
        }
    }
}

impl std::error::Error for PendingCreateError {}

impl From<YamlIoError> for PendingCreateError {
    fn from(value: YamlIoError) -> Self {
        Self::Io(value)
    }
}

fn normalize_slug(slug: &str) -> Result<String, PendingCreateError> {
    let slug = super::org_bindings::normalize_org_slug(slug);
    if slug.is_empty()
        || slug.starts_with('/')
        || slug
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(PendingCreateError::InvalidSlug);
    }
    Ok(slug)
}

fn valid_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn validate(record: &PendingOrgCreate) -> Result<(), PendingCreateError> {
    if record.schema_version != SCHEMA_VERSION
        || record.generation.trim().is_empty()
        || record.remote.provider.trim().is_empty()
        || record.remote.host.trim().is_empty()
        || record.remote.owner.trim().is_empty()
        || record.remote.repo.trim().is_empty()
        || record.credential_ref.provider != record.remote.provider
        || !valid_relative(&record.scaffold)
        || record.scaffold_fingerprint.trim().is_empty()
        || record.workspace_name.trim().is_empty()
        || (record.mode == PendingCreateMode::Create && record.private.is_none())
        || (record.mode == PendingCreateMode::Select && record.private.is_some())
    {
        return Err(PendingCreateError::InvalidRecord);
    }
    Ok(())
}

fn read_store(path: &Path) -> Result<PendingOrgCreates, PendingCreateError> {
    let store: PendingOrgCreates = read_yaml_or_default(path)?;
    for (slug, record) in &store.entries {
        if normalize_slug(slug)? != *slug {
            return Err(PendingCreateError::InvalidSlug);
        }
        validate(record)?;
    }
    Ok(store)
}

pub fn list_pending(path: &Path) -> Result<BTreeMap<String, PendingOrgCreate>, PendingCreateError> {
    Ok(read_store(path)?.entries)
}

#[cfg(test)]
mod tests {}
