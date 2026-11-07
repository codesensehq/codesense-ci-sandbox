// SPDX-License-Identifier: MIT
//! The query evaluator. Pure: no Tauri, no async, no I/O except the bounded
//! body read for `text` (behind [`BodyReader`] so tests stay hermetic).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use globset::{GlobBuilder, GlobMatcher};
use serde_json::Value;

use super::index::IndexRoot;
use super::model::{
    ContextQuery, ContextQueryResult, ContextRow, FilterClause, FilterOp, GroupCount, Layer,
    MissingOrder, OrgScope, RelRef, RowRelationships, SortDir, SortKey, SortSpec,
    CONTEXT_QUERY_SCHEMA_VERSION, DEFAULT_LIMIT, MAX_LIMIT,
};
use super::order;
use super::record::ContextRecord;
use super::relations::{ref_matches, Relations};

/// Files read for a `text` match, at most, per query.
pub const MAX_TEXT_SCAN_FILES: usize = 5_000;
/// Bytes read per file for a `text` match, at most.
pub const MAX_TEXT_SCAN_BYTES: usize = 512 * 1024;
/// Bytes read for a `text` match across one query, at most.
pub const MAX_TEXT_SCAN_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Canonical status vocabulary, for sorting and grouping by `status`.
const STATUS_ORDER: &[&str] = &[
    "planning",
    "todo",
    "in-progress",
    "blocked",
    "review",
    "qa",
    "done",
];

/// Priority vocabularies, for sorting and grouping by `priority`. Values in the
/// same slot rank equally.
const PRIORITY_ORDER: &[&[&str]] = &[
    &["p0", "critical", "urgent", "highest"],
    &["p1", "high"],
    &["p2", "medium", "normal"],
    &["p3", "low"],
    &["p4", "lowest", "trivial"],
];

/// Reads a file body for `text` matching. Returns `None` when unreadable.
pub trait BodyReader {
    fn read(&self, path: &str, max_bytes: usize) -> Option<String>;
}

/// Reads bodies from disk.
pub struct DiskBodyReader;

impl BodyReader for DiskBodyReader {
    fn read(&self, path: &str, max_bytes: usize) -> Option<String> {
        use std::io::Read;
        let file = std::fs::File::open(path).ok()?;
        let mut buf = Vec::new();
        file.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// Who is asking, and for what.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    pub(crate) scope: crate::context_repo::caller_scope::CallerScope,
    /// What `@me` means in every org (the local profile's handle).
    pub identities: Vec<String>,
    /// What `@me` also means in one org: the profile's roster entry there
    /// (`person:<id>`). Roster ids are per org, so they only match that org's files.
    pub org_identities: HashMap<String, Vec<String>>,
    /// Set when the query runs for a page (a dashboard, a `csquery` block, a
    /// pane) at this path: private items come only from the page's own org,
    /// whatever the query asks for; an unknown path sees none. Unset (an agent):
    /// private items come only from the ONE org the query names explicitly
    /// (`orgs: ["x"]` or `sameOrgAs`); `orgs: "all"` never returns private items.
    pub page: Option<String>,
}

/// The org of the content root `path` sits under (deepest root wins), if any.
fn org_of_path(roots: &[IndexRoot], path: &str) -> Option<String> {
    roots
        .iter()
        .filter(|r| std::path::Path::new(path).starts_with(&r.dir))
        .max_by_key(|r| r.dir.components().count())
        .and_then(|r| r.org.clone())
}

/// The one org whose private items a query may see, if any (see [`Viewer::page`]).
fn private_anchor(corpus: &Corpus, viewer: &Viewer, q: &ContextQuery) -> Option<String> {
    if let Some(page) = &viewer.page {
        return org_of_path(corpus.roots, page);
    }
    let named = match &q.scope.orgs {
        OrgScope::List(list) if list.len() == 1 => Some(list[0].clone()),
        _ => None,
    };
    match (&q.scope.same_org_as, named) {
        (Some(anchor), named) => {
            let org = org_of_path(corpus.roots, anchor)?;
            named.is_none_or(|n| n == org).then_some(org)
        }
        (None, named) => named,
    }
}

/// Which records a viewer may see at all. Private records outside the query's
/// one private anchor org are invisible: never returned, never matched through
/// a relationship, never listed in a row's relationships.
pub struct Visibility {
    hidden: HashSet<String>,
}

impl Visibility {
    pub fn of(corpus: &Corpus, viewer: &Viewer, q: &ContextQuery) -> Self {
        let anchor = private_anchor(corpus, viewer, q);
        let hidden = corpus
            .records
            .iter()
            .filter(|r| r.layer == Layer::Private && (anchor.is_none() || r.org != anchor))
            .map(|r| r.path.clone())
            .collect();
        Self { hidden }
    }

    fn sees(&self, r: &ContextRecord) -> bool {
        !self.hidden.contains(&r.path)
    }

    /// `refs` without the ones pointing at invisible records.
    fn refs(&self, refs: &[RelRef]) -> Vec<RelRef> {
        if self.hidden.is_empty() {
            return refs.to_vec();
        }
        refs.iter()
            .filter(|r| !(r.resolved && self.hidden.contains(&r.path)))
            .cloned()
            .collect()
    }
}

/// The code every surface answers for a [`QueryError::StaleCursor`]: the hosted
/// error, the start of the in-app bridge's message and MCP's `data.code`.
pub const STALE_CURSOR: &str = "stale_cursor";

/// Why a query was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// The cursor's snapshot is no longer kept (its pin lapsed, or it is from
    /// an earlier app run): restart paging without a cursor.
    StaleCursor,
    /// The cursor is malformed or belongs to another query.
    InvalidCursor(String),
    /// The query itself is malformed.
    Invalid(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleCursor => write!(
                f,
                "{STALE_CURSOR}: results changed since this page was fetched; restart paging without a cursor"
            ),
            Self::InvalidCursor(m) | Self::Invalid(m) => f.write_str(m),
        }
    }
}

/// Lowercase, `-` → `_`, camelCase → snake_case. Two keys are the same field
/// iff they normalize equal.
pub fn normalize_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut prev_lower = false;
    let key = if key.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("cs:")) {
        &key[3..]
    } else {
        key
    };
    for ch in key.chars() {
        if ch.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = false;
        } else if ch == '-' {
            out.push('_');
            prev_lower = false;
        } else {
            out.push(ch);
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

/// Frontmatter aliases: the built-in name a key also answers to.
fn alias(norm: &str) -> &str {
    match norm {
        "document_type" => "type",
        "epic" => "parent_epic",
        _ => norm,
    }
}

/// A value seen through a field lookup: either a borrowed JSON value or an
/// owned built-in.
enum FieldValue<'a> {
    Json(&'a Value),
    Owned(Value),
}

impl FieldValue<'_> {
    fn get(&self) -> &Value {
        match self {
            FieldValue::Json(v) => v,
            FieldValue::Owned(v) => v,
        }
    }
}

/// A frontmatter key by exact name, then by normalized name (with aliases).
fn frontmatter<'a>(r: &'a ContextRecord, key: &str, norm: &str) -> Option<FieldValue<'a>> {
    if let Some(v) = r.fields.get(key) {
        return Some(FieldValue::Json(v));
    }
    let want = alias(norm);
    r.fields
        .iter()
        .find(|(k, _)| alias(&normalize_key(k)) == want)
        .map(|(_, v)| FieldValue::Json(v))
}

/// Look `key` up on a record. Identity fields (`path`, `relPath`, `fileName`,
/// `id`, `org`, `mtime`) are always the built-ins; for the descriptive
/// built-ins (`team`, `title`, `layer`, `kind`, `dir`) a frontmatter key of the
/// same name wins; anything else is frontmatter.
fn lookup<'a>(r: &'a ContextRecord, key: &str) -> Option<FieldValue<'a>> {
    let norm = normalize_key(key);
    let owned = |v: Option<Value>| v.map(FieldValue::Owned);
    let s = |v: &str| Some(Value::String(v.to_string()));
    match norm.as_str() {
        "path" => owned(s(&r.path)),
        "rel_path" => owned(s(&r.rel_path)),
        "file_name" => owned(s(&r.file_name)),
        "id" => owned(s(r.id())),
        "org" => owned(r.org.clone().map(Value::String)),
        "mtime" => owned(r.mtime_ms.map(Value::from)),
        "team" | "title" | "layer" | "kind" | "dir" => {
            frontmatter(r, key, &norm).or_else(|| match norm.as_str() {
                "team" => owned(r.team.clone().map(Value::String)),
                "title" => owned(r.title.clone().map(Value::String)),
                "layer" => owned(s(r.layer.as_str())),
                "kind" => owned(s(r.kind.as_str())),
                _ => owned(s(r.rel_dir())),
            })
        }
        _ => frontmatter(r, key, &norm),
    }
}

/// The same field lookup used by predicates, for an occurrence's actual values.
pub(crate) fn field_value(r: &ContextRecord, key: &str) -> Option<Value> {
    lookup(r, key).map(|value| value.get().clone())
}

/// Flatten a value into comparable text items. `null`/empty → none.
fn items(v: &Value) -> Vec<String> {
    match v {
        Value::Null => vec![],
        Value::String(s) if s.is_empty() => vec![],
        Value::String(s) => vec![s.clone()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(a) => a.iter().flat_map(items).collect(),
        Value::Object(_) => vec![v.to_string()],
    }
}

/// One operand of a clause. `numeric` when the query gave a JSON number:
/// only then does `1` equal `1.0` or `01`; a string operand matches exactly.
struct Operand {
    text: String,
    numeric: Option<f64>,
}

impl Operand {
    fn matches(&self, have: &str) -> bool {
        match self.numeric {
            Some(want) => order::as_number(have).is_some_and(|h| h == want),
            None => have == self.text,
        }
    }
}

fn operands_of(v: &Value, out: &mut Vec<Operand>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| operands_of(x, out)),
        Value::Number(n) => out.push(Operand {
            text: n.to_string(),
            numeric: n.as_f64(),
        }),
        other => out.extend(items(other).into_iter().map(|text| Operand {
            text,
            numeric: None,
        })),
    }
}

/// The operands of a clause, with `@me` expanded for a record in `org`.
fn operand(value: Option<&Value>, viewer: &Viewer, org: Option<&str>) -> Vec<Operand> {
    let mut raw = Vec::new();
    if let Some(v) = value {
        operands_of(v, &mut raw);
    }
    let mut out = Vec::new();
    for o in raw {
        if o.numeric.is_none() && o.text == "@me" {
            let me = viewer.identities.iter().chain(
                org.and_then(|o| viewer.org_identities.get(o))
                    .into_iter()
                    .flatten(),
            );
            out.extend(me.map(|text| Operand {
                text: text.clone(),
                numeric: None,
            }));
        } else {
            out.push(o);
        }
    }
    out
}

pub(crate) fn clause_matches(r: &ContextRecord, c: &FilterClause, viewer: &Viewer) -> bool {
    let have: Vec<String> = lookup(r, &c.key)
        .map(|v| items(v.get()))
        .unwrap_or_default();
    match c.op {
        FilterOp::Exists => !have.is_empty(),
        FilterOp::Missing => have.is_empty(),
        FilterOp::Eq | FilterOp::In => {
            let want = operand(c.value.as_ref(), viewer, r.org.as_deref());
            have.iter().any(|h| want.iter().any(|w| w.matches(h)))
        }
        FilterOp::Not => {
            let want = operand(c.value.as_ref(), viewer, r.org.as_deref());
            !have.iter().any(|h| want.iter().any(|w| w.matches(h)))
        }
        FilterOp::Contains => {
            let want = operand(c.value.as_ref(), viewer, r.org.as_deref());
            let is_list = matches!(
                lookup(r, &c.key).as_ref().map(FieldValue::get),
                Some(Value::Array(_))
            );
            // A JSON-number operand matches a list item / value numerically;
            // otherwise matching ignores case: a list item equal to the
            // operand, or text containing it.
            want.iter().any(|w| {
                if w.numeric.is_some() {
                    return have.iter().any(|h| w.matches(h));
                }
                let w = &w.text;
                let w_lower = w.to_lowercase();
                have.iter().any(|h| {
                    if is_list {
                        h.eq_ignore_ascii_case(w)
                    } else {
                        h.to_lowercase().contains(&w_lower)
                    }
                })
            })
        }
        FilterOp::Prefix => {
            let want = operand(c.value.as_ref(), viewer, r.org.as_deref());
            have.iter()
                .any(|h| want.iter().any(|w| h.starts_with(w.text.as_str())))
        }
        FilterOp::Range => {
            let min = c.min.as_ref().and_then(|v| items(v).into_iter().next());
            let max = c.max.as_ref().and_then(|v| items(v).into_iter().next());
            // A value is only in range of bounds of its own kind (a numeric
            // bound never matches a date or text, and vice versa).
            have.iter().any(|h| {
                min.as_deref()
                    .is_none_or(|m| order::same_kind(h, m) && order::cmp(h, m) != Ordering::Less)
                    && max.as_deref().is_none_or(|m| {
                        order::same_kind(h, m) && order::cmp(h, m) != Ordering::Greater
                    })
            })
        }
    }
}

/// Filters grouped by normalized key: OR within a group, AND across groups.
fn group_filters(q: &ContextQuery) -> Vec<Vec<FilterClause>> {
    let mut groups: Vec<(String, Vec<FilterClause>)> = Vec::new();
    let mut push = |c: FilterClause| {
        let k = alias(&normalize_key(&c.key)).to_string();
        match groups.iter_mut().find(|(gk, _)| *gk == k) {
            Some((_, g)) => g.push(c),
            None => groups.push((k, vec![c])),
        }
    };
    for c in &q.filters {
        push(c.clone());
    }
    for (key, value) in &q.match_fields {
        let op = if value.is_array() {
            FilterOp::In
        } else {
            FilterOp::Eq
        };
        push(FilterClause {
            key: key.clone(),
            op,
            value: Some(value.clone()),
            min: None,
            max: None,
        });
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

/// Rank of a vocabulary value for `status`/`priority`, else `None`.
fn vocab_rank(norm_key: &str, value: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    match norm_key {
        "status" => STATUS_ORDER.iter().position(|s| *s == lower),
        "priority" => PRIORITY_ORDER
            .iter()
            .position(|slot| slot.contains(&lower.as_str())),
        _ => None,
    }
}

fn has_vocab(norm_key: &str) -> bool {
    matches!(norm_key, "status" | "priority")
}

/// Compare two (present) sort values for `norm_key`.
fn value_cmp(norm_key: &str, a: &str, b: &str) -> Ordering {
    if has_vocab(norm_key) {
        // Known values by vocabulary order, unknown values after them; ties
        // (synonyms, unknowns) by the value order, so the order stays total.
        let ra = vocab_rank(norm_key, a).unwrap_or(usize::MAX);
        let rb = vocab_rank(norm_key, b).unwrap_or(usize::MAX);
        return ra.cmp(&rb).then_with(|| order::cmp(a, b));
    }
    order::cmp(a, b)
}

/// The first text item of `key` on `r`, if any.
fn sort_value(r: &ContextRecord, key: &str) -> Option<String> {
    lookup(r, key).and_then(|v| items(v.get()).into_iter().next())
}

/// Index of the snapshot a query runs over.
pub struct Corpus<'a> {
    pub records: &'a [Arc<ContextRecord>],
    pub relations: &'a Relations,
    /// The content roots the records came from (resolves `sameOrgAs`).
    pub roots: &'a [IndexRoot],
    /// The index snapshot's generation (binds cursors).
    pub generation: u64,
}

/// Compiled `where` scope.
struct Scope {
    orgs: Option<HashSet<String>>,
    teams: Option<HashSet<String>>,
    layers: Option<Vec<super::model::Layer>>,
    kinds: Option<Vec<super::model::Kind>>,
    prefix: Option<String>,
    glob: Option<GlobMatcher>,
    /// `sameRootAs`: the content root (absolute dir) records must sit under;
    /// `Some(None)` when the anchor is under no root (nothing matches).
    root: Option<Option<String>>,
}

fn compile_scope(q: &ContextQuery, corpus: &Corpus) -> Result<Scope, QueryError> {
    let w = &q.scope;
    let mut orgs: Option<HashSet<String>> = match &w.orgs {
        OrgScope::All => None,
        OrgScope::List(list) => Some(list.iter().cloned().collect()),
    };
    if let Some(anchor) = &w.same_org_as {
        // The anchor's org is the org of the content root it sits under (a
        // file or a folder, indexed yet or not). Fail closed: an anchor under
        // no org's root (all-orgs content, a path outside the footprint) leaves
        // only all-orgs content — never every org's files.
        let anchor_org = org_of_path(corpus.roots, anchor);
        let narrowed = match (anchor_org, &orgs) {
            (Some(org), None) => Some(org),
            (Some(org), Some(list)) if list.contains(&org) => Some(org),
            _ => None,
        };
        // `""` is no org's slug, so it admits only org-less (all-orgs) records.
        orgs = Some(HashSet::from([narrowed.unwrap_or_default()]));
    }
    let glob = match &w.path_glob {
        Some(g) => Some(
            GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| QueryError::Invalid(format!("invalid pathGlob: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };
    Ok(Scope {
        orgs,
        teams: w.teams.as_ref().map(|t| t.iter().cloned().collect()),
        layers: w.layers.clone(),
        kinds: w.kinds.clone(),
        prefix: w
            .path_prefix
            .as_ref()
            .map(|p| p.trim_end_matches('/').to_string()),
        glob,
        root: w.same_root_as.as_ref().map(|anchor| {
            corpus
                .roots
                .iter()
                .filter(|r| std::path::Path::new(anchor).starts_with(&r.dir))
                .max_by_key(|r| r.dir.components().count())
                .map(|r| r.dir.to_string_lossy().replace('\\', "/"))
        }),
    })
}

fn under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn in_scope(r: &ContextRecord, s: &Scope) -> bool {
    match &s.root {
        Some(Some(root)) if !under_prefix(&r.path, root) => return false,
        Some(None) => return false,
        _ => {}
    }
    if let Some(orgs) = &s.orgs {
        // All-orgs content (no org) applies in every org.
        if let Some(org) = &r.org {
            if !orgs.contains(org) {
                return false;
            }
        }
    }
    if let Some(teams) = &s.teams {
        match &r.team {
            Some(t) if teams.contains(t) => {}
            _ => return false,
        }
    }
    if let Some(layers) = &s.layers {
        if !layers.contains(&r.layer) {
            return false;
        }
    }
    if let Some(kinds) = &s.kinds {
        if !kinds.contains(&r.kind) {
            return false;
        }
    }
    if let Some(prefix) = &s.prefix {
        let ok = if prefix.starts_with('/') {
            under_prefix(&r.path, prefix)
        } else {
            under_prefix(&r.rel_path, prefix)
        };
        if !ok {
            return false;
        }
    }
    if let Some(glob) = &s.glob {
        if !glob.is_match(&r.rel_path) {
            return false;
        }
    }
    true
}

/// Does `r` sit under the epic `token`: `parent_epic`/`epic` frontmatter equal
/// to it, or a folder on its relative path named it.
fn in_epic(r: &ContextRecord, token: &str) -> bool {
    if r.parent_epic()
        .is_some_and(|e| e.eq_ignore_ascii_case(token))
    {
        return true;
    }
    r.rel_dir().split('/').any(|seg| seg == token)
}

fn relationships_match(
    i: usize,
    r: &ContextRecord,
    q: &ContextQuery,
    rel: &Relations,
    vis: &Visibility,
) -> bool {
    let Some(f) = &q.relationships else {
        return true;
    };
    if let Some(t) = &f.depends_on {
        if !ref_matches(&vis.refs(&rel.depends_on[i]), t) {
            return false;
        }
    }
    if let Some(t) = &f.depended_on_by {
        if !ref_matches(&vis.refs(&rel.dependents[i]), t) {
            return false;
        }
    }
    if let Some(t) = &f.has_supporting_doc {
        if !ref_matches(&vis.refs(&rel.supporting_docs[i]), t) {
            return false;
        }
    }
    if let Some(t) = &f.backlinked_by {
        if !ref_matches(&vis.refs(&rel.backlinks[i]), t) {
            return false;
        }
    }
    if let Some(t) = &f.parent_epic {
        if !in_epic(r, t) {
            return false;
        }
    }
    if let Some(want) = f.has_deps {
        if vis.refs(&rel.depends_on[i]).is_empty() == want {
            return false;
        }
    }
    if let Some(want) = f.is_backlinked {
        if vis.refs(&rel.backlinks[i]).is_empty() == want {
            return false;
        }
    }
    true
}

/// The matching record indexes, in corpus order, before text/sort/paging.
/// `candidate` lets an adapter restrict which records can be returned while
/// relationships still resolve against the whole corpus.
pub fn matching(
    corpus: &Corpus,
    q: &ContextQuery,
    viewer: &Viewer,
    candidate: &dyn Fn(&ContextRecord) -> bool,
) -> Result<Vec<usize>, QueryError> {
    let scope = compile_scope(q, corpus)?;
    let groups = group_filters(q);
    let vis = Visibility::of(corpus, viewer, q);
    Ok(corpus
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| candidate(r) && vis.sees(r) && in_scope(r, &scope))
        .filter(|(_, r)| {
            groups
                .iter()
                .all(|g| g.iter().any(|c| clause_matches(r, c, viewer)))
        })
        .filter(|(i, r)| relationships_match(*i, r, q, corpus.relations, &vis))
        .map(|(i, _)| i)
        .collect())
}

/// Search tokens retain underscores so tool names remain one term.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn coverage(terms: &[String], text: &str) -> Vec<bool> {
    let words = tokens(text);
    terms
        .iter()
        .map(|term| words.iter().any(|w| w.starts_with(term)))
        .collect()
}

#[derive(Default)]
struct TextResult {
    truncated: bool,
    scores: HashMap<usize, (usize, usize, usize)>,
    matches: HashMap<usize, Vec<super::model::TextMatch>>,
}

/// Resolve root-relative schema-1 sidecar hrefs to existing page records.
/// Source and target share a root, and both must be visible to the viewer.
fn heading_chunks<'a>(
    corpus: &'a Corpus,
    vis: &Visibility,
) -> HashMap<String, Vec<&'a super::record::HeadingEntry>> {
    let visible: HashMap<&str, &ContextRecord> = corpus
        .records
        .iter()
        .filter(|r| vis.sees(r))
        .map(|r| (r.path.as_str(), r.as_ref()))
        .collect();
    let mut chunks: HashMap<String, Vec<&super::record::HeadingEntry>> = HashMap::new();
    for r in corpus
        .records
        .iter()
        .filter(|r| !r.headings.is_empty() && vis.sees(r))
    {
        let Some(root) = r.path.strip_suffix(&r.rel_path) else {
            continue;
        };
        for entry in &r.headings {
            let href = entry
                .href
                .split('#')
                .next()
                .unwrap_or("")
                .trim_start_matches("./");
            // The sidecar contract is root-relative, like the docs palette.
            if href.is_empty() || href.split('/').any(|part| part == "..") {
                continue;
            }
            let path = format!("{root}{href}");
            if visible
                .get(path.as_str())
                .is_some_and(|target| target.path.strip_suffix(&target.rel_path) == Some(root))
            {
                chunks.entry(path).or_default().push(entry);
            }
        }
    }
    chunks
}

/// Token-aware matching over titles, cached headings and bounded body reads.
/// Coverage orders titles before headings before bodies, with path ties.
fn filter_text(
    corpus: &Corpus,
    matched: &mut Vec<usize>,
    text: &str,
    reader: &dyn BodyReader,
    vis: &Visibility,
) -> TextResult {
    let terms = tokens(text);
    if terms.is_empty() {
        return TextResult::default();
    }
    let chunks = heading_chunks(corpus, vis);
    let (mut files, mut bytes) = (0usize, 0usize);
    let mut result = TextResult::default();
    let mut kept = Vec::new();
    for &i in matched.iter() {
        if files >= MAX_TEXT_SCAN_FILES || bytes >= MAX_TEXT_SCAN_TOTAL_BYTES {
            result.truncated = true;
            break;
        }
        let r = &corpus.records[i];
        files += 1;
        let budget = MAX_TEXT_SCAN_BYTES.min(MAX_TEXT_SCAN_TOTAL_BYTES - bytes);
        let body = reader.read(&r.path, budget).unwrap_or_default();
        bytes += body.len();
        let title_hits = coverage(&terms, r.title.as_deref().unwrap_or(""));
        let body_hits = coverage(&terms, &body);
        let mut all = title_hits.clone();
        for (hit, body) in all.iter_mut().zip(&body_hits) {
            *hit |= body;
        }
        let mut best_heading = 0;
        let mut matches = Vec::new();
        for chunk in chunks.get(&r.path).into_iter().flatten() {
            let heading_hits = coverage(&terms, &chunk.title);
            let chunk_hits = coverage(&terms, &format!("{} {}", chunk.title, chunk.text));
            let count = heading_hits.iter().filter(|&&v| v).count();
            best_heading = best_heading.max(count);
            for (hit, chunk) in all.iter_mut().zip(&chunk_hits) {
                *hit |= chunk;
            }
            if chunk_hits.iter().any(|&v| v) {
                let hit = super::model::TextMatch {
                    title: chunk.title.clone(),
                    anchor: chunk
                        .href
                        .split_once('#')
                        .map(|(_, a)| format!("#{a}"))
                        .unwrap_or_default(),
                };
                if !matches.contains(&hit) {
                    matches.push(hit);
                }
            }
        }
        if all.iter().all(|&v| v) {
            kept.push(i);
            result.scores.insert(
                i,
                (
                    title_hits.iter().filter(|&&v| v).count(),
                    best_heading,
                    body_hits.iter().filter(|&&v| v).count(),
                ),
            );
            result.matches.insert(i, matches);
        }
    }
    *matched = kept;
    result
}

/// Only exact docs-scoped text searches default to prose-first candidates.
fn exclude_generated(q: &ContextQuery) -> bool {
    let exact_docs = matches!(&q.scope.orgs, OrgScope::List(orgs) if orgs.len() == 1 && orgs[0] == "codesense/docs");
    let explicit = [q.scope.path_prefix.as_deref(), q.scope.path_glob.as_deref()]
        .into_iter()
        .flatten()
        .any(|p| {
            let p = p.trim_start_matches("./").trim_end_matches('/');
            under_prefix(p, "generated") || p.ends_with("/generated") || p.contains("/generated/")
        });
    exact_docs && q.text.as_deref().is_some_and(|t| !t.trim().is_empty()) && !explicit
}

/// Compare two present values for a sort key: an explicit `order` first
/// (listed values by position, unlisted after), then the key's own rules.
fn ordered_cmp(norm_key: &str, explicit: Option<&[String]>, a: &str, b: &str) -> Ordering {
    if let Some(list) = explicit {
        let rank = |v: &str| {
            list.iter()
                .position(|o| o.eq_ignore_ascii_case(v))
                .unwrap_or(usize::MAX)
        };
        // Listed values by position, unlisted after; ties by the key's own order.
        return rank(a)
            .cmp(&rank(b))
            .then_with(|| value_cmp(norm_key, a, b));
    }
    value_cmp(norm_key, a, b)
}

/// Sort `matched` by the query's sort keys, ties by path.
pub fn sort_matches(corpus: &Corpus, matched: &mut [usize], q: &ContextQuery) {
    let keys: Vec<(SortKey, String)> = q
        .sort
        .iter()
        .map(|s| {
            let k = s.parts();
            let norm = alias(&normalize_key(&k.key)).to_string();
            (k, norm)
        })
        .collect();
    // Precompute each record's sort values once.
    let values: HashMap<usize, Vec<Option<String>>> = matched
        .iter()
        .map(|&i| {
            let r = &corpus.records[i];
            (i, keys.iter().map(|(k, _)| sort_value(r, &k.key)).collect())
        })
        .collect();
    matched.sort_by(|&a, &b| {
        for (n, (k, norm)) in keys.iter().enumerate() {
            let va = values[&a][n].as_deref();
            let vb = values[&b][n].as_deref();
            let ord = match (va, vb) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => match k.missing {
                    MissingOrder::First => Ordering::Less,
                    MissingOrder::Last => Ordering::Greater,
                },
                (Some(_), None) => match k.missing {
                    MissingOrder::First => Ordering::Greater,
                    MissingOrder::Last => Ordering::Less,
                },
                (Some(x), Some(y)) => {
                    let o = ordered_cmp(norm, k.order.as_deref(), x, y);
                    let dir = &k.dir;
                    if *dir == SortDir::Desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        corpus.records[a].path.cmp(&corpus.records[b].path)
    });
}

fn group_counts(
    corpus: &Corpus,
    matched: &[usize],
    key: &str,
    sort: &[SortSpec],
) -> Vec<GroupCount> {
    let norm = alias(&normalize_key(key)).to_string();
    // A sort on the same key with an explicit `order` orders the buckets too.
    let explicit: Option<Vec<String>> = sort
        .iter()
        .map(SortSpec::parts)
        .find(|k| alias(&normalize_key(&k.key)) == norm)
        .and_then(|k| k.order);
    let mut counts: BTreeMap<Option<String>, usize> = BTreeMap::new();
    for &i in matched {
        let vals = lookup(&corpus.records[i], key)
            .map(|v| items(v.get()))
            .unwrap_or_default();
        if vals.is_empty() {
            *counts.entry(None).or_default() += 1;
        } else {
            let unique: HashSet<String> = vals.into_iter().collect();
            for v in unique {
                *counts.entry(Some(v)).or_default() += 1;
            }
        }
    }
    let mut groups: Vec<GroupCount> = counts
        .into_iter()
        .map(|(value, count)| GroupCount { value, count })
        .collect();
    groups.sort_by(|a, b| match (&a.value, &b.value) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => ordered_cmp(&norm, explicit.as_deref(), x, y).then_with(|| x.cmp(y)),
    });
    groups
}

/// Build the output row for record `i`.
pub fn to_row(
    corpus: &Corpus,
    i: usize,
    select: Option<&[String]>,
    vis: &Visibility,
) -> ContextRow {
    let r = &corpus.records[i];
    let with_summary = select.is_some_and(|keys| {
        keys.iter()
            .any(|k| normalize_key(k).eq_ignore_ascii_case("summary"))
    });
    let (fields, with_rel) = match select {
        None => (r.fields.clone(), true),
        Some(keys) => {
            let wanted: Vec<String> = keys
                .iter()
                .map(|k| alias(&normalize_key(k)).to_string())
                .collect();
            let fields = r
                .fields
                .iter()
                .filter(|(k, _)| wanted.iter().any(|w| *w == alias(&normalize_key(k))))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (fields, wanted.iter().any(|w| w == "relationships"))
        }
    };
    let rel = corpus.relations;
    ContextRow {
        text_matches: Vec::new(),
        id: r.id().to_string(),
        path: r.path.clone(),
        rel_path: r.rel_path.clone(),
        file_name: r.file_name.clone(),
        org: r.org.clone(),
        team: r.team.clone(),
        layer: r.layer,
        kind: r.kind,
        mtime: r.mtime_ms,
        title: r.title.clone(),
        fields,
        metadata_truncated: r.metadata_truncated,
        summary: with_summary.then(|| r.summary.clone()),
        relationships: with_rel.then(|| RowRelationships {
            depends_on: vis.refs(&rel.depends_on[i]),
            dependents: vis.refs(&rel.dependents[i]),
            supporting_docs: vis.refs(&rel.supporting_docs[i]),
            backlinks: vis.refs(&rel.backlinks[i]),
            parent_epic: r.parent_epic().map(str::to_string),
            folder_epic: rel.folder_epic.get(i).cloned().flatten(),
        }),
    }
}

/// What a cursor is bound to: the query (minus its cursor) and the viewer's page.
fn cursor_key(q: &ContextQuery, viewer: &Viewer) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut bare = q.clone();
    bare.cursor = None;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&bare)
        .unwrap_or_default()
        .hash(&mut h);
    viewer.page.hash(&mut h);
    h.finish()
}

/// This app run's id, carried by every cursor: a cursor from an earlier run is
/// stale even where its generation number repeats (the counter restarts at 0).
fn boot_id() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| uuid::Uuid::new_v4().simple().to_string()[..12].to_string())
}

/// A cursor `o:<offset>:<generation>:<boot>:<key>`, checked against the query
/// it is paged with.
struct Cursor {
    offset: usize,
    generation: u64,
}

/// `q`'s cursor as `viewer` pages it (`None` for a first page). Malformed, or
/// another query's, is `InvalidCursor`; one from an earlier app run is
/// `StaleCursor`.
fn checked_cursor(q: &ContextQuery, viewer: &Viewer) -> Result<Option<Cursor>, QueryError> {
    let Some(c) = q.cursor.as_deref().filter(|c| !str::is_empty(c)) else {
        return Ok(None);
    };
    let parts: Vec<&str> = c.split(':').collect();
    let parsed = match parts.as_slice() {
        ["o", offset, generation, boot, key] => offset
            .parse::<usize>()
            .ok()
            .zip(generation.parse::<u64>().ok())
            .zip(u64::from_str_radix(key, 16).ok())
            .map(|((offset, generation), key)| (offset, generation, *boot, key)),
        _ => None,
    };
    let Some((offset, generation, boot, key)) = parsed else {
        return Err(QueryError::InvalidCursor(format!("invalid cursor: {c}")));
    };
    if key != cursor_key(q, viewer) {
        return Err(QueryError::InvalidCursor(
            "this cursor belongs to a different query; page with the query that returned it".into(),
        ));
    }
    if boot != boot_id() {
        return Err(QueryError::StaleCursor);
    }
    Ok(Some(Cursor { offset, generation }))
}

fn format_cursor(offset: usize, generation: u64, key: u64) -> String {
    format!("o:{offset}:{generation}:{}:{key:x}", boot_id())
}

/// `cursor` as an earlier app run would have made it.
#[cfg(test)]
pub(crate) fn from_an_earlier_run(cursor: &str) -> String {
    let mut parts: Vec<&str> = cursor.split(':').collect();
    assert_eq!(parts.len(), 5, "{cursor}");
    parts[3] = "earlierrun00";
    parts.join(":")
}

/// The index generation `q`'s cursor pages (`None` for a first page), so the
/// index can serve its later pages from that snapshot while it is pinned.
pub fn cursor_generation(q: &ContextQuery, viewer: &Viewer) -> Result<Option<u64>, QueryError> {
    Ok(checked_cursor(q, viewer)?.map(|c| c.generation))
}

/// Where `q`'s cursor resumes over the snapshot at `generation`. A cursor is
/// bound to the query and to the index snapshot: one from another query is
/// refused, and one from another snapshot is stale, rather than silently
/// skipping or repeating rows.
fn cursor_offset(q: &ContextQuery, viewer: &Viewer, generation: u64) -> Result<usize, QueryError> {
    match checked_cursor(q, viewer)? {
        None => Ok(0),
        Some(c) if c.generation == generation => Ok(c.offset),
        Some(_) => Err(QueryError::StaleCursor),
    }
}

/// The cursor that continues after the first `kept` rows of the page `result` answered for `q`,
/// for a caller that sent fewer rows than the page held (a byte cut). `None` when no rows follow.
pub fn cursor_after_kept(
    q: &ContextQuery,
    viewer: &Viewer,
    result: &ContextQueryResult,
    kept: usize,
) -> Option<String> {
    let start = cursor_offset(q, viewer, result.generation).ok()?;
    let end = start.saturating_add(kept);
    (kept > 0 && end < result.total)
        .then(|| format_cursor(end, result.generation, cursor_key(q, viewer)))
}

/// Run `q` over `corpus`.
pub fn run(
    corpus: &Corpus,
    q: &ContextQuery,
    viewer: &Viewer,
    reader: &dyn BodyReader,
) -> Result<ContextQueryResult, QueryError> {
    let exclude = exclude_generated(q);
    let mut matched = matching(corpus, q, viewer, &|r| {
        !exclude || !r.rel_path.starts_with("generated/")
    })?;
    let vis = Visibility::of(corpus, viewer, q);
    let text = q
        .text
        .as_deref()
        .map(|t| filter_text(corpus, &mut matched, t, reader, &vis))
        .unwrap_or_default();
    let text_scan_truncated = text.truncated;
    if q.sort.is_empty() && !text.scores.is_empty() {
        matched.sort_by(|a, b| {
            text.scores[b]
                .cmp(&text.scores[a])
                .then_with(|| corpus.records[*a].path.cmp(&corpus.records[*b].path))
        });
    } else {
        sort_matches(corpus, &mut matched, q);
    }
    let scope_orgs = compile_scope(q, corpus)?.orgs.map(|set| {
        let mut v: Vec<String> = set.into_iter().filter(|o| !o.is_empty()).collect();
        v.sort();
        v
    });
    let groups = q
        .group_by
        .as_deref()
        .map(|k| group_counts(corpus, &matched, k, &q.sort));
    let total = matched.len();
    let offset = cursor_offset(q, viewer, corpus.generation)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let end = offset.saturating_add(limit).min(total);
    let page = if offset < total {
        &matched[offset..end]
    } else {
        &[][..]
    };
    let vis = Visibility::of(corpus, viewer, q);
    let rows = page
        .iter()
        .map(|&i| {
            let mut row = to_row(corpus, i, q.select.as_deref(), &vis);
            row.text_matches = text.matches.get(&i).cloned().unwrap_or_default();
            row
        })
        .collect();
    Ok(ContextQueryResult {
        excluded: if exclude {
            vec!["generated/".into()]
        } else {
            Vec::new()
        },
        schema_version: CONTEXT_QUERY_SCHEMA_VERSION,
        total,
        rows,
        groups,
        // `limit: 0` (counts/groups only) has no next page to point at.
        next_cursor: (limit > 0 && end < total)
            .then(|| format_cursor(end, corpus.generation, cursor_key(q, viewer))),
        total_approximate: text_scan_truncated,
        text_scan_truncated,
        scope_orgs,
        indexing: false,
        generation: corpus.generation,
    })
}
