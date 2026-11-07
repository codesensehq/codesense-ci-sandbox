// SPDX-License-Identifier: MIT
//! The push driver (SW-PB-22e-2): ties the watcher (`watch.rs`) to the HTTP
//! client (`client.rs`) and the local record (`state.rs`). It runs the K13
//! sequence for one watcher request, maps each failure to the watcher's
//! [`Outcome`] (DR-4 error codes, K12), and writes the record on success only.
//! The record holds server times only (AC-3), and the token comes from the
//! Keychain only (AC-7).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::client::{CallError, Client, Version, DASH_HOST, TOKEN_SERVICE};
use super::manifest::{self, PublishState};
use super::payload::Payload;
use super::state::{
    self, Event, LastError, LocalState, PublishRecord, SyncStatus, TransitionError,
};
use super::watch::{Action, Outcome, TickError, Watcher};
use crate::commands::credential_store::{default_store, CredentialStore, Secret};

/// What a successful request leaves in the record. Times are the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    /// The committed version, when the request committed or confirmed one.
    version: Option<u64>,
    /// The server's `pushed_at`, when a version was committed.
    pushed_at: Option<String>,
    /// The server's `checked_at`.
    checked_at: String,
}

#[derive(Debug)]
pub enum DriveError {
    Keychain(String),
    Tick(TickError),
    Storage(String),
    Transition(TransitionError),
    /// R6 failed. The record stays `unpublished`, and the caller decides.
    Unpublish(CallError),
    /// The token saved as refused is the one in use (no call is made with it).
    Refused,
    /// Sharing needs a dashboard that is published on this Mac.
    NotPublished,
    /// The request itself is not valid (its message says why).
    BadRequest(String),
    /// R8 or R9 for the share list failed.
    Share(CallError),
    /// R6 found no dashboard for this token, but this Mac had published one:
    /// it may still be live under the token that published it.
    NotOwner,
    /// The build named no dashboard host (`CODESENSE_DASH_HOST`), so nothing is sent.
    NoHost,
    /// R1 for a live block change on a published dashboard failed.
    Update(CallError),
}

impl DriveError {
    /// A short reason for the tool result. It never includes the token.
    pub fn reason(&self) -> String {
        match self {
            // Either no token is saved, or the Keychain could not be read (locked,
            // access denied). The message says which; it never holds the token.
            DriveError::Keychain(e) => format!("publisher token unavailable: {e}"),
            DriveError::Unpublish(CallError::NoResponse) => "share.codesense.ai did not respond".into(),
            DriveError::Unpublish(CallError::Status { status, code }) => {
                format!("share.codesense.ai refused the takedown ({status} {code})")
            }
            DriveError::Unpublish(CallError::BadReply) => {
                "share.codesense.ai sent a reply this app cannot read".into()
            }
            // Manifest and record errors name a file or field, never a token.
            DriveError::Storage(e) => format!("could not read the dashboard or its record: {e}"),
            DriveError::NoHost => "this build has no dashboard host".into(),
            DriveError::NotPublished => "publish the dashboard before sharing it".into(),
            DriveError::BadRequest(why) => why.clone(),
            DriveError::Share(CallError::NoResponse) => "share.codesense.ai did not respond".into(),
            DriveError::Share(CallError::BadReply) => {
                "share.codesense.ai sent a reply this app cannot read".into()
            }
            DriveError::Share(CallError::Status { status, code }) => {
                format!("share.codesense.ai refused the share list ({status} {code})")
            }
            DriveError::Refused => "the publisher token was refused; save a new one".into(),
            DriveError::Update(CallError::NoResponse) => "share.codesense.ai did not respond".into(),
            DriveError::Update(CallError::BadReply) => {
                "share.codesense.ai sent a reply this app cannot read".into()
            }
            DriveError::Update(CallError::Status { status, code }) => {
                format!("share.codesense.ai refused the dashboard update ({status} {code})")
            }
            DriveError::NotOwner => "share.codesense.ai has no dashboard under this name for the token on this Mac; it may still be live under the token that published it".into(),
            DriveError::Tick(_) | DriveError::Transition(_) => "the push could not run".into(),
        }
    }
}

impl From<TickError> for DriveError {
    fn from(e: TickError) -> Self {
        DriveError::Tick(e)
    }
}

impl From<TransitionError> for DriveError {
    fn from(e: TransitionError) -> Self {
        DriveError::Transition(e)
    }
}

/// What one hosted push reported, for the tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosted {
    /// R2 made the dashboard live on the server.
    Live,
    /// The push failed. The record stays `publishing`, and the next publish call pushes again.
    Retrying(LastError),
    /// The token was refused. Pushes stop until a new token is saved.
    AuthFailed,
}

fn hosted_state(record: &PublishRecord) -> Hosted {
    if record.sync_status == SyncStatus::AuthFailed || record.refused_token.is_some() { Hosted::AuthFailed }
    else if record.state == LocalState::Published && record.sync_status == SyncStatus::Ok { Hosted::Live }
    else { Hosted::Retrying(record.last_error.unwrap_or(LastError::ServerError)) }
}

/// True while a publish is owed to the server: the manifest is published, but
/// the record is still `publishing` because the first push did not finish.
pub fn push_owed(data_dir: &Path, folder: &Path) -> bool {
    let Ok(manifest) = manifest::load(folder) else {
        return false;
    };
    manifest.state == PublishState::Published
        && state::load(data_dir, &manifest.slug)
            .map(|r| r.state == LocalState::Publishing)
            .unwrap_or(false)
}

/// SW-PB-22f-1: the hosted half of `publish_dashboard`. Runs the first push
/// (R1 and R2 to make it live, then R3 to R5) for a dashboard whose manifest is
/// published. The upload is the dashboard's own listed files, as they are on disk.
pub async fn publish_hosted(data_dir: &Path, folder: &Path) -> Result<Hosted, DriveError> {
    if DASH_HOST.is_none() {
        return Err(DriveError::NoHost);
    }
    let now = Instant::now();
    let mut publisher = Publisher::open(data_dir, folder, now)
        .map_err(DriveError::Storage)?;
    publisher.drive(now).await?;
    Ok(hosted_state(publisher.record()))
}

pub async fn update_hosted(data_dir: &Path, folder: &Path, visibility: manifest::Visibility) -> Result<Hosted, DriveError> {
    let mut publisher = Publisher::open(data_dir, folder, Instant::now()).map_err(DriveError::Storage)?;
    publisher.update_now(visibility).await
}

pub async fn take_down_slug(
    data_dir: &Path,
    slug: &str,
) -> Result<Option<super::client::DeletedFiles>, DriveError> {
    let client = Client::new(DASH_HOST.ok_or(DriveError::NoHost)?).map_err(DriveError::Storage)?;
    take_down_slug_with(
        &client,
        Arc::from(default_store(TOKEN_SERVICE)),
        data_dir,
        slug,
    )
    .await
}

pub(crate) async fn take_down_slug_with(
    client: &Client,
    store: Arc<dyn CredentialStore>,
    data_dir: &Path,
    slug: &str,
) -> Result<Option<super::client::DeletedFiles>, DriveError> {
    let token = super::keys::token(store)
        .await
        .map_err(DriveError::Keychain)?
        .ok_or_else(|| DriveError::Keychain("no publisher token".into()))?;
    let deleted = client.unpublish(&token, slug).await.map_err(|e| match e {
        CallError::Status { status: 404, .. } => DriveError::NotOwner,
        e => DriveError::Unpublish(e),
    })?;
    if state::record_path(data_dir, slug)
        .map_err(DriveError::Storage)?
        .exists()
    {
        let mut record = state::load(data_dir, slug).map_err(DriveError::Storage)?;
        record.apply(Event::HostedUnpublished)?;
        state::save(data_dir, &record).map_err(DriveError::Storage)?;
    }
    Ok(deleted)
}

/// SW-PB-22e-3: the hosted half of `unpublish_dashboard`. Runs R6 for the
/// dashboard in `folder`. The local takedown must already be done, or pending
/// from an earlier failed call. Only the slug is sent.
pub async fn take_down_hosted(
    data_dir: &Path,
    folder: &Path,
) -> Result<Option<super::client::DeletedFiles>, DriveError> {
    if DASH_HOST.is_none() {
        // This build sends nothing. A dashboard that never committed a version
        // has no hosted content to remove, so the owed takedown is settled here;
        // otherwise it would block publishing for good. One that did commit
        // (with an earlier build) stays owed for a build with a host.
        let slug = manifest::load_slug(folder).map_err(|e| DriveError::Storage(e.to_string()))?;
        let mut record = state::load(data_dir, &slug).map_err(DriveError::Storage)?;
        if record.takedown_pending() && record.current_version.is_none() {
            record.apply(Event::TakedownDone)?;
            state::save(data_dir, &record).map_err(DriveError::Storage)?;
            return Ok(None);
        }
        return Err(DriveError::NoHost);
    }
    let mut publisher = Publisher::open(data_dir, folder, Instant::now())
        .map_err(DriveError::Storage)?;
    publisher.unpublish().await
}

/// One dashboard's push watcher, with its record and its endpoint.
pub struct Publisher {
    data_dir: PathBuf,
    folder: PathBuf,
    client: Client,
    store: Arc<dyn CredentialStore>,
    record: PublishRecord,
    watcher: Watcher,
}

impl Publisher {
    /// The app's entry point: the dashboard in `folder`, the build's
    /// `DASH_HOST`, and the Keychain token.
    pub fn open(
        data_dir: &Path,
        folder: &Path,
        now: Instant,
    ) -> Result<Publisher, String> {
        let host = DASH_HOST.ok_or("this build has no dashboard host")?;
        let client = Client::new(host)?;
        let store: Arc<dyn CredentialStore> = Arc::from(default_store(TOKEN_SERVICE));
        Publisher::new(data_dir, folder, client, store, now)
    }

    /// Load the record for the dashboard in `folder`. A record left in
    /// `publishing` by an earlier run is pushed again at once. A published
    /// record resumes its K1 interval from `now`, so a restart needs no state
    /// beyond the record (AC-13).
    pub fn new(
        data_dir: &Path,
        folder: &Path,
        client: Client,
        store: Arc<dyn CredentialStore>,
        now: Instant,
    ) -> Result<Publisher, String> {
        // A manifest that does not validate (a listed file is gone, say) must
        // not block a takedown, so the slug alone is enough to find the record.
        // Nothing is pushed from it: every push re-reads and validates the
        // manifest first (`Watcher::poll`, `live`).
        let slug = match super::manifest::load(folder) {
            Ok(manifest) => manifest.slug,
            Err(_) => super::manifest::load_slug(folder).map_err(|e| e.to_string())?,
        };
        let record = state::load(data_dir, &slug)?;
        Ok(Publisher::from_record(data_dir, folder, record, client, store, now))
    }

    /// The publisher for a saved record. The driver uses this, so a slug edited
    /// in the manifest after publishing cannot point a push or a takedown at
    /// another record: every request uses the record's own slug, and a push is
    /// only made while the manifest still names that slug.
    pub fn from_record(
        data_dir: &Path,
        folder: &Path,
        record: PublishRecord,
        client: Client,
        store: Arc<dyn CredentialStore>,
        now: Instant,
    ) -> Publisher {
        let mut watcher = Watcher::new(now);
        let published = super::manifest::load(folder)
            .is_ok_and(|m| m.state == PublishState::Published && m.slug == record.slug);
        if record.state == LocalState::Publishing && published {
            watcher.publish_requested(now);
        }
        Publisher {
            data_dir: data_dir.into(),
            folder: folder.into(),
            client,
            store,
            record,
            watcher,
        }
    }

    async fn update_now(&mut self, visibility: manifest::Visibility) -> Result<Hosted, DriveError> {
        let now = Instant::now();
        let manifest = manifest::load(&self.folder).map_err(|e| DriveError::Storage(e.to_string()))?;
        let built = super::payload::build_approved(
            &manifest,
            super::payload::Mode::Published,
            &super::payload::FolderReader { folder: &self.folder },
            &self.record,
        )
        .map_err(|e| DriveError::Storage(e.to_string()))?;
        let result = self.update_live(visibility).await?;
        if result != Hosted::Live {
            return Ok(result);
        }
        self.watcher.start(Action::Push {
            publish: false,
            payload: built.payload,
            title: manifest.title,
            visibility: visibility.as_str(),
        });
        self.drive(now).await?;
        Ok(hosted_state(self.record()))
    }

    pub fn record(&self) -> &PublishRecord {
        &self.record
    }

    /// Run the watcher at `now` and perform the request it queues, if any.
    /// A listed file changed on disk: arm the debounce (K2). A file outside the allowlist does nothing.
    pub fn note_change(&mut self, now: Instant, rel: &str, allowlist: &[String]) {
        self.watcher.file_changed(now, rel, allowlist);
    }

    /// Call it on each tick. With no request due, nothing is sent.
    pub async fn drive(&mut self, now: Instant) -> Result<(), DriveError> {
        // A slug edited in the manifest after publishing: the hosted copy is under
        // the record's slug, so nothing is pushed. Say so once, in the record.
        if let Ok(slug) = manifest::load_slug(&self.folder) {
            if slug != self.record.slug {
                if self.record.last_error != Some(LastError::SlugChanged) {
                    self.record_failure(LastError::SlugChanged)?;
                }
                return Ok(());
            }
        }
        // The Keychain is read only when it matters: while a refusal is in
        // force (to see whether the token was replaced), or when a request is due.
        let paused = self.record.refused_token.is_some()
            || self.record.sync_status == SyncStatus::AuthFailed;
        let mut token = None;
        if paused {
            token = self.read_token().await?;
            // Paused until the token is replaced (AC-6). The refusal holds
            // whatever the sync status is: an unpublish or a new publish does
            // not lift it.
            if self.is_refused(token.as_ref()) {
                return Ok(());
            }
            // Only a real, different token lifts the refusal. A missing token
            // (a Keychain read that came back empty) lifts nothing.
            if token.is_none() {
                return Ok(());
            }
            if self.record.sync_status == SyncStatus::AuthFailed {
                self.record.apply(Event::TokenReplaced)?;
            }
            self.record.refused_token = None;
            self.save()?;
        }
        if let Err(e) = self.watcher.poll(now, &self.record, &self.folder) {
            // The listed files do not build. Say so in the record; the watcher
            // keeps the change and tries again after the debounce, not every pass.
            self.record_failure(LastError::FilesInvalid)?;
            return Err(e.into());
        }
        let Some(request) = self.watcher.next_action(&self.record) else {
            return Ok(());
        };
        if !paused {
            token = match self.read_token().await {
                Ok(token) => token,
                Err(e) => {
                    // Finish the request, so the watcher retries it at K12 rather
                    // than waiting forever on a request that never ran. The record
                    // is not touched: nothing was sent.
                    self.watcher
                        .request_done(now, request.id, Outcome::Failed(LastError::NoResponse));
                    return Err(e);
                }
            };
        }

        let sha = match &request.action {
            Action::Push { payload, .. } => Some(payload.manifest_sha.clone()),
            Action::Checkin => None,
        };
        let result = match &token {
            // No token, no call: the state is auth_failed until one exists.
            None => Err(Outcome::AuthFailed),
            Some(token) => self.call(&request.action, token).await,
        };
        let outcome = match &result {
            Ok(_) => Outcome::Success,
            Err(outcome) => *outcome,
        };
        // A result for a request that is no longer in flight is dropped.
        if !self.watcher.request_done(now, request.id, outcome) {
            return Ok(());
        }
        log::info!(target: "dashboard_publish", "push finished: {outcome:?}");
        if outcome == Outcome::Ignored {
            // A takedown on disk ended the request. Take the on-disk record.
            self.record =
                state::load(&self.data_dir, &self.record.slug).map_err(DriveError::Storage)?;
        }
        if outcome == Outcome::AuthFailed && token.is_some() {
            self.record.refused_token = Some(fingerprint(token.as_ref()));
        }
        self.settle(result, sha)
    }

    async fn read_token(&self) -> Result<Option<Secret>, DriveError> {
        super::keys::token(self.store.clone()).await.map_err(DriveError::Keychain)
    }

    async fn call(&self, action: &Action, token: &Secret) -> Result<Stamp, Outcome> {
        let slug = self.record.slug.as_str();
        match action {
            Action::Checkin => {
                self.live()?;
                let manifest = manifest::load(&self.folder)
                    .map_err(|_| Outcome::NoRetry(LastError::ManifestUnreadable))?;
                self.put_manifest(token, &manifest, &manifest.title, manifest.visibility.as_str())
                    .await.map_err(|e| self.failure(e))?;
                self.live()?;
                let checked_at = self
                    .client
                    .checkin(token, slug)
                    .await
                    .map_err(|e| self.failure(e))?;
                Ok(Stamp {
                    version: None,
                    pushed_at: None,
                    checked_at,
                })
            }
            Action::Push {
                publish,
                payload,
                title,
                visibility,
            } => self.push(token, *publish, payload, title, visibility).await,
        }
    }

    /// Re-read the manifest before an HTTP step (owner review). A takedown
    /// written during the sequence stops it before the next call. A manifest
    /// that cannot be read also stops it: nothing is sent on a manifest we
    /// cannot check.
    fn live(&self) -> Result<manifest::Manifest, Outcome> {
        match manifest::load(&self.folder) {
            Ok(manifest)
                if manifest.state == PublishState::Published && manifest.slug == self.record.slug =>
            {
                Ok(manifest)
            }
            Ok(_) => Err(Outcome::Ignored),
            Err(_) => Err(Outcome::NoRetry(LastError::ManifestUnreadable)),
        }
    }

    /// SW-DP-19: make the saved `live` block follow `manifest`'s (an edited,
    /// added or removed block), and save it. A published dashboard's change is
    /// marked `live_pending` until R1 carries it (`update_live`); a publishing
    /// one's first push sends R1 anyway. True when the block changed.
    pub fn follow_live(&mut self, manifest: &manifest::Manifest) -> Result<bool, DriveError> {
        if manifest.state != PublishState::Published {
            return Ok(false);
        }
        let Some(next) = super::approval::follow(&self.record, manifest) else {
            return Ok(false);
        };
        let slug = &self.record.slug;
        log::info!(target: "dashboard_publish", "live block of {slug} changed in dashboard.yml: saved");
        let changed = self.record.live_approved != next.approved;
        self.record.live_approved = next.approved;
        self.record.live_request = next.request;
        self.record.visibility_declined = next.visibility_declined;
        // A held ask changes nothing hosted: R1 follows only a stored block.
        self.record.live_pending =
            (changed && self.record.state == LocalState::Published) || self.record.live_pending;
        self.save()?;
        Ok(true)
    }

    /// SW-DP-19: give up on a pending live block whose R1 cannot succeed by
    /// retrying (the server refused it, or the manifest is no longer published
    /// under this slug). The next change to the block, or a publish, sends R1 again.
    pub fn drop_live_pending(&mut self) -> Result<(), DriveError> {
        if self.record.live_pending {
            self.record.live_pending = false;
            self.save()?;
        }
        Ok(())
    }

    /// SW-DP-4b: R1 alone for a published dashboard, with its title, the
    /// requested `visibility` and the saved `live_queries`. Only after R1
    /// succeeds is `visibility` written to `dashboard.yml`, and a pending
    /// live block (SW-DP-19, `live_pending`) cleared. The refusal rule
    /// holds: a refused token is not sent, and a 401 saves its fingerprint.
    pub async fn update_live(
        &mut self,
        visibility: manifest::Visibility,
    ) -> Result<Hosted, DriveError> {
        self.send_r1(Some(visibility)).await
    }

    /// SW-DP-19: the watcher's R1 for a pending live block. Title and
    /// visibility are the manifest's; `dashboard.yml` is never written.
    pub async fn resend_live(&mut self) -> Result<Hosted, DriveError> {
        self.send_r1(None).await
    }

    /// R1 for a published dashboard. `requested` is a visibility to apply
    /// (written to `dashboard.yml` once R1 succeeds); `None` sends the
    /// manifest's own and writes nothing.
    async fn send_r1(
        &mut self,
        requested: Option<manifest::Visibility>,
    ) -> Result<Hosted, DriveError> {
        let token = self
            .read_token()
            .await?
            .ok_or_else(|| DriveError::Keychain("no publisher token".into()))?;
        if self.is_refused(Some(&token)) {
            return Ok(Hosted::AuthFailed);
        }
        let manifest =
            manifest::load(&self.folder).map_err(|e| DriveError::Storage(e.to_string()))?;
        if manifest.state != PublishState::Published || manifest.slug != self.record.slug {
            return Err(DriveError::BadRequest(
                "dashboard.yml is no longer published under this slug".into(),
            ));
        }
        let visibility = requested.unwrap_or(manifest.visibility);
        let visibility = manifest::Visibility::parse(&super::approval::sent_visibility(
            &self.record,
            visibility.as_str(),
        ))
        .unwrap_or(manifest::Visibility::Private);
        match self.put_manifest(&token, &manifest, &manifest.title, visibility.as_str()).await
        {
            Ok(()) => {
                if requested.is_some() && manifest.visibility != visibility {
                    manifest::write_publish(&self.folder, PublishState::Published, Some(visibility))
                        .map_err(|e| {
                            DriveError::Storage(format!(
                                "R1 sent the new visibility, but dashboard.yml could not be written: {e}"
                            ))
                        })?;
                }
                if self.record.live_pending {
                    self.record.live_pending = false;
                    self.save()?;
                }
                Ok(Hosted::Live)
            }
            Err(CallError::Status { status: 401, .. }) => {
                self.record.refused_token = Some(fingerprint(Some(&token)));
                let _ = self.record.apply(Event::AuthFailed);
                self.save()?;
                Ok(Hosted::AuthFailed)
            }
            Err(e) => Err(DriveError::Update(e)),
        }
    }

    async fn put_manifest(
        &self,
        token: &Secret,
        manifest: &manifest::Manifest,
        title: &str,
        visibility: &str,
    ) -> Result<(), CallError> {
        let live = super::approval::approved_block(&self.record, manifest);
        // SV-PBF-286: whatever a caller passes, a dashboard streaming terminals
        // is never sent wider than a person confirmed.
        let visibility = super::approval::sent_visibility(&self.record, visibility);
        self.client.put_dashboard(token, &self.record.slug, title, &visibility, live, manifest.session_days, manifest.presentation().as_ref()).await
    }

    /// Refresh metadata with R1, publish with R2 on a publish intent, then
    /// R3, R4 per file and R5. An unchanged R3 is confirmed with R7 for server time.
    async fn push(
        &self,
        token: &Secret,
        publish: bool,
        payload: &Payload,
        title: &str,
        visibility: &str,
    ) -> Result<Stamp, Outcome> {
        let slug = self.record.slug.as_str();
        let client = &self.client;
        let fail = |e: CallError| self.failure(e);
        self.live()?;
        let manifest = manifest::load(&self.folder)
            .map_err(|_| Outcome::NoRetry(LastError::ManifestUnreadable))?;
        self.put_manifest(token, &manifest, title, visibility).await.map_err(fail)?;
        if publish {
            self.live()?;
            client.publish(token, slug).await.map_err(fail)?;
        }
        let mut expected = self.record.current_version;
        self.live()?;
        let reply = match client.create_version(token, slug, payload, expected).await {
            // This Mac's version is out of step with the server: a commit whose
            // reply was lost, for example. Take the server's current version and
            // ask once more. With the same source the server answers unchanged.
            // One token is one publisher, so the newer version is this owner's;
            // with a different source this push replaces it (last write wins).
            Err(CallError::Status { status: 409, code }) if code == "version_conflict" => {
                self.live()?;
                expected = client.current_version(token, slug).await.map_err(fail)?;
                self.live()?;
                client
                    .create_version(token, slug, payload, expected)
                    .await
                    .map_err(fail)?
            }
            other => other.map_err(fail)?,
        };
        let version = match reply {
            Version::Unchanged {
                version,
                checked_at,
            } => {
                // The live version already has this source. Only checked_at
                // moves. The Worker's unchanged reply may not carry the server
                // time, so R7 gets it then.
                let checked_at = match checked_at {
                    Some(checked_at) => checked_at,
                    None => {
                        self.live()?;
                        client.checkin(token, slug).await.map_err(fail)?
                    }
                };
                return Ok(Stamp {
                    version: Some(version),
                    pushed_at: None,
                    checked_at,
                });
            }
            Version::New(version) => version,
        };
        // A 404 here means the new version was cleaned up before it committed
        // (it waited too long). That is retried: the next push asks for a new one.
        let fail_version = |e: CallError| match e {
            CallError::Status { status: 404, .. } => Outcome::Failed(LastError::ServerError),
            e => self.failure(e),
        };
        for file in &payload.files {
            self.live()?;
            client
                .upload(token, slug, version, file)
                .await
                .map_err(fail_version)?;
        }
        let retain = self.live()?.retain;
        let pushed_at = client
            .commit(token, slug, version, expected, retain)
            .await
            .map_err(fail_version)?;
        Ok(Stamp {
            version: Some(version),
            checked_at: pushed_at.clone(),
            pushed_at: Some(pushed_at),
        })
    }

    /// Map a failed call to the watcher's outcome. E6 is decided by the local
    /// state: silent when the owner unpublished, a failure when it is published.
    fn failure(&self, e: CallError) -> Outcome {
        match e {
            CallError::NoResponse => Outcome::Failed(LastError::NoResponse),
            CallError::Status { status: 401, .. } => Outcome::AuthFailed,
            CallError::Status { status: 409, code } if code == "version_conflict" => {
                Outcome::NoRetry(LastError::RemoteVersionChanged)
            }
            CallError::Status { status: 409, code } if code == "slug_taken" => {
                Outcome::NoRetry(LastError::SlugTaken)
            }
            // R2, R3, R7: no dashboard under this slug for this token.
            CallError::Status { status: 404, code } if code == "not_found" => {
                Outcome::NoRetry(LastError::NotOwner)
            }
            CallError::BadReply => Outcome::NoRetry(LastError::ServerError),
            CallError::Status { status: 409, code } if code == "not_published" => {
                if self.record.state == LocalState::Unpublished {
                    Outcome::Ignored
                } else {
                    Outcome::NoRetry(LastError::DashboardNotPublished)
                }
            }
            CallError::Status {
                status: 429 | 500..=599,
                ..
            } => Outcome::Failed(LastError::ServerError),
            // Other refusals (E4, E7 to E13) are not retried. The next K1 tick decides.
            CallError::Status { .. } => Outcome::NoRetry(LastError::ServerError),
        }
    }

    /// Write a finished request into the record and save it. Only a success
    /// moves the version and the sha. A failure leaves them at the last commit.
    fn settle(
        &mut self,
        result: Result<Stamp, Outcome>,
        sha: Option<String>,
    ) -> Result<(), DriveError> {
        let publishing = self.record.state == LocalState::Publishing;
        match result {
            Ok(stamp) => {
                if let Some(version) = stamp.version {
                    self.record.current_version = Some(version);
                }
                if let Some(pushed_at) = stamp.pushed_at {
                    self.record.pushed_at = Some(pushed_at);
                }
                self.record.checked_at = Some(stamp.checked_at);
                if let Some(sha) = sha {
                    self.record.last_sha = Some(sha);
                }
                let event = if publishing {
                    Event::FirstCommitOk
                } else {
                    Event::PushOk
                };
                self.record.apply(event)?;
            }
            Err(Outcome::AuthFailed) => self.record.apply(Event::AuthFailed)?,
            // E6 while unpublished: the owner took the dashboard down. No change.
            Err(Outcome::Ignored | Outcome::Success) => {}
            Err(Outcome::Failed(e) | Outcome::NoRetry(e)) => {
                let event = if publishing {
                    Event::PublishFailed(e)
                } else {
                    Event::PushFailed(e)
                };
                self.record.apply(event)?;
            }
        }
        self.save()
    }

    /// R6 (DR-4 step 5, AC-7 unpublish): take the hosted copy down. The
    /// record moves to `unpublished` first, so the watcher stops at once, and
    /// the request in flight is cancelled, so its late result is dropped. The
    /// record keeps a pending marker until R6 is confirmed, so a failure
    /// survives a restart. Calling this again retries R6.
    pub async fn unpublish(&mut self) -> Result<Option<super::client::DeletedFiles>, DriveError> {
        let id = self.watcher.cancel_for_unpublish();
        if self.record.state != LocalState::Unpublished {
            self.record.apply(Event::Unpublish)?;
        }
        // The owed takedown is saved before any request, so a request that is
        // dropped or cannot start still leaves a retry.
        if !self.record.takedown_pending() {
            self.record.apply(Event::TakedownOwed)?;
        }
        self.save()?;
        match self.call_unpublish().await {
            Ok(deleted) => {
                log::info!(target: "dashboard_publish", "unpublish request {id} done");
                self.record.apply(Event::TakedownDone)?;
                self.save()?;
                Ok(deleted)
            }
            Err(e) => {
                log::warn!(target: "dashboard_publish", "unpublish request {id} failed: {e:?}");
                // A refusal recorded by the call is kept, so it is not sent again.
                self.save()?;
                Err(e)
            }
        }
    }

    /// The R6 call itself. A `not_found` 404 means this token's publisher has no
    /// dashboard under the slug. That is "nothing hosted" only when this Mac never
    /// committed a version. Otherwise the copy may still be live under the
    /// publisher that published it (a newly minted token is a new publisher), so
    /// the takedown stays owed. Any other 404 (a wrong path, a proxy page) also
    /// leaves it owed.
    async fn call_unpublish(&mut self) -> Result<Option<super::client::DeletedFiles>, DriveError> {
        let token = self
            .read_token()
            .await?
            .ok_or_else(|| DriveError::Keychain("no publisher token".into()))?;
        // A refused token is not sent again, even for a takedown (AC-6).
        if self.is_refused(Some(&token)) {
            return Err(DriveError::Refused);
        }
        match self.client.unpublish(&token, &self.record.slug).await {
            Ok(deleted) => Ok(deleted),
            Err(CallError::Status { status: 401, code }) => {
                self.record.refused_token = Some(fingerprint(Some(&token)));
                Err(DriveError::Unpublish(CallError::Status {
                    status: 401,
                    code,
                }))
            }
            Err(CallError::Status { status: 404, code })
                if code == "not_found" && self.record.current_version.is_none() =>
            {
                Ok(None)
            }
            Err(CallError::Status { status: 404, code }) if code == "not_found" => {
                Err(DriveError::NotOwner)
            }
            Err(e) => Err(DriveError::Unpublish(e)),
        }
    }

    /// Record a failure that sent nothing, in the record's own terms, and save.
    fn record_failure(&mut self, error: LastError) -> Result<(), DriveError> {
        let event = match self.record.state {
            LocalState::Publishing => Event::PublishFailed(error),
            LocalState::Published => Event::PushFailed(error),
            LocalState::Draft | LocalState::Unpublished => return Ok(()),
        };
        // A refused state is left as it is: the refusal is the reason shown.
        if self.record.apply(event).is_ok() {
            self.save()?;
        }
        Ok(())
    }

    /// True when `token` is the one saved as refused.
    fn is_refused(&self, token: Option<&Secret>) -> bool {
        self.record.refused_token.as_deref() == Some(fingerprint(token).as_str())
    }

    /// Save the record. A takedown made on disk since our last write wins: an
    /// `unpublished` record on disk is adopted, not overwritten.
    fn save(&mut self) -> Result<(), DriveError> {
        let on_disk =
            state::load(&self.data_dir, &self.record.slug).map_err(DriveError::Storage)?;
        if on_disk.state == LocalState::Unpublished && self.record.state != LocalState::Unpublished
        {
            log::info!(target: "dashboard_publish", "record unpublished on disk: adopted");
            // A refusal made in this run is kept, so the adopted record still holds it.
            let refused = self.record.refused_token.clone();
            self.record = on_disk;
            if refused.is_some() {
                self.record.refused_token = refused;
            }
            state::save(&self.data_dir, &self.record).map_err(DriveError::Storage)?;
            return Ok(());
        }
        state::save(&self.data_dir, &self.record).map_err(DriveError::Storage)
    }
}

/// A fingerprint of a token, so a replaced token is told apart from the one
/// that failed. Only the hash is kept, never the token.
pub(crate) fn fingerprint(token: Option<&Secret>) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.map_or("", Secret::expose).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::client::fake::{Fake, Reply, Seen};
    use super::super::manifest::MANIFEST_FILE;
    use super::super::payload::{self, FolderReader, Mode};
    use super::super::state::Event;
    use super::super::verbs;
    use super::*;
    use super::super::client::TOKEN_ACCOUNT;
    use crate::commands::credential_store::FakeStore;
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    pub(crate) const TOKEN: &str = "csp_test_token";

    /// A dashboard folder with two allowlisted files and a published manifest.
    pub(crate) fn dashboard() -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("index.html"), "<h1>AI usage</h1>").unwrap();
        fs::write(folder.path().join("style.css"), "h1 { color: red }").unwrap();
        fs::write(
            folder.path().join(MANIFEST_FILE),
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: published\n  visibility: private\n  files:\n    - index.html\n    - style.css\n",
        )
        .unwrap();
        folder
    }

    fn sha_of(folder: &Path) -> String {
        let manifest = super::super::manifest::load(folder).unwrap();
        payload::build(
            &manifest,
            Mode::Published,
            &FolderReader { folder },
        )
        .unwrap()
        .payload
        .manifest_sha
    }

    pub(crate) fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// A publisher for `folder` against `base`, with `record` saved first.
    async fn publisher(
        data: &Path,
        folder: &Path,
        base: &str,
        record: PublishRecord,
        store: Arc<FakeStore>,
        now: Instant,
    ) -> Publisher {
        state::save(data, &record).unwrap();
        let store: Arc<dyn CredentialStore> = store;
        Publisher::new(
            data,
            folder,
            Client::new(base).unwrap(),
            store,
            now,
        )
        .unwrap()
    }

    /// Arm the K2 debounce with a change to `index.html` at `at`.
    fn changed(p: &mut Publisher, at: Instant) {
        p.watcher
            .file_changed(at, "index.html", &["index.html".to_string()]);
    }

    pub(crate) fn token_store(token: &str) -> Arc<FakeStore> {
        let store = FakeStore::default();
        store.set(TOKEN_ACCOUNT, &Secret::new(token)).unwrap();
        Arc::new(store)
    }

    fn published(sha: Option<String>) -> PublishRecord {
        let mut r = PublishRecord::new("ai-usage");
        r.state = LocalState::Published;
        r.sync_status = SyncStatus::Ok;
        r.current_version = Some(1);
        r.pushed_at = Some("2026-10-15T08:00:00Z".into());
        r.last_sha = sha;
        r
    }

    /// The reply for a healthy publish: every route succeeds with server times.
    pub(crate) fn healthy(seen: &Seen) -> Reply {
        let path = seen.target.split('?').next().unwrap();
        match (seen.method.as_str(), path) {
            ("PUT", p) if p.ends_with("/files") => Reply::Json(204, String::new()),
            ("PUT", _) => Reply::Json(200, "{}".into()),
            ("POST", p) if p.ends_with("/versions") => {
                Reply::Json(201, r#"{"version":2,"status":"uploading"}"#.into())
            }
            ("POST", p) if p.ends_with("/commit") => Reply::Json(
                200,
                r#"{"version":2,"pushed_at":1792141200,"url":"x"}"#.into(),
            ),
            ("POST", p) if p.ends_with("/checkin") => {
                Reply::Json(200, r#"{"checked_at":1792141500}"#.into())
            }
            _ => Reply::Json(200, "{}".into()),
        }
    }

    fn count(calls: &[Seen], suffix: &str) -> usize {
        calls
            .iter()
            .filter(|c| c.target.split('?').next().unwrap().ends_with(suffix))
            .count()
    }

    #[tokio::test]
    async fn a_publish_runs_r1_r2_r3_r4_r5_and_takes_the_server_time() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.drive(Instant::now()).await.unwrap();

        let calls = fake.calls();
        assert_eq!(
            fake.routes(),
            vec![
                "PUT /api/dash/v1/d/ai-usage",
                "POST /api/dash/v1/d/ai-usage/publish",
                "POST /api/dash/v1/d/ai-usage/versions",
                "PUT /api/dash/v1/d/ai-usage/versions/2/files?path=index.html",
                "PUT /api/dash/v1/d/ai-usage/versions/2/files?path=style.css",
                "POST /api/dash/v1/d/ai-usage/versions/2/commit",
            ]
        );
        assert_eq!(calls[0].json()["title"], "AI usage");
        assert_eq!(calls[2].json()["expected_current"], serde_json::Value::Null);
        assert_eq!(calls[2].json()["source_sha256"], sha_of(folder.path()));
        assert_eq!(calls[5].json()["expected_current"], serde_json::Value::Null);
        assert_eq!(calls[5].json()["retain"], 5);

        let r = p.record();
        assert_eq!(r.state, LocalState::Published);
        assert_eq!(r.sync_status, SyncStatus::Ok);
        assert_eq!(r.current_version, Some(2));
        // The server's time, never the Mac clock.
        assert_eq!(r.pushed_at.as_deref(), Some("2026-10-16T09:00:00Z"));
        assert_eq!(r.checked_at.as_deref(), Some("2026-10-16T09:00:00Z"));
        assert_eq!(r.last_sha.as_deref(), Some(sha_of(folder.path()).as_str()));
        assert_eq!(state::load(data.path(), "ai-usage").unwrap(), *r);
    }

    #[tokio::test]
    async fn manifest_retention_reaches_the_commit_body() {
        let folder = dashboard();
        let path = folder.path().join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("publish:", "publish:\n  retain: current"),
        )
        .unwrap();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.drive(Instant::now()).await.unwrap();
        let calls = fake.calls();
        let commit = calls
            .iter()
            .find(|c| c.target.ends_with("/commit"))
            .unwrap();
        assert_eq!(commit.json()["retain"], 1);
    }

    #[tokio::test]
    async fn an_unchanged_version_sets_checked_at_only() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/versions") {
                Reply::Json(200, r#"{"version":1,"unchanged":true}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.drive(Instant::now() + secs(900)).await.unwrap();

        assert_eq!(count(&fake.calls(), "/files"), 0);
        assert_eq!(count(&fake.calls(), "/commit"), 0);
        assert_eq!(count(&fake.calls(), "/checkin"), 1);
        let r = p.record();
        assert_eq!(r.current_version, Some(1));
        assert_eq!(r.pushed_at.as_deref(), Some("2026-10-15T08:00:00Z"));
        assert_eq!(r.checked_at.as_deref(), Some("2026-10-16T09:05:00Z"));
        assert_eq!(r.sync_status, SyncStatus::Ok);
    }

    /// Rewrite the manifest to `draft`, as the verbs do on takedown.
    fn set_draft(folder: &Path) {
        let path = folder.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(path, text.replace("state: published", "state: draft")).unwrap();
    }

    #[tokio::test]
    async fn a_publishing_record_with_a_draft_manifest_pushes_nothing() {
        // A crash between the record save and the manifest write leaves this state.
        let folder = dashboard();
        set_draft(folder.path());
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            t0,
        )
        .await;
        p.drive(t0).await.unwrap();
        p.drive(t0 + secs(900)).await.unwrap();
        assert!(fake.calls().is_empty(), "a push with a draft manifest");
        assert_eq!(p.record().state, LocalState::Publishing);
    }

    #[tokio::test]
    async fn a_published_record_with_a_draft_manifest_pushes_nothing() {
        let folder = dashboard();
        set_draft(folder.path());
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        p.drive(t0 + secs(900)).await.unwrap();
        assert!(fake.calls().is_empty(), "a push with a draft manifest");
    }

    #[tokio::test]
    async fn a_failed_first_publish_after_r1_and_r2_is_completed_by_the_retry() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fail_versions = Arc::new(AtomicBool::new(true));
        let flag = fail_versions.clone();
        let fake = Fake::start(move |seen| {
            if seen.target.ends_with("/versions") && flag.load(Ordering::SeqCst) {
                Reply::Json(503, r#"{"error":"storage_unavailable"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            t0,
        )
        .await;
        // R1 and R2 succeed, R3 fails. The record stays publishing, so the
        // watcher keeps the first publish pending.
        p.drive(t0).await.unwrap();
        assert_eq!(p.record().state, LocalState::Publishing);
        assert_eq!(
            (p.record().sync_status, p.record().last_error),
            (SyncStatus::Failing, Some(LastError::ServerError))
        );
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            LocalState::Publishing
        );

        // The retry runs at K12 and sends the whole sequence again.
        fail_versions.store(false, Ordering::SeqCst);
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(p.record().state, LocalState::Published);
        assert_eq!(p.record().sync_status, SyncStatus::Ok);
        assert_eq!(p.record().current_version, Some(2));
        let routes = fake.routes();
        assert_eq!(
            routes.iter().filter(|r| r.ends_with("/publish")).count(),
            2,
            "R2 was not repeated: {routes:?}"
        );
        assert_eq!(
            routes
                .iter()
                .filter(|r| *r == "PUT /api/dash/v1/d/ai-usage")
                .count(),
            2,
            "R1 was not repeated"
        );
    }

    #[tokio::test]
    async fn a_takedown_on_disk_is_not_overwritten_by_a_later_save() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().to_path_buf();
        let mut takedown = published(None);
        takedown.state = LocalState::Unpublished;
        takedown.sync_status = SyncStatus::Idle;
        // The owner unpublishes on disk while the commit is in flight.
        let fake = Fake::start(move |seen| {
            if seen.target.ends_with("/commit") {
                state::save(&dir, &takedown).unwrap();
            }
            healthy(seen)
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();

        // The success is not written over the takedown.
        assert_eq!(p.record().state, LocalState::Unpublished);
        let saved = state::load(data.path(), "ai-usage").unwrap();
        assert_eq!(saved.state, LocalState::Unpublished);
        assert_eq!(
            saved.current_version,
            Some(1),
            "the push was saved over the takedown"
        );
    }

    #[tokio::test]
    async fn a_first_publish_failed_after_r3_is_taken_down_with_no_further_request() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/versions") {
                Reply::Json(503, r#"{"error":"storage_unavailable"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            t0,
        )
        .await;
        p.drive(t0).await.unwrap();
        assert_eq!(
            p.record().state,
            LocalState::Publishing,
            "the first publish is pending"
        );

        // The owner takes the failed first publish down through the verb.
        verbs::unpublish(folder.path(), data.path()).unwrap();
        let sent = fake.routes().len();
        // The K12 retry and the next tick send nothing.
        p.drive(t0 + secs(60)).await.unwrap();
        p.drive(t0 + secs(900)).await.unwrap();
        assert_eq!(fake.routes().len(), sent, "a request after the takedown");
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            LocalState::Unpublished
        );
        assert_eq!(
            manifest::load(folder.path()).unwrap().state,
            PublishState::Draft
        );
    }

    #[tokio::test]
    async fn a_takedown_during_the_sequence_stops_it_before_the_next_call() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let (folder_path, data_path) = (folder.path().to_path_buf(), data.path().to_path_buf());
        let fake = Fake::start(move |seen| {
            if seen.target.ends_with("/versions") {
                // The owner takes the dashboard down while R3 is in flight.
                set_draft(&folder_path);
                let mut taken = PublishRecord::new("ai-usage");
                taken.state = LocalState::Unpublished;
                state::save(&data_path, &taken).unwrap();
                Reply::Json(201, r#"{"version":2,"status":"uploading"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            t0,
        )
        .await;
        p.drive(t0).await.unwrap();
        // No upload and no commit after the takedown.
        assert_eq!(
            fake.routes(),
            vec![
                "PUT /api/dash/v1/d/ai-usage",
                "POST /api/dash/v1/d/ai-usage/publish",
                "POST /api/dash/v1/d/ai-usage/versions",
            ]
        );
        assert_eq!(p.record().state, LocalState::Unpublished);
    }

    #[tokio::test]
    async fn an_unchanged_version_that_carries_checked_at_needs_no_checkin() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/versions") {
                Reply::Json(
                    200,
                    r#"{"version":1,"unchanged":true,"checked_at":1792141800}"#.into(),
                )
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.drive(Instant::now() + secs(900)).await.unwrap();
        assert_eq!(count(&fake.calls(), "/checkin"), 0, "R7 was called");
        assert_eq!(
            p.record().checked_at.as_deref(),
            Some("2026-10-16T09:10:00Z")
        );
    }

    #[tokio::test]
    async fn unpublish_calls_r6_once_and_moves_the_record_to_unpublished() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/unpublish") {
                Reply::Json(200, r#"{"deleted":{"versions":2,"files":4}}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(Some(sha_of(folder.path()))),
            token_store(TOKEN),
            t0,
        )
        .await;
        let deleted = p.unpublish().await.unwrap();
        let mut status = super::super::status::PublicationStatus::default();
        status.unpublished(Ok(deleted));
        assert_eq!(
            serde_json::to_value(status).unwrap()["hostedFilesDeleted"],
            serde_json::json!({"versions":2,"files":4})
        );
        assert_eq!(
            fake.routes(),
            vec!["POST /api/dash/v1/d/ai-usage/unpublish"]
        );
        let saved = state::load(data.path(), "ai-usage").unwrap();
        assert_eq!(saved.state, LocalState::Unpublished);
        assert_eq!(saved.current_version, None);
        // The watcher is silent after the takedown.
        p.drive(t0 + secs(900)).await.unwrap();
        assert_eq!(fake.calls().len(), 1, "a call after unpublish");
        let mut rec = state::load(data.path(), "ai-usage").unwrap();
        rec.apply(Event::Publish).unwrap();
        let now = Instant::now();
        let mut again = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            now,
        )
        .await;
        again.drive(now).await.unwrap();
        let calls = fake.calls();
        let open = calls
            .iter()
            .find(|c| c.target.ends_with("/versions"))
            .unwrap();
        assert_eq!(open.json()["expected_current"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_failed_unpublish_is_reported_and_retried_by_the_next_call() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fail = Arc::new(AtomicBool::new(true));
        let flag = fail.clone();
        let fake = Fake::start(move |seen| {
            if seen.target.ends_with("/unpublish") && flag.load(Ordering::SeqCst) {
                Reply::Json(503, r#"{"error":"storage_unavailable"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        let err = p.unpublish().await.unwrap_err();
        assert!(matches!(
            err,
            DriveError::Unpublish(CallError::Status { status: 503, .. })
        ));
        // The failure is not silent: the local state is unpublished, and the
        // saved record says so, but the hosted copy may still be live.
        assert_eq!(p.record().state, LocalState::Unpublished);
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            LocalState::Unpublished
        );
        // The pending marker is saved, so a restart still knows R6 is owed.
        let saved = state::load(data.path(), "ai-usage").unwrap();
        assert!(saved.takedown_pending());
        assert_eq!(saved.last_error, Some(LastError::TakedownUnconfirmed));

        fail.store(false, Ordering::SeqCst);
        p.unpublish().await.unwrap();
        assert_eq!(
            count(&fake.calls(), "/unpublish"),
            2,
            "the retry was not sent"
        );
        assert!(!state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    #[tokio::test]
    async fn a_bare_404_leaves_the_takedown_owed() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/unpublish") {
                Reply::Json(404, "<html>not found</html>".into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        assert!(p.unpublish().await.is_err());
        assert!(state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    #[tokio::test]
    async fn a_404_for_a_dashboard_this_mac_published_keeps_the_takedown_owed() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/unpublish") {
                Reply::Json(404, r#"{"error":"not_found"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        // published() has a committed version, so a 404 means "not this token's".
        assert!(matches!(p.unpublish().await, Err(DriveError::NotOwner)));
        assert!(state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    #[tokio::test]
    async fn a_404_for_a_dashboard_never_committed_counts_as_taken_down() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/unpublish") {
                Reply::Json(404, r#"{"error":"not_found"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.unpublish().await.unwrap();
        assert!(!state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    #[tokio::test]
    async fn a_takedown_is_sent_when_a_listed_file_is_missing() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        // The file list now points at a file that is gone: the manifest no longer validates.
        fs::remove_file(folder.path().join("style.css")).unwrap();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.unpublish().await.unwrap();
        assert_eq!(fake.routes(), vec!["POST /api/dash/v1/d/ai-usage/unpublish"]);
        assert!(!state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    #[tokio::test]
    async fn a_failed_first_push_is_pushed_again_by_the_next_call() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fail = Arc::new(AtomicBool::new(true));
        let flag = fail.clone();
        let fake = Fake::start(move |seen| {
            if seen.target.ends_with("/versions") && flag.load(Ordering::SeqCst) {
                Reply::Json(503, r#"{"error":"storage_unavailable"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            t0,
        )
        .await;
        // The first push fails at R3, so the record stays publishing and the owed push is saved.
        p.drive(t0).await.unwrap();
        assert_eq!(p.record().state, LocalState::Publishing);
        assert!(push_owed(data.path(), folder.path()));

        // The next publish call builds a new publisher from the saved record, and
        // the push runs at once, with no retry delay.
        fail.store(false, Ordering::SeqCst);
        let mut next = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            token_store(TOKEN),
            Instant::now(),
        )
        .unwrap();
        next.drive(Instant::now()).await.unwrap();
        assert_eq!(next.record().state, LocalState::Published);
        assert!(!push_owed(data.path(), folder.path()));
    }

    #[tokio::test]
    async fn a_refused_token_stays_refused_across_publishers_until_replaced() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.header("authorization") == Some("Bearer bad-token") {
                Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store("bad-token"),
            t0,
        )
        .await;
        p.drive(t0).await.unwrap();
        assert_eq!(p.record().sync_status, SyncStatus::AuthFailed);
        assert!(state::load(data.path(), "ai-usage").unwrap().refused_token.is_some());
        let sent = fake.calls().len();

        // The next publish call builds a new publisher. It must not send the refused token again.
        let mut again = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            token_store("bad-token"),
            Instant::now(),
        )
        .unwrap();
        again.drive(Instant::now()).await.unwrap();
        assert_eq!(fake.calls().len(), sent, "the refused token was sent again");

        // A new token clears the refusal and the push goes through.
        let mut fresh = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            token_store("good-token"),
            Instant::now(),
        )
        .unwrap();
        fresh.drive(Instant::now()).await.unwrap();
        assert_eq!(fresh.record().state, LocalState::Published);
        assert!(fresh.record().refused_token.is_none());
    }

    #[tokio::test]
    async fn a_refused_token_is_not_sent_by_an_unpublish_or_a_later_publish() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.header("authorization") == Some("Bearer bad-token") {
                Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store("bad-token"),
            t0,
        )
        .await;
        p.drive(t0).await.unwrap();
        let sent = fake.calls().len();

        // A takedown with the refused token makes no call, and is owed.
        assert!(matches!(p.unpublish().await, Err(DriveError::Refused)));
        assert_eq!(fake.calls().len(), sent, "unpublish sent the refused token");
        assert!(state::load(data.path(), "ai-usage").unwrap().takedown_pending());

        // A later publish with the same token sends nothing either.
        let mut again = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            token_store("bad-token"),
            Instant::now(),
        )
        .unwrap();
        again.drive(Instant::now()).await.unwrap();
        assert_eq!(fake.calls().len(), sent, "publish sent the refused token");
    }

    #[tokio::test]
    async fn a_version_conflict_takes_the_servers_version_and_asks_once_more() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            let path = seen.target.split('?').next().unwrap();
            if seen.method == "POST" && path.ends_with("/versions") {
                // The server is at version 3: a commit whose reply this Mac lost.
                if seen.json()["expected_current"] == 3 {
                    Reply::Json(200, r#"{"version":3,"unchanged":true}"#.into())
                } else {
                    Reply::Json(409, r#"{"error":"version_conflict"}"#.into())
                }
            } else if seen.method == "GET" {
                Reply::Json(200, r#"{"slug":"ai-usage","current_version":3,"shares":[]}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(p.record().current_version, Some(3));
        assert_eq!(p.record().sync_status, SyncStatus::Ok);
        assert_eq!(state::load(data.path(), "ai-usage").unwrap().current_version, Some(3));
    }

    #[tokio::test]
    async fn a_401_on_a_takedown_is_saved_and_the_token_is_not_sent_again() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/unpublish") {
                Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        assert!(p.unpublish().await.is_err());
        let saved = state::load(data.path(), "ai-usage").unwrap();
        assert!(saved.refused_token.is_some());
        assert!(saved.takedown_pending());
        let sent = fake.calls().len();
        assert!(matches!(p.unpublish().await, Err(DriveError::Refused)));
        assert_eq!(fake.calls().len(), sent, "the refused token was sent again");
    }

    #[tokio::test]
    async fn files_that_do_not_build_are_recorded_and_retried_after_the_debounce() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        fs::remove_file(folder.path().join("style.css")).unwrap();
        changed(&mut p, t0);
        assert!(p.drive(t0 + secs(60)).await.is_err());
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().last_error,
            Some(LastError::FilesInvalid)
        );
        // The next pass of the loop does not build again; the debounce does.
        assert!(p.drive(t0 + secs(65)).await.is_ok());
        assert!(fake.calls().is_empty(), "a request was sent for files that do not build");
    }

    #[tokio::test]
    async fn a_dashboard_unpublished_on_the_server_pauses_pushes() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(409, r#"{"error":"not_published"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(p.record().last_error, Some(LastError::DashboardNotPublished));
        let sent = fake.calls().len();
        changed(&mut p, t0 + secs(120));
        p.drive(t0 + secs(180)).await.unwrap();
        p.drive(t0 + secs(1000)).await.unwrap();
        assert_eq!(fake.calls().len(), sent, "pushed while the server says unpublished");
    }

    #[tokio::test]
    async fn a_slug_taken_by_another_publisher_is_named() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.method == "PUT" && !seen.target.contains("/files") {
                Reply::Json(409, r#"{"error":"slug_taken"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        let t0 = Instant::now();
        let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), t0).await;
        p.drive(t0).await.unwrap();
        assert_eq!(p.record().last_error, Some(LastError::SlugTaken));
        assert_eq!(fake.calls().len(), 1, "only R1 was sent");
    }

    #[tokio::test]
    async fn without_a_host_an_owed_takedown_that_never_committed_is_settled() {
        if DASH_HOST.is_some() {
            return; // only meaningful in a build with no host
        }
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        rec.apply(Event::Unpublish).unwrap();
        rec.apply(Event::TakedownOwed).unwrap();
        state::save(data.path(), &rec).unwrap();
        take_down_hosted(data.path(), folder.path()).await.unwrap();
        assert!(!state::load(data.path(), "ai-usage").unwrap().takedown_pending());
    }

    /// A Keychain that can be made to fail, as a locked one does.
    #[derive(Default)]
    struct FlakyStore {
        inner: FakeStore,
        failing: AtomicBool,
    }

    impl CredentialStore for FlakyStore {
        fn get(&self, account: &str) -> Result<Option<Secret>, String> {
            if self.failing.load(Ordering::SeqCst) {
                Err("keychain locked".into())
            } else {
                self.inner.get(account)
            }
        }
        fn set(&self, account: &str, value: &Secret) -> Result<(), String> {
            self.inner.set(account, value)
        }
    }

    #[tokio::test]
    async fn a_keychain_error_on_a_due_push_is_retried_not_left_in_flight() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let store = Arc::new(FlakyStore::default());
        store.set(TOKEN_ACCOUNT, &Secret::new(TOKEN)).unwrap();
        state::save(data.path(), &published(None)).unwrap();
        let t0 = Instant::now();
        let mut p = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            store.clone(),
            t0,
        )
        .unwrap();
        changed(&mut p, t0);
        store.failing.store(true, Ordering::SeqCst);
        assert!(matches!(p.drive(t0 + secs(60)).await, Err(DriveError::Keychain(_))));
        assert!(fake.calls().is_empty());

        // The Keychain is readable again: the retry goes out at K12.
        store.failing.store(false, Ordering::SeqCst);
        p.drive(t0 + secs(121)).await.unwrap();
        assert!(!fake.calls().is_empty(), "the push was left in flight");
    }

    #[tokio::test]
    async fn a_slug_edited_after_publishing_is_named_and_nothing_is_pushed() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        let text = fs::read_to_string(folder.path().join(MANIFEST_FILE)).unwrap();
        fs::write(
            folder.path().join(MANIFEST_FILE),
            text.replace("slug: ai-usage", "slug: other-name"),
        )
        .unwrap();
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().last_error,
            Some(LastError::SlugChanged)
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_publishing_record_told_not_published_sends_r2_again() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        rec.apply(Event::PublishFailed(LastError::DashboardNotPublished))
            .unwrap();
        let t0 = Instant::now();
        let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), t0).await;
        p.drive(t0).await.unwrap();
        assert!(fake.routes().iter().any(|r| r.ends_with("/publish")), "R2 was not sent");
        assert_eq!(p.record().state, LocalState::Published);
    }

    #[tokio::test]
    async fn an_unpublish_drops_the_request_in_flight() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.watcher
            .poll(
                t0 + secs(60),
                &p.record,
                folder.path(),
            )
            .unwrap();
        // A push is taken and is in flight when the owner unpublishes.
        let push = p.watcher.next_action(&p.record).expect("a push");

        p.unpublish().await.unwrap();
        // Its late success is dropped, so nothing is recorded as pushed.
        assert!(!p
            .watcher
            .request_done(t0 + secs(61), push.id, Outcome::Success));
        assert_eq!(
            fake.routes(),
            vec!["POST /api/dash/v1/d/ai-usage/unpublish"]
        );
        assert_eq!(p.record().last_sha, None);
    }

    #[tokio::test]
    async fn an_interval_with_an_unchanged_sha_updates_metadata_then_checks_in() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(Some(sha_of(folder.path()))),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.drive(Instant::now() + secs(900)).await.unwrap();
        assert_eq!(fake.routes(), vec!["PUT /api/dash/v1/d/ai-usage", "POST /api/dash/v1/d/ai-usage/checkin"]);
    }

    #[tokio::test]
    async fn a_503_on_commit_keeps_the_last_good_version_and_retries_at_k12() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(503, r#"{"error":"storage_unavailable"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        let r = p.record();
        assert_eq!(
            (r.sync_status, r.last_error),
            (SyncStatus::Failing, Some(LastError::ServerError))
        );
        assert_eq!(r.current_version, Some(1));
        assert_eq!(r.last_sha, None);

        // Retries at +1, +2 and +4 minutes after each failure, then the tick.
        for at in [119, 120, 239, 240, 479, 480, 800] {
            p.drive(t0 + secs(at)).await.unwrap();
        }
        assert_eq!(
            count(&fake.calls(), "/commit"),
            4,
            "one try and three retries"
        );
        p.drive(t0 + secs(900)).await.unwrap();
        assert_eq!(
            count(&fake.calls(), "/commit"),
            5,
            "the K1 tick tries again"
        );
    }

    #[tokio::test]
    async fn a_version_conflict_is_not_retried_and_is_not_overwritten() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(409, r#"{"error":"version_conflict"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        p.drive(t0 + secs(180)).await.unwrap();
        assert_eq!(count(&fake.calls(), "/commit"), 1, "E5 was retried");
        let r = p.record();
        assert_eq!(
            (r.sync_status, r.last_error),
            (SyncStatus::Failing, Some(LastError::RemoteVersionChanged))
        );
        assert_eq!(r.current_version, Some(1));
    }

    #[tokio::test]
    async fn not_published_on_a_published_record_fails_without_a_retry() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(409, r#"{"error":"not_published"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        p.drive(t0 + secs(180)).await.unwrap();
        assert_eq!(count(&fake.calls(), "/commit"), 1);
        let r = p.record();
        assert_eq!(
            (r.sync_status, r.last_error),
            (SyncStatus::Failing, Some(LastError::DashboardNotPublished))
        );
    }

    #[tokio::test]
    async fn not_published_on_an_unpublished_record_is_silent() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = published(None);
        rec.state = LocalState::Unpublished;
        rec.sync_status = SyncStatus::Idle;
        let p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            rec,
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        assert_eq!(
            p.failure(CallError::Status {
                status: 409,
                code: "not_published".into()
            }),
            Outcome::Ignored
        );
    }

    #[tokio::test]
    async fn a_401_stops_every_call_until_the_token_is_replaced() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            let auth = seen.header("authorization").unwrap_or("");
            if auth == "Bearer csp_old" {
                Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let store = token_store("csp_old");
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            store.clone(),
            t0,
        )
        .await;
        changed(&mut p, t0);
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(p.record().sync_status, SyncStatus::AuthFailed);
        assert_eq!(p.record().state, LocalState::Published, "not draft");

        p.drive(t0 + secs(120)).await.unwrap();
        p.drive(t0 + secs(900)).await.unwrap();
        assert_eq!(fake.calls().len(), 1, "a call while auth_failed");

        // The token is replaced: the watcher resumes at the next tick.
        store.set(TOKEN_ACCOUNT, &Secret::new("csp_new")).unwrap();
        p.drive(t0 + secs(1000)).await.unwrap();
        assert_eq!(p.record().sync_status, SyncStatus::Ok);
        // One push after the new token: metadata, versions, two files and the commit.
        assert_eq!(fake.calls().len(), 6, "the push after the new token");
    }

    #[tokio::test]
    async fn without_a_keychain_token_nothing_is_sent() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            Arc::new(FakeStore::default()),
            t0,
        )
        .await;
        p.drive(t0 + secs(900)).await.unwrap();
        assert!(fake.calls().is_empty(), "a call without a token");
        assert_eq!(p.record().sync_status, SyncStatus::AuthFailed);
    }

    #[tokio::test]
    async fn an_interrupted_upload_leaves_the_last_commit_and_the_next_push_expects_it() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let drop_uploads = Arc::new(AtomicBool::new(true));
        let flag = drop_uploads.clone();
        // Each push creates the next version: 2, then 3.
        let next_version = Arc::new(AtomicU64::new(2));
        let fake = Fake::start(move |seen| {
            let is_style = seen.target.ends_with("path=style.css");
            if is_style && flag.load(Ordering::SeqCst) {
                Reply::Drop
            } else if seen.target.ends_with("/versions") {
                let n = next_version.fetch_add(1, Ordering::SeqCst);
                Reply::Json(201, format!(r#"{{"version":{n},"status":"uploading"}}"#))
            } else {
                healthy(seen)
            }
        })
        .await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store(TOKEN),
            t0,
        )
        .await;
        changed(&mut p, t0);
        // The push dies mid-upload: version 2 is left `uploading` on the server.
        p.drive(t0 + secs(60)).await.unwrap();
        assert_eq!(p.record().current_version, Some(1), "the record moved");
        assert_eq!(p.record().sync_status, SyncStatus::Failing);
        assert_eq!(count(&fake.calls(), "/commit"), 0);

        // The app restarts: the record is read back from disk.
        drop(p);
        drop_uploads.store(false, Ordering::SeqCst);
        let mut p = Publisher::new(
            data.path(),
            folder.path(),
            Client::new(&fake.base).unwrap(),
            token_store(TOKEN),
            t0 + secs(100),
        )
        .unwrap();
        assert_eq!(p.record().current_version, Some(1));
        p.drive(t0 + secs(1000)).await.unwrap();

        let commits: Vec<Seen> = fake
            .calls()
            .into_iter()
            .filter(|c| c.target.ends_with("/commit"))
            .collect();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].json()["expected_current"], 1);
        assert_eq!(p.record().current_version, Some(3), "a new version");
        assert_eq!(p.record().sync_status, SyncStatus::Ok);
    }

    #[tokio::test]
    async fn a_restart_resumes_the_interval_of_a_published_dashboard() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let t0 = Instant::now();
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(Some(sha_of(folder.path()))),
            token_store(TOKEN),
            t0,
        )
        .await;
        p.drive(t0 + secs(899)).await.unwrap();
        assert!(fake.calls().is_empty(), "the interval is not due at 899 s");
        p.drive(t0 + secs(900)).await.unwrap();
        assert_eq!(
            fake.routes(),
            vec!["PUT /api/dash/v1/d/ai-usage", "POST /api/dash/v1/d/ai-usage/checkin"],
            "the interval resumes after 15 minutes"
        );
    }

    #[tokio::test]
    async fn the_token_never_reaches_the_record_file() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.target.ends_with("/commit") {
                Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())
            } else {
                healthy(seen)
            }
        })
        .await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            published(None),
            token_store("csp_secret_value"),
            Instant::now(),
        )
        .await;
        changed(&mut p, Instant::now());
        p.drive(Instant::now() + secs(60)).await.unwrap();
        let saved =
            fs::read_to_string(state::record_path(data.path(), "ai-usage").unwrap()).unwrap();
        assert!(
            !saved.contains("csp_"),
            "the token was written to the record"
        );
        assert!(!format!("{:?}", p.record()).contains("csp_"));
    }

    #[test]
    fn the_publisher_reads_no_file_and_imports_no_remote_or_routine_module() {
        let src = include_str!("push.rs");
        let src = src.split("#[cfg(test)]").next().unwrap();
        for direct in ["fs::", "File::open", "read_dir", "read_to_string"] {
            assert!(
                !src.contains(direct),
                "the push driver reads files: {direct}"
            );
        }
        for line in src.lines().filter(|l| l.trim_start().starts_with("use ")) {
            for needle in ["remote", "routine"] {
                assert!(!line.contains(needle), "push imports {needle}: {line}");
            }
        }
    }

    /// A draft dashboard whose manifest ends with `live_block`.
    fn live_dashboard(live_block: &str) -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("index.html"), "<h1>AI usage</h1>").unwrap();
        fs::write(
            folder.path().join(MANIFEST_FILE),
            format!("slug: ai-usage\ntitle: AI usage\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n{live_block}"),
        )
        .unwrap();
        folder
    }

    /// Publish `folder` with the live block `decide` returns (called directly,
    /// below the confirmation a person gives in the app), then run the first
    /// push against a healthy fake. Returns the fake's calls, the
    /// decision and the saved record.
    async fn publish_live(
        folder: &Path,
        data: &Path,
    ) -> (Vec<Seen>, super::super::approval::Decision, PublishRecord) {
        use super::super::approval::decide;
        let m = manifest::load(folder).unwrap();
        let current = state::load(data, "ai-usage").unwrap().live_approved;
        let decision = decide(&m, folder, current.as_ref(), None);
        verbs::publish(folder, data, manifest::Visibility::Private, decision.approved.clone())
            .unwrap();
        let fake = Fake::start(healthy).await;
        let store: Arc<dyn CredentialStore> = token_store(TOKEN);
        let mut p = Publisher::new(data, folder, Client::new(&fake.base).unwrap(), store, Instant::now())
            .unwrap();
        p.drive(Instant::now()).await.unwrap();
        (fake.calls(), decision, state::load(data, "ai-usage").unwrap())
    }

    fn r1_live_queries(calls: &[Seen]) -> serde_json::Value {
        let r1 = calls
            .iter()
            .find(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/ai-usage")
            .expect("R1 was sent");
        let body = r1.json();
        assert!(body.as_object().unwrap().contains_key("live_queries"), "{body}");
        body["live_queries"].clone()
    }

    /// SW-DP-19: a live block `decide` turns on at publish is saved on the
    /// record and R1 sends the list. SV-PBF-286: in the app a person confirms
    /// it first (`management::publish_bound`); this calls `decide` directly.
    #[tokio::test]
    async fn a_live_block_is_turned_on_at_publish_and_sent_on_r1() {
        use super::super::approval::LiveResult;
        let folder = live_dashboard("live:\n  queries: all\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, decision, record) = publish_live(folder.path(), data.path()).await;
        assert_eq!(decision.result, LiveResult::On);
        assert_eq!(r1_live_queries(&calls), serde_json::json!("all"));
        assert_eq!(record.state, LocalState::Published);
        let on = record.live_approved.expect("live block saved");
        assert_eq!(on.queries, manifest::LiveQueries::All);

        // A second publish of the same block (after a takedown) is unchanged.
        verbs::unpublish(folder.path(), data.path()).unwrap();
        let mut r = state::load(data.path(), "ai-usage").unwrap();
        r.apply(Event::TakedownDone).unwrap();
        state::save(data.path(), &r).unwrap();
        let (calls, decision, _) = publish_live(folder.path(), data.path()).await;
        assert_eq!(decision.result, LiveResult::Unchanged);
        assert_eq!(r1_live_queries(&calls), serde_json::json!("all"));
    }

    /// No block: R1 sends null and live is none. A presence-only block sends `[]`.
    #[tokio::test]
    async fn no_live_block_sends_null_and_presence_only_sends_an_empty_list() {
        use super::super::approval::LiveResult;
        let plain = live_dashboard("");
        let data = tempfile::tempdir().unwrap();
        let (calls, decision, record) = publish_live(plain.path(), data.path()).await;
        assert_eq!(decision.result, LiveResult::None);
        assert_eq!(r1_live_queries(&calls), serde_json::Value::Null);
        assert_eq!((record.state, record.sync_status), (LocalState::Published, SyncStatus::Ok));
        assert_eq!(record.live_approved, None);

        let presence = live_dashboard("live:\n  queries: []\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, decision, _) = publish_live(presence.path(), data.path()).await;
        assert_eq!(decision.result, LiveResult::On);
        assert_eq!(r1_live_queries(&calls), serde_json::json!([]));
    }

    /// SW-DP-20: R1 carries the block's `live_streams` (null when it declares
    /// none); a streams-only block sends `live_queries: []`, so the link opens.
    #[tokio::test]
    async fn r1_sends_the_declared_streams() {
        let r1 = |calls: &[Seen]| {
            calls
                .iter()
                .find(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/ai-usage")
                .expect("R1 was sent")
                .json()
        };
        let streams = live_dashboard("live:\n  streams: [tick, tick]\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, _, record) = publish_live(streams.path(), data.path()).await;
        let body = r1(&calls);
        assert_eq!(body["live_streams"], serde_json::json!(["tick"]));
        assert_eq!(body["live_queries"], serde_json::json!([]));
        assert_eq!(
            record.live_approved.unwrap().streams,
            vec!["tick".to_string()]
        );

        let queries = live_dashboard("live:\n  queries: [runs]\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, _, _) = publish_live(queries.path(), data.path()).await;
        let body = r1(&calls);
        assert!(
            body.as_object().unwrap().contains_key("live_streams"),
            "{body}"
        );
        assert_eq!(body["live_streams"], serde_json::Value::Null);
    }

    /// SW-DP-23: R1 carries `live_control: "owner"` for a block with
    /// `control: owner` (T2 `live_control`), saved on the record, and null for
    /// a block without it.
    #[tokio::test]
    async fn r1_sends_only_action_names_verbs_and_access_policy() {
        let folder=live_dashboard("actions:\n  close:\n    verb: setFields\n    who: owner\n    fields: {status: wontfix}\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, _, record) = publish_live(folder.path(), data.path()).await;
        let body = calls
            .iter()
            .find(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/ai-usage")
            .unwrap()
            .json();
        assert_eq!(
            body["live_actions"],
            serde_json::json!({"close":{"who":"owner","verb":"setFields"}})
        );
        assert_eq!(record.live_approved.unwrap().actions.len(), 1);
    }

    #[tokio::test]
    async fn r1_sends_presentation() {
        let folder =
            live_dashboard("viewers: {session_days: 30}\ninstall: {name: Family brief, icon: icon.png}\nchrome: minimal\n");
        fs::write(folder.path().join("icon.png"), b"png").unwrap();
        let path = folder.path().join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace("    - index.html", "    - index.html\n    - icon.png"),
        )
        .unwrap();
        let data = tempfile::tempdir().unwrap();
        let (calls, _, _) = publish_live(folder.path(), data.path()).await;
        let body = calls.iter().find(|c| c.method == "PUT").unwrap().json();
        assert_eq!(body["session_days"], 30);
        assert_eq!(
            body["presentation"],
            serde_json::json!({"install":{"name":"Family brief", "short_name":null, "icon":"icon.png", "theme_color":null}, "chrome":"minimal"})
        );

        let fake = Fake::start(healthy).await;
        let mut p = publisher(
            data.path(),
            folder.path(),
            &fake.base,
            state::load(data.path(), "ai-usage").unwrap(),
            token_store(TOKEN),
            Instant::now(),
        )
        .await;
        p.update_live(manifest::Visibility::Private).await.unwrap();
        assert_eq!(fake.calls()[0].json()["presentation"], body["presentation"]);
        assert_eq!(fake.calls()[0].json()["session_days"], 30);
        let text = fs::read_to_string(&path).unwrap();
        let mut value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        let fields = value.as_mapping_mut().unwrap();
        fields.remove(&serde_yaml::Value::String("install".into()));
        fields.remove(&serde_yaml::Value::String("chrome".into()));
        fs::write(&path, serde_yaml::to_string(&value).unwrap()).unwrap();
        p.update_live(manifest::Visibility::Private).await.unwrap();
        assert_eq!(
            fake.calls().last().unwrap().json()["presentation"],
            serde_json::Value::Null
        );
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            format!("{text}install: {{name: Family Desk, icon: icon.png}}\n"),
        )
        .unwrap();
        p.drive(Instant::now() + secs(901)).await.unwrap();
        let calls = fake.calls();
        let checkin_r1 = &calls[calls.len() - 2];
        assert_eq!(checkin_r1.method, "PUT");
        assert_eq!(checkin_r1.json()["session_days"], 30);
        assert_eq!(
            checkin_r1.json()["presentation"]["install"]["name"],
            "Family Desk"
        );
        assert!(calls.last().unwrap().target.ends_with("/checkin"));
        assert!(!calls.iter().any(|c| c.target.ends_with("/versions")));
    }

    #[tokio::test]
    async fn r1_sends_live_control() {
        let r1 = |calls: &[Seen]| {
            calls
                .iter()
                .find(|c| c.method == "PUT" && c.target == "/api/dash/v1/d/ai-usage")
                .expect("R1 was sent")
                .json()
        };
        let on = live_dashboard("live:\n  streams: [terminal]\n  control: owner\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, _, record) = publish_live(on.path(), data.path()).await;
        assert_eq!(r1(&calls)["live_control"], serde_json::json!("owner"));
        assert!(record.live_approved.unwrap().control);

        let off = live_dashboard("live:\n  streams: [terminal]\n");
        let data = tempfile::tempdir().unwrap();
        let (calls, _, record) = publish_live(off.path(), data.path()).await;
        let body = r1(&calls);
        assert!(
            body.as_object().unwrap().contains_key("live_control"),
            "{body}"
        );
        assert_eq!(body["live_control"], serde_json::Value::Null);
        assert!(!record.live_approved.unwrap().control);
    }

    /// SW-DP-4b: on a published dashboard, R1 alone carries the saved block,
    /// and only while the manifest's block is the saved one.
    #[tokio::test]
    async fn r1_carries_opaque_saved_scope_and_omits_unknown_scope() {
        let mut fingerprints = Vec::new();
        for scope in [Some(("acme", "riverside")), Some(("acme", "family")), Some(("acme", "riverside")), Some(("other", "riverside")), None] {
            let folder = live_dashboard("live: {queries: [runs]}\n");
            manifest::write_publish(folder.path(), PublishState::Published, None).unwrap();
            let data = tempfile::tempdir().unwrap();
            let fake = Fake::start(healthy).await;
            let mut rec = published(None);
            rec.folder = Some(folder.path().display().to_string());
            let m = manifest::load(folder.path()).unwrap();
            rec.live_approved = super::super::approval::decide(&m, folder.path(), None, scope).approved;
            let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), Instant::now()).await;
            p.update_live(manifest::Visibility::Private).await.unwrap();
            let body = fake.calls()[0].json();
            if scope.is_some() {
                let fingerprint = body["live_scope"].as_str().expect("R1 must carry known scope").to_string();
                assert_eq!(fingerprint.len(), 64);
                assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
                fingerprints.push(fingerprint);
            } else {
                assert!(body.get("live_scope").is_none());
            }
        }
        assert_ne!(fingerprints[0], fingerprints[1]);
        assert_eq!(fingerprints[0], fingerprints[2]);
        assert_ne!(fingerprints[0], fingerprints[3]);
    }

    #[tokio::test]
    async fn r1_carries_saved_argument_pins() {
        for (block, viewers) in [
            ("live: {queries: [storyChanges], args: {storyChanges: {storyId: [SV-CDE-46]}}}\n", serde_json::json!({"storyChanges":true})),
            ("live: {queries: [storyChanges, whyLine], args: {storyChanges: {storyId: [{epic: SV-CDE}]}, whyLine: {repo: [acme/api]}}}\n", serde_json::json!({"storyChanges":true,"whyLine":true})),
        ] {
            let folder = live_dashboard(block);
            manifest::write_publish(folder.path(), PublishState::Published, None).unwrap();
            let data = tempfile::tempdir().unwrap();
            let fake = Fake::start(healthy).await;
            let mut rec = published(None);
            rec.folder = Some(folder.path().display().to_string());
            let m = manifest::load(folder.path()).unwrap();
            rec.live_approved = super::super::approval::decide(&m, folder.path(), None, Some(("acme", "riverside"))).approved;
            let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), Instant::now()).await;
            p.update_live(manifest::Visibility::Private).await.unwrap();
            let body = fake.calls()[0].json();
            assert_eq!(body["live_args"], serde_json::to_value(&m.live_args).unwrap());
            assert_eq!(body["live_viewer_allowed"], viewers);
        }
    }

    #[tokio::test]
    async fn update_live_sends_r1_with_the_approval_of_the_current_block() {
        use super::super::state::LiveApproved;
        let folder = live_dashboard("live:\n  queries: all\n");
        manifest::write_publish(folder.path(), PublishState::Published, None).unwrap();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = published(None);
        rec.folder = Some(folder.path().display().to_string());
        rec.live_approved = Some(LiveApproved {
            scope: None,
            sha: manifest::LiveQueries::All.sha(),
            queries: manifest::LiveQueries::All,
            folder: folder.path().display().to_string(),
            fields: Default::default(),
            args: Default::default(),
            streams: Vec::new(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            approved_at: "2026-10-17T10:00:00Z".into(),
            confirmed: true,
        });
        let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), Instant::now()).await;
        assert_eq!(p.update_live(manifest::Visibility::Private).await.unwrap(), Hosted::Live);
        assert_eq!(fake.routes(), vec!["PUT /api/dash/v1/d/ai-usage"]);
        assert_eq!(r1_live_queries(&fake.calls()), serde_json::json!("all"));
        assert_eq!(fake.calls()[0].json()["title"], "AI usage");
        assert!(fake.calls()[0].json().get("live_args").is_none());
        let viewer = &fake.calls()[0].json()["live_viewer_allowed"];
        assert_eq!(viewer["storyChanges"], false);
        assert_eq!(viewer["whyLine"], false);
        assert_eq!(viewer["runs"], true);

        // The block edited, not yet saved on the record: R1 sends null (fail closed).
        let text = fs::read_to_string(folder.path().join(MANIFEST_FILE)).unwrap();
        fs::write(folder.path().join(MANIFEST_FILE), text.replace("queries: all", "queries: []")).unwrap();
        p.update_live(manifest::Visibility::Private).await.unwrap();
        assert_eq!(r1_live_queries(&fake.calls()[1..]), serde_json::Value::Null);
    }

    #[tokio::test]
    async fn update_live_keeps_the_refusal_rule() {
        let folder = live_dashboard("live:\n  queries: all\n");
        manifest::write_publish(folder.path(), PublishState::Published, None).unwrap();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|_| Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())).await;
        let mut p = publisher(data.path(), folder.path(), &fake.base, published(None), token_store(TOKEN), Instant::now()).await;
        assert_eq!(p.update_live(manifest::Visibility::Private).await.unwrap(), Hosted::AuthFailed);
        let saved = state::load(data.path(), "ai-usage").unwrap();
        assert_eq!(saved.refused_token, Some(fingerprint(Some(&Secret::new(TOKEN)))));
        // The refused token is not sent again.
        assert_eq!(p.update_live(manifest::Visibility::Private).await.unwrap(), Hosted::AuthFailed);
        assert_eq!(fake.calls().len(), 1);
    }

    /// SW-DP-4 review: the saved block holds however the folder is spelled. It
    /// is saved once through a symlink, then checked through a trailing slash
    /// and through the plain path: unchanged, and R1 always sends it.
    #[tokio::test]
    async fn the_saved_block_holds_across_a_trailing_slash_and_a_symlink() {
        use super::super::approval::{decide, LiveResult};
        let folder = live_dashboard("live:\n  queries: all\n");
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("dash-link");
        std::os::unix::fs::symlink(folder.path(), &link).unwrap();
        let slash = PathBuf::from(format!("{}/", folder.path().display()));
        let data = tempfile::tempdir().unwrap();

        let (calls, decision, record) = publish_live(&link, data.path()).await;
        assert_eq!(decision.result, LiveResult::On);
        assert_eq!(r1_live_queries(&calls), serde_json::json!("all"));
        let canonical = fs::canonicalize(folder.path()).unwrap().display().to_string();
        assert_eq!(record.folder.as_deref(), Some(canonical.as_str()));
        assert_eq!(record.live_approved.as_ref().unwrap().folder, canonical);

        // A re-publish through the trailing slash: unchanged, and the record's
        // folder and block stay in step.
        let m = manifest::load(&slash).unwrap();
        let again = decide(&m, &slash, record.live_approved.as_ref(), None);
        assert_eq!(again.result, LiveResult::Unchanged);
        let saved = state::set_live_approved(data.path(), "ai-usage", &slash, again.approved).unwrap();
        assert_eq!(saved.folder.as_deref(), Some(canonical.as_str()));

        // R1 from the plain path still carries the block.
        let fake = Fake::start(healthy).await;
        let mut p = publisher(data.path(), folder.path(), &fake.base, saved, token_store(TOKEN), Instant::now()).await;
        assert_eq!(p.update_live(manifest::Visibility::Private).await.unwrap(), Hosted::Live);
        assert_eq!(r1_live_queries(&fake.calls()), serde_json::json!("all"));

        // A record that still holds an old, uncanonical spelling matches too.
        let mut old = state::load(data.path(), "ai-usage").unwrap();
        old.folder = Some(format!("{}/", link.display()));
        let m = manifest::load(folder.path()).unwrap();
        assert!(super::super::approval::approved_for(&old, &m).is_some());
    }

    /// SW-DP-4 review: a re-publish's visibility reaches dashboard.yml only
    /// once R1 accepted it. A failed R1 leaves the manifest as it was, so the
    /// retry sends R1 again with the same request.
    #[tokio::test]
    async fn a_republish_writes_visibility_only_after_r1_succeeds() {
        let folder = live_dashboard("live:\n  queries: all\n");
        manifest::write_publish(folder.path(), PublishState::Published, None).unwrap();
        let data = tempfile::tempdir().unwrap();

        let down = Fake::start(|_| Reply::Json(500, r#"{"error":"server_error"}"#.into())).await;
        let mut p = publisher(data.path(), folder.path(), &down.base, published(None), token_store(TOKEN), Instant::now()).await;
        assert!(p.update_live(manifest::Visibility::Team).await.is_err());
        assert_eq!(down.calls()[0].json()["visibility"], "team");
        assert_eq!(manifest::load(folder.path()).unwrap().visibility, manifest::Visibility::Private);

        let up = Fake::start(healthy).await;
        let mut p = publisher(data.path(), folder.path(), &up.base, published(None), token_store(TOKEN), Instant::now()).await;
        assert_eq!(p.update_live(manifest::Visibility::Team).await.unwrap(), Hosted::Live);
        assert_eq!(up.calls()[0].json()["visibility"], "team");
        let m = manifest::load(folder.path()).unwrap();
        assert_eq!(m.visibility, manifest::Visibility::Team);
        assert_eq!(m.state, PublishState::Published);
    }

    /// SV-PBF-286 H2: a dashboard that streams terminals is never sent wider
    /// than a person confirmed, even when dashboard.yml says `team`; the
    /// watcher's R1 never writes dashboard.yml.
    #[tokio::test]
    async fn sends_keep_the_confirmed_audience_of_a_streaming_dashboard() {
        let folder = live_dashboard("live:\n  streams: [terminal]\n");
        manifest::write_publish(folder.path(), PublishState::Published, Some(manifest::Visibility::Team)).unwrap();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = published(None);
        rec.folder = Some(folder.path().display().to_string());
        let m = manifest::load(folder.path()).unwrap();
        rec.live_approved = super::super::approval::decide(&m, folder.path(), None, None).approved;
        rec.live_visibility = Some("private".into());
        let mut p = publisher(data.path(), folder.path(), &fake.base, rec, token_store(TOKEN), Instant::now()).await;
        p.resend_live().await.unwrap();
        assert_eq!(fake.calls()[0].json()["visibility"], "private");
        assert_eq!(manifest::load(folder.path()).unwrap().visibility, manifest::Visibility::Team);
    }
}

#[cfg(test)]
mod slug_tests {
    use super::super::client::fake::{Fake, Reply};
    use super::*;
    #[tokio::test]
    async fn explicit_update_checks_the_server_even_when_the_local_source_sha_is_unchanged() {
        for unchanged in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let folder = tests::dashboard();
            let manifest = manifest::load(folder.path()).unwrap();
            let payload = super::super::payload::build(&manifest, super::super::payload::Mode::Published, &super::super::payload::FolderReader { folder: folder.path() }).unwrap().payload;
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let calls = requests.clone();
            let fake = Fake::start(move |seen| {
                if seen.method == "POST" && seen.target.ends_with("/versions") {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        assert_eq!(seen.json()["expected_current"], 1);
                        Reply::Json(409, r#"{"error":"version_conflict"}"#.into())
                    } else {
                        assert_eq!(seen.json()["expected_current"], 9);
                        Reply::Json(200, if unchanged { r#"{"version":9,"unchanged":true,"checked_at":1792141500}"# } else { r#"{"version":10}"# }.into())
                    }
                } else if seen.method == "GET" { Reply::Json(200, r#"{"current_version":9}"#.into()) }
                else { tests::healthy(seen) }
            }).await;
            let mut record = PublishRecord::new("ai-usage");
            record.state = LocalState::Published;
            record.sync_status = SyncStatus::Ok;
            record.current_version = Some(1);
            record.last_sha = Some(payload.manifest_sha.clone());
            state::save(data.path(), &record).unwrap();
            let mut publisher = Publisher::new(data.path(), folder.path(), Client::new(&fake.base).unwrap(), tests::token_store(tests::TOKEN), Instant::now()).unwrap();
            assert_eq!(publisher.update_now(manifest::Visibility::Private).await.unwrap(), Hosted::Live);
            assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert_eq!(publisher.record().current_version, Some(if unchanged { 9 } else { 10 }));
            assert_eq!(publisher.record().last_sha.as_deref(), Some(payload.manifest_sha.as_str()));
            assert_eq!(fake.routes().iter().any(|route| route.ends_with("/versions/10/commit")), !unchanged);
        }
    }
    #[tokio::test]
    async fn explicit_update_pushes_the_current_folder_and_reports_a_failed_commit() {
        for fails in [false, true] {
            let data = tempfile::tempdir().unwrap();
            let folder = tests::dashboard();
            let fake = Fake::start(move |seen| {
                if seen.target.ends_with("/commit") && fails { Reply::Json(503, r#"{"error":"server_error"}"#.into()) }
                else if seen.method == "GET" { Reply::Json(200, r#"{"current_version":1}"#.into()) }
                else { tests::healthy(seen) }
            }).await;
            let mut record = PublishRecord::new("ai-usage");
            record.state = LocalState::Published; record.current_version = Some(1); record.sync_status = SyncStatus::Ok;
            state::save(data.path(), &record).unwrap();
            let mut publisher = Publisher::new(data.path(), folder.path(), Client::new(&fake.base).unwrap(), tests::token_store(tests::TOKEN), Instant::now()).unwrap();
            let result = publisher.update_now(manifest::Visibility::Private).await.unwrap();
            assert_eq!(result == Hosted::Live, !fails);
            assert!(fake.routes().iter().any(|r| r.ends_with("/versions/2/commit")));
            assert_eq!(state::load(data.path(), "ai-usage").unwrap().current_version, Some(if fails { 1 } else { 2 }));
        }
    }
    #[tokio::test]
    async fn hosted_only_takedown_requires_server_confirmation() {
        let data = tempfile::tempdir().unwrap();
        let ok = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        take_down_slug_with(
            &Client::new(&ok.base).unwrap(),
            tests::token_store(tests::TOKEN),
            data.path(),
            "hosted-only",
        )
        .await
        .unwrap();
        assert_eq!(
            ok.routes(),
            vec!["POST /api/dash/v1/d/hosted-only/unpublish"]
        );
        let failure = Fake::start(|_| Reply::Drop).await;
        assert!(take_down_slug_with(
            &Client::new(&failure.base).unwrap(),
            tests::token_store(tests::TOKEN),
            data.path(),
            "hosted-only"
        )
        .await
        .is_err());
    }
}
