// SPDX-License-Identifier: MIT
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::history::{self, ChangeKind, HistoryLocation, NoteRevision};
use super::schema::guarded_path;
use super::{Actor, ActorKind, BodyEdit, Collection, IdRule, Overlap, Scope};
use crate::commands::{
    frontmatter_editor::frontmatter_json, frontmatter_fields::set_markdown_fields,
    pipeline_format::PipelineFileFormat,
};
use crate::context_query::{
    engine::{DiskBodyReader, Viewer},
    index::Snapshot,
    model::{ContextQuery, Layer},
};

type Fields = BTreeMap<String, Value>;
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    pub id: String,
    pub fields: Fields,
    pub body: String,
    pub rev: String,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Written {
    #[serde(flatten)]
    pub record: Record,
    pub merged: bool,
    pub overlaps: Vec<Overlap>,
    pub commit: Option<String>,
    pub deleted: bool,
    pub changed: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub title: Option<String>,
    pub matched: String,
    pub snippet: String,
}

// The lock covers the read/modify/write and its retry receipt together.
#[derive(Default)]
struct Writes {
    receipts: HashMap<(PathBuf, String, String), (Instant, Written)>,
}
static WRITES: LazyLock<Mutex<Writes>> = LazyLock::new(|| Mutex::new(Writes::default()));
static FALSE: Value = Value::Bool(false);

pub(super) fn serialize_writes<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    operation()
}

#[derive(Clone)]
pub struct Store {
    caller: crate::context_repo::caller_scope::CallerScope,
    pub collection: Collection,
    pub(crate) records_transport: bool,
    root: PathBuf,
    repo: Option<PathBuf>,
    index: Option<crate::context_query::ContextIndexService>,
}
impl Store {
    pub fn new(root: &Path, collection: Collection, repo: Option<PathBuf>) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        guarded_path(&root, &collection.folder)?;
        if collection.scope == Scope::Private && repo.is_none() {
            return Err("private collection needs its private history repo".into());
        }
        let repo = repo
            .map(|repo| {
                std::fs::create_dir_all(&repo).map_err(|e| e.to_string())?;
                repo.canonicalize().map_err(|e| e.to_string())
            })
            .transpose()?;
        Ok(Self {
            caller: Default::default(),
            root,
            collection,
            repo,
            index: None,
            records_transport: false,
        })
    }
    pub(crate) fn with_caller(
        mut self,
        caller: crate::context_repo::caller_scope::CallerScope,
    ) -> Self {
        self.caller = caller;
        self
    }
    pub(crate) fn require_record(&self, id: &str) -> Result<(), String> {
        self.caller.require(
            &self
                .root
                .join(&self.collection.folder)
                .join(format!("{id}.md")),
        )
    }
    pub(crate) fn for_transport(mut self) -> Self {
        self.records_transport = true;
        self
    }
    pub(crate) fn with_index(mut self, index: crate::context_query::ContextIndexService) -> Self {
        self.index = Some(index);
        self
    }
    pub(crate) fn folder(&self) -> Result<PathBuf, String> {
        guarded_path(&self.root, &self.collection.folder)
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if !super::is_valid_id(id) {
            return Err(format!("invalid record id: {id}"));
        }
        guarded_path(&self.root, &self.collection.folder.join(format!("{id}.md")))
    }
    pub(crate) fn location(&self) -> Result<HistoryLocation, String> {
        if self.collection.scope != Scope::Private {
            return Err("unavailable: no history for team collections".into());
        }
        self.folder()?;
        Ok(HistoryLocation::records(
            self.root.clone(),
            self.repo.clone().ok_or("missing private history repo")?,
            self.collection.folder.clone(),
        ))
    }
    pub fn read(&self, id: &str) -> Result<Record, String> {
        self.require_record(id)?;
        if !matches!(
            self.caller,
            crate::context_repo::caller_scope::CallerScope::Human
        ) {
            return self
                .caller
                .read_admitted(&self.path(id)?)
                .map(|raw| parse(id, &raw));
        }
        let raw =
            std::fs::read_to_string(self.path(id)?).map_err(|e| format!("record {id}: {e}"))?;
        Ok(parse(id, &raw))
    }
    pub fn list(&self) -> Result<Vec<Record>, String> {
        if self.collection.scope == Scope::Private && self.collection.name == "notes" {
            super::notes::prepare_list(&self.location()?, &self.collection.options)?;
        }
        self.list_records()
    }
    pub(crate) fn list_records(&self) -> Result<Vec<Record>, String> {
        let folder = self.folder()?;
        if !self.caller.visible(&folder) || !folder.exists() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !self.caller.allows(&entry.path()) {
                continue;
            }
            let name = entry.file_name();
            if let Some(id) = name
                .to_str()
                .and_then(|s| s.strip_suffix(".md"))
                .filter(|s| super::is_valid_id(s))
            {
                out.push(self.read(id)?);
            }
        }
        out.sort_by(|a, b| compare_records(a, b, &self.collection));
        Ok(out)
    }
    /// `humanOnly` lists the actions only a person may take in this collection.
    fn refuse_agent(&self, actor: &Actor, action: &str, verb: &str) -> Result<(), String> {
        if actor.kind == ActorKind::Agent
            && self.collection.options.human_only.iter().any(|s| s == action)
        {
            return Err(format!("only a human can {verb} records in this collection"));
        }
        Ok(())
    }
    fn mutate(
        &self,
        key: &str,
        request_id: Option<&str>,
        actor: &Actor,
        kind: ChangeKind,
        extra: &[(&str, String)],
        operation: impl FnOnce() -> Result<(Record, Vec<Overlap>, bool, bool), String>,
    ) -> Result<Written, String> {
        if request_id.is_some_and(|r| r.chars().count() > 64) {
            return Err("requestId must be at most 64 characters".into());
        }
        // Restore decides its own guard (it can rewrite fields or only the body);
        // a metadata update is guarded as a write even though only the UI sends it.
        let action = match kind {
            ChangeKind::Delete => Some(("delete", "delete")),
            ChangeKind::Write | ChangeKind::Meta => Some(("write", "edit")),
            ChangeKind::Create | ChangeKind::Restore => None,
        };
        if let Some((action, verb)) = action {
            self.refuse_agent(actor, action, verb)?;
        }
        let loc = if self.collection.scope == Scope::Private {
            Some(self.location()?)
        } else {
            None
        };
        let hold = loc.as_ref().map(|_| history::hold_history());
        // Legacy history transactions may write through the untracked Notes
        // adapter while already holding history. Keep this order on every path.
        let _projection = super::projection::serial();
        let mut writes = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        writes
            .receipts
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        let cache_key = request_id.map(|r| {
            (
                self.root.join(&self.collection.folder),
                key.to_string(),
                r.to_string(),
            )
        });
        if let Some((_, written)) = cache_key.as_ref().and_then(|k| writes.receipts.get(k)) {
            return Ok(written.clone());
        }
        let at_once = history::commits_at_once(actor, kind);
        if let Some(loc) = &loc {
            if at_once {
                history::commit_outside_changes(hold.as_ref().unwrap(), loc)?;
            }
        }
        let (record, overlaps, merged, changed) = operation()?;
        let commit = if changed {
            if let Some(loc) = &loc {
                if at_once {
                    hold.as_ref().unwrap().commit(
                        loc,
                        actor,
                        kind,
                        &format!("{} changed {}", actor.name, record.id),
                        extra,
                    )?
                } else {
                    history::schedule_record_commit(
                        loc,
                        Duration::from_millis(self.collection.options.history.human_debounce_ms),
                        kind,
                    );
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if changed {
            if let Some(index) = &self.index {
                index.note_paths([self.path(&record.id)?]);
            }
        }
        let result = Written {
            record,
            overlaps,
            merged,
            commit,
            deleted: kind == ChangeKind::Delete,
            changed,
        };
        if let Some(key) = cache_key {
            writes
                .receipts
                .insert(key, (Instant::now(), result.clone()));
        }
        drop(writes);
        drop(hold);
        if result.changed {
            super::projection::notify(self, &result, actor, kind.as_str(), request_id);
        }
        Ok(result)
    }
    fn save(&self, id: &str, raw: &str, actor: &Actor) -> Result<Record, String> {
        let raw = crate::commands::frontmatter_fields::set_markdown_metadata(
            raw,
            "updatedAt",
            &serde_json::json!(super::notes::now_millis()),
        )?;
        let raw = crate::commands::frontmatter_fields::set_markdown_metadata(
            &raw,
            "updatedBy",
            &serde_json::to_value(actor).map_err(|e| e.to_string())?,
        )?;
        self.persist(id, &raw)
    }
    fn persist(&self, id: &str, raw: &str) -> Result<Record, String> {
        let path = self.path(id)?;
        std::fs::create_dir_all(path.parent().ok_or("record has no folder")?)
            .map_err(|e| e.to_string())?;
        super::io::atomic_write(path.parent().unwrap(), &format!("{id}.md"), raw.as_bytes())?;
        Ok(parse(id, &raw))
    }
    pub fn create(
        &self,
        id: Option<&str>,
        fields: &[(String, Value)],
        body: &str,
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.collection.validate_fields(fields)?;
        if self.collection.id == IdRule::Slug && id.is_none() {
            return Err("slug collections require an id".into());
        }
        self.mutate(
            id.unwrap_or(""),
            request_id,
            actor,
            ChangeKind::Create,
            &[],
            || {
                let id = id
                    .map(str::to_string)
                    .unwrap_or_else(super::io::generate_id);
                if self.path(&id)?.exists() {
                    return Err(format!("record already exists: {id}"));
                }
                let header = set_markdown_fields("---\n---\n", fields)?;
                let raw = format!("{header}{body}");
                for def in self.collection.fields.values().filter(|d| d.required) {
                    if !frontmatter_json(PipelineFileFormat::Markdown, &raw).contains_key(&def.name)
                    {
                        return Err(format!("{} is required", def.name));
                    }
                }
                Ok((self.save(&id, &raw, actor)?, vec![], false, true))
            },
        )
    }
    pub fn write(
        &self,
        id: &str,
        edit: &BodyEdit,
        fields: &[(String, Value)],
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.write_inner(id, edit, fields, actor, request_id, true)
    }
    /// Change UI metadata while keeping the last body editor and edit time.
    pub(crate) fn update_metadata(
        &self,
        id: &str,
        fields: &[(String, Value)],
        actor: &Actor,
    ) -> Result<Written, String> {
        self.write_inner(id, &BodyEdit::Keep, fields, actor, None, false)
    }
    fn write_inner(
        &self,
        id: &str,
        edit: &BodyEdit,
        fields: &[(String, Value)],
        actor: &Actor,
        request_id: Option<&str>,
        attribute: bool,
    ) -> Result<Written, String> {
        self.collection.validate_fields(fields)?;
        if !<[_]>::is_empty(fields) {
            self.refuse_agent(actor, "setFields", "edit")?;
        }
        let kind = if attribute {
            ChangeKind::Write
        } else {
            ChangeKind::Meta
        };
        self.mutate(id, request_id, actor, kind, &[], || {
            let path = self.path(id)?;
            let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let (_, body) = split_document(&raw);
            let (next_body, overlaps, merged) = match edit {
                BodyEdit::ReplaceFrom { base, content } => {
                    let (next, overlaps) = super::edit::merge_with_overlaps(base, content, body)?;
                    (next, overlaps, body != base && body != content)
                }
                _ => (super::edit::apply_edit(body, edit)?, vec![], false),
            };
            let (head, _) = split_document(&raw);
            let next = set_markdown_fields(&format!("{head}{next_body}"), fields)?;
            let changed = raw != next;
            let record = if !changed {
                parse(id, &raw)
            } else if attribute {
                self.save(id, &next, actor)?
            } else {
                self.persist(id, &next)?
            };
            Ok((record, overlaps, merged, changed))
        })
    }
    pub fn set_fields(
        &self,
        id: &str,
        fields: &[(String, Value)],
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.write(id, &BodyEdit::Keep, fields, actor, request_id)
    }
    pub fn delete(
        &self,
        id: &str,
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.mutate(id, request_id, actor, ChangeKind::Delete, &[], || {
            let record = self.read(id)?;
            std::fs::remove_file(self.path(id)?).map_err(|e| e.to_string())?;
            Ok((record, vec![], false, true))
        })
    }
    pub fn history(&self, id: &str, limit: usize) -> Result<Vec<NoteRevision>, String> {
        self.path(id)?;
        let loc = self.location()?;
        let hold = history::hold_history();
        history::commit_outside_changes(&hold, &loc)?;
        history::note_history(&loc, id, limit)
    }
    pub fn read_at(&self, id: &str, commit: &str) -> Result<Record, String> {
        self.path(id)?;
        let loc = self.location()?;
        let hold = history::hold_history();
        history::commit_outside_changes(&hold, &loc)?;
        let (raw, _) = history::read_content_at(&loc, id, commit)?;
        Ok(parse(id, &raw))
    }
    pub fn restore(
        &self,
        id: &str,
        commit: &str,
        undo: bool,
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.restore_document(id, commit, undo, actor, request_id, false)
    }
    pub(crate) fn restore_body(
        &self,
        id: &str,
        commit: &str,
        undo: bool,
        actor: &Actor,
        request_id: Option<&str>,
    ) -> Result<Written, String> {
        self.restore_document(id, commit, undo, actor, request_id, true)
    }
    fn restore_document(
        &self,
        id: &str,
        commit: &str,
        undo: bool,
        actor: &Actor,
        request_id: Option<&str>,
        body_only: bool,
    ) -> Result<Written, String> {
        self.path(id)?;
        self.refuse_agent(actor, "write", "edit")?;
        if !body_only {
            self.refuse_agent(actor, "setFields", "edit")?;
        }
        let loc = self.location()?;
        self.mutate(
            id,
            request_id,
            actor,
            ChangeKind::Restore,
            &[(
                if undo { "Undid" } else { "Restored-From" },
                commit.to_string(),
            )],
            || {
                let path = self.path(id)?;
                let current = if path.exists() {
                    std::fs::read_to_string(path).map_err(|e| e.to_string())?
                } else {
                    String::new()
                };
                let raw = if body_only {
                    let body = if undo {
                        history::undo_note_body(&loc, id, commit, split_document(&current).1)?
                    } else {
                        history::read_note_body_at(&loc, id, commit)?.content
                    };
                    let head = match split_document(&current).0 {
                        "" => split_document(&history::note_document_at(&loc, id, commit)?)
                            .0
                            .to_string(),
                        head => head.to_string(),
                    };
                    format!("{head}{body}")
                } else if undo {
                    history::undo_content(&loc, id, commit, &current)?
                } else {
                    history::read_content_at(&loc, id, commit)?.0
                };
                let changed = current != raw || !self.path(id)?.exists();
                Ok((
                    if changed {
                        self.save(id, &raw, actor)?
                    } else {
                        parse(id, &raw)
                    },
                    vec![],
                    false,
                    changed,
                ))
            },
        )
    }
    pub(crate) fn search_snapshot(
        &self,
        snapshot: &Snapshot,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, String> {
        let needle = query.trim().to_lowercase();
        if limit == 0 || needle.is_empty() {
            return Ok(vec![]);
        }
        let folder = self.folder()?;
        let mut q = ContextQuery {
            // Select candidates through the index; match decoded titles and the full
            // body below, without the context query's bounded raw-text prefilter.
            limit: Some(crate::context_query::model::MAX_LIMIT),
            ..ContextQuery::default()
        };
        q.scope.path_prefix = Some(folder.to_string_lossy().into_owned());
        q.scope.same_org_as = Some(folder.to_string_lossy().into_owned());
        q.scope.layers = Some(vec![if self.collection.scope == Scope::Private {
            Layer::Private
        } else {
            Layer::Team
        }]);
        let mut out = vec![];
        loop {
            let result = snapshot
                .query(
                    &q,
                    &Viewer {
                        scope: self.caller.clone(),
                        ..Viewer::default()
                    },
                    &DiskBodyReader,
                )
                .map_err(|e| e.to_string())?;
            for row in result.rows {
                let path = Path::new(&row.path);
                if path.parent() != Some(folder.as_path()) {
                    continue;
                }
                let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| super::is_valid_id(s))
                else {
                    continue;
                };
                let record = self.read(id)?;
                let title = self
                    .collection
                    .title
                    .as_ref()
                    .and_then(|t| record.fields.get(t))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let title_match = title
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(&needle));
                let body_line = record
                    .body
                    .lines()
                    .find(|s| s.to_lowercase().contains(&needle));
                if !title_match && body_line.is_none() {
                    continue;
                }
                let snippet = body_line
                    .map(|s| super::notes::make_snippet(s, &needle))
                    .unwrap_or_else(|| title.clone().unwrap_or_default());
                out.push(SearchHit {
                    id: id.into(),
                    title,
                    matched: match (title_match, body_line.is_some()) {
                        (true, true) => "both",
                        (true, false) => "title",
                        _ => "body",
                    }
                    .into(),
                    snippet,
                });
                if out.len() >= limit {
                    return Ok(out);
                }
            }
            match result.next_cursor {
                Some(cursor) => q.cursor = Some(cursor),
                None => break,
            }
        }
        Ok(out)
    }
    pub(crate) fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let folder = self.folder()?;
        if let Some(index) = &self.index {
            let snapshot = index.snapshot();
            if snapshot
                .roots
                .iter()
                .any(|root| folder.starts_with(&root.dir))
            {
                return self.search_snapshot(&snapshot, query, limit);
            }
        }
        // Headless callers and newly declared folders use the same index/query
        // engine before the background service has reconciled their root.
        let mut index = crate::context_query::index::IndexState::default();
        index.reconcile(vec![crate::context_query::index::IndexRoot {
            dir: folder,
            org: Some("records".into()),
            team: None,
            layer: if self.collection.scope == Scope::Private {
                crate::context_query::model::Layer::Private
            } else {
                crate::context_query::model::Layer::Team
            },
        }]);
        self.search_snapshot(&index.snapshot(1, true), query, limit)
    }
}

pub(crate) fn parse(id: &str, raw: &str) -> Record {
    Record {
        id: id.into(),
        fields: frontmatter_json(PipelineFileFormat::Markdown, raw),
        body: split_document(raw).1.into(),
        rev: format!("{:x}", Sha256::digest(raw.as_bytes())),
    }
}

/// Split at the closing fence without normalizing the body or its line endings.
pub(crate) fn split_document(raw: &str) -> (&str, &str) {
    let text = raw.trim_start_matches('\u{feff}');
    if text.lines().next().map(str::trim_end) != Some("---") {
        return ("", raw);
    }
    let mut offset = raw.len() - text.len();
    for (i, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        if i > 0 && matches!(line.trim_end(), "---" | "...") {
            return (&raw[..offset], &raw[offset..]);
        }
    }
    ("", raw)
}

pub(super) fn compare_records(
    a: &Record,
    b: &Record,
    collection: &Collection,
) -> std::cmp::Ordering {
    for spec in &collection.order {
        let (key, desc) = spec
            .split_once(' ')
            .map_or((spec.as_str(), false), |(k, d)| (k, d == "desc"));
        let boolean = collection
            .fields
            .get(key)
            .is_some_and(|field| matches!(field.r#type.as_deref(), Some("boolean" | "yes-no")));
        let av = normalize_sort_value(a.fields.get(key), boolean);
        let bv = normalize_sort_value(b.fields.get(key), boolean);
        let cmp = compare_values(av, bv);
        let cmp = if desc { cmp.reverse() } else { cmp };
        if !cmp.is_eq() {
            return cmp;
        }
    }
    a.id.cmp(&b.id)
}

fn normalize_sort_value(value: Option<&Value>, boolean: bool) -> Option<&Value> {
    value.or_else(|| boolean.then_some(&FALSE))
}

// This order is specified by the shared ordering cases used by Rust and the browser mock.
fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |value: Option<&Value>| match value {
        None => 0,
        Some(Value::Bool(_)) => 1,
        Some(Value::Number(_)) => 2,
        Some(Value::String(_)) => 3,
        Some(Value::Null) => 4,
        Some(Value::Array(_)) => 5,
        Some(Value::Object(_)) => 6,
    };
    let cmp = rank(a).cmp(&rank(b));
    if !cmp.is_eq() {
        return cmp;
    }
    match (a, b) {
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a.cmp(b),
        (Some(Value::Number(a)), Some(Value::Number(b))) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
        // Use the same code-unit order as JavaScript, without locale collation or JSON escaping.
        (Some(Value::String(a)), Some(Value::String(b))) => a.encode_utf16().cmp(b.encode_utf16()),
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for (a, b) in a.iter().zip(b) {
                let cmp = compare_values(Some(a), Some(b));
                if !cmp.is_eq() {
                    return cmp;
                }
            }
            a.len().cmp(&b.len())
        }
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut a: Vec<_> = a.iter().collect();
            let mut b: Vec<_> = b.iter().collect();
            a.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            b.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            for ((ak, av), (bk, bv)) in a.iter().zip(&b) {
                let cmp = ak
                    .encode_utf16()
                    .cmp(bk.encode_utf16())
                    .then_with(|| compare_values(Some(av), Some(bv)));
                if !cmp.is_eq() {
                    return cmp;
                }
            }
            a.len().cmp(&b.len())
        }
        _ => Ordering::Equal,
    }
}
