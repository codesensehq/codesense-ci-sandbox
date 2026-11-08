// SPDX-License-Identifier: MIT
//! SW-GT-18: per-team **metadata field schema** + pre-run field validation.
//!
//! Sibling to [`crate::commands::status_taxonomy`] (SW-GT-1) — the same
//! git-synced, materialized-per-team `content/` home, but for the accepted
//! **field set** rather than the state set. A team formally declares which
//! metadata fields it uses so codesense knows the accepted namespace, the
//! automation template vars are drawn from it, and a sprint's stories can be
//! validated against it **before** a run fires (rather than a prompt referencing
//! `{dev_branch}` firing against a story that lacks it).
//!
//! The accepted set = codesense's **official baseline** (auto-included, derived
//! from the story-field registry [`STORY_FIELDS`] so there is no second
//! hand-maintained list to drift) **∪** the team's **custom** fields (declared in
//! `{org_root}/{team}/content/field-schema.yml`). The team never re-enumerates the
//! official fields; adding a custom field is a deliberate edit there.
//!
//! Tolerance mirrors [`crate::commands::status_taxonomy`]: [`load_field_schema_from`]
//! is **total** — it never returns an error and never writes. A **missing** file is
//! the common zero-config case (the official baseline is the whole schema). A
//! malformed / unreadable file falls back to the baseline plus a single soft-drift
//! `warnings` entry, so a broken edit can never wedge validation.
//!
//! GT-18 is a backend mechanism: the loader, the `{fieldName}` substitution
//! ([`substitute_template_vars`]) and the per-story validation ([`validate_story_fields`]).
//! The one live validation consumer is MCP `validate_candidate` (SW-GT-2), which gains a
//! blocking required-field check.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use log::{debug, warn};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::commands::pipeline_format::{canonical_field_key, STORY_FIELDS};

/// The ONE `{fieldName}` template-var pattern — `{` + an ASCII identifier (`\w+`, via the
/// `(?-u)` flag so a unicode token like `{café}` isn't a field reference) + `}`. Shared by
/// [`extract_template_vars`] (lint) and [`substitute_template_vars`] (preview) so the two
/// can never disagree on what a `{var}` is.
static TEMPLATE_VAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\{(\w+)\}").unwrap());

/// The per-team field-schema file under a team's `content/` dir (sibling of
/// `status-taxonomy.yml` / `sprints/`).
pub(crate) const SCHEMA_FILENAME: &str = "field-schema.yml";

/// One declared metadata field. `type` is an **opaque** descriptor (documentation /
/// editor hint) — validation is presence-only (AC-3), never a value type-check, so
/// an unknown `type` can never fail a load or a story. `official` is derived, not
/// authored: `true` for a codesense-baseline field, `false` for a team custom field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// SW-AU-77E: the ordered allowed-value vocabulary for a `choice` field
    /// (empty for every other type). Empty ⇒ unconstrained (the empty-set escape
    /// hatch the value validator already honors). Serialized camelCase
    /// (`allowedValues`) for the TS `FieldDef`, with a snake_case authoring alias.
    #[serde(
        default,
        alias = "allowed_values",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_values: Vec<String>,
    #[serde(default)]
    pub required: bool,
    /// Baseline (official) fields form the accepted namespace — a story carrying one
    /// is never flagged "unknown" — but are never themselves reported "missing" (only
    /// declared CUSTOM fields drive missing-field gaps). See [`validate_story_fields`].
    pub official: bool,
}

/// The resolved per-team field schema envelope, mirroring
/// [`crate::commands::status_taxonomy::StatusTaxonomy`]: the accepted fields plus any
/// soft-drift `warnings`, so a consumer renders the schema and its drift banner from a
/// single call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSchema {
    pub fields: Vec<FieldDef>,
    pub warnings: Vec<String>,
}

impl FieldSchema {
    /// Is `name` a declared field (official or custom)?
    pub fn contains(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name)
    }

    /// The **custom** (non-official) `FieldDef` for `name`, matched case-insensitively
    /// via [`canonical_field_key`] (so a doc key `Risk_Tier` resolves the declared
    /// `risk_tier`). Unlike the [`crate::domain::TagSchema::tag`] sibling, dashes are
    /// NOT folded to underscores: [`canonical_field_key`] treats dash-vs-underscore as
    /// out of scope, and declared field names are `[a-z0-9_]` by construction
    /// (`build_declared_field`), so the whole field stack (this lookup, the key
    /// extractor, the runner gate) matches on the same canonical key. `f.name` is
    /// already canonical — the loader canonicalizes every stored name — so only the
    /// lookup key is canonicalized here. Official baseline fields return `None`: they
    /// are the accepted namespace validated by the built-in `validate_fields`, never the
    /// declared-value path.
    pub fn custom_field(&self, name: &str) -> Option<&FieldDef> {
        let canon = canonical_field_key(name);
        self.fields.iter().find(|f| !f.official && f.name == canon)
    }
}

/// The codesense-shipped **official** field baseline: every row of the story-field
/// registry ([`STORY_FIELDS`]), so there is no second hand-maintained list to drift
/// (the DRY guard the sibling taxonomy gets from `VALID_STATUSES`). Each
/// row becomes an optional, non-required official field (the registry's own test keeps
/// its keys unique). Because it is compiled from the registry, a baseline change is a
/// code change — a git-visible migration, never silent config drift (AC-1).
pub fn official_fields() -> Vec<FieldDef> {
    STORY_FIELDS
        .iter()
        .map(|field| FieldDef {
            name: canonical_field_key(field.yaml),
            r#type: None,
            allowed_values: Vec::new(),
            required: false,
            official: true,
        })
        .collect()
}

/// The shipped default schema — the official baseline with no custom fields and no
/// warnings. The zero-config fallback (AC-1): absent file → this, silently.
pub fn default_schema() -> FieldSchema {
    FieldSchema {
        fields: official_fields(),
        warnings: Vec::new(),
    }
}

/// The official baseline carrying a single soft-drift `warning` — the fallback every
/// non-happy load path returns (unreadable / malformed file).
fn defaults_with_warning(warning: String) -> FieldSchema {
    let mut s = default_schema();
    s.warnings.push(warning);
    s
}

/// The on-disk file shape: a top-level `fields:` list (only the team's **custom**
/// fields — official ones are never re-enumerated), so the format stays extensible.
#[derive(Debug, Default, Deserialize)]
struct SchemaFile {
    #[serde(default)]
    fields: Vec<RawField>,
}

/// One custom field as authored on disk — every field past `name` is optional so a
/// partial entry degrades to defaults rather than failing the whole parse. Unknown
/// keys are ignored (no `deny_unknown_fields`) so the format can widen.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawField {
    #[serde(default)]
    name: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default, alias = "allowed_values")]
    allowed_values: Vec<String>,
    #[serde(default)]
    required: bool,
}

/// SW-AU-77E: normalize a `choice` field's vocabulary — trim each entry, drop blanks, and
/// drop later duplicates (first spelling wins), preserving order. The ONE sanitizer shared
/// by the loader ([`merge_custom`]) and the declare command so a stray trailing `- ` or a
/// repeated value can never reach the picker on either path.
pub fn sanitize_allowed_values(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && seen.insert(v.clone()))
        .collect()
}

/// Merge the authored custom fields over the official baseline (official ∪ custom),
/// collecting soft-drift notices. Per-field tolerance mirrors the taxonomy loader:
/// drop empty-name and duplicate-name entries (keep first, warn); a custom entry
/// re-declaring an official name is ignored (the baseline wins) with a warning.
fn merge_custom(raw_fields: Vec<RawField>) -> FieldSchema {
    let mut fields = official_fields();
    let official: BTreeSet<String> = fields.iter().map(|f| f.name.clone()).collect();
    let mut warnings = Vec::new();
    let mut custom_seen: BTreeSet<String> = BTreeSet::new();

    for rf in raw_fields {
        // Canonicalize so a custom `Status`/`STATUS` collides with the official
        // `status` (case-insensitive namespace), matching the key extractor.
        let name = canonical_field_key(&rf.name);
        if name.is_empty() {
            warnings.push("field schema: dropped a field with no name".to_string());
            continue;
        }
        if official.contains(&name) {
            warnings.push(format!(
                "field schema: custom field '{name}' re-declares an official field — keeping the official baseline, ignoring the custom entry"
            ));
            continue;
        }
        if !custom_seen.insert(name.clone()) {
            warnings.push(format!(
                "field schema: duplicate field '{name}' — keeping the first, ignoring the rest"
            ));
            continue;
        }
        fields.push(FieldDef {
            name,
            r#type: rf.r#type.filter(|t| !t.trim().is_empty()),
            allowed_values: sanitize_allowed_values(rf.allowed_values),
            required: rf.required,
            official: false,
        });
    }

    FieldSchema { fields, warnings }
}

/// Load + resolve the field schema rooted at a team `content/` dir. Pure over the
/// filesystem (no app/Tauri state) so it is directly unit-testable; a consumer wraps
/// it with [`crate::context_repo::path_to_team_content_dir`] on the blocking pool,
/// off the webview thread. **Never returns an error and never writes** — the tolerant
/// posture is encoded in the return shape (baseline + `warnings`).
pub fn load_field_schema_from(content_dir: &Path) -> FieldSchema {
    load_field_schema_from_for(content_dir, &Default::default())
}

pub(crate) fn load_field_schema_from_for(
    content_dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> FieldSchema {
    let path = content_dir.join(SCHEMA_FILENAME);
    if !scope.allows(&path) {
        return default_schema();
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Zero-config parity: absent file → the official baseline, silently (AC-1).
            debug!(
                target: "field_schema",
                "no field schema at {} — using the official baseline", path.display()
            );
            return default_schema();
        }
        Err(e) => {
            warn!(
                target: "field_schema",
                "cannot read {}: {e} — using the official baseline", path.display()
            );
            return defaults_with_warning(format!("could not read field schema: {e}"));
        }
    };

    match serde_yaml::from_str::<SchemaFile>(&raw) {
        Ok(file) => merge_custom(file.fields),
        Err(e) => {
            warn!(
                target: "field_schema",
                "malformed {}: {e} — using the official baseline", path.display()
            );
            defaults_with_warning(format!(
                "malformed field schema, using the official baseline: {e}"
            ))
        }
    }
}

/// SW-AU-77E: the five authoring types a team field may declare, in canonical
/// storage spelling. `yes_no` (underscore) folds to the kebab `yes-no`. Returns
/// `None` for any other string — the boundary check the declare command uses to
/// reject an unknown type (AC-1) rather than persisting a stray `type:`.
pub fn canonical_field_type(input: &str) -> Option<&'static str> {
    match input.trim().to_ascii_lowercase().as_str() {
        "text" => Some("text"),
        "number" => Some("number"),
        "yes-no" | "yes_no" => Some("yes-no"),
        "choice" => Some("choice"),
        "date" => Some("date"),
        _ => None,
    }
}

/// SW-AU-77E: is `name` (matched case-insensitively via [`canonical_field_key`]) an
/// official baseline field? The declare path refuses to shadow one (AC-7 / AC-1).
pub fn is_official_field(name: &str) -> bool {
    let canon = canonical_field_key(name);
    official_fields().iter().any(|f| f.name == canon)
}

/// SW-AU-77E: parse the CUSTOM fields from a `field-schema.yml` body, applying the
/// same canonicalization + tolerance the loader uses (official baseline stripped, so
/// the result is exactly the team's custom set). A **malformed** body is an error —
/// the declare RMW aborts rather than silently clobbering an unparseable file and
/// losing every existing custom field. An empty/whitespace body ⇒ no custom fields.
pub fn custom_fields_from_yaml(raw: &str) -> Result<Vec<FieldDef>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: SchemaFile = serde_yaml::from_str(raw).map_err(|e| {
        format!("existing field schema is malformed ({e}); fix it in Settings first")
    })?;
    Ok(merge_custom(file.fields)
        .fields
        .into_iter()
        .filter(|f| !f.official)
        .collect())
}

// ----- AC-2: template-var extraction -----------------------------------------

/// SW-GT-6 (AC-3): resolve a prompt template against one story's field values, keyed off
/// the ONE [`TEMPLATE_VAR`] pattern. Every `{fieldName}` with a
/// value is substituted; a referenced field with no value is **left as its literal
/// `{fieldName}`** (an explicit unresolved marker, never a silent blank) and reported in
/// the returned list (de-duplicated, canonical). `values` is keyed by canonical field key
/// (the caller canonicalizes), so `{Risk_Tier}` resolves from a `risk_tier` value.
pub fn substitute_template_vars(
    prompt: &str,
    values: &BTreeMap<String, String>,
) -> (String, Vec<String>) {
    let mut unresolved: BTreeSet<String> = BTreeSet::new();
    let resolved = TEMPLATE_VAR.replace_all(prompt, |caps: &regex::Captures| {
        let key = canonical_field_key(&caps[1]);
        match values.get(&key) {
            Some(value) => value.clone(),
            None => {
                unresolved.insert(key);
                caps[0].to_string() // leave the literal `{token}` as the marker
            }
        }
    });
    (resolved.into_owned(), unresolved.into_iter().collect())
}

// ----- AC-3: per-story field validation --------------------------------------

/// The result of validating one story's metadata against a schema + the run's
/// referenced fields. All buckets are presence-based (a field's VALUE is never
/// inspected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FieldReport {
    /// Declared `required` fields the story lacks → always a **block** (AC-4).
    pub missing_required: Vec<String>,
    /// Declared **custom**, non-required fields the story lacks → warn/block per the
    /// run's policy. (Official fields are the namespace, never reported missing.)
    pub missing_optional: Vec<String>,
    /// Fields the run's prompts reference (and the schema declares) that the story
    /// lacks → warn/block per policy: a run would otherwise fire against a blank.
    pub missing_referenced: Vec<String>,
    /// Keys the story carries that the schema does not declare → **warn, never block**
    /// (forward-compatible — extra data is not an error).
    pub extra_unknown: Vec<String>,
}

impl FieldReport {}

/// SW-GT-18 (AC-3): validate one story's present metadata keys against the schema and
/// the run's referenced fields. Presence-only, pure. `present` is the raw key set from
/// [`extract_metadata_keys`] (official + custom).
pub fn validate_story_fields(
    present: &[String],
    schema: &FieldSchema,
    referenced: &BTreeSet<String>,
) -> FieldReport {
    let has = |name: &str| present.iter().any(|p| p == name);

    let mut missing_required = Vec::new();
    let mut missing_optional = Vec::new();
    for f in &schema.fields {
        // Official fields are the accepted namespace, not a per-story requirement —
        // their absence is never a gap. Only declared CUSTOM fields drive missing-*.
        if f.official || has(&f.name) {
            continue;
        }
        if f.required {
            missing_required.push(f.name.clone());
        } else {
            missing_optional.push(f.name.clone());
        }
    }

    // Referenced-and-declared fields the story lacks. Skip any already listed as a
    // missing custom field so a required-and-referenced field is reported once.
    let missing_referenced = referenced
        .iter()
        .filter(|r| {
            schema.contains(r)
                && !has(r)
                && !missing_required.contains(r)
                && !missing_optional.contains(r)
        })
        .cloned()
        .collect();

    // Present keys the schema doesn't declare.
    let extra_unknown = present
        .iter()
        .filter(|k| !schema.contains(k))
        .cloned()
        .collect();

    FieldReport {
        missing_required,
        missing_optional,
        missing_referenced,
        extra_unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_schema(content_dir: &Path, body: &str) {
        std::fs::create_dir_all(content_dir).unwrap();
        std::fs::write(content_dir.join(SCHEMA_FILENAME), body).unwrap();
    }

    fn custom(name: &str, required: bool) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            r#type: None,
            allowed_values: Vec::new(),
            required,
            official: false,
        }
    }

    fn refs(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ----- AC-1: loader ------------------------------------------------------

    #[test]
    fn official_baseline_is_derived_from_the_registry_and_deduped() {
        let base = official_fields();
        // Every official field is non-required, non-custom, and unique.
        assert!(base.iter().all(|f| f.official && !f.required));
        let names: Vec<&str> = base.iter().map(|f| f.name.as_str()).collect();
        let deduped: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), deduped.len(), "no duplicate official names");
        // Completion metadata is author-owned.
        assert!(!names.contains(&"done"));
        assert!(!names.iter().any(|n| n.contains('.')));
        // A representative spread of known official fields is present.
        for expected in [
            "status",
            "story_id",
            "source_repo",
            "sprint",
            "points",
            "priority",
            "parent_epic",
            "epic",
        ] {
            assert!(
                names.contains(&expected),
                "missing official field {expected}"
            );
        }
    }

    #[test]
    fn absent_file_returns_baseline_with_no_warning() {
        // AC-1 zero-config parity: nothing on disk → the official baseline, silently.
        let tmp = tempfile::tempdir().unwrap();
        let schema = load_field_schema_from(tmp.path());
        assert_eq!(schema, default_schema());
        assert!(schema.warnings.is_empty());
    }

    #[test]
    fn custom_fields_union_with_the_official_baseline() {
        // AC-1: loaded set = official ∪ custom; the team declares only its customs.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n\
             \x20 - name: risk_tier\n    type: string\n    required: true\n\
             \x20 - name: reviewer\n",
        );
        let schema = load_field_schema_from(tmp.path());
        assert!(schema.warnings.is_empty());
        assert!(schema.contains("status"), "official baseline still present");
        let risk = schema
            .fields
            .iter()
            .find(|f| f.name == "risk_tier")
            .unwrap();
        assert_eq!(risk.r#type.as_deref(), Some("string"));
        assert!(risk.required && !risk.official);
        let reviewer = schema.fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert!(!reviewer.required && reviewer.r#type.is_none());
    }

    #[test]
    fn choice_field_carries_allowed_values_dropping_blanks() {
        // SW-AU-77E: a `choice` field's allowed-value vocabulary loads through the
        // snake_case authoring alias, preserves order, and drops blank entries.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n\
             \x20 - name: risk_tier\n    type: choice\n    allowed_values: [low, high, \"\", low]\n",
        );
        let schema = load_field_schema_from(tmp.path());
        let risk = schema
            .fields
            .iter()
            .find(|f| f.name == "risk_tier")
            .unwrap();
        assert_eq!(risk.r#type.as_deref(), Some("choice"));
        // Blanks dropped, later duplicate dropped, order preserved.
        assert_eq!(
            risk.allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
    }

    #[test]
    fn non_choice_fields_have_empty_allowed_values() {
        let tmp = tempfile::tempdir().unwrap();
        write_schema(tmp.path(), "fields:\n  - name: reviewer\n    type: text\n");
        let schema = load_field_schema_from(tmp.path());
        let reviewer = schema.fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert!(reviewer.allowed_values.is_empty());
    }

    #[test]
    fn custom_redeclaring_official_keeps_baseline_and_warns() {
        // AC-1: a custom entry colliding with an official name → official wins + warn.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n  - name: status\n    required: true\n",
        );
        let schema = load_field_schema_from(tmp.path());
        // `status` stays the official (non-required) definition.
        let status = schema.fields.iter().find(|f| f.name == "status").unwrap();
        assert!(status.official && !status.required);
        assert_eq!(
            schema.fields.iter().filter(|f| f.name == "status").count(),
            1
        );
        assert_eq!(schema.warnings.len(), 1);
        assert!(schema.warnings[0].contains("re-declares an official field"));
    }

    #[test]
    fn empty_name_and_duplicate_custom_are_dropped_with_warnings() {
        // AC-1 per-field tolerance: empty-name dropped; duplicate custom keeps first.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n\
             \x20 - name: \"\"\n\
             \x20 - name: reviewer\n    required: true\n\
             \x20 - name: reviewer\n    required: false\n",
        );
        let schema = load_field_schema_from(tmp.path());
        let reviewers: Vec<&FieldDef> = schema
            .fields
            .iter()
            .filter(|f| f.name == "reviewer")
            .collect();
        assert_eq!(reviewers.len(), 1);
        assert!(reviewers[0].required, "first declaration kept");
        assert!(schema.warnings.iter().any(|w| w.contains("no name")));
        assert!(schema
            .warnings
            .iter()
            .any(|w| w.contains("duplicate field 'reviewer'")));
    }

    #[test]
    fn custom_redeclaring_official_case_insensitively_is_dropped() {
        // Code-review (Edge corollary): the official-collision guard is case-insensitive
        // — a custom `Status`/`STATUS` must not slip past the official `status`.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n  - name: STATUS\n    required: true\n",
        );
        let schema = load_field_schema_from(tmp.path());
        assert_eq!(
            schema.fields.iter().filter(|f| f.name == "status").count(),
            1,
            "no duplicate/mixed-case `status`"
        );
        assert!(schema.fields.iter().all(|f| f.name != "STATUS"));
        assert!(schema.warnings[0].contains("re-declares an official field"));
    }

    #[test]
    fn custom_field_name_is_canonicalized_on_load() {
        // A mixed-case custom field is stored canonical so it matches the canonicalized
        // present-key set from `extract_metadata_keys`.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(
            tmp.path(),
            "fields:\n  - name: Risk_Tier\n    required: true\n",
        );
        let schema = load_field_schema_from(tmp.path());
        assert!(schema.contains("risk_tier"));
        assert!(!schema.contains("Risk_Tier"));
    }

    #[test]
    fn malformed_file_falls_back_to_baseline_with_one_warning() {
        // AC-1: unparseable YAML → baseline + exactly one soft-drift, never an error.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(tmp.path(), "fields: : : not yaml\n  - [\n");
        let schema = load_field_schema_from(tmp.path());
        assert_eq!(schema.fields, default_schema().fields);
        assert_eq!(schema.warnings.len(), 1);
        assert!(schema.warnings[0].contains("malformed field schema"));
    }

    #[test]
    fn empty_fields_list_is_just_the_baseline() {
        // A stateless custom list is fine — the baseline is always the floor.
        let tmp = tempfile::tempdir().unwrap();
        write_schema(tmp.path(), "fields: []\n");
        let schema = load_field_schema_from(tmp.path());
        assert_eq!(schema, default_schema());
    }

    // ----- SW-AU-77E: declare helpers ----------------------------------------

    #[test]
    fn canonical_field_type_accepts_the_five_and_folds_yes_no() {
        assert_eq!(canonical_field_type("text"), Some("text"));
        assert_eq!(canonical_field_type(" NUMBER "), Some("number"));
        assert_eq!(canonical_field_type("Choice"), Some("choice"));
        assert_eq!(canonical_field_type("date"), Some("date"));
        assert_eq!(canonical_field_type("yes-no"), Some("yes-no"));
        assert_eq!(
            canonical_field_type("yes_no"),
            Some("yes-no"),
            "underscore folds"
        );
        assert_eq!(
            canonical_field_type("select"),
            None,
            "not an authoring type"
        );
        assert_eq!(canonical_field_type(""), None);
    }

    #[test]
    fn is_official_field_is_case_insensitive() {
        assert!(is_official_field("status"));
        assert!(is_official_field("STATUS"), "canonicalized match");
        assert!(!is_official_field("risk_tier"));
    }

    #[test]
    fn custom_fields_from_yaml_strips_official_and_errors_on_malformed() {
        // Absent/empty ⇒ no customs.
        assert!(custom_fields_from_yaml("").unwrap().is_empty());
        // A real body ⇒ only the custom fields, canonicalized.
        let customs = custom_fields_from_yaml(
            "fields:\n  - name: Risk_Tier\n    type: choice\n    allowed_values: [low, high]\n",
        )
        .unwrap();
        assert_eq!(customs.len(), 1);
        assert_eq!(customs[0].name, "risk_tier");
        assert_eq!(
            customs[0].allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
        // Malformed ⇒ error (never a silent clobber).
        assert!(custom_fields_from_yaml("fields: : : not yaml\n  - [\n").is_err());
    }

    // ----- AC-2: template-var extraction -------------------------------------

    fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn substitute_fills_present_vars_and_marks_missing() {
        // AC-3: present vars → value; a var with no value is left as its literal
        // `{token}` marker (never a silent blank) and reported.
        let values = vals(&[("story_id", "SW-GT-6"), ("dev_branch", "feat/gt6")]);
        let (out, missing) =
            substitute_template_vars("dev {story_id} on {dev_branch} for {reviewer}", &values);
        assert_eq!(out, "dev SW-GT-6 on feat/gt6 for {reviewer}");
        assert_eq!(missing, vec!["reviewer".to_string()]);
    }

    #[test]
    fn substitute_canonicalizes_lookup() {
        // `{Risk_Tier}` resolves from a canonical `risk_tier` value (parity with the
        // extractor / declared-schema canonicalization).
        let (out, missing) =
            substitute_template_vars("tier {Risk_Tier}", &vals(&[("risk_tier", "high")]));
        assert_eq!(out, "tier high");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitute_ignores_non_vars_and_dedups_missing() {
        // Empty / dashed braces are not vars; a repeated missing var is reported once.
        let (out, missing) =
            substitute_template_vars("{} {not-a-var} {x} then {x}", &BTreeMap::new());
        assert_eq!(out, "{} {not-a-var} {x} then {x}");
        assert_eq!(missing, vec!["x".to_string()]);
    }

    // ----- AC-3: per-story validation ----------------------------------------

    fn schema_with(customs: Vec<FieldDef>) -> FieldSchema {
        let mut s = default_schema();
        s.fields.extend(customs);
        s
    }

    #[test]
    fn official_fields_absent_are_never_missing() {
        // A bare story missing every optional official field is clean (namespace, not
        // per-story requirement) — the reason validate_candidate stays quiet.
        let schema = default_schema();
        let report = validate_story_fields(&[], &schema, &BTreeSet::new());
        assert_eq!(report, FieldReport::default(), "got {report:?}");
    }

    #[test]
    fn missing_required_and_optional_custom_fields_are_reported() {
        let schema = schema_with(vec![custom("risk_tier", true), custom("reviewer", false)]);
        let present = vec!["status".to_string()];
        let report = validate_story_fields(&present, &schema, &BTreeSet::new());
        assert_eq!(report.missing_required, vec!["risk_tier".to_string()]);
        assert_eq!(report.missing_optional, vec!["reviewer".to_string()]);
        assert!(report.missing_referenced.is_empty());
        assert!(report.extra_unknown.is_empty());
    }

    #[test]
    fn extra_unknown_key_is_reported() {
        let schema = default_schema();
        let present = vec!["status".to_string(), "wat".to_string()];
        let report = validate_story_fields(&present, &schema, &BTreeSet::new());
        assert_eq!(report.extra_unknown, vec!["wat".to_string()]);
    }

    #[test]
    fn referenced_official_field_absent_is_missing_referenced() {
        // The core case: a run references `{dev_branch}` (an official field) but the
        // story lacks it → surfaced as a referenced gap, not silently blank.
        let schema = default_schema();
        let report =
            validate_story_fields(&["status".to_string()], &schema, &refs(&["dev_branch"]));
        assert_eq!(report.missing_referenced, vec!["dev_branch".to_string()]);
    }

    #[test]
    fn required_and_referenced_field_is_reported_once() {
        // A field that is both required-custom AND referenced lands only in
        // missing_required (no double-listing).
        let schema = schema_with(vec![custom("risk_tier", true)]);
        let report = validate_story_fields(&[], &schema, &refs(&["risk_tier"]));
        assert_eq!(report.missing_required, vec!["risk_tier".to_string()]);
        assert!(report.missing_referenced.is_empty());
    }
}
