// SPDX-License-Identifier: MIT
//! SV-PBF-286: the human confirmation for live data.
//!
//! A `live` block that is new, or wider than the confirmed one, never takes
//! effect on an agent's word. `management::publish_for` and the watcher
//! (`approval::follow`) hold it as a [`LiveRequest`] on the record, and live
//! data stays paused until a person confirms it here: in the native dialog
//! this module opens, or the "Live data waiting for confirmation" row of the
//! Dashboards tab. The same request carries a wider audience (`team`) and
//! viewers an agent asked to add to a dashboard that streams terminals.
//!
//! The dialog's default button (Return) is Not now, and Escape does nothing:
//! a stray key press while typing in a terminal never confirms or declines.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Manager;

use super::approval::{self, Bound, Initiator, Shown};
use super::state::{self, LiveRequest, LocalState, PublishRecord};
use super::{manifest, management};

const CONFIRM: &str = "Turn on live data";
const NOT_NOW: &str = "Not now";
const DECLINE: &str = "Decline";

/// The least time between two alerts for one dashboard. An ask that arrives
/// sooner waits in the Dashboards tab.
const MIN_GAP: Duration = Duration::from_secs(60);

/// What the app has put a dialog up for, per dashboard, this run.
#[derive(Default)]
struct Slot {
    /// An alert for this dashboard is open: one at a time.
    showing: bool,
    last: Option<Instant>,
    /// The request the last alert showed.
    shown: Option<Shown>,
}

fn slots() -> &'static Mutex<HashMap<String, Slot>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

/// Characters that never belong in a name shown to a person: controls (Cc),
/// format characters such as zero-width and bidi controls (Cf), and the line
/// and paragraph separators (Zl, Zp).
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{8e2}'
            | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}'
            | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
            | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}'
            | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}')
}

/// A name from a manifest, safe to show: control, format and separator
/// characters dropped, one line, at most 60 characters. It is shown quoted,
/// never trusted.
pub fn clean_name(name: &str) -> String {
    let one: String = name.chars().filter(|c| !hidden(*c)).take(60).collect();
    one.trim().to_string()
}

/// A viewer address an agent may ask to add: `^[a-z0-9._+-]{1,40}@[a-z0-9-]+(\.[a-z0-9-]+)+$`
/// once lowercased. Anything else (Unicode, quotes, bidi characters) is refused.
pub fn plausible_email(address: &str) -> bool {
    let a = address.trim().to_ascii_lowercase();
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    let label = |l: &str| {
        !l.is_empty()
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    (1..=40).contains(&local.len())
        && local.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'+' | b'-')
        })
        && domain.split('.').count() >= 2
        && domain.split('.').all(label)
}

/// The team a dashboard folder belongs to, for the dialog's wording.
fn team_name(app: &tauri::AppHandle, folder: &std::path::Path) -> Option<String> {
    let service = app.try_state::<crate::context_query::ContextIndexService>()?;
    super::catalog::team_of(&service.snapshot().roots, folder).map(|(_, team)| team)
}

/// What the dialog adds to the ask: the team whose terminals it reaches.
#[derive(Debug, Default, Clone)]
pub struct DialogContext {
    pub team: Option<String>,
    /// The audience the dashboard ends with: `private` or `team`.
    pub audience: String,
}

/// What the person is asked, in app-authored words.
pub fn dialog_text(title: &str, request: &LiveRequest, ctx: &DialogContext) -> String {
    let mut text = format!(
        "An agent asked to change the live data of the dashboard \"{}\". Live data is paused until you confirm.\n",
        clean_name(title)
    );
    if let Some(block) = request.block_waiting() {
        text.push_str("\nWhile it is on, the published dashboard keeps a connection to share.codesense.ai");
        if block.streams.is_empty() {
            text.push_str(" and answers the queries its dashboard.yml lists.");
        } else {
            let team = ctx
                .team
                .as_deref()
                .map(|t| format!(" in team \"{}\"", clean_name(t)))
                .unwrap_or_else(|| " in its team".to_string());
            text.push_str(&format!(
                " and shows the output of any terminal{team}, including ones started later (secrets are masked, but not every one can be caught)."
            ));
            // `team` is reserved and is currently the same as private: only invited viewers get in.
            let who = if ctx.audience == manifest::Visibility::Team.as_str() {
                "only you (team visibility is reserved, currently the same as private)"
            } else {
                "only you"
            };
            text.push_str(&format!(
                "\nWho can see it: {who}, plus any viewers added to the dashboard."
            ));
        }
        if block.control {
            text.push_str(" The dashboard's owner can also type into those terminals from the published page.");
        }
    }
    if request.visibility.is_some() {
        text.push_str(
            "\nIt also asks to set visibility to team, which is reserved and currently the same as private: no one new can see it.",
        );
    }
    if !request.share_add.is_empty() {
        let shown: Vec<&str> = request.share_add.iter().take(10).map(String::as_str).collect();
        let more = request.share_add.len().saturating_sub(shown.len());
        text.push_str(&format!(
            "\nIt also asks to add {} viewer(s): {}{}.",
            request.share_add.len(),
            shown.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() }
        ));
    }
    text.push_str("\n\nYou can change this later from the Dashboards tab.");
    text
}

/// Put the native dialog up for `record`'s waiting ask. One alert per
/// dashboard at a time, and at most one a minute unless `force` (a person just
/// clicked a stale confirm). Not now leaves the ask in the Dashboards tab;
/// Decline answers it.
pub fn announce(app: &tauri::AppHandle, record: &PublishRecord, force: bool) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
    let Some(request) = record.live_request.clone().filter(|r| r.waiting()) else {
        return;
    };
    let current = record
        .folder
        .as_deref()
        .and_then(|f| manifest::load(std::path::Path::new(f)).ok());
    let file_visibility = current
        .as_ref()
        .map_or(manifest::Visibility::Private, |m| m.visibility);
    let shown = Shown::of(&request, file_visibility.as_str());
    {
        let Ok(mut slots) = slots().lock() else { return };
        let slot = slots.entry(record.slug.clone()).or_default();
        if slot.showing
            || slot.shown.as_ref() == Some(&shown)
            || (!force && slot.last.is_some_and(|t| t.elapsed() < MIN_GAP))
        {
            return;
        }
        slot.showing = true;
        slot.shown = Some(shown.clone());
    }
    let title = current
        .map(|m| m.title)
        .unwrap_or_else(|| record.slug.clone());
    let ctx = DialogContext {
        team: record
            .folder
            .as_deref()
            .and_then(|f| team_name(app, std::path::Path::new(f))),
        audience: shown.audience.clone(),
    };
    let slug = record.slug.clone();
    let handle = app.clone();
    app.dialog()
        .message(dialog_text(&title, &request, &ctx))
        .title("Turn on live data?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            NOT_NOW.to_string(),
            CONFIRM.to_string(),
            DECLINE.to_string(),
        ))
        .show_with_result(move |answer| {
            if let Ok(mut slots) = slots().lock() {
                if let Some(slot) = slots.get_mut(&slug) {
                    slot.showing = false;
                    slot.last = Some(Instant::now());
                }
            }
            let MessageDialogResult::Custom(label) = answer else {
                return;
            };
            let handle = handle.clone();
            let slug = slug.clone();
            let shown = shown.clone();
            tauri::async_runtime::spawn(async move {
                let done = match label.as_str() {
                    CONFIRM => confirm(&handle, &slug, shown).await.map(|_| ()),
                    DECLINE => decline(&handle, &slug).await,
                    _ => Ok(()),
                };
                if let Err(e) = done {
                    log::warn!(target: "dashboard_publish", "live data answer for {slug}: {e}");
                }
            });
        });
}

/// The UI publish: a person publishing a `live` block that needs confirming
/// is asked once, here. Confirming makes the publish theirs, for the block
/// they were shown; anything else publishes the static part and leaves the
/// ask waiting.
pub async fn publish_initiator(app: &tauri::AppHandle, folder: &str) -> (Initiator, Option<Bound>) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
    let Ok(folder) = management::folder(folder) else {
        return (Initiator::Agent, None);
    };
    let Ok(manifest) = manifest::load(&folder) else {
        return (Initiator::Agent, None);
    };
    let Ok(data) = management::data_dir(app) else {
        return (Initiator::Agent, None);
    };
    let record = state::load(&data, &manifest.slug).unwrap_or_else(|_| PublishRecord::new(&manifest.slug));
    let decision = approval::decide(&manifest, &folder, record.live_approved.as_ref(), None);
    let gated = approval::gate(Initiator::Agent, decision.approved, record.live_approved.as_ref());
    // Bound to the block read here, so a manifest that changes before the
    // publish settles is the agent's, not the person's.
    let Some(hold) = gated.hold else {
        return (Initiator::Human, Some(Bound::Block(manifest.live_sha())));
    };
    let sha = hold.sha.clone();
    let request = LiveRequest {
        block: Some(hold),
        visibility: None,
        share_add: Vec::new(),
        by: Initiator::Human.label().into(),
        requested_at: String::new(),
        declined: false,
    };
    let ctx = DialogContext {
        team: team_name(app, &folder),
        audience: manifest.visibility.as_str().to_string(),
    };
    let text = dialog_text(&manifest.title, &request, &ctx)
        .replacen("An agent asked to change", "You are publishing", 1);
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title("Turn on live data?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            NOT_NOW.to_string(),
            CONFIRM.to_string(),
            DECLINE.to_string(),
        ))
        .show_with_result(move |answer| {
            let _ = tx.send(answer);
        });
    match rx.await {
        Ok(MessageDialogResult::Custom(label)) if label == CONFIRM => {
            (Initiator::Human, Some(Bound::Block(Some(sha))))
        }
        // The person answered; the agent-side notice must not ask again, and
        // the held ask is theirs.
        _ => (Initiator::Agent, Some(Bound::Asked)),
    }
}

fn request_of(data: &std::path::Path, slug: &str) -> Result<(PublishRecord, LiveRequest), String> {
    let record = state::load(data, slug)?;
    let request = record
        .live_request
        .clone()
        .ok_or("Nothing is waiting for confirmation on this dashboard.")?;
    Ok((record, request))
}

/// A person confirms the waiting ask as `shown` them: the block, a wider
/// audience and added viewers are applied together, as the person's own
/// publish. A request that no longer shows exactly this is refused and asked
/// about again (`approval::STALE`).
pub async fn confirm(
    app: &tauri::AppHandle,
    slug: &str,
    shown: Shown,
) -> Result<super::status::PublicationStatus, String> {
    management::validate_slug(slug)?;
    let data = management::data_dir(app)?;
    let record = state::load(&data, slug)?;
    let folder = record
        .folder
        .clone()
        .ok_or("This dashboard's folder isn't known.")?;
    if manifest::load_slug(std::path::Path::new(&folder)).map_err(|e| e.to_string())? != slug {
        return Err("This dashboard's folder no longer holds it.".into());
    }
    let asked = record.live_request.clone();
    let status = match management::publish_bound(
        app,
        &folder,
        &shown.audience,
        crate::context_repo::caller_scope::CallerScope::Human,
        Initiator::Human,
        Some(Bound::Request(shown.clone())),
    )
    .await
    {
        Ok(status) => status,
        Err(e) => {
            if e == approval::STALE {
                if let Ok(fresh) = state::load(&data, slug) {
                    announce(app, &fresh, true);
                }
            }
            return Err(e);
        }
    };
    if !shown.share_add.is_empty() {
        let turn = super::driver::slug_lock(slug);
        let _guard = turn.lock().await;
        if let Err(e) = super::share::share_slug_hosted(&data, slug, &shown.share_add, &[]).await {
            // The block hold `settle` kept (declined or turned off) stays.
            let kept = state::load(&data, slug)
                .ok()
                .and_then(|r| r.live_request)
                .filter(|r| r.declined && r.block.is_some());
            state::set_live_request(
                &data,
                slug,
                Some(LiveRequest {
                    declined: kept.is_some(),
                    block: kept.and_then(|r| r.block),
                    visibility: None,
                    share_add: shown.share_add.clone(),
                    by: asked.as_ref().map_or("agent".into(), |r| r.by.clone()),
                    requested_at: asked.map(|r| r.requested_at).unwrap_or_default(),
                }),
            )?;
            return Err(format!("Live data is on, but adding viewers failed: {}", e.reason()));
        }
    }
    Ok(status)
}

/// A person declines the waiting ask. The agent sees `declined` on a later
/// read, and the same ask is not made again until dashboard.yml changes.
pub async fn decline(app: &tauri::AppHandle, slug: &str) -> Result<(), String> {
    management::validate_slug(slug)?;
    let data = management::data_dir(app)?;
    let turn = super::driver::slug_lock(slug);
    let _guard = turn.lock().await;
    decline_in(&data, slug)
}

/// `decline` on the record. The block stays, declined, so it is not asked
/// again; wider-audience and viewer asks are dropped.
pub(crate) fn decline_in(data: &std::path::Path, slug: &str) -> Result<(), String> {
    let (mut record, mut request) = request_of(data, slug)?;
    // The declined audience is remembered, so `follow` does not ask for it
    // again while dashboard.yml still says the same.
    record.visibility_declined |= request.visibility.is_some();
    request.declined = true;
    request.visibility = None;
    request.share_add.clear();
    record.live_request = Some(request);
    state::save(data, &record)
}

/// The kill switch: live data stops at once and pending asks are cleared. It
/// does not edit dashboard.yml; the edited block is not asked about again
/// until it changes. Streams on every link end now, not at the next pass.
pub async fn turn_off(app: &tauri::AppHandle, slug: &str) -> Result<(), String> {
    management::validate_slug(slug)?;
    let data = management::data_dir(app)?;
    let turn = super::driver::slug_lock(slug);
    let _guard = turn.lock().await;
    turn_off_in(&data, slug)?;
    super::live::stop_streams(slug);
    Ok(())
}

/// `turn_off` on the record: the confirmed block is cleared and the manifest's
/// block is kept as a declined ask.
pub(crate) fn turn_off_in(data: &std::path::Path, slug: &str) -> Result<(), String> {
    let record = state::load(data, slug)?;
    let folder = record
        .folder
        .clone()
        .ok_or("This dashboard's folder isn't known.")?;
    let held = manifest::load(std::path::Path::new(&folder))
        .ok()
        .and_then(|m| approval::turned_off(&record, &m));
    state::set_live_approved(data, slug, std::path::Path::new(&folder), None)?;
    state::set_live_request(data, slug, held)
}

/// One dashboard with live data on or an ask waiting for a person, for the
/// Dashboards tab section "Live data".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRequestView {
    pub slug: String,
    pub title: String,
    pub by: String,
    pub requested_at: String,
    /// Live data is on for the confirmed block (it may stay on while a narrower ask waits).
    pub live_on: bool,
    /// An ask is waiting for the person (Confirm and Decline).
    pub waiting: bool,
    pub terminals: bool,
    pub control: bool,
    pub wider_audience: bool,
    pub viewers_to_add: usize,
    pub declined: bool,
    /// What the row shows; the confirm sends it back so only this is applied.
    pub shown: Option<Shown>,
}

/// The asks waiting on this Mac, oldest first.
pub fn requests(data: &std::path::Path) -> Vec<LiveRequestView> {
    let mut out: Vec<LiveRequestView> = state::list(data)
        .unwrap_or_default()
        .iter()
        .filter_map(|slug| state::load(data, slug).ok())
        .filter(|r| matches!(r.state, LocalState::Published | LocalState::Publishing))
        .filter_map(|record| {
            let live_on = record.live_approved.as_ref().is_some_and(|a| a.confirmed);
            let request = record.live_request.as_ref();
            if request.is_none() && !live_on {
                return None;
            }
            let current = record
                .folder
                .as_deref()
                .and_then(|f| manifest::load(std::path::Path::new(f)).ok());
            let file_visibility = current
                .as_ref()
                .map_or(manifest::Visibility::Private, |m| m.visibility);
            let title = current
                .map(|m| m.title)
                .unwrap_or_else(|| record.slug.clone());
            let block = request
                .and_then(|r| r.block_waiting())
                .or(record.live_approved.as_ref());
            Some(LiveRequestView {
                slug: record.slug.clone(),
                title: clean_name(&title),
                by: request.map(|r| r.by.clone()).unwrap_or_default(),
                requested_at: request.map(|r| r.requested_at.clone()).unwrap_or_default(),
                live_on,
                waiting: request.is_some_and(|r| r.waiting()),
                terminals: block.is_some_and(|b| !b.streams.is_empty()),
                control: block.is_some_and(|b| b.control),
                wider_audience: request.is_some_and(|r| r.visibility.is_some()),
                viewers_to_add: request.map_or(0, |r| r.share_add.len()),
                declined: request.is_some_and(|r| r.declined),
                shown: request.map(|r| Shown::of(r, file_visibility.as_str())),
            })
        })
        .collect();
    out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
    out
}

#[tauri::command]
pub async fn dashboard_live_requests(app: tauri::AppHandle) -> Result<Vec<LiveRequestView>, String> {
    Ok(requests(&management::data_dir(&app)?))
}

#[tauri::command]
pub async fn dashboard_live_confirm(
    app: tauri::AppHandle,
    slug: String,
    shown: Shown,
) -> Result<super::status::PublicationStatus, String> {
    confirm(&app, &slug, shown).await.map_err(|e| crate::feedback::redact(&e))
}

#[tauri::command]
pub async fn dashboard_live_decline(app: tauri::AppHandle, slug: String) -> Result<(), String> {
    decline(&app, &slug).await.map_err(|e| crate::feedback::redact(&e))
}

#[tauri::command]
pub async fn dashboard_live_turn_off(app: tauri::AppHandle, slug: String) -> Result<(), String> {
    turn_off(&app, &slug).await.map_err(|e| crate::feedback::redact(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_publish::state::LiveApproved;

    fn block(streams: &[&str], control: bool) -> LiveApproved {
        LiveApproved {
            sha: "sha".into(),
            queries: manifest::LiveQueries::All,
            folder: "/dash".into(),
            fields: Default::default(),
            args: Default::default(),
            streams: streams.iter().map(|s| s.to_string()).collect(),
            control,
            embeds: Default::default(),
            actions: Default::default(),
            scope: None,
            approved_at: "2026-11-03T10:00:00Z".into(),
            confirmed: false,
        }
    }

    fn ask(block: Option<LiveApproved>) -> LiveRequest {
        LiveRequest {
            block,
            visibility: None,
            share_add: Vec::new(),
            by: "agent".into(),
            requested_at: "2026-11-03T10:00:00Z".into(),
            declined: false,
        }
    }

    /// B7: names are quoted and cleaned, addresses must look like addresses.
    #[test]
    fn untrusted_names_and_addresses_are_cleaned() {
        assert_eq!(clean_name("My\n\u{1b}[31mdash"), "My[31mdash");
        assert_eq!(clean_name(&"x".repeat(200)).len(), 60);
        // Zero-width, bidi and separator characters cannot forge app wording.
        assert_eq!(
            clean_name("x\u{2028}No terminals\u{202e}\u{200b} are shared.\u{2029}"),
            "xNo terminals are shared."
        );
        assert!(plausible_email("a@b.co"));
        assert!(plausible_email(" Ana.Lee+x@Mail.example.com "));
        for bad in [
            "", "a", "a@b", "a@@b.co", "a b@c.co", "@c.co", "a@.co", "a@c.\n", "a@b..co",
            "\"a\"@b.co", "\u{430}@b.co", "a@b\u{202e}.co", "a@b_c.co", "a@-.co x",
        ] {
            assert!(!plausible_email(bad), "{bad:?}");
        }
        assert!(!plausible_email(&format!("{}@b.co", "a".repeat(41))));
    }

    /// The person is told live data is paused until they confirm, what the
    /// dashboard will show, and whether control or a wider audience is asked.
    #[test]
    fn the_dialog_says_live_is_paused_and_what_it_asks() {
        let ctx = DialogContext {
            team: Some("emea".into()),
            audience: "private".into(),
        };
        let plain = dialog_text("AI usage", &ask(Some(block(&[], false))), &ctx);
        assert!(plain.contains("paused until you confirm"), "{plain}");
        assert!(plain.contains("\"AI usage\""));
        assert!(!plain.contains("terminals"));
        let mut wide = ask(Some(block(&["terminal"], true)));
        wide.visibility = Some("team".into());
        wide.share_add = vec!["a@b.co".into()];
        let text = dialog_text("AI usage", &wide, &ctx);
        for needle in [
            "output of any terminal in team \"emea\", including ones started later",
            "Who can see it: only you",
            "type into those terminals",
            "set visibility to team, which is reserved and currently the same as private",
            "1 viewer(s): a@b.co",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        let team_ctx = DialogContext {
            audience: "team".into(),
            ..ctx
        };
        assert!(dialog_text("AI usage", &wide, &team_ctx).contains(
            "Who can see it: only you (team visibility is reserved, currently the same as private)"
        ));
    }

    /// The Dashboards tab lists live dashboards and asks; the view carries no
    /// names from the request, only the cleaned title.
    #[test]
    fn requests_lists_asks_and_live_dashboards() {
        let data = tempfile::tempdir().unwrap();
        let mut waiting = PublishRecord::new("waiting-one");
        waiting.state = LocalState::Published;
        waiting.live_request = Some(ask(Some(block(&["terminal"], false))));
        state::save(data.path(), &waiting).unwrap();
        let mut on = PublishRecord::new("on-air");
        on.state = LocalState::Published;
        let mut confirmed = block(&[], false);
        confirmed.confirmed = true;
        on.live_approved = Some(confirmed);
        state::save(data.path(), &on).unwrap();
        let mut draft = PublishRecord::new("draft-one");
        draft.live_request = Some(ask(Some(block(&[], false))));
        state::save(data.path(), &draft).unwrap();
        state::save(data.path(), &PublishRecord::new("quiet")).unwrap();

        let mut views = requests(data.path());
        views.sort_by(|a, b| a.slug.cmp(&b.slug));
        let slugs: Vec<_> = views.iter().map(|v| v.slug.as_str()).collect();
        assert_eq!(slugs, ["on-air", "waiting-one"]);
        assert!(views[0].live_on && !views[0].waiting);
        assert!(!views[1].live_on && views[1].waiting && views[1].terminals);
    }

    /// Row 14 and the kill switch: a declined ask is not waiting; turning off
    /// stores the declined block so the watcher does not ask again.
    #[test]
    fn a_declined_ask_is_not_waiting() {
        let data = tempfile::tempdir().unwrap();
        let mut record = PublishRecord::new("declined-one");
        record.state = LocalState::Published;
        let mut declined = ask(Some(block(&[], false)));
        declined.declined = true;
        record.live_request = Some(declined);
        state::save(data.path(), &record).unwrap();
        let views = requests(data.path());
        assert_eq!(views.len(), 1);
        assert!(views[0].declined && !views[0].waiting);
    }

    /// A dashboard folder on disk with a `live` block, and the app data dir.
    fn live_on_disk(live: &str) -> (tempfile::TempDir, tempfile::TempDir, PublishRecord) {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("index.html"), "ok").unwrap();
        std::fs::write(
            folder.path().join(manifest::MANIFEST_FILE),
            format!("slug: stop-me\ntitle: Stop me\npublish:\n  state: published\n  visibility: private\n  files:\n    - index.html\n{live}"),
        )
        .unwrap();
        let data = tempfile::tempdir().unwrap();
        let m = manifest::load(folder.path()).unwrap();
        let mut record = PublishRecord::new("stop-me");
        record.state = LocalState::Published;
        record.folder = Some(state::folder_key(folder.path()));
        let mut on = block(&[], false);
        on.sha = m.live_sha().unwrap();
        on.folder = state::folder_key(folder.path());
        on.confirmed = true;
        record.live_approved = Some(on);
        state::save(data.path(), &record).unwrap();
        (folder, data, record)
    }

    /// B5, row 14: turning off clears the confirmed block and keeps the
    /// manifest's block as a declined hold, so the watcher does not ask again;
    /// the hold survives a viewer ask that arrives later (M6).
    #[test]
    fn turn_off_is_sticky_and_a_viewer_ask_does_not_revive_the_block() {
        let (folder, data, record) = live_on_disk("live:\n  queries: all\n");
        turn_off_in(data.path(), "stop-me").unwrap();
        let after = state::load(data.path(), "stop-me").unwrap();
        assert_eq!(after.live_approved, None);
        let held = after.live_request.clone().expect("the block is kept as a hold");
        assert!(held.declined && held.block.is_some() && !held.waiting());
        let m = manifest::load(folder.path()).unwrap();
        assert_eq!(approval::follow(&after, &m), None, "not asked again");
        assert!(!approval::is_live(&after, &m));
        let _ = record;

        let revived = management::with_viewer_ask(after.live_request.as_ref(), vec!["a@b.co".into()]);
        assert!(revived.declined, "the block stays declined");
        assert!(revived.block_waiting().is_none());
        assert!(revived.waiting(), "the viewer ask itself waits");
    }

    /// Decline answers the whole ask: the block stays declined, and the
    /// audience and viewer asks are dropped.
    #[test]
    fn decline_keeps_the_block_declined_and_drops_audience_asks() {
        let (_folder, data, mut record) = live_on_disk("live:\n  queries: all\n");
        let mut request = ask(Some(block(&["terminal"], false)));
        request.visibility = Some("team".into());
        request.share_add = vec!["a@b.co".into()];
        record.live_request = Some(request);
        state::save(data.path(), &record).unwrap();
        decline_in(data.path(), "stop-me").unwrap();
        let after = state::load(data.path(), "stop-me").unwrap().live_request.unwrap();
        assert!(after.declined && after.block.is_some());
        assert!(after.visibility.is_none() && after.share_add.is_empty());
        assert!(!after.waiting());
        assert!(decline_in(data.path(), "nothing-here").is_err());
    }

    /// SV-PBF-286 r5 M2, row 14: a declined file-driven audience ask is not
    /// made again while dashboard.yml still asks for `team`; a later change of
    /// the file to something else, then `team` again, is a new ask.
    #[test]
    fn a_declined_audience_ask_is_not_made_again_until_the_file_changes() {
        let (folder, data, mut record) = live_on_disk("live:\n  queries: all\n");
        let mut on = record.live_approved.clone().unwrap();
        on.streams = vec!["terminal".into()];
        record.live_approved = Some(on);
        record.live_visibility = Some("private".into());
        let mut team = manifest::load(folder.path()).unwrap();
        team.visibility = manifest::Visibility::Team;
        let held = approval::follow(&record, &team).expect("the file widens the audience");
        record.live_request = held.request;
        state::save(data.path(), &record).unwrap();

        decline_in(data.path(), "stop-me").unwrap();
        let declined = state::load(data.path(), "stop-me").unwrap();
        assert!(declined.visibility_declined);
        assert_eq!(approval::follow(&declined, &team), None, "not asked again");

        // The file stops asking for team: the marker clears.
        let mut private = team.clone();
        private.visibility = manifest::Visibility::Private;
        let cleared = approval::follow(&declined, &private).expect("the marker clears");
        assert!(!cleared.visibility_declined);
        let mut later = declined.clone();
        later.visibility_declined = cleared.visibility_declined;
        let again = approval::follow(&later, &team).expect("team again is a new ask");
        assert_eq!(again.request.unwrap().visibility.as_deref(), Some("team"));
    }

    /// The tab lists a record that is still publishing (L5), and every row
    /// carries what it shows so the confirm is bound to it (H1).
    #[test]
    fn requests_carry_what_they_show_and_include_publishing_records() {
        let data = tempfile::tempdir().unwrap();
        let mut record = PublishRecord::new("first-push");
        record.state = LocalState::Publishing;
        let mut request = ask(Some(block(&["terminal"], false)));
        request.share_add = vec!["a@b.co".into()];
        record.live_request = Some(request);
        state::save(data.path(), &record).unwrap();
        let views = requests(data.path());
        assert_eq!(views.len(), 1);
        let shown = views[0].shown.clone().unwrap();
        assert_eq!(shown.sha.as_deref(), Some("sha"));
        assert_eq!(shown.share_add, ["a@b.co"]);
    }
}
