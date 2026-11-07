// SPDX-License-Identifier: MIT
//! The `query_context` MCP tool: agents find context files by
//! metadata through the one engine (`crate::context_query`). The `#[tool]`
//! wrapper lives in `handler.rs`; this module owns the params and the body.

use rmcp::model::ErrorData;
use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::context_query::{
    self, ContextIndexService, ContextQuery, ContextQueryResult, QueryError, Viewer,
};

#[cfg(test)]
mod caller_scope_tests;

/// The tool's description (kept here, next to the params, so examples and
/// schema change together).
pub(crate) const DESCRIPTION: &str = r#"Find context files by metadata across synced orgs (team context repos, your private space per org, and all-orgs content). Footprint-wide by default; narrow with `where`. Child callers see only shared ancestor files, even with where.teams. Every arg is optional; `{}` lists the first page of visible files sorted by path.

Rows: { id (story_id or file name), path, relPath, fileName, org (null = all-orgs), team (null = private/all-orgs), layer (team|private|all-orgs), kind (story|bug|spike|epic|doc|dashboard|skill|other), mtime (ms), title, fields (ALL frontmatter / HTML cs:* meta), relationships?, metadataTruncated?, textMatches?: [{title, anchor}] }.

Private items (the user's private space) come back only for ONE org you name: `where.orgs: ["acme"]` (exactly one) or `where.sameOrgAs`. With `orgs: "all"` (the default) or several orgs you get team and all-orgs content only, never private items — and links to private items stay hidden.

Top-level `reviews/` files are excluded by default. To read them deliberately, set `where.pathPrefix` or `where.pathGlob` starting with `reviews/` (for example `reviews/sam/` or `reviews/**/*.md`). Pane queries are unchanged.

- where: { orgs: "all" | ["org"], sameOrgAs: "/abs/path", teams: [..], layers: [..], kinds: [..], pathPrefix: "_bmad-output/13-x" or "/abs", pathGlob: "**/DEMO-*.md" }
- filters: [{ key, op, value | min/max }] — op: eq, in, not, exists, missing, contains (list item or case-insensitive text), range (numeric, or text e.g. ISO dates), prefix. Any frontmatter key or built-in (path, relPath, fileName, id, org, team, layer, kind, mtime, title, dir). Same key → OR; different keys → AND. Value "@me" = the signed-in user.
- match: { status: ["todo","review"], team: "default" } — shorthand (scalar = eq, array = in).
- relationships: { dependsOn, dependedOnBy, hasSupportingDoc, backlinkedBy, parentEpic, hasDeps, isBacklinked } — tokens are a path, story_id, file name or stem.
- text: token-aware full-text across title, headings and body (case-insensitive; reads at most 5,000 files / 64 MB per query after the other filters; textScanTruncated / totalApproximate say when that bound hit).
Text results rank by title coverage, then heading coverage, then body coverage unless sort is explicit. Schema-1 search-index.json sidecars supply optional textMatches anchors on the page row. Only nonempty text queries scoped exactly to where.orgs: ["codesense/docs"] default-exclude generated/ before scanning, reported as excluded: ["generated/"]. Opt in with where.pathPrefix: "generated/" or a pathGlob starting with "generated/". Listings and other orgs are unchanged.

- select: frontmatter keys to return (+ "relationships"); omit for everything. All query field keys accept an optional cs: prefix; topic/task/tools/audience are lists, returned with canonical unprefixed keys.
- sort: ["priority", "-mtime", {key, dir, missing, order}] — status sorts by the built-in workflow order (planning, todo, in-progress, blocked, review, qa, done) unless you pass `order` (e.g. the team's own status list); priority by p0…p4. Values order numbers < dates < text (text naturally: DEMO-9 < DEMO-10). Ties by path.
- eq/in/not compare text exactly ("01" ≠ "1"); pass a JSON number to compare numerically.
- limit (default 200, max 10000), cursor (prior nextCursor, same query; one snapshot per walk, 30 s idle/120 s max, text until a change; then data.code stale_cursor: restart), groupBy: a key → groups [{value, count}] over ALL matches (board columns).

Example — open stories in epic DEMO assigned to me, by priority, as a board:
{"where":{"kinds":["story"]},"relationships":{"parentEpic":"DEMO"},"filters":[{"key":"status","op":"in","value":["todo","in-progress"]},{"key":"assigned_to","op":"eq","value":"@me"}],"sort":["priority"],"groupBy":"status","select":["status","priority"]}

Example — dashboards in one org changed since a date: {"where":{"orgs":["acme"],"kinds":["dashboard"]},"filters":[{"key":"mtime","op":"range","min":1759300000000}],"sort":["-mtime"]}"#;

/// `query_context` arguments — the context query model, one field per part.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryContextParams {
    /// Where to look: `{ orgs: "all" | [slugs], sameOrgAs, teams, layers, kinds, pathPrefix, pathGlob }`.
    #[serde(default, rename = "where")]
    pub scope: Option<Value>,
    /// Filter clauses `[{ key, op, value | min | max }]`; ops: eq, in, not, exists, missing, contains, range, prefix.
    #[serde(default)]
    pub filters: Option<Value>,
    /// Shorthand filters `{ key: value | [values] }` (scalar = eq, array = in).
    #[serde(default, rename = "match")]
    pub match_fields: Option<Value>,
    /// Relationship predicates `{ dependsOn, dependedOnBy, hasSupportingDoc, backlinkedBy, parentEpic, hasDeps, isBacklinked }`.
    #[serde(default)]
    pub relationships: Option<Value>,
    /// Token-aware title, heading and body match (bounded scan).
    #[serde(default)]
    pub text: Option<String>,
    /// Frontmatter keys to return (add "relationships" for refs). Omit for every key.
    #[serde(default)]
    pub select: Option<Vec<String>>,
    /// Sort keys: `"status"`, `"-mtime"`, or `{ key, dir: "asc"|"desc", missing: "first"|"last" }`.
    #[serde(default)]
    pub sort: Option<Value>,
    /// Rows per page (default 200, max 10000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// The `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Count matches per value of this key (board columns).
    #[serde(default)]
    pub group_by: Option<String>,
}

impl QueryContextParams {
    /// The engine query, or a message naming what is malformed.
    pub(crate) fn to_query(&self) -> Result<ContextQuery, String> {
        let mut obj = Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v.filter(|v| !v.is_null()) {
                obj.insert(k.to_string(), v);
            }
        };
        put("where", self.scope.clone());
        put("filters", self.filters.clone());
        put("match", self.match_fields.clone());
        put("relationships", self.relationships.clone());
        put("text", self.text.clone().map(Value::String));
        put("select", self.select.clone().map(Value::from));
        put("sort", self.sort.clone());
        put("limit", self.limit.map(Value::from));
        put("cursor", self.cursor.clone().map(Value::String));
        put("groupBy", self.group_by.clone().map(Value::String));
        serde_json::from_value(Value::Object(obj)).map_err(|e| format!("invalid query: {e}"))
    }
}

/// Run the query against the index for an agent (no page: the whole footprint,
/// each private item under its own org). Blocking (bounded body reads for `text`).
pub(crate) fn run<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    service: &ContextIndexService,
    params: &QueryContextParams,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<ContextQueryResult, ErrorData> {
    let query = params
        .to_query()
        .map_err(|m| ErrorData::invalid_params(m, None))?;
    let mut viewer = context_query::viewer_for(app, service, None, context_query::uses_me(&query));
    viewer.scope = scope;
    run_as(service, &query, &viewer)
}

/// The query against the index as `viewer`.
pub(crate) fn run_as(
    service: &ContextIndexService,
    query: &ContextQuery,
    viewer: &Viewer,
) -> Result<ContextQueryResult, ErrorData> {
    service
        .query_with(query, viewer, |snapshot| run_on(snapshot, query, viewer))
        .map_err(query_error)
}

/// A refused query as MCP answers it: `invalid_params`, and a stale cursor says
/// so in `data.code` so an agent restarts paging.
fn query_error(e: QueryError) -> ErrorData {
    let data = matches!(e, QueryError::StaleCursor)
        .then(|| serde_json::json!({ "code": context_query::STALE_CURSOR }));
    ErrorData::invalid_params(e.to_string(), data)
}

fn run_on(
    snapshot: &context_query::index::Snapshot,
    query: &ContextQuery,
    viewer: &Viewer,
) -> Result<ContextQueryResult, QueryError> {
    let reviews_requested = [
        query.scope.path_prefix.as_deref(),
        query.scope.path_glob.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|path| path.starts_with("reviews/"));
    // Filter the MCP corpus before the engine counts, scans bodies or pages it.
    // The shared index stays complete for panes and deliberate file opens.
    let filtered = (!reviews_requested
        && snapshot
            .records
            .iter()
            .any(|r| r.rel_path.starts_with("reviews/")))
    .then(|| {
        context_query::index::Snapshot::build(
            snapshot
                .records
                .iter()
                .filter(|r| !r.rel_path.starts_with("reviews/"))
                .cloned()
                .collect(),
            snapshot.roots.clone(),
            snapshot.generation,
            snapshot.ready,
        )
    });
    filtered.as_ref().unwrap_or(snapshot).query(
        query,
        viewer,
        &context_query::engine::DiskBodyReader,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_query::index::IndexRoot;
    use crate::context_query::model::Layer;
    use std::sync::Arc;
    use std::time::Duration;

    fn write(p: &std::path::Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn docs_json_search_and_topic_filters_use_the_running_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("docs/content");
        let html = include_str!("../../../codesense-docs/getting-started/content/arc-g-routines/command-routines.html").replace("</head>", "<meta name=\"cs:topic\" content=\"routines\"><meta name=\"cs:tools\" content=\"routine_create, routine_runs\"></head>");
        write(&root.join("arc-g-routines/command-routines.html"), &html);
        write(
            &root.join("generated/search-index.json"),
            r##"{"schema":1,"entries":[{"title":"seamonlyterm","href":"./arc-g-routines/command-routines.html#chain","text":""}]}"##,
        );
        let svc = ContextIndexService::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        svc.start(
            Arc::new(move || {
                Some(crate::context_query::service::Footprint {
                    owner: 1,
                    roots: vec![IndexRoot {
                        dir: root.clone(),
                        org: Some("codesense/docs".into()),
                        team: Some("getting-started".into()),
                        layer: Layer::Team,
                    }],
                    summary_headings: Vec::new(),
                })
            }),
            Arc::new(move |_| {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let params: QueryContextParams = serde_json::from_value(serde_json::json!({"where":{"orgs":["codesense/docs"]},"text":"seamonlyterm","select":["cs:type","cs:topic","cs:tools"]})).unwrap();
        let out = run_as(&svc, &params.to_query().unwrap(), &Viewer::default()).unwrap();
        assert_eq!(out.total, 1);
        assert_eq!(out.rows[0].file_name, "command-routines.html");
        assert_eq!(out.rows[0].text_matches[0].anchor, "#chain");
        assert_eq!(
            out.rows[0].fields["tools"],
            serde_json::json!(["routine_create", "routine_runs"])
        );
        assert_eq!(out.excluded, ["generated/"]);
        let params: QueryContextParams = serde_json::from_value(serde_json::json!({"where":{"orgs":["codesense/docs"]},"filters":[{"key":"cs:topic","op":"eq","value":"routines"}],"select":["cs:topic"]})).unwrap();
        let out = run_as(&svc, &params.to_query().unwrap(), &Viewer::default()).unwrap();
        assert_eq!(out.rows[0].fields["topic"], serde_json::json!(["routines"]));
        assert!(out.excluded.is_empty());
    }

    /// The tool seam end to end: JSON args → params → running index → rows.
    #[test]
    fn query_context_tool_answers_from_the_running_index() {
        let tmp = tempfile::tempdir().unwrap();
        let team = tmp.path().join("acme/default/content");
        let private = tmp.path().join("scratch/orgs/globex/content");
        write(
            &team.join("e/S-1.md"),
            "---\ntype: story\nstory_id: S-1\nstatus: todo\nparent_epic: E\nassigned_to: me-user\npriority: p1\n---\n",
        );
        write(
            &team.join("e/S-2.md"),
            "---\ntype: story\nstory_id: S-2\nstatus: done\nparent_epic: E\nassigned_to: me-user\n---\n",
        );
        write(
            &private.join("p.md"),
            "---\ntype: story\nstory_id: G-P\nstatus: todo\n---\n",
        );
        write(
            &team.join("reviews/sam/2026-10-20.md"),
            "A daily review.",
        );
        write(&team.join("docs/reviews/guide.md"), "Review guidance.");
        let (t, p) = (team.clone(), private.clone());
        let svc = ContextIndexService::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        svc.start(
            Arc::new(move || {
                Some(crate::context_query::service::Footprint {
                    owner: 1,
                    roots: vec![
                        IndexRoot {
                            dir: t.clone(),
                            org: Some("acme".into()),
                            team: Some("default".into()),
                            layer: Layer::Team,
                        },
                        IndexRoot {
                            dir: p.clone(),
                            org: Some("globex".into()),
                            team: None,
                            layer: Layer::Private,
                        },
                    ],
                    summary_headings: Vec::new(),
                })
            }),
            Arc::new(move |_| {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        rx.recv_timeout(Duration::from_secs(10)).unwrap();

        let args = serde_json::json!({
            "where": { "orgs": ["acme"], "kinds": ["story"] },
            "relationships": { "parentEpic": "E" },
            "filters": [
                { "key": "status", "op": "in", "value": ["todo", "in-progress"] },
                { "key": "assigned_to", "op": "eq", "value": "@me" }
            ],
            "sort": ["priority"],
            "groupBy": "status"
        });
        let params: QueryContextParams = serde_json::from_value(args).unwrap();
        let viewer = Viewer {
            identities: vec!["me-user".into()],
            ..Default::default()
        };
        let result = run_as(&svc, &params.to_query().unwrap(), &viewer).unwrap();
        let ids: Vec<&str> = result.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["S-1"]);
        assert_eq!(result.groups.unwrap()[0].value.as_deref(), Some("todo"));
        assert!(!result.indexing);

        // The other org's private item never shows in an acme-scoped query.
        let all_acme: QueryContextParams =
            serde_json::from_value(serde_json::json!({ "where": { "orgs": ["acme"] } })).unwrap();
        let rows = run_as(&svc, &all_acme.to_query().unwrap(), &viewer)
            .unwrap()
            .rows;
        assert!(rows.iter().all(|r| r.id != "G-P"));

        // Exclusion happens before pagination/counting and only in the MCP surface.
        let mut query = all_acme.to_query().unwrap();
        query.limit = Some(1);
        query.group_by = Some("kind".into());
        let first = run_as(&svc, &query, &viewer).unwrap();
        assert_eq!(first.total, 3);
        assert_eq!(
            first
                .groups
                .as_ref()
                .unwrap()
                .iter()
                .map(|g| g.count)
                .sum::<usize>(),
            3
        );
        let first_cursor = first.next_cursor.clone().unwrap();
        let mut rows = first.rows;
        let mut cursor = first.next_cursor;
        while let Some(next) = cursor {
            query.cursor = Some(next);
            let page = run_as(&svc, &query, &viewer).unwrap();
            rows.extend(page.rows);
            cursor = page.next_cursor;
        }
        assert_eq!(rows.len(), 3);
        // A cursor from an earlier app run is invalid_params with
        // data.code stale_cursor (restart paging); a malformed one has no code.
        query.cursor = Some(context_query::engine::from_an_earlier_run(&first_cursor));
        let stale = run_as(&svc, &query, &viewer).unwrap_err();
        assert_eq!(stale.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            stale.data,
            Some(serde_json::json!({ "code": context_query::STALE_CURSOR }))
        );
        assert!(
            stale.message.starts_with("stale_cursor"),
            "{}",
            stale.message
        );
        query.cursor = Some("o:1:x".into());
        let malformed = run_as(&svc, &query, &viewer).unwrap_err();
        assert_eq!(malformed.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(malformed.data, None);
        assert!(rows.iter().all(|r| !r.rel_path.starts_with("reviews/")));
        assert!(rows.iter().any(|r| r.rel_path == "docs/reviews/guide.md"));
        let bridge =
            context_query::run_query(&svc, &all_acme.to_query().unwrap(), &viewer).unwrap();
        assert_eq!(
            bridge.total, 4,
            "the bridge's shared engine still sees reviews"
        );
        for scope in [
            serde_json::json!({"orgs":["acme"],"pathPrefix":"reviews/"}),
            serde_json::json!({"orgs":["acme"],"pathGlob":"reviews/**/*.md"}),
        ] {
            let params = QueryContextParams {
                scope: Some(scope),
                ..Default::default()
            };
            let result = run_as(&svc, &params.to_query().unwrap(), &viewer).unwrap();
            assert_eq!(result.total, 1);
            assert_eq!(result.rows[0].rel_path, "reviews/sam/2026-10-20.md");
        }

        // A malformed operator is an invalid-params refusal, not a panic.
        let bad: QueryContextParams = serde_json::from_value(
            serde_json::json!({ "filters": [{ "key": "status", "op": "like" }] }),
        )
        .unwrap();
        let err = bad.to_query().unwrap_err();
        assert!(err.contains("invalid query"), "{err}");
    }
}

/// The complete MCP read boundary, shared with the runtime-independent request tests.
pub(crate) async fn request<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: Option<String>,
    remote: Option<crate::commands::dashboard_launch::LaunchTeam>,
    params: QueryContextParams,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    use tauri::Manager;
    let scope =
        crate::context_repo::caller_scope::for_session(&app, session.as_deref(), remote).await;
    let service = app
        .try_state::<context_query::ContextIndexService>()
        .ok_or_else(|| rmcp::ErrorData::internal_error("the context index is not running", None))?
        .inner()
        .clone();
    let result = crate::commands::await_blocking("mcp.query_context", move || {
        Ok(run(&app, &service, &params, scope))
    })
    .await
    .map_err(|m| rmcp::ErrorData::invalid_params(m, None))??;
    super::handler::ok_json(
        serde_json::to_value(result)
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?,
    )
}
