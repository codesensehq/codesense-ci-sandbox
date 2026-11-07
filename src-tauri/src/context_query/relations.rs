// SPDX-License-Identifier: MIT
//! The relationship resolver: `supporting_docs` (and their reverse, backlinks)
//! and `depends_on` (and their reverse, dependents) for a set of records, in
//! two linear passes.
//!
//! This is the one implementation. `doc_query` (the scanned-pipeline-tree
//! adapter) feeds it records built from a tree; the footprint index feeds it
//! every indexed file. Resolution rules (pinned for the tree adapter by the
//! golden fixture in `src/shared/lib/doc-query/__fixtures__`):
//!
//! - `supporting_docs` entries are paths relative to the declaring file. They
//!   resolve to a pipeline doc first (exact path, then extension-less path),
//!   then to any other indexed file; otherwise they surface as `resolved: false`.
//!   A file listing itself is dropped.
//! - `depends_on` entries containing `/` are relative paths to a pipeline doc.
//!   A bare entry resolves to a same-folder pipeline doc by stem, then to a
//!   pipeline doc with that `story_id` in the same content root, then the same
//!   org. A `story_id` claimed twice in a namespace resolves to nothing there
//!   (never an arbitrary pick). Self-deps drop.
//! - Links never cross orgs: a target in another org (private or not) stays
//!   `resolved: false`, so no row carries or matches on another org's files.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::model::{Layer, RelRef};
use super::record::ContextRecord;

/// Pipeline extensions, lowercase, in the order `strip_pipeline_ext` tries them.
const PIPELINE_EXTS: &[&str] = &[".md", ".html", ".htm"];

/// Resolve an absolute `/`-separated `base` dir against a relative `rel`,
/// collapsing `.`/`..`/empty segments; `..` never pops above the root.
/// String-only on purpose (logical `/` paths resolve identically everywhere).
pub(crate) fn resolve_relative_path(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        if seg == ".." {
            parts.pop();
        } else if seg != "." && !seg.is_empty() {
            parts.push(seg);
        }
    }
    let prefix = if base.starts_with('/') { "/" } else { "" };
    format!("{}{}", prefix, parts.join("/"))
}

pub(crate) fn dirname(p: &str) -> String {
    match p.rfind('/') {
        None => String::new(),
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

pub(crate) fn basename_of(p: &str) -> &str {
    match p.rfind('/') {
        None => p,
        Some(i) => &p[i + 1..],
    }
}

/// Case-insensitive strip of a pipeline extension, keeping the stem's casing.
pub(crate) fn strip_pipeline_ext(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in PIPELINE_EXTS {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    name.to_string()
}

/// Resolved relationships, indexed by record position.
#[derive(Debug, Clone, Default)]
pub struct Relations {
    pub supporting_docs: Vec<Vec<RelRef>>,
    pub backlinks: Vec<Vec<RelRef>>,
    pub depends_on: Vec<Vec<RelRef>>,
    pub dependents: Vec<Vec<RelRef>>,
    /// The folder epic each record sits in: the name of the nearest folder
    /// (itself included, below the content root) holding a main doc
    /// (`README`/`index`), as the left nav derives epics.
    pub folder_epic: Vec<Option<String>>,
}

/// Is `file_name` an epic folder's main doc, by the nav's default names?
fn is_main_doc_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    crate::commands::pipeline_scanner::MAIN_DOC_DEFAULTS.contains(&lower.as_str())
}

/// The content root `r` was found under (its path minus its relative path).
fn root_dir(r: &ContextRecord) -> &str {
    r.path
        .strip_suffix(r.rel_path.as_str())
        .map(|p| p.trim_end_matches('/'))
        .unwrap_or("")
}

/// Every record's folder epic (see [`Relations::folder_epic`]).
fn folder_epics(records: &[Arc<ContextRecord>]) -> Vec<Option<String>> {
    let epic_dirs: HashSet<String> = records
        .iter()
        .filter(|r| r.rel_path.contains('/') && is_main_doc_name(&r.file_name))
        .map(|r| dirname(&r.path))
        .collect();
    records
        .iter()
        .map(|r| {
            let root = root_dir(r);
            let mut dir = dirname(&r.path);
            while dir.len() > root.len() && dir.starts_with(root) {
                if epic_dirs.contains(&dir) {
                    return Some(basename_of(&dir).to_string());
                }
                let up = dirname(&dir);
                // `dirname("/")` is `/`: stop at the top, whatever the root.
                if up.len() >= dir.len() {
                    break;
                }
                dir = up;
            }
            None
        })
        .collect()
}

fn rel_ref(r: &ContextRecord) -> RelRef {
    RelRef {
        id: r.id().to_string(),
        path: r.path.clone(),
        file_name: r.file_name.clone(),
        resolved: true,
    }
}

fn unresolved(reference: &str, abs: String) -> RelRef {
    let file_name = basename_of(reference).to_string();
    RelRef {
        id: strip_pipeline_ext(&file_name),
        path: abs,
        file_name,
        resolved: false,
    }
}

/// Insertion-ordered refs, deduplicated by path (first write wins).
#[derive(Default)]
struct OrderedRefs {
    vec: Vec<RelRef>,
    seen: HashSet<String>,
}

impl OrderedRefs {
    fn push(&mut self, r: RelRef) {
        if self.seen.insert(r.path.clone()) {
            self.vec.push(r);
        }
    }
}

/// May `source` link to `target`? Within one org, or to all-orgs content —
/// never across orgs, so a row never carries (or matches on) another org's
/// files, private ones included. All-orgs content links only to all-orgs
/// content, since its rows show in every org.
fn visible(source: &ContextRecord, target: &ContextRecord) -> bool {
    target.org.is_none() || target.org == source.org
}

/// The story-id namespaces a record's bare `depends_on` ids resolve in, most
/// specific first: its own content root (org + team + layer), then its org.
type IdKey<'a> = (Option<&'a str>, Option<&'a str>, Option<Layer>, &'a str);

fn root_key<'a>(r: &'a ContextRecord, id: &'a str) -> IdKey<'a> {
    (r.org.as_deref(), r.team.as_deref(), Some(r.layer), id)
}

fn org_key<'a>(r: &'a ContextRecord, id: &'a str) -> IdKey<'a> {
    (r.org.as_deref(), None, None, id)
}

/// Resolve every record's relationships. Records are processed in slice order,
/// which fixes the order of backlinks/dependents.
pub fn build(records: &[Arc<ContextRecord>]) -> Relations {
    let n = records.len();
    let mut by_path: HashMap<&str, usize> = HashMap::new();
    let mut by_path_no_ext: HashMap<String, usize> = HashMap::new();
    let mut by_sibling_stem: HashMap<String, usize> = HashMap::new();
    // `None` poisons an id claimed twice within one namespace.
    let mut by_story_id: HashMap<IdKey, Option<usize>> = HashMap::new();
    let mut other_by_path: HashMap<&str, usize> = HashMap::new();
    let mut other_by_path_no_ext: HashMap<String, usize> = HashMap::new();

    for (i, r) in records.iter().enumerate() {
        if r.is_pipeline_doc {
            by_path.insert(r.path.as_str(), i);
            by_path_no_ext.insert(strip_pipeline_ext(&r.path), i);
            by_sibling_stem.insert(
                format!(
                    "{}\u{0}{}",
                    dirname(&r.path),
                    strip_pipeline_ext(&r.file_name)
                ),
                i,
            );
            if let Some(id) = r.str_field("story_id").filter(|id| !id.is_empty()) {
                for key in [root_key(r, id), org_key(r, id)] {
                    by_story_id
                        .entry(key)
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(i));
                }
            }
        } else {
            other_by_path.insert(r.path.as_str(), i);
            other_by_path_no_ext.insert(strip_pipeline_ext(&r.path), i);
        }
    }

    let resolve_pipeline = |abs: &str| -> Option<usize> {
        by_path
            .get(abs)
            .copied()
            .or_else(|| by_path_no_ext.get(&strip_pipeline_ext(abs)).copied())
    };
    let resolve_other = |abs: &str| -> Option<usize> {
        other_by_path
            .get(abs)
            .copied()
            .or_else(|| other_by_path_no_ext.get(&strip_pipeline_ext(abs)).copied())
    };

    let mut supporting: Vec<OrderedRefs> = (0..n).map(|_| OrderedRefs::default()).collect();
    let mut backlinks: Vec<OrderedRefs> = (0..n).map(|_| OrderedRefs::default()).collect();
    let mut depends: Vec<OrderedRefs> = (0..n).map(|_| OrderedRefs::default()).collect();
    let mut dependents: Vec<OrderedRefs> = (0..n).map(|_| OrderedRefs::default()).collect();

    for (i, r) in records.iter().enumerate() {
        let dir = dirname(&r.path);
        let seen = |t: Option<usize>| t.filter(|&t| visible(r, &records[t]));

        for rel in r.list_field("supporting_docs") {
            let abs = resolve_relative_path(&dir, rel);
            match seen(resolve_pipeline(&abs).or_else(|| resolve_other(&abs))) {
                Some(t) if t == i => {} // a file listing itself is not context
                Some(t) => {
                    supporting[i].push(rel_ref(&records[t]));
                    backlinks[t].push(rel_ref(r));
                }
                None => supporting[i].push(unresolved(rel, abs)),
            }
        }

        for reference in r.list_field("depends_on") {
            let (target, abs) = if reference.contains('/') {
                let abs = resolve_relative_path(&dir, reference);
                (seen(resolve_pipeline(&abs)), abs)
            } else {
                let key = format!("{}\u{0}{}", dir, strip_pipeline_ext(reference));
                let by_id = |k: IdKey| by_story_id.get(&k).copied().flatten();
                let t = seen(by_sibling_stem.get(&key).copied())
                    .or_else(|| seen(by_id(root_key(r, reference))))
                    .or_else(|| seen(by_id(org_key(r, reference))));
                let abs = t
                    .map(|t| records[t].path.clone())
                    .unwrap_or_else(|| resolve_relative_path(&dir, reference));
                (t, abs)
            };
            match target {
                Some(t) if t == i => {}
                Some(t) => {
                    depends[i].push(rel_ref(&records[t]));
                    dependents[t].push(rel_ref(r));
                }
                None => depends[i].push(unresolved(reference, abs)),
            }
        }
    }

    let finish = |v: Vec<OrderedRefs>| v.into_iter().map(|o| o.vec).collect();
    Relations {
        supporting_docs: finish(supporting),
        backlinks: finish(backlinks),
        depends_on: finish(depends),
        dependents: finish(dependents),
        folder_epic: folder_epics(records),
    }
}

/// A ref matches a path, id, file name or stem token.
pub(crate) fn ref_matches(refs: &[RelRef], token: &str) -> bool {
    let token_stem = strip_pipeline_ext(basename_of(token));
    refs.iter().any(|r| {
        let stem = strip_pipeline_ext(&r.file_name);
        r.path == token
            || r.id == token
            || r.file_name == token
            || stem == token
            || stem == token_stem
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_path_collapses_and_floors() {
        assert_eq!(
            resolve_relative_path("/ctx/epic-a", "./shared-notes.md"),
            "/ctx/epic-a/shared-notes.md"
        );
        assert_eq!(
            resolve_relative_path("/ctx/epic-b", "../epic-a/a1-foundation"),
            "/ctx/epic-a/a1-foundation"
        );
        assert_eq!(resolve_relative_path("/a", "../../../x"), "/x");
    }

    fn record(path: &str, rel_path: &str) -> Arc<ContextRecord> {
        Arc::new(ContextRecord {
            headings: Vec::new(),
            path: path.into(),
            rel_path: rel_path.into(),
            file_name: basename_of(path).into(),
            org: None,
            team: None,
            layer: Layer::Team,
            kind: super::super::model::Kind::Doc,
            mtime_ms: None,
            title: None,
            fields: Default::default(),
            is_pipeline_doc: false,
            metadata_truncated: false,
            metadata_valid: true,
            summary: None,
        })
    }

    #[test]
    fn folder_epics_terminate_when_the_relative_path_is_not_a_suffix() {
        // A tree adapter's record whose rel_path does not end its path: no root
        // to stop at, so the walk must stop at `/`.
        let records = vec![
            record("/r/content/e/README.md", "e/README.md"),
            record("/r/content/e/S-1.md", "x/S-1.md"),
            record("/elsewhere/x.md", "other/x.md"),
        ];
        assert_eq!(
            folder_epics(&records),
            vec![Some("e".to_string()), Some("e".to_string()), None]
        );
    }

    #[test]
    fn strip_pipeline_ext_is_case_insensitive_preserving_stem() {
        assert_eq!(strip_pipeline_ext("Foo.MD"), "Foo");
        assert_eq!(strip_pipeline_ext("bar.html"), "bar");
        assert_eq!(strip_pipeline_ext("baz.bin"), "baz.bin");
    }
}
