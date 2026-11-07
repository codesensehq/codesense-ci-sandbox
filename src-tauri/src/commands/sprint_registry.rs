// SPDX-License-Identifier: MIT
//! SW-SB-2 / SW-SB-24: per-team **sprint definition** registry.
//!
//! SW-SB-24 relocated the registry out of the human content tree. It is now a
//! single app-managed YAML ledger at `{org_root}/{team}/config/sprints.yaml` — a
//! `sprints:` map keyed by sprint `id` — instead of one `sprints/<id>.md` file per
//! sprint under `content/` plus a generated `SPRINTS.md` index. The ledger rides
//! the whole-repo git-sync to teammates (so the durable closed-sprint velocity
//! survives) but is invisible to the content scanner/browser (`config/` is not a
//! projection layer marker). See `context_repo::path_to_team_config_dir`.
//!
//! This module owns the read side of the ledger: [`load_sprint_registry_from`].
//! There is no per-sprint file and no generated index. The **wire shape is frozen** — [`SprintRegistry`] /
//! [`SprintDef`] (camelCase `focusEpics`/`deliveredPoints`/`carriedPoints`) are
//! unchanged, so every read consumer (SB-3 selector, SB-13 velocity, the nav
//! filter, SE-2's `focus_epics` fence) needs no change.
//!
//! Tolerance mirrors [`crate::install::team_pins::load_team_pins`]: a
//! missing ledger is the common greenfield case (empty registry, no active), and
//! a malformed file — or a single malformed entry within it — is logged and
//! skipped rather than erroring, so a hand-corrupted ledger can never wedge the
//! board (AC-3).

use std::path::Path;

use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::commands::pipeline_scanner::{
    default_summary_headings, scan_pipeline_tree_impl_with_cache, DerivedPipelineTree,
    MetadataValidityConfig, StoryRef,
};
use crate::context_repo::path_to_team_config_dir;

/// The single app-managed ledger filename under `{org_root}/{team}/config/`.
const SPRINTS_LEDGER_FILENAME: &str = "sprints.yaml";

/// The top-level YAML key holding the id→entry map. Wrapping the map under a
/// named key (rather than a bare top-level map) leaves room for future
/// ledger-level keys (a schema version, team-wide sprint defaults) without a
/// breaking migration.
const LEDGER_SPRINTS_KEY: &str = "sprints";

/// Sprint lifecycle state. An absent or unrecognized value fails
/// deserialization, so the containing entry is skipped as malformed (AC-3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SprintState {
    Planning,
    Active,
    Closed,
}

/// One sprint's on-disk **ledger entry** — every [`SprintDef`] field EXCEPT `id`,
/// which is the map key (never duplicated in the value). Field names are
/// snake_case on disk (`focus_epics`, `delivered_points`) to match the
/// `people.yml`/`teams.yml` team-config family and the prior per-file frontmatter,
/// and reuse the exact lenient field deserializers the per-file loader used.
/// `state` is the only structurally required field; the rest default, so a
/// minimal entry still parses. Empty/zero fields are omitted on write to keep the
/// git-tracked ledger tidy — they default back on read (loss-free round-trip).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SprintEntry {
    state: SprintState,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    goal: String,
    /// `deserialize_with` drops null / blank / non-string list entries (a stray
    /// `- ` or `- ~` would otherwise coerce to a phantom `"~"` epic).
    #[serde(
        default,
        deserialize_with = "de_focus_epics",
        skip_serializing_if = "Vec::is_empty"
    )]
    focus_epics: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    start: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    end: String,
    /// SW-SB-11 AC-5 durable velocity. `de_lenient_u32` accepts a bare int OR a
    /// numeric string, so a value written as `"5"` (older per-file `setField`
    /// quoting) still round-trips instead of skipping the entry as malformed.
    #[serde(
        default,
        deserialize_with = "de_lenient_u32",
        skip_serializing_if = "is_zero"
    )]
    delivered_points: u32,
    #[serde(
        default,
        deserialize_with = "de_lenient_u32",
        skip_serializing_if = "is_zero"
    )]
    carried_points: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Deserialize a `u32` leniently: accept a bare YAML integer (`5`) OR a quoted
/// numeric string (`"5"`). Anything else (a float, null, list, non-numeric or
/// out-of-range string) degrades to `0` rather than failing the whole entry —
/// the same tolerant posture as `de_lenient_points` in `pipeline_format`.
fn de_lenient_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_yaml::Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|v| match v {
            serde_yaml::Value::Number(n) => n.as_u64(),
            serde_yaml::Value::String(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        })
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0))
}

/// Deserialize a YAML sequence into epic-id strings, keeping ONLY genuine
/// (non-blank) string entries. Everything else — a null (`~`), a bare number
/// (`- 123`), a bool (`- true`), a nested map/seq — is dropped rather than
/// coerced, so a stray or mistyped list item can never surface as a phantom epic
/// id downstream. A non-sequence value (e.g. `focus_epics: foo`) still errors
/// here, so that whole entry is skipped as malformed — an ambiguous authoring
/// mistake stays loud rather than silently loading an empty focus list.
fn de_focus_epics<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_yaml::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| match v {
            serde_yaml::Value::String(s) => Some(s),
            _ => None,
        })
        .filter(|s| !s.trim().is_empty())
        .collect())
}

/// One sprint's definition — the **frozen wire shape** serialized camelCase for
/// the FE. `id` + all descriptive/velocity fields in one flat record (the ledger
/// stores `id` as the map key and re-attaches it here on load). This is what
/// every read consumer already binds to; SW-SB-24 must not change it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SprintDef {
    pub id: String,
    pub state: SprintState,
    #[serde(default)]
    pub goal: String,
    /// Serializes camelCase (`focusEpics`) for the FE; on disk it is the ledger
    /// entry's snake_case `focus_epics`. The alias also accepts the camelCase form
    /// so a `SprintDef` round-trips through the `save_sprint_registry` command
    /// payload (which arrives camelCase from the FE) losslessly.
    #[serde(default, alias = "focus_epics", deserialize_with = "de_focus_epics")]
    pub focus_epics: Vec<String>,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
    /// SW-SB-11 AC-5 durable velocity (delivered = points on `done`-role members;
    /// carried = points rolled over), stamped by the close action. Lenient parse
    /// so the camelCase FE payload (`deliveredPoints`) and any legacy quoted form
    /// both survive.
    #[serde(
        default,
        alias = "delivered_points",
        deserialize_with = "de_lenient_u32"
    )]
    pub delivered_points: u32,
    #[serde(default, alias = "carried_points", deserialize_with = "de_lenient_u32")]
    pub carried_points: u32,
}

impl SprintDef {
    /// Re-attach the ledger map key as `id` to build the wire record.
    fn from_entry(id: String, e: SprintEntry) -> Self {
        SprintDef {
            id,
            state: e.state,
            goal: e.goal,
            focus_epics: e.focus_epics,
            start: e.start,
            end: e.end,
            delivered_points: e.delivered_points,
            carried_points: e.carried_points,
        }
    }
}

/// The resolved read-side registry for one team, serialized camelCase for the FE.
/// Carries both the ordered list and the resolved active id in a single envelope
/// so SB-3 can render its selector + empty/drift states without a second call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SprintRegistry {
    /// The resolved active sprint id, or `None` when the team has zero `active`
    /// sprints — a legitimate planning→active gap that SB-3's empty state relies
    /// on (AC-2). Never an error.
    pub active_sprint: Option<String>,
    /// All parsed sprints, ordered by `start` ascending (tiebreak: `id`
    /// ascending) for a deterministic list.
    pub sprints: Vec<SprintDef>,
    /// Soft, non-fatal advisories surfaced to the FE: multi-active drift (AC-2)
    /// and skipped-malformed-entry notices (AC-3). Empty on the happy path.
    pub warnings: Vec<String>,
}

/// Load + resolve the sprint registry from a team `config/` dir (reads
/// `config_dir/sprints.yaml`). Pure over the filesystem (no app/Tauri state) so
/// it is directly unit-testable; the command wrapper resolves `config_dir` via
/// [`path_to_team_config_dir`] and delegates here. Never returns an error — the
/// tolerant posture is encoded in the return shape (empty registry + warnings).
pub fn load_sprint_registry_from(config_dir: &Path) -> SprintRegistry {
    load_sprint_registry_from_for(config_dir, &Default::default())
}

pub(crate) fn load_sprint_registry_from_for(
    config_dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> SprintRegistry {
    let path = config_dir.join(SPRINTS_LEDGER_FILENAME);
    if !scope.allows(&path) {
        return SprintRegistry::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                target: "sprint_registry",
                "no sprint ledger at {} — empty registry", path.display()
            );
            return SprintRegistry::default();
        }
        Err(e) => {
            // File exists but is unreadable (e.g. permissions) — unlike the
            // NotFound greenfield case, surface a soft warning to the FE (not
            // just the log) so an empty board isn't silently indistinguishable
            // from "no sprints yet".
            warn!(
                target: "sprint_registry",
                "cannot read {}: {e} — empty registry", path.display()
            );
            return SprintRegistry {
                warnings: vec![format!("could not read sprint ledger: {e}")],
                ..SprintRegistry::default()
            };
        }
    };

    // A brand-new / empty ledger file is greenfield, not malformed.
    if text.trim().is_empty() {
        return SprintRegistry::default();
    }

    let doc: serde_yaml::Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                target: "sprint_registry",
                "malformed sprint ledger {}: {e} — empty registry", path.display()
            );
            return SprintRegistry {
                warnings: vec![format!("malformed sprint ledger: {e}")],
                ..SprintRegistry::default()
            };
        }
    };

    let mut sprints: Vec<SprintDef> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    match doc.get(LEDGER_SPRINTS_KEY) {
        Some(serde_yaml::Value::Mapping(map)) => {
            // Sort by id first so both the parsed list and any skip warnings are
            // deterministic regardless of the mapping's iteration order.
            let mut items: Vec<(String, serde_yaml::Value)> = map
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|s| (s.to_string(), v.clone())))
                .collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            for (id, val) in items {
                match serde_yaml::from_value::<SprintEntry>(val) {
                    Ok(entry) => sprints.push(SprintDef::from_entry(id, entry)),
                    Err(e) => {
                        // One broken entry never wedges the registry (AC-3).
                        warn!(
                            target: "sprint_registry",
                            "skipping malformed sprint entry '{id}': {e}"
                        );
                        warnings.push(format!("skipped malformed sprint entry: {id}"));
                    }
                }
            }
        }
        // Present but wrong shape (e.g. `sprints: []` or a scalar) — warn, don't wedge.
        Some(_) => {
            warnings.push(format!(
                "ledger `{LEDGER_SPRINTS_KEY}` is not a mapping — ignored"
            ));
        }
        // Absent `sprints:` key → an empty (but valid) ledger.
        None => {}
    }

    // Deterministic list order: by `start` (ISO dates sort chronologically as
    // strings), tiebreak by `id`.
    sprints.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
    let active_sprint = resolve_active(&sprints, &mut warnings);

    SprintRegistry {
        active_sprint,
        sprints,
        warnings,
    }
}

/// AC-6 derived read result: a sprint's definition plus the stories currently
/// tagged to it. Serialized camelCase for both the MCP tool and the HTTP route,
/// which share the [`sprint_member_stories`] producer.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SprintStories {
    /// The sprint definition, or `None` for an unknown id (with empty members).
    pub sprint: Option<SprintDef>,
    /// Stories whose `sprint:` tag equals the id — **derived, never stored**
    /// (AC-5). The ledger holds no membership; this is a live query over the
    /// scanned content tree.
    pub stories: Vec<StoryRef>,
}

/// AC-6 — the sprint-stories producer behind the `GET /api/v1/sprints/:id/stories`
/// route. Given a team's `config_dir` (the
/// ledger home) and `content_dir` (the scan root) and a sprint `id`, returns the
/// sprint def (from the ledger) plus its member stories, derived by filtering the
/// existing pipeline scan on `story.sprint == id`. Both dirs are passed in
/// explicitly — the caller resolves them from the authority it holds
/// (`path_to_team_config_dir`/`path_to_team_content_dir` for the MCP tool;
/// `team_config_dir_beside_content` for the HTTP route, which holds only a
/// content root) — so this producer does no path math. No new scanner; membership
/// is never persisted (AC-5). Unknown id → `sprint: None` with whatever stories
/// happen to carry that tag (normally empty) — never an error.
///
/// SR-18: `cache` warms the membership scan off the app-managed
/// `SharedFrontmatterCache` — each runtime caller snapshots via
/// `pipeline_scanner::frontmatter_cache_snapshot` and passes it in, so drilling a
/// sprint's members re-parses only changed files instead of cold-scanning the
/// whole tree. `None` (tests, or an unregistered cache) falls back to inline parse.
pub fn sprint_member_stories(
    config_dir: &Path,
    content_dir: &Path,
    id: &str,
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
) -> SprintStories {
    sprint_member_stories_for(config_dir, content_dir, id, cache, &Default::default())
}

pub(crate) fn sprint_member_stories_for(
    config_dir: &Path,
    content_dir: &Path,
    id: &str,
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> SprintStories {
    let sprint = load_sprint_registry_from_for(config_dir, scope)
        .sprints
        .into_iter()
        .find(|s| s.id == id);

    // Scan the whole content root (empty pipeline root + scope) and keep the
    // stories tagged to `id`. A scan failure degrades to no members (the def
    // still returns) rather than erroring — same tolerant posture as the loader.
    let scan = super::pipeline_scanner::scan_pipeline_tree_for(content_dir, cache, scope);
    let stories = match scan {
        Ok(tree) => collect_sprint_members(&tree, id),
        Err(e) => {
            warn!(target: "sprint_registry", "sprint_member_stories scan failed: {e}");
            Vec::new()
        }
    };

    SprintStories { sprint, stories }
}

/// Recursively collect stories tagged to `id` from a scanned pipeline tree,
/// preserving the scan's natural order (parents before nested sub-epics).
fn collect_sprint_members(tree: &DerivedPipelineTree, id: &str) -> Vec<StoryRef> {
    let mut out: Vec<StoryRef> = tree
        .stories
        .iter()
        .filter(|s| s.sprint.as_deref() == Some(id))
        .cloned()
        .collect();
    for sub in &tree.sub_epics {
        out.extend(collect_sprint_members(sub, id));
    }
    out
}

/// Resolve the single active sprint id from an already-loaded list. Zero active →
/// `None` (no error). Exactly one → that id. Multiple `active` (a drift state) →
/// resolve to the one with the latest `start` (tiebreak: higher `id`) and record
/// a soft warning — the board still works, but only one sprint should be active
/// per team (AC-2).
fn resolve_active(sprints: &[SprintDef], warnings: &mut Vec<String>) -> Option<String> {
    let actives: Vec<&SprintDef> = sprints
        .iter()
        .filter(|s| s.state == SprintState::Active)
        .collect();
    match actives.as_slice() {
        [] => None,
        [only] => Some(only.id.clone()),
        many => {
            let winner = many
                .iter()
                .max_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)))
                .expect("non-empty by match arm");
            warnings.push(format!(
                "{} sprints are marked active; resolved to '{}' (latest start). \
                 Only one sprint should be active per team.",
                many.len(),
                winner.id
            ));
            Some(winner.id.clone())
        }
    }
}

/// SW-SB-2 read seam: load the active team's sprint registry (definitions +
/// resolved active + soft warnings) from `{org_root}/{team}/config/sprints.yaml`.
/// The FE already holds `orgRoot` + `team`; SB-3 calls this to default + populate
/// its sprint selector. Filesystem work runs on the blocking pool.
#[tauri::command]
pub async fn load_sprint_registry(
    org_root: String,
    team: String,
) -> Result<SprintRegistry, String> {
    crate::commands::await_blocking("load_sprint_registry", move || {
        let config_dir = path_to_team_config_dir(Path::new(&org_root), &team);
        Ok(load_sprint_registry_from(&config_dir))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the AC-1 scan-invisibility test lays out a full `{team}/content` +
    // `{team}/config` tree; the module code itself keys off `path_to_team_config_dir`.
    use crate::context_repo::{CONFIG_DIR, TEAM_CONTENT_DIR};

    use std::collections::BTreeMap;

    /// The typed on-disk ledger, for writing fixtures (deterministic key order).
    #[derive(Debug, Serialize)]
    struct SprintLedgerWrite {
        sprints: BTreeMap<String, SprintEntry>,
    }

    /// Write `sprints` as the team's ledger (`config_dir/sprints.yaml`) in the on-disk
    /// shape the loader reads.
    fn save_sprint_registry_to(config_dir: &Path, sprints: &[SprintDef]) -> Result<(), String> {
        let map: BTreeMap<String, SprintEntry> = sprints
            .iter()
            .map(|d| {
                (
                    d.id.clone(),
                    SprintEntry {
                        state: d.state,
                        goal: d.goal.clone(),
                        focus_epics: d.focus_epics.clone(),
                        start: d.start.clone(),
                        end: d.end.clone(),
                        delivered_points: d.delivered_points,
                        carried_points: d.carried_points,
                    },
                )
            })
            .collect();
        crate::shared::yaml_io::write_yaml_atomic(
            &config_dir.join(SPRINTS_LEDGER_FILENAME),
            &SprintLedgerWrite { sprints: map },
        )
        .map_err(|e| e.to_string())
    }

    /// Write a raw ledger file into `{config_dir}/sprints.yaml`.
    fn write_ledger(config_dir: &Path, body: &str) {
        std::fs::create_dir_all(config_dir).unwrap();
        std::fs::write(config_dir.join(SPRINTS_LEDGER_FILENAME), body).unwrap();
    }

    fn def(id: &str, state: SprintState) -> SprintDef {
        SprintDef {
            id: id.to_string(),
            state,
            goal: String::new(),
            focus_epics: Vec::new(),
            start: String::new(),
            end: String::new(),
            delivered_points: 0,
            carried_points: 0,
        }
    }

    #[test]
    fn unknown_ledger_keys_are_ignored() {
        // A sprint entry carrying a key this build no longer reads (e.g. a retired
        // `run_config:` block) still loads; the key is dropped on the next save.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n  2026-08-B:\n    state: active\n    run_config:\n      parallelism: 4\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg.sprints.len(), 1);
        assert_eq!(reg.sprints[0].id, "2026-08-B");
        save_sprint_registry_to(tmp.path(), &reg.sprints).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join(SPRINTS_LEDGER_FILENAME)).unwrap();
        assert!(!raw.contains("run_config"));
    }

    #[test]
    fn missing_ledger_is_empty_registry_no_error() {
        // AC-3: greenfield — no config/sprints.yaml at all.
        let tmp = tempfile::tempdir().unwrap();
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg, SprintRegistry::default());
        assert!(reg.active_sprint.is_none());
        assert!(reg.sprints.is_empty());
        assert!(reg.warnings.is_empty());
    }

    #[test]
    fn empty_ledger_file_is_empty_registry_no_error() {
        // A freshly-created (empty) ledger is greenfield, not malformed.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(tmp.path(), "   \n");
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg, SprintRegistry::default());
    }

    #[test]
    fn happy_path_parses_fields_and_orders_by_start() {
        // AC-2: parse the full field set from the id-keyed map; ordered list.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 2026-08-B:\n\
             \x20   state: planning\n\
             \x20   goal: \"Second sprint\"\n\
             \x20   focus_epics:\n\
             \x20     - 16-sprint-management\n\
             \x20   start: \"2026-08-22\"\n\
             \x20   end: \"2026-09-05\"\n\
             \x20 2026-08-A:\n\
             \x20   state: active\n\
             \x20   goal: \"First sprint\"\n\
             \x20   focus_epics:\n\
             \x20     - 14-agentic-dashboards\n\
             \x20     - 16-sprint-management\n\
             \x20   start: \"2026-08-08\"\n\
             \x20   end: \"2026-08-22\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());

        assert_eq!(reg.warnings, Vec::<String>::new());
        // Ordered by start ascending → A (08-08) before B (08-22).
        assert_eq!(
            reg.sprints
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-08-A", "2026-08-B"]
        );
        let a = &reg.sprints[0];
        assert_eq!(a.state, SprintState::Active);
        assert_eq!(a.goal, "First sprint");
        assert_eq!(
            a.focus_epics,
            vec!["14-agentic-dashboards", "16-sprint-management"]
        );
        assert_eq!(a.start, "2026-08-08");
        assert_eq!(a.end, "2026-08-22");
        assert_eq!(reg.active_sprint.as_deref(), Some("2026-08-A"));
    }

    #[test]
    fn sb11_velocity_parses_and_defaults_to_zero() {
        // SW-SB-11 AC-5: velocity persists in the ledger; absent → 0.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 closed:\n\
             \x20   state: closed\n\
             \x20   delivered_points: 21\n\
             \x20   carried_points: 8\n\
             \x20 open:\n\
             \x20   state: planning\n",
        );
        let reg = load_sprint_registry_from(tmp.path());

        let closed = reg.sprints.iter().find(|s| s.id == "closed").unwrap();
        assert_eq!(closed.delivered_points, 21);
        assert_eq!(closed.carried_points, 8);
        let open = reg.sprints.iter().find(|s| s.id == "open").unwrap();
        assert_eq!(open.delivered_points, 0);
        assert_eq!(open.carried_points, 0);
    }

    #[test]
    fn velocity_survives_a_quoted_string_value() {
        // Legacy per-file `setField` quoted integer-looking scalars
        // (`delivered_points: "21"`). The lenient parse must accept that so a
        // migrated closed sprint's velocity does not silently drop.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 closed:\n\
             \x20   state: closed\n\
             \x20   delivered_points: \"21\"\n\
             \x20   carried_points: \"0\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        let closed = reg.sprints.iter().find(|s| s.id == "closed").unwrap();
        assert_eq!(closed.delivered_points, 21);
        assert_eq!(closed.carried_points, 0);
    }

    #[test]
    fn zero_active_resolves_to_null_no_error() {
        // AC-2: a planning→active gap yields active_sprint = None, no warning.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n  2026-08-A:\n    state: planning\n    start: \"2026-08-08\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert!(reg.active_sprint.is_none());
        assert_eq!(reg.sprints.len(), 1);
        assert!(reg.warnings.is_empty());
    }

    #[test]
    fn multiple_active_resolves_latest_start_with_drift_warning() {
        // AC-2: two active → latest start wins, soft drift warning present.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 2026-08-A:\n    state: active\n    start: \"2026-08-08\"\n\
             \x20 2026-08-B:\n    state: active\n    start: \"2026-08-22\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg.active_sprint.as_deref(), Some("2026-08-B"));
        assert_eq!(reg.warnings.len(), 1);
        assert!(reg.warnings[0].contains("2 sprints are marked active"));
        assert!(reg.warnings[0].contains("2026-08-B"));
    }

    #[test]
    fn multiple_active_same_start_tiebreaks_on_higher_id() {
        // AC-2 tiebreak: equal start → higher id wins.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 2026-08-A:\n    state: active\n    start: \"2026-08-08\"\n\
             \x20 2026-08-C:\n    state: active\n    start: \"2026-08-08\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg.active_sprint.as_deref(), Some("2026-08-C"));
    }

    #[test]
    fn malformed_entry_is_skipped_with_warning_rest_still_load() {
        // AC-3: a single broken entry never wedges the registry.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 2026-08-A:\n    state: active\n    start: \"2026-08-08\"\n\
             \x20 2026-08-Z:\n    goal: \"no state\"\n",
        );
        let reg = load_sprint_registry_from(tmp.path());

        assert_eq!(
            reg.sprints
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2026-08-A"]
        );
        assert_eq!(reg.active_sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(reg.warnings.len(), 1);
        assert!(reg.warnings[0].contains("skipped malformed sprint entry: 2026-08-Z"));
    }

    #[test]
    fn malformed_ledger_file_is_empty_registry_with_warning() {
        // AC-3: a hand-corrupted (unparseable) ledger warns, never errors.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n  - this is not: a: valid: map\n :::\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert!(reg.sprints.is_empty());
        assert!(reg.active_sprint.is_none());
        assert_eq!(reg.warnings.len(), 1);
        assert!(reg.warnings[0].contains("malformed sprint ledger"));
    }

    #[test]
    fn junk_focus_epics_entries_are_dropped_keeping_only_string_slugs() {
        // null (`~`), blank, bare number, and bool list items must not surface as
        // phantom epic ids. Only genuine non-blank strings survive.
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sprints:\n\
             \x20 2026-08-A:\n\
             \x20   state: active\n\
             \x20   focus_epics:\n\
             \x20     - 14-agentic-dashboards\n\
             \x20     - ~\n\
             \x20     - \"\"\n\
             \x20     - 123\n\
             \x20     - true\n\
             \x20     - 16-sprint-management\n",
        );
        let reg = load_sprint_registry_from(tmp.path());
        assert_eq!(reg.sprints.len(), 1);
        assert_eq!(
            reg.sprints[0].focus_epics,
            vec!["14-agentic-dashboards", "16-sprint-management"]
        );
    }

    #[test]
    fn state_serializes_lowercase_for_fe() {
        // The FE selector matches on lowercase state strings.
        let json = serde_json::to_string(&SprintState::Active).unwrap();
        assert_eq!(json, "\"active\"");
    }

    // --------------------------- save / round-trip ---------------------------

    #[test]
    fn save_then_load_round_trips_loss_free() {
        // AC-2: save → load returns an identical set of defs (incl. velocity).
        let tmp = tempfile::tempdir().unwrap();
        let mut a = def("2026-08-A", SprintState::Closed);
        a.goal = "First".into();
        a.focus_epics = vec!["16-sprint-management".into()];
        a.start = "2026-08-08".into();
        a.end = "2026-08-22".into();
        a.delivered_points = 21;
        a.carried_points = 3;
        let mut b = def("2026-08-B", SprintState::Active);
        b.start = "2026-08-22".into();

        save_sprint_registry_to(tmp.path(), &[a.clone(), b.clone()]).unwrap();
        let reg = load_sprint_registry_from(tmp.path());

        assert_eq!(reg.sprints, vec![a, b]); // ordered by start; both survive
        assert_eq!(reg.active_sprint.as_deref(), Some("2026-08-B"));
        assert!(reg.warnings.is_empty());
    }

    // --------------------------- AC-6 member stories -------------------------

    /// Write a story file with `sprint:` frontmatter directly under `content_dir`.
    /// Carries `source_repo` so the story is index-eligible — without it the
    /// frontmatter cache orphan-skips the file (PTS-2), which would make a
    /// warm-cache fixture silently empty.
    fn write_story(content_dir: &Path, name: &str, sprint: Option<&str>) {
        std::fs::create_dir_all(content_dir).unwrap();
        let sprint_line = sprint.map(|s| format!("sprint: {s}\n")).unwrap_or_default();
        let body = format!(
            "---\ntype: story\nstatus: todo\nsource_repo: alderco/codesense\n{sprint_line}---\n\n# {name}\n"
        );
        std::fs::write(content_dir.join(name), body).unwrap();
    }

    #[test]
    fn member_stories_returns_def_plus_only_the_tagged_stories() {
        // AC-6: def from the ledger + exactly the stories tagged to the id;
        // other-sprint and untagged stories excluded. Membership stays derived.
        let tmp = tempfile::tempdir().unwrap();
        let content_dir = tmp.path().join("content");
        let config_dir = tmp.path().join("config"); // sibling of content/
        std::fs::create_dir_all(&content_dir).unwrap();
        save_sprint_registry_to(
            &config_dir,
            &[
                def("2026-08-A", SprintState::Active),
                def("2026-08-B", SprintState::Planning),
            ],
        )
        .unwrap();
        write_story(&content_dir, "s1.md", Some("2026-08-A"));
        write_story(&content_dir, "s2.md", Some("2026-08-A"));
        write_story(&content_dir, "s3.md", Some("2026-08-B"));
        write_story(&content_dir, "s4.md", None);

        let result = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", None);
        assert_eq!(
            result.sprint.as_ref().map(|s| s.id.as_str()),
            Some("2026-08-A")
        );
        let mut names: Vec<&str> = result
            .stories
            .iter()
            .map(|s| s.file_name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["s1.md", "s2.md"]);
    }

    #[test]
    fn member_stories_unknown_id_is_empty_not_an_error() {
        // AC-6: unknown id → sprint None + empty members, never an error.
        let tmp = tempfile::tempdir().unwrap();
        let content_dir = tmp.path().join("content");
        let config_dir = tmp.path().join("config"); // no ledger written → empty
        std::fs::create_dir_all(&content_dir).unwrap();
        write_story(&content_dir, "s1.md", Some("2026-08-A"));
        let result = sprint_member_stories(&config_dir, &content_dir, "does-not-exist", None);
        assert!(result.sprint.is_none());
        assert!(result.stories.is_empty());
    }

    #[test]
    fn member_stories_warm_cache_matches_cold_scan() {
        // SR-18: a warm frontmatter snapshot must yield the exact same membership
        // as the cold (None) scan — warming is a perf swap, never a behavior change.
        use crate::install::frontmatter_cache::FrontmatterCache;

        let tmp = tempfile::tempdir().unwrap();
        let content_dir = tmp.path().join("content");
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&content_dir).unwrap();
        save_sprint_registry_to(&config_dir, &[def("2026-08-A", SprintState::Active)]).unwrap();
        write_story(&content_dir, "s1.md", Some("2026-08-A"));
        write_story(&content_dir, "s2.md", Some("2026-08-A"));
        write_story(&content_dir, "s3.md", Some("2026-08-B"));
        write_story(&content_dir, "s4.md", None);

        // Build a cache warmed over every story (the on-disk truth), the same way
        // the app-managed cache would hold them at scan entry.
        let mut cache = FrontmatterCache::new();
        for name in ["s1.md", "s2.md", "s3.md", "s4.md"] {
            let path = content_dir.join(name);
            let entry = FrontmatterCache::prepare_refresh(&path, "default");
            cache.commit_refresh(&path, entry);
        }
        // Guard against a silently-empty warm fixture: if a story were orphan-skipped
        // (e.g. missing `source_repo`), the cache would hold nothing, every lookup
        // would miss, and `warm == cold` would pass without ever exercising a cache
        // hit. Assert the entries are actually present so the warm arm is real.
        assert_eq!(cache.entries.len(), 4, "all four stories must be cached");

        let cold = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", None);
        let warm = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", Some(&cache));

        assert_eq!(warm, cold, "warm scan must match cold scan exactly");
        let mut names: Vec<&str> = warm.stories.iter().map(|s| s.file_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["s1.md", "s2.md"]);
    }

    #[test]
    fn member_stories_reflect_disk_truth_when_the_cache_is_stale() {
        // SR-18 read-only-staleness caveat: the producer must never serve a stale
        // cached membership. Cache a story tagged sprint A, then rewrite it on disk
        // to sprint B and force the entry stale (mtime 0). The warm scan re-parses
        // from disk (SW-FX-9), so the story resolves as a B-member, not an A-member.
        use crate::install::frontmatter_cache::FrontmatterCache;

        let tmp = tempfile::tempdir().unwrap();
        let content_dir = tmp.path().join("content");
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&content_dir).unwrap();
        write_story(&content_dir, "s1.md", Some("2026-08-A"));

        let path = content_dir.join("s1.md");
        let mut cache = FrontmatterCache::new();
        let entry = FrontmatterCache::prepare_refresh(&path, "default");
        cache.commit_refresh(&path, entry);

        // Disk moves the story to sprint B; the cached entry (still sprint A) is
        // forced stale so the freshness gate must re-parse it.
        write_story(&content_dir, "s1.md", Some("2026-08-B"));
        cache.entries.get_mut(&path).expect("entry present").mtime = 0;

        let members_a = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", Some(&cache));
        let members_b = sprint_member_stories(&config_dir, &content_dir, "2026-08-B", Some(&cache));
        assert!(
            members_a.stories.is_empty(),
            "a stale cache must not leak the old sprint-A membership"
        );
        let names_b: Vec<&str> = members_b
            .stories
            .iter()
            .map(|s| s.file_name.as_str())
            .collect();
        assert_eq!(
            names_b,
            vec!["s1.md"],
            "re-parsed disk truth places s1 in sprint B"
        );
    }

    #[test]
    fn config_ledger_is_invisible_to_the_content_scan() {
        // AC-1 behavioral invisibility guard: a team dir with a `config/sprints.yaml`
        // sibling of `content/` must NOT surface the ledger through the content
        // scan (the scanner roots at `content/`). This locks in the whole point of
        // relocating out of `content/` — if a future change taught the scanner to
        // walk a sibling/parent, this fails.
        let tmp = tempfile::tempdir().unwrap();
        let team_dir = tmp.path().join("default");
        let content_dir = team_dir.join(TEAM_CONTENT_DIR);
        std::fs::create_dir_all(&content_dir).unwrap();
        write_story(&content_dir, "real-story.md", Some("2026-08-A"));
        // The ledger lives in the sibling config dir.
        save_sprint_registry_to(
            &team_dir.join(CONFIG_DIR),
            &[def("2026-08-A", SprintState::Active)],
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl_with_cache(
            &content_dir,
            "",
            "",
            "README.md",
            &default_summary_headings(),
            None,
            &MetadataValidityConfig::default(),
        )
        .unwrap();

        // Only the real content story is scanned; nothing named sprints.yaml or
        // from config/ appears as a story or doc anywhere in the tree.
        let seen = tree_file_names(&tree);
        assert!(
            seen.iter().all(|n| n != SPRINTS_LEDGER_FILENAME),
            "the config ledger must never surface in the content scan, got: {seen:?}"
        );
        assert!(seen.contains(&"real-story.md".to_string()));
    }

    /// Flatten every story + doc file name across the derived tree — the shared
    /// assertion helper for the `config/`-invisibility scan guards.
    fn tree_file_names(tree: &DerivedPipelineTree) -> Vec<String> {
        fn walk(tree: &DerivedPipelineTree, out: &mut Vec<String>) {
            out.extend(tree.stories.iter().map(|s| s.file_name.clone()));
            out.extend(tree.docs.iter().map(|d| d.file_name.clone()));
            for sub in &tree.sub_epics {
                walk(sub, out);
            }
        }
        let mut out = Vec::new();
        walk(tree, &mut out);
        out
    }
}
