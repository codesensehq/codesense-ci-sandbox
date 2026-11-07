// SPDX-License-Identifier: MIT
//! Authoring discoverability & validation — the pure logic behind
//! the three read-only MCP tools (`get_story_format`, `list_source_repos`,
//! `validate_story`).
//!
//! **Why a separate module.** Everything here is pure over its inputs (parsed
//! frontmatter + the configured repo/team sets + an optional filesystem anchor
//! for existence checks). No `AppHandle`, no in-memory singletons — so the
//! whole verdict engine is unit-testable without a running app, and
//! `handler.rs` keeps three thin `#[tool]` wrappers that only resolve context.
//!
//! **DRY.** The vocabulary is the *existing* crate consts —
//! [`PIPELINE_DOCUMENT_TYPES`] (the closed `type` enum) and [`VALID_STATUSES`]
//! (the closed `status` enum) — and the parse is the *existing*
//! [`pipeline_format::detect_format`] + [`pipeline_format::extract_meta`] +
//! [`FrontmatterFields::is_pipeline_story`]. This module NEVER re-parses or
//! re-lists; it is the synchronous, author-facing projection of the same rules
//! `build_cache_entry` / `classify_orphan` apply at index time. If this
//! module's verdict and the cache's behavior ever disagree, this module is
//! wrong.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::commands::frontmatter_validation as fv;
use crate::commands::pipeline_format::PIPELINE_DOCUMENT_TYPES;
use crate::commands::pipeline_format::{self, FrontmatterFields, PipelineFileFormat};
use crate::commands::pipeline_scanner::{STORY_META_READ_CAP, VALID_STATUSES};

/// Story-frontmatter key names — the presence of any of these makes an
/// otherwise-broken frontmatter block *story intent*, so the malformed-YAML
/// gate fires only for authors who meant to write a story (never nagging a
/// plain doc). Kept in the module so the gate and the signal check
/// agree on "what a story field looks like".
const STORY_KEY_NAMES: &[&str] = &[
    "type",
    "source_repo",
    "story_id",
    "status",
    "depends_on",
    "supporting_docs",
    "team",
];

/// Truncate `text` to at most `STORY_META_READ_CAP` bytes on a char boundary —
/// the window `pipeline_scanner::extract_story_meta` reads. Parsing the same
/// bytes as the cache keeps the tool's verdict and the engine's behavior in
/// lock-step: a story whose frontmatter runs past the cap is dropped by
/// the index, and the validator sees the same truncated block. (The cache uses
/// `from_utf8_lossy` on a raw byte buffer, so at a multibyte char straddling
/// the exact cap the two can differ by 1–3 bytes — immaterial, since both agree
/// that frontmatter must fully fit under the cap.)
fn read_window(text: &str) -> &str {
    if text.len() <= STORY_META_READ_CAP {
        return text;
    }
    let mut end = STORY_META_READ_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

// ----- Finding taxonomy (stable codes; published by `get_story_format`) ------

/// A single `validate_story` finding. Serialized as
/// `{ code, field, severity, message, hint }`.
struct Finding {
    code: &'static str,
    field: &'static str,
    severity: Severity,
    message: String,
    hint: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Blocking,
    Advisory,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Blocking => "blocking",
            Severity::Advisory => "advisory",
        }
    }
}

impl Finding {
    fn to_value(&self) -> Value {
        json!({
            "code": self.code,
            "field": self.field,
            "severity": self.severity.as_str(),
            "message": self.message,
            "hint": self.hint,
        })
    }
}

// Stable finding codes. These strings are a contract an agent may branch on —
// they are published verbatim in `get_story_format.finding_codes` and must not
// change once shipped.
const MISSING_SOURCE_REPO: &str = "MISSING_SOURCE_REPO";
const UNKNOWN_SOURCE_REPO: &str = "UNKNOWN_SOURCE_REPO";
// Advisory — `source_repo` is a configured repo but not one of the parent epic's
// `repos:`. The story still indexes and launches; the epic just doesn't claim it.
const SOURCE_REPO_NOT_IN_EPIC: &str = "SOURCE_REPO_NOT_IN_EPIC";
// Advisory — `work_repo` is set but not a syntactic owner/repo (it need not be a configured repo).
const INVALID_WORK_REPO: &str = "INVALID_WORK_REPO";
const INVALID_TYPE: &str = "INVALID_TYPE";
const INVALID_STATUS: &str = "INVALID_STATUS";
const UNKNOWN_TEAM: &str = "UNKNOWN_TEAM";
const UNRESOLVABLE_DEPENDS_ON: &str = "UNRESOLVABLE_DEPENDS_ON";
const UNPARSEABLE_FRONTMATTER: &str = "UNPARSEABLE_FRONTMATTER";
const MISSING_STATUS: &str = "MISSING_STATUS";
const MISSING_STORY_ID: &str = "MISSING_STORY_ID";
const SUPPORTING_DOC_NOT_FOUND: &str = "SUPPORTING_DOC_NOT_FOUND";
const DEPENDS_ON_UNCHECKED: &str = "DEPENDS_ON_UNCHECKED";
// Advisory — a pipeline story whose body has no extractable summary
// section (`## H2` / `<section data-cs>`) renders a BLANK pipeline card.
const MISSING_SUMMARY_HEADING: &str = "MISSING_SUMMARY_HEADING";
// Blocking — the candidate's `story_id` already belongs to a
// DIFFERENT indexed story in the org. story_id is the durable identity the
// worktree/doc linkage keys on, so a per-org collision must be
// rejected at the authoring boundary, not degraded silently at resolve time.
const DUPLICATE_STORY_ID: &str = "DUPLICATE_STORY_ID";
// Advisory (soft) lint — a `points` value off the Fibonacci scale
// (`1·2·3·5·8·13`). The scanner stores raw, so this is where an
// out-of-scale estimate is surfaced instead of silently rendered.
const OFF_FIBONACCI_POINTS: &str = "OFF_FIBONACCI_POINTS";
// Advisory — a `depends_on` entry that RESOLVES but is a path or
// bare filename stem rather than a stable `story_id`. Location-coupled: it breaks
// when the target story or its epic directory moves. Nudges authors toward the
// move-proof `story_id` form. Resolvers still accept path/stem forms
// (back-compat) — this only warns.
const LOCATION_COUPLED_DEPENDS_ON: &str = "LOCATION_COUPLED_DEPENDS_ON";
// Advisory — `assigned_to` is set to a login not in the org
// roster (`people.yml`). Assignment is soft (the board falls back to initials
// and any org member is assignable — team membership is not enforced), so an
// unknown assignee is drift to surface, not a reason to reject authoring.
const ASSIGNED_TO_UNKNOWN: &str = "ASSIGNED_TO_UNKNOWN";
// Blocking — a metadata field the team's field schema declares
// `required: true` is absent from the candidate. Presence-only (the value is never
// type-checked). The default schema declares NO required fields, so this is a no-op
// until a team opts in; the advisory "unknown field" half is confined to the opt-in
// pre-run report (`field_schema::validate_run_metadata`) so authoring an ordinary
// story is never nagged about routine keys (`author`, `parent_epic`, …).
const MISSING_REQUIRED_FIELD: &str = "MISSING_REQUIRED_FIELD";

// ----- Configured (dynamic) inputs -------------------------------------------

/// The config-limited sets `validate_story` checks against, resolved by the
/// handler from `load_org` before calling in. Borrowed slices keep this pure.
/// RF-perf-9: the seam the `DUPLICATE_STORY_ID` check uses to find the paths that
/// already declare a given `story_id`. A **point lookup**, not a full enumeration —
/// production backs it with the frontmatter cache's `O(1)` aux index (`by_story_id`),
/// so `validate_story` no longer builds+scans an org-wide `Vec<(String, PathBuf)>` on
/// every call (the measured RF-perf-9 #1 hot path). Tests back it with a plain slice.
pub trait StoryIdIndex {
    /// Absolute paths of indexed entries declaring `story_id` (empty = none).
    fn paths_for_story_id(&self, story_id: &str) -> Vec<PathBuf>;
}

/// A `StoryIdIndex` that knows nothing — used when the frontmatter cache is not yet
/// built, so the uniqueness check degrades to "not checked" (never a false green).
pub struct NoStoryIdIndex;

impl StoryIdIndex for NoStoryIdIndex {
    fn paths_for_story_id(&self, _story_id: &str) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// The `assigned_to → is-this-login-in-the-roster` seam that
/// drives the `ASSIGNED_TO_UNKNOWN` advisory. Mirrors [`StoryIdIndex`]: a
/// borrowed trait object keeps this module pure and — crucially — lets the
/// handler back it with the org roster resolver (`Org::person_by_username`) so
/// login normalization has exactly one home (never re-derived here, where it
/// could drift from the Rust/TS twins).
pub trait AssigneeResolver {
    /// True when `login` resolves to a person in the org roster (`people.yml`).
    fn is_known_login(&self, login: &str) -> bool;

    /// True when a canonical, provider-independent roster id resolves.
    fn is_known_person_id(&self, _person_id: &str) -> bool {
        false
    }
}

/// An `AssigneeResolver` that treats every login as known — used when the org
/// roster is unavailable, so the check degrades to "not checked" (never a
/// false-positive drift finding). Mirrors [`NoStoryIdIndex`].
pub struct NoAssigneeResolver;

impl AssigneeResolver for NoAssigneeResolver {
    fn is_known_login(&self, _login: &str) -> bool {
        true
    }

    fn is_known_person_id(&self, _person_id: &str) -> bool {
        true
    }
}

pub struct ValidatorConfig<'a> {
    /// Every configured `owner/repo` slug across the org's teams (the set a
    /// `source_repo` must belong to). Deduped by the caller.
    pub configured_repos: &'a [String],
    /// Every configured team name (the set a `team:` field must belong to).
    pub valid_teams: &'a [String],
    /// The active team's `source_repos` — inlined into `MISSING`/`UNKNOWN`
    /// source-repo hints as the "what a story authored now should use"
    /// candidates. May be empty.
    pub active_team_candidates: &'a [String],
    /// The parent epic's `repos:` (the nearest enclosing epic doc that declares
    /// any). Empty = the epic names no repos, so no epic-membership finding.
    pub epic_repos: &'a [String],
    /// The EFFECTIVE (config-resolved) summary-section headings the
    /// pipeline card extracts its summary from, resolved by the handler via
    /// `system::read_configured_summary_headings`. Drives the
    /// `MISSING_SUMMARY_HEADING` advisory through the SAME matcher the scanner
    /// uses, so a clean verdict ⟺ a non-blank card. Never empty (the resolver
    /// falls back to the defaults).
    pub summary_headings: &'a [String],
    /// The team's canonical status ids, resolved by the handler from the
    /// status taxonomy (`status_taxonomy::load_status_taxonomy_from`). The
    /// `INVALID_STATUS` advisory check is exact membership in this set — so a
    /// team's custom states validate clean and only a genuinely-unknown status is
    /// flagged. Never empty (the loader falls back to the shipped
    /// defaults, giving zero-config parity).
    pub valid_statuses: &'a [String],
    /// The `story_id → paths` point-lookup seam that drives
    /// the `DUPLICATE_STORY_ID` blocking finding. The handler backs it with the org-wide
    /// frontmatter cache's `O(1)` aux index (from a lock-free snapshot); the candidate's
    /// own path is excluded by the validator (self-validation of an on-disk story is not
    /// a collision). A [`NoStoryIdIndex`] when the cache is unavailable → the uniqueness
    /// check is skipped (degrades to "not checked", never a false green — mirrors the
    /// `depends_on` no-anchor posture).
    pub story_id_index: &'a dyn StoryIdIndex,
    /// The roster seam driving the `ASSIGNED_TO_UNKNOWN` advisory.
    /// The handler backs it with the loaded `Org` (delegating to the
    /// `person_by_username` resolver); a [`NoAssigneeResolver`] when the roster
    /// is unavailable → the assignee check is skipped (never a false green).
    pub assignee_resolver: &'a dyn AssigneeResolver,
    /// The team's declared metadata field schema (official ∪ custom),
    /// resolved by the handler from `field_schema::load_field_schema_from`. Drives the
    /// blocking `MISSING_REQUIRED_FIELD` check (a declared `required` field the
    /// candidate lacks). Never empty (the loader falls back to the official baseline),
    /// and the default baseline declares no required fields → no finding until a team
    /// opts in (zero regression for existing teams).
    pub schema: &'a crate::commands::field_schema::FieldSchema,
}

/// Story frontmatter still names repositories as `owner/repo`. Keep that
/// compatibility value only when it identifies one canonical repository within
/// the team whose story may consume it. Equal slugs in different teams remain
/// valid because team selection disambiguates them.
pub(crate) fn unambiguous_story_repos(team: &crate::domain::Team) -> Vec<String> {
    let identities = team.source_repos.iter().fold(
        HashMap::<String, std::collections::HashSet<String>>::new(),
        |mut acc, repo| {
            acc.entry(repo.to_string().to_ascii_lowercase())
                .or_default()
                .insert(repo.remote_key());
            acc
        },
    );
    team.source_repos
        .iter()
        .filter(|repo| {
            identities
                .get(&repo.to_string().to_ascii_lowercase())
                .is_some_and(|keys| keys.len() == 1)
        })
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn configured_story_repos(teams: &[crate::domain::Team]) -> Vec<String> {
    let mut repos: Vec<String> = teams.iter().flat_map(unambiguous_story_repos).collect();
    repos.sort();
    repos.dedup();
    repos
}

// ----- Detected kind ---------------------------------------------------------

/// Comma-join any slice of string-likes — `&[&str]` (the static field enums) and
/// `&[String]` (the dynamically-resolved status set) share one joiner.
fn comma_list<S: AsRef<str>>(items: &[S]) -> String {
    items
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ")
}

fn quoted_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("`{}`", s))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A file whose stem is `readme`/`index` **and** which carries a pipeline
/// extension (`.md`/`.html`/`.htm`) is an epic main doc — classified by
/// filename, never by `type:`. The extension guard mirrors the scanner's
/// `is_pipeline_document` discriminator so a non-pipeline `index.json` is NOT
/// waved through as an unvalidated epic doc.
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

/// Format detection: prefer the path extension; otherwise sniff the content
/// (an HTML doc opens with `<` or carries a `cs:*` meta tag). Defaults to
/// Markdown — the dominant authoring format.
fn detect_input_format(text: &str, path: Option<&Path>) -> PipelineFileFormat {
    if let Some(p) = path {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if let Some(fmt) = pipeline_format::detect_format(ext) {
                return fmt;
            }
        }
    }
    let trimmed = text
        .trim_start()
        .trim_start_matches('\u{feff}')
        .trim_start();
    // A leading `---` fence is unambiguously Markdown frontmatter — decide that
    // before the `<`-sniff so a story pasted as inline `content` is never
    // mis-parsed under HTML rules.
    if trimmed.starts_with("---") {
        return PipelineFileFormat::Markdown;
    }
    if trimmed.starts_with('<') || trimmed.contains("name=\"cs:") || trimmed.contains("name='cs:") {
        PipelineFileFormat::Html
    } else {
        PipelineFileFormat::Markdown
    }
}

/// Does the candidate carry a *pipeline signal* — i.e. is it a story attempt?
///
/// - When an explicit `type:` is present, defer entirely to `is_pipeline_story()`:
///   only `story|bug|spike` are stories. An explicit non-story `type` (e.g.
///   `doc`, `retro`, `epic`) is a deliberate opt-out and stays a plain doc —
///   so `type: doc` + `source_repo` is NOT nagged with `INVALID_TYPE` (this
///   matches the scanner's type-only routing).
/// - When there is NO `type:`, `source_repo`/`story_id` (fields meaningless on a
///   plain doc) signal a story-in-progress that simply forgot its `type`.
///
/// Deliberately never triggers on "has a frontmatter block at all" — a file
/// whose YAML is entirely some other tool's keys parses to empty
/// `FrontmatterFields` and stays a plain doc (the cardinal rule).
fn has_pipeline_signal(fields: &FrontmatterFields) -> bool {
    match fields.document_type.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => fields.is_pipeline_story(),
        _ => fields.source_repo.is_some() || fields.story_id.is_some(),
    }
}

/// Does a run of frontmatter lines contain a recognizable story-frontmatter key
/// (`type:`, `source_repo:`, …) at the **top level**? This is what makes a
/// broken block *story intent* — the malformed-frontmatter gate fires only when
/// this is true, so a plain doc that merely opens with `---` (or carries some
/// other tool's YAML) is never nagged.
///
/// The key must be at **column 0** (no leading indentation): the strict parser
/// binds only top-level keys, so an *indented child* key named like a story
/// field (e.g. `type:` nested under a foreign `image:` block, as in Jekyll/Hugo
/// frontmatter) is NOT story intent and must not be flagged. The
/// `rest.trim_start().starts_with(':')` guard also rejects prefix collisions
/// (`types:`, `status_note:`).
fn region_has_story_key(region: &str) -> bool {
    region.lines().any(|line| {
        STORY_KEY_NAMES.iter().any(|k| {
            line.strip_prefix(k)
                .map(|rest| rest.trim_start().starts_with(':'))
                .unwrap_or(false)
        })
    })
}

/// Is `trimmed` (already left-trimmed) a YAML `key:` line — an identifier token
/// (`[A-Za-z0-9_.-]+`) immediately followed by a colon? Used to tell genuine
/// frontmatter from prose in an unterminated block.
fn is_key_line(trimmed: &str) -> bool {
    let key_len = trimmed
        .char_indices()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    key_len > 0 && trimmed[key_len..].trim_start().starts_with(':')
}

/// The story-scoped malformed-frontmatter gate (markdown). `gray_matter`
/// silently swallows a broken, unterminated, or wrong-shape block into
/// `default()` — which would let an intended story pass as a zero-frontmatter
/// plain doc (a false green). This is called ONLY when the
/// parser produced nothing (`FrontmatterFields::is_empty()`), so it never
/// second-guesses a block that parsed fine (e.g. a valid `type: doc`); and it
/// fires ONLY when the frontmatter region shows story intent, so a plain doc
/// that opens with a `---` thematic break — or has a `key:` line in its body —
/// is never false-flagged.
///
/// Region gathering differs by termination:
/// - **Terminated** — everything up to the closing `---` IS frontmatter, so the
///   whole block is scanned (blank lines and `#` YAML comments within it are
///   fine).
/// - **Unterminated** — there is no delimiter, so to separate "story that forgot
///   its closing fence" from "plain doc that opens with a `---` rule" we skip
///   leading blank / `#`-comment lines, then require the first substantive line
///   to be a `key:` line and take the contiguous key run. Prose or a heading as
///   the first real line ⇒ not frontmatter ⇒ no finding.
///
/// Opener/closer semantics are parser-faithful: the opener must be `---` at byte
/// 0 (mirrors `markdown::extract_meta_fields`' `starts_with("---")`) with the
/// first line right-trimming to `---`; the closer is a line that right-trims to
/// `---` (mirrors `gray_matter`'s `trim_end() == delimiter`, so `...` and a
/// leading-space fence are NOT closers).
fn malformed_story_frontmatter(text: &str) -> Option<Finding> {
    let text = text.trim_start_matches('\u{feff}');
    if !text.starts_with("---") {
        return None; // no frontmatter opener at byte 0 → genuine plain content
    }
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None; // e.g. `---foo` on the first line — not a fence
    }
    let terminated = text.lines().skip(1).any(|l| l.trim_end() == "---");

    let mut region = String::new();
    if terminated {
        for line in lines {
            if line.trim_end() == "---" {
                break;
            }
            region.push_str(line);
            region.push('\n');
        }
    } else {
        let mut started = false;
        for line in lines {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                if started {
                    break; // blank/comment after the key run ends the block
                }
                continue; // skip leading blanks / comments
            }
            if !is_key_line(t) {
                break; // first substantive line is prose → not frontmatter
            }
            started = true;
            region.push_str(line);
            region.push('\n');
        }
    }

    if !region_has_story_key(&region) {
        return None;
    }

    // Story intent, but the parser extracted nothing → the block is malformed,
    // unterminated, or a wrong YAML shape. Message on whether a closer exists.
    let (message, hint) = if terminated {
        (
            "the frontmatter block has story fields but did not parse — check the YAML shape (e.g. a list like `depends_on` must be `[a, b]` or block items)".to_string(),
            "fix the frontmatter YAML so the story fields parse; then re-validate".to_string(),
        )
    } else {
        (
            "the file opens a `---` frontmatter block that has no closing `---` fence".to_string(),
            "close the frontmatter with a line containing only `---`; then re-validate".to_string(),
        )
    };
    Some(Finding {
        code: UNPARSEABLE_FRONTMATTER,
        field: "frontmatter",
        severity: Severity::Blocking,
        message,
        hint,
    })
}

// ----- The verdict engine ----------------------------------------------------

/// Validate a candidate file's content. Returns the full `validate_story`
/// payload. Pure: `path` is used ONLY to (a) detect format /
/// epic-main-doc by name and (b) anchor `depends_on` / `supporting_docs`
/// existence checks; when `None`, those checks degrade to a `DEPENDS_ON_UNCHECKED`
/// advisory — never a false green, never a false blocking error.
/// Normalize a path for identity comparison. Best-effort: `canonicalize` when
/// the file exists (the EDIT flow — an on-disk candidate re-validating itself
/// resolves to the exact absolute form the cache stored, so it self-excludes),
/// else lexical absolutization (the CREATE flow — the target file is not on
/// disk yet, so there is no self entry to exclude and lexical form suffices).
/// Lexical absolutization — no disk access, no symlink resolution. A relative
/// path is joined to the cwd; an absolute one is returned as-is.
fn lexical_abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// True when `a` and `b` denote the SAME file for story-identity self-exclusion.
/// Robust to the EDIT-vs-CREATE normalization asymmetry: a path
/// present on disk resolves through `canonicalize` (symlinks + `.`/`..`), while
/// an as-yet-unwritten CREATE-flow path only resolves lexically. Match on EITHER
/// the lexical forms (raw string identity — covers the CREATE self/same-target
/// case and identical strings) OR the canonical forms (both on disk — covers an
/// EDIT-flow candidate whose cache entry was stored through a symlinked prefix,
/// e.g. macOS `/var`→`/private/var`). So a story never false-positives against
/// its own cache entry regardless of which side canonicalizes.
fn same_identity_path(a: &Path, b: &Path) -> bool {
    if lexical_abs(a) == lexical_abs(b) {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(ca), Ok(cb)) if ca == cb
    )
}

/// The indexed absolute paths — OTHER than the candidate's own —
/// that already declare `story_id`. Self-exclusion uses `same_identity_path` so
/// an on-disk candidate never collides with its own cache entry. A non-empty
/// result is a per-org collision → blocking `DUPLICATE_STORY_ID`. Per-org scope
/// is inherited from the cache snapshot: the managed frontmatter cache is built
/// from a single active `org_root` (`registry_local`/boot), so `indexed` only
/// ever carries the active org's entries — no explicit org filter needed here.
fn colliding_story_id_paths(
    story_id: &str,
    candidate: &Path,
    index: &dyn StoryIdIndex,
) -> Vec<PathBuf> {
    // RF-perf-9: O(1) point lookup, then exclude the candidate's own on-disk path.
    index
        .paths_for_story_id(story_id)
        .into_iter()
        .filter(|p| !same_identity_path(p, candidate))
        .collect()
}

pub fn validate_candidate(text: &str, path: Option<&Path>, cfg: &ValidatorConfig) -> Value {
    validate_candidate_for(text, path, cfg, &Default::default())
}

pub(crate) fn validate_candidate_for(
    text: &str,
    path: Option<&Path>,
    cfg: &ValidatorConfig,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Value {
    // Scanner parity: evaluate exactly the byte window the scanner indexes.
    let text = read_window(text);
    let format = detect_input_format(text, path);

    // Epic main docs are recognized (so they are not nagged as stories) but
    // not deep-validated in MVP (deferred: epic-only field validation).
    if let Some(p) = path {
        if is_epic_main_doc_name(p) {
            return build_response(
                DetectedKind::EpicMainDoc,
                format,
                vec![],
                Some("README/index files are epic main docs, identified by filename, not `type:`. They are not pipeline stories and are not validated as such."),
                cfg.summary_headings,
            );
        }
    }

    let parsed = (format == PipelineFileFormat::Markdown)
        .then(|| pipeline_format::markdown::read_frontmatter(text));
    let fields = parsed
        .as_ref()
        .map(|p| p.fields.clone())
        .unwrap_or_else(|| pipeline_format::extract_meta(format, text));
    let mut findings: Vec<Finding> = Vec::new();
    if let Some(parsed) = &parsed {
        if !parsed.block_parse_ok {
            if parsed.repaired_lines.is_empty() {
                if let Some(finding) = malformed_story_frontmatter(text) {
                    return build_response(
                        DetectedKind::PipelineStory,
                        format,
                        vec![finding],
                        None,
                        cfg.summary_headings,
                    );
                }
            } else {
                for line in &parsed.repaired_lines {
                    let blocking =
                        pipeline_format::METADATA_CONTRACT_FIELDS.contains(&line.key.as_str());
                    findings.push(Finding {
                        code: if blocking {
                            UNPARSEABLE_FRONTMATTER
                        } else {
                            fv::UNQUOTED_VALUE
                        },
                        field: "frontmatter",
                        severity: if blocking {
                            Severity::Blocking
                        } else {
                            Severity::Advisory
                        },
                        message: fv::unquoted_value_message(line, ": "),
                        hint: "quote the value, then re-validate".to_string(),
                    });
                }
            }
        }
        for line in fv::unquoted_hash_lines(text) {
            findings.push(Finding {
                code: fv::UNQUOTED_VALUE,
                field: "frontmatter",
                severity: Severity::Advisory,
                message: fv::unquoted_value_message(&line, " #"),
                hint: "quote the value to keep the text after the hash".to_string(),
            });
        }
    }

    // A repaired type still needs validation, even if its recovered value is
    // outside the story vocabulary. A deliberate, well-formed type opts out.
    let type_was_repaired = parsed
        .as_ref()
        .is_some_and(|p| p.repaired_lines.iter().any(|line| line.key == "type"));
    if !has_pipeline_signal(&fields) && !type_was_repaired {
        return build_response(
            DetectedKind::PlainDoc,
            format,
            vec![],
            Some("No pipeline signal (no story `type`, `source_repo`, or `story_id`). This is valid content — it projects into the file tree and is never a pipeline card. Do not add `cs:*` frontmatter."),
            cfg.summary_headings,
        );
    }

    // Pipeline-story candidate → full-depth validation.

    // type ∈ story|bug|spike. Reachable here only when `type:` is absent (an
    // explicit non-story `type` is classified a plain doc upstream by
    // `has_pipeline_signal`), so this is the "started a story via source_repo /
    // story_id but forgot the type" case.
    if !fields.is_pipeline_story() {
        let message = match fields.document_type.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => format!("`type: {}` is not a pipeline story type", t),
            _ => "no `type:` field — a pipeline story needs one".to_string(),
        };
        findings.push(Finding {
            code: INVALID_TYPE,
            field: "type",
            severity: Severity::Blocking,
            message,
            hint: format!(
                "`type` must be one of (case-insensitive): {}",
                comma_list(PIPELINE_DOCUMENT_TYPES)
            ),
        });
    }

    // source_repo required + configured. Presence/requiredness is MCP-specific
    // (a story MUST carry one); the *value* check (syntactic owner/repo AND in
    // the configured set) is the shared predicate `fv::source_repo_value_ok`
    // so the nav badge, band, and this verdict can never disagree on validity.
    match fields.source_repo.as_deref().map(str::trim) {
        None | Some("") => findings.push(Finding {
            code: MISSING_SOURCE_REPO,
            field: "source_repo",
            severity: Severity::Blocking,
            message: "a pipeline story requires `source_repo` (owner/repo) — without it the file is silently dropped from the pipeline".to_string(),
            hint: source_repo_hint(cfg),
        }),
        Some(slug) if !fv::source_repo_value_ok(slug, cfg.configured_repos) => findings.push(Finding {
            code: UNKNOWN_SOURCE_REPO,
            field: "source_repo",
            severity: Severity::Blocking,
            message: format!("`source_repo: {}` is not a configured repo", slug),
            hint: source_repo_hint(cfg),
        }),
        Some(slug)
            if !cfg.epic_repos.is_empty() && !cfg.epic_repos.iter().any(|r| r == slug) =>
        {
            findings.push(Finding {
                code: SOURCE_REPO_NOT_IN_EPIC,
                field: "source_repo",
                severity: Severity::Advisory,
                message: format!("`source_repo: {slug}` is not one of this epic's repos"),
                hint: format!("the epic's `repos:` are: {}", quoted_list(cfg.epic_repos)),
            })
        }
        Some(_) => {}
    }

    if let Some(work) = Option::filter(Option::map(Option::as_deref(&fields.work_repo), str::trim), |w| !str::is_empty(w)) {
        if crate::domain::SourceRepoSlug::parse_owner_repo(work).is_none() {
            findings.push(Finding {
                code: INVALID_WORK_REPO,
                field: "work_repo",
                severity: Severity::Advisory,
                message: format!("`work_repo: {work}` is not an owner/repo slug"),
                hint: "use owner/repo, or drop the key to read `source_repo`".to_string(),
            });
        }
    }

    // Status is free text; compare with the team taxonomy for an advisory.
    // Checked against the resolved taxonomy ids (`cfg.valid_statuses`) with
    // STRICT exact match — no case-fold, no trim beyond the parser's —
    // so custom states validate clean and only a genuinely-unknown status is flagged
    // Both authoring validation and the metadata band treat drift as advisory.
    match fields.status.as_deref().map(str::trim) {
        None | Some("") => findings.push(Finding {
            code: MISSING_STATUS,
            field: "status",
            severity: Severity::Advisory,
            message: "no `status:` — the card will have no pipeline column".to_string(),
            hint: format!("suggested statuses: {}", comma_list(cfg.valid_statuses)),
        }),
        Some(s) if !cfg.valid_statuses.iter().any(|v| v == s) => findings.push(Finding {
            code: INVALID_STATUS,
            field: "status",
            severity: Severity::Advisory,
            message: format!("`status: {}` is not a valid pipeline status", s),
            hint: format!("suggested statuses: {}", comma_list(cfg.valid_statuses)),
        }),
        Some(_) => {}
    }

    // Required-field check against the team's declared field schema.
    // Presence-only (values are never type-checked). Reuses the pure
    // `field_schema::validate_story_fields` over the candidate's raw metadata keys
    // (the same seam the pre-run report uses). With no run context here, only the
    // BLOCKING `missing_required` bucket is surfaced — referenced-field gaps
    // (`{fieldName}` vs the story) are the runner's pre-run gate, and the advisory
    // `extra_unknown` half is confined to that opt-in report so an ordinary story is
    // never nagged. The default schema declares no required fields → no-op until a
    // team opts in.
    let present_keys = pipeline_format::extract_metadata_keys(format, text);
    let field_report = crate::commands::field_schema::validate_story_fields(
        &present_keys,
        cfg.schema,
        &std::collections::BTreeSet::new(),
    );
    for name in &field_report.missing_required {
        findings.push(Finding {
            code: MISSING_REQUIRED_FIELD,
            field: "frontmatter",
            severity: Severity::Blocking,
            message: format!("required metadata field `{}` is missing", name),
            hint: format!(
                "the team field schema declares `{}` as `required` — add it or drop the requirement",
                name
            ),
        });
    }

    // story_id: advisory
    if fields
        .story_id
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        findings.push(Finding {
            code: MISSING_STORY_ID,
            field: "story_id",
            severity: Severity::Advisory,
            message: "no `story_id:` — the story has no stable display label".to_string(),
            hint: "add a short id like \"ABC-1\"".to_string(),
        });
    } else if let (Some(id), Some(cand_path)) = (
        fields
            .story_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        path,
    ) {
        // story_id uniqueness. Only checked when we have BOTH a
        // non-empty id AND a candidate path to self-exclude by — content-only
        // validation cannot tell a re-validated self from a real collision, so
        // it degrades to "not checked" (like `depends_on` without a path
        // anchor) rather than risk a false positive.
        let collisions = colliding_story_id_paths(id, cand_path, cfg.story_id_index);
        if !collisions.is_empty() {
            let where_used = collisions
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            findings.push(Finding {
                code: DUPLICATE_STORY_ID,
                field: "story_id",
                severity: Severity::Blocking,
                message: format!(
                    "`story_id: {}` is already used by a different story ({}) — story_id must be unique per org",
                    id, where_used
                ),
                hint: "pick a story_id not already in use in this org (see `get_story_format`), or — if you meant to relocate this story — keep the id and move the file so its workspace reconnects".to_string(),
            });
        }
    }

    // team: if present must be a configured team. Value check shared via
    // `fv::team_value_ok` (empty valid_teams ⇒ "unknown" ⇒ not flagged).
    if let Some(team) = fields.team.as_deref().map(str::trim) {
        if !team.is_empty() && !fv::team_value_ok(team, cfg.valid_teams) {
            findings.push(Finding {
                code: UNKNOWN_TEAM,
                field: "team",
                severity: Severity::Blocking,
                message: format!("`team: {}` is not a configured team", team),
                hint: if cfg.valid_teams.is_empty() {
                    "call `list_source_repos` — its `byTeam` list names the configured teams"
                        .to_string()
                } else {
                    format!(
                        "available teams: {} (also via `list_source_repos`)",
                        quoted_list(cfg.valid_teams)
                    )
                },
            });
        }
    }

    // assigned_to: optional (absent = unassigned, a first-class state).
    // When present it must resolve to an org-roster login via the roster
    // resolver behind `assignee_resolver`. Advisory, not blocking:
    // an unknown assignee is drift, not a broken card. When the roster is
    // unavailable the resolver treats every login as known, so the check is
    // silently skipped (never a false-positive drift finding).
    if let Some(reference) = fields.assigned_to.as_deref().map(str::trim) {
        let known = reference.strip_prefix("person:").map_or_else(
            || cfg.assignee_resolver.is_known_login(reference),
            |id| !id.is_empty() && cfg.assignee_resolver.is_known_person_id(id),
        );
        if !reference.is_empty() && !known {
            findings.push(Finding {
                code: ASSIGNED_TO_UNKNOWN,
                field: "assigned_to",
                severity: Severity::Advisory,
                message: format!("`assigned_to: {}` is not a known org member", reference),
                hint: "use a canonical `person:<id>` reference (or a legacy GitHub login) from the org roster, or omit `assigned_to` to leave the story unassigned".to_string(),
            });
        }
    }

    // depends_on: resolve against the filesystem anchor
    if let Some(deps) = fields.depends_on.as_ref() {
        let deps: Vec<&String> = deps.iter().filter(|d| !d.trim().is_empty()).collect();
        if !deps.is_empty() {
            match path.and_then(|p| p.parent()) {
                None => findings.push(Finding {
                    code: DEPENDS_ON_UNCHECKED,
                    field: "depends_on",
                    severity: Severity::Advisory,
                    message: format!(
                        "{} dependency reference(s) could not be checked — no file `path` to anchor resolution",
                        deps.len()
                    ),
                    hint: "pass `path` (not just inline `content`) so depends_on can be resolved against the filesystem".to_string(),
                }),
                Some(dir) => {
                    for dep in deps {
                        let entry = dep.trim();
                        if !dependency_resolves(entry, dir, cfg.story_id_index, scope) {
                            findings.push(Finding {
                                code: UNRESOLVABLE_DEPENDS_ON,
                                field: "depends_on",
                                severity: Severity::Blocking,
                                message: format!("`depends_on` entry `{}` does not resolve to an existing file", entry),
                                hint: "each entry must be a path (relative to this file's directory) or the id/filename of a sibling story in the same epic".to_string(),
                            });
                        } else if cfg.story_id_index.paths_for_story_id(entry).len() != 1 {
                            // It resolves, but NOT via a unique
                            // `story_id` — so it's a path or bare filename stem,
                            // which breaks if the target story or its epic moves.
                            findings.push(Finding {
                                code: LOCATION_COUPLED_DEPENDS_ON,
                                field: "depends_on",
                                severity: Severity::Advisory,
                                message: format!(
                                    "`depends_on` entry `{}` is location-coupled (a path or filename stem) — it breaks if the target story or its epic directory moves",
                                    entry
                                ),
                                hint: "reference the target's stable `story_id` instead — it resolves globally and survives a move".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // points: off-Fibonacci soft lint. The scanner stores raw
    // ; this is where an out-of-scale estimate is surfaced. Absent /
    // unparseable `points` → `None` → no finding (tolerant).
    if let Some(p) = fields.points {
        if !pipeline_format::FIBONACCI_POINTS.contains(&p) {
            findings.push(Finding {
                code: OFF_FIBONACCI_POINTS,
                field: "points",
                severity: Severity::Advisory,
                message: format!("`points: {}` is off the Fibonacci scale (1·2·3·5·8·13)", p),
                hint: "estimate with a Fibonacci value: 1, 2, 3, 5, 8, or 13".to_string(),
            });
        }
    }

    // supporting_docs: advisory existence check (path-anchored only)
    if let (Some(docs), Some(dir)) = (
        fields.supporting_docs.as_ref(),
        path.and_then(|p| p.parent()),
    ) {
        for doc in docs.iter().filter(|d| !d.trim().is_empty()) {
            if !scope.allows(&dir.join(doc.trim())) || !dir.join(doc.trim()).exists() {
                findings.push(Finding {
                    code: SUPPORTING_DOC_NOT_FOUND,
                    field: "supporting_docs",
                    severity: Severity::Advisory,
                    message: format!(
                        "supporting doc `{}` was not found next to this file",
                        doc.trim()
                    ),
                    hint: "supporting_docs paths are resolved relative to this file's directory"
                        .to_string(),
                });
            }
        }
    }

    // Summary-heading advisory. The pipeline card summary is the body
    // under the FIRST matching configured heading — `## H2` (Markdown) or
    // `<section data-cs>` (HTML), format-dispatched by the SAME
    // `pipeline_format::extract_first_section` the scanner feeds
    // `get_story_details`/`extract_body`. If it yields nothing the card renders
    // BLANK, so advise (never block). Keying off `is_none()` (not literal
    // heading presence) is exact render parity: it also fires when a matching
    // heading has an empty body (which also renders blank). Runs on the same
    // 4KB window (`read_window` above) `read_first_chunk` extracts from, so the
    // verdict and the render agree — modulo the same immaterial 1–3 byte
    // multibyte-straddle at the exact cap that `read_window`'s doc already
    // notes (both agree a summary heading must fall within the first 4KB).
    if pipeline_format::extract_first_section(format, text, cfg.summary_headings, None).is_none() {
        findings.push(summary_heading_finding(format, cfg.summary_headings));
    }

    build_response(
        DetectedKind::PipelineStory,
        format,
        findings,
        None,
        cfg.summary_headings,
    )
}

/// Build the `MISSING_SUMMARY_HEADING` advisory, naming the effective
/// heading order and a concrete "add this" hint in the candidate's OWN format
/// (`## X` for Markdown, `<section data-cs="X">` for HTML) so an `.html` author
/// is never handed a Markdown-only fix.
fn summary_heading_finding(format: PipelineFileFormat, effective: &[String]) -> Finding {
    let first = first_summary_heading(effective);
    let form = match format {
        PipelineFileFormat::Markdown => md_summary_marker(first),
        PipelineFileFormat::Html => html_summary_marker(first),
    };
    Finding {
        code: MISSING_SUMMARY_HEADING,
        field: "body",
        severity: Severity::Advisory,
        message: format!(
            "no summary section — the pipeline card will render blank; add the first matching heading (first match wins): {}",
            quoted_list(effective)
        ),
        hint: format!("add `{}` with a 1–2 sentence summary", form),
    }
}

#[derive(Clone, Copy)]
enum DetectedKind {
    PlainDoc,
    PipelineStory,
    EpicMainDoc,
}

impl DetectedKind {
    fn as_str(self) -> &'static str {
        match self {
            DetectedKind::PlainDoc => "plain_doc",
            DetectedKind::PipelineStory => "pipeline_story",
            DetectedKind::EpicMainDoc => "epic_main_doc",
        }
    }
}

fn format_str(fmt: PipelineFileFormat) -> &'static str {
    match fmt {
        PipelineFileFormat::Markdown => "markdown",
        PipelineFileFormat::Html => "html",
    }
}

fn source_repo_hint(cfg: &ValidatorConfig) -> String {
    if cfg.active_team_candidates.is_empty() {
        "call `list_source_repos` for the configured `owner/repo` values — never invent one"
            .to_string()
    } else {
        format!(
            "call `list_source_repos` for the configured values (never invent one); the active team's repos are: {}",
            quoted_list(cfg.active_team_candidates)
        )
    }
}

/// The single response builder for all three detected kinds. Sorts findings
/// blocks-before-warnings (stable), sets `valid` iff there are zero blocking
/// findings, attaches an optional explanatory `note`, and — on any blocking
/// finding — inlines the `get_story_format` contract so the fix material rides
/// with the rejection. One home for the envelope shape so plain-doc,
/// pipeline-story, and epic-main-doc responses can never diverge.
fn build_response(
    kind: DetectedKind,
    format: PipelineFileFormat,
    mut findings: Vec<Finding>,
    note: Option<&str>,
    // The effective summary headings, so the contract inlined on a
    // blocking finding shows the SAME config-driven summary-heading section
    // `get_story_format` returns (no drift between the two contract surfaces).
    summary_headings: &[String],
) -> Value {
    findings.sort_by_key(|f| match f.severity {
        Severity::Blocking => 0u8,
        Severity::Advisory => 1u8,
    });
    let has_blocking = findings.iter().any(|f| f.severity == Severity::Blocking);
    let findings_json: Vec<Value> = findings.iter().map(Finding::to_value).collect();

    let mut payload = json!({
        "detectedKind": kind.as_str(),
        "format": format_str(format),
        "valid": !has_blocking,
        "findings": findings_json,
        "see_also": ["get_story_format"],
    });
    let obj = payload.as_object_mut().unwrap();
    if let Some(n) = note {
        obj.insert("note".to_string(), json!(n));
    }
    if has_blocking {
        obj.insert(
            "contract".to_string(),
            story_format_payload(summary_headings),
        );
    }
    payload
}

// ----- depends_on resolution -------------------------------------------------

/// Does a `depends_on` entry resolve? Tries, in order:
///
/// 1. the entry as a path relative to `anchor_dir` (with `.md`/`.html`/`.htm`
///    appended if it has no extension);
/// 2. a GLOBAL `story_id` match against the org index — a bare id
///    resolves regardless of directory, so the DAG survives a nav
///    reorg. A duplicated id still counts as resolved here (the ref points at a
///    real story); the collision is surfaced separately by `DUPLICATE_STORY_ID`;
/// 3. a bounded search of the enclosing epic subtree for a story file whose stem
///    equals the entry or whose filename starts with it (case-insensitive) — the
///    location-COUPLED back-compat path for bare stems not yet on the story_id
///    form (no mass rewrite required).
///
/// Bounded (depth-capped, story-extensions only) so it is cheap and never
/// blocks; a miss yields a single blocking finding at the call site.
fn dependency_resolves(
    entry: &str,
    anchor_dir: &Path,
    story_id_index: &dyn StoryIdIndex,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> bool {
    // (1) direct path forms
    let direct = anchor_dir.join(entry);
    if scope.allows(&direct) && direct.exists() {
        return true;
    }
    if direct.extension().is_none() {
        for ext in pipeline_format::PIPELINE_EXTENSIONS {
            if scope.allows(&anchor_dir.join(format!("{}.{}", entry, ext)))
                && anchor_dir.join(format!("{}.{}", entry, ext)).exists()
            {
                return true;
            }
        }
    }

    // (2) Global story_id resolution — the location-independent tier.
    // Resolve ONLY on a UNIQUE match: a duplicated (ambiguous) id is not soundly
    // resolvable, so — consistent with the runtime resolvers' poison-on-duplicate
    // — it does NOT resolve here and falls through (the collision itself is
    // surfaced as `DUPLICATE_STORY_ID` on the stories that own the id). This keeps
    // the validator's verdict in lock-step with the graph/relationship resolvers.
    if story_id_index.paths_for_story_id(entry).len() == 1 {
        return true;
    }

    // (3) bounded search of the epic subtree for a matching story filename
    let root = epic_search_root(anchor_dir, scope);
    let needle = entry.to_ascii_lowercase();
    search_for_story(&root, &needle, 0, scope)
}

/// The nearest ancestor of `dir` (including `dir`) that contains a README/index
/// main doc — the epic root — or `dir` itself if none is found within a small
/// cap. Bounds the dependency search scope.
fn epic_search_root(
    dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> std::path::PathBuf {
    let mut current = dir;
    for _ in 0..6 {
        for name in ["README.md", "readme.md", "index.md", "README.html"] {
            if scope.allows(&current.join(name)) && current.join(name).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    dir.to_path_buf()
}

/// Depth-capped recursive scan for a story file whose stem == `needle` or whose
/// filename starts with `needle` (both lowercased). Story extensions only.
fn search_for_story(
    dir: &Path,
    needle: &str,
    depth: usize,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> bool {
    if depth > 5 || !scope.visible(dir) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !scope.visible(&path) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            subdirs.push(path);
            continue;
        }
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let lower = fname.to_ascii_lowercase();
        let is_story_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                pipeline_format::PIPELINE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())
            })
            .unwrap_or(false);
        if !is_story_ext {
            continue;
        }
        let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
        // Exact stem, or the id followed by a `-` separator (so `sw-mcp-1`
        // matches `sw-mcp-1-title.md` but NOT `sw-mcp-10-title.md`).
        if stem == needle || stem.starts_with(&format!("{needle}-")) {
            return true;
        }
    }
    for sub in subdirs {
        if search_for_story(&sub, needle, depth + 1, scope) {
            return true;
        }
    }
    false
}

// ----- Tool 1 payload: get_story_format --------------------------------------

/// Build the `get_story_format` response — the decision-first orientation
/// contract. Fixed enums are pulled LIVE from the Rust consts so
/// they can never drift from the parser; dynamic sets (`source_repo`, `team`)
/// are taught by reference to `list_source_repos`, never inlined as
/// authoritative.
pub fn story_format_payload(summary_headings: &[String]) -> Value {
    json!({
        "role": "Entry point for authoring CodeSense content. Decide FIRST whether you even need frontmatter; most content is a plain doc and needs none.",
        // `decision` is an ARRAY, not an object: array order is guaranteed on
        // the wire regardless of any serde map-ordering feature, so the
        // load-bearing signal — plain_doc FIRST (most content needs no
        // frontmatter) — can never be reordered into alphabetical keys.
        "decision": [
            {
                "kind": "plain_doc",
                "when": "Reference material, notes, READMEs, architecture, UX — anything that is NOT a tracked unit of pipeline work.",
                "frontmatter_required": false,
                "guidance": "Write normal Markdown or HTML. Add NO `cs:*` frontmatter. `source_repo` does not apply. A plain doc can never be 'invalid' for missing frontmatter.",
                "example": "# Architecture Notes\n\nJust write content. No frontmatter block at all."
            },
            {
                "kind": "pipeline_story",
                "when": "A tracked unit of work with a type, status, and (optionally) dependencies — it should appear as a pipeline card.",
                "frontmatter_required": true,
                "continue_to": "pipeline_story_format"
            },
            {
                "kind": "epic_main_doc",
                "when": "A file named README/index that holds epic-level metadata.",
                "note": "Identified by FILENAME (readme/index), not `type:`. Carries epic-only fields (repos, branch). Never a pipeline card. If you are editing a README, you are here — do not add story fields."
            },
            {
                "kind": "live_data_doc",
                "when": "An HTML dashboard that reads local context and cached routine results.",
                "frontmatter_required": false,
                "guidance": "Call `get_live_data_contract` for local query and routine SDK authoring. Read cached data through `routineResult`/`routineStatus`; use an explicit, authorized `routineRefresh` action to launch collection. External service access belongs to agent-owned tools, which publish routine results. Keep credentials out of the page. Run `validate_live_data_doc` before writing it.",
                "continue_to_tool": "get_live_data_contract"
            }
        ],
        "pipeline_story_format": {
            "note": "Every field below shows its Markdown (YAML) name and its HTML `<meta name=\"cs:*\">` equivalent side by side. Both formats validate identically.",
            "quoting": "Put values containing `: ` or ` #` in double quotes. Example: title: \"Inbox: Fix #42\"",
            "fields": pipeline_format::METADATA_CONTRACT_FIELDS.iter().map(|name| {
                let description = match *name {
                    "type" => "The story type. Fixed closed set.",
                    "source_repo" => "owner/repo. MUST be a configured repo — resolve it, never invent it.",
                    "status" => "Pipeline column. Free text; unusual values are advisory.",
                    "depends_on" => "List of story ids (HTML: comma-separated). Must resolve.",
                    "team" => "Team name. If set, MUST be a configured team.",
                    "work_repo" => "Optional owner/repo holding the work when it is not `source_repo` (e.g. the context repo); read for story_changes.",
                    _ => "Metadata CodeSense reads for display, routing or flow.",
                };
                let cs_meta = pipeline_format::yaml_to_cs_name(name).map(str::to_string)
                    .unwrap_or_else(|| format!("cs:{}", name.replace('_', "-")));
                field_entry(name, &cs_meta, matches!(*name, "type" | "source_repo"), description)
            }).collect::<Vec<_>>(),
            "enums": {
                "type": PIPELINE_DOCUMENT_TYPES,
                "status": VALID_STATUSES
            },
            "dynamic_fields": {
                "source_repo": { "resolve_via": "list_source_repos", "note": "never inline a repo list as authoritative" },
                "team": { "resolve_via": "list_source_repos", "note": "the byTeam list names configured teams" }
            }
        },
        "summary_headings": summary_headings_contract(summary_headings),
        "finding_codes": finding_codes(),
        "related_tools": {
            "list_source_repos": "Call to get the valid `source_repo` values (and configured teams) — never invent one.",
            "validate_story": "Call BEFORE writing the file (pass `path` or inline `content`) to catch problems in one round.",
            "refresh_frontmatter_cache": "Call AFTER dropping a new story MD into a content layer via a non-CodeSense writer (e.g. your own filesystem `Write`). Not needed when writing via `move_content_file`, `delete_content_file`, or CodeSense's own file APIs — those refresh automatically.",
            "get_live_data_contract": "Authoring an HTML dashboard? Read the local query, action verb and routine result SDK contract. Passive reads display cached snapshots; explicit routine refresh uses existing launch authorization. External collection runs through agent-owned tools.",
            "validate_live_data_doc": "Validate an HTML dashboard before writing it. Check local query, action verb and routine SDK usage, credential exclusion, and safe rendering.",
        },
        "examples": {
            // The story examples carry a first-heading summary block
            // (`## <heading>` / `<section data-cs>`), keyed off the current
            // config, so an agent that copies an example ships a NON-blank card.
            "markdown_story": format!(
                "---\ntype: story\nstatus: todo\nstory_id: ABC-1\nsource_repo: owner/repo\ndepends_on: []\n---\n\n# Story title\n\n## {}\n\nA 1–2 sentence summary that becomes the pipeline card blurb.",
                first_summary_heading(summary_headings)
            ),
            "html_story": format!(
                "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:status\" content=\"todo\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n<title>Story title</title>\n<section data-cs=\"{}\">A 1–2 sentence summary that becomes the pipeline card blurb.</section>",
                first_summary_heading(summary_headings)
            ),
            "plain_doc": "# Any Document\n\nNo frontmatter. Valid content. Never a pipeline card."
        }
    })
}

/// The first effective summary heading (config-driven), defaulting to
/// `Purpose` only if an (impossible) empty list is passed — the resolver never
/// returns empty. One helper so the payload section, the examples, and the
/// `validate_story` hint all name the same heading.
fn first_summary_heading(effective: &[String]) -> &str {
    effective.first().map(String::as_str).unwrap_or("Purpose")
}

/// The terse "add this heading" markers — one definition each so the
/// `validate_story` hint and the `get_story_format` hint never spell the syntax
/// differently. `…` stands in for the author's 1–2 sentence summary prose.
fn md_summary_marker(heading: &str) -> String {
    format!("## {}", heading)
}
fn html_summary_marker(heading: &str) -> String {
    format!("<section data-cs=\"{}\">…</section>", heading)
}

/// The `summary_headings` contract section of `get_story_format`. The
/// listed headings are the EFFECTIVE, config-resolved list — not a
/// literal — and both the Markdown and HTML forms are shown side by side
/// , never an HTML appendix. Matches the render rule the pipeline card
/// applies: first-match-wins, H2-only, case-insensitive, blank if none match.
fn summary_headings_contract(effective: &[String]) -> Value {
    let first = first_summary_heading(effective);
    json!({
        "note": "The pipeline card summary is the body text under the FIRST matching H2 heading below (first match wins). No matching heading — or a matching heading with an empty body — renders a BLANK card. This list reflects THIS install's current config.",
        "effective_headings": effective,
        "contract": {
            "first_match_wins": true,
            "h2_only": true,
            "case_insensitive": true,
            "blank_card_if_none_match": true
        },
        "markdown": format!("## {}\n\nA 1–2 sentence summary that becomes the card blurb.", first),
        "html": format!("<section data-cs=\"{}\">A 1–2 sentence summary that becomes the card blurb.</section>", first),
        "hint": format!(
            "Add `{}` (Markdown) or `{}` (HTML) with a 1–2 sentence summary so the pipeline card is not blank.",
            md_summary_marker(first),
            html_summary_marker(first)
        )
    })
}

fn field_entry(name: &str, cs_meta: &str, required: bool, description: &str) -> Value {
    json!({
        "name": name,
        "cs_meta": cs_meta,
        "required": required,
        "description": description,
    })
}

/// The published failure taxonomy. Order: blocking first.
fn finding_codes() -> Value {
    json!([
        code_entry(fv::UNQUOTED_VALUE, "advisory", "A plain value needs quotes to preserve its colon or hash text."),
        code_entry(
            UNPARSEABLE_FRONTMATTER,
            "blocking",
            "The frontmatter block is not valid YAML."
        ),
        code_entry(
            MISSING_SOURCE_REPO,
            "blocking",
            "A pipeline story has no `source_repo` — it would be silently dropped."
        ),
        code_entry(
            UNKNOWN_SOURCE_REPO,
            "blocking",
            "`source_repo` is not a configured repo."
        ),
        code_entry(
            INVALID_WORK_REPO,
            "advisory",
            "`work_repo` is set but not an owner/repo slug."
        ),
        code_entry(
            INVALID_TYPE,
            "blocking",
            "`type` is absent or not one of story|bug|spike."
        ),
        code_entry(
            INVALID_STATUS,
            "advisory",
            "`status` is set but not a canonical status."
        ),
        code_entry(
            UNKNOWN_TEAM,
            "blocking",
            "`team` is set but not a configured team."
        ),
        code_entry(
            UNRESOLVABLE_DEPENDS_ON,
            "blocking",
            "A `depends_on` entry does not resolve to an existing file."
        ),
        code_entry(
            DUPLICATE_STORY_ID,
            "blocking",
            "`story_id` is already used by a different story in this org — it must be unique per org."
        ),
        code_entry(
            SOURCE_REPO_NOT_IN_EPIC,
            "advisory",
            "`source_repo` is configured but not one of the parent epic's `repos:`."
        ),
        code_entry(
            MISSING_STATUS,
            "advisory",
            "No `status:` — the card has no column."
        ),
        code_entry(
            MISSING_STORY_ID,
            "advisory",
            "No `story_id:` — no stable display label."
        ),
        code_entry(
            SUPPORTING_DOC_NOT_FOUND,
            "advisory",
            "A `supporting_docs` path was not found."
        ),
        code_entry(
            DEPENDS_ON_UNCHECKED,
            "advisory",
            "`depends_on` could not be checked (no path anchor) — not a pass."
        ),
        code_entry(
            MISSING_SUMMARY_HEADING,
            "advisory",
            "No configured summary heading (or an empty one) — the pipeline card renders blank."
        ),
        code_entry(
            OFF_FIBONACCI_POINTS,
            "advisory",
            "`points` is off the Fibonacci scale (1·2·3·5·8·13) — an out-of-scale estimate."
        ),
        code_entry(
            LOCATION_COUPLED_DEPENDS_ON,
            "advisory",
            "A `depends_on` entry is a path or filename stem, not a stable `story_id` — it breaks if the target moves."
        ),
        code_entry(
            ASSIGNED_TO_UNKNOWN,
            "advisory",
            "`assigned_to` is set to a login not in the org roster (`people.yml`)."
        )
    ])
}

fn code_entry(code: &str, severity: &str, meaning: &str) -> Value {
    json!({ "code": code, "severity": severity, "meaning": meaning })
}

// ----- Tool 2 payload: list_source_repos -------------------------------------

/// Build the `list_source_repos` response from the resolved org: the active
/// team's repos (primary) plus the full org listing grouped by team.
/// Pure over already-resolved data so it is unit-testable.
pub fn source_repos_payload(
    active_org: &str,
    active_team: &str,
    teams: &[(String, Vec<String>)],
) -> Value {
    let primary: Vec<&String> = teams
        .iter()
        .find(|(name, _)| name == active_team)
        .map(|(_, repos)| repos.iter().collect())
        .unwrap_or_default();
    let by_team: Vec<Value> = teams
        .iter()
        .map(|(name, repos)| json!({ "team": name, "sourceRepos": repos }))
        .collect();
    json!({
        "activeOrg": active_org,
        "activeTeam": active_team,
        "primary": primary,
        "byTeam": by_team,
        "note": "`primary` = the active team's repos, what a story authored now should use. `byTeam` = the full org listing for cross-team pairing.",
        "see_also": ["get_story_format"],
    })
}

/// The connection a source repo is bound to, for the MCP
/// payload. Provider-neutral — a repo bound to ANY forge (not just GitHub)
/// enriches `accountsByRepo`, so a downstream PR-opening agent can pick the right
/// identity per repo regardless of provider. `account` is the provider's own
/// account key (GitHub renders its `u64` user id to string; another forge its
/// opaque id — never re-parsed to `u64`); `display` is the best human label
/// (handle / display name), falling back to the account key when identity-less.
#[derive(Debug, Clone)]
pub struct RepoAccount {
    pub provider: String,
    pub account: String,
    pub display: String,
}

/// Map each configured `owner/repo` slug to its bound GitHub account
/// (or `null` when unbound / unattached), for the additive `accountsByRepo`
/// field of `list_source_repos`. So a downstream PR-opening agent can pick the
/// right identity per repo.
///
/// **Case:** config slugs (`SourceRepoSlug` `Display`) preserve case, but the
/// attachment lookup (`group_attachments_by_remote`) is keyed lowercase — so
/// the join lowercases the slug. A raw-case lookup would silently miss any
/// non-lowercase configured repo and report it unbound.
///
/// Pure over already-resolved data (the join of the configured slugs and the
/// `lowercase "owner/repo" -> RepoAccount` map the handler builds from the
/// active user's attachments) so it is unit-testable without an `AppHandle`.
pub fn resolve_repo_accounts(
    teams: &[(String, Vec<(String, String)>)],
    account_by_repo_key: &HashMap<String, RepoAccount>,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let mut remote_keys_by_slug: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (_team, repos) in teams {
        for (slug, remote_key) in repos {
            remote_keys_by_slug
                .entry(slug.to_ascii_lowercase())
                .or_default()
                .insert(remote_key.clone());
        }
    }
    for (_team, repos) in teams {
        for (slug, remote_key) in repos {
            let value = account_by_repo_key
                .get(remote_key)
                .map(|a| {
                    json!({ "provider": a.provider, "account": a.account, "display": a.display })
                })
                .unwrap_or(Value::Null);
            out.insert(remote_key.clone(), value.clone());
            // A bare compatibility key has always meant GitHub. Never let a
            // non-GitHub identity consume that namespace.
            if remote_key.starts_with("github.com/")
                && remote_keys_by_slug
                    .get(&slug.to_ascii_lowercase())
                    .is_some_and(|keys| keys.len() == 1)
            {
                out.insert(slug.clone(), value);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_format_lists_the_shared_contract_and_advisory_quoting_code() {
        let payload = story_format_payload(&[]);
        let names: Vec<_> = payload["pipeline_story_format"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, pipeline_format::METADATA_CONTRACT_FIELDS);
        assert!(!names.contains(&"sequence"));
        assert!(!names.contains(&"done"));
        let codes = payload["finding_codes"].as_array().unwrap();
        for code in ["INVALID_STATUS", "UNQUOTED_VALUE"] {
            assert_eq!(
                codes.iter().find(|c| c["code"] == code).unwrap()["severity"],
                "advisory"
            );
        }
        let quoting = payload["pipeline_story_format"]["quoting"]
            .as_str()
            .unwrap();
        assert!(quoting.contains(": ") && quoting.contains(" #"));
    }

    /// The story-format contract no longer teaches the retired
    /// epic `scope_context` / `scope_install` fields.
    #[test]
    fn xs5_story_format_omits_the_retired_epic_scope_fields() {
        let text = story_format_payload(&[]).to_string();
        assert!(!text.contains("scope_context"), "{text}");
        assert!(!text.contains("scope_install"), "{text}");
    }

    /// The default effective headings (`["Purpose","Story"]`) as a `'static`
    /// slice, so `cfg()` can hand them out without a per-call temporary.
    fn default_test_headings() -> &'static [String] {
        static H: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        H.get_or_init(|| vec!["Purpose".to_string(), "Story".to_string()])
            .as_slice()
    }

    /// The shipped default status ids (== `VALID_STATUSES`, the zero-config
    /// taxonomy) as a `'static` slice, so the `cfg*()` helpers hand `valid_statuses`
    /// out without a per-call temporary. Tests exercising custom states pass their
    /// own slice directly to `ValidatorConfig` instead of this default.
    fn default_test_statuses() -> &'static [String] {
        static S: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        S.get_or_init(|| VALID_STATUSES.iter().map(|s| s.to_string()).collect())
            .as_slice()
    }

    /// The shipped default field schema (official baseline, zero required
    /// fields) as a `'static`, so the `cfg*()` helpers hand `schema` out without a
    /// per-call temporary. Tests exercising a team's custom/required fields build
    /// their own `FieldSchema` and pass it to `cfg_schema()`.
    fn default_test_schema() -> &'static crate::commands::field_schema::FieldSchema {
        static S: std::sync::OnceLock<crate::commands::field_schema::FieldSchema> =
            std::sync::OnceLock::new();
        S.get_or_init(crate::commands::field_schema::default_schema)
    }

    /// RF-perf-9: tests back the `StoryIdIndex` seam with a plain `(id, path)` `Vec` —
    /// a linear scan is fine for fixtures, and `&Vec` coerces to `&dyn StoryIdIndex`, so
    /// the collision-test call sites pass their fixture unchanged.
    impl StoryIdIndex for Vec<(String, PathBuf)> {
        fn paths_for_story_id(&self, story_id: &str) -> Vec<PathBuf> {
            self.iter()
                .filter(|(id, _)| id == story_id)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    fn cfg<'a>(repos: &'a [String], teams: &'a [String]) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: default_test_headings(),
            valid_statuses: default_test_statuses(),
            story_id_index: &NoStoryIdIndex,
            assignee_resolver: &NoAssigneeResolver,
            schema: default_test_schema(),
        }
    }

    /// Build a real `Org` roster from a set of logins, so the
    /// assignee tests resolve through the ACTUAL roster resolver
    /// (`Org::person_by_username`, reached via the `AssigneeResolver` impl in
    /// `handler`) — login normalization (trim, one-`@` strip, ASCII case-fold) is
    /// never re-derived here. The resolver's own form/edge coverage lives in
    /// `domain::org`.
    fn roster_org(logins: &[&str]) -> crate::domain::Org {
        let people = logins
            .iter()
            .enumerate()
            .map(|(i, login)| {
                let id = (i + 1) as u64;
                crate::domain::Person::from_identities(
                    crate::domain::PersonId::from(id),
                    vec![crate::domain::Identity {
                        provider: "github".to_string(),
                        handle: Some((*login).to_string()),
                        provider_user_id: Some(id.to_string()),
                    }],
                    *login,
                    None,
                )
            })
            .collect();
        crate::domain::Org::new(
            "test-org",
            "Test Org",
            chrono::Utc::now(),
            None,
            crate::domain::OrgConfigFlags::default(),
            people,
            vec![],
        )
        .unwrap()
    }

    /// `cfg()` with an explicit roster for the `ASSIGNED_TO_UNKNOWN`
    /// tests.
    fn cfg_roster<'a>(
        repos: &'a [String],
        teams: &'a [String],
        roster: &'a dyn AssigneeResolver,
    ) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: default_test_headings(),
            valid_statuses: default_test_statuses(),
            story_id_index: &NoStoryIdIndex,
            assignee_resolver: roster,
            schema: default_test_schema(),
        }
    }

    /// `cfg()` with an explicit effective summary-heading list, for
    /// the config-driven tests.
    fn cfg_h<'a>(
        repos: &'a [String],
        teams: &'a [String],
        headings: &'a [String],
    ) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: headings,
            valid_statuses: default_test_statuses(),
            story_id_index: &NoStoryIdIndex,
            assignee_resolver: &NoAssigneeResolver,
            schema: default_test_schema(),
        }
    }

    /// `cfg()` with an explicit org-wide `(story_id, path)` index,
    /// for the `DUPLICATE_STORY_ID` uniqueness tests.
    fn cfg_ids<'a>(
        repos: &'a [String],
        teams: &'a [String],
        index: &'a dyn StoryIdIndex,
    ) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: default_test_headings(),
            valid_statuses: default_test_statuses(),
            story_id_index: index,
            assignee_resolver: &NoAssigneeResolver,
            schema: default_test_schema(),
        }
    }

    /// `cfg()` with an explicit status set, for the taxonomy-driven
    /// validation tests — a team's custom states validate clean and only a status
    /// absent from the set is flagged. The passed slice stands in for the resolved
    /// status taxonomy the handler supplies in production.
    fn cfg_status<'a>(
        repos: &'a [String],
        teams: &'a [String],
        statuses: &'a [String],
    ) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: default_test_headings(),
            valid_statuses: statuses,
            story_id_index: &NoStoryIdIndex,
            assignee_resolver: &NoAssigneeResolver,
            schema: default_test_schema(),
        }
    }

    /// `cfg()` with an explicit field schema, for the required-field
    /// validation tests. The passed schema stands in for the resolved team
    /// schema the handler supplies in production.
    fn cfg_schema<'a>(
        repos: &'a [String],
        teams: &'a [String],
        schema: &'a crate::commands::field_schema::FieldSchema,
    ) -> ValidatorConfig<'a> {
        ValidatorConfig {
            configured_repos: repos,
            valid_teams: teams,
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: default_test_headings(),
            valid_statuses: default_test_statuses(),
            story_id_index: &NoStoryIdIndex,
            assignee_resolver: &NoAssigneeResolver,
            schema,
        }
    }

    // ----- story_id uniqueness (DUPLICATE_STORY_ID) -----------

    #[test]
    fn duplicate_story_id_blocks_when_id_used_at_a_different_path() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // The org already has the candidate's id at some other absolute path.
        let indexed = vec![(
            "SLW-IN-9".to_string(),
            PathBuf::from("/org/default/content/existing/SLW-IN-9.md"),
        )];
        let text =
            "---\ntype: story\nstatus: todo\nstory_id: SLW-IN-9\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\nx\n";
        // Candidate lives at a DIFFERENT (not-yet-written) path.
        let cand = PathBuf::from("/org/default/content/new/SLW-IN-9.md");
        let v = validate_candidate(text, Some(&cand), &cfg_ids(&repos, &teams, &indexed));
        assert_eq!(v["valid"], false, "a colliding story_id must block");
        assert!(finding_codes_of(&v).contains("DUPLICATE_STORY_ID"));
    }

    #[test]
    fn unique_story_id_is_green() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed = vec![(
            "SLW-IN-8".to_string(),
            PathBuf::from("/org/default/content/other/SLW-IN-8.md"),
        )];
        let text =
            "---\ntype: story\nstatus: todo\nstory_id: SLW-IN-9\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\nx\n";
        let cand = PathBuf::from("/org/default/content/new/SLW-IN-9.md");
        let v = validate_candidate(text, Some(&cand), &cfg_ids(&repos, &teams, &indexed));
        assert_eq!(v["valid"], true, "a unique story_id must be green");
        assert!(!finding_codes_of(&v).contains("DUPLICATE_STORY_ID"));
    }

    #[test]
    fn story_validating_its_own_on_disk_copy_is_not_a_duplicate() {
        // Self-exclusion: an existing file re-validated against an index that
        // includes ITSELF must not flag DUPLICATE_STORY_ID. Uses a real temp
        // file so `canonicalize` resolves both sides to the same absolute form.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SLW-IN-9.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SLW-IN-9\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // Index holds the SAME file (possibly via a non-canonical form).
        let indexed = vec![("SLW-IN-9".to_string(), file.clone())];
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        assert!(
            !finding_codes_of(&v).contains("DUPLICATE_STORY_ID"),
            "a story validating its own copy is not a collision"
        );
        assert_eq!(v["valid"], true);
    }

    #[test]
    fn content_only_validation_skips_duplicate_check() {
        // No path anchor → cannot self-exclude → the uniqueness check is skipped
        // (degrades to "not checked", never a false positive). Mirrors the
        // depends_on no-anchor posture.
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed = vec![(
            "SLW-IN-9".to_string(),
            PathBuf::from("/org/default/content/existing/SLW-IN-9.md"),
        )];
        let text =
            "---\ntype: story\nstatus: todo\nstory_id: SLW-IN-9\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\nx\n";
        let v = validate_candidate(text, None, &cfg_ids(&repos, &teams, &indexed));
        assert!(
            !finding_codes_of(&v).contains("DUPLICATE_STORY_ID"),
            "content-only validation must not flag a duplicate (no self-exclusion possible)"
        );
    }

    // ----- depends_on resolves by GLOBAL story_id -------------

    #[test]
    fn depends_on_resolves_by_global_story_id_not_just_local_path() {
        // A ref to a story_id that lives in a DIFFERENT directory (known to the
        // org index) resolves — it is NOT flagged as an orphan. Without the
        // story_id tier the bounded epic-subtree scan would miss it.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SW-CAND-1.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SW-CAND-1\nsource_repo: owner/repo\ndepends_on:\n  - SW-FAR-9\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // The dependency lives somewhere else entirely in the org.
        let indexed = vec![(
            "SW-FAR-9".to_string(),
            PathBuf::from("/org/somewhere/else/SW-FAR-9-far.md"),
        )];
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        assert!(
            !finding_codes_of(&v).contains("UNRESOLVABLE_DEPENDS_ON"),
            "a depends_on ref matching a known global story_id must resolve, not orphan"
        );
    }

    #[test]
    fn depends_on_orphan_when_no_story_id_and_no_file() {
        // A ref that is neither a resolvable path/sibling NOR a known story_id
        // is a genuine orphan and must still be flagged.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SW-CAND-2.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SW-CAND-2\nsource_repo: owner/repo\ndepends_on:\n  - SW-GHOST-1\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed: Vec<(String, PathBuf)> = vec![]; // empty org index
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        assert!(
            finding_codes_of(&v).contains("UNRESOLVABLE_DEPENDS_ON"),
            "a depends_on ref that matches nothing anywhere must be flagged orphan"
        );
    }

    #[test]
    fn depends_on_to_a_duplicated_story_id_is_unresolvable() {
        // A duplicated (ambiguous) story_id is not soundly resolvable — the
        // validator must NOT wave it through, keeping lock-step with the runtime
        // resolvers' poison-on-duplicate (cross-resolver consistency).
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SW-CAND-3.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SW-CAND-3\nsource_repo: owner/repo\ndepends_on:\n  - SW-DUP-9\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // One id is claimed by TWO different stories → ambiguous.
        let indexed = vec![
            (
                "SW-DUP-9".to_string(),
                PathBuf::from("/org/a/SW-DUP-9-a.md"),
            ),
            (
                "SW-DUP-9".to_string(),
                PathBuf::from("/org/b/SW-DUP-9-b.md"),
            ),
        ];
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        assert!(
            finding_codes_of(&v).contains("UNRESOLVABLE_DEPENDS_ON"),
            "a dep on a duplicated (ambiguous) story_id must not resolve"
        );
    }

    // ----- location-coupled depends_on advisory --------------

    #[test]
    fn location_coupled_depends_on_stem_is_advised() {
        // A dep that RESOLVES via a sibling filename (bare stem) but is not a
        // story_id is location-coupled — advise it (would break on move).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dep-target.md"),
            "---\ntype: story\nstatus: todo\nstory_id: DEP-T\nsource_repo: owner/repo\n---\n\n# T\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let file = dir.path().join("SW-CAND-lc.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SW-CAND-lc\nsource_repo: owner/repo\ndepends_on:\n  - dep-target\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed: Vec<(String, PathBuf)> = vec![]; // `dep-target` is not a story_id
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        let codes = finding_codes_of(&v);
        assert!(
            codes.contains("LOCATION_COUPLED_DEPENDS_ON"),
            "a bare-stem dep that resolves is location-coupled and must be advised"
        );
        assert!(
            !codes.contains("UNRESOLVABLE_DEPENDS_ON"),
            "it does resolve (via the sibling file), so it is not an orphan"
        );
        assert_eq!(v["valid"], true, "advisory only — does not block");
    }

    #[test]
    fn story_id_form_depends_on_is_not_location_coupled() {
        // The move-proof form: a bare story_id that resolves via the global index
        // must NOT be flagged location-coupled.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SW-CAND-ok.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SW-CAND-ok\nsource_repo: owner/repo\ndepends_on:\n  - DEP-9\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed = vec![("DEP-9".to_string(), PathBuf::from("/org/x/DEP-9-foo.md"))];
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&file), &cfg_ids(&repos, &teams, &indexed));
        let codes = finding_codes_of(&v);
        assert!(!codes.contains("LOCATION_COUPLED_DEPENDS_ON"));
        assert!(!codes.contains("UNRESOLVABLE_DEPENDS_ON"));
    }

    #[test]
    fn work_repo_is_a_contract_field_and_only_a_malformed_slug_is_flagged() {
        let fields = story_format_payload(&[])["pipeline_story_format"]["fields"].clone();
        assert!(fields.as_array().unwrap().iter().any(|f| f["name"] == "work_repo" && f["cs_meta"] == "cs:work-repo"));
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let story = |work: &str| {
            format!("---\ntype: story\nstatus: todo\nsource_repo: owner/repo\nwork_repo: {work}\n---\n\n# S\n\n## Purpose\n\nx\n")
        };
        let ok = validate_candidate(&story("acme/codesense-context"), None, &cfg(&repos, &teams));
        assert!(!finding_codes_of(&ok).contains("INVALID_WORK_REPO"));
        let bad = validate_candidate(&story("noslash"), None, &cfg(&repos, &teams));
        assert!(finding_codes_of(&bad).contains("INVALID_WORK_REPO"));
    }

    #[test]
    fn location_coupled_code_is_published_in_finding_codes() {
        let published: Vec<String> = finding_codes()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap().to_string())
            .collect();
        assert!(published.iter().any(|c| c == "LOCATION_COUPLED_DEPENDS_ON"));
    }

    // ----- off-Fibonacci points soft lint --------------------

    #[test]
    fn off_fibonacci_points_is_an_advisory_lint() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text =
            "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\npoints: 4\n---\n\n# S\n\n## Purpose\n\nx\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert!(
            finding_codes_of(&v).contains("OFF_FIBONACCI_POINTS"),
            "points: 4 is off the Fibonacci scale and must be flagged"
        );
        // Advisory only — an off-scale estimate does not block the story.
        assert_eq!(v["valid"], true);
    }

    #[test]
    fn fibonacci_points_is_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for p in ["1", "2", "3", "5", "8", "13"] {
            let text = format!(
                "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\npoints: {p}\n---\n\n# S\n\n## Purpose\n\nx\n"
            );
            let v = validate_candidate(&text, None, &cfg(&repos, &teams));
            assert!(
                !finding_codes_of(&v).contains("OFF_FIBONACCI_POINTS"),
                "points: {p} is on-scale and must be clean"
            );
        }
    }

    #[test]
    fn non_numeric_points_is_tolerated_and_does_not_wipe_other_fields() {
        // A malformed `points` must degrade to "absent" — NOT flag off-scale and,
        // critically, NOT sink the whole frontmatter parse (which would drop
        // source_repo and cascade into unrelated blocking findings).
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text =
            "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\npoints: not-a-number\n---\n\n# S\n\n## Purpose\n\nx\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        let codes = finding_codes_of(&v);
        assert!(
            !codes.contains("OFF_FIBONACCI_POINTS"),
            "unparseable points → no lint"
        );
        assert!(
            !codes.contains("MISSING_SOURCE_REPO"),
            "a bad points value must not wipe source_repo (lenient parse)"
        );
        assert_eq!(v["valid"], true);
    }

    #[test]
    fn off_fibonacci_points_code_is_published_in_finding_codes() {
        let codes = finding_codes();
        let published: Vec<&str> = codes
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap())
            .collect();
        assert!(published.contains(&"OFF_FIBONACCI_POINTS"));
    }

    // Self-exclusion must survive a symlinked prefix (macOS
    // `/var`→`/private/var`), where the candidate and its cache entry have
    // different lexical strings but canonicalize to the same file.
    #[cfg(unix)]
    #[test]
    fn duplicate_check_self_excludes_across_a_symlinked_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let file = real.join("SLW-IN-9.md");
        std::fs::write(
            &file,
            "---\ntype: story\nstatus: todo\nstory_id: SLW-IN-9\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_link = link.join("SLW-IN-9.md"); // same file, different lexical string
        assert_ne!(lexical_abs(&via_link), lexical_abs(&file));
        assert!(same_identity_path(&via_link, &file));

        // Validating via the symlinked path against a cache entry stored at the
        // real path must NOT self-collide.
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let indexed = vec![("SLW-IN-9".to_string(), file.clone())];
        let text = std::fs::read_to_string(&file).unwrap();
        let v = validate_candidate(&text, Some(&via_link), &cfg_ids(&repos, &teams, &indexed));
        assert!(
            !finding_codes_of(&v).contains("DUPLICATE_STORY_ID"),
            "a symlinked path to the story's own file is not a collision"
        );
    }

    #[test]
    fn same_identity_path_rejects_distinct_files() {
        assert!(!same_identity_path(
            Path::new("/a/SLW-IN-9.md"),
            Path::new("/b/SLW-IN-9.md")
        ));
    }

    #[test]
    fn duplicate_story_id_code_is_published_in_finding_codes() {
        let codes = finding_codes();
        let has = codes
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "DUPLICATE_STORY_ID" && c["severity"] == "blocking");
        assert!(
            has,
            "DUPLICATE_STORY_ID must be published as a blocking code"
        );
    }

    // ----- plain doc is never nagged -----------------------------------

    #[test]
    fn zero_frontmatter_is_valid_plain_doc_with_no_findings() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let v = validate_candidate("# Just a doc\n\nHello.", None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "plain_doc");
        assert_eq!(v["valid"], true);
        assert_eq!(v["findings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unrelated_frontmatter_is_still_a_plain_doc() {
        // A frontmatter block that is ALL some other tool's keys → parses to
        // empty FrontmatterFields → plain doc, NOT "missing source_repo".
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\nsome_other_tool: true\ncolor: blue\n---\n\n# Doc\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(
            v["detectedKind"], "plain_doc",
            "unrelated YAML must not be a story"
        );
        assert_eq!(v["valid"], true);
        assert!(v["findings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unquoted_values_report_lines_and_keep_other_validation() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for (line, code, severity) in [
            (
                "title: Inbox: first pass",
                "UNPARSEABLE_FRONTMATTER",
                "blocking",
            ),
            ("title: Inbox: #42", "UNPARSEABLE_FRONTMATTER", "blocking"),
            (
                "title: Inbox: # note",
                "UNPARSEABLE_FRONTMATTER",
                "blocking",
            ),
            (
                "readiness_pass: yes: with notes",
                "UNQUOTED_VALUE",
                "advisory",
            ),
            ("title: Fix #42", "UNQUOTED_VALUE", "advisory"),
            ("title: #42", "UNQUOTED_VALUE", "advisory"),
        ] {
            let text = format!(
                "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n{line}\n---\n# S\n"
            );
            let result = validate_candidate(&text, None, &cfg(&repos, &teams));
            assert_eq!(result["valid"], severity != "blocking", "{line}: {result}");
            let finding = result["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["code"] == code)
                .expect("unquoted value finding");
            assert_eq!(finding["severity"], severity);
            assert!(finding["message"].as_str().unwrap().contains("Line 5 ("));
            assert!(finding["message"].as_str().unwrap().contains("Quote it:"));
        }
        for line in [
            "title: value # note",
            "title: value # note #42",
            "notes: Fix #42",
            "title: \"Fix #42\"",
        ] {
            let text =
                format!("---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n{line}\n---\n");
            let result = validate_candidate(&text, None, &cfg(&repos, &teams));
            assert!(
                !finding_codes_of(&result).contains("UNQUOTED_VALUE"),
                "{result}"
            );
        }
        let text = "---\ntype: story\nstatus: todo # next: review\nsource_repo: owner/repo\ntitle: Inbox # note: first pass\nreadiness_pass: yes: with notes\n---\n";
        let result = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(result["valid"], true, "{result}");
        assert!(!finding_codes_of(&result).contains("UNPARSEABLE_FRONTMATTER"));
        assert!(!finding_codes_of(&result).contains("INVALID_STATUS"));
        assert!(finding_codes_of(&result).contains("UNQUOTED_VALUE"));
        let text = "---\ntype: story\ntitle: a: b\nteam: missing\n---\n";
        let result = validate_candidate(text, None, &cfg(&repos, &teams));
        let codes = finding_codes_of(&result);
        for code in [
            "UNPARSEABLE_FRONTMATTER",
            "MISSING_SOURCE_REPO",
            "UNKNOWN_TEAM",
        ] {
            assert!(codes.contains(code), "{result}");
        }
    }

    #[test]
    fn repaired_type_keeps_its_blocking_diagnostic() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text =
            "---\ntype: story: manual\nstatus: todo\nsource_repo: owner/repo\nstory_id: A\n---\n";
        let result = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(result["valid"], false, "{result}");
        assert!(finding_codes_of(&result).contains("UNPARSEABLE_FRONTMATTER"));
        assert!(finding_codes_of(&result).contains("INVALID_TYPE"));
    }

    // ----- full-depth validation of a pipeline story -------------------

    #[test]
    fn story_missing_source_repo_blocks() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nstatus: todo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "pipeline_story");
        assert_eq!(v["valid"], false);
        let codes = finding_codes_of(&v);
        assert!(codes.contains("MISSING_SOURCE_REPO"));
        // contract rides with a blocking rejection
        assert!(v.get("contract").is_some());
    }

    #[test]
    fn story_unknown_source_repo_and_bad_status() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // Valid story `type` so it reaches full validation; unknown repo + bad status.
        let text = "---\ntype: story\nstatus: nonsense\nsource_repo: who/what\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["valid"], false);
        let codes = finding_codes_of(&v);
        assert!(codes.contains("UNKNOWN_SOURCE_REPO"));
        assert!(codes.contains("INVALID_STATUS"));
    }

    // ----- validate_story reads the team's status taxonomy -----------

    /// A status that is one of the team's configured (custom) states
    /// validates clean — no `INVALID_STATUS`.
    #[test]
    fn custom_status_in_taxonomy_validates_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let statuses = vec!["grooming".to_string(), "shipping".to_string()];
        let text = "---\ntype: story\nstatus: grooming\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_status(&repos, &teams, &statuses));
        assert!(!finding_codes_of(&v).contains("INVALID_STATUS"));
    }

    /// An off-taxonomy status produces an advisory.
    /// `todo` is a shipped default id, but with a custom set that omits it the
    /// verdict must reject it — proving the check reads the resolved set, not the
    /// hardcoded `VALID_STATUSES`. The hint lists the resolved set.
    #[test]
    fn status_absent_from_taxonomy_is_advisory() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let statuses = vec!["grooming".to_string()];
        let text = "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_status(&repos, &teams, &statuses));
        assert_eq!(v["valid"], true);
        let findings = v["findings"].as_array().unwrap();
        let status_finding = findings
            .iter()
            .find(|f| f["code"] == "INVALID_STATUS")
            .expect("expected an advisory INVALID_STATUS finding");
        assert_eq!(status_finding["severity"], "advisory");
        assert!(status_finding["hint"]
            .as_str()
            .unwrap()
            .contains("grooming"));
    }

    /// A MISSING (absent) status is the distinct advisory `MISSING_STATUS`,
    /// never `INVALID_STATUS` — "unset" is not "typo'd a real state".
    #[test]
    fn missing_status_is_distinct_advisory_not_invalid() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        let codes = finding_codes_of(&v);
        assert!(codes.contains("MISSING_STATUS"));
        assert!(!codes.contains("INVALID_STATUS"));
    }

    /// Taxonomy ids are canonical — matching is exact, no silent case-fold.
    /// `Done` is not `done`, so it produces an advisory.
    #[test]
    fn status_match_is_case_sensitive() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let statuses = vec!["done".to_string()];
        let text = "---\ntype: story\nstatus: Done\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_status(&repos, &teams, &statuses));
        assert!(finding_codes_of(&v).contains("INVALID_STATUS"));
    }

    /// Zero-config parity: every shipped default status still validates clean
    /// through the taxonomy seam (guards the migration against changing default
    /// behavior for teams with no `status-taxonomy.yml`).
    #[test]
    fn default_statuses_all_validate_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for status in VALID_STATUSES {
            let text = format!(
                "---\ntype: story\nstatus: {}\nsource_repo: owner/repo\n---\n\n# S\n",
                status
            );
            let v = validate_candidate(&text, None, &cfg(&repos, &teams));
            assert!(
                !finding_codes_of(&v).contains("INVALID_STATUS"),
                "default status `{}` should validate clean",
                status
            );
        }
    }

    // ----- validate_story reads the team's field schema --------

    /// The default (baseline-only) schema declares no required fields, so a
    /// normal story never gains a `MISSING_REQUIRED_FIELD` finding — zero regression
    /// for existing teams, and no nagging about routine keys like `parent_epic`.
    #[test]
    fn default_schema_adds_no_required_field_finding() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n\
                    parent_epic: e\nsub_epic: se\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert!(!finding_codes_of(&v).contains("MISSING_REQUIRED_FIELD"));
    }

    /// A field the team schema declares `required` but the story lacks
    /// is a blocking `MISSING_REQUIRED_FIELD`.
    #[test]
    fn missing_required_custom_field_blocks() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let mut schema = crate::commands::field_schema::default_schema();
        schema.fields.push(crate::commands::field_schema::FieldDef {
            name: "risk_tier".to_string(),
            r#type: None,
            allowed_values: Vec::new(),
            required: true,
            official: false,
        });
        let text = "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_schema(&repos, &teams, &schema));
        assert_eq!(v["valid"], false);
        assert!(finding_codes_of(&v).contains("MISSING_REQUIRED_FIELD"));
    }

    /// With the required field present, the story validates clean —
    /// presence-only (the value is never inspected).
    #[test]
    fn present_required_custom_field_validates_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let mut schema = crate::commands::field_schema::default_schema();
        schema.fields.push(crate::commands::field_schema::FieldDef {
            name: "risk_tier".to_string(),
            r#type: None,
            allowed_values: Vec::new(),
            required: true,
            official: false,
        });
        let text = "---\ntype: story\nstatus: todo\nsource_repo: owner/repo\n\
                    risk_tier: high\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_schema(&repos, &teams, &schema));
        assert!(!finding_codes_of(&v).contains("MISSING_REQUIRED_FIELD"));
    }

    /// An explicit non-story `type` (e.g. `doc`) is a
    /// deliberate opt-out — it is a plain doc even WITH a `source_repo`, and
    /// must NOT be nagged with `INVALID_TYPE`.
    #[test]
    fn explicit_doc_type_with_source_repo_is_plain_doc_not_nagged() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: doc\nsource_repo: owner/repo\n---\n\n# Architecture\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "plain_doc");
        assert_eq!(v["valid"], true);
        assert!(v["findings"].as_array().unwrap().is_empty());
    }

    /// A `source_repo` with no `type:` is a story-in-progress that forgot its
    /// type → pipeline_story with a blocking `INVALID_TYPE`.
    #[test]
    fn source_repo_without_type_is_a_story_missing_its_type() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\nsource_repo: owner/repo\nstatus: todo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "pipeline_story");
        assert_eq!(v["valid"], false);
        assert!(finding_codes_of(&v).contains("INVALID_TYPE"));
    }

    /// Regression: a story that forgot its closing `---` fence
    /// must NOT pass as a valid plain doc (a false green).
    #[test]
    fn unterminated_story_frontmatter_is_flagged_not_green() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nsource_repo: owner/repo\n\n# Body without a closing fence\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(
            v["valid"], false,
            "unterminated story frontmatter must not be green"
        );
        assert!(finding_codes_of(&v).contains("UNPARSEABLE_FRONTMATTER"));
    }

    /// `...` is NOT a valid frontmatter closer (gray_matter honors only `---`),
    /// so a `...`-closed story is an unterminated block → flagged, not green.
    #[test]
    fn dot_dot_dot_close_fence_is_not_accepted() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nsource_repo: owner/repo\n...\n\n# Body\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["valid"], false);
        assert!(finding_codes_of(&v).contains("UNPARSEABLE_FRONTMATTER"));
    }

    /// Plain-doc safety: a plain doc that merely OPENS with a `---` thematic break
    /// (no story keys, no closing fence) is NOT false-flagged as unparseable.
    #[test]
    fn plain_doc_leading_dashes_is_not_false_flagged() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\n\n# A Doc\n\nContent with a leading rule and no story frontmatter.\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "plain_doc");
        assert_eq!(v["valid"], true);
        assert!(v["findings"].as_array().unwrap().is_empty());
    }

    /// Round-2 HIGH regression: valid YAML but wrong *shape* (a scalar where a
    /// list is required) fails the strict `FrontmatterFields` parse → empty
    /// fields → must be caught, not passed as a plain doc.
    #[test]
    fn wrong_shape_frontmatter_is_flagged_not_green() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text =
            "---\ntype: story\nsource_repo: owner/repo\ndepends_on: not-a-list\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(
            v["valid"], false,
            "wrong-shape story frontmatter must not be green"
        );
        assert!(finding_codes_of(&v).contains("UNPARSEABLE_FRONTMATTER"));
    }

    /// Round-2 MEDIUM regression: an unterminated `---` opener whose BODY (after
    /// prose that is not a `key:` line) contains a `key:` line must NOT be
    /// mistaken for story intent — the first substantive line is prose.
    #[test]
    fn body_key_line_in_unterminated_doc_is_not_flagged() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\n\nNotes on fields:\ntype: the story type, e.g. story or bug\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "plain_doc");
        assert_eq!(v["valid"], true);
        assert!(v["findings"].as_array().unwrap().is_empty());
    }

    /// Round-4 MEDIUM regression: a plain doc whose foreign frontmatter has an
    /// INDENTED child key named like a story field (`image:\n  type: cover`, a
    /// common Jekyll/Hugo shape) must NOT be flagged — only top-level (column-0)
    /// story keys count.
    #[test]
    fn indented_child_story_key_is_not_flagged() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for text in [
            "---\ntitle: My Post\nimage:\n  path: /a.png\n  type: cover\n---\n\n# Post\n",
            "---\nauthor:\n  team: infra\n---\n\n# Doc\n",
            "---\nmytool:\n  type: fancy\n\n# body, no closing fence\n",
        ] {
            let v = validate_candidate(text, None, &cfg(&repos, &teams));
            assert_eq!(
                v["detectedKind"], "plain_doc",
                "must be a plain doc: {text:?}"
            );
            assert_eq!(v["valid"], true, "must not be flagged: {text:?}");
            assert!(v["findings"].as_array().unwrap().is_empty());
        }
    }

    /// Round-3 MEDIUM regression: an unterminated story whose keys sit AFTER a
    /// leading blank line (or `#` YAML comment) inside the frontmatter must
    /// still be flagged — the gate skips leading blanks/comments rather than
    /// stopping at them.
    #[test]
    fn unterminated_story_with_leading_blank_is_flagged() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for text in [
            "---\n\ntype: story\nsource_repo: owner/repo\n\n# Body, no closing fence\n",
            "---\n# a yaml comment\ntype: story\nsource_repo: owner/repo\n\n# Body\n",
        ] {
            let v = validate_candidate(text, None, &cfg(&repos, &teams));
            assert_eq!(v["valid"], false, "must flag unterminated story: {text:?}");
            assert!(finding_codes_of(&v).contains("UNPARSEABLE_FRONTMATTER"));
        }
    }

    /// Round-2 regression: the `is_empty()` gate means a block that parses fine
    /// into a non-story doc (`type: doc` + source_repo) is NOT re-flagged as
    /// malformed — it stays a clean plain doc (guards against a false UNPARSEABLE
    /// on an explicit non-story `type`).
    #[test]
    fn parsed_doc_frontmatter_is_not_flagged_malformed() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: doc\nsource_repo: owner/repo\n---\n\n# Architecture\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "plain_doc");
        assert_eq!(v["valid"], true);
        assert!(v["findings"].as_array().unwrap().is_empty());
    }

    /// Scanner parity: frontmatter whose closing fence lands past the scanner's
    /// read window is dropped by the cache — the validator must agree (flag it),
    /// not report a green story the pipeline never sees.
    #[test]
    fn frontmatter_past_read_window_is_not_green() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let filler = "x".repeat(STORY_META_READ_CAP);
        let text =
            format!("---\ntype: story\nsource_repo: owner/repo\nnote: {filler}\n---\n\n# S\n");
        let v = validate_candidate(&text, None, &cfg(&repos, &teams));
        assert_eq!(
            v["valid"], false,
            "story with frontmatter past the read cap must not be green"
        );
    }

    #[test]
    fn valid_story_passes_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text =
            "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "pipeline_story");
        assert_eq!(v["valid"], true, "findings: {}", v["findings"]);
        // no blocking findings; advisory ones may exist but valid stays true
        assert!(v.get("contract").is_none());
    }

    // ----- assigned_to roster validation ------------------

    /// A story with no `assigned_to` is unassigned — a first-class state,
    /// never an `ASSIGNED_TO_UNKNOWN` finding (even with a roster configured).
    #[test]
    fn absent_assigned_to_is_unassigned_not_a_finding() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&["alice"]);
        let text =
            "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_roster(&repos, &teams, &roster));
        assert_eq!(v["valid"], true, "findings: {}", v["findings"]);
        assert!(!finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"));
    }

    /// An empty / whitespace-only `assigned_to` is treated as unassigned,
    /// not a bad login — no finding.
    #[test]
    fn empty_assigned_to_is_not_a_finding() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&["alice"]);
        let text = "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: \"   \"\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_roster(&repos, &teams, &roster));
        assert!(!finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"));
    }

    /// A login in the roster resolves clean — including `@`-prefixed and
    /// mixed-case forms (guards that normalization is single-sourced through the
    /// resolver, not re-derived here).
    #[test]
    fn known_assigned_to_login_is_clean_across_forms() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&["alice"]);
        for form in ["alice", "@alice", "Alice", "  @ALICE  "] {
            let text = format!(
                "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: \"{form}\"\n---\n\n# S\n"
            );
            let v = validate_candidate(&text, None, &cfg_roster(&repos, &teams, &roster));
            assert!(
                !finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"),
                "form {form:?} should resolve clean, findings: {}",
                v["findings"]
            );
        }
    }

    #[test]
    fn known_canonical_person_reference_is_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&["alice"]);
        let person_id = roster.people[0].id.as_str();
        let text = format!(
            "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: person:{person_id}\n---\n\n# S\n"
        );
        let v = validate_candidate(&text, None, &cfg_roster(&repos, &teams, &roster));
        assert!(!finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"));
    }

    /// A login NOT in the roster emits exactly one advisory finding on the
    /// `assigned_to` field — and the story stays `valid` (advisory does not block).
    #[test]
    fn unknown_assigned_to_login_is_advisory_not_blocking() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&["alice"]);
        let text = "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: bob\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_roster(&repos, &teams, &roster));
        assert_eq!(
            v["valid"], true,
            "advisory must not block: {}",
            v["findings"]
        );
        let hits: Vec<&Value> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["code"] == "ASSIGNED_TO_UNKNOWN")
            .collect();
        assert_eq!(hits.len(), 1, "exactly one finding: {}", v["findings"]);
        assert_eq!(hits[0]["field"], "assigned_to");
        assert_eq!(hits[0]["severity"], "advisory");
    }

    /// When the roster is unavailable (`NoAssigneeResolver`), the check is
    /// skipped — an unknown login yields no finding rather than a false positive.
    #[test]
    fn no_roster_skips_the_assigned_to_check() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: nobody\n---\n\n# S\n";
        // cfg() backs the resolver with NoAssigneeResolver.
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert!(!finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"));
    }

    /// An empty roster (present-but-unpopulated `people.yml`, e.g. the org
    /// bootstrap window) carries no data to check against, so the check is
    /// skipped rather than flagging every assignee — matches the roster-absent
    /// posture (never a false positive).
    #[test]
    fn empty_roster_skips_the_assigned_to_check() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let roster = roster_org(&[]);
        let text = "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\nassigned_to: anyone\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg_roster(&repos, &teams, &roster));
        assert!(!finding_codes_of(&v).contains("ASSIGNED_TO_UNKNOWN"));
    }

    /// The code is published in the failure taxonomy as advisory.
    #[test]
    fn assigned_to_unknown_is_published_as_advisory() {
        let codes = finding_codes();
        let entry = codes
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["code"] == "ASSIGNED_TO_UNKNOWN")
            .expect("ASSIGNED_TO_UNKNOWN must be published in finding_codes()");
        assert_eq!(entry["severity"], "advisory");
    }

    #[test]
    fn blocks_are_ordered_before_warnings() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // missing source_repo (block) + missing status (advisory) + missing story_id (advisory)
        let text = "---\ntype: story\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        let arr = v["findings"].as_array().unwrap();
        let sevs: Vec<&str> = arr
            .iter()
            .map(|f| f["severity"].as_str().unwrap())
            .collect();
        let first_advisory = sevs.iter().position(|s| *s == "advisory");
        let last_blocking = sevs.iter().rposition(|s| *s == "blocking");
        if let (Some(fa), Some(lb)) = (first_advisory, last_blocking) {
            assert!(
                lb < fa,
                "all blocking findings must precede advisory ones: {sevs:?}"
            );
        }
    }

    // ----- HTML validates with identical rules -------------------------

    #[test]
    fn html_story_validates_like_markdown() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let ok = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n<meta name=\"cs:status\" content=\"todo\">\n<title>S</title>";
        let v = validate_candidate(ok, Some(Path::new("s.html")), &cfg(&repos, &teams));
        assert_eq!(v["format"], "html");
        assert_eq!(v["detectedKind"], "pipeline_story");
        assert_eq!(v["valid"], true, "findings: {}", v["findings"]);

        let bad = "<meta name=\"cs:type\" content=\"story\">\n<title>S</title>";
        let v = validate_candidate(bad, Some(Path::new("s.html")), &cfg(&repos, &teams));
        assert_eq!(v["valid"], false);
        assert!(finding_codes_of(&v).contains("MISSING_SOURCE_REPO"));
    }

    // ----- no false green ---------------------------------------------

    #[test]
    fn malformed_yaml_is_never_a_green_plain_doc() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // Broken YAML (bad indentation / unterminated) inside a frontmatter block.
        let text = "---\ntype: story\n  : : bad\nsource_repo\n---\n\n# S\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["valid"], false);
        assert!(finding_codes_of(&v).contains("UNPARSEABLE_FRONTMATTER"));
    }

    // ----- epic main doc is recognized, not validated ------------------------

    #[test]
    fn readme_is_epic_main_doc_not_a_story() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: epic\nrepos:\n  - owner/repo\n---\n\n# Epic\n";
        let v = validate_candidate(
            text,
            Some(Path::new("some/epic/README.md")),
            &cfg(&repos, &teams),
        );
        assert_eq!(v["detectedKind"], "epic_main_doc");
        assert_eq!(v["valid"], true);
    }

    // ----- Tool 2 shape ------------------------------------------------------

    #[test]
    fn source_repos_payload_partitions_primary_and_by_team() {
        let teams = vec![
            ("default".to_string(), vec!["owner/a".to_string()]),
            (
                "other".to_string(),
                vec!["owner/b".to_string(), "owner/c".to_string()],
            ),
        ];
        let v = source_repos_payload("myorg", "default", &teams);
        assert_eq!(v["activeOrg"], "myorg");
        assert_eq!(v["activeTeam"], "default");
        assert_eq!(v["primary"], json!(["owner/a"]));
        assert_eq!(v["byTeam"].as_array().unwrap().len(), 2);
    }

    // ----- resolve_repo_accounts join ---------------

    #[test]
    fn resolve_repo_accounts_surfaces_bound_account_case_insensitively() {
        // Configured slug preserves case; the attachment map is keyed lowercase
        // (as `group_attachments_by_remote` produces) — the join must still hit.
        let teams = vec![(
            "default".to_string(),
            vec![(
                "Alderco/CodeSense".to_string(),
                "github.com/alderco/codesense".to_string(),
            )],
        )];
        let mut map = std::collections::HashMap::new();
        map.insert(
            "github.com/alderco/codesense".to_string(),
            RepoAccount {
                provider: "github".to_string(),
                account: "999".to_string(),
                display: "work-alice".to_string(),
            },
        );
        let out = resolve_repo_accounts(&teams, &map);
        assert_eq!(
            out.get("Alderco/CodeSense").unwrap(),
            &json!({ "provider": "github", "account": "999", "display": "work-alice" }),
            "mixed-case slug must resolve its bound account"
        );
    }

    #[test]
    fn resolve_repo_accounts_reports_unattached_as_null() {
        let teams = vec![(
            "default".to_string(),
            vec![
                (
                    "owner/bound".to_string(),
                    "github.com/owner/bound".to_string(),
                ),
                (
                    "owner/unbound".to_string(),
                    "github.com/owner/unbound".to_string(),
                ),
            ],
        )];
        let mut map = std::collections::HashMap::new();
        map.insert(
            "github.com/owner/bound".to_string(),
            RepoAccount {
                provider: "github".to_string(),
                account: "7".to_string(),
                display: "acct7".to_string(),
            },
        );
        let out = resolve_repo_accounts(&teams, &map);
        assert_eq!(
            out.get("owner/bound").unwrap(),
            &json!({ "provider": "github", "account": "7", "display": "acct7" })
        );
        assert_eq!(
            out.get("owner/unbound").unwrap(),
            &Value::Null,
            "a repo with no bound account is reported null"
        );
    }

    /// A repo bound to a NON-GitHub forge enriches the catalog with
    /// its provider + account (no `u64` parse) — proving the projection is now
    /// provider-neutral, not github-only.
    #[test]
    fn resolve_repo_accounts_surfaces_non_github_binding() {
        let teams = vec![(
            "default".to_string(),
            vec![(
                "group/proj".to_string(),
                "gitlab.com/group/proj".to_string(),
            )],
        )];
        let mut map = std::collections::HashMap::new();
        map.insert(
            "gitlab.com/group/proj".to_string(),
            RepoAccount {
                provider: "gitlab".to_string(),
                account: "42".to_string(),
                display: "work-bob".to_string(),
            },
        );
        let out = resolve_repo_accounts(&teams, &map);
        assert_eq!(
            out.get("gitlab.com/group/proj").unwrap(),
            &json!({ "provider": "gitlab", "account": "42", "display": "work-bob" }),
            "a non-github binding must appear with its provider + account"
        );
        assert!(!out.contains_key("group/proj"));
    }

    #[test]
    fn resolve_repo_accounts_does_not_alias_same_slug_across_hosts() {
        let teams = vec![(
            "default".to_string(),
            vec![
                ("acme/api".to_string(), "github.com/acme/api".to_string()),
                ("acme/api".to_string(), "gitlab.com/acme/api".to_string()),
            ],
        )];
        let map = HashMap::from([
            (
                "github.com/acme/api".to_string(),
                RepoAccount {
                    provider: "github".to_string(),
                    account: "gh".to_string(),
                    display: "GitHub".to_string(),
                },
            ),
            (
                "gitlab.com/acme/api".to_string(),
                RepoAccount {
                    provider: "gitlab".to_string(),
                    account: "gl".to_string(),
                    display: "GitLab".to_string(),
                },
            ),
        ]);
        let out = resolve_repo_accounts(&teams, &map);
        assert_eq!(out["github.com/acme/api"]["account"], "gh");
        assert_eq!(out["gitlab.com/acme/api"]["account"], "gl");
        assert!(!out.contains_key("acme/api"));
    }

    // ----- Tool 1 uses the live consts, not a copy ---------------------------

    #[test]
    fn get_story_format_enums_come_from_rust_consts() {
        let v = story_format_payload(default_test_headings());
        let types: Vec<&str> = v["pipeline_story_format"]["enums"]["type"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(types, PIPELINE_DOCUMENT_TYPES);
        let statuses: Vec<&str> = v["pipeline_story_format"]["enums"]["status"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(statuses, VALID_STATUSES);

        // `decision` is an ordered ARRAY with `plain_doc` FIRST (order
        // signals the default — most content needs no frontmatter). Array order
        // is guaranteed on the wire, independent of serde map key-ordering.
        let decision = v["decision"].as_array().expect("decision is an array");
        assert_eq!(decision[0]["kind"], "plain_doc");
        assert_eq!(decision[0]["frontmatter_required"], false);
        assert_eq!(decision[1]["kind"], "pipeline_story");
        assert_eq!(decision[2]["kind"], "epic_main_doc");

        // A `live_data_doc` branch routes an authoring agent to
        // the live-data surface, and `related_tools` cross-refs both new tools,
        // so `get_story_format` is the single discovery path.
        let live = decision
            .iter()
            .find(|d| d["kind"] == "live_data_doc")
            .expect("live_data_doc decision branch present");
        assert_eq!(live["continue_to_tool"], "get_live_data_contract");
        // The entry point teaches passive routine snapshots and an explicit,
        // authorized refresh rather than the retired service-proxy contract.
        let guidance = live["guidance"].as_str().unwrap();
        assert!(guidance.contains("routineResult") && guidance.contains("routineStatus"));
        assert!(guidance.contains("routineRefresh") && guidance.contains("authorized"));
        assert!(guidance.contains("agent-owned tools"));
        // No grants or trust: any allowlisted verb runs when called.
        for retired in ["capabilit", "trust", "grant"] {
            assert!(
                !guidance.contains(retired),
                "guidance still mentions {retired}"
            );
        }
        assert!(!guidance.contains("__codesenseProxy"));
        let related = &v["related_tools"];
        assert!(related.get("get_live_data_contract").is_some());
        assert!(related.get("validate_live_data_doc").is_some());
        assert!(
            related["get_live_data_contract"]
                .as_str()
                .unwrap()
                .contains("act"),
            "related_tools cross-ref mentions the action tier"
        );

        // `refresh_frontmatter_cache` is advertised to
        // authoring agents so a raw filesystem write has a documented path
        // back into the cache, and its description explicitly contrasts
        // non-CodeSense writers with CodeSense's own file APIs.
        let refresh = related
            .get("refresh_frontmatter_cache")
            .and_then(|v| v.as_str())
            .expect("related_tools lists refresh_frontmatter_cache");
        assert!(
            refresh.contains("non-CodeSense writer"),
            "refresh_frontmatter_cache cross-ref names the non-CodeSense writer case: {refresh}"
        );
    }

    // ----- enforced Rust↔TS vocabulary sync ---------------------------

    #[test]
    fn status_and_doc_type_vocab_in_sync_with_ts() {
        const TYPES_TS: &str = include_str!("../../../src/shared/lib/pipeline-graph/types.ts");
        const ICON_TS: &str =
            include_str!("../../../src/shared/lib/pipeline-graph/doc-type-icon.ts");

        // (a) status: EQUALITY (set parity) — Rust VALID_STATUSES == TS PIPELINE_STATUSES.
        let ts_statuses = extract_ts_string_array(TYPES_TS, "PIPELINE_STATUSES");
        assert!(
            !ts_statuses.is_empty(),
            "failed to parse PIPELINE_STATUSES from types.ts"
        );
        let rust_statuses: BTreeSet<String> =
            VALID_STATUSES.iter().map(|s| s.to_string()).collect();
        let ts_status_set: BTreeSet<String> = ts_statuses.into_iter().collect();
        assert_eq!(
            rust_statuses, ts_status_set,
            "VALID_STATUSES drifted from TS PIPELINE_STATUSES"
        );

        // (b) doc-type: SUBSET — every Rust pipeline type has a TS icon.
        // DOC_TYPE_ICON legitimately carries extra non-pipeline keys
        // (doc/retro/canvas/image), so this is subset, NOT equality.
        let ts_icon_keys = extract_ts_object_keys(ICON_TS, "DOC_TYPE_ICON");
        assert!(
            !ts_icon_keys.is_empty(),
            "failed to parse DOC_TYPE_ICON keys from doc-type-icon.ts"
        );
        for t in PIPELINE_DOCUMENT_TYPES {
            assert!(
                ts_icon_keys.iter().any(|k| k == t),
                "Rust pipeline type {t:?} is missing from TS DOC_TYPE_ICON keys {ts_icon_keys:?}"
            );
        }
    }

    // Minimal TS-literal scrapers (mirror the `include_str!` self-parity idiom
    // of `mcp_tools_registered`; no JS parser needed for these simple forms).
    fn extract_ts_string_array(src: &str, name: &str) -> Vec<String> {
        let Some(start) = src
            .find(name)
            .and_then(|i| src[i..].find('[').map(|j| i + j + 1))
        else {
            return Vec::new();
        };
        let Some(end_rel) = src[start..].find(']') else {
            return Vec::new();
        };
        let body = &src[start..start + end_rel];
        collect_quoted(body)
    }

    fn extract_ts_object_keys(src: &str, name: &str) -> Vec<String> {
        // Anchor on the ASSIGNMENT brace (`= {`), not the first `{` after the
        // name — the declaration may carry a `Record<string, { ... }>` type
        // annotation whose `{` would otherwise be mistaken for the value object.
        let Some(name_at) = src.find(name) else {
            return Vec::new();
        };
        let Some(eq_at) = src[name_at..].find('=').map(|j| name_at + j) else {
            return Vec::new();
        };
        let Some(start) = src[eq_at..].find('{').map(|j| eq_at + j + 1) else {
            return Vec::new();
        };
        // Find the matching closing brace by depth.
        let bytes = src[start..].char_indices();
        let mut depth = 1i32;
        let mut end = src.len();
        for (idx, ch) in bytes {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + idx;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[start..end];
        // Keys are top-level `ident:` at the start of a (trimmed) line.
        let mut keys = Vec::new();
        let mut brace_depth = 0i32;
        for line in body.lines() {
            let trimmed = line.trim();
            if brace_depth == 0 {
                if let Some(colon) = trimmed.find(':') {
                    let key = trimmed[..colon].trim().trim_matches(['"', '\'']);
                    if !key.is_empty()
                        && key
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        keys.push(key.to_string());
                    }
                }
            }
            brace_depth += line.matches('{').count() as i32;
            brace_depth -= line.matches('}').count() as i32;
        }
        keys
    }

    fn collect_quoted(body: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' || c == '\'' {
                let quote = c;
                let mut s = String::new();
                for n in chars.by_ref() {
                    if n == quote {
                        break;
                    }
                    s.push(n);
                }
                out.push(s);
            }
        }
        out
    }

    fn finding_codes_of(v: &Value) -> BTreeSet<String> {
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_string())
            .collect()
    }

    // ----- round-trip proof (tool verdict ⟺ engine behavior) ---------

    /// A candidate `validate_story` reports valid as a `pipeline_story` IS
    /// actually picked up by the frontmatter cache; a `plain_doc` it passes is
    /// projected as content but is correctly ABSENT from the cache. If the
    /// tool's verdict and the engine's behavior ever disagree, the tool is
    /// wrong — this test is that binding.
    #[test]
    fn validate_verdict_matches_cache_behavior() {
        use crate::context_repo::TEAM_CONTENT_DIR;
        use crate::install::frontmatter_cache::FrontmatterCache;
        use std::fs;

        let root = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("mcp10_roundtrip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let content = root.join("default").join(TEAM_CONTENT_DIR);
        fs::create_dir_all(&content).unwrap();

        let story_path = content.join("a-story.md");
        fs::write(
            &story_path,
            "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: alderco/codesense\n---\n\n# A Story\n",
        )
        .unwrap();

        let doc_path = content.join("plain-notes.md");
        fs::write(
            &doc_path,
            "# Plain Notes\n\nJust content, no frontmatter.\n",
        )
        .unwrap();

        // --- Engine behavior ---
        let cache = FrontmatterCache::build_from_scan(&root);
        assert!(
            cache.entries.contains_key(&story_path),
            "valid story must be cached (build_cache_entry → Some)"
        );
        assert!(
            !cache.entries.contains_key(&doc_path),
            "zero-frontmatter plain doc must NOT be cached (projected as content only)"
        );

        // --- Tool verdict on the SAME files ---
        let repos = vec!["alderco/codesense".to_string()];
        let teams = vec!["default".to_string()];
        let story_text = fs::read_to_string(&story_path).unwrap();
        let sv = validate_candidate(&story_text, Some(&story_path), &cfg(&repos, &teams));
        assert_eq!(sv["detectedKind"], "pipeline_story");
        assert_eq!(
            sv["valid"], true,
            "cached story must validate clean: {}",
            sv["findings"]
        );

        let doc_text = fs::read_to_string(&doc_path).unwrap();
        let dv = validate_candidate(&doc_text, Some(&doc_path), &cfg(&repos, &teams));
        assert_eq!(dv["detectedKind"], "plain_doc");
        assert_eq!(dv["valid"], true);
        assert!(dv["findings"].as_array().unwrap().is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    // ----- summary-heading contract -------------------------------

    fn codes_contains(v: &Value, code: &str) -> bool {
        finding_codes_of(v).contains(code)
    }

    /// The `summary_headings` section lists the EFFECTIVE (passed) headings,
    /// verbatim and in order — not a hardcoded literal.
    #[test]
    fn get_story_format_summary_headings_reflect_config() {
        let default = story_format_payload(default_test_headings());
        assert_eq!(
            default["summary_headings"]["effective_headings"],
            json!(["Purpose", "Story"])
        );

        let custom = vec!["Overview".to_string()];
        let v = story_format_payload(&custom);
        assert_eq!(
            v["summary_headings"]["effective_headings"],
            json!(["Overview"])
        );
        // The concrete forms name the first configured heading, not "Purpose".
        assert!(v["summary_headings"]["markdown"]
            .as_str()
            .unwrap()
            .contains("## Overview"));
        assert!(v["summary_headings"]["html"]
            .as_str()
            .unwrap()
            .contains("data-cs=\"Overview\""));
    }

    /// The section states the contract (first-match-wins, H2-only,
    /// case-insensitive, blank-if-none) with both the Markdown and HTML forms.
    #[test]
    fn summary_headings_section_states_contract_both_forms() {
        let v = story_format_payload(default_test_headings());
        let c = &v["summary_headings"]["contract"];
        assert_eq!(c["first_match_wins"], true);
        assert_eq!(c["h2_only"], true);
        assert_eq!(c["case_insensitive"], true);
        assert_eq!(c["blank_card_if_none_match"], true);
        assert!(v["summary_headings"]["markdown"]
            .as_str()
            .unwrap()
            .contains("## Purpose"));
        assert!(v["summary_headings"]["html"]
            .as_str()
            .unwrap()
            .contains("<section data-cs=\"Purpose\">"));
    }

    /// `MISSING_SUMMARY_HEADING` is published in the finding-codes taxonomy.
    #[test]
    fn missing_summary_heading_published_in_finding_codes() {
        let v = story_format_payload(default_test_headings());
        let codes: Vec<&str> = v["finding_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"MISSING_SUMMARY_HEADING"));
    }

    /// The story examples carry a first-heading summary block, so a copied
    /// example ships a non-blank card.
    #[test]
    fn examples_include_summary_block() {
        let v = story_format_payload(default_test_headings());
        assert!(v["examples"]["markdown_story"]
            .as_str()
            .unwrap()
            .contains("## Purpose"));
        assert!(v["examples"]["html_story"]
            .as_str()
            .unwrap()
            .contains("<section data-cs=\"Purpose\">"));
    }

    /// A pipeline story with no summary section gets the advisory — never
    /// blocking (valid stays true), naming the effective order.
    #[test]
    fn story_without_summary_heading_gets_advisory_not_block() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\n---\n\n# S\n\nProse but no `## Purpose` or `## Story` heading.\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "pipeline_story");
        assert_eq!(
            v["valid"], true,
            "advisory must not block: {}",
            v["findings"]
        );
        assert!(codes_contains(&v, "MISSING_SUMMARY_HEADING"));
        // hint names the Markdown form of the first effective heading.
        let f = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["code"] == "MISSING_SUMMARY_HEADING")
            .unwrap();
        assert_eq!(f["severity"], "advisory");
        assert_eq!(f["field"], "body");
        assert!(f["hint"].as_str().unwrap().contains("## Purpose"));
    }

    /// A story WITH a matching heading (non-empty body) draws no advisory —
    /// including a case-insensitive match on the second configured heading.
    #[test]
    fn story_with_matching_heading_is_clean() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        for body in [
            "# S\n\n## Purpose\n\nWhat this does.\n",
            "# S\n\n## story\n\nlower-case matches case-insensitively.\n", // AC: case-insensitive, second heading
        ] {
            let text = format!(
                "---\ntype: story\nstatus: todo\nstory_id: X-1\nsource_repo: owner/repo\n---\n\n{body}"
            );
            let v = validate_candidate(&text, None, &cfg(&repos, &teams));
            assert!(
                !codes_contains(&v, "MISSING_SUMMARY_HEADING"),
                "clean summary must not advise: {body:?}"
            );
        }
    }

    /// Scanner parity: a matching heading with an EMPTY body renders a blank card,
    /// so `extract_first_section` returns None and the advisory still fires.
    #[test]
    fn matching_heading_with_empty_body_still_advises() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        // `## Purpose` immediately followed by the next `##` — empty section body.
        let text = "---\ntype: story\nsource_repo: owner/repo\n---\n\n# S\n\n## Purpose\n\n## Next\n\ncontent under next\n";
        let v = validate_candidate(text, None, &cfg(&repos, &teams));
        assert!(
            codes_contains(&v, "MISSING_SUMMARY_HEADING"),
            "empty summary body renders blank ⇒ must advise (render parity): {}",
            v["findings"]
        );
        // Parity check against the exact extractor the scanner uses.
        assert!(pipeline_format::extract_first_section(
            PipelineFileFormat::Markdown,
            text,
            default_test_headings(),
            None
        )
        .is_none());
    }

    /// Config-driven both ways — with `["Overview"]`, only `## Overview`
    /// validates clean; `## Purpose` now draws the advisory.
    #[test]
    fn config_overview_drives_advisory_both_ways() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let headings = vec!["Overview".to_string()];
        let base = "---\ntype: story\nsource_repo: owner/repo\n---\n\n# S\n\n";

        let with_overview = format!("{base}## Overview\n\nSummary.\n");
        let v = validate_candidate(&with_overview, None, &cfg_h(&repos, &teams, &headings));
        assert!(
            !codes_contains(&v, "MISSING_SUMMARY_HEADING"),
            "## Overview matches config: {}",
            v["findings"]
        );

        let with_purpose = format!("{base}## Purpose\n\nSummary.\n");
        let v = validate_candidate(&with_purpose, None, &cfg_h(&repos, &teams, &headings));
        assert!(
            codes_contains(&v, "MISSING_SUMMARY_HEADING"),
            "## Purpose does NOT match config ['Overview']: {}",
            v["findings"]
        );
    }

    /// HTML stories are checked against `<section data-cs>` (not `## H2`);
    /// the hint names the HTML form. No `.html` story is false-nagged by a
    /// Markdown-only check.
    #[test]
    fn html_story_summary_uses_section_data_cs() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let head = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n<title>S</title>\n";

        let with_section = format!("{head}<section data-cs=\"Purpose\">A summary blurb.</section>");
        let v = validate_candidate(
            &with_section,
            Some(Path::new("s.html")),
            &cfg(&repos, &teams),
        );
        assert_eq!(v["format"], "html");
        assert!(
            !codes_contains(&v, "MISSING_SUMMARY_HEADING"),
            "html <section data-cs> is a valid summary: {}",
            v["findings"]
        );

        let v = validate_candidate(head, Some(Path::new("s.html")), &cfg(&repos, &teams));
        assert!(codes_contains(&v, "MISSING_SUMMARY_HEADING"));
        let f = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["code"] == "MISSING_SUMMARY_HEADING")
            .unwrap();
        assert!(
            f["hint"].as_str().unwrap().contains("<section data-cs="),
            "html hint must name the HTML form, not `## Heading`: {}",
            f["hint"]
        );
    }

    /// Scanner parity: an HTML `<section data-cs>` whose body is markup-only
    /// (e.g. an `<img>`) strips to empty text — the scanner's tag-stripping path
    /// (distinct from Markdown's line-join) renders a blank card, so the advisory
    /// must fire. Guards the HTML extractor's `HTML_TAG.replace_all` branch.
    #[test]
    fn html_section_with_markup_only_body_still_advises() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];
        let text = "<meta name=\"cs:type\" content=\"story\">\n<meta name=\"cs:source-repo\" content=\"owner/repo\">\n<title>S</title>\n<section data-cs=\"Purpose\"><img src=\"x.png\"></section>";
        let v = validate_candidate(text, Some(Path::new("s.html")), &cfg(&repos, &teams));
        assert_eq!(v["format"], "html");
        assert!(
            codes_contains(&v, "MISSING_SUMMARY_HEADING"),
            "markup-only section body renders blank ⇒ must advise: {}",
            v["findings"]
        );
        // Parity: the extractor the scanner uses also yields None here.
        assert!(pipeline_format::extract_first_section(
            PipelineFileFormat::Html,
            text,
            default_test_headings(),
            None
        )
        .is_none());
    }

    /// Plain docs and epic READMEs are never nagged for a missing summary.
    #[test]
    fn plain_doc_and_readme_never_get_summary_advisory() {
        let repos = vec!["owner/repo".to_string()];
        let teams = vec!["default".to_string()];

        let plain = validate_candidate(
            "# Just notes\n\nNo frontmatter.\n",
            None,
            &cfg(&repos, &teams),
        );
        assert_eq!(plain["detectedKind"], "plain_doc");
        assert!(!codes_contains(&plain, "MISSING_SUMMARY_HEADING"));

        let readme = "---\ntype: epic\nrepos:\n  - owner/repo\n---\n\n# Epic\n";
        let v = validate_candidate(readme, Some(Path::new("README.md")), &cfg(&repos, &teams));
        assert_eq!(v["detectedKind"], "epic_main_doc");
        assert!(!codes_contains(&v, "MISSING_SUMMARY_HEADING"));
    }

    #[test]
    fn source_repo_outside_the_epic_repos_is_a_distinct_advisory() {
        let repos = vec!["acme/api".to_string(), "acme/web".to_string()];
        let teams: Vec<String> = Vec::new();
        let epic = vec!["acme/api".to_string()];
        let mut c = cfg(&repos, &teams);
        c.epic_repos = &epic;
        let story = |repo: &str| {
            format!("---\ntype: story\nstory_id: X-1\nstatus: todo\nsource_repo: {repo}\n---\n\n# S\n\n## Purpose\n\nx\n")
        };

        let outside = validate_candidate(&story("acme/web"), None, &c);
        assert!(
            codes_contains(&outside, "SOURCE_REPO_NOT_IN_EPIC"),
            "{outside}"
        );
        assert!(!codes_contains(&outside, "UNKNOWN_SOURCE_REPO"));
        assert_eq!(outside["valid"], true, "an epic mismatch never blocks");

        let inside = validate_candidate(&story("acme/api"), None, &c);
        assert!(
            !codes_contains(&inside, "SOURCE_REPO_NOT_IN_EPIC"),
            "{inside}"
        );

        let unknown = validate_candidate(&story("other/thing"), None, &c);
        assert!(codes_contains(&unknown, "UNKNOWN_SOURCE_REPO"));
        assert!(!codes_contains(&unknown, "SOURCE_REPO_NOT_IN_EPIC"));

        let no_epic_repos = validate_candidate(&story("acme/web"), None, &cfg(&repos, &teams));
        assert!(!codes_contains(&no_epic_repos, "SOURCE_REPO_NOT_IN_EPIC"));

        let published: Vec<String> = finding_codes()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap().to_string())
            .collect();
        assert!(published.contains(&"SOURCE_REPO_NOT_IN_EPIC".to_string()));
    }
}
