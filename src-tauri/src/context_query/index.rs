// SPDX-License-Identifier: MIT
//! The footprint index: every file under every content root the user has
//! synced (team context repos, the private space per org, all-orgs content),
//! with its frontmatter, kind and relationships.
//!
//! [`IndexState`] is the mutable side, owned by one worker (see `service`). It
//! is mtime-diffed: a reconcile walk re-reads only files whose mtime changed,
//! and a path-level refresh re-reads exactly the files a watcher reported.
//! Each change publishes an immutable [`Snapshot`] that queries run against
//! with no lock held.
//!
//! Memory is bounded: only the frontmatter block (first [`META_READ_CAP`]
//! bytes) of Markdown/HTML files is parsed and kept; bodies are never held
//! (full-text reads them on demand, bounded). Past [`MAX_INDEXED_FILES`] the
//! walk stops and logs.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::engine::{self, BodyReader, Corpus, QueryError, Viewer};
use super::model::{ContextQuery, ContextQueryResult, Kind, Layer};
use super::record::{ContextRecord, HeadingEntry};
use super::relations::{self, Relations};
use crate::commands::pipeline_format::{self, PipelineFileFormat};

/// Bytes read from the top of a Markdown file to parse its frontmatter.
pub const META_READ_CAP: usize = 16 * 1024;
/// Bytes read from the top of an HTML file to parse its `cs:*` metas (heads run long).
pub const HTML_META_READ_CAP: usize = 64 * 1024;
/// SW-DNS-2: the longest summary kept (characters, the ellipsis included).
pub const SUMMARY_CAP: usize = 500;
/// Files indexed at most, across the whole footprint.
/// Search sidecars exceed the metadata cap; keep their own bounded cache.
pub const HEADING_READ_CAP: usize = 4 * 1024 * 1024;

pub const MAX_INDEXED_FILES: usize = 200_000;

/// Directories never descended into.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", ".DS_Store"];

/// A content root and where it sits in the footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRoot {
    pub dir: PathBuf,
    pub org: Option<String>,
    pub team: Option<String>,
    pub layer: Layer,
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// The top of a file, and whether the file runs past what was read.
fn read_head(path: &Path, cap: usize) -> Option<(String, bool)> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(4096);
    file.take(cap as u64 + 1).read_to_end(&mut buf).ok()?;
    let more = buf.len() > cap;
    buf.truncate(cap);
    Some((String::from_utf8_lossy(&buf).into_owned(), more))
}

/// Did the metadata block run past the bytes read? Markdown: a frontmatter
/// block opened but not closed; HTML: the head not closed.
fn metadata_cut_off(format: PipelineFileFormat, head: &str, more: bool) -> bool {
    if !more {
        return false;
    }
    match format {
        PipelineFileFormat::Markdown => {
            let t = head.trim_start_matches('\u{feff}');
            t.starts_with("---") && !t[3..].contains("\n---")
        }
        PipelineFileFormat::Html => !head.to_ascii_lowercase().contains("</head>"),
    }
}

/// Is `path` (under `root`) inside a directory the index never descends into?
fn in_skipped_dir(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|rel| {
        rel.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|n| SKIPPED_DIRS.contains(&n))
        })
    })
}

/// Classify a file by the existing rules: skills by location, then frontmatter
/// `type` (story/bug/spike/epic/dashboard), epic main docs (README/index inside
/// a folder) and epic-only fields, HTML dashboards (`cs:capabilities` or any HTML
/// page); everything else is a doc (Markdown) or other.
fn classify(
    rel_path: &str,
    format: Option<PipelineFileFormat>,
    fields: &BTreeMap<String, serde_json::Value>,
) -> (Kind, bool) {
    // The bool: a pipeline work item (`type` story/bug/spike, exactly the
    // scanner's `is_pipeline_story`), the only `depends_on` targets.
    use crate::context_repo::scratch_space::{inert_kind, InertKind};
    if matches!(inert_kind(rel_path), Some(InertKind::Skill)) {
        return (Kind::Skill, false);
    }
    let Some(format) = format else {
        return (Kind::Other, false);
    };
    let doc_type = fields
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase());
    match doc_type.as_deref() {
        Some(t) if pipeline_format::PIPELINE_DOCUMENT_TYPES.contains(&t) => {
            let kind = match t {
                "bug" => Kind::Bug,
                "spike" => Kind::Spike,
                _ => Kind::Story,
            };
            return (kind, true);
        }
        Some("epic") => return (Kind::Epic, false),
        Some("dashboard") => return (Kind::Dashboard, false),
        Some("skill") => return (Kind::Skill, false),
        Some("content" | "doc") => return (Kind::Doc, false),
        _ => {}
    }
    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let stem = file_name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    let in_folder = rel_path.contains('/');
    let epic_fields = ["repos", "scope_context", "scope_install"]
        .iter()
        .any(|k| fields.contains_key(*k));
    if doc_type.is_none() && ((in_folder && (stem == "readme" || stem == "index")) || epic_fields) {
        return (Kind::Epic, false);
    }
    if format == PipelineFileFormat::Html {
        return (Kind::Dashboard, false);
    }
    (Kind::Doc, false)
}

mod plain {
    use regex::Regex;
    use std::sync::LazyLock;

    macro_rules! re {
        ($name:ident, $pat:expr) => {
            pub static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
        };
    }
    // Line prefixes: headings, block quotes, list bullets / numbers, task boxes.
    re!(
        PREFIX,
        r"(?m)^[ \t]*(?:#{1,6}[ \t]+|>[ \t]?|(?:[-*+]|\d+[.)])[ \t]+(?:\[[ xX]\][ \t]+)?)"
    );
    re!(IMAGE, r"!\[([^\]]*)\]\([^)]*\)");
    re!(LINK, r"\[([^\]]+)\](?:\([^)]*\)|\[[^\]]*\])");
    re!(CODE, r"`+([^`]*)`+");
    re!(STRONG, r"(\*\*|__)(\S(?:.*?\S)?)(?:\*\*|__)");
    re!(STRIKE, r"~~(.+?)~~");
    re!(EMPH, r"(^|[\s(\[])[*_](\S(?:[^*_]*?\S)?)[*_]");
    re!(TAG, r"<[^>]+>");
    re!(ENTITY, r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);");
}

/// Decode the HTML character references a summary may carry (`&amp;`, `&#39;`,
/// `&#x2014;` …); an unknown name is kept as written.
fn decode_entities(text: &str) -> String {
    plain::ENTITY
        .replace_all(text, |c: &regex::Captures| {
            let body = &c[1];
            let ch = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                match body {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "mdash" => Some('—'),
                    "ndash" => Some('–'),
                    "hellip" => Some('…'),
                    "rsquo" => Some('’'),
                    "lsquo" => Some('‘'),
                    "rdquo" => Some('”'),
                    "ldquo" => Some('“'),
                    _ => None,
                }
            };
            ch.map(String::from).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

/// SW-DNS-2: a section as plain prose: Markdown's inline markup (links, images,
/// code, emphasis, bullets, headings, quotes) and any HTML tags dropped, and
/// character references decoded, so a dashboard can show it as text.
fn plain_text(format: PipelineFileFormat, text: &str) -> String {
    let mut t = plain::TAG.replace_all(text, " ").into_owned();
    if format == PipelineFileFormat::Markdown {
        t = plain::PREFIX.replace_all(&t, "").into_owned();
        t = plain::IMAGE.replace_all(&t, "$1").into_owned();
        t = plain::LINK.replace_all(&t, "$1").into_owned();
        t = plain::CODE.replace_all(&t, "$1").into_owned();
        t = plain::STRONG.replace_all(&t, "$2").into_owned();
        t = plain::STRIKE.replace_all(&t, "$1").into_owned();
        t = plain::EMPH.replace_all(&t, "$1$2").into_owned();
    }
    decode_entities(&t)
}

/// SW-DNS-2: the summary text of a pipeline doc's head: the first of `headings`
/// that matches, whitespace collapsed, capped at [`SUMMARY_CAP`] characters.
fn summary_of(format: PipelineFileFormat, head: &str, headings: &[String]) -> Option<String> {
    let text = pipeline_format::extract_first_section(format, head, headings, None)?;
    let plain = plain_text(format, &text);
    let collapsed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= SUMMARY_CAP {
        return Some(collapsed);
    }
    let mut cut: String = collapsed.chars().take(SUMMARY_CAP - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

/// Build the record for one file under `root`. `summary_headings` is the
/// effective summary-heading list (empty: the defaults).
pub fn build_record(
    root: &IndexRoot,
    path: &Path,
    meta: &std::fs::Metadata,
    summary_headings: &[String],
) -> Option<ContextRecord> {
    let rel = path.strip_prefix(&root.dir).ok()?;
    let rel_path = path_str(rel);
    if rel_path.is_empty() {
        return None;
    }
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let format = pipeline_format::detect_format(ext);
    let (fields, title, metadata_truncated, head, metadata_valid) = match format {
        Some(f) => {
            let cap = match f {
                PipelineFileFormat::Markdown => META_READ_CAP,
                PipelineFileFormat::Html => HTML_META_READ_CAP,
            };
            match read_head(path, cap) {
                Some((head, more)) => {
                    let (fields, valid) =
                        crate::commands::frontmatter_editor::frontmatter_json_checked(f, &head);
                    let truncated = metadata_cut_off(f, &head, more);
                    let valid = valid && !truncated && !head.trim().is_empty();
                    (
                        fields,
                        pipeline_format::extract_title(f, &head),
                        truncated,
                        Some(head),
                        valid,
                    )
                }
                None => (BTreeMap::new(), None, false, None, false),
            }
        }
        None => (BTreeMap::new(), None, false, None, true),
    };
    let (kind, is_pipeline_doc) = classify(&rel_path, format, &fields);
    // Only work items carry a summary: the index stays small, and a plain doc's
    // `## Purpose` is not an overview of a story.
    let summary = match (format, head.as_deref()) {
        (Some(f), Some(head)) if is_pipeline_doc || kind == Kind::Epic => {
            let effective = if summary_headings.is_empty() {
                crate::commands::pipeline_scanner::default_summary_headings()
            } else {
                summary_headings.to_vec()
            };
            summary_of(f, head, &effective)
        }
        _ => None,
    };
    let headings = if file_name == "search-index.json" && meta.len() <= HEADING_READ_CAP as u64 {
        #[derive(serde::Deserialize)]
        struct Sidecar {
            schema: u32,
            entries: Vec<HeadingEntry>,
        }
        read_head(path, HEADING_READ_CAP)
            .and_then(|(body, more)| (!more).then_some(body))
            .and_then(|body| serde_json::from_str::<Sidecar>(&body).ok())
            .filter(|s| s.schema == 1)
            .map(|s| s.entries)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Some(ContextRecord {
        headings,
        path: path_str(path),
        rel_path,
        file_name,
        org: root.org.clone(),
        team: root.team.clone(),
        layer: root.layer,
        kind,
        mtime_ms: Some(mtime_ns(meta) / 1_000_000),
        title,
        fields,
        is_pipeline_doc,
        metadata_truncated,
        metadata_valid,
        summary,
    })
}

/// An immutable, queryable view of the index.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Sorted by path.
    pub records: Vec<Arc<ContextRecord>>,
    pub relations: Relations,
    /// The roots the records came from.
    pub roots: Vec<IndexRoot>,
    /// Bumped on every publish.
    pub generation: u64,
    /// False until the first full build finishes.
    pub ready: bool,
}

impl Snapshot {
    pub fn build(
        records: Vec<Arc<ContextRecord>>,
        roots: Vec<IndexRoot>,
        generation: u64,
        ready: bool,
    ) -> Self {
        let relations = relations::build(&records);
        Self {
            records,
            relations,
            roots,
            generation,
            ready,
        }
    }

    pub fn corpus(&self) -> Corpus<'_> {
        Corpus {
            records: &self.records,
            relations: &self.relations,
            roots: &self.roots,
            generation: self.generation,
        }
    }

    pub fn query(
        &self,
        q: &ContextQuery,
        viewer: &Viewer,
        reader: &dyn BodyReader,
    ) -> Result<ContextQueryResult, QueryError> {
        // Build relationships from the visible corpus too: hidden records must
        // behave like absent records in counts, backlinks and body scans.
        let page_scope = matches!(
            viewer.scope,
            crate::context_repo::caller_scope::CallerScope::Human
        )
        .then(|| {
            viewer
                .page
                .as_deref()
                .and_then(|page| crate::context_repo::caller_scope::for_content(Path::new(page)))
        })
        .flatten();
        let scope = page_scope.as_ref().unwrap_or(&viewer.scope);
        let scoped = !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human);
        // One canonicalize per distinct parent directory, not two per record. The
        // directory is canonicalized (so a symlinked root, or a directory swapped
        // for a symlink inside the watcher's debounce, resolves to where it
        // really points); only the final file component is taken as indexed.
        let memo =
            std::cell::RefCell::new(crate::context_repo::inheritance::ScopeMemo::for_index_rows());
        let filtered = scoped.then(|| {
            let mut memo = memo.borrow_mut();
            Self::build(
                self.records
                    .iter()
                    .filter(|r| match row_real_path(&mut memo, r) {
                        Some(real) => scope.allows_real(&mut memo, real),
                        None => scope.allows(Path::new(&r.path)),
                    })
                    .cloned()
                    .collect(),
                self.roots.clone(),
                self.generation,
                self.ready,
            )
        });
        // A file swapped for a symlink after indexing must not leak its target:
        // every body that is read is checked again, through the same memo.
        let rechecked = RecheckedReader {
            inner: reader,
            scope,
            memo: &memo,
        };
        let reader: &dyn BodyReader = if scoped { &rechecked } else { reader };
        let mut result = engine::run(
            &filtered.as_ref().unwrap_or(self).corpus(),
            q,
            viewer,
            reader,
        )?;
        result.indexing = !self.ready;
        Ok(result)
    }
}

/// The canonical path of an indexed record: its parent directory's canonical
/// path (memoised per directory) plus its file name. `None` falls back to the
/// full per-path check.
fn row_real_path(
    memo: &mut crate::context_repo::inheritance::ScopeMemo,
    r: &ContextRecord,
) -> Option<PathBuf> {
    let path = Path::new(&r.path);
    let name = path.file_name()?;
    Some(memo.real_dir(path.parent()?)?.join(name))
}

/// Checks a body read against the caller's scope: the leaf must be a regular
/// file (not a link), and its directory-resolved path must be admitted.
struct RecheckedReader<'a> {
    inner: &'a dyn BodyReader,
    scope: &'a crate::context_repo::caller_scope::CallerScope,
    memo: &'a std::cell::RefCell<crate::context_repo::inheritance::ScopeMemo>,
}
impl BodyReader for RecheckedReader<'_> {
    fn read(&self, path: &str, max_bytes: usize) -> Option<String> {
        let leaf_is_file = std::fs::symlink_metadata(path)
            .map(|m| std::fs::FileType::is_file(&m.file_type()))
            .unwrap_or(false);
        if !leaf_is_file {
            return None;
        }
        let mut memo = std::cell::RefCell::borrow_mut(self.memo);
        let real = match Path::new(path).parent().and_then(|p| memo.real_dir(p)) {
            Some(dir) => dir.join(Path::new(path).file_name()?),
            None => {
                return self
                    .scope
                    .allows(Path::new(path))
                    .then(|| self.inner.read(path, max_bytes))
                    .flatten()
            }
        };
        self.scope
            .allows_real(&mut memo, real)
            .then(|| self.inner.read(path, max_bytes))
            .flatten()
    }
}

/// One indexed file plus the mtime it was read at.
#[derive(Debug, Clone)]
struct Entry {
    mtime_ns: u64,
    root: usize,
    record: Arc<ContextRecord>,
}

/// The mutable index, owned by the index worker.
#[derive(Debug, Default)]
pub struct IndexState {
    roots: Vec<IndexRoot>,
    entries: BTreeMap<PathBuf, Entry>,
    /// SW-DNS-2: the effective summary headings records were built with
    /// (empty: the defaults).
    summary_headings: Vec<String>,
}

impl IndexState {
    pub fn roots(&self) -> &[IndexRoot] {
        &self.roots
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// SW-DNS-2: adopt the configured summary headings. When they changed, every
    /// record is re-read on the next walk (so each summary follows the new list)
    /// and `true` is returned.
    pub fn set_summary_headings(&mut self, headings: Vec<String>) -> bool {
        let effective = crate::commands::pipeline_scanner::effective_headings(headings);
        let current = if self.summary_headings.is_empty() {
            crate::commands::pipeline_scanner::default_summary_headings()
        } else {
            self.summary_headings.clone()
        };
        if effective == current {
            return false;
        }
        self.summary_headings = effective;
        // Re-read only the files that carry a summary (work items and epic main
        // docs): no stored mtime matches a real file's.
        for e in self.entries.values_mut() {
            if e.record.is_pipeline_doc || e.record.kind == Kind::Epic {
                e.mtime_ns = u64::MAX;
            }
        }
        true
    }

    /// The deepest root containing `path`.
    fn root_of(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .enumerate()
            .filter(|(_, r)| path.starts_with(&r.dir))
            .max_by_key(|(_, r)| r.dir.components().count())
            .map(|(i, _)| i)
    }

    /// Walk `dir` (under root `ri`), reusing entries whose mtime is unchanged.
    /// Fills `next`, and records changed paths into `changed`. `indexed_elsewhere`
    /// counts files already held outside `next`, for the index cap.
    fn walk_into(
        &self,
        ri: usize,
        dir: &Path,
        next: &mut BTreeMap<PathBuf, Entry>,
        changed: &mut Vec<PathBuf>,
        indexed_elsewhere: usize,
    ) {
        let root = &self.roots[ri];
        let walker = walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                !(e.file_type().is_dir()
                    && e.file_name()
                        .to_str()
                        .is_some_and(|n| SKIPPED_DIRS.contains(&n)))
            });
        for entry in walker.filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            if indexed_elsewhere + next.len() >= MAX_INDEXED_FILES {
                log::warn!(target: "context_query", "index cap of {MAX_INDEXED_FILES} files reached; the rest of {} is not indexed", dir.display());
                return;
            }
            let path = entry.path();
            // A nested root owns its own subtree.
            if self.root_of(path) != Some(ri) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = mtime_ns(&meta);
            if let Some(prev) = self.entries.get(path) {
                if prev.mtime_ns == mtime && prev.root == ri {
                    next.insert(path.to_path_buf(), prev.clone());
                    continue;
                }
            }
            if let Some(record) = build_record(root, path, &meta, &self.summary_headings) {
                changed.push(path.to_path_buf());
                next.insert(
                    path.to_path_buf(),
                    Entry {
                        mtime_ns: mtime,
                        root: ri,
                        record: Arc::new(record),
                    },
                );
            }
        }
    }

    /// Re-walk every root (adopting a new root set), re-reading only files
    /// whose mtime changed. Returns the paths added, changed or removed.
    pub fn reconcile(&mut self, roots: Vec<IndexRoot>) -> Vec<PathBuf> {
        if roots != self.roots {
            // Root membership changed: every entry is re-attributed on the walk.
            self.roots = roots;
            for e in self.entries.values_mut() {
                e.root = usize::MAX;
            }
        }
        let mut next = BTreeMap::new();
        let mut changed = Vec::new();
        for ri in 0..self.roots.len() {
            let dir = self.roots[ri].dir.clone();
            self.walk_into(ri, &dir, &mut next, &mut changed, 0);
        }
        changed.extend(
            self.entries
                .keys()
                .filter(|p| !next.contains_key(*p))
                .cloned(),
        );
        self.entries = next;
        changed
    }

    /// Refresh exactly the reported paths: a file is re-read (or dropped when
    /// gone), a directory is re-walked (catching moves and deletes inside it).
    /// Returns the paths that actually changed.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        for path in paths {
            let Some(ri) = self.root_of(path) else {
                continue;
            };
            if in_skipped_dir(&self.roots[ri].dir, path) {
                continue;
            }
            match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => {
                    let mut fresh = BTreeMap::new();
                    // Files held outside this folder (the folder's own are re-walked).
                    let under = self
                        .entries
                        .range(path.clone()..)
                        .take_while(|(p, _)| p.starts_with(path))
                        .count();
                    let elsewhere = self.entries.len() - under;
                    self.walk_into(ri, path, &mut fresh, &mut changed, elsewhere);
                    // Only this root's files: a nested root's files under `path`
                    // were skipped by the walk, not deleted.
                    let gone: Vec<PathBuf> = self
                        .entries
                        .range(path.clone()..)
                        .take_while(|(p, _)| p.starts_with(path))
                        .filter(|(p, e)| e.root == ri && !fresh.contains_key(*p))
                        .map(|(p, _)| p.clone())
                        .collect();
                    for p in gone {
                        self.entries.remove(&p);
                        changed.push(p);
                    }
                    self.entries.extend(fresh);
                }
                Ok(meta) if meta.is_file() => {
                    let mtime = mtime_ns(&meta);
                    let unchanged = self
                        .entries
                        .get(path)
                        .is_some_and(|e| e.mtime_ns == mtime && e.root == ri);
                    if unchanged {
                        continue;
                    }
                    if let Some(record) =
                        build_record(&self.roots[ri], path, &meta, &self.summary_headings)
                    {
                        self.entries.insert(
                            path.clone(),
                            Entry {
                                mtime_ns: mtime,
                                root: ri,
                                record: Arc::new(record),
                            },
                        );
                        changed.push(path.clone());
                    }
                }
                // Gone, or replaced by a symlink (never followed): drop the file,
                // or everything under a removed folder.
                _ => {
                    let gone: Vec<PathBuf> = self
                        .entries
                        .range(path.clone()..)
                        .take_while(|(p, _)| p.starts_with(path))
                        .map(|(p, _)| p.clone())
                        .collect();
                    for p in gone {
                        self.entries.remove(&p);
                        changed.push(p);
                    }
                }
            }
        }
        changed
    }

    /// Publish the current state.
    pub fn snapshot(&self, generation: u64, ready: bool) -> Snapshot {
        let records = self
            .entries
            .values()
            .map(|e| Arc::clone(&e.record))
            .collect();
        Snapshot::build(records, self.roots.clone(), generation, ready)
    }
}

/// The orgs a set of changed paths touches, and whether any is all-orgs content
/// (the change event carries these; consumers re-query).
pub fn changed_orgs(state: &IndexState, paths: &[PathBuf]) -> (Vec<String>, bool) {
    let mut orgs = std::collections::BTreeSet::new();
    let mut all_orgs = false;
    for p in paths {
        if let Some(ri) = state.root_of(p) {
            match &state.roots[ri].org {
                Some(org) => {
                    orgs.insert(org.clone());
                }
                None => all_orgs = true,
            }
        }
    }
    (orgs.into_iter().collect(), all_orgs)
}
