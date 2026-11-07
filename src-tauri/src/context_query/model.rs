// SPDX-License-Identifier: MIT
//! The query model: one serde type shared by every surface (the `query_context`
//! MCP tool, the `query_context` Tauri command behind `csquery` blocks and the
//! dashboard bridge, and the legacy `doc_query` adapter).
//!
//! JSON is camelCase. Every part is optional; `{}` returns the first page of
//! every indexed file across all synced orgs, sorted by path.
//!
//! ```json
//! {
//!   "where": { "orgs": ["alderco"], "kinds": ["story"], "pathPrefix": "planning/onboarding" },
//!   "filters": [
//!     { "key": "status", "op": "in", "value": ["todo", "in-progress"] },
//!     { "key": "assigned_to", "op": "eq", "value": "@me" }
//!   ],
//!   "relationships": { "parentEpic": "SW-OC" },
//!   "sort": ["priority", "-mtime"],
//!   "groupBy": "status",
//!   "limit": 50
//! }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Bump on a breaking change to [`ContextQueryResult`] / [`ContextRow`].
pub const CONTEXT_QUERY_SCHEMA_VERSION: u32 = 1;

/// Rows returned when the caller gives no `limit`.
pub const DEFAULT_LIMIT: usize = 200;
/// Hard cap on one page, whatever the caller asks for.
pub const MAX_LIMIT: usize = 10_000;

/// Which layer of the context footprint a file lives in.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    /// A team's shared context repo.
    Team,
    /// The user's private space for one org. Only ever returned for its own org.
    Private,
    /// Content that applies in every org (e.g. the CodeSense docs bundle).
    AllOrgs,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Team => "team",
            Layer::Private => "private",
            Layer::AllOrgs => "all-orgs",
        }
    }
}

/// What a file is, by the existing classification rules (frontmatter `type`,
/// file extension, location).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Story,
    Bug,
    Spike,
    Epic,
    Doc,
    Dashboard,
    Skill,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Story => "story",
            Kind::Bug => "bug",
            Kind::Spike => "spike",
            Kind::Epic => "epic",
            Kind::Doc => "doc",
            Kind::Dashboard => "dashboard",
            Kind::Skill => "skill",
            Kind::Other => "other",
        }
    }
}

/// Org selector: every synced org (the default) or an explicit list.
/// JSON: `"all"` or `["org-a", "org-b"]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OrgScope {
    #[default]
    All,
    List(Vec<String>),
}

impl Serialize for OrgScope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            OrgScope::All => s.serialize_str("all"),
            OrgScope::List(orgs) => orgs.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for OrgScope {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            One(String),
            Many(Vec<String>),
        }
        match Wire::deserialize(d)? {
            Wire::One(s) if s == "all" => Ok(OrgScope::All),
            Wire::One(s) => Ok(OrgScope::List(vec![s])),
            Wire::Many(v) => Ok(OrgScope::List(v)),
        }
    }
}

/// Where to look. Every dimension narrows; omitted means unconstrained.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WhereScope {
    /// Orgs to search. Default: every synced org. All-orgs content matches any org.
    #[serde(default)]
    #[schemars(schema_with = "org_scope_schema")]
    pub orgs: OrgScope,
    /// Limit to the org of the content root this absolute path (a file or a
    /// folder) sits under — a UI default, e.g. a `csquery` block searching its
    /// own org. Fails closed: a path under no org's root (all-orgs content, or
    /// outside the footprint) leaves only all-orgs content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_org_as: Option<String>,
    /// Limit to the one content root (a team's context repo, a private space, or
    /// all-orgs content) this absolute path sits under — e.g. a team board
    /// listing its own team's stories. Nothing matches when the path is under no root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_root_as: Option<String>,
    /// Team names. Private and all-orgs files have no team, so naming teams excludes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<Layer>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<Kind>>,
    /// An absolute path prefix, or a prefix of the path relative to the content root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// A glob over the path relative to the content root (e.g. `**/DEMO-*.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_glob: Option<String>,
}

/// A filter operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    /// Equals `value` (any item, for list fields).
    Eq,
    /// Equals any of `value` (an array).
    In,
    /// Equals none of `value` (a scalar or an array). Files without the key match.
    Not,
    /// The key is present and non-empty.
    Exists,
    /// The key is absent or empty.
    Missing,
    /// A list field has an item equal to `value`, or a text field contains it —
    /// both ignoring case. A JSON-number `value` matches numerically instead.
    Contains,
    /// `min <= value <= max` (either bound optional). A value only matches bounds
    /// of its own kind: numbers numerically, ISO dates chronologically, text
    /// naturally; a numeric bound never matches a date or text.
    Range,
    /// Text starts with `value`.
    Prefix,
}

/// One filter clause. Clauses on the same key OR together; different keys AND.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilterClause {
    /// Any frontmatter key, or a built-in field (`path`, `relPath`, `fileName`,
    /// `id`, `org`, `team`, `layer`, `kind`, `mtime`, `title`, `parentEpic`).
    /// Matching ignores case, `-`/`_` and camelCase vs snake_case.
    pub key: String,
    pub op: FilterOp,
    /// The operand for `eq`/`in`/`not`/`contains`/`prefix`. `"@me"` stands for the
    /// signed-in user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Value>,
}

/// Relationship predicates. Each token is a path, a `story_id`, a file name or a
/// file stem. All given predicates AND together.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipFilter {
    /// The file `depends_on` this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// This depends on the file (the file is a dependency of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depended_on_by: Option<String>,
    /// The file lists this in `supporting_docs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_supporting_doc: Option<String>,
    /// This lists the file in its `supporting_docs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlinked_by: Option<String>,
    /// The file's parent epic (`parent_epic`/`epic` frontmatter, or a folder on its path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_epic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_deps: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_backlinked: Option<bool>,
}

/// Sort direction.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

/// Where files without a value for the sort key go. Default: last.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum MissingOrder {
    First,
    #[default]
    Last,
}

/// One sort key: `"status"`, `"-mtime"` (descending), or the object form.
/// `order` ranks values explicitly (e.g. a team's own status order); values
/// not listed sort after the listed ones. It also orders `groupBy` buckets
/// when the group key is sorted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(untagged)]
pub enum SortSpec {
    Short(String),
    Full(FullSort),
}

/// The object form of a sort key. Unknown fields reject, so a typo such as
/// `direction` fails loudly instead of sorting ascending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FullSort {
    pub key: String,
    #[serde(default)]
    pub dir: SortDir,
    #[serde(default)]
    pub missing: MissingOrder,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// A sort key, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub key: String,
    pub dir: SortDir,
    pub missing: MissingOrder,
    pub order: Option<Vec<String>>,
}

impl SortSpec {
    pub fn parts(&self) -> SortKey {
        match self {
            SortSpec::Short(s) => {
                let (key, dir) = match s.strip_prefix('-') {
                    Some(rest) => (rest.to_string(), SortDir::Desc),
                    None => (s.clone(), SortDir::Asc),
                };
                SortKey {
                    key,
                    dir,
                    missing: MissingOrder::Last,
                    order: None,
                }
            }
            SortSpec::Full(f) => SortKey {
                key: f.key.clone(),
                dir: f.dir,
                missing: f.missing,
                order: f.order.clone(),
            },
        }
    }
}

/// The query. See the module docs for an example.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextQuery {
    #[serde(default, rename = "where")]
    pub scope: WhereScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterClause>,
    /// Shorthand: `{ "status": ["todo", "review"], "team": "default" }` — a
    /// scalar is `eq`, an array is `in`.
    #[serde(default, rename = "match", skip_serializing_if = "BTreeMap::is_empty")]
    pub match_fields: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationships: Option<RelationshipFilter>,
    /// Token-aware title, heading and body match (bounded scan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Frontmatter keys to return (plus the built-in row fields). Include
    /// `"relationships"` to also get relationship refs. Omitted: everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Vec<String>>,
    /// Sort keys, applied in order; ties fall back to path. Default: path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<SortSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// The `nextCursor` of a previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Count matches per value of this key (every item of a list counts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
}

/// A reference from one file to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RelRef {
    /// `story_id` of the target, else its file name (or, when unresolved, the ref stem).
    pub id: String,
    /// Absolute path of the target (or best-effort resolved path).
    pub path: String,
    pub file_name: String,
    /// True iff the target is an indexed file.
    pub resolved: bool,
}

/// A row's relationships.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowRelationships {
    pub depends_on: Vec<RelRef>,
    /// Files that depend on this one.
    pub dependents: Vec<RelRef>,
    pub supporting_docs: Vec<RelRef>,
    /// Files that list this one in their `supporting_docs`.
    pub backlinks: Vec<RelRef>,
    pub parent_epic: Option<String>,
    /// The folder epic the file sits in: the nearest folder holding a main doc
    /// (`README`/`index`), named as the left nav names it. Independent of
    /// `parent_epic` frontmatter.
    pub folder_epic: Option<String>,
}

/// A matching heading from a schema-1 search sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextMatch {
    pub title: String,
    pub anchor: String,
}

/// One matching file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRow {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_matches: Vec<TextMatch>,
    /// `story_id`, else the file name.
    pub id: String,
    pub path: String,
    /// Path relative to the file's content root.
    pub rel_path: String,
    pub file_name: String,
    /// `null` for all-orgs content.
    pub org: Option<String>,
    /// `null` for private and all-orgs content.
    pub team: Option<String>,
    pub layer: Layer,
    pub kind: Kind,
    /// Modified time, epoch milliseconds.
    pub mtime: Option<u64>,
    pub title: Option<String>,
    /// Frontmatter keys (all, or those in `select`), with their parsed values.
    pub fields: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationships: Option<RowRelationships>,
    /// True when the file's metadata block ran past what the index reads, so
    /// some keys may be missing from `fields`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub metadata_truncated: bool,
    /// SW-DNS-2: present only when `select` names `"summary"`: the first
    /// configured summary section of a story/bug/spike/epic (`null` when none
    /// matches, or for other files). Parsed at index time, never per query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Option<String>>,
}

/// One group-by bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCount {
    /// The value, or `null` for files without one.
    pub value: Option<String>,
    pub count: usize,
}

/// The result envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextQueryResult {
    /// Default candidate exclusions applied before the bounded text scan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    pub schema_version: u32,
    /// Matches before paging.
    pub total: usize,
    pub rows: Vec<ContextRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<GroupCount>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// True when the body scan for `text` hit its bound, so matches may be missing.
    #[serde(default)]
    pub text_scan_truncated: bool,
    /// True when `total` may undercount (the `text` scan stopped early).
    #[serde(default)]
    pub total_approximate: bool,
    /// The orgs the query was narrowed to (`None`: every org). Live subscribers
    /// re-run only when a change touches one of these (or all-orgs content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_orgs: Option<Vec<String>>,
    /// True while the first index build is still running (results may be partial).
    #[serde(default)]
    pub indexing: bool,
    /// The index snapshot this answer came from (bumps on every index change).
    #[serde(default)]
    pub generation: u64,
}

fn org_scope_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({"anyOf":[{"type":"string"},{"type":"array","items":{"type":"string"}}]})
}
