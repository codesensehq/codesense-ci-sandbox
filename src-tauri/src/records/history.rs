// SPDX-License-Identifier: MIT
//! Who changed a note, and its history.
//!
//! Notes live in a local git folder that is never pushed: an org's notes are
//! `scratch/orgs/<org>/notes/` inside the profile's scratch repo (the machine-local
//! fallback store is its own repo). Every agent write is committed at once; a
//! person's typing is committed after a quiet period, so a burst of saves is one
//! revision. Each commit carries trailers naming the change and who made it, and
//! touches only the notes folder, so anything else in the repo is left as it is.
//!
//! A change also emits [`NOTES_CHANGED`] so an open notes view reloads.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::commands::scratch::{
    is_valid_note_id, merge_note_text, notes_dir, MergeOverlap, NoteMeta,
};
#[cfg(test)]
use crate::commands::scratch::{write_note_as, Note, NoteEdit};

/// Emitted after any note write, from the app or from an agent.
pub const NOTES_CHANGED: &str = "notes-changed";

/// How long a person's edits sit before they are committed as one revision.
const HUMAN_QUIET: Duration = Duration::from_secs(30);

/// The author email every notes commit carries; the actor is in the trailers.
const COMMIT_EMAIL: &str = "notes@codesense.local";

/// Whether a person or an agent made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActorKind {
    Human,
    Agent,
}

/// Who made a change: the person at this Mac, or an agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteActor {
    pub kind: ActorKind,
    /// "You" for the person; an agent's tab name.
    pub name: String,
    /// The agent's tab session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// How others address the agent (`list_sessions` handle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The face the agent wore, as `pack:id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

impl NoteActor {
    pub fn human() -> Self {
        Self {
            kind: ActorKind::Human,
            name: "You".to_string(),
            session_id: None,
            handle: None,
            avatar: None,
        }
    }

    pub fn agent(name: &str, session_id: Option<String>, handle: Option<String>) -> Self {
        let name = one_line(name);
        Self {
            kind: ActorKind::Agent,
            name: if name.is_empty() {
                "Agent".to_string()
            } else {
                name
            },
            session_id: session_id.map(|s| one_line(&s)).filter(|s| !s.is_empty()),
            handle: handle.map(|h| one_line(&h)).filter(|h| !h.is_empty()),
            avatar: None,
        }
    }

    /// This actor, wearing `avatar` (`pack:id`).
    pub fn with_avatar(mut self, avatar: Option<String>) -> Self {
        self.avatar = avatar.map(|a| one_line(&a)).filter(|a| !a.is_empty());
        self
    }

    fn trailers(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![
            (
                "Actor-Kind",
                match self.kind {
                    ActorKind::Human => "human",
                    ActorKind::Agent => "agent",
                }
                .to_string(),
            ),
            ("Actor-Name", self.name.clone()),
        ];
        if let Some(s) = &self.session_id {
            out.push(("Actor-Session", s.clone()));
        }
        if let Some(h) = &self.handle {
            out.push(("Actor-Handle", h.clone()));
        }
        if let Some(a) = &self.avatar {
            out.push(("Actor-Avatar", a.clone()));
        }
        out
    }
}

/// A trailer value never spans lines.
fn one_line(s: &str) -> String {
    s.split(['\n', '\r'])
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// What a change did to a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Create,
    Write,
    Restore,
    Delete,
    Meta,
}

impl ChangeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Write => "write",
            ChangeKind::Restore => "restore",
            ChangeKind::Delete => "delete",
            ChangeKind::Meta => "meta",
        }
    }
}

/// The [`NOTES_CHANGED`] payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesChanged {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    /// The org whose notes changed; `None` for the machine-local store.
    pub org_slug: Option<String>,
    pub id: String,
    /// The history commit the change was saved as; `None` while a person's
    /// typing waits for its quiet period (a second event follows with the commit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub actor: NoteActor,
    pub kind: ChangeKind,
}

/// Tell open notes views that note `id` changed.
pub(crate) fn emit_notes_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    org_slug: Option<&str>,
    id: &str,
    commit: Option<&str>,
    actor: &NoteActor,
    kind: ChangeKind,
) {
    emit_change(app, org_slug, id, commit, actor, kind, None);
}

/// Records panes also notify the native Notes view, including human edits
/// made outside its own editor.
pub(crate) fn emit_records_notes_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    org: Option<&str>,
    id: &str,
    commit: Option<&str>,
    actor: &NoteActor,
    kind: ChangeKind,
) {
    emit_change(app, org, id, commit, actor, kind, Some("records"));
}
fn emit_change<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    org: Option<&str>,
    id: &str,
    commit: Option<&str>,
    actor: &NoteActor,
    kind: ChangeKind,
    source: Option<&'static str>,
) {
    use tauri::Emitter;
    let payload = NotesChanged {
        source,
        org_slug: org.map(str::to_owned),
        id: id.into(),
        commit: commit.map(str::to_owned),
        actor: actor.clone(),
        kind,
    };
    if let Err(error) = app.emit(NOTES_CHANGED, payload) {
        log::debug!(target: "scratch", "{NOTES_CHANGED} emit failed: {error}");
    }
}

/// Where one store of notes lives: `root` holds the `notes/` folder, and `repo`
/// is the git repo its history is committed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLocation {
    pub root: PathBuf,
    pub repo: PathBuf,
    pub folder: PathBuf,
    legacy_notes: bool,
}

pub(crate) type NotesLocation = HistoryLocation;

impl HistoryLocation {
    pub(crate) fn for_records(&self) -> Self {
        let mut loc = self.clone();
        loc.legacy_notes = false;
        loc
    }

    pub(crate) fn records(root: PathBuf, repo: PathBuf, folder: PathBuf) -> Self {
        Self {
            root,
            repo,
            folder,
            legacy_notes: false,
        }
    }
    fn valid_id(&self, id: &str) -> bool {
        if self.legacy_notes {
            is_valid_note_id(id)
        } else {
            super::schema::is_valid_id(id)
        }
    }
    fn trailer(&self) -> &str {
        if self.legacy_notes {
            "Note"
        } else {
            "Record"
        }
    }

    /// An org's notes in the profile's scratch repo.
    pub(crate) fn in_scratch(root: PathBuf, scratch_repo: PathBuf) -> Self {
        Self {
            root,
            repo: scratch_repo,
            folder: "notes".into(),
            legacy_notes: true,
        }
    }

    /// The machine-local store: its notes folder is its own repo.
    pub(crate) fn local(root: PathBuf) -> Self {
        let repo = notes_dir(&root);
        Self {
            root,
            repo,
            folder: "notes".into(),
            legacy_notes: true,
        }
    }

    pub(crate) fn notes_dir(&self) -> PathBuf {
        self.root.join(&self.folder)
    }

    /// The notes folder inside the repo, `/`-separated; empty when the folder
    /// is the repo.
    fn prefix(&self) -> Result<String, String> {
        let rel = self
            .notes_dir()
            .strip_prefix(&self.repo)
            .map_err(|_| "the notes folder is outside its history repo".to_string())?
            .to_path_buf();
        Ok(rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"))
    }

    fn path_of(&self, prefix: &str, name: &str) -> String {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        }
    }
}

/// One process-wide lock over every notes commit, and over every write that a
/// commit could otherwise pick up half-way: the scratch repo is shared by every
/// org's notes.
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

/// The notes history held still: until it is dropped no other notes write or
/// commit lands, so a write and the commit that credits it can't be split by
/// someone else's change.
pub(crate) struct HistoryHold {
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Hold the notes history (see [`HistoryHold`]). Take it before the notes lock,
/// never while holding it.
pub(crate) fn hold_history() -> HistoryHold {
    HistoryHold {
        _guard: HISTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

impl HistoryHold {
    /// Commit the notes folder as it is on disk, crediting `actor`: the new
    /// commit, or `None` when nothing in the folder changed since the last one.
    pub(crate) fn commit(
        &self,
        loc: &NotesLocation,
        actor: &NoteActor,
        change: ChangeKind,
        summary: &str,
        extra: &[(&str, String)],
    ) -> Result<Option<String>, String> {
        commit_held(loc, actor, change, summary, extra).map(|c| c.map(|c| c.rev))
    }
}

fn open_or_init(repo: &Path) -> Result<git2::Repository, String> {
    match git2::Repository::open(repo) {
        Ok(r) => Ok(r),
        Err(_) => {
            std::fs::create_dir_all(repo)
                .map_err(|e| format!("cannot create the notes history folder: {e}"))?;
            git2::Repository::init(repo)
                .map_err(|e| format!("cannot initialise the notes history: {e}"))
        }
    }
}

/// The files of a notes folder that are committed: the index, the bodies, the
/// ignore rules and the seed marker. Agent scaffolding and temp files are not.
fn is_tracked_name(loc: &HistoryLocation, name: &str) -> bool {
    (loc.legacy_notes && matches!(name, "index.json" | ".gitignore" | ".prompts-seeded"))
        || name.strip_suffix(".md").is_some_and(|id| loc.valid_id(id))
}

fn git_err(what: &str) -> impl Fn(git2::Error) -> String + '_ {
    move |e| format!("notes history: {what}: {}", e.message())
}

/// A notes commit: its id, and the notes whose bodies it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Committed {
    pub rev: String,
    pub ids: BTreeSet<String>,
}

/// Commit the notes folder as it is on disk, crediting `actor`. Returns the new
/// commit, or `None` when nothing in the folder changed since the last one. The
/// caller holds the history.
fn commit_held(
    loc: &NotesLocation,
    actor: &NoteActor,
    change: ChangeKind,
    summary: &str,
    extra: &[(&str, String)],
) -> Result<Option<Committed>, String> {
    let repo = open_or_init(&loc.repo)?;
    let prefix = loc.prefix()?;
    let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let head_tree = head
        .as_ref()
        .map(|c| c.tree())
        .transpose()
        .map_err(git_err("read HEAD"))?;

    let mut index = git2::Index::new().map_err(git_err("new index"))?;
    if let Some(tree) = &head_tree {
        index.read_tree(tree).map_err(git_err("read tree"))?;
    }
    let under_prefix =
        |path: &[u8]| prefix.is_empty() || path.starts_with(format!("{prefix}/").as_bytes());
    let stale: Vec<Vec<u8>> = index
        .iter()
        .map(|e| e.path)
        .filter(|p| under_prefix(p))
        .collect();
    for path in stale {
        let path = String::from_utf8_lossy(&path).into_owned();
        index
            .remove_path(Path::new(&path))
            .map_err(git_err("remove path"))?;
    }
    let mut entries = Vec::new();
    if let Ok(dir) = std::fs::read_dir(loc.notes_dir()) {
        let mut names: Vec<String> = dir
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| is_tracked_name(loc, n))
            .collect();
        names.sort();
        for name in names {
            let bytes = match std::fs::read(loc.notes_dir().join(&name)) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let oid = repo.blob(&bytes).map_err(git_err("write blob"))?;
            let path = loc.path_of(&prefix, &name);
            let entry = git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: bytes.len() as u32,
                id: oid,
                flags: 0,
                flags_extended: 0,
                path: path.into_bytes(),
            };
            index.add(&entry).map_err(git_err("stage"))?;
            entries.push(entry);
        }
    }
    let tree_id = index.write_tree_to(&repo).map_err(git_err("write tree"))?;
    if head_tree.as_ref().map(|t| t.id()) == Some(tree_id) {
        return Ok(None);
    }
    let tree = repo.find_tree(tree_id).map_err(git_err("find tree"))?;

    let changed_ids = changed_note_ids(&repo, head_tree.as_ref(), &tree, &prefix);
    let mut message = one_line(summary);
    message.push_str("\n\n");
    message.push_str(&format!("{}-Change: {}\n", loc.trailer(), change.as_str()));
    for id in &changed_ids {
        message.push_str(&format!("{}-Id: {id}\n", loc.trailer()));
    }
    for (k, v) in actor.trailers() {
        message.push_str(&format!("{k}: {v}\n"));
    }
    for (k, v) in extra {
        message.push_str(&format!("{k}: {}\n", one_line(v)));
    }
    let author = git2::Signature::now(&actor.name, COMMIT_EMAIL).map_err(git_err("signature"))?;
    let committer =
        git2::Signature::now("CodeSense", COMMIT_EMAIL).map_err(git_err("signature"))?;
    let parents: Vec<&git2::Commit> = head.iter().collect();
    let oid = repo
        .commit(Some("HEAD"), &author, &committer, &message, &tree, &parents)
        .map_err(git_err("commit"))?;

    // Keep the repo's own index in step for the notes folder, so `git status`
    // in the scratch repo shows the folder clean. Best-effort: the commit stands.
    if let Ok(mut disk) = repo.index() {
        if prefix.is_empty() {
            let _ = disk.clear();
        } else {
            let _ = disk.remove_dir(Path::new(&prefix), 0);
        }
        for entry in &entries {
            let _ = disk.add(entry);
        }
        let _ = disk.write();
    }
    if actor.kind == super::ActorKind::Human {
        super::projection::committed(loc, &changed_ids.iter().cloned().collect::<Vec<_>>());
    }
    Ok(Some(Committed {
        rev: oid.to_string(),
        ids: changed_ids,
    }))
}

/// The note ids whose body differs between `old` and `new`.
fn changed_note_ids(
    repo: &git2::Repository,
    old: Option<&git2::Tree>,
    new: &git2::Tree,
    prefix: &str,
) -> BTreeSet<String> {
    let mut opts = git2::DiffOptions::new();
    if !prefix.is_empty() {
        opts.pathspec(prefix);
    }
    let mut ids = BTreeSet::new();
    if let Ok(diff) = repo.diff_tree_to_tree(old, Some(new), Some(&mut opts)) {
        for delta in diff.deltas() {
            for file in [delta.old_file(), delta.new_file()] {
                let Some(path) = file.path() else { continue };
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if let Some(id) = name
                    .strip_suffix(".md")
                    .filter(|id| super::schema::is_valid_id(id))
                {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}

/// One revision of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRevision {
    /// The revision's commit id: pass it to `notes_read_at` / `notes_restore`.
    pub commit: String,
    /// When it was committed, ms since the epoch.
    pub at: u64,
    pub actor: NoteActor,
    /// `create`, `write`, `restore`, `delete` or `meta`.
    pub change: String,
    /// What changed, in a few words.
    pub summary: String,
    /// Lines added and removed in this note's body.
    pub added: usize,
    pub removed: usize,
    /// For a restore: the commit whose text it brought back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<String>,
}

fn trailers_of(message: &str) -> HashMap<String, String> {
    let body = message.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    body.lines()
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn actor_of(trailers: &HashMap<String, String>, author: &str) -> NoteActor {
    match trailers.get("Actor-Kind").map(String::as_str) {
        Some("human") => NoteActor::human(),
        _ => NoteActor::agent(
            trailers
                .get("Actor-Name")
                .map(String::as_str)
                .unwrap_or(author),
            trailers.get("Actor-Session").cloned(),
            trailers.get("Actor-Handle").cloned(),
        )
        .with_avatar(trailers.get("Actor-Avatar").cloned()),
    }
}

fn blob_at(tree: &git2::Tree, path: &str) -> Option<git2::Oid> {
    tree.get_path(Path::new(path)).ok().map(|e| e.id())
}

/// Note `id`'s revisions, newest first, at most `limit`.
pub(crate) fn note_history(
    loc: &NotesLocation,
    id: &str,
    limit: usize,
) -> Result<Vec<NoteRevision>, String> {
    if !loc.valid_id(id) {
        return Err(format!("invalid note id: {id}"));
    }
    let Ok(repo) = git2::Repository::open(&loc.repo) else {
        return Ok(Vec::new());
    };
    let Ok(head) = repo.head().and_then(|h| h.peel_to_commit()) else {
        return Ok(Vec::new());
    };
    let path = loc.path_of(&loc.prefix()?, &format!("{id}.md"));
    let mut walk = repo.revwalk().map_err(git_err("walk"))?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .map_err(git_err("walk"))?;
    walk.push(head.id()).map_err(git_err("walk"))?;
    let mut out = Vec::new();
    for oid in walk.flatten() {
        if out.len() >= limit {
            break;
        }
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let new = blob_at(&tree, &path);
        let old_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let old = old_tree.as_ref().and_then(|t| blob_at(t, &path));
        if new == old {
            continue;
        }
        let sidecar = loc.path_of(&loc.prefix()?, "index.json");
        let old_legacy = old_tree
            .as_ref()
            .is_some_and(|tree| legacy_meta_at(&repo, tree, &sidecar, id).is_some());
        let new_legacy = legacy_meta_at(&repo, &tree, &sidecar, id).is_some();
        let (added, removed) = line_stats(&repo, (old, old_legacy), (new, new_legacy));
        let message = commit.message().unwrap_or("");
        let trailers = trailers_of(message);
        out.push(NoteRevision {
            commit: oid.to_string(),
            at: (commit.time().seconds().max(0) as u64) * 1000,
            actor: actor_of(&trailers, commit.author().name().unwrap_or("Agent")),
            change: trailers
                .get(&format!("{}-Change", loc.trailer()))
                .or_else(|| trailers.get("Note-Change"))
                .cloned()
                .unwrap_or_else(|| "write".to_string()),
            summary: Option::flatten(Result::ok(commit.summary()))
                .unwrap_or("")
                .trim()
                .to_string(),
            added,
            removed,
            restored_from: trailers.get("Restored-From").cloned(),
        });
    }
    Ok(out)
}

fn line_stats(
    repo: &git2::Repository,
    old: (Option<git2::Oid>, bool),
    new: (Option<git2::Oid>, bool),
) -> (usize, usize) {
    let bytes = |(oid, legacy): (Option<git2::Oid>, bool)| {
        oid.and_then(|o| repo.find_blob(o).ok())
            .map(|b| {
                if legacy {
                    b.content().to_vec()
                } else {
                    super::store::split_document(&String::from_utf8_lossy(b.content()))
                        .1
                        .as_bytes()
                        .to_vec()
                }
            })
            .unwrap_or_default()
    };
    let (old, new) = (bytes(old), bytes(new));
    git2::Patch::from_buffers(&old, None, &new, None, None)
        .ok()
        .and_then(|p| p.line_stats().ok())
        .map(|(_, a, r)| (a, r))
        .unwrap_or((0, 0))
}

/// A note's text as it was at one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteAtCommit {
    pub id: String,
    /// The full commit id of the revision.
    pub commit: String,
    /// Its title then, when the index of that commit names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
}

/// A revision named by its full or abbreviated hex commit id.
fn find_commit<'r>(repo: &'r git2::Repository, commit: &str) -> Result<git2::Commit<'r>, String> {
    let commit = commit.trim();
    if commit.len() < 4 || commit.len() > 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "commit {commit:?} must be a commit id from notes_history"
        ));
    }
    repo.revparse_single(commit)
        .and_then(|o| o.peel_to_commit())
        .map_err(|_| format!("no notes commit {commit}"))
}

/// Note `id` as it was at `commit`.
struct DocumentVersion {
    raw: String,
    commit: String,
    legacy: Option<NoteMeta>,
}

fn legacy_meta_at(
    repo: &git2::Repository,
    tree: &git2::Tree,
    sidecar: &str,
    id: &str,
) -> Option<NoteMeta> {
    blob_at(tree, sidecar)
        .and_then(|oid| repo.find_blob(oid).ok())
        .and_then(|blob| serde_json::from_slice::<Vec<NoteMeta>>(blob.content()).ok())
        .and_then(|metas| metas.into_iter().find(|meta| meta.id == id))
}

fn document_at(loc: &HistoryLocation, id: &str, commit: &str) -> Result<DocumentVersion, String> {
    if !loc.valid_id(id) {
        return Err(format!("invalid note id: {id}"));
    }
    let repo = git2::Repository::open(&loc.repo)
        .map_err(|_| "this note has no history yet".to_string())?;
    let found = find_commit(&repo, commit)?;
    let tree = found.tree().map_err(git_err("read tree"))?;
    let prefix = loc.prefix()?;
    let body = blob_at(&tree, &loc.path_of(&prefix, &format!("{id}.md")))
        .and_then(|o| repo.find_blob(o).ok())
        .ok_or_else(|| format!("note {id} does not exist at commit {commit}"))?;
    let legacy = legacy_meta_at(&repo, &tree, &loc.path_of(&prefix, "index.json"), id);
    Ok(DocumentVersion {
        raw: String::from_utf8_lossy(body.content()).into_owned(),
        commit: found.id().to_string(),
        legacy,
    })
}

pub(crate) fn read_note_at_commit(
    loc: &NotesLocation,
    id: &str,
    commit: &str,
) -> Result<NoteAtCommit, String> {
    let version = document_at(loc, id, commit)?;
    let title = version
        .legacy
        .as_ref()
        .map(|meta| meta.title.clone())
        .or_else(|| {
            super::store::parse(id, &version.raw)
                .fields
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let content = if loc.legacy_notes && version.legacy.is_none() {
        super::store::split_document(&version.raw).1.into()
    } else {
        version.raw
    };
    Ok(NoteAtCommit {
        id: id.into(),
        commit: version.commit,
        title,
        content,
    })
}

pub(crate) fn note_document_at(
    loc: &HistoryLocation,
    id: &str,
    commit: &str,
) -> Result<String, String> {
    let version = document_at(loc, id, commit)?;
    match version.legacy {
        Some(meta) => super::notes::wrap_legacy_body(&version.raw, &meta),
        None => Ok(version.raw),
    }
}

pub(crate) fn read_note_body_at(
    loc: &HistoryLocation,
    id: &str,
    commit: &str,
) -> Result<NoteAtCommit, String> {
    let mut notes = loc.clone();
    notes.legacy_notes = true;
    read_note_at_commit(&notes, id, commit)
}

pub(crate) fn undo_note_body(
    loc: &HistoryLocation,
    id: &str,
    commit: &str,
    current: &str,
) -> Result<String, String> {
    let repo = git2::Repository::open(&loc.repo).map_err(git_err("open"))?;
    let found = find_commit(&repo, commit)?;
    let after = read_note_body_at(loc, id, commit)?.content;
    let path = loc.path_of(&loc.prefix()?, &format!("{id}.md"));
    let before = match found.parent(0) {
        Ok(parent) if blob_at(&parent.tree().map_err(git_err("read parent"))?, &path).is_some() => {
            read_note_body_at(loc, id, &parent.id().to_string())?.content
        }
        _ => String::new(),
    };
    merge_note_text(&after, current, &before, MergeOverlap::Refuse)?.ok_or_else(|| {
        "later changes overlap this revision; restore a whole version instead".into()
    })
}

pub(crate) fn read_content_at(
    loc: &HistoryLocation,
    id: &str,
    commit: &str,
) -> Result<(String, String), String> {
    let version = document_at(loc, id, commit)?;
    Ok((version.raw, version.commit))
}

pub(crate) fn undo_content(
    loc: &HistoryLocation,
    id: &str,
    commit: &str,
    current: &str,
) -> Result<String, String> {
    let repo = git2::Repository::open(&loc.repo).map_err(git_err("open"))?;
    let found = find_commit(&repo, commit)?;
    let path = loc.path_of(&loc.prefix()?, &format!("{id}.md"));
    let text = |tree: Option<git2::Tree>| {
        tree.and_then(|t| blob_at(&t, &path))
            .and_then(|o| repo.find_blob(o).ok())
            .map(|b| String::from_utf8_lossy(b.content()).into_owned())
            .unwrap_or_default()
    };
    let after = text(found.tree().ok());
    let before = text(found.parent(0).ok().and_then(|p| p.tree().ok()));
    let strip_attribution = |raw: &str| {
        crate::commands::frontmatter_fields::set_markdown_fields(
            raw,
            &[
                ("updatedAt".into(), serde_json::Value::Null),
                ("updatedBy".into(), serde_json::Value::Null),
            ],
        )
    };
    let (after, current, before) = (
        strip_attribution(&after)?,
        strip_attribution(current)?,
        strip_attribution(&before)?,
    );
    merge_note_text(&after, &current, &before, MergeOverlap::Refuse)?.ok_or_else(|| {
        "later changes overlap this revision; restore a whole version instead".into()
    })
}

/// Whether `actor`'s `kind` of change is committed at once, as its own revision
/// after the person's pending saves, rather than after their quiet period. A
/// restore or delete must not fold the typing before it into its own revision.
pub(crate) fn commits_at_once(actor: &NoteActor, kind: ChangeKind) -> bool {
    actor.kind == ActorKind::Agent || matches!(kind, ChangeKind::Restore | ChangeKind::Delete)
}

pub(crate) fn schedule_record_commit(loc: &HistoryLocation, quiet: Duration, kind: ChangeKind) {
    schedule_commit_with(loc, quiet, kind, |_| {});
}

fn schedule_commit_with(
    loc: &HistoryLocation,
    quiet: Duration,
    kind: ChangeKind,
    on_commit: impl FnOnce(Committed) + Send + 'static,
) {
    let generation = mark_human_pending(loc, kind);
    let loc = loc.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(quiet).await;
        match crate::commands::async_helpers::await_blocking("records.human", move || {
            flush_human_if_latest(&loc, generation)
        })
        .await
        {
            Ok(Some(committed)) => on_commit(committed),
            Ok(None) => {}
            Err(e) => log::warn!("record history commit failed: {e}"),
        }
    });
}

/// A person's saves waiting for their quiet period, by notes folder. The value
/// is the generation of the latest save, so only the last timer commits.
static PENDING_HUMAN: Mutex<Option<HashMap<PathBuf, (u64, NotesLocation, ChangeKind)>>> =
    Mutex::new(None);
static HUMAN_GENERATION: AtomicU64 = AtomicU64::new(0);

fn with_pending<T>(
    f: impl FnOnce(&mut HashMap<PathBuf, (u64, NotesLocation, ChangeKind)>) -> T,
) -> T {
    let mut guard = PENDING_HUMAN.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// Note that a person changed `loc`; returns the generation to commit at.
fn mark_human_pending(loc: &NotesLocation, kind: ChangeKind) -> u64 {
    let generation = HUMAN_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    with_pending(|map| map.insert(loc.notes_dir(), (generation, loc.clone(), kind)));
    generation
}

/// Commit whatever in `loc` is not in its history yet, credited to the person:
/// their saves still waiting for the quiet period, saves from before a restart,
/// and the notes as they were when history began. Agents change notes only
/// through the notes tools, which commit at once, so anything uncommitted is
/// the person's.
pub(crate) fn commit_outside_changes(
    _hold: &HistoryHold,
    loc: &NotesLocation,
) -> Result<Option<Committed>, String> {
    let pending = with_pending(|map| map.remove(&loc.notes_dir()));
    let summary = if has_history(loc) {
        "Edited by you"
    } else {
        "Notes history started"
    };
    commit_held(
        loc,
        &NoteActor::human(),
        pending.map_or(ChangeKind::Write, |(_, _, kind)| kind),
        summary,
        &[],
    )
}

/// Whether `loc`'s notes have been committed before.
fn has_history(loc: &NotesLocation) -> bool {
    let Ok(repo) = git2::Repository::open(&loc.repo) else {
        return false;
    };
    let Ok(tree) = repo.head().and_then(|h| h.peel_to_tree()) else {
        return false;
    };
    match loc.prefix() {
        Ok(prefix) if prefix.is_empty() => true,
        Ok(prefix) => tree.get_path(Path::new(&prefix)).is_ok(),
        Err(_) => false,
    }
}

/// Commit what the person changed in `loc` now, if anything.
#[cfg(test)]
pub(crate) fn flush_human(loc: &NotesLocation) -> Result<Option<String>, String> {
    let hold = hold_history();
    commit_outside_changes(&hold, loc).map(|c| c.map(|c| c.rev))
}

/// Commit `loc`'s pending saves only if no save came after `generation`.
fn flush_human_if_latest(
    loc: &NotesLocation,
    generation: u64,
) -> Result<Option<Committed>, String> {
    let hold = hold_history();
    let latest = with_pending(|map| match map.get(&loc.notes_dir()) {
        Some((g, _, _)) if *g == generation => true,
        _ => false,
    });
    if !latest {
        return Ok(None);
    }
    commit_outside_changes(&hold, loc)
}

/// A person saved `loc` (org `org`'s notes): commit it once they have been quiet
/// for a while, then tell open notes views which revision it became.
pub(crate) fn schedule_human_commit_kind<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    loc: &NotesLocation,
    org: Option<&str>,
    kind: ChangeKind,
) {
    let app = app.clone();
    let org = org.map(str::to_string);
    schedule_commit_with(loc, HUMAN_QUIET, kind, move |committed| {
        for id in &committed.ids {
            emit_notes_changed(
                &app,
                org.as_deref(),
                id,
                Some(&committed.rev),
                &NoteActor::human(),
                kind,
            );
        }
    });
}

/// An agent is about to write `loc`: hold the history so nothing else lands
/// until its change is committed, and first commit whatever is not committed
/// yet as the person's, so each revision is credited to whoever made it.
#[cfg(test)]
pub(crate) fn begin_agent_write(loc: &NotesLocation) -> HistoryHold {
    let hold = hold_history();
    if let Err(e) = commit_outside_changes(&hold, loc) {
        log::warn!(target: "scratch", "notes history commit failed: {e}");
    }
    hold
}

/// Bring note `id` back to its text at `commit`, as a new change by `actor`.
/// The current text is committed first, so the restore can itself be undone.
/// Returns the note as written and the restore's commit (`None` when the text
/// was already the same).
#[cfg(test)]
pub(crate) fn restore_note(
    loc: &NotesLocation,
    id: &str,
    commit: &str,
    actor: &NoteActor,
) -> Result<(Note, Option<String>), String> {
    let old = read_note_at_commit(loc, id, commit)?;
    let hold = hold_history();
    commit_outside_changes(&hold, loc)?;
    let (note, changed) = write_note_as(
        &loc.notes_dir(),
        id,
        &NoteEdit::Replace(old.content),
        None,
        actor,
    )?;
    if !changed {
        return Ok((note, None));
    }
    let short = &old.commit[..old.commit.len().min(8)];
    let sha = hold.commit(
        loc,
        actor,
        ChangeKind::Restore,
        &format!(
            "{} restored \u{201c}{}\u{201d} to {short}",
            actor.name, note.title
        ),
        &[("Restored-From", old.commit.clone())],
    )?;
    Ok((note, sha))
}

/// Take back only what revision `commit` did to note `id`, keeping every later
/// change, as a new change by `actor`. Refused when later changes rewrote the
/// same lines, since there is then no telling which text to keep; restoring a
/// whole version from history is the way out.
#[cfg(test)]
pub(crate) fn undo_change(
    loc: &NotesLocation,
    id: &str,
    commit: &str,
    actor: &NoteActor,
) -> Result<(Note, Option<String>), String> {
    if !loc.valid_id(id) {
        return Err(format!("invalid note id: {id}"));
    }
    let hold = hold_history();
    commit_outside_changes(&hold, loc)?;
    let (_, rev) = read_content_at(loc, id, commit)?;
    let current = std::fs::read_to_string(loc.notes_dir().join(format!("{id}.md")))
        .map_err(|e| e.to_string())?;
    let raw = undo_content(loc, id, commit, &current)?;
    let undone = super::store::split_document(&raw).1.to_string();
    let (note, changed) = write_note_as(
        &loc.notes_dir(),
        id,
        &NoteEdit::Replace(undone),
        None,
        actor,
    )?;
    if !changed {
        return Ok((note, None));
    }
    let short = &rev[..rev.len().min(8)];
    let sha = hold.commit(
        loc,
        actor,
        ChangeKind::Restore,
        &format!(
            "{} undid change {short} to \u{201c}{}\u{201d}",
            actor.name, note.title
        ),
        &[("Undid", rev.clone())],
    )?;
    Ok((note, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::scratch::{create_note_as, ensure_notes_migrated, read_note_at};
    use tempfile::tempdir;

    fn commit_notes(
        loc: &NotesLocation,
        actor: &NoteActor,
        change: ChangeKind,
        summary: &str,
        extra: &[(&str, String)],
    ) -> Result<Option<String>, String> {
        hold_history().commit(loc, actor, change, summary, extra)
    }

    fn agent() -> NoteActor {
        NoteActor::agent("Notes tidy", Some("pty-7".into()), Some("tidy".into()))
    }

    fn scratch_loc(home: &Path) -> NotesLocation {
        let me = crate::domain::Principal::from(42u64);
        let (space, _) =
            crate::context_repo::scratch_space::ensure_org_space(home, &me, "acme/ctx").unwrap();
        NotesLocation::in_scratch(
            space.root(),
            crate::context_repo::scratch_space::scratch_root(home, &me),
        )
    }

    #[test]
    fn an_agent_write_is_committed_with_its_actor_in_the_trailers() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let note = create_note_as(&loc.notes_dir(), "Plan", "one", false, &agent()).unwrap();
        let sha = commit_notes(&loc, &agent(), ChangeKind::Create, "created", &[])
            .unwrap()
            .expect("a commit");

        let repo = git2::Repository::open(&loc.repo).unwrap();
        let commit = repo
            .find_commit(git2::Oid::from_str(&sha).unwrap())
            .unwrap();
        let msg = commit.message().unwrap();
        assert!(msg.contains("Actor-Kind: agent"), "{msg}");
        assert!(msg.contains("Actor-Name: Notes tidy"), "{msg}");
        assert!(msg.contains("Actor-Session: pty-7"), "{msg}");
        assert!(msg.contains(&format!("Note-Id: {}", note.id)), "{msg}");
        // Only the notes folder is committed.
        let tree = commit.tree().unwrap();
        assert!(tree
            .get_path(Path::new("orgs/acme/ctx/notes/index.json"))
            .is_err());
        assert!(tree
            .get_path(Path::new(&format!("orgs/acme/ctx/notes/{}.md", note.id)))
            .is_ok());

        // Nothing changed → no commit.
        assert_eq!(
            commit_notes(&loc, &agent(), ChangeKind::Write, "again", &[]).unwrap(),
            None
        );
    }

    #[test]
    fn other_files_in_the_scratch_repo_are_left_out_of_notes_commits() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        std::fs::write(loc.repo.join("orgs/acme/ctx/content/other.md"), "x").unwrap();
        std::fs::write(loc.notes_dir().join("CLAUDE.md"), "scaffold").unwrap();
        let sha = commit_notes(&loc, &agent(), ChangeKind::Write, "s", &[])
            .unwrap()
            .unwrap();
        let repo = git2::Repository::open(&loc.repo).unwrap();
        let tree = repo
            .find_commit(git2::Oid::from_str(&sha).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        assert!(tree
            .get_path(Path::new("orgs/acme/ctx/content/other.md"))
            .is_err());
        assert!(tree
            .get_path(Path::new("orgs/acme/ctx/notes/CLAUDE.md"))
            .is_err());
    }

    #[test]
    fn history_lists_who_changed_a_note_and_restore_round_trips() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let nd = loc.notes_dir();
        let note = create_note_as(&nd, "Plan", "first\n", false, &NoteActor::human()).unwrap();
        mark_human_pending(&loc, ChangeKind::Write);
        let first = flush_human(&loc).unwrap().expect("human commit");

        let hold = begin_agent_write(&loc);
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Append("second".into()),
            None,
            &agent(),
        )
        .unwrap();
        hold.commit(&loc, &agent(), ChangeKind::Write, "appended", &[])
            .unwrap();
        drop(hold);

        let history = note_history(&loc, &note.id, 50).unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].actor, agent());
        assert_eq!((history[0].added, history[0].removed), (1, 0));
        assert_eq!(history[1].actor.kind, ActorKind::Human);
        assert_eq!(history[1].commit, first);

        let then = read_note_at_commit(&loc, &note.id, &first[..10]).unwrap();
        assert_eq!(then.content, "first\n");
        assert_eq!(then.title.as_deref(), Some("Plan"));

        let (restored, sha) = restore_note(&loc, &note.id, &first, &NoteActor::human()).unwrap();
        assert_eq!(restored.content, "first\n");
        assert!(sha.is_some());
        assert_eq!(read_note_at(&nd, &note.id).unwrap().content, "first\n");
        let history = note_history(&loc, &note.id, 50).unwrap();
        assert_eq!(history[0].change, "restore");
        assert_eq!(history[0].restored_from.as_deref(), Some(first.as_str()));
        assert_eq!(history[0].actor.kind, ActorKind::Human);
        // The agent's text is still in history: the restore can be undone.
        let agent_rev = history.iter().find(|r| r.actor == agent()).unwrap();
        assert_eq!(
            read_note_at_commit(&loc, &note.id, &agent_rev.commit)
                .unwrap()
                .content,
            "first\nsecond"
        );
    }

    #[test]
    fn a_stale_save_merges_with_an_agent_write_instead_of_replacing_it() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let nd = loc.notes_dir();
        let base = "one\ntwo\nthree\n";
        let note = create_note_as(&nd, "Plan", base, false, &NoteActor::human()).unwrap();
        // The agent edits the last line while the person's save of an edit to the
        // first line is still pending.
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Patch {
                find: "three".into(),
                replace: "THREE".into(),
            },
            None,
            &agent(),
        )
        .unwrap();
        let (saved, changed) = write_note_as(
            &nd,
            &note.id,
            &NoteEdit::ReplaceFrom {
                base: base.into(),
                content: "ONE\ntwo\nthree\n".into(),
            },
            None,
            &NoteActor::human(),
        )
        .unwrap();
        assert!(changed);
        assert_eq!(saved.content, "ONE\ntwo\nTHREE\n");
        // An unchanged disk body takes the save as it is.
        let (saved, _) = write_note_as(
            &nd,
            &note.id,
            &NoteEdit::ReplaceFrom {
                base: "ONE\ntwo\nTHREE\n".into(),
                content: "ONE\nmine\nTHREE\n".into(),
            },
            None,
            &NoteActor::human(),
        )
        .unwrap();
        assert_eq!(saved.content, "ONE\nmine\nTHREE\n");
        // The agent rewrites the line the person is still editing.
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Replace("ONE\ntheirs\nTHREE\n".into()),
            None,
            &agent(),
        )
        .unwrap();
        let (saved, _) = write_note_as(
            &nd,
            &note.id,
            &NoteEdit::ReplaceFrom {
                base: "ONE\nmine\nTHREE\n".into(),
                content: "ONE\nmine2\nTHREE\n".into(),
            },
            None,
            &NoteActor::human(),
        )
        .unwrap();
        assert!(saved.content.contains("theirs"), "{}", saved.content);
        assert!(saved.content.contains("mine2"), "{}", saved.content);
    }

    #[test]
    fn uncommitted_notes_are_credited_to_the_person_before_an_agent_commits() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let nd = loc.notes_dir();
        // Notes that predate history, and an edit saved just before a restart
        // (nothing pending in memory).
        let old = create_note_as(&nd, "Old", "kept\n", false, &NoteActor::human()).unwrap();
        let other = create_note_as(&nd, "Other", "x\n", false, &NoteActor::human()).unwrap();

        let hold = begin_agent_write(&loc);
        write_note_as(
            &nd,
            &other.id,
            &NoteEdit::Append("by agent".into()),
            None,
            &agent(),
        )
        .unwrap();
        hold.commit(&loc, &agent(), ChangeKind::Write, "edited", &[])
            .unwrap()
            .expect("agent commit");
        drop(hold);

        let untouched = note_history(&loc, &old.id, 50).unwrap();
        assert_eq!(untouched.len(), 1, "{untouched:?}");
        assert_eq!(untouched[0].actor.kind, ActorKind::Human);
        assert_eq!(untouched[0].summary, "Notes history started");
        let touched = note_history(&loc, &other.id, 50).unwrap();
        assert_eq!(touched[0].actor, agent());
        assert_eq!((touched[0].added, touched[0].removed), (1, 0));
        assert_eq!(touched[1].actor.kind, ActorKind::Human);

        // A later save the person made without a pending timer is theirs too.
        write_note_as(
            &nd,
            &old.id,
            &NoteEdit::Replace("kept\nmore\n".into()),
            None,
            &NoteActor::human(),
        )
        .unwrap();
        let (_, sha) = restore_note(&loc, &other.id, &touched[1].commit, &agent()).unwrap();
        assert!(sha.is_some());
        let old_history = note_history(&loc, &old.id, 50).unwrap();
        assert_eq!(
            old_history[0].actor.kind,
            ActorKind::Human,
            "{old_history:?}"
        );
        assert_eq!(old_history[0].summary, "Edited by you");
    }

    #[test]
    fn undoing_an_agent_change_keeps_what_the_person_typed_after_it() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let nd = loc.notes_dir();
        let note = create_note_as(&nd, "Plan", "a\nb\nc\n", false, &NoteActor::human()).unwrap();
        flush_human(&loc).unwrap();
        let hold = begin_agent_write(&loc);
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Patch {
                find: "a\n".into(),
                replace: "a\nagent line\n".into(),
            },
            None,
            &agent(),
        )
        .unwrap();
        let agent_rev = hold
            .commit(&loc, &agent(), ChangeKind::Write, "edited", &[])
            .unwrap()
            .unwrap();
        drop(hold);
        // The person keeps typing; the save is still waiting for its quiet period.
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Replace("a\nagent line\nb\nc\nmy line\n".into()),
            None,
            &NoteActor::human(),
        )
        .unwrap();
        mark_human_pending(&loc, ChangeKind::Write);

        let (undone, sha) = undo_change(&loc, &note.id, &agent_rev, &NoteActor::human()).unwrap();
        assert!(sha.is_some());
        assert_eq!(undone.content, "a\nb\nc\nmy line\n");
        let history = note_history(&loc, &note.id, 50).unwrap();
        assert_eq!(history[0].change, "restore");
        assert_eq!(history[1].actor.kind, ActorKind::Human, "{history:?}");

        // A later edit to the very lines the agent wrote: refused, nothing lost.
        let hold = begin_agent_write(&loc);
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Append("tail".into()),
            None,
            &agent(),
        )
        .unwrap();
        let rev = hold
            .commit(&loc, &agent(), ChangeKind::Write, "edited", &[])
            .unwrap()
            .unwrap();
        drop(hold);
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Patch {
                find: "tail".into(),
                replace: "tail, edited".into(),
            },
            None,
            &NoteActor::human(),
        )
        .unwrap();
        assert!(undo_change(&loc, &note.id, &rev, &NoteActor::human()).is_err());
        assert!(read_note_at(&nd, &note.id)
            .unwrap()
            .content
            .ends_with("tail, edited"));
    }

    #[test]
    fn a_person_save_waits_for_an_agent_write_and_its_commit() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let nd = loc.notes_dir();
        let note = create_note_as(&nd, "Plan", "one\n", false, &NoteActor::human()).unwrap();
        let hold = begin_agent_write(&loc);
        write_note_as(
            &nd,
            &note.id,
            &NoteEdit::Append("agent".into()),
            None,
            &agent(),
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let human = std::thread::spawn({
            let nd = nd.clone();
            let id = note.id.clone();
            move || {
                let _hold = hold_history();
                tx.send(()).unwrap();
                write_note_as(
                    &nd,
                    &id,
                    &NoteEdit::Append("mine".into()),
                    None,
                    &NoteActor::human(),
                )
                .unwrap();
            }
        });
        // The person's save can't land while the agent's change is uncommitted.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        let sha = hold
            .commit(&loc, &agent(), ChangeKind::Write, "edited", &[])
            .unwrap()
            .unwrap();
        drop(hold);
        human.join().unwrap();
        let repo = git2::Repository::open(&loc.repo).unwrap();
        let tree = repo
            .find_commit(git2::Oid::from_str(&sha).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        let body = repo
            .find_blob(
                tree.get_path(Path::new(&format!("orgs/acme/ctx/notes/{}.md", note.id)))
                    .unwrap()
                    .id(),
            )
            .unwrap();
        assert_eq!(
            super::super::store::split_document(std::str::from_utf8(body.content()).unwrap()).1,
            "one\nagent"
        );
    }

    #[test]
    fn a_later_save_supersedes_an_earlier_quiet_timer() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        let first = mark_human_pending(&loc, ChangeKind::Write);
        let second = mark_human_pending(&loc, ChangeKind::Write);
        assert_eq!(flush_human_if_latest(&loc, first).unwrap(), None);
        assert!(flush_human_if_latest(&loc, second).unwrap().is_some());
        // Nothing pending any more.
        assert_eq!(flush_human(&loc).unwrap(), None);
    }

    #[test]
    fn the_local_store_is_its_own_repo() {
        let tmp = tempdir().unwrap();
        let loc = NotesLocation::local(tmp.path().to_path_buf());
        ensure_notes_migrated(&loc.root).unwrap();
        let sha = commit_notes(&loc, &NoteActor::human(), ChangeKind::Write, "s", &[]).unwrap();
        assert!(sha.is_some());
        assert!(loc.notes_dir().join(".git").exists());
    }

    #[test]
    fn a_change_is_announced_with_its_org_note_actor_and_commit() {
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen = std::sync::Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let sink = std::sync::Arc::clone(&seen);
        app.listen(NOTES_CHANGED, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        emit_notes_changed(
            app.handle(),
            Some("acme/ctx"),
            "abc-1",
            Some("deadbeef"),
            &agent(),
            ChangeKind::Write,
        );
        emit_notes_changed(
            app.handle(),
            None,
            "abc-2",
            None,
            &NoteActor::human(),
            ChangeKind::Delete,
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0],
            serde_json::json!({
                "orgSlug": "acme/ctx",
                "id": "abc-1",
                "commit": "deadbeef",
                "actor": { "kind": "agent", "name": "Notes tidy", "sessionId": "pty-7", "handle": "tidy" },
                "kind": "write",
            })
        );
        assert_eq!(
            seen[1],
            serde_json::json!({
                "orgSlug": null,
                "id": "abc-2",
                "actor": { "kind": "human", "name": "You" },
                "kind": "delete",
            })
        );
    }

    #[test]
    fn bad_commit_ids_are_refused() {
        let tmp = tempdir().unwrap();
        let loc = scratch_loc(tmp.path());
        ensure_notes_migrated(&loc.root).unwrap();
        commit_notes(&loc, &NoteActor::human(), ChangeKind::Write, "s", &[]).unwrap();
        let id = crate::commands::scratch::read_index(&loc.notes_dir()).unwrap()[0]
            .id
            .clone();
        assert!(read_note_at_commit(&loc, &id, "HEAD~1").is_err());
        assert!(read_note_at_commit(&loc, &id, "../x").is_err());
    }
}
