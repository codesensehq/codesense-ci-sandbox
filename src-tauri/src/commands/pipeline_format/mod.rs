// SPDX-License-Identifier: MIT
//! Pipeline file format detection and dispatch.
//!
//! Each supported format (`markdown`, `html`) implements the same 4-function
//! surface. `mod.rs` provides the enum, extension registry, and dispatch
//! functions that route to the correct format module.
//!
//! Adding a new format requires:
//!   1. A new module implementing the 4-function surface
//!   2. A new `PipelineFileFormat` variant + match arms in each dispatch fn
//!   3. Extension(s) added to `PIPELINE_EXTENSIONS`

pub mod fields;
pub mod html;
pub mod markdown;

pub use fields::{
    contract_fields, known_cs_names, StoryField, FIELD_NAME_MAP, KNOWN_TOP_LEVEL_KEYS,
    METADATA_CONTRACT_FIELDS, STORY_FIELDS,
};

use log::debug;
use serde::{Deserialize, Serialize};

use crate::domain::{derive_integration_branch, SourceRepoSlug};

/// Supported pipeline file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineFileFormat {
    Markdown,
    Html,
}

/// Extensions accepted as pipeline documents.
/// Keep in sync with `PIPELINE_EXTENSIONS` in `pipeline-file-filter.ts`.
pub const PIPELINE_EXTENSIONS: &[&str] = &["md", "html", "htm"];

/// Detect format from a file extension (without leading dot).
/// Returns `None` for unrecognized extensions.
pub fn detect_format(extension: &str) -> Option<PipelineFileFormat> {
    match extension.to_ascii_lowercase().as_str() {
        "md" => Some(PipelineFileFormat::Markdown),
        "html" | "htm" => Some(PipelineFileFormat::Html),
        _ => None,
    }
}

/// Canonical "path → format" resolver: [`detect_format`] on the path's
/// extension, defaulting to [`PipelineFileFormat::Markdown`] for a missing /
/// unrecognized extension. The single home for the `unwrap_or(Markdown)`
/// convention several writers share (`apply_metadata_edit`, epic frontmatter
/// load, the SW-GS-35 `dev_branch` write-back) so they can never diverge.
pub fn format_of(path: &std::path::Path) -> PipelineFileFormat {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    detect_format(ext).unwrap_or(PipelineFileFormat::Markdown)
}

// ----- Dispatch functions --------------------------------------------------

/// Extract metadata fields from file content.
pub fn extract_meta(format: PipelineFileFormat, text: &str) -> FrontmatterFields {
    match format {
        PipelineFileFormat::Markdown => markdown::extract_meta_fields(text),
        PipelineFileFormat::Html => html::extract_meta_fields(text),
    }
}

/// SW-GT-18 (AC-5): the raw set of top-level metadata **key names** a story carries
/// — official AND team-custom — resolved format-agnostically. Unlike [`extract_meta`]
/// (which deserializes into the closed [`FrontmatterFields`], silently dropping any
/// key it doesn't name), this preserves every authored key, so the field-schema
/// validator (`field_schema::validate_story_fields`) can flag custom / unknown fields.
///
/// Names are [`canonical_field_key`]-normalized (trimmed, ASCII-lowercased, top-level
/// collapsed) so equivalent markdown and HTML stories yield the **same** key set: HTML
/// maps each `cs:*` meta name back through [`FIELD_NAME_MAP`] to its YAML name (an
/// unmapped custom `cs:foo` is de-prefixed to `foo`), and both sides then run through
/// the same canonicalization — so a mixed-case custom field, dashes vs underscores in
/// the official map, and a nested `done.completed_at` all reconcile identically. The
/// result is de-duplicated, first-seen order. Unparseable/duplicate-key markdown
/// frontmatter degrades to a tolerant top-level line scan rather than silently dropping
/// every key.
pub fn extract_metadata_keys(format: PipelineFileFormat, text: &str) -> Vec<String> {
    match format {
        PipelineFileFormat::Markdown => markdown::extract_metadata_keys(text),
        PipelineFileFormat::Html => html::extract_metadata_keys(text),
    }
}

/// SW-SR-10: the format-agnostic **raw-metadata → template-values** map — the value
/// sibling of [`extract_metadata_keys`], and the extraction half that pairs with
/// `field_schema::extract_template_vars` (the lint half). Every top-level frontmatter
/// field resolves to a template value keyed by [`canonical_field_key`], so an authored
/// `{fieldName}` prompt var (markdown YAML or HTML `cs:*`) resolves trusted-side at
/// dispatch — including a team's custom field the scanner never named.
///
/// Format parity (the hard requirement): equivalent markdown and HTML stories produce the
/// **identical** map for **string-authored** scalar and list values. This holds because each
/// format's `template_value_pairs` funnels values through the same rules —
///   - **`", "`-join of the flattened scalar items** (a list `[a, b]` and an HTML CSV
///     `content="a, b"` both render `"a, b"`; a scalar passes verbatim);
///   - **dotted / nested keys are dropped** (a markdown `done:` mapping flattens to `""` and
///     is skipped here; an HTML `cs:done.completed_at` has a `.` and is skipped here) — nested
///     metadata is not scalar-template-referenceable;
///   - **the HTML `cs:` name is folded to the underscore schema convention** (hyphens →
///     underscores via [`cs_field_to_yaml_name`], matching every `FIELD_NAME_MAP` entry —
///     `cs:depends-on` ↔ `depends_on`), and **case is folded on both sides** (via
///     [`canonical_field_key`]), so a custom `cs:risk-tier` matches a markdown `risk_tier:`.
///     Markdown keys are underscore by convention (mirroring `FIELD_NAME_MAP` and the
///     `{fieldName}` var extractor, which likewise do not fold dashes) — a markdown key
///     authored with a hyphen stays hyphenated, the same as elsewhere in the crate;
///   - **first-seen wins** for a duplicated key.
///
/// Parity is intentionally NOT claimed for cases where YAML's typed parse and HTML's verbatim
/// string inherently diverge — these are format-specific and out of scope:
///   - a value YAML coerces (`True`→`"true"`, `007`→`"7"`, `1.10`→`"1.1"`) vs the raw HTML
///     string (`"True"`/`"007"`/`"1.10"`);
///   - a YAML `null`/`~` (dropped as non-scalar) vs a literal HTML `content="null"` (kept);
///   - malformed markdown (a duplicate top-level key makes `serde_yaml` reject the whole
///     mapping → no field values, story held at dispatch) vs HTML (first-seen per tag, siblings
///     intact).
///
/// The synthesized `{storyPath}`/`{story_path}` (both spellings, `story_path` verbatim — the
/// managed doc path) are inserted **last and unconditionally**, so they always win over any
/// frontmatter field that happens to share the name. Pure/µs — the caller reads `text`
/// off-thread.
pub fn story_template_values(
    format: PipelineFileFormat,
    text: &str,
    story_path: &str,
) -> std::collections::BTreeMap<String, String> {
    let pairs = match format {
        PipelineFileFormat::Markdown => markdown::template_value_pairs(text),
        PipelineFileFormat::Html => html::template_value_pairs(text),
    };
    let mut values = std::collections::BTreeMap::new();
    for (raw_key, value) in pairs {
        // Dotted/nested keys are not scalar-template-referenceable — dropped on both
        // formats so the maps stay identical (see the doc comment).
        if raw_key.contains('.') {
            continue;
        }
        let key = canonical_field_key(&raw_key);
        // A blank (or whitespace-only) value is treated as absent — it would resolve a
        // `{field}` to nothing, which the substitutor reports as unresolved rather than
        // silently blanking. First-seen wins, so two keys that canonicalize to the same
        // slot (case-aliased, or a mapped/custom cs-name fold-collision) keep the first.
        if key.is_empty() || value.trim().is_empty() {
            continue;
        }
        values.entry(key).or_insert(value);
    }
    // Synthesized path vars — both spellings, always authoritative.
    values.insert(canonical_field_key("storyPath"), story_path.to_string());
    values.insert(canonical_field_key("story_path"), story_path.to_string());
    values
}

/// Return the verbatim on-disk metadata block for `format` — the YAML
/// frontmatter body between `---` fences (markdown) or the contiguous run of
/// `<meta name="cs:*">` tags (HTML), byte-for-byte with no re-serialization.
/// `None` when the doc has no metadata block. Populated even when the block is
/// malformed (SLW-MD-1 AC-12 raw editor): it is the exact text a
/// `ReplaceRawBlock` edit swaps, so it round-trips with `apply_metadata_edit`.
pub fn raw_meta_block(format: PipelineFileFormat, text: &str) -> Option<String> {
    match format {
        PipelineFileFormat::Markdown => markdown::raw_meta_block(text),
        PipelineFileFormat::Html => html::raw_meta_block(text),
    }
}

/// Typed fields and strict block validity from one shared read path. Markdown
/// may recover plain colon values while keeping the validity bit false; HTML
/// has no malformed-block case.
pub fn extract_meta_and_parse_ok(
    format: PipelineFileFormat,
    text: &str,
) -> (FrontmatterFields, bool) {
    match format {
        PipelineFileFormat::Markdown => markdown::extract_meta_and_parse_ok(text),
        PipelineFileFormat::Html => (html::extract_meta_fields(text), true),
    }
}

/// SW-LN-34: the title declared in the document's metadata — frontmatter
/// `title:` (quoted or bare) for markdown, `<meta name="cs:title" content="…">`
/// for HTML. Trimmed; blank counts as absent. A frontmatter block that doesn't
/// parse as YAML yields `None` (same posture as the template values).
pub fn extract_meta_title(format: PipelineFileFormat, text: &str) -> Option<String> {
    let pairs = match format {
        PipelineFileFormat::Markdown => markdown::template_value_pairs(text),
        PipelineFileFormat::Html => html::template_value_pairs(text),
    };
    pairs
        .into_iter()
        .filter(|(key, _)| canonical_field_key(key) == "title")
        .map(|(_, value)| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Extract the document title from file content.
pub fn extract_title(format: PipelineFileFormat, text: &str) -> Option<String> {
    match format {
        PipelineFileFormat::Markdown => markdown::extract_title(text),
        PipelineFileFormat::Html => html::extract_title(text),
    }
}

/// Extract a named section's body text from file content.
pub fn extract_section(
    format: PipelineFileFormat,
    text: &str,
    name: &str,
    max_chars: Option<usize>,
) -> Option<String> {
    match format {
        PipelineFileFormat::Markdown => markdown::extract_section(text, name, max_chars),
        PipelineFileFormat::Html => html::extract_section(text, name, max_chars),
    }
}

/// SW-EW-32: extract the first matching section over an ordered heading list,
/// first `Some` wins. Lets the summary key off a configurable, ordered list of
/// headings (e.g. `["Purpose", "Story", "Overview"]`) instead of a single
/// hardcoded name. Callers pass an already-resolved, non-empty list (see
/// `pipeline_scanner::effective_headings`, which trims blanks and falls back to
/// the defaults); an empty slice yields `None`.
pub fn extract_first_section(
    format: PipelineFileFormat,
    text: &str,
    headings: &[String],
    max_chars: Option<usize>,
) -> Option<String> {
    headings
        .iter()
        .find_map(|name| extract_section(format, text, name, max_chars))
}

/// Rewrite (or insert) a metadata field in file content. Returns the updated
/// content string. The `field_name` uses the canonical YAML-style name
/// (e.g. `"status"`, `"type"`); format modules map it to the native
/// representation (e.g. `cs:status` for HTML).
pub fn rewrite_field(
    format: PipelineFileFormat,
    content: &str,
    field_name: &str,
    new_value: &str,
) -> String {
    match format {
        PipelineFileFormat::Markdown => markdown::rewrite_field(content, field_name, new_value),
        PipelineFileFormat::Html => html::rewrite_field(content, field_name, new_value),
    }
}

/// Rewrite (or insert/remove) a top-level YAML *list* field as a block
/// sequence. The block-list sibling of [`rewrite_field`]; an empty `items`
/// slice removes the field. Markdown only — epic docs (the sole caller, SW-EW-9)
/// are markdown; HTML is a defensive no-op (callers must guard the format).
pub fn rewrite_list_field(
    format: PipelineFileFormat,
    content: &str,
    field_name: &str,
    items: &[String],
) -> String {
    match format {
        PipelineFileFormat::Markdown => markdown::rewrite_list_field(content, field_name, items),
        PipelineFileFormat::Html => content.to_string(),
    }
}

// ----- Shared types --------------------------------------------------------

/// The top-level metadata key of a (possibly nested) field name: `done.completed_at`
/// → `done`, `status` → `status`. The one home for the nested-key-collapse convention
/// shared by [`extract_metadata_keys`] (HTML side) and the SW-GT-18 field-schema
/// baseline, so the two can never disagree on what key a nested field belongs to.
pub fn top_level_field(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// Canonicalize a metadata field name to its match key: trimmed, ASCII-lowercased,
/// collapsed to its [`top_level_field`]. Metadata field names are **case-insensitive**
/// identifiers, so this is the single normalization every name boundary funnels through
/// — the markdown/HTML key extractors ([`extract_metadata_keys`]), the SW-GT-18 field
/// schema (official baseline + custom names), and the `{fieldName}` template-var
/// extractor. Because both formats and the schema share it, equivalent stories yield the
/// same key set regardless of authored case, dashes-vs-underscores are out of scope (a
/// separate concern), and a custom `Status` can never masquerade past the official
/// `status`.
pub fn canonical_field_key(name: &str) -> String {
    top_level_field(name.trim()).to_ascii_lowercase()
}

/// Map a YAML-style field name to its `cs:*` HTML meta tag name.
pub fn yaml_to_cs_name(field_name: &str) -> Option<&'static str> {
    FIELD_NAME_MAP
        .iter()
        .find(|(yaml, _)| *yaml == field_name)
        .map(|(_, cs)| *cs)
}

/// Map a de-prefixed `cs:*` meta name (e.g. `dev-branch`, `done.completed_at`) back to
/// its YAML field name via [`FIELD_NAME_MAP`] — the inverse of [`yaml_to_cs_name`]. An
/// unmapped custom name (e.g. `cs:risk-tier`) has its hyphens folded to underscores
/// (`risk_tier`), matching the cs-name→schema-name convention every `FIELD_NAME_MAP`
/// entry follows (`cs:depends-on` ↔ `depends_on`) and the sibling
/// `frontmatter_editor::html_open_tag_values` — so an HTML custom field authored with the
/// natural hyphen spelling reconciles with the equivalent markdown `risk_tier:` (format
/// parity for custom fields, not just mapped ones). The single home for the HTML cs→yaml
/// reconciliation the key extractor and the value extractor both need, so they can never
/// disagree on what YAML field a `cs:*` tag denotes.
pub(crate) fn cs_field_to_yaml_name(cs_field: &str) -> String {
    FIELD_NAME_MAP
        .iter()
        .find(|(_, cs)| cs.strip_prefix("cs:") == Some(cs_field))
        .map(|(yaml, _)| (*yaml).to_string())
        .unwrap_or_else(|| cs_field.replace('-', "_"))
}

/// Flatten a `serde_yaml::Value` into its scalar string item(s): a sequence →
/// each scalar member; a scalar → a single item. Non-scalar members (nested
/// mappings, nulls) are dropped, so a `done:` block yields `[]`. The one flatten shared by the
/// open-tag extractor (`frontmatter_editor::markdown_open_tag_values`) and the template-value
/// extractor ([`markdown::template_value_pairs`]), so both agree on how a frontmatter value
/// becomes template/tag items — one copy, one behavior.
pub(crate) fn yaml_value_items(v: &serde_yaml::Value) -> Vec<String> {
    use serde_yaml::Value;
    match v {
        Value::Sequence(seq) => seq.iter().filter_map(yaml_scalar_str).collect(),
        _ => yaml_scalar_str(v).into_iter().collect(),
    }
}

/// A single YAML scalar (`String`/`Bool`/`Number`) as its string form; `None`
/// for any non-scalar (sequence, mapping, null, tagged). The atom
/// [`yaml_value_items`] maps over.
pub(crate) fn yaml_scalar_str(v: &serde_yaml::Value) -> Option<String> {
    use serde_yaml::Value;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Truncate a string to at most `n` characters on a char boundary.
/// Shared by format modules for `max_chars` truncation in `extract_section`.
pub fn truncate_to_char_boundary(body: &str, n: usize) -> String {
    if body.chars().count() <= n {
        return body.to_string();
    }
    let end = body
        .char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    body[..end].to_string()
}

/// Strongly-typed view of frontmatter / metadata fields the scanner reads.
/// Shared across all format modules — each module produces this struct from
/// its native metadata representation.
///
/// For markdown, populated via `gray_matter` + serde deserialization.
/// For HTML, populated manually from `<meta name="cs:*">` tag parsing.
///
/// **R-14 invariant:** This struct IS the cache's frontmatter shape.
/// `CacheEntry.fields` composes this directly (see `frontmatter_cache.rs`).
/// When you add a new YAML field here, the cache inherits it for free —
/// no schema-mirroring update needed in `CacheEntry`. The accessor methods
/// on `CacheEntry` (`status()`, `story_id()`, etc.) thinly proxy to this
/// struct's fields, so adding a new YAML field only requires:
///   1. New field here, and its [`STORY_FIELDS`] row made [`fields::FieldRole::Typed`]
///      (a parity test fails until both exist and the HTML reader fills it).
///   2. (Optional) New accessor method on `CacheEntry` for ergonomic access.
///   3. Bump `CACHE_VERSION` if the field's serde shape isn't `#[serde(default)]`
///      backward-compat.
#[derive(Default, Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FrontmatterFields {
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
    /// SW-AU-77F: curated built-in list field (e.g. `["infra", "urgent"]`). Populated
    /// from `tags:` frontmatter (markdown serde) and `<meta name="cs:tags">` (HTML,
    /// CSV-split in `extract_meta_fields`). Typed here rather than left to `open_tags`
    /// so the value is captured identically on both formats once `cs:tags` joins
    /// the known-key (non-open-tag) set. Additive `#[serde(default)]`, but it lands
    /// in `CacheEntry.fields` (R-14 rule), so a `CACHE_VERSION` bump is required to
    /// re-scan already-cached stories and populate it.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<String>,
    /// Raw `type` string. Mapped to `document_type` in Rust (since `type`
    /// is a reserved keyword) via serde rename. Passed through as-is —
    /// validation happens on the TS side.
    #[serde(rename = "type", default)]
    pub document_type: Option<String>,
    /// Relative paths to supporting documents.
    #[serde(default)]
    pub supporting_docs: Option<Vec<String>>,
    /// Authoritative display label (e.g. "W-5").
    #[serde(default)]
    pub story_id: Option<String>,
    /// Raw `source_repo` slug (e.g. `"alderco/codesense"`).
    #[serde(default)]
    pub source_repo: Option<String>,
    /// SV-CDE-85: optional `owner/repo` whose history holds the story's work; overrides
    /// `source_repo` for history reads only.
    #[serde(default)]
    pub work_repo: Option<String>,
    /// SW-GS-35 → **LEGACY** (SW-OR-1b): a *story's* observed dev branch. Merged
    /// into the unified [`branch`](Self::branch) — new code never writes this; it
    /// is read only as a last-resort fallback for docs authored before the merge
    /// (see [`CacheEntry::story_branch`](crate::install::frontmatter_cache::CacheEntry::story_branch)
    /// precedence `branch` → `branch_name` → `dev_branch`). Left in place on old
    /// docs (additive, no migration). Additive `#[serde(default)]`.
    #[serde(default)]
    pub dev_branch: Option<String>,
    /// SW-WB-1 → **LEGACY** (SW-OR-1b): a *story's* intended branch. Merged into
    /// the unified [`branch`](Self::branch) — new code never writes this; it is
    /// read only as a fallback (ranked above [`dev_branch`](Self::dev_branch)) for
    /// docs authored before the merge. Left in place on old docs (additive, no
    /// migration). Additive `#[serde(default)]`.
    #[serde(default)]
    pub branch_name: Option<String>,
    /// Team name from frontmatter (e.g. `"default"`).
    #[serde(default)]
    pub team: Option<String>,
    /// SW-EW-5: assigned repos for an *epic* doc, in `owner/repo` form. Read
    /// via [`epic_repos`](Self::epic_repos) (validated, malformed dropped).
    /// Missing ⇒ `[]`. Read-only here; write-back is EW-6. Backward-compat
    /// `#[serde(default)]` — no `CACHE_VERSION` bump (per the R-14 rule above;
    /// epics are not cached anyway — the cache is story-keyed).
    #[serde(default)]
    pub repos: Option<Vec<String>>,
    /// The unified `branch` field, read by doc kind (a doc is one or the other,
    /// so the key never collides):
    /// - **Epic (SW-EW-5):** the integration branch. Absent ⇒ derived
    ///   `integration/{epic-id}` (see
    ///   [`resolve_integration_branch`](Self::resolve_integration_branch)), never
    ///   the repo default.
    /// - **Story (SW-OR-1b):** the single canonical story branch that supersedes
    ///   the legacy [`branch_name`](Self::branch_name)/[`dev_branch`](Self::dev_branch);
    ///   drives worktree creation and is auto-stamped to the observed branch on
    ///   establish/open (`write_story_branch`). Read via `CacheEntry::story_branch()`.
    #[serde(default)]
    pub branch: Option<String>,
    /// SLW-UD-2 legacy epic list fields (`scope_context:` / `scope_install:`).
    /// SW-XS-5: directory ownership is gone and nothing interprets them; they
    /// are parsed only so existing epic docs keep showing them as ordinary
    /// frontmatter in the metadata editor.
    #[serde(default)]
    pub scope_context: Option<Vec<String>>,
    /// See [`scope_context`](Self::scope_context).
    #[serde(default)]
    pub scope_install: Option<Vec<String>>,
    /// SW-SB-9 / SW-SB-1: story estimate in points. Stored RAW — the scanner
    /// never validates the scale; the off-Fibonacci soft lint lives in
    /// `validate_story` (AC-6). Parsed leniently: a non-numeric value degrades to
    /// `None` rather than failing the whole frontmatter parse. Additive
    /// `#[serde(default)]` — no `CACHE_VERSION` bump (R-14 rule above).
    #[serde(default, deserialize_with = "de_lenient_points")]
    pub points: Option<f64>,
    /// SW-SB-1: the sprint tag a story is assigned to (e.g. `"2026-08-A"`).
    /// Genuine metadata (unlike epic/sub-epic, which is folder position) — the
    /// board's sprint filter (SB-3) reads it off `StoryRef`. Parsed leniently
    /// (`de_lenient_string`): a non-scalar value degrades to `None` rather than
    /// wiping every sibling field (see `de_lenient_points`). Additive
    /// `#[serde(default)]` — no `CACHE_VERSION` bump (R-14 rule above).
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub sprint: Option<String>,
    /// SW-SB-1: the person a story is assigned to (a username, e.g.
    /// `"octocat-dev"`). Drives SB-12's `getReadyUnassigned` + SE-7 assignee
    /// avatars. Same lenient parse + additive posture as [`sprint`](Self::sprint).
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub assigned_to: Option<String>,
    /// SW-SB-22: opaque board-ordering rank — a string fractional index (see the
    /// FE `rank.ts`) that sorts a card within its column. Compared lexically only;
    /// the scanner never parses or validates its shape. Stored as a quoted YAML
    /// scalar (`render_yaml_scalar`) and read back leniently (`de_lenient_string`):
    /// a non-scalar value degrades to `None` rather than wiping every sibling
    /// field. Absent ⇒ unranked (sorts after ranked cards). Requires a
    /// `CACHE_VERSION` bump (this field lands in `CacheEntry.fields`, R-14 rule).
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub rank: Option<String>,
    /// SW-AU-77H: an author-declared automation subject id for a standalone
    /// (non-story) document — the durable, move-safe identity that makes a plain
    /// doc or rendered report an addressable automation subject without turning it
    /// into a pipeline story (a `type: story` file uses `story_id` instead; when
    /// both are present `story_id` wins). Parsed leniently like the sibling SB-1
    /// scalars. HTML declares it as `<meta name="cs:subject-id">` via
    /// [`FIELD_NAME_MAP`]. Additive `#[serde(default)]`; lands in `CacheEntry.fields`
    /// (R-14) → drove the `CACHE_VERSION` 7→8 bump.
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub subject_id: Option<String>,
}

/// The Fibonacci story-point scale from `docs/sprint-model.md`. The SINGLE
/// source of truth for the off-scale lint (SW-SB-9 AC-6) and — when SW-SB-6's
/// inline points quick-pick lands — its option set, so an estimate the board
/// offers can never be one the validator flags.
pub const FIBONACCI_POINTS: &[f64] = &[1.0, 2.0, 3.0, 5.0, 8.0, 13.0];

/// Parse `points` leniently: a **finite** numeric value yields `Some(n)`; ANY
/// other shape (string, bool, list, null, `.nan`/`.inf`, …) yields `None` rather
/// than erroring. Because `extract_meta` turns a struct-level parse failure into
/// an all-fields-`default` wipe, a strict parse would let one malformed `points`
/// silently drop every other field — so a bad `points` degrades to "absent",
/// like every other tolerant field. Non-finite is rejected so a `points: .nan`
/// can't reach `Some(NaN)` (which serde_json would emit as `null` on the wire
/// anyway, and which breaks value equality) — the HTML path filters identically.
fn de_lenient_points<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_yaml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| v.as_f64()).filter(|n| n.is_finite()))
}

/// SW-SB-1: parse an optional string field leniently — a YAML string yields
/// `Some(s)`, any other shape yields `None`. Same rationale as
/// [`de_lenient_points`]: `extract_meta` turns a struct-level parse failure into
/// an all-fields-`default` wipe, so a strict `Option<String>` would let one
/// malformed value drop every sibling field. Shared by the string board fields.
fn de_lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_yaml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| v.as_str().map(|s| s.to_string())))
}

/// Document `type:` values that route a file to `stories[]` (vs `docs[]`).
/// Applies uniformly to both markdown frontmatter `type:` and HTML
/// `<meta name="cs:type">`. Files with no `type:` field or any value
/// off this list (e.g. `doc`, `retro`, `epic`, `llm-distillate`) are
/// documents — they surface in the file tree only, not as
/// status-dropdown / pipeline-card entries.
///
/// Post-burn-in review intentionally omits `doc` and `retro` — the
/// prior visual-differentiation behavior for those types was dropped,
/// since ordinary planning docs (e.g. a `prd.md` or `architecture.md`)
/// was surfacing as ghost pipeline cards.
///
/// SF-1: renamed from `PIPELINE_MARKDOWN_TYPES`; the list is format-
/// neutral now that HTML respects `cs:type=doc` as an opt-out.
///
/// SW-MCP-10: `pub(crate)` so the authoring tools (`get_story_format` /
/// `validate_story`) read the SAME closed `type` enum the parser enforces —
/// there is exactly one story-type vocabulary in the crate.
pub(crate) const PIPELINE_DOCUMENT_TYPES: &[&str] = &["story", "bug", "spike"];

impl FrontmatterFields {
    /// Returns `true` when no metadata fields are populated.
    /// Used to gate HTML files: if the HTML extractor found zero `cs:*` meta
    /// tags, all fields remain `None` and `is_empty()` returns `true`.
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.document_type.is_none()
            && self.depends_on.is_none()
            && self.supporting_docs.is_none()
            && self.story_id.is_none()
            && self.source_repo.is_none()
            && self.work_repo.is_none()
            && self.dev_branch.is_none()
            && self.branch_name.is_none()
            && self.team.is_none()
            && self.repos.is_none()
            && self.branch.is_none()
            && self.scope_context.is_none()
            && self.scope_install.is_none()
            // SW-SB-1: board metadata counts as metadata — an HTML doc carrying
            // only these must NOT be dropped by the F.30 empty gate. (`points`
            // was missing here since SB-9; folded in with its siblings.)
            && self.sprint.is_none()
            && self.points.is_none()
            && self.assigned_to.is_none()
            // SW-SB-22: `rank` is board metadata too — same non-empty contract.
            && self.rank.is_none()
            // SW-AU-77H: a `subject_id` is the whole point of a standalone doc —
            // an HTML doc carrying ONLY `cs:subject-id` must NOT be dropped by the
            // F.30 empty gate, or its marker never reaches `DocRef` (AC-3).
            && self.subject_id.is_none()
    }

    /// Returns `true` when `document_type` (markdown `type:` or HTML
    /// `cs:type`) marks this file as a pipeline story — i.e. it belongs to
    /// [`PIPELINE_DOCUMENT_TYPES`] (`story` / `bug` / `spike`) and should
    /// route to `stories[]` rather than `docs[]`. Files with no `type:` or
    /// any value off the list are documents.
    ///
    /// F.30 gates HTML presence upstream (HTML with zero `cs:*` meta tags is
    /// excluded from the scan entirely before reaching this predicate); here
    /// we only decide story-vs-doc classification.
    ///
    /// The value is normalized before membership: leading/trailing whitespace
    /// is trimmed and the comparison is ASCII case-insensitive
    /// (`eq_ignore_ascii_case`). HTML meta values arrive verbatim from
    /// `<meta name="cs:type" content="…">`, so `" story"` / `"Story"` would
    /// otherwise silently demote a pipeline-intended story to a doc. Markdown's
    /// YAML parser already trims unquoted scalars but is case-sensitive —
    /// uniform normalization keeps both formats symmetric. The membership check
    /// does not allocate (`eq_ignore_ascii_case` operates on `&str` slices);
    /// the surrounding `as_deref()` + `str::trim()` are O(n) over the value
    /// length with no heap allocation either.
    ///
    /// SW-FX-12: relocated here from `pipeline_scanner::is_pipeline_story` (and
    /// renamed from the SF-1 `is_pipeline_markdown`) so the predicate lives on
    /// the type it interrogates, in the lowest shared module. Both the scanner
    /// (story-vs-doc routing) and the frontmatter cache (orphan-log level +
    /// miss-count scope) now share exactly one definition.
    pub fn is_pipeline_story(&self) -> bool {
        let Some(raw) = self.document_type.as_deref() else {
            return false;
        };
        let trimmed = raw.trim();
        PIPELINE_DOCUMENT_TYPES
            .iter()
            .any(|allowed| trimmed.eq_ignore_ascii_case(allowed))
    }

    /// SW-EW-5: the epic's assigned repos as validated [`SourceRepoSlug`]s.
    ///
    /// Reads the raw `repos:` strings (`owner/repo` form), parses each through
    /// the canonical [`SourceRepoSlug::parse_owner_repo`], and **drops malformed
    /// entries with a `debug!`** rather than failing — a single typo in a
    /// hand-edited epic doc must never brick the panel. Missing `repos:` ⇒ `[]`.
    pub fn epic_repos(&self) -> Vec<SourceRepoSlug> {
        let Some(raw) = self.repos.as_ref() else {
            return Vec::new();
        };
        raw.iter()
            .filter_map(|entry| {
                let slug = SourceRepoSlug::parse_owner_repo(entry);
                if slug.is_none() {
                    debug!(
                        target: "epic_repos",
                        "dropping malformed repos entry {entry:?} (expected owner/repo)"
                    );
                }
                slug
            })
            .collect()
    }

    /// SW-EW-5: the epic's integration branch. Returns `branch:` verbatim when
    /// set (and non-blank), else derives `integration/{epic_id}` via
    /// [`derive_integration_branch`] — **never** the repo default.
    pub fn resolve_integration_branch(&self, epic_id: &str) -> String {
        match self.branch.as_deref().map(str::trim) {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => derive_integration_branch(epic_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SW-AU-77H: a declared `subject_id` parses identically from markdown
    /// frontmatter and the HTML `cs:subject-id` meta (format parity, AC-3).
    #[test]
    fn subject_id_parses_from_markdown_and_html() {
        let md = extract_meta(
            PipelineFileFormat::Markdown,
            "---\nsubject_id: DOC-1\n---\n\n# Doc\n",
        );
        assert_eq!(md.subject_id.as_deref(), Some("DOC-1"), "markdown");
        let html = extract_meta(
            PipelineFileFormat::Html,
            "<meta name=\"cs:subject-id\" content=\"DOC-1\"><h1>Doc</h1>",
        );
        assert_eq!(html.subject_id.as_deref(), Some("DOC-1"), "html");
    }

    #[test]
    fn detect_format_md() {
        assert_eq!(detect_format("md"), Some(PipelineFileFormat::Markdown));
        assert_eq!(detect_format("MD"), Some(PipelineFileFormat::Markdown));
    }

    /// SW-FX-12 (AC-5): the relocated `is_pipeline_story` predicate — the
    /// single story-vs-doc classifier shared by the scanner and the cache.
    /// `story`/`bug`/`spike` (trim- and case-normalized) are stories;
    /// everything else, incl. no `type:`, is a document.
    #[test]
    fn is_pipeline_story_membership_and_normalization() {
        let with_type = |t: Option<&str>| FrontmatterFields {
            document_type: t.map(str::to_string),
            ..Default::default()
        };
        for t in ["story", "bug", "spike", " Story ", "BUG"] {
            assert!(
                with_type(Some(t)).is_pipeline_story(),
                "{t:?} should be a story"
            );
        }
        for t in ["doc", "retro", "epic", "llm-distillate", ""] {
            assert!(
                !with_type(Some(t)).is_pipeline_story(),
                "{t:?} should be a doc"
            );
        }
        assert!(!with_type(None).is_pipeline_story(), "no type → doc");
    }

    // ----- SW-EW-32: extract_first_section ordering --------------------------

    #[test]
    fn extract_first_section_first_match_wins() {
        let md = "# Title\n\n## Purpose\nthe purpose body\n\n## Story\nthe story body\n";
        let headings = vec!["Purpose".to_string(), "Story".to_string()];
        assert_eq!(
            extract_first_section(PipelineFileFormat::Markdown, md, &headings, None).as_deref(),
            Some("the purpose body"),
        );
    }

    #[test]
    fn extract_first_section_falls_through_to_later_heading() {
        // Default list ["Purpose", "Story"] on a story doc with no `## Purpose`
        // resolves to `## Story` — the back-compat case (AC-5).
        let md = "# Title\n\n## Story\nthe story body\n";
        let headings = vec!["Purpose".to_string(), "Story".to_string()];
        assert_eq!(
            extract_first_section(PipelineFileFormat::Markdown, md, &headings, None).as_deref(),
            Some("the story body"),
        );
    }

    #[test]
    fn extract_first_section_none_when_no_heading_matches() {
        let md = "# Title\n\n## Overview\nbody\n";
        let headings = vec!["Purpose".to_string(), "Story".to_string()];
        assert!(extract_first_section(PipelineFileFormat::Markdown, md, &headings, None).is_none());
    }

    #[test]
    fn extract_first_section_empty_list_is_none() {
        let md = "# Title\n\n## Purpose\nbody\n";
        assert!(extract_first_section(PipelineFileFormat::Markdown, md, &[], None).is_none());
    }

    #[test]
    fn extract_first_section_honors_max_chars() {
        let md = "# Title\n\n## Purpose\nabcdefghij\n";
        let headings = vec!["Purpose".to_string()];
        let out = extract_first_section(PipelineFileFormat::Markdown, md, &headings, Some(4));
        assert_eq!(out.as_deref(), Some("abcd"));
    }

    #[test]
    fn detect_format_html() {
        assert_eq!(detect_format("html"), Some(PipelineFileFormat::Html));
        assert_eq!(detect_format("htm"), Some(PipelineFileFormat::Html));
        assert_eq!(detect_format("HTML"), Some(PipelineFileFormat::Html));
        assert_eq!(detect_format("HTM"), Some(PipelineFileFormat::Html));
    }

    #[test]
    fn detect_format_unknown() {
        assert_eq!(detect_format("txt"), None);
        assert_eq!(detect_format("json"), None);
        assert_eq!(detect_format(""), None);
    }

    #[test]
    fn pipeline_extensions_contains_expected() {
        assert!(PIPELINE_EXTENSIONS.contains(&"md"));
        assert!(PIPELINE_EXTENSIONS.contains(&"html"));
        assert!(PIPELINE_EXTENSIONS.contains(&"htm"));
        assert!(!PIPELINE_EXTENSIONS.contains(&"txt"));
    }

    #[test]
    fn yaml_to_cs_name_maps_known_fields() {
        assert_eq!(yaml_to_cs_name("status"), Some("cs:status"));
        assert_eq!(yaml_to_cs_name("type"), Some("cs:type"));
        assert_eq!(yaml_to_cs_name("depends_on"), Some("cs:depends-on"));
        assert_eq!(
            yaml_to_cs_name("supporting_docs"),
            Some("cs:supporting-docs")
        );
        assert_eq!(yaml_to_cs_name("story_id"), Some("cs:story-id"));
        assert_eq!(yaml_to_cs_name("source_repo"), Some("cs:source-repo"));
        assert_eq!(yaml_to_cs_name("team"), Some("cs:team"));
    }

    #[test]
    fn yaml_to_cs_name_unknown_returns_none() {
        assert_eq!(yaml_to_cs_name("unknown"), None);
        assert_eq!(yaml_to_cs_name(""), None);
    }

    // ----- F.30: FrontmatterFields::is_empty ---------------------------------

    #[test]
    fn frontmatter_fields_default_is_empty() {
        assert!(FrontmatterFields::default().is_empty());
    }

    #[test]
    fn frontmatter_fields_status_set_not_empty() {
        let fm = FrontmatterFields {
            status: Some("todo".into()),
            ..Default::default()
        };
        assert!(!fm.is_empty());
    }

    #[test]
    fn frontmatter_fields_any_single_field_not_empty() {
        let with_type = FrontmatterFields {
            document_type: Some("spike".into()),
            ..Default::default()
        };
        assert!(!with_type.is_empty());

        let with_deps = FrontmatterFields {
            depends_on: Some(vec!["a".into()]),
            ..Default::default()
        };
        assert!(!with_deps.is_empty());

        let with_story_id = FrontmatterFields {
            story_id: Some("F-1".into()),
            ..Default::default()
        };
        assert!(!with_story_id.is_empty());

        let with_source_repo = FrontmatterFields {
            source_repo: Some("org/repo".into()),
            ..Default::default()
        };
        assert!(!with_source_repo.is_empty());

        let with_supporting = FrontmatterFields {
            supporting_docs: Some(vec!["./spec.md".into()]),
            ..Default::default()
        };
        assert!(!with_supporting.is_empty());

        let with_team = FrontmatterFields {
            team: Some("default".into()),
            ..Default::default()
        };
        assert!(!with_team.is_empty());

        // SW-SB-1: board metadata alone makes a doc non-empty (F.30 gate) — an
        // HTML doc carrying only these is a pipeline doc, not dropped.
        for board_only in [
            FrontmatterFields {
                sprint: Some("2026-08-A".into()),
                ..Default::default()
            },
            FrontmatterFields {
                points: Some(5.0),
                ..Default::default()
            },
            FrontmatterFields {
                assigned_to: Some("octocat-dev".into()),
                ..Default::default()
            },
            // SW-SB-22: a doc carrying only a rank is likewise non-empty.
            FrontmatterFields {
                rank: Some("h0".into()),
                ..Default::default()
            },
        ] {
            assert!(!board_only.is_empty());
        }
    }

    #[test]
    fn markdown_team_field_deserialized() {
        let md = "---\nstatus: todo\nteam: my-team\nsource_repo: org/repo\n---\n# Title\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert_eq!(fm.team.as_deref(), Some("my-team"));
        assert_eq!(fm.status.as_deref(), Some("todo"));
        assert_eq!(fm.source_repo.as_deref(), Some("org/repo"));
    }

    // SW-SB-1 (AC-1): the three board fields deserialize from markdown; `points`
    // is an f64, the strings are plain scalars.
    #[test]
    fn markdown_board_metadata_deserialized() {
        let md =
            "---\nstatus: todo\nsprint: 2026-08-A\npoints: 5\nassigned_to: octocat-dev\n---\n# T\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert_eq!(fm.sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(fm.points, Some(5.0));
        assert_eq!(fm.assigned_to.as_deref(), Some("octocat-dev"));
    }

    // SW-SB-1: `de_lenient_points` rejects non-finite YAML floats (`.nan`/`.inf`)
    // on the serde-direct `FrontmatterFields` path — a `.nan` never reaches
    // `Some(NaN)`. (The gray_matter markdown path pre-coerces non-finite to a
    // finite value, so this drives the deserializer the filter actually guards.)
    // Siblings survive — the whole struct isn't wiped.
    #[test]
    fn de_lenient_points_rejects_non_finite() {
        for bad in [".nan", ".inf", "-.inf"] {
            let yaml = format!("status: todo\npoints: {bad}\n");
            let fm: FrontmatterFields = serde_yaml::from_str(&yaml).expect("parses");
            assert!(fm.points.is_none(), "points: {bad} must degrade to None");
            assert_eq!(fm.status.as_deref(), Some("todo"), "siblings survive");
        }
    }

    // SW-SB-1 (AC-1): `de_lenient_string` — a non-scalar `assigned_to` (a list)
    // degrades to None WITHOUT wiping sibling fields. Guards the invariant that
    // `extract_meta` turns a struct-level parse failure into an all-fields wipe.
    #[test]
    fn markdown_board_string_non_scalar_degrades_without_wiping_siblings() {
        let md = "---\nstatus: todo\nsprint: 2026-08-A\nassigned_to:\n  - a\n  - b\n---\n# T\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert!(fm.assigned_to.is_none(), "list `assigned_to` → None");
        // Siblings survive — the whole struct did NOT collapse to defaults.
        assert_eq!(fm.status.as_deref(), Some("todo"));
        assert_eq!(fm.sprint.as_deref(), Some("2026-08-A"));
    }

    // ----- SW-GS-35: story-scoped dev_branch (AC-1/AC-5/AC-6) ----------------

    #[test]
    fn format_of_resolves_by_extension_defaulting_markdown() {
        use std::path::Path;
        assert_eq!(format_of(Path::new("a.md")), PipelineFileFormat::Markdown);
        assert_eq!(format_of(Path::new("a.html")), PipelineFileFormat::Html);
        assert_eq!(format_of(Path::new("a.HTM")), PipelineFileFormat::Html);
        // Missing / unrecognized extension → Markdown (the shared convention).
        assert_eq!(format_of(Path::new("noext")), PipelineFileFormat::Markdown);
        assert_eq!(format_of(Path::new("a.txt")), PipelineFileFormat::Markdown);
    }

    #[test]
    fn dev_branch_field_maps_to_cs_dev_branch() {
        // AC-1: markdown `dev_branch:` ↔ HTML `<meta name="cs:dev-branch">`.
        assert_eq!(yaml_to_cs_name("dev_branch"), Some("cs:dev-branch"));
    }

    #[test]
    fn board_fields_map_to_cs_names() {
        // SW-SB-1 (AC-7): markdown ↔ HTML meta names. `assigned_to` follows the
        // dash convention (cs:assigned-to), like cs:dev-branch/cs:source-repo.
        assert_eq!(yaml_to_cs_name("sprint"), Some("cs:sprint"));
        assert_eq!(yaml_to_cs_name("points"), Some("cs:points"));
        assert_eq!(yaml_to_cs_name("assigned_to"), Some("cs:assigned-to"));
    }

    #[test]
    fn frontmatter_fields_dev_branch_set_not_empty() {
        // AC-1: a dev_branch-only block is NOT empty (gates the HTML extractor).
        let fm = FrontmatterFields {
            dev_branch: Some("feat/SW-GS-35-foo".into()),
            ..Default::default()
        };
        assert!(!fm.is_empty());
    }

    #[test]
    fn markdown_dev_branch_deserialized() {
        // AC-1: parses into FrontmatterFields; AC-5: epic `branch` stays None.
        let md = "---\nstatus: todo\ndev_branch: feat/SW-GS-35-story-dev-branch-frontmatter\nsource_repo: org/repo\n---\n# Title\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert_eq!(
            fm.dev_branch.as_deref(),
            Some("feat/SW-GS-35-story-dev-branch-frontmatter")
        );
        assert!(fm.branch.is_none(), "epic `branch` untouched (AC-5)");
    }

    #[test]
    fn markdown_dev_branch_absent_is_none() {
        // AC-6: older/never-opened doc parses exactly as today, dev_branch None.
        let md = "---\nstatus: todo\n---\n# Title\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert!(fm.dev_branch.is_none());
    }

    #[test]
    fn html_dev_branch_field_extracted() {
        // AC-1: HTML `cs:dev-branch` round-trips via FIELD_NAME_MAP.
        let html = r#"<html><head>
            <meta name="cs:status" content="todo">
            <meta name="cs:dev-branch" content="feat/SW-GS-35-foo">
        </head><body><h1>Title</h1></body></html>"#;
        let fm = extract_meta(PipelineFileFormat::Html, html);
        assert_eq!(fm.dev_branch.as_deref(), Some("feat/SW-GS-35-foo"));
    }

    #[test]
    fn rewrite_dev_branch_inserts_and_is_idempotent() {
        // AC-2: byte-preserving insert, then a same-value rewrite is a no-op.
        let md = "---\nstatus: todo\nsource_repo: org/repo\n---\n# Title\n";
        let once = rewrite_field(
            PipelineFileFormat::Markdown,
            md,
            "dev_branch",
            "feat/SW-GS-35-foo",
        );
        assert!(once.contains("dev_branch: feat/SW-GS-35-foo"));
        // Re-parse to confirm the value round-trips.
        assert_eq!(
            extract_meta(PipelineFileFormat::Markdown, &once)
                .dev_branch
                .as_deref(),
            Some("feat/SW-GS-35-foo")
        );
        // Idempotent: re-writing the same value yields byte-identical content.
        let twice = rewrite_field(
            PipelineFileFormat::Markdown,
            &once,
            "dev_branch",
            "feat/SW-GS-35-foo",
        );
        assert_eq!(twice, once, "same-value rewrite must be byte-identical");
    }

    #[test]
    fn rewrite_dev_branch_html_round_trips() {
        // AC-1/AC-2 for the HTML format: writing `dev_branch` to an HTML story
        // doc via rewrite_field and re-reading yields the exact value, and a
        // same-value rewrite is byte-identical (idempotent) — symmetric with the
        // markdown coverage above.
        let html =
            "<html><head><meta name=\"cs:status\" content=\"todo\"></head><body></body></html>";
        let once = rewrite_field(
            PipelineFileFormat::Html,
            html,
            "dev_branch",
            "feat/SW-GS-35-foo",
        );
        assert_eq!(
            extract_meta(PipelineFileFormat::Html, &once)
                .dev_branch
                .as_deref(),
            Some("feat/SW-GS-35-foo")
        );
        let twice = rewrite_field(
            PipelineFileFormat::Html,
            &once,
            "dev_branch",
            "feat/SW-GS-35-foo",
        );
        assert_eq!(
            twice, once,
            "HTML same-value rewrite must be byte-identical"
        );
    }

    #[test]
    fn rewrite_dev_branch_updates_on_change() {
        // AC-4: a new branch replaces the stale value (no lingering old branch).
        let md = "---\nstatus: todo\ndev_branch: feat/old-branch\n---\n# Title\n";
        let updated = rewrite_field(
            PipelineFileFormat::Markdown,
            md,
            "dev_branch",
            "fix/new-branch",
        );
        assert_eq!(
            extract_meta(PipelineFileFormat::Markdown, &updated)
                .dev_branch
                .as_deref(),
            Some("fix/new-branch")
        );
        assert!(!updated.contains("old-branch"), "stale value gone (AC-4)");
    }

    // ----- SW-WB-1: story-scoped branch_name (intended branch) --------------
    // Symmetric with the dev_branch coverage above: branch_name is the editable
    // INTENT field (distinct from the observed dev_branch and the epic-only
    // `branch`). Same parse/rewrite machinery, additive + back-compatible.

    #[test]
    fn branch_name_field_maps_to_cs_branch_name() {
        // AC-1: markdown `branch_name:` ↔ HTML `<meta name="cs:branch-name">`.
        assert_eq!(yaml_to_cs_name("branch_name"), Some("cs:branch-name"));
    }

    #[test]
    fn frontmatter_fields_branch_name_set_not_empty() {
        // AC-1: a branch_name-only block is NOT empty (gates the HTML extractor).
        let fm = FrontmatterFields {
            branch_name: Some("feat/SW-WB-1-foo".into()),
            ..Default::default()
        };
        assert!(!fm.is_empty());
    }

    #[test]
    fn markdown_branch_name_deserialized_distinct_from_dev_branch_and_branch() {
        // AC-1: parses independently; the observed dev_branch and epic `branch`
        // are untouched (three distinct fields).
        let md = "---\nstatus: todo\nbranch_name: feat/intended\ndev_branch: feat/observed\nsource_repo: org/repo\n---\n# Title\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert_eq!(fm.branch_name.as_deref(), Some("feat/intended"));
        assert_eq!(fm.dev_branch.as_deref(), Some("feat/observed"));
        assert!(fm.branch.is_none(), "epic `branch` untouched");
    }

    #[test]
    fn markdown_branch_name_absent_is_none() {
        // AC-4: older/never-edited doc parses exactly as today, branch_name None.
        let md = "---\nstatus: todo\n---\n# Title\n";
        let fm = extract_meta(PipelineFileFormat::Markdown, md);
        assert!(fm.branch_name.is_none());
    }

    #[test]
    fn html_branch_name_field_extracted() {
        // AC-1: HTML `cs:branch-name` round-trips via FIELD_NAME_MAP.
        let html = r#"<html><head>
            <meta name="cs:status" content="todo">
            <meta name="cs:branch-name" content="feat/SW-WB-1-foo">
        </head><body><h1>Title</h1></body></html>"#;
        let fm = extract_meta(PipelineFileFormat::Html, html);
        assert_eq!(fm.branch_name.as_deref(), Some("feat/SW-WB-1-foo"));
    }

    #[test]
    fn rewrite_branch_name_inserts_and_is_idempotent() {
        // AC-1: byte-preserving insert, then a same-value rewrite is a no-op.
        let md = "---\nstatus: todo\nsource_repo: org/repo\n---\n# Title\n";
        let once = rewrite_field(
            PipelineFileFormat::Markdown,
            md,
            "branch_name",
            "feat/SW-WB-1-foo",
        );
        assert!(once.contains("branch_name: feat/SW-WB-1-foo"));
        assert_eq!(
            extract_meta(PipelineFileFormat::Markdown, &once)
                .branch_name
                .as_deref(),
            Some("feat/SW-WB-1-foo")
        );
        let twice = rewrite_field(
            PipelineFileFormat::Markdown,
            &once,
            "branch_name",
            "feat/SW-WB-1-foo",
        );
        assert_eq!(twice, once, "same-value rewrite must be byte-identical");
    }

    #[test]
    fn rewrite_branch_name_html_round_trips() {
        // AC-1 for HTML: write `branch_name`, re-read exact value, idempotent.
        let html =
            "<html><head><meta name=\"cs:status\" content=\"todo\"></head><body></body></html>";
        let once = rewrite_field(
            PipelineFileFormat::Html,
            html,
            "branch_name",
            "feat/SW-WB-1-foo",
        );
        assert_eq!(
            extract_meta(PipelineFileFormat::Html, &once)
                .branch_name
                .as_deref(),
            Some("feat/SW-WB-1-foo")
        );
        let twice = rewrite_field(
            PipelineFileFormat::Html,
            &once,
            "branch_name",
            "feat/SW-WB-1-foo",
        );
        assert_eq!(
            twice, once,
            "HTML same-value rewrite must be byte-identical"
        );
    }

    #[test]
    fn html_team_field_extracted() {
        let html = r#"<html><head>
            <meta name="cs:status" content="todo">
            <meta name="cs:team" content="backend">
        </head><body><h1>Title</h1></body></html>"#;
        let fm = extract_meta(PipelineFileFormat::Html, html);
        assert_eq!(fm.team.as_deref(), Some("backend"));
        assert_eq!(fm.status.as_deref(), Some("todo"));
    }

    // ---------- SW-EW-5: epic frontmatter accessors ----------

    fn fields_with_repos(repos: &[&str]) -> FrontmatterFields {
        FrontmatterFields {
            repos: Some(repos.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn epic_repos_missing_is_empty() {
        assert!(FrontmatterFields::default().epic_repos().is_empty());
    }

    #[test]
    fn epic_repos_parses_valid_entries() {
        let fields = fields_with_repos(&["alderco/codesense", "alderco/install"]);
        let slugs = fields.epic_repos();
        assert_eq!(slugs.len(), 2);
        assert_eq!(
            slugs[0],
            SourceRepoSlug::new("alderco", "codesense").unwrap()
        );
        assert_eq!(slugs[1], SourceRepoSlug::new("alderco", "install").unwrap());
    }

    #[test]
    fn epic_repos_drops_malformed_keeps_valid() {
        // One typo (no slash) and one path-traversal entry are dropped; the
        // valid entry survives — a bad entry never fails the whole epic load.
        let fields = fields_with_repos(&["noslash", "alderco/codesense", "a/b/c"]);
        let slugs = fields.epic_repos();
        assert_eq!(slugs.len(), 1);
        assert_eq!(
            slugs[0],
            SourceRepoSlug::new("alderco", "codesense").unwrap()
        );
    }

    #[test]
    fn integration_branch_absent_derives_from_epic_id() {
        let fields = FrontmatterFields::default();
        assert_eq!(
            fields.resolve_integration_branch("SW-EW"),
            "integration/SW-EW"
        );
    }

    #[test]
    fn integration_branch_present_used_verbatim() {
        let fields = FrontmatterFields {
            branch: Some("release/2026-q3".to_string()),
            ..Default::default()
        };
        assert_eq!(
            fields.resolve_integration_branch("SW-EW"),
            "release/2026-q3"
        );
    }

    #[test]
    fn integration_branch_blank_falls_back_to_derived() {
        let fields = FrontmatterFields {
            branch: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(
            fields.resolve_integration_branch("SW-EW"),
            "integration/SW-EW"
        );
    }

    // ----- SW-GT-18 (AC-5): extract_metadata_keys ----------------------------

    #[test]
    fn metadata_keys_include_official_and_custom_markdown() {
        // The raw key set preserves custom keys `extract_meta` would drop.
        let md = "---\nstatus: todo\nstory_id: SW-9\nrisk_tier: high\n---\n# Body\n";
        let keys = extract_metadata_keys(PipelineFileFormat::Markdown, md);
        assert!(keys.contains(&"status".to_string()));
        assert!(keys.contains(&"story_id".to_string()));
        assert!(
            keys.contains(&"risk_tier".to_string()),
            "custom key preserved"
        );
    }

    #[test]
    fn metadata_keys_absent_or_empty_frontmatter_is_empty() {
        assert!(extract_metadata_keys(PipelineFileFormat::Markdown, "# no frontmatter").is_empty());
        assert!(extract_metadata_keys(PipelineFileFormat::Markdown, "---\n---\n").is_empty());
        assert!(extract_metadata_keys(PipelineFileFormat::Html, "<p>no meta</p>").is_empty());
    }

    #[test]
    fn metadata_keys_parity_between_markdown_and_html() {
        // AC-5: equivalent stories in either format yield the same normalized key set
        // — official names map cs:dash-form → yaml underscore-form; a custom field and
        // the nested `done` key both round-trip identically.
        let md = "---\nstatus: done\ndev_branch: feat/x\ndepends_on: [a]\n\
                  done:\n  completed_at: 2026-01-01\nrisk_tier: high\n---\n";
        let html = "<meta name=\"cs:status\" content=\"done\">\n\
                    <meta name=\"cs:dev-branch\" content=\"feat/x\">\n\
                    <meta name=\"cs:depends-on\" content=\"a\">\n\
                    <meta name=\"cs:done.completed_at\" content=\"2026-01-01\">\n\
                    <meta name=\"cs:risk_tier\" content=\"high\">";
        let mut md_keys = extract_metadata_keys(PipelineFileFormat::Markdown, md);
        let mut html_keys = extract_metadata_keys(PipelineFileFormat::Html, html);
        md_keys.sort();
        html_keys.sort();
        assert_eq!(md_keys, html_keys);
        assert_eq!(
            md_keys,
            vec!["depends_on", "dev_branch", "done", "risk_tier", "status"]
        );
    }

    #[test]
    fn metadata_keys_dedup_first_seen() {
        // A duplicate cs:* name is collapsed to one key (first-seen).
        let html = "<meta name=\"cs:status\" content=\"todo\">\n\
                    <meta name=\"cs:status\" content=\"done\">";
        assert_eq!(
            extract_metadata_keys(PipelineFileFormat::Html, html),
            vec!["status"]
        );
    }

    #[test]
    fn metadata_keys_mixed_case_custom_field_reconciles_across_formats() {
        // Code-review (Blind/Edge): HTML lowercases cs:* names; markdown preserved
        // author case. Canonicalization now makes both yield `risk_tier`, so a mixed-
        // case custom field can't produce a false cross-format mismatch.
        let md = "---\nRisk_Tier: high\n---\n";
        let html = "<meta name=\"cs:Risk_Tier\" content=\"high\">";
        assert_eq!(
            extract_metadata_keys(PipelineFileFormat::Markdown, md),
            vec!["risk_tier"]
        );
        assert_eq!(
            extract_metadata_keys(PipelineFileFormat::Html, html),
            vec!["risk_tier"]
        );
    }

    #[test]
    fn metadata_keys_flat_dotted_markdown_key_collapses_like_html() {
        // Code-review (Edge #3): a flat-dotted markdown key must collapse to its
        // top-level parent, matching the HTML `cs:done.completed_at` → `done` behavior.
        let md = "---\ndone.completed_at: 2026-01-01\n---\n";
        assert_eq!(
            extract_metadata_keys(PipelineFileFormat::Markdown, md),
            vec!["done"]
        );
    }

    #[test]
    fn metadata_keys_duplicate_yaml_key_recovers_via_line_scan() {
        // Code-review (Edge #1, HIGH): serde_yaml 0.9 errors on a duplicate mapping
        // key. The tolerant line-scan fallback must still surface the present keys
        // rather than silently wiping every one (which would falsely report a present
        // required field as missing to the runner consumer).
        let md = "---\nstatus: todo\nstatus: done\nrisk_tier: high\n---\n";
        let keys = extract_metadata_keys(PipelineFileFormat::Markdown, md);
        assert!(keys.contains(&"status".to_string()));
        assert!(
            keys.contains(&"risk_tier".to_string()),
            "keys not wiped: {keys:?}"
        );
    }

    // ----- SW-SR-10: story_template_values --------------------------------------

    #[test]
    fn template_values_markdown_html_parity() {
        // AC-5: the same logical story authored in each format yields the IDENTICAL
        // map — scalar, comma-scalar, multi-element list, custom field, and a nested
        // field (which must be absent from BOTH). This is the format-parity gate.
        let md = "---\n\
                  status: todo\n\
                  depends_on:\n  - SW-SR-3\n  - SW-GT-7\n\
                  risk_tier: high\n\
                  summary: needs auth, then dispatch\n\
                  done:\n  completed_at: 2026-01-01\n\
                  ---\n# Body\n";
        let html = "<meta name=\"cs:status\" content=\"todo\">\n\
                    <meta name=\"cs:depends-on\" content=\"SW-SR-3, SW-GT-7\">\n\
                    <meta name=\"cs:risk_tier\" content=\"high\">\n\
                    <meta name=\"cs:summary\" content=\"needs auth, then dispatch\">\n\
                    <meta name=\"cs:done.completed_at\" content=\"2026-01-01\">";
        let p = "/content/SW-SR-10.md";
        let from_md = story_template_values(PipelineFileFormat::Markdown, md, p);
        let from_html = story_template_values(PipelineFileFormat::Html, html, p);
        assert_eq!(from_md, from_html, "markdown/HTML maps must be identical");
        // Multi-element list joins to the CSV form on both sides (not first-value).
        assert_eq!(
            from_md.get("depends_on").map(String::as_str),
            Some("SW-SR-3, SW-GT-7")
        );
        // Comma-bearing scalar survives verbatim (not split).
        assert_eq!(
            from_md.get("summary").map(String::as_str),
            Some("needs auth, then dispatch")
        );
        // Nested field is dropped on both sides.
        assert!(!from_md.contains_key("done"), "nested `done` dropped");
    }

    #[test]
    fn template_values_hyphenated_custom_cs_field_matches_markdown_underscore() {
        // Custom fields follow the map convention (`cs:depends-on` ↔ `depends_on`): an HTML
        // custom field authored with the natural hyphen spelling must reconcile with the
        // equivalent markdown underscore field, or `{risk_tier}` resolves for one format and
        // fails dispatch for the other. Guards the cs→yaml hyphen fold (previously omitted).
        let md = "---\nstatus: todo\nrisk_tier: high\n---\n";
        let html = "<meta name=\"cs:status\" content=\"todo\">\n\
                    <meta name=\"cs:risk-tier\" content=\"high\">";
        let p = "/content/x.md";
        let from_md = story_template_values(PipelineFileFormat::Markdown, md, p);
        let from_html = story_template_values(PipelineFileFormat::Html, html, p);
        assert_eq!(
            from_md, from_html,
            "hyphen/underscore custom field must reconcile"
        );
        assert_eq!(from_html.get("risk_tier").map(String::as_str), Some("high"));
    }

    #[test]
    fn template_values_synthesizes_both_story_path_spellings() {
        // AC-1/§3: `{storyPath}` and `{story_path}` both resolve to the managed path.
        let md = "---\nstatus: todo\n---\n";
        let values = story_template_values(PipelineFileFormat::Markdown, md, "/content/x.md");
        assert_eq!(
            values.get("storypath").map(String::as_str),
            Some("/content/x.md")
        );
        assert_eq!(
            values.get("story_path").map(String::as_str),
            Some("/content/x.md")
        );
    }

    #[test]
    fn template_values_synthesized_path_wins_over_frontmatter_field() {
        // §3: a frontmatter field literally named `story_path` must NOT shadow the
        // synthesized managed path (dispatch relies on `{storyPath}` always resolving).
        let md = "---\nstatus: todo\nstory_path: /decoy/other.md\n---\n";
        let values = story_template_values(PipelineFileFormat::Markdown, md, "/content/real.md");
        assert_eq!(
            values.get("story_path").map(String::as_str),
            Some("/content/real.md")
        );
    }

    #[test]
    fn template_values_whitespace_only_value_is_absent() {
        // A blank/whitespace-only value resolves a `{field}` to nothing — treated as absent
        // (held at dispatch as unresolved) rather than substituted as a blank. Parity: HTML
        // `content="   "` is dropped the same way.
        let md = "---\nstatus: todo\nnote: \"   \"\n---\n";
        let values = story_template_values(PipelineFileFormat::Markdown, md, "/x.md");
        assert!(
            !values.contains_key("note"),
            "whitespace-only value dropped"
        );
        let html = "<meta name=\"cs:status\" content=\"todo\">\n\
                    <meta name=\"cs:note\" content=\"   \">";
        let hvalues = story_template_values(PipelineFileFormat::Html, html, "/x.md");
        assert!(
            !hvalues.contains_key("note"),
            "HTML whitespace-only dropped too"
        );
    }

    #[test]
    fn template_values_custom_field_resolves_a_prompt() {
        // AC-1: a custom field the scanner never names still resolves — the whole point.
        let md = "---\nstatus: todo\nrisk_tier: high\n---\n";
        let values = story_template_values(PipelineFileFormat::Markdown, md, "/content/x.md");
        assert_eq!(values.get("risk_tier").map(String::as_str), Some("high"));
    }

    #[test]
    fn template_values_absent_frontmatter_is_path_only() {
        // A doc with no frontmatter still yields the synthesized path vars (nothing else).
        let values =
            story_template_values(PipelineFileFormat::Markdown, "# just a title\n", "/p.md");
        assert_eq!(values.len(), 2);
        assert_eq!(values.get("storypath").map(String::as_str), Some("/p.md"));
    }

    #[test]
    fn template_values_malformed_frontmatter_yields_only_path() {
        // A duplicate-key (unparseable) markdown block resolves no field values — the
        // story is held at dispatch, never fired against blank vars.
        let md = "---\nstatus: todo\nstatus: done\n---\n";
        let values = story_template_values(PipelineFileFormat::Markdown, md, "/p.md");
        assert_eq!(values.len(), 2, "only the two path spellings: {values:?}");
    }
}
