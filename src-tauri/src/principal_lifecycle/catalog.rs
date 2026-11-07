// SPDX-License-Identifier: MIT
//! The local Principal catalog and the one durable active-Principal pointer.
//!
//! The catalog is an app-wide file (sibling of `servers.json` under
//! `~/.codesense/`, not a principal-scoped path) listing every local Principal
//! plus one `active` pointer. This type is pure: it validates and mutates an
//! in-memory catalog and knows its on-disk filename, but performs no I/O —
//! SW-PRN-1 owns reading/writing it through the credential/filesystem seam.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::Principal;

/// App-wide catalog filename, resolved against the codesense home directory.
pub const CATALOG_FILENAME: &str = "principals.json";

/// On-disk schema version. Bumped when the catalog layout changes so a reader
/// rejects an unknown shape rather than silently misparsing it; a forward
/// upgrade seam is future work once a v2 shape exists.
pub const CATALOG_VERSION: u32 = 1;

/// Absolute path to the catalog for a given codesense home.
pub fn catalog_path(codesense_home: &Path) -> PathBuf {
    codesense_home.join(CATALOG_FILENAME)
}

/// How a Principal came to exist. The GitHub numeric id is retained only as the
/// legacy bridge value (INV-1) — it is never promoted to a new path key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalOrigin {
    /// Generated natively by CodeSense (installs created after the migration).
    Native,
    /// Adopted from a legacy GitHub-derived install. The path segment stays
    /// byte-identical to the pre-migration `users/{github_user_id}` layout.
    LegacyGithub { github_user_id: u64 },
}

/// One catalog entry: the durable id plus display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRecord {
    pub id: Principal,
    pub display_name: String,
    pub origin: PrincipalOrigin,
}

impl PrincipalRecord {
    pub fn new(id: Principal, display_name: impl Into<String>, origin: PrincipalOrigin) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            origin,
        }
    }
}

/// The local Principal catalog: the set of known Principals and the single
/// active pointer. Serde round-trips through `version`, so an older/newer file
/// is a typed [`CatalogError::UnsupportedVersion`], never a silent misread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalCatalog {
    version: u32,
    records: Vec<PrincipalRecord>,
    active: Option<Principal>,
}

impl Default for PrincipalCatalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            records: Vec::new(),
            active: None,
        }
    }
}

impl PrincipalCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn list(&self) -> &[PrincipalRecord] {
        &self.records
    }

    pub fn get(&self, id: &Principal) -> Option<&PrincipalRecord> {
        self.records.iter().find(|r| &r.id == id)
    }

    /// The durably-committed active Principal, if the catalog has one.
    pub fn active(&self) -> Option<&Principal> {
        self.active.as_ref()
    }

    /// Insert or replace a record by id. Idempotent by design so the migration
    /// driver can re-run the prepare phase after an interruption without
    /// creating a duplicate entry.
    pub fn upsert(&mut self, record: PrincipalRecord) {
        match self.records.iter_mut().find(|r| r.id == record.id) {
            Some(existing) => *existing = record,
            None => self.records.push(record),
        }
    }

    /// Commit the active pointer. The target must already be a known record —
    /// an active pointer to an absent Principal is an invalid state we refuse to
    /// represent, so callers cannot leave a dangling pointer after migration.
    pub fn set_active(&mut self, id: &Principal) -> Result<(), CatalogError> {
        if self.get(id).is_none() {
            return Err(CatalogError::UnknownPrincipal(id.clone()));
        }
        self.active = Some(id.clone());
        Ok(())
    }

    /// Validate an already-deserialized catalog: supported version, no duplicate
    /// ids, and an active pointer (if any) that names a known record. Returns the
    /// catalog on success so it composes as `from_json(..)?.checked()?`.
    pub fn checked(self) -> Result<Self, CatalogError> {
        if self.version != CATALOG_VERSION {
            return Err(CatalogError::UnsupportedVersion(self.version));
        }
        for (i, r) in self.records.iter().enumerate() {
            if r.id.as_str().is_empty() {
                return Err(CatalogError::EmptyPrincipalId);
            }
            if self.records[..i].iter().any(|prev| prev.id == r.id) {
                return Err(CatalogError::DuplicatePrincipal(r.id.clone()));
            }
        }
        if let Some(active) = &self.active {
            if !self.records.iter().any(|r| &r.id == active) {
                return Err(CatalogError::UnknownPrincipal(active.clone()));
            }
        }
        Ok(self)
    }

    /// Parse and validate a catalog from JSON. A malformed file is a typed
    /// [`CatalogError::Corrupt`] so bootstrap can preserve the prior readable
    /// state and surface bounded recovery instead of guessing.
    pub fn from_json(raw: &str) -> Result<Self, CatalogError> {
        let parsed: Self =
            serde_json::from_str(raw).map_err(|e| CatalogError::Corrupt(e.to_string()))?;
        parsed.checked()
    }
}

/// Failure modes for catalog mutation and parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// Two records share an id.
    DuplicatePrincipal(Principal),
    /// A pointer or lookup named a Principal absent from the catalog.
    UnknownPrincipal(Principal),
    /// A record carries an empty id — an unusable path key.
    EmptyPrincipalId,
    /// The persisted `version` is not one this build understands.
    UnsupportedVersion(u32),
    /// The catalog bytes could not be parsed; the detail carries the cause.
    Corrupt(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::DuplicatePrincipal(p) => {
                write!(f, "duplicate principal in catalog: {p}")
            }
            CatalogError::UnknownPrincipal(p) => write!(f, "unknown principal: {p}"),
            CatalogError::EmptyPrincipalId => write!(f, "catalog record has an empty principal id"),
            CatalogError::UnsupportedVersion(v) => {
                write!(f, "unsupported catalog version: {v}")
            }
            CatalogError::Corrupt(detail) => write!(f, "corrupt catalog: {detail}"),
        }
    }
}

impl std::error::Error for CatalogError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, origin: PrincipalOrigin) -> PrincipalRecord {
        PrincipalRecord::new(Principal::from(id), id, origin)
    }

    #[test]
    fn catalog_path_is_app_wide_sibling() {
        let home = Path::new("/home/.codesense");
        assert_eq!(
            catalog_path(home),
            PathBuf::from("/home/.codesense/principals.json")
        );
    }

    #[test]
    fn upsert_is_idempotent_by_id() {
        let mut c = PrincipalCatalog::new();
        c.upsert(rec(
            "42",
            PrincipalOrigin::LegacyGithub { github_user_id: 42 },
        ));
        c.upsert(rec(
            "42",
            PrincipalOrigin::LegacyGithub { github_user_id: 42 },
        ));
        assert_eq!(c.list().len(), 1);
    }

    #[test]
    fn set_active_requires_known_principal() {
        let mut c = PrincipalCatalog::new();
        let ghost = Principal::from("99");
        assert_eq!(
            c.set_active(&ghost),
            Err(CatalogError::UnknownPrincipal(ghost.clone()))
        );
        c.upsert(rec("99", PrincipalOrigin::Native));
        assert!(c.set_active(&ghost).is_ok());
        assert_eq!(c.active(), Some(&ghost));
    }

    #[test]
    fn json_round_trips_with_version() {
        let mut c = PrincipalCatalog::new();
        c.upsert(rec("7", PrincipalOrigin::Native));
        c.set_active(&Principal::from("7")).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back = PrincipalCatalog::from_json(&json).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.version(), CATALOG_VERSION);
    }

    #[test]
    fn checked_rejects_unknown_active_pointer() {
        // Hand-craft a catalog whose active pointer names an absent record.
        let json = r#"{"version":1,"records":[],"active":"5"}"#;
        assert_eq!(
            PrincipalCatalog::from_json(json),
            Err(CatalogError::UnknownPrincipal(Principal::from("5")))
        );
    }

    #[test]
    fn checked_rejects_unsupported_version() {
        let json = r#"{"version":999,"records":[],"active":null}"#;
        assert_eq!(
            PrincipalCatalog::from_json(json),
            Err(CatalogError::UnsupportedVersion(999))
        );
    }

    #[test]
    fn corrupt_json_is_typed_not_a_panic() {
        assert!(matches!(
            PrincipalCatalog::from_json("{not json"),
            Err(CatalogError::Corrupt(_))
        ));
    }

    #[test]
    fn checked_rejects_empty_principal_id() {
        let json = r#"{"version":1,"records":[{"id":"","display_name":"x","origin":"Native"}],"active":null}"#;
        assert_eq!(
            PrincipalCatalog::from_json(json),
            Err(CatalogError::EmptyPrincipalId)
        );
    }

    #[test]
    fn checked_rejects_duplicate_ids() {
        let json = r#"{"version":1,"records":[{"id":"1","display_name":"a","origin":"Native"},{"id":"1","display_name":"b","origin":"Native"}],"active":null}"#;
        assert_eq!(
            PrincipalCatalog::from_json(json),
            Err(CatalogError::DuplicatePrincipal(Principal::from("1")))
        );
    }

    #[test]
    fn wire_shape_is_stable() {
        let mut c = PrincipalCatalog::new();
        c.upsert(PrincipalRecord::new(
            Principal::from("1234567"),
            "Fixture User",
            PrincipalOrigin::LegacyGithub {
                github_user_id: 1234567,
            },
        ));
        c.set_active(&Principal::from("1234567")).unwrap();
        // Pin the on-disk shape: a Rust rename that would silently break older
        // files without a CATALOG_VERSION bump fails this test instead.
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"version":1,"records":[{"id":"1234567","display_name":"Fixture User","origin":{"LegacyGithub":{"github_user_id":1234567}}}],"active":"1234567"}"#
        );
    }
}
