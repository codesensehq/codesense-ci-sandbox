// SPDX-License-Identifier: MIT
//! The one-time move of every legacy `config/routines/<id>/` routine to its
//! agent's dir (SW-AGR-1, SPEC §3 and §9.2).
//!
//! **Copy forward, never move.** Each legacy routine is copied, revisions and
//! all, to the dir [`super::layout`] gives its agent, keeping its id, revision
//! and every field, plus `mode: fresh` written explicitly (what it always did)
//! and `migrated: {from}`. The legacy file stays for one release as a
//! tombstone: the full definition an older client still reads and runs, plus
//! `moved_to: <new dir>`, which such a client keeps as an unknown key. Its key
//! (`<org>/<team>/<id>`) does not change, so the run store (enabled flag,
//! cursors, runs) carries over untouched.
//!
//! **The legacy base.** Beside the migrated routine, [`BASE_FILE`] keeps the
//! legacy definition as last brought forward (or last mirrored back), as a
//! canonical YAML value without `moved_to`. Everything here compares and
//! hashes that value, never a re-serialized [`RoutineDef`], so a field this
//! build adds (and defaults) never makes an unchanged file look edited.
//!
//! **Idempotent and 3-way.** A rerun over a legacy file equal to its base
//! writes nothing. An older client that edits its (tombstone) file makes it
//! differ, and only the fields that changed between the base and that file
//! are applied to the migrated routine, as its next revision: a newer
//! client's own edits, its archive and the fields only it knows stay. An
//! older client never un-archives (it has no way to), so a missing
//! `archived_at` is never carried over an archived routine.
//!
//! **Mirroring back.** What an older client fires by (`armed_host`,
//! `archived_at`, `runs_on`, `agent`) is mirrored to the tombstone whenever
//! the migrated routine is saved, archived or moved ([`mirror_to_tombstone`]),
//! and the base follows, so an older Mac neither fires a routine moved off it
//! nor one archived, and the mirror never reads as its edit.
//!
//! **Deterministic.** Every write is a pure function of the files, so two Macs
//! migrating the same tree write the same bytes and their syncs merge.
//!
//! **Ids stay unique in the space.** A legacy id already taken under an agent
//! by another routine (two clients racing) is renamed `<id>-2`, … with a note
//! the routine shows. Run-store rows (runs, cursors, the switch) stay with
//! the key, so with the routine that keeps the id: before this pass that key
//! only ever named that routine on this Mac (a create refuses an id the legacy
//! dir holds), and the renamed one starts with no history here. The known gap:
//! a Mac that ran the legacy routine under that key before a teammate's
//! routine of the same id synced in keeps those rows with the teammate's.
//! **Read-only docs contexts** are never written; SW-AGR-10 migrates their
//! templates.

use std::path::{Component, Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::context_repo::codesense_docs::{is_docs_slug, refuse_write_at};
use crate::domain::Principal;
use crate::shared::revisioned_defs::{read_optional, revision_path, yaml_err};
use crate::shared::yaml_io::write_yaml_value_atomic;

use super::defs::{self, DefsError, RevisionRecord};
use super::discover::invalidate_discovery_cache;
use super::layout::{self, Home};
use super::model::{Author, Migrated, RoutineDef, RoutineKey, RoutineMode};

/// The tombstone key: where the routine lives now, relative to `config/`.
pub const MOVED_TO: &str = "moved_to";
/// The legacy base beside a migrated routine's `routine.yml`.
pub const BASE_FILE: &str = "legacy-base.yml";
/// When a re-applied legacy edit has no revision record to date it by.
const FALLBACK_AT: &str = "1970-01-01T00:00:00Z";
/// What an older client fires by, mirrored to its tombstone.
const MIRRORED: [&str; 4] = ["armed_host", "archived_at", "runs_on", "agent"];
/// Never taken from a legacy edit: the routine's identity and history are
/// the migrated routine's own.
const NEVER_APPLIED: [&str; 4] = ["id", "revision", MOVED_TO, "migrated"];

/// What one pass did, by legacy id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// `(legacy id, new dir relative to config/)`.
    pub moved: Vec<(String, String)>,
    /// Legacy edits re-applied forward.
    pub reapplied: Vec<String>,
    /// `(legacy id, new id)` for ids that clashed.
    pub renamed: Vec<(String, String)>,
}

impl MigrationReport {
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.reapplied.is_empty() && self.renamed.is_empty()
    }
}

/// Whether the space of `org` at `config_dir` is one the migrator may write:
/// never the read-only CodeSense docs.
fn writable(org: &str, config_dir: &Path) -> bool {
    !is_docs_slug(org) && refuse_write_at(config_dir).is_ok()
}

/// `value` with every mapping's keys in order, so equal content is equal
/// bytes whatever order a writer used.
fn canonical(value: Value) -> Value {
    match value {
        Value::Mapping(map) => {
            let mut entries: Vec<(Value, Value)> =
                map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            entries.sort_by(|(a, _), (b, _)| key_text(a).cmp(&key_text(b)));
            Value::Mapping(entries.into_iter().collect())
        }
        Value::Sequence(items) => Value::Sequence(items.into_iter().map(canonical).collect()),
        Value::Tagged(tagged) => Value::Tagged(Box::new(serde_yaml::value::TaggedValue {
            tag: tagged.tag,
            value: canonical(tagged.value),
        })),
        other => other,
    }
}

fn key_text(key: &Value) -> String {
    match key {
        Value::String(s) => s.clone(),
        other => serde_yaml::to_string(other).unwrap_or_default(),
    }
}

/// A legacy file's content as compared and stored: its YAML value without
/// the tombstone key, canonical.
fn legacy_content(mut raw: Value) -> Value {
    if let Value::Mapping(map) = &mut raw {
        map.remove(MOVED_TO);
    }
    canonical(raw)
}

/// The content hash of a legacy file, keyed on its YAML value (not on this
/// build's [`RoutineDef`]) without `moved_to`.
pub fn content_hash(raw: &Value) -> String {
    let yaml = serde_yaml::to_string(&legacy_content(raw.clone())).unwrap_or_default();
    crate::agents::manifest::content_hash(yaml.as_bytes())
}

fn read_value(path: &Path) -> Result<Option<Value>, DefsError> {
    read_optional(path)
}

fn write_value(path: &Path, value: &Value) -> Result<(), DefsError> {
    write_yaml_value_atomic(path, value).map_err(yaml_err(path))
}

/// `dir` relative to `config_dir`, `/`-separated.
fn rel(config_dir: &Path, dir: &Path) -> String {
    dir.strip_prefix(config_dir)
        .unwrap_or(dir)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The legacy `routines/<id>` a migrated routine names, as `migrated.from`.
fn legacy_rel(id: &str) -> String {
    format!("{}/{id}", layout::ROUTINES_DIR)
}

/// A tombstone's `moved_to`, when it names a dir under `config/agents/` (a
/// hand edit or a hostile file never sends the migrator elsewhere).
fn moved_to_dir(config_dir: &Path, raw: &Value) -> Option<PathBuf> {
    let to = raw.get(MOVED_TO)?.as_str()?;
    let path = Path::new(to);
    let mut parts = path.components();
    if parts.next() != Some(Component::Normal(layout::AGENTS_DIR.as_ref())) {
        return None;
    }
    if !parts.all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(config_dir.join(path))
}

/// The legacy dir a migrated routine's `migrated.from` names, when it is
/// exactly `routines/<id>` (normal components only), like [`moved_to_dir`].
fn legacy_dir_of(config_dir: &Path, from: &str) -> Option<PathBuf> {
    let path = Path::new(from);
    let parts: Vec<Component> = path.components().collect();
    match parts.as_slice() {
        [Component::Normal(first), Component::Normal(_)]
            if *first == std::ffi::OsStr::new(layout::ROUTINES_DIR) =>
        {
            Some(config_dir.join(path))
        }
        _ => None,
    }
}

/// Bring every legacy routine of `org/team` (as `principal` sees it) forward.
/// A space with no legacy dir costs one stat. Failures are logged per routine
/// and never stop the rest.
pub fn migrate_space(
    codesense_home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    config_dir: &Path,
) -> MigrationReport {
    let legacy = layout::legacy_routines_dir(config_dir);
    if !legacy.is_dir() || !writable(org, config_dir) {
        return MigrationReport::default();
    }
    let report = defs::with_routine_lock(&legacy, || {
        let mut report = MigrationReport::default();
        for (dir, def) in defs::scan(&legacy) {
            let id = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            // `scan` fills nothing in for a legacy dir: this is the file as written.
            let def = match def {
                Ok(def) if def.id == id => def,
                Ok(def) => {
                    log::warn!(target: "routines", "not migrating {}: id {:?} does not match its directory", dir.display(), def.id);
                    continue;
                }
                Err(e) => {
                    log::warn!(target: "routines", "not migrating unreadable routine: {e}");
                    continue;
                }
            };
            let raw = match read_value(&defs::def_path(&dir)) {
                Ok(Some(raw)) => raw,
                Ok(None) => continue,
                Err(e) => {
                    log::warn!(target: "routines", "not migrating unreadable routine: {e}");
                    continue;
                }
            };
            let key = RoutineKey::scoped(principal, org, team, &id);
            if let Err(e) = migrate_one(config_dir, &key, &dir, def, raw, &mut report) {
                log::warn!(target: "routines", "routine {key}: not migrated: {e}");
            }
        }
        report
    });
    if !report.is_empty() {
        log::info!(target: "routines", "migrated routines of {org}/{team}: {report:?}");
        invalidate_discovery_cache(codesense_home);
    }
    report
}

/// The migrated routine that came from legacy `id`: where its tombstone
/// points, else any routine under the space's agents that says it came from
/// there (it moved to another agent since, or an older client dropped the
/// tombstone key).
fn migrated_from(config_dir: &Path, raw: &Value, id: &str) -> Option<(PathBuf, RoutineDef)> {
    let from = legacy_rel(id);
    let came_from = |def: &RoutineDef| def.migrated.as_ref().is_some_and(|m| m.from == from);
    if let Some(dir) = moved_to_dir(config_dir, raw) {
        if let Ok(Some(def)) = defs::read_def(&dir) {
            if came_from(&def) {
                return Some((dir, def));
            }
        }
    }
    layout::scan_agents(config_dir)
        .into_iter()
        .find_map(|(_, dir)| match defs::read_def(&dir) {
            Ok(Some(def)) if came_from(&def) => Some((dir, def)),
            _ => None,
        })
}

/// `legacy` as it is written forward under id `id`.
fn forward(legacy: &RoutineDef, id: &str) -> RoutineDef {
    let mut def = legacy.clone();
    def.id = id.to_string();
    def.extra.remove(MOVED_TO);
    if def.mode.trim().is_empty() {
        // §9.2: every legacy routine says what it always did.
        def.mode = RoutineMode::Fresh.as_str().to_string();
    }
    def
}

/// The base stored beside the migrated routine at `dir`.
fn read_base(dir: &Path) -> Result<Option<Value>, DefsError> {
    read_value(&dir.join(BASE_FILE))
}

fn write_base(dir: &Path, content: &Value) -> Result<(), DefsError> {
    write_value(&dir.join(BASE_FILE), content)
}

/// Point the tombstone at `to`, changing nothing else in it.
fn write_tombstone(
    config_dir: &Path,
    legacy_dir: &Path,
    raw: &Value,
    to: &Path,
) -> Result<(), DefsError> {
    let to = rel(config_dir, to);
    if raw.get(MOVED_TO).and_then(Value::as_str) == Some(to.as_str()) {
        return Ok(());
    }
    let mut tombstone = raw.clone();
    if let Value::Mapping(map) = &mut tombstone {
        map.insert(Value::from(MOVED_TO), Value::from(to));
    }
    write_value(&defs::def_path(legacy_dir), &tombstone)
}

fn migrate_one(
    config_dir: &Path,
    key: &RoutineKey,
    legacy_dir: &Path,
    legacy: RoutineDef,
    raw: Value,
    report: &mut MigrationReport,
) -> Result<(), DefsError> {
    let dir = bring_forward(config_dir, key, legacy_dir, legacy, raw, report)?;
    // Mirror what an older client fires by once the routine is where it lives,
    // which also settles a mirror a crash interrupted. The legacy lock is held.
    if let Some(dir) = dir {
        mirror_locked(config_dir, &dir)?;
    }
    Ok(())
}

/// [`migrate_one`] without the mirror: where the routine lives now, or
/// `None` when it was left for the next pass.
fn bring_forward(
    config_dir: &Path,
    key: &RoutineKey,
    legacy_dir: &Path,
    legacy: RoutineDef,
    raw: Value,
    report: &mut MigrationReport,
) -> Result<Option<PathBuf>, DefsError> {
    let id = key.id.clone();
    let content = legacy_content(raw.clone());
    if let Some((dir, _)) = migrated_from(config_dir, &raw, &id) {
        let base = read_base(&dir)?;
        let dir = if base.as_ref() == Some(&content) {
            dir
        } else {
            // Under the routine's own lock, as every service save is: an edit
            // here and the re-applied one never interleave.
            let applied = defs::with_routine_lock(&dir.clone(), || {
                reapply(config_dir, key, legacy_dir, &raw, &dir, base.as_ref())
            })?;
            match applied {
                Some(dir) => {
                    report.reapplied.push(id.clone());
                    dir
                }
                // It went away or moved meanwhile: the next pass looks again.
                None => return Ok(None),
            }
        };
        write_tombstone(config_dir, legacy_dir, &raw, &dir)?;
        return Ok(Some(dir));
    }
    // First time: the agent's dir, under the legacy id unless another routine
    // already holds it there. A routine holding it that came from this very
    // legacy file (another process migrated it a moment ago) is no clash.
    let home = layout::home_for_ref(key, &legacy.agent);
    let (new_id, note) = match layout::find_in_agents(config_dir, &id) {
        Some(holder) => {
            let same = defs::read_def(&holder)
                .ok()
                .flatten()
                .and_then(|def| def.migrated)
                .is_some_and(|m| m.from == legacy_rel(&id));
            if same {
                write_tombstone(config_dir, legacy_dir, &raw, &holder)?;
                return Ok(Some(holder));
            }
            let new_id = layout::first_free_id(config_dir, &format!("{}-2", trimmed(&id)));
            report.renamed.push((id.clone(), new_id.clone()));
            let note = format!(
                "Renamed from {id} when it moved under its agent: another routine already had that id"
            );
            (new_id, Some(note))
        }
        None => (id.clone(), None),
    };
    let dir = layout::routine_dir_in(config_dir, &home, &new_id);
    for n in defs::revision_numbers(legacy_dir)? {
        copy_revision(legacy_dir, &dir, n, &new_id)?;
    }
    let mut def = forward(&legacy, &new_id);
    def.migrated = Some(Migrated {
        from: legacy_rel(&id),
        note,
        revision: Some(def.revision),
    });
    defs::write_current(&dir, &def)?;
    write_base(&dir, &content)?;
    report.moved.push((id, rel(config_dir, &dir)));
    write_tombstone(config_dir, legacy_dir, &raw, &dir)?;
    Ok(Some(dir))
}

/// An id short enough to take a `-N` suffix and stay valid.
fn trimmed(id: &str) -> String {
    id.chars()
        .take(56)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// Copy legacy snapshot `n` into `dir`, as revision `n` of routine `id`.
/// One this build cannot read is copied byte for byte.
fn copy_revision(legacy_dir: &Path, dir: &Path, n: u32, id: &str) -> Result<(), DefsError> {
    let to = revision_path(dir, n);
    if to.exists() {
        return Ok(());
    }
    match defs::read_revision_raw(legacy_dir, n) {
        Ok(Some(record)) => defs::write_revision_record(
            dir,
            &RevisionRecord {
                routine: forward(&record.routine, id),
                ..record
            },
        ),
        Ok(None) => Ok(()),
        Err(_) => {
            let from = revision_path(legacy_dir, n);
            std::fs::create_dir_all(to.parent().unwrap_or(dir))
                .and_then(|()| std::fs::copy(&from, &to).map(|_| ()))
                .map_err(|source| DefsError::Io { path: to, source })
        }
    }
}

/// The value at `path` (one key, or `parent.key`) in a mapping.
fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |at, key| at.get(*key))
}

/// Set (or with `None`, remove) the value at `path` in `target`, creating the
/// parent mapping when needed.
fn set_path(target: &mut Value, path: &[&str], value: Option<Value>) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut at = target;
    for key in parents {
        let Value::Mapping(map) = at else { return };
        at = map
            .entry(Value::from(*key))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
    }
    let Value::Mapping(map) = at else { return };
    match value {
        Some(value) => {
            map.insert(Value::from(*last), value);
        }
        None => {
            map.remove(*last);
        }
    }
}

/// Every field path that differs between two legacy contents: each top-level
/// key, and each key of a nested mapping (`where`, `policy`) on its own, so
/// two clients' edits to different policy fields both survive.
fn changed_paths(base: &Value, current: &Value) -> Vec<Vec<String>> {
    let keys = |v: &Value| -> Vec<String> {
        v.as_mapping()
            .map(|m| m.keys().map(key_text).collect())
            .unwrap_or_default()
    };
    let mut top: Vec<String> = keys(base);
    top.extend(keys(current));
    top.sort();
    top.dedup();
    let mut out = Vec::new();
    for key in top {
        if NEVER_APPLIED.contains(&key.as_str()) {
            continue;
        }
        let (b, c) = (base.get(key.as_str()), current.get(key.as_str()));
        if b == c {
            continue;
        }
        match (b, c) {
            (Some(Value::Mapping(_)), Some(Value::Mapping(_))) => {
                let (b, c) = (b.unwrap(), c.unwrap());
                let mut inner = keys(b);
                inner.extend(keys(c));
                inner.sort();
                inner.dedup();
                for sub in inner {
                    if b.get(sub.as_str()) != c.get(sub.as_str()) {
                        out.push(vec![key.clone(), sub]);
                    }
                }
            }
            _ => out.push(vec![key]),
        }
    }
    out
}

/// Re-apply an older client's edit of the legacy file onto the migrated
/// routine at `dir`, as its next revision: only the fields that changed
/// between `base` (the legacy content last brought forward) and the file now
/// are applied; a missing `archived_at` never un-archives. A changed agent
/// moves the routine to that agent's dir. Returns where it lives now, or
/// `None` when the routine is gone from `dir` (the caller looks again next
/// pass) or the merged body does not read.
fn reapply(
    config_dir: &Path,
    key: &RoutineKey,
    legacy_dir: &Path,
    raw: &Value,
    dir: &Path,
    base: Option<&Value>,
) -> Result<Option<PathBuf>, DefsError> {
    let Some(current) = defs::read_def(dir)? else {
        log::warn!(target: "routines", "routine {key}: {} went away while re-applying a legacy edit", dir.display());
        return Ok(None);
    };
    let content = legacy_content(raw.clone());
    let Some(base) = base else {
        // No base (written by a build before it kept one): take the file as
        // the base from now on and apply nothing, rather than guess.
        write_base(dir, &content)?;
        return Ok(Some(dir.to_path_buf()));
    };
    let mut merged = match serde_yaml::to_value(&current) {
        Ok(merged) => merged,
        Err(e) => {
            log::warn!(target: "routines", "routine {key}: legacy edit not re-applied: {e}");
            return Ok(None);
        }
    };
    let mut applied = false;
    for path in changed_paths(base, &content) {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let value = get_path(&content, &path).cloned();
        if path == ["archived_at"] && value.as_ref().is_none_or(Value::is_null) {
            // An older client cannot un-archive: this is a stale copy.
            continue;
        }
        set_path(&mut merged, &path, value);
        applied = true;
    }
    let mut next: RoutineDef = match serde_yaml::from_value(merged) {
        Ok(next) => next,
        Err(e) => {
            log::warn!(target: "routines", "routine {key}: legacy edit not re-applied, it does not read: {e}");
            return Ok(None);
        }
    };
    let (author, at) = match defs::read_revision_raw(legacy_dir, legacy_revision(raw)) {
        Ok(Some(record)) => (record.author, record.at),
        // Every Mac re-applying the same edit writes the same bytes.
        _ => (
            Author::user(raw.get("owner").and_then(Value::as_str).unwrap_or_default()),
            FALLBACK_AT.to_string(),
        ),
    };
    let mut dir = dir.to_path_buf();
    if applied {
        if next.archived_at.is_some() && current.archived_at.is_none() {
            next.archived_with_agent = false;
        }
        next.id = current.id.clone();
        next.migrated = current.migrated.clone();
        next.revision = current.revision + 1;
        let target = layout::routine_dir_in(
            config_dir,
            &layout::home_for_ref(key, &next.agent),
            &current.id,
        );
        if target != dir && layout::home_of_dir(&dir) != Some(Home::Legacy) {
            if target.exists() {
                log::warn!(target: "routines", "routine {key}: legacy agent edit not followed, {} is taken", target.display());
                next.agent = current.agent.clone();
            } else {
                move_dir(&dir, &target)?;
                dir = target;
            }
        }
        defs::write_revision(&dir, &next, &author, &at)?;
    }
    write_base(&dir, &content)?;
    Ok(Some(dir))
}

fn legacy_revision(raw: &Value) -> u32 {
    raw.get("revision")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Mirror what an older client fires by (`armed_host`, `archived_at`,
/// `runs_on`, `agent`) and where the routine lives now to the tombstone of
/// the migrated routine at `dir`, and move its base along, so an older Mac
/// stops firing a routine moved off it or archived, and the mirror never
/// reads as an older client's edit. A no-op for a routine that was never
/// migrated or whose tombstone is gone. Takes the legacy lock, then the
/// routine's, as the migrator does; callers hold neither.
pub fn mirror_to_tombstone(dir: &Path) {
    let Some(config_dir) = layout::config_dir_of(dir) else {
        return;
    };
    if layout::home_of_dir(dir) == Some(Home::Legacy) {
        return;
    }
    let mirrored = defs::with_routine_lock(&layout::legacy_routines_dir(&config_dir), || {
        mirror_locked(&config_dir, dir)
    });
    if let Err(e) = mirrored {
        log::warn!(target: "routines", "{}: not mirrored to its legacy file: {e}", dir.display());
    }
}

/// Move a routine dir, revisions and all, to `to` (same filesystem: one
/// rename). A routine already at `to` is a clash the caller refused earlier.
pub fn move_dir(from: &Path, to: &Path) -> Result<(), DefsError> {
    let io = |source| DefsError::Io {
        path: to.to_path_buf(),
        source,
    };
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::rename(from, to).map_err(io)?;
    // Leave no empty `routines/` (or agent dir holding nothing else) behind.
    for empty in from.ancestors().skip(1).take(2) {
        if std::fs::remove_dir(empty).is_err() {
            break;
        }
    }
    Ok(())
}

/// [`mirror_to_tombstone`] for a caller that already holds the legacy lock.
fn mirror_locked(config_dir: &Path, dir: &Path) -> Result<(), DefsError> {
    defs::with_routine_lock(dir, || -> Result<(), DefsError> {
        let Some(def) = defs::read_def(dir)? else {
            return Ok(());
        };
        let Some(migrated) = &def.migrated else {
            return Ok(());
        };
        let Some(legacy_dir) = legacy_dir_of(config_dir, &migrated.from) else {
            return Ok(());
        };
        let Some(mut raw) = read_value(&defs::def_path(&legacy_dir))? else {
            return Ok(());
        };
        let agent = def.agent.to_string();
        for field in MIRRORED {
            let value = match field {
                // SW-AGR-8: armed on a machine, an older client fires it on
                // no Mac (it cannot know the machine fires it).
                "armed_host" => Some(Value::from(if def.armed_machine().is_some() {
                    String::new()
                } else {
                    def.armed_host.clone()
                })),
                "archived_at" => def.archived_at.clone().map(Value::from),
                "runs_on" => (!def.runs_on.is_empty()).then(|| Value::from(def.runs_on.clone())),
                _ => (!agent.is_empty()).then(|| Value::from(agent.clone())),
            };
            set_path(&mut raw, &[field], value);
        }
        set_path(
            &mut raw,
            &[MOVED_TO],
            Some(Value::from(rel(config_dir, dir))),
        );
        let content = legacy_content(raw.clone());
        if read_base(dir)?.as_ref() == Some(&content)
            && read_value(&defs::def_path(&legacy_dir))?.as_ref() == Some(&raw)
        {
            return Ok(());
        }
        write_value(&defs::def_path(&legacy_dir), &raw)?;
        write_base(dir, &content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_to_only_ever_names_a_dir_under_config_agents() {
        let config = Path::new("/h/acme/api/backend/config");
        let tombstone = |to: &str| -> Value {
            serde_yaml::from_str(&format!("id: t\nmoved_to: {to}\n")).unwrap()
        };
        assert_eq!(
            moved_to_dir(config, &tombstone("agents/barb/routines/t")),
            Some(config.join("agents/barb/routines/t"))
        );
        for hostile in [
            "../../../etc/routines/t",
            "/etc/passwd",
            "agents/../../x",
            "routines/t",
            "content/agents/t",
        ] {
            assert_eq!(moved_to_dir(config, &tombstone(hostile)), None, "{hostile}");
        }
    }

    #[test]
    fn only_changed_fields_are_applied_one_policy_key_at_a_time() {
        let base: Value = serde_yaml::from_str(
            "task: a\npolicy:\n  overlap: skip\n  visible: false\nrevision: 2\n",
        )
        .unwrap();
        let current: Value = serde_yaml::from_str(
            "task: a\npolicy:\n  overlap: queue\n  visible: false\nrevision: 3\nmoved_to: x\n",
        )
        .unwrap();
        assert_eq!(
            changed_paths(&base, &current),
            vec![vec!["policy".to_string(), "overlap".to_string()]],
            "revision and moved_to are never taken from a legacy edit"
        );
    }
}
