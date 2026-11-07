// SPDX-License-Identifier: MIT
//! SLW-MD-1: in-file frontmatter / `cs:*` metadata editor — read-model + write
//! commands behind the editor's Metadata band.
//!
//! Three Tauri commands:
//! - [`get_metadata_model`] (Task 1) — detect format, parse the block, return
//!   known + **unknown** (verbatim, round-tripped) fields, `is_epic`,
//!   `block_parse_ok`, and per-field validity (shared with `validate_story` via
//!   [`crate::commands::frontmatter_validation`]).
//! - [`list_source_repos_for_team`] (Task 2) — the team's **configured** repos
//!   (locked decision #5), optionally flagged attached-first for the picker.
//! - [`apply_metadata_edit`] (Task 3) — format-aware writes composing the
//!   existing `pipeline_format` primitives (never hand-rolled serialization),
//!   gated on the read-only write-guard.
//!
//! **What this does NOT do:** it never touches `.meta/` sidecars (in-file only),
//! and never trusts a caller-supplied read-only flag (the backend
//! `is_installed_path` gate is the source of truth).

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::commands::field_schema::FieldSchema;
use crate::commands::files::validate_context_path_owned;
use crate::commands::frontmatter_validation::{self as fv, FieldValidity};
use crate::commands::pipeline_format::{
    self, yaml_to_cs_name, FrontmatterFields, PipelineFileFormat, FIELD_NAME_MAP,
};
use crate::domain::{load_org, TagSchema, TagType};

// ----- Read-model (Task 1) ---------------------------------------------------

/// The metadata band's read-model for one doc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataModel {
    /// `"markdown"` | `"html"` — drives the band's header label + write path.
    pub format: String,
    /// Known canonical fields, parsed. All optional.
    pub fields: FrontmatterFields,
    /// Frontmatter keys NOT in the known schema (typos, future keys, foreign
    /// tool keys). Preserved verbatim as `(key, raw_value)` and round-tripped on
    /// every save (AC-8) — the editor shows them under a disclosure.
    pub unknown_fields: Vec<(String, String)>,
    /// Is this an epic doc? Gates the epic-only fields (`repos`, `branch`) in the
    /// band's "+ Add field" menu. (SW-XS-5: the retired `scope_context` /
    /// `scope_install` are never offered; an existing value still displays.)
    pub is_epic: bool,
    /// `false` when the metadata block is present but malformed (YAML that
    /// doesn't parse / a broken meta region) — the band falls back to the raw
    /// editor + explicit Apply (AC-12). `true` for a clean parse OR no block at
    /// all (a plain doc with no frontmatter is not "malformed").
    pub block_parse_ok: bool,
    /// Per-field validity (shared predicate — see `frontmatter_validation`).
    pub per_field_validity: Vec<FieldValidity>,
    /// SW-AU-77G/77I: the schema-declared fields PRESENT on this doc — team custom
    /// fields (`source: Team`) AND org `TagSchema` tags (`source: Org`) — surfaced
    /// as first-class typed rows in the band (so they leave the `unknown_fields`
    /// disclosure). Self-contained per-doc read projection — `name` is canonical,
    /// `values` are the present value(s) (scalar = 1-elem; `multi_select` = per
    /// item), `type`/`allowed_values` are the declared render config, and org rows
    /// carry their `color`/`icon` resolved once here — so the row renders correctly
    /// on first paint without a second schema round-trip on the FE. Validity rides
    /// `per_field_validity`.
    pub schema_rows: Vec<SchemaFieldRow>,
    /// SW-AU-77J: the team-declared custom fields that are DECLARED BUT ABSENT from
    /// this doc — the "+ Add field" menu offers these so a user can drop in an empty
    /// typed row and fill it (the complement of `schema_rows`). Same self-contained
    /// per-doc projection: excludes any field already present (a `schema_rows` row)
    /// and any org-tag name (those ride SW-AU-77I). Carries the declared config only,
    /// no value.
    pub declarable_fields: Vec<DeclaredFieldDef>,
    /// The verbatim on-disk metadata block, byte-for-byte (no re-serialization):
    /// the YAML frontmatter body between the `---` fences (markdown) or the
    /// contiguous run of `<meta name="cs:*">` tags (HTML). `None` when the doc
    /// has no metadata block at all.
    ///
    /// Populated **even when `block_parse_ok == false`** (malformed) — that is
    /// its whole purpose: the raw editor (AC-12) seeds from this so the user can
    /// see and repair a broken block. It is the exact text a
    /// `ReplaceRawBlock { text }` edit swaps, so `raw_block` round-trips with
    /// `apply_metadata_edit(ReplaceRawBlock)`.
    pub raw_block: Option<String>,
}

/// Which registry a [`SchemaFieldRow`] came from — a legibility/provenance marker
/// (never a permission), so the band can tint an org tag's label glyph while
/// keeping team rows plain. Serialized as `"Team"` / `"Org"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowSource {
    Team,
    Org,
}

/// SW-AU-77G/77I: one schema-declared field present on a doc, projected for the
/// band's first-class row. `name` is the canonical field/tag key; `values` its
/// present value(s) (a scalar type yields a 1-element vec; a `multi_select` tag the
/// per-item list); `type`/`allowed_values` the declared render config. Org tags
/// additionally carry their `color`/`icon` — resolved ONCE here from the `TagDef`,
/// so the FE never re-derives them from a separate schema prop. Team rows carry
/// `source: Team` and no color/icon (the team `FieldDef` declares none). Validity
/// is NOT carried here — it rides `per_field_validity` keyed by `name`, so the one
/// shared validator stays the single source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaFieldRow {
    pub name: String,
    pub values: Vec<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub source: RowSource,
}

/// SW-AU-77J: one team-declared custom field DECLARED BUT ABSENT from a doc, offered
/// by the band's "+ Add field" menu. Carries the declaration config only (no value) —
/// `name` is canonical, `type`/`allowed_values` drive the empty typed row, and
/// `required` is a "worth adding" hint (never a block here — presence enforcement stays
/// the runner gate + the badge). The mirror of [`SchemaFieldRow`] for absent fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredFieldDef {
    pub name: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
    pub required: bool,
}

fn format_str(fmt: PipelineFileFormat) -> &'static str {
    match fmt {
        PipelineFileFormat::Markdown => "markdown",
        PipelineFileFormat::Html => "html",
    }
}

/// Canonical top-level field names known to the schema. Unknown = anything else.
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "type",
    "status",
    "story_id",
    "source_repo",
    "work_repo", // SV-CDE-85: typed on FrontmatterFields, not an open tag
    "dev_branch",  // SW-GS-35: story-scoped dev branch (observed; known field)
    "branch_name", // SW-WB-1: story-scoped intended branch (editable; known field)
    "team",
    "depends_on",
    "supporting_docs",
    "tags", // SW-AU-77F: typed on StoryRef, so NOT an open tag (symmetric with cs:tags)
    "repos",
    "branch",
    "scope_context", // SW-XS-5: retired, still parsed so existing docs display it
    "scope_install", // SW-XS-5: retired, still parsed so existing docs display it
    "sprint",        // SW-SB-1: board metadata — typed on StoryRef, so NOT an open tag
    "points",        // SW-SB-1: without this it double-appears in `open_tags`
    "assigned_to",   // SW-SB-1: board metadata (assignee username)
    "rank",          // SW-SB-22: board-ordering rank — typed on StoryRef, not an open tag
    "gate_evidence", // stamped by the retired status gate; tolerated, never an open tag
];

/// Is this doc an epic? Convention (matching `mcp::story_authoring` +
/// `pipeline_scanner`): an epic **main doc** is identified by filename
/// (README/index with a pipeline extension), NOT by `type:`. We additionally
/// treat an explicit `type: epic` or the presence of any epic-only scope/repos
/// field as epic — so a hand-authored epic doc under a non-standard name still
/// offers its epic fields.
pub(crate) fn detect_is_epic(path: &Path, fields: &FrontmatterFields) -> bool {
    if is_epic_main_doc_name(path) {
        return true;
    }
    if fields
        .document_type
        .as_deref()
        .map(str::trim)
        .map(|t| t.eq_ignore_ascii_case("epic"))
        .unwrap_or(false)
    {
        return true;
    }
    // Epic-only fields present ⇒ treat as epic (offer the rest). NOTE (SW-OR-1b):
    // `branch` is NOT in this set — it is now the unified STORY branch field as
    // well as the epic integration branch, so its presence no longer implies an
    // epic. `repos` remains epic-exclusive, so a real epic (which declares its
    // repos) is still detected here. SW-XS-5: the retired `scope_context` /
    // `scope_install` no longer mark a doc as an epic.
    fields.repos.is_some()
}

/// README/index main-doc detection by filename (mirrors
/// `mcp::story_authoring::is_epic_main_doc_name` — kept local to avoid a cross-
/// module `pub` on that private helper; both key off the same convention).
fn is_epic_main_doc_name(path: &Path) -> bool {
    let has_pipeline_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| pipeline_format::detect_format(e).is_some())
        .unwrap_or(false);
    if !has_pipeline_ext {
        return false;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|stem| {
            let lower = stem.to_ascii_lowercase();
            lower == "readme" || lower == "index"
        })
        .unwrap_or(false)
}

/// Partition a markdown doc's raw frontmatter into `(block_parse_ok,
/// unknown_fields)`. Uses `gray_matter` to slice the raw YAML, then parses it
/// into a `serde_yaml::Mapping` to enumerate top-level keys verbatim.
///
/// `block_parse_ok = false` when a frontmatter block is *present* (`---` opener)
/// but the raw YAML fails to parse into a mapping — the malformed-recovery case.
/// No block at all ⇒ `(true, [])` (a plain doc is not malformed).
fn markdown_unknowns(content: &str) -> (bool, Vec<(String, String)>) {
    let (block_parse_ok, mapping) = markdown_frontmatter_mapping(content);
    let unknown = mapping
        .as_ref()
        .map(|m| {
            open_mapping_fields(m)
                .into_iter()
                .map(|(k, v)| (k, render_yaml_value(v)))
                .collect()
        })
        .unwrap_or_default();
    (block_parse_ok, unknown)
}

/// Parse a markdown doc's frontmatter into a YAML mapping, ONCE — shared by the
/// unknown-field partition and the open-tag-value extractor so both agree on
/// block-ok semantics without duplicating the gray_matter + serde_yaml dance.
///
/// - `(true, None)` — no frontmatter block (a plain doc; not malformed), or a
///   well-formed but empty block (`---\n---`);
/// - `(false, None)` — a block is present but cannot be recovered;
/// - `(false, Some(map))` — plain colon values were recovered in memory;
/// - `(true, Some(map))` — a well-formed, non-empty block.
fn markdown_frontmatter_mapping(content: &str) -> (bool, Option<serde_yaml::Mapping>) {
    let parsed = pipeline_format::markdown::read_frontmatter(content);
    (parsed.block_parse_ok, parsed.mapping)
}

/// The OPEN (non-[`KNOWN_TOP_LEVEL_KEYS`]) top-level entries of a frontmatter
/// mapping, paired with their raw values — the shared iteration behind both the
/// stringified unknown-field partition and the structured open-tag extraction.
fn open_mapping_fields(mapping: &serde_yaml::Mapping) -> Vec<(String, &serde_yaml::Value)> {
    use serde_yaml::Value;
    let mut out = Vec::new();
    for (k, v) in mapping.iter() {
        let key = match k {
            Value::String(s) => s.clone(),
            other => other.as_str().map(|s| s.to_string()).unwrap_or_default(),
        };
        if key.is_empty() || KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            continue;
        }
        out.push((key, v));
    }
    out
}

/// Render a `serde_yaml::Value` as a compact one-line raw string for the
/// unknown-field disclosure. Scalars pass through; collections serialize
/// back to YAML flow form (trimmed). Best-effort — display only; the write
/// path preserves the original bytes with the line-based field writers.
fn render_yaml_value(v: &serde_yaml::Value) -> String {
    use serde_yaml::Value;
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        other => serde_yaml::to_string(other)
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    }
}

/// Partition an HTML doc's `cs:*` meta tags into `(block_parse_ok,
/// unknown_fields)`. HTML has no "malformed block" the way YAML does — a `cs:*`
/// tag either matches the regex or it doesn't — so `block_parse_ok` is always
/// `true` for HTML. Unknown = a `cs:*` meta whose name maps to no known field.
fn html_unknowns(content: &str) -> (bool, Vec<(String, String)>) {
    // Collect every cs:* pair, map back to canonical, and keep the ones that
    // aren't in the known schema.
    let known_cs = known_cs_names();
    let mut unknown = Vec::new();
    for (cs_field, value) in collect_cs_meta_pairs(content) {
        if !known_cs.iter().any(|k| k.eq_ignore_ascii_case(&cs_field)) {
            unknown.push((format!("cs:{cs_field}"), value));
        }
    }
    (true, unknown)
}

/// The known `cs:*` field names (without the `cs:` prefix) — the closed set an
/// open/schema tag is anything NOT in. `FIELD_NAME_MAP` holds the canonical
/// mapping. Shared by [`html_unknowns`] and [`html_open_tag_values`].
fn known_cs_names() -> Vec<&'static str> {
    FIELD_NAME_MAP
        .iter()
        .map(|(_, cs)| *cs)
        // Retired gate evidence meta, still tolerated in old HTML stories.
        .chain(std::iter::once("cs:gate-evidence"))
        .map(|cs| cs.strip_prefix("cs:").unwrap_or(cs))
        .collect()
}

/// Collect `(cs_field_without_prefix, value)` pairs from HTML `<meta cs:*>`
/// tags. Mirrors `html::collect_cs_meta` (which is private) using the same two
/// attribute orders; kept minimal since MD-1 only needs the unknown partition.
fn collect_cs_meta_pairs(text: &str) -> Vec<(String, String)> {
    use regex::Regex;
    use std::sync::LazyLock;

    static NAME_FIRST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)<meta\s+name=["']cs:([^"']+)["']\s+content=["']([^"']*)["']\s*/?>"#)
            .unwrap()
    });
    static CONTENT_FIRST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)<meta\s+content=["']([^"']*)["']\s+name=["']cs:([^"']+)["']\s*/?>"#)
            .unwrap()
    });

    let mut pairs: Vec<(String, String)> = Vec::new();
    for cap in NAME_FIRST.captures_iter(text) {
        pairs.push((cap[1].to_ascii_lowercase(), cap[2].to_string()));
    }
    for cap in CONTENT_FIRST.captures_iter(text) {
        let field = cap[2].to_ascii_lowercase();
        if !pairs.iter().any(|(f, _)| f == &field) {
            pairs.push((field, cap[1].to_string()));
        }
    }
    pairs
}

/// Pure read-model builder — no IO. Uses an empty tag schema (no schema-tag
/// validity); see [`build_metadata_model_with_schema`] for the schema-aware path
/// `get_metadata_model` drives. Test-only ergonomic wrapper so the many existing
/// tests (which predate the schema) stay unchanged; prod always resolves a schema.
#[cfg(test)]
fn build_metadata_model(path: &Path, content: &str, cfg: &fv::ValidatorConfig) -> MetadataModel {
    build_metadata_model_with_schema(
        path,
        content,
        cfg,
        &TagSchema::default(),
        &crate::commands::field_schema::default_schema(),
    )
}

/// SW-DX-1 / SW-AU-77G: the schema-aware read-model builder. Identical to
/// [`build_metadata_model`] but ALSO (a) emits per-tag validity for the org
/// `TagSchema` tags present in the doc and (b) validates + projects the team
/// `FieldSchema` custom fields present in the doc — both appended to
/// `per_field_validity` after the built-in field validity, with the declared
/// customs surfaced as first-class `schema_rows` rows (and lifted out of
/// `unknown_fields`). Pure (no IO): testable in-memory.
fn build_metadata_model_with_schema(
    path: &Path,
    content: &str,
    cfg: &fv::ValidatorConfig,
    schema: &TagSchema,
    field_schema: &FieldSchema,
) -> MetadataModel {
    let format = pipeline_format::format_of(path);
    let (fields, _) = pipeline_format::extract_meta_and_parse_ok(format, content);
    let (block_parse_ok, mut unknown_fields) = match format {
        PipelineFileFormat::Markdown => markdown_unknowns(content),
        PipelineFileFormat::Html => html_unknowns(content),
    };
    let is_epic = detect_is_epic(path, &fields);
    let mut per_field_validity = fv::validate_fields(&fields, cfg);

    // The doc's PRESENT open keys + structured values — ONE extraction shared by
    // org-tag validity, team-declared-field validity, and the declared-field row
    // projection (the closed known fields are validated by `validate_fields`).
    let present = open_tag_values(format, content);

    // SW-DX-1: additive org tag-schema validity (sibling, not a fork). Only present
    // open tags matched by a `TagDef` produce entries; unknown open keys stay
    // unopinionated (no entry).
    if !schema.is_empty() {
        per_field_validity.extend(fv::validate_schema_tags(&present, schema));
    }
    // SW-AU-77G: team-declared custom-field VALUE validity, into the SAME list,
    // reusing 77E's one shared value validator.
    per_field_validity.extend(fv::validate_declared_field_values(&present, field_schema));
    // A name declared in BOTH the org tag schema and the team field schema is
    // validated ONCE — the org-tag entry (appended first) wins.
    dedup_validity_first_wins(&mut per_field_validity);

    // SW-AU-77G/77I: project the present declared fields (team customs AND org
    // tags) as first-class rows and graduate them OUT of the unknown-fields
    // disclosure (rendered once, as rows).
    let schema_rows = collect_schema_rows(&present, field_schema, schema);
    let declared_names: std::collections::HashSet<&str> =
        schema_rows.iter().map(|d| d.name.as_str()).collect();
    unknown_fields
        .retain(|(k, _)| !declared_names.contains(unknown_key_canonical(format, k).as_str()));

    // SW-AU-77J: project the team-declared customs that are ABSENT from the doc — the
    // complement of `schema_rows`. Reuses `declared_names` (the present set just
    // computed) to skip present fields, and the org-tag skip mirrors
    // `collect_schema_rows` so a name owned by an org tag rides SW-AU-77I, not here.
    // Pure in-memory over the already-loaded `field_schema` — no new I/O (AC-6).
    let declarable_fields: Vec<DeclaredFieldDef> = field_schema
        .fields
        .iter()
        .filter(|f| {
            !f.official
                && !declared_names.contains(f.name.as_str())
                && schema.tag(&f.name).is_none()
        })
        .map(|f| DeclaredFieldDef {
            name: f.name.clone(),
            r#type: f.r#type.clone(),
            allowed_values: f.allowed_values.clone(),
            required: f.required,
        })
        .collect();

    // Verbatim on-disk block for the raw editor (AC-12) — computed even when
    // `block_parse_ok` is false (malformed YAML / broken meta run); it is the
    // exact text `ReplaceRawBlock` swaps, so it round-trips with the write path.
    let raw_block = pipeline_format::raw_meta_block(format, content);
    MetadataModel {
        format: format_str(format).to_string(),
        fields,
        unknown_fields,
        is_epic,
        block_parse_ok,
        per_field_validity,
        schema_rows,
        declarable_fields,
        raw_block,
    }
}

/// SW-AU-77G/77I: the schema-declared fields present on the doc, as band rows. A
/// single pass over the present open keys resolves each to EITHER an org `TagDef`
/// (`source: Org`, carrying its `color`/`icon` and per-item `values`) OR a team
/// custom `FieldDef` (`source: Team`, scalar `values`). An empty/absent value yields
/// no row (nothing to show or fix).
///
/// The org tag is checked FIRST, so on an org/team name collision exactly one
/// `source: Org` row is emitted (AC-6) — mirroring the built-ins → org → team append
/// order [`dedup_validity_first_wins`] relies on, so a row binds the org verdict and
/// never the team one. This walks the SAME `present` slice as the validators and, for
/// `multi_select`, derives `values` via the SAME [`fv::split_multi_select_items`] the
/// verdict uses, so a row and its validity agree by construction.
fn collect_schema_rows(
    present: &[(String, Vec<String>)],
    field_schema: &FieldSchema,
    tag_schema: &TagSchema,
) -> Vec<SchemaFieldRow> {
    let mut out: Vec<SchemaFieldRow> = Vec::new();
    // Borrowed keys — `def` lives in the schemas, which outlive this loop — so the
    // dedup set costs no per-row String allocation.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (key, items) in present {
        // Org tag wins on a name collision — checked first, mirroring the validity
        // append order — so a collision resolves to one Org row, never a Team one.
        if let Some(def) = tag_schema.tag(key) {
            // Lock onto the FIRST matched occurrence — exactly as `validate_schema_tags`'s
            // seen-dedup does — so the row and the verdict bind the SAME occurrence. A
            // repeated key whose first value is empty stays Absent on BOTH (no row, no
            // badge); it can never bind a row to a later value the verdict never saw.
            if !seen.insert(def.name.as_str()) {
                continue;
            }
            // Scalar types carry a single value (first present) per AC-4 — never the
            // whole list; only `multi_select` is the per-item list, via the shared split.
            let values = if def.r#type == TagType::MultiSelect {
                fv::split_multi_select_items(items, true)
            } else {
                items
                    .iter()
                    .map(|s| s.trim())
                    .find(|s| !s.is_empty())
                    .map(|s| vec![s.to_string()])
                    .unwrap_or_default()
            };
            if values.is_empty() {
                continue;
            }
            out.push(SchemaFieldRow {
                name: def.name.clone(),
                values,
                r#type: Some(def.r#type.as_str().to_string()),
                allowed_values: def.allowed_values.clone(),
                color: def.color.clone(),
                icon: def.icon.clone(),
                source: RowSource::Org,
            });
            continue;
        }
        let Some(def) = field_schema.custom_field(key) else {
            continue;
        };
        let Some(value) = items.iter().map(|s| s.trim()).find(|s| !s.is_empty()) else {
            continue;
        };
        if seen.insert(def.name.as_str()) {
            out.push(SchemaFieldRow {
                name: def.name.clone(),
                values: vec![value.to_string()],
                r#type: def.r#type.clone(),
                allowed_values: def.allowed_values.clone(),
                color: None,
                icon: None,
                source: RowSource::Team,
            });
        }
    }
    // AC-9: render team rows before org rows, each in its registry's declared order
    // (not doc order, not alphabetical). Built-ins render before these rows already.
    // `sort_by_cached_key` computes each row's key once (not per comparison), so the
    // registry `position` scan runs O(rows), not O(rows·log rows).
    out.sort_by_cached_key(|row| match row.source {
        RowSource::Team => (
            0usize,
            field_schema
                .fields
                .iter()
                .position(|f| f.name == row.name)
                .unwrap_or(usize::MAX),
        ),
        RowSource::Org => (
            1usize,
            tag_schema
                .tags
                .iter()
                .position(|t| t.name == row.name)
                .unwrap_or(usize::MAX),
        ),
    });
    out
}

/// Keep the FIRST validity entry per `field` name, order-preserving. Built-ins are
/// pushed first, then org tags, then team-declared fields — so on a name collision
/// the earlier (owning) source wins and the band never double-counts one field.
fn dedup_validity_first_wins(validities: &mut Vec<FieldValidity>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    validities.retain(|v| seen.insert(v.field.clone()));
}

/// Canonical match key for an entry in `unknown_fields`, so a declared custom field
/// rendered as its own row (SW-AU-77G) is excluded from the unknown disclosure on
/// BOTH formats. Markdown keys are raw frontmatter names; HTML keys carry the `cs:`
/// prefix (and may use dashes) — normalize both to the schema's canonical field key.
fn unknown_key_canonical(format: PipelineFileFormat, raw_key: &str) -> String {
    let base = match format {
        PipelineFileFormat::Html => raw_key
            .strip_prefix("cs:")
            .unwrap_or(raw_key)
            .replace('-', "_"),
        PipelineFileFormat::Markdown => raw_key.to_string(),
    };
    pipeline_format::canonical_field_key(&base)
}

/// SW-DX-1: the doc's OPEN frontmatter keys (those NOT in
/// [`KNOWN_TOP_LEVEL_KEYS`]) with STRUCTURED values — a scalar becomes a 1-item
/// vec, a list becomes N items — so a `multi_select` schema tag can be validated
/// per item. Parallel to [`markdown_unknowns`]/[`html_unknowns`] (which
/// stringify a compact display form); this keeps list structure for validation.
/// Only these open keys can ever be schema tags (the closed known fields are
/// validated by `validate_fields`).
///
/// SW-DX-2 (AC-13): `pub(crate)` so the pipeline scanner + frontmatter cache
/// populate `StoryRef.open_tags` through this SAME single frontmatter-key walker
/// — no second extractor. Given the parsed `format` + raw `content`.
pub(crate) fn open_tag_values(
    format: PipelineFileFormat,
    content: &str,
) -> Vec<(String, Vec<String>)> {
    match format {
        PipelineFileFormat::Markdown => markdown_open_tag_values(content),
        PipelineFileFormat::Html => html_open_tag_values(content),
    }
}

/// Every frontmatter key of a document, as JSON — the context index's view of a
/// file's metadata. Markdown: the whole YAML mapping (lists stay arrays, nested
/// maps stay objects; non-string keys are skipped). HTML: every `cs:*` meta,
/// keyed by its YAML name (`cs:depends-on` → `depends_on`), with the built-in
/// list fields split on commas. An absent or unparseable block yields `{}`.
pub(crate) fn frontmatter_json(
    format: PipelineFileFormat,
    content: &str,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    frontmatter_json_checked(format, content).0
}

/// Values and parse validity come from the same pass, so observers never infer
/// absence from a parse the metadata reader rejected.
pub(crate) fn frontmatter_json_checked(
    format: PipelineFileFormat,
    content: &str,
) -> (std::collections::BTreeMap<String, serde_json::Value>, bool) {
    use serde_json::Value;
    let mut out = std::collections::BTreeMap::new();
    let valid;
    match format {
        PipelineFileFormat::Markdown => {
            let (ok, mapping) = markdown_frontmatter_mapping(content);
            valid = ok;
            for (k, v) in mapping.into_iter().flatten() {
                let Some(key) = k.as_str() else { continue };
                if let Ok(json) = serde_json::to_value(&v) {
                    out.insert(key.to_string(), json);
                }
            }
        }
        PipelineFileFormat::Html => {
            let lower = content.to_ascii_lowercase();
            valid = !lower.contains("<head") || lower.contains("</head>");
            const LIST_FIELDS: &[&str] = &[
                "depends_on",
                "supporting_docs",
                "tags",
                "repos",
                "topic",
                "task",
                "tools",
                "audience",
            ];
            for (yaml_name, value) in
                crate::commands::pipeline_format::html::template_value_pairs(content)
            {
                let key = yaml_name.replace('-', "_");
                let json = if LIST_FIELDS.contains(&key.as_str()) {
                    Value::Array(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|s| Value::String(s.to_string()))
                            .collect(),
                    )
                } else {
                    Value::String(value)
                };
                out.insert(key, json);
            }
        }
    }
    (out, valid)
}

fn markdown_open_tag_values(content: &str) -> Vec<(String, Vec<String>)> {
    let (_block_parse_ok, mapping) = markdown_frontmatter_mapping(content);
    mapping
        .as_ref()
        .map(|m| {
            open_mapping_fields(m)
                .into_iter()
                .map(|(k, v)| (k, pipeline_format::yaml_value_items(v)))
                .collect()
        })
        .unwrap_or_default()
}

/// The HTML analogue: unknown `cs:*` metas, keyed by the tag name in
/// underscore form (`cs:context-level` → `context_level`) to match schema tag
/// names, each carrying its raw (possibly comma-joined) value as one item.
/// `validate_schema_tags` splits multi_select CSV per item.
fn html_open_tag_values(content: &str) -> Vec<(String, Vec<String>)> {
    let known_cs = known_cs_names();
    let mut out = Vec::new();
    for (cs_field, value) in collect_cs_meta_pairs(content) {
        if known_cs.iter().any(|k| k.eq_ignore_ascii_case(&cs_field)) {
            continue;
        }
        // cs names use hyphens (`context-level`); schema tag names use
        // underscores (`context_level`) — normalize so they match.
        out.push((cs_field.replace('-', "_"), vec![value]));
    }
    out
}

// ----- Configured-repo / team resolution -------------------------------------

/// Resolve the org from the working-tree registry ONCE. `None` inputs return
/// `None` without touching disk; a missing/unresolved org also yields `None`
/// (callers surface empty sets, never an error). Loading once lets a single
/// `get_metadata_model` call derive BOTH the team constrained-sets and the tag
/// schema from one read — no double I/O on the file-open path (SW-DX-1).
fn resolve_org(org_root: Option<&str>, org_slug: Option<&str>) -> Option<crate::domain::Org> {
    let (root, slug) = (org_root?, org_slug?);
    load_org(slug.to_string(), Path::new(root), None).org
}

/// SW-DX-1: the org-global tag registry for the given anchor. `None`/empty
/// anchors or an unresolved org ⇒ an empty (unopinionated) schema — never an
/// error. This is the resolution behind the `get_tag_schema` command.
fn resolve_tag_schema(org_root: Option<&str>, org_slug: Option<&str>) -> crate::domain::TagSchema {
    resolve_org(org_root, org_slug)
        .map(|o| o.tag_schema)
        .unwrap_or_default()
}

/// The single definition of "configured `source_repo`s for a team" — the team's
/// configured `owner/repo` slugs, deduped, preserving configured order. This is
/// the ONE place that turns a `Team` into its slug set; both the validity path
/// (`configured_sets` → `get_metadata_model`) and the picker
/// (`list_source_repos_for_team`) derive from it so they can never disagree on
/// what "configured" means (locked decision #5 — configured, NOT attached).
fn configured_source_repos(team: &crate::domain::Team) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut slugs = Vec::new();
    for slug in team.source_repos.iter().map(|s| s.to_string()) {
        if seen.insert(slug.clone()) {
            slugs.push(slug);
        }
    }
    slugs
}

/// The active team's configured repo slugs (the validity bar) + all configured
/// team names, from an already-loaded team list. Deduped.
fn configured_sets(
    teams: &[crate::domain::Team],
    active_team: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut repos: Vec<String> = teams
        .iter()
        .filter(|t| active_team.map(|a| a == t.name).unwrap_or(true))
        .flat_map(configured_source_repos)
        .collect();
    repos.sort();
    repos.dedup();
    let team_names: Vec<String> = teams.iter().map(|t| t.name.clone()).collect();
    (repos, team_names)
}

#[tauri::command]
pub async fn get_metadata_model(
    app: tauri::AppHandle,
    path: String,
    // The FE already loads org config (`load_org_cmd`), so it passes the
    // resolved org anchor + active team. `None` ⇒ validity computed against an
    // empty configured set ⇒ source_repo/team membership badging disabled
    // (present `source_repo` values are checked syntactic-only, not flagged on
    // membership; a malformed slug is still Invalid). The FE supplies these in
    // practice, so membership checks are the norm.
    org_root: Option<String>,
    org_slug: Option<String>,
    team: Option<String>,
) -> Result<MetadataModel, String> {
    crate::commands::await_blocking("get_metadata_model", move || {
        let file_path = validate_context_path_owned(&path, &app)?;
        let content = std::fs::read_to_string(&file_path)
            .map_err(|e| format!("failed to read {}: {e}", file_path.display()))?;
        // Resolve the org ONCE — the team constrained-sets AND the tag schema
        // both come from this single load (SW-DX-1: no double I/O on file-open).
        let org = resolve_org(org_root.as_deref(), org_slug.as_deref());
        let teams = org.as_ref().map(|o| o.teams.as_slice()).unwrap_or(&[]);
        let (configured_repos, valid_teams) = configured_sets(teams, team.as_deref());
        let cfg = fv::ValidatorConfig {
            configured_repos: &configured_repos,
            valid_teams: &valid_teams,
        };
        let empty_schema = TagSchema::default();
        let tag_schema = org.as_ref().map(|o| &o.tag_schema).unwrap_or(&empty_schema);
        // SW-AU-77G: the team `FieldSchema` (the store 77E/Settings write) drives the
        // declared-custom-field validity + rows. Loaded here, inside the same
        // `await_blocking` boundary, only when both anchors resolve — else the baseline
        // (official-only) schema ⇒ no custom fields ⇒ no declared rows/validity. One
        // small YAML parse off the webview thread (AC-6).
        let field_schema = match (org_root.as_deref(), team.as_deref()) {
            (Some(root), Some(team)) => {
                let content_dir =
                    crate::context_repo::path_to_team_content_dir(Path::new(root), team);
                crate::commands::field_schema::load_field_schema_from(&content_dir)
            }
            _ => crate::commands::field_schema::default_schema(),
        };
        Ok(build_metadata_model_with_schema(
            &file_path,
            &content,
            &cfg,
            tag_schema,
            &field_schema,
        ))
    })
    .await
}

/// SW-DX-1 read seam: the active team's tag registry as JSON. `None`/empty
/// anchors (or an unresolved org) return an empty schema — never an error. The
/// `team` arg is accepted for signature-parity with `get_metadata_model` and
/// forward-compat with a future per-team override; v1 resolves the ONE
/// org-global schema and ignores it. This is the DX-2 / DX-4 / metadata-editor
/// contract; the FE memoizes it per team (it is not a per-file-open read).
#[tauri::command]
pub async fn get_tag_schema(
    org_root: Option<String>,
    org_slug: Option<String>,
    team: Option<String>,
) -> Result<TagSchema, String> {
    crate::commands::await_blocking("get_tag_schema", move || {
        let _ = team; // reserved: v1 schema is org-global.
        Ok(resolve_tag_schema(org_root.as_deref(), org_slug.as_deref()))
    })
    .await
}

// ----- Source-repo picker (Task 2) -------------------------------------------

/// One `source_repo` picker option. `attached` is a **sort convenience only**
/// (materialized repos may sort first) — NOT a filter and NOT a validity input
/// (locked decision #5). Every configured repo is a valid, selectable option
/// regardless of `attached`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRepoOption {
    pub slug: String,
    pub attached: bool,
}

#[tauri::command]
pub async fn list_source_repos_for_team(
    org_root: String,
    org_slug: String,
    team: String,
) -> Result<Vec<SourceRepoOption>, String> {
    crate::commands::await_blocking("list_source_repos_for_team", move || {
        let root = Path::new(&org_root);
        let teams = load_org(org_slug.clone(), root, None)
            .org
            .map(|o| o.teams)
            .unwrap_or_default();
        let Some(found) = teams.iter().find(|t| t.name == team) else {
            // Unknown team ⇒ empty pick-list (not an error) — matches the
            // MCP authoring tools' posture on a missing/partial org.
            return Ok(Vec::new());
        };

        // The CONFIGURED set (deduped, configured order) is the correct,
        // complete answer via the shared `configured_source_repos` resolver —
        // the SAME definition of "configured repos for a team" the validity path
        // uses. `attached` is a sort hint only (never a filter, never a validity
        // input — locked decision #5); every option here is valid and
        // selectable.
        //
        // TODO(follow-up): mark attached-first for picker sort via
        // `enumerate_attached_code_repos_for_team`. That enumerator takes a
        // `ResolverContext` + the team's `source_repos` (not a plain
        // `(root, team)`), so wiring it here means constructing the context
        // (codesense_home + user_id + org_slug + team). Deliberately deferred —
        // attachment is a convenience, and every configured repo is already a
        // valid selectable option, so `attached: false` never mis-labels a
        // repo as invalid or hides it.
        let options: Vec<SourceRepoOption> = configured_source_repos(found)
            .into_iter()
            .map(|slug| SourceRepoOption {
                slug,
                attached: false,
            })
            .collect();
        Ok(options)
    })
    .await
}

// ----- Write path (Task 3) ---------------------------------------------------

/// One structured metadata edit. Serde-tagged (`{ "kind": "setField", ... }`).
/// Scalar and list edits use the byte-preserving format primitives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MetadataEdit {
    /// Set/insert a top-level scalar field (`type`, `status`, `source_repo`, …).
    SetField { field: String, value: String },
    /// Set/insert a top-level list field (`depends_on`, `supporting_docs`,
    /// `repos`, and the retired `scope_context` / `scope_install` so an existing
    /// value can still be edited or cleared). Rejects a comma-bearing item
    /// (gap #2). Empty `items` clears the field.
    SetList { field: String, items: Vec<String> },
    /// Remove a top-level field entirely (returns to absent/valid).
    ClearField { field: String },
    /// Replace the WHOLE metadata block (malformed-recovery Apply, AC-12).
    ReplaceRawBlock { text: String },
}

/// The comma HTML uses to join list items (`split_csv`). A list item containing
/// it would silently split into two on HTML round-trip, so we reject it for
/// BOTH formats (gap #2) — parity is data-fidelity.
const LIST_SEPARATOR: char = ',';

/// SLW-MD-1 (FIX 2): reject a value bearing a raw newline or any other control
/// character. A frontmatter scalar is single-line and a `content="…"` meta
/// attribute is single-line; a newline (or `\r`, `\t`, NUL, …) cannot be
/// represented without changing the block's shape, so we refuse it rather than
/// write a corrupting value. `label` names the field for the error message.
fn reject_control_chars(label: &str, value: &str) -> Result<(), String> {
    if let Some(c) = value.chars().find(|c| c.is_control()) {
        return Err(format!(
            "{label} contains a control character ({c:?}) — not allowed in a single-line metadata value"
        ));
    }
    Ok(())
}

/// Apply one edit to `content`, returning the new content. Pure string
/// transform (no IO) so the round-trip is unit-testable. `format` selects the
/// per-format primitive.
fn apply_one_edit(
    format: PipelineFileFormat,
    content: &str,
    edit: &MetadataEdit,
) -> Result<String, String> {
    match edit {
        MetadataEdit::SetField { field, value } => {
            // FIX 2: a raw newline / control char can't live in a single
            // frontmatter scalar or a `content="…"` meta attribute — reject
            // rather than corrupt the block. (Quoting/escaping of the OTHER
            // YAML/HTML-significant chars happens inside `rewrite_field`.)
            reject_control_chars("value", value)?;
            // Byte-preserving scalar set (gap #3 — NOT the whole-mapping serde
            // path). Works for both formats. Escaping is applied per-format
            // inside `rewrite_field` (YAML-quote for markdown, attr-escape for
            // HTML) so a YAML/HTML-significant value round-trips exactly.
            Ok(pipeline_format::rewrite_field(
                format, content, field, value,
            ))
        }
        MetadataEdit::ClearField { field } => {
            // Clear = remove the key entirely (AC-10), not set to empty. Both
            // scalar and list forms are removed by `rewrite_list_field(&[])`
            // for markdown; for HTML there is no list block, so clearing routes
            // through the empty-value meta removal below.
            Ok(clear_field(format, content, field))
        }
        MetadataEdit::SetList { field, items } => {
            for item in items {
                if item.contains(LIST_SEPARATOR) {
                    return Err(format!(
                        "list item {item:?} contains a comma — not allowed (HTML joins lists with commas, which would split it)"
                    ));
                }
                // FIX 2: no newline/control chars in a list item either.
                reject_control_chars("list item", item)?;
                // FIX 5: reject empty / whitespace-only items for BOTH formats.
                // The HTML CSV-join (`", "`) + `split_csv` (trims + drops
                // empties) would silently drop or mutate them, diverging from
                // markdown's block-list — mirror the comma-rejection guard.
                if item.trim().is_empty() {
                    return Err(format!(
                        "list item {item:?} is empty or whitespace-only — not allowed (it would be silently dropped on HTML round-trip)"
                    ));
                }
            }
            match format {
                PipelineFileFormat::Markdown => Ok(pipeline_format::rewrite_list_field(
                    format, content, field, items,
                )),
                PipelineFileFormat::Html => {
                    // Write-path gap #1: HTML has no list primitive. CSV-join
                    // the items into a single `cs:*` meta via `rewrite_field`.
                    // Empty ⇒ clear the meta tag entirely.
                    if items.is_empty() {
                        Ok(clear_field(format, content, field))
                    } else {
                        let joined = items.join(", ");
                        Ok(pipeline_format::rewrite_field(
                            format, content, field, &joined,
                        ))
                    }
                }
            }
        }
        MetadataEdit::ReplaceRawBlock { text } => Ok(replace_raw_block(format, content, text)),
    }
}

/// Remove a top-level field entirely. Markdown: `remove_block_field` drops the
/// `field:` line + ALL its more-indented continuation lines — list items AND
/// nested mapping children (FIX 7) — so clearing a key whose value is a nested
/// map doesn't orphan child lines into a malformed block. No-ops when absent;
/// works for scalars, lists, and nested maps alike. HTML: strip the meta tag.
fn clear_field(format: PipelineFileFormat, content: &str, field: &str) -> String {
    match format {
        PipelineFileFormat::Markdown => {
            pipeline_format::markdown::remove_block_field(content, field)
        }
        PipelineFileFormat::Html => remove_html_meta(content, field),
    }
}

/// Remove the `<meta name="cs:{field}">` tag for `field` entirely (HTML clear).
/// Handles both attribute orders and self-closing forms; trims the line.
fn remove_html_meta(content: &str, field: &str) -> String {
    use regex::Regex;
    let Some(cs_name) = yaml_to_cs_name(field) else {
        // Unknown field name → nothing to remove.
        return content.to_string();
    };
    let pattern = format!(
        r#"(?im)[ \t]*<meta\s+(?:name=["']{cs}["']\s+content=["'][^"']*["']|content=["'][^"']*["']\s+name=["']{cs}["'])\s*/?>[ \t]*\r?\n?"#,
        cs = regex::escape(cs_name),
    );
    Regex::new(&pattern)
        .map(|re| re.replace_all(content, "").to_string())
        .unwrap_or_else(|_| content.to_string())
}

/// Replace the whole metadata block with `text` (the raw-editor Apply, AC-12).
///
/// - Markdown: `text` is the raw YAML body (without fences); we swap the region
///   between the opening and closing `---` fences, preserving the doc body. If
///   there is no existing block, we prepend one.
/// - HTML: `text` is the raw block of `<meta cs:*>` lines; we replace the run of
///   existing `cs:*` meta tags with it (inserted before `</head>` when none
///   exist). Best-effort — the malformed-recovery path.
fn replace_raw_block(format: PipelineFileFormat, content: &str, text: &str) -> String {
    match format {
        PipelineFileFormat::Markdown => replace_markdown_block(content, text),
        PipelineFileFormat::Html => replace_html_block(content, text),
    }
}

/// FIX 8: byte offset in `body_src` (frontmatter starting at index 0 with a
/// leading `---`) of the first byte of the document body — i.e. just past the
/// closing `---` fence line and its line ending (CRLF or LF). `None` when the
/// frontmatter is unclosed. Line-ending aware so CRLF bodies are sliced (not
/// rebuilt) verbatim.
fn closing_fence_body_offset(body_src: &str) -> Option<usize> {
    // Skip the opening fence line.
    let after_open = body_src.find('\n').map(|i| i + 1)?;
    let mut pos = after_open;
    for line in body_src[after_open..].split_inclusive('\n') {
        // `split_inclusive` yields a final line with no trailing '\n' too, so
        // an unterminated closing fence is handled here as well.
        let content_only = line.trim_end_matches(['\n', '\r']);
        if content_only.trim() == "---" {
            return Some(pos + line.len());
        }
        pos += line.len();
    }
    None
}

fn replace_markdown_block(content: &str, new_yaml: &str) -> String {
    let has_bom = content.starts_with('\u{feff}');
    let body_src = if has_bom { &content[3..] } else { content };
    let yaml = new_yaml.trim_matches('\n');

    let prefix = if has_bom { "\u{feff}" } else { "" };

    if !body_src.starts_with("---") {
        // No existing block — prepend one.
        return format!("{prefix}---\n{yaml}\n---\n{body_src}");
    }

    // FIX 8: slice the body by the BYTE OFFSET immediately after the closing
    // fence's line ending, rather than `lines().join("\n")` — the latter
    // converts CRLF→LF and normalizes trailing blank lines, violating
    // byte-preservation of the (unchanged) body. Byte-offset slicing keeps the
    // body exactly as authored (CRLF, trailing blanks, no trailing newline).
    let body_byte_start = closing_fence_body_offset(body_src);

    match body_byte_start {
        Some(offset) => {
            let body = &body_src[offset..];
            if body.is_empty() {
                format!("{prefix}---\n{yaml}\n---\n")
            } else {
                // Body kept byte-for-byte; only the block region is rewritten.
                format!("{prefix}---\n{yaml}\n---\n{body}")
            }
        }
        None => {
            // Unclosed block — treat the whole thing as body, prepend fresh.
            format!("{prefix}---\n{yaml}\n---\n{body_src}")
        }
    }
}

fn replace_html_block(content: &str, new_block: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static CS_META_LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?im)[ \t]*<meta\s+(?:name=["']cs:[^"']+["']\s+content=["'][^"']*["']|content=["'][^"']*["']\s+name=["']cs:[^"']+["'])\s*/?>[ \t]*\r?\n?"#).unwrap()
    });
    let block = new_block.trim_end_matches('\n');
    if CS_META_LINE.is_match(content) {
        // Replace the FIRST cs:* meta with the new block, drop the rest.
        let mut replaced_first = false;
        let out = CS_META_LINE.replace_all(content, |_: &regex::Captures| {
            if replaced_first {
                String::new()
            } else {
                replaced_first = true;
                format!("{block}\n")
            }
        });
        out.to_string()
    } else if let Some(pos) = content.to_ascii_lowercase().find("</head>") {
        let mut out = String::with_capacity(content.len() + block.len() + 1);
        out.push_str(&content[..pos]);
        out.push_str(block);
        out.push('\n');
        out.push_str(&content[pos..]);
        out
    } else {
        format!("{content}\n{block}")
    }
}

/// FIX 1 (symlink write-guard bypass): resolve `requested_path` through symlinks
/// and confirm it is a writable team-workspace content-root file. Returns the
/// SYMLINK-RESOLVED path to feed to the write (guard-checked == written). A
/// canonicalize failure or a resolved path that lands in a read-only
/// projected/installed tree is REJECTED (fail-safe) — never falls back to the
/// raw path. Factored out of `apply_metadata_edit` so the symlink-escape guard is
/// unit-testable without a Tauri `AppHandle`.
fn resolve_writable_target(
    requested_path: &Path,
    codesense_home: &Path,
) -> Result<std::path::PathBuf, String> {
    let file_path = std::fs::canonicalize(requested_path).map_err(|e| {
        format!(
            "refusing to edit metadata: cannot resolve {} ({e})",
            requested_path.display()
        )
    })?;
    // Canonicalize the home too so the guard's `strip_prefix` anchor matches a
    // symlink-resolved `file_path` exactly (the home may itself sit behind a
    // symlinked ancestor). A home that can't be resolved REJECTS (fail-safe).
    let home = std::fs::canonicalize(codesense_home).map_err(|e| {
        format!(
            "refusing to edit metadata: cannot resolve codesense home {} ({e})",
            codesense_home.display()
        )
    })?;
    if crate::content_engine::is_installed_path(&file_path, &home) {
        return Err(format!(
            "refusing to edit metadata: {} is a read-only projected/installed view, not a team-workspace content file",
            file_path.display()
        ));
    }
    Ok(file_path)
}

#[tauri::command]
pub async fn apply_metadata_edit<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
    edits: Vec<MetadataEdit>,
    page_path: Option<String>,
    frame_id: Option<String>,
) -> Result<(), String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let readable = scope.allows(std::path::Path::new(&path));
    let result = crate::commands::await_blocking("apply_metadata_edit", move || {
        // Serialized with the other status writers (the board move, the auto-run park).
        let _status_lock = crate::commands::pipeline_scanner::status_write_lock();
        let (file_path, format, content) = resolve_and_read(&app, &path)?;

        // SW-GT-13 AC-2: capture the story identity + prior `status` BEFORE the edits,
        // so a human status change of ANY edit kind (SetField, ReplaceRawBlock, …)
        // funnels the one shared audit seam — comparing pre/post is more robust than
        // sniffing for a status SetField.
        let pre = pipeline_format::story_template_values(format, &content, "");
        let story_id = pre.get("story_id").cloned();
        let from_status = pre.get("status").cloned();

        let mut content = content;
        for edit in &edits {
            content = apply_one_edit(format, &content, edit)?;
        }
        write_doc_suppressed(&app, &file_path, content.as_bytes())?;
        note_written(&app, &path, &file_path);

        // Best-effort, after the write: record the human advance when `status` actually
        // changed.
        record_human_status_change(format, story_id, from_status, &content);
        Ok(())
    })
    .await;
    crate::context_repo::caller_scope::readback(readable, result)
}

/// After a metadata write: when `status` actually changed on a story, record the
/// human advance in the run store (best-effort). `story_id`/`from_status` are read
/// from the content BEFORE the write.
fn record_human_status_change(
    format: PipelineFileFormat,
    story_id: Option<String>,
    from_status: Option<String>,
    content: &str,
) {
    use crate::commands::sprint_run::run_store::{self, AdvanceEvent, Trigger};
    let Some(story_id) = story_id else {
        return;
    };
    let to_status = pipeline_format::story_template_values(format, content, "")
        .get("status")
        .cloned();
    if let Some(to) = to_status {
        if from_status.as_deref() != Some(to.as_str()) {
            run_store::record_status_advance(AdvanceEvent {
                story_id,
                from: from_status,
                to,
                trigger: Trigger::Human,
                overridden: false,
                reason: None,
                steps: Vec::new(),
                action_identity: None,
                run_id: None,
                by: run_store::human_actor(),
            });
        }
    }
}

/// The metadata writes are watcher-suppressed (no team-wide fan-out echo), so
/// the context index hears about them here instead: it re-reads the file and
/// emits its change event, which re-runs every page's `subscribeContext`.
pub(crate) fn note_written<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    requested: &str,
    written: &Path,
) {
    crate::context_query::note_path(app, written);
    if Path::new(requested) != written {
        crate::context_query::note_path(app, Path::new(requested));
    }
}

/// Translate one typed field write into the HTML edit that carries it. HTML
/// stories keep metadata in `cs:*` meta tags, so only fields with a known tag
/// can be written there.
fn html_field_edit(field: &str, value: &serde_json::Value) -> Result<MetadataEdit, String> {
    use serde_json::Value;
    if pipeline_format::yaml_to_cs_name(field).is_none() {
        return Err(format!(
            "invalid: {field} has no cs:* meta tag, so it cannot be written to an HTML doc"
        ));
    }
    let scalar = |v: &Value| {
        match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!(
            "invalid: {field}: only strings, numbers, booleans, lists of those, or null can be written"
        )),
    }
    };
    Ok(match value {
        Value::Null => MetadataEdit::ClearField {
            field: field.to_string(),
        },
        Value::Array(items) => MetadataEdit::SetList {
            field: field.to_string(),
            items: items.iter().map(scalar).collect::<Result<_, _>>()?,
        },
        v => MetadataEdit::SetField {
            field: field.to_string(),
            value: scalar(v)?,
        },
    })
}

/// The page bridge's `setField`: set (or, with `null`, remove) top-level
/// frontmatter keys of a content file inside one of the user's known content
/// roots (a team's `content/` or the private space). Values are strings, numbers,
/// booleans, or lists of those. All keys are written in one atomic write that
/// changes only the bytes of those keys (Markdown); the index is told, so pages
/// subscribed with `subscribeContext` re-query. A `status` change is recorded as
/// a human advance, as `apply_metadata_edit` does.
#[tauri::command]
pub async fn update_frontmatter<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
    fields: serde_json::Map<String, serde_json::Value>,
    page_path: Option<String>,
    frame_id: Option<String>,
) -> Result<(), String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let readable = scope.allows(std::path::Path::new(&path));
    let result = crate::commands::await_blocking("update_frontmatter", move || {
        if fields.is_empty() {
            return Err("invalid: no fields to write".into());
        }
        super::frontmatter_fields::validate_field_count(fields.len())
            .map_err(|e| format!("invalid: {e}"))?;
        for field in fields.keys() {
            super::frontmatter_fields::validate_field_name(field)?;
        }
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if pipeline_format::detect_format(ext).is_none() {
            return Err(format!(
                "refusing to edit metadata: {path} is not a Markdown or HTML content file"
            ));
        }
        let roots = crate::context_query::known_roots(&app);
        let in_content = super::pane_bridge::root_of(&roots, Path::new(&path)).is_some_and(|r| {
            matches!(
                r.layer,
                crate::context_query::model::Layer::Team
                    | crate::context_query::model::Layer::Private
            )
        });
        if !in_content {
            return Err(format!(
                "refusing to edit metadata: {path} is not inside a team or private content root"
            ));
        }

        // Serialized with the other status writers (the board move, the auto-run park).
        let _status_lock = crate::commands::pipeline_scanner::status_write_lock();
        let (file_path, format, before) = resolve_and_read(&app, &path)?;
        let pre = pipeline_format::story_template_values(format, &before, "");
        let story_id = pre.get("story_id").cloned();
        let from_status = pre.get("status").cloned();

        let fields: Vec<(String, serde_json::Value)> = fields.into_iter().collect();
        let after = match format {
            PipelineFileFormat::Markdown => {
                super::frontmatter_fields::set_markdown_fields(&before, &fields)?
            }
            PipelineFileFormat::Html => {
                let mut content = before.clone();
                for (field, value) in &fields {
                    content = apply_one_edit(format, &content, &html_field_edit(field, value)?)?;
                }
                content
            }
        };
        if after == before {
            return Ok(());
        }
        write_doc_suppressed(&app, &file_path, after.as_bytes())?;
        note_written(&app, &path, &file_path);
        record_human_status_change(format, story_id, from_status, &after);
        Ok(())
    })
    .await;
    crate::context_repo::caller_scope::readback(readable, result)
}

/// Resolve a context `path` to a symlink-checked, writable target and read it,
/// returning `(file_path, format, content)`. The shared read prologue for the
/// metadata write command ([`apply_metadata_edit`]).
///
/// The symlink write-guard (FIX 1) lives in [`resolve_writable_target`], which
/// canonicalizes BEFORE the `is_installed_path` check and returns the SAME resolved
/// path fed to the write — so the guard checks the real on-disk location the write
/// will hit, with no gap. A canonicalize failure REJECTS (fail-safe). Blocking —
/// call inside `await_blocking`.
fn resolve_and_read<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &str,
) -> Result<(std::path::PathBuf, PipelineFileFormat, String), String> {
    let requested_path = crate::commands::files::validate_context_write_path_owned(path, app)?;
    let codesense_home = crate::commands::paths::codesense_home_dir()
        .ok_or_else(|| "refusing to edit metadata: codesense home dir unresolved".to_string())?;
    let file_path = resolve_writable_target(&requested_path, &codesense_home)?;
    let format = pipeline_format::format_of(&file_path);
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("failed to read {}: {e}", file_path.display()))?;
    Ok((file_path, format, content))
}

/// Watcher-suppressed atomic write — same posture as `write_content_file` (the
/// dest is inside a watched content dir; an un-suppressed write would enqueue a
/// team-wide fan-out wave). FE handles refresh after. Shared by
/// [`apply_metadata_edit`] so the atomic-write
/// mechanism has one home. Blocking — call inside `await_blocking`.
fn write_doc_suppressed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    file_path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    match app.try_state::<crate::commands::watch::WatcherState>() {
        Some(ws) => {
            let handle = ws.dispatcher.suppression_handle();
            crate::content_engine::write_tmp_fsync_rename_suppressed(file_path, bytes, &handle)
        }
        None => crate::content_engine::write_tmp_fsync_rename(file_path, bytes),
    }
    .map_err(|e| format!("failed to write {}: {e}", file_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg<'a>(repos: &'a [String], teams: &'a [String]) -> fv::ValidatorConfig<'a> {
        fv::ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
        }
    }

    // ----- 10.1 read-model: md/html parity, unknowns, is_epic ---------------

    #[test]
    fn detect_is_epic_story_with_branch_is_not_an_epic() {
        // SW-OR-1b (AC-8): a story carrying the unified `branch` must NOT be
        // classified as an epic — else the metadata band offers epic-only fields
        // on it and the `set_epic_repos_gated` safety gate is bypassed.
        let story = FrontmatterFields {
            document_type: Some("story".to_string()),
            branch: Some("feat/SW-OR-1b-foo".to_string()),
            ..Default::default()
        };
        assert!(
            !detect_is_epic(Path::new("/content/SW-OR-1b.md"), &story),
            "a story with `branch` is not an epic"
        );

        // An epic is still detected — by `type: epic`, by repos, and by main-doc name.
        let typed_epic = FrontmatterFields {
            document_type: Some("epic".to_string()),
            branch: Some("integration/EP-1".to_string()),
            ..Default::default()
        };
        assert!(detect_is_epic(Path::new("/content/EP-1.md"), &typed_epic));
        let repos_epic = FrontmatterFields {
            repos: Some(vec!["owner/repo".to_string()]),
            branch: Some("integration/EP-1".to_string()),
            ..Default::default()
        };
        assert!(detect_is_epic(Path::new("/content/EP-1.md"), &repos_epic));
        let named_epic = FrontmatterFields::default();
        assert!(detect_is_epic(Path::new("/content/README.md"), &named_epic));
    }

    #[test]
    fn md_and_html_parse_to_identical_fields() {
        let md = "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\ndepends_on:\n  - A\n  - B\n---\n# T\n";
        let html = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:status\" content=\"todo\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n<meta name=\"cs:depends-on\" content=\"A, B\">\n<title>T</title>";
        let repos = vec!["owner/repo".to_string()];
        let m = build_metadata_model(Path::new("s.md"), md, &cfg(&repos, &[]));
        let h = build_metadata_model(Path::new("s.html"), html, &cfg(&repos, &[]));
        assert_eq!(m.fields.document_type, h.fields.document_type);
        assert_eq!(m.fields.status, h.fields.status);
        assert_eq!(m.fields.source_repo, h.fields.source_repo);
        assert_eq!(m.fields.depends_on, h.fields.depends_on);
        assert_eq!(m.format, "markdown");
        assert_eq!(h.format, "html");
    }

    #[test]
    fn unknown_fields_partitioned_markdown() {
        let md = "---\ntype: story\npriority: high\ncolor: blue\n---\n# T\n";
        let model = build_metadata_model(Path::new("s.md"), md, &cfg(&[], &[]));
        assert!(model.block_parse_ok);
        let keys: Vec<&str> = model
            .unknown_fields
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert!(keys.contains(&"priority"));
        assert!(keys.contains(&"color"));
        assert!(!keys.contains(&"type"), "known key must not be unknown");
    }

    #[test]
    fn unknown_fields_partitioned_html() {
        let html = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:priority\" content=\"high\">\n";
        let model = build_metadata_model(Path::new("s.html"), html, &cfg(&[], &[]));
        assert!(model.block_parse_ok);
        assert_eq!(
            model.unknown_fields,
            vec![("cs:priority".to_string(), "high".to_string())]
        );
    }

    #[test]
    fn is_epic_by_readme_name() {
        let md = "---\ntype: epic\n---\n# Epic\n";
        let model = build_metadata_model(Path::new("some/epic/README.md"), md, &cfg(&[], &[]));
        assert!(model.is_epic);
    }

    #[test]
    fn is_epic_by_type_and_scope_fields() {
        let by_type =
            build_metadata_model(Path::new("s.md"), "---\ntype: epic\n---\n", &cfg(&[], &[]));
        assert!(by_type.is_epic);
        let by_field = build_metadata_model(
            Path::new("s.md"),
            "---\nrepos:\n  - a/b\n---\n",
            &cfg(&[], &[]),
        );
        assert!(by_field.is_epic);
        let plain =
            build_metadata_model(Path::new("s.md"), "---\ntype: story\n---\n", &cfg(&[], &[]));
        assert!(!plain.is_epic);
    }

    // ----- 10.4 malformed block ---------------------------------------------

    #[test]
    fn recovered_fields_keep_the_raw_repair_editor_and_authored_block() {
        let text = "---\ntype: story\nstory_id: REC-A\ntitle: Inbox: first pass\nreadiness_pass: yes: with notes\n---\n# Body\n";
        let model = build_metadata_model(Path::new("story.md"), text, &cfg(&[], &[]));
        assert_eq!(model.fields.story_id.as_deref(), Some("REC-A"));
        assert!(!model.block_parse_ok);
        assert_eq!(model.raw_block.as_deref(), Some("type: story\nstory_id: REC-A\ntitle: Inbox: first pass\nreadiness_pass: yes: with notes"));
        assert!(model
            .unknown_fields
            .iter()
            .any(|(k, v)| k == "title" && v == "Inbox: first pass"));
    }

    #[test]
    fn malformed_block_sets_parse_ok_false() {
        // Broken YAML shape inside a present block.
        let md = "---\ntype: story\n  : : bad\nsource_repo\n---\n# S\n";
        let model = build_metadata_model(Path::new("s.md"), md, &cfg(&[], &[]));
        assert!(!model.block_parse_ok, "malformed block → parse_ok false");
    }

    #[test]
    fn no_block_is_parse_ok_true() {
        let model = build_metadata_model(Path::new("s.md"), "# Plain doc\n", &cfg(&[], &[]));
        assert!(model.block_parse_ok);
        assert!(model.unknown_fields.is_empty());
    }

    #[test]
    fn raw_block_populated_for_malformed_markdown() {
        // Malformed YAML inside a present block → parse_ok false, but raw_block
        // MUST carry the verbatim broken YAML so the raw editor can repair it.
        let broken_yaml = "type: story\n  : : bad\nsource_repo";
        let md = format!("---\n{broken_yaml}\n---\n# S\n");
        let model = build_metadata_model(Path::new("s.md"), &md, &cfg(&[], &[]));
        assert!(!model.block_parse_ok, "malformed → parse_ok false");
        assert_eq!(
            model.raw_block.as_deref(),
            Some(broken_yaml),
            "raw_block carries the verbatim broken YAML"
        );
    }

    #[test]
    fn raw_block_matches_on_disk_frontmatter_markdown() {
        let body = "type: story\nstatus: todo\nsource_repo: owner/repo";
        let md = format!("---\n{body}\n---\n# Body\n");
        let model = build_metadata_model(Path::new("s.md"), &md, &cfg(&[], &[]));
        assert!(model.block_parse_ok);
        assert_eq!(model.raw_block.as_deref(), Some(body));
    }

    #[test]
    fn raw_block_none_without_block() {
        let model = build_metadata_model(Path::new("s.md"), "# Plain doc\n", &cfg(&[], &[]));
        assert!(model.raw_block.is_none());
    }

    #[test]
    fn raw_block_populated_for_html() {
        let html =
            "<head>\n<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:status\" content=\"todo\">\n</head>";
        let model = build_metadata_model(Path::new("s.html"), html, &cfg(&[], &[]));
        let raw = model.raw_block.expect("html raw_block present");
        assert!(raw.contains("cs:type"), "raw: {raw}");
        assert!(raw.contains("cs:status"), "raw: {raw}");
        assert!(
            !raw.contains("</head>"),
            "block is the cs:* run only: {raw}"
        );
    }

    #[test]
    fn raw_block_round_trips_through_replace_raw_block_markdown() {
        // get_metadata_model().raw_block fed back through ReplaceRawBlock is a
        // faithful round-trip (identity), even when the source block is malformed.
        let broken = "---\ntype: story\n  : : bad\n---\n# S\n";
        let model = build_metadata_model(Path::new("s.md"), broken, &cfg(&[], &[]));
        let raw = model.raw_block.clone().expect("raw_block present");
        let round_tripped = apply_one_edit(
            PipelineFileFormat::Markdown,
            broken,
            &MetadataEdit::ReplaceRawBlock { text: raw.clone() },
        )
        .unwrap();
        // Re-reading the round-tripped doc yields the SAME raw_block (identity).
        let after = build_metadata_model(Path::new("s.md"), &round_tripped, &cfg(&[], &[]));
        assert_eq!(
            after.raw_block.as_deref(),
            Some(raw.as_str()),
            "raw_block ⇄ ReplaceRawBlock is a faithful round-trip: {round_tripped}"
        );
        assert!(round_tripped.contains("# S"), "body preserved");
    }

    #[test]
    fn raw_block_round_trips_through_replace_raw_block_html() {
        let html =
            "<head>\n<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:status\" content=\"todo\">\n</head>";
        let model = build_metadata_model(Path::new("s.html"), html, &cfg(&[], &[]));
        let raw = model.raw_block.clone().expect("raw_block present");
        let round_tripped = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::ReplaceRawBlock { text: raw.clone() },
        )
        .unwrap();
        let after = build_metadata_model(Path::new("s.html"), &round_tripped, &cfg(&[], &[]));
        assert_eq!(after.raw_block.as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn replace_markdown_block_preserves_crlf_body() {
        // FIX 8: a CRLF-bodied doc must keep its CRLF body after a block replace
        // (byte-offset slicing, not `lines().join("\n")`).
        let crlf = "---\ntype: story\n---\n# Body\r\nsecond line\r\ntrailing\r\n";
        let out = replace_raw_block(PipelineFileFormat::Markdown, crlf, "type: bug");
        assert_eq!(
            out,
            "---\ntype: bug\n---\n# Body\r\nsecond line\r\ntrailing\r\n"
        );
        assert!(out.contains("\r\n"), "CRLF body preserved: {out:?}");
    }

    #[test]
    fn replace_markdown_block_preserves_trailing_blank_lines() {
        // Trailing blank lines in the body are byte-preserved (not normalized).
        let doc = "---\nstatus: todo\n---\nBody\n\n\n";
        let out = replace_raw_block(PipelineFileFormat::Markdown, doc, "status: done");
        assert_eq!(out, "---\nstatus: done\n---\nBody\n\n\n");
    }

    #[test]
    fn replace_raw_block_recovers_markdown() {
        let broken = "---\ntype: story\n  : : bad\n---\n# S\n";
        let fixed = replace_raw_block(
            PipelineFileFormat::Markdown,
            broken,
            "type: story\nsource_repo: owner/repo",
        );
        let model = build_metadata_model(
            Path::new("s.md"),
            &fixed,
            &cfg(&["owner/repo".to_string()], &[]),
        );
        assert!(model.block_parse_ok, "recovered block must parse: {fixed}");
        assert_eq!(model.fields.document_type.as_deref(), Some("story"));
        assert_eq!(model.fields.source_repo.as_deref(), Some("owner/repo"));
        assert!(fixed.contains("# S"), "body preserved");
    }

    // ----- 10.2 per-field validity (delegated to shared predicate) ----------

    #[test]
    fn validity_flows_through_shared_predicate() {
        let repos = vec!["owner/repo".to_string()];
        let bad = build_metadata_model(
            Path::new("s.md"),
            "---\nsource_repo: nope/nope\n---\n",
            &cfg(&repos, &[]),
        );
        assert!(fv::has_invalid(&bad.per_field_validity));
        let good = build_metadata_model(
            Path::new("s.md"),
            "---\nsource_repo: owner/repo\n---\n",
            &cfg(&repos, &[]),
        );
        assert!(!fv::has_invalid(&good.per_field_validity));
    }

    #[test]
    fn status_and_band_edits_preserve_authored_bytes_and_open_done_block() {
        let input = "---\n# Keep this comment\nnotes: see #42\ndone:\n  completed_at: '2026-01-01' # authored stamp\npoints: 3\ntitle: \"Quoted: title\"\nstatus: todo\ntype: story\n---\n# Body\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("story.md");
        std::fs::write(&path, input).unwrap();
        crate::commands::pipeline_scanner::write_story_status(&path, "done", None, Some("todo"))
            .unwrap();
        let moved = std::fs::read_to_string(&path).unwrap();
        let edited = apply_one_edit(
            PipelineFileFormat::Markdown,
            &moved,
            &MetadataEdit::SetField {
                field: "points".into(),
                value: "5".into(),
            },
        )
        .unwrap();
        assert_eq!(
            edited,
            input
                .replace("status: todo", "status: done")
                .replace("points: 3", "points: \"5\"")
        );
        let model = build_metadata_model(&path, &edited, &cfg(&[], &[]));
        assert!(model.block_parse_ok);
        assert!(model
            .unknown_fields
            .iter()
            .any(|(k, v)| k == "done" && v.contains("completed_at")));
        assert!(!model.per_field_validity.iter().any(|v| v.field == "done"));
    }

    // ----- Write round trips ---------------------------------------------

    fn slugs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn set_field_markdown_byte_preserving() {
        let md =
            "---\ntype: story\nstatus: todo\n# a comment\nsource_repo: owner/repo\n---\n# Body\n";
        let out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::SetField {
                field: "status".to_string(),
                value: "done".to_string(),
            },
        )
        .unwrap();
        // Byte-preserving: comment + other keys survive, only status line changed.
        assert!(out.contains("# a comment"), "comment preserved: {out}");
        assert!(out.contains("type: story"));
        assert!(out.contains("status: done"));
        assert!(!out.contains("status: todo"));
    }

    #[test]
    fn clear_field_removes_key_markdown() {
        let md = "---\ntype: story\nsource_repo: owner/repo\n---\n# B\n";
        let out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::ClearField {
                field: "source_repo".to_string(),
            },
        )
        .unwrap();
        assert!(!out.contains("source_repo"), "key removed entirely: {out}");
        assert!(out.contains("type: story"), "other keys survive");
    }

    #[test]
    fn clear_field_removes_meta_html() {
        let html = "<head>\n<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n</head>";
        let out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::ClearField {
                field: "source_repo".to_string(),
            },
        )
        .unwrap();
        assert!(!out.contains("cs:source-repo"), "meta removed: {out}");
        assert!(out.contains("cs:type"), "other meta survives");
    }

    #[test]
    fn set_field_preserves_unknown_keys() {
        let md = "---\ntype: story\npriority: high\n---\n# B\n";
        let out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::SetField {
                field: "status".to_string(),
                value: "qa".to_string(),
            },
        )
        .unwrap();
        assert!(
            out.contains("priority: high"),
            "unknown key round-trips: {out}"
        );
    }

    // ----- FIX 2: scalar value escaping (markdown YAML quote / HTML attr) ----

    #[test]
    fn setfield_markdown_yaml_significant_value_round_trips() {
        // A `:`-bearing value and a `[`-leading value must re-read exactly and
        // keep the block parseable (block_parse_ok=true).
        for (field, value) in [("story_id", "SLW: fix"), ("status", "[weird]")] {
            let md = "---\ntype: story\n---\n# B\n";
            let out = apply_one_edit(
                PipelineFileFormat::Markdown,
                md,
                &MetadataEdit::SetField {
                    field: field.to_string(),
                    value: value.to_string(),
                },
            )
            .unwrap();
            let model = build_metadata_model(Path::new("s.md"), &out, &cfg(&[], &[]));
            assert!(
                model.block_parse_ok,
                "SetField({field}, {value:?}) must keep block parseable: {out}"
            );
            let got = match field {
                "story_id" => model.fields.story_id.clone(),
                "status" => model.fields.status.clone(),
                _ => unreachable!(),
            };
            assert_eq!(
                got.as_deref(),
                Some(value),
                "exact value round-trips: {out}"
            );
        }
    }

    #[test]
    fn setfield_markdown_safe_value_stays_unquoted() {
        // A plain safe value must NOT be quoted (clean diffs).
        let md = "---\ntype: story\n---\n# B\n";
        let out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::SetField {
                field: "status".to_string(),
                value: "in-progress".to_string(),
            },
        )
        .unwrap();
        assert!(out.contains("status: in-progress"), "unquoted: {out}");
        assert!(!out.contains("\"in-progress\""));
    }

    #[test]
    fn setfield_html_special_chars_round_trip_and_valid_meta() {
        // A value with `"`/`<`/`>` must re-read exactly and produce a valid meta.
        let html = "<head>\n<meta name=\"cs:type\" content=\"story\">\n</head>";
        let out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetField {
                field: "story_id".to_string(),
                value: "a\"b<c>d&e".to_string(),
            },
        )
        .unwrap();
        // No raw `"`/`<`/`>` inside the content attribute value — escaped form.
        assert!(out.contains("cs:story-id"), "meta written: {out}");
        assert!(out.contains("&quot;") && out.contains("&lt;") && out.contains("&gt;"));
        let model = build_metadata_model(Path::new("s.html"), &out, &cfg(&[], &[]));
        assert_eq!(model.fields.story_id.as_deref(), Some("a\"b<c>d&e"));
    }

    #[test]
    fn setfield_rejects_control_chars_both_formats() {
        let bad = MetadataEdit::SetField {
            field: "status".to_string(),
            value: "line1\nline2".to_string(),
        };
        assert!(apply_one_edit(PipelineFileFormat::Markdown, "---\n---\n", &bad).is_err());
        assert!(apply_one_edit(PipelineFileFormat::Html, "<head></head>", &bad).is_err());
    }

    // ----- FIX 3: HTML epic list/scalar fields write (not a no-op) -----------

    #[test]
    fn html_epic_setlist_repos_writes_and_reads_back() {
        let html = "<head>\n<meta name=\"cs:type\" content=\"epic\">\n</head>";
        let out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetList {
                field: "repos".to_string(),
                items: slugs(&["a/b", "c/d"]),
            },
        )
        .unwrap();
        assert_ne!(out, html, "HTML epic SetList(repos) must not be a no-op");
        assert!(out.contains("cs:repos"), "cs:repos written: {out}");
        let model = build_metadata_model(Path::new("e.html"), &out, &cfg(&[], &[]));
        assert_eq!(model.fields.repos, Some(slugs(&["a/b", "c/d"])));
    }

    #[test]
    fn html_epic_setfield_branch_round_trips() {
        let html = "<head>\n<meta name=\"cs:type\" content=\"epic\">\n</head>";
        let out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetField {
                field: "branch".to_string(),
                value: "integration/x".to_string(),
            },
        )
        .unwrap();
        assert!(out.contains("cs:branch"), "cs:branch written: {out}");
        let model = build_metadata_model(Path::new("e.html"), &out, &cfg(&[], &[]));
        assert_eq!(model.fields.branch.as_deref(), Some("integration/x"));
    }

    // ----- FIX 5: HTML/markdown SetList empty/whitespace item rejected -------

    #[test]
    fn setlist_empty_or_whitespace_item_rejected_both_formats() {
        let bad = MetadataEdit::SetList {
            field: "depends_on".to_string(),
            items: slugs(&["a", " ", "b"]),
        };
        assert!(apply_one_edit(PipelineFileFormat::Markdown, "---\n---\n", &bad).is_err());
        assert!(apply_one_edit(PipelineFileFormat::Html, "<head></head>", &bad).is_err());
    }

    // ----- FIX 7: ClearField on an unknown nested-map key --------------------

    #[test]
    fn clear_field_removes_nested_map_block_markdown() {
        // Clearing `owner:` whose value is a nested map removes the whole block,
        // leaving a parseable doc.
        let md = "---\ntype: story\nowner:\n  name: x\n  id: 1\nstatus: todo\n---\n# B\n";
        let out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::ClearField {
                field: "owner".to_string(),
            },
        )
        .unwrap();
        assert!(
            !out.contains("owner"),
            "nested-map key + children removed: {out}"
        );
        assert!(!out.contains("name: x"));
        let model = build_metadata_model(Path::new("s.md"), &out, &cfg(&[], &[]));
        assert!(model.block_parse_ok, "block still parseable: {out}");
        assert_eq!(model.fields.status.as_deref(), Some("todo"));
    }

    // ----- 10.3b cross-format list parity fuzz (gaps #1 / #2) ---------------

    #[test]
    fn setlist_parity_md_and_html_read_back_same_items() {
        let items = slugs(&["SLW-RI-1", "SLW-RI-2", "SLW-MD-1"]);
        let md = "---\ntype: story\n---\n# B\n";
        let html = "<head>\n<meta name=\"cs:type\" content=\"story\">\n</head>";
        let md_out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::SetList {
                field: "depends_on".to_string(),
                items: items.clone(),
            },
        )
        .unwrap();
        let html_out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetList {
                field: "depends_on".to_string(),
                items: items.clone(),
            },
        )
        .unwrap();
        let md_fields = pipeline_format::extract_meta(PipelineFileFormat::Markdown, &md_out);
        let html_fields = pipeline_format::extract_meta(PipelineFileFormat::Html, &html_out);
        assert_eq!(md_fields.depends_on, Some(items.clone()));
        assert_eq!(
            html_fields.depends_on,
            Some(items),
            "HTML SetList must actually write (not a no-op): {html_out}"
        );
    }

    #[test]
    fn setlist_html_actually_writes_not_noop() {
        // Guards Write-path gap #1: the markdown-only `rewrite_list_field`
        // would leave HTML unchanged; our CSV-join path must change it.
        let html = "<head>\n<meta name=\"cs:type\" content=\"story\">\n</head>";
        let out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetList {
                field: "depends_on".to_string(),
                items: slugs(&["X-1"]),
            },
        )
        .unwrap();
        assert_ne!(out, html, "HTML SetList must not be a no-op");
        assert!(out.contains("cs:depends-on"));
    }

    #[test]
    fn setlist_comma_item_rejected_both_formats() {
        let bad = MetadataEdit::SetList {
            field: "depends_on".to_string(),
            items: slugs(&["ok", "has,comma"]),
        };
        assert!(apply_one_edit(PipelineFileFormat::Markdown, "---\n---\n", &bad).is_err());
        assert!(apply_one_edit(PipelineFileFormat::Html, "<head></head>", &bad).is_err());
    }

    #[test]
    fn setlist_empty_clears_both_formats() {
        let md = "---\ndepends_on:\n  - A\n---\n# B\n";
        let md_out = apply_one_edit(
            PipelineFileFormat::Markdown,
            md,
            &MetadataEdit::SetList {
                field: "depends_on".to_string(),
                items: vec![],
            },
        )
        .unwrap();
        assert!(!md_out.contains("depends_on"));

        let html = "<head>\n<meta name=\"cs:depends-on\" content=\"A\">\n</head>";
        let html_out = apply_one_edit(
            PipelineFileFormat::Html,
            html,
            &MetadataEdit::SetList {
                field: "depends_on".to_string(),
                items: vec![],
            },
        )
        .unwrap();
        assert!(!html_out.contains("cs:depends-on"));
    }

    // ----- FIX 1: symlink write-guard bypass ---------------------------------

    #[cfg(unix)]
    #[test]
    fn resolve_writable_target_rejects_symlink_escape_into_install() {
        // A symlink `content/evil -> ../install` inside the user's own content
        // root: an edit through `…/content/evil/foo.md` shows the LEXICAL guard a
        // clean content-root window, but the OS resolves it into the read-only
        // `…/install/` tree. Canonicalize-before-guard must REJECT it.
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        // Realistic layout: …/{team}/content and …/{team}/install.
        let team = tmp.path().join("_team").join("default");
        let content = team.join("content");
        let install = team.join("install");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::create_dir_all(&install).unwrap();
        // The real (read-only) target the symlink escapes into.
        std::fs::write(install.join("foo.md"), "---\ntype: story\n---\n# X\n").unwrap();

        // Plant `content/evil -> ../install`.
        symlink("../install", content.join("evil")).unwrap();

        // Edit path threads through the symlink into the install tree → REJECT.
        let through_symlink = content.join("evil").join("foo.md");
        let err = resolve_writable_target(&through_symlink, tmp.path())
            .expect_err("symlink escape into install must be rejected");
        assert!(
            err.contains("read-only projected/installed view"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_writable_target_allows_normal_content_file() {
        // A plain `…/content/foo.md` (no symlink games) resolves to a real
        // content-root file and is allowed.
        let tmp = tempfile::tempdir().unwrap();
        // SLW-IN-11: realistic layout under the codesense home — the writable
        // content root anchors on `{home}/users/{uid}/orgs/{owner}/{repo}/{team}/content`.
        let content = tmp
            .path()
            .join("users")
            .join("42")
            .join("orgs")
            .join("acme")
            .join("api")
            .join("default")
            .join("content");
        std::fs::create_dir_all(&content).unwrap();
        let doc = content.join("foo.md");
        std::fs::write(&doc, "---\ntype: story\n---\n# X\n").unwrap();

        let resolved = resolve_writable_target(&doc, tmp.path())
            .expect("a normal content-root file must be writable");
        // The resolved path still ends in the content-root window.
        assert!(resolved.ends_with("default/content/foo.md"), "{resolved:?}");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_writable_target_allows_content_under_symlinked_ancestor() {
        // The codesense home / content root reached through a symlinked ANCESTOR
        // must still pass: canonicalization resolves the ancestor on BOTH the file
        // path and the home, so the `strip_prefix` anchor still matches and the
        // `users/{uid}/orgs/…/{team}/content` window is recognized as writable.
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let real_home = tmp.path().join("real_home");
        let content = real_home
            .join("users")
            .join("42")
            .join("orgs")
            .join("acme")
            .join("api")
            .join("default")
            .join("content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(content.join("foo.md"), "---\ntype: story\n---\n# X\n").unwrap();

        // A symlinked ancestor pointing at the real home dir.
        let linked_home = tmp.path().join("linked_home");
        symlink(&real_home, &linked_home).unwrap();

        // Reach the content file THROUGH the symlinked ancestor; pass the SAME
        // symlinked home so both canonicalize to `real_home`.
        let via_link = linked_home
            .join("users")
            .join("42")
            .join("orgs")
            .join("acme")
            .join("api")
            .join("default")
            .join("content")
            .join("foo.md");
        let resolved = resolve_writable_target(&via_link, &linked_home)
            .expect("content reached via a symlinked ancestor must still be writable");
        assert!(
            resolved.ends_with("default/content/foo.md"),
            "resolved into the real content root: {resolved:?}"
        );
    }

    #[test]
    fn resolve_writable_target_rejects_nonexistent_path() {
        // Fail-safe: a path that can't be canonicalized (does not exist) is
        // REJECTED, never waved through on the raw path.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("_team/default/content/nope.md");
        let err = resolve_writable_target(&missing, tmp.path())
            .expect_err("nonexistent path must be rejected");
        assert!(err.contains("cannot resolve"), "unexpected error: {err}");
    }

    // ----- configured_sets helper -------------------------------------------

    #[test]
    fn configured_sets_active_team_only() {
        use crate::domain::{SourceRepoSlug, Team};
        let teams = vec![
            Team::new("default", vec![SourceRepoSlug::new("owner", "a").unwrap()]).unwrap(),
            Team::new("other", vec![SourceRepoSlug::new("owner", "b").unwrap()]).unwrap(),
        ];
        let (repos, team_names) = configured_sets(&teams, Some("default"));
        assert_eq!(repos, vec!["owner/a".to_string()]);
        assert_eq!(team_names, vec!["default".to_string(), "other".to_string()]);
    }

    // ===== SW-DX-1: schema-tag open extraction + carry-through ==============

    use crate::domain::{TagDef, TagSchema, TagType};

    fn tag(name: &str, ty: TagType, allowed: &[&str]) -> TagDef {
        TagDef {
            name: name.to_string(),
            r#type: ty,
            allowed_values: allowed.iter().map(|s| s.to_string()).collect(),
            color: None,
            icon: None,
        }
    }

    fn find<'a>(vs: &'a [FieldValidity], field: &str) -> Option<&'a FieldValidity> {
        vs.iter().find(|v| v.field == field)
    }

    #[test]
    fn open_tag_values_markdown_scalar_and_list() {
        let md = "---\ntype: story\ncontext_level: team\nowners:\n  - alice\n  - bob\n---\n# B\n";
        let vals = open_tag_values(PipelineFileFormat::Markdown, md);
        // Known key `type` excluded; open keys kept with structured values.
        assert!(!vals.iter().any(|(k, _)| k == "type"), "known key excluded");
        let cl = vals.iter().find(|(k, _)| k == "context_level").unwrap();
        assert_eq!(cl.1, vec!["team".to_string()]);
        let owners = vals.iter().find(|(k, _)| k == "owners").unwrap();
        assert_eq!(owners.1, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn open_tag_values_excludes_tags_on_both_formats() {
        // SW-AU-77F (AC-4): `tags` is a named field now, so it must NOT also appear as an
        // open tag — on markdown (KNOWN_TOP_LEVEL_KEYS) or HTML (known_cs_names), keeping
        // the exclusion symmetric and preventing the double-capture into `declared`.
        let md = "---\ntype: story\ntags:\n  - infra\n  - urgent\n---\n# B\n";
        let md_vals = open_tag_values(PipelineFileFormat::Markdown, md);
        assert!(
            !md_vals.iter().any(|(k, _)| k == "tags"),
            "markdown `tags:` is excluded from open tags"
        );

        let html = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:tags\" content=\"infra, urgent\">\n";
        let html_vals = open_tag_values(PipelineFileFormat::Html, html);
        assert!(
            !html_vals.iter().any(|(k, _)| k == "tags"),
            "HTML `cs:tags` is excluded from open tags"
        );
    }

    #[test]
    fn gate_evidence_is_never_an_open_tag() {
        let md = "---\ntype: story\ngate_evidence:\n  - {\"step\":\"dev\",\"verdict\":\"passed\"}\n---\n# B\n";
        assert!(!open_tag_values(PipelineFileFormat::Markdown, md)
            .iter()
            .any(|(k, _)| k == "gate_evidence"));
        let html = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:gate-evidence\" content=\"[]\">\n";
        assert!(open_tag_values(PipelineFileFormat::Html, html).is_empty());
    }

    #[test]
    fn open_tag_values_html_normalizes_cs_name() {
        let html = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:context-level\" content=\"team\">\n";
        let vals = open_tag_values(PipelineFileFormat::Html, html);
        // cs:type is known → excluded; cs:context-level → underscore tag name.
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].0, "context_level");
        assert_eq!(vals[0].1, vec!["team".to_string()]);
    }

    #[test]
    fn schema_tag_validity_carried_through_and_open_key_untouched() {
        // AC-7 + AC-8: the schema tag validates (badge on bad value), the
        // arbitrary open key is neither validated nor stripped, and the existing
        // five closed-field entries are all still present + unchanged.
        let schema = TagSchema {
            tags: vec![
                tag(
                    "context_level",
                    TagType::Ordered,
                    &["machine", "team", "management", "leadership"],
                ),
                tag("priority", TagType::Select, &["low", "high"]),
            ],
        };
        let md =
            "---\ntype: story\ncontext_level: cosmic\npriority: low\narbitrary: keepme\n---\n# B\n";
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &schema,
            &crate::commands::field_schema::default_schema(),
        );

        // Existing constrained fields still have validity entries.
        for f in ["source_repo", "type", "status", "team"] {
            assert!(find(&model.per_field_validity, f).is_some(), "missing {f}");
        }
        // Bad ordered value badges as INVALID_TAG_VALUE.
        let cl = find(&model.per_field_validity, "context_level").expect("context_level entry");
        assert_eq!(cl.state, fv::FieldState::Invalid);
        assert_eq!(cl.code.as_deref(), Some(fv::INVALID_TAG_VALUE));
        // In-vocab select is Valid.
        let pr = find(&model.per_field_validity, "priority").expect("priority entry");
        assert_eq!(pr.state, fv::FieldState::Valid);
        // Arbitrary open key: NO validity entry, and round-trips in unknown_fields.
        assert!(find(&model.per_field_validity, "arbitrary").is_none());
        assert!(model
            .unknown_fields
            .iter()
            .any(|(k, v)| k == "arbitrary" && v == "keepme"));
    }

    #[test]
    fn empty_schema_adds_no_tag_entries() {
        // With no schema, only the four constrained fields have validity entries.
        let md = "---\ntype: story\ncontext_level: cosmic\n---\n# B\n";
        let model = build_metadata_model(Path::new("s.md"), md, &cfg(&[], &[]));
        assert_eq!(model.per_field_validity.len(), 4);
        assert!(find(&model.per_field_validity, "context_level").is_none());
    }

    // ===== SW-AU-77G: declared-field validity + first-class rows =============

    fn fdef(name: &str, ty: &str, allowed: &[&str]) -> crate::commands::field_schema::FieldDef {
        crate::commands::field_schema::FieldDef {
            name: name.to_string(),
            r#type: Some(ty.to_string()),
            allowed_values: allowed.iter().map(|s| s.to_string()).collect(),
            required: false,
            official: false,
        }
    }

    fn fschema(customs: Vec<crate::commands::field_schema::FieldDef>) -> FieldSchema {
        let mut s = crate::commands::field_schema::default_schema();
        s.fields.extend(customs);
        s
    }

    #[test]
    fn declared_bad_value_flags_and_graduates_out_of_unknown_markdown() {
        // AC-1/2/7: a present declared custom with a bad value badges via
        // per_field_validity, is projected as a row, and leaves unknown_fields.
        let md = "---\ntype: story\nrisk: cosmic\narbitrary: keepme\n---\n# B\n";
        let fs = fschema(vec![fdef("risk", "choice", &["low", "high"])]);
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &TagSchema::default(),
            &fs,
        );
        let risk = find(&model.per_field_validity, "risk").expect("risk validity");
        assert_eq!(risk.state, fv::FieldState::Invalid);
        assert_eq!(risk.code.as_deref(), Some(fv::INVALID_TAG_VALUE));
        // Projected as a row with its value + declared render config.
        let row = model
            .schema_rows
            .iter()
            .find(|d| d.name == "risk")
            .expect("declared row");
        assert_eq!(row.values, vec!["cosmic".to_string()]);
        assert_eq!(row.source, RowSource::Team);
        assert_eq!(row.r#type.as_deref(), Some("choice"));
        assert_eq!(
            row.allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
        // Graduated OUT of the unknown disclosure; the truly-unknown key stays.
        assert!(!model.unknown_fields.iter().any(|(k, _)| k == "risk"));
        assert!(model.unknown_fields.iter().any(|(k, _)| k == "arbitrary"));
    }

    #[test]
    fn declared_good_value_row_present_no_flag() {
        let md = "---\ntype: story\nrisk: low\n---\n# B\n";
        let fs = fschema(vec![fdef("risk", "choice", &["low", "high"])]);
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &TagSchema::default(),
            &fs,
        );
        assert_eq!(
            find(&model.per_field_validity, "risk").unwrap().state,
            fv::FieldState::Valid
        );
        assert!(model.schema_rows.iter().any(|d| d.name == "risk"));
        assert!(!model.unknown_fields.iter().any(|(k, _)| k == "risk"));
    }

    #[test]
    fn declared_graduates_out_of_unknown_html() {
        // AC-8: HTML `cs:*` custom fields graduate identically (cs:/dash normalized).
        let html =
            "<meta name=\"cs:risk\" content=\"cosmic\">\n<meta name=\"cs:foo\" content=\"x\">";
        let fs = fschema(vec![fdef("risk", "choice", &["low"])]);
        let model = build_metadata_model_with_schema(
            Path::new("s.html"),
            html,
            &cfg(&[], &[]),
            &TagSchema::default(),
            &fs,
        );
        assert_eq!(
            find(&model.per_field_validity, "risk").unwrap().state,
            fv::FieldState::Invalid
        );
        assert!(model.schema_rows.iter().any(|d| d.name == "risk"));
        assert!(!model.unknown_fields.iter().any(|(k, _)| k.contains("risk")));
        assert!(model.unknown_fields.iter().any(|(k, _)| k.contains("foo")));
    }

    #[test]
    fn org_tag_wins_collision_over_declared_field() {
        // AC-6: a name in BOTH schemas is validated once (org tag) and renders as
        // EXACTLY ONE `source: Org` row — never a team row for that name (the org
        // tag is resolved first, mirroring the org-wins validity dedup).
        let md = "---\ntype: story\nrisk: cosmic\n---\n# B\n";
        let tags = TagSchema {
            tags: vec![tag("risk", TagType::Select, &["low", "high"])],
        };
        let fs = fschema(vec![fdef("risk", "choice", &["low"])]);
        let model =
            build_metadata_model_with_schema(Path::new("s.md"), md, &cfg(&[], &[]), &tags, &fs);
        assert_eq!(
            model
                .per_field_validity
                .iter()
                .filter(|v| v.field == "risk")
                .count(),
            1,
            "validated once"
        );
        let risk_rows: Vec<&SchemaFieldRow> = model
            .schema_rows
            .iter()
            .filter(|d| d.name == "risk")
            .collect();
        assert_eq!(risk_rows.len(), 1, "exactly one row for the collided name");
        assert_eq!(
            risk_rows[0].source,
            RowSource::Org,
            "the org tag owns the row"
        );
        // The org tag's closed set (low/high) rode through, not the team field's (low).
        assert_eq!(
            risk_rows[0].allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
    }

    // ===== SW-AU-77I: org TagSchema tags as first-class rows ================

    #[test]
    fn org_tag_becomes_row_with_identity_and_graduates_markdown() {
        // AC-1/2/7/8: a present org tag renders as one `source: Org` row carrying
        // its color/icon + wire type, graduates out of unknown_fields, and its bad
        // value still flags via the already-computed validity.
        let tags = TagSchema {
            tags: vec![TagDef {
                name: "context_level".to_string(),
                r#type: TagType::Ordered,
                allowed_values: vec!["low".to_string(), "high".to_string()],
                color: Some("--cs-tag-ordered".to_string()),
                icon: Some("Gauge".to_string()),
            }],
        };
        let md = "---\ntype: story\ncontext_level: cosmic\narbitrary: keepme\n---\n# B\n";
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &tags,
            &crate::commands::field_schema::default_schema(),
        );
        let row = model
            .schema_rows
            .iter()
            .find(|r| r.name == "context_level")
            .expect("org row");
        assert_eq!(row.source, RowSource::Org);
        assert_eq!(row.values, vec!["cosmic".to_string()]);
        assert_eq!(row.r#type.as_deref(), Some("ordered"));
        assert_eq!(
            row.allowed_values,
            vec!["low".to_string(), "high".to_string()]
        );
        assert_eq!(row.color.as_deref(), Some("--cs-tag-ordered"));
        assert_eq!(row.icon.as_deref(), Some("Gauge"));
        // Graduated OUT of the unknown disclosure; the truly-unknown key stays.
        assert!(!model
            .unknown_fields
            .iter()
            .any(|(k, _)| k == "context_level"));
        assert!(model.unknown_fields.iter().any(|(k, _)| k == "arbitrary"));
        // The already-computed validity flags the out-of-ladder value (no new validator).
        assert_eq!(
            find(&model.per_field_validity, "context_level")
                .unwrap()
                .state,
            fv::FieldState::Invalid
        );
    }

    #[test]
    fn org_tag_graduates_out_of_unknown_html_cs_prefix() {
        // AC-7: an HTML `cs:context-level` key canonicalizes to `context_level`
        // and graduates identically (the md/HTML parity landmine).
        let tags = TagSchema {
            tags: vec![tag("context_level", TagType::Ordered, &["low", "high"])],
        };
        let html =
            "<meta name=\"cs:context-level\" content=\"cosmic\">\n<meta name=\"cs:foo\" content=\"x\">";
        let model = build_metadata_model_with_schema(
            Path::new("s.html"),
            html,
            &cfg(&[], &[]),
            &tags,
            &crate::commands::field_schema::default_schema(),
        );
        let row = model
            .schema_rows
            .iter()
            .find(|r| r.name == "context_level")
            .expect("org row from cs: key");
        assert_eq!(row.source, RowSource::Org);
        assert!(!model
            .unknown_fields
            .iter()
            .any(|(k, _)| k.contains("context")));
        assert!(model.unknown_fields.iter().any(|(k, _)| k.contains("foo")));
    }

    #[test]
    fn org_multi_select_row_values_parity_md_vs_html() {
        // AC-4: a `multi_select` tag's per-item `values` are identical whether the
        // doc uses a markdown list or an HTML CSV — via the shared split.
        let tags = TagSchema {
            tags: vec![tag("owners", TagType::MultiSelect, &["alice", "bob"])],
        };
        let default = crate::commands::field_schema::default_schema();
        let md = "---\ntype: story\nowners:\n  - alice\n  - bob\n---\n# B\n";
        let html = "<meta name=\"cs:owners\" content=\"alice, bob\">";
        let md_model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &tags,
            &default,
        );
        let html_model = build_metadata_model_with_schema(
            Path::new("s.html"),
            html,
            &cfg(&[], &[]),
            &tags,
            &default,
        );
        let md_row = md_model
            .schema_rows
            .iter()
            .find(|r| r.name == "owners")
            .expect("md owners row");
        let html_row = html_model
            .schema_rows
            .iter()
            .find(|r| r.name == "owners")
            .expect("html owners row");
        assert_eq!(md_row.values, vec!["alice".to_string(), "bob".to_string()]);
        assert_eq!(md_row.values, html_row.values, "md list and HTML CSV agree");
    }

    #[test]
    fn repeated_html_org_tag_key_yields_one_row() {
        // AC (landmine): a doubled `cs:owners` yields exactly one row (first-wins),
        // matching the one-badge validity dedup.
        let tags = TagSchema {
            tags: vec![tag("owners", TagType::MultiSelect, &["alice", "bob"])],
        };
        let html =
            "<meta name=\"cs:owners\" content=\"alice\">\n<meta name=\"cs:owners\" content=\"bob\">";
        let model = build_metadata_model_with_schema(
            Path::new("s.html"),
            html,
            &cfg(&[], &[]),
            &tags,
            &crate::commands::field_schema::default_schema(),
        );
        assert_eq!(
            model
                .schema_rows
                .iter()
                .filter(|r| r.name == "owners")
                .count(),
            1
        );
    }

    #[test]
    fn rows_ordered_team_then_org() {
        // AC-9: team rows render before org rows regardless of doc order.
        let tags = TagSchema {
            tags: vec![tag("priority", TagType::Select, &["p0", "p1"])],
        };
        let fs = fschema(vec![fdef("risk", "choice", &["low", "high"])]);
        // Doc order is org-then-team; the projection must regroup team-first.
        let md = "---\ntype: story\npriority: p0\nrisk: low\n---\n# B\n";
        let model =
            build_metadata_model_with_schema(Path::new("s.md"), md, &cfg(&[], &[]), &tags, &fs);
        let names: Vec<&str> = model.schema_rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["risk", "priority"], "team row before org row");
        let sources: Vec<RowSource> = model.schema_rows.iter().map(|r| r.source).collect();
        assert_eq!(sources, vec![RowSource::Team, RowSource::Org]);
    }

    #[test]
    fn org_repeated_key_first_empty_yields_no_unflagged_row() {
        // Regression (code-review): the row must bind the SAME occurrence the verdict
        // does. A repeated HTML key whose first value is empty stays Absent on both —
        // it must never bind a row to a LATER bad value that carries no flag.
        let tags = TagSchema {
            tags: vec![tag("context_level", TagType::Ordered, &["low", "high"])],
        };
        let html = "<meta name=\"cs:context-level\" content=\"\">\n<meta name=\"cs:context-level\" content=\"cosmic\">";
        let model = build_metadata_model_with_schema(
            Path::new("s.html"),
            html,
            &cfg(&[], &[]),
            &tags,
            &crate::commands::field_schema::default_schema(),
        );
        assert!(
            !model.schema_rows.iter().any(|r| r.name == "context_level"),
            "no row bound to the later, unflagged value"
        );
        // The verdict is Absent (first occurrence empty), never a silent Invalid.
        assert!(find(&model.per_field_validity, "context_level")
            .map(|v| v.state != fv::FieldState::Invalid)
            .unwrap_or(true));
    }

    #[test]
    fn org_scalar_tag_with_list_value_takes_first_only() {
        // AC-4: a scalar (non-multi_select) org tag carries a 1-element `values`, even
        // when the doc authored it as a YAML list — never the whole list (which the FE
        // would then truncate on edit).
        let tags = TagSchema {
            tags: vec![tag("context_level", TagType::Ordered, &["low", "high"])],
        };
        let md = "---\ntype: story\ncontext_level:\n  - low\n  - high\n---\n# B\n";
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &tags,
            &crate::commands::field_schema::default_schema(),
        );
        let row = model
            .schema_rows
            .iter()
            .find(|r| r.name == "context_level")
            .expect("org row");
        assert_eq!(
            row.values,
            vec!["low".to_string()],
            "scalar takes first only"
        );
    }

    #[test]
    fn no_declared_rows_without_custom_schema() {
        // Regression: default (official-only) schema ⇒ no declared rows, unknown
        // keys untouched.
        let md = "---\ntype: story\nrisk: cosmic\n---\n# B\n";
        let model = build_metadata_model(Path::new("s.md"), md, &cfg(&[], &[]));
        assert!(model.schema_rows.is_empty());
        assert!(model.unknown_fields.iter().any(|(k, _)| k == "risk"));
    }

    // ===== SW-AU-77J: declarable-absent projection ==========================

    fn fdef_required(name: &str, ty: &str) -> crate::commands::field_schema::FieldDef {
        crate::commands::field_schema::FieldDef {
            required: true,
            ..fdef(name, ty, &[])
        }
    }

    #[test]
    fn declarable_lists_absent_customs_excludes_present_and_org_tags() {
        // AC-1/AC-3: an absent declared custom is offered; a present one (a declared
        // row) is NOT; an org-tag-owned name rides SW-AU-77I, not this menu.
        let md = "---\ntype: story\nrisk: high\n---\n# B\n";
        let fs = fschema(vec![
            fdef("risk", "choice", &["low", "high"]), // present → a declared row
            fdef("effort", "number", &[]),            // absent → declarable
            fdef("owner", "text", &[]),               // absent → declarable
            fdef("priority", "choice", &["p0", "p1"]), // absent BUT org-tag name → excluded
        ]);
        let tags = TagSchema {
            tags: vec![tag("priority", TagType::Select, &["p0", "p1"])],
        };
        let model =
            build_metadata_model_with_schema(Path::new("s.md"), md, &cfg(&[], &[]), &tags, &fs);

        let names: Vec<&str> = model
            .declarable_fields
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert!(names.contains(&"effort"), "absent custom is offered");
        assert!(names.contains(&"owner"), "absent custom is offered");
        assert!(!names.contains(&"risk"), "present field is not offered");
        assert!(
            !names.contains(&"priority"),
            "org-tag-owned name rides 77I, not here"
        );
        // The offered entry carries the declared render config (empty-row control).
        let effort = model
            .declarable_fields
            .iter()
            .find(|d| d.name == "effort")
            .expect("effort declarable");
        assert_eq!(effort.r#type.as_deref(), Some("number"));
        assert!(effort.allowed_values.is_empty());
        assert!(!effort.required);
    }

    #[test]
    fn declarable_required_field_carries_flag() {
        // AC-4: a required absent field is offered with `required: true` (a "worth
        // adding" hint) — never a doc-level block.
        let md = "---\ntype: story\n---\n# B\n";
        let fs = fschema(vec![fdef_required("approval", "text")]);
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &TagSchema::default(),
            &fs,
        );
        let approval = model
            .declarable_fields
            .iter()
            .find(|d| d.name == "approval")
            .expect("approval declarable");
        assert!(approval.required, "required flag rides the offered field");
    }

    #[test]
    fn declarable_empty_without_custom_schema() {
        // Regression: the official-only baseline declares no customs ⇒ nothing to add.
        let md = "---\ntype: story\n---\n# B\n";
        let model = build_metadata_model(Path::new("s.md"), md, &cfg(&[], &[]));
        assert!(model.declarable_fields.is_empty());
    }

    #[test]
    fn declared_mixed_case_markdown_key_matches_and_graduates() {
        // A markdown key authored in mixed case (`Risk`) resolves the canonical
        // declared `risk` — the row/validity use the canonical name, and the raw
        // mixed-case key still graduates out of the unknown disclosure.
        let md = "---\ntype: story\nRisk: cosmic\n---\n# B\n";
        let fs = fschema(vec![fdef("risk", "choice", &["low", "high"])]);
        let model = build_metadata_model_with_schema(
            Path::new("s.md"),
            md,
            &cfg(&[], &[]),
            &TagSchema::default(),
            &fs,
        );
        let row = model
            .schema_rows
            .iter()
            .find(|d| d.name == "risk")
            .expect("canonical-named declared row");
        assert_eq!(row.values, vec!["cosmic".to_string()]);
        assert_eq!(
            find(&model.per_field_validity, "risk").unwrap().state,
            fv::FieldState::Invalid
        );
        assert!(
            !model
                .unknown_fields
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("risk")),
            "the mixed-case raw key graduated out of unknown_fields"
        );
    }

    #[test]
    fn resolve_tag_schema_none_anchors_is_empty() {
        // AC-2: None/empty anchors ⇒ empty schema, no I/O, no error.
        assert!(resolve_tag_schema(None, None).is_empty());
        assert!(resolve_tag_schema(Some("/x"), None).is_empty());
        assert!(resolve_tag_schema(None, Some("slug")).is_empty());
    }
}

#[cfg(test)]
mod attached_context_tests {
    use super::*;

    #[test]
    fn attached_context_metadata_target_is_writable_but_install_and_escape_are_not() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = crate::context_repo::context_attachments::tests::checkout(
            temp.path(),
            "content/repos/checkout",
            "acme/context",
        );
        for layer in ["content", "install"] {
            std::fs::create_dir_all(root.join("default").join(layer)).unwrap();
            std::fs::write(
                root.join("default").join(layer).join("story.md"),
                "saved work",
            )
            .unwrap();
        }
        crate::context_repo::context_attachments::attach(
            &home,
            &crate::domain::Principal::local_guest(),
            &root,
        )
        .unwrap();
        assert_eq!(
            resolve_writable_target(&root.join("default/content/story.md"), &home).unwrap(),
            root.join("default/content/story.md")
        );
        assert!(resolve_writable_target(&root.join("default/install/story.md"), &home).is_err());
        assert!(
            resolve_writable_target(&root.join("default/content/../install/story.md"), &home)
                .is_err()
        );
    }
}

#[cfg(all(test, feature = "dashboard-publish"))]
mod dashboard_action_tests {
    use super::*;
    #[test]
    fn dashboard_publish_fixed_fields_preserve_other_metadata_and_body() {
        let action=serde_json::from_value(serde_json::json!({"verb":"setFields","who":"owner","fields":{"status":"wontfix","tags":"urgent","estimate":3,"approved":true,"owner":null}})).unwrap();
        let args = crate::dashboard_publish::actions::Args {
            id: Some("BUG-1".into()),
            choice: None,
        };
        let fields = crate::dashboard_publish::actions::fixed_fields(&action, &args)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        let before="---\nstory_id: BUG-1\nstatus: open\ntags:\n  - regression\nowner: alice\nkeep: value # keep\n---\n# A bug\n\nUnchanged body.\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bug.md");
        std::fs::write(&path, before).unwrap();
        let after = crate::commands::frontmatter_fields::set_markdown_fields(
            &std::fs::read_to_string(&path).unwrap(),
            &fields,
        )
        .unwrap();
        std::fs::write(&path, after).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("keep: value # keep"));
        assert!(saved.ends_with("# A bug\n\nUnchanged body.\n"));
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(saved.split("---").nth(1).unwrap()).unwrap();
        assert_eq!(yaml["status"].as_str(), Some("wontfix"));
        assert_eq!(yaml["tags"].as_str(), Some("urgent"));
        assert_eq!(yaml["estimate"].as_i64(), Some(3));
        assert_eq!(yaml["approved"].as_bool(), Some(true));
        assert!(yaml.get("owner").is_none());
        assert!(html_field_edit("owner", &serde_json::json!("alice")).is_err());
    }
}
