// SPDX-License-Identifier: MIT
//! SW-XS-7 (SPEC §11.2): the managed record of one destination — every file
//! Step 2 (or Step 3) wrote there, with the hash it wrote, who wrote it and
//! which layer it came from. Kept in its own file beside the worktree index,
//! never in the lockfile, so older lockfile readers are unaffected.
//!
//! The record doubles as the stat cache: each entry keeps the destination's
//! and the source's `(dev, ino, size, mtime_ns, ctime_ns)` as of the write, so
//! an unchanged file on either side is never read again.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::content_engine::destination::DestinationIdentity;
use crate::domain::Principal;

const VERSION: u32 = 1;

/// The origin of entries seeded from a legacy lockfile (or a team-workspace
/// dir staged by the old clear-and-recopy): files the old projection wrote.
pub const LEGACY_ORIGIN: &str = "legacy";

/// What identifies a file's state without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatKey {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
}

/// What one mtime "tick" is worth for racy-clean (SPEC §11.6): a stat that
/// is not older than the record's save by at least this much is not trusted,
/// because an edit inside the same tick can keep every field equal. A file
/// system with whole-second mtimes gets a whole second.
pub fn tick_ns(mtime_ns: i64) -> i64 {
    if mtime_ns % 1_000_000_000 == 0 {
        1_000_000_000
    } else {
        10_000_000
    }
}

impl StatKey {
    /// Whether a cached hash for this stat can be trusted: the stat equals
    /// `recorded`, and the file's mtime is at least one tick older than the
    /// record's save (`saved_at_ns`), so a same-tick edit cannot hide.
    pub fn trusted(&self, recorded: Option<StatKey>, saved_at_ns: i64) -> bool {
        recorded == Some(*self)
            && self.mtime_ns.saturating_add(tick_ns(self.mtime_ns)) <= saved_at_ns
    }

    #[cfg(unix)]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime_ns: meta
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.mtime_nsec()),
            ctime_ns: meta
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.ctime_nsec()),
        }
    }
}

/// A BLAKE3 hash, stored as hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash32(pub [u8; 32]);

impl Serialize for Hash32 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&blake3::Hash::from_bytes(self.0).to_hex())
    }
}
impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        blake3::Hash::from_hex(text.trim_start_matches("blake3:"))
            .map(|hash| Hash32(*hash.as_bytes()))
            .map_err(serde::de::Error::custom)
    }
}

/// One managed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<String>,
    /// What the engine last wrote there (or the legacy lockfile's
    /// `applied_hash` for a seeded entry).
    pub hash: Hash32,
    /// `step2:<agentId>`, `step3:<operationId>` or [`LEGACY_ORIGIN`].
    pub origin: String,
    /// The tier it was read from (`team:org/team`, `repo:host/owner/repo`,
    /// `personal`), the publish target for a later Step 3 reverse.
    pub source_layer: String,
    #[serde(default)]
    pub mode: u32,
    /// The destination file as written (local destinations only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_stat: Option<StatKey>,
    /// The source file it was copied from, as of the copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_stat: Option<StatKey>,
    /// SW-XS-8: the team-content file it was copied from: where an explicit
    /// publish writes it back (`None` for a seeded or older entry, which is
    /// resolved by its path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
}

/// A remote batch sent but not yet known to be settled: what the managed
/// record learns when its outcome is recovered (SPEC §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBatch {
    pub batch_id: uuid::Uuid,
    /// The batch's writes, by position in its `writes`.
    pub writes: Vec<(String, ManagedEntry)>,
    /// The batch's removals, by position in its `removes`.
    #[serde(default)]
    pub removes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedRecord {
    #[serde(default)]
    pub version: u32,
    /// When the record was last saved (UNIX ns): the racy-clean reference.
    #[serde(default)]
    pub saved_at_ns: i64,
    /// How the last Step 2 here ended (`ready`, `failed`): a workspace is
    /// "prepared" only once a run ended `ready` (X5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    /// Seeded from the legacy lockfile (or the old staging) already.
    #[serde(default)]
    pub seeded: bool,
    #[serde(default)]
    pub entries: BTreeMap<String, ManagedEntry>,
    /// This workspace's exclude block as last written (`None`: unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Remote batches sent and not yet known settled, oldest first: each is
    /// learned from a later recovery; none is dropped by a failed apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingBatch>,
    /// SW-XS-8: every context prefix a Step 2 or Step 3 run declared here:
    /// where an explicit publish looks for new files (SPEC §11.11).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub prefixes: std::collections::BTreeSet<String>,
    /// SW-XS-8: the epic directories `hydrate_story` brought here, by their
    /// team-content path (`list_hydrated`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub hydrated_epics: std::collections::BTreeSet<String>,
    /// SW-XS-8 (review r1): merged-file excludes XS-14 left in the clone's
    /// SHARED CodeSense block, kept there until a Step 2 has rewritten this
    /// workspace's own block (then removed from the shared one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_exclude_pending: Vec<String>,
}

/// Where `identity`'s record lives: beside the worktree index, one file per
/// destination.
pub fn record_path(home: &Path, principal: &Principal, identity: &DestinationIdentity) -> PathBuf {
    let key = serde_json::to_vec(identity).unwrap_or_default();
    crate::context_repo::path_to_user_home(home, principal)
        .join("worktrees")
        .join("managed-v1")
        .join(format!("{}.json", blake3::hash(&key).to_hex()))
}

impl ManagedRecord {
    /// The destination's record; empty when it has none. An unreadable
    /// record is moved aside (`*.corrupt-<ns>`, kept for diagnosis) and
    /// treated as empty: every file becomes unmanaged, so nothing is
    /// overwritten or removed.
    pub fn load(home: &Path, principal: &Principal, identity: &DestinationIdentity) -> Self {
        let path = record_path(home, principal, identity);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(record) => record,
                Err(error) => {
                    let aside = path.with_extension(format!("corrupt-{}", now_ns()));
                    let _ = std::fs::rename(&path, &aside);
                    log::warn!(
                        target: "agents",
                        "managed record {} is unreadable ({error}); moved to {} and every file treated as unmanaged",
                        path.display(),
                        aside.display()
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// SW-XS-27: the destination's record for a read-only caller (a count).
    /// Unlike [`Self::load`] it never touches the file: an unreadable or
    /// unparsable record is an error (the caller says it couldn't check),
    /// never moved aside and never read as empty. No record is an empty one.
    pub fn read(
        home: &Path,
        principal: &Principal,
        identity: &DestinationIdentity,
    ) -> Result<Self, String> {
        let path = record_path(home, principal, identity);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .map_err(|error| format!("its record of copied files is unreadable ({error})")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!(
                "its record of copied files can't be read ({error})"
            )),
        }
    }

    /// Whether this destination has a record at all.
    pub fn exists(home: &Path, principal: &Principal, identity: &DestinationIdentity) -> bool {
        record_path(home, principal, identity).is_file()
    }

    /// Whether the last Step 2 here ended `ready`.
    pub fn last_ready(home: &Path, principal: &Principal, identity: &DestinationIdentity) -> bool {
        Self::exists(home, principal, identity)
            && Self::load(home, principal, identity).last_status.as_deref() == Some("ready")
    }

    pub fn save(
        &mut self,
        home: &Path,
        principal: &Principal,
        identity: &DestinationIdentity,
    ) -> Result<(), String> {
        self.version = VERSION;
        self.saved_at_ns = now_ns();
        let path = record_path(home, principal, identity);
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        crate::content_engine::write_tmp_fsync_rename(&path, &bytes)
            .map_err(|e| format!("couldn't save the managed record: {e}"))
    }

    /// Forget this destination: its record goes (worktree teardown).
    pub fn remove(home: &Path, principal: &Principal, identity: &DestinationIdentity) {
        let _ = std::fs::remove_file(record_path(home, principal, identity));
    }

    /// Seed once from `(path, applied hash, layer)` rows: every file the old
    /// projection wrote becomes managed, so an unmodified one is replaced
    /// when stale and an unwanted one is offered to Clean up (SPEC §11.1).
    pub fn seed(&mut self, rows: impl IntoIterator<Item = (String, [u8; 32], String)>) -> usize {
        if self.seeded {
            return 0;
        }
        self.seeded = true;
        let mut seeded = 0;
        for (path, hash, layer) in rows {
            self.entries.entry(path).or_insert_with(|| {
                seeded += 1;
                ManagedEntry {
                    source_context: None,
                    hash: Hash32(hash),
                    origin: LEGACY_ORIGIN.into(),
                    source_layer: layer,
                    mode: 0,
                    dest_stat: None,
                    source_stat: None,
                    source: None,
                }
            });
        }
        seeded
    }

    /// The exclude block this record implies: every managed path.
    pub fn exclude_paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
}

/// The interim `generated-workspace-files` record XS-14 kept per local
/// workspace (its writer is gone since SW-XS-8; only records an older build
/// left remain).
#[derive(Debug, Deserialize)]
struct Interim {
    #[serde(default)]
    root: PathBuf,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

const INTERIM_DIR: &str = "generated-workspace-files";

fn interim_path(home: &Path, root: &Path) -> PathBuf {
    let key = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
    home.join(INTERIM_DIR).join(format!("{}.json", &key[..32]))
}

/// Fold `interim` into `record` (no I/O): each merged path becomes a managed
/// entry with the hash CodeSense wrote (an existing entry wins), and its
/// shared-block exclude is queued for removal once a Step 2 has rewritten
/// this workspace's own block. Generated entries never publish back
/// (`source_layer` `generated`).
fn fold_into(record: &mut ManagedRecord, interim: &Interim) {
    for (rel, hex) in &interim.files {
        let Ok(hash) = blake3::Hash::from_hex(hex) else {
            continue;
        };
        record
            .entries
            .entry(rel.clone())
            .or_insert_with(|| ManagedEntry {
                source_context: None,
                hash: Hash32(*hash.as_bytes()),
                origin: LEGACY_ORIGIN.into(),
                source_layer: super::want::GENERATED_LAYER.into(),
                mode: 0,
                dest_stat: None,
                source_stat: None,
                source: None,
            });
        let entry = format!("/{rel}");
        if !record.shared_exclude_pending.contains(&entry) {
            record.shared_exclude_pending.push(entry);
        }
    }
}

/// SW-XS-8 (the XS-14 handoff, review r1): fold the interim record of the
/// local workspace at `root` into `principal`'s managed record. Called by
/// [`super::prepare`] under the destination's lock, before it loads the
/// record. The interim file goes; its shared-block excludes stay until the
/// run has rewritten this workspace's own block
/// ([`release_shared_excludes`]). Best effort: a failure is logged and
/// retried on the next run.
pub fn fold_generated_workspace_files(home: &Path, principal: &Principal, root: &Path) {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = interim_path(home, &canonical);
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(interim) = serde_json::from_slice::<Interim>(&bytes) else {
        log::warn!(target: "agents", "unreadable merged-file record {}; left as it is", path.display());
        return;
    };
    let identity = crate::content_engine::context_lifecycle::local_identity(&canonical);
    let mut record = ManagedRecord::load(home, principal, &identity);
    fold_into(&mut record, &interim);
    if let Err(error) = record.save(home, principal, &identity) {
        log::warn!(target: "agents", "couldn't fold {}: {error}", path.display());
        return;
    }
    let _ = std::fs::remove_file(&path);
}

/// After a Step 2 run rewrote this workspace's own exclude block: drop the
/// entries XS-14 left in the clone's shared block. Returns whether the record
/// changed (the caller saves it).
pub fn release_shared_excludes(record: &mut ManagedRecord, root: &Path) -> bool {
    if record.shared_exclude_pending.is_empty() {
        return false;
    }
    if root.join(".git").exists() {
        let entries: Vec<&str> = record
            .shared_exclude_pending
            .iter()
            .map(String::as_str)
            .collect();
        if let Err(error) = crate::content_engine::git_exclude::remove_entries(root, &entries) {
            log::warn!(target: "agents", "couldn't drop interim excludes in {}: {error}", root.display());
            return false;
        }
    }
    record.shared_exclude_pending.clear();
    true
}

/// SW-XS-8 (review r2): a workspace being torn down gives back the shared-block
/// excludes still pending for it — from its managed record, or from an
/// interim record that was never folded. Best effort.
pub fn release_on_teardown(home: &Path, principal: &Principal, root: &Path) {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let identity = crate::content_engine::context_lifecycle::local_identity(&canonical);
    let mut record = ManagedRecord::load(home, principal, &identity);
    let interim_path = interim_path(home, &canonical);
    if let Some(interim) = std::fs::read(&interim_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Interim>(&bytes).ok())
    {
        fold_into(&mut record, &interim);
        let _ = std::fs::remove_file(&interim_path);
    }
    if release_shared_excludes(&mut record, &canonical)
        && ManagedRecord::exists(home, principal, &identity)
    {
        let _ = record.save(home, principal, &identity);
    }
}

/// The merged-file hash an unfolded interim record holds for `logical` in the
/// workspace at `root` (what a launch will fold as managed).
pub fn interim_hash(home: &Path, root: &Path, logical: &str) -> Option<[u8; 32]> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let interim: Interim =
        serde_json::from_slice(&std::fs::read(interim_path(home, &canonical)).ok()?).ok()?;
    let hex = interim.files.get(logical)?;
    blake3::Hash::from_hex(hex)
        .ok()
        .map(|hash| *hash.as_bytes())
}

/// Shared-block entries of interim records whose workspace is gone: drop them
/// from the clone's shared block when a live workspace of the same clone no
/// longer needs them. The clone is found through the workspace's story
/// registration (its managed source clone).
fn release_for_gone(
    home: &Path,
    interim: &Interim,
    still_needed: &std::collections::BTreeSet<String>,
) {
    let Ok(registrations) =
        crate::install::worktree_index::story_registrations(home, &interim.root)
    else {
        return;
    };
    let entries: Vec<String> = interim
        .files
        .keys()
        .map(|rel| format!("/{rel}"))
        .filter(|entry| !still_needed.contains(entry))
        .collect();
    let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
    for (principal, record) in registrations {
        let clone = crate::context_repo::path_to_managed_source_clone(
            home,
            &principal,
            &record.source_repo,
        );
        if clone.join(".git").exists() {
            if let Err(error) = crate::content_engine::git_exclude::remove_entries(&clone, &entries)
            {
                log::warn!(target: "agents", "couldn't drop interim excludes in {}: {error}", clone.display());
            }
        }
    }
}

/// Review r1: the one-shot boot sweep of interim records (`codesense_home`):
/// a record whose workspace is gone is deleted; one whose workspace exists is
/// folded into its owner's managed record when exactly one principal has it
/// registered (else it waits for that workspace's next launch, which folds it
/// for the launching user). Nothing runs once no interim record is left.
pub fn sweep_interim_records(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home.join(INTERIM_DIR)) else {
        return;
    };
    let records: Vec<(PathBuf, Interim)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let interim = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Interim>(&bytes).ok())?;
            (!interim.root.as_os_str().is_empty()).then_some((path, interim))
        })
        .collect();
    // What live workspaces' records still need in a shared block.
    let still_needed: std::collections::BTreeSet<String> = records
        .iter()
        .filter(|(_, interim)| interim.root.exists())
        .flat_map(|(_, interim)| interim.files.keys().map(|rel| format!("/{rel}")))
        .collect();
    for (path, interim) in records {
        if !interim.root.exists() {
            // Gone: give its shared-block entries back, then forget it.
            release_for_gone(home, &interim, &still_needed);
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let owners: Vec<Principal> =
            crate::install::worktree_index::story_registrations(home, &interim.root)
                .unwrap_or_default()
                .into_iter()
                .map(|(principal, _)| principal)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
        if let [owner] = owners.as_slice() {
            fold_generated_workspace_files(home, owner, &interim.root);
        }
    }
}

/// Now, as UNIX nanoseconds.
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
