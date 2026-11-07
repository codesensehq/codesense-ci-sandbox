// SPDX-License-Identifier: MIT
//! Hot reload: a change under a `panes/` dir becomes one `pane-changed` event
//! per affected pane.
//!
//! A team's panes ride the recursive watch every team dir of the org already
//! has ([`team_dir_handler`]); the private space's panes ride the scratch
//! repo's existing watch. Either way a changed path is classified into
//! a [`PaneChange`] and fed to a [`Coalescer`], which emits each distinct change
//! once its burst has been quiet for [`QUIET`], so a `git pull` that rewrites
//! fifty files reloads each affected pane once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{pane_id_of_file, PaneScope, PANE_CHANGED_EVENT};
use crate::content_engine::DispatchHandler;

/// How long a pane's changes must be quiet before its reload is emitted.
pub const QUIET: Duration = Duration::from_millis(150);

/// The longest a burst that never goes quiet is held before it is emitted.
pub const MAX_GATHER: Duration = Duration::from_millis(750);

/// What changed: one pane (`id`), or something else in a layer's `panes/` dir
/// (an asset, or the dir itself) when `id` is absent, which reloads every pane
/// of that layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneChange {
    pub scope: PaneScope,
    pub org: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip)]
    pub declarations: bool,
    #[serde(skip)]
    pub installed: Option<(String, String)>,
}

/// What a change at `rel` (relative to a `panes/` dir) touches: `Some(Some(id))`
/// for pane `id`, `Some(None)` for an asset or the dir itself, `None` for noise
/// (dot files such as a staged write, editor swap and backup files).
pub(crate) fn classify_rel(rel: &Path) -> Option<Option<String>> {
    let parts = crate::commands::scratch_watch::segments(rel)?;
    if parts
        .iter()
        .any(|p| p.starts_with('.') || crate::commands::scratch_watch::is_noise(p))
    {
        return None;
    }
    match parts.as_slice() {
        [file] => Some(pane_id_of_file(file).map(str::to_string)),
        _ => Some(None),
    }
}

// Only entry declarations can change metadata. Nested HTML files are assets.
fn source_kind(rel: &Path, private: bool) -> (bool, Option<(String, String)>) {
    let Some(parts) = crate::commands::scratch_watch::segments(rel) else {
        return (false, None);
    };
    match parts.as_slice() {
        [] => (true, None),
        [file] => (
            file == super::manifest::MANIFEST_FILE || pane_id_of_file(file).is_some(),
            None,
        ),
        [area, team, id, file] if private && area == "installed" => {
            if file == &format!("{id}.html") || file == super::manifest::MANIFEST_FILE {
                (true, Some((team.clone(), id.clone())))
            } else {
                (file == "install.json", None)
            }
        }
        _ => (false, None),
    }
}

/// The change `path` is under the team `panes/` dir `panes_dir`, if any. Pure:
/// the caller passes the spelling of `panes_dir` the path arrived under.
pub(crate) fn team_pane_change(
    panes_dir: &Path,
    org: &str,
    team: &str,
    path: &Path,
) -> Option<PaneChange> {
    let rel = path.strip_prefix(panes_dir).ok()?;
    let (declarations, installed) = source_kind(rel, false);
    Some(PaneChange {
        scope: PaneScope::Team,
        org: org.to_string(),
        team: Some(team.to_string()),
        id: classify_rel(rel)?,
        declarations,
        installed,
    })
}

/// The private-pane change `path` is in the scratch repo at `scratch_root` (or
/// its already-resolved `canonical_root`), under `orgs/<owner>/<repo>/panes/…`.
pub(crate) fn scratch_pane_change(
    scratch_root: &Path,
    canonical_root: Option<&Path>,
    path: &Path,
) -> Option<PaneChange> {
    let relative = path
        .strip_prefix(scratch_root)
        .ok()
        .or_else(|| canonical_root.and_then(|root| path.strip_prefix(root).ok()))?;
    let parts = crate::commands::scratch_watch::segments(relative)?;
    match parts.as_slice() {
        [first, owner, repo, area, rest @ ..]
            if first == "orgs" && area == crate::context_repo::PANES_DIR =>
        {
            let rel: PathBuf = rest.iter().collect();
            let (declarations, installed) = source_kind(&rel, true);
            Some(PaneChange {
                scope: PaneScope::Private,
                org: format!("{owner}/{repo}"),
                team: None,
                id: classify_rel(&rel)?,
                declarations,
                installed,
            })
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Burst {
    first: Instant,
    last: Instant,
}

/// Gathers changes per [`PaneChange`] and hands each to its sink once, when its
/// burst has been quiet for `quiet` (or has gathered for `max`). Never blocks
/// the caller: each burst waits on its own short-lived thread.
pub struct Coalescer {
    quiet: Duration,
    max: Duration,
    pending: Arc<Mutex<HashMap<PaneChange, Burst>>>,
    sink: Arc<dyn Fn(PaneChange) + Send + Sync>,
}

impl Coalescer {
    pub fn new(
        quiet: Duration,
        max: Duration,
        sink: Arc<dyn Fn(PaneChange) + Send + Sync>,
    ) -> Self {
        Self {
            quiet,
            max,
            pending: Arc::new(Mutex::new(HashMap::new())),
            sink,
        }
    }

    pub fn note(&self, change: PaneChange) {
        let now = Instant::now();
        {
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            if let Some(burst) = pending.get_mut(&change) {
                burst.last = now;
                return;
            }
            pending.insert(
                change.clone(),
                Burst {
                    first: now,
                    last: now,
                },
            );
        }
        let (pending, sink) = (Arc::clone(&self.pending), Arc::clone(&self.sink));
        let (quiet, max) = (self.quiet, self.max);
        std::thread::spawn(move || loop {
            let wait = {
                let Ok(mut locked) = pending.lock() else {
                    return;
                };
                let Some(burst) = locked.get(&change).copied() else {
                    return;
                };
                let due = (burst.last + quiet).min(burst.first + max);
                let wait = due.saturating_duration_since(Instant::now());
                if wait.is_zero() {
                    locked.remove(&change);
                    drop(locked);
                    sink(change);
                    return;
                }
                wait
            };
            std::thread::sleep(wait);
        });
    }
}

/// The dispatch handler for a team dir `{org_root}/{team}/`: a change under its
/// `panes/` is a pane change; anything else in the team dir goes to `rest`.
/// The team dirs are already watched recursively for every team of the org
/// (not only the active chain), so this needs no watch of its own and never
/// creates a `panes/` dir.
pub(crate) fn team_dir_handler(
    team_dir: PathBuf,
    org: String,
    coalescer: Arc<Coalescer>,
    rest: DispatchHandler,
) -> DispatchHandler {
    let team = team_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let panes_dir = team_dir.join(crate::context_repo::PANES_DIR);
    // Resolved once: events may arrive under the canonical path (a symlinked
    // home), and the hot path must not hit the filesystem per event.
    let canonical_team = team_dir.canonicalize().ok();
    let canonical_panes = canonical_team
        .as_ref()
        .map(|dir| dir.join(crate::context_repo::PANES_DIR));
    Arc::new(move |path: &Path| {
        if path == team_dir || canonical_team.as_ref().is_some_and(|dir| path == dir) {
            if let Some(change) = team_pane_change(&panes_dir, &org, &team, &panes_dir) {
                coalescer.note(change);
            }
            rest(path);
            return;
        }
        let in_panes = path.starts_with(&panes_dir)
            || canonical_panes
                .as_deref()
                .is_some_and(|dir| path.starts_with(dir));
        if !in_panes {
            rest(path);
            return;
        }
        let dir = if path.starts_with(&panes_dir) {
            panes_dir.as_path()
        } else {
            canonical_panes.as_deref().unwrap_or(&panes_dir)
        };
        if let Some(change) = team_pane_change(dir, &org, &team, path) {
            coalescer.note(change);
        }
    })
}

/// The app's one coalescer, emitting `pane-changed` on `app`.
pub(crate) fn app_coalescer<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Arc<Coalescer> {
    // Tests build many mock apps: a process-wide coalescer would keep emitting on
    // the first one, so each call gets its own there.
    #[cfg(test)]
    {
        build_app_coalescer(app)
    }
    #[cfg(not(test))]
    {
        static COALESCER: std::sync::OnceLock<Arc<Coalescer>> = std::sync::OnceLock::new();
        Arc::clone(COALESCER.get_or_init(|| build_app_coalescer(app)))
    }
}

fn build_app_coalescer<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Arc<Coalescer> {
    {
        let app = app.clone();
        Arc::new(Coalescer::new(
            QUIET,
            MAX_GATHER,
            Arc::new(move |change: PaneChange| {
                use tauri::Emitter;
                if let Err(error) = crate::live_hub::pane_sources::changed(&app, &change) {
                    log::warn!(target: "panes", "source projection failed: {error}");
                }
                if let Err(e) = app.emit(PANE_CHANGED_EVENT, &change) {
                    log::warn!(target: "panes", "emit {PANE_CHANGED_EVENT} failed: {e}");
                }
            }),
        ))
    }
}
