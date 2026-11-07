// SPDX-License-Identifier: MIT
//! Lockfile v3 schema, CRUD, and per-(team, code-repo) path resolution — SS.3.
//!
//! The v3 lockfile is layer-id-agnostic: every materialization is a
//! single [`MaterializedEntry`] regardless of which layer supplied the
//! content. Adding org/personal layers in a future sub-epic does not
//! change the schema — only [`super::layers::LayerId`] grows. The schema
//! also drops v2's `gitignore_entries` (managed via
//! [`super::git_exclude`] under the new engine) and v2's
//! `install_symlinks`/`story_symlinks` split (one entry type covers
//! every materialization origin).
//!
//! The retired v2 install lockfile and v3/v4 lockfiles lived at **disjoint
//! paths**, so the current engine never collided with v2 state (SS-11b
//! deleted the v2 lockfile module). [`Lockfile::read`] migrates any version
//! in `MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION` forward in-memory
//! (SLW-RS-1: a v3 lockfile loads with `source_oid` defaulted to `None`) and
//! rejects anything outside that range defensively, in case a future
//! regression writes a stray incompatible document at a live path.
//!
//! Atomic writes go through [`super::atomic_io::write_tmp_fsync_rename`]
//! — **not** `crate::shared::json_io::write_json_atomic` — because the
//! latter does not fsync, and lockfile durability across power loss is
//! load-bearing for the seamless-sync engine.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::layers::{is_safe_logical_name, validate_path_segment, LayerId, ResolverContext};
use super::sync_state_store::{self, SyncStateStore};
use super::Error;
use crate::context_repo::{
    attachments_io::read_attachments, path_to_install_root, path_to_org_root,
    path_to_user_attachments_file, CODESENSE_INTERNAL_DIR, INSTALLS_DIR, ORGS_DIR, USERS_DIR,
};
use crate::domain::SourceRepoSlug;

/// Lockfile schema version. Bumped when the on-disk shape changes. The
/// constant is the single source of truth — no version literal lives
/// anywhere else in the module.
///
/// SLW-RS-1: bumped 3→4 for the additive `source_oid` field on
/// [`MaterializedEntry`]. The read path migrates any version in
/// `MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION` forward in-memory — a v3
/// lockfile (no `source_oid`) loads with the field defaulted to `None`,
/// recomputed on the next materialize.
pub const SCHEMA_VERSION: u32 = 4;

/// Oldest on-disk schema version the read path still migrates forward.
/// A v3 lockfile predates `source_oid` (SLW-RS-1); it loads via the
/// tolerant [`WireEntry`] path with `source_oid == None`. Anything older
/// (v2 lived at a disjoint path — see module docs) still surfaces as
/// [`Error::UnsupportedSchemaVersion`].
pub const MIN_SUPPORTED_SCHEMA_VERSION: u32 = 3;

/// SLW-RS-1: the single schema-version gate shared by BOTH read paths — the
/// full parse ([`Lockfile::from_bytes_counting_dropped`]) and the lightweight
/// `code_repo_path` header probe ([`parse_code_repo_path`]). A version in
/// `MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION` is migratable-forward (Ok);
/// anything outside surfaces as [`Error::UnsupportedSchemaVersion`]. Funneling
/// both callers through one predicate is deliberate: an earlier draft that
/// updated only one guard let the two drift, so the probe rejected a v3
/// lockfile the full parse accepted — silently breaking the orphan sweep.
fn check_supported_schema_version(found: u32) -> Result<(), Error> {
    if !(MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&found) {
        return Err(Error::UnsupportedSchemaVersion {
            found,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// String substituted for `team_name = None` in [`lockfile_path`]. Picked
/// to be visibly distinct from any valid team name (which is validated to
/// reject leading dots; see [`validate_team_name`]).
pub(crate) const NO_TEAM_SEGMENT: &str = "_none";

/// P4 (iter-3): hard upper bound on lockfile size to guard against
/// OOM if a corrupted or malicious lockfile is gigabytes. Real
/// lockfiles are bounded by the number of materialized entries (each
/// entry is a few hundred bytes); 16 MiB is multiple orders of
/// magnitude above any realistic shape.
const MAX_LOCKFILE_BYTES: u64 = 16 * 1024 * 1024;

/// v3 lockfile per `(org, team, code-repo)`. Records what codesense has
/// materialized into a specific code repo, with per-destination hash +
/// mtime tracking so the sync engine can detect both upstream and
/// downstream drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
// P7 (iter-2): `deny_unknown_fields` prevents a typo'd field name (e.g.
// `enteries` instead of `entries`) from deserializing successfully with
// the misspelled field dropped on the floor — that failure mode would
// silently corrupt resolver state on next write.
#[serde(deny_unknown_fields)]
pub struct Lockfile<Dest = PathBuf, Root = PathBuf> {
    /// Always equals [`SCHEMA_VERSION`] for live lockfiles. Older values
    /// surface as [`Error::UnsupportedSchemaVersion`] on read.
    pub schema_version: u32,
    /// Org this lockfile belongs to. Matches the `org_slug` segment of
    /// [`lockfile_path`].
    pub org_slug: String,
    /// Active team for this materialization state. `None` means a no-team
    /// install context; the [`lockfile_path`] uses the literal `_none`
    /// segment in that case.
    pub team_name: Option<String>,
    /// Absolute path to the code repo this lockfile tracks. Stored so
    /// that consumers can detect a "lockfile points at a moved repo"
    /// situation without re-deriving from filesystem traversal.
    pub code_repo_path: Root,
    /// One entry per logical name currently materialized into the code
    /// repo. Order is the order entries were inserted; consumers needing
    /// O(1) lookup should call [`Lockfile::entries_by_name`].
    pub entries: Vec<MaterializedEntry<Dest>>,
}

impl<Dest, Root> Lockfile<Dest, Root> {
    /// Change only destination representation; source ownership and every applied
    /// baseline field remain owned by the same lockfile model.
    pub fn map_destination<NextDest, NextRoot>(
        self,
        root: impl FnOnce(Root) -> NextRoot,
        mut entry: impl FnMut(Dest) -> NextDest,
    ) -> Lockfile<NextDest, NextRoot> {
        Lockfile {
            schema_version: self.schema_version,
            org_slug: self.org_slug,
            team_name: self.team_name,
            code_repo_path: root(self.code_repo_path),
            entries: self
                .entries
                .into_iter()
                .map(|e| e.map_destination(&mut entry))
                .collect(),
        }
    }
}

/// One materialized file's bookkeeping. README-committed shape: both
/// `dest_path` and `source_path` are stored explicitly even though both
/// are technically derivable, so that materialization-time lookups don't
/// re-resolve layers on every call and so the lockfile is debuggable
/// without rehydrating the resolver context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaterializedEntry<Dest = PathBuf> {
    /// Layer-root-relative `/`-separated path.
    pub logical_name: String,
    /// Absolute path of the materialized file inside the code repo.
    ///
    /// **Informational field** (iter-1 code review D3): downstream sync
    /// helpers (e.g., the projection materialize) recompute
    /// `code_repo_path.join(logical_name)` from the inputs they already
    /// carry rather than reading this field. Stored so the lockfile is
    /// debuggable without rehydrating the resolver context.
    ///
    /// Iter-3 BH3-MED-1: an earlier `debug_assert_eq!` checking this
    /// field against the caller's computed dest was dropped — it
    /// false-tripped on benign macOS path-equivalence drift
    /// (`/var/folders/...` vs `/private/var/folders/...`). The
    /// production invariant comes from callers passing a consistent
    /// `code_repo_path` per call, not from this struct field.
    pub dest_path: Dest,
    /// Which layer the content currently comes from. Changing layers
    /// (resolver shift) overwrites this in place — no separate
    /// "previous layer" tracking.
    pub source_layer: LayerId,
    /// Absolute source path on disk. Every layer is filesystem-backed.
    pub source_path: PathBuf,
    /// blake3 of the last bytes codesense wrote into `dest_path`.
    /// Compared against the file's current hash to detect user edits
    /// (skip-silently path).
    pub last_materialized_hash: [u8; 32],
    /// Nanoseconds-since-`UNIX_EPOCH` mtime captured when codesense
    /// wrote `dest_path`. The fast gate before
    /// [`last_materialized_hash`]: equal mtime ⇒ provably unchanged.
    pub last_materialized_mtime_ns: u128,
    /// SS-7: projection identity discriminator.
    ///
    /// - `None` for the retired resolver-driven (install-layer) entries —
    ///   drained on store read post-SW-CP-31b, so no live entry carries it.
    /// - `Some("story")` for entries written by SS-7's story projection.
    /// - `Some("docs")` / `Some("runbooks")` / etc. for future projections
    ///   that share the projection-engine flow.
    ///
    /// `#[serde(default)]` makes the field optional on read: existing v3
    /// lockfiles written by SS-3..SS-4c (which predate this field) round-
    /// trip cleanly with `projection_key == None`. Together with the
    /// [`LayerId::TeamProjection`] discriminator on `source_layer`, this
    /// field lets the projection engine's force-unmaterialize pass scope
    /// strictly to its own entries — e.g., a `story` projection re-run
    /// cannot accidentally delete `docs` projection entries (see
    /// `install::story_linker::materialize_projection`).
    #[serde(default)]
    pub projection_key: Option<String>,
    /// SLW-RS-1: context-repo git `HEAD` commit OID at materialize time —
    /// the source git state this entry was materialized from. Captured once
    /// per wave via [`context_repo_head_oid`] and stamped on every entry the
    /// wave (re)writes; `None` for entries created outside a forward wave
    /// (adopt, structural rename) or loaded from a pre-SLW-RS-1 (v3) lockfile
    /// — recomputed on the next materialize.
    ///
    /// **One anchor, two uses:** (a) SLW-RS-3's commit-before-clobber
    /// recovery anchor — recover the tracked source via
    /// `git show <source_oid>:<source_relpath>` (best-effort; RS-3's own live
    /// snapshot is the guaranteed net); (b) SLW-FX-4's O(1) launch
    /// source-signature ("did team content move since last derive?"). It is a
    /// **commit** OID (whole-source-tree signature), NOT a per-file blob OID —
    /// a blob cannot answer (b). **Not** a 3-way merge base; reverse stays the
    /// hash classifier.
    ///
    /// `#[serde(default)]` keeps v3 lockfiles loading (SLW-RS-1 AC-3).
    #[serde(default)]
    pub source_oid: Option<String>,
    /// SLW-RS-2 AC-8: byte length of the last bytes codesense wrote into
    /// `dest_path`, captured from the same `metadata` read as
    /// [`last_materialized_mtime_ns`]. Pairs with the mtime fast-gate
    /// (`classify_dest_against_prev`): a dest whose mtime matches the baseline
    /// but whose **size** differs is provably modified without hashing (rsync's
    /// mtime+size quick-check; git's `ce_match_stat`). `None` for entries
    /// written before SLW-RS-2 (loaded from an older lockfile) or where the
    /// post-write stat failed — treated as "size unknown, fall through to the
    /// hash" so a `None` never fast-paths a stale dest. Every current write path
    /// (forward materialize, structural rename, reverse adopt, reverse/conflict
    /// re-baseline) stamps the exact written length.
    ///
    /// `#[serde(default)]` keeps older lockfiles loading (additive-optional,
    /// no schema bump — SLW-RS-1 D1 precedent).
    #[serde(default)]
    pub last_materialized_size: Option<u64>,
}

impl<Dest> MaterializedEntry<Dest> {
    pub fn map_destination<Next>(self, map: impl FnOnce(Dest) -> Next) -> MaterializedEntry<Next> {
        MaterializedEntry {
            logical_name: self.logical_name,
            dest_path: map(self.dest_path),
            source_layer: self.source_layer,
            source_path: self.source_path,
            last_materialized_hash: self.last_materialized_hash,
            last_materialized_mtime_ns: self.last_materialized_mtime_ns,
            projection_key: self.projection_key,
            source_oid: self.source_oid,
            last_materialized_size: self.last_materialized_size,
        }
    }
}

/// SW-CP-7 AC-3 — tolerate-and-drop deserialization shim.
///
/// Read-only wire form of [`LayerId`]. A live [`LayerId`] deserializes by
/// variant name, so a legacy on-disk entry written by a pre-SW-CP-7 binary
/// carries `"source_layer": "Core"` — an unknown variant once Core is
/// deleted, which would hard-fail the whole-file deserialize and brick
/// sync. `#[serde(other)]` folds any unknown name into [`WireLayerId::Unknown`]
/// instead; [`Lockfile::read`] drops those entries with a `warn!` and keeps
/// the rest. No migration/back-fill code — migrations are manual (unreleased).
#[derive(Deserialize)]
pub(crate) enum WireLayerId {
    /// SW-XS-8: the retired per-repo install tier.
    TeamRepo,
    /// SW-XS-8: the retired repo-agnostic install tier.
    TeamWorkspace,
    TeamProjection,
    /// Any name not matching a live [`LayerId`] — e.g. the removed `"Core"`,
    /// or the SLW-IN-4-retired `"PersonalRepo"`/`"PersonalWorkspace"`/
    /// `"OrgRepo"`/`"OrgWorkspace"`. Folds to a drop-on-load.
    #[serde(other)]
    Unknown,
}

/// The stored names of the install tiers SW-XS-8 deleted from [`LayerId`].
pub(crate) const RETIRED_INSTALL_LAYERS: [&str; 2] = ["TeamRepo", "TeamWorkspace"];

impl WireLayerId {
    /// SW-XS-8 load-time mapping. Map to a live [`LayerId`], or `None` for an
    /// entry that should be dropped on load:
    /// - a retired install tier (`TeamRepo` / `TeamWorkspace`) maps to
    ///   [`LayerId::TeamProjection`] when the entry is a projection entry
    ///   (`projection_key` set — those were tagged with the install layer),
    ///   and is drained otherwise: a wave-managed install entry has had no
    ///   producer since the install resolver was retired (SW-CP-31b);
    /// - an unknown name is dropped (SW-CP-7).
    ///
    /// `pub(crate)` so the install-state store reuses this one policy.
    pub(crate) fn into_layer_id(self, projection_key: Option<&str>) -> Option<LayerId> {
        match self {
            WireLayerId::TeamRepo | WireLayerId::TeamWorkspace => {
                projection_key.map(|_| LayerId::TeamProjection)
            }
            WireLayerId::TeamProjection => Some(LayerId::TeamProjection),
            WireLayerId::Unknown => None,
        }
    }
}

/// Wire mirror of [`MaterializedEntry`] with a tolerant `source_layer`.
/// Used only by [`Lockfile::read`] (see [`WireLayerId`]).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntry {
    logical_name: String,
    dest_path: PathBuf,
    source_layer: WireLayerId,
    source_path: PathBuf,
    last_materialized_hash: [u8; 32],
    last_materialized_mtime_ns: u128,
    #[serde(default)]
    projection_key: Option<String>,
    /// SLW-RS-1: mirrors [`MaterializedEntry::source_oid`]. `#[serde(default)]`
    /// so a v3 lockfile (no field) loads with `None`; declared here (not just
    /// dropped by `deny_unknown_fields`) so a v4 lockfile that *carries* the
    /// field round-trips instead of hard-failing the read.
    #[serde(default)]
    source_oid: Option<String>,
    /// SLW-RS-2: mirrors [`MaterializedEntry::last_materialized_size`].
    /// `#[serde(default)]` so a pre-RS-2 lockfile (no field) loads with `None`
    /// (= "size unknown, must hash"); declared here (`deny_unknown_fields`) so a
    /// lockfile that carries it round-trips instead of failing the read.
    #[serde(default)]
    last_materialized_size: Option<u64>,
}

/// Wire mirror of [`Lockfile`] with tolerant entries. Used only by
/// [`Lockfile::read`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLockfile {
    schema_version: u32,
    org_slug: String,
    team_name: Option<String>,
    code_repo_path: PathBuf,
    entries: Vec<WireEntry>,
}

/// SS-4c: per-(slug, code_repo_path) destination identifier used by the
/// wave engine as the unit of fan-out. Lives in `lockfile` rather than
/// `sync::wave` because it's derived from lockfile state and consumed
/// by both modules. Re-exported from `sync::wave` for ergonomic import
/// at wave call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedCodeRepo {
    pub slug: SourceRepoSlug,
    pub code_repo_path: PathBuf,
    /// SW-MC-2 (AC-6): `true` when this `code_repo_path` is a codesense-managed
    /// clone (its `Attachment` record has `managed == true`). Resolved ONCE per
    /// wave at [`enumerate_attached_code_repos_for_slug`] — never re-read per
    /// file. Forward-materialize boundaries OR this into the managed-surface
    /// predicate (`managed || is_codesense_managed_target(..)`) so a managed
    /// clone at a configured `workspace_root_dir` (outside `~/.codesense`) still
    /// materializes. Non-enumeration constructions default to `false` via
    /// [`AttachedCodeRepo::attached`].
    pub managed: bool,
}

impl AttachedCodeRepo {
    /// Construct a non-managed (`managed = false`) attached code repo — the
    /// common case for callers that don't consult the attachment marker (tests,
    /// worktree/route targets already covered by the registry branch of the
    /// predicate). The wave enumeration path sets `managed` explicitly instead.
    /// Centralizing construction here keeps a future field addition off the ~50
    /// call sites.
    pub fn attached(slug: SourceRepoSlug, code_repo_path: PathBuf) -> Self {
        Self {
            slug,
            code_repo_path,
            managed: false,
        }
    }
}

impl Lockfile {
    /// Empty starter lockfile for a fresh `(org, team, code-repo)` triple.
    pub fn empty(org_slug: String, team_name: Option<String>, code_repo_path: PathBuf) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            org_slug,
            team_name,
            code_repo_path,
            entries: Vec::new(),
        }
    }

    /// Read and parse a lockfile from disk, migrating any supported version
    /// forward in-memory (SLW-RS-1).
    ///
    /// Returns [`Error::UnsupportedSchemaVersion`] via
    /// [`check_supported_schema_version`] if `schema_version` falls outside
    /// `MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION` — a genuinely old stray
    /// doc or a too-new file from a future binary. In-range versions load and
    /// normalize to the current shape (a v3 lockfile gains `source_oid == None`).
    /// Legacy/unknown-layer entries (e.g. the removed `"Core"` layer —
    /// SW-CP-7 AC-3) are dropped on read and surfaced as a SINGLE
    /// aggregated `warn!` per file, not one line per entry — a stale
    /// lockfile can carry dozens, and per-entry logging floods the log on
    /// every load (the read path never persists the cleaned file). The
    /// on-disk file is healed once by [`prune_install_lockfiles_at_boot`]
    /// at startup, after which this branch goes quiet.
    pub fn read(path: &Path) -> Result<Self, Error> {
        let (lockfile, dropped) = Self::read_counting_dropped(path)?;
        if dropped > 0 {
            log::warn!(
                target: "content_engine::lockfile",
                "{}: dropped {} legacy lockfile entr{} (an unknown source_layer, or a retired install tier's wave entry); will be pruned at next startup",
                path.display(),
                dropped,
                if dropped == 1 { "y" } else { "ies" },
            );
        }
        Ok(lockfile)
    }

    /// [`read`] without the aggregated `warn!`: returns the parsed lockfile
    /// plus the count of legacy/unknown-layer entries dropped. The drop
    /// policy lives here in ONE place; [`read`] logs the count and
    /// [`prune_install_lockfiles_at_boot`] rewrites the file to persist the
    /// cleanup.
    fn read_counting_dropped(path: &Path) -> Result<(Self, usize), Error> {
        // P4 (iter-3): refuse to load lockfiles past the size bound.
        // Cheaper than letting `fs::read` allocate gigabytes.
        let bytes = read_lockfile_bytes_bounded(path)?;
        Self::from_bytes_counting_dropped(&bytes)
    }

    /// Parse + drop-policy half of [`read_counting_dropped`], split out so a
    /// caller that already holds the file bytes (SW-FX-6's boot sweep) can
    /// parse without a second disk read. The I/O half is
    /// [`read_lockfile_bytes_bounded`].
    fn from_bytes_counting_dropped(bytes: &[u8]) -> Result<(Self, usize), Error> {
        // SW-CP-7 AC-3: deserialize through the tolerant wire form so a
        // legacy `source_layer:"Core"` entry doesn't hard-fail the whole
        // file. Unknown-layer entries are dropped (with a `warn!`) below.
        let wire: WireLockfile = serde_json::from_slice(bytes).map_err(|e| {
            // Collapse serde failures into the existing Io variant via
            // an InvalidData wrapper. SS-4/SS-13 may later promote this
            // to a dedicated `LockfileSerde(serde_json::Error)` variant
            // if richer surfacing is needed; SS-3 keeps the surface
            // minimal per D-7.
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid lockfile JSON: {}", e),
            ))
        })?;
        // SLW-RS-1 AC-3: migrate any version in the supported range forward
        // in-memory rather than hard-rejecting on inequality. A v3 lockfile
        // (predates `source_oid`) loads through the tolerant `WireEntry` path
        // with `source_oid == None`; the in-memory `schema_version` is
        // normalized to `SCHEMA_VERSION` below, so the next `Lockfile::write`
        // persists the current shape. Anything below MIN_SUPPORTED or above
        // SCHEMA_VERSION still surfaces as `UnsupportedSchemaVersion` (a
        // too-new file written by a future binary, or a stray legacy doc).
        check_supported_schema_version(wire.schema_version)?;

        let mut entries = Vec::with_capacity(wire.entries.len());
        let mut dropped = 0usize;
        for we in wire.entries {
            match we.source_layer.into_layer_id(we.projection_key.as_deref()) {
                Some(source_layer) => entries.push(MaterializedEntry {
                    logical_name: we.logical_name,
                    dest_path: we.dest_path,
                    source_layer,
                    source_path: we.source_path,
                    last_materialized_hash: we.last_materialized_hash,
                    last_materialized_mtime_ns: we.last_materialized_mtime_ns,
                    projection_key: we.projection_key,
                    // SLW-RS-1: `None` for a v3 lockfile (field absent, serde
                    // default); carried through verbatim for a v4.
                    source_oid: we.source_oid,
                    // SLW-RS-2: `None` for a pre-RS-2 lockfile (serde default);
                    // carried through verbatim otherwise.
                    last_materialized_size: we.last_materialized_size,
                }),
                // SW-CP-7 AC-3: legacy/unknown layer (e.g. the removed
                // `"Core"`) — drop the entry, keep the rest of the file.
                // The dest file becomes an ordinary untracked repo file
                // (never auto-deleted, never auto-restored).
                // Counted (not logged per-entry) so [`read`] emits ONE
                // aggregated warning instead of flooding the log per load.
                None => dropped += 1,
            }
        }

        let lockfile = Lockfile {
            // SLW-RS-1: normalize to the current version — a migrated v3
            // lockfile is upgraded in-memory so the next `Lockfile::write`
            // persists v4 (with the `source_oid` fields once repopulated).
            schema_version: SCHEMA_VERSION,
            org_slug: wire.org_slug,
            team_name: wire.team_name,
            code_repo_path: wire.code_repo_path,
            entries,
        };
        // P9 (iter-3): reject NUL bytes in deserialized PathBuf fields
        // — JSON `"\0"` parses into a valid Rust string but
        // downstream syscalls EINVAL on the path.
        invariant_check_entries(&lockfile.entries)?;
        invariant_check_path("code_repo_path", &lockfile.code_repo_path)?;
        Ok((lockfile, dropped))
    }

    /// O(n) lookup returning the **first** entry with `logical_name`; no allocation.
    ///
    /// ⚠️ **Ambiguous when two entries share a `logical_name` under distinct
    /// `projection_key`s** — a legitimate shape since SW-IS-3ab keyed the store's
    /// entries on `(logical_name, projection_key)` (the SW-FX-14 fold). This returns
    /// whichever appears first in `entries`, so it must NOT be used in production for a
    /// target that can hold such a pair. The entry identity is `(logical_name,
    /// projection_key)`: production entry lookups go through the store (whose entry PK
    /// carries `projection_key`) and match on BOTH. There are no production callers of
    /// this method today — it is a test convenience for targets with a single entry per
    /// `logical_name`.
    pub fn find_entry(&self, logical_name: &str) -> Option<&MaterializedEntry> {
        self.entries.iter().find(|e| e.logical_name == logical_name)
    }

    /// Borrowed index of entries by logical name; amortizes the O(n) [`find_entry`] scan
    /// over many lookups against one snapshot.
    ///
    /// ⚠️ Keyed on `logical_name` alone, so a wave+projection pair sharing a name
    /// COLLAPSES (last wins) — see [`find_entry`] for the identity caveat. Test-only;
    /// production entry lookups go through the store, whose entry PK carries
    /// `projection_key`.
    pub fn entries_by_name(&self) -> HashMap<&str, &MaterializedEntry> {
        self.entries
            .iter()
            .map(|e| (e.logical_name.as_str(), e))
            .collect()
    }

    /// SS-4b AC-7: insert-or-replace an entry by `logical_name`. Preserves
    /// existing insertion order when replacing in place; appends to the
    /// end on first insert. O(n) scan over `entries`.
    ///
    /// Test-only after the wave switched to a fold-at-end model that
    /// applies mutations into its own `HashMap<String, MaterializedEntry>`
    /// index rather than via repeated `upsert_entry`-into-`Vec` calls
    /// (the latter is O(n) per call, O(n²) wave-wide). Kept here purely
    /// for the test fixture's in-place mutation shim.
    #[cfg(test)]
    pub(crate) fn upsert_entry(&mut self, entry: MaterializedEntry) {
        match self
            .entries
            .iter()
            .position(|e| e.logical_name == entry.logical_name)
        {
            Some(idx) => self.entries[idx] = entry,
            None => self.entries.push(entry),
        }
    }
}

/// SS-FU-7 AC-2: stable, deterministic hash of an absolute
/// `code_repo_path`. Used to derive per-path lockfile filenames under
/// the SS-FU-7 layout `{owner}/{repo}/{path-hash}.lock.json`.
///
/// Returns 16 hex characters (the first 8 bytes of the blake3 hash).
/// Truncating to 8 bytes keeps lockfile filenames short while staying
/// collision-resistant for the domain: the hash space is per-`(team,
/// owner, repo)`, so a collision requires two different `code_repo_path`s
/// under the same slug under the same team — astronomically unlikely
/// for realistic install counts.
///
/// **Contract:** caller MUST pass an absolute path. The helper does
/// NOT call `canonicalize` — a symlinked `code_repo_path` and its
/// target produce different hashes. This matches the wave engine's
/// per-path lock semantics (`WaveCoordinator::acquire_repo_lock` keys
/// on the supplied path; see `content_engine/sync/wave.rs:306`), and
/// avoids macOS `/var/...` vs `/private/var/...` divergence between
/// sibling tests and production.
///
/// Uses [`std::ffi::OsStr::as_encoded_bytes`] for byte access so the
/// helper compiles on every platform Tauri targets, not just unix.
///
/// Rejects non-absolute input with `Error::Io(InvalidInput, ...)`,
/// matching the path-rejection posture used by `invariant_check_path`
/// at lockfile read/write time.
pub fn code_repo_path_hash(code_repo_path: &Path) -> Result<String, Error> {
    if !code_repo_path.is_absolute() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "code_repo_path_hash requires an absolute path; got {:?}",
                code_repo_path
            ),
        )));
    }
    let bytes = code_repo_path.as_os_str().as_encoded_bytes();
    let hex = blake3::hash(bytes).to_hex();
    Ok(hex.as_str()[..16].to_string())
}

/// The on-disk lockfile filename for a `code_repo_path` hash: `{hash}.lock.json`.
/// The single place this naming convention is spelled — shared by [`lockfile_path`]
/// (the full path) and the store-backed readers (filename-only, to match a store
/// target against its JSON sidecar without opening it).
pub(crate) fn lockfile_filename(path_hash: &str) -> String {
    format!("{path_hash}.lock.json")
}

/// SS-FU-7 AC-1 (T2.1): the per-`(team, slug)` lockfile DIRECTORY.
/// Composition root for both [`lockfile_path`] (which appends
/// `{path-hash}.lock.json`) and
/// [`enumerate_attached_code_repos_for_slug`] (which walks every
/// `*.lock.json` child file under this dir). Validates
/// `ctx.org_slug` / `ctx.team_name` / `code_repo_slug` identically to
/// [`lockfile_path`] so the two helpers stay in lock-step on path
/// safety.
///
/// Layout:
/// ```text
/// {codesense_home}/users/{user_id}/installs/{org_slug}/{team_or__none}/{owner}/{repo}/
/// ```
///
/// This root is a sibling of `orgs/` — machine-local install state lives
/// OUTSIDE the context-repo clone so it can't be committed to a context
/// repo.
///
/// Pure path math — no filesystem touch, no existence check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockfilePathAuthority {
    Legacy,
    HostAware,
}

/// Resolve compatibility authority without guessing from the remote's path
/// shape. A retained legacy directory stays authoritative until the tracked
/// group epoch proves cutover; this keeps identity-ambiguous lock state visible
/// to all existing consumers and writers.
pub(crate) fn lockfile_path_authority(
    ctx: &ResolverContext,
    code_repo_slug: &SourceRepoSlug,
) -> Result<LockfilePathAuthority, Error> {
    validate_org_slug(&ctx.org_slug)?;
    validate_source_repo_slug(code_repo_slug)?;
    let team_segment = match &ctx.team_name {
        Some(name) => {
            validate_team_name(name)?;
            name.as_str()
        }
        None => NO_TEAM_SEGMENT,
    };
    let root = path_to_install_root(&ctx.codesense_home, &ctx.principal())
        .join(&ctx.org_slug)
        .join(team_segment);
    let legacy = crate::context_repo::layout_resolver::LegacyRepositoryPathLocator::under(
        &root,
        code_repo_slug,
    );
    let canonical = code_repo_slug
        .physical_path_key()
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?
        .append_to(&root);
    let selected = crate::context_repo::layout_resolver::resolve_owned_layout(
        &canonical,
        &legacy,
        &path_to_org_root(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?,
        ctx.team_name.as_deref(),
        code_repo_slug,
    )
    .map_err(|e| Error::Io(std::io::Error::other(e)))?;
    Ok(if selected == canonical {
        LockfilePathAuthority::HostAware
    } else {
        LockfilePathAuthority::Legacy
    })
}

pub(crate) fn lockfile_dir_path(
    ctx: &ResolverContext,
    code_repo_slug: &SourceRepoSlug,
) -> Result<PathBuf, Error> {
    // P5 (iter-1): defensive validation — `ResolverContext` is a
    // public-fields struct, so org_slug arrives from anywhere a caller
    // chooses. The upstream `normalize_source_repo_url` already
    // enforces a similar rule set, but the team_name guard would be
    // meaningless without the symmetric org_slug guard (an unsafe
    // org_slug bypasses the entire path safety story).
    validate_org_slug(&ctx.org_slug)?;
    // P1 (iter-3): `SourceRepoSlug { owner, repo }` exposes `pub` fields
    // — a caller can build `SourceRepoSlug { owner: "..".into(), repo: ".." }`
    // and bypass `SourceRepoSlug::new`'s constructor. Re-validate here
    // so the pure `PathBuf::join` chain below can't escape org_root.
    validate_source_repo_slug(code_repo_slug)?;

    let team_segment: &str = match &ctx.team_name {
        Some(name) => {
            validate_team_name(name)?;
            name
        }
        None => NO_TEAM_SEGMENT,
    };

    let install_root = path_to_install_root(&ctx.codesense_home, &ctx.principal());
    let repo_root = install_root.join(&ctx.org_slug).join(team_segment);
    match lockfile_path_authority(ctx, code_repo_slug)? {
        LockfilePathAuthority::Legacy => Ok(
            crate::context_repo::layout_resolver::LegacyRepositoryPathLocator::under(
                &repo_root,
                code_repo_slug,
            ),
        ),
        LockfilePathAuthority::HostAware => Ok(code_repo_slug
            .physical_path_key()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
            .append_to(&repo_root)),
    }
}

/// Compute the canonical on-disk path of the v3 lockfile for a given
/// `(ctx, code-repo, code_repo_path)` triple.
///
/// Layout (SS-FU-7):
/// ```text
/// {codesense_home}/users/{user_id}/installs/{org_slug}/{team_or__none}/{owner}/{repo}/{path-hash}.lock.json
/// ```
///
/// Composes [`lockfile_dir_path`] (per-`(team, slug)` dir) with
/// [`code_repo_path_hash`] (per-path-bucket filename). Multiple
/// code repos (main + N worktrees) sharing the same source slug each
/// get their own lockfile under the shared dir.
///
/// The `{owner}/{repo}` segment uses
/// [`SourceRepoSlug`]'s pre-validated fields directly — the constructor
/// already rejects `/`, `\`, `.`, `..`, and control chars, so no
/// additional slug validation lives here.
///
/// `team_name` (when `Some`) is validated for slug safety via
/// [`validate_team_name`]; rejection surfaces as
/// [`Error::InvalidTeamName`] before the path is constructed. `None`
/// resolves to the literal segment `_none` (see [`NO_TEAM_SEGMENT`]).
///
/// `code_repo_path` MUST be absolute (validated by
/// [`code_repo_path_hash`]); see that helper's doc for why no
/// canonicalization happens.
///
/// Pure path math — no filesystem touch, no existence check.
pub fn lockfile_path(
    ctx: &ResolverContext,
    code_repo_slug: &SourceRepoSlug,
    code_repo_path: &Path,
) -> Result<PathBuf, Error> {
    let dir = lockfile_dir_path(ctx, code_repo_slug)?;
    let path_hash = code_repo_path_hash(code_repo_path)?;
    Ok(dir.join(lockfile_filename(&path_hash)))
}

/// SR-3: a recency proxy for "when the sync engine last materialized this attached
/// repo". The capacity gate reaps over the ATTACHED set (which, unlike the worktree
/// index, carries no per-record status/timestamp), so it uses this both to order
/// reap victims oldest-first and to honour an idle grace floor.
///
/// Returns the store's per-target `updated_at` (an indexed by-path read, advanced on
/// every materialize wave). SW-IS-3b made the store the sole writer, so its
/// `updated_at` captures every write and the pre-3c JSON-mtime term is retired.
/// `None` when there is no store row (or the store can't be opened) — fail-safe
/// not-reapable.
pub fn lockfile_mtime(
    ctx: &ResolverContext,
    code_repo_path: &Path,
) -> Option<std::time::SystemTime> {
    open_sync_state_store(&ctx.codesense_home)
        .and_then(|store| store.updated_at(code_repo_path).ok().flatten())
}

/// SW-EW-5: idempotently register `(slug, code_repo_path)` as an attached
/// content-engine target by writing an **empty** path-hash-keyed lockfile when
/// one does not already exist. Creates the per-`(team, slug)` parent dir as
/// needed.
///
/// Returns `true` when a new lockfile was created, `false` when one was already
/// present — an existing lockfile is left untouched so its materialization
/// baseline is preserved (never re-emptied), matching the relocation path's
/// conflict-safety.
///
/// This is the registration primitive behind **synthetic** targets — the epic
/// workspace dir (SW-EW-5) and future workspace types. A git-attached repo gets
/// its store row through the normal sync/materialization path instead; this
/// helper only stamps the bookkeeping that makes a `(slug, path)` enumerate via
/// [`enumerate_attached_code_repos_for_slug`].
///
/// SW-IS-3c: registration writes an empty target row into the SQLite install store
/// (the authoritative substrate) rather than a JSON lockfile.
pub fn register_attached_code_repo(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    code_repo_path: &Path,
) -> Result<bool, Error> {
    let store = open_sync_state_store(&ctx.codesense_home).ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "install store unavailable; cannot register attached code repo",
        ))
    })?;
    // Register atomically: create the empty target if absent, else add THIS slug as an
    // alias (the SW-EW-6 dual-slug case — one clone attached under two owner/repo names
    // enumerates under both). `register_or_alias` does the find-or-create + alias-insert
    // in one IMMEDIATE transaction, so a concurrent materialize can't slip between an
    // existence check and an empty-lockfile write and get its entries wiped. Returns
    // `true` when a target or a new alias was recorded, `false` when the slug was already
    // registered.
    store
        .register_or_alias(
            ctx.user_id,
            slug,
            &Lockfile::empty(
                ctx.org_slug.clone(),
                ctx.team_name.clone(),
                code_repo_path.to_path_buf(),
            ),
        )
        .map_err(store_err_to_io)
}

/// Map an [`sync_state_store::SyncStateStoreError`] onto this module's [`Error`] at a
/// call boundary. The store surfaces I/O, SQLite, JSON, and data faults; this
/// module has a single io-style variant ([`Error::Io`]) for environment faults, so
/// collapse the store error's `Display` into it rather than adding a blanket `From`
/// (which would silently swallow every store error into `Io` across the module).
fn store_err_to_io(e: sync_state_store::SyncStateStoreError) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

/// One-time, idempotent relocation of v3 lockfiles from the legacy
/// in-clone location (`{org_root}/_codesense/installs/`) to the
/// machine-local install root (`{install_root}/{org_slug}/`). Run at
/// startup before any wave so consumers see the new layout.
///
/// Idempotent and markerless: it walks `users/*/orgs/*/_codesense/installs`
/// and moves each that still exists; once a clone's `_codesense/installs/`
/// is gone the walk skips it, so steady state is a handful of `read_dir`
/// checks. It MOVES the files (never rebuilds), preserving each lockfile's
/// materialization baseline so the next sync doesn't re-write
/// already-projected files as first-time writes (which would clobber the
/// user's local edits). It also drains the leak-prone lockfiles out of the
/// clone, completing the structural goal for pre-upgrade installs.
///
/// Conflict-safe: if a lockfile already exists at the destination (a fresh
/// install scaffolded at the new root post-upgrade), the legacy file is
/// left untouched rather than clobbering newer state.
///
/// Best-effort and self-isolating: per-user, per-org, and per-relocate
/// failures are logged and skipped so one bad entry can't strand the rest
/// of the walk. Only an unreadable top-level `users/` dir surfaces as an
/// error to the caller. A failed relocation must not abort startup, and
/// leaving legacy files in place is recoverable (gitignored; retried next
/// boot).
pub fn relocate_install_state_out_of_clone(codesense_home: &Path) -> std::io::Result<()> {
    let users_dir = codesense_home.join(USERS_DIR);
    let user_entries = match std::fs::read_dir(&users_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for user in user_entries {
        let user_dir = match user {
            Ok(u) => u.path(),
            Err(e) => {
                log::warn!(target: "content_engine::lockfile", "skipping unreadable users/ entry: {e}");
                continue;
            }
        };
        if !user_dir.is_dir() {
            continue;
        }
        let org_entries = match std::fs::read_dir(user_dir.join(ORGS_DIR)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                log::warn!(
                    target: "content_engine::lockfile",
                    "skipping {} (orgs/ unreadable): {}",
                    user_dir.display(),
                    e
                );
                continue;
            }
        };
        for org in org_entries {
            let org_dir = match org {
                Ok(o) => o.path(),
                Err(e) => {
                    log::warn!(
                        target: "content_engine::lockfile",
                        "skipping unreadable orgs/ entry under {}: {}",
                        user_dir.display(),
                        e
                    );
                    continue;
                }
            };
            if !org_dir.is_dir() {
                continue;
            }
            // SS-FU-12: org clones are nested at `orgs/{owner}/{repo}`. A
            // direct child that still holds `_codesense/installs` is a
            // legacy single-segment `owner--repo` clone — relocate it with
            // its own dir name as the slug. Otherwise descend one level and
            // relocate each `{owner}/{repo}` clone with the two-segment
            // slug, so the destination matches `lockfile_path`'s
            // `installs/{org_slug}/…` layout.
            let direct_installs = org_dir.join(CODESENSE_INTERNAL_DIR).join(INSTALLS_DIR);
            if direct_installs.is_dir() {
                if let Some(slug) = org_dir.file_name().and_then(|n| n.to_str()) {
                    relocate_one_org_install_state(&user_dir, &direct_installs, slug);
                }
                continue;
            }
            let Some(owner) = org_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let repo_entries = match std::fs::read_dir(&org_dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for repo in repo_entries {
                let repo_dir = match repo {
                    Ok(r) => r.path(),
                    Err(_) => continue,
                };
                let nested_installs = repo_dir.join(CODESENSE_INTERNAL_DIR).join(INSTALLS_DIR);
                if !nested_installs.is_dir() {
                    continue;
                }
                if let Some(repo_name) = repo_dir.file_name().and_then(|n| n.to_str()) {
                    let slug = format!("{}/{}", owner, repo_name);
                    relocate_one_org_install_state(&user_dir, &nested_installs, &slug);
                }
            }
        }
    }
    Ok(())
}

/// Relocate one org's legacy in-clone `_codesense/installs/` tree to the
/// machine-local `{install_root}/{org_slug}/` root. Per-org failures are
/// logged and skipped (the legacy files stay put — gitignored — and are
/// retried next boot) so one bad org can't strand the rest of the walk.
fn relocate_one_org_install_state(user_dir: &Path, old_installs: &Path, org_slug: &str) {
    // `user_dir` IS `path_to_user_home(home, uid)`, so
    // `user_dir.join(INSTALLS_DIR)` IS `path_to_install_root(home, uid)`.
    // A two-segment `org_slug` (SS-FU-12) nests for free through `join`.
    let new_root = user_dir.join(INSTALLS_DIR).join(org_slug);
    if let Err(e) = relocate_dir_no_clobber(old_installs, &new_root) {
        log::warn!(
            target: "content_engine::lockfile",
            "install-state relocate {} -> {} failed: {} — legacy files left in place, retried next boot",
            old_installs.display(),
            new_root.display(),
            e
        );
    }
}

/// Move `src` onto `dst`. Fast path when `dst` is absent: a single atomic
/// directory rename. Otherwise merge file-by-file, never overwriting an
/// existing destination file, then prune the emptied source dirs.
///
/// Conflict-safe and best-effort: it never clobbers an existing destination
/// (file or dir), and type mismatches (a dir whose destination name is a
/// regular file, or vice versa) leave the legacy entry in place rather than
/// erroring.
fn relocate_dir_no_clobber(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.is_file() {
        // A regular file shadows the destination dir — can't merge into it.
        // Leave the legacy tree untouched (conflict-safe).
        return Ok(());
    }
    if !dst.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(src, dst).is_ok() {
            return Ok(());
        }
        // rename can fail across mount points or on a race — fall through
        // to the per-file merge.
    }
    merge_tree_no_clobber(src, dst)?;
    let _ = std::fs::remove_dir(src); // best-effort prune if now empty
    Ok(())
}

fn merge_tree_no_clobber(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if to.is_file() {
                // Type mismatch (source dir vs existing dest file): leave the
                // legacy subtree in place rather than aborting the walk.
                continue;
            }
            merge_tree_no_clobber(&from, &to)?;
            let _ = std::fs::remove_dir(&from); // prune if emptied
        } else if !to.exists() {
            std::fs::rename(&from, &to)?;
        }
        // dest already present → leave the legacy file untouched (no clobber).
    }
    Ok(())
}

/// Read a lockfile leaf's bytes, refusing files past [`MAX_LOCKFILE_BYTES`]
/// before allocating (P4 iter-3). Shared by
/// [`Lockfile::read_counting_dropped`] and the boot-time orphan sweep so the
/// size guard lives in ONE place.
fn read_lockfile_bytes_bounded(path: &Path) -> Result<Vec<u8>, Error> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_LOCKFILE_BYTES {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "lockfile at {} is {} bytes; exceeds MAX_LOCKFILE_BYTES ({})",
                path.display(),
                meta.len(),
                MAX_LOCKFILE_BYTES
            ),
        )));
    }
    Ok(std::fs::read(path)?)
}

/// `true` only when `path` is **confirmed** absent (`try_exists() == Ok(false)`).
/// An `Err` (flaky / network mount, permission error) returns `false`: a caller
/// must never treat an ambiguous probe as "gone", because that would supersede a
/// live repo's lockfile (the re-attach reconcile). Used by
/// [`reconcile_stale_slug_lockfile`], which moves the baseline forward rather than
/// dropping it, so confirmed-absence is enough (no parent-dir guard).
fn path_is_confirmed_absent(path: &Path) -> bool {
    matches!(path.try_exists(), Ok(false))
}

/// Per-run tally for the boot-time JSON lockfile deletion sweep.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct JsonDeleteTally {
    /// `*.lock.json` files removed.
    removed: usize,
    /// Files whose `remove_file` failed — warn-logged, skipped, retried next boot.
    skipped_err: usize,
}

/// SW-IS-3c: boot-time bulk deletion of the retired per-file JSON lockfiles.
///
/// The SQLite install store is now the authoritative substrate; the per-file
/// `*.lock.json` sidecars are dead weight. This sweep deletes them — but ONLY once
/// the store's JSON→store backfill has completed (its marker exists). Until then the
/// JSON files are the store's rebuild source (a corrupt/reset store repopulates from
/// them via `import_all_from_json`), so deleting early would strand a fresh store
/// empty. Marker present ⇒ every JSON has been imported ⇒ safe to delete.
///
/// Best-effort and never-abort: it walks `users/*/installs/` and removes every
/// `*.lock.json`; a single failed `remove_file` is warn-logged and skipped, never
/// aborting the walk. Directories the sweep leaves empty are NOT removed — they are
/// harmless residue left in place. Only an
/// unreadable top-level `users/` dir surfaces as an error to the caller. Idempotent:
/// once the tree is drained, a later boot finds nothing.
pub fn prune_install_lockfiles_at_boot(codesense_home: &Path) -> std::io::Result<()> {
    // Gate on the backfill marker: the JSON files are the store's rebuild source
    // until every one has been imported. Deleting before that would leave a
    // corrupt/reset store with nothing to repopulate from.
    let store_path = crate::content_engine::sync_state_store::default_store_path(codesense_home);
    if !crate::content_engine::sync_state_store::backfill_marker_path_for(&store_path).exists() {
        log::debug!(
            target: "content_engine::lockfile",
            "JSON deletion deferred: backfill not complete"
        );
        return Ok(());
    }

    let users_dir = codesense_home.join(USERS_DIR);
    let user_entries = match std::fs::read_dir(&users_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut tally = JsonDeleteTally::default();
    for user in user_entries {
        let user_dir = match user {
            Ok(u) => u.path(),
            Err(e) => {
                log::warn!(target: "content_engine::lockfile", "prune: skipping unreadable users/ entry: {e}");
                continue;
            }
        };
        if !user_dir.is_dir() {
            continue;
        }
        delete_lockfiles_in_tree(&user_dir.join(INSTALLS_DIR), &mut tally);
    }
    if tally.removed > 0 || tally.skipped_err > 0 {
        log::info!(
            target: "content_engine::lockfile",
            "retired JSON lockfile sweep: {} deleted, {} skipped (io error)",
            tally.removed,
            tally.skipped_err,
        );
    } else {
        log::debug!(
            target: "content_engine::lockfile",
            "retired JSON lockfile sweep: nothing to delete"
        );
    }
    Ok(())
}

/// Recursively walk `dir`, deleting every `*.lock.json` leaf. A missing or
/// unreadable dir (or a single bad entry) is skipped; the walk never aborts. Any
/// dirs the sweep leaves empty are harmless residue under `installs/` and are left
/// in place (the shared pre-order [`walk_dir`](crate::shared::dir_walker::walk_dir)
/// does no post-order dir cleanup).
fn delete_lockfiles_in_tree(dir: &Path, tally: &mut JsonDeleteTally) {
    use crate::shared::dir_walker::{walk_dir, WalkAction};
    use std::convert::Infallible;

    let _ = walk_dir(dir, &mut |path, meta| -> Result<WalkAction, Infallible> {
        // Tolerant: a per-entry metadata/iter error is skipped, matching the old walk.
        let Ok(meta) = meta else {
            return Ok(WalkAction::Skip);
        };
        if meta.is_dir() {
            return Ok(WalkAction::Recurse);
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|f| f.ends_with(".lock.json"))
        {
            match std::fs::remove_file(path) {
                Ok(()) => tally.removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => tally.removed += 1,
                Err(e) => {
                    tally.skipped_err += 1;
                    log::warn!(
                        target: "content_engine::lockfile",
                        "JSON lockfile sweep: failed to delete {} ({}) -- retried next boot",
                        path.display(),
                        e,
                    );
                }
            }
        }
        Ok(WalkAction::Skip)
    });
}

fn validate_team_name(name: &str) -> Result<(), Error> {
    // P6 (iter-1) + P4 (iter-2): a team literally named `_none` — or any
    // ASCII-case variant of it — would write into the same lockfile path
    // as `team_name = None` on case-insensitive filesystems
    // (APFS/HFS+/NTFS default). `eq_ignore_ascii_case` rejects all
    // variants (`_None`, `_NONE`, `_NoNe`, etc.) closing the regression
    // surfaced by iter-2 review B-M4 / E-6.
    if name.eq_ignore_ascii_case(NO_TEAM_SEGMENT) {
        return Err(Error::InvalidTeamName {
            reason: format!(
                "team_name {:?}: reserved (collides with NO_TEAM_SEGMENT, case-insensitive)",
                name
            ),
        });
    }
    if let Some(reason) = validate_path_segment(name) {
        return Err(Error::InvalidTeamName {
            reason: format!("team_name {:?}: {}", name, reason),
        });
    }
    Ok(())
}

fn validate_org_slug(slug: &str) -> Result<(), Error> {
    // SS-FU-12: org_slug is a two-segment `<owner>/<repo>` value, so it
    // must NOT be validated as a single path segment (that would reject
    // the `/`). Delegate to the slash-aware validator in `layers`, which
    // splits and validates each side with `validate_path_segment` — the
    // safe-segment rules stay in one place.
    if let Some(reason) = super::layers::validate_org_slug(slug) {
        return Err(Error::InvalidOrgSlug {
            reason: format!("org_slug {:?}: {}", slug, reason),
        });
    }
    Ok(())
}

/// P1 (iter-3): defensive validation of [`SourceRepoSlug`] fields. The
/// struct's `pub` fields admit hand-construction that bypasses
/// `SourceRepoSlug::new`'s safety guard; the resolver and lockfile path
/// math both depend on the slug being a safe path-segment.
fn validate_source_repo_slug(slug: &SourceRepoSlug) -> Result<(), Error> {
    if let Some(reason) = validate_path_segment(&slug.owner) {
        return Err(Error::InvalidSourceRepoSlug {
            field: "owner",
            reason: format!("{:?}: {}", slug.owner, reason),
        });
    }
    if let Some(reason) = validate_path_segment(&slug.repo) {
        return Err(Error::InvalidSourceRepoSlug {
            field: "repo",
            reason: format!("{:?}: {}", slug.repo, reason),
        });
    }
    if !slug.has_valid_host() {
        if let Some(host) = slug.host.as_deref() {
            return Err(Error::InvalidSourceRepoSlug {
                field: "host",
                reason: format!("{:?}: must be a bare DNS name", host),
            });
        } else {
            return Err(Error::InvalidSourceRepoSlug {
                field: "host",
                reason: "missing for unknown provider".to_string(),
            });
        }
    }
    Ok(())
}

/// Enforce the path invariants (NUL-free, absolute) on every entry's
/// `dest_path` and `source_path`. Every layer is filesystem-backed, so
/// both paths are always real on-disk paths.
fn invariant_check_entries(entries: &[MaterializedEntry]) -> Result<(), Error> {
    for entry in entries {
        invariant_check_path("dest_path", &entry.dest_path)?;
        invariant_check_path("source_path", &entry.source_path)?;
    }
    Ok(())
}

/// P9 (iter-3): reject `PathBuf` fields that contain NUL bytes. JSON
/// `\0` parses into a valid Rust `String` (and thus `PathBuf`),
/// but downstream syscalls return EINVAL or behave unpredictably on
/// NUL-bearing paths. Cheap defensive check at the deserialization
/// boundary.
fn invariant_check_path(field: &'static str, p: &Path) -> Result<(), Error> {
    if p.to_string_lossy().contains('\0') {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "lockfile field {} contains NUL byte; refusing to load: {:?}",
                field, p
            ),
        )));
    }
    // SS-13 review-iter-5 (F5): reject non-absolute paths at read time.
    // `Lockfile::write` always emits absolute paths (callers thread
    // resolved roots through `ResolverContext`), but hand-edited or
    // migrated-from-v2 lockfiles could carry relative paths. Downstream
    // strip_prefix walks (e.g. `sense_sync_scan_cwd`'s dirty loop)
    // silently dropped every entry when `code_repo_path` was relative;
    // refusing the load surfaces the corruption as a typed
    // `LockfileRead` error instead.
    if !p.is_absolute() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("lockfile field {} must be an absolute path: {:?}", field, p),
        )));
    }
    Ok(())
}

/// SS-4c D-14 / SS-FU-7 AC-3: enumerate every code repo for which a
/// lockfile exists under the given `(team, slug)` pair.
///
/// Reads `{codesense_home}/users/{user_id}/installs/{org_slug}/{team}/{owner}/{repo}/`
/// (the per-slug dir produced by [`lockfile_dir_path`]) and emits one
/// [`AttachedCodeRepo`] per `*.lock.json` child file. After SS-FU-7
/// each `(team, slug)` directory can contain multiple lockfiles — one
/// per attached code repo path — so this enumerator can return >1
/// entry per slug (main + N worktrees of the same source slug).
///
/// Namespace integrity comes from the dir name (`{owner}/{repo}/`),
/// not the filename — every `*.lock.json` child is in the right slug
/// bucket by construction.
///
/// Errors:
/// - Dir-not-found → `Ok(vec![])` (no lockfiles yet for this team/slug).
/// - Single lockfile read failure → skip with `warn!` and continue
///   (matches the per-repo-error tolerance of SS-4c AC-11).
/// - Filesystem walk error → propagate as `Err(Error::Io(...))`.
pub fn enumerate_attached_code_repos_for_slug(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
) -> Result<Vec<AttachedCodeRepo>, Error> {
    // SW-MC-2 (AC-6): resolve the managed-clone set ONCE for this enumeration
    // (off the predicate's per-file hot path) so each record can carry its
    // `managed` marker to the forward-materialize boundaries. `_for_team` hoists
    // this read across its slug loop via `_with_managed`.
    let managed_paths = read_managed_repo_paths(&ctx.codesense_home, ctx.user_id);
    enumerate_attached_code_repos_for_slug_with_managed(ctx, slug, &managed_paths)
}

/// Inner enumeration that consumes a pre-resolved managed-clone set instead of
/// re-reading `attachments.yml` — so a team fan-out (`_for_team`) reads the set
/// once and reuses it across every slug rather than once per slug.
fn enumerate_attached_code_repos_for_slug_with_managed(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    managed_paths: &HashSet<PathBuf>,
) -> Result<Vec<AttachedCodeRepo>, Error> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut paths: Vec<PathBuf> = Vec::new();

    // SW-IS-3c: the store is the authoritative substrate — its indexed per-slug
    // SELECT is the attached set. A store open `None` or an enumerate `Err` has no
    // JSON fallback anymore; log and return empty (fail-safe: the wave/reaper see no
    // attached targets rather than a partial/stale set).
    let Some(store) = open_sync_state_store(&ctx.codesense_home) else {
        log::warn!(
            target: "content_engine::lockfile",
            "install store unavailable; no attached code repos enumerated for slug"
        );
        return Ok(Vec::new());
    };
    match store.enumerate_for_slug(&ctx.org_slug, ctx.team_name.as_deref(), slug) {
        Ok(store_paths) => {
            for p in store_paths {
                if seen.insert(p.clone()) {
                    paths.push(p);
                }
            }
        }
        Err(e) => {
            log::warn!(
                target: "content_engine::lockfile",
                "install store enumerate_for_slug failed ({e}); no attached code repos enumerated"
            );
            return Ok(Vec::new());
        }
    }

    // SS-FU-7: build one `AttachedCodeRepo` per distinct `code_repo_path`, tagging
    // the managed marker from the pre-resolved set (unchanged, orthogonal to the
    // store).
    Ok(paths
        .into_iter()
        .map(|code_repo_path| {
            let managed = managed_paths.contains(&code_repo_path);
            AttachedCodeRepo {
                slug: slug.clone(),
                code_repo_path,
                managed,
            }
        })
        .collect())
}

/// SW-MC-2 (AC-6): the set of `code_repo_path`s whose `Attachment` record is
/// `managed == true`, read ONCE from the user's `attachments.yml`. Shared by the
/// wave enumeration ([`enumerate_attached_code_repos_for_slug`]) and the
/// team-switch feeder (`commands::install`) so the managed marker is resolved the
/// same way wherever an `AttachedCodeRepo` set feeds the materialize boundaries.
/// Best-effort: a missing/unreadable file yields an empty set, so a managed clone
/// under the hidden default still materializes via the `users/`-prefix branch of
/// `is_codesense_managed_target` — only a configured-root clone would lose
/// recognition, and that degrades to today's behavior rather than erroring.
pub(crate) fn read_managed_repo_paths(codesense_home: &Path, user_id: u64) -> HashSet<PathBuf> {
    // SW-FG-1 re-keyed path scope to `Principal`; bridge the caller's `user_id`
    // (still carried on `ResolverContext`) at this single site.
    let att_path =
        path_to_user_attachments_file(codesense_home, &crate::domain::Principal::from(user_id));
    match read_attachments(&att_path) {
        Ok(atts) => atts
            .attachments
            .into_iter()
            .filter(|a| a.managed)
            .map(|a| PathBuf::from(a.repo_path))
            .collect(),
        Err(_) => HashSet::new(),
    }
}

/// SS-4c D-14: enumerate every code repo across the team's source_repos.
/// Used by the team-wide content fan-out (the `ContentLayerEvent` projection
/// enumerates every attached repo for the active team; slug-agnostic — one
/// file change fans out to every attached code repo for the team).
///
/// Internally calls [`enumerate_attached_code_repos_for_slug`] over each
/// supplied slug, concatenates, and dedups by `code_repo_path`. A
/// worktree of repo A under team T appears in `_for_slug(T, A)` only —
/// no duplication possible across slugs unless a code repo is somehow
/// mounted for two source repos (which SS-3's validation rejects).
pub fn enumerate_attached_code_repos_for_team(
    ctx: &ResolverContext,
    slugs: &[SourceRepoSlug],
) -> Result<Vec<AttachedCodeRepo>, Error> {
    // SW-MC-2 (AC-6): read the managed-clone set ONCE for the whole fan-out (not
    // once per slug), then reuse it across every slug — the wave dispatch is a
    // hot path, so N slugs must not mean N reads of `attachments.yml`.
    let managed_paths = read_managed_repo_paths(&ctx.codesense_home, ctx.user_id);
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for slug in slugs {
        let per_slug =
            enumerate_attached_code_repos_for_slug_with_managed(ctx, slug, &managed_paths)?;
        for repo in per_slug {
            if seen.insert(repo.code_repo_path.clone()) {
                out.push(repo);
            }
        }
    }
    Ok(out)
}

/// SW-TS-3 §4.6 (AC-4b): the team(s) a worktree is **currently materialized
/// under on disk**, discovered without knowing the team in advance.
///
/// The lockfile path embeds the team (`installs/{org}/{team}/{owner}/{repo}/
/// {hash}.lock.json`), so to learn which team a worktree's files came from we
/// scan every team segment under `installs/{org}/` for a non-empty lockfile
/// matching this `code_repo_path`'s hash. Each match's authoritative team is
/// read from the lockfile's own `team_name` field (not inferred from the dir
/// name). Normally 0 or 1; an interrupted re-team can briefly leave two.
///
/// This is the *actual* half of the actual-vs-intent reconcile: the caller
/// compares it against the record's *intended* team and, on a mismatch, passes
/// the stale team as `from` so the existing `WorktreeDerive { from, .. }`
/// teardown removes its files before the intended team materializes. Only
/// non-empty lockfiles count — an empty one has nothing to tear down.
///
/// A missing `installs/{org}` dir or no matching lockfile yields an empty
/// `Vec` (the common fresh / single-team case). Unreadable individual
/// lockfiles are skipped (per-file tolerance), never aborting the scan; only
/// an unreadable org dir surfaces as `Err`.
pub fn materialized_teams_for_repo(
    codesense_home: &Path,
    org_slug: &str,
    slug: &SourceRepoSlug,
    code_repo_path: &Path,
) -> Result<Vec<Option<String>>, Error> {
    // SW-IS-3c: store-only. The store's natural key is `(org_slug, team_name,
    // code_repo_path)`, so a path holds one row per team that materialized it — the
    // exact question this reader answers (e.g. an interrupted re-team whose stale
    // team must still be torn down). A store open `None` yields an empty result
    // (fail-safe: nothing reported to tear down rather than a partial JSON view).
    let mut teams: Vec<Option<String>> = Vec::new();
    if let Some(store) = open_sync_state_store(codesense_home) {
        match store.teams_for_materialized_path(org_slug, slug, code_repo_path) {
            Ok(store_teams) => teams = store_teams,
            Err(e) => log::warn!(
                target: "content_engine::lockfile",
                "materialized_teams_for_repo: store query failed ({e})"
            ),
        }
    }
    Ok(teams)
}

/// Open the install-state store for a read, or `None` (warn-logged) if it cannot
/// be opened. The repointed readers use the store as the fast path and fall back
/// to the JSON walk on `None`, which stays the durable truth this slice — so a
/// missing/unopenable store (a not-yet-rebuilt corrupt DB is SW-IS-4's concern)
/// degrades to today's behaviour instead of failing the read. Opening is cheap: a
/// clone of a `PathBuf` plus an idempotent `CREATE IF NOT EXISTS`.
pub(crate) fn open_sync_state_store(codesense_home: &Path) -> Option<SyncStateStore> {
    let path = sync_state_store::default_store_path(codesense_home);
    match SyncStateStore::open(path) {
        Ok(store) => Some(store),
        Err(e) => {
            log::warn!(
                target: "content_engine::lockfile",
                "install store unavailable ({e}); falling back to the JSON lockfile walk"
            );
            None
        }
    }
}

/// Read every `*.lock.json` file under the per-`(ctx, slug)` lockfile dir,
/// returning `(file_path, Lockfile)` pairs. A missing dir yields an empty
/// `Vec`; a lockfile that won't parse is warn-logged and skipped (per-file
/// tolerance, matching SS-4c AC-11). The single dir-walk shared by
/// [`enumerate_attached_code_repos_for_slug`], [`reconcile_stale_slug_lockfile`],
/// and the install-store rebuild/backfill-from-JSON
/// (`sync_state_store::import_all_from_json`), which is why it is `pub(crate)`.
pub(crate) fn read_slug_lockfiles(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
) -> Result<Vec<(PathBuf, Lockfile)>, Error> {
    use log::warn;
    use std::io;

    let dir = lockfile_dir_path(ctx, slug)?;
    let read_dir = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut out = Vec::new();
    for entry in read_dir {
        let path = entry.map_err(Error::Io)?.path();
        let name = path.file_name().and_then(|f| f.to_str());
        // Only `*.lock.json` files — defensive against any non-lockfile
        // sidecar a future feature might drop into the per-slug dir.
        if !name.is_some_and(|f| f.ends_with(".lock.json")) {
            continue;
        }
        match Lockfile::read(&path) {
            Ok(lf) => out.push((path, lf)),
            Err(e) => warn!(
                target: "content_engine::lockfile",
                "read_slug_lockfiles: skipping unreadable {}: {}",
                path.display(),
                e
            ),
        }
    }
    Ok(out)
}

/// SS-FU-17 AC-8: slug-keyed re-attach reconcile. When a repo is re-attached
/// at `new_code_repo_path`, an older lockfile for the same `slug` may linger
/// at a now-dead path (the repo moved → its absolute `code_repo_path` no
/// longer resolves, and a new location mints a new path-hash → a new file).
///
/// Finds that stale lockfile and **moves its materialization baseline
/// forward** to the new path — rewriting `code_repo_path` and every entry's
/// `dest_path`, but preserving each entry's hash / mtime / layer so the next
/// wave classifies local edits correctly instead of rebuilding from empty
/// (the move-don't-rebuild discipline of SS-FU-9 AC-7). The stale file is
/// then deleted so it can't dispatch phantom waves.
///
/// Scope guards:
/// - Only lockfiles whose `code_repo_path` **no longer resolves** are
///   superseded — a live sibling worktree's lockfile is never disturbed.
/// - If the new path already has its own lockfile, that one wins (no
///   clobber); the stale file is still cleaned up.
///
/// Returns the path of the primary superseded stale lockfile (the one whose
/// baseline was moved forward), if any. Idempotent: a second call (no stale
/// left) returns `Ok(None)`.
pub fn reconcile_stale_slug_lockfile(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    new_code_repo_path: &Path,
) -> Result<Option<PathBuf>, Error> {
    use log::info;
    use std::io;

    // Stale = path is CONFIRMED gone AND differs from the new path (the new
    // path's own lockfile and a live sibling worktree are both left
    // untouched). `path_is_confirmed_absent` is Err-safe: a flaky/unmounted
    // mount (try_exists → Err) is NOT treated as stale, so a live repo on a
    // temporarily-unreachable volume is never wrongly superseded (the old
    // `!is_dir()` test folded Err → stale). There is usually exactly one, but
    // a repo moved twice (or abandoned clones) can leave several — collect ALL
    // so none is left behind to dispatch a perpetual phantom wave.
    let stale: Vec<(PathBuf, Lockfile)> = read_slug_lockfiles(ctx, slug)?
        .into_iter()
        .filter(|(_, lf)| {
            lf.code_repo_path != new_code_repo_path && path_is_confirmed_absent(&lf.code_repo_path)
        })
        .collect();

    // Carry the baseline forward from the **richest** stale lockfile (most
    // entries). `read_slug_lockfiles` is `read_dir`-ordered, not recency-
    // ordered, so when a repo moved more than once an abandoned/empty clone
    // could otherwise be picked first and overwrite the real materialization
    // baseline with empty — then every stale file (including the real one) is
    // deleted below, defeating move-don't-rebuild. Max-by-entry-count ensures
    // an empty clone never wins over a real baseline.
    let Some(primary_idx) = stale
        .iter()
        .enumerate()
        .max_by_key(|(_, (_, lf))| lf.entries.len())
        .map(|(i, _)| i)
    else {
        return Ok(None);
    };

    // Move the baseline forward from the primary stale lockfile (unless the
    // new path already has its own store row, which wins — no clobber).
    // SW-IS-3c: the store is the authoritative substrate — this transitional
    // reconcile still SOURCES stale baselines from any surviving JSON lockfiles
    // (`read_slug_lockfiles`, kept for the boot importer) but PERSISTS the moved
    // baseline into the store rather than a JSON file. Once JSON is fully retired
    // `stale` is empty and this whole function no-ops.
    let store = open_sync_state_store(&ctx.codesense_home).ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "install store unavailable; cannot reconcile stale slug lockfile",
        ))
    })?;
    let new_key = sync_state_store::TargetKey::from_ctx(ctx, new_code_repo_path);
    if store.read(&new_key).map_err(store_err_to_io)?.is_some() {
        info!(
            target: "content_engine::lockfile",
            "reconcile: new path {} already has a store row; superseding {} stale file(s) without moving baseline",
            new_code_repo_path.display(),
            stale.len()
        );
    } else {
        let primary_lf = &stale[primary_idx].1;
        let moved = rebase_lockfile_to_path(primary_lf, new_code_repo_path);
        let moved_count = moved.entries.len();
        store
            .upsert(ctx.user_id, slug, &moved)
            .map_err(store_err_to_io)?;
        info!(
            target: "content_engine::lockfile",
            "reconcile: moved {} baseline entries from {} → {} (superseding {} stale file(s))",
            moved_count,
            primary_lf.code_repo_path.display(),
            new_code_repo_path.display(),
            stale.len()
        );
    }

    // Drop every stale JSON lockfile for this slug (primary + any abandoned dupes)
    // so none can dispatch a phantom wave. Tolerate already-gone.
    for (path, _) in &stale {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    let mut stale = stale;
    Ok(Some(stale.swap_remove(primary_idx).0))
}

/// Rebuild `stale`'s baseline at `new_path`: rewrite `code_repo_path` and
/// each entry's `dest_path` (`new_path.join(logical_name)`, mirroring the
/// projection materialize's dest computation), preserving every
/// entry's hash / mtime / layer / projection_key so local-edit
/// classification survives the re-point.
///
/// Entries whose `logical_name` fails [`is_safe_logical_name`] (`..` /
/// absolute / NUL segments — only reachable via a corrupt or hand-edited
/// lockfile) are dropped, not carried forward: `new_path.join("/abs")`
/// escapes the repo, so a poisoned name must never be minted into a fresh
/// `dest_path`.
fn rebase_lockfile_to_path(stale: &Lockfile, new_path: &Path) -> Lockfile {
    let entries = stale
        .entries
        .iter()
        .filter(|e| {
            let safe = is_safe_logical_name(&e.logical_name);
            if !safe {
                log::warn!(
                    target: "content_engine::lockfile",
                    "rebase: dropping entry with unsafe logical_name {:?}",
                    e.logical_name
                );
            }
            safe
        })
        .map(|e| {
            let mut moved = e.clone();
            moved.dest_path = new_path.join(&e.logical_name);
            moved
        })
        .collect();
    Lockfile {
        schema_version: SCHEMA_VERSION,
        org_slug: stale.org_slug.clone(),
        team_name: stale.team_name.clone(),
        code_repo_path: new_path.to_path_buf(),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Principal;
    use tempfile::TempDir;

    fn ctx(home: &Path, team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: team.map(String::from),
        }
    }

    fn slug() -> SourceRepoSlug {
        SourceRepoSlug::new("alderco", "codesense").unwrap()
    }

    // ===== SW-MC-2 (AC-6): managed-marker set from attachments.yml =====

    fn managed_att(repo_path: &str, managed: bool) -> crate::domain::Attachment {
        crate::domain::Attachment {
            repo_path: repo_path.into(),
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed,
        }
    }

    #[test]
    fn read_managed_repo_paths_collects_only_managed() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let att_path = path_to_user_attachments_file(home, &crate::domain::Principal::from(42u64));
        std::fs::create_dir_all(att_path.parent().unwrap()).unwrap();
        let atts = crate::domain::Attachments {
            attachments: vec![
                managed_att("/Users/dev/mycode/acme/managed", true),
                managed_att("/Users/dev/attached-own", false),
            ],
        };
        crate::context_repo::attachments_io::write_attachments_atomic(&att_path, &atts).unwrap();

        let set = read_managed_repo_paths(home, 42);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&PathBuf::from("/Users/dev/mycode/acme/managed")));
        assert!(!set.contains(&PathBuf::from("/Users/dev/attached-own")));
    }

    #[test]
    fn read_managed_repo_paths_missing_file_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read_managed_repo_paths(tmp.path(), 42).is_empty());
    }

    #[test]
    fn enumerate_marks_managed_from_attachments() {
        // A lockfile whose code_repo_path matches a managed attachment record
        // enumerates with `managed = true`; a non-managed path stays `false`.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let ctx = ctx(home, Some("default"));

        let managed_path = PathBuf::from("/Users/dev/mycode/alderco/codesense");
        let att_path = path_to_user_attachments_file(home, &crate::domain::Principal::from(42u64));
        std::fs::create_dir_all(att_path.parent().unwrap()).unwrap();
        crate::context_repo::attachments_io::write_attachments_atomic(
            &att_path,
            &crate::domain::Attachments {
                attachments: vec![managed_att(&managed_path.to_string_lossy(), true)],
            },
        )
        .unwrap();

        // Register a lockfile for this (ctx, slug, managed_path).
        register_attached_code_repo(&ctx, &slug(), &managed_path).unwrap();
        let repos = enumerate_attached_code_repos_for_slug(&ctx, &slug()).unwrap();
        let found = repos
            .iter()
            .find(|r| r.code_repo_path == managed_path)
            .expect("enumerated the registered managed clone");
        assert!(
            found.managed,
            "a managed clone must enumerate with managed=true"
        );
    }

    #[test]
    fn enumerate_for_team_carries_managed_marker() {
        // The team fan-out reads the managed set ONCE and reuses it across slugs;
        // a managed clone must still enumerate with managed=true through this path.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let ctx = ctx(home, Some("default"));

        let managed_path = PathBuf::from("/Users/dev/mycode/alderco/codesense");
        let att_path = path_to_user_attachments_file(home, &crate::domain::Principal::from(42u64));
        std::fs::create_dir_all(att_path.parent().unwrap()).unwrap();
        crate::context_repo::attachments_io::write_attachments_atomic(
            &att_path,
            &crate::domain::Attachments {
                attachments: vec![managed_att(&managed_path.to_string_lossy(), true)],
            },
        )
        .unwrap();
        register_attached_code_repo(&ctx, &slug(), &managed_path).unwrap();

        let repos = enumerate_attached_code_repos_for_team(&ctx, &[slug()]).unwrap();
        let found = repos
            .iter()
            .find(|r| r.code_repo_path == managed_path)
            .expect("team fan-out enumerated the managed clone");
        assert!(found.managed, "team fan-out must carry managed=true");
    }

    // ===== SLW-RS-1: source_oid + schema v3→v4 migration =====

    fn rs1_entry(source_oid: Option<&str>) -> MaterializedEntry {
        MaterializedEntry {
            last_materialized_size: None,
            logical_name: "story/foo.md".to_string(),
            dest_path: PathBuf::from("/repo/_bmad-output/foo.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/ctx/default/content/foo.md"),
            last_materialized_hash: [7u8; 32],
            last_materialized_mtime_ns: 123,
            projection_key: Some("story".to_string()),
            source_oid: source_oid.map(String::from),
        }
    }

    fn rs1_lockfile(entry: MaterializedEntry) -> Lockfile {
        let mut lf = Lockfile::empty(
            "acme".to_string(),
            Some("default".to_string()),
            PathBuf::from("/repo"),
        );
        lf.entries.push(entry);
        lf
    }

    /// AC-3: a native v4 lockfile round-trips `source_oid` through the
    /// tolerant read path.
    #[test]
    fn v4_lockfile_roundtrips_source_oid() {
        let lf = rs1_lockfile(rs1_entry(Some("deadbeef")));
        let bytes = serde_json::to_vec(&lf).unwrap();
        let (parsed, dropped) = Lockfile::from_bytes_counting_dropped(&bytes).unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.entries[0].source_oid.as_deref(), Some("deadbeef"));
        assert_eq!(parsed, lf);
    }

    /// AC-3: a v3 lockfile (no `source_oid`, schema_version 3) migrates
    /// forward in-memory — loads with `source_oid == None`, version
    /// normalized to current. No crash on old lockfiles.
    #[test]
    fn v3_lockfile_migrates_with_source_oid_none() {
        let lf = rs1_lockfile(rs1_entry(None));
        let mut json = serde_json::to_value(&lf).unwrap();
        json["schema_version"] = serde_json::json!(3);
        // Mirror a pre-SLW-RS-1 writer: the field simply isn't present.
        for e in json["entries"].as_array_mut().unwrap() {
            e.as_object_mut().unwrap().remove("source_oid");
        }
        let bytes = serde_json::to_vec(&json).unwrap();
        let (parsed, dropped) = Lockfile::from_bytes_counting_dropped(&bytes).unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(
            parsed.schema_version, SCHEMA_VERSION,
            "normalized to current"
        );
        assert_eq!(
            parsed.entries[0].source_oid, None,
            "defaulted, recomputed later"
        );
    }

    /// SLW-RS-2 AC-8: a lockfile written before `last_materialized_size`
    /// existed (field absent) loads with `None` (additive-optional, serde
    /// default) — no crash, no schema bump. An entry that *carries* the field
    /// round-trips it verbatim.
    #[test]
    fn last_materialized_size_absent_loads_none_and_present_roundtrips() {
        // Present: round-trips.
        let mut entry = rs1_entry(Some("deadbeef"));
        entry.last_materialized_size = Some(4096);
        let lf = rs1_lockfile(entry);
        let bytes = serde_json::to_vec(&lf).unwrap();
        let (parsed, _) = Lockfile::from_bytes_counting_dropped(&bytes).unwrap();
        assert_eq!(parsed.entries[0].last_materialized_size, Some(4096));
        assert_eq!(parsed, lf);

        // Absent (pre-RS-2 writer): defaults to None.
        let lf2 = rs1_lockfile(rs1_entry(None));
        let mut json = serde_json::to_value(&lf2).unwrap();
        for e in json["entries"].as_array_mut().unwrap() {
            e.as_object_mut().unwrap().remove("last_materialized_size");
        }
        let bytes2 = serde_json::to_vec(&json).unwrap();
        let (parsed2, dropped) = Lockfile::from_bytes_counting_dropped(&bytes2).unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(parsed2.entries[0].last_materialized_size, None);
    }

    /// AC-3: below the supported floor still hard-rejects (a genuinely old
    /// stray doc), not a silent migrate.
    #[test]
    fn schema_below_min_supported_is_rejected() {
        let lf = rs1_lockfile(rs1_entry(None));
        let mut json = serde_json::to_value(&lf).unwrap();
        json["schema_version"] = serde_json::json!(MIN_SUPPORTED_SCHEMA_VERSION - 1);
        let bytes = serde_json::to_vec(&json).unwrap();
        match Lockfile::from_bytes_counting_dropped(&bytes) {
            Err(Error::UnsupportedSchemaVersion { found, expected }) => {
                assert_eq!(found, MIN_SUPPORTED_SCHEMA_VERSION - 1);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {:?}", other),
        }
    }

    /// AC-3: a too-new file (written by a future binary) also rejects.
    #[test]
    fn schema_above_current_is_rejected() {
        let lf = rs1_lockfile(rs1_entry(None));
        let mut json = serde_json::to_value(&lf).unwrap();
        json["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
        let bytes = serde_json::to_vec(&json).unwrap();
        assert!(matches!(
            Lockfile::from_bytes_counting_dropped(&bytes),
            Err(Error::UnsupportedSchemaVersion { .. })
        ));
    }

    /// SW-AN-20 (AC-12): this file is clean UTF-8 text with NO stray NUL (`0x00`)
    /// bytes, so `file` reports it as text and `grep`/`ripgrep` match it without
    /// `-a`. Regression pin for the NUL-in-comment bug SW-AN-20 stripped (a raw
    /// `0x00` had been embedded where the escape text `\0` was meant).
    #[test]
    fn swan20_lockfile_source_has_no_nul_bytes() {
        const SRC: &str = include_str!("lockfile.rs");
        assert!(
            !SRC.contains('\0'),
            "content_engine/lockfile.rs must contain no NUL bytes"
        );
    }

    // ===== SW-EW-5: synthetic-target registration =====

    #[test]
    fn register_attached_code_repo_is_idempotent_and_enumerable() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let s = crate::domain::derive_epic_workspace_slug("SW-EW").unwrap();
        let target = TempDir::new().unwrap();
        let path = target.path().to_path_buf();

        // First call creates the store row and reports it as newly registered.
        assert!(register_attached_code_repo(&c, &s, &path).unwrap());
        assert!(store_at(&c)
            .read(&sync_state_store::TargetKey::from_ctx(&c, &path))
            .unwrap()
            .is_some());

        // It now enumerates as an attached target for its slug.
        let attached = enumerate_attached_code_repos_for_slug(&c, &s).unwrap();
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].code_repo_path, path);

        // Second call is a no-op: returns false, no duplicate target.
        assert!(!register_attached_code_repo(&c, &s, &path).unwrap());
        assert_eq!(
            enumerate_attached_code_repos_for_slug(&c, &s)
                .unwrap()
                .len(),
            1,
            "re-registering must not create a duplicate target"
        );
    }

    /// SW-EW-6 dual-slug: one clone attached under TWO different slugs shares a single
    /// target but records a second `target_slug` alias — it enumerates under BOTH slugs.
    #[test]
    fn register_attached_code_repo_records_a_second_slug_alias() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let slug_a = crate::domain::derive_epic_workspace_slug("SW-EW").unwrap();
        let slug_b = slug(); // a different owner/repo slug
        assert_ne!(slug_a, slug_b, "the two slugs must differ");
        let target = TempDir::new().unwrap();
        let path = target.path().to_path_buf();

        // First slug creates the target; the second adds an alias on the SAME path.
        assert!(register_attached_code_repo(&c, &slug_a, &path).unwrap());
        assert!(
            register_attached_code_repo(&c, &slug_b, &path).unwrap(),
            "the second slug on the same path is a newly-recorded alias"
        );

        // Both aliases enumerate the same path.
        let under_a = enumerate_attached_code_repos_for_slug(&c, &slug_a).unwrap();
        assert_eq!(under_a.len(), 1);
        assert_eq!(under_a[0].code_repo_path, path);

        let under_b = enumerate_attached_code_repos_for_slug(&c, &slug_b).unwrap();
        assert_eq!(under_b.len(), 1);
        assert_eq!(under_b[0].code_repo_path, path);
    }

    // ===== SS-FU-17 AC-8: slug-keyed re-attach reconcile =====

    /// A stale lockfile at a dead path is superseded: its baseline is moved
    /// forward to the new path (paths rewritten, hashes/mtimes preserved)
    /// and the stale file is deleted.
    #[test]
    fn reconcile_moves_baseline_to_new_path_and_deletes_stale() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let s = slug();

        let old_path = PathBuf::from("/nonexistent/old-clone"); // dead — does not resolve
        let new_dir = TempDir::new().unwrap();
        let new_path = new_dir.path().to_path_buf();

        let stale_lf_path = lockfile_path(&c, &s, &old_path).unwrap();
        std::fs::create_dir_all(stale_lf_path.parent().unwrap()).unwrap();
        // The store now serves only projection entries (a wave/resolver entry with
        // `projection_key == None` on a `TeamRepo`/`TeamWorkspace` layer is drained
        // on read), so the moved baseline is a projection entry — the move-don't-
        // rebuild mechanism is entry-type-agnostic.
        let entry = MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/hello.md".to_string(),
            dest_path: old_path.join(".claude/skills/hello.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/src/hello.md"),
            last_materialized_hash: [7u8; 32],
            last_materialized_mtime_ns: 123,
            projection_key: Some(".claude/skills/hello.md".to_string()),
            source_oid: None,
        };
        let stale_lf = Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: c.org_slug.clone(),
            team_name: c.team_name.clone(),
            code_repo_path: old_path.clone(),
            entries: vec![entry],
        };
        std::fs::write(&stale_lf_path, serde_json::to_vec(&stale_lf).unwrap()).unwrap();

        let superseded = reconcile_stale_slug_lockfile(&c, &s, &new_path).unwrap();
        assert_eq!(superseded.as_deref(), Some(stale_lf_path.as_path()));
        assert!(!stale_lf_path.exists(), "stale lockfile must be deleted");

        // The moved baseline lands in the install store at the new path.
        let new_lf = store_at(&c)
            .read(&sync_state_store::TargetKey::from_ctx(&c, &new_path))
            .unwrap()
            .expect("moved baseline persisted at the new path");
        assert_eq!(new_lf.code_repo_path, new_path);
        assert_eq!(new_lf.entries.len(), 1);
        let moved = &new_lf.entries[0];
        assert_eq!(moved.dest_path, new_path.join(".claude/skills/hello.md"));
        // Move-don't-rebuild: baseline carried forward verbatim.
        assert_eq!(moved.last_materialized_hash, [7u8; 32]);
        assert_eq!(moved.last_materialized_mtime_ns, 123);
        assert_eq!(moved.source_layer, LayerId::TeamProjection);
    }

    /// No stale lockfile for the slug → reconcile is a no-op.
    #[test]
    fn reconcile_noop_when_no_stale_lockfile() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let new_dir = TempDir::new().unwrap();
        assert_eq!(
            reconcile_stale_slug_lockfile(&c, &slug(), new_dir.path()).unwrap(),
            None
        );
    }

    /// A live sibling worktree's lockfile (path still resolves) is never
    /// superseded — only dead paths are.
    #[test]
    fn reconcile_leaves_live_sibling_lockfile_alone() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let s = slug();

        let sibling = TempDir::new().unwrap();
        let sibling_lf = lockfile_path(&c, &s, sibling.path()).unwrap();
        std::fs::create_dir_all(sibling_lf.parent().unwrap()).unwrap();
        let sibling_lockfile = Lockfile::empty(
            c.org_slug.clone(),
            c.team_name.clone(),
            sibling.path().to_path_buf(),
        );
        std::fs::write(&sibling_lf, serde_json::to_vec(&sibling_lockfile).unwrap()).unwrap();

        let new_dir = TempDir::new().unwrap();
        assert_eq!(
            reconcile_stale_slug_lockfile(&c, &s, new_dir.path()).unwrap(),
            None,
            "a live sibling must not be superseded"
        );
        assert!(sibling_lf.exists());
    }

    /// The new path already has its OWN store row → the "new-path-wins" guard fires:
    /// reconcile supersedes (deletes) the stale file WITHOUT moving the stale baseline
    /// onto the occupied new path, so the occupant's baseline is not clobbered.
    #[test]
    fn reconcile_does_not_clobber_occupied_new_path_store_row() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let s = slug();

        // Seed a REAL store row at the new path (the occupant) with a distinctive entry.
        let new_dir = TempDir::new().unwrap();
        let new_path = new_dir.path().to_path_buf();
        // The store now serves only projection entries (a wave/resolver entry with
        // `projection_key == None` on a `TeamRepo`/`TeamWorkspace` layer is drained
        // on read), so the occupant is a projection entry.
        let occupant_entry = MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/occupant.md".to_string(),
            dest_path: new_path.join(".claude/skills/occupant.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/src/occupant.md"),
            last_materialized_hash: [9u8; 32],
            last_materialized_mtime_ns: 999,
            projection_key: Some(".claude/skills/occupant.md".to_string()),
            source_oid: None,
        };
        let occupant_lf = Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: c.org_slug.clone(),
            team_name: c.team_name.clone(),
            code_repo_path: new_path.clone(),
            entries: vec![occupant_entry],
        };
        store_at(&c).upsert(c.user_id, &s, &occupant_lf).unwrap();

        // A stale JSON lockfile at a dead path, with a DIFFERENT baseline that must NOT
        // overwrite the occupant.
        let old_path = PathBuf::from("/nonexistent/moved-clone");
        let stale_lf_path = lockfile_path(&c, &s, &old_path).unwrap();
        std::fs::create_dir_all(stale_lf_path.parent().unwrap()).unwrap();
        let stale_entry = MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/stale.md".to_string(),
            dest_path: old_path.join(".claude/skills/stale.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/src/stale.md"),
            last_materialized_hash: [1u8; 32],
            last_materialized_mtime_ns: 111,
            projection_key: None,
            source_oid: None,
        };
        let stale_lf = Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: c.org_slug.clone(),
            team_name: c.team_name.clone(),
            code_repo_path: old_path.clone(),
            entries: vec![stale_entry],
        };
        std::fs::write(&stale_lf_path, serde_json::to_vec(&stale_lf).unwrap()).unwrap();

        // The stale file is still cleaned up, but its baseline is NOT moved.
        let superseded = reconcile_stale_slug_lockfile(&c, &s, &new_path).unwrap();
        assert_eq!(superseded.as_deref(), Some(stale_lf_path.as_path()));
        assert!(!stale_lf_path.exists(), "stale file cleaned up");

        // The occupant's store row is intact — the stale baseline did NOT clobber it.
        let after = store_at(&c)
            .read(&sync_state_store::TargetKey::from_ctx(&c, &new_path))
            .unwrap()
            .expect("occupant row still present");
        assert_eq!(after.entries.len(), 1);
        assert_eq!(after.entries[0].logical_name, ".claude/skills/occupant.md");
        assert_eq!(after.entries[0].last_materialized_hash, [9u8; 32]);
    }

    /// Review M: ALL stale lockfiles for the slug are superseded, not just
    /// the first — a repo moved twice leaves no perpetual phantom — AND the
    /// baseline is carried from the RICHEST stale lockfile, so an abandoned
    /// empty clone never overwrites a real materialization baseline.
    #[test]
    fn reconcile_supersedes_all_stale_and_keeps_richest_baseline() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("default"));
        let s = slug();

        // dead_empty: an abandoned clone with no entries.
        let dead_empty = PathBuf::from("/nonexistent/clone-empty");
        let p_empty = lockfile_path(&c, &s, &dead_empty).unwrap();
        std::fs::create_dir_all(p_empty.parent().unwrap()).unwrap();
        let empty_lf = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), dead_empty.clone());
        std::fs::write(&p_empty, serde_json::to_vec(&empty_lf).unwrap()).unwrap();

        // dead_rich: the real previous baseline with one entry.
        let dead_rich = PathBuf::from("/nonexistent/clone-rich");
        let p_rich = lockfile_path(&c, &s, &dead_rich).unwrap();
        let rich_lf = Lockfile {
            schema_version: SCHEMA_VERSION,
            org_slug: c.org_slug.clone(),
            team_name: c.team_name.clone(),
            code_repo_path: dead_rich.clone(),
            // The store serves only projection entries (a `projection_key == None`
            // wave/resolver entry is drained on read), so the richest baseline that
            // is carried forward is a projection entry.
            entries: vec![MaterializedEntry {
                last_materialized_size: None,
                logical_name: ".claude/skills/hello.md".to_string(),
                dest_path: dead_rich.join(".claude/skills/hello.md"),
                source_layer: LayerId::TeamProjection,
                source_path: PathBuf::from("/src/hello.md"),
                last_materialized_hash: [9u8; 32],
                last_materialized_mtime_ns: 77,
                projection_key: Some(".claude/skills/hello.md".to_string()),
                source_oid: None,
            }],
        };
        std::fs::write(&p_rich, serde_json::to_vec(&rich_lf).unwrap()).unwrap();

        let new_dir = TempDir::new().unwrap();
        reconcile_stale_slug_lockfile(&c, &s, new_dir.path()).unwrap();

        // Both stale lockfiles deleted (no perpetual phantom).
        assert!(!p_empty.exists(), "empty stale lockfile must be deleted");
        assert!(!p_rich.exists(), "rich stale lockfile must be deleted");

        // The new path carries the RICH baseline, not the empty one.
        let new_lf = store_at(&c)
            .read(&sync_state_store::TargetKey::from_ctx(&c, new_dir.path()))
            .unwrap()
            .expect("moved baseline persisted at the new path");
        assert_eq!(new_lf.entries.len(), 1, "richest baseline must win");
        assert_eq!(new_lf.entries[0].last_materialized_hash, [9u8; 32]);
    }

    /// SW-TS-3 (AC-4b): the cross-team scan reports the team a worktree is
    /// materialized under — read from the lockfile's own `team_name`, found
    /// without knowing the team up front. An empty lockfile under a different
    /// team is ignored (nothing to tear down). SW-CP-31b: the materialization
    /// must be a NON-drained (projection) entry, so the store's raw non-empty
    /// check mirrors what `read()` would return.
    #[test]
    fn materialized_teams_for_repo_discovers_team_from_lockfile() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap(); // worktree path — keyed by its hash
        let s = slug();
        let c_alpha = ctx(home.path(), Some("alpha"));

        // Non-empty materialization under "alpha" — a projection entry (survives the
        // load-side drain, unlike a legacy install row).
        let mut lf = Lockfile::empty(
            c_alpha.org_slug.clone(),
            Some("alpha".into()),
            wt.path().to_path_buf(),
        );
        let mut proj = sample_entry(".claude/x.md");
        proj.projection_key = Some("story".to_string());
        lf.upsert_entry(proj);
        store_at(&c_alpha).upsert(c_alpha.user_id, &s, &lf).unwrap();

        // An EMPTY lockfile for the same worktree under "beta" must be ignored.
        let c_beta = ctx(home.path(), Some("beta"));
        let beta_lf = Lockfile::empty(
            c_beta.org_slug.clone(),
            Some("beta".into()),
            wt.path().to_path_buf(),
        );
        store_at(&c_beta)
            .upsert(c_beta.user_id, &s, &beta_lf)
            .unwrap();

        let found =
            materialized_teams_for_repo(home.path(), &c_alpha.org_slug, &s, wt.path()).unwrap();
        assert_eq!(found, vec![Some("alpha".to_string())]);
    }

    /// No install dir / no matching lockfile → empty (the fresh / single-team
    /// common case; the caller reads this as "no stale team to tear down").
    #[test]
    fn materialized_teams_for_repo_empty_when_nothing_materialized() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let found =
            materialized_teams_for_repo(home.path(), "acme/api", &slug(), wt.path()).unwrap();
        assert!(found.is_empty());
    }

    /// SW-IS-3b: a team that materialized ONLY into the store (no JSON on disk) is
    /// still discovered — the reader consults the store first, so cross-team teardown
    /// stays complete once the materialize path stops writing JSON. SW-CP-31b: the
    /// materialization is a NON-drained (projection) entry, so the store's raw
    /// non-empty check mirrors what `read()` would return.
    #[test]
    fn materialized_teams_for_repo_discovers_a_store_only_team() {
        let home = TempDir::new().unwrap();
        let wt = TempDir::new().unwrap();
        let s = slug();
        let c = ctx(home.path(), Some("gamma"));

        let mut lf = Lockfile::empty(
            c.org_slug.clone(),
            Some("gamma".into()),
            wt.path().to_path_buf(),
        );
        let mut proj = sample_entry(".claude/x.md");
        proj.projection_key = Some("story".to_string());
        lf.upsert_entry(proj);
        store_at(&c).upsert(c.user_id, &s, &lf).unwrap();

        let found = materialized_teams_for_repo(home.path(), &c.org_slug, &s, wt.path()).unwrap();
        assert_eq!(found, vec![Some("gamma".to_string())]);
    }

    /// SS-FU-7: stub `code_repo_path` for lockfile-path tests that don't
    /// care about which path-hash they end up at — most validation
    /// tests just want to assert the team/org/slug check fires before
    /// the hash step. Tests that DO care about path-hash determinism or
    /// per-path keying pass their own absolute `PathBuf` instead.
    fn test_repo() -> &'static Path {
        Path::new("/code/repo")
    }

    fn sample_entry(name: &str) -> MaterializedEntry {
        MaterializedEntry {
            last_materialized_size: None,
            logical_name: name.to_string(),
            dest_path: PathBuf::from(format!("/code-repo/{}", name)),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from(format!("/layer/{}", name)),
            last_materialized_hash: [7u8; 32],
            last_materialized_mtime_ns: 1_700_000_000_000_000_000,
            projection_key: None,
            source_oid: None,
        }
    }

    #[test]
    fn empty_lockfile_uses_current_schema_version() {
        let lf = Lockfile::empty(
            "acme/api".into(),
            Some("default".into()),
            PathBuf::from("/r"),
        );
        assert_eq!(lf.schema_version, SCHEMA_VERSION);
        assert_eq!(lf.org_slug, "acme/api");
        assert_eq!(lf.team_name.as_deref(), Some("default"));
        assert!(lf.entries.is_empty());
    }

    #[test]
    fn lockfile_round_trip_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sub/dir/lock.json");

        let lf = Lockfile::empty("acme".into(), None, PathBuf::from("/r"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&lf).unwrap()).unwrap();
        let read_back = Lockfile::read(&path).unwrap();
        assert_eq!(lf, read_back);
    }

    #[test]
    fn lockfile_round_trip_with_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        let mut lf = Lockfile::empty("acme".into(), Some("default".into()), PathBuf::from("/r"));
        lf.entries.push(sample_entry(".claude/skills/a.md"));
        lf.entries
            .push(sample_entry(".claude/hooks/codesense-x.sh"));

        std::fs::write(&path, serde_json::to_vec(&lf).unwrap()).unwrap();
        let read_back = Lockfile::read(&path).unwrap();
        assert_eq!(lf, read_back);
    }

    #[test]
    fn relocate_moves_legacy_lockfiles_out_of_clone_and_is_idempotent() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let rel = Path::new("default/alderco/codesense/abc123.lock.json");

        let old = path_to_org_root(home, &Principal::from(uid), "acme")
            .unwrap()
            .join("_codesense")
            .join("installs")
            .join(rel);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"{\"payload\":1}").unwrap();

        relocate_install_state_out_of_clone(home).unwrap();

        let new = path_to_install_root(home, &Principal::from(uid))
            .join("acme")
            .join(rel);
        assert!(new.exists(), "lockfile should be moved to {new:?}");
        assert_eq!(std::fs::read(&new).unwrap(), b"{\"payload\":1}");
        assert!(
            !path_to_org_root(home, &Principal::from(uid), "acme")
                .unwrap()
                .join("_codesense")
                .join("installs")
                .exists(),
            "legacy in-clone installs dir should be drained",
        );

        // Idempotent: a second pass is a clean no-op — content unchanged,
        // nothing re-created under the clone.
        relocate_install_state_out_of_clone(home).unwrap();
        assert_eq!(std::fs::read(&new).unwrap(), b"{\"payload\":1}");
        assert!(!path_to_org_root(home, &Principal::from(uid), "acme")
            .unwrap()
            .join("_codesense")
            .join("installs")
            .exists());
    }

    #[test]
    fn relocate_handles_nested_owner_repo_clone_layout() {
        // SS-FU-12: org clones live at `orgs/{owner}/{repo}`. The relocate
        // walk must descend one level and key the destination by the
        // two-segment `owner/repo` slug, matching `lockfile_path`'s
        // `installs/{org_slug}/…` layout.
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let rel = Path::new("default/alderco/codesense/abc123.lock.json");

        let old = path_to_org_root(home, &Principal::from(uid), "acme/team-context")
            .unwrap()
            .join("_codesense")
            .join("installs")
            .join(rel);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"{\"payload\":1}").unwrap();

        relocate_install_state_out_of_clone(home).unwrap();

        let new = path_to_install_root(home, &Principal::from(uid))
            .join("acme/team-context")
            .join(rel);
        assert!(new.exists(), "nested lockfile should be moved to {new:?}");
        assert_eq!(std::fs::read(&new).unwrap(), b"{\"payload\":1}");
        assert!(
            !path_to_org_root(home, &Principal::from(uid), "acme/team-context")
                .unwrap()
                .join("_codesense")
                .join("installs")
                .exists(),
            "legacy in-clone installs dir should be drained",
        );
    }

    /// Merge path (destination dir already exists, leaf absent): the leaf is
    /// moved rather than skipped. Exercises `merge_tree_no_clobber`'s
    /// successful-move branch, which the atomic-rename fast path bypasses.
    #[test]
    fn relocate_merges_into_existing_destination_dir() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let rel = Path::new("default/alderco/codesense/abc123.lock.json");

        let old = path_to_org_root(home, &Principal::from(uid), "acme")
            .unwrap()
            .join("_codesense")
            .join("installs")
            .join(rel);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"LEGACY").unwrap();
        // Destination ROOT dir pre-exists (forcing the merge path) but the
        // leaf file does not.
        std::fs::create_dir_all(path_to_install_root(home, &Principal::from(uid)).join("acme"))
            .unwrap();

        relocate_install_state_out_of_clone(home).unwrap();

        let new = path_to_install_root(home, &Principal::from(uid))
            .join("acme")
            .join(rel);
        assert_eq!(std::fs::read(&new).unwrap(), b"LEGACY");
        assert!(
            !old.exists(),
            "legacy leaf should be moved, not left behind"
        );
    }

    /// Type mismatch (a source directory whose destination name already
    /// exists as a regular file) must not abort the walk — the legacy
    /// subtree is left in place and other entries still relocate.
    #[test]
    fn relocate_tolerates_type_mismatch_without_aborting() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let old_installs = path_to_org_root(home, &Principal::from(uid), "acme")
            .unwrap()
            .join("_codesense")
            .join("installs");

        // Source has a directory `default/`; destination has a FILE `default`.
        let src_dir = old_installs.join("default/alderco/codesense");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("abc123.lock.json"), b"LEGACY").unwrap();
        let dst_root = path_to_install_root(home, &Principal::from(uid)).join("acme");
        std::fs::create_dir_all(&dst_root).unwrap();
        std::fs::write(dst_root.join("default"), b"i am a file").unwrap();

        // Must not error despite the dir-vs-file collision.
        relocate_install_state_out_of_clone(home).unwrap();

        // Conflicting file preserved; legacy subtree left in place (not lost).
        assert_eq!(
            std::fs::read(dst_root.join("default")).unwrap(),
            b"i am a file"
        );
        assert!(src_dir.join("abc123.lock.json").exists());
    }

    /// Best-effort walk: a stray non-directory entry under `orgs/` is
    /// skipped, and every relocatable org in the same pass is still moved.
    #[test]
    fn relocate_continues_across_orgs_and_skips_stray_entries() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let leaf = Path::new("default/alderco/codesense/abc.lock.json");

        for org in ["acme", "beta"] {
            let old = path_to_org_root(home, &Principal::from(uid), org)
                .unwrap()
                .join("_codesense")
                .join("installs")
                .join(leaf);
            std::fs::create_dir_all(old.parent().unwrap()).unwrap();
            std::fs::write(&old, org.as_bytes()).unwrap();
        }
        // A stray file directly under orgs/ (not a dir) must be skipped.
        std::fs::write(
            path_to_org_root(home, &Principal::from(uid), "stray.txt").unwrap(),
            b"x",
        )
        .unwrap();

        relocate_install_state_out_of_clone(home).unwrap();

        for org in ["acme", "beta"] {
            let new = path_to_install_root(home, &Principal::from(uid))
                .join(org)
                .join(leaf);
            assert_eq!(
                std::fs::read(&new).unwrap(),
                org.as_bytes(),
                "{org} should relocate despite the stray sibling entry",
            );
        }
    }

    #[test]
    fn relocate_does_not_clobber_existing_destination() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let uid = 42u64;
        let rel = Path::new("default/alderco/codesense/abc123.lock.json");

        let old = path_to_org_root(home, &Principal::from(uid), "acme")
            .unwrap()
            .join("_codesense")
            .join("installs")
            .join(rel);
        let new = path_to_install_root(home, &Principal::from(uid))
            .join("acme")
            .join(rel);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&old, b"OLD").unwrap();
        std::fs::write(&new, b"NEW").unwrap();

        relocate_install_state_out_of_clone(home).unwrap();

        // Newer dest wins; legacy file left in place rather than clobbering.
        assert_eq!(std::fs::read(&new).unwrap(), b"NEW");
        assert_eq!(std::fs::read(&old).unwrap(), b"OLD");
    }

    #[test]
    fn relocate_is_noop_without_users_dir() {
        let tmp = TempDir::new().unwrap();
        relocate_install_state_out_of_clone(tmp.path()).unwrap();
    }

    #[test]
    fn lockfile_read_rejects_schema_version_2() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");
        std::fs::write(
            &path,
            r#"{"schema_version":2,"org_slug":"a","team_name":null,"code_repo_path":"/r","entries":[]}"#,
        )
        .unwrap();
        let err = Lockfile::read(&path).unwrap_err();
        match err {
            Error::UnsupportedSchemaVersion { found, expected } => {
                assert_eq!(found, 2);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {:?}", other),
        }
    }

    /// SLW-RS-1: versions outside the supported migration range
    /// `MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION` (currently 3..=4) still
    /// hard-reject — a genuinely old stray doc (1, 2) or a too-new file from a
    /// future binary (5). In-range 3 (migrated) and 4 (native) are accepted;
    /// see `v3_lockfile_migrates_with_source_oid_none` / `v4_lockfile_roundtrips_source_oid`.
    #[test]
    fn lockfile_read_rejects_out_of_range_schema_versions() {
        let tmp = TempDir::new().unwrap();
        for found in [1u32, 2u32, SCHEMA_VERSION + 1] {
            let path = tmp.path().join(format!("lock-v{}.json", found));
            std::fs::write(
                &path,
                format!(
                    r#"{{"schema_version":{},"org_slug":"a","team_name":null,"code_repo_path":"/r","entries":[]}}"#,
                    found
                ),
            )
            .unwrap();
            let err = Lockfile::read(&path).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::UnsupportedSchemaVersion { found: f, .. } if f == found
                ),
                "version {} should surface as UnsupportedSchemaVersion, got {:?}",
                found,
                err
            );
        }
    }

    #[test]
    fn lockfile_find_entry_and_entries_by_name_agree() {
        let mut lf = Lockfile::empty("acme".into(), Some("default".into()), PathBuf::from("/r"));
        lf.entries.push(sample_entry(".claude/skills/a.md"));
        lf.entries.push(sample_entry(".claude/skills/b.md"));

        let by_name = lf.entries_by_name();
        for entry in &lf.entries {
            let via_find = lf.find_entry(&entry.logical_name).unwrap();
            let via_index = by_name.get(entry.logical_name.as_str()).copied().unwrap();
            assert_eq!(
                via_find as *const _, via_index as *const _,
                "find_entry and entries_by_name must return the same borrow"
            );
        }
        assert!(lf.find_entry(".claude/skills/missing.md").is_none());
        assert!(!by_name.contains_key(".claude/skills/missing.md"));
    }

    /// Pin the exact lockfile path formula. If this expectation
    /// changes, downstream stories need their path math updated in
    /// lockstep. SS-FU-7: layout now ends in
    /// `{owner}/{repo}/{path-hash}.lock.json`; the hash is computed
    /// inline so the assertion remains insensitive to blake3 output
    /// (deterministic, but length-bounded).
    #[test]
    fn lockfile_path_matches_expected_layout() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("backend"));
        let code_repo = Path::new("/code/repo");
        let path = lockfile_path(&c, &slug(), code_repo).unwrap();
        let expected_hash = code_repo_path_hash(code_repo).unwrap();
        assert_eq!(
            path,
            PathBuf::from(format!(
                "/home/dev/.codesense/users/42/installs/acme/api/backend/github.com/alderco/codesense/{}.lock.json",
                expected_hash
            ))
        );
    }

    #[test]
    fn retained_legacy_lock_directory_remains_authoritative_without_epoch() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("backend"));
        let install_root = path_to_install_root(home.path(), &c.principal())
            .join(&c.org_slug)
            .join("backend");
        let legacy = crate::context_repo::layout_resolver::LegacyRepositoryPathLocator::under(
            &install_root,
            &slug(),
        );
        std::fs::create_dir_all(&legacy).unwrap();

        assert_eq!(
            lockfile_path_authority(&c, &slug()).unwrap(),
            LockfilePathAuthority::Legacy
        );
        assert_eq!(lockfile_dir_path(&c, &slug()).unwrap(), legacy);
    }

    #[test]
    fn empty_canonical_lock_directory_does_not_resurrect_legacy_state() {
        let home = TempDir::new().unwrap();
        let c = ctx(home.path(), Some("backend"));
        let root = path_to_install_root(home.path(), &c.principal())
            .join(&c.org_slug)
            .join("backend");
        let legacy = crate::context_repo::layout_resolver::LegacyRepositoryPathLocator::under(
            &root,
            &slug(),
        );
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("state.yml"), "legacy state").unwrap();
        let canonical = slug().physical_path_key().unwrap().append_to(&root);
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("state.yml"), "new state").unwrap();
        std::fs::remove_file(canonical.join("state.yml")).unwrap();
        assert_eq!(
            lockfile_path_authority(&c, &slug()).unwrap(),
            LockfilePathAuthority::HostAware
        );
        assert_eq!(lockfile_dir_path(&c, &slug()).unwrap(), canonical);
        assert_eq!(
            std::fs::read_to_string(legacy.join("state.yml")).unwrap(),
            "legacy state"
        );
    }

    #[test]
    fn lockfile_path_preserves_owner_repo_layout_for_non_github() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("backend"));
        let code_repo = Path::new("/code/repo");
        let gitlab =
            SourceRepoSlug::with_provider_and_host("acme", "api", "gitlab", Some("gitlab.com"))
                .unwrap();
        let path = lockfile_path(&c, &gitlab, code_repo).unwrap();
        assert!(path.starts_with(
            "/home/dev/.codesense/users/42/installs/acme/api/backend/gitlab.com/acme/api"
        ));
        assert_ne!(path, lockfile_path(&c, &slug(), code_repo).unwrap());
    }

    /// SS-FU-7: the per-`(team, slug)` dir is the composition root that
    /// `enumerate_attached_code_repos_for_slug` walks. Pin its layout.
    #[test]
    fn lockfile_dir_path_matches_expected_layout() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("backend"));
        let dir = lockfile_dir_path(&c, &slug()).unwrap();
        assert_eq!(
            dir,
            PathBuf::from(
                "/home/dev/.codesense/users/42/installs/acme/api/backend/github.com/alderco/codesense"
            )
        );
    }

    /// The lockfile dir must live OUTSIDE the context-repo clone
    /// (`path_to_org_root`) so machine-local state can never be committed
    /// to a context repo. It lives under the user-scoped install root.
    #[test]
    fn lockfile_dir_is_outside_the_context_repo_clone() {
        use crate::context_repo::{path_to_install_root, path_to_org_root};
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("backend"));
        let dir = lockfile_dir_path(&c, &slug()).unwrap();
        let org_root = path_to_org_root(&home, &c.principal(), &c.org_slug).unwrap();
        assert!(
            !dir.starts_with(&org_root),
            "lockfile dir {dir:?} must not live inside the clone {org_root:?}",
        );
        assert!(
            dir.starts_with(path_to_install_root(&home, &c.principal())),
            "lockfile dir {dir:?} must live under the user install root",
        );
    }

    #[test]
    fn lockfile_path_uses_none_segment_when_team_is_missing() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, None);
        let path = lockfile_path(&c, &slug(), test_repo()).unwrap();
        let p = path.to_string_lossy();
        assert!(
            p.contains("/installs/acme/api/_none/github.com/alderco/codesense/"),
            "expected `_none` segment for missing team; got {}",
            p
        );
        assert!(
            p.ends_with(".lock.json"),
            "expected lockfile filename to end with .lock.json; got {}",
            p
        );
    }

    #[test]
    fn lockfile_path_rejects_team_name_with_path_separator() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("evil/team"));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        assert!(
            matches!(err, Error::InvalidTeamName { .. }),
            "expected InvalidTeamName, got {:?}",
            err
        );
    }

    #[test]
    fn hostless_install_state_is_not_shared_across_registered_hosts() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let org = path_to_org_root(&c.codesense_home, &c.principal(), &c.org_slug).unwrap();
        std::fs::create_dir_all(&org).unwrap();
        std::fs::write(org.join("teams.yml"), "teams:\n  - name: default\n    source_repos:\n      - owner: acme\n        repo: app\n      - owner: acme\n        repo: app\n        provider: gitlab\n").unwrap();
        let github = SourceRepoSlug::new("acme", "app").unwrap();
        let gitlab =
            SourceRepoSlug::with_provider_and_host("acme", "app", "gitlab", None::<String>)
                .unwrap();
        let root = path_to_install_root(&c.codesense_home, &c.principal())
            .join(&c.org_slug)
            .join("default");
        let legacy = root.join("acme/app");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("state.yml"), "saved install state").unwrap();
        for slug in [&github, &gitlab] {
            assert!(lockfile_path_authority(&c, slug)
                .unwrap_err()
                .to_string()
                .contains("ambiguous across hosts"));
        }
        let canonical = gitlab.physical_path_key().unwrap().append_to(&root);
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("state.yml"), "gitlab install state").unwrap();
        assert!(matches!(
            lockfile_path_authority(&c, &gitlab).unwrap(),
            LockfilePathAuthority::HostAware
        ));
    }

    #[test]
    fn lockfile_path_rejects_team_name_with_traversal() {
        for hostile in ["..", ".", " .. ", " . "] {
            let home = PathBuf::from("/home/dev/.codesense");
            let c = ctx(&home, Some(hostile));
            let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
            assert!(
                matches!(err, Error::InvalidTeamName { .. }),
                "{:?} should be rejected as traversal; got {:?}",
                hostile,
                err
            );
        }
    }

    #[test]
    fn lockfile_path_rejects_team_name_with_nul() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("ev\0il"));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        assert!(matches!(err, Error::InvalidTeamName { .. }));
    }

    #[test]
    fn lockfile_path_rejects_team_name_with_control_character() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("ev\nil"));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        assert!(matches!(err, Error::InvalidTeamName { .. }));
    }

    #[test]
    fn lockfile_path_rejects_empty_team_name() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some(""));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        assert!(matches!(err, Error::InvalidTeamName { .. }));
    }

    #[test]
    fn lockfile_path_rejects_backslash() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("ev\\il"));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        assert!(matches!(err, Error::InvalidTeamName { .. }));
    }

    /// P6: team literally named `_none` would collide with the no-team
    /// segment — two distinct contexts pointed at the same lockfile.
    #[test]
    fn lockfile_path_rejects_team_name_equal_to_no_team_segment() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("_none"));
        let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
        match err {
            Error::InvalidTeamName { reason } => {
                assert!(
                    reason.contains("reserved"),
                    "expected reason to mention reserved segment, got {:?}",
                    reason
                );
            }
            other => panic!("expected InvalidTeamName, got {:?}", other),
        }
    }

    /// P7: whitespace-only team_name is rejected.
    #[test]
    fn lockfile_path_rejects_team_name_whitespace_only() {
        let home = PathBuf::from("/home/dev/.codesense");
        for hostile in ["   ", "\t", "\t \t"] {
            let c = ctx(&home, Some(hostile));
            let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
            assert!(
                matches!(err, Error::InvalidTeamName { .. }),
                "{:?} should be rejected, got {:?}",
                hostile,
                err
            );
        }
    }

    /// P15 (iter-2): leading-dot team names are accepted now — the
    /// earlier `.hidden`-rejection rule was reverted. Pin the accepting
    /// behavior so a future regression doesn't re-introduce the rule.
    #[test]
    fn lockfile_path_accepts_team_name_with_leading_dot() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some(".github"));
        let path =
            lockfile_path(&c, &slug(), test_repo()).expect("leading-dot team should be accepted");
        assert!(path.to_string_lossy().contains("/.github/"));
    }

    /// P5: defensive validation of `ctx.org_slug`. Symmetric with the
    /// team_name guard — an unsafe org_slug would otherwise escape
    /// `org_root` regardless of team_name validation. `.hidden` was
    /// removed from the hostile set in iter-2 (P15 revert — leading
    /// dot is legitimate, e.g. GitHub's `.github` slug convention).
    #[test]
    fn lockfile_path_rejects_unsafe_org_slug() {
        let home = PathBuf::from("/home/dev/.codesense");
        for hostile in [
            "",
            // SS-FU-12: a single `/` is now the legitimate owner/repo
            // separator, so the hostile cases are >2 segments, empty
            // sides, traversal, and the usual unsafe-segment inputs.
            "a/b/c",
            "/repo",
            "owner/",
            "evil/../team",
            "ev\\il",
            "..",
            ".",
            "ev\0il",
            "   ",
            "  acme  ", // P3 (iter-2): surrounding whitespace
        ] {
            let c = ResolverContext {
                context_root: None,
                codesense_home: home.clone(),
                user_id: 42,
                org_slug: hostile.to_string(),
                team_name: Some("default".to_string()),
            };
            let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
            assert!(
                matches!(err, Error::InvalidOrgSlug { .. }),
                "org_slug {:?} should be rejected, got {:?}",
                hostile,
                err
            );
        }
    }

    /// P4 (iter-2): case-insensitive `_none` collision check.
    #[test]
    fn lockfile_path_rejects_team_name_case_insensitive_none() {
        let home = PathBuf::from("/home/dev/.codesense");
        for hostile in ["_none", "_NONE", "_None", "_NoNe", "_nONE"] {
            let c = ctx(&home, Some(hostile));
            let err = lockfile_path(&c, &slug(), test_repo()).unwrap_err();
            match err {
                Error::InvalidTeamName { reason } => {
                    assert!(
                        reason.contains("reserved"),
                        "expected reserved-segment reason for {:?}, got {:?}",
                        hostile,
                        reason
                    );
                }
                other => panic!(
                    "expected InvalidTeamName for {:?}, got {:?}",
                    hostile, other
                ),
            }
        }
    }

    /// P1 (iter-3): hand-constructed `SourceRepoSlug` with unsafe
    /// fields is rejected by `lockfile_path` regardless of the
    /// constructor guard.
    #[test]
    fn lockfile_path_rejects_hand_constructed_unsafe_slug() {
        let home = PathBuf::from("/home/dev/.codesense");
        let c = ctx(&home, Some("default"));

        for (owner, repo, field) in [
            ("..", "codesense", "owner"),
            ("alderco", "..", "repo"),
            ("/etc", "codesense", "owner"),
            ("alderco", "evil/repo", "repo"),
            ("ev\0il", "codesense", "owner"),
            ("alderco", " trailing-ws ", "repo"),
        ] {
            // Bypass SourceRepoSlug::new by direct struct construction.
            let bad = SourceRepoSlug {
                owner: owner.to_string(),
                repo: repo.to_string(),
                provider: "github".to_string(),
                host: Some("github.com".to_string()),
            };
            let err = lockfile_path(&c, &bad, test_repo()).unwrap_err();
            match err {
                Error::InvalidSourceRepoSlug { field: f, reason } => {
                    assert_eq!(
                        f, field,
                        "expected field {} for {{ owner={:?}, repo={:?} }}, reason={}",
                        field, owner, repo, reason
                    );
                }
                other => panic!(
                    "expected InvalidSourceRepoSlug for {:?}/{:?}, got {:?}",
                    owner, repo, other
                ),
            }
        }
    }

    /// P4 (iter-3): refuse to load lockfiles past MAX_LOCKFILE_BYTES
    /// — guards against OOM from a corrupted/malicious gigabyte file.
    #[test]
    fn lockfile_read_rejects_oversized_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("oversized.lock.json");

        // Write a file just past the bound. `std::fs::write` doesn't
        // need to allocate; we can write zero-filled bytes.
        let oversized_bytes = vec![b'a'; (super::MAX_LOCKFILE_BYTES + 1) as usize];
        std::fs::write(&path, &oversized_bytes).unwrap();

        let err = Lockfile::read(&path).unwrap_err();
        match err {
            Error::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
                assert!(
                    e.to_string().contains("MAX_LOCKFILE_BYTES"),
                    "expected size-bound message, got {}",
                    e
                );
            }
            other => panic!("expected Io InvalidData, got {:?}", other),
        }
    }

    /// P5 (iter-3): a hand-mutated `Lockfile` with a stale
    /// `schema_version` writes successfully but the on-disk bytes
    /// carry the canonical `SCHEMA_VERSION` — the force-overwrite
    /// silently corrects misuse rather than leaving a self-rejected
    /// file on disk.
    #[test]
    fn lockfile_write_forces_canonical_schema_version() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        let mut tampered =
            Lockfile::empty("acme".into(), Some("default".into()), PathBuf::from("/r"));
        tampered.schema_version = SCHEMA_VERSION - 1; // hand-mutation, still in the migrate range

        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();

        // On read, the file must normalize to SCHEMA_VERSION regardless of
        // the on-disk (migratable) value — the read path is the canonicalizer.
        let read_back = Lockfile::read(&path).unwrap();
        assert_eq!(read_back.schema_version, SCHEMA_VERSION);
    }

    /// A non-absolute `source_path` is rejected at read time — every layer
    /// is filesystem-backed, so source paths must be absolute (no sentinel
    /// exception since SW-CP-7 removed Core).
    #[test]
    fn lockfile_read_rejects_relative_source_path() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        let json = r#"{
            "schema_version": 3,
            "org_slug": "acme",
            "team_name": null,
            "code_repo_path": "/r",
            "entries": [
                {
                    "logical_name": ".claude/skills/foo.md",
                    "dest_path": "/r/.claude/skills/foo.md",
                    "source_layer": "TeamProjection",
                    "source_path": "relative/not/absolute.md",
                    "last_materialized_hash": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
                    "last_materialized_mtime_ns": 1
                }
            ]
        }"#;
        std::fs::write(&path, json).unwrap();

        let err = Lockfile::read(&path).unwrap_err();
        match err {
            Error::Io(e) => assert!(
                e.to_string().contains("absolute path"),
                "expected absolute-path message, got {}",
                e
            ),
            other => panic!("expected Io, got {:?}", other),
        }
    }

    /// P9 (iter-3): NUL bytes in deserialized `PathBuf` fields are
    /// rejected — downstream syscalls EINVAL on NUL-bearing paths.
    #[test]
    fn lockfile_read_rejects_nul_in_code_repo_path() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        let json = "{\"schema_version\":3,\"org_slug\":\"acme\",\"team_name\":null,\"code_repo_path\":\"/r\\u0000evil\",\"entries\":[]}";
        std::fs::write(&path, json).unwrap();

        let err = Lockfile::read(&path).unwrap_err();
        match err {
            Error::Io(e) => assert!(
                e.to_string().contains("NUL"),
                "expected NUL message, got {}",
                e
            ),
            other => panic!("expected Io, got {:?}", other),
        }
    }

    /// SS-7 AC-3: a v3 lockfile written by SS-3..SS-4c predates the
    /// `projection_key` field. `#[serde(default)]` MUST make those
    /// existing lockfiles round-trip with `projection_key == None`.
    ///
    /// This is the DIRECT serde shape (`serde_json::from_str::<Lockfile>`):
    /// `TeamWorkspace` loads as `TeamProjection` through `LayerId`'s own
    /// Deserialize, while `TeamRepo` is refused there. Production reads go
    /// through [`Lockfile::read`] and its tolerant [`WireLayerId`] mapping
    /// instead (tested in `lockfile_read_drops_legacy_core_entries_keeps_rest`).
    #[test]
    fn materialized_entry_deserializes_pre_ss7_lockfile_without_projection_key() {
        // Hand-crafted JSON matching the pre-SS-7 entry shape — no
        // `projection_key` field present.
        let json = r#"{
            "schema_version": 3,
            "org_slug": "acme",
            "team_name": "default",
            "code_repo_path": "/repo",
            "entries": [
                {
                    "logical_name": ".claude/skills/foo.md",
                    "dest_path": "/repo/.claude/skills/foo.md",
                    "source_layer": "TeamWorkspace",
                    "source_path": "/src/foo.md",
                    "last_materialized_hash": [
                        1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,
                        17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32
                    ],
                    "last_materialized_mtime_ns": 1700000000000000000
                }
            ]
        }"#;

        let lf: Lockfile = serde_json::from_str(json).expect("pre-SS-7 lockfile must deserialize");
        assert_eq!(lf.entries.len(), 1);
        let e = &lf.entries[0];
        assert_eq!(
            e.projection_key, None,
            "missing projection_key must default to None"
        );
        assert_eq!(e.logical_name, ".claude/skills/foo.md");
        // SW-XS-8: the retired tier name loads through the serde alias.
        assert_eq!(e.source_layer, LayerId::TeamProjection);
    }

    /// SS-7 AC-3: explicit `projection_key` round-trips through write +
    /// read — both `None` and `Some(_)` survive byte-for-byte.
    #[test]
    fn materialized_entry_round_trips_projection_key_field() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        let mut lf = Lockfile::empty(
            "acme".into(),
            Some("default".into()),
            PathBuf::from("/repo"),
        );

        // Entry 1: resolver-driven, projection_key = None.
        lf.entries.push(MaterializedEntry {
            last_materialized_size: None,
            logical_name: ".claude/skills/foo.md".into(),
            dest_path: PathBuf::from("/repo/.claude/skills/foo.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/src/foo.md"),
            last_materialized_hash: [1u8; 32],
            last_materialized_mtime_ns: 1_700_000_000_000_000_000,
            projection_key: None,
            source_oid: None,
        });
        // Entry 2: projection-driven (story), projection_key = Some.
        lf.entries.push(MaterializedEntry {
            last_materialized_size: None,
            logical_name: "epic/story.md".into(),
            dest_path: PathBuf::from("/repo/_bmad-output/epic/story.md"),
            source_layer: LayerId::TeamProjection,
            source_path: PathBuf::from("/content/epic/story.md"),
            last_materialized_hash: [2u8; 32],
            last_materialized_mtime_ns: 1_700_000_000_000_000_000,
            projection_key: Some("story".into()),
            source_oid: None,
        });

        std::fs::write(&path, serde_json::to_vec(&lf).unwrap()).unwrap();
        let read_back = Lockfile::read(&path).unwrap();
        assert_eq!(lf, read_back);
        assert_eq!(read_back.entries[0].projection_key, None);
        assert_eq!(
            read_back.entries[1].projection_key.as_deref(),
            Some("story")
        );
        assert_eq!(read_back.entries[1].source_layer, LayerId::TeamProjection);
    }

    /// SW-CP-7 AC-3: a legacy on-disk lockfile carrying a
    /// `source_layer:"Core"` entry (written by a pre-SW-CP-7 binary) must
    /// **tolerate-and-drop** — the Core entry is silently dropped on read
    /// and the rest of the file loads intact, rather than hard-failing the
    /// whole-file deserialize (which would brick sync until a manual wipe).
    #[test]
    fn lockfile_read_drops_legacy_core_entries_keeps_rest() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("lock.json");

        // Hand-written JSON: one legacy Core entry, one wave entry and one
        // projection entry on the retired install tiers (SW-XS-8).
        // The Core variant no longer exists in `LayerId`, so this shape can
        // only be produced by a prior binary — exactly the migration case.
        let json = r#"{
            "schema_version": 3,
            "org_slug": "acme",
            "team_name": "default",
            "code_repo_path": "/repo",
            "entries": [
                {
                    "logical_name": ".claude/skills/sense-sync/SKILL.md",
                    "dest_path": "/repo/.claude/skills/sense-sync/SKILL.md",
                    "source_layer": "Core",
                    "source_path": "<embedded:core>",
                    "last_materialized_hash": [9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9],
                    "last_materialized_mtime_ns": 1700000000000000000
                },
                {
                    "logical_name": ".claude/skills/team/SKILL.md",
                    "dest_path": "/repo/.claude/skills/team/SKILL.md",
                    "source_layer": "TeamRepo",
                    "source_path": "/src/.claude/skills/team/SKILL.md",
                    "last_materialized_hash": [1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],
                    "last_materialized_mtime_ns": 1700000000000000001
                },
                {
                    "logical_name": "docs/story.md",
                    "dest_path": "/repo/docs/story.md",
                    "source_layer": "TeamWorkspace",
                    "source_path": "/src/docs/story.md",
                    "last_materialized_hash": [2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],
                    "last_materialized_mtime_ns": 1700000000000000002,
                    "projection_key": "story"
                }
            ]
        }"#;
        std::fs::write(&path, json).unwrap();

        let read_back = Lockfile::read(&path).unwrap();
        assert_eq!(
            read_back.entries.len(),
            1,
            "Core dropped, the retired install-tier wave entry drained"
        );
        // SW-XS-8 load-time mapping: a projection entry tagged with a retired
        // install tier loads as team content.
        let entry = &read_back.entries[0];
        assert_eq!(entry.source_layer, LayerId::TeamProjection);
        assert_eq!(entry.logical_name, "docs/story.md");
        assert_eq!(entry.projection_key.as_deref(), Some("story"));
    }

    // ===== SW-CP-7 follow-up: prune_install_lockfiles_at_boot =====

    /// A hand-written v3 lockfile JSON with `core` legacy `"Core"` entries
    /// plus `live` TeamRepo entries. The `"Core"` shape no longer exists in
    /// `LayerId`, so only a pre-SW-CP-7 binary could have written it — the
    /// exact migration case.
    fn lockfile_json_with_core(code_repo_path: &str, core: usize, live: usize) -> String {
        let mut entries = Vec::new();
        for i in 0..core {
            entries.push(format!(
                r#"{{"logical_name":".claude/skills/sense-sync/{i}.md","dest_path":"{code_repo_path}/.claude/skills/sense-sync/{i}.md","source_layer":"Core","source_path":"<embedded:core>","last_materialized_hash":[9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9],"last_materialized_mtime_ns":1700000000000000000}}"#
            ));
        }
        for i in 0..live {
            entries.push(format!(
                r#"{{"logical_name":".claude/skills/team/{i}.md","dest_path":"{code_repo_path}/.claude/skills/team/{i}.md","source_layer":"TeamRepo","source_path":"/src/{i}.md","last_materialized_hash":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"last_materialized_mtime_ns":1700000000000000001}}"#
            ));
        }
        format!(
            r#"{{"schema_version":3,"org_slug":"acme","team_name":"default","code_repo_path":"{code_repo_path}","entries":[{}]}}"#,
            entries.join(",")
        )
    }

    /// SW-IS-3c: once the store backfill is complete (marker present), the boot
    /// sweep DELETES the retired JSON lockfiles from the install tree — the store
    /// is now the authoritative substrate, so the JSON rebuild source can go.
    #[test]
    fn prune_walks_nested_install_tree_and_deletes_json_after_backfill() {
        let home = TempDir::new().unwrap();
        let code_repo = TempDir::new().unwrap();
        let lf = home
            .path()
            .join("users")
            .join("42")
            .join("installs")
            .join("acme")
            .join("default")
            .join("owner")
            .join("repo")
            .join("abc123.lock.json");
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(
            &lf,
            lockfile_json_with_core(&code_repo.path().to_string_lossy(), 2, 1),
        )
        .unwrap();

        // Arm the completion marker so deletion is allowed (the JSON has been
        // imported into the store).
        let store_path = sync_state_store::default_store_path(home.path());
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        std::fs::write(sync_state_store::backfill_marker_path_for(&store_path), b"").unwrap();

        prune_install_lockfiles_at_boot(home.path()).unwrap();

        assert!(
            !lf.exists(),
            "retired JSON lockfile must be deleted after backfill"
        );
    }

    /// SW-IS-3c AC-2 (the load-bearing safety gate): when the backfill-completion
    /// marker is ABSENT, the boot sweep MUST NOT delete any JSON — the JSON is still
    /// the store's rebuild source until every file has been imported. Sibling of
    /// `prune_walks_nested_install_tree_and_deletes_json_after_backfill`, minus the
    /// marker.
    #[test]
    fn prune_defers_when_backfill_marker_absent() {
        let home = TempDir::new().unwrap();
        let code_repo = TempDir::new().unwrap();
        let lf = home
            .path()
            .join("users")
            .join("42")
            .join("installs")
            .join("acme")
            .join("default")
            .join("owner")
            .join("repo")
            .join("abc123.lock.json");
        std::fs::create_dir_all(lf.parent().unwrap()).unwrap();
        std::fs::write(
            &lf,
            lockfile_json_with_core(&code_repo.path().to_string_lossy(), 2, 1),
        )
        .unwrap();

        // Deliberately do NOT arm the completion marker.
        let store_path = sync_state_store::default_store_path(home.path());
        assert!(
            !sync_state_store::backfill_marker_path_for(&store_path).exists(),
            "precondition: marker absent"
        );

        // Ok, and never deletes before the backfill is proven complete.
        prune_install_lockfiles_at_boot(home.path()).unwrap();

        assert!(
            lf.exists(),
            "JSON lockfile must survive when the backfill marker is absent"
        );
    }

    #[test]
    fn prune_missing_users_dir_is_ok() {
        let home = TempDir::new().unwrap();
        // No `users/` dir present at all -> Ok no-op, never an error.
        prune_install_lockfiles_at_boot(home.path()).unwrap();
    }

    #[test]
    fn prune_ignores_non_lockfile_files() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("users").join("42").join("installs");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("notes.txt");
        std::fs::write(&other, "not a lockfile").unwrap();

        prune_install_lockfiles_at_boot(home.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "not a lockfile");
    }

    #[test]
    fn path_is_confirmed_absent_only_on_ok_false() {
        let tmp = TempDir::new().unwrap();
        assert!(
            !path_is_confirmed_absent(tmp.path()),
            "an existing dir is not confirmed absent"
        );
        assert!(
            path_is_confirmed_absent(&tmp.path().join("does-not-exist")),
            "a missing path is confirmed absent"
        );
    }

    // ===== SS-4c D-14: enumerate_attached_code_repos_for_slug / _for_team =====

    fn slug_b() -> SourceRepoSlug {
        SourceRepoSlug::new("bravo", "two").unwrap()
    }

    // ===== SW-IS-3a: readers repointed onto the install store (store ∪ JSON) =====

    fn store_at(c: &ResolverContext) -> SyncStateStore {
        SyncStateStore::open(sync_state_store::default_store_path(&c.codesense_home)).unwrap()
    }

    /// AC-1: enumerate serves a target that lives ONLY in the store (no JSON file
    /// on disk) — proving the read goes through the indexed SELECT, not the walk.
    #[test]
    fn enumerate_reads_store_targets_without_a_json_file() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let code_repo = PathBuf::from("/code/store-only");

        let lf = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), code_repo.clone());
        store_at(&c).upsert(c.user_id, &s, &lf).unwrap();

        let result = enumerate_attached_code_repos_for_slug(&c, &s).unwrap();
        assert_eq!(result.len(), 1, "store-only target is enumerated");
        assert_eq!(result[0].code_repo_path, code_repo);
    }

    /// SW-IS-3c: the store is the sole substrate — every attached target is enumerated
    /// exactly once, and re-upserting the same path is naturally de-duplicated by the
    /// store's `(org, team, code_repo_path)` key.
    #[test]
    fn enumerate_unions_store_and_json_only_targets_without_duplicates() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let first = PathBuf::from("/code/first");
        let second = PathBuf::from("/code/second");
        let repeated = PathBuf::from("/code/repeated");

        // Seed three targets, upserting `repeated` twice — the store's PK collapses
        // the duplicate rather than emitting it twice.
        let store = store_at(&c);
        for p in [&first, &second, &repeated, &repeated] {
            let lf = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), p.clone());
            store.upsert(c.user_id, &s, &lf).unwrap();
        }

        let paths: HashSet<PathBuf> = enumerate_attached_code_repos_for_slug(&c, &s)
            .unwrap()
            .into_iter()
            .map(|r| r.code_repo_path)
            .collect();
        assert_eq!(
            paths,
            HashSet::from([first, second, repeated]),
            "every target appears exactly once (de-duplicated)"
        );
    }

    /// SW-IS-3c: `lockfile_mtime` returns the store's per-target `updated_at` (the
    /// store is now the sole writer, so its timestamp captures every materialize wave);
    /// `None` when there is no store row (fail-safe not-reapable).
    #[test]
    fn lockfile_mtime_takes_the_freshest_of_store_and_file() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let seed_store = |repo: &PathBuf, secs: u64| {
            let lf = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), repo.clone());
            store_at(&c)
                .upsert_with_updated_at(
                    c.user_id,
                    &s,
                    &lf,
                    sync_state_store::system_time_to_unix_nanos(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
                    ),
                )
                .unwrap();
        };

        // A seeded target reports its store `updated_at`.
        let seeded = PathBuf::from("/code/seeded");
        seed_store(&seeded, 1_000_000_000);
        assert_eq!(
            lockfile_mtime(&c, &seeded),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
        );

        // An updated wave advances the timestamp (store is the sole writer).
        seed_store(&seeded, 1_600_000_000);
        assert_eq!(
            lockfile_mtime(&c, &seeded),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
        );

        // No store row: None (fail-safe not-reapable).
        assert_eq!(lockfile_mtime(&c, Path::new("/code/absent")), None);
    }

    #[test]
    fn enumerate_attached_code_repos_for_slug_returns_empty_when_dir_absent() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let result = enumerate_attached_code_repos_for_slug(&c, &s).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn enumerate_attached_code_repos_for_slug_returns_one_per_matching_lockfile() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let code_repo = PathBuf::from("/code/repo-A");

        let lf = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), code_repo.clone());
        store_at(&c).upsert(c.user_id, &s, &lf).unwrap();

        let result = enumerate_attached_code_repos_for_slug(&c, &s).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, s);
        assert_eq!(result[0].code_repo_path, code_repo);
    }

    #[test]
    fn enumerate_attached_code_repos_for_slug_filters_by_repo_filename() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s_a = SourceRepoSlug::new("alderco", "codesense").unwrap();
        let s_b = SourceRepoSlug::new("alderco", "other").unwrap();
        let repo_a = PathBuf::from("/A");
        let repo_b = PathBuf::from("/B");

        let lf_a = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), repo_a.clone());
        let lf_b = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), repo_b.clone());
        let store = store_at(&c);
        store.upsert(c.user_id, &s_a, &lf_a).unwrap();
        store.upsert(c.user_id, &s_b, &lf_b).unwrap();

        let result_a = enumerate_attached_code_repos_for_slug(&c, &s_a).unwrap();
        assert_eq!(result_a.len(), 1);
        assert_eq!(result_a[0].code_repo_path, repo_a);
    }

    /// SS-FU-7 AC-3 (b): two lockfiles for the SAME slug but DIFFERENT
    /// `code_repo_path` (main + worktree pattern) produce two entries.
    /// This is the load-bearing per-path isolation that motivates the
    /// substrate change.
    #[test]
    fn enumerate_attached_code_repos_for_slug_returns_multiple_for_same_slug_different_paths() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s = slug();
        let main = PathBuf::from("/repos/codesense");
        let worktree = PathBuf::from("/repos/codesense-feat-x");

        let lf_main = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), main.clone());
        let lf_wt = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), worktree.clone());
        let store = store_at(&c);
        store.upsert(c.user_id, &s, &lf_main).unwrap();
        store.upsert(c.user_id, &s, &lf_wt).unwrap();

        let result = enumerate_attached_code_repos_for_slug(&c, &s).unwrap();
        assert_eq!(result.len(), 2, "expected one entry per code_repo_path");
        let paths: std::collections::HashSet<_> =
            result.iter().map(|r| r.code_repo_path.clone()).collect();
        assert!(paths.contains(&main));
        assert!(paths.contains(&worktree));
    }

    #[test]
    fn enumerate_attached_code_repos_for_team_unions_across_slugs_with_dedup() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let s_a = slug();
        let s_b = slug_b();
        let repo_a = PathBuf::from("/A");
        let repo_b = PathBuf::from("/B");

        let lf_a = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), repo_a.clone());
        let lf_b = Lockfile::empty(c.org_slug.clone(), c.team_name.clone(), repo_b.clone());
        let store = store_at(&c);
        store.upsert(c.user_id, &s_a, &lf_a).unwrap();
        store.upsert(c.user_id, &s_b, &lf_b).unwrap();

        let union =
            enumerate_attached_code_repos_for_team(&c, &[s_a.clone(), s_b.clone()]).unwrap();
        assert_eq!(union.len(), 2);
        let paths: std::collections::HashSet<_> =
            union.iter().map(|r| r.code_repo_path.clone()).collect();
        assert!(paths.contains(&repo_a));
        assert!(paths.contains(&repo_b));
    }

    #[test]
    fn enumerate_attached_code_repos_for_team_empty_slugs_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let result = enumerate_attached_code_repos_for_team(&c, &[]).unwrap();
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------
    // SS-FU-7 AC-2: code_repo_path_hash
    // -----------------------------------------------------------------

    #[test]
    fn code_repo_path_hash_is_deterministic() {
        let p = Path::new("/Users/alice/projects/codesense");
        let a = code_repo_path_hash(p).unwrap();
        let b = code_repo_path_hash(p).unwrap();
        assert_eq!(a, b, "same input must yield same hash across calls");
        assert_eq!(a.len(), 16, "hash must be 16 hex chars");
        assert!(
            a.chars().all(|c| c.is_ascii_hexdigit()),
            "hash must be lowercase hex: {}",
            a
        );
    }

    #[test]
    fn code_repo_path_hash_distinct_paths_produce_distinct_hashes() {
        let h1 = code_repo_path_hash(Path::new("/Users/alice/projects/codesense")).unwrap();
        let h2 =
            code_repo_path_hash(Path::new("/Users/alice/projects/codesense-worktree-1")).unwrap();
        let h3 = code_repo_path_hash(Path::new("/Users/alice/projects/other-repo")).unwrap();
        assert_ne!(h1, h2);
        assert_ne!(h1, h3);
        assert_ne!(h2, h3);
    }

    /// AC-2 (c): symlink path and its target produce DIFFERENT hashes.
    /// The helper does NOT canonicalize — matches the wave engine's
    /// per-path lock semantics. Verify with a real on-disk symlink so
    /// the test wouldn't be misleadingly trivial (an in-memory test
    /// would just assert two different PathBufs hash differently).
    #[cfg(unix)]
    #[test]
    fn code_repo_path_hash_symlink_and_target_distinct() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("real-repo");
        let link = tmp.path().join("link-to-repo");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let h_target = code_repo_path_hash(&target).unwrap();
        let h_link = code_repo_path_hash(&link).unwrap();
        assert_ne!(
            h_target, h_link,
            "symlink path and target path must hash differently (no canonicalization)"
        );

        // Sanity: both paths actually canonicalize to the same target.
        // If canonicalization equates them, but our hashes differ, then
        // the helper's "no canonicalize" contract is genuinely
        // load-bearing.
        let c_target = target.canonicalize().unwrap();
        let c_link = link.canonicalize().unwrap();
        assert_eq!(
            c_target, c_link,
            "test setup invariant: symlink and target canonicalize together"
        );
    }

    #[test]
    fn code_repo_path_hash_rejects_relative_paths() {
        let err = code_repo_path_hash(Path::new("relative/path")).unwrap_err();
        match err {
            Error::Io(io_err) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidInput);
                assert!(
                    io_err.to_string().contains("absolute"),
                    "error should mention absolute requirement: {}",
                    io_err
                );
            }
            other => panic!("expected Io(InvalidInput), got {:?}", other),
        }

        // Also reject empty path (it's not absolute).
        assert!(code_repo_path_hash(Path::new("")).is_err());
        // And reject single-segment relative paths.
        assert!(code_repo_path_hash(Path::new("foo")).is_err());
    }
}
