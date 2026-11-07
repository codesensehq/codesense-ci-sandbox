// SPDX-License-Identifier: MIT
use super::edit::BodyEdit as NoteEdit;
use super::history::NoteActor;
use super::io::{atomic_write, generate_id as generate_note_id};
use crate::commands::scratch::load_notes_at;
use crate::context_repo::NOTES_DIR;
use log::{info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// Serializes migration, local recency and the one-time seed/reap operations.
static NOTES_LOCK: Mutex<()> = Mutex::new(());

/// Acquire `NOTES_LOCK`, tolerant of poisoning (a panic mid-mutation must not
/// wedge every future note operation).
fn lock_notes() -> std::sync::MutexGuard<'static, ()> {
    NOTES_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Per-note metadata (the `index.json` entries + `list_notes` return). Camel
/// case on the wire so the FE store consumes `lastOpenedAt` directly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub pinned: bool,
    pub last_opened_at: u64,
    /// When the body last changed (ms since the epoch); absent on notes written
    /// before change tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    /// Who last changed the body: the human, or an agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<NoteActor>,
}

/// A note's metadata + its markdown body (the `read_note` / `create_note`
/// return).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub title: String,
    pub pinned: bool,
    pub last_opened_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<NoteActor>,
    pub content: String,
}

impl Note {
    fn from_meta(meta: NoteMeta, content: String) -> Self {
        Self {
            id: meta.id,
            title: meta.title,
            pinned: meta.pinned,
            last_opened_at: meta.last_opened_at,
            updated_at: meta.updated_at,
            updated_by: meta.updated_by,
            content,
        }
    }

    /// The note's metadata without its body.
    pub fn meta(&self) -> NoteMeta {
        NoteMeta {
            id: self.id.clone(),
            title: self.title.clone(),
            pinned: self.pinned,
            last_opened_at: self.last_opened_at,
            updated_at: self.updated_at,
            updated_by: self.updated_by.clone(),
        }
    }
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Path-traversal guard: generated ids are hex + hyphen only, so reject
/// anything else — an id arrives from the FE and is interpolated into a file
/// path, so `read_note("../secret")` must not escape the notes dir.
pub(crate) fn is_valid_note_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub(crate) fn notes_dir(scratch_dir: &Path) -> PathBuf {
    scratch_dir.join(NOTES_DIR)
}

pub(crate) fn index_path(notes_dir: &Path) -> PathBuf {
    notes_dir.join("index.json")
}

pub(crate) fn note_body_path(notes_dir: &Path, id: &str) -> PathBuf {
    notes_dir.join(format!("{}.md", id))
}

pub(crate) fn read_index(nd: &Path) -> Result<Vec<NoteMeta>, String> {
    if !nd.exists() {
        return Ok(vec![]);
    }
    let recent = read_recency(nd)?;
    Ok(untracked_store(nd)?
        .list()?
        .into_iter()
        .map(|r| note_from_record(r, &recent).meta())
        .collect())
}

/// How many notes the notes store at `nd` holds (its `index.json` entries); 0
/// for a store that does not exist yet. Read-only.
pub(crate) fn note_count(nd: &Path) -> Result<usize, String> {
    read_index(nd).map(|metas| metas.len())
}

fn seed_metadata(nd: &Path, metas: &[NoteMeta]) -> Result<(), String> {
    let mut recent = read_recency(nd)?;
    for meta in metas {
        let path = super::schema::guarded_path(nd, Path::new(&format!("{}.md", meta.id)))?;
        let raw = fs::read_to_string(&path).unwrap_or_default();
        let next = wrap_legacy_body(&raw, meta)?;
        atomic_write(nd, &format!("{}.md", meta.id), next.as_bytes())?;
        recent.insert(meta.id.clone(), meta.last_opened_at);
    }
    write_recency(nd, &recent)
}

/// Recover metadata from existing bodies when a legacy sidecar cannot be read.
pub(crate) fn rebuild_index_from_bodies(nd: &Path) -> Result<Vec<NoteMeta>, String> {
    let mut metas = Vec::new();
    let Ok(entries) = fs::read_dir(nd) else {
        return Ok(metas);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".md") else {
            continue;
        };
        if !is_valid_note_id(id) {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        let content = fs::read_to_string(entry.path()).unwrap_or_default();
        let title = super::store::split_document(&content)
            .1
            .lines()
            .map(|l| l.trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(60).collect::<String>())
            .unwrap_or_else(|| "Recovered note".to_string());
        let last_opened_at = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or_else(now_millis);
        metas.push(NoteMeta {
            id: id.to_string(),
            title,
            pinned: false,
            last_opened_at,
            ..Default::default()
        });
    }
    Ok(metas)
}

/// The default title every seed path gives a note (fresh-install migration,
/// corrupt-index rebuild, delete-last reseed) and the marker SW-SN-13's reaper
/// uses to tell "untouched default" from "user-renamed". One source of truth so
/// the reaper's title-match can never drift from what the seeds actually write.
pub(crate) const DEFAULT_SCRATCH_TITLE: &str = "Note";

/// Upgrade the legacy sidecar once, or seed a new store from the single-file notes.
pub(crate) fn ensure_notes_migrated(root: &Path) -> Result<(), String> {
    let _guard = lock_notes();
    let nd = super::schema::guarded_path(root, Path::new("notes"))?;
    fs::create_dir_all(&nd).map_err(|e| e.to_string())?;
    if nd.join(".records-v1").exists() {
        return Ok(());
    }
    let idx = index_path(&nd);
    if idx.exists() {
        migrate_frontmatter(&nd)?;
    } else {
        let mut recovered = rebuild_index_from_bodies(&nd)?;
        if recovered.is_empty() {
            let body = load_notes_at(root).unwrap_or_default();
            let id = generate_note_id();
            atomic_write(&nd, &format!("{id}.md"), body.as_bytes())?;
            recovered.push(NoteMeta {
                id,
                title: DEFAULT_SCRATCH_TITLE.into(),
                last_opened_at: now_millis(),
                ..Default::default()
            });
        }
        seed_metadata(&nd, &recovered)?;
    }
    ensure_recency_ignored(&nd)?;
    atomic_write(&nd, ".records-v1", b"1\n")
}

// ===== SW-SN-4: delete-sticky pinned "Prompts" seed =====

/// SW-SN-4: sentinel recording that the "Prompts" note was already seeded.
/// It lives in the notes dir but is neither an `{id}.md` body nor `index.json`,
/// so it survives `delete_note` (which only removes the body + index entry) and
/// is ignored by `rebuild_index_from_bodies` and `sweep_notes_tmp_files`. That
/// survival is exactly what makes the seed **delete-sticky**: once it exists we
/// never re-create the note, so a user who deletes Prompts never sees it return.
pub(crate) const PROMPTS_SEED_SENTINEL: &str = ".prompts-seeded";

/// SW-SN-4 AC-2: the seed body. An ordinary markdown note demonstrating that
/// "a prompt = a sendable code block" — each fenced block is sendable via the
/// existing per-code-block send button (⌘↩), a selection + ⌘Enter, or SN-8's
/// selection popover. No new send mechanism; the demo IS a code block.
pub(crate) const PROMPTS_SEED_BODY: &str = r#"# Prompts

Keep your go-to prompts here as **code blocks**. Each block is sendable: hover it
and click the send button (⌘↩), or select any text and press ⌘Enter.

## Review the current diff

```
Review the current git diff for correctness bugs and obvious simplifications.
Call out anything that changes behavior. Keep it concise.
```

## Write the failing test first

```
Following TDD, write a single failing test for <behavior>, then the minimal
code to make it pass, then refactor. Show me the test before the implementation.
```

## Explain this file

```
Walk me through what this file does, its key responsibilities, and any
non-obvious edge cases. Reference specific functions.
```
"#;

/// SW-SN-4 AC-2: seed one pinned "Prompts" note, once, **delete-sticky**.
///
/// The `.prompts-seeded` sentinel is created with `create_new` — an atomic
/// "claim" so exactly one caller wins even across concurrent processes (mirrors
/// the migration's `fs::hard_link` TOCTOU guard). The sentinel is written
/// **before** the note so a crash mid-seed can never double-create; a present
/// sentinel (seeded before, or the user deleted Prompts) short-circuits.
///
/// Best-effort by design: every failure path is logged and swallowed so a seed
/// problem can never break `list_notes` (its only caller). Called from the boot
/// enumeration only — NOT from `ensure_notes_migrated` (which runs on every
/// mutation) — so it's attempted once at startup; the sentinel makes it
/// idempotent regardless. Takes `NOTES_LOCK`, so it must not be called from a
/// context that already holds it.
pub(crate) fn ensure_prompts_seeded(scratch_dir: &Path) {
    let nd = notes_dir(scratch_dir);
    let sentinel = nd.join(PROMPTS_SEED_SENTINEL);
    // Fast path (lock-free): `list_notes` runs this on every boot AND every
    // refresh/re-sync, but the seed only ever fires once. After the first run
    // the sentinel exists, so skip `NOTES_LOCK` entirely — the authoritative
    // `create_new` claim below still guards the first-seed race, so this is a
    // pure optimization, not a correctness shortcut.
    if sentinel.exists() {
        return;
    }
    let _guard = lock_notes();
    if let Err(e) = fs::create_dir_all(&nd) {
        warn!(target: "scratch", "prompts seed: could not create notes dir: {}", e);
        return;
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&sentinel)
    {
        Ok(_) => {}
        // Already claimed (prior seed, or the user deleted Prompts) → never resurrect.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(e) => {
            warn!(target: "scratch", "prompts seed: sentinel write failed: {}", e);
            return;
        }
    }
    match append_note_locked(&nd, "Prompts", true, PROMPTS_SEED_BODY) {
        Ok(note) => info!(target: "scratch", "seeded pinned Prompts note {}", note.id),
        Err(e) => {
            // The sentinel is claimed BEFORE the note write to keep the seed
            // single across processes. If the write then fails (transient I/O),
            // a stuck sentinel would suppress the seed forever via the fast path.
            // Release the claim so a later `list_notes` retries — the `create_new`
            // guard above still prevents a concurrent double-seed during a live
            // attempt, so this stays self-healing without risking a duplicate.
            warn!(target: "scratch", "prompts seed: note write failed, releasing sentinel for retry: {}", e);
            let _ = fs::remove_file(&sentinel);
        }
    }
}

// ---- notes-dir-scoped helpers (unit-tested against a tempdir) ----

#[cfg(test)]
pub(crate) fn read_note_at(nd: &Path, id: &str) -> Result<Note, String> {
    let recent = read_recency(nd)?;
    Ok(note_from_record(untracked_store(nd)?.read(id)?, &recent))
}

/// SW-SN-11: cap on a body-search snippet's length (chars). Scratch-note lines
/// are short; a pathologically long single line is windowed + capped char-safely
/// so the scan payload and the switcher row stay bounded.
pub(crate) const SNIPPET_MAX: usize = 200;

/// Lead-in chars kept before the match when a long line is windowed, so the
/// matched text isn't flush against the snippet's left edge.
pub(crate) const SNIPPET_LEAD: usize = 24;

/// Trim a matched line to a legible snippet: char-safe (never splits a UTF-8
/// codepoint), length-capped, and **windowed around the match** so the matched
/// text is always present. The windowing is load-bearing, not cosmetic: the FE
/// re-scores each body row's snippet against the query with its own fuzzy filter
/// and drops any row whose snippet no longer contains the query — so a
/// start-anchored cap would make a match past `SNIPPET_MAX` vanish from the
/// results entirely. `needle` is the already-lowercased query.
pub(crate) fn make_snippet(line: &str, needle: &str) -> String {
    let trimmed = line.trim();
    let orig: Vec<char> = trimmed.chars().collect();
    if orig.len() <= SNIPPET_MAX {
        return trimmed.to_string();
    }
    // Locate the match using the SAME lowercasing the line finder used
    // (`to_lowercase().contains`) so the two never disagree, then map the match
    // back to an EXACT `orig` char index. `to_lowercase()` can expand one char
    // into several (`İ` → `i̇`), so a char count in the lowered string would drift
    // from `orig`; instead build a per-lowered-char → orig-char-index table so the
    // anchor is precise regardless of expansion. A miss (impossible once the
    // finder matched) falls back to the head window.
    let lower = trimmed.to_lowercase();
    let mut lower_char_to_orig: Vec<usize> = Vec::with_capacity(lower.chars().count());
    for (orig_idx, ch) in orig.iter().enumerate() {
        for _ in ch.to_lowercase() {
            lower_char_to_orig.push(orig_idx);
        }
    }
    let anchor = lower
        .find(needle)
        .map(|byte_off| {
            let lower_char_idx = lower[..byte_off].chars().count();
            lower_char_to_orig.get(lower_char_idx).copied().unwrap_or(0)
        })
        .unwrap_or(0);
    let start = anchor.saturating_sub(SNIPPET_LEAD);
    let end = (start + SNIPPET_MAX).min(orig.len());
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(&orig[start..end]);
    if end < orig.len() {
        s.push('…');
    }
    s
}

/// Body-write + index-append tail shared by `create_note_at` and the SW-SN-4
/// "Prompts" seed. Mints a fresh id, atomically writes the `{id}.md` body, then
/// appends the metadata entry to `index.json` and returns the new note.
/// **Assumes the caller already holds `NOTES_LOCK`** (both callers take it) —
/// it must not lock again (the std mutex is not reentrant → deadlock).
fn append_note_locked(nd: &Path, title: &str, pinned: bool, content: &str) -> Result<Note, String> {
    append_note_locked_as(nd, title, pinned, content, None)
}

/// [`append_note_locked`], crediting the new note to `actor` when one is given.
fn append_note_locked_as(
    nd: &Path,
    title: &str,
    pinned: bool,
    content: &str,
    actor: Option<&NoteActor>,
) -> Result<Note, String> {
    fs::create_dir_all(nd).map_err(|e| e.to_string())?;
    let actor = actor.cloned().unwrap_or_else(NoteActor::human);
    let written = untracked_store(nd)?.create(
        None,
        &[
            ("title".into(), serde_json::json!(title)),
            ("pinned".into(), serde_json::json!(pinned)),
        ],
        content,
        &actor,
        None,
    )?;
    let mut recent = read_recency(nd)?;
    recent.insert(written.record.id.clone(), now_millis());
    write_recency(nd, &recent)?;
    Ok(note_from_record(written.record, &recent))
}

#[cfg(test)]
pub(crate) fn create_note_at(nd: &Path, title: &str) -> Result<Note, String> {
    create_note_as(nd, title, "", false, &NoteActor::human())
}

/// Create a note with `content`, credited to `actor`. A blank title becomes
/// "Untitled".
#[cfg(test)]
pub(crate) fn create_note_as(
    nd: &Path,
    title: &str,
    content: &str,
    pinned: bool,
    actor: &NoteActor,
) -> Result<Note, String> {
    let _guard = lock_notes();
    fs::create_dir_all(nd).map_err(|e| e.to_string())?;
    atomic_write(nd, ".records-v1", b"1\n")?;
    let title = if title.trim().is_empty() {
        "Untitled"
    } else {
        title
    };
    append_note_locked_as(nd, title, pinned, content, Some(actor))
}

/// A person replacing note `id`'s body (the tests' shorthand for `write_note_as`).
#[cfg(test)]
pub(crate) fn write_note_at(nd: &Path, id: &str, content: &str) -> Result<(), String> {
    write_note_as(
        nd,
        id,
        &NoteEdit::Replace(content.to_string()),
        None,
        &NoteActor::human(),
    )
    .map(|_| ())
}

/// Apply `edit` (and an optional new `title`) to note `id`, crediting `actor`.
/// Returns the note as written and whether anything changed: an edit that leaves
/// the body and title as they were writes nothing and keeps `updatedBy`.
#[cfg(test)]
pub(crate) fn write_note_as(
    nd: &Path,
    id: &str,
    edit: &NoteEdit,
    title: Option<&str>,
    actor: &NoteActor,
) -> Result<(Note, bool), String> {
    let fields = title
        .filter(|s| !s.trim().is_empty())
        .map(|s| vec![("title".into(), serde_json::json!(s))])
        .unwrap_or_default();
    let written = untracked_store(nd)?.write(id, edit, &fields, actor, None)?;
    Ok((
        note_from_record(written.record, &read_recency(nd)?),
        written.changed,
    ))
}

#[cfg(test)]
pub(crate) fn update_note_meta_at(
    nd: &Path,
    id: &str,
    title: Option<String>,
    pinned: Option<bool>,
    last_opened_at: Option<u64>,
) -> Result<NoteMeta, String> {
    let mut fields = vec![];
    if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        fields.push(("title".into(), serde_json::json!(t)));
    }
    if let Some(p) = pinned {
        fields.push(("pinned".into(), serde_json::json!(p)));
    }
    if !fields.is_empty() {
        untracked_store(nd)?.set_fields(id, &fields, &NoteActor::human(), None)?;
    }
    if let Some(at) = last_opened_at {
        touch(nd, id, at)?;
    }
    Ok(read_note_at(nd, id)?.meta())
}

/// SW-SN-13 AC-4: one-time, guarded cleanup of the empty duplicate "Note"
/// notes the SW-SN-12 null-active-note lazy-create minted on cold boot. Reaps a
/// note ONLY when EVERY guard holds: it is in `boot_ids` (the caller's snapshot of
/// the notes that existed at session start), its title is still the default
/// `"Note"`, it is not pinned, it is not the caller's active note (`keep_id`),
/// and its body is empty/whitespace. Never deletes a note with any body content or
/// a user-renamed title, and never leaves a zero-notes state — the sole survivor is
/// always kept (which also avoids tripping `delete_note_at`'s reseed).
///
/// `boot_ids` is the concurrency guard: a note created AFTER the snapshot (e.g. via
/// the panel's 3s editable-hatch lazy-create racing this reap) is not in the set,
/// so it is never reaped even if momentarily empty — closing the reap-vs-create
/// data-loss race. It also sharpens the AC-4 intent: reap only the fallout "already
/// on disk", never a note born this session.
///
/// One atomic locked pass: the keep-set is computed up front, reaped bodies are
/// removed, and the index is rewritten once. Idempotent — a clean store returns
/// unchanged with no write. An unreadable body is treated as "keep" (never delete
/// on uncertainty). A read: takes `lock_notes()` for the whole compute+write so it
/// can't interleave with a concurrent create/delete.
///
/// With `history`, the reap is a person's delete like any other: their pending
/// saves are committed first, then the reap as its own revision.
pub(crate) fn reap_empty_scratch_notes_at(
    nd: &Path,
    history: Option<&NotesLocation>,
    keep_id: Option<&str>,
    boot_ids: &[String],
) -> Result<Vec<NoteMeta>, String> {
    let history = history.map(|loc| (super::history::hold_history(), loc));
    let _guard = lock_notes();
    super::store::serialize_writes(|| {
        let boot: std::collections::HashSet<&str> = boot_ids.iter().map(|s| s.as_str()).collect();
        let mut keep: Vec<NoteMeta> = Vec::new();
        let mut reap: Vec<NoteMeta> = Vec::new();
        for m in read_index(nd)?.into_iter() {
            let candidate = boot.contains(m.id.as_str())
                && m.title == DEFAULT_SCRATCH_TITLE
                && !m.pinned
                && keep_id != Some(m.id.as_str());
            let body_empty = candidate
                && match fs::read_to_string(note_body_path(nd, &m.id)) {
                    Ok(c) => super::store::split_document(&c).1.trim().is_empty(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(_) => false, // unreadable → keep (never delete on uncertainty)
                };
            if body_empty {
                reap.push(m);
            } else {
                keep.push(m);
            }
        }
        // Idempotent no-op: nothing reapable → don't rewrite the index.
        if reap.is_empty() {
            return Ok(keep);
        }
        // Never a zero-notes state: if EVERY note was reapable, keep the most-recent
        // one as the sole survivor (mirrors `delete_note_at`'s never-zero invariant
        // without reseeding a brand-new note).
        if keep.is_empty() {
            reap.sort_by_key(|m| std::cmp::Reverse(m.last_opened_at));
            keep.push(reap.remove(0));
        }
        if let Some((hold, loc)) = &history {
            super::history::commit_outside_changes(hold, loc)?;
        }
        for m in &reap {
            let _ = fs::remove_file(note_body_path(nd, &m.id)); // best-effort
        }
        if let Some((hold, loc)) = &history {
            hold.commit(
                loc,
                &NoteActor::human(),
                super::history::ChangeKind::Delete,
                "Removed empty notes",
                &[],
            )?;
        }
        Ok(keep)
    })
}

#[cfg(test)]
pub(crate) fn delete_note_at(nd: &Path, id: &str) -> Result<Vec<NoteMeta>, String> {
    untracked_store(nd)?.delete(id, &NoteActor::human(), None)?;
    reseed_if_empty(nd)?;
    read_index(nd)
}

fn reseed_if_empty(nd: &Path) -> Result<(), String> {
    let _guard = lock_notes();
    if read_index(nd)?.is_empty() {
        append_note_locked(nd, DEFAULT_SCRATCH_TITLE, false, "")?;
    }
    Ok(())
}

use super::history::NotesLocation;
use super::{Collection, Record, Scope, Store, Written};
use crate::commands::frontmatter_fields::{set_markdown_fields, set_markdown_metadata};
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Recency = BTreeMap<String, u64>;
fn read_recency(nd: &Path) -> Result<Recency, String> {
    match fs::read(nd.join(".recency.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.to_string()),
    }
}
fn write_recency(nd: &Path, recent: &Recency) -> Result<(), String> {
    atomic_write(
        nd,
        ".recency.json",
        &serde_json::to_vec(recent).map_err(|e| e.to_string())?,
    )
}
fn ensure_recency_ignored(nd: &Path) -> Result<(), String> {
    let path = nd.join(".gitignore");
    let mut ignore = fs::read_to_string(path).unwrap_or_default();
    for name in [".recency.json", ".frontmatter-backup", ".records-v1"] {
        if !ignore.lines().any(|l| l == name) {
            if !ignore.is_empty() && !ignore.ends_with('\n') {
                ignore.push('\n');
            }
            ignore.push_str(name);
            ignore.push('\n');
        }
    }
    atomic_write(nd, ".gitignore", ignore.as_bytes())
}
fn metadata_document(raw: &str, meta: &NoteMeta) -> Result<String, String> {
    let mut next = set_markdown_fields(
        raw,
        &[
            ("title".into(), json!(meta.title)),
            ("pinned".into(), json!(meta.pinned)),
        ],
    )?;
    if let Some(at) = meta.updated_at {
        next = set_markdown_metadata(&next, "updatedAt", &json!(at))?;
    }
    if let Some(actor) = &meta.updated_by {
        next = set_markdown_metadata(&next, "updatedBy", &json!(actor))?;
    }
    Ok(next)
}

pub(super) fn wrap_legacy_body(body: &str, meta: &NoteMeta) -> Result<String, String> {
    Ok(format!(
        "{}{}",
        metadata_document("---\n---\n", meta)?,
        body
    ))
}

/// Keep the original folder until every replacement verifies. A retry always
/// derives its bytes from that backup, so a partial migration cannot prepend twice.
pub(crate) fn migrate_frontmatter(nd: &Path) -> Result<(), String> {
    let marker = nd.join(".frontmatter-backup");
    let backup = if marker.exists() {
        PathBuf::from(fs::read_to_string(&marker).map_err(|e| e.to_string())?)
    } else {
        let parent = nd.parent().ok_or("notes have no scope root")?;
        let backup = parent.join(format!(
            "notes-backup-{}-{}",
            now_millis(),
            generate_note_id()
        ));
        copy_folder(nd, &backup)?;
        atomic_write(
            nd,
            ".frontmatter-backup",
            backup.to_string_lossy().as_bytes(),
        )?;
        backup
    };
    let bytes = fs::read(backup.join("index.json")).map_err(|e| e.to_string())?;
    let metas: Vec<NoteMeta> = match serde_json::from_slice(&bytes) {
        Ok(metas) => metas,
        Err(_) => {
            fs::copy(backup.join("index.json"), nd.join("index.json.corrupt"))
                .map_err(|e| e.to_string())?;
            rebuild_index_from_bodies(nd)?
        }
    };
    let mut replacements = vec![];
    let mut recent = read_recency(nd)?;
    for meta in &metas {
        if !super::is_valid_id(&meta.id) {
            return Err(format!("invalid note id: {}", meta.id));
        }
        let filename = format!("{}.md", meta.id);
        super::schema::guarded_path(nd, Path::new(&filename))?;
        let raw = fs::read_to_string(backup.join(&filename))
            .map_err(|e| format!("cannot back up note {}: {e}", meta.id))?;
        let next = wrap_legacy_body(&raw, meta)?;
        if super::store::split_document(&next).1.as_bytes() != raw.as_bytes() {
            return Err(format!("note {} body changed during migration", meta.id));
        }
        replacements.push((filename, next));
        recent.insert(meta.id.clone(), meta.last_opened_at);
    }
    for (filename, next) in &replacements {
        atomic_write(nd, filename, next.as_bytes())?;
    }
    write_recency(nd, &recent)?;
    ensure_recency_ignored(nd)?;
    for (filename, next) in &replacements {
        if fs::read(nd.join(filename)).map_err(|e| e.to_string())? != next.as_bytes() {
            return Err(format!("migration verification failed: {filename}"));
        }
    }
    if metas.is_empty()
        && replacements.is_empty()
        && serde_json::from_slice::<Vec<NoteMeta>>(&bytes).is_err()
    {
        append_note_locked(nd, DEFAULT_SCRATCH_TITLE, false, "")?;
    }
    fs::remove_file(index_path(nd)).map_err(|e| e.to_string())?;
    Ok(())
}
fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir(to).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("cannot migrate a notes folder containing symlinks".into());
        }
        if kind.is_dir() {
            copy_folder(&entry.path(), &to.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
fn untracked_store(nd: &Path) -> Result<Store, String> {
    let root = nd.parent().ok_or("notes have no scope root")?;
    let mut collection = Collection::notes();
    collection.scope = Scope::Team;
    Store::new(root, collection, None)
}
pub(crate) fn store(loc: &NotesLocation) -> Result<Store, String> {
    ensure_notes_migrated(&loc.root)?;
    Store::new(&loc.root, Collection::notes(), Some(loc.repo.clone()))
}
fn note_from_record(record: Record, recent: &Recency) -> Note {
    let meta = NoteMeta {
        title: record
            .fields
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .into(),
        pinned: record
            .fields
            .get("pinned")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        last_opened_at: recent.get(&record.id).copied().unwrap_or(0),
        updated_at: record.fields.get("updatedAt").and_then(Value::as_u64),
        updated_by: record
            .fields
            .get("updatedBy")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        id: record.id,
    };
    Note::from_meta(meta, record.body)
}
pub(crate) fn touch(nd: &Path, id: &str, at: u64) -> Result<(), String> {
    let _guard = lock_notes();
    untracked_store(nd)?.read(id)?;
    let mut recent = read_recency(nd)?;
    recent.insert(id.into(), at);
    ensure_recency_ignored(nd)?;
    write_recency(nd, &recent)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteWritten {
    pub org: String,
    pub note: Note,
    pub changed: bool,
    pub commit: Option<String>,
    pub merged: bool,
    pub overlaps: Vec<super::Overlap>,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteRow {
    #[serde(flatten)]
    pub meta: NoteMeta,
    pub preview: String,
    /// The agents the note's lines were handed to, as `{handle, name}`.
    pub refs: Vec<Value>,
}
impl std::ops::Deref for NoteRow {
    type Target = NoteMeta;
    fn deref(&self) -> &NoteMeta {
        &self.meta
    }
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteHit {
    pub id: String,
    pub title: String,
    pub matched: String,
    pub snippet: Option<String>,
}

pub(crate) fn written(loc: &NotesLocation, result: Written) -> Result<NoteWritten, String> {
    Ok(NoteWritten {
        org: loc
            .root
            .strip_prefix(&loc.repo)
            .ok()
            .and_then(|p| p.strip_prefix("orgs").ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        note: note_from_record(result.record, &read_recency(&loc.notes_dir())?),
        changed: result.changed,
        commit: result.commit,
        merged: result.merged,
        overlaps: result.overlaps,
    })
}
pub(crate) fn list(loc: &NotesLocation) -> Result<Vec<NoteRow>, String> {
    let store = store(loc)?;
    let records = store.list()?;
    let recent = read_recency(&loc.notes_dir())?;
    let mut notes: Vec<_> = records
        .into_iter()
        .map(|record| {
            let (preview, refs) = super::projection::summary(&record.body);
            NoteRow {
                preview,
                refs,
                meta: note_from_record(record, &recent).meta(),
            }
        })
        .collect();
    notes.sort_by_key(|n| {
        (
            std::cmp::Reverse(n.pinned),
            std::cmp::Reverse(n.updated_at.unwrap_or(0).max(n.last_opened_at)),
        )
    });
    Ok(notes)
}
pub(crate) fn read(loc: &NotesLocation, id: &str) -> Result<Note, String> {
    let result = store(loc)?.read(id)?;
    Ok(note_from_record(result, &read_recency(&loc.notes_dir())?))
}
pub(crate) fn create(
    loc: &NotesLocation,
    title: &str,
    body: &str,
    pinned: bool,
    actor: &NoteActor,
    request_id: Option<&str>,
) -> Result<NoteWritten, String> {
    let title = if title.trim().is_empty() {
        "Untitled"
    } else {
        title
    };
    let result = store(loc)?.create(
        None,
        &[
            ("title".into(), json!(title)),
            ("pinned".into(), json!(pinned)),
        ],
        body,
        actor,
        request_id,
    )?;
    {
        let _guard = lock_notes();
        let mut recent = read_recency(&loc.notes_dir())?;
        if !recent.contains_key(&result.record.id) {
            let at = result
                .record
                .fields
                .get("updatedAt")
                .and_then(Value::as_u64)
                .unwrap_or_else(now_millis);
            recent.insert(result.record.id.clone(), at);
            write_recency(&loc.notes_dir(), &recent)?;
        }
    }
    written(loc, result)
}
pub(crate) fn write(
    loc: &NotesLocation,
    id: &str,
    edit: &NoteEdit,
    title: Option<&str>,
    pinned: Option<bool>,
    actor: &NoteActor,
    request_id: Option<&str>,
) -> Result<NoteWritten, String> {
    written(
        loc,
        store(loc)?.write(id, edit, &metadata_fields(title, pinned), actor, request_id)?,
    )
}
fn metadata_fields(title: Option<&str>, pinned: Option<bool>) -> Vec<(String, Value)> {
    let mut fields = vec![];
    if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
        fields.push(("title".into(), json!(t)));
    }
    if let Some(p) = pinned {
        fields.push(("pinned".into(), json!(p)));
    }
    fields
}
pub(crate) fn update_meta(
    loc: &NotesLocation,
    id: &str,
    title: Option<&str>,
    pinned: Option<bool>,
) -> Result<NoteWritten, String> {
    written(
        loc,
        store(loc)?.update_metadata(id, &metadata_fields(title, pinned), &NoteActor::human())?,
    )
}
pub(crate) fn delete(
    loc: &NotesLocation,
    id: &str,
    request_id: Option<&str>,
) -> Result<NoteWritten, String> {
    let result = store(loc)?.delete(id, &NoteActor::human(), request_id)?;
    reseed_if_empty(&loc.notes_dir())?;
    written(loc, result)
}
pub(crate) fn restore(
    loc: &NotesLocation,
    id: &str,
    commit: &str,
    undo: bool,
    actor: &NoteActor,
    request_id: Option<&str>,
) -> Result<NoteWritten, String> {
    written(
        loc,
        store(loc)?.restore_body(id, commit, undo, actor, request_id)?,
    )
}
pub(crate) fn search(
    loc: &NotesLocation,
    query: &str,
    limit: usize,
    index: Option<crate::context_query::ContextIndexService>,
) -> Result<Vec<NoteHit>, String> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let store = store(loc)?;
    let store = match index {
        Some(index) => store.with_index(index),
        None => store,
    };
    let hits = store.search(query, usize::MAX)?;
    let metadata: std::collections::HashMap<_, _> = read_index(&loc.notes_dir())?
        .into_iter()
        .map(|meta| (meta.id.clone(), meta))
        .collect();
    let mut hits = hits;
    hits.sort_by_key(|hit| {
        let meta = metadata.get(&hit.id);
        (
            hit.matched == "body",
            std::cmp::Reverse(meta.is_some_and(|m| m.pinned)),
            std::cmp::Reverse(meta.map_or(0, |m| m.updated_at.unwrap_or(0).max(m.last_opened_at))),
        )
    });
    hits.truncate(limit);
    Ok(hits
        .into_iter()
        .map(|h| NoteHit {
            id: h.id,
            title: h.title.unwrap_or_default(),
            matched: if h.matched == "body" {
                "text".into()
            } else {
                h.matched
            },
            snippet: Some(h.snippet),
        })
        .collect())
}

pub(crate) fn read_at(
    loc: &NotesLocation,
    id: &str,
    commit: &str,
) -> Result<super::history::NoteAtCommit, String> {
    store(loc)?.history(id, 0)?;
    super::history::read_note_body_at(loc, id, commit)
}

pub(crate) fn prepare_list(loc: &NotesLocation, options: &super::Options) -> Result<(), String> {
    let root = loc.root.as_path();
    static REAPED: std::sync::LazyLock<Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
    ensure_notes_migrated(root)?;
    if options.seed.iter().any(|s| s == "Prompts") {
        ensure_prompts_seeded(root);
    }
    if !options.reap_empty {
        return Ok(());
    }
    let mut reaped = REAPED.lock().unwrap_or_else(|e| e.into_inner());
    if reaped.contains(root) {
        return Ok(());
    }
    let nd = notes_dir(root);
    let boot = read_index(&nd)?;
    let keep = boot
        .iter()
        .filter(|m| m.title == DEFAULT_SCRATCH_TITLE && !m.pinned)
        .max_by_key(|m| m.last_opened_at)
        .map(|m| m.id.as_str());
    reap_empty_scratch_notes_at(
        &nd,
        Some(loc),
        keep,
        &boot.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
    )?;
    reaped.insert(root.into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_note_records_recency_without_a_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
        let id = create(&loc, "Plan", "body", false, &NoteActor::human(), None)
            .unwrap()
            .note
            .id;
        let commits = |id: &str| -> Vec<String> {
            store(&loc)
                .unwrap()
                .history(id, 20)
                .unwrap()
                .into_iter()
                .map(|r| r.commit)
                .collect()
        };
        let before = commits(&id);

        // `update_note_meta` with only `lastOpenedAt`: no tracked fields, then `touch`.
        let result = update_meta(&loc, &id, None, None).unwrap();
        touch(&loc.notes_dir(), &id, 4242).unwrap();

        assert!(!result.changed);
        assert_eq!(result.commit, None);
        assert_eq!(read(&loc, &id).unwrap().last_opened_at, 4242);
        assert_eq!(commits(&id), before);
    }
}
