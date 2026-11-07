// SPDX-License-Identifier: MIT
//! The on-disk files of one routine definition and its revisions.
//!
//! ```text
//! <routine dir>/routine.yml          current definition
//! <routine dir>/revisions/<n>.yml    immutable snapshot of revision n
//! ```
//!
//! A routine dir lives under its agent in the space's `config/` (see
//! [`super::layout`] for where), or, before SW-AGR-1, in `config/routines/<id>`.
//! `config/` is the team's git-tracked, never-projected sibling of `content/`
//! (the same home as `config/sprints.yaml`), so a routine travels with the
//! context repo but is never materialized into a source repo or overlaid onto
//! a consumer team.
//!
//! Every read fills in the agent the dir's path names, and every write under
//! an agent leaves `agent:` out ([`super::layout::implied_agent`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use crate::shared::revisioned_defs::DefsError;
use crate::shared::revisioned_defs::{self, read_optional, revision_path, yaml_err};
use crate::shared::yaml_io::write_yaml_value_atomic;

use super::layout::implied_agent;
use super::model::{AgentRef, Author, RoutineDef};

pub const ROUTINE_FILE: &str = "routine.yml";

/// `def` as read from `dir`: the agent its path names, whatever the file says.
fn filled(dir: &Path, mut def: RoutineDef) -> RoutineDef {
    if !def.is_command() {
        if let Some(agent) = implied_agent(dir) {
            def.agent = agent;
        }
    }
    def
}

/// `def` as written into `dir`: no `agent:` where the path names it.
fn stripped(dir: &Path, def: &RoutineDef) -> RoutineDef {
    let mut def = def.clone();
    if implied_agent(dir).is_some() {
        def.agent = AgentRef::default();
    }
    def
}

/// One immutable revision file: the full definition as saved, plus who saved it
/// and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionRecord {
    pub revision: u32,
    pub author: Author,
    /// RFC 3339 UTC.
    pub at: String,
    pub routine: RoutineDef,
}

/// The current definition in `dir`, or `None` when there is none.
pub fn read_def(dir: &Path) -> Result<Option<RoutineDef>, DefsError> {
    Ok(read_raw(dir)?.map(|def| filled(dir, def)))
}

/// The definition in `dir` exactly as written: no agent filled in. What the
/// migrator reads a legacy file as.
pub fn read_raw(dir: &Path) -> Result<Option<RoutineDef>, DefsError> {
    read_optional(&dir.join(ROUTINE_FILE))
}

/// Revision `n` of the routine in `dir`, or `None` when it does not exist.
pub fn read_revision(dir: &Path, n: u32) -> Result<Option<RevisionRecord>, DefsError> {
    Ok(
        read_optional::<RevisionRecord>(&revision_path(dir, n))?.map(|mut record| {
            record.routine = filled(dir, record.routine);
            record
        }),
    )
}

/// The definition at revision `n`, which a run pinned to `n` runs: its
/// snapshot, or the live definition while it is still at `n`.
pub fn read_pinned(dir: &Path, n: u32) -> Result<Option<RoutineDef>, DefsError> {
    if let Some(record) = read_revision(dir, n)? {
        return Ok(Some(record.routine));
    }
    Ok(read_def(dir)?.filter(|def| def.revision == n))
}

/// Every revision of the routine in `dir`, oldest first. Files that are not
/// `<n>.yml` are ignored.
pub fn read_revisions(dir: &Path) -> Result<Vec<RevisionRecord>, DefsError> {
    let numbers = revisioned_defs::revision_numbers(dir)?;
    let mut out = Vec::with_capacity(numbers.len());
    for n in numbers {
        if let Some(rec) = read_revision(dir, n)? {
            out.push(rec);
        }
    }
    Ok(out)
}

/// Write `def` as revision `def.revision`: first the immutable snapshot, then
/// the current `routine.yml`. Both writes are atomic renames, so a crash between
/// them leaves the previous current definition intact and the orphan snapshot
/// is overwritten by the next save of the same number. Callers hold the
/// routine's lock (see [`with_routine_lock`]).
pub fn write_revision(
    dir: &Path,
    def: &RoutineDef,
    author: &Author,
    at: &str,
) -> Result<(), DefsError> {
    let def = stripped(dir, def);
    let record = RevisionRecord {
        revision: def.revision,
        author: author.clone(),
        at: at.to_string(),
        routine: def.clone(),
    };
    let rev_path = revision_path(dir, def.revision);
    write_yaml_value_atomic(&rev_path, &record).map_err(yaml_err(&rev_path))?;
    write_current(dir, &def)
}

/// Write `def` as `dir`'s current definition only, with no snapshot: the
/// migrator's tombstone, and a copy whose snapshots are already in place.
pub fn write_current(dir: &Path, def: &RoutineDef) -> Result<(), DefsError> {
    let path = dir.join(ROUTINE_FILE);
    write_yaml_value_atomic(&path, &stripped(dir, def)).map_err(yaml_err(&path))
}

/// Write one snapshot record as is (agent stripped under an agent dir): what
/// the migrator copies a legacy revision forward with.
pub fn write_revision_record(dir: &Path, record: &RevisionRecord) -> Result<(), DefsError> {
    let record = RevisionRecord {
        routine: stripped(dir, &record.routine),
        ..record.clone()
    };
    let rev_path = revision_path(dir, record.revision);
    write_yaml_value_atomic(&rev_path, &record).map_err(yaml_err(&rev_path))
}

/// Every routine definition directly under `routines_dir` (one agent's
/// `routines/`, or the legacy dir), as `(dir, result)`, each read as
/// [`read_def`] reads it. A dir with no `routine.yml` is skipped; a malformed
/// one is returned as an error so the caller decides whether to skip it.
pub fn scan(routines_dir: &Path) -> Vec<(PathBuf, Result<RoutineDef, DefsError>)> {
    revisioned_defs::scan(routines_dir, ROUTINE_FILE)
        .into_iter()
        .map(|(dir, def)| {
            let def = def.map(|def| filled(&dir, def));
            (dir, def)
        })
        .collect()
}

/// The revision numbers that have a snapshot in `dir`, ascending.
pub fn revision_numbers(dir: &Path) -> Result<Vec<u32>, DefsError> {
    revisioned_defs::revision_numbers(dir)
}

/// Snapshot `n` of `dir` exactly as written.
pub fn read_revision_raw(dir: &Path, n: u32) -> Result<Option<RevisionRecord>, DefsError> {
    read_optional(&revision_path(dir, n))
}

/// `PathBuf` of `dir`'s current definition file.
pub fn def_path(dir: &Path) -> PathBuf {
    dir.join(ROUTINE_FILE)
}

/// Run `f` while holding the in-process lock for the routine at `dir`, so two
/// read-check-write saves of one routine cannot interleave.
pub fn with_routine_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    revisioned_defs::with_def_lock(dir, f)
}

/// Tests: the dir routine `id` lives in within the space at
/// `team_config_dir`, under whichever agent holds it, else where a legacy
/// (pre SW-AGR-1) one would be.
#[cfg(test)]
pub fn routine_dir(team_config_dir: &Path, id: &str) -> PathBuf {
    super::layout::find_in_agents(team_config_dir, id)
        .unwrap_or_else(|| super::layout::legacy_routine_dir(team_config_dir, id))
}

/// Tests: where routine dir `dir` lands in the space at `to_config_dir`
/// when a sync brings it there: the same path under that `config/`.
#[cfg(test)]
pub fn same_dir_in(dir: &Path, to_config_dir: &Path) -> PathBuf {
    let from_config = super::layout::config_dir_of(dir).expect("a routine dir");
    to_config_dir.join(
        dir.strip_prefix(&from_config)
            .expect("under its config dir"),
    )
}

/// Tests: the dir a sync of routine dir `from` lands at on a clone where
/// [`routine_dir`] answers `to`: `to` when the clone already has it, else
/// `from`'s path under that clone's `config/`.
#[cfg(test)]
pub fn sync_target(from: &Path, to: &Path) -> PathBuf {
    if to.exists() {
        return to.to_path_buf();
    }
    same_dir_in(
        from,
        &super::layout::config_dir_of(to).expect("a routine dir"),
    )
}
