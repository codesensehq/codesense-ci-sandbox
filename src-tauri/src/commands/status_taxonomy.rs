// SPDX-License-Identifier: MIT
//! SW-GT-1: per-team **status taxonomy** source-of-truth + loader.
//!
//! Reads `{org_root}/{team}/content/status-taxonomy.yml` — the git-synced,
//! materialized-per-team source of truth for a team's development states — and
//! resolves it into an ordered, role-keyed [`StatusTaxonomy`]. This is the
//! foundation the SE-6 epic keys off: code branches on a state's fixed [`StatusRole`]
//! while teams customize the labels / colors / order / count of the states carrying
//! each role. Consumers migrate onto this loader in GT-2 (`validate_story`),
//! GT-3 (FE switchers + board), and GT-4 (dashboards + rollups).
//!
//! Tolerance mirrors [`crate::commands::sprint_registry`] and
//! [`crate::install::team_pins::load_team_pins`]: the loader is **total** —
//! it never returns an error and never writes. A **missing** file is the common
//! zero-config case (the shipped defaults are the in-memory fallback → nothing
//! changes until a team customizes, silently). A malformed / unreadable /
//! degenerate file falls back to the same defaults and surfaces a single soft-drift
//! `warnings` entry, so a broken edit can never wedge the board (AC-5, AC-7).
//!
//! The default taxonomy is derived from [`VALID_STATUSES`] (the one canonical id
//! list) with metadata attached in [`default_meta`], so no second hand-maintained
//! status literal is introduced (AC-3). The existing Rust↔TS `PIPELINE_STATUSES`
//! drift test stays as the cross-language guard until the FE migrates in GT-3.

use std::path::Path;

use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::commands::pipeline_scanner::VALID_STATUSES;
use crate::context_repo::path_to_team_content_dir;

/// The per-team taxonomy file under a team's `content/` dir (sibling of
/// the team pins file / `sprints/`).
pub(crate) const TAXONOMY_FILENAME: &str = "status-taxonomy.yml";

/// Neutral fallback color for a custom state that omits `color` (AC-7). Slate-500,
/// the same neutral the default `planning` state uses.
const NEUTRAL_COLOR: &str = "#64748b";

/// The fixed 5-member role set. Code keys off the **role**; teams customize the
/// states that carry each role. Serialized snake_case (`not_started`, …) — the
/// on-disk `role:` value and the FE-facing string are the same token.
///
/// An absent **or unknown** `role` resolves to [`StatusRole::Active`] (AC-7): a
/// custom state with no declared role — or a typo'd one — is treated as
/// work-in-progress rather than dropped. Absence is silent; an unknown token
/// soft-drifts (see [`resolve_role`]) so a single typo defaults that one field
/// instead of discarding the team's whole taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StatusRole {
    NotStarted,
    #[default]
    Active,
    Review,
    Done,
    Blocked,
}

impl StatusRole {
    /// Parse a snake_case role token (the on-disk / wire form) into a role, or
    /// `None` if it isn't one of the fixed five. The token set mirrors the
    /// `rename_all = "snake_case"` serde form above; `role_token_set_matches_serde`
    /// guards the two against drift.
    fn from_token(token: &str) -> Option<Self> {
        match token {
            "not_started" => Some(Self::NotStarted),
            "active" => Some(Self::Active),
            "review" => Some(Self::Review),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// One resolved state in a team's taxonomy. `order` is concrete after load (missing
/// values are assigned append-after-max in [`normalize`]), so consumers never see a
/// gap. Serialize + Deserialize both derived so GT-6's editor round-trips this exact
/// type back to disk (no forked write model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusState {
    pub id: String,
    pub label: String,
    pub color: String,
    /// The state's icon, as a Lucide icon **name** (e.g. `CircleDashed`) — codesense
    /// standardizes on Lucide. The Rust side treats it as an opaque string; the FE
    /// (GT-3) resolves the name to a component and falls back for an unknown name.
    /// Never empty after load (absent → the state's role icon).
    pub icon: String,
    pub order: u32,
    pub role: StatusRole,
    /// SW-GT-6: the one manual per-state semantic marker (the design decision's lone
    /// exception flag). `Some(false)` marks a terminal state as *dropped / won't-do* so
    /// velocity excludes it (counted work is `role: done AND delivered != false`); absent
    /// or `Some(true)` is an ordinary state. `Option` + `skip_serializing_if` so an
    /// unmarked state never writes the key — the round-trip stays clean and the FE reads
    /// `delivered !== false` (absent ⇒ delivered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<bool>,
}

/// The resolved per-team taxonomy envelope, mirroring
/// [`crate::commands::sprint_registry::SprintRegistry`]: the ordered states plus any
/// soft-drift `warnings`, so a consumer renders the board and its drift banner from a
/// single call. `states` is ordered by `order` ascending (stable — declaration order
/// breaks `order` ties, AC-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusTaxonomy {
    pub states: Vec<StatusState>,
    pub warnings: Vec<String>,
}

// The role-resolution read API (AC-6/AC-8). GT-1 is foundation-only: these are the
// seams GT-2 (`validate_story`), GT-3 (board/switchers), and GT-4 (rollups) key off,
// so they have no in-crate caller yet and are exercised only by this module's tests.
// `allow(dead_code)` until the first consumer lands, rather than deferring the
// contract — canonical-per-role is defined here by design.
#[allow(dead_code)]
impl StatusTaxonomy {
    /// The role a given state id carries, or `None` if the id isn't in the taxonomy
    /// (the caller decides whether an unknown id is blocking — GT-2 — or soft drift —
    /// GT-3).
    pub fn role_of(&self, id: &str) -> Option<StatusRole> {
        self.states.iter().find(|s| s.id == id).map(|s| s.role)
    }

    /// Every state carrying `role`, in taxonomy order.
    pub fn states_for_role(&self, role: StatusRole) -> impl Iterator<Item = &StatusState> {
        self.states.iter().filter(move |s| s.role == role)
    }

    /// The **canonical** state for a role — the single state a role-keyed write
    /// resolves to (GT-3 "advance into the active column", GT-4 rollups): the
    /// lowest-`order` state carrying that role. `None` when no state carries the
    /// role (a degenerate taxonomy the GT-6 editor guards against; the loader never
    /// panics on it — AC-6).
    pub fn canonical_for_role(&self, role: StatusRole) -> Option<&StatusState> {
        self.states
            .iter()
            .filter(|s| s.role == role)
            .min_by_key(|s| s.order)
    }

    /// The state ids in taxonomy order.
    pub fn ids(&self) -> Vec<&str> {
        self.states.iter().map(|s| s.id.as_str()).collect()
    }
}

/// The on-disk file shape: a top-level `states:` list, so the format stays
/// extensible (future top-level keys) without breaking existing files.
#[derive(Debug, Default, Deserialize)]
struct TaxonomyFile {
    #[serde(default)]
    states: Vec<RawState>,
}

/// One state as authored on disk — every field past `id` is optional so a partial
/// entry degrades to defaults rather than failing the whole parse (AC-7). Unknown
/// keys are ignored (no `deny_unknown_fields`) so the format can widen without
/// breaking older files. `order` is `Option` so "absent" is distinguishable from an
/// explicit `0` before [`normalize`] assigns append-after-max. `role` is a raw
/// `Option<serde_yaml::Value>` (not `StatusRole`) so ANY wrong role — an unknown
/// token, a non-string scalar, or even a list/map — defaults that one field in
/// [`resolve_role`] instead of failing the whole-file deserialize.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawState {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    order: Option<u32>,
    #[serde(default)]
    role: Option<serde_yaml::Value>,
    /// SW-GT-6 dropped flag. Absent ⇒ `None` (ordinary state); an explicit `false` marks
    /// the state dropped. Carried through [`normalize`] untouched (no per-field recovery
    /// — a non-bool value fails the tolerant top-level parse like any other type error).
    #[serde(default)]
    delivered: Option<bool>,
}

/// Resolve an authored `role` value into a [`StatusRole`], tolerating anything.
/// Absent / null / blank → the default (`active`) silently; a wrong scalar (unknown
/// token, bool, number) or a non-scalar (list/map) → `active` **plus** a soft-drift
/// naming what was authored — so one bad role defaults that field instead of
/// discarding the whole taxonomy (per-field tolerance, AC-7). This is why `role` is
/// parsed as a raw `Value`: a `StatusRole`/`String` field would fail the whole-file
/// deserialize on a non-string role.
fn resolve_role(
    raw: Option<&serde_yaml::Value>,
    id: &str,
    warnings: &mut Vec<String>,
) -> StatusRole {
    use serde_yaml::Value;
    let token = match raw {
        None | Some(Value::Null) => return StatusRole::default(),
        Some(Value::String(s)) => s.trim().to_string(),
        // A non-string scalar — stringify so the drift notice names it.
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        // Structurally not a role token (sequence / mapping / tagged).
        Some(_) => {
            warnings.push(format!(
                "status taxonomy: state '{id}' has a non-scalar role — defaulting to active"
            ));
            return StatusRole::default();
        }
    };
    if token.is_empty() {
        return StatusRole::default(); // blank string → absent-like, silent
    }
    StatusRole::from_token(&token).unwrap_or_else(|| {
        warnings.push(format!(
            "status taxonomy: state '{id}' has unknown role '{token}' — defaulting to active"
        ));
        StatusRole::default()
    })
}

/// The shipped default taxonomy — the in-memory fallback that delivers zero-config
/// parity (AC-4). Derived from [`VALID_STATUSES`] (the one id source) with per-id
/// metadata from [`default_meta`]; `order` is the id's index, so the default ids in
/// order match `PIPELINE_STATUSES` byte-for-byte.
pub fn default_taxonomy() -> StatusTaxonomy {
    let states = VALID_STATUSES
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let (label, color, icon, role) = default_meta(id);
            StatusState {
                id: id.to_string(),
                label,
                color,
                icon,
                order: i as u32,
                role,
                // Shipped defaults are all ordinary (delivered) states — none is a
                // won't-do terminal, so the key is omitted.
                delivered: None,
            }
        })
        .collect();
    StatusTaxonomy {
        states,
        warnings: Vec::new(),
    }
}

/// Shipped defaults carrying a single soft-drift `warning` — the fallback every
/// non-happy load path returns (unreadable / stateless / malformed file). Keeps the
/// "defaults are total, and say why" posture in one place.
fn defaults_with_warning(warning: String) -> StatusTaxonomy {
    let mut tax = default_taxonomy();
    tax.warnings.push(warning);
    tax
}

/// Default label / color / icon / role for a shipped state id. Values are lifted
/// verbatim from the FE `STATUS_CONFIG` (`src/shared/lib/pipeline-graph/status-config.ts`)
/// — including the Lucide icon **name** — so zero-config parity holds across the
/// Rust↔TS boundary until GT-3 migrates the FE. An id outside the shipped set (only
/// reachable if `VALID_STATUSES` grows without a matching arm) degrades gracefully to
/// a humanized label + neutral color + the `active` role's icon + `active` role; the
/// `default_meta_covers_all_valid_statuses` test guards against that gap.
fn default_meta(id: &str) -> (String, String, String, StatusRole) {
    use StatusRole::*;
    let (label, color, icon, role) = match id {
        "planning" => ("Planning", "#64748b", "CircleDashed", NotStarted),
        "todo" => ("Todo", "#94a3b8", "CircleDot", NotStarted),
        "in-progress" => ("In progress", "#0ea5e9", "Circle", Active),
        "blocked" => ("Blocked", "#ef4444", "Ban", Blocked),
        "review" => ("Review", "#8b5cf6", "Eye", Review),
        "qa" => ("QA", "#f59e0b", "ShieldCheck", Review),
        "done" => ("Done", "#10b981", "CheckCircle2", Done),
        other => {
            return (
                humanize(other),
                NEUTRAL_COLOR.to_string(),
                icon_for_role(Active).to_string(),
                Active,
            )
        }
    };
    (label.to_string(), color.to_string(), icon.to_string(), role)
}

/// The fallback icon (a Lucide name) for a state that omits `icon` — the icon of the
/// role's canonical default member, so a custom `done`-role state reads as done, a
/// `blocked`-role state as blocked, etc. Keeps custom states legible without forcing
/// teams to name an icon.
fn icon_for_role(role: StatusRole) -> &'static str {
    match role {
        StatusRole::NotStarted => "CircleDashed",
        StatusRole::Active => "Circle",
        StatusRole::Review => "Eye",
        StatusRole::Done => "CheckCircle2",
        StatusRole::Blocked => "Ban",
    }
}

/// Turn an id into a display label when none is authored: `in-progress` → `In
/// progress`. Splits on `-`/`_` and capitalizes only the first character (matching
/// the shipped `STATUS_CONFIG` label style, e.g. "In progress", not "In Progress").
fn humanize(id: &str) -> String {
    let spaced = id.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Resolve raw on-disk states into ordered [`StatusState`]s, collecting soft-drift
/// notices. Single pass in declaration order: drop empty-id and duplicate-id entries
/// (keep first, warn); fill missing `label`/`color`/`icon` with defaults (icon → the
/// role's default icon); then assign any missing `order` append-after-the-current-max
/// and stable-sort by `order` so duplicate orders keep declaration order (AC-7).
fn normalize(raw_states: Vec<RawState>, warnings: &mut Vec<String>) -> Vec<StatusState> {
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<RawState> = Vec::new();
    for mut rs in raw_states {
        let id = rs.id.trim().to_string();
        if id.is_empty() {
            warnings.push("status taxonomy: dropped a state with no id".to_string());
            continue;
        }
        if seen.iter().any(|s| s == &id) {
            warnings.push(format!(
                "status taxonomy: duplicate id '{id}' — keeping the first, ignoring the rest"
            ));
            continue;
        }
        seen.push(id.clone());
        rs.id = id;
        kept.push(rs);
    }

    // Append missing orders after the highest explicit order, in declaration order.
    let explicit_max = kept.iter().filter_map(|r| r.order).max();
    if explicit_max == Some(u32::MAX) && kept.iter().any(|r| r.order.is_none()) {
        // Can't place appended states strictly after u32::MAX — they'd tie. Flag it
        // rather than silently violating append-after-max (declaration order still
        // breaks the tie deterministically).
        warnings.push(
            "status taxonomy: an order at the u32 maximum prevents later states from \
             sorting after it — their order may tie"
                .to_string(),
        );
    }
    let mut next = explicit_max.map_or(0, |m| m.saturating_add(1));
    let mut states: Vec<StatusState> = kept
        .into_iter()
        .map(|r| {
            let order = r.order.unwrap_or_else(|| {
                let assigned = next;
                next = next.saturating_add(1);
                assigned
            });
            let label = if r.label.trim().is_empty() {
                humanize(&r.id)
            } else {
                r.label
            };
            let color = if r.color.trim().is_empty() {
                NEUTRAL_COLOR.to_string()
            } else {
                r.color
            };
            let role = resolve_role(r.role.as_ref(), &r.id, warnings);
            // Absent icon → the role's default icon (resolved after role, so a custom
            // state inherits a legible icon without naming one).
            let icon = if r.icon.trim().is_empty() {
                icon_for_role(role).to_string()
            } else {
                r.icon
            };
            StatusState {
                id: r.id,
                label,
                color,
                icon,
                order,
                role,
                // The one manual semantic marker — carried through verbatim (AC-1).
                delivered: r.delivered,
            }
        })
        .collect();

    // Stable sort → duplicate `order` values keep declaration order (AC-7).
    states.sort_by_key(|s| s.order);
    states
}

/// Load + resolve the status taxonomy rooted at a team `content/` dir. Pure over the
/// filesystem (no app/Tauri state) so it is directly unit-testable; the command
/// wrapper resolves `content_dir` via [`path_to_team_content_dir`] and delegates
/// here. **Never returns an error and never writes** — the tolerant posture is
/// encoded in the return shape (defaults + `warnings`).
pub fn load_status_taxonomy_from(content_dir: &Path) -> StatusTaxonomy {
    load_status_taxonomy_from_for(content_dir, &Default::default())
}

pub(crate) fn load_status_taxonomy_from_for(
    content_dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> StatusTaxonomy {
    let path = content_dir.join(TAXONOMY_FILENAME);
    if !scope.allows(&path) {
        return default_taxonomy();
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Zero-config parity: absent file → defaults, silently (AC-4).
            debug!(
                target: "status_taxonomy",
                "no taxonomy at {} — using shipped defaults", path.display()
            );
            return default_taxonomy();
        }
        Err(e) => {
            // Present but unreadable (e.g. permissions) → defaults + soft drift so an
            // empty-looking board isn't silently indistinguishable from the defaults.
            warn!(
                target: "status_taxonomy",
                "cannot read {}: {e} — using shipped defaults", path.display()
            );
            return defaults_with_warning(format!("could not read status taxonomy: {e}"));
        }
    };

    match serde_yaml::from_str::<TaxonomyFile>(&raw) {
        Ok(file) => {
            let mut warnings = Vec::new();
            let states = normalize(file.states, &mut warnings);
            if states.is_empty() {
                // A parseable but stateless file (`states: []`, or every entry
                // dropped) would leave every role empty — fall back to defaults so
                // the board always renders, and flag it.
                warn!(
                    target: "status_taxonomy",
                    "taxonomy at {} has no usable states — using shipped defaults", path.display()
                );
                let mut tax = defaults_with_warning(
                    "status taxonomy has no usable states — using defaults".to_string(),
                );
                // Carry the per-entry drop diagnostics (duplicate / empty id) so the
                // user learns *why* nothing was usable, not just that. (Role warnings
                // only attach to kept states, which can't reach this all-dropped path.)
                tax.warnings.extend(warnings);
                return tax;
            }
            StatusTaxonomy { states, warnings }
        }
        Err(e) => {
            // Unparseable → defaults + one soft-drift banner (AC-7).
            warn!(
                target: "status_taxonomy",
                "malformed {}: {e} — using shipped defaults", path.display()
            );
            defaults_with_warning(format!("malformed status taxonomy, using defaults: {e}"))
        }
    }
}

/// SW-GT-1 read seam (AC-8): load the team's status taxonomy from
/// `{org_root}/{team}/content/status-taxonomy.yml`. The single reachable read
/// surface GT-3/GT-4 consume (GT-2 calls [`load_status_taxonomy_from`] directly in
/// Rust). Filesystem work runs on the blocking pool, off the webview thread — the
/// FE reads this once into a store rather than per-render, so nothing here blocks the
/// UI.
#[tauri::command]
pub async fn load_status_taxonomy(
    org_root: String,
    team: String,
) -> Result<StatusTaxonomy, String> {
    crate::commands::await_blocking("load_status_taxonomy", move || {
        let content_dir = path_to_team_content_dir(Path::new(&org_root), &team);
        Ok(load_status_taxonomy_from(&content_dir))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_taxonomy(content_dir: &Path, body: &str) {
        std::fs::create_dir_all(content_dir).unwrap();
        std::fs::write(content_dir.join(TAXONOMY_FILENAME), body).unwrap();
    }

    #[test]
    fn absent_file_returns_defaults_with_no_warning() {
        // AC-4: zero-config parity — nothing on disk → shipped defaults, silently.
        let tmp = tempfile::tempdir().unwrap();
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax, default_taxonomy());
        assert!(tax.warnings.is_empty());
    }

    #[test]
    fn defaults_match_valid_statuses_in_order() {
        // AC-3: the default taxonomy is derived from the one id source, in order.
        let tax = default_taxonomy();
        assert_eq!(tax.ids(), VALID_STATUSES.to_vec());
        // Orders are the dense 0..n index.
        assert_eq!(
            tax.states.iter().map(|s| s.order).collect::<Vec<_>>(),
            (0..VALID_STATUSES.len() as u32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn default_meta_covers_all_valid_statuses() {
        // Guards the graceful-fallback arm in `default_meta`: every shipped id has
        // explicit metadata (non-empty label, a real hex color, a Lucide icon name,
        // a deliberate role).
        for &id in VALID_STATUSES {
            let (label, color, icon, _role) = default_meta(id);
            assert!(!label.is_empty(), "no default label for '{id}'");
            assert!(color.starts_with('#'), "no default color for '{id}'");
            assert!(!icon.is_empty(), "no default icon for '{id}'");
        }
    }

    #[test]
    fn default_icons_match_status_config_and_custom_icon_defaults_from_role() {
        // Zero-config parity: the 7 shipped states keep their exact Lucide icons.
        let tax = default_taxonomy();
        let icon_of = |id: &str| {
            tax.states
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.icon.as_str())
        };
        assert_eq!(icon_of("planning"), Some("CircleDashed"));
        assert_eq!(icon_of("qa"), Some("ShieldCheck"));
        assert_eq!(icon_of("done"), Some("CheckCircle2"));

        // A custom state: explicit icon is preserved verbatim; an omitted icon falls
        // back to the state's role icon (not a generic blank).
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: shipped\n    role: done\n    icon: Rocket\n\
             \x20 - id: parked\n    role: blocked\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        let shipped = tax.states.iter().find(|s| s.id == "shipped").unwrap();
        let parked = tax.states.iter().find(|s| s.id == "parked").unwrap();
        assert_eq!(shipped.icon, "Rocket", "explicit icon preserved");
        assert_eq!(parked.icon, "Ban", "absent icon → role (blocked) default");
    }

    #[test]
    fn default_roles_cover_all_five_and_canonical_is_lowest_order() {
        // AC-2 / AC-6: every role is populated and canonical = lowest-order member.
        let tax = default_taxonomy();
        for (role, canonical) in [
            (StatusRole::NotStarted, "planning"),
            (StatusRole::Active, "in-progress"),
            (StatusRole::Review, "review"),
            (StatusRole::Done, "done"),
            (StatusRole::Blocked, "blocked"),
        ] {
            assert_eq!(
                tax.canonical_for_role(role).map(|s| s.id.as_str()),
                Some(canonical),
                "canonical for {role:?}"
            );
        }
        // `qa` and `review` both carry the `review` role; canonical is `review`
        // (lower order).
        assert_eq!(
            tax.states_for_role(StatusRole::Review)
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["review", "qa"]
        );
        assert_eq!(tax.role_of("qa"), Some(StatusRole::Review));
        assert_eq!(tax.role_of("nope"), None);
    }

    #[test]
    fn custom_file_reflects_relabel_recolor_reorder_and_drop() {
        // AC-1/AC-2: a team's edits are honored verbatim; canonical tracks the new
        // lowest order.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: shipping\n    label: Shipping\n    color: \"#123456\"\n    order: 5\n    role: active\n\
             \x20 - id: idea\n    label: Idea\n    color: \"#abcdef\"\n    order: 1\n    role: not_started\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert!(tax.warnings.is_empty());
        // Sorted by order → idea (1) before shipping (5).
        assert_eq!(tax.ids(), vec!["idea", "shipping"]);
        assert_eq!(
            tax.canonical_for_role(StatusRole::Active).unwrap().id,
            "shipping"
        );
        assert_eq!(tax.states[0].label, "Idea");
        assert_eq!(tax.states[1].color, "#123456");
    }

    #[test]
    fn malformed_file_falls_back_to_defaults_with_one_warning() {
        // AC-7: unparseable YAML → defaults + exactly one soft-drift, never an error.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(tmp.path(), "states: : : not yaml at all\n  - [\n");
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.states, default_taxonomy().states);
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("malformed status taxonomy"));
    }

    #[test]
    fn empty_states_list_falls_back_to_defaults() {
        // A parseable-but-stateless file would empty every role — fall back + warn.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(tmp.path(), "states: []\n");
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.states, default_taxonomy().states);
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("no usable states"));
    }

    #[test]
    fn duplicate_id_keeps_first_and_warns() {
        // AC-7: duplicate id → keep the first declaration, drop the rest, one warning.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: todo\n    label: First\n    order: 0\n\
             \x20 - id: todo\n    label: Second\n    order: 1\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.states.len(), 1);
        assert_eq!(tax.states[0].label, "First");
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("duplicate id 'todo'"));
    }

    #[test]
    fn missing_optional_fields_take_defaults() {
        // AC-7: only `id` present → humanized label, neutral color, active role,
        // order appended after the max explicit order.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: anchored\n    order: 3\n\
             \x20 - id: in-review\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        let in_review = tax.states.iter().find(|s| s.id == "in-review").unwrap();
        assert_eq!(in_review.label, "In review");
        assert_eq!(in_review.color, NEUTRAL_COLOR);
        assert_eq!(in_review.role, StatusRole::Active);
        assert_eq!(in_review.order, 4, "append after max explicit order (3)");
    }

    #[test]
    fn empty_id_entry_is_dropped_with_warning() {
        // AC-7: a state with no id can't be keyed — drop it, keep the rest.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: \"\"\n    label: Ghost\n\
             \x20 - id: real\n    label: Real\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.ids(), vec!["real"]);
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("no id"));
    }

    #[test]
    fn duplicate_order_breaks_ties_on_declaration_order() {
        // AC-7: two states share order 0 → stable sort keeps declaration order.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: alpha\n    order: 0\n\
             \x20 - id: beta\n    order: 0\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.ids(), vec!["alpha", "beta"]);
    }

    #[test]
    fn default_taxonomy_round_trips_through_the_loader() {
        // AC-3: serialize the shipped defaults, read them back through the loader,
        // and get the identical taxonomy — the "loader ↔ defaults" contract GT-6's
        // editor write-back depends on.
        let tmp = tempfile::tempdir().unwrap();
        // Serialize via the public StatusState shape (what GT-6's editor writes).
        let yaml = serde_yaml::to_string(&serde_json::json!({
            "states": default_taxonomy().states,
        }))
        .unwrap();
        write_taxonomy(tmp.path(), &yaml);
        let reloaded = load_status_taxonomy_from(tmp.path());
        assert_eq!(reloaded.states, default_taxonomy().states);
        assert!(reloaded.warnings.is_empty());
    }

    #[test]
    fn role_serializes_snake_case() {
        // The on-disk `role:` token and the FE-facing string are snake_case.
        assert_eq!(
            serde_json::to_string(&StatusRole::NotStarted).unwrap(),
            "\"not_started\""
        );
    }

    #[test]
    fn role_token_set_matches_serde() {
        // Guard `from_token` against drift from the serde `snake_case` wire form:
        // every variant's serialized token must parse back to that same variant.
        for role in [
            StatusRole::NotStarted,
            StatusRole::Active,
            StatusRole::Review,
            StatusRole::Done,
            StatusRole::Blocked,
        ] {
            let wire = serde_json::to_value(role).unwrap();
            let token = wire.as_str().unwrap();
            assert_eq!(StatusRole::from_token(token), Some(role), "token '{token}'");
        }
        assert_eq!(StatusRole::from_token("nonsense"), None);
        // Case-sensitive: the wire form is snake_case, not TitleCase.
        assert_eq!(StatusRole::from_token("Active"), None);
    }

    #[test]
    fn unknown_role_defaults_to_active_and_keeps_the_state() {
        // Per-field tolerance (AC-7): a role typo defaults that one field + soft-drifts
        // — it does NOT fail the whole file and drop every state.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: draft\n    role: not_a_role\n\
             \x20 - id: shipped\n    role: done\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.ids(), vec!["draft", "shipped"]);
        assert_eq!(tax.role_of("draft"), Some(StatusRole::Active));
        assert_eq!(tax.role_of("shipped"), Some(StatusRole::Done));
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("unknown role 'not_a_role'"));
    }

    #[test]
    fn non_scalar_role_defaults_that_field_not_the_whole_file() {
        // A list/map `role:` is structurally not a token — it must degrade that one
        // field (active + soft-drift), NOT fail the whole-file parse and drop every
        // other valid state.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: weird\n    role: [a, b]\n\
             \x20 - id: fine\n    role: review\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.ids(), vec!["weird", "fine"]);
        assert_eq!(tax.role_of("weird"), Some(StatusRole::Active));
        assert_eq!(tax.role_of("fine"), Some(StatusRole::Review));
        assert_eq!(tax.warnings.len(), 1);
        assert!(tax.warnings[0].contains("non-scalar role"));
    }

    #[test]
    fn order_at_u32_max_plus_absent_order_warns_without_panic() {
        // An explicit u32::MAX order can't be "appended after" — flag the tie instead
        // of silently violating append-after-max, and never panic (saturating add).
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: capped\n    order: 4294967295\n\
             \x20 - id: after\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.states.len(), 2);
        assert!(tax.warnings.iter().any(|w| w.contains("u32 maximum")));
    }

    #[test]
    fn stateless_fallback_carries_drop_diagnostics() {
        // Every entry dropped → defaults, but the user still learns *why* (the
        // per-entry drop notices ride along with the headline).
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: \"\"\n    label: One\n\
             \x20 - id: \"  \"\n    label: Two\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(tax.states, default_taxonomy().states);
        assert!(tax.warnings.iter().any(|w| w.contains("no usable states")));
        assert!(tax.warnings.iter().any(|w| w.contains("no id")));
    }

    #[test]
    fn delivered_flag_parses_and_absent_is_none() {
        // SW-GT-6 (AC-1): the one manual semantic marker. An explicit `false` marks a
        // dropped/won't-do terminal; an unmarked state carries no flag (absent ⇒ None,
        // which the FE reads as delivered).
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: done\n    role: done\n\
             \x20 - id: wont-do\n    role: done\n    delivered: false\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert!(tax.warnings.is_empty());
        let done = tax.states.iter().find(|s| s.id == "done").unwrap();
        let wont = tax.states.iter().find(|s| s.id == "wont-do").unwrap();
        assert_eq!(done.delivered, None, "unmarked state carries no flag");
        assert_eq!(
            wont.delivered,
            Some(false),
            "explicit dropped flag preserved"
        );
    }

    #[test]
    fn delivered_absent_is_omitted_on_serialize() {
        // Round-trip cleanliness: an ordinary (unmarked) state must not grow a
        // `delivered:` key when the editor writes the taxonomy back.
        let states = default_taxonomy().states;
        let yaml = serde_yaml::to_string(&serde_json::json!({ "states": states })).unwrap();
        assert!(
            !yaml.contains("delivered"),
            "unmarked states omit the delivered key: {yaml}"
        );
    }

    #[test]
    fn delivered_true_round_trips_through_the_loader() {
        // An explicit `delivered: true` (an author un-dropping a state) survives a
        // write→reload cycle as `Some(true)`, distinct from absent.
        let tmp = tempfile::tempdir().unwrap();
        write_taxonomy(
            tmp.path(),
            "states:\n\
             \x20 - id: shipped\n    role: done\n    delivered: true\n",
        );
        let tax = load_status_taxonomy_from(tmp.path());
        assert_eq!(
            tax.states
                .iter()
                .find(|s| s.id == "shipped")
                .unwrap()
                .delivered,
            Some(true)
        );
    }
}
