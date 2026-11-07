// SPDX-License-Identifier: MIT
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use log::{debug, error, info, warn};
use serde::Serialize;

use tauri::Manager; // brings `try_state` into scope for write_content_file

use codesense_fs_core::gate::{normalize_path_components, path_within_any_root};
use codesense_fs_core::{FileEntry, FsGate, FsService};

use crate::commands::system::contexts_dir;

use super::paths::codesense_home_dir;

#[cfg(test)]
mod caller_scope_tests;

/// Whether `path` is a pipeline content file (`.md`/`.html`) — the only files
/// the frontmatter cache keys on, and the only OVERWRITES whose suppressed write
/// can change something the FE nav cares about (status / `supporting_docs:`).
pub(crate) fn is_pipeline_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| crate::commands::pipeline_format::detect_format(ext).is_some())
        .unwrap_or(false)
}

/// SW-VE-4: whether an OVERWRITE of `path` should surface live to the context
/// editor (fire the FE nudge). VE-2's viewer catch-all makes every non-binary file
/// editable as `code` — including unknown extensions (`.txt/.conf`) and
/// EXTENSIONLESS files (`Dockerfile`, `LICENSE`) — so the editable set can't be an
/// allowlist; it is "anything that is not a known binary redraw". Only the
/// canvas/image redraws (SW-FX-26) stay silent. An extensionless file is editable
/// text, so it nudges.
fn is_editable_text_context_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => !crate::commands::file_index::is_binary_redraw_extension(ext),
        None => true,
    }
}

/// SW-FX-9 Part 2B: self-heal the frontmatter cache after a watcher-suppressed
/// write to a content file.
///
/// A write through the suppressed writers (e.g. [`write_content_file`]) is
/// invisible to the watcher (loop prevention), so the watcher never runs its
/// `refresh_under_lock`. Without this, the cached entry for a rewritten story
/// `.md` — say one that just gained a `supporting_docs` ref — stays stale until
/// the next cold boot, and the cache's index consumers
/// (`story_linker::by_team_and_repo`, `worktree_index::by_story_id`) read stale
/// fields. (The pipeline *scan* is already disk-truthful via SW-FX-9 Part 1;
/// this heals the *live* cache the index consumers read.)
///
/// Self-guarding, so it is safe to call from any suppressed writer:
/// - only pipeline files (`.md`/`.html`) are cached — non-pipeline writes
///   (canvases, images) skip the cache lock entirely, keeping the ~1/sec
///   canvas-save path off it;
/// - only `_team/{team}/...` content/repo paths refresh — `parse_context_repo_parts`
///   yields the `dir_team` and returns `None` for anything else (so a stray
///   non-context path is a no-op). It also matches the legacy `repos/` layout,
///   but that's harmless here: the content writers never target `repos/` dests,
///   and `build_cache_entry` independently self-guards (pipeline-ext +
///   frontmatter `source_repo`), so a non-pipeline/orphan path can never
///   pollute the cache regardless.
///
/// The refresh is **transition-free** (`refresh_under_lock_no_transition`): the
/// source layer owns status transitions, and running one here could rewrite the
/// file and defeat the very suppression that prevents a reverse-sync loop.
fn refresh_cache_after_suppressed_write(app: &tauri::AppHandle, file_path: &Path) {
    if !is_pipeline_file(file_path) {
        return;
    }
    let Some(path_str) = file_path.to_str() else {
        return;
    };
    // Only `_team/{team}/content/...` paths are cache keys; this also derives
    // the directory-inferred team. A non-content path → `None` → no-op.
    let Some((team, _, _)) = crate::install::capture::parse_context_repo_parts(path_str) else {
        return;
    };
    let Some(cache) = app.try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
    else {
        return;
    };
    crate::install::frontmatter_cache::FrontmatterCache::refresh_under_lock_no_transition(
        cache.inner(),
        file_path,
        &team,
    );
    debug!(
        target: "files",
        "refreshed frontmatter cache after suppressed write to {}",
        file_path.display()
    );
}

/// Whether a suppressed write should fire the FE live-surface nudge (SW-FX-26).
///
/// - A **create** always nudges: a new sibling the nav must learn about (the
///   `apply_canvas` / attach `.excalidraw`, an image, a new story).
/// - An **editable-text overwrite** nudges (SW-VE-4): a `.md`/`.html` rewrite may
///   have changed frontmatter the nav renders, AND — now that VE-2 makes every
///   non-binary file editable in the context editor (config, code, AND unknown /
///   extensionless text like `Dockerfile`) — an overwrite may be landing under a
///   human who has that file open. The nudge is that editor's concurrent-write
///   signal: without it, an agent overwrite of a file the user is editing is
///   silent and the stale-guard / clean-take-disk never fires (a silent clobber).
///   Because the editable set is a catch-all, the gate is defined by EXCLUSION —
///   everything except a known binary redraw nudges.
/// - A **binary-redraw overwrite** does NOT nudge: a canvas/image redraw — the
///   ~1/sec autosave path SW-SC-1 made silent — changes nothing nav-relevant and
///   is not text a human edits. Re-emitting there would reintroduce the per-save
///   scan churn suppression exists to avoid (and the `.excalidraw` +
///   `.excalidraw.png` pair would double it).
fn should_emit_fe_nudge(file_path: &Path, was_create: bool) -> bool {
    was_create || is_editable_text_context_file(file_path)
}

/// Propagate a watcher-suppressed write to the rest of the app. A suppressed
/// write fires no FS event (loop prevention), so without this nothing
/// downstream — the FE nav, the frontmatter cache — hears about it. Two
/// independent, self-guarding effects, safe to call from any suppressed writer:
///
/// - **FE live-surface (SW-FX-26):**
///   [`crate::commands::watch::emit_context_change_for_suppressed_write`] emits
///   the `context-file-change` nudge so the pipeline nav re-scans and a
///   newly-created sibling (incl. **non-pipeline** canvases/images) plus any
///   `supporting_docs:` badge surface live, without a reload and without the
///   `ContentLayerEvent` projection wave. Gated by [`should_emit_fe_nudge`] so
///   the silent canvas-redraw path stays silent.
/// - **Frontmatter-cache self-heal (pipeline `.md`/`.html` only, SW-FX-9):**
///   [`refresh_cache_after_suppressed_write`] keeps the story index the
///   `story_linker` / `worktree_index` consumers read live.
fn propagate_suppressed_write(app: &tauri::AppHandle, file_path: &Path, was_create: bool) {
    if should_emit_fe_nudge(file_path, was_create) {
        crate::commands::watch::emit_context_change_for_suppressed_write(app, file_path);
    }
    refresh_cache_after_suppressed_write(app, file_path);
}

/// SW-FX-26: live-surface a watcher-suppressed STRUCTURAL op (move/delete) in
/// the FE pipeline nav. Same blindness as the content writers — the structural
/// source op (`apply_structural_source_op`) is watcher-suppressed and only
/// reconciles the cache, so without this a moved-in sibling never appears and a
/// deleted node never disappears until a reload.
///
/// Unlike [`propagate_suppressed_write`], this bypasses the create-vs-overwrite
/// gate ([`should_emit_fe_nudge`]) and emits unconditionally: a move/delete is a
/// rare, user-initiated gesture that ALWAYS changes nav structure (incl. moving
/// or deleting a non-pipeline `.excalidraw`), so the redraw-churn concern the
/// gate guards against does not apply. Emits for both endpoints of a rename so a
/// cross-directory move surfaces in both groups.
fn nudge_structural_change<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    change: &crate::install::story_linker::StructuralChange,
) {
    use crate::install::story_linker::StructuralChange;
    let emit = |p: &Path| crate::commands::watch::emit_context_change_for_suppressed_write(app, p);
    match change {
        StructuralChange::Delete { source_path, .. } => emit(source_path),
        StructuralChange::Rename { from, to, .. } => {
            emit(from);
            emit(to);
        }
    }
}

/// True for derived render sidecars (`<name>.excalidraw.png`) that are
/// agent-only artifacts and must never surface in any user-facing listing
/// (file tree, quick-open, supporting-doc routing). Making `png` viewable
/// (SW-SC-7) is what would otherwise un-hide SW-SC-2's derived canvas render.
///
/// Matched on the full file NAME, not the extension — `Path::extension()` is
/// just `png`. Single source of truth for both `file_index` (quick-open) and
/// `pipeline_scanner` (nav routing); mirror of `isDerivedRenderArtifact` in
/// `src/shared/lib/viewer-file-type.ts`.
pub(crate) fn is_derived_render_artifact(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".excalidraw.png")
}

/// Non-logging decision: is `path` inside the **context-repo** allow-list —
/// the app contexts dir or `~/.codesense/users/` — after `..`-normalization?
/// The pure core of [`validate_context_path`], split out so the composite
/// reader gate ([`EditorReadFs`]) can test context membership
/// **without** emitting the context gate's "traversal blocked" warn for a path
/// that is a legitimate *worktree* path (SLW-WDE-6 §C — no false-alarm log spam
/// on every worktree read).
fn path_in_context_roots<R: tauri::Runtime>(path: &Path, app: &tauri::AppHandle<R>) -> bool {
    matched_context_root(path, app).is_some()
}

/// Which accepted context root does `path` live under (normalized), if any — the
/// **contexts app-data dir**, `~/.codesense/users/`, or a registered attachment root.
/// `None` ⇒ the path is outside every context root.
fn registered_editor_root(home: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
    Ok(
        crate::context_repo::context_attachments::location_for_path(home, path)?
            .map(|location| location.attachment.root),
    )
}

fn matched_context_root<R: tauri::Runtime>(
    path: &Path,
    app: &tauri::AppHandle<R>,
) -> Option<PathBuf> {
    let normalized = normalize_path_components(path);

    // App-data contexts dir
    if let Ok(contexts) = contexts_dir(app) {
        if normalized.starts_with(&contexts) {
            return Some(contexts);
        }
    }

    // Org repo paths: ~/.codesense/users/
    if let Some(home) = codesense_home_dir() {
        let users = home.join(crate::context_repo::USERS_DIR);
        if normalized.starts_with(&users)
            || users
                .canonicalize()
                .is_ok_and(|real| normalized.starts_with(real))
        {
            return Some(users);
        }
        match registered_editor_root(&home, &normalized) {
            Ok(Some(root)) => return Some(root),
            Ok(None) => {}
            Err(error) => log::warn!("{error}"),
        }
    }

    None
}

/// Non-logging decision: is `path` inside a **registered worktree root**? The
/// pure, env-touching core shared by [`validate_worktree_path`] and the reader
/// gate. Fetches the allow-list ([`worktree_roots`]) synchronously and
/// uid-agnostically (no auth/active-uid on the hot read path, SLW-WDE-6 §D).
///
/// [`worktree_roots`]: crate::install::worktree_index::worktree_roots
fn path_in_worktree_roots(path: &Path) -> bool {
    let roots = codesense_home_dir()
        .map(|home| crate::install::worktree_index::worktree_roots(&home))
        .unwrap_or_default();
    path_within_any_root(path, &roots)
}

/// Validate that a path is within an accepted **context-repo** directory.
/// Accepts paths under:
/// - Contexts dir (`~/Library/Application Support/ai.codesense/contexts/`)
/// - Org repos (`~/.codesense/users/`)
///
/// Normalizes path components to prevent traversal via `..` even for non-existent paths.
/// This is the tight gate for the context-repo writers
/// (`write_content_file`/`write_binary_content_file`/`delete_content_file`/…);
/// the worktree editor uses [`validate_worktree_path`] instead (SLW-WDE-6 AC-3).
pub(crate) fn validate_context_path<R: tauri::Runtime>(
    path: &Path,
    app: &tauri::AppHandle<R>,
) -> Result<(), String> {
    let scope = crate::context_repo::caller_scope::for_page(app, None, None)?;
    validate_context_path_for(path, app, &scope)
}

pub(crate) fn validate_context_path_for<R: tauri::Runtime>(
    path: &Path,
    app: &tauri::AppHandle<R>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(), String> {
    scope.require(path)?;
    if matched_context_root(path, app).is_none() {
        warn!(target: "files", "path traversal attempt blocked: {}", path.display());
        return Err("Access denied: path is outside context repo directory".to_string());
    }
    Ok(())
}

/// Validate that a path is within a **registered worktree root** — a
/// `code_repo_path` recorded in the worktree index
/// (`install::worktree_index::worktree_roots`). This is the worktree editor's
/// sandbox (SLW-WDE-6): it admits the story worktrees codesense materialized,
/// which for an **attached repo** live OUTSIDE `~/.codesense/users/` (e.g.
/// `~/dev/acme-api-wt/feat-story-123`) — the exact case
/// [`validate_context_path`] rejects, which is why the editor never loaded.
///
/// - **Allow-list only:** the roots come from the recorded set, never "any git
///   worktree on disk" (AC-2/RD-2).
/// - **Traversal-safe:** normalized the same way as [`validate_context_path`]
///   before the prefix test, so `..` can't escape a root (AC-2).
/// - **Synchronous + uid-agnostic:** [`worktree_roots`] scans every user's
///   index dir, so this stays a drop-in for the sync call sites with no auth /
///   active-uid dependency on the hot read path (SLW-WDE-6 §D).
///
/// [`worktree_roots`]: crate::install::worktree_index::worktree_roots
pub(crate) fn validate_worktree_path(path: &Path) -> Result<(), String> {
    if path_in_worktree_roots(path) {
        return Ok(());
    }

    warn!(target: "files", "worktree path rejected (no registered worktree root): {}", path.display());
    Err("Access denied: path is outside any registered worktree".to_string())
}

/// Owned-`PathBuf` variant of `validate_context_path`. Builds the `PathBuf`
/// from a string, runs validation, and returns the owned path so handlers
/// can collapse the two-line preamble into a single call.
pub(crate) fn validate_context_path_owned<R: tauri::Runtime>(
    path_str: &str,
    app: &tauri::AppHandle<R>,
) -> Result<PathBuf, String> {
    let p = PathBuf::from(path_str);
    validate_context_path(&p, app)?;
    Ok(p)
}

/// [`validate_context_path_owned`] for a WRITE: additionally refuses the read-only
/// bundled CodeSense docs repo (SW-OC-16) and a write that a symlink carries into
/// a scratch space it does not name. Reads keep using the plain gate.
pub(crate) fn validate_context_write_path_owned<R: tauri::Runtime>(
    path_str: &str,
    app: &tauri::AppHandle<R>,
) -> Result<PathBuf, String> {
    let p = PathBuf::from(path_str);
    validate_context_path_for(
        &p,
        app,
        &crate::context_repo::caller_scope::CallerScope::Human,
    )?;
    refuse_read_only_context_write(&p)?;
    // The context-root check above is lexical; a link inside a team (or one
    // org's scratch space) must never carry a write into a scratch space.
    if let Some(refusal) = crate::context_repo::scratch_space::write_refusal(&p, true) {
        warn!(target: "files", "refused write through a link into a scratch space: {}", p.display());
        return Err(format!("Access denied: {refusal}"));
    }
    Ok(p)
}

/// Refuses a write (including a delete or rename) landing in the read-only CodeSense docs.
pub(crate) fn refuse_read_only_context_write(path: &Path) -> Result<(), String> {
    crate::context_repo::codesense_docs::refuse_write_at(path)
}

/// Every registered **source-repo attachment** root across ALL users — the
/// `repo_path` of each entry in `{codesense_home}/users/*/attachments.yml`.
///
/// Mirror of [`crate::install::worktree_index::worktree_roots`] but for the
/// source-repo attachment registry (`commands::attachments::save_source_repo_attachment`
/// writes it). Missing / malformed files are skipped — the same fault-isolation
/// posture: a stray row never blinds the whole sweep.
pub(crate) fn source_repo_roots() -> Vec<PathBuf> {
    let Some(home) = codesense_home_dir() else {
        return Vec::new();
    };
    source_repo_roots_at(&home)
}

/// Pure, home-dir-injected form of [`source_repo_roots`] — walks a given
/// `{codesense_home}/users/*/attachments.yml` tree and returns every registered
/// `repo_path`. Factored out so the pure decision logic is unit-testable without
/// touching the real `$HOME`.
pub(crate) fn source_repo_roots_at(codesense_home: &Path) -> Vec<PathBuf> {
    let users_dir = codesense_home.join(crate::context_repo::USERS_DIR);
    let user_entries = match std::fs::read_dir(&users_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut roots = Vec::new();
    for user in user_entries.flatten() {
        let path = user.path().join(crate::context_repo::ATTACHMENTS_FILE);
        if let Ok(data) = crate::context_repo::attachments_io::read_attachments(&path) {
            for row in data.attachments {
                roots.push(PathBuf::from(row.repo_path));
            }
        }
    }
    roots
}

/// Non-logging decision: is `path` inside a registered **source-repo
/// attachment** root? The pure core shared by the canvas-write gate.
fn path_in_source_repo_roots(path: &Path) -> bool {
    let roots = source_repo_roots();
    path_within_any_root(path, &roots)
}

/// SW-OC-CANVAS: write-path validator for `apply_canvas` — accepts the
/// context-repo allow-list AND a registered source-repo attachment root AND a
/// registered worktree root, while still refusing writes into the read-only
/// bundled CodeSense docs.
///
/// Rationale (MCP architectural reframe): `apply_canvas` is the agent's single
/// atomic writer for `.excalidraw` scenes, used BOTH to live-edit a canvas on
/// the content rails AND to author a canvas source into a source-repo clone
/// (pre-release docs authoring under `codesense-docs/`). The former ride the
/// existing context-write gate; the latter ride the source-repo attachment
/// registry (walked across every user, exactly like `worktree_roots`).
///
/// Composition:
/// 1. If the path is inside the context-repo allow-list, delegate to
///    `validate_context_write_path_owned` — this preserves the scratch-space
///    link refusal and the context-dir normalization/traversal check unchanged.
/// 2. Otherwise, accept if the path falls under a registered source-repo
///    attachment root or a registered worktree root (traversal-safe via
///    `normalize_path_components`).
/// 3. In every path, re-run [`refuse_read_only_context_write`] so a pattern-
///    matching path into the bundled docs is still refused even if it happens
///    to live under a source-repo attachment.
pub(crate) fn validate_canvas_write_path_owned(
    path_str: &str,
    app: &tauri::AppHandle,
) -> Result<PathBuf, String> {
    // Fast path: already inside the context-repo allow-list. Reuse the full
    // context-write gate so the scratch-space symlink refusal is preserved.
    let p = PathBuf::from(path_str);
    if path_in_context_roots(&p, app) {
        return validate_context_write_path_owned(path_str, app);
    }

    // Normalize before the prefix tests so `..` can never escape a root.
    let normalized = normalize_path_components(&p);
    let in_source_repo = path_in_source_repo_roots(&normalized);
    let in_worktree = path_in_worktree_roots(&normalized);
    if !in_source_repo && !in_worktree {
        warn!(
            target: "files",
            "canvas write rejected (outside context roots, source-repo attachments and worktrees): {}",
            p.display()
        );
        return Err(
            "Access denied: path is outside context roots, registered source-repo attachments and registered worktrees"
                .to_string(),
        );
    }

    // Even a path that pattern-matches a source-repo attachment root must still
    // refuse to overwrite the read-only bundled CodeSense docs (SW-OC-16).
    refuse_read_only_context_write(&normalized)?;
    Ok(normalized)
}

/// SW-OC-CANVAS: read-side companion to [`validate_canvas_write_path_owned`]
/// for the `render_canvas` MCP verb. Accepts any path the write gate accepts
/// MINUS the read-only refusal — a canvas authored into the bundled docs is
/// a defined-nonsense case for the writer, but the reader must still be able
/// to render ANY canvas on the content rails, including one shipped read-only
/// by the bundled docs. Mirrors the two acceptance paths:
/// - context-repo allow-list (via [`path_in_context_roots`]); OR
/// - a registered source-repo attachment root OR worktree root
///   (traversal-safe via [`normalize_path_components`]).
pub(crate) fn validate_canvas_read_path<R: tauri::Runtime>(
    path_str: &str,
    app: &tauri::AppHandle<R>,
) -> Result<(), String> {
    let p = PathBuf::from(path_str);
    if path_in_context_roots(&p, app) {
        return Ok(());
    }
    let normalized = normalize_path_components(&p);
    if path_in_source_repo_roots(&normalized) || path_in_worktree_roots(&normalized) {
        return Ok(());
    }
    warn!(
        target: "files",
        "canvas read rejected (outside context roots, source-repo attachments and worktrees): {}",
        p.display()
    );
    Err(
        "Access denied: path is outside context roots, registered source-repo attachments and registered worktrees"
            .to_string(),
    )
}

/// Owned-`PathBuf` variant of [`validate_worktree_path`] — the gate for the
/// worktree editor's **writer** (`write_worktree_file`). Worktree-root only
/// (no context-repo roots): a worktree save can never land in the context
/// repo, honoring RD-1's blast-radius invariant (SLW-WDE-6 §C).
pub(crate) fn validate_worktree_path_owned(path_str: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path_str);
    validate_worktree_path(&p)?;
    Ok(p)
}

/// The in-process file service behind the worktree editor's **readers**
/// (`list_directory` as the tree uses it, `read_file`): its gate admits the
/// context-repo allow-list **OR** a registered worktree root. These commands are
/// shared with the context nav, so they must keep accepting context paths; the
/// editor additionally needs worktree paths (SLW-WDE-6 §C).
///
/// **Cheap-first (SLW-WDE-6 §E):** the context check is an in-memory prefix
/// test, so a context-repo path — the common nav / quick-open case —
/// short-circuits and never touches the worktree index; only a non-context
/// path pays the `worktree_roots` scan. Keeps the hot read path free of extra
/// disk I/O.
///
/// Composes the **non-logging** `path_in_*` cores (not the two `validate_*`
/// fns): a legitimate worktree read must not trip `validate_context_path`'s
/// "path traversal attempt blocked" warn on its way to being allowed. Exactly
/// one accurate warn fires, and only on a true denial (SLW-WDE-6 §C).
///
/// The two membership tests are injected so the composite decision is testable
/// without an app handle or a real `$HOME`; [`editor_read_fs`] wires the live ones.
struct EditorReadFs<C, W> {
    in_context: C,
    in_worktree: W,
}

impl<C, W> FsGate for EditorReadFs<C, W>
where
    C: Fn(&Path) -> bool,
    W: Fn(&Path) -> bool,
{
    fn admit(&self, path: &Path) -> Result<(), String> {
        if (self.in_context)(path) || (self.in_worktree)(path) {
            return Ok(());
        }

        warn!(target: "files", "editor read path rejected (not a context or worktree path): {}", path.display());
        Err(
            "Access denied: path is outside the context repo and any registered worktree"
                .to_string(),
        )
    }
}

/// The live reader service: the app's context roots, then the registered
/// worktree roots.
fn editor_read_fs<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> impl FsService + '_ {
    let scope = crate::context_repo::caller_scope::for_page(app, None, None)
        .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown);
    editor_read_fs_for(app, scope)
}

fn editor_read_fs_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> impl FsService + '_ {
    ScopedReadFs {
        scope,
        inner: EditorReadFs {
            in_context: move |p: &Path| path_in_context_roots(p, app),
            in_worktree: path_in_worktree_roots,
        },
    }
}

struct ScopedReadFs<F> {
    inner: F,
    scope: crate::context_repo::caller_scope::CallerScope,
}
impl<F: FsGate> FsGate for ScopedReadFs<F> {
    fn admit(&self, path: &Path) -> Result<(), String> {
        self.scope.require(path)?;
        self.inner.admit(path)
    }
}

#[tauri::command]
pub async fn check_path_exists(path: String) -> Result<bool, String> {
    crate::commands::await_blocking("check_path_exists", move || {
        let p = PathBuf::from(&path);
        Ok(p.exists() && p.is_dir())
    })
    .await
}

#[tauri::command]
pub async fn read_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<String, String> {
    crate::commands::await_blocking("read_file", move || {
        debug!(target: "files", "read_file: path={}", path);
        // Reader gate: context-repo OR registered worktree root (SLW-WDE-6 §C/§E).
        editor_read_fs(&app).read(Path::new(&path))
    })
    .await
}

/// The same editor reader gate, with bounded bytes for the page/MCP file API.
pub(crate) async fn read_file_snapshot_for<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
    max_bytes: Option<usize>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<serde_json::Value, String> {
    crate::commands::await_blocking("read_file_snapshot", move || {
        scope.existing(Path::new(&path))?;
        editor_read_fs_for(&app, scope).admit(Path::new(&path))?;
        crate::commands::file_surface::read_snapshot(Path::new(&path), max_bytes)
    })
    .await
}

// ── SW-DX-11: doc-workspace scoped file read (`readWorkspace` bridge verb) ────

/// SW-DX-11: max bytes `read_workspace_file` will pull into memory for a single
/// file. A larger file rejects `too-large:` BEFORE its bytes are read, so a
/// dashboard can never blow a frame or the heap on a giant file (UI-non-blocking).
const MAX_WORKSPACE_READ_BYTES: u64 = 1024 * 1024; // 1 MiB

/// SW-DX-11: one entry in a `read_workspace_file` directory listing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDirEntry {
    pub name: String,
    pub is_dir: bool,
    /// File byte size; `0` for a directory.
    pub size: u64,
}

/// SW-DX-11: the result of a `read_workspace_file` call — EITHER file `content`
/// OR a directory `entries` listing, never both. The empty arm is omitted from
/// the wire so the FE branches on which field is present.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReadResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<WorkspaceDirEntry>>,
}

/// Pure core of `read_workspace_file` — no Tauri/app dependency, so the
/// traversal / managed / extension / size / binary gates all unit-test in
/// isolation. `codesense_home` is threaded in (resolved by the command) so the
/// managed-surface wall (`is_codesense_managed_target`) is testable too.
///
/// Gate order (defense in depth): resolve+canonicalize the doc's own dir →
/// managed-surface wall (AC-3) → lexical `..`/absolute reject (AC-2) → join +
/// canonicalize target + subtree-confinement (AC-2 symlink escape) → dir listing
/// OR (extension whitelist → size → utf8) for a file (AC-4). Every reject is a
/// stable typed prefix.
#[cfg(test)]
fn read_workspace_file_core(
    doc_path: &str,
    rel_path: &str,
    allowed_exts: &[String],
    codesense_home: &Path,
) -> Result<WorkspaceReadResult, String> {
    read_workspace_file_for(
        doc_path,
        rel_path,
        allowed_exts,
        codesense_home,
        &Default::default(),
    )
}

fn read_workspace_file_for(
    doc_path: &str,
    rel_path: &str,
    allowed_exts: &[String],
    codesense_home: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<WorkspaceReadResult, String> {
    use std::path::Component;

    // The dashboard doc's own directory — the confinement root. Canonicalize so
    // the subtree + managed checks operate on a real, symlink-resolved path.
    let doc_dir = Path::new(doc_path)
        .parent()
        .ok_or_else(|| format!("traversal: doc path has no parent dir ({doc_path})"))?;
    let doc_dir_canon = doc_dir.canonicalize().map_err(|e| {
        format!(
            "unavailable: doc dir not accessible ({}): {e}",
            doc_dir.display()
        )
    })?;

    // AC-3 managed-surface wall: the doc's own dir MUST be a codesense-managed
    // target (registered worktree OR under `{home}/users/`). Non-managed → reject.
    if !crate::install::worktree_index::is_codesense_managed_target(&doc_dir_canon, codesense_home)
    {
        return Err(format!("unmanaged: {}", doc_dir_canon.display()));
    }

    // AC-2 lexical guard: reject absolute paths and any `..`/rooted component
    // BEFORE touching the FS, so `../../secrets` never resolves at all.
    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        return Err(format!("traversal: absolute path not allowed ({rel_path})"));
    }
    for comp in rel.components() {
        if matches!(
            comp,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(format!(
                "traversal: parent/rooted component not allowed ({rel_path})"
            ));
        }
    }

    // AC-2 symlink escape: canonicalize the joined target (resolves a symlink's
    // real destination) and require it to stay inside the canonical doc dir.
    let joined = doc_dir_canon.join(rel);
    let missing = || {
        if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
            format!("not-found: {rel_path}")
        } else {
            format!("not-found: {}", crate::context_repo::caller_scope::MISSING)
        }
    };
    if !scope.visible(&joined) {
        return Err(missing());
    }
    match joined.canonicalize() {
        Ok(canon) => {
            if !canon.starts_with(&doc_dir_canon) {
                return Err(format!("traversal: resolves outside doc dir ({rel_path})"));
            }
            if !scope.visible(&canon) {
                return Err(missing());
            }
            if canon.is_dir() {
                read_workspace_dir(&canon, scope)
            } else {
                read_workspace_regular_file(&canon, allowed_exts)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(missing()),
        Err(e) => Err(format!("unavailable: cannot access {rel_path}: {e}")),
    }
}

/// List a confined directory: `{name, isDir, size}` per child, name-sorted for a
/// deterministic result. Unreadable children degrade to `{isDir:false, size:0}`
/// rather than failing the whole listing.
fn read_workspace_dir(
    dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<WorkspaceReadResult, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("unavailable: cannot list dir: {e}"))?;
    let mut entries: Vec<WorkspaceDirEntry> = rd
        .flatten()
        .filter(|ent| scope.visible(&ent.path()))
        .map(|ent| {
            let name = ent.file_name().to_string_lossy().into_owned();
            // Use `file_type()` (from readdir — does NOT follow symlinks) for the
            // kind, so a symlink child is reported as a non-dir with no size and
            // NEVER leaks its (possibly out-of-tree) target's dir/size (review r1).
            // Size is stat'd only for a real regular file.
            let (is_dir, size) = match ent.file_type() {
                Ok(ft) if ft.is_dir() => (true, 0),
                Ok(ft) if ft.is_file() => (false, ent.metadata().map(|m| m.len()).unwrap_or(0)),
                _ => (false, 0),
            };
            WorkspaceDirEntry { name, is_dir, size }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(WorkspaceReadResult {
        content: None,
        entries: Some(entries),
    })
}

/// Read a confined regular file behind the extension whitelist + size + utf8
/// walls. The whitelist is supplied by the TRUSTED bridge entry (a forged doc
/// can't widen it); an extensionless or non-whitelisted file is NEVER read.
fn read_workspace_regular_file(
    file: &Path,
    allowed_exts: &[String],
) -> Result<WorkspaceReadResult, String> {
    let file_name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // AC-4 extension whitelist (second wall behind managed confinement).
    let ext_ok = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| allowed_exts.iter().any(|a| a.eq_ignore_ascii_case(e)))
        .unwrap_or(false);
    if !ext_ok {
        return Err(format!("unsupported-type: {file_name}"));
    }

    // AC-4 size guard BEFORE reading bytes — heap/frame safe.
    let meta = fs::metadata(file).map_err(|e| format!("unavailable: cannot stat file: {e}"))?;
    if meta.len() > MAX_WORKSPACE_READ_BYTES {
        return Err(format!(
            "too-large: {} bytes (max {MAX_WORKSPACE_READ_BYTES})",
            meta.len()
        ));
    }

    let bytes = fs::read(file).map_err(|e| format!("unavailable: cannot read file: {e}"))?;
    match String::from_utf8(bytes) {
        Ok(content) => Ok(WorkspaceReadResult {
            content: Some(content),
            entries: None,
        }),
        Err(_) => Err(format!("binary: {file_name}")),
    }
}

/// SW-DX-11 (AC-1..4, AC-9): the `readWorkspace` bridge verb's Rust command —
/// a doc-scoped file read/dir-list STRICTLY within the dashboard doc's own
/// directory subtree on a codesense-managed surface. Runs on the blocking pool;
/// the size guard fires before any bytes are read (UI-non-blocking). All rejects
/// are stable typed prefixes: `traversal:`, `unmanaged:`, `unsupported-type:`,
/// `too-large:`, `binary:`, `not-found:`. `allowed_exts` is supplied by the
/// trusted bridge entry, not the doc.
#[tauri::command]
pub async fn read_workspace_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    doc_path: String,
    rel_path: String,
    allowed_exts: Vec<String>,
    frame_id: Option<String>,
) -> Result<WorkspaceReadResult, String> {
    let scope =
        crate::context_repo::caller_scope::for_page(&app, Some(&doc_path), frame_id.as_deref())?;
    crate::commands::await_blocking("read_workspace_file", move || {
        let home = codesense_home_dir()
            .ok_or_else(|| "unavailable: cannot resolve codesense home".to_string())?;
        read_workspace_file_for(&doc_path, &rel_path, &allowed_exts, &home, &scope)
    })
    .await
}

/// Atomically write UTF-8 `content` to a context-repo file.
///
/// The general editable-content writer (companion to [`read_file`]). The
/// SW-SC-1 Excalidraw canvas save is its first caller, but it is
/// deliberately format-agnostic so future editable file types reuse it.
///
/// - **Sandboxed** to the same allow-list as `read_file` via
///   [`validate_context_path_owned`] — writes outside `~/.codesense/users/`
///   or the app contexts dir are rejected.
/// - **Atomic + durable** via the audited `write_tmp_fsync_rename*`: a crash
///   mid-write leaves the previous file intact rather than a truncated/
///   half-written one (NFR5 — the scene JSON is never corrupted).
/// - **Watcher-suppressed:** the destination is inside a watched content dir,
///   so an un-suppressed write would dispatch a content-layer event (index
///   refresh + `context-file-change` emit) on every save — ~1/sec while
///   drawing. Suppression makes our own writes invisible to the watcher.
///
/// Symlink note: `write_tmp_fsync_rename*` replaces a symlink dest with a
/// regular file rather than writing through it. Safe for SW-SC-1 —
/// `.excalidraw` files are plain standalone content (the projection engine
/// materializes only `.md`/`.html`, so a canvas is never a projected
/// symlink). A future story that projects `.excalidraw` must resolve the
/// link and re-validate the target before writing.
#[tauri::command]
pub async fn write_content_file(
    app: tauri::AppHandle,
    path: String,
    content: String,
    expected_sha256: Option<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("write_content_file", move || {
        debug!(target: "files", "write_content_file: path={} bytes={}", path, content.len());
        let file_path = validate_context_write_path_owned(&path, &app)?;
        // The editor's lock: refuse a save over a version it never showed.
        write_locked(&file_path, expected_sha256.as_deref(), || {
            suppressed_content_write(&app, &file_path, content.as_bytes())
        })
    })
    .await
}

/// SW-OC-CANVAS: atomic `.excalidraw` scene writer for `apply_canvas` —
/// authors a canvas source onto the context rails OR into a registered
/// source-repo attachment / worktree.
///
/// Gate: [`validate_canvas_write_path_owned`]. The write dispatch tracks the
/// gate's two acceptance paths:
/// - **Context-repo path:** reuse [`suppressed_content_write`] — same
///   watcher-suppression + FE-nudge propagation as `write_content_file`, so a
///   live-edit canvas on the content rails hot-reloads with no watcher churn.
/// - **Source-repo attachment or worktree path:** plain atomic write (same
///   durability as `write_worktree_file`). The context watcher does not watch
///   source-repo trees, so no suppression is needed; keeping the write visible
///   to `git status` is a feature.
///
/// Rust-only entry point: `apply_canvas` calls it directly, not through a
/// Tauri command. Keeps the Tauri IPC contract (`commands.contract.test.ts`)
/// unchanged — this is a backend widening of the MCP verb, not a new frontend
/// command.
pub(crate) fn write_canvas_file(
    app: &tauri::AppHandle,
    path: &str,
    content: &str,
) -> Result<(), String> {
    let file_path = validate_canvas_write_path_owned(path, app)?;
    if path_in_context_roots(&file_path, app) {
        suppressed_content_write(app, &file_path, content.as_bytes())
    } else {
        crate::content_engine::write_tmp_fsync_rename(&file_path, content.as_bytes()).map_err(
            |e| {
                error!(target: "files", "canvas source-repo write failed for {}: {}", file_path.display(), e);
                format!("failed to write file {}: {}", file_path.display(), e)
            },
        )
    }
}

/// Atomically write `bytes` to a **context-repo** content path with watcher
/// suppression (loop prevention), then reconcile the frontmatter cache and
/// live-nudge the FE nav ([`propagate_suppressed_write`]). Shared core of
/// [`write_content_file`] — one source of truth for "author content into a
/// team's content tree."
///
/// - **Create-vs-overwrite** is captured before the atomic rename (SW-FX-26:
///   gates the FE nudge). Fail-safe: a stat error (permission, ELOOP, ejected
///   volume) is treated as a create so an uncertain case still nudges rather
///   than silently dropping a new sibling.
/// - **Missing parents are created by the atomic writer** (`ensure_parent_dir`),
///   so a write into a not-yet-materialized target team dir lands cleanly.
/// - The caller MUST have already validated `file_path` (`validate_context_path`).
pub(crate) fn suppressed_content_write(
    app: &tauri::AppHandle,
    file_path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    let was_create = file_path.try_exists().map(|exists| !exists).unwrap_or(true);

    let result = match app.try_state::<crate::commands::watch::WatcherState>() {
        Some(ws) => {
            let handle = ws.dispatcher.suppression_handle();
            crate::content_engine::write_tmp_fsync_rename_suppressed(file_path, bytes, &handle)
        }
        // WatcherState is always registered in the running app; the
        // un-suppressed fallback only applies in headless/test contexts.
        None => crate::content_engine::write_tmp_fsync_rename(file_path, bytes),
    };

    match result {
        Ok(()) => {
            propagate_suppressed_write(app, file_path, was_create);
            Ok(())
        }
        Err(e) => {
            error!(target: "files", "suppressed write failed for {}: {}", file_path.display(), e);
            Err(format!(
                "failed to write file {}: {}",
                file_path.display(),
                e
            ))
        }
    }
}

/// Atomically write UTF-8 `content` to a **worktree** file, **without**
/// watcher suppression (SLW-WDE-2).
///
/// The in-app code editor's save path. It is the deliberate counterpart to
/// [`write_content_file`]: same atomic `write_tmp_fsync_rename` durability, but
/// it **never acquires a `SuppressionHandle` and never calls
/// `propagate_suppressed_write`**. Its sandbox is
/// [`validate_worktree_path_owned`] — a **registered worktree root only**
/// (SLW-WDE-6), NOT the context-repo gate: the worktree files it edits live
/// outside `~/.codesense/users/` for an attached repo, and a worktree save
/// must never be able to land in the context repo. That non-suppression is the
/// whole point:
///
/// - `write_content_file` targets a **context-repo** path inside a watched
///   content dir, where an un-suppressed write would dispatch a watcher event
///   on every keystroke — so it suppresses.
/// - `write_worktree_file` targets a **story worktree** file that an agent is
///   editing live in the terminal. The edit MUST stay visible — to `git
///   status` and to the worktree watcher SLW-WDE-3 will attach — or the
///   two-writers reconciliation (WDE-3/WDE-4) has nothing to observe.
///   Suppressing it would re-introduce exactly the silent-second-writer bug
///   the sub-epic is built to avoid.
///
/// Conflict check: when the editor passes `expected_sha256` (the hash of the
/// content its buffer diverged from), a file whose bytes no longer match is
/// refused with a `conflict:`-prefixed error instead of being overwritten.
///
/// Symlink note: like `write_content_file`, the atomic rename replaces a
/// symlink dest with a regular file rather than writing through it.
#[tauri::command]
pub async fn write_worktree_file(
    path: String,
    content: String,
    expected_sha256: Option<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("write_worktree_file", move || {
        debug!(target: "files", "write_worktree_file: path={} bytes={}", path, content.len());
        // Writer gate: registered worktree root ONLY — a worktree save can never
        // land in the context repo (SLW-WDE-6 §C/RD-1). No `AppHandle` needed:
        // `validate_worktree_path` reads the index via `codesense_home_dir()`.
        let file_path = validate_worktree_path_owned(&path)?;
        // The watcher flags most external edits before a save, but it debounces;
        // checking the editor's baseline here closes that window so a save never
        // silently overwrites an agent's write that landed moments earlier.
        write_locked(&file_path, expected_sha256.as_deref(), || {
            write_worktree_bytes(&file_path, content.as_bytes())
        })
    })
    .await
}

/// Run `write` only if disk still holds the version `expected` names (the
/// editor's lock: a SHA-256 of the base text, or "absent"). Shared by the
/// worktree and context-file save commands; `None` writes unconditionally.
fn write_locked(
    file_path: &Path,
    expected: Option<&str>,
    write: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    crate::commands::worktree_editor::check_save_baseline(file_path, expected)?;
    write()
}

/// The worktree save's write: atomic and unsuppressed, keeping an existing
/// file's mode (the rename lands a fresh file, and an executable script that
/// lost `+x` would read to Git as a mode change).
fn write_worktree_bytes(file_path: &Path, content: &[u8]) -> Result<(), String> {
    // NON-suppressed on purpose (see `write_worktree_file`): a plain atomic
    // write so the edit is observable to `git status` and the WDE-3 watcher.
    // The file's mode (an executable bit) is set on the temp file before the
    // rename, so the worktree never shows a mode change for a content save.
    crate::content_engine::write_tmp_fsync_rename_keep_mode(file_path, content).map_err(|e| {
        error!(target: "files", "write_worktree_file failed for {}: {}", file_path.display(), e);
        format!("failed to write file {}: {}", file_path.display(), e)
    })
}

/// Atomically write base64-decoded **binary** `base64` bytes to a context-repo
/// file. The binary companion to [`write_content_file`] — same sandbox, same
/// atomic + watcher-suppressed write path, but for content that isn't UTF-8.
///
/// SW-SC-2's first caller is the Excalidraw PNG render (`exportToBlob` →
/// base64 → here), but it is deliberately format-agnostic so any future binary
/// content type reuses it. It is distinct from `export_pdf::write_binary_file`
/// (the PDF/screenshot exporter), which targets a user-picked path **outside**
/// the watched tree and is therefore un-suppressed; writing a derived sidecar
/// **inside** a watched content dir on every save must suppress the watcher or
/// it re-introduces the per-save watcher churn SW-SC-1 eliminated for the JSON
/// write.
#[tauri::command]
pub async fn write_binary_content_file(
    app: tauri::AppHandle,
    path: String,
    base64: String,
) -> Result<(), String> {
    crate::commands::await_blocking("write_binary_content_file", move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64.as_bytes())
            .map_err(|e| {
                error!(target: "files", "write_binary_content_file base64 decode failed for {}: {}", path, e);
                format!("invalid base64 for {}: {}", path, e)
            })?;
        debug!(target: "files", "write_binary_content_file: path={} bytes={}", path, bytes.len());
        let file_path = validate_context_write_path_owned(&path, &app)?;
        // Binary content is non-pipeline (images/canvas): the shared core
        // suppresses the watcher, writes atomically, and fires the FE nudge only
        // on CREATE (a new attached canvas/image to surface) — a redraw overwrite
        // stays silent (SW-FX-26 `should_emit_fe_nudge`). Same entry point as
        // `write_content_file`.
        write_context_bytes_suppressed(&app, &file_path, &bytes)
    })
    .await
}

/// SW-VE-7: watcher-suppressed atomic write of raw `bytes` to an already-validated
/// context-layer `file_path`, then the create/overwrite FE-nudge propagation —
/// the shared core behind both [`write_binary_content_file`] (base64 sidecar) and
/// [`ingest_file_into_context`] (dropped OS file). Suppresses the watcher in the
/// running app; falls back to a plain atomic write in headless/test contexts
/// (no `WatcherState`), matching `write_content_file`.
fn write_context_bytes_suppressed(
    app: &tauri::AppHandle,
    file_path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    // Capture create-vs-overwrite before the atomic rename (SW-FX-26 gate).
    // Fail-safe to create on stat error — see `write_content_file`.
    let was_create = file_path.try_exists().map(|exists| !exists).unwrap_or(true);
    let result = match app.try_state::<crate::commands::watch::WatcherState>() {
        Some(ws) => {
            let handle = ws.dispatcher.suppression_handle();
            crate::content_engine::write_tmp_fsync_rename_suppressed(file_path, bytes, &handle)
        }
        // WatcherState is always registered in the running app; the un-suppressed
        // fallback only applies in headless/test contexts.
        None => crate::content_engine::write_tmp_fsync_rename(file_path, bytes),
    };
    match result {
        Ok(()) => {
            propagate_suppressed_write(app, file_path, was_create);
            Ok(())
        }
        Err(e) => {
            error!(target: "files", "context byte-write failed for {}: {}", file_path.display(), e);
            Err(format!(
                "failed to write file {}: {}",
                file_path.display(),
                e
            ))
        }
    }
}

/// SW-CP-14: resolve the watcher suppression handle + frontmatter cache from
/// app state for a structural source op. Both are `Some` in the running app
/// (always registered); `None` only in headless/test contexts, where the
/// source op runs unsuppressed (matching `write_content_file`'s fallback).
#[allow(clippy::type_complexity)]
fn structural_op_deps<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> (
    Option<crate::content_engine::SuppressionHandle>,
    Option<crate::install::frontmatter_cache::SharedFrontmatterCache>,
) {
    let suppression = app
        .try_state::<crate::commands::watch::WatcherState>()
        .map(|ws| ws.dispatcher.suppression_handle());
    let cache = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .map(|c| std::sync::Arc::clone(c.inner()));
    (suppression, cache)
}

/// SW-CP-14: a structural-op target must be a context-layer content path
/// (`_team/{team}/content/...`). v1 scope is **source-initiated** — the path
/// the nav edits and the path the EW/PT restructuring touched. A dest-side
/// (`_bmad-output`) explicit gesture maps to its source via the reverse
/// resolver and is a documented follow-up (AC-4 second half).
fn require_context_content_path(path: &Path) -> Result<(), String> {
    let Some(path_str) = path.to_str() else {
        return Err("invalid (non-UTF-8) path".to_string());
    };
    if crate::install::capture::parse_context_repo_parts(path_str).is_none() {
        return Err(
            "structural op target must be a context-layer content path (_team/{team}/content/…)"
                .to_string(),
        );
    }
    Ok(())
}

/// SW-CP-14: shared scaffold for the structural gesture commands. Runs the
/// caller's `build` (path validation + `StructuralChange` construction) on the
/// blocking pool, then applies the watcher-suppressed source op + cache
/// reconcile. SW-XS-3 (F13): the change is NOT fanned out to the projected
/// copies in open workspaces — nothing writes into a workspace on its own; an
/// explicit operation does.
/// `delete_content_file` / `move_content_file` differ ONLY in
/// `build`; everything downstream is identical, so it lives here.
async fn run_structural_gesture<R: tauri::Runtime, F>(
    app: tauri::AppHandle<R>,
    label: &'static str,
    build: F,
) -> Result<(), String>
where
    F: FnOnce(
            &tauri::AppHandle<R>,
        ) -> Result<crate::install::story_linker::StructuralChange, String>
        + Send
        + 'static,
{
    let change = {
        let app = app.clone();
        crate::commands::await_blocking(label, move || {
            let change = build(&app)?;
            let (suppression, cache) = structural_op_deps(&app);
            crate::install::story_linker::apply_structural_source_op(
                &change,
                suppression.as_ref(),
                cache.as_ref(),
            )
            .map_err(|e| format!("{}: {}", label, e))?;
            Ok::<_, String>(change)
        })
        .await?
    };
    // SW-FX-26: nudge the FE nav so the structural change (moved-in sibling /
    // removed node) surfaces live, mirroring the content-writer path. Emitted
    // for the SOURCE change — the nav scans the team content source tree.
    nudge_structural_change(&app, &change);
    Ok(())
}

/// SW-CP-14 (AC-4/AC-5/D5): delete a context-layer content file or directory.
/// Git-backed (D6): deletes unconditionally — recovery is `git revert`.
/// SW-XS-3 (F13): workspace copies are left alone.
#[tauri::command]
pub async fn delete_content_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<(), String> {
    run_structural_gesture(app, "delete_content_file", move |app| {
        debug!(target: "files", "delete_content_file: path={}", path);
        let p = validate_context_write_path_owned(&path, app)?;
        require_context_content_path(&p)?;
        let is_dir = p.is_dir();
        Ok(crate::install::story_linker::StructuralChange::Delete {
            source_path: p,
            is_dir,
        })
    })
    .await
}

/// SW-CP-14 (AC-1/AC-2/AC-5/D5): rename/move a context-layer content file or
/// directory, preserving content + frontmatter (incl. `story_id`). SW-XS-3
/// (F13): workspace copies are left alone. The explicit two-path gesture carries `from`+`to` atomically, so no
/// delete+create→rename correlation heuristic is needed (D1).
#[tauri::command]
pub async fn move_content_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    from_path: String,
    to_path: String,
) -> Result<(), String> {
    run_structural_gesture(app, "move_content_file", move |app| {
        debug!(target: "files", "move_content_file: {} -> {}", from_path, to_path);
        let from = validate_context_write_path_owned(&from_path, app)?;
        let to = validate_context_write_path_owned(&to_path, app)?;
        require_context_content_path(&from)?;
        require_context_content_path(&to)?;
        if !from.exists() {
            return Err(format!("source path does not exist: {}", from_path));
        }
        if to.exists() {
            return Err(format!("target already exists: {}", to_path));
        }
        let is_dir = from.is_dir();
        Ok(crate::install::story_linker::StructuralChange::Rename { from, to, is_dir })
    })
    .await
}

#[tauri::command]
pub async fn list_directory<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    dir_path: String,
) -> Result<Vec<FileEntry>, String> {
    crate::commands::await_blocking("list_directory", move || {
        debug!(target: "files", "list_directory: path={}", dir_path);
        // Reader gate: context-repo OR registered worktree root (SLW-WDE-6 §C).
        // This is THE first call the worktree tree makes on mount
        // (`list_directory(ws.cwd)`) — it MUST accept an attached-repo worktree
        // path or the editor never loads. Shared with the context nav, which
        // keeps working via the context half of the OR (cheap-first, §E).
        editor_read_fs(&app).list(Path::new(&dir_path))
    })
    .await
}

/// A team's landing page: the first of `candidates` that is a regular file
/// really inside `content_root` (symlinks resolved), so the viewer can open it on
/// its own. Gated like the context nav (`content_root` must be a context path),
/// and the content folder's real path must stay inside that context root.
#[tauri::command]
pub async fn resolve_team_landing_page(
    app: tauri::AppHandle,
    content_root: String,
    candidates: Vec<crate::commands::team_landing::LandingCandidate>,
) -> Result<crate::commands::team_landing::LandingPage, String> {
    crate::commands::await_blocking("resolve_team_landing_page", move || {
        let Some(context_root) = matched_context_root(Path::new(&content_root), &app) else {
            return Err(format!("not a context path: {content_root}"));
        };
        Ok(crate::commands::team_landing::resolve(
            &content_root,
            &candidates,
            &context_root,
        ))
    })
    .await
}

#[tauri::command]
pub async fn create_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
    content: String,
    page_path: Option<String>,
    frame_id: Option<String>,
) -> Result<(), String> {
    let scope = crate::context_repo::caller_scope::for_management_view(
        &app,
        page_path.as_deref(),
        frame_id.as_deref(),
    )?;
    let readable = scope.allows(std::path::Path::new(&path));
    let result = crate::commands::await_blocking("create_file", move || {
        debug!(target: "files", "create_file: path={}", path);
        let file_path = validate_context_write_path_owned(&path, &app)?;

        if file_path.exists() {
            return Err(format!("File already exists: {}", path));
        }

        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                error!(target: "files", "create_file failed to create parent dir for {}: {}", path, e);
                format!("failed to create parent dir: {}", e)
            })?;
        }

        fs::write(&file_path, &content).map_err(|e| {
            error!(target: "files", "create_file failed for {}: {}", path, e);
            format!("failed to create file: {}", e)
        })?;

        info!(target: "files", "created file: {}", path);
        Ok(())
    })
    .await;
    crate::context_repo::caller_scope::readback(readable, result)
}

/// SW-VE-7: cap on a single drag-dropped file. Generous enough for images and
/// docs (the viewable content types) while rejecting an accidental huge drop
/// (a video / disk image) that would bloat the git-backed context repo. This is
/// a per-file limit (not an aggregate read budget).
pub(crate) const MAX_INGEST_BYTES: u64 = 25 * 1024 * 1024; // 25 MB

/// SW-VE-7: pure source-side guards for a dropped-file ingest — the checks that
/// need no `AppHandle`, so they are unit-testable. Rejects a missing file, a
/// directory (v1 is single-file — recursive folder ingest is deferred), and an
/// oversized file, with the same human-readable messages the command surfaces
/// to the FE (AC-5 quiet notices).
fn validate_ingest_source(src: &Path, max_bytes: u64) -> Result<(), String> {
    let meta = fs::metadata(src).map_err(|e| format!("cannot read dropped file: {}", e))?;
    if meta.is_dir() {
        return Err("Folders can't be added yet — drop a single file.".to_string());
    }
    if meta.len() > max_bytes {
        return Err(format!(
            "File is too large to add (max {} MB).",
            max_bytes / (1024 * 1024)
        ));
    }
    Ok(())
}

/// SW-VE-7: ingest an OS file dropped on the context nav. Reads `src_path` (an
/// absolute path OUTSIDE the managed tree) on the blocking pool and writes it
/// into `dest_path` (a validated context content path) through the same
/// watcher-suppressed + propagated core as `write_binary_content_file`. A raw
/// byte copy → binary- and text-safe (no UTF-8 / base64 round-trip) and
/// non-blocking (no file bytes cross the IPC boundary — only two path strings
/// do). Guards are typed so the FE renders one quiet notice per rejection class:
/// a folder drop, an oversized file, a name collision (`already exists` — the FE
/// resolves it as a non-destructive Keep-both), or a dest outside the managed
/// content tree. The `require_context_content_path` gate also CLOSES the gap
/// `create_file` lacks (SW-VE-6 backend note) for the ingest entry point.
#[tauri::command]
pub async fn ingest_file_into_context(
    app: tauri::AppHandle,
    src_path: String,
    dest_path: String,
) -> Result<(), String> {
    crate::commands::await_blocking("ingest_file_into_context", move || {
        debug!(target: "files", "ingest_file_into_context: {} -> {}", src_path, dest_path);
        // Dest gate: a validated context-layer content path — a dropped file can
        // only land in the team content tree, never elsewhere on disk.
        let dest = validate_context_write_path_owned(&dest_path, &app)?;
        require_context_content_path(&dest)?;
        // Source guards (typed for quiet FE notices — AC-5) BEFORE the collision
        // check, so a folder / oversized drop reports its own cause rather than a
        // misleading "already exists" when the basename happens to collide.
        let src = Path::new(&src_path);
        validate_ingest_source(src, MAX_INGEST_BYTES)?;
        if dest.exists() {
            // Match `create_file`'s wording so the FE's `isCollision`
            // (/already exists/i) resolves it as a non-destructive Keep-both.
            return Err(format!("File already exists: {}", dest_path));
        }
        // Ensure the destination parent exists (mirrors `create_file`).
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create parent dir: {}", e))?;
        }
        let bytes = fs::read(src).map_err(|e| format!("cannot read dropped file: {}", e))?;
        write_context_bytes_suppressed(&app, &dest, &bytes)?;
        info!(target: "files", "ingested file: {} -> {}", src_path, dest_path);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn rename_path(
    app: tauri::AppHandle,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    crate::commands::await_blocking("rename_path", move || {
        debug!(target: "files", "rename_path: old={}, new={}", old_path, new_path);
        let old = validate_context_write_path_owned(&old_path, &app)?;
        let new = validate_context_write_path_owned(&new_path, &app)?;

        if !old.exists() {
            return Err(format!("Path does not exist: {}", old_path));
        }
        if new.exists() {
            return Err(format!("Target already exists: {}", new_path));
        }

        fs::rename(&old, &new).map_err(|e| {
            error!(target: "files", "rename_path failed from {} to {}: {}", old_path, new_path, e);
            format!("failed to rename: {}", e)
        })?;

        info!(target: "files", "renamed path: {} -> {}", old_path, new_path);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn delete_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    crate::commands::await_blocking("delete_path", move || {
        debug!(target: "files", "delete_path: path={}", path);
        let target = validate_context_write_path_owned(&path, &app)?;

        if !target.exists() {
            return Ok(()); // Idempotent
        }

        let result = if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|e| {
                error!(target: "files", "delete_path failed for directory {}: {}", path, e);
                format!("failed to delete directory: {}", e)
            })
        } else {
            fs::remove_file(&target).map_err(|e| {
                error!(target: "files", "delete_path failed for file {}: {}", path, e);
                format!("failed to delete file: {}", e)
            })
        };

        if result.is_ok() {
            info!(target: "files", "deleted path: {}", path);
        }
        result
    })
    .await
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_write_gate_refuses_the_codesense_docs_even_through_dot_dot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = crate::domain::Principal::from(9_u64);
        let archive = include_bytes!("../../resources/codesense-docs.tar.gz");
        let placed =
            crate::context_repo::codesense_docs::ensure_placed(&home, &who, archive).unwrap();
        let root = &placed.attachment.root;
        let message = crate::context_repo::codesense_docs::READ_ONLY_MESSAGE;
        assert_eq!(
            refuse_read_only_context_write(&placed.readme_path).unwrap_err(),
            message
        );
        let sneaky = root.join("getting-started/content/handbook/../new.md");
        assert_eq!(
            refuse_read_only_context_write(&sneaky).unwrap_err(),
            message
        );
        refuse_read_only_context_write(&temp.path().join("elsewhere/a.md")).unwrap();
    }
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("codesense_test").join(format!(
            "{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── SW-DX-11: read_workspace_file_core gates (AC-1..4, AC-9) ──────────────

    /// A doc dir under `{home}/users/…` (a managed surface) + the doc file path.
    fn managed_doc(name: &str) -> (PathBuf, PathBuf, String) {
        let home = test_dir(name).canonicalize().unwrap();
        let doc_dir = home.join("users").join("1234567").join("dash");
        fs::create_dir_all(&doc_dir).unwrap();
        let doc_path = doc_dir.join("sync.html").to_string_lossy().into_owned();
        (home, doc_dir, doc_path)
    }

    fn rw_exts() -> Vec<String> {
        ["md", "json", "yaml", "txt"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn read_workspace_reads_managed_file() {
        let (home, doc_dir, doc_path) = managed_doc("rw_read");
        fs::write(doc_dir.join("DEMO-1.md"), "hello").unwrap();
        let r = read_workspace_file_core(&doc_path, "./DEMO-1.md", &rw_exts(), &home).unwrap();
        assert_eq!(r.content.as_deref(), Some("hello"));
        assert!(r.entries.is_none());
        cleanup(&home);
    }

    #[test]
    fn read_workspace_lists_dir_sorted() {
        let (home, doc_dir, doc_path) = managed_doc("rw_list");
        fs::write(doc_dir.join("b.md"), "b").unwrap();
        fs::write(doc_dir.join("a.md"), "a").unwrap();
        fs::create_dir_all(doc_dir.join("sub")).unwrap();
        let r = read_workspace_file_core(&doc_path, "./", &rw_exts(), &home).unwrap();
        let entries = r.entries.unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert!(
            names.windows(2).all(|w| w[0] <= w[1]),
            "not sorted: {names:?}"
        );
        assert!(names.contains(&"a.md".to_string()));
        assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));
        cleanup(&home);
    }

    #[test]
    fn read_workspace_rejects_parent_traversal() {
        let (home, _d, doc_path) = managed_doc("rw_parent");
        let e = read_workspace_file_core(&doc_path, "../secret.md", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("traversal:"), "{e}");
        cleanup(&home);
    }

    #[test]
    fn read_workspace_rejects_absolute() {
        let (home, _d, doc_path) = managed_doc("rw_abs");
        let e = read_workspace_file_core(&doc_path, "/etc/passwd", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("traversal:"), "{e}");
        cleanup(&home);
    }

    #[cfg(unix)]
    #[test]
    fn read_workspace_rejects_symlink_escape() {
        let (home, doc_dir, doc_path) = managed_doc("rw_symlink");
        // A secret OUTSIDE the doc dir; a symlink INSIDE the doc dir points at it.
        let secret = home.join("secret.txt");
        fs::write(&secret, "TOP SECRET").unwrap();
        std::os::unix::fs::symlink(&secret, doc_dir.join("leak.txt")).unwrap();
        let e = read_workspace_file_core(&doc_path, "./leak.txt", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("traversal:"), "{e}");
        cleanup(&home);
    }

    #[test]
    fn read_workspace_rejects_unmanaged_dir() {
        // Doc dir NOT under `{home}/users/` — a non-managed surface.
        let base = test_dir("rw_unmanaged").canonicalize().unwrap();
        let doc_dir = base.join("random").join("dash");
        fs::create_dir_all(&doc_dir).unwrap();
        fs::write(doc_dir.join("x.md"), "x").unwrap();
        let doc_path = doc_dir.join("sync.html").to_string_lossy().into_owned();
        let e = read_workspace_file_core(&doc_path, "./x.md", &rw_exts(), &base).unwrap_err();
        assert!(e.starts_with("unmanaged:"), "{e}");
        cleanup(&base);
    }

    #[test]
    fn read_workspace_rejects_unsupported_ext() {
        let (home, doc_dir, doc_path) = managed_doc("rw_ext");
        fs::write(doc_dir.join("tool.exe"), "MZ").unwrap();
        let e = read_workspace_file_core(&doc_path, "./tool.exe", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("unsupported-type:"), "{e}");
        cleanup(&home);
    }

    #[test]
    fn read_workspace_rejects_too_large() {
        let (home, doc_dir, doc_path) = managed_doc("rw_large");
        let big = vec![b'a'; (MAX_WORKSPACE_READ_BYTES + 1) as usize];
        fs::write(doc_dir.join("big.txt"), &big).unwrap();
        let e = read_workspace_file_core(&doc_path, "./big.txt", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("too-large:"), "{e}");
        cleanup(&home);
    }

    #[test]
    fn read_workspace_rejects_binary() {
        let (home, doc_dir, doc_path) = managed_doc("rw_binary");
        fs::write(doc_dir.join("data.txt"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        let e = read_workspace_file_core(&doc_path, "./data.txt", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("binary:"), "{e}");
        cleanup(&home);
    }

    #[test]
    fn read_workspace_not_found() {
        let (home, _d, doc_path) = managed_doc("rw_missing");
        let e = read_workspace_file_core(&doc_path, "./nope.md", &rw_exts(), &home).unwrap_err();
        assert!(e.starts_with("not-found:"), "{e}");
        cleanup(&home);
    }

    // -- SW-FX-26: FE-nudge gate (`should_emit_fe_nudge`) --

    #[test]
    fn nudge_on_create_of_non_pipeline_canvas() {
        // apply_canvas creating a new `.excalidraw`: the case the old
        // `is_pipeline` early-return dropped entirely.
        assert!(should_emit_fe_nudge(Path::new("/c/foo.excalidraw"), true));
    }

    #[test]
    fn nudge_on_pipeline_overwrite() {
        // Attaching `supporting_docs:` rewrites an existing story `.md`
        // (overwrite) — frontmatter changed, badge must surface.
        assert!(should_emit_fe_nudge(Path::new("/c/story.md"), false));
    }

    #[test]
    fn no_nudge_on_non_pipeline_overwrite() {
        // The ~1/sec canvas redraw (and its `.excalidraw.png` sidecar): an
        // overwrite of a BINARY-redraw file changes nothing nav-relevant, so it
        // stays silent — no scan churn (SW-SC-1's whole point). SW-VE-4 preserves
        // this exactly (the exclusion is the binary set, not "non-pipeline").
        assert!(!should_emit_fe_nudge(Path::new("/c/foo.excalidraw"), false));
        assert!(!should_emit_fe_nudge(
            Path::new("/c/foo.excalidraw.png"),
            false
        ));
    }

    // -- SW-VE-7: drag-drop ingest source guards + dest gate --

    #[test]
    fn ingest_source_accepts_a_normal_file() {
        let dir = test_dir("ingest_ok");
        let f = dir.join("drop.md");
        fs::write(&f, b"# hi").unwrap();
        assert!(validate_ingest_source(&f, MAX_INGEST_BYTES).is_ok());
        cleanup(&dir);
    }

    #[test]
    fn ingest_source_rejects_a_directory() {
        let dir = test_dir("ingest_dir");
        let err = validate_ingest_source(&dir, MAX_INGEST_BYTES).unwrap_err();
        assert!(err.contains("Folders"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn ingest_source_rejects_oversized() {
        let dir = test_dir("ingest_big");
        let f = dir.join("big.bin");
        fs::write(&f, vec![0u8; 64]).unwrap();
        // Tiny cap forces the oversized branch deterministically.
        let err = validate_ingest_source(&f, 16).unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn ingest_source_rejects_missing_file() {
        let dir = test_dir("ingest_missing");
        let err = validate_ingest_source(&dir.join("nope.md"), MAX_INGEST_BYTES).unwrap_err();
        assert!(err.contains("cannot read"), "got: {err}");
        cleanup(&dir);
    }

    #[test]
    fn ingest_dest_gate_rejects_unmanaged_path() {
        // Reuses the same gate move/delete enforce — a dest outside a
        // `_team/{team}/content/…` tree is refused (closes create_file's gap).
        assert!(require_context_content_path(Path::new("/tmp/not-context/x.md")).is_err());
    }

    #[test]
    fn nudge_on_editable_text_overwrite_ve4() {
        // SW-VE-4: an agent overwriting a file a human may have open in the context
        // editor MUST fire the FE nudge (the editor's concurrent-write signal).
        // These were silent before VE-4 — the gap it closes. Enumerated config/code
        // types...
        assert!(should_emit_fe_nudge(Path::new("/c/config.yaml"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/config.yml"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/data.toml"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/x.json"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/x.xml"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/deploy.sh"), false));
        // ...AND the VE-2 catch-all editable set the enumerated list would miss:
        // unknown text extensions and EXTENSIONLESS files (Dockerfile / LICENSE)
        // are editable `code` in the viewer, so their overwrite must nudge too or a
        // save silently clobbers the agent (the code-review M1 gap).
        assert!(should_emit_fe_nudge(Path::new("/c/notes.txt"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/app.conf"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/Dockerfile"), false));
        assert!(should_emit_fe_nudge(Path::new("/c/LICENSE"), false));
    }

    #[test]
    fn no_nudge_on_binary_redraw_overwrite_ve4() {
        // SW-FX-26 preserved: canvas/image redraws stay silent (rendered binary,
        // not text a human edits) — the ONLY overwrites that don't nudge.
        assert!(!should_emit_fe_nudge(Path::new("/c/pic.png"), false));
        assert!(!should_emit_fe_nudge(Path::new("/c/pic.svg"), false));
        assert!(!should_emit_fe_nudge(
            Path::new("/c/board.excalidraw"),
            false
        ));
        assert!(!should_emit_fe_nudge(
            Path::new("/c/board.excalidraw.png"),
            false
        ));
        // ...but a CREATE of any of them still nudges (a new nav sibling).
        assert!(should_emit_fe_nudge(Path::new("/c/pic.png"), true));
    }

    #[test]
    fn is_pipeline_file_detects_md_html_only() {
        assert!(is_pipeline_file(Path::new("/c/a.md")));
        assert!(is_pipeline_file(Path::new("/c/a.html")));
        assert!(!is_pipeline_file(Path::new("/c/a.excalidraw")));
        assert!(!is_pipeline_file(Path::new("/c/a.png")));
        assert!(!is_pipeline_file(Path::new("/c/noext")));
    }

    // -- SLW-WDE-6: worktree editor path sandbox --
    //
    // The worktree-editor gate is `validate_worktree_path`, whose env-dependent
    // half (`codesense_home_dir()` → `worktree_roots`) is covered by the
    // scan+decide test below over an explicit temp home; its pure allow-list
    // decision (`path_within_any_root`) — the AC-2 traversal/deny matrix — is
    // covered in `codesense_fs_core::gate` without any real `$HOME`.

    #[test]
    fn validate_worktree_path_scan_admits_registered_root_denies_rest() {
        use crate::install::worktree_index::{self, WorktreeStoryRecord};

        // A temp codesense home + a worktree that lives OUTSIDE it (the
        // attached-repo case: a sibling of the user's clone, e.g.
        // `~/dev/acme-api-wt/feat-…`).
        let home = test_dir("wde6_home");
        let worktree = test_dir("wde6_worktree");

        // Register it exactly as `establish_worktree_story_link` does: one
        // record whose `code_repo_path` IS the worktree cwd.
        let slug = worktree_index::slug_from_source_repo("acme/api").unwrap();
        let record = WorktreeStoryRecord::new(
            &worktree,
            "acme-api",
            slug,
            "SLW-WDE-6",
            worktree.join("story.md"),
        );
        worktree_index::write_record(&home, &crate::domain::Principal::from(42u64), &record)
            .unwrap();

        // The uid-agnostic scan surfaces the recorded root…
        let roots = worktree_index::worktree_roots(&home);
        assert!(
            roots.iter().any(|r| r == &worktree),
            "worktree_roots must include the recorded code_repo_path; got {roots:?}"
        );

        // …and the allow-list decision admits inside, denies outside + traversal.
        assert!(path_within_any_root(&worktree.join("src/main.rs"), &roots));
        assert!(!path_within_any_root(Path::new("/etc/passwd"), &roots));
        assert!(!path_within_any_root(
            &worktree.join("../../etc/passwd"),
            &roots
        ));

        cleanup(&home);
        cleanup(&worktree);
    }

    // -- the composite reader gate behind list_directory/read_file --

    const EDITOR_READ_DENIED: &str =
        "Access denied: path is outside the context repo and any registered worktree";

    fn editor_read_fixture(
        context: &Path,
        worktree: &Path,
    ) -> EditorReadFs<impl Fn(&Path) -> bool, impl Fn(&Path) -> bool> {
        let context_roots = vec![context.to_path_buf()];
        let worktree_roots = vec![worktree.to_path_buf()];
        EditorReadFs {
            in_context: move |p: &Path| path_within_any_root(p, &context_roots),
            in_worktree: move |p: &Path| path_within_any_root(p, &worktree_roots),
        }
    }

    #[test]
    fn editor_read_gate_admits_context_and_worktree_paths_denies_the_rest() {
        let context = test_dir("editor_read_ctx");
        let worktree = test_dir("editor_read_wt");
        let outside = test_dir("editor_read_outside");
        fs::write(context.join("story.md"), "ctx").unwrap();
        fs::write(worktree.join("main.rs"), "wt").unwrap();
        fs::write(outside.join("secret.md"), "nope").unwrap();
        let service = editor_read_fixture(&context, &worktree);

        assert_eq!(service.read(&context.join("story.md")).unwrap(), "ctx");
        assert_eq!(service.read(&worktree.join("main.rs")).unwrap(), "wt");
        assert_eq!(service.list(&worktree).unwrap()[0].name, "main.rs");

        assert_eq!(
            service.read(&outside.join("secret.md")).unwrap_err(),
            EDITOR_READ_DENIED
        );
        assert_eq!(service.list(&outside).unwrap_err(), EDITOR_READ_DENIED);
        assert_eq!(
            service.list(&worktree.join("..")).unwrap_err(),
            EDITOR_READ_DENIED
        );

        cleanup(&context);
        cleanup(&worktree);
        cleanup(&outside);
    }

    #[test]
    fn editor_read_gate_skips_the_worktree_scan_for_a_context_path() {
        let context = test_dir("editor_read_cheap_first");
        let scanned = std::cell::Cell::new(false);
        let service = EditorReadFs {
            in_context: |p: &Path| p.starts_with(&context),
            in_worktree: |_: &Path| {
                scanned.set(true);
                false
            },
        };

        assert!(service.admit(&context.join("a.md")).is_ok());
        assert!(
            !scanned.get(),
            "a context hit must not pay the worktree scan"
        );
        assert!(service.admit(Path::new("/elsewhere/a.md")).is_err());
        assert!(
            scanned.get(),
            "a context miss falls through to the worktree scan"
        );

        cleanup(&context);
    }

    // -- write_content_file write-path contract (SW-SC-1 AC-4) --
    //
    // The command itself is `validate_context_path_owned` (AppHandle-gated,
    // not unit-testable without a mock app — same as `read_file`) composed
    // with `write_tmp_fsync_rename`. These tests pin the content-fidelity
    // guarantee the canvas relies on: a `.excalidraw` scene JSON survives a
    // write→read round-trip byte-for-byte, and a re-save fully replaces the
    // prior scene (never appends / never leaves a half-written file).

    #[test]
    fn locked_writes_refuse_a_version_the_editor_never_showed() {
        use crate::commands::worktree_editor::{ABSENT_TOKEN, CONFLICT_PREFIX};
        use crate::shared::hash::sha256_hex;
        let dir = test_dir("write_locked");
        let path = dir.join("note.md");
        fs::write(&path, "agent wrote this").unwrap();
        let attempt = |expected: Option<&str>| {
            write_locked(&path, expected, || {
                crate::content_engine::write_tmp_fsync_rename(&path, b"mine")
                    .map_err(|e| e.to_string())
            })
        };
        // Locked to the version the editor read before the agent's write.
        let err = attempt(Some(&sha256_hex(b"what I opened"))).unwrap_err();
        assert!(err.starts_with(CONFLICT_PREFIX), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "agent wrote this");
        // "absent" refuses a file that exists.
        assert!(attempt(Some(ABSENT_TOKEN)).is_err());
        // Locked to what is there: written.
        attempt(Some(&sha256_hex(b"agent wrote this"))).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
        // "absent" creates a file that is gone.
        fs::remove_file(&path).unwrap();
        attempt(Some(ABSENT_TOKEN)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
        cleanup(&dir);
    }

    #[test]
    fn write_content_file_round_trips_excalidraw_json_losslessly() {
        let dir = test_dir("write_content_roundtrip");
        let path = dir.join("sketch.excalidraw");
        let scene = r##"{"type":"excalidraw","version":2,"source":"local","elements":[{"id":"a1","type":"rectangle","x":10,"y":20}],"appState":{"viewBackgroundColor":"#ffffff"},"files":{}}"##;

        crate::content_engine::write_tmp_fsync_rename(&path, scene.as_bytes()).unwrap();

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, scene, "scene JSON must round-trip byte-for-byte");

        cleanup(&dir);
    }

    #[test]
    fn write_content_file_overwrites_prior_scene_not_appends() {
        let dir = test_dir("write_content_overwrite");
        let path = dir.join("sketch.excalidraw");
        let first = r#"{"type":"excalidraw","elements":[{"id":"old"}]}"#;
        let second = r#"{"type":"excalidraw","elements":[]}"#;

        crate::content_engine::write_tmp_fsync_rename(&path, first.as_bytes()).unwrap();
        crate::content_engine::write_tmp_fsync_rename(&path, second.as_bytes()).unwrap();

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, second, "re-save must replace, not append");
        // No tmp sibling left behind after a successful atomic write.
        let leftover: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("sketch.excalidraw.tmp")
            })
            .collect();
        assert!(
            leftover.is_empty(),
            "no .tmp sibling should remain: {:?}",
            leftover
        );

        cleanup(&dir);
    }

    // -- write_worktree_file write-path contract (SLW-WDE-2 AC-3/RD-1) --
    //
    // Like the other writers, the command is AppHandle-gated (validate_context_
    // path_owned) and not unit-testable without a mock app. What is unique here
    // and worth pinning is the NON-suppressed atomic mechanic: the command
    // calls `write_tmp_fsync_rename` directly — it never acquires a
    // SuppressionHandle and never calls propagate_suppressed_write — so the
    // worktree edit stays observable to `git status` and the WDE-3 watcher.
    // These pin the same content-fidelity guarantee the editor buffer relies
    // on (byte-for-byte round-trip, full overwrite, no tmp sibling left).

    #[cfg(unix)]
    #[test]
    fn write_worktree_bytes_keeps_an_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("write_worktree_mode");
        let path = dir.join("run.sh");
        fs::write(&path, "echo 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        write_worktree_bytes(&path, b"echo 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "echo 2\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        cleanup(&dir);
    }

    #[test]
    fn write_worktree_file_round_trips_source_text_losslessly() {
        let dir = test_dir("write_worktree_roundtrip");
        let path = dir.join("main.rs");
        let source = "fn main() {\n    println!(\"héllo, 世界\");\n}\n";

        // The exact mechanic write_worktree_file uses (unsuppressed).
        crate::content_engine::write_tmp_fsync_rename(&path, source.as_bytes()).unwrap();

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(
            read_back, source,
            "source text must round-trip byte-for-byte"
        );

        cleanup(&dir);
    }

    #[test]
    fn write_worktree_file_overwrites_prior_content_no_tmp_leftover() {
        let dir = test_dir("write_worktree_overwrite");
        let path = dir.join("edit.ts");
        let first = "export const a = 1;\n";
        let second = "export const a = 2;\n";

        crate::content_engine::write_tmp_fsync_rename(&path, first.as_bytes()).unwrap();
        crate::content_engine::write_tmp_fsync_rename(&path, second.as_bytes()).unwrap();

        let read_back = fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, second, "re-save must replace, not append");
        // Atomic write leaves no tmp sibling behind.
        let leftover: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("edit.ts.tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "no .tmp sibling should remain: {:?}",
            leftover
        );

        cleanup(&dir);
    }

    // -- write_binary_content_file write-path contract (SW-SC-2 AC-3/AC-6) --
    //
    // Like the text writer, the command is AppHandle-gated; these pin the
    // binary-specific guarantee: base64 decodes to the exact source bytes and
    // a PNG sidecar round-trips through the atomic write byte-for-byte (so the
    // render the agent reads is never truncated/corrupted).

    #[test]
    fn write_binary_content_file_round_trips_png_bytes_losslessly() {
        let dir = test_dir("write_binary_roundtrip");
        let path = dir.join("sketch.excalidraw.png");
        // A minimal PNG header (\x89PNG\r\n\x1a\n) — non-UTF-8 bytes that the
        // text writer could not carry.
        let png_bytes: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff, 0x10,
        ];
        let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes);

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap();
        assert_eq!(decoded, png_bytes, "base64 must decode to the source bytes");

        crate::content_engine::write_tmp_fsync_rename(&path, &decoded).unwrap();

        let read_back = fs::read(&path).unwrap();
        assert_eq!(read_back, png_bytes, "PNG bytes must round-trip exactly");

        cleanup(&dir);
    }

    #[test]
    fn write_binary_content_file_rejects_invalid_base64() {
        let bad = "not%%%base64!!!";
        let result = base64::engine::general_purpose::STANDARD.decode(bad.as_bytes());
        assert!(
            result.is_err(),
            "malformed base64 must be rejected, not written"
        );
    }
}

/// SW-OC-CANVAS: tests for the `apply_canvas` / `render_canvas` write/read gate
/// widening — accept paths under a registered **source-repo attachment** root
/// AND a registered **worktree** root, while still refusing writes into the
/// read-only bundled CodeSense docs. See [`validate_canvas_write_path_owned`]
/// and [`validate_canvas_read_path`] for the production decision; these pure
/// tests cover the composable [`source_repo_roots_at`] / [`path_within_any_root`]
/// decision core without touching the real `$HOME`.
#[cfg(test)]
mod canvas_gate_tests {
    use super::*;
    use crate::domain::{Attachment, Attachments};
    use codesense_fs_core::gate::{normalize_path_components, path_within_any_root};

    fn seed_attachments(home: &Path, principal_id: u64, repo_paths: &[&str]) {
        let user_dir = home
            .join(crate::context_repo::USERS_DIR)
            .join(principal_id.to_string());
        fs::create_dir_all(&user_dir).unwrap();
        let data = Attachments {
            attachments: repo_paths
                .iter()
                .map(|p| Attachment {
                    repo_path: (*p).to_string(),
                    remote_url: None,
                    attached_at: None,
                    tracking_prompt_dismissed_at: None,
                    credential_ref: None,
                    managed: false,
                })
                .collect(),
        };
        let path = user_dir.join(crate::context_repo::ATTACHMENTS_FILE);
        crate::context_repo::attachments_io::write_attachments_atomic(&path, &data).unwrap();
    }

    #[test]
    fn source_repo_roots_at_enumerates_every_user_attachment() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        seed_attachments(&home, 1, &["/Users/dev/alpha", "/Users/dev/beta"]);
        seed_attachments(&home, 2, &["/Users/dev/gamma"]);

        let roots = source_repo_roots_at(&home);
        let as_strs: Vec<String> = roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(as_strs.iter().any(|p| p == "/Users/dev/alpha"));
        assert!(as_strs.iter().any(|p| p == "/Users/dev/beta"));
        assert!(as_strs.iter().any(|p| p == "/Users/dev/gamma"));
        assert_eq!(as_strs.len(), 3);
    }

    #[test]
    fn source_repo_roots_at_is_empty_when_no_attachments_dir() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("empty_home");
        // No users/ dir written at all — the walk must return [], not panic.
        let roots = source_repo_roots_at(&home);
        assert!(roots.is_empty());
    }

    #[test]
    fn canvas_gate_core_accepts_registered_source_repo_path() {
        // A `.excalidraw` file inside a registered source-repo attachment root
        // is accepted — this is the pre-release-docs-authoring case.
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos").join("codesense");
        seed_attachments(&home, 42, &[repo.to_str().unwrap()]);

        let roots = source_repo_roots_at(&home);
        let target = repo.join("codesense-docs/getting-started/content/diagrams/x.excalidraw");
        assert!(
            path_within_any_root(&normalize_path_components(&target), &roots),
            "a path under a registered source-repo root must be admitted"
        );
    }

    #[test]
    fn canvas_gate_core_accepts_subdir_of_source_repo_including_git_worktree() {
        // A linked git worktree lives under the attachment root (for an
        // in-tree worktree) OR under its own registered worktree record (for
        // the attached-repo case). Here we cover the in-tree case; the
        // out-of-tree worktree case is covered by `path_in_worktree_roots`
        // (already tested by SLW-WDE-6).
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos").join("codesense");
        seed_attachments(&home, 7, &[repo.to_str().unwrap()]);

        let roots = source_repo_roots_at(&home);
        let worktree_path = repo.join(".git/worktrees/feat-x/content/diagrams/x.excalidraw");
        assert!(path_within_any_root(
            &normalize_path_components(&worktree_path),
            &roots
        ));
    }

    #[test]
    fn canvas_gate_core_rejects_unregistered_path() {
        // A path OUTSIDE every registered attachment is refused — the gate is
        // an allow-list, not "any directory on disk".
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos").join("codesense");
        seed_attachments(&home, 9, &[repo.to_str().unwrap()]);

        let roots = source_repo_roots_at(&home);
        let outside = temp.path().join("elsewhere/x.excalidraw");
        assert!(!path_within_any_root(
            &normalize_path_components(&outside),
            &roots
        ));
    }

    #[test]
    fn canvas_gate_core_rejects_dotdot_escape() {
        // Normalization must happen BEFORE the prefix check — a `../` escape
        // out of the source-repo root must not sneak past the gate.
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos").join("codesense");
        seed_attachments(&home, 3, &[repo.to_str().unwrap()]);

        let roots = source_repo_roots_at(&home);
        let sneaky = repo.join("../outside.excalidraw");
        assert!(
            !path_within_any_root(&normalize_path_components(&sneaky), &roots),
            "a `..` escape out of the source-repo root must be refused"
        );
    }

    #[test]
    fn canvas_gate_still_refuses_write_into_bundled_docs() {
        // Even a path that pattern-matches a source-repo attachment must still
        // refuse writes into the read-only bundled CodeSense docs — the write
        // validator calls `refuse_read_only_context_write` as its last gate.
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let who = crate::domain::Principal::from(9_u64);
        let archive = include_bytes!("../../resources/codesense-docs.tar.gz");
        let placed =
            crate::context_repo::codesense_docs::ensure_placed(&home, &who, archive).unwrap();
        let doc_write = placed.attachment.root.join("getting-started/content/x.md");
        // The bundled-docs refusal is path-based and does not consult
        // `codesense_home_dir()`, so a direct call is enough.
        assert!(
            refuse_read_only_context_write(&doc_write).is_err(),
            "shipped-docs path must be refused"
        );
    }
}

#[cfg(test)]
mod attached_context_gate_tests {
    use super::*;

    #[test]
    fn registered_viewer_root_admits_saved_content_and_refuses_escape_and_git_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = crate::context_repo::context_attachments::tests::checkout(
            temp.path(),
            "content/repos/checkout",
            "acme/context",
        );
        crate::context_repo::context_attachments::attach(
            &home,
            &crate::domain::Principal::local_guest(),
            &root,
        )
        .unwrap();
        let path = root.join("default/content/story.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "saved user edits").unwrap();
        assert_eq!(
            registered_editor_root(&home, &path).unwrap(),
            Some(root.clone())
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved user edits");
        assert!(registered_editor_root(&home, &root.join(".git/config")).is_err());
        assert!(registered_editor_root(&home, &root.join("../outside.md")).is_err());
        let outside = temp.path().join("unregistered.md");
        assert_eq!(registered_editor_root(&home, &outside).unwrap(), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join(".git"), root.join("hidden-metadata")).unwrap();
            assert!(registered_editor_root(&home, &root.join("hidden-metadata/config")).is_err());
        }
    }
}
