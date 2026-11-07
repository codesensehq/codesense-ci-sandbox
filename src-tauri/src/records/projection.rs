// SPDX-License-Identifier: MIT
use super::{Actor, Record, Scope, Store, Written};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock, Mutex, OnceLock},
};
use tauri::Manager;

type WriteObserver = dyn Fn(&Store, &Written, &Actor, &str, Option<&str>) + Send + Sync;
type CommitObserver = dyn Fn(&super::history::HistoryLocation, &[String]) + Send + Sync;
static WRITES: OnceLock<Arc<WriteObserver>> = OnceLock::new();
static COMMITS: OnceLock<Arc<CommitObserver>> = OnceLock::new();
/// Each record's newest commit, by id, for the folder it was read in: read from
/// history once, then again only for the records a write or commit changed.
#[derive(Clone, Default)]
struct Heads {
    folder: Option<std::path::PathBuf>,
    commits: BTreeMap<String, Option<String>>,
}
#[derive(Default)]
pub struct Views(Mutex<BTreeMap<String, (super::binding::Binding, String, Heads)>>);
static SERIAL: Mutex<()> = Mutex::new(());
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}
pub fn notify(store: &Store, result: &Written, actor: &Actor, kind: &str, request: Option<&str>) {
    static AUDIT: OnceLock<Mutex<AuditWindow>> = OnceLock::new();
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let now = START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64;
    if let Ok(folder) = store.folder() {
        if AUDIT
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .admit(folder, &result.record.id, now)
        {
            log::info!(target:"dashboard-action", "{}", json!({"verb":format!("records.{kind}"),"collection":store.collection.name,"id":result.record.id,"actor":actor,"decision":"allow"}));
        }
    }
    if let Some(observer) = WRITES.get() {
        observer(store, result, actor, kind, request);
    }
}
pub fn committed(location: &super::history::HistoryLocation, ids: &[String]) {
    if let Some(observer) = COMMITS.get() {
        observer(location, ids);
    }
}
#[derive(Default)]
pub(crate) struct AuditWindow(BTreeMap<(std::path::PathBuf, String), u64>);
impl AuditWindow {
    pub(crate) fn admit(&mut self, folder: std::path::PathBuf, id: &str, now: u64) -> bool {
        self.0.retain(|_, at| now.saturating_sub(*at) < 60_000);
        match self.0.entry((folder, id.into())) {
            std::collections::btree_map::Entry::Occupied(_) => false,
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        }
    }
}
static AGENT_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[@([^\]]*)\]\(cs:agent:([^)\s]+)\)").unwrap());
static HEADING_OR_QUOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:#{1,6}\s+|>\s*)").unwrap());
static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?").unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
static EMPHASIS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[*_`~]").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
const PREVIEW_LINES: usize = 4;
const PREVIEW_CHARS: usize = 240;

/// A record body as a list shows it under the title: the words of its first few
/// lines joined with " · " (markdown syntax, links and agent handoffs set aside),
/// and the agents its lines were handed to as `{handle, name}`.
pub(crate) fn summary(body: &str) -> (String, Vec<Value>) {
    let stripped = AGENT_REF.replace_all(body, "");
    let lines: Vec<String> = stripped
        .split('\n')
        .map(normalize_line)
        .filter(|l| {
            !l.is_empty() && !l.starts_with("```") && !l.starts_with("~~~") && !l.starts_with("---")
        })
        .take(PREVIEW_LINES)
        .collect();
    let preview = lines.join(" · ").chars().take(PREVIEW_CHARS).collect();
    let mut seen = std::collections::BTreeSet::new();
    let refs = AGENT_REF
        .captures_iter(body)
        .filter_map(|m| {
            let handle = percent_decode(&m[2]).filter(|h| !h.is_empty())?;
            if !seen.insert(handle.clone()) {
                return None;
            }
            let label = m[1].trim();
            let name = label.strip_prefix('@').unwrap_or(label).trim();
            let name = if name.is_empty() { &handle } else { name };
            Some(json!({"handle": handle, "name": name}))
        })
        .collect();
    (preview, refs)
}
/// A line's words: heading, quote and list markers, link targets, emphasis and
/// extra spacing set aside.
fn normalize_line(line: &str) -> String {
    let line = line.replace('\u{a0}', " ");
    let line = HEADING_OR_QUOTE.replace(&line, "");
    let line = LIST_MARKER.replace(&line, "");
    let line = LINK.replace_all(&line, "$1");
    let line = EMPHASIS.replace_all(&line, "");
    SPACES.replace_all(&line, " ").trim().to_owned()
}
/// `decodeURIComponent`: `None` for a malformed escape or invalid UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
pub fn row(store: &Store, record: &Record) -> Value {
    projected(store, record, head_commit(store, &record.id).as_deref())
}
fn projected(store: &Store, record: &Record, head: Option<&str>) -> Value {
    let fields: Map<String, Value> = record
        .fields
        .iter()
        .filter(|(k, _)| store.collection.fields.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (preview, refs) = summary(&record.body);
    let mut row = json!({"id":record.id,"fields":fields,"preview":preview,"refs":refs,"updatedAt":record.fields.get("updatedAt"),"updatedBy":record.fields.get("updatedBy")});
    if let Some(title) = store
        .collection
        .title
        .as_ref()
        .and_then(|k| record.fields.get(k))
    {
        row["title"] = title.clone();
    }
    if let Some(head) = head {
        row["headCommit"] = json!(head);
    }
    row
}
/// `id`'s newest commit, for a private collection's record.
fn head_commit(store: &Store, id: &str) -> Option<String> {
    if store.collection.scope != Scope::Private {
        return None;
    }
    #[cfg(test)]
    HISTORY_READS.with(|reads| reads.set(reads.get() + 1));
    let loc = store.location().ok()?;
    let revisions = super::history::note_history(&loc, id, 1).ok()?;
    revisions.into_iter().next().map(|rev| rev.commit)
}
#[cfg(test)]
thread_local! {
    static HISTORY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// The view's rows. History is read for the `changed` records and those `heads`
/// lacks; the rest reuse their cached commit. Removed records leave `heads`.
fn rows(store: &Store, heads: &mut Heads, changed: &[String]) -> Result<Vec<Value>, String> {
    let records = store.list_records()?;
    // A collection repointed at another folder has other histories: read them all.
    let folder = store.folder()?;
    if heads.folder.as_ref() != Some(&folder) {
        *heads = Heads {
            folder: Some(folder),
            commits: BTreeMap::new(),
        };
    }
    let commits = &mut heads.commits;
    commits.retain(|id, _| records.iter().any(|r| &r.id == id));
    Ok(records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            if changed.contains(&record.id) || !commits.contains_key(&record.id) {
                commits.insert(record.id.clone(), head_commit(store, &record.id));
            }
            let mut row = projected(store, record, commits[&record.id].as_deref());
            row["order"] = json!(index);
            row
        })
        .collect())
}
pub fn prepare<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    binding: &super::binding::Binding,
    name: &str,
) -> Result<String, String> {
    let store = binding.resolve(name)?;
    store.list()?;
    let _serial = serial();
    let key = binding.view_key(&store);
    let mut heads = Heads::default();
    let rows = rows(&store, &mut heads, &[])?;
    app.state::<Views>()
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(key.clone(), (binding.clone(), name.into(), heads));
    crate::live_hub::replace_records(app, &key, rows, None, None, None)?;
    Ok(key)
}
fn refresh<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    folder: Option<&std::path::Path>,
    actor: Option<&Actor>,
    kind: Option<&str>,
    request: Option<&str>,
    changed: &[String],
) {
    let views = app.state::<Views>().0.lock().unwrap().clone();
    for (key, (binding, name, mut heads)) in views {
        let store = match binding.resolve_declared(&name) {
            Ok(store) => store,
            Err(error) => {
                log::warn!("records projection: {error}");
                if let Err(error) =
                    crate::live_hub::replace_records(app, &key, vec![], actor, kind, request)
                {
                    log::warn!("records projection invalidation: {error}");
                }
                continue;
            }
        };
        if binding.view_key(&store) != key {
            app.state::<Views>().0.lock().unwrap().remove(&key);
            if let Err(error) =
                crate::live_hub::replace_records(app, &key, vec![], None, Some("outside"), None)
            {
                log::warn!("records projection invalidation: {error}");
            }
            continue;
        }
        if folder.is_some_and(|path| store.folder().ok().as_deref() != Some(path)) {
            continue;
        }
        let rows = rows(&store, &mut heads, changed);
        if let Some(view) = app.state::<Views>().0.lock().unwrap().get_mut(&key) {
            view.2 = heads;
        }
        if let Err(error) = rows.and_then(|rows| {
            crate::live_hub::replace_records(app, &key, rows, actor, kind, request)
        }) {
            log::warn!("records projection: {error}");
        }
    }
}
pub fn start<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Listener;
    app.manage(Views::default());
    let handle = app.clone();
    let _ = WRITES.set(Arc::new(move |store, result, actor, kind, request| {
        if let Ok(folder) = store.folder() {
            let written = [result.record.id.clone()];
            refresh(
                &handle,
                Some(&folder),
                Some(actor),
                Some(kind),
                request,
                &written,
            );
        }
        if store.records_transport
            && store.collection.scope == Scope::Private
            && store.collection.name == "notes"
        {
            if let Ok(loc) = store.location() {
                let org = loc
                    .root
                    .strip_prefix(&loc.repo)
                    .ok()
                    .and_then(|p| p.strip_prefix("orgs").ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"));
                let change = match kind {
                    "create" => super::history::ChangeKind::Create,
                    "delete" => super::history::ChangeKind::Delete,
                    "restore" => super::history::ChangeKind::Restore,
                    "meta" => super::history::ChangeKind::Meta,
                    _ => super::history::ChangeKind::Write,
                };
                super::history::emit_records_notes_changed(
                    &handle,
                    org.as_deref(),
                    &result.record.id,
                    result.commit.as_deref(),
                    actor,
                    change,
                );
            }
        }
        if let Some(index) = handle.try_state::<crate::context_query::ContextIndexService>() {
            if let Ok(folder) = store.folder() {
                index.note_paths([folder]);
            }
        }
    }));
    let handle = app.clone();
    let _ = COMMITS.set(Arc::new(move |loc, ids| {
        let app = handle.clone();
        let folder = loc.notes_dir();
        let ids = ids.to_vec();
        tauri::async_runtime::spawn_blocking(move || {
            let _serial = serial();
            refresh(&app, Some(&folder), None, Some("commit"), None, &ids)
        });
    }));
    let handle = app.clone();
    app.listen(crate::context_query::service::CHANGE_EVENT, move |_| {
        let app = handle.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _serial = serial();
            refresh(&app, None, None, Some("outside"), None, &[])
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    use tauri::Listener;

    #[test]
    fn removed_declaration_clears_subscribed_rows_and_restoring_it_reloads_them() {
        let home = tempfile::tempdir().unwrap();
        let org =
            crate::context_repo::path_to_org_root(home.path(), &42u64.into(), "acme/ctx").unwrap();
        let schema = org.join("family/content/schema/care.yaml");
        std::fs::create_dir_all(schema.parent().unwrap()).unwrap();
        let declaration = "collection: care\nfolder: hq/data/care\nid: slug\n";
        std::fs::write(&schema, declaration).unwrap();
        let binding = super::super::binding::Binding::from_context(
            home.path(),
            &42u64.into(),
            "acme/ctx",
            Some("family"),
        )
        .unwrap();
        binding
            .resolve("care")
            .unwrap()
            .create(Some("filter"), &[], "Instructions", &Actor::human(), None)
            .unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(crate::live_hub::LiveHub::default());
        app.manage(Views::default());
        let key = prepare(app.handle(), &binding, "care").unwrap();
        let first =
            crate::live_hub::subscribe(app.handle(), "records", &key, None, Some("pane"), None)
                .unwrap();
        assert_eq!(first.snapshot.unwrap().len(), 1);
        let (send, receive) = mpsc::channel();
        app.listen("codesense:subscription-delta", move |event| {
            let _ = send.send(serde_json::from_str::<Value>(event.payload()).unwrap());
        });
        std::fs::remove_file(&schema).unwrap();
        let _serial = serial();
        refresh(app.handle(), None, None, Some("outside"), None, &[]);
        let removed = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(removed["delta"]["removes"], json!(["filter"]));
        assert_eq!(removed["delta"]["total"], 0);
        assert!(
            crate::live_hub::subscribe(app.handle(), "records", &key, None, None, None)
                .unwrap()
                .snapshot
                .unwrap()
                .is_empty()
        );
        refresh(app.handle(), None, None, Some("outside"), None, &[]);
        assert!(receive.try_recv().is_err());
        std::fs::write(&schema, declaration).unwrap();
        refresh(app.handle(), None, None, Some("outside"), None, &[]);
        let restored = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(restored["delta"]["upserts"][0]["id"], "filter");
    }

    #[test]
    fn moving_a_collection_to_private_unregisters_the_team_view_and_requires_a_private_subscription(
    ) {
        use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore, FrameTeam};
        use crate::verbs::{capabilities, check_frame_capability};
        for missing_phase in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let org = crate::context_repo::path_to_org_root(home.path(), &42u64.into(), "acme/ctx")
                .unwrap();
            let schema = org.join("family/content/schema/care.yaml");
            std::fs::create_dir_all(schema.parent().unwrap()).unwrap();
            let declaration = "collection: care\nfolder: hq/data/care\nid: slug\ntitle: title\n";
            std::fs::write(&schema, declaration).unwrap();
            let binding = super::super::binding::Binding::from_context(
                home.path(),
                &42u64.into(),
                "acme/ctx",
                Some("family"),
            )
            .unwrap();
            let team_store = binding.resolve("care").unwrap();
            team_store
                .create(
                    Some("filter"),
                    &[("title".into(), json!("Team title"))],
                    "Team instructions",
                    &Actor::human(),
                    None,
                )
                .unwrap();
            let frames = FrameStore::default();
            let make_frame = |private: bool| {
                let scope = if private { "private" } else { "team" };
                let mut frame = frames.mint(
                    9321,
                    FrameGrant {
                        frame_id: String::new(),
                        principal: "human".into(),
                        dashboard: "care".into(),
                        slot: if private { "right-pane" } else { "centre" }.into(),
                        team: (!private).then(|| FrameTeam {
                            org: "acme/ctx".into(),
                            team: "family".into(),
                        }),
                        scope: scope.into(),
                    },
                    "/ctx".into(),
                    "/ctx/board.html".into(),
                    "/ctx".into(),
                    FramePolicy {
                        team_safe: true,
                        ..Default::default()
                    },
                    None,
                );
                frame.records_binding = Some(binding.clone());
                frame
                    .effective_grant
                    .retain(|cap| capabilities::scope_allows(scope, cap));
                frame
            };
            let frame = make_frame(false);
            let args = json!({"topic":"records","collection":"care"});
            assert!(check_frame_capability(&frame, "subscribe", &args).is_ok());
            let app = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            app.manage(crate::live_hub::LiveHub::default());
            app.manage(Views::default());
            let team_key = prepare(app.handle(), &binding, "care").unwrap();
            assert_eq!(team_key, "records:team:acme/ctx/family:care");
            let initial = crate::live_hub::subscribe(
                app.handle(),
                "records",
                &team_key,
                None,
                Some("team-pane"),
                None,
            )
            .unwrap();
            assert_eq!(initial.snapshot.unwrap()[0]["title"], "Team title");
            let (send, receive) = mpsc::channel();
            app.listen("codesense:subscription-delta", move |event| {
                let _ = send.send(serde_json::from_str::<Value>(event.payload()).unwrap());
            });
            std::fs::remove_file(&schema).unwrap();
            if missing_phase {
                let _serial = serial();
                refresh(app.handle(), None, None, Some("outside"), None, &[]);
                assert!(app
                    .state::<Views>()
                    .0
                    .lock()
                    .unwrap()
                    .contains_key(&team_key));
            }
            let private_schema = binding.private_root.join("content/schema/care.yaml");
            std::fs::create_dir_all(private_schema.parent().unwrap()).unwrap();
            std::fs::write(&private_schema, declaration).unwrap();
            let private_store = binding.resolve("care").unwrap();
            let private_folder = private_store.folder().unwrap();
            std::fs::create_dir_all(private_folder.parent().unwrap()).unwrap();
            std::fs::rename(team_store.folder().unwrap(), &private_folder).unwrap();
            std::fs::write(
                private_folder.join("filter.md"),
                "---\ntitle: Private title\n---\nPrivate instructions",
            )
            .unwrap();
            {
                let _serial = serial();
                refresh(app.handle(), None, None, Some("outside"), None, &[]);
            }
            assert!(!app
                .state::<Views>()
                .0
                .lock()
                .unwrap()
                .contains_key(&team_key));
            let empty_team =
                crate::live_hub::subscribe(app.handle(), "records", &team_key, None, None, None)
                    .unwrap();
            assert!(empty_team.snapshot.unwrap().is_empty());
            let removed = receive.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(removed["delta"]["viewKey"], team_key);
            assert_eq!(removed["delta"]["removes"], json!(["filter"]));
            assert_eq!(removed["delta"]["upserts"], json!([]));
            assert!(receive.try_recv().is_err());
            assert!(check_frame_capability(&frame, "subscribe", &args).is_err());
            let private_frame = make_frame(true);
            assert!(check_frame_capability(&private_frame, "subscribe", &args).is_ok());
            let private_key = prepare(app.handle(), &binding, "care").unwrap();
            assert_eq!(private_key, "records:private:acme/ctx:care");
            let private = crate::live_hub::subscribe(
                app.handle(),
                "records",
                &private_key,
                None,
                Some("private-pane"),
                None,
            )
            .unwrap();
            let private_rows = private.snapshot.unwrap();
            assert_eq!(private_rows[0]["title"], "Private title");
            assert_eq!(private_rows[0]["preview"], "Private instructions");
            let actor = Actor::agent("Helper", None, None);
            private_store
                .write(
                    "filter",
                    &super::super::BodyEdit::Replace("Later private instructions".into()),
                    &[("title".into(), json!("Later private title"))],
                    &actor,
                    Some("private-write"),
                )
                .unwrap();
            {
                let _serial = serial();
                refresh(
                    app.handle(),
                    Some(&private_folder),
                    Some(&actor),
                    Some("write"),
                    Some("private-write"),
                    &["filter".into()],
                );
            }
            let changed = receive.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(changed["delta"]["viewKey"], private_key);
            assert_eq!(
                changed["delta"]["upserts"][0]["preview"],
                "Later private instructions"
            );
            assert!(receive.try_recv().is_err());
            let still_empty =
                crate::live_hub::subscribe(app.handle(), "records", &team_key, None, None, None)
                    .unwrap();
            assert_eq!(still_empty.rev, empty_team.rev);
            assert!(still_empty.snapshot.unwrap().is_empty());
        }
    }

    #[test]
    fn a_native_notes_view_receives_one_delta_for_an_agent_notes_write() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        // The view the native list prepares, and the notes an agent's notes_write reaches.
        let binding =
            super::super::binding::Binding::from_context(home.path(), &principal, "acme/ctx", None)
                .unwrap();
        let loc = crate::commands::scratch::org_notes_location_unchecked(
            home.path(),
            &principal,
            "acme/ctx",
        )
        .unwrap();
        let folder = binding.resolve("notes").unwrap().folder().unwrap();
        assert_eq!(
            loc.notes_dir().canonicalize().unwrap(),
            folder.canonicalize().unwrap()
        );
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(crate::live_hub::LiveHub::default());
        app.manage(Views::default());
        let agent = Actor::agent("Helper", None, Some("helper#1".into()));
        let note = super::super::notes::create(&loc, "Plan", "Old", false, &agent, None).unwrap();
        let key = prepare(app.handle(), &binding, "notes").unwrap();
        assert_eq!(key, "records:private:acme/ctx:notes");
        let before =
            crate::live_hub::subscribe(app.handle(), "records", &key, None, Some("native"), None)
                .unwrap()
                .snapshot
                .unwrap()
                .len();
        let (send, receive) = mpsc::channel();
        app.listen("codesense:subscription-delta", move |event| {
            let _ = send.send(serde_json::from_str::<Value>(event.payload()).unwrap());
        });
        super::super::notes::write(
            &loc,
            &note.note.id,
            &super::super::BodyEdit::Replace(
                "- [ ] Call the bank [@Jimmy](cs:agent:jimmy)\n".into(),
            ),
            None,
            None,
            &agent,
            Some("agent-write"),
        )
        .unwrap();
        {
            let _serial = serial();
            refresh(
                app.handle(),
                Some(&folder),
                Some(&agent),
                Some("write"),
                Some("agent-write"),
                &[note.note.id.clone()],
            );
        }
        let written = receive.recv_timeout(Duration::from_secs(1)).unwrap()["delta"].clone();
        assert!(receive.try_recv().is_err());
        assert_eq!(written["viewKey"], key);
        assert_eq!(written["requestId"], "agent-write");
        assert_eq!(written["total"], before);
        // The written note, and any note whose place in the order changed with it.
        let row = written["upserts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == note.note.id.as_str())
            .unwrap();
        assert_eq!(row["title"], "Plan");
        assert_eq!(row["fields"]["pinned"], false);
        assert_eq!(row["preview"], "Call the bank");
        assert_eq!(row["refs"], json!([{"handle":"jimmy","name":"Jimmy"}]));
        assert_eq!(row["updatedBy"]["kind"], "agent");
        let listed = super::super::notes::list(&loc).unwrap();
        let listed = listed.iter().find(|n| n.id == note.note.id).unwrap();
        assert_eq!(listed.preview, "Call the bank");
        assert_eq!(listed.refs, vec![json!({"handle":"jimmy","name":"Jimmy"})]);
    }

    #[test]
    fn a_refresh_reads_history_only_for_the_note_written_or_committed() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        let binding =
            super::super::binding::Binding::from_context(home.path(), &principal, "acme/ctx", None)
                .unwrap();
        let loc = crate::commands::scratch::org_notes_location_unchecked(
            home.path(),
            &principal,
            "acme/ctx",
        )
        .unwrap();
        let folder = binding.resolve("notes").unwrap().folder().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(crate::live_hub::LiveHub::default());
        app.manage(Views::default());
        let agent = Actor::agent("Helper", None, None);
        let ids: Vec<String> = ["One", "Two", "Three"]
            .iter()
            .map(|title| {
                super::super::notes::create(&loc, title, title, false, &agent, None)
                    .unwrap()
                    .note
                    .id
            })
            .collect();
        let reads = || HISTORY_READS.with(|r| r.replace(0));
        reads();
        let key = prepare(app.handle(), &binding, "notes").unwrap();
        let rows = crate::live_hub::subscribe(app.handle(), "records", &key, None, Some("n"), None)
            .unwrap()
            .snapshot
            .unwrap();
        // The first prepare reads every note's history once.
        assert_eq!(reads(), rows.len());
        let head = |rows: &[Value], id: &str| {
            rows.iter().find(|r| r["id"] == id).unwrap()["headCommit"].clone()
        };
        let (send, receive) = mpsc::channel();
        app.listen("codesense:subscription-delta", move |event| {
            let _ = send.send(serde_json::from_str::<Value>(event.payload()).unwrap());
        });
        let refreshed = |actor: Option<&Actor>, kind: &str, changed: &[String]| {
            let _serial = serial();
            refresh(
                app.handle(),
                Some(&folder),
                actor,
                Some(kind),
                None,
                changed,
            );
            receive.recv_timeout(Duration::from_secs(1)).unwrap()["delta"]["upserts"]
                .as_array()
                .unwrap()
                .clone()
        };

        // An agent's write commits at once: only its note's history is read again.
        super::super::notes::write(
            &loc,
            &ids[0],
            &super::super::BodyEdit::Replace("Agent".into()),
            None,
            None,
            &agent,
            None,
        )
        .unwrap();
        let upserts = refreshed(Some(&agent), "write", &ids[..1]);
        assert_eq!(reads(), 1);
        assert_ne!(head(&upserts, &ids[0]), head(&rows, &ids[0]));

        // The person's write commits later; its commit reads that note's history.
        super::super::notes::write(
            &loc,
            &ids[1],
            &super::super::BodyEdit::Replace("Mine".into()),
            None,
            None,
            &Actor::human(),
            None,
        )
        .unwrap();
        refreshed(Some(&Actor::human()), "write", &ids[1..2]);
        assert_eq!(reads(), 1);
        let rev = super::super::history::flush_human(&loc).unwrap().unwrap();
        let upserts = refreshed(None, "commit", &ids[1..2]);
        assert_eq!(reads(), 1);
        assert_eq!(head(&upserts, &ids[1]), json!(rev));
        assert!(app.state::<Views>().0.lock().unwrap()[&key].2.commits.len() == rows.len());

        // A collection repointed at another folder holding the same id: its head is
        // that folder's, and the unchanged notes view reads nothing again.
        let schema = binding.private_root.join("content/schema/care.yaml");
        std::fs::create_dir_all(schema.parent().unwrap()).unwrap();
        std::fs::write(
            &schema,
            "collection: care\nfolder: data/current\nid: slug\n",
        )
        .unwrap();
        let care = || binding.resolve("care").unwrap();
        care()
            .create(Some("same"), &[], "Current", &agent, None)
            .unwrap();
        let care_key = prepare(app.handle(), &binding, "care").unwrap();
        let care_head = || {
            crate::live_hub::subscribe(app.handle(), "records", &care_key, None, None, None)
                .unwrap()
                .snapshot
                .unwrap()[0]["headCommit"]
                .clone()
        };
        let before = care_head();
        std::fs::write(&schema, "collection: care\nfolder: data/import\nid: slug\n").unwrap();
        care()
            .create(Some("same"), &[], "Imported", &agent, None)
            .unwrap();
        reads();
        {
            let _serial = serial();
            refresh(app.handle(), None, None, Some("outside"), None, &[]);
        }
        assert_eq!(reads(), 1);
        let after = care_head();
        assert_ne!(after, before);
        assert_eq!(after, json!(head_commit(&care(), "same").unwrap()));
    }
}
