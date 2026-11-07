// SPDX-License-Identifier: MIT
//! P2: the local publish record, one JSON file per slug in the app data dir
//! (never in the context repo). Transitions follow DR-13; any transition DR-13
//! does not list is refused and changes nothing.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::{valid_slug, LiveEmbeds, LiveFields, LiveQueries};

/// P2 `state`. `unpublished` lives here, not in the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Draft,
    Publishing,
    Published,
    Unpublished,
}

/// P2 `sync_status` (registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    Idle,
    Ok,
    Failing,
    AuthFailed,
}

/// P2 `last_error`: the fixed human texts from the registry. Tests assert
/// these values, not the wording of any UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LastError {
    #[serde(rename = "remote version changed")]
    RemoteVersionChanged,
    #[serde(rename = "dashboard not published on server")]
    DashboardNotPublished,
    #[serde(rename = "server error")]
    ServerError,
    #[serde(rename = "no response")]
    NoResponse,
    #[serde(rename = "no bug result yet")]
    NoBugResult,
    #[serde(rename = "manifest unreadable")]
    ManifestUnreadable,
    /// The local takedown is done, but the hosted copy's removal (R6) was not
    /// confirmed. Set only on an unpublished record: it is the durable pending marker.
    #[serde(rename = "takedown not confirmed")]
    TakedownUnconfirmed,
    /// R1 409 `slug_taken`: another publisher owns this slug on the server.
    #[serde(rename = "slug taken on server")]
    SlugTaken,
    /// R2, R3 or R7 404: the token on this Mac does not own the dashboard (a
    /// newly minted token is a new publisher).
    #[serde(rename = "not this token's dashboard")]
    NotOwner,
    /// The listed files do not build into a payload (a missing file, a bridge
    /// call, a file over the size cap). Nothing is sent until they do.
    #[serde(rename = "dashboard files invalid")]
    FilesInvalid,
    /// The manifest's slug was edited after publishing. Nothing is pushed: the
    /// hosted copy is under the old slug. Put the slug back to update or take it down.
    #[serde(rename = "manifest slug changed after publishing")]
    SlugChanged,
}

/// P2 `live_approved` (SW-DP-4b, DP-D39): the dashboard's `live` block that
/// is on. The name is kept from the approval dialog it replaced, so saved
/// records still load. It holds only for this slug, this folder and this block:
/// `sha` must equal the manifest's current `live` sha and `folder` the record's
/// folder. `publish_dashboard` and the watcher loop save the manifest's block
/// here (`approval::decide`, `approval::follow`); until they do, an edited
/// block, or another folder reusing the slug, is not answered.
///
/// The record is a local file. A process that can write the app data folder
/// can forge it; local agents already have that access (accepted, SW-DP-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveApproved {
    /// The canonical sha of the saved block (`manifest::live_sha`).
    pub sha: String,
    pub queries: LiveQueries,
    /// The absolute dashboard folder it was saved for. Empty in a
    /// record from before this field: then it matches no folder (fail closed).
    #[serde(default)]
    pub folder: String,
    /// The `live.fields` saved with the queries (SW-DP-5a review S1).
    #[serde(default, skip_serializing_if = "LiveFields::is_empty")]
    pub fields: LiveFields,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub args: super::manifest::LiveArgs,
    /// The `live.streams` saved with the queries (SW-DP-20).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<String>,
    /// `live.control: owner` saved with the block (SW-DP-23, DP-D48).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
    /// The `live.embeds` saved with the block (SV-INP-29).
    #[serde(default, skip_serializing_if = "LiveEmbeds::is_empty")]
    pub embeds: LiveEmbeds,
    #[serde(default, skip_serializing_if = "super::actions::Actions::is_empty")]
    pub actions: super::actions::Actions,
    /// The org and team `queryContext` reads (security review), worked out
    /// from the folder at publish. `None` when it was unknown: then
    /// `queryContext` is refused. A query whose folder now resolves to another
    /// team is refused too (`catalog::check_scope`). The watcher keeps it while
    /// the folder is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// When it was saved, this Mac's time (RFC 3339). Local only.
    pub approved_at: String,
    /// A person confirmed this block in the app (SV-PBF-286). A record from
    /// before this field loads as `false` and is not served until a person
    /// confirms it (no silent grandfathering).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirmed: bool,
}

/// SV-PBF-286: what an agent or the watcher asked for that only a person may
/// grant: a new or wider `live` block, a wider audience, or viewers added to a
/// dashboard that streams terminals. Held until the person confirms or declines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRequest {
    /// The `live` block asked for (its `sha` identifies the request); `None`
    /// when only the audience is asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<LiveApproved>,
    /// `team` when a publish asked for a wider visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Viewer addresses an agent asked to add.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub share_add: Vec<String>,
    /// Who asked: `agent` (MCP) or `watcher` (dashboard.yml edited).
    pub by: String,
    pub requested_at: String,
    /// The person declined; the agent sees `declined` until the ask changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declined: bool,
}

impl LiveRequest {
    /// The block a person can still answer: not one they declined.
    pub fn block_waiting(&self) -> Option<&LiveApproved> {
        self.block.as_ref().filter(|_| !self.declined)
    }

    /// A wider audience or viewers wait for a person. A declined block does
    /// not hide these: they are a separate ask.
    pub fn audience_waiting(&self) -> bool {
        self.visibility.is_some() || !self.share_add.is_empty()
    }

    /// Something in this request waits for a person.
    pub fn waiting(&self) -> bool {
        self.block_waiting().is_some() || self.audience_waiting()
    }
}

/// An org and team, as `catalog::team_of` works them out from a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub org: String,
    pub team: String,
}

/// The local record for one slug (P2). The optional fields are filled by the
/// watcher in 22e. This story only sets `state`, `sync_status` and
/// `last_error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishRecord {
    pub slug: String,
    pub state: LocalState,
    pub publisher_id: Option<String>,
    pub current_version: Option<u64>,
    pub pushed_at: Option<String>,
    pub checked_at: Option<String>,
    pub sync_status: SyncStatus,
    pub last_error: Option<LastError>,
    pub last_sha: Option<String>,
    /// SHA-256 (hex) of the token that got E1 (401). Only the fingerprint is saved,
    /// never the token. Publishing pauses while the saved token has this fingerprint,
    /// and a different token clears it (AC-6, DR-13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_token: Option<String>,
    /// Absolute path of the dashboard folder on this Mac. Local only: it is never uploaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The `live` block that is on (SW-DP-4b, DP-D39). Local only: R1 sends
    /// its query list, never the record. Absent in records saved before 4b.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_approved: Option<LiveApproved>,
    /// SW-DP-19: a changed `live` block, saved by the watcher or by a
    /// re-publish (`set_live_approved`), that R1 has not yet carried to the
    /// hosted row. Cleared when R1 succeeds.
    #[serde(default, skip_serializing_if = "is_false")]
    pub live_pending: bool,
    /// SV-PBF-286: an ask waiting for a person (see [`LiveRequest`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_request: Option<LiveRequest>,
    /// SV-PBF-286: the audience (`private` or `team`) a person last confirmed for
    /// this dashboard. While it streams terminals, every send to the host uses
    /// this value if `dashboard.yml` says wider; the difference waits as a
    /// [`LiveRequest`]. `None` counts as `private`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_visibility: Option<String>,
    /// SV-PBF-286: a person declined the wider audience that `dashboard.yml`
    /// asks for. The watcher does not ask for it again until the file stops
    /// asking for it (`approval::follow`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub visibility_declined: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The events DR-13 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// `publish_dashboard` (AC-7): draft or unpublished to publishing.
    Publish,
    /// First version committed or checked in: publishing to published/ok.
    FirstCommitOk,
    /// First push failed: the record stays `publishing` with `last_error`, so
    /// the watcher retries the whole first-publish sequence (R1 and R2 are
    /// idempotent) and the manifest stays consistent with the record.
    PublishFailed(LastError),
    /// A later push or check-in succeeded: any published state to ok.
    PushOk,
    /// A push failed (5xx, timeout, 409): published to failing.
    PushFailed(LastError),
    /// 401: overlay `auth_failed` on a publishing or published state. No state change.
    AuthFailed,
    /// A new token replaced the old one: `auth_failed` clears to idle.
    TokenReplaced,
    /// `unpublish_dashboard` (AC-7): publishing or published to unpublished.
    Unpublish,
    /// The takedown is owed: set before R6 is sent, so it is saved before any request.
    TakedownOwed,
    /// R6 confirmed the hosted takedown (or the server already had none): the marker clears.
    TakedownDone,
    /// The server confirmed takedown, including a hosted dashboard whose attached source is a local draft.
    HostedUnpublished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    pub state: LocalState,
    pub sync_status: SyncStatus,
    pub event: Event,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot {:?} while the dashboard is {:?} (sync {:?})",
            self.event, self.state, self.sync_status
        )
    }
}

impl PublishRecord {
    pub fn new(slug: &str) -> PublishRecord {
        PublishRecord {
            slug: slug.into(),
            state: LocalState::Draft,
            publisher_id: None,
            current_version: None,
            pushed_at: None,
            checked_at: None,
            sync_status: SyncStatus::Idle,
            last_error: None,
            last_sha: None,
            refused_token: None,
            folder: None,
            live_approved: None,
            live_request: None,
            live_visibility: None,
            visibility_declined: false,
            live_pending: false,
        }
    }

    /// True while a takedown is unconfirmed: local `unpublished`, and R6 still owed.
    pub fn takedown_pending(&self) -> bool {
        self.state == LocalState::Unpublished
            && self.last_error == Some(LastError::TakedownUnconfirmed)
    }

    /// Apply one DR-13 transition, or refuse it and leave the record unchanged.
    pub fn apply(&mut self, event: Event) -> Result<(), TransitionError> {
        let refuse = TransitionError {
            state: self.state,
            sync_status: self.sync_status,
            event,
        };
        match (event, self.state) {
            // A takedown still owed to the server comes first: publishing now
            // could leave the old hosted copy live with nothing recording it.
            (Event::Publish, LocalState::Draft | LocalState::Unpublished)
                if !self.takedown_pending() =>
            {
                self.state = LocalState::Publishing;
                self.sync_status = SyncStatus::Idle;
                self.last_error = None;
            }
            (Event::FirstCommitOk, LocalState::Publishing) => {
                self.state = LocalState::Published;
                self.sync_status = SyncStatus::Ok;
                self.last_error = None;
            }
            (Event::PublishFailed(e), LocalState::Publishing) => {
                self.sync_status = SyncStatus::Failing;
                self.last_error = Some(e);
            }
            (Event::PushOk, LocalState::Published)
                if self.sync_status != SyncStatus::AuthFailed =>
            {
                self.sync_status = SyncStatus::Ok;
                self.last_error = None;
            }
            (Event::PushFailed(e), LocalState::Published)
                if self.sync_status != SyncStatus::AuthFailed =>
            {
                self.sync_status = SyncStatus::Failing;
                self.last_error = Some(e);
            }
            (Event::AuthFailed, LocalState::Publishing | LocalState::Published)
                if self.sync_status != SyncStatus::AuthFailed =>
            {
                self.sync_status = SyncStatus::AuthFailed;
            }
            (Event::TokenReplaced, LocalState::Publishing | LocalState::Published)
                if self.sync_status == SyncStatus::AuthFailed =>
            {
                self.sync_status = SyncStatus::Idle;
            }
            (Event::HostedUnpublished, _) | (Event::TakedownDone, LocalState::Unpublished) => {
                self.current_version = None;
                self.state = LocalState::Unpublished;
                self.sync_status = SyncStatus::Idle;
                self.last_error = None;
            }
            (Event::Unpublish, LocalState::Publishing | LocalState::Published) => {
                self.state = LocalState::Unpublished;
                self.sync_status = SyncStatus::Idle;
                self.last_error = None;
            }
            (Event::TakedownOwed, LocalState::Unpublished) => {
                self.sync_status = SyncStatus::Failing;
                self.last_error = Some(LastError::TakedownUnconfirmed);
            }
            _ => return Err(refuse),
        }
        Ok(())
    }
}

/// `<data_dir>/dashboard-publish/<slug>.json`. Refuses a slug that is not F4,
/// so a slug can never name a path outside the directory.
/// The slugs that have a saved record. A missing folder has none.
pub fn list(data_dir: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(data_dir.join("dashboard-publish")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut slugs: Vec<String> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|p| p.file_stem().and_then(|x| x.to_str()).map(String::from))
        .collect();
    slugs.sort();
    Ok(slugs)
}

pub fn record_path(data_dir: &Path, slug: &str) -> Result<PathBuf, String> {
    if !valid_slug(slug) {
        return Err(format!("slug {slug:?} is not a valid dashboard slug"));
    }
    Ok(data_dir
        .join("dashboard-publish")
        .join(format!("{slug}.json")))
}

/// Load the record for `slug`, or a new draft record when none exists.
pub fn load(data_dir: &Path, slug: &str) -> Result<PublishRecord, String> {
    let path = record_path(data_dir, slug)?;
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("publish record for {slug} is unreadable: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PublishRecord::new(slug)),
        Err(e) => Err(format!("publish record for {slug} could not be read: {e}")),
    }
}

/// The one spelling of a dashboard folder that is stored and compared: symlinks
/// resolved, `.`/`..` and trailing slashes gone (SW-DP-4 review). A folder that
/// cannot be resolved (it no longer exists) falls back to its lexical form.
pub fn folder_key(folder: &Path) -> String {
    fs::canonicalize(folder)
        .unwrap_or_else(|_| folder.components().collect())
        .display()
        .to_string()
}

/// Set (or clear) the record's `live_approved` and save it, with the record's
/// folder set to the same canonical folder, so the two never disagree. On a
/// published dashboard a changed block is marked `live_pending`, so the
/// watcher retries R1 until it lands (SW-DP-19 review). The caller holds the
/// dashboard's turn lock (`driver::slug_lock`).
pub fn set_live_approved(
    data_dir: &Path,
    slug: &str,
    folder: &Path,
    approved: Option<LiveApproved>,
) -> Result<PublishRecord, String> {
    let mut record = load(data_dir, slug)?;
    record.folder = Some(folder_key(folder));
    if record.state == LocalState::Published && record.live_approved != approved {
        record.live_pending = true;
    }
    record.live_approved = approved;
    save(data_dir, &record)?;
    Ok(record)
}

/// Set (or clear) the record's [`LiveRequest`] and save it. The caller holds
/// the dashboard's turn lock (`driver::slug_lock`).
pub fn set_live_request(
    data_dir: &Path,
    slug: &str,
    request: Option<LiveRequest>,
) -> Result<(), String> {
    let mut record = load(data_dir, slug)?;
    if record.live_request == request {
        return Ok(());
    }
    record.live_request = request;
    save(data_dir, &record)
}

/// Record the audience a person confirmed. The caller holds the dashboard's
/// turn lock (`driver::slug_lock`).
pub fn set_live_visibility(data_dir: &Path, slug: &str, visibility: Option<&str>) -> Result<(), String> {
    let mut record = load(data_dir, slug)?;
    let next = visibility.map(str::to_string);
    if record.live_visibility == next {
        return Ok(());
    }
    record.live_visibility = next;
    save(data_dir, &record)
}

/// Save the record for its slug. Writes a temp file, then renames it into place.
pub fn save(data_dir: &Path, record: &PublishRecord) -> Result<(), String> {
    let path = record_path(data_dir, &record.slug)?;
    let dir = path.parent().expect("record path has a parent");
    fs::create_dir_all(dir).map_err(|e| format!("publish record dir: {e}"))?;
    let text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("publish record write: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("publish record rename: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_state(state: LocalState, sync: SyncStatus) -> PublishRecord {
        let mut r = PublishRecord::new("ai-usage");
        r.state = state;
        r.sync_status = sync;
        r
    }

    #[test]
    fn every_dr13_transition_once() {
        // draft -> publishing
        let mut r = PublishRecord::new("ai-usage");
        r.apply(Event::Publish).unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Publishing, SyncStatus::Idle)
        );

        // publishing -> published/ok
        r.apply(Event::FirstCommitOk).unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Published, SyncStatus::Ok)
        );

        // published/ok -> published/failing, with last_error
        r.apply(Event::PushFailed(LastError::ServerError)).unwrap();
        assert_eq!(r.sync_status, SyncStatus::Failing);
        assert_eq!(r.last_error, Some(LastError::ServerError));

        // failing -> failing (self loop on another failure)
        r.apply(Event::PushFailed(LastError::NoResponse)).unwrap();
        assert_eq!(r.sync_status, SyncStatus::Failing);

        // failing -> ok when the next push succeeds
        r.apply(Event::PushOk).unwrap();
        assert_eq!((r.sync_status, r.last_error), (SyncStatus::Ok, None));

        // published -> auth_failed overlay, then back to idle on a new token
        r.apply(Event::AuthFailed).unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Published, SyncStatus::AuthFailed)
        );
        r.apply(Event::TokenReplaced).unwrap();
        assert_eq!(r.sync_status, SyncStatus::Idle);

        // published -> unpublished
        r.apply(Event::Unpublish).unwrap();
        assert_eq!(r.state, LocalState::Unpublished);

        // unpublished -> publishing
        r.apply(Event::Publish).unwrap();
        assert_eq!(r.state, LocalState::Publishing);
    }

    #[test]
    fn a_failed_first_publish_stays_publishing_with_last_error() {
        let mut r = in_state(LocalState::Publishing, SyncStatus::Idle);
        r.apply(Event::PublishFailed(LastError::RemoteVersionChanged))
            .unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Publishing, SyncStatus::Failing)
        );
        assert_eq!(r.last_error, Some(LastError::RemoteVersionChanged));
    }

    #[test]
    fn an_auth_failure_does_not_change_the_state() {
        let mut r = in_state(LocalState::Published, SyncStatus::Ok);
        r.apply(Event::AuthFailed).unwrap();
        assert_eq!(r.state, LocalState::Published);
        assert!(r.apply(Event::PushOk).is_err(), "no push while auth_failed");
        assert!(r.apply(Event::PushFailed(LastError::ServerError)).is_err());
    }

    #[test]
    fn an_auth_failure_while_publishing_keeps_the_publish_state() {
        let mut r = in_state(LocalState::Publishing, SyncStatus::Idle);
        r.apply(Event::AuthFailed).unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Publishing, SyncStatus::AuthFailed)
        );
        r.apply(Event::TokenReplaced).unwrap();
        assert_eq!(
            (r.state, r.sync_status),
            (LocalState::Publishing, SyncStatus::Idle)
        );
    }

    #[test]
    fn unpublish_from_publishing_moves_to_unpublished() {
        // A first publish that failed is still publishing. The owner can take it down.
        let mut r = in_state(LocalState::Publishing, SyncStatus::Failing);
        r.last_error = Some(LastError::ServerError);
        r.apply(Event::Unpublish).unwrap();
        assert_eq!(
            (r.state, r.sync_status, r.last_error),
            (LocalState::Unpublished, SyncStatus::Idle, None)
        );
    }

    #[test]
    fn illegal_transitions_are_refused_and_change_nothing() {
        let cases = [
            // unpublish from draft (the story's example)
            (
                in_state(LocalState::Draft, SyncStatus::Idle),
                Event::Unpublish,
            ),
            (
                in_state(LocalState::Unpublished, SyncStatus::Idle),
                Event::Unpublish,
            ),
            // publish while already published or publishing
            (
                in_state(LocalState::Published, SyncStatus::Ok),
                Event::Publish,
            ),
            (
                in_state(LocalState::Publishing, SyncStatus::Idle),
                Event::Publish,
            ),
            // commit results without a publish in flight
            (
                in_state(LocalState::Draft, SyncStatus::Idle),
                Event::FirstCommitOk,
            ),
            (in_state(LocalState::Draft, SyncStatus::Idle), Event::PushOk),
            (
                in_state(LocalState::Draft, SyncStatus::Idle),
                Event::PushFailed(LastError::ServerError),
            ),
            (
                in_state(LocalState::Draft, SyncStatus::Idle),
                Event::AuthFailed,
            ),
            (
                in_state(LocalState::Published, SyncStatus::Ok),
                Event::TokenReplaced,
            ),
        ];
        for (mut record, event) in cases {
            let before = record.clone();
            let err = record.apply(event).unwrap_err();
            assert_eq!(err.event, event);
            assert_eq!(record, before, "refused {event:?} changed the record");
        }
    }

    #[test]
    fn a_takedown_stays_pending_until_r6_confirms_it() {
        let mut r = PublishRecord::new("ai-usage");
        r.apply(Event::Publish).unwrap();
        r.apply(Event::FirstCommitOk).unwrap();
        r.apply(Event::Unpublish).unwrap();
        assert!(!r.takedown_pending());

        r.apply(Event::TakedownOwed).unwrap();
        assert!(r.takedown_pending());
        // Publishing is refused while the takedown is owed.
        assert!(r.apply(Event::Publish).is_err());
        assert!(r.takedown_pending());
        assert_eq!(r.sync_status, SyncStatus::Failing);
        assert_eq!(r.last_error, Some(LastError::TakedownUnconfirmed));

        // R6 confirms the takedown: the marker clears.
        r.apply(Event::TakedownDone).unwrap();
        assert!(!r.takedown_pending());
        assert_eq!(r.sync_status, SyncStatus::Idle);

        // The marker only exists on an unpublished record.
        let mut draft = PublishRecord::new("ai-usage");
        assert!(draft.apply(Event::TakedownOwed).is_err());
        assert_eq!(draft, PublishRecord::new("ai-usage"));
    }

    #[test]
    fn last_error_values_are_the_registry_text() {
        let texts: Vec<String> = [
            LastError::RemoteVersionChanged,
            LastError::DashboardNotPublished,
            LastError::ServerError,
            LastError::NoResponse,
            LastError::NoBugResult,
            LastError::ManifestUnreadable,
            LastError::TakedownUnconfirmed,
            LastError::SlugTaken,
            LastError::NotOwner,
            LastError::FilesInvalid,
            LastError::SlugChanged,
        ]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
        assert_eq!(
            texts,
            vec![
                "\"remote version changed\"",
                "\"dashboard not published on server\"",
                "\"server error\"",
                "\"no response\"",
                "\"no bug result yet\"",
                "\"manifest unreadable\"",
                "\"takedown not confirmed\"",
                "\"slug taken on server\"",
                "\"not this token's dashboard\"",
                "\"dashboard files invalid\"",
                "\"manifest slug changed after publishing\"",
            ]
        );
    }

    #[test]
    fn sync_status_values_are_the_registry_enum() {
        let values: Vec<String> = [
            SyncStatus::Idle,
            SyncStatus::Ok,
            SyncStatus::Failing,
            SyncStatus::AuthFailed,
        ]
        .iter()
        .map(|s| serde_json::to_string(s).unwrap())
        .collect();
        assert_eq!(
            values,
            vec!["\"idle\"", "\"ok\"", "\"failing\"", "\"auth_failed\""]
        );
    }

    #[test]
    fn record_round_trips_through_the_data_dir_and_a_missing_file_is_a_draft() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = load(dir.path(), "ai-usage").unwrap();
        assert_eq!(fresh, PublishRecord::new("ai-usage"));

        let mut r = fresh;
        r.apply(Event::Publish).unwrap();
        save(dir.path(), &r).unwrap();
        assert_eq!(load(dir.path(), "ai-usage").unwrap(), r);
    }

    #[test]
    fn a_slug_that_is_not_f4_cannot_name_a_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(record_path(dir.path(), "../evil").is_err());
        assert!(load(dir.path(), "AB").is_err());
    }

    #[test]
    fn an_old_record_without_live_approved_loads_and_the_field_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = record_path(dir.path(), "ai-usage").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A record saved before SW-DP-4b.
        fs::write(
            &path,
            r#"{"slug":"ai-usage","state":"published","publisher_id":null,"current_version":1,"pushed_at":null,"checked_at":null,"sync_status":"ok","last_error":null,"last_sha":null}"#,
        )
        .unwrap();
        let old = load(dir.path(), "ai-usage").unwrap();
        assert_eq!(old.live_approved, None);

        let approved = LiveApproved {
            scope: None,
            sha: LiveQueries::All.sha(),
            queries: LiveQueries::All,
            folder: "/dash".into(),
            fields: Default::default(),
            args: Default::default(),
            streams: Vec::new(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            approved_at: "2026-10-17T10:00:00Z".into(),
            confirmed: false,
        };
        let saved = set_live_approved(dir.path(), "ai-usage", Path::new("/dash/"), Some(approved.clone())).unwrap();
        assert_eq!(load(dir.path(), "ai-usage").unwrap(), saved);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""queries": "all""#), "{text}");
        assert_eq!(saved.live_approved, Some(approved));
        // SV-PBF-286: a block saved before confirmation existed loads unconfirmed.
        fs::write(
            &path,
            r#"{"slug":"ai-usage","state":"published","publisher_id":null,"current_version":1,"pushed_at":null,"checked_at":null,"sync_status":"ok","last_error":null,"last_sha":null,"live_approved":{"sha":"x","queries":"all","folder":"/dash","approved_at":"2026-10-17T10:00:00Z"}}"#,
        )
        .unwrap();
        let legacy = load(dir.path(), "ai-usage").unwrap();
        assert!(!legacy.live_approved.unwrap().confirmed);
        assert_eq!(legacy.live_request, None);
        // The record's folder is set with the approval, in its canonical form.
        assert_eq!(saved.folder.as_deref(), Some("/dash"));
    }

    #[test]
    fn folder_key_resolves_symlinks_and_trailing_slashes() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dash");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let key = folder_key(&real);
        assert_eq!(key, fs::canonicalize(&real).unwrap().display().to_string());
        assert_eq!(folder_key(&link), key);
        assert_eq!(folder_key(Path::new(&format!("{}/", real.display()))), key);
        assert_eq!(folder_key(Path::new(&format!("{}/./", link.display()))), key);
        // A folder that is gone keeps its lexical form, without the slash.
        assert_eq!(folder_key(Path::new("/no/such/dash/")), "/no/such/dash");
    }
}
