// SPDX-License-Identifier: MIT
//! The versioned migration state machine that moves the single legacy auth blob
//! into per-Principal credential storage.
//!
//! The failure model (INV-4) is the whole point: there is exactly one durable
//! commit point (the active-pointer commit). Before it, any failure restores the
//! prior state; after it, recovery only moves forward. The driver never guesses
//! which of two secret copies is authoritative — duplicate material halts.
//!
//! Storage is expressed as two ports so the state machine is pure and fully
//! testable with in-memory fakes. SW-PRN-1 implements the ports over the real
//! keychain (`codesense-auth`/`auth-file`) and reuses its existing
//! write→verify→retire routine, of which this is the state-machine contract.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::Principal;

use super::catalog::{CatalogError, PrincipalCatalog, PrincipalOrigin, PrincipalRecord};

/// Journal schema version — the journal is the restart-recovery source of truth.
pub const JOURNAL_VERSION: u32 = 1;

/// Ordered phases of one Principal's credential migration. Persisted in the
/// journal so an interrupted run resumes at the exact phase it reached. Declared
/// in progression order so the derived `Ord` is the phase ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MigrationPhase {
    /// Nothing durable yet; safe to restart from scratch.
    NotStarted,
    /// Catalog record for the target exists and the journal is persisted.
    Prepared,
    /// Legacy blob copied into the per-Principal store (not yet verified).
    Copied,
    /// Read-back verified byte-identical to the legacy blob.
    Verified,
    /// Active pointer durably committed. THE single commit point — recovery past
    /// here only moves forward.
    Committed,
    /// Legacy blob retired (zeroed, not deleted — a recovery aid) post-commit.
    LegacyRetired,
    /// Terminal success.
    Done,
}

/// Restart-safe migration journal. `target` is the Principal being migrated to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationJournal {
    pub version: u32,
    pub phase: MigrationPhase,
    pub target: Principal,
}

impl MigrationJournal {
    pub fn start(target: Principal) -> Self {
        Self {
            version: JOURNAL_VERSION,
            phase: MigrationPhase::NotStarted,
            target,
        }
    }

    /// Serialize for the caller to persist between phase advances.
    pub fn to_json(&self) -> String {
        // A fixed-shape struct of plain fields cannot fail to serialize.
        serde_json::to_string(self).expect("journal serializes")
    }

    /// Parse a persisted journal; malformed bytes are a typed
    /// [`MigrationError::CorruptState`] and an unknown version a typed
    /// [`MigrationError::UnsupportedVersion`], so recovery is bounded, never a guess.
    pub fn from_json(raw: &str) -> Result<Self, MigrationError> {
        let parsed: Self = serde_json::from_str(raw).map_err(|e| MigrationError::CorruptState {
            detail: e.to_string(),
        })?;
        if parsed.version != JOURNAL_VERSION {
            return Err(MigrationError::UnsupportedVersion(parsed.version));
        }
        Ok(parsed)
    }
}

/// Reads the single legacy auth blob (one keychain entry today). Returns `None`
/// on a fresh install with nothing to migrate.
pub trait LegacyBlobSource {
    fn load(&self) -> Result<Option<String>, MigrationError>;
}

/// Per-Principal credential storage. Isolation is the contract: `read(a)` never
/// observes material written under a different Principal.
pub trait PrincipalCredentialStore {
    fn read(&self, principal: &Principal) -> Result<Option<String>, MigrationError>;
    fn write(&mut self, principal: &Principal, blob: &str) -> Result<(), MigrationError>;
    /// Retire the legacy blob after the commit point. Zeroed, not deleted.
    fn retire_legacy(&mut self) -> Result<(), MigrationError>;
}

/// What a (possibly resumed) run accomplished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// No legacy material; catalog bootstrapped and pointer committed.
    FreshInstall,
    /// Legacy material migrated (or confirmed already-migrated) and committed.
    Migrated,
}

/// Migration failure modes. All are recoverable to a readable prior state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    /// The store already holds *different* material for the target. We refuse to
    /// choose an authoritative copy (INV-4); the run halts for bounded recovery.
    DuplicateMaterial { principal: Principal },
    /// Read-back after copy did not match — the write→verify guard tripped.
    VerifyMismatch { principal: Principal },
    /// Journal or catalog could not be parsed; prior state is preserved.
    CorruptState { detail: String },
    /// A persisted journal `version` is not one this build understands.
    UnsupportedVersion(u32),
    /// An underlying store operation failed; detail carries the cause.
    Store { detail: String },
    /// A catalog invariant was violated while committing the pointer.
    Catalog(CatalogError),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationError::DuplicateMaterial { principal } => {
                write!(f, "duplicate credential material for principal {principal}")
            }
            MigrationError::VerifyMismatch { principal } => {
                write!(f, "credential read-back mismatch for principal {principal}")
            }
            MigrationError::CorruptState { detail } => {
                write!(f, "corrupt migration state: {detail}")
            }
            MigrationError::UnsupportedVersion(v) => {
                write!(f, "unsupported journal version: {v}")
            }
            MigrationError::Store { detail } => write!(f, "credential store error: {detail}"),
            MigrationError::Catalog(e) => write!(f, "catalog error during migration: {e}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<CatalogError> for MigrationError {
    fn from(e: CatalogError) -> Self {
        MigrationError::Catalog(e)
    }
}

/// Run (or resume) the migration for `record`'s Principal to completion.
///
/// Idempotent and restart-safe: each phase is guarded on the journal, so calling
/// this after an interruption at any phase drives forward to `Done` without
/// repeating committed work. The single commit point is the `set_active` pointer
/// write; a failure before it leaves the prior active pointer intact.
///
/// Restart-safety depends on the caller persisting `journal` (via
/// [`MigrationJournal::to_json`]) after each returned phase advance and before the
/// next side effect, so a crash resumes at the last durably-recorded phase.
pub fn run_migration(
    journal: &mut MigrationJournal,
    catalog: &mut PrincipalCatalog,
    record: &PrincipalRecord,
    source: &dyn LegacyBlobSource,
    store: &mut dyn PrincipalCredentialStore,
) -> Result<MigrationOutcome, MigrationError> {
    let target = record.id.clone();
    let is_legacy = matches!(record.origin, PrincipalOrigin::LegacyGithub { .. });

    // An empty id is an unusable path key. Reject it here so it can never reach
    // upsert/set_active, matching the catalog's own empty-id guard.
    if target.as_str().is_empty() {
        return Err(MigrationError::CorruptState {
            detail: "record has an empty principal id".to_string(),
        });
    }

    // A resumed journal must describe the same identity we were handed. A
    // disagreement means the journal and record are out of sync — halt for
    // bounded recovery rather than silently migrate the wrong Principal.
    if journal.phase != MigrationPhase::NotStarted && journal.target != target {
        return Err(MigrationError::CorruptState {
            detail: format!(
                "journal target {} does not match record {}",
                journal.target, target
            ),
        });
    }

    // Prepare: ensure the catalog record exists. Idempotent and phase-independent,
    // so a run resumed past NotStarted still guarantees the record is present
    // before the pointer commit (set_active requires a known record).
    catalog.upsert(record.clone());
    if journal.phase == MigrationPhase::NotStarted {
        journal.phase = MigrationPhase::Prepared;
    }

    // The legacy blob is only needed through the copy/verify phases; once past
    // the commit point it has been (or will be) retired.
    let legacy = if journal.phase < MigrationPhase::Verified {
        source.load()?
    } else {
        None
    };

    // Copy: move the legacy blob into per-Principal storage. A fresh install has
    // no blob and skips straight to the shared commit path.
    if journal.phase == MigrationPhase::Prepared {
        match &legacy {
            Some(blob) => {
                match store.read(&target)? {
                    Some(existing) if existing != *blob => {
                        return Err(MigrationError::DuplicateMaterial { principal: target });
                    }
                    Some(_) => { /* already copied — idempotent resume */ }
                    None => store.write(&target, blob)?,
                }
                journal.phase = MigrationPhase::Copied;
            }
            None => journal.phase = MigrationPhase::Verified,
        }
    }

    // Verify: read back and byte-compare (reuse of the migrate_into_store guard).
    if journal.phase == MigrationPhase::Copied {
        let blob = legacy
            .as_ref()
            .ok_or_else(|| MigrationError::CorruptState {
                detail: "reached verify with no legacy blob".to_string(),
            })?;
        match store.read(&target)? {
            Some(readback) if readback == *blob => journal.phase = MigrationPhase::Verified,
            _ => return Err(MigrationError::VerifyMismatch { principal: target }),
        }
    }

    // Commit: the single durable commit point.
    if journal.phase == MigrationPhase::Verified {
        catalog.set_active(&target)?;
        journal.phase = MigrationPhase::Committed;
    }

    // Retire legacy material — only after the commit, only if there was any.
    if journal.phase == MigrationPhase::Committed {
        if is_legacy {
            store.retire_legacy()?;
        }
        journal.phase = MigrationPhase::LegacyRetired;
    }

    if journal.phase == MigrationPhase::LegacyRetired {
        journal.phase = MigrationPhase::Done;
    }

    Ok(if is_legacy {
        MigrationOutcome::Migrated
    } else {
        MigrationOutcome::FreshInstall
    })
}

/// INV-1 guard for the IPC boundary: a command carries exactly one identity.
/// A request bearing both a legacy `github_user_id` and an explicit `principal`
/// is ambiguous and rejected rather than silently preferring one path.
pub fn resolve_identity(
    github_user_id: Option<u64>,
    principal: Option<Principal>,
) -> Result<Principal, IdentityError> {
    match (github_user_id, principal) {
        (Some(_), Some(_)) => Err(IdentityError::Ambiguous),
        // An empty explicit principal is no usable identity — reject it at the
        // seam rather than let an empty path key flow downstream.
        (None, Some(p)) if !p.as_str().is_empty() => Ok(p),
        (None, Some(_)) => Err(IdentityError::Missing),
        (Some(id), None) => Ok(Principal::from(id)),
        (None, None) => Err(IdentityError::Missing),
    }
}

/// Failure modes for [`resolve_identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// Both a `github_user_id` and a `principal` were supplied (INV-1).
    Ambiguous,
    /// Neither identity parameter was supplied.
    Missing,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::Ambiguous => {
                write!(f, "request carries both github_user_id and principal")
            }
            IdentityError::Missing => write!(f, "request carries no identity"),
        }
    }
}

impl std::error::Error for IdentityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory fakes standing in for the keychain (SW-PRN-1 supplies the real
    /// adapters). The store keys material by Principal to prove isolation.
    struct FakeSource {
        blob: Option<String>,
    }
    impl LegacyBlobSource for FakeSource {
        fn load(&self) -> Result<Option<String>, MigrationError> {
            Ok(self.blob.clone())
        }
    }

    #[derive(Default)]
    struct FakeStore {
        per_principal: HashMap<String, String>,
        legacy_retired: bool,
        fail_write: bool,
    }
    impl PrincipalCredentialStore for FakeStore {
        fn read(&self, principal: &Principal) -> Result<Option<String>, MigrationError> {
            Ok(self.per_principal.get(principal.as_str()).cloned())
        }
        fn write(&mut self, principal: &Principal, blob: &str) -> Result<(), MigrationError> {
            if self.fail_write {
                return Err(MigrationError::Store {
                    detail: "injected write failure".to_string(),
                });
            }
            self.per_principal
                .insert(principal.as_str().to_string(), blob.to_string());
            Ok(())
        }
        fn retire_legacy(&mut self) -> Result<(), MigrationError> {
            self.legacy_retired = true;
            Ok(())
        }
    }

    fn legacy_record(id: u64) -> PrincipalRecord {
        PrincipalRecord::new(
            Principal::from(id),
            format!("user {id}"),
            PrincipalOrigin::LegacyGithub { github_user_id: id },
        )
    }

    fn native_record(id: &str) -> PrincipalRecord {
        PrincipalRecord::new(Principal::from(id), id, PrincipalOrigin::Native)
    }

    // Fixture 1 — fresh-install bootstrap.
    #[test]
    fn fresh_install_bootstraps_without_migrating() {
        let record = native_record("new-1");
        let mut journal = MigrationJournal::start(record.id.clone());
        let mut catalog = PrincipalCatalog::new();
        let source = FakeSource { blob: None };
        let mut store = FakeStore::default();

        let outcome =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap();

        assert_eq!(outcome, MigrationOutcome::FreshInstall);
        assert_eq!(journal.phase, MigrationPhase::Done);
        assert_eq!(catalog.active(), Some(&record.id));
        assert!(store.per_principal.is_empty(), "no credential copied");
        assert!(
            !store.legacy_retired,
            "nothing to retire on a fresh install"
        );
    }

    // Fixture 2 — legacy GitHub install: migrate, byte-identical path, retire.
    #[test]
    fn legacy_install_migrates_and_retires() {
        let record = legacy_record(1234567);
        let mut journal = MigrationJournal::start(record.id.clone());
        let mut catalog = PrincipalCatalog::new();
        let source = FakeSource {
            blob: Some("auth-blob".to_string()),
        };
        let mut store = FakeStore::default();

        let outcome =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap();

        assert_eq!(outcome, MigrationOutcome::Migrated);
        assert_eq!(journal.phase, MigrationPhase::Done);
        assert_eq!(
            store.per_principal.get("1234567").map(String::as_str),
            Some("auth-blob")
        );
        assert!(store.legacy_retired);
        // Byte-identical: the adopted Principal is the same path segment as the
        // pre-migration github_user_id layout.
        assert_eq!(record.id.as_str(), "1234567");
    }

    // Fixture 3a — interruption at each phase resumes idempotently to Done.
    #[test]
    fn resume_from_each_phase_reaches_done() {
        for start in [
            MigrationPhase::Prepared,
            MigrationPhase::Copied,
            MigrationPhase::Verified,
            MigrationPhase::Committed,
            MigrationPhase::LegacyRetired,
        ] {
            let record = legacy_record(42);
            let mut catalog = PrincipalCatalog::new();
            catalog.upsert(record.clone());
            let mut store = FakeStore::default();
            // Reconstruct store state consistent with the resume phase.
            if start >= MigrationPhase::Copied {
                store
                    .per_principal
                    .insert("42".to_string(), "auth-blob".to_string());
            }
            if start >= MigrationPhase::LegacyRetired {
                store.legacy_retired = true;
            }
            let mut journal = MigrationJournal {
                version: JOURNAL_VERSION,
                phase: start,
                target: record.id.clone(),
            };
            let source = FakeSource {
                blob: Some("auth-blob".to_string()),
            };

            let outcome =
                run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap();

            assert_eq!(outcome, MigrationOutcome::Migrated, "resume from {start:?}");
            assert_eq!(journal.phase, MigrationPhase::Done, "resume from {start:?}");
            assert!(store.legacy_retired, "resume from {start:?}");
        }
    }

    // Fixture 3b — pre-commit failure restores the prior pointer (never committed).
    #[test]
    fn pre_commit_failure_leaves_active_pointer_untouched() {
        let record = legacy_record(7);
        let mut journal = MigrationJournal::start(record.id.clone());
        let mut catalog = PrincipalCatalog::new();
        let source = FakeSource {
            blob: Some("auth-blob".to_string()),
        };
        let mut store = FakeStore {
            fail_write: true,
            ..Default::default()
        };

        let err =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap_err();

        assert!(matches!(err, MigrationError::Store { .. }));
        assert_eq!(catalog.active(), None, "no commit before the commit point");
        assert!(journal.phase < MigrationPhase::Committed);
    }

    // Fixture 4 — corrupt journal parses to a typed error, not a panic.
    #[test]
    fn corrupt_journal_is_bounded_recovery() {
        assert!(matches!(
            MigrationJournal::from_json("{garbage"),
            Err(MigrationError::CorruptState { .. })
        ));
    }

    // Fixture 5 — duplicate keychain material halts without guessing.
    #[test]
    fn duplicate_material_halts() {
        let record = legacy_record(8);
        let mut journal = MigrationJournal::start(record.id.clone());
        let mut catalog = PrincipalCatalog::new();
        let source = FakeSource {
            blob: Some("new-bytes".to_string()),
        };
        let mut store = FakeStore::default();
        // Store already holds DIFFERENT material for the target.
        store
            .per_principal
            .insert("8".to_string(), "other-bytes".to_string());

        let err =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap_err();

        assert_eq!(
            err,
            MigrationError::DuplicateMaterial {
                principal: Principal::from(8u64)
            }
        );
        assert_eq!(catalog.active(), None);
    }

    // Fixture 6 — recovery after the commit point completes forward.
    #[test]
    fn recovery_after_commit_completes_forward() {
        let record = legacy_record(9);
        let mut catalog = PrincipalCatalog::new();
        catalog.upsert(record.clone());
        catalog.set_active(&record.id).unwrap(); // already committed
        let mut store = FakeStore::default();
        store
            .per_principal
            .insert("9".to_string(), "auth-blob".to_string());
        let mut journal = MigrationJournal {
            version: JOURNAL_VERSION,
            phase: MigrationPhase::Committed,
            target: record.id.clone(),
        };
        let source = FakeSource { blob: None }; // legacy already unreadable post-commit

        let outcome =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap();

        assert_eq!(outcome, MigrationOutcome::Migrated);
        assert_eq!(journal.phase, MigrationPhase::Done);
        assert!(store.legacy_retired);
    }

    // Fixture 7 — two Principals sharing an org remote keep isolated credentials.
    #[test]
    fn two_principals_do_not_cross_read_credentials() {
        let mut store = FakeStore::default();
        let a = Principal::from("a");
        let b = Principal::from("b");
        store.write(&a, "secret-a").unwrap();
        store.write(&b, "secret-b").unwrap();
        assert_eq!(store.read(&a).unwrap().as_deref(), Some("secret-a"));
        assert_eq!(store.read(&b).unwrap().as_deref(), Some("secret-b"));
        assert_ne!(store.read(&a).unwrap(), store.read(&b).unwrap());
    }

    // Fixture 8 — dual identity parameter is rejected (INV-1).
    #[test]
    fn dual_identity_parameter_is_rejected() {
        assert_eq!(
            resolve_identity(Some(42), Some(Principal::from("42"))),
            Err(IdentityError::Ambiguous)
        );
        assert_eq!(resolve_identity(Some(42), None), Ok(Principal::from(42u64)));
        assert_eq!(
            resolve_identity(None, Some(Principal::from("x"))),
            Ok(Principal::from("x"))
        );
        assert_eq!(resolve_identity(None, None), Err(IdentityError::Missing));
    }

    // Resuming at Prepared when the store already holds the SAME bytes exercises
    // the matching-bytes idempotent-copy arm without a double write.
    #[test]
    fn resume_at_prepared_with_matching_bytes_is_idempotent() {
        let record = legacy_record(11);
        let mut catalog = PrincipalCatalog::new();
        catalog.upsert(record.clone());
        let mut store = FakeStore::default();
        store
            .per_principal
            .insert("11".to_string(), "auth-blob".to_string());
        let mut journal = MigrationJournal {
            version: JOURNAL_VERSION,
            phase: MigrationPhase::Prepared,
            target: record.id.clone(),
        };
        let source = FakeSource {
            blob: Some("auth-blob".to_string()),
        };

        let outcome =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap();

        assert_eq!(outcome, MigrationOutcome::Migrated);
        assert_eq!(journal.phase, MigrationPhase::Done);
        assert_eq!(
            store.per_principal.get("11").map(String::as_str),
            Some("auth-blob")
        );
    }

    // A resumed journal whose target disagrees with the record halts for bounded
    // recovery rather than migrating the wrong identity.
    #[test]
    fn journal_target_mismatch_halts() {
        let record = legacy_record(12);
        let mut catalog = PrincipalCatalog::new();
        catalog.upsert(record.clone());
        let mut store = FakeStore::default();
        let mut journal = MigrationJournal {
            version: JOURNAL_VERSION,
            phase: MigrationPhase::Prepared,
            target: Principal::from("999"),
        };
        let source = FakeSource {
            blob: Some("auth-blob".to_string()),
        };

        let err =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap_err();

        assert!(matches!(err, MigrationError::CorruptState { .. }));
    }

    #[test]
    fn resolve_identity_rejects_empty_principal() {
        assert_eq!(
            resolve_identity(None, Some(Principal::from(""))),
            Err(IdentityError::Missing)
        );
    }

    #[test]
    fn run_migration_rejects_empty_id() {
        let record = PrincipalRecord::new(Principal::from(""), "x", PrincipalOrigin::Native);
        let mut journal = MigrationJournal::start(record.id.clone());
        let mut catalog = PrincipalCatalog::new();
        let source = FakeSource { blob: None };
        let mut store = FakeStore::default();

        let err =
            run_migration(&mut journal, &mut catalog, &record, &source, &mut store).unwrap_err();

        assert!(matches!(err, MigrationError::CorruptState { .. }));
        assert!(catalog.list().is_empty(), "no empty-id record upserted");
    }

    #[test]
    fn journal_json_round_trips() {
        let journal = MigrationJournal {
            version: JOURNAL_VERSION,
            phase: MigrationPhase::Committed,
            target: Principal::from("42"),
        };
        let back = MigrationJournal::from_json(&journal.to_json()).unwrap();
        assert_eq!(back, journal);
    }

    #[test]
    fn journal_rejects_unsupported_version() {
        let json = r#"{"version":999,"phase":"NotStarted","target":"42"}"#;
        assert_eq!(
            MigrationJournal::from_json(json),
            Err(MigrationError::UnsupportedVersion(999))
        );
    }
}
