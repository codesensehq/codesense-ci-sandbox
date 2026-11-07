// SPDX-License-Identifier: MIT
//! SW-OC-26 — one query engine for context files by metadata.
//!
//! Every surface that finds context files by their metadata runs through here:
//! the `query_context` MCP tool (agents), the `query_context` Tauri command
//! (`csquery` blocks, right panes and the dashboard bridge's
//! `queryContext`/`subscribeContext`), and `doc_query` (the scanned-tree
//! adapter automation predicates use).
//!
//! - [`model`]: the query + result types (serde, camelCase).
//! - [`record`]: one indexed file.
//! - [`relations`]: the one relationship resolver.
//! - [`engine`]: the pure evaluator (filters, relationships, text, sort, paging, groups).
//! - [`index`]: the footprint index (mtime-diffed, path-refreshable).
//! - [`service`]: the background worker + Tauri wiring.
//!
//! Team content is footprint-wide by default (every synced org); a caller
//! scopes explicitly with `where.orgs` / `where.sameOrgAs`. The private layer is
//! only ever searched for ONE anchor org: a page's own org, or the single org an
//! agent names — never "all orgs". All-orgs content matches every org.

pub mod engine;
pub mod file_list;
#[cfg(test)]
mod file_list_tests;
pub mod index;
pub mod model;
pub mod order;
pub mod record;
pub mod relations;
pub mod service;
#[cfg(test)]
mod tests;

use std::path::Path;

use tauri::{Emitter, Manager};

pub use engine::{cursor_after_kept, QueryError, Viewer, STALE_CURSOR};
pub use model::{ContextQuery, ContextQueryResult};
pub use service::ContextIndexService;

/// Run `q` against the index snapshot it pages: the current one for a first
/// page, the cursor's pinned one after that. Blocking (a `text` query reads
/// file bodies); call off the async executor.
pub fn run_query(
    service: &ContextIndexService,
    q: &ContextQuery,
    viewer: &Viewer,
) -> Result<ContextQueryResult, QueryError> {
    service.query_with(q, viewer, |snapshot| {
        snapshot.query(q, viewer, &engine::DiskBodyReader)
    })
}

/// The local profile's entry in one org's roster, as a `person:<id>` reference:
/// the person whose GitHub id is the profile's id, else whose GitHub login is
/// the profile's handle. `None` when the profile is not on the roster.
pub fn roster_me(org: &crate::domain::Org, user_id: u64, handle: &str) -> Option<String> {
    org.people
        .iter()
        .find(|p| p.github_user_id() == Some(user_id))
        .or_else(|| org.person_by_username(handle))
        .map(|p| format!("person:{}", p.id.as_str()))
}

/// Who is asking. `@me` is the selected local profile's handle everywhere,
/// plus its `person:<id>` roster entry in each org whose roster lists it
/// (rosters are read only when the query uses `@me`). `page` marks a query run
/// for a page at that path (see [`Viewer::page`]).
pub fn viewer_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    service: &ContextIndexService,
    page: Option<String>,
    uses_me: bool,
) -> Viewer {
    let profile = crate::commands::local_profile::peek_active(app);
    let handle = profile
        .as_ref()
        .map(|p| p.display_name.trim().trim_start_matches('@').to_string())
        .filter(|h| !h.is_empty());
    let mut org_identities = std::collections::HashMap::new();
    if uses_me {
        if let Some(user_id) = profile.as_ref().and_then(|p| p.principal.numeric_id().ok()) {
            let snapshot = service.snapshot();
            for root in snapshot
                .roots
                .iter()
                .filter(|r| r.layer == model::Layer::Team)
            {
                let (Some(org), Some(dir)) = (&root.org, root.dir.to_str()) else {
                    continue;
                };
                if org_identities.contains_key(org) {
                    continue;
                }
                let Some(org_root) = crate::context_repo::derive_org_root(dir) else {
                    continue;
                };
                let loaded = crate::domain::load_org(org.clone(), Path::new(org_root), None);
                if let Some(me) = loaded
                    .org
                    .as_ref()
                    .and_then(|o| roster_me(o, user_id, handle.as_deref().unwrap_or("")))
                {
                    org_identities.insert(org.clone(), vec![me]);
                }
            }
        }
    }
    Viewer {
        scope: if page.is_some() {
            crate::context_repo::caller_scope::for_page(app, page.as_deref(), None)
                .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown)
        } else {
            Default::default()
        },
        identities: handle.into_iter().collect(),
        org_identities,
        page,
    }
}

/// Does the query mention `@me` anywhere (filters or `match`)?
pub fn uses_me(q: &ContextQuery) -> bool {
    serde_json::to_string(q).is_ok_and(|s| s.contains("\"@me\""))
}

/// The user's known content roots: the index's footprint, or — before the
/// index has walked it (or when it is not running) — the footprint resolved
/// now from the selected profile. Empty with no profile selected.
pub fn known_roots<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Vec<index::IndexRoot> {
    if let Some(service) = app.try_state::<ContextIndexService>() {
        let snapshot = service.snapshot();
        if !snapshot.roots.is_empty() {
            return snapshot.roots.clone();
        }
    }
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        return Vec::new();
    };
    let Some(user_id) = crate::commands::local_profile::peek_active(app)
        .and_then(|p| p.principal.numeric_id().ok())
    else {
        return Vec::new();
    };
    service::footprint_roots(&home, user_id)
}

/// SW-DNS-2: the configured summary headings rows' `summary` follows, read from
/// app state once and then kept current by [`note_summary_headings`].
static SUMMARY_HEADINGS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

fn summary_headings(app: &tauri::AppHandle) -> Vec<String> {
    let mut cached = SUMMARY_HEADINGS.lock().unwrap_or_else(|e| e.into_inner());
    cached
        .get_or_insert_with(|| crate::commands::system::read_configured_summary_headings(app))
        .clone()
}

/// SW-DNS-2: app state was saved with these effective summary headings. When
/// they changed, the index re-walks so every `summary` follows the new list.
pub fn note_summary_headings<R: tauri::Runtime>(app: &tauri::AppHandle<R>, headings: Vec<String>) {
    let changed = {
        let mut cached = SUMMARY_HEADINGS.lock().unwrap_or_else(|e| e.into_inner());
        let changed = cached.as_ref().is_some_and(|c| *c != headings);
        *cached = Some(headings);
        changed
    };
    if changed {
        request_reconcile(app);
    }
}

/// Tell the index a watched path changed. A no-op before the index exists.
pub fn note_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>, path: &Path) {
    if let Some(service) = app.try_state::<ContextIndexService>() {
        service.note_paths([path.to_path_buf()]);
    }
}

/// The selected profile changed or was revoked, or an org was attached or
/// detached: drop the old footprint now and re-walk.
pub fn reset_footprint(app: &tauri::AppHandle) {
    if let Some(service) = app.try_state::<ContextIndexService>() {
        ContextIndexService::reset(&service);
    }
}

/// Ask the index to re-walk its roots soon (the synced team set may have changed).
pub fn request_reconcile<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(service) = app.try_state::<ContextIndexService>() {
        service.request_reconcile();
    }
}

pub(crate) fn bind_rescan_reconciliation<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    dispatcher: &crate::content_engine::Dispatcher,
) {
    let app = app.clone();
    dispatcher.set_context_index_rescan_handler(std::sync::Arc::new(move |scope| {
        request_reconcile(&app);
        crate::commands::watch::recover_context_sources(&app, scope);
    }));
}

/// Start the index worker for the app: roots from the selected profile's
/// footprint, changes emitted as [`service::CHANGE_EVENT`].
pub fn start_for_app(app: &tauri::AppHandle) {
    let Some(service) = app.try_state::<ContextIndexService>() else {
        return;
    };
    let watches = app.state::<crate::commands::watch::WatcherState>();
    bind_rescan_reconciliation(app, &watches.dispatcher);
    let roots_app = app.clone();
    // `None` until a profile is selected: the index then stays "indexing"
    // rather than reporting an empty footprint as complete.
    let roots: service::RootsFn = std::sync::Arc::new(move || {
        let home = crate::commands::paths::codesense_home_dir()?;
        let user_id = crate::commands::local_profile::peek_active(&roots_app)?
            .principal
            .numeric_id()
            .ok()?;
        Some(service::Footprint {
            owner: user_id,
            roots: service::footprint_roots(&home, user_id),
            summary_headings: summary_headings(&roots_app),
        })
    });
    let emit_app = app.clone();
    let on_change: service::ChangeFn = std::sync::Arc::new(move |change| {
        let _ = emit_app.emit(service::CHANGE_EVENT, change);
    });
    let observe_app = app.clone();
    service.start_observed(
        roots,
        on_change,
        std::sync::Arc::new(move |change| {
            if let Some(ticker) = observe_app.try_state::<crate::routines::tick::RoutineTicker>() {
                let index = observe_app.state::<ContextIndexService>();
                let viewer = viewer_for(&observe_app, &index, None, true);
                ticker.offer_metadata(&index, &change, &viewer);
            }
        }),
    );
}
