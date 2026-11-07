// SPDX-License-Identifier: MIT
//! SLW-MD-1: the ONE per-field frontmatter *value* validator, shared by the
//! MCP authoring path (`mcp::story_authoring::validate_candidate`) and the
//! in-file metadata editor (`commands::frontmatter_editor`).
//!
//! **Why a separate module (DRY, story "Validation Model").** Before MD-1 the
//! per-field value checks — is `source_repo` a configured `owner/repo`? is
//! `type` in the closed set? is `status` canonical? is `team` configured? —
//! lived *inline* inside `validate_candidate`. MD-1 needs the exact same value
//! checks (same finding codes, same repo/team sets) for its nav badge and band,
//! but with a **different presence policy**: absence is always valid (a plain
//! doc that omits `type`/`source_repo` is not "invalid"), whereas the
//! story-authoring verdict treats a *story*'s missing `source_repo` as Blocking.
//!
//! So this module owns the **value** predicates (the shared core); each caller
//! supplies its own **presence/requiredness** layer on top:
//!   - `validate_candidate` keeps its `MISSING_*` requiredness (a story needs a
//!     `source_repo`) — it calls the individual field-check helpers here so the
//!     "is this present value valid?" rule can never drift between the two.
//!   - `frontmatter_editor` calls [`validate_fields`], whose rule is
//!     *absence = Valid*.
//!
//! Finding codes are the same stable strings `validate_story` publishes
//! (`UNKNOWN_SOURCE_REPO`, `INVALID_TYPE`, `INVALID_STATUS`, `UNKNOWN_TEAM`),
//! Severity maps onto the metadata band's
//! badge rule: Blocking → [`FieldState::Invalid`] (badge), Advisory/warning →
//! [`FieldState::Warning`] (no badge).

use serde::{Deserialize, Serialize};

use crate::commands::field_schema::{FieldDef, FieldSchema};
use crate::commands::pipeline_format::{FrontmatterFields, PIPELINE_DOCUMENT_TYPES};
use crate::commands::pipeline_scanner::VALID_STATUSES;
use crate::domain::{SourceRepoSlug, TagDef, TagSchema, TagType};

// ----- Stable finding codes (shared with `validate_story`) -------------------

/// `source_repo` present, a valid `owner/repo` slug, but NOT a member of a
/// *non-empty* configured set (matches `story_authoring::UNKNOWN_SOURCE_REPO`).
/// An empty configured set disables the membership check (syntactic-only) — see
/// [`source_repo_value_ok`]. A *malformed* slug emits [`MALFORMED_SOURCE_REPO`]
/// instead (AC-5).
pub const UNKNOWN_SOURCE_REPO: &str = "UNKNOWN_SOURCE_REPO";
/// `source_repo` present but NOT a syntactic `owner/repo` slug (fails
/// [`SourceRepoSlug::parse_owner_repo`]) — distinct from [`UNKNOWN_SOURCE_REPO`]
/// (parses fine but isn't in the non-empty configured set). AC-5: the band
/// shows `⚑ malformed` for this vs `⚑ not recognized` for unknown.
pub const MALFORMED_SOURCE_REPO: &str = "MALFORMED_SOURCE_REPO";
/// `type` present and not in [`PIPELINE_DOCUMENT_TYPES`] (matches
/// `story_authoring::INVALID_TYPE`). Note MD-1 only fires this on a *present*
/// value; absence is valid (a plain doc).
pub const INVALID_TYPE: &str = "INVALID_TYPE";
/// `status` present and not in [`VALID_STATUSES`] — advisory in MD-1 (the
/// locked decision keeps `status` free-text with suggestions), so it maps to
/// [`FieldState::Warning`], not a badge.
pub const INVALID_STATUS: &str = "INVALID_STATUS";
/// `team` present and not a configured team (matches
/// `story_authoring::UNKNOWN_TEAM`).
pub const UNKNOWN_TEAM: &str = "UNKNOWN_TEAM";
/// SW-DX-1: a schema-declared constrained tag (`select`/`ordered`/`status`/
/// `multi_select`) has a present value NOT in its `allowed_values`, or a `date`
/// tag's value isn't ISO-8601. Blocking → [`FieldState::Invalid`] (badge),
/// mirroring [`INVALID_TYPE`]. Keyed by the tag's own name (not a fixed field).
pub const INVALID_TAG_VALUE: &str = "INVALID_TAG_VALUE";

/// A plain value needs quotes, but does not necessarily invalidate a story.
pub const UNQUOTED_VALUE: &str = "UNQUOTED_VALUE";

pub(crate) fn unquoted_value_message(
    line: &crate::commands::pipeline_format::markdown::UnquotedLine,
    delimiter: &str,
) -> String {
    let quoted = crate::commands::pipeline_format::markdown::render_yaml_scalar(&line.value);
    format!(
        "Line {} ({}): the value contains \"{}\". Quote it: {}: {}",
        line.line, line.key, delimiter, line.key, quoted
    )
}

/// The scanner caches this diagnostic alongside recovered fields so a warm
/// scan has the same badge and reason as a cold scan.
pub(crate) fn metadata_parse_error(
    parsed: &crate::commands::pipeline_format::markdown::MarkdownFrontmatter,
) -> Option<String> {
    use crate::commands::pipeline_format::METADATA_CONTRACT_FIELDS;
    if parsed.block_parse_ok {
        return None;
    }
    if parsed.repaired_lines.is_empty() {
        return Some("unparseable metadata block".to_string());
    }
    let messages: Vec<_> = parsed
        .repaired_lines
        .iter()
        .filter(|line| METADATA_CONTRACT_FIELDS.contains(&line.key.as_str()))
        .map(|line| unquoted_value_message(line, ": "))
        .collect();
    (!messages.is_empty()).then(|| messages.join("\n"))
}

/// YAML interprets ` #42` as a comment, even when the author meant an issue id.
/// A spaced comment (` # note`) and author-owned metadata remain quiet.
pub(crate) fn unquoted_hash_lines(
    text: &str,
) -> Vec<crate::commands::pipeline_format::markdown::UnquotedLine> {
    use crate::commands::pipeline_format::{markdown, METADATA_CONTRACT_FIELDS};
    let Some(body) = markdown::raw_meta_block(text) else {
        return Vec::new();
    };
    body.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (key, value) = markdown::plain_scalar_line(line)?;
            if !METADATA_CONTRACT_FIELDS.contains(&key) {
                return None;
            }
            let after_hash = value
                .strip_prefix('#')
                .or_else(|| value.split_once(" #").map(|(_, after)| after))?;
            if after_hash.chars().next().is_none_or(char::is_whitespace) {
                return None;
            }
            Some(markdown::UnquotedLine {
                line: index + 2,
                key: key.to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

// ----- Shared per-field VALUE checks -----------------------------------------
// Each returns `true` when the *present* value is valid. They are the single
// predicate both callers key off — extract-not-copy so the two can't drift.

/// Is a *present* `source_repo` value valid? An empty/whitespace value is
/// treated as absent (valid) — presence policy is the caller's concern.
///
/// The membership rule depends on whether the configured set is known:
///   - **Empty `configured_repos`** ⇒ the configured set is *unknown* (a team
///     with zero configured repos, or a transiently-unresolved org that passes
///     `[]` via `unwrap_or_default()`). We do NOT flag on membership — badging
///     is disabled and we fall back to a **syntactic-only** check. A well-formed
///     `owner/repo` not in any set is still Valid (no badge). This mirrors
///     [`team_value_ok`]'s empty-set escape hatch, so an unresolved org never
///     badges every present `source_repo`. A *malformed* slug is STILL Invalid —
///     malformed is malformed regardless of the set.
///   - **Non-empty `configured_repos`** ⇒ Valid iff it parses as a syntactic
///     `owner/repo` slug AND is a member of the team's **configured** repo set
///     (locked decision #5 — configured, NOT attached).
pub fn source_repo_value_ok(value: &str, configured_repos: &[String]) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    // A malformed slug is always Invalid — even when the configured set is
    // unknown (malformed is malformed).
    if SourceRepoSlug::parse_owner_repo(trimmed).is_none() {
        return false;
    }
    // Empty configured set ⇒ unknown ⇒ syntactic check only (no membership
    // gate ⇒ no badge). Compare on the trimmed string so a doc value of
    // ` owner/repo ` still matches a configured `owner/repo`.
    if configured_repos.is_empty() {
        return true;
    }
    configured_repos.iter().any(|r| r.trim() == trimmed)
}

/// Is a *present* `type` value valid? Valid iff it is one of
/// [`PIPELINE_DOCUMENT_TYPES`] (case-insensitive, trimmed) — OR any other
/// non-empty string that isn't a *pipeline* type is ALSO valid for MD-1,
/// because a `type: doc` / `type: epic` is a legitimate plain-doc/epic marker,
/// not an error. The ONLY invalid `type` for MD-1 is one that looks like it
/// meant to be a pipeline story type but is misspelled — which we can't
/// distinguish — so MD-1's rule is: a present `type` is invalid only when it is
/// **not** a recognized pipeline type. This matches the story's Field Schema
/// table (`type` is a constrained select over `{story, bug, spike}` + none).
///
/// See [`type_value_ok`] callers: the editor flags a present non-pipeline
/// `type` as [`FieldState::Invalid`] so an unrecognized value surfaces a badge,
/// exactly as AC-11 requires ("`type` not in the set").
pub fn type_value_ok(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    PIPELINE_DOCUMENT_TYPES
        .iter()
        .chain(NON_STORY_DOCUMENT_TYPES)
        .any(|t| trimmed.eq_ignore_ascii_case(t))
}

/// Recognized `type` values for documents that are NOT stories: a live-data
/// dashboard page (`cs:type=dashboard`, the SDK's documented marker), an epic,
/// and the plain-doc opt-out (`cs:type=doc`). They are valid metadata, not a
/// misspelled story type, so they never badge. They do not make a file a
/// pipeline story: routing still keys on [`PIPELINE_DOCUMENT_TYPES`] alone.
pub const NON_STORY_DOCUMENT_TYPES: &[&str] = &["dashboard", "epic", "doc"];

/// Is a *present* `status` value valid (canonical)? Trimmed, case-sensitive
/// membership in [`VALID_STATUSES`] (matches `validate_candidate`). Empty =
/// treated as absent (valid).
pub fn status_value_ok(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || VALID_STATUSES.contains(&trimmed)
}

/// Is a *present* `team` value valid? Trimmed membership in the configured
/// team set (matches `validate_candidate`). Empty = absent (valid). An empty
/// `valid_teams` set means "teams unknown" → treat any value as valid (do not
/// badge when we simply couldn't resolve the team list).
pub fn team_value_ok(value: &str, valid_teams: &[String]) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || valid_teams.is_empty() {
        return true;
    }
    valid_teams.iter().any(|t| t.trim() == trimmed)
}

/// SW-DX-1: the ONE closed-set membership predicate shared by every
/// `select`/`ordered`/`status` (single) and `multi_select` (per item) schema
/// tag — the generalization of [`type_value_ok`] with a passed-in
/// `allowed_values` (exactly as `source_repo` gets its `configured_repos`).
///
/// Empty value ⇒ absent ⇒ valid (the cardinal rule). Empty `allowed_values` ⇒
/// the vocabulary is *unknown* ⇒ syntactic-only (no badge) — the same escape
/// hatch as `configured_repos.is_empty()` / `valid_teams.is_empty()`. Otherwise
/// Valid iff the trimmed value is a trimmed member of the set.
pub fn allowed_value_ok(value: &str, allowed_values: &[String]) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || allowed_values.is_empty() {
        return true;
    }
    allowed_values.iter().any(|a| a.trim() == trimmed)
}

/// SW-DX-1: a `date` tag's value must parse as an ISO-8601 calendar date
/// (`YYYY-MM-DD`) or an RFC-3339 datetime. Reuses the already-present `chrono`
/// dependency — no new crate for a validity badge. Empty ⇒ absent ⇒ valid.
pub fn date_value_ok(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Accept a calendar date (`YYYY-MM-DD`), an RFC-3339 datetime WITH offset
    // (`…+05:00` / `…Z`), and an offset-less ISO datetime (`YYYY-MM-DDTHH:MM:SS`)
    // — the last is a common hand-authored form `parse_from_rfc3339` rejects.
    chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").is_ok()
        || chrono::DateTime::parse_from_rfc3339(trimmed).is_ok()
        || chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S").is_ok()
}

/// SW-AU-77E: a `number` field's value must parse as a finite number. Empty ⇒
/// absent ⇒ valid (the cardinal rule). `inf`/`nan` are rejected — a metadata
/// number is a plain finite quantity, not a float sentinel.
pub fn number_value_ok(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    trimmed.parse::<f64>().map(f64::is_finite).unwrap_or(false)
}

/// SW-AU-77E: a `yes-no` field's value must be a boolean literal — `true`/`false`
/// or `yes`/`no` (case-insensitive, trimmed). Empty ⇒ absent ⇒ valid.
pub fn yes_no_value_ok(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no"
    )
}

/// SW-AU-77E: the ONE value validator for a team-declared custom field, keyed off
/// its authoring `type` (`text`/`number`/`yes-no`/`choice`/`date`). Reuses the
/// existing shared predicates so a declared field can never disagree with a schema
/// tag on what a valid value is:
/// - `choice` ⇒ [`allowed_value_ok`] membership;
/// - `date` ⇒ [`date_value_ok`] ISO-8601;
/// - `number` ⇒ [`number_value_ok`]; `yes-no` ⇒ [`yes_no_value_ok`];
/// - `text`, an unknown type, or an absent type ⇒ free-text (always valid) — the
///   tolerant posture the field-schema loader already takes toward `type`.
///
/// Empty value ⇒ valid (each predicate applies the absent-is-valid rule). The
/// type string is trimmed + lowercased and accepts the `yes_no` underscore spelling
/// alongside the canonical kebab `yes-no`.
pub fn validate_custom_field_value(
    value: &str,
    field_type: Option<&str>,
    allowed_values: &[String],
) -> bool {
    match field_type.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Some("choice") => allowed_value_ok(value, allowed_values),
        Some("date") => date_value_ok(value),
        Some("number") => number_value_ok(value),
        Some("yes-no") | Some("yes_no") => yes_no_value_ok(value),
        // `text`, unknown, or absent ⇒ free-text (tolerant): never invalid.
        _ => true,
    }
}

// ----- MD-1 field-validity model ---------------------------------------------

/// Per-field validity state (MD-1 nav badge + band). `Absent` and `Valid` and
/// `Warning` are all badge-free; only `Invalid` badges. Serialized camelCase
/// for the TS `FieldValidity` interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldState {
    /// Field is not present. Always valid (MD-1's cardinal rule; no badge).
    Absent,
    /// Field present with a valid value. No badge.
    Valid,
    /// Field present with an invalid value (Blocking → badge).
    Invalid,
    /// Field present, technically valid but worth flagging (Advisory → no
    /// badge), such as a non-canonical status.
    Warning,
}

/// One field's validity (MD-1). Serialized `{ field, state, message, code }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldValidity {
    /// Canonical field name (`source_repo`, `type`, `status`, `team`).
    pub field: String,
    pub state: FieldState,
    /// Human-readable reason when `state != Valid/Absent`. `None` otherwise.
    pub message: Option<String>,
    /// Stable finding code (SAME strings as `validate_story` where they overlap:
    /// `UNKNOWN_SOURCE_REPO`, `INVALID_TYPE`, `INVALID_STATUS`, `UNKNOWN_TEAM`).
    /// `None` for Valid/Absent.
    pub code: Option<String>,
}

/// Config the MD-1 validator checks against. Borrowed slices keep it pure.
pub struct ValidatorConfig<'a> {
    /// The team's **configured** `owner/repo` slugs (the validity bar — locked
    /// decision #5). NOT the attached set. Empty ⇒ "unknown" ⇒ source_repo
    /// membership badging disabled (syntactic check only; a malformed slug is
    /// still Invalid).
    pub configured_repos: &'a [String],
    /// Configured team names (a `team:` value must belong to this set). Empty
    /// ⇒ "unknown" ⇒ never badge a `team` value.
    pub valid_teams: &'a [String],
}

/// MD-1's per-field validity over the whole [`FrontmatterFields`] snapshot.
///
/// Presence policy: **absence = Valid** for every field (`Absent`). Present
/// values are checked with the shared per-field predicates above. This is the
/// shape the editor's `get_metadata_model` and the nav scan both consume.
///
/// Only these constrained fields produce a badge-worthy `Invalid`:
/// `source_repo`, `type`. `status` maps to `Warning` (no badge). `team` maps to `Invalid` (matches `validate_story`'s
/// Blocking `UNKNOWN_TEAM`). Free-text fields (`story_id`, `depends_on`,
/// `supporting_docs`, epic list fields, `branch`) are always `Valid`/`Absent`.
pub fn validate_fields(fields: &FrontmatterFields, cfg: &ValidatorConfig) -> Vec<FieldValidity> {
    let mut out = Vec::new();

    // source_repo — constrained; Invalid → badge. AC-5: distinguish
    // MALFORMED (not owner/repo) from UNKNOWN (owner/repo but not configured).
    out.push(source_repo_validity(
        fields.source_repo.as_deref(),
        cfg.configured_repos,
    ));

    // type — constrained; Invalid → badge.
    out.push(field_validity(
        "type",
        fields.document_type.as_deref(),
        type_value_ok,
        FieldState::Invalid,
        INVALID_TYPE,
        |v| format!("type '{v}' is not a recognized story type"),
    ));

    // status — free-text with suggestions (locked decision #4); a non-canonical
    // value is a WARNING, never a badge.
    out.push(field_validity(
        "status",
        fields.status.as_deref(),
        status_value_ok,
        FieldState::Warning,
        INVALID_STATUS,
        |v| format!("status '{v}' is not a canonical pipeline status"),
    ));

    // team — constrained; Invalid → badge (matches validate_story Blocking).
    out.push(field_validity(
        "team",
        fields.team.as_deref(),
        |v| team_value_ok(v, cfg.valid_teams),
        FieldState::Invalid,
        UNKNOWN_TEAM,
        |v| format!("team '{v}' is not a configured team"),
    ));

    out
}

/// SW-DX-1: per-tag validity for the schema-declared tags PRESENT in a doc.
///
/// **Additive sibling of [`validate_fields`], NOT a fork.** It emits the SAME
/// [`FieldValidity`] shape (keyed by the tag's own name) using the SAME
/// [`FieldState`] enum and the SAME shared predicates ([`allowed_value_ok`] /
/// [`date_value_ok`]) — so the MCP authoring verdict and the in-file editor
/// badge can never disagree.
///
/// `present_tags` are the doc's OPEN frontmatter keys with structured values (a
/// scalar is a 1-item slice, a list is N items) — see the editor's
/// `open_tag_values` extractor. Rules:
/// - a key with NO matching [`TagDef`] produces **no** entry (the cardinal
///   open-key rule — unknown keys stay unopinionated);
/// - only PRESENT schema tags produce an entry (absence is valid, so no entry);
/// - `text`/`relation`, and any closed-set tag with an EMPTY `allowed_values`,
///   are syntactic-only ⇒ `Valid`, no badge (the empty-set escape hatch).
pub fn validate_schema_tags(
    present_tags: &[(String, Vec<String>)],
    schema: &TagSchema,
) -> Vec<FieldValidity> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (key, items) in present_tags {
        if let Some(def) = schema.tag(key) {
            // First-wins per tag: a doc that repeats an open key (an HTML doc
            // with two identical `<meta cs:*>` tags — markdown mappings can't
            // repeat a key) must not emit two badges for the same tag.
            if seen.insert(def.name.as_str()) {
                out.push(tag_validity(def, items));
            }
        }
    }
    out
}

/// Normalize a present open-tag value list into per-item values. A `multi_select`
/// value may arrive comma-joined (the HTML `cs:*` CSV form) or as a real list
/// (markdown); split on comma so both formats yield identical items. Non-multi
/// types are taken whole — a scalar `select`/`date` value is never comma-split.
/// Each item is trimmed and empties dropped.
///
/// Splitting on comma for `multi_select` is sound because the write path
/// (`apply_metadata_edit` SetList) REJECTS a comma inside any list item for both
/// formats, so a well-formed item never legitimately contains a comma — a comma is
/// always a separator. ONE home for this rule so a `multi_select` row's chips
/// (SW-AU-77I `collect_schema_rows`) and its validity verdict (`tag_validity`) are
/// derived identically and can never disagree.
pub(crate) fn split_multi_select_items(items: &[String], is_multi_select: bool) -> Vec<String> {
    if is_multi_select {
        items
            .iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        items
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// One schema tag's validity over its present value(s). See [`validate_schema_tags`].
fn tag_validity(def: &TagDef, items: &[String]) -> FieldValidity {
    let field = def.name.clone();

    let present: Vec<String> = split_multi_select_items(items, def.r#type == TagType::MultiSelect);

    // Absence is valid (the cardinal rule): no present value ⇒ Absent, no badge.
    if present.is_empty() {
        return FieldValidity {
            field,
            state: FieldState::Absent,
            message: None,
            code: None,
        };
    }

    // The first offending value, per the tag's type. `text`/`relation` (and any
    // other non-closed, non-date type) never offend.
    let bad = match def.r#type {
        TagType::Date => present.iter().find(|v| !date_value_ok(v)),
        t if t.is_closed_set() => present
            .iter()
            .find(|v| !allowed_value_ok(v, &def.allowed_values)),
        _ => None,
    };

    match bad {
        None => FieldValidity {
            field,
            state: FieldState::Valid,
            message: None,
            code: None,
        },
        Some(v) => {
            let message = if def.r#type == TagType::Date {
                format!("{field} value '{v}' is not an ISO-8601 date")
            } else {
                format!("{field} value '{v}' is not in the allowed set")
            };
            FieldValidity {
                field,
                state: FieldState::Invalid,
                message: Some(message),
                code: Some(INVALID_TAG_VALUE.to_string()),
            }
        }
    }
}

/// SW-AU-77G: per-field validity for the **team-declared** custom fields PRESENT
/// in a doc — the sibling of [`validate_schema_tags`] for the team `FieldSchema`
/// (the store 77E/Settings write), routed into the SAME `per_field_validity` list
/// the built-ins and org tags already use. Only present keys matched to a CUSTOM
/// (`!official`) `FieldDef` produce an entry; official fields are already checked
/// by [`validate_fields`], and an unmatched open key stays unopinionated. Reuses
/// the one shared [`validate_custom_field_value`] 77E ships, so the drawer's live
/// flag can never diverge from its declare-time check. First value per field only
/// (single-valued declared fields; list-typed follow the built-in list path).
pub fn validate_declared_field_values(
    present_declared: &[(String, Vec<String>)],
    schema: &FieldSchema,
) -> Vec<FieldValidity> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (key, items) in present_declared {
        if let Some(def) = schema.custom_field(key) {
            // First-wins per field: an HTML doc may repeat an open `cs:*` key; it
            // must not emit two entries (mirrors `validate_schema_tags`).
            if seen.insert(def.name.as_str()) {
                out.push(declared_field_validity(def, items));
            }
        }
    }
    out
}

/// One declared field's validity over its present value(s). Empty/whitespace ⇒
/// `Absent` (the cardinal missing-≠-invalid rule); otherwise the shared value
/// validator decides, with a type-specific reason. See [`validate_declared_field_values`].
fn declared_field_validity(def: &FieldDef, items: &[String]) -> FieldValidity {
    let field = def.name.clone();
    let ty = def.r#type.as_deref();
    let value = items.iter().map(|s| s.trim()).find(|s| !s.is_empty());
    let Some(value) = value else {
        return FieldValidity {
            field,
            state: FieldState::Absent,
            message: None,
            code: None,
        };
    };
    if validate_custom_field_value(value, ty, &def.allowed_values) {
        return FieldValidity {
            field,
            state: FieldState::Valid,
            message: None,
            code: None,
        };
    }
    let message = format!("{field} value '{value}' {}", declared_invalid_reason(ty));
    FieldValidity {
        field,
        state: FieldState::Invalid,
        message: Some(message),
        code: Some(INVALID_TAG_VALUE.to_string()),
    }
}

/// The type-specific tail of a declared field's invalid reason (AC-3). `text`
/// (and any unknown type) never reaches here — the value validator returns valid.
fn declared_invalid_reason(field_type: Option<&str>) -> &'static str {
    match field_type.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Some("choice") => "is not an allowed value",
        Some("number") => "is not a number",
        Some("date") => "is not a date (use YYYY-MM-DD)",
        Some("yes-no") | Some("yes_no") => "is not yes or no",
        _ => "is invalid",
    }
}

/// True when at least one field is `Invalid` — the nav-badge trigger for the
/// *present-invalid* case (the *unparseable* case is handled by the caller).
pub fn has_invalid(validities: &[FieldValidity]) -> bool {
    validities.iter().any(|v| v.state == FieldState::Invalid)
}

/// The first `Invalid` field's message (for the nav badge tooltip / aria-label).
pub fn first_invalid_reason(validities: &[FieldValidity]) -> Option<String> {
    validities
        .iter()
        .find(|v| v.state == FieldState::Invalid)
        .and_then(|v| v.message.clone())
}

// ----- helpers ---------------------------------------------------------------

/// Build a `FieldValidity` for a simple present-value-checked field. Absent
/// (or whitespace-only) ⇒ `Absent`. Present + `ok` ⇒ `Valid`. Present + !ok ⇒
/// `bad_state` with `msg(value)`.
fn field_validity(
    field: &str,
    value: Option<&str>,
    ok: impl Fn(&str) -> bool,
    bad_state: FieldState,
    bad_code: &str,
    msg: impl Fn(&str) -> String,
) -> FieldValidity {
    match value.map(str::trim) {
        None | Some("") => FieldValidity {
            field: field.to_string(),
            state: FieldState::Absent,
            message: None,
            code: None,
        },
        Some(v) if ok(v) => FieldValidity {
            field: field.to_string(),
            state: FieldState::Valid,
            message: None,
            code: None,
        },
        Some(v) => FieldValidity {
            field: field.to_string(),
            state: bad_state,
            message: Some(msg(v)),
            code: Some(bad_code.to_string()),
        },
    }
}

/// AC-5: `source_repo` validity, distinguishing MALFORMED from UNKNOWN.
///   - Absent / whitespace-only ⇒ `Absent`.
///   - Fails `parse_owner_repo` (not a syntactic `owner/repo`) ⇒ `Invalid`
///     with [`MALFORMED_SOURCE_REPO`].
///   - Parses but not in the *non-empty* configured set ⇒ `Invalid` with
///     [`UNKNOWN_SOURCE_REPO`].
///   - Parses and (empty set OR member) ⇒ `Valid`.
///
/// The membership half mirrors [`source_repo_value_ok`]'s empty-set escape
/// hatch: an empty configured set disables the membership check, so a
/// well-formed slug is Valid there (never UNKNOWN) — only malformed still flags.
fn source_repo_validity(value: Option<&str>, configured_repos: &[String]) -> FieldValidity {
    let field = "source_repo".to_string();
    let trimmed = value.map(str::trim);
    match trimmed {
        None | Some("") => FieldValidity {
            field,
            state: FieldState::Absent,
            message: None,
            code: None,
        },
        Some(v) if SourceRepoSlug::parse_owner_repo(v).is_none() => FieldValidity {
            field,
            state: FieldState::Invalid,
            message: Some(format!("source_repo '{v}' is not a valid owner/repo slug")),
            code: Some(MALFORMED_SOURCE_REPO.to_string()),
        },
        // Parses. Empty configured set ⇒ membership disabled ⇒ Valid.
        Some(v)
            if configured_repos.is_empty() || configured_repos.iter().any(|r| r.trim() == v) =>
        {
            FieldValidity {
                field,
                state: FieldState::Valid,
                message: None,
                code: None,
            }
        }
        Some(v) => FieldValidity {
            field,
            state: FieldState::Invalid,
            message: Some(format!("source_repo '{v}' is not a configured repo")),
            code: Some(UNKNOWN_SOURCE_REPO.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg<'a>(repos: &'a [String], teams: &'a [String]) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
        }
    }

    #[test]
    fn split_multi_select_items_matches_across_formats() {
        // The HTML CSV form and the markdown list form yield identical items.
        let html_csv = vec!["alice, bob".to_string()];
        let md_list = vec!["alice".to_string(), "bob".to_string()];
        let want = vec!["alice".to_string(), "bob".to_string()];
        assert_eq!(split_multi_select_items(&html_csv, true), want);
        assert_eq!(split_multi_select_items(&md_list, true), want);
        // Trims and drops empties.
        assert_eq!(
            split_multi_select_items(&[" a , , b ".to_string()], true),
            vec!["a".to_string(), "b".to_string()]
        );
        // Non-multi is never comma-split — a scalar value is taken whole.
        assert_eq!(
            split_multi_select_items(&["alice, bob".to_string()], false),
            vec!["alice, bob".to_string()]
        );
    }

    fn state_of<'a>(vs: &'a [FieldValidity], field: &str) -> &'a FieldValidity {
        vs.iter().find(|v| v.field == field).expect("field present")
    }

    #[test]
    fn source_repo_configured_is_valid() {
        let repos = vec!["alderco/codesense".to_string()];
        assert!(source_repo_value_ok("alderco/codesense", &repos));
    }

    #[test]
    fn source_repo_configured_not_attached_is_valid() {
        // Decision #5: configured-but-not-attached is valid. "Attached" is not
        // an input here at all — the configured set is the whole bar.
        let repos = vec![
            "alderco/codesense".to_string(),
            "alderco/install".to_string(),
        ];
        assert!(source_repo_value_ok("alderco/install", &repos));
    }

    #[test]
    fn source_repo_not_configured_is_invalid() {
        let repos = vec!["alderco/codesense".to_string()];
        assert!(!source_repo_value_ok("acme/old", &repos));
    }

    #[test]
    fn source_repo_malformed_is_invalid() {
        let repos = vec!["alderco/codesense".to_string()];
        assert!(!source_repo_value_ok("noslash", &repos));
        assert!(!source_repo_value_ok("a/b/c", &repos));
    }

    #[test]
    fn source_repo_empty_is_valid() {
        assert!(source_repo_value_ok("", &[]));
        assert!(source_repo_value_ok("   ", &[]));
    }

    #[test]
    fn source_repo_empty_configured_set_never_flags() {
        // Empty configured_repos = "we don't know the repos" (zero-configured
        // team or an unresolved org passing `[]`) → do NOT badge. A well-formed
        // owner/repo that isn't in any set is still Valid (syntactic-only).
        assert!(source_repo_value_ok("acme/anything", &[]));
        assert!(source_repo_value_ok("owner/repo", &[]));
    }

    #[test]
    fn source_repo_empty_configured_set_still_flags_malformed() {
        // Even with an unknown (empty) set, a malformed slug is Invalid —
        // malformed is malformed regardless of the configured set.
        assert!(!source_repo_value_ok("noslash", &[]));
        assert!(!source_repo_value_ok("a/b/c", &[]));
    }

    #[test]
    fn source_repo_nonempty_set_not_a_member_is_invalid() {
        // Non-empty configured set ⇒ syntactic AND membership.
        let repos = vec!["alderco/codesense".to_string()];
        assert!(!source_repo_value_ok("acme/not-a-member", &repos));
    }

    #[test]
    fn type_in_set_valid_out_of_set_invalid() {
        assert!(type_value_ok("story"));
        assert!(type_value_ok("BUG"));
        assert!(type_value_ok(" spike "));
        assert!(!type_value_ok("tale"));
        assert!(type_value_ok("")); // empty = valid (absent)
                                    // Recognized non-story document types are valid metadata, not a badge.
        assert!(type_value_ok("dashboard"));
        assert!(type_value_ok("Epic"));
        assert!(type_value_ok("doc"));
    }

    #[test]
    fn status_canonical_vs_freetext() {
        assert!(status_value_ok("todo"));
        assert!(!status_value_ok("nonsense"));
    }

    #[test]
    fn team_unknown_set_never_flags() {
        // Empty valid_teams = "we don't know the teams" → never badge.
        assert!(team_value_ok("anything", &[]));
    }

    #[test]
    fn validate_fields_absent_all_valid() {
        let fields = FrontmatterFields::default();
        let vs = validate_fields(&fields, &cfg(&[], &[]));
        assert!(vs.iter().all(|v| v.state == FieldState::Absent));
        assert!(!has_invalid(&vs));
    }

    #[test]
    fn validate_fields_invalid_source_repo_badges() {
        let repos = vec!["alderco/codesense".to_string()];
        let fields = FrontmatterFields {
            source_repo: Some("acme/old".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&repos, &[]));
        assert_eq!(state_of(&vs, "source_repo").state, FieldState::Invalid);
        assert!(has_invalid(&vs));
        assert!(first_invalid_reason(&vs).unwrap().contains("acme/old"));
    }

    #[test]
    fn validate_fields_bad_status_is_warning_not_badge() {
        let fields = FrontmatterFields {
            status: Some("weird".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&[], &[]));
        assert_eq!(state_of(&vs, "status").state, FieldState::Warning);
        assert!(!has_invalid(&vs), "bad status must not badge");
    }

    #[test]
    fn validate_fields_malformed_source_repo_emits_malformed_code() {
        // AC-5: a value that fails owner/repo parse → MALFORMED_SOURCE_REPO,
        // regardless of the configured set (malformed is malformed).
        let repos = vec!["alderco/codesense".to_string()];
        let fields = FrontmatterFields {
            source_repo: Some("noslash".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&repos, &[]));
        let sr = state_of(&vs, "source_repo");
        assert_eq!(sr.state, FieldState::Invalid);
        assert_eq!(sr.code.as_deref(), Some(MALFORMED_SOURCE_REPO));

        // Empty configured set: malformed STILL flags as MALFORMED.
        let vs_empty = validate_fields(&fields, &cfg(&[], &[]));
        assert_eq!(
            state_of(&vs_empty, "source_repo").code.as_deref(),
            Some(MALFORMED_SOURCE_REPO)
        );
    }

    #[test]
    fn validate_fields_unknown_source_repo_emits_unknown_code() {
        // AC-5: parses as owner/repo but not in the non-empty set → UNKNOWN.
        let repos = vec!["alderco/codesense".to_string()];
        let fields = FrontmatterFields {
            source_repo: Some("acme/other".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&repos, &[]));
        let sr = state_of(&vs, "source_repo");
        assert_eq!(sr.state, FieldState::Invalid);
        assert_eq!(sr.code.as_deref(), Some(UNKNOWN_SOURCE_REPO));
    }

    #[test]
    fn validate_fields_empty_configured_set_wellformed_is_valid() {
        // A well-formed slug with an unknown (empty) set is Valid — never
        // UNKNOWN — per the empty-set escape hatch.
        let fields = FrontmatterFields {
            source_repo: Some("acme/anything".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&[], &[]));
        assert_eq!(state_of(&vs, "source_repo").state, FieldState::Valid);
    }

    #[test]
    fn validate_fields_bad_type_badges() {
        let fields = FrontmatterFields {
            document_type: Some("tale".to_string()),
            ..Default::default()
        };
        let vs = validate_fields(&fields, &cfg(&[], &[]));
        assert_eq!(state_of(&vs, "type").state, FieldState::Invalid);
        assert!(has_invalid(&vs));
    }

    // ===== SW-DX-1: schema tag predicates + validate_schema_tags =============

    fn tag(name: &str, ty: TagType, allowed: &[&str]) -> TagDef {
        TagDef {
            name: name.to_string(),
            r#type: ty,
            allowed_values: allowed.iter().map(|s| s.to_string()).collect(),
            color: None,
            icon: None,
        }
    }

    fn schema(tags: Vec<TagDef>) -> TagSchema {
        TagSchema { tags }
    }

    fn present(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(k, vs)| {
                (
                    k.to_string(),
                    vs.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    // ---- AC-3/4: shared closed-set + date predicates -----------------------

    #[test]
    fn allowed_value_ok_membership_and_escape_hatch() {
        let set = vec!["low".to_string(), "high".to_string()];
        assert!(allowed_value_ok("low", &set));
        assert!(allowed_value_ok(" high ", &set), "trimmed match");
        assert!(!allowed_value_ok("urgent", &set));
        // Empty value ⇒ absent ⇒ valid.
        assert!(allowed_value_ok("", &set));
        // Empty allowed set ⇒ vocabulary unknown ⇒ syntactic-only (no badge).
        assert!(allowed_value_ok("anything", &[]));
    }

    #[test]
    fn date_value_ok_iso8601() {
        assert!(date_value_ok("2026-07-24"));
        assert!(date_value_ok("2026-01-01T00:00:00Z"), "rfc3339 datetime");
        assert!(
            date_value_ok("2026-01-01T00:00:00"),
            "offset-less ISO datetime (review m4)"
        );
        assert!(
            date_value_ok("2026-01-01T12:30:00+05:00"),
            "rfc3339 with offset"
        );
        assert!(date_value_ok(""), "empty ⇒ valid");
        assert!(!date_value_ok("24/07/2026"));
        assert!(!date_value_ok("not-a-date"));
        assert!(!date_value_ok("2026-13-40"), "impossible date");
    }

    // ---- SW-AU-77E: number / yes-no / custom-field value validator ----------

    #[test]
    fn number_value_ok_finite_only() {
        assert!(number_value_ok("42"));
        assert!(number_value_ok("-3.5"));
        assert!(number_value_ok(" 1000 "), "trimmed");
        assert!(number_value_ok(""), "empty ⇒ valid");
        assert!(number_value_ok("+5"), "leading sign accepted");
        assert!(number_value_ok("5."), "trailing dot accepted");
        assert!(!number_value_ok("abc"));
        assert!(!number_value_ok("1.2.3"));
        assert!(!number_value_ok("1,000"), "thousands separator rejected");
        assert!(!number_value_ok("1_000"), "underscore rejected");
        assert!(!number_value_ok("inf"), "sentinel rejected");
        assert!(!number_value_ok("nan"), "sentinel rejected");
    }

    #[test]
    fn yes_no_value_ok_boolean_literals() {
        for ok in ["true", "false", "yes", "no", "YES", "No", " true "] {
            assert!(yes_no_value_ok(ok), "{ok} should be valid");
        }
        assert!(yes_no_value_ok(""), "empty ⇒ valid");
        assert!(!yes_no_value_ok("maybe"));
        assert!(!yes_no_value_ok("1"));
    }

    #[test]
    fn validate_custom_field_value_dispatches_by_type() {
        let choices = vec!["low".to_string(), "high".to_string()];
        // choice ⇒ membership
        assert!(validate_custom_field_value(
            "high",
            Some("choice"),
            &choices
        ));
        assert!(!validate_custom_field_value(
            "urgent",
            Some("choice"),
            &choices
        ));
        // number
        assert!(validate_custom_field_value("3.14", Some("number"), &[]));
        assert!(!validate_custom_field_value("ten", Some("number"), &[]));
        // yes-no (both spellings)
        assert!(validate_custom_field_value("yes", Some("yes-no"), &[]));
        assert!(validate_custom_field_value("false", Some("yes_no"), &[]));
        assert!(!validate_custom_field_value("perhaps", Some("yes-no"), &[]));
        // date
        assert!(validate_custom_field_value("2026-09-02", Some("date"), &[]));
        assert!(!validate_custom_field_value("someday", Some("date"), &[]));
        // text / unknown / absent ⇒ tolerant
        assert!(validate_custom_field_value("anything", Some("text"), &[]));
        assert!(validate_custom_field_value("anything", Some("kanban"), &[]));
        assert!(validate_custom_field_value("anything", None, &[]));
        // empty value ⇒ valid regardless of type
        assert!(validate_custom_field_value("", Some("number"), &[]));
        assert!(validate_custom_field_value("  ", Some("choice"), &choices));
    }

    #[test]
    fn schema_tag_duplicate_open_keys_dedupe_first_wins() {
        // Review (Blind #1): an HTML doc repeating `cs:priority` must not emit
        // two badges for `priority` — first-wins dedupe.
        let s = schema(vec![tag("priority", TagType::Select, &["low", "high"])]);
        let vs = validate_schema_tags(
            &present(&[("priority", &["low"]), ("priority", &["bogus"])]),
            &s,
        );
        assert_eq!(vs.len(), 1, "one entry per tag");
        assert_eq!(vs[0].state, FieldState::Valid, "first value wins");
    }

    #[test]
    fn schema_tag_unknown_type_is_always_valid() {
        // Review M1: an `Unknown`-typed tag (unknown `type:` string) is treated
        // as free-text — a present value never badges.
        let s = schema(vec![tag("board", TagType::Unknown, &["a", "b"])]);
        let vs = validate_schema_tags(&present(&[("board", &["anything"])]), &s);
        assert_eq!(vs[0].state, FieldState::Valid);
    }

    // ---- AC-3: select/ordered/status closed-set validity -------------------

    #[test]
    fn schema_tag_select_present_out_of_vocab_badges() {
        let s = schema(vec![tag("priority", TagType::Select, &["low", "high"])]);
        let vs = validate_schema_tags(&present(&[("priority", &["urgent"])]), &s);
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].state, FieldState::Invalid);
        assert_eq!(vs[0].code.as_deref(), Some(INVALID_TAG_VALUE));
        assert_eq!(vs[0].field, "priority");
    }

    #[test]
    fn schema_tag_select_in_vocab_is_valid() {
        let s = schema(vec![tag("priority", TagType::Select, &["low", "high"])]);
        let vs = validate_schema_tags(&present(&[("priority", &["high"])]), &s);
        assert_eq!(vs[0].state, FieldState::Valid);
        assert!(!has_invalid(&vs));
    }

    #[test]
    fn schema_tag_empty_value_is_absent() {
        // AC-3: absence is Valid (Absent) — a present-but-empty value doesn't badge.
        let s = schema(vec![tag("priority", TagType::Select, &["low"])]);
        let vs = validate_schema_tags(&present(&[("priority", &[""])]), &s);
        assert_eq!(vs[0].state, FieldState::Absent);
    }

    #[test]
    fn schema_tag_empty_allowed_values_is_syntactic_only() {
        // AC-3/4: empty allowed_values ⇒ vocabulary unknown ⇒ no badge.
        let s = schema(vec![tag("priority", TagType::Select, &[])]);
        let vs = validate_schema_tags(&present(&[("priority", &["whatever"])]), &s);
        assert_eq!(vs[0].state, FieldState::Valid, "empty-set escape hatch");
    }

    // ---- AC-4: multi_select per item (list + CSV forms) --------------------

    #[test]
    fn schema_tag_multi_select_per_item_list_form() {
        let s = schema(vec![tag("owners", TagType::MultiSelect, &["alice", "bob"])]);
        // Markdown list form: real N items.
        let ok = validate_schema_tags(&present(&[("owners", &["alice", "bob"])]), &s);
        assert_eq!(ok[0].state, FieldState::Valid);
        let bad = validate_schema_tags(&present(&[("owners", &["alice", "carol"])]), &s);
        assert_eq!(bad[0].state, FieldState::Invalid);
        assert_eq!(bad[0].code.as_deref(), Some(INVALID_TAG_VALUE));
    }

    #[test]
    fn schema_tag_multi_select_per_item_csv_form() {
        // HTML `cs:*` CSV form arrives as one comma-joined item — must be split
        // and validated per item.
        let s = schema(vec![tag("owners", TagType::MultiSelect, &["alice", "bob"])]);
        let ok = validate_schema_tags(&present(&[("owners", &["alice, bob"])]), &s);
        assert_eq!(ok[0].state, FieldState::Valid, "CSV split per item");
        let bad = validate_schema_tags(&present(&[("owners", &["alice, carol"])]), &s);
        assert_eq!(bad[0].state, FieldState::Invalid);
    }

    // ---- AC-4: date / text / relation --------------------------------------

    #[test]
    fn schema_tag_date_validity() {
        let s = schema(vec![tag("due", TagType::Date, &[])]);
        assert_eq!(
            validate_schema_tags(&present(&[("due", &["2026-07-24"])]), &s)[0].state,
            FieldState::Valid
        );
        let bad = validate_schema_tags(&present(&[("due", &["someday"])]), &s);
        assert_eq!(bad[0].state, FieldState::Invalid);
        assert_eq!(bad[0].code.as_deref(), Some(INVALID_TAG_VALUE));
    }

    #[test]
    fn schema_tag_text_and_relation_always_valid() {
        let s = schema(vec![
            tag("note", TagType::Text, &[]),
            tag("blocks", TagType::Relation, &[]),
        ]);
        let vs = validate_schema_tags(
            &present(&[("note", &["anything goes"]), ("blocks", &["SW-DX-2"])]),
            &s,
        );
        assert!(vs.iter().all(|v| v.state == FieldState::Valid));
    }

    // ---- AC-5/6: ordered closed-set + order preserved ----------------------

    #[test]
    fn schema_tag_ordered_out_of_vocab_badges_in_vocab_valid() {
        let s = schema(vec![tag(
            "context_level",
            TagType::Ordered,
            &["machine", "team", "management", "leadership"],
        )]);
        assert_eq!(
            validate_schema_tags(&present(&[("context_level", &["team"])]), &s)[0].state,
            FieldState::Valid
        );
        let bad = validate_schema_tags(&present(&[("context_level", &["cosmic"])]), &s);
        assert_eq!(bad[0].state, FieldState::Invalid);
        assert_eq!(bad[0].code.as_deref(), Some(INVALID_TAG_VALUE));
    }

    // ---- AC-7: open key with no TagDef gets no entry -----------------------

    #[test]
    fn open_key_without_tagdef_produces_no_entry() {
        let s = schema(vec![tag("priority", TagType::Select, &["low"])]);
        // `arbitrary` has no TagDef → no validity entry; `priority` does.
        let vs = validate_schema_tags(
            &present(&[("priority", &["low"]), ("arbitrary", &["whatever"])]),
            &s,
        );
        assert_eq!(vs.len(), 1, "only the schema tag is validated");
        assert_eq!(vs[0].field, "priority");
    }

    #[test]
    fn empty_schema_validates_nothing() {
        let vs = validate_schema_tags(&present(&[("priority", &["low"])]), &TagSchema::default());
        assert!(vs.is_empty(), "no schema ⇒ no opinion on any key");
    }

    // ===== SW-AU-77G: validate_declared_field_values ========================

    fn custom(name: &str, ty: &str, allowed: &[&str]) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            r#type: Some(ty.to_string()),
            allowed_values: allowed.iter().map(|s| s.to_string()).collect(),
            required: false,
            official: false,
        }
    }

    fn field_schema(customs: Vec<FieldDef>) -> FieldSchema {
        FieldSchema {
            fields: customs,
            warnings: Vec::new(),
        }
    }

    fn state_by<'a>(vs: &'a [FieldValidity], field: &str) -> &'a FieldValidity {
        vs.iter().find(|v| v.field == field).expect("entry present")
    }

    #[test]
    fn declared_choice_out_of_vocab_is_invalid_with_reason() {
        let s = field_schema(vec![custom("risk", "choice", &["low", "high"])]);
        let vs = validate_declared_field_values(&present(&[("risk", &["cosmic"])]), &s);
        let v = state_by(&vs, "risk");
        assert_eq!(v.state, FieldState::Invalid);
        assert_eq!(v.code.as_deref(), Some(INVALID_TAG_VALUE));
        assert_eq!(
            v.message.as_deref(),
            Some("risk value 'cosmic' is not an allowed value")
        );
    }

    #[test]
    fn declared_choice_in_vocab_is_valid_no_flag() {
        let s = field_schema(vec![custom("risk", "choice", &["low", "high"])]);
        let vs = validate_declared_field_values(&present(&[("risk", &["low"])]), &s);
        assert_eq!(state_by(&vs, "risk").state, FieldState::Valid);
        assert!(!has_invalid(&vs));
    }

    #[test]
    fn declared_number_and_date_and_yes_no_reasons() {
        let s = field_schema(vec![
            custom("cost", "number", &[]),
            custom("due", "date", &[]),
            custom("blocking", "yes-no", &[]),
        ]);
        let vs = validate_declared_field_values(
            &present(&[
                ("cost", &["ten"]),
                ("due", &["someday"]),
                ("blocking", &["maybe"]),
            ]),
            &s,
        );
        assert_eq!(
            state_by(&vs, "cost").message.as_deref(),
            Some("cost value 'ten' is not a number")
        );
        assert_eq!(
            state_by(&vs, "due").message.as_deref(),
            Some("due value 'someday' is not a date (use YYYY-MM-DD)")
        );
        assert_eq!(
            state_by(&vs, "blocking").message.as_deref(),
            Some("blocking value 'maybe' is not yes or no")
        );
        // Good values of the same types produce no flag.
        let ok = validate_declared_field_values(
            &present(&[
                ("cost", &["42"]),
                ("due", &["2026-01-01"]),
                ("blocking", &["yes"]),
            ]),
            &s,
        );
        assert!(!has_invalid(&ok), "valid values do not flag");
    }

    #[test]
    fn declared_text_is_always_valid() {
        let s = field_schema(vec![custom("note", "text", &[])]);
        let vs = validate_declared_field_values(&present(&[("note", &["anything at all"])]), &s);
        assert_eq!(state_by(&vs, "note").state, FieldState::Valid);
    }

    #[test]
    fn declared_empty_value_is_absent_not_invalid() {
        // AC-5: missing ≠ invalid — an empty present value never flags.
        let s = field_schema(vec![custom("risk", "choice", &["low"])]);
        let vs = validate_declared_field_values(&present(&[("risk", &[""])]), &s);
        assert_eq!(state_by(&vs, "risk").state, FieldState::Absent);
        assert!(!has_invalid(&vs));
    }

    #[test]
    fn declared_official_field_is_never_validated_here() {
        // AC-6: an official baseline field in the schema is NOT checked by this path
        // (the built-in `validate_fields` owns it) — `custom_field` skips officials.
        let mut fields = vec![custom("risk", "choice", &["low"])];
        fields.push(FieldDef {
            name: "status".to_string(),
            r#type: None,
            allowed_values: Vec::new(),
            required: false,
            official: true,
        });
        let s = field_schema(fields);
        let vs = validate_declared_field_values(
            &present(&[("status", &["weird"]), ("risk", &["low"])]),
            &s,
        );
        assert!(vs.iter().all(|v| v.field != "status"), "official skipped");
        assert_eq!(vs.len(), 1, "only the custom field is validated");
    }

    #[test]
    fn declared_lookup_canonicalizes_key_case_and_dashes() {
        // A doc key `Risk_Tier` / `risk-tier` resolves the declared `risk_tier`.
        let s = field_schema(vec![custom("risk_tier", "choice", &["low"])]);
        let vs = validate_declared_field_values(&present(&[("Risk_Tier", &["nope"])]), &s);
        assert_eq!(state_by(&vs, "risk_tier").state, FieldState::Invalid);
    }

    #[test]
    fn declared_repeated_key_first_wins() {
        let s = field_schema(vec![custom("risk", "choice", &["low"])]);
        let vs =
            validate_declared_field_values(&present(&[("risk", &["bad"]), ("risk", &["low"])]), &s);
        assert_eq!(vs.len(), 1, "one entry per field");
        assert_eq!(state_by(&vs, "risk").state, FieldState::Invalid);
    }

    #[test]
    fn declared_unmatched_key_produces_no_entry() {
        let s = field_schema(vec![custom("risk", "choice", &["low"])]);
        let vs = validate_declared_field_values(&present(&[("arbitrary", &["x"])]), &s);
        assert!(vs.is_empty(), "an undeclared open key stays unopinionated");
    }
}
