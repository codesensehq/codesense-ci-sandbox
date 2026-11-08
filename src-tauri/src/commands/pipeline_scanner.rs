// SPDX-License-Identifier: MIT
//! Pipeline directory scanner and related path helpers (story 30.7).
//!
//! Centralized normalization for `pipelineRoot` and scope path values.
//! These are the single source of truth on the Rust side — every Rust
//! consumer (scanner, find_nearest_valid_scope, future 30.8 MCP tools)
//! must call the helpers rather than re-implementing the rules.
//!
//! Rules (identical to the TS counterparts):
//!   - Trim whitespace
//!   - Strip leading and trailing slashes (both `/` and `\`)
//!   - Reject any value containing `..` component
//!   - Empty string is a valid output
//!
//! keep rules in sync with normalizePipelineRoot / normalizeScopePath
//! in src/shared/lib/pipeline-graph/normalize.ts

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use log::{debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::files::{is_derived_render_artifact, validate_context_path_owned};
use crate::commands::pipeline_format;
use crate::content_engine::atomic_io;
use tauri::Manager;

mod inherited;

// ----- Normalization ---------------------------------------------------------

/// Normalize a value used for `pipelineRoot` or a scope path.
/// Private helper — callers should use `normalize_pipeline_root` or
/// `normalize_scope_path` for clearer error messages.
fn normalize_inner(value: &str, field_name: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let stripped = trimmed.trim_matches(|c: char| c == '/' || c == '\\');
    if stripped.is_empty() {
        return Ok(String::new());
    }
    for part in stripped.split(['/', '\\']) {
        if part == ".." {
            return Err(format!(
                "{} must not contain '..' — got: {}",
                field_name, value
            ));
        }
    }
    Ok(stripped.to_string())
}

pub fn normalize_pipeline_root(value: &str) -> Result<String, String> {
    normalize_inner(value, "pipelineRoot")
}

pub fn normalize_scope_path(value: &str) -> Result<String, String> {
    normalize_inner(value, "scopePath")
}

// ----- Types -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpicStep {
    pub story_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpicParallelGroup {
    pub parallel: Vec<EpicStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpicSubEpicStep {
    pub sub_epic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EpicStepInput {
    Story(EpicStep),
    Parallel(EpicParallelGroup),
    SubEpic(EpicSubEpicStep),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpicPipelineSpec {
    pub version: u32,
    pub steps: Vec<EpicStepInput>,
}

/// R-14: `title` and `story_text` were removed from this struct.
/// They're body-extracted (H1 line + `## Story` section), not frontmatter,
/// and consumers fetch them on-demand via the `get_story_details` Tauri
/// command. The scanner now does zero body-extraction work — its output
/// is pure frontmatter-derived metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoryRef {
    pub file_name: String,
    pub file_path: String,
    /// `depends_on` array from the story's metadata (YAML frontmatter for
    /// `.md`, `<meta name="cs:depends-on">` for `.html`/`.htm`). `None` for:
    /// no metadata, field absent, malformed, or wrong shape.
    pub depends_on: Option<Vec<String>>,
    /// SW-AU-77F: curated built-in `tags` list (YAML `tags:` for `.md`,
    /// `<meta name="cs:tags">` for HTML). `None` when absent. Captured into the
    /// subject snapshot's `declared` bag so `tags contains X` filters resolve
    /// format-neutrally. Unlike `depends_on`, it drives no relation walk.
    pub tags: Option<Vec<String>>,
    /// Raw `status` string from metadata. Validated against `PipelineStatus`
    /// on the TS side; Rust passes through as-is.
    pub status: Option<String>,
    /// Raw `type` string from metadata (e.g. `"bug"`, `"spike"`, `"doc"`,
    /// `"retro"`). `None` when absent — interpreted as `"story"` on the TS
    /// side. Rust passes through as-is.
    pub document_type: Option<String>,
    /// Relative paths to supporting documents from metadata
    /// (e.g. `["./bug-report.md"]`). `None` when absent. Resolution and
    /// filtering happen in the TS transform layer.
    pub supporting_docs: Option<Vec<String>>,
    /// Authoritative display label from `story_id` metadata field.
    pub story_id: Option<String>,
    /// Raw `source_repo` slug from metadata (e.g. `"alderco/codesense"`).
    /// `None` when absent. Rust passes through as-is.
    pub source_repo: Option<String>,
    /// Team name from metadata (e.g. `"default"`). `None` when absent.
    pub team: Option<String>,
    /// SW-SB-1: sprint tag (e.g. `"2026-08-A"`). `None` when absent. The board's
    /// sprint filter (SB-3) reads it here — DocSummary does not reach the board.
    pub sprint: Option<String>,
    /// SW-SB-1: story estimate in points. `None` when absent/malformed. Raw —
    /// the off-Fibonacci lint lives in `validate_story` (SB-9), not the scanner.
    pub points: Option<f64>,
    /// SW-SB-1: assignee username (e.g. `"octocat-dev"`). `None` when absent.
    /// Feeds SB-12 `getReadyUnassigned` + SE-7 assignee avatars.
    pub assigned_to: Option<String>,
    /// SW-SB-22: opaque board-ordering rank (string fractional index). `None` when
    /// absent — such cards sort after ranked ones. The board's per-column sort
    /// (`sortColumnStories`) reads it here; compared lexically, never parsed.
    pub rank: Option<String>,
    /// SLW-MD-1: `false` when this doc's metadata is present-invalid (a
    /// constrained field like `source_repo`/`type`/`team` has a bad value) OR
    /// unparseable. `true` for absent/warning/unknown-only (absent = valid).
    /// Drives the nav's amber invalid-metadata badge (AC-11).
    #[serde(default = "default_metadata_valid")]
    pub metadata_valid: bool,
    /// SLW-MD-1: the first offending field's reason (badge tooltip/aria-label).
    /// `None` when `metadata_valid`.
    #[serde(default)]
    pub metadata_invalid_reason: Option<String>,
    /// SW-DX-2 (AC-13): open/arbitrary frontmatter tag values — every
    /// frontmatter key NOT in the hardcoded set above, mapped to its structured
    /// value list (a scalar → 1-item vec, a YAML list → N items). Includes
    /// SW-DX-1 typed-schema tags (e.g. `context_level`). Populated by the
    /// scanner from the SAME single parse that fills the named fields (reuses
    /// the `frontmatter_editor::open_tag_values` extractor), so the query layer
    /// filters/returns tags with zero per-doc filesystem I/O. `#[serde(default)]`
    /// (an empty object) on older payloads / non-story docs. `BTreeMap` for
    /// deterministic key order (serializes as the TS `openTags` object).
    #[serde(default)]
    pub open_tags: std::collections::BTreeMap<String, Vec<String>>,
    /// SW-DX-9 (query v2): the doc file's mtime in epoch **nanoseconds** — the
    /// cached `CacheEntry.mtime` on the warm arm, an on-scan `mtime_of` stat on
    /// the inline arm. `None` when unavailable (a stat failure, or a synthetic
    /// fixture `StoryRef`). Surfaced onto `DocSummary` for the Freshness
    /// dashboard. `skip_serializing_if` keeps it off older payloads and the
    /// nav's wire shape when absent; `#[serde(default)]` deserializes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

/// Serde default for `metadata_valid` — absent = valid (MD-1's cardinal rule).
fn default_metadata_valid() -> bool {
    true
}

/// Metadata extracted from the epic's main doc (e.g. README.md, README.html).
/// Placed on `DerivedPipelineTree.main_doc` — never in `stories[]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MainDoc {
    pub file_path: String,
    #[serde(default)]
    pub title_meta: Option<String>,
    #[serde(default)]
    pub story_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sprint: Option<String>,
    #[serde(default = "default_metadata_valid")]
    pub metadata_valid: bool,
    #[serde(default)]
    pub metadata_invalid_reason: Option<String>,
    pub supporting_docs: Option<Vec<String>>,
    pub purpose_text: Option<String>,
    /// SW-AU-77K: an author-declared automation `subject_id` on the epic main doc,
    /// mirroring [`DocRef::subject_id`]. Read from the frontmatter the summary parse
    /// already extracts (no extra read). Absent ⇒ the main doc is not an addressable
    /// subject; its directory is still an `epic`-kind subject independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
}

/// PTS-2 follow-up: companion to `StoryRef` for markdown/HTML files that
/// pass `is_pipeline_document` (extension + non-readme/index name check)
/// but fail the pipeline-type predicate — e.g. a `prd.md` with no
/// frontmatter, a `type: doc` story, a `type: llm-distillate` planning
/// artifact. These appear in the nav as plain file rows alongside
/// pipeline cards; they do NOT appear in the pipeline view, do NOT have
/// status dropdowns, and do NOT carry frontmatter metadata at this
/// level (consumers that need it should re-read via `get_story_details`
/// or the cache directly).
///
/// Shape is deliberately minimal so adding fields later is a
/// non-breaking change for TS consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DocRef {
    pub file_name: String,
    pub file_path: String,
    /// SLW-MD-1: same as `StoryRef.metadata_valid` — a *plain* doc can be
    /// badge-worthy too (AC-16: e.g. an invalid `source_repo` on a `type: doc`).
    /// Without this the nav badge would silently never fire for plain docs.
    #[serde(default = "default_metadata_valid")]
    pub metadata_valid: bool,
    #[serde(default)]
    pub metadata_invalid_reason: Option<String>,
    /// SW-AU-77H: an author-declared automation `subject_id` (frontmatter, or the
    /// HTML `<meta name="cs:subject-id">` twin) that makes this non-story doc a
    /// durable, move-safe automation subject. Absent ⇒ not an addressable subject.
    /// Read from the same typed frontmatter parse the routing pass already performs
    /// (no extra file read), so a doc still does no full open-tags work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// SW-XS-20: the parent team (in the scanned team's `parent_team` chain)
    /// this doc is inherited from. Set only on a file the scanned team does
    /// not have itself; `None` for own-team files. `file_path` is then the
    /// parent's real file, so edits from the child team's view update it.
    /// Drives the nav's inherited marker (`DocRef.sourceTeam` on the frontend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_team: Option<String>,
    /// SW-XS-20: on an own-team doc only — the nearest parent team that has a
    /// file at the same path, which this doc overrides for the team. Deleting
    /// it brings that parent's version back into view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_team: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerivedPipelineTree {
    pub dir_path: String,
    pub dir_name: String,
    pub absolute_dir_path: String,
    pub stories: Vec<StoryRef>,
    /// PTS-2 follow-up: markdown/HTML files in this dir that did NOT
    /// pass the pipeline-type predicate. Sibling to `stories`; the
    /// scanner is the single source of truth for the split. UI renders
    /// these as plain file rows.
    #[serde(default)]
    pub docs: Vec<DocRef>,
    pub sub_epics: Vec<DerivedPipelineTree>,
    pub order: Option<EpicPipelineSpec>,
    pub order_error: Option<String>,
    pub main_doc: Option<MainDoc>,
}

// Natural sort comparator lives in `codesense_fs_core::natural_sort` so the file
// tree (the fs-core listing behind `list_directory`) and the scanner can never disagree on
// sibling order. See that module for the full implementation.
use codesense_fs_core::natural_sort::natural_cmp;

use crate::commands::sprint_run::run_store;

// ----- Scanner core (no AppHandle; callable from tests) ----------------------

/// Bytes the scanner reads from the head of a pipeline file when extracting
/// frontmatter. SW-MCP-10: `pub(crate)` so `validate_story` parses the SAME
/// window the cache does — a story whose frontmatter runs past this cap is
/// dropped by the index, and the validator must agree (AC-11 parity).
pub(crate) const STORY_META_READ_CAP: usize = 4096;
/// Maximum characters returned for the configured summary section on main docs.
const PURPOSE_TEXT_CAP: usize = 500;

/// SW-EW-32: default summary-section headings, tried in order (first match
/// wins). Mirrors the FE `DEFAULT_SETTINGS.summaryHeadings`. A story doc (no
/// `## Purpose`) falls through to `## Story`; an epic doc hits `## Purpose`
/// first — the single list covers both kinds.
const DEFAULT_SUMMARY_HEADINGS: &[&str] = &["Purpose", "Story"];

/// SW-EW-32: the default list as an owned `Vec`. Used by the cache-miss scan
/// path (where the body's `story_text` is not display-bound) and as the
/// fallback inside [`effective_headings`].
/// SW-MCP-11: `pub(crate)` so the MCP authoring reader shares the one default.
pub(crate) fn default_summary_headings() -> Vec<String> {
    DEFAULT_SUMMARY_HEADINGS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// SW-EW-32: resolve a caller-supplied heading list into an effective one.
/// Trims each entry and drops blanks; if nothing usable remains, falls back to
/// [`DEFAULT_SUMMARY_HEADINGS`]. This is the backend safety net — a malformed
/// or empty FE payload can never blank every doc's summary.
/// SW-MCP-11: `pub(crate)` so the MCP authoring tools resolve headings through
/// the SAME matcher the scanner uses — guidance can never drift from render.
pub(crate) fn effective_headings(headings: Vec<String>) -> Vec<String> {
    let cleaned: Vec<String> = headings
        .into_iter()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    if cleaned.is_empty() {
        default_summary_headings()
    } else {
        cleaned
    }
}

/// R-14: Single builder for `StoryRef` from a `FrontmatterFields` snapshot.
/// Both the cache-hit path (entry.fields) and the inline-parse fallback path
/// (StoryMeta → fields via `story_meta_to_fields`) go through this helper, so
/// the frontmatter→StoryRef contract has one source of truth.
/// `title` and `story_text` are intentionally dropped — they're fetched
/// on-demand via `get_story_details`.
///
/// SW-DX-2 (AC-13): `open_tags` carries the doc's open/arbitrary frontmatter
/// tags (extracted once by the caller via
/// `frontmatter_editor::open_tag_values`) into the `StoryRef` so the query
/// layer can filter/return them with no per-doc re-read. The cache-hit arm
/// sources it from the cached `CacheEntry.open_tags`; the inline arm computes it
/// from the raw chunk it already read.
fn build_story_ref(
    name: String,
    path: &Path,
    fields: &crate::commands::pipeline_format::FrontmatterFields,
    meta_cfg: &MetadataValidityConfig,
    parse_error: Option<&str>,
    open_tags: std::collections::BTreeMap<String, Vec<String>>,
    mtime: Option<u64>,
) -> StoryRef {
    // RF-perf-9 (AC-1): always-on frequency tally (one bump per story per scan). No
    // per-call gated emit here — this fires N times per scan; a per-call log would
    // flood, so per-call cost is read from the `#[ignore]`d benchmark instead, while
    // this counter supplies the live "× frequency" half. (Reuses the RF-perf-0 spine.)
    crate::install::frontmatter_perf::note_build_story_ref();
    let (metadata_valid, metadata_invalid_reason) =
        compute_metadata_validity(fields, meta_cfg, parse_error);
    StoryRef {
        file_name: name,
        file_path: path.display().to_string(),
        depends_on: fields.depends_on.clone(),
        tags: fields.tags.clone(),
        status: fields.status.clone(),
        document_type: fields.document_type.clone(),
        supporting_docs: fields.supporting_docs.clone(),
        story_id: fields.story_id.clone(),
        source_repo: fields.source_repo.clone(),
        team: fields.team.clone(),
        // SW-SB-1: board metadata carried straight off the FrontmatterFields
        // snapshot — one copy site covers both the cache-hit and inline arms.
        sprint: fields.sprint.clone(),
        points: fields.points,
        assigned_to: fields.assigned_to.clone(),
        rank: fields.rank.clone(),
        metadata_valid,
        metadata_invalid_reason,
        open_tags,
        mtime,
    }
}

/// SW-DX-2 (AC-13): convert the `open_tag_values` extractor's ordered
/// `Vec<(key, values)>` into the `BTreeMap` the `StoryRef.open_tags` field
/// carries. On the rare duplicate open key, last-write-wins (matches the TS
/// object-build). Shared by the scanner's inline path and the frontmatter cache
/// so there is one conversion.
pub(crate) fn open_tags_map(
    pairs: Vec<(String, Vec<String>)>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    pairs.into_iter().collect()
}

/// SLW-MD-1 (Task 4): the configured repo set resolved **once per scan** from
/// the already-loaded org config (no per-file lockfile walk — locked decision
/// #5). Threaded into the walk; `build_story_ref` / the doc builder validate
/// each doc's constrained fields against it via the shared predicate.
#[derive(Default)]
pub struct MetadataValidityConfig {
    /// The active team's **configured** `owner/repo` slugs (the validity bar).
    /// Empty ⇒ "unknown" ⇒ source_repo membership badging disabled (syntactic
    /// check only; a malformed slug is still flagged).
    pub configured_repos: Vec<String>,
    /// Configured team names. Empty ⇒ "unknown" ⇒ team never badges.
    pub valid_teams: Vec<String>,
}

/// Compute `(metadata_valid, first_invalid_reason)` for one doc's fields. Uses
/// the SHARED per-field validator (`frontmatter_validation`), so the nav badge
/// and the MCP `validate_story` verdict can never disagree. Present-invalid ⇒
/// `false`; absent/warning/unknown ⇒ `true`.
///
/// A structural parse failure or repaired contract value carries a diagnostic.
/// Custom scalar repairs remain valid, while all recovered fields still receive
/// the usual value checks.
fn compute_metadata_validity(
    fields: &crate::commands::pipeline_format::FrontmatterFields,
    cfg: &MetadataValidityConfig,
    parse_error: Option<&str>,
) -> (bool, Option<String>) {
    use crate::commands::frontmatter_validation as fv;
    if let Some(reason) = parse_error {
        return (false, Some(reason.to_string()));
    }
    let vc = fv::ValidatorConfig {
        configured_repos: &cfg.configured_repos,
        valid_teams: &cfg.valid_teams,
    };
    let validities = fv::validate_fields(fields, &vc);
    if fv::has_invalid(&validities) {
        (false, fv::first_invalid_reason(&validities))
    } else {
        (true, None)
    }
}

/// First-4KB metadata extracted from a story `.md` file in a single read.
/// Both fields are independently `Option`-typed — title and frontmatter can
/// each be absent or malformed without affecting the other.
///
/// R-14: `title` and `story_text` are populated for the upcoming
/// `get_story_details` Tauri command (Task 5). They're not consumed by
/// the scanner's `StoryRef` output anymore (body-extracted fields are
/// fetched on-demand, not eagerly).
#[derive(Default)]
pub(crate) struct StoryMeta {
    pub metadata_parse_error: Option<String>,
    #[allow(dead_code)] // Consumed by get_story_details (Task 5) and tests.
    pub title: Option<String>,
    pub depends_on: Option<Vec<String>>,
    /// SW-AU-77F: `tags` list, threaded so the cache-miss projection
    /// (`story_meta_to_fields`) reaches `StoryRef.tags` losslessly.
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
    #[allow(dead_code)] // Consumed by get_story_details (Task 5) and tests.
    pub story_text: Option<String>,
    pub document_type: Option<String>,
    pub supporting_docs: Option<Vec<String>>,
    pub story_id: Option<String>,
    pub source_repo: Option<String>,
    /// SW-GS-35: story-scoped dev branch, threaded through so it reaches the
    /// frontmatter cache (`CacheEntry::dev_branch()`) — the viewer's serde-direct
    /// path is separate and already covered by the `FrontmatterFields` field.
    pub dev_branch: Option<String>,
    /// SW-WB-1: story-scoped INTENDED branch, threaded through so the
    /// StoryMeta → FrontmatterFields projection stays lossless (the cache view
    /// carries it too). The viewer's serde-direct path reads it via
    /// `FrontmatterFields::branch_name` independently.
    pub branch_name: Option<String>,
    /// SW-OR-1b: the story's single canonical branch. The `branch` key already
    /// exists as the epic integration branch (SW-EW-5); on a STORY doc it is the
    /// unified branch that supersedes `branch_name`/`dev_branch`. Threaded here so
    /// it reaches the cache (`CacheEntry::story_branch()` precedence). A story doc
    /// and an epic doc never share a `StoryMeta`, so no cross-scope confusion.
    pub branch: Option<String>,
    pub team: Option<String>,
    /// SW-SB-1: board metadata (see `FrontmatterFields`), threaded through so it
    /// survives the `StoryMeta → story_meta_to_fields` projection and reaches the
    /// board (the seam SB-9 left is closed in `story_meta_to_fields`).
    pub sprint: Option<String>,
    pub points: Option<f64>,
    pub assigned_to: Option<String>,
    /// SW-SB-22: board-ordering rank — threaded through the same way as `sprint`
    /// so it survives the `StoryMeta → story_meta_to_fields` cache projection.
    pub rank: Option<String>,
    /// SW-AU-77H: a standalone (non-story) document's author-declared automation
    /// `subject_id`. Threaded through `story_meta_to_fields` so it reaches
    /// `FrontmatterFields` and, from there, `DocRef.subject_id` on the docs arm.
    pub subject_id: Option<String>,
}

/// R-14: Body-extracted view (H1 title + `## Story` section). Returned by
/// `extract_body` and consumed by both `extract_story_meta` (fallback path)
/// and the `get_story_details` Tauri command (on-demand fetch). Single
/// source of truth for body parsing — see AC #27 in the R-14 story for
/// the DRY guardrail.
#[derive(Default, Debug, Clone)]
pub(crate) struct StoryBody {
    pub title: Option<String>,
    /// SW-LN-34: frontmatter `title:` / HTML `cs:title`, from the same head.
    pub title_meta: Option<String>,
    pub story_text: Option<String>,
}

/// Single-source helper for "read up to STORY_META_READ_CAP bytes from
/// `path` and detect its pipeline format from the extension." Both
/// `extract_body` and `extract_story_meta` go through this so file I/O +
/// format detection have one home — AC #27 DRY guardrail.
fn read_first_chunk(path: &Path) -> Option<(String, pipeline_format::PipelineFileFormat)> {
    let mut buf = [0u8; STORY_META_READ_CAP];
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!(target: "pipeline_scanner", "read_first_chunk open failed {}: {}", path.display(), e);
            return None;
        }
    };
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            warn!(target: "pipeline_scanner", "read_first_chunk read failed {}: {}", path.display(), e);
            return None;
        }
    };
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    let format = pipeline_format::format_of(path);
    Some((text, format))
}

/// Single-source helper for body extraction from already-read text. Both
/// `extract_body` (file path entry) and `extract_story_meta` (which also
/// parses frontmatter from the same chunk) go through this so there is
/// exactly one place that maps text → `StoryBody`.
fn compute_body_from_text(
    format: pipeline_format::PipelineFileFormat,
    text: &str,
    headings: &[String],
) -> StoryBody {
    StoryBody {
        title: pipeline_format::extract_title(format, text),
        title_meta: pipeline_format::extract_meta_title(format, text),
        // SW-EW-32: keys off the configured ordered heading list (first match
        // wins) instead of a hardcoded `## Story`.
        story_text: pipeline_format::extract_first_section(format, text, headings, None),
    }
}

/// R-14: Read the first 4KB of `path` and extract title + story_text via
/// the format-dispatch helpers. Used by both `extract_story_meta` (the
/// scanner's cache-miss path, indirectly via `compute_body_from_text`)
/// and `get_story_details` (the on-demand fetch). Returns an empty body
/// on any I/O error — same conservative posture as `extract_story_meta`'s
/// pre-R-14 behaviour.
pub(crate) fn extract_body(path: &Path, headings: &[String]) -> StoryBody {
    let Some((text, format)) = read_first_chunk(path) else {
        return StoryBody::default();
    };
    compute_body_from_text(format, &text, headings)
}

/// Single-read extraction of story-level metadata. Shares the file-I/O +
/// format-detect scaffold with `extract_body` via `read_first_chunk`, and
/// the body extraction itself via `compute_body_from_text` — so AC #27's
/// DRY guardrail ("no parallel body-scanner implementations") holds.
///
/// Returns `None` for HTML/HTM files that contain zero `cs:*` meta tags,
/// signalling to `walk_dir` that the file is not a pipeline document.
/// Markdown files always return `Some` (AC-3).
///
/// R-14: With the cache wire-up landed (Task 4), this function is now the
/// **cache-miss fallback path** — production scans hit the cache. Kept for
/// cold-start and post-upgrade transient correctness.
/// Round-2 R2: trim + reject empty for `source_repo` / `team`. The
/// frontmatter parser yields `Some("")` for explicit empty strings and
/// `Some("  ")` for whitespace-only values; downstream consumers treat
/// presence as truthy, so without sanitisation a blank field surfaces
/// as a populated value.
fn sanitize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

/// Round-2 R2: trim + reject empty + reject YAML null sentinels for
/// `story_id`. Matches the TS-side parser at `worktree/resolve.ts` so
/// `story_id: null`, `story_id: ~`, `story_id: ""`, and
/// `story_id: "   "` all collapse to `None` regardless of which side
/// (TS pre-parse or Rust scanner) processed the file first.
fn sanitize_story_id(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let t = s.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("null") || t == "~" {
            None
        } else {
            Some(t.to_string())
        }
    })
}

/// Parse a pipeline candidate's frontmatter into [`StoryMeta`].
///
/// # Returns
///
/// `None` has **exactly one** meaning — the F.30 gate: an `.html`/`.htm` file
/// with zero `cs:*` meta tags. Every other input returns `Some`, including a
/// frontmatter-less `.md` (`Some(StoryMeta::default())`) and an unreadable file
/// (also `Some(default)` — see the `read_first_chunk` `None` branch below).
///
/// Callers rely on this single-meaning contract:
/// - `walk_dir`'s classify loop (SW-FX-39) treats `None` as "route to `docs[]`
///   with default fields" — a cs-less HTML is a supporting doc, not a story.
/// - `build_cache_entry` (seam 2, `frontmatter_cache.rs`) uses `?` to skip
///   cs-less HTML from the source-repo story cache.
///
/// If you ever add a *new* `None` return (e.g. a genuine parse-failure that
/// should be skipped or surfaced), update BOTH callers — otherwise `walk_dir`
/// will silently route the failure into `docs[]` as an empty-field entry.
pub(crate) fn extract_story_meta(path: &Path) -> Option<StoryMeta> {
    extract_story_meta_and_parse_ok(path).0
}

/// SW-DX-2 (AC-13): the doc's open/arbitrary frontmatter tags, extracted from
/// the first chunk via the SAME `frontmatter_editor::open_tag_values` walker the
/// SW-DX-1 schema-tag validity uses (no second key-walker). Returns `{}` for an
/// unreadable file. Used by the scanner's cache-miss inline path (the cache-hit
/// path reads `CacheEntry.open_tags` instead, so this stays off the warm path).
pub(crate) fn extract_open_tags(path: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let Some((text, format)) = read_first_chunk(path) else {
        return std::collections::BTreeMap::new();
    };
    open_tags_map(crate::commands::frontmatter_editor::open_tag_values(
        format, &text,
    ))
}

/// FIX 6 / AC-11: single-read variant returning both the [`StoryMeta`] and
/// whether the metadata block parses (`block_parse_ok`). The `walk_dir` inline
/// path uses this so detecting a present-but-unparseable block costs NO extra
/// file read — the block-parse verdict is derived from the SAME 4KB chunk the
/// meta parse already read. `extract_story_meta` delegates here and drops the
/// bool, preserving its `None`-only-for-cs-less-HTML contract.
///
/// The bool is `true` (not malformed) whenever the file is unreadable or has no
/// block — only a present-but-unparseable block yields `false`.
pub(crate) fn extract_story_meta_and_parse_ok(path: &Path) -> (Option<StoryMeta>, bool) {
    let Some((text, format)) = read_first_chunk(path) else {
        return (Some(StoryMeta::default()), true);
    };
    meta_and_parse_ok_from_chunk(format, &text)
}

/// Shared frontmatter extraction from an already-read story or main-doc chunk.
fn fields_from_chunk(
    format: pipeline_format::PipelineFileFormat,
    text: &str,
) -> (pipeline_format::FrontmatterFields, bool, Option<String>) {
    match format {
        pipeline_format::PipelineFileFormat::Markdown => {
            let parsed = pipeline_format::markdown::read_frontmatter(text);
            let error = crate::commands::frontmatter_validation::metadata_parse_error(&parsed);
            (parsed.fields, parsed.block_parse_ok, error)
        }
        pipeline_format::PipelineFileFormat::Html => {
            (pipeline_format::html::extract_meta_fields(text), true, None)
        }
    }
}

fn meta_and_parse_ok_from_chunk(
    format: pipeline_format::PipelineFileFormat,
    text: &str,
) -> (Option<StoryMeta>, bool) {
    let (fm, block_parse_ok, metadata_parse_error) = fields_from_chunk(format, text);

    // F.30 gate: HTML/HTM files without any cs:* meta tags are not pipeline
    // documents. Skip them so ghost stories don't pollute the nav/graph.
    if format == pipeline_format::PipelineFileFormat::Html && fm.is_empty() {
        return (None, block_parse_ok);
    }

    // SW-EW-32: this is the cache-miss scan path — `StoryMeta.story_text` is not
    // display-bound (the scanner drops it from `StoryRef`; the pipeline card +
    // HUD fetch on-demand via `get_story_details`). Use the default headings so
    // back-compat fixtures resolve as before; the on-demand path is what honors
    // the user's configured list.
    let body = compute_body_from_text(format, text, &default_summary_headings());

    let meta = StoryMeta {
        metadata_parse_error,
        title: body.title,
        depends_on: fm.depends_on,
        tags: fm.tags,
        status: fm.status,
        story_text: body.story_text,
        document_type: fm.document_type,
        supporting_docs: fm.supporting_docs,
        // R-18 code-review P2 + round-2 R2: symmetric with TS parser —
        // trim whitespace, reject empty, reject YAML null literals
        // (`null` / `Null` / `NULL` / `~`). Without this the Rust scanner
        // would emit `Some("null")` for `story_id: null`, which would
        // render literally in workspace tabs. `source_repo` and `team`
        // get the same trim+empty treatment for consistency.
        story_id: sanitize_story_id(fm.story_id),
        source_repo: sanitize_optional_string(fm.source_repo),
        // SW-GS-35: same trim/empty/null-literal sanitize as source_repo/team.
        dev_branch: sanitize_optional_string(fm.dev_branch),
        // SW-WB-1: same sanitize as dev_branch — the intended branch field.
        branch_name: sanitize_optional_string(fm.branch_name),
        // SW-OR-1b: the unified `branch` field, same sanitize. For an epic doc
        // this is its integration branch; for a story it is the canonical branch.
        branch: sanitize_optional_string(fm.branch),
        team: sanitize_optional_string(fm.team),
        // SW-SB-1: board metadata. Strings get the same trim/empty/null-literal
        // sanitize as source_repo/team; `points` is already lenient-parsed.
        sprint: sanitize_optional_string(fm.sprint),
        points: fm.points,
        assigned_to: sanitize_optional_string(fm.assigned_to),
        // SW-SB-22: opaque ordering rank — same string sanitize as sprint.
        rank: sanitize_optional_string(fm.rank),
        // SW-AU-77H: standalone-doc subject marker — same trim/empty/null sanitize.
        subject_id: sanitize_optional_string(fm.subject_id),
    };
    (Some(meta), block_parse_ok)
}

/// RF-perf-11 (AC-4): single-read extractor for the `walk_dir` cache-miss arm.
/// Returns the [`StoryMeta`] + `block_parse_ok` AND hands back the raw chunk, so
/// the caller can derive open frontmatter tags from the SAME read — but only for a
/// pipeline story, exactly as before. The cold-scan `read_parse` phase (the ranked
/// #1 cost, 91% of a 62 ms N=1000 cold scan) previously opened + read the same
/// 4 KB chunk TWICE (`extract_story_meta_and_parse_ok` then a second
/// `extract_open_tags`); now a story reads once and a doc does no open-tags work —
/// strictly fewer syscalls, byte-identical output. The chunk is `None` only when
/// the file is unreadable (open tags then default to empty, as they did before).
fn read_meta_parse_ok_and_chunk(
    path: &Path,
) -> (
    Option<StoryMeta>,
    bool,
    Option<(String, pipeline_format::PipelineFileFormat)>,
) {
    let Some((text, format)) = read_first_chunk(path) else {
        return (Some(StoryMeta::default()), true, None);
    };
    let (meta, block_parse_ok) = meta_and_parse_ok_from_chunk(format, &text);
    (meta, block_parse_ok, Some((text, format)))
}

/// Check if a filename has a recognized pipeline extension.
fn has_pipeline_extension(name: &str) -> bool {
    name.to_ascii_lowercase()
        .rsplit_once('.')
        .is_some_and(|(_, ext)| pipeline_format::PIPELINE_EXTENSIONS.contains(&ext))
}

fn is_pipeline_document(name: &str) -> bool {
    if !has_pipeline_extension(name) {
        return false;
    }
    let stem = name
        .to_ascii_lowercase()
        .rsplit_once('.')
        .map(|(s, _)| s.to_string())
        .unwrap_or_default();
    stem != "readme" && stem != "index"
}

/// Non-pipeline file types that are still first-class **supporting docs** —
/// they carry no pipeline frontmatter (so they never route to `stories[]`),
/// but they belong in the file tree alongside the stories they support.
/// SW-SC-1: `.excalidraw` canvases. SW-SC-7: images. They land in `docs[]`.
/// Keep in sync with VIEWER_EXTENSIONS in src/shared/lib/viewer-file-type.ts.
const SUPPORTING_DOC_EXTENSIONS: &[&str] =
    &["excalidraw", "png", "jpg", "jpeg", "gif", "webp", "svg"];

fn is_supporting_doc(name: &str) -> bool {
    // SW-SC-7: a derived `*.excalidraw.png` render is an agent-only artifact,
    // never a user-facing supporting doc — exclude it even though `png` is now
    // a supporting-doc extension.
    if is_derived_render_artifact(name) {
        return false;
    }
    name.to_ascii_lowercase()
        .rsplit_once('.')
        .is_some_and(|(_, ext)| SUPPORTING_DOC_EXTENSIONS.contains(&ext))
}

/// Route a single md/html entry to either `stories` or `docs` based on
/// the `is_pipeline_story` predicate. Centralises the routing call so
/// the cache-hit and cache-miss arms of `walk_dir` cannot diverge in
/// their classification: they both call this with the same `fields`
/// source.
///
/// Visibility intentionally narrow (`fn`, not `pub`) — this is internal
/// to walker. Returns `()` because both branches are terminal pushes.
/// `parse_error`: a diagnostic when metadata requires a badge
/// (FIX 6 / AC-11). The cache-hit path passes `true` (a cached story parsed to
/// get cached; a malformed block yields empty fields → not a pipeline story →
/// always takes the inline path where the raw content IS available). The inline
/// path computes it from the raw chunk it already read.
///
/// SW-DX-2 (AC-13): `open_tags` is the doc's open frontmatter tags, extracted
/// once by the caller. Only the `stories[]` arm carries them (a `docs[]` plain
/// file has no queryable `StoryRef`).
#[allow(clippy::too_many_arguments)]
fn route_file_entry(
    name: String,
    path: &Path,
    fields: &pipeline_format::FrontmatterFields,
    stories: &mut Vec<StoryRef>,
    docs: &mut Vec<DocRef>,
    meta_cfg: &MetadataValidityConfig,
    parse_error: Option<&str>,
    open_tags: std::collections::BTreeMap<String, Vec<String>>,
    mtime: Option<u64>,
) {
    if fields.is_pipeline_story() {
        stories.push(build_story_ref(
            name,
            path,
            fields,
            meta_cfg,
            parse_error,
            open_tags,
            mtime,
        ));
    } else {
        // SLW-MD-1: plain docs can be badge-worthy too (AC-16).
        let (metadata_valid, metadata_invalid_reason) =
            compute_metadata_validity(fields, meta_cfg, parse_error);
        docs.push(DocRef {
            file_name: name,
            file_path: path.display().to_string(),
            metadata_valid,
            metadata_invalid_reason,
            // SW-AU-77H: a standalone doc that declares `subject_id` (or the HTML
            // `cs:subject-id` twin) becomes an addressable automation subject. Read
            // from the same typed parse used to route the file — no extra work.
            subject_id: fields.subject_id.clone(),
            source_team: None,
            overrides_team: None,
        });
    }
}

/// Default fallback chain for main doc filename detection. The context query
/// index derives folder epics from the same names.
pub(crate) const MAIN_DOC_DEFAULTS: &[&str] = &[
    "readme.md",
    "index.md",
    "readme.html",
    "index.html",
    "readme.htm",
    "index.htm",
];

/// Build `MainDoc` from a file path. Single read: extracts frontmatter
/// fields and metadata validity, plus the summary section (capped), from one buffer.
///
/// SW-EW-32: `headings` is the configured ordered list the summary keys off
/// (first match wins); defaults to `["Purpose", "Story"]` at the command
/// boundary so the epic banner resolves `## Purpose` as before.
fn build_main_doc(path: &Path, headings: &[String], meta_cfg: &MetadataValidityConfig) -> MainDoc {
    let mut buf = [0u8; STORY_META_READ_CAP];
    let text = match File::open(path) {
        Ok(mut f) => match f.read(&mut buf) {
            Ok(n) => String::from_utf8_lossy(&buf[..n]).to_string(),
            Err(e) => {
                warn!(target: "pipeline_scanner", "main_doc read failed {}: {}", path.display(), e);
                String::new()
            }
        },
        Err(e) => {
            warn!(target: "pipeline_scanner", "main_doc open failed {}: {}", path.display(), e);
            String::new()
        }
    };
    let format = pipeline_format::format_of(path);
    let (fm, _, parse_error) = fields_from_chunk(format, &text);
    let (metadata_valid, metadata_invalid_reason) =
        compute_metadata_validity(&fm, meta_cfg, parse_error.as_deref());
    let purpose_text =
        pipeline_format::extract_first_section(format, &text, headings, Some(PURPOSE_TEXT_CAP));
    MainDoc {
        file_path: path.display().to_string(),
        metadata_valid,
        metadata_invalid_reason,
        title_meta: pipeline_format::extract_meta_title(format, &text),
        story_id: sanitize_optional_string(fm.story_id),
        status: sanitize_optional_string(fm.status),
        sprint: sanitize_optional_string(fm.sprint),
        supporting_docs: fm.supporting_docs,
        purpose_text,
        subject_id: sanitize_optional_string(fm.subject_id),
    }
}

/// The mainDoc name precedence, single-sourced: configured name first (if any),
/// then [`MAIN_DOC_DEFAULTS`], deduped, all lowercased for case-insensitive
/// matching. Shared by [`detect_main_doc`] (tree walk) and
/// [`resolve_epic_main_doc`] (SW-DX-16 dir-based resolver) so the precedence rule
/// lives in exactly one place.
fn main_doc_priority_names(configured_name: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(cfg) = configured_name {
        names.push(cfg.to_ascii_lowercase());
    }
    for d in MAIN_DOC_DEFAULTS {
        let lower = d.to_ascii_lowercase();
        if !names.contains(&lower) {
            names.push(lower);
        }
    }
    names
}

/// Detect the main doc. Searches `main_doc_candidates` (files rejected by
/// `is_pipeline_document()`, e.g. README/index) and optionally
/// `story_entries` (for configured names like "Epic.md" that pass
/// `is_pipeline_document()`). Case-insensitive, first match wins.
/// Returns `(Option<MainDoc>, Option<usize>)` — the second value is the
/// index into `story_entries` to remove if the main doc was found there.
fn detect_main_doc(
    main_doc_candidates: &[(String, PathBuf)],
    story_entries: &[(String, PathBuf)],
    configured_name: Option<&str>,
    headings: &[String],
    meta_cfg: &MetadataValidityConfig,
) -> (Option<MainDoc>, Option<usize>) {
    for target in &main_doc_priority_names(configured_name) {
        // Check main_doc_candidates first (README/index).
        for (name, path) in main_doc_candidates {
            if name.to_ascii_lowercase() == *target {
                return (Some(build_main_doc(path, headings, meta_cfg)), None);
            }
        }
        // Then check story_entries (for configured names like "Epic.md"
        // that pass is_pipeline_document()).
        for (idx, (name, path)) in story_entries.iter().enumerate() {
            if name.to_ascii_lowercase() == *target {
                return (Some(build_main_doc(path, headings, meta_cfg)), Some(idx));
            }
        }
    }

    (None, None)
}

/// SW-DX-16: resolve an epic directory's mainDoc file path by the scanner's name
/// precedence (configured name first, then [`MAIN_DOC_DEFAULTS`];
/// case-insensitive, first match wins). A dir-based entry point for callers that
/// have NOT walked the tree — the trusted `resolve_epic_path` epic resolver.
/// Shares `MAIN_DOC_DEFAULTS` with [`detect_main_doc`] so the precedence is
/// single-sourced (the FE mirror is `MAIN_DOC_PRIORITY` in
/// dashboard-launch-controller.ts). Returns the absolute mainDoc path, or `None`
/// when the dir has no qualifying doc (the caller honest-degrades). Unlike
/// `detect_main_doc` this reads only the directory listing — no frontmatter/body
/// parse — because the resolver needs the path, not a `MainDoc`.
pub(crate) fn resolve_epic_main_doc(dir: &Path, configured_name: Option<&str>) -> Option<PathBuf> {
    // Index the dir's files by lowercased name. On a same-name case collision
    // (`README.md` + `readme.md`, only possible on a case-sensitive FS) the first
    // `read_dir` entry wins — read_dir-order-dependent, but such a collision is
    // immaterial to WHICH epic opens, and matches the FE own-epic resolver.
    let mut by_lower: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            by_lower.entry(name.to_ascii_lowercase()).or_insert(path);
        }
    }
    main_doc_priority_names(configured_name)
        .iter()
        .find_map(|target| by_lower.get(target).cloned())
}

/// Parse and validate `.meta/pipeline.json` content. On any error returns
/// `Err(order_error_message)` matching AC-14.
fn parse_pipeline_json(
    content: &str,
    stories: &[StoryRef],
    sub_epics: &[DerivedPipelineTree],
) -> Result<EpicPipelineSpec, String> {
    let value: Value = serde_json::from_str(content).map_err(|e| format!("Invalid JSON: {}", e))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "Invalid JSON: root must be an object".to_string())?;

    let version = obj
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "Invalid JSON: missing or invalid 'version' field".to_string())?;
    if version != 1 {
        return Err(format!("Unsupported pipeline.json version: {}", version));
    }

    let steps_val = obj
        .get("steps")
        .ok_or_else(|| "Invalid JSON: missing 'steps' field".to_string())?;
    let steps_arr = steps_val
        .as_array()
        .ok_or_else(|| "Invalid JSON: 'steps' must be an array".to_string())?;

    let mut steps: Vec<EpicStepInput> = Vec::with_capacity(steps_arr.len());
    for (i, step) in steps_arr.iter().enumerate() {
        let step_obj = step
            .as_object()
            .ok_or_else(|| format!("Invalid step at index {}: not an object", i))?;
        let parsed = if let Some(sf) = step_obj.get("storyFile") {
            let name = sf
                .as_str()
                .ok_or_else(|| format!("Invalid step at index {}: storyFile must be a string", i))?
                .to_string();
            EpicStepInput::Story(EpicStep { story_file: name })
        } else if let Some(parallel) = step_obj.get("parallel") {
            let arr = parallel
                .as_array()
                .ok_or_else(|| format!("Invalid step at index {}: parallel must be an array", i))?;
            let mut inner = Vec::with_capacity(arr.len());
            for (k, child) in arr.iter().enumerate() {
                let child_obj = child.as_object().ok_or_else(|| {
                    format!(
                        "Invalid step at index {}: parallel[{}] is not an object",
                        i, k
                    )
                })?;
                let name = child_obj
                    .get("storyFile")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        format!(
                            "Invalid step at index {}: parallel[{}] missing storyFile",
                            i, k
                        )
                    })?
                    .to_string();
                inner.push(EpicStep { story_file: name });
            }
            EpicStepInput::Parallel(EpicParallelGroup { parallel: inner })
        } else if let Some(se) = step_obj.get("subEpic") {
            let name = se
                .as_str()
                .ok_or_else(|| format!("Invalid step at index {}: subEpic must be a string", i))?
                .to_string();
            EpicStepInput::SubEpic(EpicSubEpicStep { sub_epic: name })
        } else {
            return Err(format!(
                "Invalid step at index {}: must be storyFile, parallel, or subEpic",
                i
            ));
        };
        steps.push(parsed);
    }

    // Reference resolution + duplicate detection. RF-16: prebuild name sets so
    // each membership test is O(1) — the loop was O(steps × stories). Behavior
    // is identical: `Duplicate` is still checked before `Unresolved`, and a name
    // lands in `seen_story_files` only after BOTH checks pass (same precedence
    // and same error-path state as the prior `Vec`-scan version).
    use std::collections::HashSet;
    let story_names: HashSet<&str> = stories.iter().map(|sr| sr.file_name.as_str()).collect();
    let sub_epic_names: HashSet<&str> = sub_epics.iter().map(|e| e.dir_name.as_str()).collect();
    let mut seen_story_files: HashSet<String> = HashSet::new();
    for step in &steps {
        match step {
            EpicStepInput::Story(s) => {
                if seen_story_files.contains(&s.story_file) {
                    return Err(format!("Duplicate storyFile: {}", s.story_file));
                }
                if !story_names.contains(s.story_file.as_str()) {
                    return Err(format!("Unresolved storyFile: {}", s.story_file));
                }
                seen_story_files.insert(s.story_file.clone());
            }
            EpicStepInput::Parallel(g) => {
                for s in &g.parallel {
                    if seen_story_files.contains(&s.story_file) {
                        return Err(format!("Duplicate storyFile: {}", s.story_file));
                    }
                    if !story_names.contains(s.story_file.as_str()) {
                        return Err(format!("Unresolved storyFile: {}", s.story_file));
                    }
                    seen_story_files.insert(s.story_file.clone());
                }
            }
            EpicStepInput::SubEpic(se) => {
                if !sub_epic_names.contains(se.sub_epic.as_str()) {
                    return Err(format!("Unresolved subEpic: {}", se.sub_epic));
                }
            }
        }
    }

    Ok(EpicPipelineSpec { version: 1, steps })
}

/// Cap on the number of missed paths buffered for the SF-3 warn-site log
/// emit. Misses beyond this count still increment the total — only the
/// buffered path list is bounded.
const MAX_LOGGED_MISSES: usize = 20;

/// Tracks cache miss counts during a scan. R-14 AC #23: one warn log per
/// scan, not per miss. The scanner instantiates one of these at entry and
/// threads it through recursive walk_dir calls.
///
/// SF-3 (Bug C): names the missed paths in the warn so the next sub-story
/// can fix the underlying cache-index mismatch. `missed_paths` is capped at
/// `MAX_LOGGED_MISSES` to bound allocation under hypothetical large-miss scans.
struct ScanStats {
    caller: crate::context_repo::caller_scope::CallerScope,
    view: Option<std::sync::Arc<crate::context_repo::inheritance::TeamView>>,
    deadline: Option<std::time::Instant>,
    read_failure: std::cell::RefCell<Option<String>>,
    cache_misses: std::cell::Cell<usize>,
    /// SW-FX-9: count of stale cache hits — entries whose backing file is
    /// newer than the cached mtime, so the scan re-parsed from disk. Tracked
    /// separately from `cache_misses` so the miss diagnostic keeps meaning
    /// "cache-index gap" rather than conflating it with disk-truth re-parses.
    cache_stale: std::cell::Cell<usize>,
    missed_paths: std::cell::RefCell<Vec<PathBuf>>,
    /// SW-LN-B50: cache-index gaps the story index itself would accept — a story
    /// with a `source_repo` that parses fine now but has no index entry (it was read
    /// mid-write, before its frontmatter landed). The scan command re-indexes these
    /// so the index heals on the next scan instead of waiting for another write.
    index_gaps: std::cell::RefCell<Vec<PathBuf>>,
    /// RF-perf-11 (AC-1): fresh cache-hit story entries (the zero-read warm arm).
    cache_hits: std::cell::Cell<usize>,
    /// RF-perf-11: story entries served via the cache-miss inline-parse arm
    /// (genuine index gap, stale re-parse, or a cache-less scan). `cache_hits +
    /// inline_parses` = total story entries; the ratio is the cache-hit ratio (OQ-2).
    inline_parses: std::cell::Cell<usize>,
    /// RF-perf-11 (AC-1): per-phase wall-time accumulators (nanos, summed across the
    /// recursive walk). `Some` **only** when the RF-perf-0/9 timing gate is armed —
    /// resolved ONCE at construction, so a normal prod build reads the clock zero
    /// times anywhere in the scan (the inert-when-off invariant, extended to the
    /// scan phases). Read live by grepping the gated per-scan `debug!` emit; measured
    /// cold/warm by the `#[ignore]`d benchmark (which derives phases externally).
    phase: Option<PhaseAccum>,
}

/// RF-perf-11 (AC-1): the three scan phases the ranking hangs off. `Cell<u128>`
/// (single-threaded scan) nanos, only ever touched when the gate is armed.
#[derive(Default)]
struct PhaseAccum {
    /// Directory enumeration: `read_dir` + entry classification + sibling sort.
    walk_ns: std::cell::Cell<u128>,
    /// Cache-miss arm only: bounded read + frontmatter parse + open-tags + mtime.
    read_parse_ns: std::cell::Cell<u128>,
    /// Both arms: `route_file_entry` → `build_story_ref` clone + validity compute.
    build_ns: std::cell::Cell<u128>,
}

/// RF-perf-11: a plain-value readout of the phase accumulators for the one gated
/// per-scan emit. Micros (the emit unit); `None` when the gate is off.
struct PhaseTotals {
    walk_us: u128,
    read_parse_us: u128,
    build_us: u128,
}

impl ScanStats {
    fn new() -> Self {
        Self {
            view: None,
            caller: Default::default(),
            deadline: None,
            read_failure: std::cell::RefCell::new(None),
            cache_misses: std::cell::Cell::new(0),
            cache_stale: std::cell::Cell::new(0),
            missed_paths: std::cell::RefCell::new(Vec::new()),
            index_gaps: std::cell::RefCell::new(Vec::new()),
            cache_hits: std::cell::Cell::new(0),
            inline_parses: std::cell::Cell::new(0),
            // Resolve the gate ONCE — every per-phase `timer()` below is then a
            // branchless `Option` check, never a repeated gate read or clock read.
            phase: crate::install::frontmatter_perf::timing_enabled().then(PhaseAccum::default),
        }
    }
    // Navigation rejects partial enumeration. Legacy scanner helpers retain
    // their existing best-effort behavior while callers migrate independently.
    fn record_read_failure(&self, path: &Path, error: &std::io::Error) -> bool {
        if self.deadline.is_none() {
            return false;
        }
        self.read_failure.borrow_mut().get_or_insert_with(|| {
            format!("Failed to read directory: {}: {}", path.display(), error)
        });
        true
    }
    fn expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }

    /// SW-FX-9: record one stale cache hit (entry older than disk → re-parsed).
    fn bump_stale(&self) {
        self.cache_stale.set(self.cache_stale.get() + 1);
    }
    fn stale(&self) -> usize {
        self.cache_stale.get()
    }
    /// RF-perf-11: record one fresh cache hit (zero-read warm arm).
    fn bump_hit(&self) {
        self.cache_hits.set(self.cache_hits.get() + 1);
    }
    fn hits(&self) -> usize {
        self.cache_hits.get()
    }
    /// RF-perf-11: record one inline-parsed entry (cache-miss arm, any reason).
    fn bump_inline(&self) {
        self.inline_parses.set(self.inline_parses.get() + 1);
    }
    fn inline(&self) -> usize {
        self.inline_parses.get()
    }
    /// RF-perf-11: `Some(Instant::now())` only when timing is armed; `None` (no
    /// clock read) otherwise. Paired with an `add_*` call to accumulate a phase.
    #[inline]
    fn timer(&self) -> Option<std::time::Instant> {
        self.phase.as_ref().map(|_| std::time::Instant::now())
    }
    #[inline]
    fn add_walk(&self, t: Option<std::time::Instant>) {
        if let (Some(p), Some(t)) = (&self.phase, t) {
            p.walk_ns.set(p.walk_ns.get() + t.elapsed().as_nanos());
        }
    }
    #[inline]
    fn add_read_parse(&self, t: Option<std::time::Instant>) {
        if let (Some(p), Some(t)) = (&self.phase, t) {
            p.read_parse_ns
                .set(p.read_parse_ns.get() + t.elapsed().as_nanos());
        }
    }
    #[inline]
    fn add_build(&self, t: Option<std::time::Instant>) {
        if let (Some(p), Some(t)) = (&self.phase, t) {
            p.build_ns.set(p.build_ns.get() + t.elapsed().as_nanos());
        }
    }
    /// RF-perf-11: the phase totals in micros for the gated emit — `None` when the
    /// gate is off (the caller then skips the `debug!` entirely).
    fn phase_totals(&self) -> Option<PhaseTotals> {
        self.phase.as_ref().map(|p| PhaseTotals {
            walk_us: p.walk_ns.get() / 1000,
            read_parse_us: p.read_parse_ns.get() / 1000,
            build_us: p.build_ns.get() / 1000,
        })
    }
    /// SF-3: record one miss + buffer the path if under cap. Both arms run
    /// unconditionally — `misses()` reflects the true count, `missed_paths`
    /// reflects up to the first `MAX_LOGGED_MISSES` paths.
    fn bump_miss(&self, path: &Path) {
        self.cache_misses.set(self.cache_misses.get() + 1);
        let mut buf = self.missed_paths.borrow_mut();
        if buf.len() < MAX_LOGGED_MISSES {
            buf.push(path.to_path_buf());
        }
    }
    fn misses(&self) -> usize {
        self.cache_misses.get()
    }
    /// SW-LN-B50: record an index gap the story index would accept (see
    /// `index_gaps`). Not capped like `missed_paths`: every gap is healed, and a
    /// gap is re-indexed once, then served from the index.
    fn note_index_gap(&self, path: &Path) {
        self.index_gaps.borrow_mut().push(path.to_path_buf());
    }
    /// SF-3: clone the buffered path list for the warn-site emit. Returns at
    /// most `MAX_LOGGED_MISSES` entries even if `misses()` is larger.
    fn missed_paths_snapshot(&self) -> Vec<PathBuf> {
        self.missed_paths.borrow().clone()
    }
}

/// Recursively scan one directory. `dir_absolute` must be a real dir;
/// `dir_path_rel_pipeline_root` is the path relative to `pipelineRoot`
/// (empty string for scope at pipelineRoot itself).
///
/// R-14: `cache` is `Some(&snapshot)` when the scanner has access to the
/// frontmatter cache (warm production path). `None` triggers inline-parse
/// for every story file — used by tests that don't construct a cache.
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    dir_absolute: &Path,
    dir_path_rel: String,
    main_doc: &str,
    summary_headings: &[String],
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    stats: &ScanStats,
    meta_cfg: &MetadataValidityConfig,
) -> DerivedPipelineTree {
    let dir_name = dir_absolute
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut stories: Vec<StoryRef> = Vec::new();
    let mut docs: Vec<DocRef> = Vec::new();
    let mut sub_epics: Vec<DerivedPipelineTree> = Vec::new();
    let mut pipeline_json_path: Option<PathBuf> = None;

    // RF-perf-11 (AC-1): time the walk phase — `read_dir` + entry classification +
    // sibling sort for THIS directory (the recursive sub-dir walks accumulate their
    // own walk time into the same shared `stats`, so no double-count).
    let walk_t = stats.timer();

    let entries = match fs::read_dir(dir_absolute) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                target: "pipeline_scanner",
                "read_dir failed for {}: {}",
                dir_absolute.display(),
                e
            );
            // RF-perf-11 (code-review): attribute the failed-`read_dir` latency to
            // the walk phase before bailing — a hung/slow FS is exactly the walk
            // cost the ranking wants to see, so the early return must not drop it.
            stats.add_walk(walk_t);
            return DerivedPipelineTree {
                dir_path: dir_path_rel,
                dir_name,
                absolute_dir_path: dir_absolute.display().to_string(),
                stories,
                docs,
                sub_epics,
                order: None,
                order_error: Some(format!("Failed to read directory: {}", e)),
                main_doc: None,
            };
        }
    };

    let mut meta_dir_path: Option<PathBuf> = None;
    let mut sub_dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut story_entries: Vec<(String, PathBuf)> = Vec::new();
    // Candidate main doc files (README.md, index.md, etc.) — collected
    // separately from story_entries because is_pipeline_document() rejects them.
    let mut main_doc_candidates: Vec<(String, PathBuf)> = Vec::new();
    // Non-pipeline supporting docs (e.g. `.excalidraw`) — go straight to
    // `docs[]` as plain file rows, no frontmatter read.
    let mut supporting_doc_entries: Vec<(String, PathBuf)> = Vec::new();

    for entry in entries {
        if stats.expired() {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if stats.record_read_failure(dir_absolute, &error) {
                    break;
                }
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".meta" {
            continue;
        }

        if !stats.caller.visible(&entry.path()) {
            continue;
        }
        if stats
            .view
            .as_ref()
            .is_some_and(|view| view.read_path(&entry.path()).is_err())
        {
            continue;
        }

        // Hard-reject symlinks (files AND dirs) per AC-12.
        let symlink_meta = match fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    target: "pipeline_scanner",
                    "symlink_metadata failed for {}: {}",
                    entry.path().display(),
                    e
                );
                if stats.record_read_failure(&entry.path(), &e) {
                    break;
                }
                continue;
            }
        };
        if symlink_meta.file_type().is_symlink() {
            continue;
        }

        if symlink_meta.file_type().is_dir() {
            if name == ".meta" {
                meta_dir_path = Some(entry.path());
                continue;
            }
            sub_dirs.push((name, entry.path()));
        } else if symlink_meta.file_type().is_file() {
            if is_pipeline_document(&name) {
                story_entries.push((name, entry.path()));
            } else if has_pipeline_extension(&name) {
                // Rejected by is_pipeline_document() — potential main doc
                // (README/index or a configured name with any pipeline extension).
                main_doc_candidates.push((name, entry.path()));
            } else if is_supporting_doc(&name) {
                // Non-pipeline supporting doc (e.g. `.excalidraw`) → docs[].
                supporting_doc_entries.push((name, entry.path()));
            }
        }
    }

    // Deterministic ordering (matches file-tree natural sort).
    story_entries.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    sub_dirs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    // RF-perf-11: walk phase ends here (enumeration + classification + sort). The
    // per-story read/build below and the sub-dir recursion are their own phases.
    stats.add_walk(walk_t);

    // Locate pipeline.json inside .meta if present
    if let Some(meta) = meta_dir_path {
        let candidate = meta.join("pipeline.json");
        if candidate.is_file()
            && stats
                .view
                .as_ref()
                .is_none_or(|view| view.read_path(&candidate).is_ok())
        {
            pipeline_json_path = Some(candidate);
        }
    }

    // Read pipeline.json content once for ordering validation.
    // Config-only files (no `version` field) produce no order_error (P.3 AC-5).
    let mut pipeline_read_error: Option<String> = None;
    let pipeline_json_content: Option<String> =
        pipeline_json_path
            .as_ref()
            .and_then(|p| match fs::read_to_string(p) {
                Ok(content) => Some(content),
                Err(e) => {
                    pipeline_read_error =
                        Some(format!("Failed to read pipeline.json: {}", e.kind()));
                    None
                }
            });

    // ----- P.3: Detect main doc ------------------------------------------
    // Uses the global `main_doc` param instead of per-epic pipeline.json.
    // Must happen BEFORE story_entries are consumed into stories[], so
    // configured names like "Epic.md" can be removed from story_entries.
    let configured_main_doc = if main_doc.is_empty() {
        None
    } else {
        Some(main_doc)
    };
    let (main_doc_result, remove_story_idx) = detect_main_doc(
        &main_doc_candidates,
        &story_entries,
        configured_main_doc,
        summary_headings,
        meta_cfg,
    );
    if let Some(idx) = remove_story_idx {
        story_entries.remove(idx);
    }

    // R-14: cache-hit path reads `entry.fields` directly (zero file I/O).
    // Cache miss falls back to inline `extract_story_meta` — same code
    // path as pre-R-14 minus the title/story_text fields.
    //
    // PTS-2 follow-up: both arms converge on `route_file_entry`, which
    // splits the entry into either `stories` (pipeline-type frontmatter)
    // or `docs` (no/non-pipeline type) via the `is_pipeline_story`
    // predicate. Single classification path, single rouster — no risk
    // of cache-hit and cache-miss disagreeing.
    for (name, path) in &story_entries {
        if stats.expired() {
            break;
        }
        let cached = cache.and_then(|c| c.entries.get(path));
        if let Some(entry) = cached {
            // SW-FX-9 disk-truth gate: trust the cached fields only when the
            // backing file is no newer than the cached mtime. A stale entry
            // (a watcher-suppressed materialize, or a status write the watcher
            // hasn't refreshed yet) must NOT shadow disk — fall through to the
            // inline parse below so the scan reflects exactly what's on disk.
            if crate::install::frontmatter_cache::entry_is_fresh(entry) {
                // Recovered fields and their diagnostic travel together in the cache.
                // SW-DX-2 (AC-13): open tags come off the cached entry (zero I/O
                // — populated once at cache-build via `open_tag_values`).
                // RF-perf-11 (AC-1): the warm arm is walk + BUILD only (zero read).
                stats.bump_hit();
                let build_t = stats.timer();
                route_file_entry(
                    name.clone(),
                    path,
                    &entry.fields,
                    &mut stories,
                    &mut docs,
                    meta_cfg,
                    entry.metadata_parse_error.as_deref(),
                    entry.open_tags.clone(),
                    // Warm arm: the cached mtime is authoritative (disk-truth
                    // gate above already proved the file is no newer).
                    Some(entry.mtime),
                );
                stats.add_build(build_t);
                continue;
            }
            stats.bump_stale();
            // stale → fall through to the disk parse (same path as a miss).
        }
        // Cache miss (or no cache available, e.g. tests), or a stale entry
        // that fell through the gate above. Inline-parse from disk.
        //
        // SW-FX-39: `extract_story_meta` returns `None` for exactly one case —
        // the F.30 gate: `.html`/`.htm` with no `cs:*` meta tags. Such a file is
        // not a pipeline STORY, but it IS a supporting doc — route it to `docs[]`
        // like a frontmatter-less `.md` (via `route_file_entry`'s
        // `is_pipeline_story()` split, which guarantees empty fields can never
        // land in `stories[]`). Previously this `continue`d and dropped the file
        // from the tree entirely, so a plain HTML doc was invisible in the nav
        // even though ⌘P found it. F.30's ghost-STORY concern is preserved by
        // `is_pipeline_story()`; the drop was over-reach. Seam 2 (the story
        // frontmatter cache) still keeps its F.30 `None`-return — docs aren't
        // cached, so `docs[]` visibility is a scanner-only concern.
        // RF-perf-11 (AC-1): the cache-miss arm is the read+parse phase — this is
        // the only arm that touches disk beyond the walk's `read_dir`. Its cost is
        // what `cold − warm` isolates in the benchmark.
        stats.bump_inline();
        let read_parse_t = stats.timer();
        // FIX 6 / AC-11: single read yields both the meta and whether the
        // metadata block parses — the present-but-unparseable badge case, with
        // NO second file read (was a latency regression on cache-miss scans).
        // RF-perf-11 (AC-4): single read — meta + block_parse_ok + the raw chunk
        // come from ONE `read_first_chunk` (was two: this call + a second
        // `extract_open_tags` below). Halves the cold `read_parse` phase's file
        // opens for stories, byte-identical.
        let (meta_opt, _block_parse_ok, chunk) = read_meta_parse_ok_and_chunk(path);
        if chunk.is_none() && stats.deadline.is_some() {
            stats
                .read_failure
                .borrow_mut()
                .get_or_insert_with(|| format!("Failed to read content file: {}", path.display()));
            stats.add_read_parse(read_parse_t);
            break;
        }
        let mut meta = meta_opt.unwrap_or_default();
        let parse_error = meta.metadata_parse_error.take();
        let fields = crate::install::frontmatter_cache::story_meta_to_fields(meta);
        // Computed once — the miss-diagnostic guard and the open-tags keep both
        // key off it (was two `is_pipeline_story()` calls).
        let is_story = fields.is_pipeline_story();
        // SF-3 AC-14: only count files that pass `extract_story_meta` as
        // misses. SW-FX-9: count a *miss* only for a genuine cache-index gap
        // (cache present, no entry) — a stale entry was already recorded via
        // `bump_stale` and must not double-count here, so the named-paths
        // list stays scoped to "files that look like pipeline docs but aren't
        // in the cache index," not disk-truth re-parses.
        // SW-FX-12: also gate on `is_pipeline_story`. A file routing to `docs[]`
        // is expected non-cached content — it can never be in the
        // `source_repo`-keyed index, so it's not an actionable cache-index gap
        // and must not inflate the `cache miss(es) inline-parsed` WARN emitted
        // below. A genuine pipeline story absent from the index still counts.
        // Note: transient races (e.g. a file just written and not yet
        // indexed by the frontmatter cache) can still appear in the list —
        // they are legitimate-but-non-actionable misses.
        if cache.is_some() && cached.is_none() && is_story {
            stats.bump_miss(path);
        }
        // SW-LN-B50: an indexable story (story + `source_repo`) the index has no entry
        // for, or a stale one whose story_id differs from disk (a mid-write read that
        // caught `source_repo` but not `story_id` yet) — the scan command re-indexes it.
        if cache.is_some()
            && is_story
            && fields.source_repo.is_some()
            && cached.is_none_or(|e| e.story_id() != fields.story_id.as_deref())
        {
            stats.note_index_gap(path);
        }
        // SW-DX-2 (AC-13): open tags only for a story (a `docs[]` entry carries no
        // queryable `StoryRef`). Off the warm path — the cache-hit arm above uses
        // `entry.open_tags` and never reaches here. RF-perf-11: derived from the
        // chunk `read_meta_parse_ok_and_chunk` already read — no second open.
        let open_tags = match (is_story, &chunk) {
            (true, Some((text, format))) => open_tags_map(
                crate::commands::frontmatter_editor::open_tag_values(*format, text),
            ),
            _ => std::collections::BTreeMap::new(),
        };
        // Inline arm (a cache-less scan):
        // stat the file for its mtime so freshness data reaches `DocSummary`
        // even with no warm cache. A stat failure degrades to `None` (Freshness
        // simply excludes the doc), never an error.
        let mtime = crate::install::frontmatter_cache::mtime_of(path);
        stats.add_read_parse(read_parse_t);
        // RF-perf-11: build phase — same `route_file_entry` → `build_story_ref`
        // clone the warm arm times, so cold/warm build cost is directly comparable.
        let build_t = stats.timer();
        route_file_entry(
            name.clone(),
            path,
            &fields,
            &mut stories,
            &mut docs,
            meta_cfg,
            parse_error.as_deref(),
            open_tags,
            mtime,
        );
        stats.add_build(build_t);
    }

    // Supporting docs (e.g. `.excalidraw`) join `docs[]` directly — no
    // frontmatter, so no story/doc routing decision. They carry no metadata,
    // so `metadata_valid` is always true (absent = valid). Sorted to match the
    // file-tree natural order.
    supporting_doc_entries.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    for (name, path) in supporting_doc_entries {
        if stats.expired() {
            break;
        }
        docs.push(DocRef {
            file_name: name,
            file_path: path.display().to_string(),
            metadata_valid: true,
            metadata_invalid_reason: None,
            // Supporting docs (images, canvases) carry no frontmatter → never a subject.
            subject_id: None,
            source_team: None,
            overrides_team: None,
        });
    }

    for (name, path) in sub_dirs {
        if stats.expired() {
            break;
        }
        let child_rel = if dir_path_rel.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", dir_path_rel, name)
        };
        sub_epics.push(walk_dir(
            &path,
            child_rel,
            main_doc,
            summary_headings,
            cache,
            stats,
            meta_cfg,
        ));
    }

    // Only run ordering validation when the JSON has a `version` field —
    // a config-only file (e.g. `{}` or any JSON without `version`) should
    // not produce an order_error badge. Read errors and unparseable JSON
    // still surface as order_error.
    let (order, order_error) = if let Some(read_err) = pipeline_read_error {
        (None, Some(read_err))
    } else {
        match &pipeline_json_content {
            None => (None, None),
            Some(content) => match serde_json::from_str::<Value>(content) {
                Err(e) => (None, Some(format!("Invalid JSON: {}", e))),
                Ok(ref val) if val.get("version").is_none() => {
                    // Config-only file (no version field) — not an ordering spec.
                    (None, None)
                }
                Ok(_) => match parse_pipeline_json(content, &stories, &sub_epics) {
                    Ok(spec) => (Some(spec), None),
                    Err(msg) => (None, Some(msg)),
                },
            },
        }
    };

    DerivedPipelineTree {
        dir_path: dir_path_rel,
        dir_name,
        absolute_dir_path: dir_absolute.display().to_string(),
        stories,
        docs,
        sub_epics,
        order,
        order_error,
        main_doc: main_doc_result,
    }
}

/// Compute `{teamContentPath}/{pipeline_root}`, returning an error if the
/// base doesn't resolve to an existing directory.
///
/// PTS-2: dropped `viewer_root` — callers now pass `team_content_path`
/// (the per-team content dir) directly, so the function no longer needs
/// to compose source-side projection segments.
///
/// SW-FX-56 (flow-2 / COMPAT SEAM — deliberately KEPT, not renamed): the
/// `pipeline_root` param here is the retired **scan-scope** arg, NOT the
/// staleness-override glob prefix (that one was `override_glob_prefix`, a
/// different value, retired by SW-XS-14/XS-12). Every prod caller passes
/// `""` (R-14/PTS-2 scope-collapse), so the non-empty branch below is
/// exercised only by scope-recovery + ~70 tests. Kept intact so the
/// `scan_pipeline_tree` / `find_nearest_valid_scope` IPC signatures stay
/// non-breaking.
fn resolve_pipeline_base(team_content_path: &Path, pipeline_root: &str) -> Result<PathBuf, String> {
    let normalized_pipeline = normalize_pipeline_root(pipeline_root)?;

    let mut base = team_content_path.to_path_buf();
    if !normalized_pipeline.is_empty() {
        base.push(&normalized_pipeline);
    }
    if !base.is_dir() {
        return Err(format!("Pipeline root not found: {}", base.display()));
    }
    Ok(base)
}

/// Pure scanner callable from tests. `context_repo_path` must be an absolute
/// path to the repo. No validation against `contexts_dir` here — the Tauri
/// command wrapper does that.
///
/// R-14: gated to test builds. Production callers go through
/// `scan_pipeline_tree` (the Tauri command), which acquires a cache
/// snapshot before delegating to `scan_pipeline_tree_impl_with_cache`.
/// This thin wrapper exists only so unit tests can run the scanner with
/// `cache: None` (forcing the inline-parse fallback path) without
/// constructing a Tauri `AppHandle`.
#[cfg(test)]
pub fn scan_pipeline_tree_impl(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
) -> Result<DerivedPipelineTree, String> {
    // SW-EW-32: the 4-arg test wrapper keeps its signature; it threads the
    // default summary headings so the ~70 existing call sites stay untouched.
    scan_pipeline_tree_impl_with_cache(
        team_content_path,
        pipeline_root,
        scope_path,
        main_doc,
        &default_summary_headings(),
        None,
        &MetadataValidityConfig::default(),
    )
}

fn directory_read_failure(tree: &DerivedPipelineTree) -> Option<&str> {
    let mut pending = vec![tree];
    while let Some(node) = pending.pop() {
        if let Some(error) = node.order_error.as_deref() {
            if error.starts_with("Failed to read directory:") {
                return Some(error);
            }
        }
        pending.extend(&node.sub_epics);
    }
    None
}

/// R-14: Cache-aware variant. Pass `Some(&snapshot)` for the zero-read warm
/// path; `None` triggers inline-parse (tests). Production `scan_pipeline_tree`
/// command wraps this with the cache state from Tauri.
///
/// PTS-2: takes `team_content_path` (`{org_root}/{team}/content/`)
/// directly — `viewer_root` was retired since the context-side anchor no
/// longer mirrors the source-side projection name.
#[allow(clippy::too_many_arguments)]
pub fn scan_pipeline_tree_impl_with_cache(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
    summary_headings: &[String],
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    // SLW-MD-1: configured repo/team sets resolved once per scan (locked
    // decision #5). `MetadataValidityConfig::default()` (empty) disables badging.
    meta_cfg: &MetadataValidityConfig,
) -> Result<DerivedPipelineTree, String> {
    scan_pipeline_tree_with_index_gaps(
        team_content_path,
        pipeline_root,
        scope_path,
        main_doc,
        summary_headings,
        cache,
        meta_cfg,
    )
    .map(|(tree, _)| tree)
}

/// SW-LN-B50: [`scan_pipeline_tree_impl_with_cache`] plus the stories this scan
/// parsed as indexable (story + `source_repo`) that the index has no entry for —
/// the files a mid-write read left out of the index. The scan command re-indexes
/// them, so a story the index missed heals on the next scan.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_pipeline_tree_with_index_gaps(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
    summary_headings: &[String],
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    meta_cfg: &MetadataValidityConfig,
) -> Result<(DerivedPipelineTree, Vec<PathBuf>), String> {
    scan_pipeline_tree_with_deadline(
        team_content_path,
        pipeline_root,
        scope_path,
        main_doc,
        summary_headings,
        cache,
        meta_cfg,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_pipeline_tree_with_deadline(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
    summary_headings: &[String],
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    meta_cfg: &MetadataValidityConfig,
    deadline: Option<std::time::Instant>,
) -> Result<(DerivedPipelineTree, Vec<PathBuf>), String> {
    scan_pipeline_tree_with_deadline_for(
        team_content_path,
        pipeline_root,
        scope_path,
        main_doc,
        summary_headings,
        cache,
        meta_cfg,
        deadline,
        &Default::default(),
    )
}

pub(crate) fn scan_pipeline_tree_for(
    content: &Path,
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<DerivedPipelineTree, String> {
    scan_pipeline_tree_with_deadline_for(
        content,
        "",
        "",
        "README.md",
        &default_summary_headings(),
        cache,
        &MetadataValidityConfig::default(),
        None,
        caller,
    )
    .map(|(tree, _)| tree)
}

fn scan_pipeline_tree_with_deadline_for(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
    summary_headings: &[String],
    cache: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    meta_cfg: &MetadataValidityConfig,
    deadline: Option<std::time::Instant>,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(DerivedPipelineTree, Vec<PathBuf>), String> {
    // PTS-2: if the team's content root itself is missing, the team has no
    // content yet (capture/migration hasn't populated it). Return an empty
    // tree so the UI renders an empty state rather than surfacing a
    // "Pipeline root not found" / "Scope path not found" error — this is
    // the post-migration shape for teams that had no legacy files to copy
    // and no captures yet. Short-circuits BEFORE `resolve_pipeline_base`
    // so we don't trip its Err on a missing `{team_content}/{pipeline_root}`.
    //
    // The Err paths in `resolve_pipeline_base` + the `target.is_dir` check
    // below stay live for the in-content-but-scope-missing case (drilled
    // into a deleted subdir within a real content dir), where R-14 scope
    // recovery is expected to fix the drill.
    if deadline.is_some() {
        let metadata = fs::metadata(team_content_path).map_err(|error| {
            format!(
                "Team content is unavailable at {}: {error}",
                team_content_path.display()
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!(
                "Team content is not a directory: {}",
                team_content_path.display()
            ));
        }
    }
    if !team_content_path.is_dir() {
        return Ok((
            DerivedPipelineTree {
                dir_path: String::new(),
                dir_name: String::new(),
                absolute_dir_path: team_content_path.display().to_string(),
                stories: Vec::new(),
                docs: Vec::new(),
                sub_epics: Vec::new(),
                order: None,
                order_error: None,
                main_doc: None,
            },
            Vec::new(),
        ));
    }

    let normalized_scope = normalize_scope_path(scope_path)?;
    let base = resolve_pipeline_base(team_content_path, pipeline_root)?;

    let mut target = base.clone();
    if !normalized_scope.is_empty() {
        target.push(&normalized_scope);
    }
    if !target.is_dir() {
        return Err(format!("Scope path not found: {}", target.display()));
    }

    let view = crate::context_repo::inheritance::view_for_content(team_content_path);
    if view
        .as_ref()
        .is_some_and(|v| v.read_path_strict(&target).is_err())
    {
        return Err(format!("Scope path not found: {}", target.display()));
    }
    if !caller.visible(&target) {
        return Err("Scope path not found".into());
    }
    let stats = ScanStats {
        view,
        caller: caller.clone(),
        deadline,
        ..ScanStats::new()
    };
    let dir_path_rel = normalized_scope.clone();
    let tree = walk_dir(
        &target,
        dir_path_rel,
        main_doc,
        summary_headings,
        cache,
        &stats,
        meta_cfg,
    );

    if stats.expired() {
        return Err("Reading team context timed out; the incomplete scan was discarded".into());
    }
    if deadline.is_some() {
        if let Some(error) = stats.read_failure.borrow().as_ref() {
            return Err(error.clone());
        }
        if let Some(error) = directory_read_failure(&tree) {
            return Err(error.to_string());
        }
    }

    let misses = stats.misses();
    if misses > 0 {
        // SF-3 AC-15: name the missed paths so the next sub-story can fix
        // the cache-index mismatch. Old "cold start or post-upgrade transient"
        // phrasing removed — the investigation log showed persistent
        // steady-state misses, not transients.
        let paths = stats.missed_paths_snapshot();
        let path_strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let suffix = if misses > path_strs.len() {
            format!(" (showing first {} of {misses})", path_strs.len())
        } else {
            String::new()
        };
        warn!(
            target: "pipeline_scanner",
            "scan_pipeline_tree: {misses} cache miss(es) inline-parsed{suffix}: [{}]",
            path_strs.join(", ")
        );
    }
    // SW-FX-9: stale cache hits are expected and benign (the disk-truth gate
    // working — e.g. after a watcher-suppressed materialize). Log at debug so
    // a high steady-state count is discoverable without spamming the warn
    // channel reserved for genuine cache-index gaps.
    let stale = stats.stale();
    if stale > 0 {
        debug!(
            target: "pipeline_scanner",
            "scan_pipeline_tree: {stale} stale cache hit(s) re-parsed from disk"
        );
    }

    // RF-perf-11 (AC-1): always-on aggregate frequency tally (one set of Relaxed
    // adds per scan) + a single gated per-scan phase breakdown. The tally supplies
    // the live cache-hit ratio (OQ-2) without scraping logs; the `debug!` supplies
    // per-phase wall-time when the RF-perf-0/9 gate is armed (dev build, or
    // `CODESENSE_LOG=debug`). Reuses the one gate — no parallel switch.
    let hits = stats.hits();
    let inline = stats.inline();
    crate::install::frontmatter_perf::note_scan(hits, inline);
    if let Some(phase) = stats.phase_totals() {
        let total = hits + inline;
        // Vacuously 1.0 for an empty scan (0 stories) — the emit is a diagnostic,
        // never a control-flow input. `inline_parses` folds genuine index-gap
        // misses AND disk-truth stale re-parses (`entry_is_fresh` fall-through), so
        // `stale` is emitted alongside: a depressed `hit_ratio` that is mostly
        // `stale` is a watcher lag, not a cache-index problem (SW-FX-9's split).
        // Note the warm-arm `entry_is_fresh` stat itself lands in neither `walk`
        // nor `build` in-situ (it runs before the build timer); the `#[ignore]`d
        // bench's `walk+stat` bucket is where that per-hit stat is measured.
        let hit_ratio = if total > 0 {
            hits as f64 / total as f64
        } else {
            1.0
        };
        debug!(
            target: crate::install::frontmatter_perf::SYNC_TIMING_TARGET,
            "scan_pipeline_tree phases: walk={}us read_parse={}us build={}us \
             stories={total} cache_hits={hits} inline_parses={inline} stale={} \
             hit_ratio={hit_ratio:.3}",
            phase.walk_us, phase.read_parse_us, phase.build_us, stale,
        );
    }
    let index_gaps = stats.index_gaps.take();
    Ok((tree, index_gaps))
}

/// Pure scope recovery callable from tests.
///
/// PTS-2: takes `team_content_path` directly; `viewer_root` retired.
pub fn find_nearest_valid_scope_impl(
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
) -> Result<String, String> {
    let normalized_scope = normalize_scope_path(scope_path)?;
    let base = resolve_pipeline_base(team_content_path, pipeline_root)?;

    if normalized_scope.is_empty() {
        return Ok(String::new());
    }

    let mut segments: Vec<&str> = normalized_scope.split('/').collect();
    while !segments.is_empty() {
        let candidate = segments.join("/");
        let mut abs = base.clone();
        abs.push(&candidate);
        if abs.is_dir() {
            return Ok(candidate);
        }
        segments.pop();
    }
    Ok(String::new())
}

// ----- Status update (story 34.5) --------------------------------------------

/// Valid pipeline status values. Must be kept in sync with the TS
/// `PIPELINE_STATUSES` array in `src/shared/lib/pipeline-graph/types.ts`
/// (enforced by the SW-MCP-10 `status_and_doc_type_vocab_in_sync_with_ts`
/// drift test).
///
/// SW-MCP-10: `pub(crate)` so `get_story_format` / `validate_story` reuse the
/// SAME canonical status list — no third vocabulary is introduced.
pub(crate) const VALID_STATUSES: &[&str] = &[
    "planning",
    "todo",
    "in-progress",
    "blocked",
    "review",
    "qa",
    "done",
];

/// Serializes a story's `status` read-modify-writes (the board move, the metadata editor) so two concurrent writers can't interleave their frontmatter cycles. Coarse,
/// but status writes are infrequent.
static STATUS_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold the status write lock for the guard's lifetime. Poison-tolerant: a panic in a prior
/// writer must not wedge every later status write.
pub(crate) fn status_write_lock() -> std::sync::MutexGuard<'static, ()> {
    STATUS_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write a story's `status:` (and optionally `rank`) in one read/write cycle. `status` is plain
/// frontmatter: any non-empty single-line value (no control characters) is written as given, in or out of the team's taxonomy (an
/// off-taxonomy value renders as drift). When `expect_current` is `Some`, the write is a
/// **compare-and-swap** that no-ops (returns `Ok(false)`) unless the story's current on-disk
/// `status:` still equals it. `Ok(true)` iff the file was written.
///
/// The write is atomic (temp+fsync+rename, never a torn file). Folding `rank` into this cycle
/// (rather than a separate `apply_metadata_edit` call) is deliberate: a cross-column board drop
/// sets both fields atomically, so two concurrent read-modify-write cycles can't clobber each
/// other (SW-SB-22 D-2).
pub fn write_story_status(
    file_path: &Path,
    new_status: &str,
    new_rank: Option<&str>,
    expect_current: Option<&str>,
) -> Result<bool, String> {
    if new_status.trim().is_empty() {
        return Err("A status cannot be empty".to_string());
    }
    // `rewrite_field` writes the value as a single-line scalar: a newline would inject a
    // frontmatter line, so any control character is refused before the file is touched.
    for (label, value) in [("Status", Some(new_status)), ("Rank", new_rank)] {
        if let Some(c) = value.and_then(|v| v.chars().find(|c| c.is_control())) {
            return Err(format!(
                "{label} contains a control character ({c:?}) — not allowed in a single-line metadata value"
            ));
        }
    }

    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

    let format = pipeline_format::format_of(file_path);
    let old_status = pipeline_format::extract_meta(format, &content).status;

    if let Some(expect) = expect_current {
        if old_status.as_deref() != Some(expect) {
            return Ok(false);
        }
    }

    let with_status = pipeline_format::rewrite_field(format, &content, "status", new_status);
    let final_content = match new_rank {
        Some(rank) => pipeline_format::rewrite_field(format, &with_status, "rank", rank),
        None => with_status,
    };

    atomic_io::write_tmp_fsync_rename(file_path, final_content.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", file_path.display(), e))?;

    Ok(true)
}

/// A user's status move: a locked compare-and-set write of `to` (and `rank`) against the status
/// read just before it, recorded as a human advance in the run store. No gate, no taxonomy
/// check — `status` is plain frontmatter.
pub(crate) fn move_story_status(path: &Path, to: &str, rank: Option<&str>) -> Result<(), String> {
    let to = to.trim();
    let _lock = status_write_lock();
    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let fields = pipeline_format::extract_meta(pipeline_format::format_of(path), &content);
    let from = fields.status;

    if !write_story_status(path, to, rank, from.as_deref())? {
        return Err(
            "The story's status changed while it was being moved. Refresh and try again."
                .to_string(),
        );
    }
    if from.as_deref() == Some(to) {
        return Ok(());
    }
    match fields.story_id {
        None => warn!(
            target: "run_store",
            "status move wrote {} with no story_id — status moved but not audited",
            path.display()
        ),
        Some(story_id) => run_store::record_status_advance(run_store::AdvanceEvent {
            story_id,
            from,
            to: to.to_string(),
            trigger: run_store::Trigger::Human,
            overridden: false,
            reason: None,
            steps: Vec::new(),
            action_identity: None,
            run_id: None,
            by: run_store::human_actor(),
        }),
    }
    Ok(())
}

/// Thin wrapper used by tests for readability.
#[cfg(test)]
fn rewrite_frontmatter_status(content: &str, new_status: &str) -> String {
    pipeline_format::markdown::rewrite_field(content, "status", new_status)
}

// ----- Tauri command wrappers ------------------------------------------------

/// The one place the warm-scan boundary takes its frontmatter-cache snapshot.
/// Every warm scan caller routes the `try_state → snapshot → warn-once` dance
/// through here so it lives in a single place rather than being re-inlined per
/// call site. Returns `None` when the cache state is not yet registered (first
/// launch, before install populates it) — callers fall back to an inline cold
/// parse, so a missing cache degrades gracefully rather than breaking the scan.
/// The snapshot is an O(1) `Arc` clone taken off the read lock, so it never
/// holds a lock across the scan walk.
///
/// Generic over the Tauri runtime purely for testability: production always
/// passes an `AppHandle<Wry>`, but a mock runtime (`AppHandle<MockRuntime>`)
/// lets a seam test register a `SharedFrontmatterCache` and assert this wiring.
pub(crate) fn frontmatter_cache_snapshot<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Option<std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>> {
    match app.try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>() {
        None => {
            // Warn at most ONCE per process, not per call. The unregistered-cache
            // state is monotonic — Tauri managed state is add-only, so once the cache
            // registers it never un-registers, and this arm is only reachable in the
            // pre-registration (boot / headless) window. This boundary now sits on hot
            // poll paths (the FE doc-query route, the sprint-stories HTTP route), so a
            // per-call warn would flood the log while adding no new signal. A plain
            // `AtomicBool::swap` gives at-most-once with NO blocking — unlike `Once`, no
            // concurrent caller parks while the first thread writes to a slow log sink,
            // and there is no poison/re-entrancy hazard (best-effort logging never needs
            // the run-exactly-once guarantee, only run-at-most-once). One line surfaces
            // the misconfiguration; the rest degrade quietly to the inline cold parse.
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!(
                    target: "pipeline_scanner",
                    "frontmatter_cache_snapshot: cache state not registered — falling back to inline-parse (warned once per process)"
                );
            }
            None
        }
        Some(state) => {
            Some(crate::install::frontmatter_cache::FrontmatterCache::snapshot(state.inner()))
        }
    }
}

/// SW-LN-B50: the scan command's body — snapshot the index, scan against it, then
/// re-index every story the scan parsed as indexable but the index lacked (or held a
/// stale partial read of), so a story a mid-write read left out heals on the next scan
/// without waiting for another write. `None` cache (tests / pre-setup): a cache-less
/// scan, nothing to heal.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_pipeline_tree_and_heal_index(
    shared: Option<&crate::install::frontmatter_cache::SharedFrontmatterCache>,
    team_content_path: &Path,
    pipeline_root: &str,
    scope_path: &str,
    main_doc: &str,
    summary_headings: &[String],
    meta_cfg: &MetadataValidityConfig,
) -> Result<DerivedPipelineTree, String> {
    // R-14 + code-review P11: snapshot-clone at scan entry, then walk the owned
    // snapshot, so watcher commits don't queue behind the recursive walk.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let snapshot = shared.map(crate::install::frontmatter_cache::FrontmatterCache::snapshot);
    let (mut tree, mut index_gaps) = scan_pipeline_tree_with_deadline(
        team_content_path,
        pipeline_root,
        scope_path,
        main_doc,
        summary_headings,
        snapshot.as_deref(),
        meta_cfg,
        Some(deadline),
    )?;
    inherited::overlay_inherited_teams(&mut tree, team_content_path, pipeline_root, |path| {
        if std::time::Instant::now() >= deadline {
            return Err("Reading team context timed out during inherited resolution".into());
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if is_supporting_doc(&name) {
            return Ok(DocRef {
                file_name: name,
                file_path: path.to_string_lossy().into_owned(),
                metadata_valid: true,
                metadata_invalid_reason: None,
                subject_id: None,
                source_team: None,
                overrides_team: None,
            });
        }
        let (meta, _, chunk) = read_meta_parse_ok_and_chunk(path);
        if chunk.is_none() {
            return Err(format!("Failed to read content file: {}", path.display()));
        }
        let mut meta = meta.unwrap_or_default();
        let parse_error = meta.metadata_parse_error.take();
        let fields = crate::install::frontmatter_cache::story_meta_to_fields(meta);
        if shared.is_some()
            && fields.is_pipeline_story()
            && fields.source_repo.is_some()
            && snapshot
                .as_ref()
                .is_some_and(|s| !s.entries.contains_key(path))
        {
            index_gaps.push(path.to_path_buf());
        }
        let (metadata_valid, metadata_invalid_reason) =
            compute_metadata_validity(&fields, meta_cfg, parse_error.as_deref());
        Ok(DocRef {
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            file_path: path.to_string_lossy().into_owned(),
            metadata_valid,
            metadata_invalid_reason,
            subject_id: fields.subject_id,
            source_team: None,
            overrides_team: None,
        })
    })?;
    drop(snapshot);
    if std::time::Instant::now() >= deadline {
        return Err("Reading team context timed out during inherited resolution".into());
    }
    if let (Some(shared), false) = (shared, index_gaps.is_empty()) {
        if !crate::install::frontmatter_cache::FrontmatterCache::reindex_scan_gaps_with_deadline(
            shared,
            &index_gaps,
            deadline,
        ) {
            return Err("Reading team context timed out during index refresh".into());
        }
    }
    if std::time::Instant::now() >= deadline {
        return Err("Reading team context timed out during index refresh".into());
    }
    Ok(tree)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn scan_pipeline_tree(
    app: tauri::AppHandle,
    team_content_path: String,
    pipeline_root: String,
    scope_path: String,
    main_doc: String,
    // SW-EW-32: ordered summary-section headings (first match wins). `Option`
    // for forward/backward tolerance — `None`/empty falls back to the defaults
    // via `effective_headings`.
    summary_headings: Option<Vec<String>>,
    // SLW-MD-1 (Task 4): the active team's CONFIGURED repo slugs + configured
    // team names, resolved ONCE per scan by the FE from the loaded org config
    // (no per-file lockfile walk — locked decision #5). Drives the nav's
    // invalid-metadata badge. `None`/empty configured set ⇒ source_repo/team
    // membership badging is disabled (a well-formed `source_repo` is checked
    // syntactic-only, never flagged on membership — a malformed slug is still
    // Invalid). Backward compatible — old callers omit these and see no
    // membership badges.
    configured_source_repos: Option<Vec<String>>,
    valid_teams: Option<Vec<String>>,
) -> Result<DerivedPipelineTree, String> {
    let headings = effective_headings(summary_headings.unwrap_or_default());
    let meta_cfg = MetadataValidityConfig {
        configured_repos: configured_source_repos.unwrap_or_default(),
        valid_teams: valid_teams.unwrap_or_default(),
    };
    crate::commands::await_blocking("scan_pipeline_tree", move || {
        debug!(
            target: "pipeline_scanner",
            "scan_pipeline_tree: content={}, root={}, scope={}, main_doc={}",
            team_content_path, pipeline_root, scope_path, main_doc
        );
        let repo = validate_context_path_owned(&team_content_path, &app)?;
        // R-14 + code-review P11: snapshot-clone the frontmatter cache at scan
        // entry (via the shared `frontmatter_cache_snapshot` boundary), then walk
        // against the owned snapshot. Trades one brief read lock + `Arc` clone for
        // write fairness — watcher commits don't queue behind the recursive walk,
        // and the snapshot stays consistent across the scan even if a writer
        // commits mid-walk.
        let shared = app
            .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
            .map(|s| s.inner().clone());
        if shared.is_none() {
            // Keeps the once-per-process "cache not registered" warning.
            drop(frontmatter_cache_snapshot(&app));
        }
        scan_pipeline_tree_and_heal_index(
            shared.as_ref(),
            &repo,
            &pipeline_root,
            &scope_path,
            &main_doc,
            &headings,
            &meta_cfg,
        )
    })
    .await
}

/// R-14: On-demand fetch for body-extracted fields (H1 title +
/// `## Story` section). The scanner no longer carries these on `StoryRef`
/// — consumers call this command lazily (e.g. pipeline-card preview,
/// board card subtitle) and a TS-side hook caches results client-side
/// with watcher-driven invalidation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryDetails {
    pub title: Option<String>,
    pub story_text: Option<String>,
    /// SW-LN-34: the metadata title (frontmatter `title:` / HTML `cs:title`),
    /// which cards prefer over the body `title`.
    pub title_meta: Option<String>,
}

#[tauri::command]
pub async fn get_story_details(
    app: tauri::AppHandle,
    file_path: String,
    // SW-EW-32: ordered summary-section headings (first match wins). `Option`
    // for tolerance — `None`/empty falls back to the defaults.
    summary_headings: Option<Vec<String>>,
) -> Result<StoryDetails, String> {
    let headings = effective_headings(summary_headings.unwrap_or_default());
    crate::commands::await_blocking("get_story_details", move || {
        let path = validate_context_path_owned(&file_path, &app)?;
        let body = extract_body(&path, &headings);
        Ok(StoryDetails {
            title: body.title,
            story_text: body.story_text,
            title_meta: body.title_meta,
        })
    })
    .await
}

#[tauri::command]
pub async fn update_pipeline_status(
    app: tauri::AppHandle,
    file_path: String,
    status: String,
    // SW-SB-22: optional board-ordering rank set atomically alongside status, so a
    // cross-column drop writes both fields in one cycle (D-2). `None` ⇒ status-only,
    // exactly today's behavior.
    rank: Option<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("update_pipeline_status", move || {
        debug!(
            target: "pipeline_scanner",
            "update_pipeline_status: file={}, status={}, rank={:?}",
            file_path, status, rank
        );
        let path = crate::commands::files::validate_context_write_path_owned(&file_path, &app)?;
        move_story_status(&path, &status, rank.as_deref())
    })
    .await
}

// ----- SW-EW-9: epic-workspace repo management -------------------------------

/// RF-5: guard for the epic-frontmatter writers, which only support markdown
/// docs. Returns the resolved [`PipelineFileFormat`] (always `Markdown` on the
/// `Ok` path) so the caller can pass it straight to the block-list rewriter;
/// errors — naming `caller` — when the doc is not markdown. The extension is
/// only computed on the error path (it exists solely for the message). The one
/// home for the "epic doc must be markdown" check both `set_epic_*_core`
/// writers shared.
fn require_markdown_doc(
    path: &Path,
    caller: &str,
) -> Result<pipeline_format::PipelineFileFormat, String> {
    let format = pipeline_format::format_of(path);
    if !matches!(format, pipeline_format::PipelineFileFormat::Markdown) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        return Err(format!(
            "{caller}: epic doc must be markdown, got .{ext}: {}",
            path.display()
        ));
    }
    Ok(format)
}

/// Read an epic doc's `repos:` as canonical `owner/repo` slug strings.
/// Reuses [`content_engine::load_epic_frontmatter`] + `epic_repos()` so the
/// read agrees with the assemble path; malformed entries are dropped there.
pub(crate) fn read_epic_repo_slugs(path: &Path) -> Result<Vec<String>, String> {
    let fields = crate::commands::content_engine::load_epic_frontmatter(&path.to_string_lossy())?;
    Ok(fields.epic_repos().iter().map(|s| s.to_string()).collect())
}

/// Surgically replace an epic doc's `repos:` block, then return the re-read
/// list (source of truth in one round-trip). Validates/normalizes each entry
/// via `SourceRepoSlug::parse_owner_repo` and drops duplicates (first wins).
/// Markdown only — an empty list removes the field. AppHandle-free so it is
/// unit-testable against a tempfile (the established `*_core` pattern).
pub(crate) fn set_epic_repos_core(path: &Path, repos: &[String]) -> Result<Vec<String>, String> {
    let format = require_markdown_doc(path, "set_epic_repos")?;

    // Validate + normalize each slug; drop duplicates (first occurrence wins).
    let mut validated: Vec<String> = Vec::with_capacity(repos.len());
    for raw in repos {
        let slug = crate::domain::SourceRepoSlug::parse_owner_repo(raw).ok_or_else(|| {
            format!("set_epic_repos: invalid repo slug {raw:?} (expected owner/repo)")
        })?;
        let canon = slug.to_string();
        if !validated.contains(&canon) {
            validated.push(canon);
        }
    }

    // Read-modify-write. A missing file fails the read (no create) — the
    // containment guard already ran in the command wrapper.
    let content = fs::read_to_string(path)
        .map_err(|e| format!("set_epic_repos: read {}: {e}", path.display()))?;
    let updated = pipeline_format::rewrite_list_field(format, &content, "repos", &validated);
    if updated != content {
        fs::write(path, &updated)
            .map_err(|e| format!("set_epic_repos: write {}: {e}", path.display()))?;
    }

    // Re-read from disk so the FE updates from the source of truth.
    read_epic_repo_slugs(path)
}

pub(crate) fn set_epic_repos_for(
    path: &Path,
    repos: &[String],
    scope: &crate::context_repo::caller_scope::CallerScope,
    on_written: impl FnOnce(&[String]),
) -> Result<Vec<String>, String> {
    let readable = scope.allows(path);
    let result = set_epic_repos_gated(path, repos);
    if let Ok(next) = &result {
        on_written(next);
    }
    crate::context_repo::caller_scope::readback(readable, result)
}

/// Epic-gated `set_epic_repos_core`: reject a non-epic target *before* writing.
/// The MCP `set_epic_repos` tool routes through here so an automated agent can
/// never rewrite `repos:` on a plain doc — there is no human in the loop to
/// catch it. Reuses the canonical epic predicate
/// (`frontmatter_editor::detect_is_epic`) rather than inventing a second test.
/// The shipped UI command skips this gate: it only ever targets an epic node,
/// and gating it would change a shipped contract for no behavioral gain.
/// AppHandle-free so it is unit-testable against a tempfile.
pub(crate) fn set_epic_repos_gated(path: &Path, repos: &[String]) -> Result<Vec<String>, String> {
    let fields = crate::commands::content_engine::load_epic_frontmatter(&path.to_string_lossy())?;
    if !crate::commands::frontmatter_editor::detect_is_epic(path, &fields) {
        return Err(format!(
            "set_epic_repos: {} is not an epic doc — repos can only be set on an epic",
            path.display()
        ));
    }
    set_epic_repos_core(path, repos)
}

/// SW-EW-9 AC-2/6: read the epic's configured `repos:` (source of truth).
/// The epic-node card reads here on expand — never from app state.
#[tauri::command]
pub async fn read_epic_repos(
    app: tauri::AppHandle,
    epic_file_path: String,
) -> Result<Vec<String>, String> {
    crate::commands::await_blocking("read_epic_repos", move || {
        let path = validate_context_path_owned(&epic_file_path, &app)?;
        read_epic_repo_slugs(&path)
    })
    .await
}

/// SW-EW-9 AC-3/4/5/6: persist the epic's `repos:` and return the re-read list.
/// The path is sandboxed by [`validate_context_path_owned`] (normalizes away
/// `..` and rejects anything outside the context/users tree) — the proper
/// containment guard, since this command writes to disk.
#[tauri::command]
pub async fn set_epic_repos(
    app: tauri::AppHandle,
    epic_file_path: String,
    repos: Vec<String>,
) -> Result<Vec<String>, String> {
    crate::commands::await_blocking("set_epic_repos", move || {
        debug!(
            target: "pipeline_scanner",
            "set_epic_repos: file={}, repos={:?}",
            epic_file_path, repos
        );
        let path =
            crate::commands::files::validate_context_write_path_owned(&epic_file_path, &app)?;
        set_epic_repos_core(&path, &repos)
    })
    .await
}

#[tauri::command]
pub async fn find_nearest_valid_scope(
    app: tauri::AppHandle,
    team_content_path: String,
    pipeline_root: String,
    scope_path: String,
) -> Result<String, String> {
    crate::commands::await_blocking("find_nearest_valid_scope", move || {
        debug!(
            target: "pipeline_scanner",
            "find_nearest_valid_scope: content={}, root={}, scope={}",
            team_content_path, pipeline_root, scope_path
        );
        let repo = validate_context_path_owned(&team_content_path, &app)?;
        find_nearest_valid_scope_impl(&repo, &pipeline_root, &scope_path)
    })
    .await
}

// ----- Tests -----------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use tempfile::TempDir;

    /// Declarative fixture spec consumed by `build_fixture_tree`.
    pub struct FixtureDir {
        pub name: &'static str,
        pub stories: Vec<FixtureStory>,
        pub sub_dirs: Vec<FixtureDir>,
        pub pipeline_json: Option<&'static str>,
    }

    pub struct FixtureStory {
        pub file_name: &'static str,
        pub body: &'static str,
    }

    impl FixtureDir {
        pub fn new(name: &'static str) -> Self {
            Self {
                name,
                stories: vec![],
                sub_dirs: vec![],
                pipeline_json: None,
            }
        }
        pub fn story(mut self, file_name: &'static str, body: &'static str) -> Self {
            self.stories.push(FixtureStory { file_name, body });
            self
        }
        pub fn sub(mut self, dir: FixtureDir) -> Self {
            self.sub_dirs.push(dir);
            self
        }
        pub fn pipeline(mut self, json: &'static str) -> Self {
            self.pipeline_json = Some(json);
            self
        }
    }

    /// Populate `tempdir` with the given fixture rooted at the tempdir's root.
    pub fn build_fixture_tree(tempdir: &TempDir, root: &FixtureDir) {
        let root_path = tempdir.path().join(root.name);
        build_dir(&root_path, root);
    }

    fn build_dir(abs: &Path, spec: &FixtureDir) {
        fs::create_dir_all(abs).expect("create fixture dir");
        for s in &spec.stories {
            // PTS-2 follow-up: auto-inject `type: story` for .md fixtures
            // so legacy `.story("a.md", "# Body")` builders still round-trip
            // through the scanner's pipeline-type gate. HTML fixtures pass
            // through unchanged — they use F.30's cs:* gate, which the
            // injector skips (no `---` opening fence on HTML).
            let body: String = if s.file_name.ends_with(".md") {
                inject_type_story(s.body)
            } else {
                s.body.to_string()
            };
            fs::write(abs.join(s.file_name), body).expect("write story");
        }
        for child in &spec.sub_dirs {
            build_dir(&abs.join(child.name), child);
        }
        if let Some(json) = spec.pipeline_json {
            let meta = abs.join(".meta");
            fs::create_dir_all(&meta).expect("create .meta");
            fs::write(meta.join("pipeline.json"), json).expect("write pipeline.json");
        }
    }

    /// PTS-2 follow-up: shared with `tests::inject_type_story`. Lives in
    /// `test_helpers` so `build_dir` (here) and the `tests` module's
    /// `scan_one` / `write_story_md` can both use it. Kept `pub(super)`
    /// so the `tests` module imports via `use super::test_helpers::*`.
    ///
    /// See module-level docstring above `inject_type_story_inner` for
    /// the contract. This is a thin alias so callers don't reach across
    /// module boundaries with awkward `super::tests::...` paths.
    pub(super) fn inject_type_story(body: &str) -> String {
        let (bom, rest) = match body.strip_prefix('\u{feff}') {
            Some(r) => ("\u{feff}", r),
            None => ("", body),
        };
        let (eol, after_open) = if let Some(r) = rest.strip_prefix("---\n") {
            ("\n", r)
        } else if let Some(r) = rest.strip_prefix("---\r\n") {
            ("\r\n", r)
        } else {
            return format!("{}---\ntype: story\n---\n\n{}", bom, rest);
        };
        let close_marker = format!("{}---", eol);
        let close_at = if after_open.starts_with("---") {
            Some(0usize)
        } else {
            after_open.find(&close_marker).map(|p| p + eol.len())
        };
        let Some(close_at) = close_at else {
            return body.to_string();
        };
        let inner = &after_open[..close_at];
        if inner.lines().any(|l| l.trim_start().starts_with("type:")) {
            return body.to_string();
        }
        format!("{}---{eol}type: story{eol}{after_open}", bom)
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use tempfile::tempdir;

    /// An unconditional status (and optional rank) write.
    fn update_frontmatter_status(
        file_path: &Path,
        new_status: &str,
        new_rank: Option<&str>,
    ) -> Result<(), String> {
        write_story_status(file_path, new_status, new_rank, None).map(|_| ())
    }

    // ----- SW-EW-9: epic repo management (set/read cores) ----------------

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn write_epic(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("epic.md");
        fs::write(&p, body).unwrap();
        p
    }

    /// RF-perf-9 (AC-1/AC-2): `#[ignore]`d benchmark for the per-story `build_story_ref`
    /// clone (8 field clones, of which only `depends_on`/`supporting_docs` `Vec<String>`
    /// are non-trivial). Reports per-call ns + the frequency-weighted cost of an N-story
    /// scan (the scan pays this N times). Machine-dependent → `#[ignore]`d. Run:
    /// `cargo test -p codesense --lib rf_perf9_measure_build_story_ref -- --ignored --nocapture`.
    #[test]
    #[ignore = "manual story-reference construction benchmark; run explicitly with --ignored --nocapture"]
    fn rf_perf9_measure_build_story_ref() {
        use std::time::Instant;

        // A representative story: 2 deps + 2 supporting docs (the Vec<String> clones
        // that actually cost); the six Option<String> clones are cheap small strings.
        let fields = crate::commands::pipeline_format::FrontmatterFields {
            story_id: Some("RF-bench-42".into()),
            status: Some("todo".into()),
            source_repo: Some("alderco/codesense".into()),
            document_type: Some("story".into()),
            depends_on: Some(vec!["RF-bench-41".into(), "RF-bench-40".into()]),
            supporting_docs: Some(vec!["README.md".into(), "RF-perf-0.md".into()]),
            team: Some("default".into()),
            ..Default::default()
        };
        let path = std::path::PathBuf::from("/tmp/rf_perf9_bench/story_42.md");
        let meta = MetadataValidityConfig::default();
        let open_tags: std::collections::BTreeMap<String, Vec<String>> = Default::default();

        const REPS: u32 = 20_000;
        let before = crate::install::frontmatter_perf::COUNTERS.snapshot();
        let mut sink = 0usize;
        let t = Instant::now();
        for _ in 0..REPS {
            // `open_tags.clone()` mirrors the real caller (moves a per-story map in).
            let r = build_story_ref(
                "story_42.md".to_string(),
                &path,
                &fields,
                &meta,
                None,
                open_tags.clone(),
                Some(0),
            );
            sink += r.file_name.len();
        }
        let per = t.elapsed().as_nanos() as f64 / REPS as f64;
        let after = crate::install::frontmatter_perf::COUNTERS.snapshot();

        println!("=== RF-perf-9 build_story_ref (per story per scan) ===");
        println!(
            "build_story_ref = {per:.0} ns/call (2 depends_on + 2 supporting_docs); \
             tally delta={} (sink={sink})",
            after.build_story_ref_calls - before.build_story_ref_calls,
        );
        for &n in &[10usize, 50, 200] {
            println!(
                "  scan of N={n:3} stories → {:.1} µs of build_story_ref clone",
                per * n as f64 / 1000.0
            );
        }
    }

    /// RF-perf-11 (AC-1): the inert-when-off invariant, extended from `start_timer`
    /// to the per-scan phase timers. With no logger armed in the test binary the
    /// gate is off → `ScanStats` carries no phase accumulator → `timer()` never
    /// reads the clock and there is nothing to emit. Guards a normal prod build
    /// against paying for scan instrumentation it isn't collecting.
    #[test]
    fn rf_perf11_scanstats_untimed_when_gate_off() {
        assert!(
            !crate::install::frontmatter_perf::timing_enabled(),
            "gate must be off with no logger armed"
        );
        let stats = ScanStats::new();
        assert!(
            stats.phase.is_none(),
            "no phase accumulator constructed when the gate is off"
        );
        assert!(
            stats.timer().is_none(),
            "timer() must not read the clock when the gate is off"
        );
        assert!(
            stats.phase_totals().is_none(),
            "no phase totals to emit when the gate is off"
        );
    }

    /// RF-perf-11 (AC-2): the scaled scan benchmark. Measures the WHOLE
    /// `scan_pipeline_tree` at N ∈ {50, 200, 1000} stories, **cold** (all
    /// cache-miss — the first-nav path) and **warm** (all cache-hit — the steady
    /// per-save path), reporting the total latency the FE waits on plus an external
    /// per-phase derivation:
    ///   - `read_parse ≈ cold − warm` — the miss-arm's extra read + parse + mtime
    ///     (this is where the cache-miss double-read shows up if it ranks),
    ///   - `build ≈ build_story_ref/call × N` — measured inline (self-contained),
    ///   - `walk+stat ≈ warm − build` — `read_dir` enumeration + per-file freshness stat.
    ///
    /// The precise in-situ split is the gated per-scan `debug!` (AC-1); this bench is
    /// the offline scale readout, mirroring `rf_perf9_measure_build_story_ref`. It is
    /// wall-clock + load-flaky by nature — hence `#[ignore]` (never a CI gate).
    /// Run: `cargo test -p codesense --lib rf_perf11_measure_scan_at_scale -- --ignored --nocapture`
    #[test]
    #[ignore = "manual pipeline scan benchmark; run explicitly with --ignored --nocapture"]
    fn rf_perf11_measure_scan_at_scale() {
        use crate::install::frontmatter_cache::FrontmatterCache;
        use std::time::Instant;

        // Per-call build_story_ref cost (representative 2-dep / 2-doc story),
        // measured inline so the bench needs no companion run.
        let rep_fields = crate::commands::pipeline_format::FrontmatterFields {
            story_id: Some("RF-bench-42".into()),
            status: Some("todo".into()),
            source_repo: Some("alderco/codesense".into()),
            document_type: Some("story".into()),
            depends_on: Some(vec!["RF-bench-41".into(), "RF-bench-40".into()]),
            supporting_docs: Some(vec!["README.md".into(), "RF-perf-0.md".into()]),
            team: Some("default".into()),
            ..Default::default()
        };
        let rep_path = std::path::PathBuf::from("/tmp/rf_perf11_bench/story_42.md");
        let cfg = MetadataValidityConfig::default();
        const BUILD_REPS: u32 = 20_000;
        let mut sink = 0usize;
        let t = Instant::now();
        for _ in 0..BUILD_REPS {
            let r = build_story_ref(
                "story_42.md".to_string(),
                &rep_path,
                &rep_fields,
                &cfg,
                None,
                Default::default(),
                Some(0),
            );
            sink += r.file_name.len();
        }
        let build_per_ns = t.elapsed().as_nanos() as f64 / BUILD_REPS as f64;

        println!("=== RF-perf-11 scan_pipeline_tree at scale (build sink={sink}) ===");
        println!("build_story_ref = {build_per_ns:.0} ns/story (2 dep + 2 doc)");

        for &n in &[50usize, 200, 1000] {
            let (_td, repo) = fix_repo();
            fs::create_dir_all(repo.join("pipe")).unwrap();
            for i in 0..n {
                let body = format!(
                    "---\nstory_id: RF-bench-{i}\nstatus: todo\nsource_repo: alderco/codesense\ntype: story\ndepends_on: [RF-bench-{a}, RF-bench-{b}]\nsupporting_docs: [README.md, RF-perf-0.md]\nteam: default\n---\n# Story {i}\n\n## Story\n\nBody paragraph for story {i}.\n",
                    a = i.saturating_sub(1),
                    b = i.saturating_sub(2),
                );
                write_story_md(repo.join(format!("pipe/{i:04}-story.md")), &body);
            }

            // COLD: no cache → every story takes the inline read+parse arm.
            let t = Instant::now();
            let cold_tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
            let cold = t.elapsed();
            assert_eq!(cold_tree.stories.len(), n, "cold scan story count N={n}");

            // Warm cache over the same tree — keyed by absolute path, exactly what
            // `walk_dir` looks up (the production warm arm).
            let mut cache = FrontmatterCache::new();
            for i in 0..n {
                let path = repo.join(format!("pipe/{i:04}-story.md"));
                let (_, built) = cache
                    .commit_refresh(&path, FrontmatterCache::prepare_refresh(&path, "default"));
                assert!(built.is_some(), "cache build for story {i}");
            }

            // WARM: every story is a fresh cache hit → walk + build, zero read.
            let t = Instant::now();
            let warm_tree = scan_pipeline_tree_impl_with_cache(
                &repo,
                "pipe",
                "",
                "",
                &default_summary_headings(),
                Some(&cache),
                &MetadataValidityConfig::default(),
            )
            .unwrap();
            let warm = t.elapsed();
            assert_eq!(warm_tree.stories.len(), n, "warm scan story count N={n}");
            // Cold and warm must agree byte-for-byte (AC-5 invariant, spot-checked).
            assert_eq!(
                cold_tree.stories, warm_tree.stories,
                "cold/warm StoryRef parity N={n}"
            );

            let cold_us = cold.as_micros() as f64;
            let warm_us = warm.as_micros() as f64;
            let read_parse_us = (cold_us - warm_us).max(0.0);
            let build_us = build_per_ns * n as f64 / 1000.0;
            let walk_stat_us = (warm_us - build_us).max(0.0);
            println!("--- N={n} ---");
            println!(
                "  cold total   = {cold_us:9.1} us  ({:6.2} us/story)  [first nav / all miss]",
                cold_us / n as f64
            );
            println!(
                "  warm total   = {warm_us:9.1} us  ({:6.2} us/story)  [steady / all hit]",
                warm_us / n as f64
            );
            println!(
                "  ├ read_parse ≈ {read_parse_us:9.1} us  (cold − warm; miss-arm read+parse+mtime)"
            );
            println!("  ├ build      ≈ {build_us:9.1} us  (build_story_ref × N)");
            println!(
                "  └ walk+stat  ≈ {walk_stat_us:9.1} us  (warm − build; read_dir + freshness stat)"
            );
        }
    }

    #[test]
    fn set_epic_repos_core_adds_to_empty_doc() {
        let td = tempdir().unwrap();
        let p = write_epic(td.path(), "---\ntype: epic\nstatus: draft\n---\n# Epic\n");
        let out = set_epic_repos_core(&p, &s(&["a/one", "b/two"])).unwrap();
        assert_eq!(out, s(&["a/one", "b/two"]));
        // Re-reading independently agrees with the returned list.
        assert_eq!(read_epic_repo_slugs(&p).unwrap(), s(&["a/one", "b/two"]));
        let disk = fs::read_to_string(&p).unwrap();
        assert!(disk.contains("repos:\n  - a/one\n  - b/two"));
        assert!(disk.contains("status: draft"), "other fields preserved");
    }

    // ----- SW-SR-9: epic-gated writer (the MCP-tool path) ---------------

    #[test]
    fn set_epic_repos_gated_writes_on_epic_doc() {
        let td = tempdir().unwrap();
        let p = write_epic(td.path(), "---\ntype: epic\nstatus: draft\n---\n# Epic\n");
        let out = set_epic_repos_gated(&p, &s(&["a/one", "b/two"])).unwrap();
        assert_eq!(out, s(&["a/one", "b/two"]));
        assert_eq!(read_epic_repo_slugs(&p).unwrap(), s(&["a/one", "b/two"]));
    }

    #[test]
    fn set_epic_repos_gated_rejects_non_epic_doc() {
        let td = tempdir().unwrap();
        // A plain story doc: not README/index-named, no `type: epic`, no epic
        // fields → the gate must refuse and leave the doc untouched.
        let p = write_epic(td.path(), "---\ntype: story\nstatus: draft\n---\n# Story\n");
        let before = fs::read_to_string(&p).unwrap();
        let err = set_epic_repos_gated(&p, &s(&["a/one"])).unwrap_err();
        assert!(err.contains("not an epic doc"), "got: {err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), before, "doc unchanged");
    }

    #[test]
    fn set_epic_repos_core_removes_on_empty() {
        let td = tempdir().unwrap();
        let p = write_epic(
            td.path(),
            "---\ntype: epic\nrepos:\n  - a/one\nstatus: draft\n---\n# Epic\n",
        );
        let out = set_epic_repos_core(&p, &[]).unwrap();
        assert!(out.is_empty());
        let disk = fs::read_to_string(&p).unwrap();
        assert!(!disk.contains("repos:"), "field removed");
        assert!(disk.contains("status: draft"));
    }

    #[test]
    fn set_epic_repos_core_dedups_and_normalizes() {
        let td = tempdir().unwrap();
        let p = write_epic(td.path(), "---\ntype: epic\n---\n# Epic\n");
        let out = set_epic_repos_core(&p, &s(&["a/one", "a/one", "b/two"])).unwrap();
        assert_eq!(out, s(&["a/one", "b/two"]), "duplicate dropped, order kept");
    }

    #[test]
    fn set_epic_repos_core_rejects_invalid_slug() {
        let td = tempdir().unwrap();
        let p = write_epic(td.path(), "---\ntype: epic\n---\n# Epic\n");
        let err = set_epic_repos_core(&p, &s(&["not-a-slug"])).unwrap_err();
        assert!(err.contains("invalid repo slug"), "got: {err}");
        // The doc is untouched on validation failure.
        assert!(!fs::read_to_string(&p).unwrap().contains("repos:"));
    }

    #[test]
    fn set_epic_repos_core_rejects_non_markdown() {
        let td = tempdir().unwrap();
        let p = td.path().join("epic.html");
        fs::write(&p, "<html></html>").unwrap();
        let err = set_epic_repos_core(&p, &s(&["a/one"])).unwrap_err();
        assert!(err.contains("must be markdown"), "got: {err}");
    }

    #[test]
    fn read_epic_repos_empty_when_absent() {
        let td = tempdir().unwrap();
        let p = write_epic(td.path(), "---\ntype: epic\n---\n# Epic\n");
        assert!(read_epic_repo_slugs(&p).unwrap().is_empty());
    }

    // ----- Round-2 R2: sanitize helpers (TS-parser symmetry) -------------

    #[test]
    fn sanitize_optional_string_none_passthrough() {
        assert_eq!(sanitize_optional_string(None), None);
    }

    #[test]
    fn sanitize_optional_string_empty_to_none() {
        assert_eq!(sanitize_optional_string(Some(String::new())), None);
    }

    #[test]
    fn sanitize_optional_string_whitespace_to_none() {
        assert_eq!(sanitize_optional_string(Some("   ".to_string())), None);
    }

    #[test]
    fn sanitize_optional_string_trims_surrounding_whitespace() {
        assert_eq!(
            sanitize_optional_string(Some("  alderco/codesense  ".to_string())),
            Some("alderco/codesense".to_string())
        );
    }

    #[test]
    fn sanitize_story_id_rejects_null_literal_any_case() {
        for literal in &["null", "Null", "NULL", "  null  "] {
            assert_eq!(
                sanitize_story_id(Some(literal.to_string())),
                None,
                "rejected: {literal:?}"
            );
        }
    }

    #[test]
    fn sanitize_story_id_rejects_tilde_literal() {
        assert_eq!(sanitize_story_id(Some("~".to_string())), None);
    }

    #[test]
    fn sanitize_story_id_rejects_empty_and_whitespace() {
        assert_eq!(sanitize_story_id(Some(String::new())), None);
        assert_eq!(sanitize_story_id(Some("   ".to_string())), None);
    }

    #[test]
    fn sanitize_story_id_passes_real_values_through_with_trim() {
        assert_eq!(
            sanitize_story_id(Some("  R-18  ".to_string())),
            Some("R-18".to_string())
        );
    }

    // ----- Normalization ------------------------------------------------

    const VALID_CASES: &[(&str, &str)] = &[
        ("", ""),
        ("implementation-artifacts", "implementation-artifacts"),
        ("  implementation-artifacts  ", "implementation-artifacts"),
        ("/implementation-artifacts", "implementation-artifacts"),
        ("implementation-artifacts/", "implementation-artifacts"),
        ("/implementation-artifacts/", "implementation-artifacts"),
        ("//implementation-artifacts//", "implementation-artifacts"),
        ("\\implementation-artifacts\\", "implementation-artifacts"),
        (
            "in-progress/30-pipeline-viewer-ui",
            "in-progress/30-pipeline-viewer-ui",
        ),
        (
            "/in-progress/30-pipeline-viewer-ui/",
            "in-progress/30-pipeline-viewer-ui",
        ),
        ("   ", ""),
        ("/", ""),
    ];

    const INVALID_CASES: &[&str] = &["..", "../foo", "foo/..", "foo/../bar", "./../baz"];

    #[test]
    fn normalize_pipeline_root_valid() {
        for (input, expected) in VALID_CASES {
            let result = normalize_pipeline_root(input).expect("should not error");
            assert_eq!(&result, expected, "input: {:?}", input);
        }
    }

    #[test]
    fn normalize_pipeline_root_invalid() {
        for input in INVALID_CASES {
            assert!(
                normalize_pipeline_root(input).is_err(),
                "expected error for: {:?}",
                input
            );
        }
    }

    #[test]
    fn normalize_pipeline_root_error_message_mentions_field() {
        let err = normalize_pipeline_root("foo/../bar").unwrap_err();
        assert!(err.contains("pipelineRoot"));
        assert!(err.contains("foo/../bar"));
    }

    #[test]
    fn normalize_scope_path_valid() {
        for (input, expected) in VALID_CASES {
            let result = normalize_scope_path(input).expect("should not error");
            assert_eq!(&result, expected, "input: {:?}", input);
        }
    }

    #[test]
    fn normalize_scope_path_invalid() {
        for input in INVALID_CASES {
            assert!(normalize_scope_path(input).is_err());
        }
    }

    #[test]
    fn normalize_scope_path_error_message_mentions_field() {
        let err = normalize_scope_path("foo/..").unwrap_err();
        assert!(err.contains("scopePath"));
    }

    // ----- R-14: cache wire-up tests ------------------------------------

    /// AC #37: cache-backed and inline-parse paths produce identical
    /// `StoryRef`s for the same on-disk fixture. The two paths must be
    /// structurally equivalent — any divergence here is a bug.
    ///
    #[test]
    fn cache_backed_and_inline_parse_produce_identical_story_refs() {
        use crate::install::frontmatter_cache::FrontmatterCache;

        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // PTS-2: orphan-skip requires frontmatter source_repo on every entry.
        fs::write(
            repo.join("pipe/1-alpha.md"),
            "---\nstatus: todo\nstory_id: A-1\ndepends_on: [foo]\nsource_repo: org/repo\n---\n# Alpha\n\n## Story\n\nA body.\n",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/2-beta.md"),
            "---\nstatus: in-progress\ntype: bug\nteam: explicit\nsource_repo: org/repo\n---\n# Beta\n",
        )
        .unwrap();

        // Build cache FIRST so any disk mutations (none expected with these
        // fixtures, but defensive) settle before either scan runs.
        let mut cache = FrontmatterCache::new();
        for name in ["1-alpha.md", "2-beta.md"] {
            let path = repo.join("pipe").join(name);
            let (_, new) =
                cache.commit_refresh(&path, FrontmatterCache::prepare_refresh(&path, "default"));
            assert!(new.is_some(), "cache build should succeed for {name}");
        }

        // Path A: no cache → inline-parse for every entry.
        let tree_inline = scan_pipeline_tree_impl(&repo, "pipe", "", "").expect("inline scan");

        // Path B: same scan with cache.
        let tree_cached = scan_pipeline_tree_impl_with_cache(
            &repo,
            "pipe",
            "",
            "",
            &default_summary_headings(),
            Some(&cache),
            &MetadataValidityConfig::default(),
        )
        .expect("cache-backed scan");

        assert_eq!(tree_inline.stories, tree_cached.stories);
        assert_eq!(tree_inline.sub_epics, tree_cached.sub_epics);
    }

    /// PTS-2 follow-up: when the team's content root doesn't exist on disk
    /// (a team with no migration content + no captures yet), the scanner
    /// must return an empty tree rather than an `Err`. The frontend uses
    /// the empty-tree shape to render an empty-state view; an `Err` would
    /// surface a "Scope path not found" message even though the team is
    /// simply unpopulated.
    ///
    /// The Err path is preserved for the in-content-but-scope-missing case
    /// (asserted by the existing `scope_path_not_found_returns_err` test).
    #[test]
    fn missing_team_content_dir_returns_empty_tree() {
        let td = tempfile::tempdir().unwrap();
        // Point at a non-existent content dir — never created on disk.
        let absent_content_root = td.path().join("never-created/content");

        let tree = scan_pipeline_tree_impl(&absent_content_root, "", "", "")
            .expect("missing team content dir should yield empty tree, not Err");

        assert_eq!(tree.stories.len(), 0);
        assert_eq!(tree.sub_epics.len(), 0);
        assert!(tree.main_doc.is_none());
        assert!(tree.order_error.is_none());
    }

    /// AC #38: a cache that's missing one of N entries still produces N
    /// stories — the missing one is served via inline-parse fallback.
    #[test]
    fn cache_miss_falls_back_to_inline_parse() {
        use crate::install::frontmatter_cache::FrontmatterCache;

        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(
            repo.join("pipe/1-cached.md"),
            "---\nstatus: todo\n---\n# Cached\n",
        );
        write_story_md(
            repo.join("pipe/2-uncached.md"),
            "---\nstatus: review\n---\n# Uncached\n",
        );

        // Cache contains only the first file.
        let mut cache = FrontmatterCache::new();
        let cached_path = repo.join("pipe/1-cached.md");
        let (_, _) = cache.commit_refresh(
            &cached_path,
            FrontmatterCache::prepare_refresh(&cached_path, "default"),
        );

        let tree = scan_pipeline_tree_impl_with_cache(
            &repo,
            "pipe",
            "",
            "",
            &default_summary_headings(),
            Some(&cache),
            &MetadataValidityConfig::default(),
        )
        .expect("scan");

        assert_eq!(tree.stories.len(), 2, "both stories should be present");
        let names: Vec<&str> = tree.stories.iter().map(|s| s.file_name.as_str()).collect();
        assert!(names.contains(&"1-cached.md"));
        assert!(names.contains(&"2-uncached.md"));

        // Status values should match the on-disk frontmatter regardless of
        // which path served them.
        let cached_entry = tree
            .stories
            .iter()
            .find(|s| s.file_name == "1-cached.md")
            .unwrap();
        let uncached_entry = tree
            .stories
            .iter()
            .find(|s| s.file_name == "2-uncached.md")
            .unwrap();
        assert_eq!(cached_entry.status.as_deref(), Some("todo"));
        assert_eq!(uncached_entry.status.as_deref(), Some("review"));
    }

    /// SW-LN-B50 (AC-1): a story the index skipped (read before its frontmatter
    /// landed) is reported by the next scan as an index gap, and re-indexing the
    /// gaps makes it resolvable by id with no further change to the file. A story
    /// with no `source_repo` (which the index never keys) is not a gap.
    #[test]
    fn scan_reports_index_gaps_and_reindex_makes_them_resolvable() {
        use crate::install::frontmatter_cache::FrontmatterCache;

        let (_td, root) = fix_repo();
        let content = root.join("general").join("content");
        let epic = content.join("two-repo-epic");
        fs::create_dir_all(&epic).unwrap();
        let tr1 = epic.join("TR-1-api-health-endpoint.md");
        // The index's first read caught the file with no frontmatter.
        fs::write(&tr1, "").unwrap();
        let shared = FrontmatterCache::shared(FrontmatterCache::new());
        FrontmatterCache::refresh_under_lock(&shared, &tr1, "general");
        // The write completes; no further event reaches the index.
        fs::write(
            &tr1,
            "---\ntype: story\nstory_id: TR-1\nstatus: ready\nsource_repo: local/fixture-api\n---\n# TR-1\n",
        )
        .unwrap();
        fs::write(
            epic.join("TR-9-orphan.md"),
            "---\ntype: story\nstory_id: TR-9\nstatus: ready\n---\n# TR-9\n",
        )
        .unwrap();

        let snap = FrontmatterCache::snapshot(&shared);
        assert!(
            crate::commands::dashboard_launch::resolve_story_path_in(&snap, "TR-1").is_err(),
            "the mid-write read left TR-1 out of the index"
        );
        let (_tree, gaps) = scan_pipeline_tree_with_index_gaps(
            &content,
            "",
            "",
            "",
            &default_summary_headings(),
            Some(&snap),
            &MetadataValidityConfig::default(),
        )
        .expect("scan");
        drop(snap);
        assert_eq!(gaps, vec![tr1.clone()], "only the indexable story is a gap");

        // The scan command's own body heals the index — no further file change.
        scan_pipeline_tree_and_heal_index(
            Some(&shared),
            &content,
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        )
        .expect("scan");
        let snap = FrontmatterCache::snapshot(&shared);
        let resolved = crate::commands::dashboard_launch::resolve_story_path_in(&snap, "TR-1")
            .expect("TR-1 resolves after the scan heals the index");
        assert_eq!(resolved.team, "general");
    }

    // ----- SW-FX-9: disk-truth freshness gate ----------------------------

    /// Build a cache containing exactly one entry for `path`, parsed from the
    /// file currently on disk. Returns the cache so the caller can mutate the
    /// entry (status/mtime) to simulate a stale index. Drives `walk_dir`
    /// directly so tests own the `&ScanStats` reference (mirrors
    /// `scan_warn_emits_missed_paths`).
    fn cache_with_entry(path: &Path) -> crate::install::frontmatter_cache::FrontmatterCache {
        use crate::install::frontmatter_cache::FrontmatterCache;
        let mut cache = FrontmatterCache::new();
        let (_, _) = cache.commit_refresh(path, FrontmatterCache::prepare_refresh(path, "default"));
        assert!(
            cache.entries.contains_key(path),
            "fixture must carry frontmatter source_repo, else build_cache_entry \
             orphan-skips it and the cache is empty (every path takes the miss branch)"
        );
        cache
    }

    /// AC-5: a status change written to disk is reflected by the next scan
    /// even when the cache entry still holds the old status (older mtime) —
    /// no dependence on watcher timing. The stale entry must not shadow disk.
    #[test]
    fn stale_status_entry_reparses_from_disk() {
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        let path = pipe.join("1-story.md");
        // Disk says done.
        write_story_md(
            &path,
            "---\nsource_repo: org/repo\nstatus: done\n---\n# S\n",
        );

        // Cache holds the OLD status (todo) with an mtime far in the past —
        // the shape left by a watcher-suppressed write or a status-command
        // → async-watcher race.
        let mut cache = cache_with_entry(&path);
        let entry = cache.entries.get_mut(&path).expect("entry present");
        entry.fields.status = Some("todo".into());
        entry.mtime = 0;

        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        let story = tree
            .stories
            .iter()
            .find(|s| s.file_name == "1-story.md")
            .expect("story present");
        assert_eq!(
            story.status.as_deref(),
            Some("done"),
            "scan must reflect disk status, not the stale cached status"
        );
        assert_eq!(stats.stale(), 1, "the stale entry should be counted");
        assert_eq!(
            stats.misses(),
            0,
            "a stale re-parse must not double-count as a cache-index miss"
        );
    }

    #[test]
    fn fresh_cache_entry_is_served_as_a_hit() {
        // The companion to `stale_status_entry_reparses_from_disk`, inverted: a
        // FRESH entry (mtime matches disk) must be served FROM CACHE, not
        // re-parsed. This is the warm arm every SR-18/SR-12/SW-AU-101 warm caller
        // relies on. Prove it positively — divert the cached status so it differs
        // from disk while leaving the entry fresh; a genuine hit then surfaces the
        // CACHED value and increments `stats.hits()`. (A plain warm==cold check
        // can't distinguish a hit from a silent fall-through — both arms share the
        // same story-ref sink.)
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        let path = pipe.join("1-story.md");
        // Disk says done.
        write_story_md(
            &path,
            "---\nsource_repo: org/repo\nstatus: done\n---\n# S\n",
        );

        // Fresh entry (mtime == disk), but the cached status is diverted to todo.
        // mtime is left untouched, so the freshness gate takes the HIT arm.
        let mut cache = cache_with_entry(&path);
        let entry = cache.entries.get_mut(&path).expect("entry present");
        entry.fields.status = Some("todo".into());

        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        let story = tree
            .stories
            .iter()
            .find(|s| s.file_name == "1-story.md")
            .expect("story present");
        assert_eq!(
            story.status.as_deref(),
            Some("todo"),
            "a fresh entry must be served from cache (the diverted value), not re-parsed from disk"
        );
        assert_eq!(stats.hits(), 1, "the fresh entry must count as a cache hit");
        assert_eq!(
            stats.stale(),
            0,
            "a fresh entry must not be treated as stale"
        );
        assert_eq!(stats.misses(), 0, "a fresh hit is not a cache-index miss");
    }

    /// AC-4 (SW-FX-8 repro): a `supporting_docs` ref present on disk but
    /// absent from a stale cache entry surfaces on the next scan, with no
    /// app reload — the gate re-parses because disk is newer.
    #[test]
    fn stale_supporting_docs_entry_reparses_from_disk() {
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        let path = pipe.join("1-story.md");
        // Disk carries a supporting-docs ref (the SW-FX-8 shape).
        write_story_md(
            &path,
            "---\nsource_repo: org/repo\nstatus: todo\nsupporting_docs:\n  - diagram.excalidraw\n---\n# S\n",
        );

        // Cache predates the supporting_docs addition: field absent, old mtime.
        let mut cache = cache_with_entry(&path);
        let entry = cache.entries.get_mut(&path).expect("entry present");
        entry.fields.supporting_docs = None;
        entry.mtime = 0;

        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        let story = tree
            .stories
            .iter()
            .find(|s| s.file_name == "1-story.md")
            .expect("story present");
        assert!(
            story
                .supporting_docs
                .as_ref()
                .is_some_and(|d| d.iter().any(|s| s == "diagram.excalidraw")),
            "supporting_docs must surface from disk on a stale re-parse, got {:?}",
            story.supporting_docs
        );
        assert_eq!(stats.stale(), 1);
    }

    /// AC-6: an unchanged file (cache mtime == disk mtime) is served from the
    /// cache and is NOT re-parsed. Proven by stamping the cached fields with a
    /// sentinel status that does not exist on disk: a disk re-parse would
    /// yield "todo", so seeing the sentinel proves the cache branch ran.
    #[test]
    fn fresh_entry_served_from_cache_not_disk() {
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        let path = pipe.join("1-story.md");
        write_story_md(
            &path,
            "---\nsource_repo: org/repo\nstatus: todo\n---\n# S\n",
        );

        // Mutate ONLY the cached fields (the file is untouched, so its mtime
        // stays equal to the entry's → fresh). The sentinel never exists on
        // disk, so serving it can only mean the cache branch was taken.
        let mut cache = cache_with_entry(&path);
        let entry = cache.entries.get_mut(&path).expect("entry present");
        entry.fields.status = Some("cache-sentinel".into());

        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        let story = tree
            .stories
            .iter()
            .find(|s| s.file_name == "1-story.md")
            .expect("story present");
        assert_eq!(
            story.status.as_deref(),
            Some("cache-sentinel"),
            "a fresh hit must be served from cache, not re-parsed from disk"
        );
        assert_eq!(stats.stale(), 0, "a fresh entry is not stale");
        assert_eq!(stats.misses(), 0, "a fresh entry is not a miss");
    }

    // ----- SF-3 (Bug C): Named cache-miss warn ---------------------------

    /// AC-16: when the scanner walks N pipeline-doc files with no cache,
    /// `ScanStats.missed_paths_snapshot()` returns all N paths. Drives
    /// `walk_dir` directly so the test owns the `&ScanStats` reference.
    #[test]
    fn scan_warn_emits_missed_paths() {
        use std::collections::HashSet;

        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        let p1 = pipe.join("1-alpha.md");
        let p2 = pipe.join("2-beta.md");
        let p3 = pipe.join("3-gamma.md");
        write_story_md(&p1, "---\nstatus: todo\n---\n# Alpha\n");
        write_story_md(&p2, "---\nstatus: review\n---\n# Beta\n");
        write_story_md(&p3, "---\nstatus: done\n---\n# Gamma\n");

        // SF-3 AC-14: bump_miss only fires for files that pass
        // `extract_story_meta` — so we need `cache: Some(&empty)` (not `None`,
        // which short-circuits the counter even on pipeline docs) to exercise
        // the named-path path.
        let cache = crate::install::frontmatter_cache::FrontmatterCache::new();
        let stats = ScanStats::new();
        let _tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        assert_eq!(stats.misses(), 3, "all 3 pipeline files should be misses");
        let snapshot = stats.missed_paths_snapshot();
        assert_eq!(snapshot.len(), 3, "all 3 missed paths should be buffered");
        let snapshot_set: HashSet<PathBuf> = snapshot.into_iter().collect();
        let expected: HashSet<PathBuf> = [p1, p2, p3].into_iter().collect();
        assert_eq!(snapshot_set, expected, "snapshot must match fixture set");
    }

    /// AC-17: `ScanStats` caps the buffered path list at `MAX_LOGGED_MISSES`
    /// while `misses()` reflects the true count. Direct unit test on the
    /// struct — no scanner fixture needed.
    #[test]
    fn scan_stats_cap_drops_beyond_max() {
        let stats = ScanStats::new();
        // SF-3 review patch: bound the loop in terms of the constant so the
        // assertions stay coherent if MAX_LOGGED_MISSES is ever changed.
        let total = MAX_LOGGED_MISSES + 5;
        for i in 0..total {
            stats.bump_miss(&PathBuf::from(format!("/p/{i}.md")));
        }
        assert_eq!(stats.misses(), total, "full count preserved past cap");
        assert_eq!(
            stats.missed_paths_snapshot().len(),
            MAX_LOGGED_MISSES,
            "buffered path list capped at MAX_LOGGED_MISSES"
        );
    }

    /// SF-3 AC-14: `bump_miss` is gated by `extract_story_meta` succeeding.
    /// A non-pipeline file (e.g. plain HTML or markdown without `cs:*` meta
    /// where required) that lives among story candidates must NOT count as
    /// a miss — the named-paths list is meant to be strictly actionable.
    #[test]
    fn scan_warn_excludes_non_pipeline_files() {
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        // Pipeline doc (extract_story_meta returns Some for .md).
        write_story_md(pipe.join("1-real.md"), "---\nstatus: todo\n---\n# Real\n");
        // HTML without `cs:*` metadata. Post-SW-FX-39 it routes to docs[]
        // (no longer dropped), but it still must NOT count as a cache miss:
        // its empty fields make `is_pipeline_story()` false, so the
        // `bump_miss` gate (SW-FX-12) skips it — same as a doc `.md`.
        fs::write(
            pipe.join("2-junk.html"),
            "<html><body>no metadata</body></html>",
        )
        .unwrap();

        let cache = crate::install::frontmatter_cache::FrontmatterCache::new();
        let stats = ScanStats::new();
        let _tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        assert_eq!(stats.misses(), 1, "only the pipeline-doc miss should count");
        assert_eq!(stats.missed_paths_snapshot().len(), 1);
    }

    // ----- SW-FX-12: miss-count scoped to genuine pipeline stories -----------

    /// AC-3 / AC-4 / AC-6: with an empty cache (so every candidate takes the
    /// miss path), non-story files (`type: doc`, no `type:`) route to `docs[]`
    /// and record **zero** misses, while a genuine `type: story` absent from
    /// the cache routes to `stories[]` and **does** count as a miss. The
    /// `cache miss(es) inline-parsed` WARN therefore fires only for real
    /// cache-index gaps, not for expected non-cached content. Raw `fs::write` is used for
    /// the non-story fixtures because `write_story_md` injects `type: story`.
    #[test]
    fn scan_miss_count_excludes_non_story_content() {
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        // Non-story content — expected to be uncached, must NOT count as misses.
        fs::write(
            pipe.join("1-typed-doc.md"),
            "---\ntype: doc\nstatus: ready\n---\n# Spec\n",
        )
        .unwrap();
        fs::write(
            pipe.join("2-no-type.md"),
            "---\nstatus: todo\n---\n# Notes\n",
        )
        .unwrap();
        // Genuine pipeline story, absent from the (empty) cache → a real gap.
        fs::write(
            pipe.join("3-story.md"),
            "---\ntype: story\nstatus: todo\n---\n# Feature\n",
        )
        .unwrap();

        // `Some(&empty)` (not `None`, which short-circuits the counter) so the
        // miss-count gate is actually exercised.
        let cache = crate::install::frontmatter_cache::FrontmatterCache::new();
        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        // AC-4: routing is unchanged — docs to docs[], story to stories[].
        let doc_names: Vec<&str> = tree.docs.iter().map(|d| d.file_name.as_str()).collect();
        assert!(
            doc_names.contains(&"1-typed-doc.md"),
            "type: doc routes to docs[], got {doc_names:?}"
        );
        assert!(
            doc_names.contains(&"2-no-type.md"),
            "no-type file routes to docs[], got {doc_names:?}"
        );
        let story_names: Vec<&str> = tree.stories.iter().map(|s| s.file_name.as_str()).collect();
        assert_eq!(
            story_names,
            vec!["3-story.md"],
            "only the story routes to stories[]"
        );

        // AC-3 / AC-6: only the genuine story counts as a cache-index miss.
        assert_eq!(
            stats.misses(),
            1,
            "only the pipeline-story gap is a miss, not the 2 docs"
        );
        let snapshot = stats.missed_paths_snapshot();
        assert_eq!(snapshot.len(), 1, "exactly one missed path buffered");
        assert!(
            snapshot[0].ends_with("3-story.md"),
            "the buffered miss must be the story, not a doc: {snapshot:?}",
        );
    }

    // ----- R-14: extract_body / get_story_details ------------------------

    /// AC #40: Body extraction returns title + story_text for typical files.
    #[test]
    fn extract_body_extracts_title_and_story_section() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(
            &path,
            "---\nstatus: todo\n---\n# H1 Title\n\n## Story\n\nAs a user, I want X.\n\n## Acceptance\n",
        )
        .unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(body.title.as_deref(), Some("H1 Title"));
        assert_eq!(body.story_text.as_deref(), Some("As a user, I want X."));
    }

    /// SW-LN-34: a quoted frontmatter title keeps a `: ` inside it, and wins
    /// alongside (not instead of) the body H1 — precedence is the card's job.
    #[test]
    fn extract_body_reads_quoted_frontmatter_title_with_colon() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(
            &path,
            "---\ntitle: \"Story details: read the title\"\nstory_id: SW-LN-34\n---\n# Body H1\n",
        )
        .unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(
            body.title_meta.as_deref(),
            Some("Story details: read the title")
        );
        assert_eq!(body.title.as_deref(), Some("Body H1"));
    }

    #[test]
    fn extract_body_reads_bare_frontmatter_title() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(&path, "---\nstatus: todo\ntitle:  Bare title \n---\nbody\n").unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(body.title_meta.as_deref(), Some("Bare title"));
        assert!(body.title.is_none());
    }

    #[test]
    fn extract_body_reads_html_cs_title_meta() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.html");
        fs::write(
            &path,
            "<html><head><meta name=\"cs:status\" content=\"todo\">\n<meta name=\"cs:title\" content=\"Refunds &amp; retries: keys\">\n<title>Doc title</title></head><body></body></html>",
        )
        .unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(body.title_meta.as_deref(), Some("Refunds & retries: keys"));
        assert_eq!(body.title.as_deref(), Some("Doc title"));
    }

    #[test]
    fn extract_body_without_meta_title_is_none() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let md = repo.join("pipe/a.md");
        fs::write(&md, "---\nstatus: todo\ntitle: \"  \"\n---\n# H1\n").unwrap();
        assert!(extract_body(&md, &default_summary_headings())
            .title_meta
            .is_none());
        let html = repo.join("pipe/b.html");
        fs::write(
            &html,
            "<meta name=\"cs:status\" content=\"todo\"><title>T</title>",
        )
        .unwrap();
        assert!(extract_body(&html, &default_summary_headings())
            .title_meta
            .is_none());
        let plain = repo.join("pipe/c.md");
        fs::write(&plain, "# Only an H1\n").unwrap();
        assert!(extract_body(&plain, &default_summary_headings())
            .title_meta
            .is_none());
    }

    /// AC #40: No H1 → title is None.
    #[test]
    fn extract_body_no_h1_returns_none_title() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(&path, "---\nstatus: todo\n---\n\nbody only no heading\n").unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert!(body.title.is_none());
        assert!(body.story_text.is_none());
    }

    /// AC #40: No `## Story` → story_text is None.
    #[test]
    fn extract_body_no_story_section_returns_none_story_text() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(&path, "# Title\n\n## Acceptance\n\nNo story heading.\n").unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(body.title.as_deref(), Some("Title"));
        assert!(body.story_text.is_none());
    }

    /// AC #40: unreadable file returns empty body (defensive).
    #[test]
    fn extract_body_missing_file_returns_default() {
        let (_td, repo) = fix_repo();
        let body = extract_body(
            &repo.join("pipe/does-not-exist.md"),
            &default_summary_headings(),
        );
        assert!(body.title.is_none());
        assert!(body.story_text.is_none());
    }

    // ----- SW-EW-32: configurable summary headings -----------------------

    /// First match wins: a doc with both `## Purpose` and `## Story` resolves
    /// to `## Purpose` under the default list (AC-2).
    #[test]
    fn extract_body_first_match_wins_over_ordered_list() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(
            &path,
            "# Title\n\n## Purpose\n\nThe purpose.\n\n## Story\n\nThe story.\n",
        )
        .unwrap();
        let body = extract_body(&path, &default_summary_headings());
        assert_eq!(body.story_text.as_deref(), Some("The purpose."));
    }

    /// A configured non-default heading resolves where the default list finds
    /// nothing — the core of the story (AC-2/AC-4).
    #[test]
    fn extract_body_resolves_configured_non_default_heading() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(&path, "# Title\n\n## Overview\n\nThe overview body.\n").unwrap();
        // Default list misses (no Purpose/Story).
        assert!(extract_body(&path, &default_summary_headings())
            .story_text
            .is_none());
        // Configured list hits.
        let custom = vec!["Overview".to_string()];
        assert_eq!(
            extract_body(&path, &custom).story_text.as_deref(),
            Some("The overview body."),
        );
    }

    /// `effective_headings` trims blanks and falls back to the defaults so a
    /// malformed/empty FE payload can never blank every doc (Risks/notes guard).
    #[test]
    fn effective_headings_falls_back_to_defaults_when_empty() {
        assert_eq!(effective_headings(vec![]), default_summary_headings());
        assert_eq!(
            effective_headings(vec!["  ".to_string(), String::new()]),
            default_summary_headings(),
        );
        assert_eq!(
            effective_headings(vec![" Purpose ".to_string(), "Story".to_string()]),
            vec!["Purpose".to_string(), "Story".to_string()],
        );
    }

    /// The epic-banner path (`build_main_doc` via the scan) keys off the
    /// configured list. This is the SW-EW-28 origin scenario: a README leading
    /// with `## What this sub-epic is` is empty under the default list but
    /// resolves once that heading is configured (AC-2/AC-4).
    #[test]
    fn scan_main_doc_purpose_honors_configured_headings() {
        let (_td, repo) = fix_repo();
        let epic = repo.join("pipe/epic-x");
        fs::create_dir_all(&epic).unwrap();
        fs::write(
            epic.join("README.md"),
            "# Epic X\n\n## What this sub-epic is\n\nThe epic summary.\n",
        )
        .unwrap();

        // Default list: the banner is empty (no Purpose/Story).
        let default_tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let default_epic = &default_tree.sub_epics[0];
        assert_eq!(default_epic.dir_name, "epic-x");
        assert!(default_epic
            .main_doc
            .as_ref()
            .unwrap()
            .purpose_text
            .is_none());

        // Configured list: the banner resolves.
        let custom = vec!["What this sub-epic is".to_string()];
        let custom_tree = scan_pipeline_tree_impl_with_cache(
            &repo,
            "pipe",
            "",
            "",
            &custom,
            None,
            &MetadataValidityConfig::default(),
        )
        .unwrap();
        assert_eq!(
            custom_tree.sub_epics[0]
                .main_doc
                .as_ref()
                .unwrap()
                .purpose_text
                .as_deref(),
            Some("The epic summary."),
        );
    }

    // ----- Helpers ------------------------------------------------------

    fn fix_repo() -> (tempfile::TempDir, PathBuf) {
        let td = tempdir().expect("tempdir");
        let repo = td.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        (td, repo)
    }

    #[test]
    fn recovered_stories_keep_dependencies_and_precise_badges_on_cold_and_warm_scans() {
        let (_td, repo) = fix_repo();
        let dir = repo.join("pipe");
        fs::create_dir_all(&dir).unwrap();
        let a = "---\ntype: story\nsource_repo: owner/repo\nstory_id: REC-A\ntitle: Inbox: first pass\nstatus: todo\n---\n";
        fs::write(dir.join("a.md"), a).unwrap();
        fs::write(dir.join("b.md"), "---\ntype: story\nsource_repo: owner/repo\nstory_id: REC-B\ndepends_on: [REC-A]\nstatus: todo # next: review\ntitle: Inbox # note: first pass\nreadiness_pass: a: b\n---\n").unwrap();
        let cfg = MetadataValidityConfig::default();
        let mut cache = crate::install::frontmatter_cache::FrontmatterCache::new();
        for name in ["a.md", "b.md"] {
            let path = dir.join(name);
            let entry = crate::install::frontmatter_cache::FrontmatterCache::prepare_refresh(
                &path, "default",
            );
            cache.commit_refresh(&path, entry);
        }
        for cached in [None, Some(&cache)] {
            let tree = scan_pipeline_tree_impl_with_cache(
                &repo,
                "pipe",
                "",
                "",
                &default_summary_headings(),
                cached,
                &cfg,
            )
            .unwrap();
            assert_eq!(tree.stories.len(), 2);
            let a = tree
                .stories
                .iter()
                .find(|s| s.story_id.as_deref() == Some("REC-A"))
                .unwrap();
            let b = tree
                .stories
                .iter()
                .find(|s| s.story_id.as_deref() == Some("REC-B"))
                .unwrap();
            assert!(!a.metadata_valid);
            assert!(a
                .metadata_invalid_reason
                .as_deref()
                .unwrap()
                .contains("Line 5 (title)"));
            assert!(b.metadata_valid, "{:?}", b.metadata_invalid_reason);
            assert_eq!(b.status.as_deref(), Some("todo"));
            assert_eq!(
                b.depends_on.as_ref().unwrap(),
                &[a.story_id.clone().unwrap()]
            );
        }
        assert_eq!(fs::read_to_string(dir.join("a.md")).unwrap(), a);
    }

    // ----- SLW-MD-1 (Task 10.5): scan validity flag ---------------------

    /// Only present-invalid metadata sets `metadata_valid=false`; absent /
    /// warning / configured-repo stay `true`. Threaded configured-repo set
    /// resolved once per scan (locked decision #5 — configured, not attached).
    #[test]
    fn scan_metadata_valid_flag_only_on_present_invalid() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // A story with a NOT-configured source_repo → invalid.
        fs::write(
            repo.join("pipe/bad.md"),
            "---\ntype: story\nsource_repo: acme/old\n---\n# Bad\n",
        )
        .unwrap();
        // A story with a configured source_repo → valid.
        fs::write(
            repo.join("pipe/good.md"),
            "---\ntype: story\nsource_repo: owner/repo\n---\n# Good\n",
        )
        .unwrap();
        // A plain doc with an invalid source_repo → badge-worthy DocRef (AC-16).
        fs::write(
            repo.join("pipe/plaindoc.md"),
            "---\ntype: doc\nsource_repo: acme/old\n---\n# Doc\n",
        )
        .unwrap();
        // A plain doc with no metadata → valid (absent = valid).
        fs::write(repo.join("pipe/notes.md"), "# Just notes\n").unwrap();

        let meta_cfg = MetadataValidityConfig {
            configured_repos: vec!["owner/repo".to_string()],
            valid_teams: vec![],
        };
        let tree = scan_pipeline_tree_impl_with_cache(
            &repo,
            "pipe",
            "",
            "",
            &default_summary_headings(),
            None,
            &meta_cfg,
        )
        .unwrap();

        let bad = tree
            .stories
            .iter()
            .find(|s| s.file_name == "bad.md")
            .expect("bad story");
        assert!(!bad.metadata_valid, "not-configured source_repo → invalid");
        assert!(bad
            .metadata_invalid_reason
            .as_deref()
            .unwrap()
            .contains("acme/old"));

        let good = tree
            .stories
            .iter()
            .find(|s| s.file_name == "good.md")
            .expect("good story");
        assert!(good.metadata_valid, "configured source_repo → valid");
        assert!(good.metadata_invalid_reason.is_none());

        let plain_bad = tree
            .docs
            .iter()
            .find(|d| d.file_name == "plaindoc.md")
            .expect("plain doc with bad repo");
        assert!(
            !plain_bad.metadata_valid,
            "plain doc invalid source_repo must badge (AC-16)"
        );

        let notes = tree
            .docs
            .iter()
            .find(|d| d.file_name == "notes.md")
            .expect("notes doc");
        assert!(notes.metadata_valid, "no metadata → valid (absent = valid)");
    }

    /// FIX 6 / AC-11: a doc whose frontmatter block is PRESENT but unparseable
    /// (broken YAML) must set `metadata_valid=false` with an "unparseable
    /// metadata block" reason — even though the parsed fields are empty. A
    /// malformed block yields empty fields → not a pipeline story → routes to
    /// `docs[]` via the inline path where the raw content is available.
    #[test]
    fn scan_unparseable_metadata_block_badges() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // A present-but-unparseable frontmatter block (unclosed `[`).
        fs::write(
            repo.join("pipe/broken.md"),
            "---\nstatus: [broken\n---\n# Broken\n",
        )
        .unwrap();

        let meta_cfg = MetadataValidityConfig::default();
        let tree = scan_pipeline_tree_impl_with_cache(
            &repo,
            "pipe",
            "",
            "",
            &default_summary_headings(),
            None,
            &meta_cfg,
        )
        .unwrap();

        let broken = tree
            .docs
            .iter()
            .find(|d| d.file_name == "broken.md")
            .expect("broken doc present in docs[]");
        assert!(
            !broken.metadata_valid,
            "present-but-unparseable block → metadata_valid=false (AC-11)"
        );
        assert!(broken
            .metadata_invalid_reason
            .as_deref()
            .unwrap()
            .contains("unparseable"));
    }

    /// Empty `MetadataValidityConfig` (badging disabled) never flags a
    /// well-formed source_repo. The 4-arg `scan_pipeline_tree_impl` uses the
    /// default (empty) config; with an empty configured set the check is
    /// syntactic-only, so a well-formed `owner/repo` stays VALID (no badge on an
    /// unresolved / zero-configured team). A *malformed* slug would still be
    /// flagged — pinned separately below.
    #[test]
    fn scan_default_config_does_not_flag_wellformed_source_repo() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/s.md"),
            "---\ntype: story\nsource_repo: owner/repo\n---\n# S\n",
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let s = &tree.stories[0];
        // Empty configured set ⇒ syntactic-only ⇒ well-formed repo is valid.
        assert!(s.metadata_valid);
    }

    /// Even with the default (empty) config, a *malformed* source_repo slug is
    /// still flagged — the empty-set escape hatch is syntactic-only, not
    /// blanket-valid.
    #[test]
    fn scan_default_config_flags_malformed_source_repo() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/s.md"),
            "---\ntype: story\nsource_repo: noslash\n---\n# S\n",
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let s = &tree.stories[0];
        assert!(
            !s.metadata_valid,
            "malformed slug is invalid even with empty config"
        );
    }

    // ----- Scanner ------------------------------------------------------

    #[test]
    fn scan_empty_directory() {
        let (_td, repo) = fix_repo();
        let root = FixtureDir::new("pipe");
        let td = tempdir().unwrap();
        build_fixture_tree(&td, &root);
        let pr = td.path().join("pipe");
        fs::create_dir_all(repo.join("pipe")).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.dir_path, "");
        assert!(tree.stories.is_empty());
        assert!(tree.sub_epics.is_empty());
        assert!(tree.order.is_none());
        assert!(tree.order_error.is_none());
        // use pr to silence unused warning
        let _ = pr;
    }

    #[test]
    fn scan_two_stories_no_order() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/1-alpha.md"), "# Alpha Title\n\nbody");
        write_story_md(repo.join("pipe/2-beta.md"), "no heading");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 2);
        assert_eq!(tree.stories[0].file_name, "1-alpha.md");
        // R-14: title removed from StoryRef; the body-extraction pathway is
        // now exercised in `get_story_details` tests (Task 5). Keeping
        // file_name + path assertions here for scanner coverage.
        assert_eq!(tree.stories[1].file_name, "2-beta.md");
        // filePath is absolute and matches DirEntry::path() semantics.
        assert!(tree.stories[0].file_path.ends_with("1-alpha.md"));
        assert!(Path::new(&tree.stories[0].file_path).is_absolute());
    }

    #[test]
    fn scan_readme_and_index_excluded() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/README.md"), "x");
        write_story_md(repo.join("pipe/index.md"), "x");
        write_story_md(repo.join("pipe/INDEX.md"), "x");
        write_story_md(repo.join("pipe/readme.md"), "x");
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "story.md");
    }

    // ----- R.7: is_pipeline_document extension registry --------------------

    #[test]
    fn is_pipeline_document_accepts_html() {
        assert!(is_pipeline_document("foo.html"));
        assert!(is_pipeline_document("1-story.html"));
        assert!(is_pipeline_document("bar.htm"));
        assert!(is_pipeline_document("foo.md"));
    }

    #[test]
    fn is_pipeline_document_rejects_readme_index_any_ext() {
        assert!(!is_pipeline_document("readme.html"));
        assert!(!is_pipeline_document("README.HTML"));
        assert!(!is_pipeline_document("index.htm"));
        assert!(!is_pipeline_document("INDEX.HTM"));
        assert!(!is_pipeline_document("readme.md"));
        assert!(!is_pipeline_document("index.md"));
    }

    #[test]
    fn is_pipeline_document_rejects_unrecognized() {
        assert!(!is_pipeline_document("notes.txt"));
        assert!(!is_pipeline_document("data.json"));
        assert!(!is_pipeline_document("image.png"));
        assert!(!is_pipeline_document("noext"));
    }

    // ----- SW-SC-1: `.excalidraw` supporting docs ------------------------

    #[test]
    fn is_supporting_doc_matches_canvas_and_images() {
        // SW-SC-1: canvases.
        assert!(is_supporting_doc("canvas.excalidraw"));
        assert!(is_supporting_doc("CANVAS.EXCALIDRAW"));
        // SW-SC-7: images are first-class supporting docs.
        assert!(is_supporting_doc("shot.png"));
        assert!(is_supporting_doc("photo.JPG"));
        assert!(is_supporting_doc("diagram.svg"));
        assert!(is_supporting_doc("anim.gif"));
        assert!(is_supporting_doc("pic.webp"));
        // Pipeline docs and non-viewable files are not supporting docs here.
        assert!(!is_supporting_doc("notes.md"));
        assert!(!is_supporting_doc("page.html"));
        assert!(!is_supporting_doc("data.json"));
        assert!(!is_supporting_doc("noext"));
    }

    // SW-SC-7 AC-9: the derived `*.excalidraw.png` render is agent-only and
    // must never route into docs[], even though `png` is a supporting-doc ext.
    #[test]
    fn is_supporting_doc_excludes_derived_excalidraw_png() {
        assert!(!is_supporting_doc("design.excalidraw.png"));
        assert!(!is_supporting_doc("DESIGN.EXCALIDRAW.PNG"));
        assert!(is_derived_render_artifact("design.excalidraw.png"));
        // a plain png is still a supporting doc
        assert!(is_supporting_doc("plain.png"));
        assert!(!is_derived_render_artifact("plain.png"));
    }

    #[test]
    fn scan_routes_excalidraw_to_docs_not_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/1-alpha.md"), "# Alpha\n");
        fs::write(
            repo.join("pipe/canvas.excalidraw"),
            r#"{"type":"excalidraw","elements":[],"appState":{},"files":{}}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();

        // The story still routes to stories[].
        assert!(tree.stories.iter().any(|s| s.file_name == "1-alpha.md"));
        // The canvas lands in docs[] (plain file row), never in stories[].
        let doc_names: Vec<&str> = tree.docs.iter().map(|d| d.file_name.as_str()).collect();
        assert!(
            doc_names.contains(&"canvas.excalidraw"),
            "excalidraw should be in docs[], got: {doc_names:?}"
        );
        assert!(!tree
            .stories
            .iter()
            .any(|s| s.file_name == "canvas.excalidraw"));
    }

    #[test]
    fn scan_html_stories_discovered() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // PTS-2 follow-up: md fixture wrapped with `type: story` so it
        // round-trips through the scanner's pipeline-type gate.
        write_story_md(repo.join("pipe/1-alpha.md"), "# Alpha\n");
        // F.30: HTML files need at least one cs:* meta tag to be included.
        // SF-1: `cs:type=story` is now required to route HTML to `stories[]`;
        // bare `cs:status` HTML now lands in `docs[]` (parity with markdown).
        fs::write(
            repo.join("pipe/2-beta.html"),
            "<meta name=\"cs:status\" content=\"todo\"><meta name=\"cs:type\" content=\"story\"><h1>Beta</h1>",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/3-gamma.htm"),
            "<meta name=\"cs:status\" content=\"todo\"><meta name=\"cs:type\" content=\"story\"><h1>Gamma</h1>",
        )
        .unwrap();
        fs::write(repo.join("pipe/notes.txt"), "not a pipeline doc").unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let names: Vec<&str> = tree.stories.iter().map(|s| s.file_name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"1-alpha.md"));
        assert!(names.contains(&"2-beta.html"));
        assert!(names.contains(&"3-gamma.htm"));
    }

    #[test]
    fn scan_html_story_all_meta_fields() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let html = r#"<!DOCTYPE html>
<html><head>
<title>HTML Story Title</title>
<meta name="cs:status" content="in-progress">
<meta name="cs:type" content="spike">
<meta name="cs:story-id" content="T-99">
<meta name="cs:depends-on" content="T-1-alpha, T-2-beta">
<meta name="cs:supporting-docs" content="./spec.md, ./notes.html">
<meta name="cs:source-repo" content="alderco/codesense">
</head><body>
<section data-cs="story">As a user, I want to test HTML pipeline extraction.</section>
</body></html>"#;
        fs::write(repo.join("pipe/T-99-full-html.html"), html).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        let s = &tree.stories[0];
        assert_eq!(s.file_name, "T-99-full-html.html");
        // R-14: title removed from StoryRef (body-extracted, fetched on-demand).
        assert_eq!(s.status.as_deref(), Some("in-progress"));
        assert_eq!(s.document_type.as_deref(), Some("spike"));
        assert_eq!(s.story_id.as_deref(), Some("T-99"));
        assert_eq!(
            s.depends_on.as_deref(),
            Some(&["T-1-alpha".to_string(), "T-2-beta".to_string()][..])
        );
        assert_eq!(
            s.supporting_docs.as_deref(),
            Some(&["./spec.md".to_string(), "./notes.html".to_string()][..])
        );
        assert_eq!(s.source_repo.as_deref(), Some("alderco/codesense"));
        // R-14: story_text removed from StoryRef (body-extracted via get_story_details).
    }

    // ----- F.30: HTML metadata gate ------------------------------------------

    #[test]
    fn html_without_meta_routes_to_docs_not_stories() {
        // SW-FX-39: cs-less HTML/HTM is a supporting DOC, not a ghost story.
        // It must never route to stories[] (AC-2, is_pipeline_story() guards
        // this), and — post-fix — it MUST appear in docs[] (AC-1) so it is
        // discoverable in the nav's Documentation bucket, at parity with a
        // frontmatter-less .md. (Was `html_without_meta_excluded_from_stories`,
        // which only pinned the stories[] exclusion; the docs[] presence is the
        // behavior this fix adds.)
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // HTML/HTM with no cs:* meta tags — must NOT be a story, but MUST be a doc.
        fs::write(repo.join("pipe/test-harness.html"), "<h1>Test Page</h1>").unwrap();
        fs::write(
            repo.join("pipe/raw-report.htm"),
            "<!DOCTYPE html><html><body>Report</body></html>",
        )
        .unwrap();
        // Markdown with a story type — the one genuine story here.
        write_story_md(repo.join("pipe/story.md"), "# Real Story\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        // AC-2: the two cs-less HTML files are never ghost stories.
        assert_eq!(
            tree.stories.len(),
            1,
            "only the story-typed .md routes to stories[]"
        );
        assert_eq!(tree.stories[0].file_name, "story.md");
        // AC-1: the two cs-less HTML files ARE discoverable as docs.
        assert!(
            tree.docs.iter().any(|d| d.file_name == "test-harness.html"),
            "cs-less .html must route to docs[] (was dropped pre-SW-FX-39)"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "raw-report.htm"),
            "cs-less .htm must route to docs[] (was dropped pre-SW-FX-39)"
        );
    }

    #[test]
    fn html_with_meta_included_in_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // SF-1: `cs:type=story` is now required to route HTML to `stories[]`
        // (parity with markdown `type: story`); bare `cs:status` lands in docs[].
        fs::write(
            repo.join("pipe/real-story.html"),
            "<meta name=\"cs:status\" content=\"todo\"><meta name=\"cs:type\" content=\"story\"><h1>Real</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "real-story.html");
        assert_eq!(tree.stories[0].status.as_deref(), Some("todo"));
    }

    // ----- SW-FX-39: plain HTML (no cs:* meta) is a doc, not a dropped file ---
    //
    // Pre-fix, the F.30 gate made `extract_story_meta` return None for cs-less
    // HTML and the scanner classify loop `continue`d past it, dropping the file
    // from BOTH stories[] and docs[] — invisible in the nav even though ⌘P found
    // it. The fix routes it to docs[] like a frontmatter-less .md. F.30's real
    // intent (no ghost STORY nodes) is preserved by `is_pipeline_story()`.

    #[test]
    fn fx39_cs_less_html_routes_to_docs() {
        // AC-1 + AC-2 — the RED→GREEN proof. A cs-less .html in a scanned dir
        // lands in docs[] (discoverable) and never in stories[] (no ghost story).
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/sync-state-flows.html"),
            "<!DOCTYPE html><html><body><h1>Report</h1></body></html>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.docs
                .iter()
                .any(|d| d.file_name == "sync-state-flows.html"),
            "AC-1: cs-less HTML must be discoverable in docs[]"
        );
        assert!(
            tree.stories
                .iter()
                .all(|s| s.file_name != "sync-state-flows.html"),
            "AC-2: cs-less HTML must never route to stories[] (no ghost story)"
        );
    }

    #[test]
    fn fx39_html_with_cs_type_unchanged() {
        // AC-3 regression — the cs:* paths are byte-identical to pre-fix:
        // cs:type=story → stories[], cs:type=doc → docs[].
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/bar.html"),
            "<meta name=\"cs:type\" content=\"story\"><h1>Bar</h1>",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/baz.html"),
            "<meta name=\"cs:type\" content=\"doc\"><h1>Baz</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().any(|s| s.file_name == "bar.html"),
            "cs:type=story still routes to stories[]"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "baz.html"),
            "cs:type=doc still routes to docs[]"
        );
    }

    #[test]
    fn fx39_frontmatter_less_md_parity_anchor() {
        // AC-3 parity anchor — a frontmatter-less .md already routes to docs[];
        // assert HTML now matches it in the same scan (raw write, NOT
        // write_story_md, which would inject `type: story`).
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(repo.join("pipe/qux.md"), "# Qux\n").unwrap();
        fs::write(repo.join("pipe/quux.html"), "<h1>Quux</h1>").unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.docs.iter().any(|d| d.file_name == "qux.md"),
            "frontmatter-less .md routes to docs[] (parity baseline)"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "quux.html"),
            "cs-less .html now matches the .md doc route"
        );
        assert!(
            tree.stories.is_empty(),
            "neither frontmatter-less file is a story"
        );
    }

    #[test]
    fn fx39_cs_less_html_not_counted_as_cache_miss() {
        // Task 2 guard — with a cache present, the newly-flowing cs-less HTML
        // must NOT inflate the miss stats: `is_pipeline_story()` is false, so
        // the SW-FX-12 gate at the `bump_miss` call skips it, exactly as a
        // frontmatter-less doc .md is skipped. Only the story-typed .md counts.
        let (_td, repo) = fix_repo();
        let pipe = repo.join("pipe");
        fs::create_dir_all(&pipe).unwrap();
        write_story_md(pipe.join("1-real.md"), "---\nstatus: todo\n---\n# Real\n");
        fs::write(pipe.join("2-plain.html"), "<h1>Plain</h1>").unwrap();

        let cache = crate::install::frontmatter_cache::FrontmatterCache::new();
        let stats = ScanStats::new();
        let tree = walk_dir(
            &pipe,
            String::new(),
            "",
            &default_summary_headings(),
            Some(&cache),
            &stats,
            &MetadataValidityConfig::default(),
        );

        assert_eq!(
            stats.misses(),
            1,
            "cs-less HTML must not be counted as a cache miss (only the story .md)"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "2-plain.html"),
            "cs-less HTML still lands in docs[] even on the cache-present path"
        );
    }

    // ----- SF-1: HTML cs:type opt-out (parity with markdown `type:`) ---------
    //
    // F.30 gates HTML presence (must carry at least one `cs:*` meta tag).
    // SF-1 makes the story-vs-doc routing symmetric: `cs:type` ∈
    // PIPELINE_DOCUMENT_TYPES → stories[], anything else → docs[]. These
    // tests pin the four corner cases enumerated in AC-12.

    #[test]
    fn sf1_html_cs_type_doc_routes_to_docs() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/spec.html"),
            "<meta name=\"cs:type\" content=\"doc\"><meta name=\"cs:status\" content=\"ready\"><h1>Spec</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().all(|s| s.file_name != "spec.html"),
            "cs:type=doc must not route to stories[]"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "spec.html"),
            "cs:type=doc must route to docs[]"
        );
    }

    #[test]
    fn sf1_html_cs_type_story_routes_to_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/feature.html"),
            "<meta name=\"cs:type\" content=\"story\"><meta name=\"cs:status\" content=\"todo\"><h1>Feature</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().any(|s| s.file_name == "feature.html"),
            "cs:type=story must route to stories[]"
        );
        assert!(
            tree.docs.iter().all(|d| d.file_name != "feature.html"),
            "cs:type=story must not route to docs[]"
        );
    }

    #[test]
    fn sf1_html_missing_cs_type_routes_to_docs() {
        // Parity with markdown's missing-type behavior: a file that
        // carries `cs:*` meta but no `cs:type` defaults to a doc.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/notes.html"),
            "<meta name=\"cs:status\" content=\"todo\"><h1>Notes</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().all(|s| s.file_name != "notes.html"),
            "HTML with cs:status but no cs:type must default to docs[]"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "notes.html"),
            "HTML with cs:status but no cs:type must land in docs[]"
        );
    }

    #[test]
    fn sf1_html_cs_type_retro_routes_to_docs() {
        // Any off-list `cs:type` value (here `retro`) lands in docs[],
        // matching markdown's behavior for unknown frontmatter `type:` values.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/retro.html"),
            "<meta name=\"cs:type\" content=\"retro\"><h1>Retro</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().all(|s| s.file_name != "retro.html"),
            "cs:type=retro must not route to stories[]"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "retro.html"),
            "cs:type=retro must route to docs[]"
        );
    }

    // ----- SF-1 AC-16: cs:type input normalization (trim + case-insensitive) -
    //
    // The predicate `is_pipeline_story` trims whitespace and uses
    // `eq_ignore_ascii_case` so authoring variants of the document type
    // route consistently across both markdown and HTML. These tests pin
    // the AC-16b enumeration: mixed-case HTML, whitespace-padded HTML,
    // case-insensitive off-list HTML, and markdown parity for both.

    #[test]
    fn sf1_ac16_html_cs_type_mixed_case_routes_to_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/proper-noun.html"),
            "<meta name=\"cs:type\" content=\"Story\"><h1>Proper Noun</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories
                .iter()
                .any(|s| s.file_name == "proper-noun.html"),
            "cs:type=Story must route to stories[] (case-insensitive)"
        );
    }

    #[test]
    fn sf1_ac16_html_cs_type_whitespace_routes_to_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/padded.html"),
            "<meta name=\"cs:type\" content=\" story \"><h1>Padded</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().any(|s| s.file_name == "padded.html"),
            "cs:type=' story ' must route to stories[] (whitespace-tolerant)"
        );
    }

    #[test]
    fn sf1_ac16_html_cs_type_uppercase_off_list_routes_to_docs() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/shouty.html"),
            "<meta name=\"cs:type\" content=\"DOC\"><h1>Shouty</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().all(|s| s.file_name != "shouty.html"),
            "cs:type=DOC (case-insensitive off-list) must not route to stories[]"
        );
        assert!(
            tree.docs.iter().any(|d| d.file_name == "shouty.html"),
            "cs:type=DOC must route to docs[]"
        );
    }

    #[test]
    fn sf1_ac16_markdown_type_mixed_case_routes_to_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/proper-md.md"),
            "---\ntype: Story\n---\n# Title\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().any(|s| s.file_name == "proper-md.md"),
            "markdown type=Story must route to stories[] (parity with HTML case-insensitivity)"
        );
    }

    #[test]
    fn sf1_ac16_markdown_type_with_padding_routes_to_stories() {
        // YAML strips trailing whitespace from unquoted scalars by spec,
        // but the leading-space variant `type:  story` could in theory
        // round-trip through gray_matter with internal padding. This
        // test pins the predicate-side trim so the symmetry guarantee
        // does not depend on YAML parser behavior.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/padded-md.md"),
            "---\ntype: \"  story  \"\n---\n# Title\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(
            tree.stories.iter().any(|s| s.file_name == "padded-md.md"),
            "markdown type=\"  story  \" (quoted, padded) must route to stories[] (predicate trim)"
        );
    }

    // ----- PTS-2 follow-up: markdown pipeline-type gate ----------------------
    //
    // Positive cases: `type: story | bug | spike` → tree.stories[].
    // Negative cases: no frontmatter, no type field, `type: doc | retro |
    // llm-distillate | anything-else` → tree.docs[].
    //
    // These tests use raw fs::write (not write_story_md) because their
    // whole purpose is to verify what the scanner does on bodies without
    // injected `type: story` frontmatter.

    #[test]
    fn gate_markdown_without_frontmatter_lands_in_docs() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/no-fm.md"),
            "# No Frontmatter\n\nbody only.\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 0);
        assert_eq!(tree.docs.len(), 1);
        assert_eq!(tree.docs[0].file_name, "no-fm.md");
    }

    #[test]
    fn gate_markdown_with_frontmatter_but_no_type_lands_in_docs() {
        // Common shape: a planning `prd.md` carries `stepsCompleted` etc. but
        // no recognized story frontmatter. Goes to docs[].
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/prd.md"),
            "---\nstepsCompleted: [1, 2, 3]\n---\n\n# PRD\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 0);
        assert_eq!(tree.docs.len(), 1);
        assert_eq!(tree.docs[0].file_name, "prd.md");
    }

    #[test]
    fn gate_type_story_bug_spike_land_in_stories() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(repo.join("pipe/s.md"), "---\ntype: story\n---\n\n# Story\n").unwrap();
        fs::write(repo.join("pipe/b.md"), "---\ntype: bug\n---\n\n# Bug\n").unwrap();
        fs::write(repo.join("pipe/k.md"), "---\ntype: spike\n---\n\n# Spike\n").unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 3);
        assert_eq!(tree.docs.len(), 0);
    }

    #[test]
    fn gate_type_doc_retro_other_land_in_docs() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(repo.join("pipe/d.md"), "---\ntype: doc\n---\n\n# Doc\n").unwrap();
        fs::write(repo.join("pipe/r.md"), "---\ntype: retro\n---\n\n# Retro\n").unwrap();
        fs::write(
            repo.join("pipe/l.md"),
            "---\ntype: llm-distillate\nsource_repo: a/b\n---\n\n# Distillate\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 0);
        assert_eq!(tree.docs.len(), 3);
    }

    // ----- SW-AU-77H: standalone-doc subject_id marker on DocRef ----------------

    #[test]
    fn subject_id_marker_surfaces_on_doc_ref_md_and_html() {
        // A non-story document (no `type: story`) declares a `subject_id` — the
        // marker must land on `DocRef.subject_id`, identically from markdown
        // frontmatter and the HTML `cs:subject-id` meta (AC-3 format parity).
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/runbook.md"),
            "---\nsubject_id: DOC-RUNBOOK-1\n---\n\n# Runbook\n",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/report.html"),
            "<meta name=\"cs:subject-id\" content=\"DOC-RUNBOOK-1\"><h1>Report</h1>",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.stories.len(),
            0,
            "no `type: story` ⇒ neither is a story"
        );
        assert_eq!(tree.docs.len(), 2);
        let by_name = |name: &str| {
            tree.docs
                .iter()
                .find(|d| d.file_name == name)
                .and_then(|d| d.subject_id.as_deref())
        };
        assert_eq!(by_name("runbook.md"), Some("DOC-RUNBOOK-1"), "markdown");
        assert_eq!(by_name("report.html"), Some("DOC-RUNBOOK-1"), "html");
    }

    #[test]
    fn doc_without_subject_id_has_no_marker() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/prd.md"),
            "---\nstepsCompleted: [1, 2, 3]\n---\n\n# PRD\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.docs.len(), 1);
        assert_eq!(tree.docs[0].subject_id, None);
    }

    #[test]
    fn scan_html_readme_excluded_md_readme_excluded() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(repo.join("pipe/README.html"), "<h1>R</h1>").unwrap();
        fs::write(repo.join("pipe/index.htm"), "<h1>I</h1>").unwrap();
        write_story_md(repo.join("pipe/README.md"), "# R\n");
        write_story_md(repo.join("pipe/story.md"), "# S\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "story.md");
    }

    #[test]
    fn scan_hidden_and_meta_excluded() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::create_dir_all(repo.join("pipe/.hidden-dir")).unwrap();
        fs::write(repo.join("pipe/.meta/pipeline.json"), "{}").unwrap();
        write_story_md(repo.join("pipe/.hidden.md"), "x");
        write_story_md(repo.join("pipe/visible.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "visible.md");
        assert!(tree.sub_epics.is_empty());
    }

    #[test]
    fn scan_valid_pipeline_json() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "x");
        write_story_md(repo.join("pipe/b.md"), "x");
        let json = r#"{"version":1,"steps":[{"storyFile":"a.md"},{"storyFile":"b.md"}]}"#;
        fs::write(repo.join("pipe/.meta/pipeline.json"), json).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order_error.is_none());
        let spec = tree.order.expect("order present");
        assert_eq!(spec.version, 1);
        assert_eq!(spec.steps.len(), 2);
    }

    #[test]
    fn scan_pipeline_json_invalid_json() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(repo.join("pipe/.meta/pipeline.json"), "{ not json").unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order.is_none());
        assert!(tree
            .order_error
            .as_deref()
            .unwrap()
            .starts_with("Invalid JSON:"));
    }

    #[test]
    fn scan_pipeline_json_unsupported_version() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":2,"steps":[]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.order_error.as_deref(),
            Some("Unsupported pipeline.json version: 2")
        );
    }

    #[test]
    fn scan_pipeline_json_invalid_shape() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"wat":"nope"}]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree
            .order_error
            .as_deref()
            .unwrap()
            .contains("Invalid step at index 0"));
    }

    #[test]
    fn scan_pipeline_json_unresolved_story() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "x");
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"storyFile":"ghost.md"}]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.order_error.as_deref(),
            Some("Unresolved storyFile: ghost.md")
        );
    }

    // R.7: pipeline.json with HTML storyFile refs
    #[test]
    fn pipeline_json_html_story_file_resolves() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        // F.30: HTML needs cs:* meta to be included as a story.
        // SF-1: also needs `cs:type=story` to route to `stories[]`.
        fs::write(
            repo.join("pipe/a.html"),
            "<meta name=\"cs:status\" content=\"todo\"><meta name=\"cs:type\" content=\"story\"><h1>A</h1>",
        )
        .unwrap();
        write_story_md(repo.join("pipe/b.md"), "# B\n");
        let json = r#"{"version":1,"steps":[{"storyFile":"a.html"},{"storyFile":"b.md"}]}"#;
        fs::write(repo.join("pipe/.meta/pipeline.json"), json).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order_error.is_none());
        let spec = tree.order.expect("order present");
        assert_eq!(spec.steps.len(), 2);
    }

    #[test]
    fn pipeline_json_no_extension_unresolved() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(repo.join("pipe/a.html"), "<h1>A</h1>").unwrap();
        let json = r#"{"version":1,"steps":[{"storyFile":"a"}]}"#;
        fs::write(repo.join("pipe/.meta/pipeline.json"), json).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.order_error.as_deref(), Some("Unresolved storyFile: a"));
    }

    #[test]
    fn scan_pipeline_json_unresolved_subepic() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"subEpic":"ghost"}]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.order_error.as_deref(),
            Some("Unresolved subEpic: ghost")
        );
    }

    #[test]
    fn scan_pipeline_json_duplicate_story() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "x");
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"storyFile":"a.md"},{"storyFile":"a.md"}]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.order_error.as_deref(),
            Some("Duplicate storyFile: a.md")
        );
    }

    #[test]
    fn scan_sub_epic_nested() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/epic-a")).unwrap();
        write_story_md(repo.join("pipe/epic-a/story.md"), "# Story A\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.sub_epics.len(), 1);
        let child = &tree.sub_epics[0];
        assert_eq!(child.dir_name, "epic-a");
        assert_eq!(child.dir_path, "epic-a");
        assert_eq!(child.stories.len(), 1);
        assert_eq!(child.stories[0].file_name, "story.md");
    }

    #[test]
    fn scan_three_level_nesting() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/a/b/c")).unwrap();
        write_story_md(repo.join("pipe/a/b/c/leaf.md"), "# Leaf\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let a = &tree.sub_epics[0];
        assert_eq!(a.dir_path, "a");
        let b = &a.sub_epics[0];
        assert_eq!(b.dir_path, "a/b");
        let c = &b.sub_epics[0];
        assert_eq!(c.dir_path, "a/b/c");
        assert_eq!(c.stories[0].file_name, "leaf.md");
    }

    #[test]
    fn scan_sub_epic_reference_resolved() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/epic-a")).unwrap();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/epic-a/x.md"), "x");
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"subEpic":"epic-a"}]}"#,
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order_error.is_none());
        assert_eq!(tree.order.unwrap().steps.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn scan_symlink_file_skipped() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/real.md"), "real");
        write_story_md(repo.join("target.md"), "tgt");
        std::os::unix::fs::symlink(repo.join("target.md"), repo.join("pipe/link.md")).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let names: Vec<&str> = tree.stories.iter().map(|s| s.file_name.as_str()).collect();
        assert!(names.contains(&"real.md"));
        assert!(!names.contains(&"link.md"));
    }

    #[cfg(unix)]
    #[test]
    fn scan_symlink_dir_skipped() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/real-dir")).unwrap();
        fs::create_dir_all(repo.join("other")).unwrap();
        write_story_md(repo.join("other/x.md"), "x");
        std::os::unix::fs::symlink(repo.join("other"), repo.join("pipe/link-dir")).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let child_names: Vec<&str> = tree.sub_epics.iter().map(|e| e.dir_name.as_str()).collect();
        assert!(child_names.contains(&"real-dir"));
        assert!(!child_names.contains(&"link-dir"));
    }

    #[test]
    fn title_extraction_finds_first_h1() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "intro\n\n# My Title\n\n# Second\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        // R-14: title body-extraction is now exercised via get_story_details
        // tests (Task 5). The scanner's StoryRef no longer carries title.
        assert_eq!(tree.stories[0].file_name, "a.md");
    }

    // R-14: `title_extraction_cutoff_4kb` removed — title is no longer in
    // StoryRef. The 4KB read cap is enforced inside `extract_story_meta`
    // and covered by get_story_details tests (Task 5).

    #[test]
    fn pipeline_root_not_found() {
        let (_td, repo) = fix_repo();
        let err = scan_pipeline_tree_impl(&repo, "missing", "", "").unwrap_err();
        assert!(err.contains("Pipeline root not found"));
    }

    #[test]
    fn path_traversal_rejected() {
        let (_td, repo) = fix_repo();
        let err = scan_pipeline_tree_impl(&repo, "..", "", "").unwrap_err();
        assert!(err.contains("pipelineRoot"));
    }

    // ----- find_nearest_valid_scope ------------------------------------

    #[test]
    fn find_scope_valid_returns_same() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/a/b")).unwrap();
        let result = find_nearest_valid_scope_impl(&repo, "pipe", "a/b").unwrap();
        assert_eq!(result, "a/b");
    }

    #[test]
    fn find_scope_drops_one_segment() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/a")).unwrap();
        let result = find_nearest_valid_scope_impl(&repo, "pipe", "a/ghost").unwrap();
        assert_eq!(result, "a");
    }

    #[test]
    fn find_scope_full_chain_invalid() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let result = find_nearest_valid_scope_impl(&repo, "pipe", "x/y/z").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn find_scope_empty_input_returns_empty() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let result = find_nearest_valid_scope_impl(&repo, "pipe", "").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn find_scope_pipeline_root_missing_errors() {
        let (_td, repo) = fix_repo();
        let err = find_nearest_valid_scope_impl(&repo, "pipe", "a").unwrap_err();
        assert!(err.contains("Pipeline root not found"));
    }

    // ----- Fixture helper + AC-17a round-trip --------------------------

    #[test]
    fn fixture_helper_builds_tree() {
        let td = tempdir().unwrap();
        let spec = FixtureDir::new("pipe")
            .story("a.md", "# A\n")
            .sub(
                FixtureDir::new("epic-a")
                    .story("x.md", "# X\n")
                    .pipeline(r#"{"version":1,"steps":[{"storyFile":"x.md"}]}"#),
            )
            .pipeline(r#"{"version":1,"steps":[{"subEpic":"epic-a"},{"storyFile":"a.md"}]}"#);

        build_fixture_tree(&td, &spec);
        assert!(td.path().join("pipe/a.md").is_file());
        assert!(td.path().join("pipe/epic-a/x.md").is_file());
        assert!(td.path().join("pipe/.meta/pipeline.json").is_file());
        assert!(td.path().join("pipe/epic-a/.meta/pipeline.json").is_file());
    }

    #[test]
    fn ac_17a_round_trip_paths_consumable() {
        // Build a fixture via the helper, run the scanner, verify every emitted
        // filePath / absoluteDirPath is real, absolute, and readable. This is
        // the canary for path-format drift between the scanner and read_file
        // (which uses validate_context_path).
        let td = tempdir().unwrap();
        let spec = FixtureDir::new("pipe")
            .story("root.md", "# Root\n")
            .sub(
                FixtureDir::new("epic-a")
                    .story("a.md", "# A\n")
                    .sub(FixtureDir::new("epic-a-1").story("deep.md", "# Deep\n")),
            )
            .sub(FixtureDir::new("epic-b").story("b.md", "# B\n"));
        build_fixture_tree(&td, &spec);

        let repo = td.path().to_path_buf();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();

        // Walk and collect every filePath + absoluteDirPath
        fn walk(t: &DerivedPipelineTree, files: &mut Vec<String>, dirs: &mut Vec<String>) {
            dirs.push(t.absolute_dir_path.clone());
            for s in &t.stories {
                files.push(s.file_path.clone());
            }
            for sub in &t.sub_epics {
                walk(sub, files, dirs);
            }
        }
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        walk(&tree, &mut files, &mut dirs);

        assert!(!files.is_empty());
        assert!(!dirs.is_empty());

        for f in &files {
            let p = Path::new(f);
            assert!(p.is_absolute(), "filePath not absolute: {}", f);
            // Must be inside the fixture repo root (plays the role of contextRepoPath).
            assert!(
                p.starts_with(&repo),
                "filePath {} not under repo {}",
                f,
                repo.display()
            );
            // Readable via fs::read_to_string — same mechanism read_file uses.
            fs::read_to_string(p).expect("filePath must be readable");
        }
        for d in &dirs {
            let p = Path::new(d);
            assert!(p.is_absolute(), "absoluteDirPath not absolute: {}", d);
            assert!(p.starts_with(&repo));
            fs::read_dir(p).expect("absoluteDirPath must be listable");
        }
    }

    // ----- Transient IO error handling (AC-16) ------------------------

    #[test]
    fn pipeline_json_empty_steps_ok() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[]}"#,
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order_error.is_none());
        assert_eq!(tree.order.unwrap().steps.len(), 0);
    }

    #[test]
    fn pipeline_json_parallel_step() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "x");
        write_story_md(repo.join("pipe/b.md"), "x");
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"parallel":[{"storyFile":"a.md"},{"storyFile":"b.md"}]}]}"#,
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.order_error.is_none());
        match &tree.order.as_ref().unwrap().steps[0] {
            EpicStepInput::Parallel(g) => assert_eq!(g.parallel.len(), 2),
            _ => panic!("expected parallel step"),
        }
    }

    #[test]
    fn pipeline_json_parallel_duplicate_rejected() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        write_story_md(repo.join("pipe/a.md"), "x");
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"version":1,"steps":[{"storyFile":"a.md"},{"parallel":[{"storyFile":"a.md"}]}]}"#,
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(
            tree.order_error.as_deref(),
            Some("Duplicate storyFile: a.md")
        );
    }

    #[test]
    fn scope_path_drill_in() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/in-progress/30-pipeline-viewer-ui")).unwrap();
        write_story_md(
            repo.join("pipe/in-progress/30-pipeline-viewer-ui/story.md"),
            "# Story\n",
        );

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "in-progress/30-pipeline-viewer-ui", "")
            .unwrap();
        assert_eq!(tree.dir_path, "in-progress/30-pipeline-viewer-ui");
        assert_eq!(tree.dir_name, "30-pipeline-viewer-ui");
        assert_eq!(tree.stories.len(), 1);
    }

    // ----- pipeline_root composition (PTS-2: viewer_root retired) -------

    #[test]
    fn scanner_joins_pipeline_root_under_team_content_path() {
        // PTS-2: caller passes the per-team content path directly (no
        // viewer_root composition); `pipeline_root` is the only sub-segment
        // the scanner composes internally.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("implementation-artifacts")).unwrap();
        write_story_md(repo.join("implementation-artifacts/story.md"), "# Story\n");

        let tree = scan_pipeline_tree_impl(&repo, "implementation-artifacts", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert!(tree.stories[0]
            .file_path
            .contains("implementation-artifacts"));
    }

    #[test]
    fn scanner_rejects_pipeline_root_traversal() {
        let (_td, repo) = fix_repo();
        let err = scan_pipeline_tree_impl(&repo, "../escape", "", "").unwrap_err();
        assert!(err.contains("pipelineRoot"));
    }

    #[test]
    fn scanner_errors_when_pipeline_root_missing() {
        let (_td, repo) = fix_repo();
        let err = scan_pipeline_tree_impl(&repo, "implementation-artifacts", "", "").unwrap_err();
        assert!(err.contains("Pipeline root not found"));
    }

    #[test]
    fn find_scope_works_with_pipeline_root_subdirs() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("impl/a/b")).unwrap();
        let result = find_nearest_valid_scope_impl(&repo, "impl", "a/b/ghost").unwrap();
        assert_eq!(result, "a/b");
    }

    #[test]
    fn scanner_works_with_empty_pipeline_root() {
        // PTS-2: the per-team content path IS the scan root; an empty
        // pipeline_root means "scan at the content-dir root".
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("impl")).unwrap();
        write_story_md(repo.join("impl/story.md"), "# Root Story\n");

        let tree = scan_pipeline_tree_impl(&repo, "impl", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert!(tree.stories[0].file_path.contains("impl"));
    }

    // ----- 30.9: extract_story_meta + frontmatter depends_on ------------

    /// Helper: scan a single story body and return its `StoryRef`.
    ///
    /// PTS-2 follow-up: auto-injects `type: story` into well-formed
    /// frontmatter so legacy fixtures still emit a pipeline story. Tests
    /// that exercise the gate's NEGATIVE path (no-frontmatter, `type: doc`,
    /// etc.) use raw `fs::write` + `scan_pipeline_tree_impl` and assert on
    /// `tree.docs`. Malformed-frontmatter tests use `meta_one` (which
    /// calls `extract_story_meta` directly, bypassing the scanner's
    /// classification step).
    fn scan_one(body: &str) -> StoryRef {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(repo.join("pipe/a.md"), inject_type_story(body)).unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        tree.stories.into_iter().next().expect("one story")
    }

    /// Sibling helper for bulk-fixture tests: write a markdown story file
    /// with `type: story` frontmatter auto-injected. Replaces raw
    /// `fs::write(..., body).unwrap()` at every site whose fixture is
    /// intended to be a pipeline story. Tests that intentionally write a
    /// non-pipeline doc (gate negative cases) use raw `fs::write` directly.
    fn write_story_md(path: impl AsRef<Path>, body: &str) {
        fs::write(path, inject_type_story(body)).unwrap();
    }

    /// PTS-2 follow-up: re-export from `test_helpers` so existing
    /// `scan_one` / `write_story_md` call sites in this `tests` module
    /// don't need to qualify the path. The actual implementation +
    /// docstring lives in `test_helpers::inject_type_story` so that
    /// `build_dir` (FixtureDir builder) can share it.
    use super::test_helpers::inject_type_story;

    /// R-14: Helper for testing body-extraction (title + story_text). These
    /// fields are no longer on `StoryRef` — they're returned by
    /// `extract_story_meta` and consumed by `get_story_details` (Task 5).
    /// Tests that previously asserted on `scan_one().title` / `.story_text`
    /// migrate to this helper.
    fn meta_one(body: &str) -> StoryMeta {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let path = repo.join("pipe/a.md");
        fs::write(&path, body).unwrap();
        extract_story_meta(&path).expect("md always returns Some")
    }

    #[test]
    fn frontmatter_absent_returns_none() {
        let s = scan_one("# Title\n\nbody only, no frontmatter.\n");
        assert!(s.depends_on.is_none());
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_present_no_depends_on_returns_none() {
        let s = scan_one("---\nstatus: ready\n---\n\n# Title\n");
        assert!(s.depends_on.is_none());
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_empty_block_returns_none() {
        // `---\n---\n` — open and close on consecutive lines, no body.
        let s = scan_one("---\n---\n\n# Title\n");
        assert!(s.depends_on.is_none());
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_depends_on_empty_array() {
        let s = scan_one("---\ndepends_on: []\n---\n\n# Title\n");
        assert_eq!(s.depends_on.as_deref(), Some(&[] as &[String]));
    }

    #[test]
    fn dev_branch_extracted_into_story_meta() {
        // SW-GS-35 (AC-1): dev_branch reaches the cache-parse StoryMeta path
        // (the serde-direct viewer path is covered in pipeline_format tests).
        let m =
            meta_one("---\nsource_repo: org/repo\ndev_branch: feat/SW-GS-35-foo\n---\n# Title\n");
        assert_eq!(m.dev_branch.as_deref(), Some("feat/SW-GS-35-foo"));
    }

    #[test]
    fn dev_branch_sanitized_and_absent_cases() {
        // Trim surrounding whitespace like source_repo/team.
        let trimmed =
            meta_one("---\nsource_repo: org/repo\ndev_branch: \"  feat/x  \"\n---\n# T\n");
        assert_eq!(trimmed.dev_branch.as_deref(), Some("feat/x"));
        // Absent ⇒ None (AC-6).
        let absent = meta_one("---\nsource_repo: org/repo\n---\n# T\n");
        assert!(absent.dev_branch.is_none());
    }

    // SW-LN-B42: board cards and nav rows read the story title from the scan (no
    // file fetch), so a frontmatter `title:` / HTML `cs:title` must reach
    // `StoryRef.open_tags.title` — distinct from the body H1.
    #[test]
    fn open_tags_carry_the_metadata_title() {
        let s = scan_one(
            "---\nstatus: todo\ntitle: \"API health endpoint: returns build info\"\n---\n\n# Body heading\n\nAdd a health endpoint.\n",
        );
        assert_eq!(
            s.open_tags.get("title"),
            Some(&vec!["API health endpoint: returns build info".to_string()])
        );
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/b.html"),
            "<html><head><meta name=\"cs:type\" content=\"story\"><meta name=\"cs:story-id\" content=\"H-1\"><meta name=\"cs:status\" content=\"todo\"><meta name=\"cs:title\" content=\"Html title\"><title>Doc</title></head><body><h1>Body</h1></body></html>",
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let json = serde_json::to_string(&tree).unwrap();
        assert!(
            json.contains("\"openTags\":{\"title\":[\"Html title\"]}"),
            "HTML cs:title reaches openTags.title: {json}"
        );
    }

    // SW-DX-2 (AC-13): a scanned markdown story surfaces its OPEN frontmatter
    // tags (every key not in the hardcoded set) on `StoryRef.open_tags`, via the
    // same `open_tag_values` extractor SW-DX-1 uses. A schema tag like
    // `context_level` lives in open frontmatter and must be queryable.
    #[test]
    fn open_tags_surface_context_level_on_story_ref() {
        let s = scan_one("---\nstatus: todo\ncontext_level: team\n---\n\n# Title\n");
        assert_eq!(
            s.open_tags.get("context_level"),
            Some(&vec!["team".to_string()]),
            "open schema tag surfaced on StoryRef.open_tags"
        );
        // The named `status` field is NOT an open tag (it's a hardcoded field).
        assert!(!s.open_tags.contains_key("status"));
        // And the open tag is queryable via query_docs → DocSummary.tags.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/a.md"),
            inject_type_story("---\nstory_id: Q-1\nstatus: todo\ncontext_level: team\n---\n# T\n"),
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let mut tag = std::collections::BTreeMap::new();
        tag.insert(
            "context_level".to_string(),
            crate::doc_query::TagFilterValue::One("team".to_string()),
        );
        let res = crate::doc_query::query_docs(
            &tree,
            &crate::doc_query::DocQuery {
                tag: Some(tag),
                ..Default::default()
            },
        );
        assert_eq!(res.docs.len(), 1);
        assert_eq!(
            res.docs[0].tags.get("context_level"),
            Some(&vec!["team".to_string()])
        );
    }

    // SW-SB-1 (AC-5 scanner arm + AC-6): a scanned story surfaces the three
    // board fields on `StoryRef` (through the full StoryMeta → story_meta_to_fields
    // → build_story_ref path), and none of them leak into `open_tags` (they're
    // known top-level keys now). Also round-trips onward to `DocSummary` (AC-5).
    #[test]
    fn board_metadata_surfaces_on_story_ref_and_not_in_open_tags() {
        let s = scan_one(
            "---\nstatus: todo\nsprint: 2026-08-A\npoints: 5\nassigned_to: octocat-dev\n---\n\n# Title\n",
        );
        assert_eq!(s.sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(s.points, Some(5.0));
        assert_eq!(s.assigned_to.as_deref(), Some("octocat-dev"));
        // AC-6: none of the three double-appear as open tags.
        for k in ["sprint", "points", "assigned_to"] {
            assert!(!s.open_tags.contains_key(k), "{k} must not be an open tag");
        }

        // AC-5 onward arm: the same three reach `DocSummary` via query_docs.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/a.md"),
            inject_type_story(
                "---\nstory_id: SB-1\nstatus: todo\nsprint: 2026-08-A\npoints: 5\nassigned_to: octocat-dev\n---\n# T\n",
            ),
        )
        .unwrap();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let res = crate::doc_query::query_docs(&tree, &crate::doc_query::DocQuery::default());
        assert_eq!(res.docs.len(), 1);
        assert_eq!(res.docs[0].sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(res.docs[0].points, Some(5.0));
        assert_eq!(res.docs[0].assigned_to.as_deref(), Some("octocat-dev"));
    }

    // SW-SB-1 (AC-5 absent arm): a story with none of the three fields omits
    // them cleanly (`None`) — never `""`/`0`.
    #[test]
    fn board_metadata_absent_is_none() {
        let s = scan_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.sprint.is_none());
        assert!(s.points.is_none());
        assert!(s.assigned_to.is_none());
    }

    #[test]
    fn frontmatter_depends_on_one_entry() {
        let s = scan_one("---\ndepends_on: [foo]\n---\n\n# Title\n");
        assert_eq!(s.depends_on.as_deref(), Some(&["foo".to_string()][..]));
    }

    #[test]
    fn frontmatter_depends_on_multi_entry_flow_style() {
        let s = scan_one("---\ndepends_on: [a, b, c]\n---\n\n# Title\n");
        assert_eq!(
            s.depends_on.as_deref(),
            Some(&["a".to_string(), "b".to_string(), "c".to_string()][..])
        );
    }

    #[test]
    fn frontmatter_tags_multi_entry_flow_style() {
        // SW-AU-77F: markdown `tags:` populates the typed `tags` list on StoryRef.
        let s = scan_one("---\ntags: [infra, urgent]\n---\n\n# Title\n");
        assert_eq!(
            s.tags.as_deref(),
            Some(&["infra".to_string(), "urgent".to_string()][..])
        );
    }

    #[test]
    fn frontmatter_no_tags_is_none() {
        let s = scan_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.tags.is_none());
    }

    #[test]
    fn frontmatter_depends_on_block_style() {
        let s = scan_one("---\ndepends_on:\n  - a\n  - b\n---\n\n# Title\n");
        assert_eq!(
            s.depends_on.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
    }

    #[test]
    fn frontmatter_malformed_yaml_returns_none() {
        // Trailing colon with no value plus dangling bracket — invalid YAML.
        // PTS-2 follow-up: tests extract_story_meta extraction, not the
        // scanner's pipeline gate — use meta_one to bypass the gate.
        let s = meta_one("---\ndepends_on: [a, b\n---\n\n# Title\n");
        assert!(s.depends_on.is_none());
    }

    #[test]
    fn frontmatter_depends_on_wrong_shape_returns_none() {
        // PTS-2 follow-up: extraction-only — see frontmatter_malformed_yaml note.
        // Number where array is expected.
        let s1 = meta_one("---\ndepends_on: 42\n---\n\n# Title\n");
        assert!(s1.depends_on.is_none());
        // String where array is expected.
        let s2 = meta_one("---\ndepends_on: not-an-array\n---\n\n# Title\n");
        assert!(s2.depends_on.is_none());
        // Mixed-type array.
        let s3 = meta_one("---\ndepends_on: [a, 42]\n---\n\n# Title\n");
        assert!(s3.depends_on.is_none());
    }

    #[test]
    fn frontmatter_with_other_ignored_fields() {
        // Many fields, only depends_on is read — others are accepted silently.
        let s = scan_one(
            "---\nstatus: ready\npipeline: dev-pipeline\nbranch_name: feat/x\ndepends_on: [a]\nstacks_on: b\nowners: [platform]\n---\n\n# Title\n",
        );
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_no_title_only_frontmatter() {
        let s = scan_one("---\ndepends_on: [a]\n---\n\nno heading here\n");
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
        // R-14: title body-extraction is exercised via get_story_details tests.
    }

    #[test]
    fn frontmatter_crlf_line_endings() {
        let s = scan_one("---\r\ndepends_on: [a, b]\r\n---\r\n\r\n# Title\r\n");
        assert_eq!(
            s.depends_on.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_unclosed_returns_none() {
        // Open marker but no closing `\n---\n` within 4KB — treated as absent.
        // PTS-2 follow-up: extraction-only — use meta_one to bypass the gate.
        let s = meta_one("---\ndepends_on: [a]\n\n# Title\n");
        assert!(s.depends_on.is_none());
    }

    #[test]
    fn frontmatter_utf8_bom_is_stripped() {
        // Files written by some Windows editors and a few JetBrains defaults
        // start with a UTF-8 BOM (`\u{feff}`). The frontmatter delimiter
        // match must still succeed in that case.
        let s = scan_one("\u{feff}---\ndepends_on: [a]\n---\n\n# Title\n");
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
        // R-14: title removed from StoryRef; covered separately via extract_body tests.
    }

    #[test]
    fn frontmatter_trailing_whitespace_on_fence() {
        // Common authoring style: `---   ` (trailing spaces) on the open
        // and/or close lines. gray_matter's line-based matcher trims
        // trailing whitespace; the custom prefix check did not.
        // PTS-2 follow-up: extraction-only — use meta_one to bypass gate.
        let s = meta_one("---  \ndepends_on: [a]\n---   \n\n# Title\n");
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
    }

    #[test]
    fn frontmatter_depends_on_explicit_null() {
        // `depends_on: null` (or YAML's `~`) is a valid "explicitly empty"
        // signal that's distinct from "field missing." Both deserialize
        // into `Option::None` via the struct's `Option<Vec<String>>`.
        let s = scan_one("---\ndepends_on: null\n---\n\n# Title\n");
        assert!(s.depends_on.is_none());
        let s2 = scan_one("---\ndepends_on: ~\n---\n\n# Title\n");
        assert!(s2.depends_on.is_none());
    }

    // ----- 34.4: frontmatter status extraction ----------------------------

    #[test]
    fn frontmatter_status_extracted() {
        let s = scan_one("---\nstatus: review\ndepends_on: [a]\n---\n\n# Title\n");
        assert_eq!(s.status.as_deref(), Some("review"));
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
    }

    #[test]
    fn frontmatter_status_absent_returns_none() {
        let s = scan_one("---\ndepends_on: [a]\n---\n\n# Title\n");
        assert!(s.status.is_none());
    }

    #[test]
    fn frontmatter_status_only_no_depends_on() {
        let s = scan_one("---\nstatus: in-progress\n---\n\n# Title\n");
        assert_eq!(s.status.as_deref(), Some("in-progress"));
        assert!(s.depends_on.is_none());
    }

    #[test]
    fn frontmatter_no_frontmatter_status_none() {
        let s = scan_one("# Title\n\nbody only.\n");
        assert!(s.status.is_none());
    }

    // ----- M.1: frontmatter source_repo extraction -----------------------

    #[test]
    fn frontmatter_source_repo_extracted() {
        let s = scan_one("---\nsource_repo: alderco/codesense\n---\n\n# Title\n");
        assert_eq!(s.source_repo.as_deref(), Some("alderco/codesense"));
    }

    #[test]
    fn frontmatter_source_repo_absent_returns_none() {
        let s = scan_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.source_repo.is_none());
    }

    #[test]
    fn frontmatter_source_repo_empty_returns_none() {
        let s = scan_one("---\nsource_repo:\n---\n\n# Title\n");
        assert!(s.source_repo.is_none());
    }

    #[test]
    fn frontmatter_source_repo_explicit_empty_string_returns_none() {
        let s = scan_one("---\nsource_repo: \"\"\n---\n\n# Title\n");
        assert!(s.source_repo.is_none());
    }

    #[test]
    fn frontmatter_source_repo_malformed_yaml_returns_none() {
        // PTS-2 follow-up: extraction-only — use meta_one to bypass gate.
        let s = meta_one("---\nsource_repo: [unclosed\n---\n\n# Title\n");
        assert!(s.source_repo.is_none());
    }

    // ----- 34.4: story section extraction --------------------------------

    #[test]
    fn story_section_extracted() {
        // R-14: story_text body-extraction tested via meta_one (consumed by
        // get_story_details, not by the scanner's StoryRef).
        let m = meta_one("---\nstatus: review\n---\n\n# Title\n\n## Story\n\nAs a user, I want X.\n\n## Context\n\nSome context.\n");
        assert_eq!(m.story_text.as_deref(), Some("As a user, I want X."));
    }

    #[test]
    fn story_section_multiline() {
        // R-14: story_text body-extraction is exercised via extract_story_meta
        // (consumed by get_story_details in Task 5), not via StoryRef.
        let m = meta_one(
            "# Title\n\n## Story\n\nLine one.\nLine two.\nLine three.\n\n## Acceptance Criteria\n",
        );
        assert_eq!(
            m.story_text.as_deref(),
            Some("Line one.\nLine two.\nLine three.")
        );
    }

    #[test]
    fn story_section_absent() {
        let m = meta_one("# Title\n\n## Context\n\nNo story section here.\n");
        assert!(m.story_text.is_none());
    }

    #[test]
    fn story_section_empty_body() {
        let m = meta_one("# Title\n\n## Story\n\n## Context\n");
        assert!(m.story_text.is_none());
    }

    #[test]
    fn story_section_runs_to_eof() {
        let m = meta_one("# Title\n\n## Story\n\nLast section, no next heading.\n");
        assert_eq!(
            m.story_text.as_deref(),
            Some("Last section, no next heading.")
        );
    }

    #[test]
    fn frontmatter_cutoff_past_4kb_returns_none() {
        // Frontmatter that opens within 4KB but doesn't close within 4KB.
        // PTS-2 follow-up: extraction-only — use meta_one to bypass gate.
        let mut body = String::from("---\ndepends_on: [a]\n");
        body.push_str(&"x: y\n".repeat(900)); // ~4.5KB of filler
        body.push_str("---\n\n# Title\n");
        let s = meta_one(&body);
        assert!(s.depends_on.is_none());
    }

    #[test]
    fn extract_story_meta_file_shorter_than_buffer() {
        // Very short file — must not error or panic on small reads.
        // R-14: title moved to StoryMeta-only; assert via meta_one.
        let s = scan_one("# Tiny\n");
        assert!(s.depends_on.is_none());
        let m = meta_one("# Tiny\n");
        assert_eq!(m.title.as_deref(), Some("Tiny"));
    }

    // ----- 30.9 AC-11: latency budget ----------------------------------
    //
    // Scanning a synthetic 50-story directory must complete in well under
    // 100ms of wall-clock time. The threshold is loose by design — it's
    // generous enough not to be flaky on a slow CI runner, but tight
    // enough to catch a regression like "we accidentally invoke
    // `serde_yaml::from_str` on every story regardless of whether
    // frontmatter exists." Real-world scans are typically <10ms.

    #[test]
    fn latency_50_stories_under_budget_no_frontmatter() {
        // Common case: no frontmatter at all. Should hit the 3-byte
        // cheap-bail path on every story.
        //
        // PTS-2 follow-up: under the pipeline-type gate, no-frontmatter
        // md files land in `tree.docs` (not `tree.stories`). The latency
        // assertion is what this test cares about — the array they land
        // in is a routing detail. Assert total count = docs since stories
        // would be empty.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        for i in 0..50 {
            fs::write(
                repo.join(format!("pipe/{:02}-story.md", i)),
                format!("# Story {}\n\nbody only.\n", i),
            )
            .unwrap();
        }
        let start = std::time::Instant::now();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let elapsed = start.elapsed();
        assert_eq!(tree.docs.len(), 50);
        assert_eq!(tree.stories.len(), 0);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "50-story scan took {:?}, expected < 100ms",
            elapsed
        );
    }

    // ----- 34.5: update_frontmatter_status ------------------------------------

    #[test]
    fn update_status_replaces_existing() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(
            &p,
            "---\ndepends_on: [a]\nstatus: todo\n---\n\n# Title\n\nbody\n",
        )
        .unwrap();
        update_frontmatter_status(&p, "in-progress", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: in-progress"));
        assert!(result.contains("depends_on: [a]"));
        assert!(result.contains("# Title"));
        assert!(result.contains("body"));
    }

    #[test]
    fn status_writer_stays_verdict_free() {
        // SW-GT-15 AC-6 regression guard: the durable status writer writes ONLY `status`
        // (+ optional rank / done-metadata) — NEVER a per-step verdict / `gate_state`
        // field. Verdicts live in the machine-local run store, never in the git-synced
        // doc, so a status advance can't leak execution telemetry into content-sync. If a
        // later change re-introduces a doc-side verdict write, this fails.
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(
            &p,
            "---\nstory_id: SW-GT-15\nstatus: in-progress\n---\n\n# Title\n",
        )
        .unwrap();
        update_frontmatter_status(&p, "review", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: review"));
        assert!(
            !result.contains("gate_state"),
            "status writer must not emit a gate_state field: {result}"
        );
        assert!(
            !result.contains("verdict"),
            "status writer must not emit a verdict field: {result}"
        );
    }

    #[test]
    fn update_status_inserts_when_absent() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "---\ndepends_on: [a]\n---\n\n# Title\n").unwrap();
        update_frontmatter_status(&p, "review", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: review"));
        assert!(result.contains("depends_on: [a]"));
        assert!(result.contains("# Title"));
    }

    #[test]
    fn update_status_creates_frontmatter_when_none() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "# Title\n\nbody only.\n").unwrap();
        update_frontmatter_status(&p, "todo", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.starts_with("---\nstatus: todo\n---\n"));
        assert!(result.contains("# Title"));
        assert!(result.contains("body only."));
    }

    #[test]
    fn update_status_writes_an_off_taxonomy_value_and_rejects_empty() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "---\nstatus: todo\n---\n").unwrap();
        update_frontmatter_status(&p, "parked-for-now", None).unwrap();
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("status: parked-for-now"));
        let err = update_frontmatter_status(&p, "  ", None).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn update_status_missing_file() {
        let td = tempdir().unwrap();
        let p = td.path().join("does-not-exist.md");
        let err = update_frontmatter_status(&p, "done", None).unwrap_err();
        assert!(err.contains("Failed to read"));
    }

    #[test]
    fn update_status_preserves_type_field() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(
            &p,
            "---\ndepends_on: [a]\ntype: story\nstatus: todo\n---\n\n# Title\n",
        )
        .unwrap();
        update_frontmatter_status(&p, "blocked", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: blocked"));
        assert!(result.contains("type: story"));
        assert!(result.contains("depends_on: [a]"));
    }

    #[test]
    fn update_status_round_trips_with_scanner() {
        // Write status, then scan — scanner should read the new value.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let p = repo.join("pipe/a.md");
        write_story_md(&p, "---\nstatus: todo\n---\n\n# Title\n");

        update_frontmatter_status(&p, "blocked", None).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories[0].status.as_deref(), Some("blocked"));
    }

    #[test]
    fn update_status_round_trips_html() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let p = repo.join("pipe/a.html");
        // SF-1: `cs:type=story` required to route HTML to `stories[]`
        // (so this round-trip test can keep asserting `tree.stories[0]`).
        fs::write(&p, r#"<head><meta name="cs:status" content="todo"><meta name="cs:type" content="story"></head><body><h1>Title</h1></body>"#).unwrap();

        update_frontmatter_status(&p, "in-progress", None).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories[0].status.as_deref(), Some("in-progress"));
        // R-14: title removed from StoryRef.
    }

    // ----- B-5: update_frontmatter_status transition wiring --------------------

    // ----- SW-SB-22: atomic status + rank (D-2) --------------------------------

    #[test]
    fn update_status_sets_rank_atomically() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "---\nstatus: in-progress\n---\n# Title\n").unwrap();
        update_frontmatter_status(&p, "done", Some("V")).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: done"), "status should be done");
        assert!(
            result.contains("rank: V"),
            "rank should be written in the same cycle, got: {}",
            result
        );
        assert!(result.contains("# Title"), "body preserved");
    }

    #[test]
    fn update_status_away_from_done_leaves_legacy_done_block() {
        // SR-13 contract: the status writer no longer clears a legacy `done:`
        // block on an away-from-done transition (the DeleteBlock path is retired
        // with the transition engine). The stale block is harmless dead
        // frontmatter — unread by staleness and by the viewer for a non-done
        // story — and is left in place per the migration decision (no mass-strip).
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(
            &p,
            "---\nstatus: done\ndone:\n  completed_at: \"2026-05-26T14:30:00Z\"\n---\n# Title\n",
        )
        .unwrap();
        update_frontmatter_status(&p, "qa", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: qa"), "status should be qa");
        assert!(
            result.contains("completed_at"),
            "legacy done block is left in place, not cleared"
        );
    }

    #[test]
    fn update_status_without_rank_leaves_rank_untouched() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "---\nstatus: todo\nrank: h0\n---\n# Title\n").unwrap();
        update_frontmatter_status(&p, "in-progress", None).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(result.contains("status: in-progress"), "status updated");
        assert!(
            result.contains("rank: h0"),
            "a status-only write must not disturb an existing rank, got: {}",
            result
        );
    }

    #[test]
    fn update_status_overwrites_existing_rank() {
        let td = tempdir().unwrap();
        let p = td.path().join("story.md");
        fs::write(&p, "---\nstatus: todo\nrank: h0\n---\n# Title\n").unwrap();
        update_frontmatter_status(&p, "todo", Some("h5")).unwrap();
        let result = fs::read_to_string(&p).unwrap();
        assert!(
            result.contains("rank: h5"),
            "rank replaced, got: {}",
            result
        );
        assert!(!result.contains("rank: h0"), "old rank gone");
    }

    #[test]
    fn rewrite_frontmatter_pure_replace() {
        let input = "---\nstatus: todo\n---\n\n# Title\n";
        let result = rewrite_frontmatter_status(input, "done");
        assert!(result.contains("status: done"));
        assert!(!result.contains("status: todo"));
    }

    #[test]
    fn rewrite_frontmatter_pure_insert() {
        let input = "---\ndepends_on: [a]\n---\n\n# Title\n";
        let result = rewrite_frontmatter_status(input, "qa");
        assert!(result.contains("status: qa"));
        assert!(result.contains("depends_on: [a]"));
    }

    #[test]
    fn rewrite_frontmatter_pure_prepend() {
        let input = "# Title\n\nbody\n";
        let result = rewrite_frontmatter_status(input, "planning");
        assert!(result.starts_with("---\nstatus: planning\n---\n"));
        assert!(result.contains("# Title"));
    }

    #[test]
    fn rewrite_frontmatter_ignores_indented_status_in_block_scalar() {
        // A multi-line YAML value containing "status:" on an indented line
        // must not be matched — only top-level (non-indented) keys count.
        let input = "---\ndescription: |\n  status: blocked because of dependency\nstatus: todo\n---\n\n# Title\n";
        let result = rewrite_frontmatter_status(input, "done");
        // The real status field is updated.
        assert!(result.contains("\nstatus: done\n"));
        // The indented line inside the block scalar is preserved.
        assert!(result.contains("  status: blocked because of dependency"));
    }

    #[test]
    fn rewrite_frontmatter_preserves_supporting_docs_block() {
        // A multi-line supporting_docs block must survive a status update
        // without corruption — the line-based rewriter only touches the
        // `status:` line.
        let input = "---\nstatus: todo\nsupporting_docs:\n  - ./bug-report.md\n  - ./spike.md\n---\n\n# Title\n";
        let result = rewrite_frontmatter_status(input, "in-progress");
        assert!(result.contains("\nstatus: in-progress\n"));
        assert!(result.contains("supporting_docs:\n  - ./bug-report.md\n  - ./spike.md\n"));
    }

    #[test]
    fn valid_statuses_round_trip_through_scanner() {
        // Guard against VALID_STATUSES drifting out of sync with the
        // scanner's forgiving frontmatter parser. Every status value
        // accepted by update_frontmatter_status must be readable by
        // extract_frontmatter_fields → scan_pipeline_tree.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        for status in VALID_STATUSES {
            let p = repo.join("pipe/a.md");
            write_story_md(&p, &format!("---\nstatus: {}\n---\n\n# Title\n", status));
            let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
            assert_eq!(
                tree.stories[0].status.as_deref(),
                Some(*status),
                "VALID_STATUSES entry '{}' not round-tripped by scanner",
                status
            );
        }
    }

    // ----- F.3: document_type frontmatter field --------------------------

    #[test]
    fn document_type_extracted_from_frontmatter() {
        let s = scan_one("---\ntype: bug\nstatus: todo\n---\n\n# Bug Report\n");
        assert_eq!(s.document_type.as_deref(), Some("bug"));
    }

    #[test]
    fn document_type_absent_returns_none() {
        // PTS-2 follow-up: probes extract_story_meta — use meta_one so
        // the scanner's pipeline gate doesn't reroute this to docs[].
        let s = meta_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.document_type.is_none());
    }

    #[test]
    fn document_type_no_frontmatter_returns_none() {
        // PTS-2 follow-up: probes extract_story_meta — use meta_one.
        let s = meta_one("# Title\n\nbody only.\n");
        assert!(s.document_type.is_none());
    }

    #[test]
    fn document_type_unrecognized_value_passed_through() {
        // PTS-2 follow-up: probes extract_story_meta — use meta_one. (A
        // `type: research` md file would land in `tree.docs` under the
        // scanner gate, not `tree.stories`; the extraction layer still
        // surfaces the raw type for downstream consumers.)
        let s = meta_one("---\ntype: research\n---\n\n# Title\n");
        assert_eq!(s.document_type.as_deref(), Some("research"));
    }

    #[test]
    fn document_type_coexists_with_other_fields() {
        let s =
            scan_one("---\ndepends_on: [a]\ntype: spike\nstatus: in-progress\n---\n\n# Title\n");
        assert_eq!(s.document_type.as_deref(), Some("spike"));
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
        assert_eq!(s.status.as_deref(), Some("in-progress"));
    }

    // ----- F.4: supporting_docs frontmatter field ------------------------

    #[test]
    fn supporting_docs_extracted_from_frontmatter() {
        let s = scan_one(
            "---\nsupporting_docs:\n  - ./bug-report.md\n  - ./spike.md\n---\n\n# Title\n",
        );
        assert_eq!(
            s.supporting_docs.as_deref(),
            Some(&["./bug-report.md".to_string(), "./spike.md".to_string()][..])
        );
    }

    #[test]
    fn supporting_docs_absent_returns_none() {
        let s = scan_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.supporting_docs.is_none());
    }

    #[test]
    fn supporting_docs_no_frontmatter_returns_none() {
        let s = scan_one("# Title\n\nbody only.\n");
        assert!(s.supporting_docs.is_none());
    }

    #[test]
    fn supporting_docs_empty_array() {
        let s = scan_one("---\nsupporting_docs: []\n---\n\n# Title\n");
        assert_eq!(s.supporting_docs.as_deref(), Some(&[] as &[String]));
    }

    #[test]
    fn supporting_docs_single_entry_flow_style() {
        let s = scan_one("---\nsupporting_docs: [./bug.md]\n---\n\n# Title\n");
        assert_eq!(
            s.supporting_docs.as_deref(),
            Some(&["./bug.md".to_string()][..])
        );
    }

    #[test]
    fn supporting_docs_wrong_shape_returns_none() {
        // PTS-2 follow-up: extraction-only — use meta_one to bypass gate.
        // String where array is expected.
        let s = meta_one("---\nsupporting_docs: not-an-array\n---\n\n# Title\n");
        assert!(s.supporting_docs.is_none());
        // Number where array is expected.
        let s2 = meta_one("---\nsupporting_docs: 42\n---\n\n# Title\n");
        assert!(s2.supporting_docs.is_none());
    }

    #[test]
    fn supporting_docs_coexists_with_other_fields() {
        let s = scan_one(
            "---\ndepends_on: [a]\ntype: story\nstatus: in-progress\nsupporting_docs:\n  - ./bug.md\n---\n\n# Title\n",
        );
        assert_eq!(
            s.supporting_docs.as_deref(),
            Some(&["./bug.md".to_string()][..])
        );
        assert_eq!(s.depends_on.as_deref(), Some(&["a".to_string()][..]));
        assert_eq!(s.document_type.as_deref(), Some("story"));
        assert_eq!(s.status.as_deref(), Some("in-progress"));
    }

    // ----- W.8: story_id frontmatter field --------------------------------

    #[test]
    fn story_id_extracted_from_frontmatter() {
        let s = scan_one("---\nstory_id: W-5\nstatus: review\n---\n\n# Title\n");
        assert_eq!(s.story_id.as_deref(), Some("W-5"));
    }

    #[test]
    fn story_id_absent_returns_none() {
        let s = scan_one("---\nstatus: todo\n---\n\n# Title\n");
        assert!(s.story_id.is_none());
    }

    #[test]
    fn story_id_no_frontmatter_returns_none() {
        let s = scan_one("# Title\n\nbody only.\n");
        assert!(s.story_id.is_none());
    }

    #[test]
    fn story_id_coexists_with_other_fields() {
        let s = scan_one(
            "---\nstory_id: F-12\ndepends_on: [a]\nstatus: in-progress\ntype: bug\n---\n\n# Title\n",
        );
        assert_eq!(s.story_id.as_deref(), Some("F-12"));
        assert_eq!(s.status.as_deref(), Some("in-progress"));
        assert_eq!(s.document_type.as_deref(), Some("bug"));
    }

    #[test]
    fn latency_50_stories_under_budget_with_frontmatter() {
        // Worst case: every story has frontmatter — full YAML parse path.
        // PTS-2 follow-up: inject `type: story` so each fixture round-trips
        // through the scanner's pipeline-type gate. write_story_md is a
        // no-op for bodies whose frontmatter already declares `type:`.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        for i in 0..50 {
            let body = format!(
                "---\ndepends_on: [{:02}-prev]\nstatus: ready\nbranch_name: feat/x-{}\n---\n\n# Story {}\n\nbody.\n",
                i, i, i
            );
            write_story_md(repo.join(format!("pipe/{:02}-story.md", i)), &body);
        }
        let start = std::time::Instant::now();
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let elapsed = start.elapsed();
        assert_eq!(tree.stories.len(), 50);
        // Every story has a populated `depends_on`.
        for s in &tree.stories {
            assert!(
                s.depends_on.is_some(),
                "{}: expected depends_on",
                s.file_name
            );
        }
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "50-story frontmatter scan took {:?}, expected < 100ms",
            elapsed
        );
    }

    // ----- P.1: main doc detection and purpose text ----------------------

    #[test]
    fn main_doc_readme_detected() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/README.md"),
            "---\nsupporting_docs:\n  - ../spec.md\n---\n\n# Epic\n\n## Purpose\n\nThis epic does X.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "# Story\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        // README not in stories
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "story.md");
        // main_doc populated
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("README.md"));
        assert_eq!(md.purpose_text.as_deref(), Some("This epic does X."));
        assert_eq!(
            md.supporting_docs.as_deref(),
            Some(&["../spec.md".to_string()][..])
        );
    }

    #[test]
    fn main_doc_index_fallback() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/index.md"),
            "# Index\n\n## Purpose\n\nFallback doc.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("index.md"));
        assert_eq!(md.purpose_text.as_deref(), Some("Fallback doc."));
    }

    #[test]
    fn main_doc_readme_wins_over_index() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/README.md"),
            "# R\n\n## Purpose\n\nFrom README.\n",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/index.md"),
            "# I\n\n## Purpose\n\nFrom index.\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("README.md"));
        assert_eq!(md.purpose_text.as_deref(), Some("From README."));
    }

    #[test]
    fn main_doc_none_when_absent() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert!(tree.main_doc.is_none());
    }

    // ----- SW-DX-16: dir-based mainDoc resolver (resolve_epic_main_doc) ------

    #[test]
    fn resolve_epic_main_doc_finds_readme() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("ep")).unwrap();
        fs::write(repo.join("ep/README.md"), "# Epic").unwrap();
        let got = resolve_epic_main_doc(&repo.join("ep"), None).expect("resolves README");
        assert!(got.ends_with("README.md"), "got {}", got.display());
    }

    #[test]
    fn resolve_epic_main_doc_readme_wins_over_index() {
        // Same precedence as detect_main_doc / the FE MAIN_DOC_PRIORITY.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("ep")).unwrap();
        fs::write(repo.join("ep/index.md"), "# I").unwrap();
        fs::write(repo.join("ep/README.md"), "# R").unwrap();
        let got = resolve_epic_main_doc(&repo.join("ep"), None).unwrap();
        assert!(
            got.ends_with("README.md"),
            "readme beats index; got {}",
            got.display()
        );
    }

    #[test]
    fn resolve_epic_main_doc_configured_name_wins() {
        // The resolver IS custom-mainDoc-capable: a configured name outranks the
        // defaults (proves the param; the FE passes None → default precedence).
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("ep")).unwrap();
        fs::write(repo.join("ep/README.md"), "# R").unwrap();
        fs::write(repo.join("ep/Epic.md"), "# E").unwrap();
        let got = resolve_epic_main_doc(&repo.join("ep"), Some("Epic.md")).unwrap();
        assert!(
            got.ends_with("Epic.md"),
            "configured wins; got {}",
            got.display()
        );
    }

    #[test]
    fn resolve_epic_main_doc_none_when_absent() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("ep")).unwrap();
        fs::write(repo.join("ep/notes.txt"), "x").unwrap(); // not a mainDoc name
        assert!(resolve_epic_main_doc(&repo.join("ep"), None).is_none());
    }

    #[test]
    fn main_doc_configured_name_from_global_param() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // Both Epic.md and README.md exist — configured name wins.
        fs::write(
            repo.join("pipe/Epic.md"),
            "# Epic\n\n## Purpose\n\nConfigured.\n",
        )
        .unwrap();
        fs::write(
            repo.join("pipe/README.md"),
            "# R\n\n## Purpose\n\nDefault.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "Epic.md").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("Epic.md"));
        assert_eq!(md.purpose_text.as_deref(), Some("Configured."));
    }

    #[test]
    fn main_doc_configured_name_falls_back_to_readme() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        // Configured name doesn't exist, but README does.
        fs::write(
            repo.join("pipe/README.md"),
            "# R\n\n## Purpose\n\nFallback.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "Epic.md").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("README.md"));
    }

    #[test]
    fn main_doc_config_only_pipeline_json_no_ordering() {
        // P.3 AC-5: a config-only pipeline.json (no version field) should
        // not produce an order_error.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/.meta")).unwrap();
        // Config-only pipeline.json — no `version` field, just arbitrary config.
        fs::write(
            repo.join("pipe/.meta/pipeline.json"),
            r#"{"custom_setting": true}"#,
        )
        .unwrap();
        fs::write(
            repo.join("pipe/Epic.md"),
            "# Epic\n\n## Purpose\n\nWorks.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        // Global param used for main_doc detection; pipeline.json only
        // tested for order_error suppression here.
        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "Epic.md").unwrap();
        assert!(tree.order.is_none());
        assert!(tree.order_error.is_none());
        let md = tree.main_doc.expect("main_doc present");
        assert_eq!(md.purpose_text.as_deref(), Some("Works."));
    }

    #[test]
    fn main_doc_purpose_text_capped_at_500_chars() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let long_text = "A".repeat(800);
        let content = format!("# Epic\n\n## Purpose\n\n{}\n", long_text);
        fs::write(repo.join("pipe/README.md"), content).unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        let purpose = md.purpose_text.expect("purpose_text present");
        assert_eq!(purpose.chars().count(), 500);
    }

    #[test]
    fn main_doc_no_purpose_section() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(
            repo.join("pipe/README.md"),
            "# Epic\n\nNo purpose heading here.\n",
        );

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.purpose_text.is_none());
    }

    #[test]
    fn main_doc_case_insensitive() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        fs::write(
            repo.join("pipe/readme.md"),
            "# R\n\n## Purpose\n\nLowercase.\n",
        )
        .unwrap();

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert_eq!(md.purpose_text.as_deref(), Some("Lowercase."));
    }

    #[test]
    fn extract_section_parameterized_story_still_works() {
        // R-14: story_text exercised via meta_one (extract_story_meta) — the
        // scanner no longer surfaces it on StoryRef.
        let m = meta_one("# Title\n\n## Story\n\nAs a user, I want X.\n\n## Context\n");
        assert_eq!(m.story_text.as_deref(), Some("As a user, I want X."));
    }

    #[test]
    fn extract_section_purpose_heading() {
        let text = "# Title\n\n## Purpose\n\nThis does Y.\n\n## Details\n";
        let result = pipeline_format::markdown::extract_section(text, "Purpose", None);
        assert_eq!(result.as_deref(), Some("This does Y."));
    }

    #[test]
    fn main_doc_still_excluded_from_stories() {
        // Regression: README must NOT appear in stories[] even when main_doc is populated.
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(repo.join("pipe/README.md"), "# R\n");
        write_story_md(repo.join("pipe/index.md"), "# I\n");
        write_story_md(repo.join("pipe/INDEX.md"), "# I\n");
        write_story_md(repo.join("pipe/readme.md"), "# r\n");
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        assert_eq!(tree.stories[0].file_name, "story.md");
        // But main_doc IS populated.
        assert!(tree.main_doc.is_some());
    }

    // R.7: HTML main doc detection
    #[test]
    fn main_doc_readme_html_detected() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        let html = r#"<html><head>
<meta name="cs:supporting-docs" content="../spec.md, ./api.html">
</head><body>
<section data-cs="purpose">This HTML epic does X.</section>
</body></html>"#;
        fs::write(repo.join("pipe/README.html"), html).unwrap();
        write_story_md(repo.join("pipe/story.md"), "# Story\n");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.stories.len(), 1);
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("README.html"));
        assert_eq!(md.purpose_text.as_deref(), Some("This HTML epic does X."));
        assert_eq!(
            md.supporting_docs.as_deref(),
            Some(&["../spec.md".to_string(), "./api.html".to_string()][..])
        );
    }

    #[test]
    fn main_doc_md_wins_over_html_readme() {
        // readme.md takes priority over readme.html (MAIN_DOC_DEFAULTS order)
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe")).unwrap();
        write_story_md(
            repo.join("pipe/README.md"),
            "# R\n\n## Purpose\n\nFrom MD.\n",
        );
        fs::write(
            repo.join("pipe/README.html"),
            "<section data-cs=\"purpose\">From HTML.</section>",
        )
        .unwrap();
        write_story_md(repo.join("pipe/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        let md = tree.main_doc.expect("main_doc present");
        assert!(md.file_path.ends_with("README.md"));
        assert_eq!(md.purpose_text.as_deref(), Some("From MD."));
    }

    #[test]
    fn main_doc_in_sub_epic() {
        let (_td, repo) = fix_repo();
        fs::create_dir_all(repo.join("pipe/sub-epic")).unwrap();
        fs::write(
            repo.join("pipe/sub-epic/README.md"),
            "# Sub\n\n## Purpose\n\nNested.\n",
        )
        .unwrap();
        write_story_md(repo.join("pipe/sub-epic/story.md"), "x");

        let tree = scan_pipeline_tree_impl(&repo, "pipe", "", "").unwrap();
        assert_eq!(tree.sub_epics.len(), 1);
        let sub = &tree.sub_epics[0];
        let md = sub.main_doc.as_ref().expect("sub-epic main_doc");
        assert_eq!(md.purpose_text.as_deref(), Some("Nested."));
    }

    /// RF-16 Site 3 (`parse_pipeline_json` step validation): measure the
    /// `O(steps × stories)` reference-resolution + dup-detection loop. Every
    /// step is a unique, resolvable `storyFile` so the loop runs to completion
    /// (no early error return). Reports per-parse wall-time at small/large N;
    /// machine-dependent → `#[ignore]`d. Run:
    /// `cargo test -p codesense --lib rf16_measure_pipeline -- --ignored --nocapture`.
    #[test]
    #[ignore = "perf bench — run with --ignored --nocapture"]
    fn rf16_measure_pipeline_step_validation() {
        fn story(i: usize) -> StoryRef {
            StoryRef {
                file_name: format!("s{i}.md"),
                file_path: format!("/x/s{i}.md"),
                depends_on: None,
                tags: None,
                status: Some("todo".into()),
                document_type: Some("story".into()),
                supporting_docs: None,
                story_id: Some(format!("S-{i}")),
                source_repo: None,
                team: None,
                sprint: None,
                points: None,
                assigned_to: None,
                rank: None,
                metadata_valid: true,
                metadata_invalid_reason: None,
                open_tags: std::collections::BTreeMap::new(),
                mtime: None,
            }
        }
        for &n in &[10usize, 50, 200] {
            let stories: Vec<StoryRef> = (0..n).map(story).collect();
            let steps: String = (0..n)
                .map(|i| format!("{{\"storyFile\":\"s{i}.md\"}}"))
                .collect::<Vec<_>>()
                .join(",");
            let content = format!("{{\"version\":1,\"steps\":[{steps}]}}");
            let sub_epics: Vec<DerivedPipelineTree> = vec![];
            const REPS: u32 = 500;
            let t = std::time::Instant::now();
            let mut sink = 0usize;
            for _ in 0..REPS {
                let spec =
                    parse_pipeline_json(&content, &stories, &sub_epics).expect("valid manifest");
                sink += spec.steps.len();
            }
            let per = t.elapsed().as_micros() as f64 / REPS as f64;
            println!(
                "RF-16 pipeline validate: N={n:3} steps × {n:3} stories → \
                 parse_pipeline_json {per:8.1} µs/call (steps={})",
                sink / REPS as usize
            );
        }
    }
}

/// The atomic, compare-and-swap status writer (`write_story_status`) and the user status move
/// (`move_story_status`).
#[cfg(test)]
mod gt10_status_writer_tests {
    use super::*;
    use std::fs;

    fn story_at(status: &str) -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("s.md");
        fs::write(
            &path,
            format!("---\ntype: story\nstatus: {status}\n---\n# S\n"),
        )
        .unwrap();
        (td, path)
    }

    #[test]
    fn writer_flips_status() {
        let (_td, path) = story_at("in-progress");
        let wrote = write_story_status(&path, "review", None, None).unwrap();
        assert!(wrote);
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("status: review"));
    }

    #[test]
    fn terminal_advance_persists_done_without_stamping_completed_at() {
        // A terminal advance to `done` persists the status and writes NO
        // `done.completed_at` (SR-13 retired the auto-stamp).
        let (_td, path) = story_at("review");
        write_story_status(&path, "done", None, Some("review")).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("status: done"));
        assert!(
            !content.contains("completed_at"),
            "terminal advance must not stamp completed_at"
        );
    }

    #[test]
    fn cas_no_ops_when_current_status_no_longer_matches() {
        // AC-5: the story already moved past `from` (replay / double-advance) → the
        // compare-and-swap skips the write and leaves the file byte-identical.
        let (_td, path) = story_at("review");
        let orig = fs::read_to_string(&path).unwrap();
        let wrote = write_story_status(&path, "done", None, Some("in-progress")).unwrap();
        assert!(!wrote, "CAS must not write when current != expected `from`");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            orig,
            "file left untouched"
        );
    }

    #[test]
    fn writer_refuses_control_chars_and_leaves_frontmatter_unchanged() {
        // A newline in the value would inject a second frontmatter line
        // (`story_id: X`); any control character is refused before the write.
        let (_td, path) = story_at("review");
        let orig = fs::read_to_string(&path).unwrap();
        for bad in ["done\nstory_id: X", "done\r", "do\tne", "done\0"] {
            let err = write_story_status(&path, bad, None, None).unwrap_err();
            assert!(err.contains("control character"), "{err}");
        }
        let err = write_story_status(&path, "done", Some("a\nstory_id: X"), None).unwrap_err();
        assert!(err.contains("control character"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), orig, "file untouched");
    }

    #[test]
    fn cas_writes_when_current_status_matches() {
        let (_td, path) = story_at("in-progress");
        let wrote = write_story_status(&path, "review", None, Some("in-progress")).unwrap();
        assert!(wrote);
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("status: review"));
    }

    #[test]
    fn move_story_status_writes_any_status_and_no_ops_on_same() {
        // No gate and no taxonomy: a move to an off-taxonomy status is written as given, and a
        // repeat move to the same status is a no-op success.
        let (_td, path) = story_at("review");
        move_story_status(&path, "staging", Some("m")).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("status: staging"));
        assert!(content.contains("rank: m"));
        move_story_status(&path, "staging", None).unwrap();
    }
}

#[cfg(test)]
mod navigation_scan_tests {
    use super::*;

    #[test]
    fn navigation_missing_context_is_unavailable_not_empty() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-content");
        let result = scan_pipeline_tree_and_heal_index(
            None,
            &missing,
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        );
        assert!(
            result.is_err(),
            "missing content must not masquerade as an empty team"
        );
    }
    #[test]
    fn navigation_deadline_discards_incomplete_traversal() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("notes.md"), "# Notes").unwrap();
        let result = scan_pipeline_tree_with_deadline(
            temp.path(),
            "",
            "",
            "",
            &default_summary_headings(),
            None,
            &MetadataValidityConfig::default(),
            Some(std::time::Instant::now()),
        );
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn navigation_unreadable_pipeline_file_is_not_reclassified_as_plain_content() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("story.md");
        fs::write(
            &file,
            "---\ntype: story\nstatus: ready-for-dev\n---\n# Story",
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
        let result = scan_pipeline_tree_and_heal_index(
            None,
            temp.path(),
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.unwrap_err().contains("Failed to read content file"));
    }

    #[cfg(unix)]
    #[test]
    fn navigation_subtree_read_failure_is_not_partial_success() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o0)).unwrap();
        let result = scan_pipeline_tree_and_heal_index(
            None,
            temp.path(),
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().contains("Failed to read directory"));
    }

    #[cfg(unix)]
    #[test]
    fn unshared_inherited_directories_do_not_interrupt_the_child_overlay() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("org.yml"),
            "slug: acme/context\ndisplay_name: Context\ncreated_at: '2026-01-01T00:00:00Z'\n",
        )
        .unwrap();
        fs::write(temp.path().join("teams.yml"), "teams:\n- name: parent\n  source_repos: []\n- name: child\n  source_repos: []\n  parent_team: parent\n").unwrap();
        fs::write(
            temp.path().join("people.yml"),
            "people:\n  - github_user_id: 1\n    github_username: alice\n",
        )
        .unwrap();
        let child = temp.path().join("child/content");
        let locked = temp.path().join("parent/content/locked");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o0)).unwrap();
        let result = scan_pipeline_tree_and_heal_index(
            None,
            &child,
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            result.unwrap().docs.is_empty(),
            "a missing sharing declaration contributes no inherited files"
        );
        let parent = temp.path().join("parent");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o0)).unwrap();
        let result = scan_pipeline_tree_and_heal_index(
            None,
            &child,
            "",
            "",
            "",
            &default_summary_headings(),
            &MetadataValidityConfig::default(),
        );
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            result.unwrap().docs.is_empty(),
            "an unreadable parent declaration shares nothing without interrupting the child"
        );
    }
}
