// SPDX-License-Identifier: MIT
//! The watcher loop (SW-PB-22f-3). Each pass looks at every dashboard that is
//! publishing or published. A listed file whose length or modified time has
//! changed is noted with its publisher, which arms the K2 debounce. Then the
//! publisher runs its due work. The loop reads the listed files' metadata only;
//! file content is read by the payload build, as before.
//!
//! A pending takedown (R6 not yet confirmed) is retried once a minute.
//!
//! An edited `live` block (SW-DP-19, DP-D39) is saved on the record at once,
//! so answers follow it, and R1 carries the new list to the hosted row. An R1
//! that failed on the network or a 5xx (or found no token) is retried once a
//! minute; one the server refused, or that the manifest no longer allows, is
//! dropped. Each failure reason is logged once, not on every retry.
//!
//! The loop, the publish tool and the unpublish tool change the same record,
//! so each takes the dashboard's turn lock (`slug_lock`) before it touches it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use super::client::{CallError, Client, DASH_HOST, TOKEN_SERVICE};
use super::manifest;
use super::push::{DriveError, Hosted, Publisher};
use super::state::{self, LocalState, PublishRecord};
use crate::commands::credential_store::{default_store, CredentialStore};

/// How long to wait before retrying a takedown the server has not confirmed.
const TAKEDOWN_RETRY: Duration = Duration::from_secs(60);

/// How long to wait before retrying an R1 for an edited live block.
const LIVE_RETRY: Duration = Duration::from_secs(60);

/// A listed file's length and modified time.
type Stamp = (u64, Option<SystemTime>);

/// One dashboard's turn lock. Whoever holds it may change that dashboard's record.
pub fn slug_lock(slug: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(Default::default);
    let mut locks = locks.lock().unwrap_or_else(|e| e.into_inner());
    locks.entry(slug.into()).or_default().clone()
}

struct Entry {
    publisher: Publisher,
    stamps: HashMap<String, Stamp>,
}

/// Keeps every live dashboard's hosted copy in step with its folder.
pub struct Driver {
    app: Option<tauri::AppHandle>,
    data_dir: PathBuf,
    /// The dashboard host. `None` in a build without one: nothing is sent.
    base: Option<String>,
    store: Arc<dyn CredentialStore>,
    entries: HashMap<String, Entry>,
    /// When a pending takedown may next be retried, per dashboard.
    takedown_retry_at: HashMap<String, Instant>,
    /// When R1 for an edited live block may next be retried, per dashboard.
    live_retry_at: HashMap<String, Instant>,
    /// The last R1 failure logged per dashboard, so a retry does not log it again.
    live_logged: HashMap<String, String>,
}

impl Driver {
    /// The app's driver: this build's dashboard host and the Keychain token.
    pub fn open(data_dir: &Path) -> Driver {
        let store: Arc<dyn CredentialStore> = Arc::from(default_store(TOKEN_SERVICE));
        Driver::new(data_dir, DASH_HOST, store)
    }

    pub fn with_app(mut self, app: tauri::AppHandle) -> Self {
        self.app = Some(app);
        self
    }

    pub fn new(data_dir: &Path, base: Option<&str>, store: Arc<dyn CredentialStore>) -> Driver {
        Driver {
            app: None,
            data_dir: data_dir.into(),
            base: base.map(String::from),
            store,
            entries: HashMap::new(),
            takedown_retry_at: HashMap::new(),
            live_retry_at: HashMap::new(),
            live_logged: HashMap::new(),
        }
    }

    /// One pass over every saved dashboard. A failure is logged for that
    /// dashboard only; the others still run.
    pub async fn tick(&mut self, now: Instant) {
        let Some(base) = self.base.clone() else {
            return;
        };
        let slugs = match state::list(&self.data_dir) {
            Ok(slugs) => slugs,
            Err(e) => {
                log::warn!(target: "dashboard_publish", "watcher: cannot list dashboards: {e}");
                return;
            }
        };
        for slug in slugs {
            if let Err(e) = self.tick_one(&base, &slug, now).await {
                log::warn!(target: "dashboard_publish", "watcher for {slug}: {}", e.reason());
            }
        }
    }

    async fn tick_one(&mut self, base: &str, slug: &str, now: Instant) -> Result<(), DriveError> {
        let lock = slug_lock(slug);
        let _turn = lock.lock().await;

        let record = state::load(&self.data_dir, slug).map_err(DriveError::Storage)?;
        if record.takedown_pending() {
            self.forget(slug);
            return self.retry_takedown(base, slug, &record, now).await;
        }
        let folder = match live_folder(&record) {
            Some(folder) => folder,
            None => {
                self.forget(slug);
                return Ok(());
            }
        };
        // A record changed on disk by a publish or takedown: rebuild the publisher
        // from it, so the next push works from what is saved.
        if self
            .entries
            .get(slug)
            .is_some_and(|e| e.publisher.record() != &record)
        {
            self.entries.remove(slug);
        }
        if !self.entries.contains_key(slug) {
            let client = Client::new(base).map_err(DriveError::Storage)?;
            let publisher = Publisher::from_record(
                &self.data_dir,
                &folder,
                record.clone(),
                client,
                self.store.clone(),
                now,
            );
            self.entries.insert(
                slug.into(),
                Entry {
                    publisher,
                    stamps: HashMap::new(),
                },
            );
        }
        let entry = self.entries.get_mut(slug).expect("inserted above");

        // A manifest that does not validate is reported by the publisher's own
        // poll when a push is due, not on every 5 s pass.
        if let Ok(manifest) = manifest::load(&folder) {
            if super::approval::follow(entry.publisher.record(), &manifest).is_some()
                && manifest
                    .actions
                    .values()
                    .any(|a| a.agent.is_some() || a.routine.is_some())
            {
                let app = self.app.as_ref().ok_or_else(|| {
                    DriveError::Storage(
                        "manifest_invalid: action reference reader unavailable".into(),
                    )
                })?;
                let scope = entry
                    .publisher
                    .record()
                    .live_approved
                    .as_ref()
                    .and_then(|a| a.scope.as_ref());
                super::actions::validate_references(app, &manifest.actions, scope)
                    .await
                    .map_err(|e| DriveError::Storage(format!("manifest_invalid: {e}")))?;
            }
            // SW-DP-19: an edited live block takes effect without a publish.
            if entry.publisher.follow_live(&manifest)? {
                self.live_retry_at.remove(slug);
                self.live_logged.remove(slug);
                if let Some(app) = &self.app {
                    super::confirm::announce(app, entry.publisher.record(), false);
                }
            }
            if entry.publisher.record().live_pending
                && self.live_retry_at.get(slug).is_none_or(|at| *at <= now)
            {
                // The refusal rule and a 401 are handled by resend_live. It
                // sends the manifest's title and visibility and writes nothing.
                let outcome = entry.publisher.resend_live().await;
                let failure = match &outcome {
                    Ok(Hosted::Live) => None,
                    Ok(_) => Some("the publisher token was refused; save a new one".to_string()),
                    Err(e) => Some(e.reason()),
                };
                match failure {
                    None => {
                        self.live_retry_at.remove(slug);
                        self.live_logged.remove(slug);
                    }
                    Some(reason) => {
                        let gave_up = matches!(&outcome, Err(e) if !worth_retrying(e));
                        if self.live_logged.get(slug) != Some(&reason) {
                            let then = if gave_up {
                                "not retried"
                            } else {
                                "retried in a minute"
                            };
                            log::warn!(target: "dashboard_publish", "watcher: R1 for the live block of {slug}: {reason} ({then})");
                            self.live_logged.insert(slug.into(), reason);
                        }
                        if gave_up {
                            entry.publisher.drop_live_pending()?;
                            self.live_retry_at.remove(slug);
                        } else {
                            self.live_retry_at.insert(slug.into(), now + LIVE_RETRY);
                        }
                    }
                }
            }
            let stamps = stamps_of(&folder, &manifest.files);
            // A publishing record is pushed at once by its publisher. A changed
            // file only matters once the dashboard is live.
            if record.state == LocalState::Published {
                for (rel, stamp) in &stamps {
                    if entry.stamps.get(rel) != Some(stamp) {
                        entry.publisher.note_change(now, rel, &manifest.files);
                    }
                }
            }
            entry.stamps = stamps;
        }
        entry.publisher.drive(now).await
    }

    /// Drop what the loop holds for a dashboard it no longer watches.
    fn forget(&mut self, slug: &str) {
        self.entries.remove(slug);
        self.live_retry_at.remove(slug);
        self.live_logged.remove(slug);
    }

    /// Retry R6 for a takedown the server has not confirmed, at most once a minute.
    async fn retry_takedown(
        &mut self,
        base: &str,
        slug: &str,
        record: &PublishRecord,
        now: Instant,
    ) -> Result<(), DriveError> {
        if self.takedown_retry_at.get(slug).is_some_and(|at| *at > now) {
            return Ok(());
        }
        self.takedown_retry_at
            .insert(slug.into(), now + TAKEDOWN_RETRY);
        let Some(folder) = record.folder.as_deref().map(PathBuf::from) else {
            // A record saved before the folder was kept: the loop cannot find
            // the dashboard. Running unpublish_dashboard on it again retries.
            log::warn!(target: "dashboard_publish", "watcher: takedown for {slug} is owed but its folder is unknown");
            return Ok(());
        };
        let client = Client::new(base).map_err(DriveError::Storage)?;
        let mut publisher = Publisher::from_record(
            &self.data_dir,
            &folder,
            record.clone(),
            client,
            self.store.clone(),
            now,
        );
        publisher.unpublish().await?;
        self.takedown_retry_at.remove(slug);
        Ok(())
    }
}

/// True when an R1 that failed with `e` may succeed if sent again: no answer, a
/// 5xx, 408 or 429, or a token or file that could not be read. A refusal
/// (409 slug_taken, 404, other 4xx), an unreadable reply, or a manifest that
/// is no longer published under the slug is not retried.
fn worth_retrying(e: &DriveError) -> bool {
    match e {
        DriveError::Update(CallError::NoResponse) => true,
        DriveError::Update(CallError::Status { status, .. }) => {
            *status >= 500 || *status == 408 || *status == 429
        }
        DriveError::Update(CallError::BadReply) | DriveError::BadRequest(_) => false,
        _ => true,
    }
}

/// The folder of a dashboard that is publishing or published, if it has one.
fn live_folder(record: &PublishRecord) -> Option<PathBuf> {
    match record.state {
        LocalState::Publishing | LocalState::Published => {
            record.folder.as_deref().map(PathBuf::from)
        }
        LocalState::Draft | LocalState::Unpublished => None,
    }
}

/// The length and modified time of each listed file that can be read.
fn stamps_of(folder: &Path, files: &[String]) -> HashMap<String, Stamp> {
    files
        .iter()
        .filter_map(|rel| {
            let meta = std::fs::metadata(folder.join(rel)).ok()?;
            Some((rel.clone(), (meta.len(), meta.modified().ok())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::client::fake::Fake;
    use super::super::push::tests::{dashboard, healthy, secs, token_store, TOKEN};
    use super::super::state::Event;
    use super::*;
    use std::fs;

    /// A published dashboard whose record names its folder.
    fn saved_published(data: &Path, folder: &Path) {
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        rec.apply(Event::FirstCommitOk).unwrap();
        rec.folder = Some(folder.display().to_string());
        state::save(data, &rec).unwrap();
    }

    #[tokio::test]
    async fn a_changed_listed_file_is_pushed_after_the_debounce() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        saved_published(data.path(), folder.path());
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));

        let t0 = Instant::now();
        // The first look sees every listed file as new, so the debounce arms and nothing is sent yet.
        driver.tick(t0).await;
        assert!(fake.calls().is_empty(), "pushed before the debounce");

        // The debounce is due: the dashboard goes up.
        driver.tick(t0 + secs(61)).await;
        let pushed = fake.calls().len();
        assert!(pushed > 0, "the change was not pushed");

        // A second edit is picked up on the next look, and pushed after its own debounce.
        fs::write(folder.path().join("index.html"), "<h1>changed</h1>").unwrap();
        driver.tick(t0 + secs(120)).await;
        assert_eq!(fake.calls().len(), pushed, "pushed before the second debounce");
        driver.tick(t0 + secs(181)).await;
        assert!(fake.calls().len() > pushed, "the second change was not pushed");
    }

    #[tokio::test]
    async fn a_draft_dashboard_is_never_touched() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.folder = Some(folder.path().display().to_string());
        state::save(data.path(), &rec).unwrap();
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));
        let t0 = Instant::now();
        driver.tick(t0).await;
        driver.tick(t0 + secs(600)).await;
        assert!(fake.calls().is_empty(), "a draft dashboard sent a request");
    }

    #[tokio::test]
    async fn a_build_with_no_host_sends_nothing() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        saved_published(data.path(), folder.path());
        let mut driver = Driver::new(data.path(), None, token_store(TOKEN));
        driver.tick(Instant::now()).await;
        assert!(driver.entries.is_empty());
    }

    #[tokio::test]
    async fn an_unconfirmed_takedown_is_retried_once_a_minute() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(healthy).await;
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        rec.apply(Event::FirstCommitOk).unwrap();
        rec.apply(Event::Unpublish).unwrap();
        rec.apply(Event::TakedownOwed).unwrap();
        rec.folder = Some(folder.path().display().to_string());
        state::save(data.path(), &rec).unwrap();
        let mut driver = Driver::new(data.path(), Some(&fake.base), token_store(TOKEN));

        let t0 = Instant::now();
        driver.tick(t0).await;
        let retried = fake.calls().len();
        assert!(retried > 0, "the owed takedown was not retried");
        assert!(!state::load(data.path(), "ai-usage").unwrap().takedown_pending());

        // Once the server has confirmed it, the loop stops asking.
        driver.tick(t0 + secs(61)).await;
        assert_eq!(fake.calls().len(), retried, "a confirmed takedown was sent again");
    }

    /// SW-DP-19 review: which failed R1s for a pending live block are retried.
    #[test]
    fn only_an_r1_that_may_succeed_later_is_retried() {
        let status = |status| {
            DriveError::Update(CallError::Status {
                status,
                code: "x".into(),
            })
        };
        for retried in [
            DriveError::Update(CallError::NoResponse),
            status(500),
            status(503),
            status(408),
            status(429),
            DriveError::Keychain("locked".into()),
            DriveError::Storage("busy".into()),
        ] {
            assert!(worth_retrying(&retried), "{retried:?} was not retried");
        }
        for dropped in [
            status(404),
            status(409),
            status(400),
            DriveError::Update(CallError::BadReply),
            DriveError::BadRequest("draft".into()),
        ] {
            assert!(!worth_retrying(&dropped), "{dropped:?} was retried");
        }
    }
}
