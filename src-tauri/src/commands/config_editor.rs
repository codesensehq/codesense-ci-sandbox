// SPDX-License-Identifier: MIT
//! SW-GT-6: the team config **write** commands — the FE consumer surface over the two
//! per-team config loaders shipped by SW-GT-1 (`status-taxonomy.yml`) and SW-GT-18
//! (`field-schema.yml`). The *read* commands live next to
//! their loaders; this module owns the editor-specific commands:
//!
//! - [`save_team_config`] — persist an edited config to the team content layer through
//!   the ONE generic writer ([`write_team_config_file`], over the existing
//!   [`write_edit_to_workdir`] seam). One command with a per-file arm rather than
//!   near-identical commands; each arm serializes its file's on-disk shape (the taxonomy's
//!   `states`, the schema's *custom* fields only — official ones are never re-enumerated).
//! - [`can_declare_team_field`] / [`declare_team_field`] — declare one typed custom field
//!   from the metadata drawer.
//!
//! **No UI-thread work (AC-7):** every command runs its filesystem / serialization work on
//! the blocking pool via [`crate::commands::await_blocking`] — nothing touches the webview
//! thread, and the write returns before the file-watcher's sync wave materializes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::commands::field_schema::{
    canonical_field_type, custom_fields_from_yaml, is_official_field, load_field_schema_from,
    sanitize_allowed_values, FieldDef, FieldSchema, SCHEMA_FILENAME,
};
use crate::commands::pipeline_format::canonical_field_key;
use crate::commands::status_taxonomy::{StatusState, TAXONOMY_FILENAME};
use crate::context_repo::{
    path_to_team_content_dir, with_branch_lock, write_edit_to_workdir, AppliedEdit,
    TEAM_CONTENT_DIR,
};

// ----- AC-4 / AC-8: persist via the ONE generic writer ------------------------

/// An edited config to persist, tagged by which file it is. The FE sends the same shape
/// its matching `load_*` returned (minus the runtime `warnings`), so read and write are
/// symmetric. Each variant maps to exactly one on-disk file + serialization shape in
/// [`TeamConfigWrite::to_file`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamConfigWrite {
    /// `status-taxonomy.yml` — the ordered states (incl. the SW-GT-6 `delivered` flag).
    StatusTaxonomy { states: Vec<StatusState> },
    /// `field-schema.yml` — only the team's **custom** fields (official are never written).
    FieldSchema { fields: Vec<FieldDef> },
}

/// The taxonomy file's on-disk shape: a top-level `states:` list (no runtime `warnings`).
#[derive(Serialize)]
struct TaxonomyFileOut<'a> {
    states: &'a [StatusState],
}

/// The field-schema file's on-disk shape: a `fields:` list of the team's custom fields.
#[derive(Serialize)]
struct SchemaFileOut {
    fields: Vec<CustomFieldOut>,
}

/// One custom field as written to disk — the `RawField` shape (no derived `official`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CustomFieldOut {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<String>,
    /// SW-AU-77E: a `choice` field's allowed-value vocabulary. Omitted when empty so
    /// a non-`choice` field's YAML stays compact.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allowed_values: Vec<String>,
    required: bool,
}

impl TeamConfigWrite {
    /// Resolve this edit to its `(filename, YAML body)` — the per-file serialization the
    /// generic writer is agnostic to. Fails only if serialization does (never expected for
    /// these plain models).
    /// Returns the target subdir, the on-disk filename, and the serialized body. The taxonomy
    /// and field schema (authored *content*) live in `content/`; keeping the subdir in this
    /// per-kind match is the single place that owns the kind→location map.
    pub(crate) fn to_file(&self) -> Result<(&'static str, &'static str, String), String> {
        let (subdir, filename, yaml) = match self {
            Self::StatusTaxonomy { states } => (
                TEAM_CONTENT_DIR,
                TAXONOMY_FILENAME,
                serde_yaml::to_string(&TaxonomyFileOut { states }),
            ),
            Self::FieldSchema { fields } => {
                let custom = fields
                    .iter()
                    .filter(|f| !f.official)
                    .map(|f| CustomFieldOut {
                        name: f.name.clone(),
                        r#type: f.r#type.clone(),
                        allowed_values: f.allowed_values.clone(),
                        required: f.required,
                    })
                    .collect();
                (
                    TEAM_CONTENT_DIR,
                    SCHEMA_FILENAME,
                    serde_yaml::to_string(&SchemaFileOut { fields: custom }),
                )
            }
        };
        yaml.map(|body| (subdir, filename, body))
            .map_err(|e| format!("serialize {filename}: {e}"))
    }
}

/// The ONE generic team-config writer (AC-8): write `contents` to
/// `{org_root}/{team}/{subdir}/{filename}` through the existing [`write_edit_to_workdir`]
/// seam — same `BRANCH_LOCK` serialization + publish-set coupling every registry edit
/// gets, so the file-watcher picks it up and the sync wave carries it to the team (AC-4).
/// `subdir` is the caller's target (`content/` for [`TeamConfigWrite::to_file`]'s taxonomy +
/// schema, `config/` for control-plane files); the seam is path-agnostic about the subdir, so
/// both classes travel to teammates while only `content/` also projects.
/// Synchronous (the caller runs it off-thread); returns once the bytes are on disk (the
/// wave is fire-and-forget, not awaited).
pub(crate) fn write_team_config_file(
    org_root: &Path,
    team: &str,
    subdir: &str,
    filename: &str,
    contents: String,
) -> Result<(), String> {
    let rel_path = PathBuf::from(team).join(subdir).join(filename);
    let applied = AppliedEdit {
        files: vec![(rel_path, contents.into_bytes())],
        // A content-layer config edit — publishes on the team's tracking branch. The
        // titles/body are only used if the user later runs `/cs-publish`.
        commit_title: format!("Update {filename}"),
    };
    write_edit_to_workdir(org_root, &applied).map_err(|e| e.to_string())
}

/// Result of a serialized config mutation. `Unchanged` returns a value while deliberately skipping
/// the filesystem write, which is useful for idempotent or optimistic-concurrency operations.
pub(crate) enum RmwDecision<T> {
    Write(String, T),
    Unchanged(T),
}

/// SW-AU-25: the serialized, atomic **read-modify-write** of ONE team-config file — the
/// contention-safe core a structured config writer needs. Holds `BRANCH_LOCK` across the **entire** read → `mutate` → write so two concurrent
/// writers can't both read version N and last-writer-wins (the lost-update the plain
/// [`write_team_config_file`] path allowed, whose read leg sat outside the lock). The write lands
/// via temp-file + rename ([`crate::shared::fs_atomic::write_bytes_atomic`]) so a concurrent reader
/// sees the old or the new file whole, never a torn one.
///
/// `mutate` receives the file's current contents (`None` when absent) and returns an
/// [`RmwDecision`]: `Write(new_body, value)` persists `new_body` and hands `value` back;
/// `Unchanged(value)` skips the write. A `mutate` error aborts the write, leaving the file
/// untouched.
///
/// **Not** routed through [`write_edit_to_workdir`]: `BRANCH_LOCK` is a non-reentrant
/// `std::sync::Mutex`, so acquiring it here and then calling that (which re-acquires it) would
/// deadlock. This writes into the working tree directly — the same publish-set / file-watcher
/// coupling (bytes in the tree), minus the re-entrant lock. Path-based (no `AppHandle`) so the
/// contention guarantees are directly thread-testable.
///
/// `team`/`subdir`/`filename` are joined onto `org_root` verbatim, so a caller must pass
/// **validated** components, mirroring `write_team_config_file`. Do not hand this raw,
/// caller-supplied path segments.
pub(crate) fn rmw_team_config_file_with_decision<T>(
    org_root: &Path,
    team: &str,
    subdir: &str,
    filename: &str,
    mutate: impl FnOnce(Option<String>) -> Result<RmwDecision<T>, String>,
) -> Result<T, String> {
    let abs_path = org_root.join(team).join(subdir).join(filename);
    with_branch_lock(|| {
        let existing = read_config_file(&abs_path)?;
        match mutate(existing)? {
            RmwDecision::Write(body, value) => {
                crate::shared::fs_atomic::write_bytes_atomic(&abs_path, body.as_bytes())
                    .map_err(|e| format!("write {}: {e}", abs_path.display()))?;
                Ok(value)
            }
            RmwDecision::Unchanged(value) => Ok(value),
        }
    })
}

/// Read a config file's current contents, mapping an absent file to `None` (never an error).
fn read_config_file(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// SW-GT-6 (AC-4): persist an edited team config to the content layer. Off the webview
/// thread; the file-watcher → sync wave propagates it (no explicit publish call).
#[tauri::command]
pub async fn save_team_config(
    org_root: String,
    team: String,
    write: TeamConfigWrite,
) -> Result<(), String> {
    crate::commands::await_blocking("save_team_config", move || {
        // SW-AU-77E: gate every team-config write on the SAME writability guard the drawer
        // declare path uses — a config edit to a read-only projected/installed team tree is
        // refused at the backend, not just relied on the FE to withhold. Mirrors the
        // story-metadata write guard (`resolve_writable_target`).
        if let Some(reason) = team_config_write_block_reason(&org_root, &team) {
            return Err(reason);
        }
        let (subdir, filename, body) = write.to_file()?;
        write_team_config_file(Path::new(&org_root), &team, subdir, filename, body)?;
        Ok(())
    })
    .await
}

// ----- SW-AU-77E: declare a typed custom field from the metadata drawer --------

/// SW-AU-77E: the drawer's field input — a name, one of the five authoring types
/// (validated at the command boundary), and an optional `choice` vocabulary.
/// Serialized camelCase (`allowedValues`) to match the TS `FieldDef`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFieldInput {
    pub name: String,
    pub r#type: String,
    #[serde(default)]
    pub allowed_values: Vec<String>,
    #[serde(default)]
    pub required: bool,
    /// AC-3/AC-6: the value the drawer is about to set on the current story (declare +
    /// set in one action). Validated against the declared type here so an invalid initial
    /// value is refused BEFORE the frontmatter write, never a silent bad value. `None`
    /// when declaring without setting.
    #[serde(default)]
    pub value: Option<String>,
}

/// SW-AU-77E (AC-5): whether the current context may declare a team field, and — when
/// not — a truthful reason the drawer shows on the disabled action.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclareGate {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// AC-5 gate: writing a team's config (declaring a field, or a Settings taxonomy/
/// transitions/field-schema edit) requires a WRITABLE team-content tree (not a read-only
/// projected/installed view) — the SAME [`is_installed_path`] guard the story-metadata
/// write path uses. Ownership is TeamOwned by construction today (SW-AU-77C), so
/// writability is the effective gate. `Some(reason)` ⇒ blocked, `None` ⇒ allowed. Blocking
/// (canonicalize / home lookup) — call off-thread.
///
/// `pub(crate)` so the SW-AU-78 assignment write reuses the SAME writability authority (one
/// read-only-source signal for every team-config write), rather than duplicating the guard.
pub(crate) fn team_config_write_block_reason(org_root: &str, team: &str) -> Option<String> {
    let home =
        crate::commands::paths::codesense_home_dir().and_then(|h| std::fs::canonicalize(h).ok());
    let Some(home) = home else {
        return Some("codesense home is unresolved".to_string());
    };
    let content_dir = path_to_team_content_dir(Path::new(org_root), team);
    // Resolve symlinks where the dir exists (matches `resolve_writable_target`); fall
    // back to the lexical path so `is_installed_path`'s own containment proof still runs.
    let resolved = std::fs::canonicalize(&content_dir).unwrap_or(content_dir);
    let target = resolved.join(SCHEMA_FILENAME);
    if crate::content_engine::is_installed_path(&target, &home) {
        return Some(format!(
            "team '{team}' is a read-only projected/installed view, not a writable team workspace"
        ));
    }
    None
}

/// SW-AU-77E (AC-5): pre-flight gate the drawer reads to enable/disable "+ Add field".
#[tauri::command]
pub async fn can_declare_team_field(org_root: String, team: String) -> Result<DeclareGate, String> {
    crate::commands::await_blocking("can_declare_team_field", move || {
        Ok(match team_config_write_block_reason(&org_root, &team) {
            Some(reason) => DeclareGate {
                allowed: false,
                reason: Some(reason),
            },
            None => DeclareGate {
                allowed: true,
                reason: None,
            },
        })
    })
    .await
}

/// AC-7/AC-8: the pure merge of ONE field into the team's current custom set. Reconciles
/// onto an existing field by canonical name (identical ⇒ [`RmwDecision::Unchanged`], so no
/// needless write / fan-out wave; different ⇒ in-place edit); a new name appends. A
/// malformed existing body errors (never a silent clobber). Pure over the file body, so
/// the merge rule is unit-testable without the filesystem.
fn merge_declared_field(
    existing: Option<String>,
    new_def: &FieldDef,
) -> Result<RmwDecision<()>, String> {
    let mut customs = match existing {
        Some(body) => custom_fields_from_yaml(&body)?,
        None => Vec::new(),
    };
    match customs.iter().position(|f| f.name == new_def.name) {
        Some(i) if &customs[i] == new_def => return Ok(RmwDecision::Unchanged(())),
        Some(i) => customs[i] = new_def.clone(),
        None => customs.push(new_def.clone()),
    }
    let (_, _, body) = TeamConfigWrite::FieldSchema { fields: customs }.to_file()?;
    Ok(RmwDecision::Write(body, ()))
}

/// AC-8: persist one declared field through the serialized, atomic RMW seam — the
/// contention-safe write two concurrent declares can share without a lost update.
fn declare_field_rmw(org_root: &str, team: &str, new_def: FieldDef) -> Result<(), String> {
    rmw_team_config_file_with_decision(
        Path::new(org_root),
        team,
        TEAM_CONTENT_DIR,
        SCHEMA_FILENAME,
        |existing| merge_declared_field(existing, &new_def),
    )
}

/// SW-AU-77E: validate + normalize a drawer field input into the `FieldDef` to declare —
/// the pure, gate-free core of [`declare_team_field`]. Enforces the full ladder: a canonical
/// name that is a usable identifier, a known authoring type, a non-empty `choice` vocabulary,
/// and an initial value that matches the type. Pure over the input, so the ladder is
/// unit-testable without the filesystem or the writability gate.
fn build_declared_field(field: &NewFieldInput) -> Result<FieldDef, String> {
    let name = canonical_field_key(&field.name);
    if name.is_empty() {
        return Err("a field needs a name".to_string());
    }
    // Defense in depth (the FE validates too, but the backend never trusts the client): a
    // name that isn't a plain `[a-z0-9_]` identifier can't be referenced by a `{var}` or the
    // metadata key extractor, so refuse it rather than persist an unusable field.
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!(
            "'{}' must be letters, numbers, or underscores",
            field.name
        ));
    }
    if is_official_field(&name) {
        return Err(format!("'{name}' is a built-in field — pick another name"));
    }
    let kind = canonical_field_type(&field.r#type)
        .ok_or_else(|| format!("'{}' is not a valid field type", field.r#type))?;
    // Only a `choice` field carries a vocabulary — compute (and require) it just there.
    let allowed_values = if kind == "choice" {
        let vals = sanitize_allowed_values(field.allowed_values.clone());
        if vals.is_empty() {
            return Err("a choice field needs at least one allowed value".to_string());
        }
        vals
    } else {
        Vec::new()
    };
    // AC-3/AC-6: validate the initial value against the declared type BEFORE any write,
    // reusing the shared value validator (never persist an invalid value).
    if let Some(value) = field.value.as_deref() {
        if !crate::commands::frontmatter_validation::validate_custom_field_value(
            value,
            Some(kind),
            &allowed_values,
        ) {
            return Err(format!("'{value}' is not a valid {kind} value"));
        }
    }
    Ok(FieldDef {
        name,
        r#type: Some(kind.to_string()),
        allowed_values,
        required: field.required,
        official: false,
    })
}

/// SW-AU-77E: declare (or reconcile onto) ONE typed custom field in the team FieldSchema
/// from the metadata drawer. Additive + RMW-safe (AC-8) — reads the current
/// `field-schema.yml` and merges the one field under `BRANCH_LOCK`, never the full-vector
/// overwrite `save_team_config` does (Settings owns the full lifecycle; the drawer only
/// adds). Gated by writability (AC-5), name+type validated at the boundary (AC-1), and an
/// existing name reconciles rather than duplicating (AC-7). Returns the reloaded schema so
/// the FE re-reads the authoritative set. All work off the webview thread (AC-9).
#[tauri::command]
pub async fn declare_team_field(
    org_root: String,
    team: String,
    field: NewFieldInput,
) -> Result<FieldSchema, String> {
    crate::commands::await_blocking("declare_team_field", move || {
        if let Some(reason) = team_config_write_block_reason(&org_root, &team) {
            return Err(reason);
        }
        let new_def = build_declared_field(&field)?;
        declare_field_rmw(&org_root, &team, new_def)?;

        let content_dir = path_to_team_content_dir(Path::new(&org_root), &team);
        Ok(load_field_schema_from(&content_dir))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::status_taxonomy::{load_status_taxonomy_from, StatusRole};
    use crate::context_repo::CONFIG_DIR;
    use std::collections::BTreeMap;

    fn state(id: &str, role: StatusRole, delivered: Option<bool>) -> StatusState {
        StatusState {
            id: id.to_string(),
            label: id.to_string(),
            color: "#64748b".to_string(),
            icon: "Circle".to_string(),
            order: 0,
            role,
            delivered,
        }
    }

    // ----- write path --------------------------------------------------------

    #[test]
    fn write_team_config_file_writes_under_the_given_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        write_team_config_file(
            tmp.path(),
            "payments",
            CONFIG_DIR,
            "x.yml",
            "items: []\n".into(),
        )
        .unwrap();
        let written = tmp.path().join("payments").join(CONFIG_DIR).join("x.yml");
        assert_eq!(std::fs::read_to_string(written).unwrap(), "items: []\n");
    }

    // ----- SW-AU-25: serialized + atomic RMW ---------------------------------

    /// Read the current `items:` list from a config body, append `id`, re-serialize — a minimal
    /// structured upsert exercising the RMW's read-under-lock.
    fn append_item(existing: Option<String>, id: &str) -> Result<(String, ()), String> {
        let mut items: Vec<String> = match existing {
            None => Vec::new(),
            Some(raw) => serde_yaml::from_str::<BTreeMap<String, Vec<String>>>(&raw)
                .map_err(|e| e.to_string())?
                .remove("items")
                .unwrap_or_default(),
        };
        items.push(id.to_string());
        let mut map = BTreeMap::new();
        map.insert("items".to_string(), items);
        Ok((serde_yaml::to_string(&map).map_err(|e| e.to_string())?, ()))
    }

    /// The config file every RMW test writes to: `{tmp}/default/config/p.yml`.
    fn p_yml(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join("default").join("config").join("p.yml")
    }

    /// Run one RMW against that shared test drop-file, so the tests read as "RMW this file with
    /// `mutate`" rather than repeating the fixed team/subdir/filename triple.
    fn rmw_p<T>(
        tmp: &tempfile::TempDir,
        mutate: impl FnOnce(Option<String>) -> Result<(String, T), String>,
    ) -> Result<T, String> {
        rmw_team_config_file_with_decision(tmp.path(), "default", "config", "p.yml", |existing| {
            let (body, value) = mutate(existing)?;
            Ok(RmwDecision::Write(body, value))
        })
    }

    fn items_of(tmp: &tempfile::TempDir) -> Vec<String> {
        serde_yaml::from_str::<BTreeMap<String, Vec<String>>>(
            &std::fs::read_to_string(p_yml(tmp)).unwrap(),
        )
        .unwrap()
        .remove("items")
        .unwrap()
    }

    #[test]
    fn rmw_creates_then_extends_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        rmw_p(&tmp, |e| append_item(e, "a")).unwrap();
        rmw_p(&tmp, |e| append_item(e, "b")).unwrap();
        assert_eq!(items_of(&tmp), vec!["a", "b"]);
    }

    #[test]
    fn rmw_mutate_error_leaves_the_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        rmw_p(&tmp, |e| append_item(e, "a")).unwrap();
        let before = std::fs::read_to_string(p_yml(&tmp)).unwrap();

        let err = rmw_p::<()>(&tmp, |_| Err("boom".to_string())).unwrap_err();
        assert_eq!(err, "boom");
        assert_eq!(std::fs::read_to_string(p_yml(&tmp)).unwrap(), before);
    }

    #[test]
    fn rmw_unchanged_neither_creates_nor_rewrites_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let value = rmw_team_config_file_with_decision(
            tmp.path(),
            "default",
            "config",
            "p.yml",
            |existing| {
                assert!(existing.is_none());
                Ok(RmwDecision::Unchanged("absent"))
            },
        )
        .unwrap();
        assert_eq!(value, "absent");
        assert!(!p_yml(&tmp).exists(), "a no-op must not create the file");

        rmw_p(&tmp, |e| append_item(e, "keep")).unwrap();
        let before = std::fs::read(p_yml(&tmp)).unwrap();
        rmw_team_config_file_with_decision(tmp.path(), "default", "config", "p.yml", |existing| {
            assert!(existing.is_some());
            Ok(RmwDecision::Unchanged(()))
        })
        .unwrap();
        assert_eq!(std::fs::read(p_yml(&tmp)).unwrap(), before);
    }

    #[test]
    fn rmw_no_lost_update_under_concurrent_writers() {
        // AC-2: two writers RMW the SAME file concurrently, each reading then appending its own
        // id. With the read INSIDE the lock, both survive; a read outside the lock would lose one
        // (both read the empty file, last-writer-wins). A barrier forces genuine contention.
        use std::sync::{Arc, Barrier};
        let tmp = Arc::new(tempfile::tempdir().unwrap());
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|id| {
                let tmp = Arc::clone(&tmp);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    rmw_p(&tmp, |e| append_item(e, id)).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut items = items_of(&tmp);
        items.sort();
        assert_eq!(items, vec!["a", "b"], "a concurrent writer was lost");
    }

    #[test]
    fn rmw_reader_never_observes_a_torn_file() {
        // AC-1: while a writer loops the RMW (bytes change every write), a reader hammering the
        // same path must always parse a whole, valid document (old or new) — temp+rename never
        // exposes a truncated file. No lock on the reader side.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let tmp = Arc::new(tempfile::tempdir().unwrap());
        // Seed so the first read has a whole file.
        rmw_p(&tmp, |e| append_item(e, "seed")).unwrap();
        let path = p_yml(&tmp);

        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let tmp = Arc::clone(&tmp);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for i in 0..400 {
                    rmw_p(&tmp, |e| append_item(e, &format!("id-{i}"))).unwrap();
                }
                stop.store(true, Ordering::Relaxed);
            })
        };
        while !stop.load(Ordering::Relaxed) {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                assert!(
                    serde_yaml::from_str::<BTreeMap<String, Vec<String>>>(&raw).is_ok(),
                    "reader saw a torn/malformed file: {raw:?}"
                );
            }
        }
        writer.join().unwrap();
    }

    #[test]
    fn to_file_routes_content_configs_to_content() {
        // The taxonomy and field schema (authored content) live in `content/`. The
        // kind→location map lives entirely in `to_file`.
        let (subdir, filename, _) = TeamConfigWrite::StatusTaxonomy { states: vec![] }
            .to_file()
            .unwrap();
        assert_eq!(
            (subdir, filename),
            (TEAM_CONTENT_DIR, "status-taxonomy.yml")
        );

        let (subdir, filename, _) = TeamConfigWrite::FieldSchema { fields: vec![] }
            .to_file()
            .unwrap();
        assert_eq!((subdir, filename), (TEAM_CONTENT_DIR, "field-schema.yml"));
    }

    #[test]
    fn save_taxonomy_round_trips_and_preserves_delivered() {
        // AC-1/AC-4: an edited taxonomy (incl. a dropped terminal) writes to disk and
        // reloads value-preserving; an unmarked state omits the `delivered` key.
        let tmp = tempfile::tempdir().unwrap();
        let write = TeamConfigWrite::StatusTaxonomy {
            states: vec![
                state("todo", StatusRole::NotStarted, None),
                state("done", StatusRole::Done, None),
                state("wont-do", StatusRole::Done, Some(false)),
            ],
        };
        let (subdir, filename, body) = write.to_file().unwrap();
        assert_eq!(
            (subdir, filename),
            (TEAM_CONTENT_DIR, "status-taxonomy.yml")
        );
        assert!(
            !body.contains("warnings"),
            "runtime warnings are never written"
        );
        write_team_config_file(tmp.path(), "default", subdir, filename, body).unwrap();

        let content_dir = tmp.path().join("default").join("content");
        let reloaded = load_status_taxonomy_from(&content_dir);
        assert!(reloaded.warnings.is_empty());
        assert_eq!(reloaded.states.len(), 3);
        assert_eq!(
            reloaded
                .states
                .iter()
                .find(|s| s.id == "wont-do")
                .unwrap()
                .delivered,
            Some(false)
        );
        assert_eq!(
            reloaded
                .states
                .iter()
                .find(|s| s.id == "done")
                .unwrap()
                .delivered,
            None
        );
    }

    #[test]
    fn save_field_schema_writes_only_custom_fields() {
        // AC-6/AC-8: the official baseline is never re-enumerated on disk — only the
        // team's custom fields are written; reload restores official ∪ custom.
        let tmp = tempfile::tempdir().unwrap();
        let schema = crate::commands::field_schema::default_schema(); // official baseline
        let mut fields = schema.fields;
        fields.push(FieldDef {
            name: "risk_tier".to_string(),
            r#type: Some("string".to_string()),
            allowed_values: Vec::new(),
            required: true,
            official: false,
        });
        let write = TeamConfigWrite::FieldSchema { fields };
        let (subdir, filename, body) = write.to_file().unwrap();
        assert_eq!((subdir, filename), (TEAM_CONTENT_DIR, "field-schema.yml"));
        assert!(body.contains("risk_tier"));
        assert!(
            !body.contains("status"),
            "official fields are not written: {body}"
        );

        write_team_config_file(tmp.path(), "default", subdir, filename, body).unwrap();
        let content_dir = tmp.path().join("default").join("content");
        let reloaded = load_field_schema_from(&content_dir);
        assert!(reloaded.contains("status"), "official baseline restored");
        let risk = reloaded
            .fields
            .iter()
            .find(|f| f.name == "risk_tier")
            .unwrap();
        assert!(risk.required && !risk.official);
    }

    #[test]
    fn save_field_schema_round_trips_choice_allowed_values() {
        // SW-AU-77E: a `choice` field's type + allowed-value vocabulary survives
        // save→load; a non-choice field emits no `allowedValues` key.
        let tmp = tempfile::tempdir().unwrap();
        let mut fields = crate::commands::field_schema::default_schema().fields;
        fields.push(FieldDef {
            name: "risk_tier".to_string(),
            r#type: Some("choice".to_string()),
            allowed_values: vec!["low".to_string(), "high".to_string()],
            required: false,
            official: false,
        });
        fields.push(FieldDef {
            name: "reviewer".to_string(),
            r#type: Some("text".to_string()),
            allowed_values: Vec::new(),
            required: false,
            official: false,
        });
        let write = TeamConfigWrite::FieldSchema { fields };
        let (subdir, filename, body) = write.to_file().unwrap();
        assert!(
            body.contains("allowedValues"),
            "choice vocab written: {body}"
        );

        write_team_config_file(tmp.path(), "default", subdir, filename, body).unwrap();
        let content_dir = tmp.path().join("default").join("content");
        let reloaded = load_field_schema_from(&content_dir);
        let risk = reloaded
            .fields
            .iter()
            .find(|f| f.name == "risk_tier")
            .unwrap();
        assert_eq!(risk.r#type.as_deref(), Some("choice"));
        assert_eq!(
            risk.allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
        let reviewer = reloaded
            .fields
            .iter()
            .find(|f| f.name == "reviewer")
            .unwrap();
        assert!(reviewer.allowed_values.is_empty(), "non-choice: no vocab");
    }

    // ===== SW-AU-77E: declare a typed custom field ===========================

    fn declared(name: &str) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            r#type: Some("text".to_string()),
            allowed_values: Vec::new(),
            required: false,
            official: false,
        }
    }

    #[test]
    fn merge_declared_field_appends_edits_and_dedups() {
        // AC-7: new name → Write; identical re-declare → Unchanged (no needless write);
        // a changed type → Write (in-place edit); a malformed existing body → Err.
        let write = merge_declared_field(None, &declared("risk")).unwrap();
        let RmwDecision::Write(body, ()) = write else {
            panic!("new field should write");
        };
        assert!(body.contains("risk"));

        assert!(
            matches!(
                merge_declared_field(Some(body.clone()), &declared("risk")).unwrap(),
                RmwDecision::Unchanged(())
            ),
            "identical re-declare is idempotent"
        );

        let mut edited = declared("risk");
        edited.r#type = Some("number".to_string());
        assert!(matches!(
            merge_declared_field(Some(body.clone()), &edited).unwrap(),
            RmwDecision::Write(_, ())
        ));

        assert!(
            merge_declared_field(Some("fields: : bad\n  - [".to_string()), &declared("x")).is_err(),
            "malformed existing body must not clobber"
        );
    }

    #[test]
    fn concurrent_declares_of_distinct_fields_all_persist() {
        // AC-8: eight threads declare distinct fields through the RMW seam — the
        // BRANCH_LOCK read-modify-write means none is lost to a stale-snapshot overwrite.
        let tmp = tempfile::tempdir().unwrap();
        let org = tmp.path().to_str().unwrap().to_string();
        std::thread::scope(|s| {
            for i in 0..8 {
                let org = org.clone();
                s.spawn(move || {
                    declare_field_rmw(&org, "default", declared(&format!("field_{i}"))).unwrap();
                });
            }
        });
        let content_dir = Path::new(&org).join("default").join("content");
        let schema = load_field_schema_from(&content_dir);
        for i in 0..8 {
            assert!(
                schema.contains(&format!("field_{i}")),
                "field_{i} survived concurrent declares"
            );
        }
    }

    fn new_field(name: &str, ty: &str) -> NewFieldInput {
        NewFieldInput {
            name: name.to_string(),
            r#type: ty.to_string(),
            allowed_values: Vec::new(),
            required: false,
            value: None,
        }
    }

    #[test]
    fn build_declared_field_enforces_the_full_ladder() {
        // AC-1: a plain text field builds cleanly.
        let ok = build_declared_field(&new_field("risk", "text")).unwrap();
        assert_eq!(ok.name, "risk");
        assert_eq!(ok.r#type.as_deref(), Some("text"));

        // Empty / non-identifier names are refused (backend never trusts the client).
        assert!(build_declared_field(&new_field("", "text")).is_err());
        assert!(
            build_declared_field(&new_field("risk tier", "text")).is_err(),
            "space"
        );
        assert!(
            build_declared_field(&new_field("risk-tier", "text")).is_err(),
            "hyphen"
        );

        // AC-1: an official name can't be shadowed; an unknown type is refused.
        assert!(build_declared_field(&new_field("status", "text")).is_err());
        assert!(build_declared_field(&new_field("x", "select")).is_err());

        // AC-1: a choice field needs a non-empty vocabulary (deduped/trimmed).
        assert!(build_declared_field(&new_field("tier", "choice")).is_err());
        let mut choice = new_field("tier", "choice");
        choice.allowed_values = vec!["low".into(), "high".into(), "low".into()];
        assert_eq!(
            build_declared_field(&choice).unwrap().allowed_values,
            vec!["low".to_string(), "high".to_string()],
            "vocabulary deduped, order preserved"
        );

        // AC-3/AC-6: the initial value is validated against the declared type before any
        // write — a bad number/choice value is refused; a matching value builds.
        let mut bad_num = new_field("count", "number");
        bad_num.value = Some("ten".into());
        assert!(build_declared_field(&bad_num).is_err());
        let mut bad_choice = new_field("tier", "choice");
        bad_choice.allowed_values = vec!["low".into()];
        bad_choice.value = Some("mid".into());
        assert!(build_declared_field(&bad_choice).is_err());
        let mut good_choice = new_field("tier", "choice");
        good_choice.allowed_values = vec!["low".into()];
        good_choice.value = Some("low".into());
        assert!(build_declared_field(&good_choice).is_ok());
    }

    #[test]
    fn team_config_write_is_blocked_on_a_non_writable_tree() {
        // AC-5: a bare tempdir is not a writable team-content tree under the codesense
        // home → every team-config write (declare AND Settings save) is refused with a
        // truthful reason, never a silent no-op.
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            team_config_write_block_reason(tmp.path().to_str().unwrap(), "default").is_some(),
            "non-writable tree must block"
        );
    }
}
