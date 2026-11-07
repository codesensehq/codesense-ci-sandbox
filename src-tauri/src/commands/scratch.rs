// SPDX-License-Identifier: MIT
pub(crate) use crate::records::edit::{
    merge_text as merge_note_text, BodyEdit as NoteEdit, MergeOverlap,
};
#[cfg(test)]
use crate::records::io::{atomic_write, generate_id as generate_note_id, sweep_notes_tmp_files};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use log::{debug, info};

/// R3-P7: per-invocation tmp suffix counter. Shared `notes.md.tmp` would
/// race under concurrent saves (write1 → write2 → rename1 moves write2's
/// payload → rename2 fails because tmp is gone, surfacing a Tauri command
/// failure even though disk content is correct). Monotonic counter + PID
/// gives a unique tmp path per save call.
static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);

use super::async_helpers::await_blocking;
use super::notes_history::{
    self as history, emit_notes_changed, ChangeKind, NoteActor, NotesLocation,
};
use super::paths::codesense_home_dir;

/// Returns the exe-hash-namespaced scratch directory:
/// `{codesense_home}/scratch-notes/{exe_hash[..8]}`. The exe-hash is
/// derived from `std::env::current_exe()` so dev (`target/debug/codesense`)
/// and release (`/Applications/CodeSense.app/.../codesense`) installs get
/// distinct directories (F-6 AC-15 preserves the W-1 isolation behavior).
fn scratch_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("failed to get current exe: {}", e))?;
    let exe_hash = crate::shared::hash::sha256_hex(exe.to_string_lossy().as_bytes());

    let dir = codesense_home_dir()
        .ok_or("could not determine home directory")?
        .join("scratch-notes")
        .join(&exe_hash[..8]);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create notes dir: {}", e))?;
    Ok(dir)
}

/// F-6 AC-9: single notes file. `notes.md` under the exe-hash directory.
fn notes_file_path(scratch_dir: &Path) -> PathBuf {
    scratch_dir.join("notes.md")
}

/// F-6 AC-14: identify legacy per-repo scratch files.
///
/// Legacy filename format (W-1 era): `{sanitized-slug}-{8 hex chars}.md`.
/// We match anything ending with `-<8 hex>.md` to be tolerant of varied
/// slugs (sanitize_name historically allowed mixed hyphen/underscore). The
/// global `notes.md` itself doesn't match (no `-<8hex>` suffix).
fn looks_like_legacy_note(filename: &str) -> bool {
    if !filename.ends_with(".md") {
        return false;
    }
    // P6 patch: do all byte-arithmetic via ASCII-only check first. The
    // legacy filename format is `<sanitized-slug>-<8 ASCII hex>.md` — all
    // ASCII by construction (sanitize_name strips non-ASCII before
    // hashing). Rejecting non-ASCII upfront prevents a panic when
    // `split_at(stem.len() - 9)` lands mid-UTF-8 codepoint for adversarial
    // filenames that happen to live in scratch_dir.
    if !filename.is_ascii() {
        return false;
    }
    let stem = &filename[..filename.len() - 3];
    // Need at least 1-char slug + "-" + 8 hex chars, so stem length >= 10.
    if stem.len() < 10 {
        return false;
    }
    // Split structure: <slug>-<8 hex>. The 9-char suffix is "-<8hex>".
    let (slug, hash_part) = stem.split_at(stem.len() - 9);
    // Slug must be non-empty (filters out things like "-12345678.md").
    if slug.is_empty() {
        return false;
    }
    // hash_part must be exactly "-" followed by 8 hex chars.
    if !hash_part.starts_with('-') {
        return false;
    }
    hash_part[1..].chars().all(|c| c.is_ascii_hexdigit())
}

struct LegacyCandidate {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

/// F-6 AC-14: find legacy notes in `scratch_dir`, **sorted newest-first**,
/// for seeding the global `notes.md`.
///
/// P6 patch: returns a sorted Vec rather than a single best so the caller
/// can retry on a read failure. If the newest file happens to be unreadable
/// (permissions / disk error), the caller falls back to the next-newest.
///
/// Returns `Ok(Vec::new())` if no candidate qualifies (zero legacy files,
/// or all are empty). On filesystem errors during scan, individual files
/// are skipped (logged at debug) — never propagates a per-file failure.
fn find_legacy_notes_newest_first(scratch_dir: &Path) -> Result<Vec<LegacyCandidate>, String> {
    let entries = match fs::read_dir(scratch_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read notes dir: {}", e)),
    };

    let mut candidates: Vec<LegacyCandidate> = Vec::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name == "notes.md" {
            continue;
        }
        if !looks_like_legacy_note(&file_name) {
            continue;
        }
        // R5A-P2: use `symlink_metadata()` so we DON'T follow symlinks. A
        // symlink in the scratch dir whose name matches the legacy pattern
        // (sync-tool artifact, dotfile-sync collision, HFS+ alias) would
        // otherwise be followed by `entry.metadata()` and its target read
        // verbatim into `notes.md` — potentially OOM-ing on a huge target
        // or seeding the user's notes with content from an unrelated file.
        let metadata = match entry.path().symlink_metadata() {
            Ok(m) => m,
            Err(e) => {
                debug!(target: "scratch", "skip {} — symlink_metadata failed: {}", file_name, e);
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            debug!(target: "scratch", "skip {} — symlink (not following)", file_name);
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() == 0 {
            continue; // Skip empty files per AC-14.
        }
        let modified = match metadata.modified() {
            Ok(m) => m,
            Err(e) => {
                debug!(target: "scratch", "skip {} — modified() failed: {}", file_name, e);
                continue;
            }
        };

        candidates.push(LegacyCandidate {
            path: entry.path(),
            modified,
            size: metadata.len(),
        });
    }

    // Sort newest-first (stable sort preserves read_dir order for ties).
    candidates.sort_by_key(|c| std::cmp::Reverse(c.modified));
    Ok(candidates)
}

/// F-6 AC-14: ensure `notes.md` exists. If absent, seed from the newest
/// non-empty legacy file (non-destructive — legacy files stay in place).
/// Returns the path to `notes.md` (which may or may not exist on disk
/// after this call — when there's no legacy to seed from, we leave it
/// absent and `load_notes_at` returns empty string).
fn ensure_notes_seeded(scratch_dir: &Path) -> Result<(), String> {
    let notes = notes_file_path(scratch_dir);
    if notes.exists() {
        return Ok(()); // Already migrated or freshly created.
    }
    // P6 patch: walk candidates newest-first; skip any that fail to read so
    // a single unreadable file (permissions, disk error) doesn't block the
    // whole load.
    let candidates = find_legacy_notes_newest_first(scratch_dir)?;
    for candidate in &candidates {
        let content = match fs::read_to_string(&candidate.path) {
            Ok(c) => c,
            Err(e) => {
                debug!(
                    target: "scratch",
                    "skip legacy {} — read failed: {}",
                    candidate.path.display(),
                    e
                );
                continue;
            }
        };
        // R5A-P1: TOCTOU-safe seed write. The previous approach called
        // `save_notes_at` which does atomic-rename-OVERWRITE — that would
        // clobber any `notes.md` that appeared between the `notes.exists()`
        // check above and the write below. Concrete scenario: first launch
        // after F-6 upgrade, the panel mounts and fires `loadScratchNote`
        // (migration starts scanning legacy files), user types
        // immediately, debounced `save_scratch_note` (1s later) creates
        // `notes.md` with the user's text. If the still-running migration
        // then writes its legacy seed, the user's content is silently
        // wiped.
        //
        // Fix: write content to a per-invocation tmp file, then
        // `fs::hard_link` it into place. The hard_link fails atomically
        // with `AlreadyExists` if another writer (user save / concurrent
        // migration) created `notes.md` in the interim — we accept their
        // content and discard the legacy seed. The tmp file is unlinked
        // either way; orphaned tmp on power-loss between write and
        // hard_link will be reaped by `sweep_cross_pid_tmp_files` on the
        // next save.
        let counter = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_path = scratch_dir.join(format!("notes.md.tmp.{}.{}", std::process::id(), counter));
        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(format!("failed to write migration tmp: {}", e));
        }
        match fs::hard_link(&tmp_path, &notes) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp_path);
                info!(
                    target: "scratch",
                    "migrated {} bytes from legacy note {}",
                    candidate.size,
                    candidate.path.display()
                );
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp_path);
                info!(
                    target: "scratch",
                    "notes.md appeared concurrently — skipping migration seed (winner's content preserved)"
                );
                return Ok(());
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!("failed to hard-link migration seed: {}", e));
            }
        }
    }
    // No readable candidate — leave notes.md absent (load_notes_at will
    // return empty string).
    Ok(())
}

/// Pure helper for tests: load `notes.md` from `scratch_dir`, seeding from
/// legacy on first call if needed.
pub(crate) fn load_notes_at(scratch_dir: &Path) -> Result<String, String> {
    ensure_notes_seeded(scratch_dir)?;
    let path = notes_file_path(scratch_dir);
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&path).map_err(|e| format!("failed to read note: {}", e))
}

// ===================================================================
// SW-SN-1: plural notes model
// ===================================================================
//
// Storage layout under `{scratch_dir}/notes/`:
// Metadata and bodies are stored by the shared records module.

pub(crate) use crate::records::notes::*;

// ===================================================================
// SLW-PT-19: notes live per ORG in the scratch space
// ===================================================================

/// The notes **root** — the dir whose `notes/` subdir holds the plural store —
/// for the org the CALLER names: `scratch/orgs/<org>/` of the selected local
/// profile, created on first use. One set of notes per org; the org is captured
/// by the caller when the edit or read was made and never resolved here from
/// whatever org is active at call time, so a save that lands after an org switch
/// still goes to its own org. The org must be synced on this Mac. With no org
/// (or the FE's logged-out sentinel `0`) it is the machine-local
/// `{codesense_home}/scratch-notes/{exe_hash}/` fallback, so a note is never
/// lost. Returning the ROOT lets every command keep using `notes_dir(root)` /
/// `ensure_notes_migrated(root)` unchanged.
async fn resolve_notes_root(
    app: &tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
) -> Result<PathBuf, String> {
    resolve_notes_location(app, github_user_id, org_slug)
        .await
        .map(|loc| loc.root)
}

/// [`resolve_notes_root`], with the git repo the notes' history is kept in.
async fn resolve_notes_location(
    app: &tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
) -> Result<NotesLocation, String> {
    match org_slug.filter(|_| github_user_id != 0) {
        Some(org) => {
            let (home, user_id) = super::route_to_context::resolve_ambient(app).await?;
            await_blocking("resolve_notes_root", move || {
                org_notes_location(&home, user_id, &org)
            })
            .await
        }
        None => {
            await_blocking("resolve_notes_root.local", || {
                scratch_dir().map(NotesLocation::local)
            })
            .await
        }
    }
}

/// Org `org`'s notes for profile `user_id`, refused when the org is not synced on
/// this Mac. Blocking.
pub(crate) fn org_notes_location(
    home: &Path,
    user_id: u64,
    org: &str,
) -> Result<NotesLocation, String> {
    let synced = super::route_to_context::synced_contexts_in(home, user_id)?
        .iter()
        .any(|c| c.org_slug == org);
    if !synced {
        return Err(format!("org {org:?} is not synced on this Mac"));
    }
    org_notes_location_unchecked(home, &crate::domain::Principal::from(user_id), org)
}

/// Org `org`'s notes location, created on first use, without the synced check.
pub(crate) fn org_notes_location_unchecked(
    home: &Path,
    principal: &crate::domain::Principal,
    org: &str,
) -> Result<NotesLocation, String> {
    let root = scratch_notes_root(home, principal, org)?;
    let repo = crate::context_repo::scratch_space::scratch_root(home, principal);
    Ok(NotesLocation::in_scratch(root, repo))
}

/// `org_slug`'s scratch-space notes root (created on first use). Pure
/// `codesense_home`-parameterized core of [`resolve_notes_root`].
fn scratch_notes_root(
    codesense_home: &Path,
    principal: &crate::domain::Principal,
    org_slug: &str,
) -> Result<PathBuf, String> {
    let (space, _created) =
        crate::context_repo::scratch_space::ensure_org_space(codesense_home, principal, org_slug)?;
    let root = space.root();
    debug_assert_eq!(notes_dir(&root), space.notes_dir());
    Ok(root)
}

// ---- Tauri commands ----

use crate::records::notes as records_notes;

#[tauri::command]
pub async fn list_notes(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
) -> Result<Vec<records_notes::NoteRow>, String> {
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    await_blocking("list_notes", move || records_notes::list(&loc)).await
}
#[tauri::command]
pub async fn read_note(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
) -> Result<Note, String> {
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    await_blocking("read_note", move || records_notes::read(&loc, &id)).await
}
#[tauri::command]
pub async fn search_notes(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    query: String,
) -> Result<Vec<records_notes::NoteHit>, String> {
    use tauri::Manager;
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let index = app
        .try_state::<crate::context_query::ContextIndexService>()
        .map(|s| s.inner().clone());
    await_blocking("search_notes", move || {
        records_notes::search(&loc, &query, 200, index)
    })
    .await
}
#[tauri::command]
pub async fn create_note(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    title: String,
    content: Option<String>,
    pinned: Option<bool>,
    request_id: Option<String>,
) -> Result<records_notes::NoteWritten, String> {
    let org = org_slug.clone();
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let result = await_blocking("create_note", {
        let loc = loc.clone();
        move || {
            records_notes::create(
                &loc,
                &title,
                content.as_deref().unwrap_or(""),
                pinned.unwrap_or(false),
                &NoteActor::human(),
                request_id.as_deref(),
            )
        }
    })
    .await?;
    notify_written(&app, &loc, org.as_deref(), &result, ChangeKind::Create);
    Ok(result)
}
fn notify_written(
    app: &tauri::AppHandle,
    loc: &NotesLocation,
    org: Option<&str>,
    result: &records_notes::NoteWritten,
    kind: ChangeKind,
) {
    if !result.changed {
        return;
    }
    let path = loc.notes_dir().join(format!("{}.md", result.note.id));
    super::frontmatter_editor::note_written(app, &path.to_string_lossy(), &path);
    if result.commit.is_none() {
        history::schedule_human_commit_kind(app, &loc.for_records(), org, kind);
    }
    emit_notes_changed(
        app,
        org,
        &result.note.id,
        result.commit.as_deref(),
        &NoteActor::human(),
        kind,
    );
}
#[tauri::command]
pub async fn write_note(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    content: Option<String>,
    base: Option<String>,
    append: Option<String>,
    patch: Option<crate::records::Patch>,
    pinned: Option<bool>,
    title: Option<String>,
    request_id: Option<String>,
) -> Result<records_notes::NoteWritten, String> {
    let org = org_slug.clone();
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let edit = crate::records::edit::requested_edit(
        content.as_deref(),
        base.as_deref(),
        append.as_deref(),
        patch.as_ref(),
        title.is_some() || pinned.is_some(),
    )?;
    let result = await_blocking("write_note", {
        let loc = loc.clone();
        move || {
            write_note_in(
                &loc,
                &id,
                &edit,
                title.as_deref(),
                pinned,
                request_id.as_deref(),
            )
        }
    })
    .await?;
    notify_written(&app, &loc, org.as_deref(), &result, ChangeKind::Write);
    Ok(result)
}
pub(crate) fn write_note_in(
    loc: &NotesLocation,
    id: &str,
    edit: &NoteEdit,
    title: Option<&str>,
    pinned: Option<bool>,
    request_id: Option<&str>,
) -> Result<records_notes::NoteWritten, String> {
    records_notes::write(
        loc,
        id,
        edit,
        title,
        pinned,
        &NoteActor::human(),
        request_id,
    )
}

#[tauri::command]
pub async fn update_note_meta(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    title: Option<String>,
    pinned: Option<bool>,
    last_opened_at: Option<u64>,
) -> Result<records_notes::NoteWritten, String> {
    let org = org_slug.clone();
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let result = await_blocking("update_note_meta", {
        let loc = loc.clone();
        move || {
            let mut result = records_notes::update_meta(&loc, &id, title.as_deref(), pinned)?;
            if let Some(at) = last_opened_at {
                records_notes::touch(&loc.notes_dir(), &id, at)?;
                result.note = records_notes::read(&loc, &id)?;
            }
            Ok(result)
        }
    })
    .await?;
    notify_written(&app, &loc, org.as_deref(), &result, ChangeKind::Meta);
    Ok(result)
}
#[tauri::command]
pub async fn delete_note(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    request_id: Option<String>,
) -> Result<records_notes::NoteWritten, String> {
    let org = org_slug.clone();
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let result = await_blocking("delete_note", {
        let loc = loc.clone();
        move || records_notes::delete(&loc, &id, request_id.as_deref())
    })
    .await?;
    notify_written(&app, &loc, org.as_deref(), &result, ChangeKind::Delete);
    Ok(result)
}
#[tauri::command]
pub async fn notes_history(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    limit: Option<usize>,
) -> Result<Vec<history::NoteRevision>, String> {
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    await_blocking("notes_history", move || {
        records_notes::store(&loc)?.history(&id, limit.unwrap_or(50).clamp(1, 500))
    })
    .await
}
#[tauri::command]
pub async fn notes_read_at(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    commit: String,
) -> Result<history::NoteAtCommit, String> {
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    await_blocking("notes_read_at", move || {
        records_notes::read_at(&loc, &id, &commit)
    })
    .await
}
#[tauri::command]
pub async fn notes_restore(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    id: String,
    commit: String,
    undo: Option<bool>,
    request_id: Option<String>,
) -> Result<records_notes::NoteWritten, String> {
    let org = org_slug.clone();
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    let result = await_blocking("notes_restore", {
        let loc = loc.clone();
        move || {
            records_notes::restore(
                &loc,
                &id,
                &commit,
                undo.unwrap_or(false),
                &NoteActor::human(),
                request_id.as_deref(),
            )
        }
    })
    .await?;
    notify_written(&app, &loc, org.as_deref(), &result, ChangeKind::Restore);
    Ok(result)
}

/// SW-SN-13 AC-4: thin wrapper — resolve dir + migrate, delegate to the reaper.
/// `active_id` is the FE's current active note, preserved from reaping. `boot_ids`
/// is the FE's snapshot of the note ids present at session start — only those are
/// reap-eligible, so a note created concurrently with this call is never reaped.
/// Returns the surviving note list (unchanged when there was nothing to reap).
///
/// `boot_ids` stays valid across this second `ensure_notes_migrated`: migration is
/// idempotent (it early-returns on a present, readable index — see the fn), so the
/// FE's `list_notes` and this reap observe the same ids. The corrupt-index rebuild
/// that reassigns ids only runs when the index is unreadable; if that somehow fires
/// between the two calls, `boot_ids` simply matches nothing and the reap is a safe
/// no-op (cleanup defers to the next boot) — never a wrong deletion.
#[tauri::command]
pub async fn reap_empty_scratch_notes(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
    active_id: Option<String>,
    boot_ids: Vec<String>,
) -> Result<Vec<NoteMeta>, String> {
    let loc = resolve_notes_location(&app, github_user_id, org_slug).await?;
    await_blocking("reap_empty_scratch_notes", move || {
        let history = records_notes::store(&loc)?.location()?;
        reap_empty_scratch_notes_at(
            &history.notes_dir(),
            Some(&history),
            active_id.as_deref(),
            &boot_ids,
        )
    })
    .await
}

/// The absolute notes directory (each note is `<dir>/<id>.md`), so a note handed
/// to an agent can name its file. The path is derived in Rust, so it must come
/// from a command. Ensures the notes dir + migration first.
#[tauri::command]
pub async fn notes_dir_path(
    app: tauri::AppHandle,
    github_user_id: u64,
    org_slug: Option<String>,
) -> Result<String, String> {
    let dir = resolve_notes_root(&app, github_user_id, org_slug).await?;
    await_blocking("notes_dir_path", move || {
        ensure_notes_migrated(&dir)?;
        let nd = notes_dir(&dir);
        fs::create_dir_all(&nd).map_err(|e| format!("failed to create notes dir: {}", e))?;
        Ok(nd.to_string_lossy().to_string())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::tempdir;

    // ===== legacy-filename pattern detection =====

    #[test]
    fn looks_like_legacy_note_matches_canonical_form() {
        assert!(looks_like_legacy_note("my-repo-a1b2c3d4.md"));
        assert!(looks_like_legacy_note("repo-12345678.md"));
        assert!(looks_like_legacy_note("foo-deadbeef.md"));
    }

    #[test]
    fn looks_like_legacy_note_rejects_notes_md_itself() {
        // notes.md ends in .md but has no `-<8hex>` suffix.
        assert!(!looks_like_legacy_note("notes.md"));
    }

    #[test]
    fn looks_like_legacy_note_rejects_non_md() {
        assert!(!looks_like_legacy_note("my-repo-a1b2c3d4.txt"));
        assert!(!looks_like_legacy_note("foo.json"));
    }

    #[test]
    fn looks_like_legacy_note_rejects_short_hex() {
        assert!(!looks_like_legacy_note("repo-a1b2.md")); // 4 hex, not 8
    }

    #[test]
    fn looks_like_legacy_note_rejects_non_hex_in_hash() {
        assert!(!looks_like_legacy_note("repo-xyz12345.md")); // x, y, z not hex
    }

    #[test]
    fn looks_like_legacy_note_rejects_no_dash() {
        assert!(!looks_like_legacy_note("repoa1b2c3d4.md")); // no dash before hash
    }

    #[test]
    fn looks_like_legacy_note_rejects_non_ascii_filenames_without_panicking() {
        // P6 patch: byte-arithmetic on UTF-8 must not panic.
        assert!(!looks_like_legacy_note("📄-12345678.md"));
        assert!(!looks_like_legacy_note("naïve-12345678.md"));
        assert!(!looks_like_legacy_note("café-deadbeef.md"));
    }

    // ===== legacy notes.md load path (migration source) =====

    #[test]
    fn load_returns_empty_when_no_notes_md_and_no_legacy() {
        let tmp = tempdir().unwrap();
        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "");
        // Should NOT have created notes.md (nothing to seed from).
        assert!(!notes_file_path(tmp.path()).exists());
    }

    // ===== migration (AC-14) =====

    #[test]
    fn migration_seeds_from_single_legacy_file() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("my-repo-a1b2c3d4.md"), "legacy content").unwrap();
        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "legacy content");
        // notes.md should now exist with the legacy content.
        assert_eq!(
            fs::read_to_string(notes_file_path(tmp.path())).unwrap(),
            "legacy content"
        );
        // Legacy file is preserved (non-destructive).
        assert!(tmp.path().join("my-repo-a1b2c3d4.md").exists());
    }

    #[test]
    fn migration_picks_newest_non_empty_legacy_when_multiple_present() {
        let tmp = tempdir().unwrap();
        // Create three legacy files with deliberately-spaced mtimes.
        fs::write(tmp.path().join("repo-a-11111111.md"), "OLDEST").unwrap();
        sleep(Duration::from_millis(20));
        fs::write(tmp.path().join("repo-b-22222222.md"), "MIDDLE").unwrap();
        sleep(Duration::from_millis(20));
        fs::write(tmp.path().join("repo-c-33333333.md"), "NEWEST").unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "NEWEST");
    }

    #[test]
    fn migration_skips_empty_legacy_files() {
        let tmp = tempdir().unwrap();
        // Newest file is empty — should be skipped in favor of the older non-empty one.
        fs::write(tmp.path().join("repo-a-11111111.md"), "real content").unwrap();
        sleep(Duration::from_millis(20));
        fs::write(tmp.path().join("repo-b-22222222.md"), "").unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "real content");
    }

    #[test]
    fn migration_returns_empty_when_all_legacy_files_empty() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("repo-a-11111111.md"), "").unwrap();
        fs::write(tmp.path().join("repo-b-22222222.md"), "").unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "");
        assert!(!notes_file_path(tmp.path()).exists());
    }

    #[test]
    fn migration_does_not_overwrite_existing_notes_md() {
        let tmp = tempdir().unwrap();
        fs::write(notes_file_path(tmp.path()), "user-edited content").unwrap();
        fs::write(
            tmp.path().join("repo-a-11111111.md"),
            "legacy that should NOT win",
        )
        .unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "user-edited content");
    }

    #[test]
    fn migration_ignores_non_legacy_md_files() {
        let tmp = tempdir().unwrap();
        // Files that look superficially similar but don't match the pattern.
        fs::write(tmp.path().join("README.md"), "should not seed").unwrap();
        fs::write(tmp.path().join("foo.md"), "should not seed").unwrap();
        fs::write(tmp.path().join("repo-short.md"), "should not seed").unwrap(); // not 8 hex

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "");
        assert!(!notes_file_path(tmp.path()).exists());
    }

    #[test]
    fn migration_skips_directories_matching_pattern() {
        let tmp = tempdir().unwrap();
        // A directory named like a legacy note shouldn't seed.
        fs::create_dir(tmp.path().join("repo-a1b2c3d4.md")).unwrap();
        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "");
    }

    #[cfg(unix)]
    #[test]
    fn migration_skips_symlinks_matching_legacy_pattern() {
        // R5A-P2: a symlink in scratch_dir whose name matches the legacy
        // pattern must NOT be followed (would otherwise read symlink target
        // content into notes.md — potential OOM or content-from-wrong-file).
        use std::os::unix::fs::symlink;
        let tmp = tempdir().unwrap();
        // Plant a real file with sensitive content somewhere ELSE in the
        // tempdir tree — what the symlink points to.
        let secret_dir = tmp.path().join("not-scratch");
        fs::create_dir(&secret_dir).unwrap();
        let secret_path = secret_dir.join("secret.md");
        fs::write(&secret_path, "DO NOT LEAK").unwrap();
        // Plant a symlink in scratch_dir matching the legacy pattern.
        let link_path = tmp.path().join("repo-deadbeef.md");
        symlink(&secret_path, &link_path).unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        // Migration must NOT follow the symlink — notes.md stays absent,
        // load returns empty, secret content not exfiltrated.
        assert_eq!(loaded, "");
        assert!(!notes_file_path(tmp.path()).exists());
    }

    #[test]
    fn migration_does_not_overwrite_notes_md_created_after_exists_check() {
        // R5A-P1: simulates the TOCTOU race by pre-creating notes.md
        // BEFORE calling load_notes_at. Even though a legacy candidate
        // exists, the seed write path uses `fs::hard_link` which fails
        // with AlreadyExists when notes.md is present — defending against
        // the (real) scenario where a user save races a still-running
        // migration. Note: the early-return on `notes.exists()` also
        // covers this case; this test belt-and-suspenders the hard_link
        // guard by ensuring the recorded behavior is "user content wins".
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("repo-a-deadbeef.md"), "legacy seed").unwrap();
        fs::write(notes_file_path(tmp.path()), "user typed first").unwrap();

        let loaded = load_notes_at(tmp.path()).unwrap();
        assert_eq!(loaded, "user typed first");
        // Legacy file untouched.
        assert!(tmp.path().join("repo-a-deadbeef.md").exists());
    }

    // ===== exe-hash namespacing =====

    #[test]
    fn scratch_dir_includes_exe_hash_subdirectory() {
        let dir = scratch_dir().unwrap();
        let dir_str = dir.to_string_lossy().to_string();
        assert!(
            dir_str.contains(".codesense/scratch-notes/"),
            "expected exe-hash subdir under scratch-notes/, got: {}",
            dir_str
        );
        // The last segment should be 8 chars (exe_hash[..8]).
        let last = dir.file_name().unwrap().to_string_lossy();
        assert_eq!(last.len(), 8, "exe-hash subdir should be 8 chars");
    }

    #[test]
    fn scratch_dir_is_deterministic_within_process() {
        let a = scratch_dir().unwrap();
        let b = scratch_dir().unwrap();
        assert_eq!(a, b);
    }

    // ===== SW-SN-1: plural notes model =====

    use std::collections::HashSet;

    #[test]
    fn note_id_is_valid_unique_and_filename_safe() {
        let a = generate_note_id();
        let b = generate_note_id();
        assert_ne!(a, b);
        assert!(is_valid_note_id(&a));
        // Bulk uniqueness (counter differentiates same-nanosecond calls).
        let ids: HashSet<_> = (0..1000).map(|_| generate_note_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn is_valid_note_id_rejects_traversal_and_junk() {
        assert!(!is_valid_note_id(""));
        assert!(!is_valid_note_id("../secret"));
        assert!(!is_valid_note_id("foo/bar"));
        assert!(!is_valid_note_id("note.md"));
        assert!(!is_valid_note_id(&"a".repeat(65)));
        assert!(is_valid_note_id("1a2b-ff"));
    }

    #[test]
    fn create_list_write_read_round_trip() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let note = create_note_at(&nd, "My Note").unwrap();
        assert_eq!(note.title, "My Note");
        assert!(!note.pinned);
        assert_eq!(note.content, "");
        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, note.id);
        write_note_at(&nd, &note.id, "# hello\n\nbody").unwrap();
        let read = read_note_at(&nd, &note.id).unwrap();
        assert_eq!(read.content, "# hello\n\nbody");
        assert_eq!(read.title, "My Note");
    }

    #[test]
    fn create_defaults_blank_title_to_untitled() {
        let tmp = tempdir().unwrap();
        let note = create_note_at(&tmp.path().join("notes"), "   ").unwrap();
        assert_eq!(note.title, "Untitled");
    }

    #[test]
    fn write_note_rejects_unknown_and_invalid_id() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        create_note_at(&nd, "a").unwrap();
        assert!(write_note_at(&nd, "deadbeef-0", "x").is_err()); // unknown
        assert!(write_note_at(&nd, "../escape", "x").is_err()); // traversal
        assert!(read_note_at(&nd, "../escape").is_err());
    }

    #[test]
    fn update_meta_renames_pins_touches_and_leaves_body_and_id() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let note = create_note_at(&nd, "orig").unwrap();
        write_note_at(&nd, &note.id, "BODY").unwrap();
        update_note_meta_at(&nd, &note.id, Some("renamed".into()), None, None).unwrap();
        update_note_meta_at(&nd, &note.id, None, Some(true), None).unwrap();
        update_note_meta_at(&nd, &note.id, None, None, Some(12345)).unwrap();
        let read = read_note_at(&nd, &note.id).unwrap();
        assert_eq!(read.title, "renamed");
        assert!(read.pinned);
        assert_eq!(read.last_opened_at, 12345);
        assert_eq!(read.content, "BODY"); // metadata writes never touch the body
        assert_eq!(read.id, note.id); // id immutable across rename (AC-1)
    }

    #[test]
    fn update_meta_ignores_blank_rename() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let note = create_note_at(&nd, "keep").unwrap();
        update_note_meta_at(&nd, &note.id, Some("  ".into()), None, None).unwrap();
        assert_eq!(read_note_at(&nd, &note.id).unwrap().title, "keep");
    }

    #[test]
    fn delete_removes_note_and_body() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let a = create_note_at(&nd, "a").unwrap();
        let b = create_note_at(&nd, "b").unwrap();
        write_note_at(&nd, &a.id, "aaa").unwrap();
        let list = delete_note_at(&nd, &a.id).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, b.id);
        assert!(!note_body_path(&nd, &a.id).exists());
    }

    #[test]
    fn delete_last_note_reseeds_fresh_scratch() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let only = create_note_at(&nd, "only").unwrap();
        let list = delete_note_at(&nd, &only.id).unwrap();
        assert_eq!(list.len(), 1); // never a zero-notes state (AC-4)
        assert_ne!(list[0].id, only.id);
        assert_eq!(list[0].title, "Note");
    }

    // ===== SW-SN-13 AC-4: reap empty duplicate "Note" notes =====

    /// Boot snapshot = every note currently on disk (the FE captures this from its
    /// first `listNotes` before invoking the reap).
    fn boot_ids(nd: &Path) -> Vec<String> {
        read_index(nd).unwrap().into_iter().map(|m| m.id).collect()
    }

    #[test]
    fn reap_removes_only_empty_untitled_scratch_dupes() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        // Two empty "Note" dupes (the bug's fallout)...
        let dupe1 = create_note_at(&nd, "Note").unwrap();
        let dupe2 = create_note_at(&nd, "Note").unwrap();
        // ...one "Note" WITH content (must survive)...
        let with_body = create_note_at(&nd, "Note").unwrap();
        write_note_at(&nd, &with_body.id, "real content").unwrap();
        // ...and one user-renamed empty note (must survive).
        let renamed = create_note_at(&nd, "My Note").unwrap();

        let survivors = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();

        let ids: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
        assert!(!ids.contains(&dupe1.id.as_str()), "empty dupe reaped");
        assert!(!ids.contains(&dupe2.id.as_str()), "empty dupe reaped");
        assert!(ids.contains(&with_body.id.as_str()), "content note kept");
        assert!(ids.contains(&renamed.id.as_str()), "renamed note kept");
        // Reaped bodies removed from disk.
        assert!(!note_body_path(&nd, &dupe1.id).exists());
        assert!(!note_body_path(&nd, &dupe2.id).exists());
    }

    #[test]
    fn reap_never_deletes_whitespace_only_check_uses_trim() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        // Whitespace-only body counts as empty and IS reaped...
        let ws = create_note_at(&nd, "Note").unwrap();
        write_note_at(&nd, &ws.id, "   \n\t ").unwrap();
        // ...but a body that trims to real text is kept.
        let kept = create_note_at(&nd, "Note").unwrap();
        write_note_at(&nd, &kept.id, "  hi  ").unwrap();

        let survivors = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();
        let ids: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
        assert!(!ids.contains(&ws.id.as_str()), "whitespace-only reaped");
        assert!(ids.contains(&kept.id.as_str()), "non-blank body kept");
    }

    #[test]
    fn reap_never_deletes_pinned_empty_scratch() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let pinned = create_note_at(&nd, "Note").unwrap();
        update_note_meta_at(&nd, &pinned.id, None, Some(true), None).unwrap();
        let dupe = create_note_at(&nd, "Note").unwrap();

        let survivors = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();
        let ids: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&pinned.id.as_str()), "pinned note kept");
        assert!(
            !ids.contains(&dupe.id.as_str()),
            "unpinned empty dupe reaped"
        );
    }

    #[test]
    fn reap_preserves_the_active_note_even_when_empty() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let active = create_note_at(&nd, "Note").unwrap(); // empty + active
        let dupe = create_note_at(&nd, "Note").unwrap();

        let survivors =
            reap_empty_scratch_notes_at(&nd, None, Some(&active.id), &boot_ids(&nd)).unwrap();
        let ids: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&active.id.as_str()), "active note kept");
        assert!(
            !ids.contains(&dupe.id.as_str()),
            "non-active empty dupe reaped"
        );
    }

    #[test]
    fn reap_keeps_sole_survivor_when_all_notes_are_empty_scratch() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let a = create_note_at(&nd, "Note").unwrap();
        update_note_meta_at(&nd, &a.id, None, None, Some(10)).unwrap();
        let b = create_note_at(&nd, "Note").unwrap();
        update_note_meta_at(&nd, &b.id, None, None, Some(99)).unwrap(); // most recent

        let survivors = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();
        // Never a zero-notes state; keeps exactly the most-recent one.
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].id, b.id);
    }

    // SW-SN-13 (review r1, Edge Case Hunter): a note created AFTER the boot
    // snapshot (e.g. the panel's 3s editable-hatch lazy-create racing the reap)
    // is NOT reap-eligible, even when momentarily empty + untitled — closing the
    // reap-vs-create data-loss race.
    #[test]
    fn reap_never_touches_a_note_created_after_boot_snapshot() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let existing = create_note_at(&nd, "Note").unwrap(); // empty dupe on disk
        let boot = boot_ids(&nd); // snapshot BEFORE the concurrent create
        let racer = create_note_at(&nd, "Note").unwrap(); // born after the snapshot

        let survivors = reap_empty_scratch_notes_at(&nd, None, None, &boot).unwrap();
        let ids: Vec<&str> = survivors.iter().map(|m| m.id.as_str()).collect();
        // The racer survives (not in boot snapshot); the pre-existing dupe is reaped
        // (boot has >=1 survivor — the racer — so the never-zero guard isn't needed).
        assert!(
            ids.contains(&racer.id.as_str()),
            "post-boot note never reaped"
        );
        assert!(
            !ids.contains(&existing.id.as_str()),
            "pre-existing empty dupe still reaped"
        );
    }

    #[test]
    fn reap_is_idempotent_noop_on_clean_store() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let a = create_note_at(&nd, "Note").unwrap();
        write_note_at(&nd, &a.id, "keep me").unwrap();
        let b = create_note_at(&nd, "Renamed").unwrap();

        let first = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();
        let second = reap_empty_scratch_notes_at(&nd, None, None, &boot_ids(&nd)).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        let ids: Vec<&str> = second.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&a.id.as_str()) && ids.contains(&b.id.as_str()));
    }

    // SW-SN-13 (review r1, Blind Hunter): idempotence AFTER a real reap — the
    // second pass over the already-reaped store is a stable no-op.
    #[test]
    fn reap_is_stable_after_an_actual_reap() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let keep = create_note_at(&nd, "Note").unwrap();
        write_note_at(&nd, &keep.id, "content").unwrap();
        create_note_at(&nd, "Note").unwrap(); // empty dupe → reaped on pass 1

        let boot = boot_ids(&nd);
        let first = reap_empty_scratch_notes_at(&nd, None, None, &boot).unwrap();
        assert_eq!(first.len(), 1); // dupe gone
                                    // Second pass over the SAME boot snapshot: nothing left to reap → unchanged.
        let second = reap_empty_scratch_notes_at(&nd, None, None, &boot).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, keep.id);
    }

    #[test]
    fn migration_seeds_first_note_from_legacy_notes_md() {
        let tmp = tempdir().unwrap();
        fs::write(notes_file_path(tmp.path()), "<p>legacy html</p>").unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        let nd = notes_dir(tmp.path());
        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Note");
        // Rust relocates the string verbatim (HTML→MD conversion is editor-side).
        assert_eq!(
            read_note_at(&nd, &list[0].id).unwrap().content,
            "<p>legacy html</p>"
        );
    }

    #[test]
    fn migration_seeds_from_pre_f6_legacy_via_notes_md_chain() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("repo-a1b2c3d4.md"), "per-repo content").unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        let nd = notes_dir(tmp.path());
        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            read_note_at(&nd, &list[0].id).unwrap().content,
            "per-repo content"
        );
    }

    #[test]
    fn migration_fresh_install_creates_one_empty_note() {
        let tmp = tempdir().unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        let nd = notes_dir(tmp.path());
        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Note");
        assert_eq!(read_note_at(&nd, &list[0].id).unwrap().content, "");
    }

    #[test]
    fn migration_is_idempotent() {
        let tmp = tempdir().unwrap();
        fs::write(notes_file_path(tmp.path()), "once").unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        assert_eq!(read_index(&notes_dir(tmp.path())).unwrap().len(), 1);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_sibling() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        fs::create_dir_all(&nd).unwrap();
        atomic_write(&nd, "x.md", b"data").unwrap();
        let leftover: Vec<_> = fs::read_dir(&nd)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty());
        assert_eq!(fs::read_to_string(nd.join("x.md")).unwrap(), "data");
    }

    #[test]
    fn sweep_removes_cross_pid_tmp_but_keeps_own() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path();
        let other_pid = if std::process::id() == 99999 {
            88888
        } else {
            99999
        };
        let orphan = nd.join(format!("a.md.tmp.{}.0", other_pid));
        fs::write(&orphan, "orphan").unwrap();
        let own = nd.join(format!("b.md.tmp.{}.7", std::process::id()));
        fs::write(&own, "mine").unwrap();
        sweep_notes_tmp_files(nd);
        assert!(!orphan.exists());
        assert!(own.exists());
    }

    // ===== corrupt-index recovery (review hardening) =====

    #[test]
    fn corrupt_index_is_rebuilt_from_bodies_preserving_content() {
        let tmp = tempdir().unwrap();
        let nd = notes_dir(tmp.path());
        let id = generate_note_id();
        fs::create_dir_all(&nd).unwrap();
        fs::write(note_body_path(&nd, &id), "# Title line\n\nbody").unwrap();
        // Corrupt the index behind the recovery's back.
        fs::write(index_path(&nd), "{ this is not valid json").unwrap();

        ensure_notes_migrated(tmp.path()).unwrap();

        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id); // body on disk → id recovered
        assert_eq!(list[0].title, "Title line"); // first non-empty line, '#' stripped
        assert_eq!(
            read_note_at(&nd, &id).unwrap().content,
            "# Title line\n\nbody" // content never lost
        );
        assert!(nd.join("index.json.corrupt").exists()); // bad index quarantined
    }

    #[test]
    fn corrupt_index_with_no_bodies_seeds_fresh() {
        let tmp = tempdir().unwrap();
        let nd = notes_dir(tmp.path());
        fs::create_dir_all(&nd).unwrap();
        fs::write(index_path(&nd), "garbage").unwrap();

        ensure_notes_migrated(tmp.path()).unwrap();

        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Note");
    }

    #[test]
    fn rebuild_from_bodies_ignores_non_note_files() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path();
        fs::write(nd.join("1a2b-ff.md"), "hello").unwrap();
        fs::write(nd.join("index.json.corrupt"), "old").unwrap();
        // legacy-shaped name is not a valid note id → skipped.
        fs::write(nd.join("repo-a1b2c3d4.md"), "not a note").unwrap();

        let rebuilt = rebuild_index_from_bodies(nd).unwrap();
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].id, "1a2b-ff");
    }

    // ===== SW-SN-11: body/content search =====

    fn search_notes_in(nd: &Path, query: &str) -> Vec<records_notes::NoteHit> {
        let root = nd.parent().unwrap();
        let loc = NotesLocation::in_scratch(root.into(), root.into());
        records_notes::search(&loc, query, 200, None).unwrap()
    }

    #[test]
    fn search_matches_body_substring_case_insensitively_with_trimmed_snippet() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let note = create_note_at(&nd, "Untitled").unwrap();
        write_note_at(
            &nd,
            &note.id,
            "first line\n   the TLS repro steps here   \nlast",
        )
        .unwrap();

        let hits = search_notes_in(&nd, "tls"); // lower query vs mixed-case body
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, note.id);
        // First matching line, trimmed of surrounding whitespace.
        assert_eq!(hits[0].matched, "text");
        assert_eq!(hits[0].snippet.as_deref(), Some("the TLS repro steps here"));
    }

    #[test]
    fn search_returns_empty_on_no_match_and_on_empty_query() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let note = create_note_at(&nd, "n").unwrap();
        write_note_at(&nd, &note.id, "curl example").unwrap();

        assert!(search_notes_in(&nd, "nonesuch").is_empty());
        assert!(search_notes_in(&nd, "").is_empty());
        assert!(search_notes_in(&nd, "   ").is_empty()); // whitespace-only
    }

    #[test]
    fn search_skips_empty_or_absent_bodies() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        // A note with an empty body cannot match anything.
        let empty = create_note_at(&nd, "empty").unwrap();
        let ghost = create_note_at(&nd, "ghost").unwrap();
        fs::remove_file(note_body_path(&nd, &ghost.id)).unwrap();

        let hits = search_notes_in(&nd, "anything");
        assert!(hits.is_empty());
        // Sanity: the empty note exists but matched nothing.
        assert!(read_index(&nd).unwrap().iter().any(|m| m.id == empty.id));
    }

    #[test]
    fn search_orders_hits_by_most_recent_change_or_open() {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        let older = create_note_at(&nd, "older").unwrap();
        let newer = create_note_at(&nd, "newer").unwrap();
        write_note_at(&nd, &older.id, "shared keyword A").unwrap();
        write_note_at(&nd, &newer.id, "shared keyword B").unwrap();
        // Opening counts like a change: open `newer` after both writes.
        let later = now_millis() + 60_000;
        update_note_meta_at(&nd, &older.id, None, None, Some(later)).unwrap();
        update_note_meta_at(&nd, &newer.id, None, None, Some(later + 1)).unwrap();

        let hits = search_notes_in(&nd, "keyword");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, newer.id); // newest-first (AC-2 recency)
        assert_eq!(hits[1].id, older.id);
    }

    #[test]
    fn make_snippet_windows_around_match_and_bounds_length_char_safe() {
        // Fits under the cap → returned verbatim (trimmed), no ellipsis.
        assert_eq!(make_snippet("  hello world  ", "world"), "hello world");

        // Match at the START of a long line → head window, trailing ellipsis,
        // no panic on the multibyte tail.
        let head = format!("MATCH{}", "é".repeat(SNIPPET_MAX + 50));
        let snip = make_snippet(&head, "match");
        assert!(snip.to_lowercase().contains("match"));
        assert!(snip.ends_with('…'));
        assert!(snip.chars().count() <= SNIPPET_MAX + 2);

        // Match DEEP past the cap → windowed so the match stays visible. This is
        // the regression guard: a start-anchored cap would drop the match text
        // entirely and the FE fuzzy re-filter would then hide the whole row.
        let deep = format!("{}MATCHhere", "é".repeat(SNIPPET_MAX + 50));
        let snip = make_snippet(&deep, "match");
        assert!(
            snip.to_lowercase().contains("match"),
            "deep match must survive windowing: {snip}"
        );
        assert!(snip.starts_with('…'));
        assert!(snip.chars().count() <= SNIPPET_MAX + 2);

        // MANY expanding-lowercase chars (`İ`→`i̇`, 2 each) before the match, with
        // a long suffix after it. The anchor maps each lowered char back to its
        // EXACT `orig` index, so the expansion can't drift the window off the
        // match. (A char-count-in-lowered anchor would land at ~char 76 here —
        // past the match at char 50 — and drop it.)
        let expanding = format!("{}MATCH{}", "İ".repeat(50), "a".repeat(SNIPPET_MAX));
        let snip = make_snippet(&expanding, "match");
        assert!(
            snip.to_lowercase().contains("match"),
            "deep match after many expanding-lowercase chars must survive: {snip}"
        );
        assert!(snip.chars().count() <= SNIPPET_MAX + 2);
    }

    #[test]
    fn absent_index_with_orphan_bodies_adopts_them_not_seeds_fresh() {
        // Interrupted recovery / externally-deleted index: bodies exist but no
        // index.json. Migration must adopt the bodies, not seed an empty note
        // that orphans them.
        let tmp = tempdir().unwrap();
        let nd = notes_dir(tmp.path());
        fs::create_dir_all(&nd).unwrap();
        let id = generate_note_id();
        fs::write(nd.join(format!("{}.md", id)), "orphan body").unwrap();

        ensure_notes_migrated(tmp.path()).unwrap();

        let list = read_index(&nd).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(read_note_at(&nd, &id).unwrap().content, "orphan body");
    }

    // ===== SW-SN-4: delete-sticky pinned "Prompts" seed =====

    #[test]
    fn prompts_seed_creates_one_pinned_note_with_body() {
        let tmp = tempdir().unwrap();
        ensure_prompts_seeded(tmp.path());
        let nd = notes_dir(tmp.path());
        let list = read_index(&nd).unwrap();
        let prompts: Vec<_> = list.iter().filter(|m| m.title == "Prompts").collect();
        assert_eq!(prompts.len(), 1, "exactly one Prompts note seeded");
        assert!(prompts[0].pinned, "AC-2: the seed is pinned");
        // The sentinel + a non-empty body were written.
        assert!(nd.join(PROMPTS_SEED_SENTINEL).exists());
        assert!(read_note_at(&nd, &prompts[0].id)
            .unwrap()
            .content
            .contains("```"));
    }

    #[test]
    fn prompts_seed_is_idempotent() {
        let tmp = tempdir().unwrap();
        ensure_prompts_seeded(tmp.path());
        ensure_prompts_seeded(tmp.path());
        let nd = notes_dir(tmp.path());
        let count = read_index(&nd)
            .unwrap()
            .iter()
            .filter(|m| m.title == "Prompts")
            .count();
        assert_eq!(count, 1, "second call is a no-op — no duplicate");
    }

    #[test]
    fn prompts_seed_is_delete_sticky() {
        // AC-2 headline: once seeded, deleting the note must NOT resurrect it.
        let tmp = tempdir().unwrap();
        ensure_prompts_seeded(tmp.path());
        let nd = notes_dir(tmp.path());
        let prompts = read_index(&nd)
            .unwrap()
            .into_iter()
            .find(|m| m.title == "Prompts")
            .expect("seeded");
        delete_note_at(&nd, &prompts.id).unwrap();
        // Re-run the seed: the sentinel survived the delete → no Prompts note.
        ensure_prompts_seeded(tmp.path());
        assert!(
            read_index(&nd)
                .unwrap()
                .iter()
                .all(|m| m.title != "Prompts"),
            "deleted Prompts note is never resurrected"
        );
    }

    #[test]
    fn prompts_seed_skips_when_sentinel_already_present() {
        // Concurrency/create_new guard: a pre-existing sentinel short-circuits
        // (swallowed, no error surfaced, no note written).
        let tmp = tempdir().unwrap();
        let nd = notes_dir(tmp.path());
        fs::create_dir_all(&nd).unwrap();
        fs::write(nd.join(PROMPTS_SEED_SENTINEL), b"").unwrap();
        ensure_prompts_seeded(tmp.path());
        assert!(
            read_index(&nd).unwrap().is_empty(),
            "no note seeded past the sentinel"
        );
    }

    #[test]
    fn prompts_seed_coexists_with_migration() {
        // Fresh install: migration seeds "Note", then SN-4 adds pinned "Prompts".
        let tmp = tempdir().unwrap();
        ensure_notes_migrated(tmp.path()).unwrap();
        ensure_prompts_seeded(tmp.path());
        let list = read_index(&notes_dir(tmp.path())).unwrap();
        assert_eq!(list.len(), 2);
        let prompts = list.iter().find(|m| m.title == "Prompts").expect("seeded");
        assert!(prompts.pinned);
        assert!(list.iter().any(|m| m.title == "Note"));
    }

    #[test]
    fn prompts_seed_body_is_a_code_block_demo() {
        // AC-2/AC-3: the demo IS a code block (no new send mechanism needed).
        assert!(
            PROMPTS_SEED_BODY.contains("```"),
            "seed body has a fenced code block"
        );
    }

    #[test]
    fn prompts_seed_no_longer_depends_on_the_legacy_sidecar() {
        let tmp = tempdir().unwrap();
        let nd = notes_dir(tmp.path());
        fs::create_dir_all(index_path(&nd)).unwrap();
        ensure_prompts_seeded(tmp.path());
        ensure_prompts_seeded(tmp.path());
        assert!(nd.join(PROMPTS_SEED_SENTINEL).exists());
        assert_eq!(
            read_index(&nd)
                .unwrap()
                .iter()
                .filter(|m| m.title == "Prompts")
                .count(),
            1
        );
    }

    // ===== SLW-PT-19: notes resolve per active org in the scratch space =====

    #[test]
    fn notes_root_is_the_active_orgs_scratch_space_and_never_another_orgs() {
        let tmp = tempdir().unwrap();
        let me = crate::domain::Principal::from(42u64);
        let a = scratch_notes_root(tmp.path(), &me, "acme/ctx").unwrap();
        let b = scratch_notes_root(tmp.path(), &me, "other/ctx").unwrap();
        assert_ne!(a, b, "one set of notes per org");
        assert!(
            notes_dir(&a).ends_with("scratch/orgs/acme/ctx/notes"),
            "{}",
            notes_dir(&a).display()
        );

        ensure_notes_migrated(&a).unwrap();
        ensure_notes_migrated(&b).unwrap();
        let note = create_note_at(&notes_dir(&a), "Org A only").unwrap();
        write_note_at(&notes_dir(&a), &note.id, "secret plan").unwrap();

        assert!(read_index(&notes_dir(&b))
            .unwrap()
            .iter()
            .all(|m| m.id != note.id));
        assert!(search_notes_in(&notes_dir(&b), "secret").is_empty());
        assert_eq!(
            search_notes_in(&notes_dir(&a), "secret").len(),
            1,
            "org A still finds its own note"
        );
    }

    #[test]
    fn notes_root_creates_the_local_scratch_repo() {
        let tmp = tempdir().unwrap();
        let me = crate::domain::Principal::from(42u64);
        scratch_notes_root(tmp.path(), &me, "acme/ctx").unwrap();
        let repo_root = crate::context_repo::scratch_space::scratch_root(tmp.path(), &me);
        let repo = git2::Repository::open(repo_root).unwrap();
        assert!(repo.remotes().unwrap().is_empty(), "never a remote");
    }

    #[test]
    fn signed_out_profile_has_no_scratch_notes_root() {
        let tmp = tempdir().unwrap();
        assert!(scratch_notes_root(
            tmp.path(),
            &crate::domain::Principal::from(0u64),
            "acme/ctx"
        )
        .is_err());
    }
}
