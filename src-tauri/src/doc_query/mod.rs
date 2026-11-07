// SPDX-License-Identifier: MIT
//! SW-DX-2 — the scanned-pipeline-tree query, now an adapter over the one
//! context query engine (`crate::context_query`, SW-OC-26).
//!
//! `query_docs(tree, filter)` is a pure function: no I/O, no Tauri, no async.
//! It turns a scanned [`DerivedPipelineTree`] into context records (stories
//! are the returnable set; plain docs are resolution targets only), resolves
//! relationships with the shared resolver, translates the legacy [`DocQuery`]
//! into a [`ContextQuery`], and shapes the matches into the stable, versioned
//! [`DocQueryResult`] the frontend's `DocSummary` mirrors.
//!
//! The JSON shape is pinned by the golden fixture: this query over
//! `sample-tree.json` with `{ sort: "storyId" }` must equal
//! `golden-query-result.json` (compared as parsed JSON).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::pipeline_scanner::{DerivedPipelineTree, DocRef, StoryRef};
use crate::context_query::engine::{self, Corpus, Viewer};
use crate::context_query::model::{
    ContextQuery, FilterClause, FilterOp, FullSort, Kind, Layer, MissingOrder, RelationshipFilter,
    SortDir, SortSpec, WhereScope,
};
use crate::context_query::record::ContextRecord;
use crate::context_query::relations::{self, Relations};

pub use crate::context_query::model::RelRef;

/// Current stable shape version. Mirror of the TS `DOC_QUERY_SCHEMA_VERSION`.
/// Bump on any breaking [`DocSummary`] change so downstream consumers fail fast.
///
/// v2 (SW-DX-9): [`DocSummary`] gains `mtime` + `source_repo` (freshness data).
/// v3 (SW-SB-1): [`DocSummary`] gains `sprint` + `points` + `assigned_to`
/// (board metadata, for non-board readers).
pub const DOC_QUERY_SCHEMA_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// Stable result types (camelCase JSON, mirror types.ts EXACTLY)
// ---------------------------------------------------------------------------

/// The relationship bundle carried on every [`DocSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocRelationships {
    /// Docs this doc forward-links via `supporting_docs`.
    pub supporting_docs: Vec<RelRef>,
    /// Docs that list THIS doc in their `supporting_docs`.
    pub backlinks: Vec<RelRef>,
    /// Docs this doc `depends_on`.
    pub depends_on: Vec<RelRef>,
}

/// One doc in a query result. The stable per-doc contract.
// SW-SB-1: `Eq` dropped — `points: Option<f64>` is not `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocSummary {
    /// `story_id ?? file_name`.
    pub id: String,
    /// Absolute `file_path`.
    pub path: String,
    pub file_name: String,
    /// Open/arbitrary frontmatter tags (incl. SW-DX-1 schema tags), value lists.
    pub tags: BTreeMap<String, Vec<String>>,
    /// Raw status string (unknown values retained literally). `null` when absent.
    pub status: Option<String>,
    /// Raw `type` frontmatter (`null` ⇒ treated as `"story"` by consumers).
    pub document_type: Option<String>,
    /// SW-DX-9 (v2): the doc file's mtime in epoch **milliseconds**. `null` when
    /// the scan didn't record one. Milliseconds keep the value within JS's
    /// safe-integer range.
    pub mtime: Option<u64>,
    /// SW-DX-9 (v2): the doc's raw `source_repo` slug. `null` when absent.
    pub source_repo: Option<String>,
    /// SW-SB-1 (v3): sprint tag. `null` when absent.
    pub sprint: Option<String>,
    /// SW-SB-1 (v3): story estimate in points. `null` when absent/malformed.
    pub points: Option<f64>,
    /// SW-SB-1 (v3): assignee username. `null` when absent.
    pub assigned_to: Option<String>,
    pub relationships: DocRelationships,
}

/// The versioned result envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocQueryResult {
    pub schema_version: u32,
    pub docs: Vec<DocSummary>,
}

// ---------------------------------------------------------------------------
// Filter types (the legacy query shape; translated onto `ContextQuery`)
// ---------------------------------------------------------------------------

/// Relationship predicates. Multiple predicates AND together.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRelationshipFilter {
    /// Doc forward-links this path/id/stem via `supporting_docs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_supporting_doc: Option<String>,
    /// Doc is backlinked BY this path/id/stem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlinked_by: Option<String>,
    /// Doc `depends_on` this id/stem/path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// Doc has ≥1 backlink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_backlinked: Option<bool>,
    /// Doc has ≥1 resolved-or-unresolved dependency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_deps: Option<bool>,
}

/// A tag filter value: a single required value or an any-of list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TagFilterValue {
    One(String),
    Any(Vec<String>),
}

/// A status filter value: a single status or an any-of list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatusFilter {
    One(String),
    Any(Vec<String>),
}

fn one_or_many(one: Option<&String>, many: Option<&Vec<String>>) -> Value {
    match (one, many) {
        (Some(s), _) => Value::Array(vec![Value::String(s.clone())]),
        (None, Some(v)) => Value::Array(v.iter().cloned().map(Value::String).collect()),
        (None, None) => Value::Array(vec![]),
    }
}

impl TagFilterValue {
    fn to_json(&self) -> Value {
        match self {
            TagFilterValue::One(s) => one_or_many(Some(s), None),
            TagFilterValue::Any(v) => one_or_many(None, Some(v)),
        }
    }
}

impl StatusFilter {
    fn to_json(&self) -> Value {
        match self {
            StatusFilter::One(s) => one_or_many(Some(s), None),
            StatusFilter::Any(v) => one_or_many(None, Some(v)),
        }
    }
}

/// The filter. All dimensions optional; omitted ⇒ no constraint on that axis.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocQuery {
    /// field → required value(s). Reads open frontmatter tags AND well-known
    /// scalar `StoryRef` fields. Multiple fields AND; an array value is any-of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<BTreeMap<String, TagFilterValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<DocRelationshipFilter>,
    /// Single status or any-of array, matched against raw `status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusFilter>,
    /// Absolute path prefix bounding which docs are RETURNED (backlinks still
    /// resolve against the full tree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Caps result length AFTER sort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl DocQuery {
    /// The same query in the engine's model. Filters keep their meaning: tag
    /// keys AND, an array is any-of, `status` matches raw status; the sort keys
    /// reproduce the legacy order (status rank, then `story_id` with
    /// id-less docs first, then path).
    pub fn to_context_query(&self) -> ContextQuery {
        let mut filters = Vec::new();
        if let Some(status) = &self.status {
            filters.push(FilterClause {
                key: "status".into(),
                op: FilterOp::In,
                value: Some(status.to_json()),
                min: None,
                max: None,
            });
        }
        for (key, wanted) in self.tag.iter().flatten() {
            filters.push(FilterClause {
                key: key.clone(),
                op: FilterOp::In,
                value: Some(wanted.to_json()),
                min: None,
                max: None,
            });
        }
        let story_id_first = SortSpec::Full(FullSort {
            key: "story_id".into(),
            dir: SortDir::Asc,
            missing: MissingOrder::First,
            order: None,
        });
        let sort = match self.sort.as_deref() {
            Some("path") => vec![],
            Some("fileName") => vec![SortSpec::Short("fileName".into())],
            Some("storyId") => vec![story_id_first],
            _ => vec![SortSpec::Short("status".into()), story_id_first],
        };
        ContextQuery {
            scope: WhereScope {
                path_prefix: self.scope.clone(),
                ..Default::default()
            },
            filters,
            relationships: self.relationship.as_ref().map(|r| RelationshipFilter {
                depends_on: r.depends_on.clone(),
                has_supporting_doc: r.has_supporting_doc.clone(),
                backlinked_by: r.backlinked_by.clone(),
                has_deps: r.has_deps,
                is_backlinked: r.is_backlinked,
                ..Default::default()
            }),
            sort,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Tree → records
// ---------------------------------------------------------------------------

fn collect_all_stories(tree: &DerivedPipelineTree) -> Vec<&StoryRef> {
    fn walk<'a>(node: &'a DerivedPipelineTree, out: &mut Vec<&'a StoryRef>) {
        out.extend(node.stories.iter());
        for sub in &node.sub_epics {
            walk(sub, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

fn collect_all_docs(tree: &DerivedPipelineTree) -> Vec<&DocRef> {
    fn walk<'a>(node: &'a DerivedPipelineTree, out: &mut Vec<&'a DocRef>) {
        out.extend(node.docs.iter());
        for sub in &node.sub_epics {
            walk(sub, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

fn str_list(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

/// A story as a context record: open tags plus the named scalar fields under
/// their frontmatter names. `rel_path` is the absolute path, so a legacy
/// `scope` compares against the full path either way.
fn story_record(s: &StoryRef) -> ContextRecord {
    let mut fields: BTreeMap<String, Value> = s
        .open_tags
        .iter()
        .map(|(k, v)| (k.clone(), str_list(v)))
        .collect();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            fields.insert(k.to_string(), v);
        }
    };
    put("status", s.status.clone().map(Value::String));
    put("team", s.team.clone().map(Value::String));
    put("source_repo", s.source_repo.clone().map(Value::String));
    put("type", s.document_type.clone().map(Value::String));
    put("story_id", s.story_id.clone().map(Value::String));
    put("depends_on", s.depends_on.as_deref().map(str_list));
    put(
        "supporting_docs",
        s.supporting_docs.as_deref().map(str_list),
    );
    put("sprint", s.sprint.clone().map(Value::String));
    put("assigned_to", s.assigned_to.clone().map(Value::String));
    put(
        "points",
        s.points
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
    );
    ContextRecord {
        headings: Vec::new(),
        path: s.file_path.clone(),
        rel_path: s.file_path.clone(),
        file_name: s.file_name.clone(),
        org: None,
        team: None,
        layer: Layer::Team,
        kind: Kind::Story,
        mtime_ms: s.mtime.map(|ns| ns / 1_000_000),
        title: None,
        fields,
        is_pipeline_doc: true,
        metadata_truncated: false,
        metadata_valid: true,
        summary: None,
    }
}

/// A plain doc: a resolution target only (never returned).
fn doc_record(d: &DocRef) -> ContextRecord {
    ContextRecord {
        headings: Vec::new(),
        path: d.file_path.clone(),
        rel_path: d.file_path.clone(),
        file_name: d.file_name.clone(),
        org: None,
        team: None,
        layer: Layer::Team,
        kind: Kind::Doc,
        mtime_ms: None,
        title: None,
        fields: BTreeMap::new(),
        is_pipeline_doc: false,
        metadata_truncated: false,
        metadata_valid: true,
        summary: None,
    }
}

fn to_summary(s: &StoryRef, relations: &Relations, i: usize) -> DocSummary {
    DocSummary {
        id: s.story_id.clone().unwrap_or_else(|| s.file_name.clone()),
        path: s.file_path.clone(),
        file_name: s.file_name.clone(),
        tags: s.open_tags.clone(),
        status: s.status.clone(),
        document_type: s.document_type.clone(),
        // ns → ms at the JS boundary (see `DocSummary::mtime`).
        mtime: s.mtime.map(|ns| ns / 1_000_000),
        source_repo: s.source_repo.clone(),
        sprint: s.sprint.clone(),
        points: s.points,
        assigned_to: s.assigned_to.clone(),
        relationships: DocRelationships {
            supporting_docs: relations.supporting_docs[i].clone(),
            backlinks: relations.backlinks[i].clone(),
            depends_on: relations.depends_on[i].clone(),
        },
    }
}

/// The pure query seam. Deterministic given its inputs and never panics: empty
/// tree / no match / null fields / unknown values → `{ schema_version, docs: [] }`.
pub fn query_docs(tree: &DerivedPipelineTree, filter: &DocQuery) -> DocQueryResult {
    let stories = collect_all_stories(tree);
    let records: Vec<Arc<ContextRecord>> = stories
        .iter()
        .map(|s| Arc::new(story_record(s)))
        .chain(
            collect_all_docs(tree)
                .into_iter()
                .map(|d| Arc::new(doc_record(d))),
        )
        .collect();
    let relations = relations::build(&records);
    let corpus = Corpus {
        records: &records,
        relations: &relations,
        roots: &[],
        generation: 0,
    };
    let q = filter.to_context_query();
    // The translated query carries no glob, so compiling it cannot fail.
    let mut matched = engine::matching(&corpus, &q, &Viewer::default(), &|r| r.is_pipeline_doc)
        .unwrap_or_default();
    engine::sort_matches(&corpus, &mut matched, &q);
    if let Some(limit) = filter.limit {
        matched.truncate(limit);
    }
    DocQueryResult {
        schema_version: DOC_QUERY_SCHEMA_VERSION,
        // Stories come first in `records`, so a match index is a story index.
        docs: matched
            .into_iter()
            .map(|i| to_summary(stories[i], &relations, i))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deserialize the shared sample-tree fixture into a `DerivedPipelineTree`.
    fn sample_tree() -> DerivedPipelineTree {
        let raw = include_str!("../../../src/shared/lib/doc-query/__fixtures__/sample-tree.json");
        serde_json::from_str(raw).expect("sample-tree.json deserializes into DerivedPipelineTree")
    }

    // ── AC-12: the cross-language golden ────────────────────────────────
    #[test]
    fn golden_query_result_matches_ts() {
        let tree = sample_tree();
        let filter = DocQuery {
            sort: Some("storyId".to_string()),
            ..Default::default()
        };
        let result = query_docs(&tree, &filter);
        let got: serde_json::Value = serde_json::to_value(&result).expect("serialize result");

        let golden_raw =
            include_str!("../../../src/shared/lib/doc-query/__fixtures__/golden-query-result.json");
        let want: serde_json::Value =
            serde_json::from_str(golden_raw).expect("golden parses as JSON");

        assert_eq!(
            got, want,
            "Rust query_docs output must equal the cross-language golden (AC-12)"
        );
    }

    // ── leaf resolved:false + backlink fan + self-dep drop ──────────────
    #[test]
    fn unresolved_leaf_is_surfaced_not_dropped() {
        let tree = sample_tree();
        let res = query_docs(&tree, &DocQuery::default());
        let a1 = res.docs.iter().find(|d| d.id == "A-1").unwrap();
        // `./diagram.bin` is a binary leaf → resolved:false, still present.
        let leaf = a1
            .relationships
            .supporting_docs
            .iter()
            .find(|r| r.file_name == "diagram.bin")
            .expect("unresolved leaf surfaced");
        assert!(!leaf.resolved);
        // A-1 is backlinked by A-2 (which supports it).
        assert!(a1.relationships.backlinks.iter().any(|b| b.id == "A-2"));
    }

    #[test]
    fn cross_epic_depends_on_resolves() {
        let tree = sample_tree();
        let res = query_docs(&tree, &DocQuery::default());
        let b1 = res.docs.iter().find(|d| d.id == "B-1").unwrap();
        let dep = &b1.relationships.depends_on;
        assert_eq!(dep.len(), 1);
        assert_eq!(dep[0].id, "A-1");
        assert!(dep[0].resolved);
    }

    // ── SW-SB-9: location-independent (story_id) depends_on resolution ────
    /// Two epics in different dirs; `a1` depends on the bare story_id `B-1`
    /// living in `epic-b`. A bare stem could never reach it — story_id does.
    fn story_id_tree(target_dir: &str) -> DerivedPipelineTree {
        let raw = format!(
            r#"{{
                "dirPath": "", "dirName": "", "absoluteDirPath": "/ctx",
                "stories": [], "subEpics": [
                    {{"dirPath":"epic-a","dirName":"epic-a","absoluteDirPath":"/ctx/epic-a",
                     "stories":[
                        {{"fileName":"by-id.md","filePath":"/ctx/epic-a/by-id.md","dependsOn":["TGT"],
                          "status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"BY-ID"}},
                        {{"fileName":"by-stem.md","filePath":"/ctx/epic-a/by-stem.md","dependsOn":["tgt"],
                          "status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"BY-STEM"}}
                     ],
                     "subEpics":[],"orderError":null,"mainDoc":null}},
                    {{"dirPath":"{dir}","dirName":"{dir}","absoluteDirPath":"/ctx/{dir}",
                     "stories":[{{"fileName":"tgt.md","filePath":"/ctx/{dir}/tgt.md","dependsOn":null,
                       "status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"TGT"}}],
                     "subEpics":[],"orderError":null,"mainDoc":null}}
                ], "orderError": null, "mainDoc": null
            }}"#,
            dir = target_dir
        );
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn depends_on_resolves_by_global_story_id_across_dirs() {
        // Target lives in epic-a alongside the dependants (sibling); both forms resolve.
        let res = query_docs(&story_id_tree("epic-a"), &DocQuery::default());
        let by_id = res.docs.iter().find(|d| d.id == "BY-ID").unwrap();
        assert_eq!(by_id.relationships.depends_on[0].id, "TGT");
        assert!(by_id.relationships.depends_on[0].resolved);
    }

    #[test]
    fn story_id_edge_survives_move_bare_stem_does_not() {
        // Move the target out to epic-b: the bare-stem ref breaks; story_id survives.
        let res = query_docs(&story_id_tree("epic-b"), &DocQuery::default());
        let by_id = res.docs.iter().find(|d| d.id == "BY-ID").unwrap();
        let by_stem = res.docs.iter().find(|d| d.id == "BY-STEM").unwrap();
        assert!(
            by_id.relationships.depends_on[0].resolved,
            "story_id edge survives the move"
        );
        assert_eq!(by_id.relationships.depends_on[0].path, "/ctx/epic-b/tgt.md");
        assert!(
            !by_stem.relationships.depends_on[0].resolved,
            "bare-stem edge breaks on the move"
        );
    }

    #[test]
    fn duplicate_story_id_is_poisoned_not_resolved() {
        // Two stories claim `DUP`; a dependant referencing it must stay unresolved
        // rather than silently bind to an arbitrary one (AC-5 soundness).
        let raw = r#"{
            "dirPath": "", "dirName": "", "absoluteDirPath": "/ctx",
            "stories": [], "subEpics": [
                {"dirPath":"a","dirName":"a","absoluteDirPath":"/ctx/a",
                 "stories":[{"fileName":"dep.md","filePath":"/ctx/a/dep.md","dependsOn":["DUP"],
                   "status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"DEP"}],
                 "subEpics":[],"orderError":null,"mainDoc":null},
                {"dirPath":"b","dirName":"b","absoluteDirPath":"/ctx/b",
                 "stories":[
                    {"fileName":"one.md","filePath":"/ctx/b/one.md","dependsOn":null,"status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"DUP"},
                    {"fileName":"two.md","filePath":"/ctx/b/two.md","dependsOn":null,"status":"todo","documentType":"story","supportingDocs":null,"sourceRepo":null,"team":null,"storyId":"DUP"}
                 ],
                 "subEpics":[],"orderError":null,"mainDoc":null}
            ], "orderError": null, "mainDoc": null
        }"#;
        let tree: DerivedPipelineTree = serde_json::from_str(raw).unwrap();
        let res = query_docs(&tree, &DocQuery::default());
        let dep = res.docs.iter().find(|d| d.id == "DEP").unwrap();
        assert!(!dep.relationships.depends_on[0].resolved);
    }

    // ── tag AND / any-of / unknown-literal ──────────────────────────────
    #[test]
    fn tag_filter_and_semantics() {
        let tree = sample_tree();
        let mut tag = BTreeMap::new();
        tag.insert(
            "context_level".to_string(),
            TagFilterValue::One("team".to_string()),
        );
        tag.insert("area".to_string(), TagFilterValue::One("query".to_string()));
        let res = query_docs(
            &tree,
            &DocQuery {
                tag: Some(tag),
                ..Default::default()
            },
        );
        // Only A-1 has BOTH context_level:team AND area:query.
        assert_eq!(res.docs.len(), 1);
        assert_eq!(res.docs[0].id, "A-1");
    }

    #[test]
    fn tag_filter_any_of() {
        let tree = sample_tree();
        let mut tag = BTreeMap::new();
        tag.insert(
            "context_level".to_string(),
            TagFilterValue::Any(vec!["org".to_string(), "galaxy".to_string()]),
        );
        let res = query_docs(
            &tree,
            &DocQuery {
                tag: Some(tag),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"A-2")); // org
        assert!(ids.contains(&"B-2")); // galaxy
        assert!(!ids.contains(&"A-1")); // team
    }

    #[test]
    fn tag_filter_unknown_literal_still_matches() {
        let tree = sample_tree();
        // `context_level: galaxy` is off-vocabulary but must match literally.
        let mut tag = BTreeMap::new();
        tag.insert(
            "context_level".to_string(),
            TagFilterValue::One("galaxy".to_string()),
        );
        let res = query_docs(
            &tree,
            &DocQuery {
                tag: Some(tag),
                ..Default::default()
            },
        );
        assert_eq!(res.docs.len(), 1);
        assert_eq!(res.docs[0].id, "B-2");
    }

    #[test]
    fn tag_filter_named_scalar_field() {
        let tree = sample_tree();
        // `status` is a named scalar field, targetable via `tag`.
        let mut tag = BTreeMap::new();
        tag.insert(
            "status".to_string(),
            TagFilterValue::One("done".to_string()),
        );
        let res = query_docs(
            &tree,
            &DocQuery {
                tag: Some(tag),
                ..Default::default()
            },
        );
        assert_eq!(res.docs.len(), 1);
        assert_eq!(res.docs[0].id, "A-S");
    }

    // ── status any-of + unknown retained ────────────────────────────────
    #[test]
    fn status_any_of_and_unknown_retained() {
        let tree = sample_tree();
        // `shipped` is off-vocabulary but retained literally on B-2, and the
        // any-of matches it plus a known status.
        let res = query_docs(
            &tree,
            &DocQuery {
                status: Some(StatusFilter::Any(vec![
                    "shipped".to_string(),
                    "todo".to_string(),
                ])),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"B-2")); // shipped (unknown, retained)
        assert!(ids.contains(&"A-1")); // todo
        assert_eq!(ids.len(), 2);
    }

    // ── relationship predicates ─────────────────────────────────────────
    #[test]
    fn relationship_predicates() {
        let tree = sample_tree();

        // is_backlinked: A-1 and A-S are backlinked; A-2, B-1, B-2 are not.
        let res = query_docs(
            &tree,
            &DocQuery {
                relationship: Some(DocRelationshipFilter {
                    is_backlinked: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"A-1"));
        assert!(ids.contains(&"A-S"));
        assert!(!ids.contains(&"A-2"));

        // has_deps: A-2 and B-1 have deps.
        let res = query_docs(
            &tree,
            &DocQuery {
                relationship: Some(DocRelationshipFilter {
                    has_deps: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"A-2"));
        assert!(ids.contains(&"B-1"));
        assert_eq!(ids.len(), 2);

        // depends_on a specific target (by id/stem).
        let res = query_docs(
            &tree,
            &DocQuery {
                relationship: Some(DocRelationshipFilter {
                    depends_on: Some("a1-foundation".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"A-2"));
        assert!(ids.contains(&"B-1"));
    }

    // ── scope bounding (backlinks still resolve against full tree) ──────
    #[test]
    fn scope_bounds_returned_docs_only() {
        let tree = sample_tree();
        let res = query_docs(
            &tree,
            &DocQuery {
                scope: Some("/ctx/epic-a".to_string()),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.iter().all(|id| ["A-1", "A-2", "A-S"].contains(id)));
        assert!(!ids.contains(&"B-1"));
        // A-1 is still backlinked by A-2 (both in scope here) — full-tree resolve.
        let a1 = res.docs.iter().find(|d| d.id == "A-1").unwrap();
        assert!(a1.relationships.backlinks.iter().any(|b| b.id == "A-2"));
    }

    // ── sort + limit ────────────────────────────────────────────────────
    #[test]
    fn sort_story_id_then_limit() {
        let tree = sample_tree();
        let res = query_docs(
            &tree,
            &DocQuery {
                sort: Some("storyId".to_string()),
                limit: Some(2),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = res.docs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["A-1", "A-2"]);
    }

    #[test]
    fn empty_filter_returns_all_with_schema_version() {
        let tree = sample_tree();
        let res = query_docs(&tree, &DocQuery::default());
        assert_eq!(res.schema_version, DOC_QUERY_SCHEMA_VERSION);
        assert_eq!(res.docs.len(), 5);
    }

    // ── self-dep is dropped (a story depending on itself) ───────────────
    #[test]
    fn self_dependency_is_dropped() {
        // Build a minimal tree with one story that depends on its own stem.
        let raw = r#"{
            "dirPath": "", "dirName": "", "absoluteDirPath": "/x",
            "stories": [{
                "fileName": "solo.md", "filePath": "/x/solo.md",
                "dependsOn": ["solo"], "status": "todo",
                "documentType": "story", "supportingDocs": null,
                "sourceRepo": null, "team": null, "storyId": "S-1"
            }],
            "subEpics": [], "orderError": null, "mainDoc": null
        }"#;
        let tree: DerivedPipelineTree = serde_json::from_str(raw).unwrap();
        let res = query_docs(&tree, &DocQuery::default());
        let s1 = res.docs.iter().find(|d| d.id == "S-1").unwrap();
        assert!(
            s1.relationships.depends_on.is_empty(),
            "self-dep must be dropped"
        );
    }

    // ── RF-16: measurement-before-optimize benches (AC-1) ───────────────
    // Machine-dependent → `#[ignore]`d (never a CI gate), RF-perf-9 style.
    // Run: `cargo test -p codesense --lib rf16_ -- --ignored --nocapture`.

    /// Minimal synthetic `StoryRef` for the benches (all-default except the
    /// axes a bench exercises). Kept flat so a bench reads as data, not setup.
    fn synth_story(
        idx: usize,
        supporting: Vec<String>,
        open_tags: BTreeMap<String, Vec<String>>,
    ) -> StoryRef {
        StoryRef {
            file_name: format!("s{idx}.md"),
            file_path: format!("/x/s{idx}.md"),
            depends_on: None,
            tags: None,
            status: Some("todo".into()),
            document_type: Some("story".into()),
            supporting_docs: (!supporting.is_empty()).then_some(supporting),
            story_id: Some(format!("S-{idx}")),
            source_repo: None,
            team: None,
            sprint: None,
            points: None,
            assigned_to: None,
            rank: None,
            metadata_valid: true,
            metadata_invalid_reason: None,
            open_tags,
            mtime: None,
        }
    }

    fn synth_tree(stories: Vec<StoryRef>) -> DerivedPipelineTree {
        DerivedPipelineTree {
            dir_path: String::new(),
            dir_name: String::new(),
            absolute_dir_path: "/x".into(),
            stories,
            docs: vec![],
            sub_epics: vec![],
            order: None,
            order_error: None,
            main_doc: None,
        }
    }

    /// RF-16 Site 2 (`matches_tags` / `matches_status`): per-story tag filtering.
    /// Each story carries two open tags; the filter is a 2-field AND with an
    /// any-of on one field. Reports `query_docs` wall-time — the delta across
    /// the `as_slice()`→`iter()` change is the per-story filter-value alloc win.
    #[test]
    #[ignore = "perf bench — run with --ignored --nocapture"]
    fn rf16_measure_matches_tags_query() {
        use std::time::Instant;
        for &n in &[50usize, 200, 1000] {
            let stories: Vec<StoryRef> = (0..n)
                .map(|i| {
                    let mut tags = BTreeMap::new();
                    tags.insert(
                        "area".to_string(),
                        vec!["query".to_string(), "sync".to_string()],
                    );
                    tags.insert("context_level".to_string(), vec!["team".to_string()]);
                    synth_story(i, vec![], tags)
                })
                .collect();
            let tree = synth_tree(stories);
            let mut tag = BTreeMap::new();
            tag.insert(
                "area".to_string(),
                TagFilterValue::Any(vec!["docs".to_string(), "query".to_string()]),
            );
            tag.insert(
                "context_level".to_string(),
                TagFilterValue::One("team".to_string()),
            );
            let filter = DocQuery {
                tag: Some(tag),
                ..Default::default()
            };
            const REPS: u32 = 200;
            let t = Instant::now();
            let mut sink = 0usize;
            for _ in 0..REPS {
                sink += query_docs(&tree, &filter).docs.len();
            }
            let per = t.elapsed().as_micros() as f64 / REPS as f64;
            println!(
                "RF-16 matches_tags: N={n:4} × 2 tag fields → query_docs \
                 {per:9.1} µs/call (matched={})",
                sink / REPS as usize
            );
        }
    }
}
