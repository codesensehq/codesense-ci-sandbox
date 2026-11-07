// SPDX-License-Identifier: MIT
use super::{
    approval, catalog, manifest, payload, push, share, state, status::PublicationStatus, verbs,
};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub fn folder(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("Dashboard must be an absolute folder path on this Mac.".into());
    }
    Ok(PathBuf::from(state::folder_key(path)))
}
/// Architecture: the persistent publisher cannot retain a child caller's authority.
/// Its source and each declared asset must be eligible before any preparation.
pub(crate) fn admit_source(
    path: &str,
    caller: &crate::context_repo::caller_scope::CallerScope,
    reads_payload: bool,
) -> Result<PathBuf, String> {
    let folder = folder(path)?;
    caller.require_preparation_path(&folder)?;
    let source = folder.join(manifest::MANIFEST_FILE);
    caller.require_preparation_path(&source)?;
    // Keep baseline manifest diagnostics for admitted inputs. Inspect declarations
    // before the ordinary validator can stat or read any payload source.
    if !reads_payload {
        return Ok(folder);
    }
    if let Ok(text) = std::fs::read_to_string(&source) {
        if let Ok(raw) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            if let Some(files) = raw["publish"]["files"].as_sequence() {
                for file in files.iter().filter_map(serde_yaml::Value::as_str) {
                    caller.require_preparation_path(&folder.join(file))?;
                }
            }
        }
    }
    Ok(folder)
}

#[cfg(test)]
mod caller_scope_tests;

pub fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

pub(crate) async fn publish_for(
    app: &tauri::AppHandle,
    path: &str,
    visibility: &str,
    caller: crate::context_repo::caller_scope::CallerScope,
    initiator: approval::Initiator,
) -> Result<PublicationStatus, String> {
    publish_bound(app, path, visibility, caller, initiator, None).await
}

/// `publish_for`, with a person's answer tied to what they were shown
/// (`approval::Bound`): a confirm applies only the request they saw.
pub(crate) async fn publish_bound(
    app: &tauri::AppHandle,
    path: &str,
    visibility: &str,
    caller: crate::context_repo::caller_scope::CallerScope,
    initiator: approval::Initiator,
    bound: Option<approval::Bound>,
) -> Result<PublicationStatus, String> {
    let folder = folder(path)?;
    let visibility =
        manifest::Visibility::parse(visibility).ok_or("Visibility must be private or team")?;
    let data = data_dir(app)?;
    let slug = manifest::load_slug(&folder).map_err(|e| e.to_string())?;
    let turn = super::driver::slug_lock(&slug);
    let _guard = turn.lock().await;
    let manifest = manifest::load(&folder).map_err(|e| e.to_string())?;
    let scope = app
        .try_state::<crate::context_query::ContextIndexService>()
        .and_then(|service| catalog::team_of(&service.snapshot().roots, &folder));
    super::actions::validate_references_for(
        app,
        &manifest.actions,
        scope
            .as_ref()
            .map(|(org, team)| state::Scope {
                org: org.clone(),
                team: team.clone(),
            })
            .as_ref(),
        Some(caller),
    )
    .await?;
    let record = state::load(&data, &slug)?;
    manifest::validate_pin_scope(
        &manifest.live_args,
        scope
            .as_ref()
            .map(|(org, team)| state::Scope {
                org: org.clone(),
                team: team.clone(),
            })
            .as_ref(),
    )
    .map_err(|e| e.to_string())?;
    // SV-PBF-286: only a person turns live data on, widens it, or widens the
    // audience of a dashboard that streams terminals.
    let settled = approval::settle(
        &record,
        &manifest,
        &folder,
        scope
            .as_ref()
            .map(|(org, team)| (org.as_str(), team.as_str())),
        visibility,
        initiator,
        bound.as_ref(),
    )?;
    let visibility = settled.visibility;
    let owed = push::push_owed(&data, &folder);
    let republish = !owed
        && record.state == state::LocalState::Published
        && manifest.state == manifest::PublishState::Published;
    if let Some(confirmed) = settled.confirmed_visibility {
        state::set_live_visibility(&data, &slug, Some(confirmed.as_str()))?;
    }
    let mut status: PublicationStatus = if owed || republish {
        state::set_live_approved(&data, &slug, &folder, settled.gated.applied.clone())?;
        verbs::VerbOutcome {
            slug: slug.clone(),
            manifest_state: "published",
            local_state: record.state,
            visibility: Some(
                if republish {
                    visibility
                } else {
                    manifest.visibility
                }
                .as_str(),
            ),
        }
        .into()
    } else {
        verbs::publish(&folder, &data, visibility, settled.gated.applied.clone())
            .map_err(|e| e.to_string())?
            .into()
    };
    state::set_live_request(&data, &slug, settled.request.clone())?;
    if settled.announce {
        super::confirm::announce(app, &state::load(&data, &slug)?, false);
    }
    status.retry = owed;
    status.notes = payload::toolkit_notes(&manifest, &payload::FolderReader { folder: &folder });
    status.live = Some(settled.live_result);
    if settled.asked_visibility.is_some() {
        status.notes.push(
            "Visibility stays private: this dashboard streams terminals, so only a person can widen who sees it. CodeSense is asking in the app; live data and the wider audience wait until they confirm.".into(),
        );
    }
    if matches!(settled.live_result, approval::LiveResult::PendingConfirmation) {
        status.notes.push(
            "Live data is paused until a person confirms it in the CodeSense app. The dashboard is published without it.".into(),
        );
    }
    status.published(if republish {
        push::update_hosted(&data, &folder, visibility).await
    } else {
        push::publish_hosted(&data, &folder).await
    });
    if republish {
        if let Ok(now) = manifest::load(&folder) {
            status.visibility = Some(now.visibility.as_str().into());
        }
    }
    status.local_state = Some(state::load(&data, &slug)?.state);
    Ok(status)
}

pub async fn unpublish(app: &tauri::AppHandle, path: &str) -> Result<PublicationStatus, String> {
    let folder = folder(path)?;
    let data = data_dir(app)?;
    let slug = manifest::load_slug(&folder).map_err(|e| e.to_string())?;
    let turn = super::driver::slug_lock(&slug);
    let _guard = turn.lock().await;
    let mut status: PublicationStatus = verbs::unpublish(&folder, &data)
        .map_err(|e| e.to_string())?
        .into();
    status.unpublished(push::take_down_hosted(&data, &folder).await);
    Ok(status)
}

pub async fn viewers(
    app: &tauri::AppHandle,
    path: &str,
    add: &[String],
    remove: &[String],
) -> Result<PublicationStatus, String> {
    let folder = folder(path)?;
    let data = data_dir(app)?;
    let slug = manifest::load_slug(&folder).map_err(|e| e.to_string())?;
    let turn = super::driver::slug_lock(&slug);
    let _guard = turn.lock().await;
    // SV-PBF-286: an agent may not widen the audience of a dashboard that
    // streams terminals; the addresses wait for a person. Removing is automatic.
    let record = state::load(&data, &slug)?;
    let (add, held) = split_viewers(&record, add)?;
    let shared = share::share_hosted(&data, &folder, &add, remove)
        .await
        .map_err(|e| e.reason())?;
    let mut status = PublicationStatus::sharing(slug.clone(), shared);
    if !held.is_empty() {
        let request = with_viewer_ask(record.live_request.as_ref(), held);
        state::set_live_request(&data, &slug, Some(request))?;
        status.notes.push("Viewers were not added: this dashboard streams terminals, so a person must confirm in the CodeSense app first. The audience is unchanged until then.".into());
        super::confirm::announce(app, &state::load(&data, &slug)?, false);
    }
    Ok(status)
}

/// The addresses to add now and the ones that wait for a person. A dashboard
/// that streams terminals (on, or asked for) holds every address.
pub(crate) fn split_viewers(
    record: &state::PublishRecord,
    add: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let streams = record
        .live_approved
        .iter()
        .chain(record.live_request.iter().filter_map(|r| r.block.as_ref()))
        .any(|b| !b.streams.is_empty());
    if !streams || add.is_empty() {
        return Ok((add.to_vec(), Vec::new()));
    }
    if let Some(bad) = add.iter().find(|a| !super::confirm::plausible_email(a)) {
        return Err(format!(
            "\"{}\" isn't a valid email address.",
            super::confirm::clean_name(bad)
        ));
    }
    Ok((
        Vec::new(),
        add.iter().map(|a| a.trim().to_ascii_lowercase()).collect(),
    ))
}

/// `existing` with `held` addresses added to its viewer ask. The block's
/// `declined` is left alone: a declined or turned-off block stays declined.
pub(crate) fn with_viewer_ask(
    existing: Option<&state::LiveRequest>,
    held: Vec<String>,
) -> state::LiveRequest {
    let mut request = existing.cloned().unwrap_or(state::LiveRequest {
        block: None,
        visibility: None,
        share_add: Vec::new(),
        by: approval::Initiator::Agent.label().into(),
        requested_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        declined: false,
    });
    for a in held {
        if !request.share_add.contains(&a) {
            request.share_add.push(a);
        }
    }
    request
}

#[derive(serde::Serialize)]
pub struct DashboardDetail {
    pub dashboard: super::status::DashboardView,
    pub capabilities: Vec<super::status::Capability>,
}
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if manifest::valid_slug(slug) {
        Ok(())
    } else {
        Err("Invalid dashboard slug".into())
    }
}
pub async fn detail(slug: &str) -> Result<DashboardDetail, String> {
    validate_slug(slug)?;
    let client = super::client::Client::new(
        super::client::DASH_HOST.ok_or("This build has no dashboard host")?,
    )?;
    let token = super::keys::token(super::keys::store())
        .await?
        .ok_or("Add a publishing key")?;
    let dashboard = client
        .view(&token, slug)
        .await
        .map_err(|e| call_error(&e))?;
    let capabilities = dashboard.capabilities();
    Ok(DashboardDetail {
        dashboard,
        capabilities,
    })
}

#[tauri::command]
pub async fn dashboard_move_link(slug: String) -> Result<String, String> {
    validate_slug(&slug)?;
    let client = super::client::Client::new(
        super::client::DASH_HOST.ok_or("This build has no dashboard host")?,
    )?;
    let token = super::keys::token(super::keys::store())
        .await?
        .ok_or("Add a publishing key")?;
    let url = client
        .dashboard_move_link(&token, &slug)
        .await
        .map_err(|e| call_error(&e))?;
    validate_move_url(&url)?;
    Ok(url)
}

fn validate_move_url(url: &str) -> Result<(), String> {
    let prefix = format!("https://{}/account/move/", env!("CODESENSE_PUBLIC_HOST"));
    let Some(code) = url.strip_prefix(&prefix) else {
        return Err("The server returned an invalid move link. Try again.".into());
    };
    if code.len() != 43
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("The server returned an invalid move link. Try again.".into());
    }
    Ok(())
}

#[cfg(test)]
mod move_link_tests {
    use super::validate_move_url;

    #[test]
    fn move_links_must_be_single_use_routes_on_the_build_public_host() {
        let code = "a".repeat(43);
        assert!(validate_move_url(&format!(
            "https://{}/account/move/{code}",
            env!("CODESENSE_PUBLIC_HOST")
        ))
        .is_ok());
        for url in [
            format!(
                "http://{}/account/move/{code}",
                env!("CODESENSE_PUBLIC_HOST")
            ),
            format!("https://evil.example/account/move/{code}"),
            format!(
                "https://{}/account/control/{code}",
                env!("CODESENSE_PUBLIC_HOST")
            ),
            format!(
                "https://{}/account/move/{code}?next=/",
                env!("CODESENSE_PUBLIC_HOST")
            ),
            format!(
                "https://{}/account/move/short",
                env!("CODESENSE_PUBLIC_HOST")
            ),
        ] {
            assert!(validate_move_url(&url).is_err(), "accepted {url}");
        }
    }
}
pub fn call_error(error: &super::client::CallError) -> String {
    use super::client::CallError;
    match error {
        CallError::Status { status: 401, .. } => "The server refused this key. Fix key, then try again.".into(),
        CallError::Status { status: 404, .. } => "This key can't manage this dashboard. Switch back to the key it was published with, or ask its owner.".into(),
        CallError::Status { status, code } => crate::feedback::redact(&format!("The server refused the request ({status}: {code}). Try again.")),
        _ => "Couldn't reach the dashboard server. Try again.".into(),
    }
}

#[tauri::command]
pub async fn dashboard_detail(slug: String) -> Result<DashboardDetail, String> {
    detail(&slug).await
}
#[tauri::command]
pub async fn dashboard_publish(
    app: tauri::AppHandle,
    folder: String,
) -> Result<PublicationStatus, String> {
    let visibility = manifest::load(Path::new(&folder))
        .map_err(|e| crate::feedback::redact(&e.to_string()))?
        .visibility;
    // A person publishing a `live` block that needs confirming is asked once.
    let (initiator, bound) = super::confirm::publish_initiator(&app, &folder).await;
    publish_bound(
        &app,
        &folder,
        visibility.as_str(),
        crate::context_repo::caller_scope::CallerScope::Human,
        initiator,
        bound,
    )
    .await
    .map_err(|e| crate::feedback::redact(&e))
}
fn local_unpublish_folder(
    data: &Path,
    slug: &str,
    source: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(source) = source else {
        return Ok(None);
    };
    let source = folder(source)?;
    if manifest::load_slug(&source).map_err(|e| e.to_string())? != slug {
        return Err("Dashboard folder doesn't match the selected slug".into());
    }
    let record = state::load(data, slug)?;
    Ok((matches!(
        record.state,
        state::LocalState::Publishing | state::LocalState::Published
    ) || record.takedown_pending())
    .then_some(source))
}

#[tauri::command]
pub async fn dashboard_unpublish(
    app: tauri::AppHandle,
    slug: String,
    folder: Option<String>,
) -> Result<PublicationStatus, String> {
    validate_slug(&slug)?;
    let data = data_dir(&app)?;
    if let Some(folder) = local_unpublish_folder(&data, &slug, folder.as_deref())? {
        return unpublish(
            &app,
            folder.to_str().ok_or("Dashboard folder is not UTF-8")?,
        )
        .await
        .map_err(|e| crate::feedback::redact(&e));
    }
    let turn = super::driver::slug_lock(&slug);
    let _guard = turn.lock().await;
    let mut status = PublicationStatus {
        slug: slug.clone(),
        ..Default::default()
    };
    status.unpublished(push::take_down_slug(&data, &slug).await);
    Ok(status)
}
#[tauri::command]
pub async fn dashboard_share(
    app: tauri::AppHandle,
    slug: String,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<PublicationStatus, String> {
    validate_slug(&slug)?;
    let turn = super::driver::slug_lock(&slug);
    let _guard = turn.lock().await;
    let dashboard = detail(&slug).await?;
    if dashboard.dashboard.state != "published" {
        return Err("This dashboard isn't published, so it can't have viewers.".into());
    }
    let shared = share::share_slug_hosted(&data_dir(&app)?, &slug, &add, &remove)
        .await
        .map_err(|e| crate::feedback::redact(&e.reason()))?;
    Ok(PublicationStatus::sharing(slug, shared))
}

#[tauri::command]
pub async fn dashboard_viewer_sign_out(slug: String, share_id: String) -> Result<(), String> {
    validate_slug(&slug)?;
    let client = super::client::Client::new(
        super::client::DASH_HOST.ok_or("This build has no dashboard host")?,
    )?;
    let token = super::keys::token(super::keys::store())
        .await?
        .ok_or("Add a publishing key")?;
    client
        .revoke_share_sessions(&token, &slug, &share_id)
        .await
        .map_err(|e| call_error(&e))
}

#[cfg(test)]
mod tests {
    use super::super::client::{
        fake::{Fake, Reply},
        Client,
    };
    use super::super::push::tests::{dashboard, token_store, TOKEN};
    use super::*;
    #[tokio::test]
    async fn a_remotely_republished_dashboard_uses_slug_takedown_after_local_unpublish() {
        let data = tempfile::tempdir().unwrap();
        let folder = dashboard();
        super::super::inventory::attach_folder(data.path(), folder.path())
            .await
            .unwrap();
        let mut record = state::load(data.path(), "ai-usage").unwrap();
        record.apply(state::Event::HostedUnpublished).unwrap();
        state::save(data.path(), &record).unwrap();
        assert!(local_unpublish_folder(
            data.path(),
            "ai-usage",
            Some(folder.path().to_str().unwrap())
        )
        .unwrap()
        .is_none());
        let server = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        push::take_down_slug_with(
            &Client::new(&server.base).unwrap(),
            token_store(TOKEN),
            data.path(),
            "ai-usage",
        )
        .await
        .unwrap();
        assert_eq!(
            server.routes(),
            vec!["POST /api/dash/v1/d/ai-usage/unpublish"]
        );
        record = state::load(data.path(), "ai-usage").unwrap();
        record.apply(state::Event::TakedownOwed).unwrap();
        state::save(data.path(), &record).unwrap();
        assert!(local_unpublish_folder(
            data.path(),
            "ai-usage",
            Some(folder.path().to_str().unwrap())
        )
        .unwrap()
        .is_some());
    }
    #[tokio::test]
    async fn attaching_a_hosted_source_keeps_takedown_on_the_confirmed_slug_route() {
        let data = tempfile::tempdir().unwrap();
        let folder = dashboard();
        super::super::inventory::attach_folder(data.path(), folder.path())
            .await
            .unwrap();
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            state::LocalState::Draft
        );
        assert!(local_unpublish_folder(
            data.path(),
            "ai-usage",
            Some(folder.path().to_str().unwrap())
        )
        .unwrap()
        .is_none());
        let failed = Fake::start(|_| Reply::Drop).await;
        assert!(push::take_down_slug_with(
            &Client::new(&failed.base).unwrap(),
            token_store(TOKEN),
            data.path(),
            "ai-usage"
        )
        .await
        .is_err());
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            state::LocalState::Draft
        );
        let server = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        push::take_down_slug_with(
            &Client::new(&server.base).unwrap(),
            token_store(TOKEN),
            data.path(),
            "ai-usage",
        )
        .await
        .unwrap();
        assert_eq!(
            server.routes(),
            vec!["POST /api/dash/v1/d/ai-usage/unpublish"]
        );
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            state::LocalState::Unpublished
        );
    }
}

#[cfg(test)]
mod viewer_hold_tests {
    use super::*;

    fn record(streams: &[&str]) -> state::PublishRecord {
        let mut record = state::PublishRecord::new("ai-usage");
        record.live_approved = Some(state::LiveApproved {
            sha: "sha".into(),
            queries: manifest::LiveQueries::All,
            folder: "/dash".into(),
            fields: Default::default(),
            args: Default::default(),
            streams: streams.iter().map(|s| s.to_string()).collect(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            scope: None,
            approved_at: "2026-11-03T10:00:00Z".into(),
            confirmed: true,
        });
        record
    }

    /// SV-PBF-286 row 13, B7: on a dashboard that streams terminals every
    /// address waits for a person (lowercased, deduplicated); anything that is
    /// not an address is refused; without terminals nothing is held.
    #[test]
    fn viewers_wait_only_on_a_dashboard_that_streams_terminals() {
        let add = vec![" Ana@Example.com ".to_string()];
        let (now, held) = split_viewers(&record(&["terminal"]), &add).unwrap();
        assert!(now.is_empty());
        assert_eq!(held, ["ana@example.com"]);
        let (now, held) = split_viewers(&record(&[]), &add).unwrap();
        assert_eq!(now, add);
        assert!(held.is_empty());
        let err = split_viewers(&record(&["terminal"]), &["a@b.co\u{202e}".to_string()]).unwrap_err();
        assert!(err.contains("isn't a valid email address"), "{err}");
        // A held block with terminals counts too, before anything is confirmed.
        let mut asked = state::PublishRecord::new("ai-usage");
        let mut block = record(&["terminal"]).live_approved.unwrap();
        block.confirmed = false;
        asked.live_request = Some(approval::next_request(
            approval::Initiator::Agent,
            None,
            Some(block),
            None,
        ).unwrap());
        assert!(!split_viewers(&asked, &add).unwrap().1.is_empty());
    }

    /// M6: a held address joins the ask once; a declined block stays declined.
    #[test]
    fn a_viewer_ask_joins_the_request_without_reviving_a_declined_block() {
        let mut declined = approval::next_request(
            approval::Initiator::Agent,
            None,
            record(&["terminal"]).live_approved,
            None,
        )
        .unwrap();
        declined.declined = true;
        let mut next = with_viewer_ask(Some(&declined), vec!["a@b.co".into()]);
        next = with_viewer_ask(Some(&next), vec!["a@b.co".into(), "c@d.co".into()]);
        assert_eq!(next.share_add, ["a@b.co", "c@d.co"]);
        assert!(next.declined && next.block_waiting().is_none());
        let fresh = with_viewer_ask(None, vec!["a@b.co".into()]);
        assert!(!fresh.declined && fresh.waiting());
    }
}
