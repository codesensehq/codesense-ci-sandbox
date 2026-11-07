// SPDX-License-Identifier: MIT
//! SV-PBF-278: the beta channel with no license. The app reads one static manifest,
//! `<base>/v1/update/{target}/{arch}/latest.json`, at launch, about daily and once on a wake,
//! and hands a newer version to the updater (`updates.rs`), which downloads, verifies against
//! the channel's key and stages it as before. The request carries no token, install id, device
//! clock or query. A staged build installs only while the latest manifest still names it.
//!
//! Nothing here gates the app: `view` is a fixed "no license" view that only carries the
//! update fields. The license records an older build stored are removed once, locally.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Runtime, Url};
use tokio::time::{Instant, MissedTickBehavior};

use super::fallback::{fallback_file, FALLBACK_FILE};
use super::keychain;
use super::scheduler;
use super::updates::{
    endpoint, is_newer, lock, parse_version, Offer, Outcome, Updates, CHECK_TIMEOUT,
};
use super::view::{UpdateStatus, View, CHANNEL};
use crate::commands::credential_store::CredentialStore;

/// The largest manifest the app reads. The real one is a few hundred bytes.
pub const MANIFEST_MAX_BYTES: usize = 64 * 1024;

/// The Keychain services the license used, in both beta channels.
pub const LICENSE_SERVICES: [&str; 2] = ["ai.codesense.beta", "ai.codesense.beta-staging"];

/// What the static manifest says. Only the version and the notes are read here; the updater
/// reads the rest (`url`, `signature`) through the plugin.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
}

/// The updater's endpoint for this Mac, with the target and arch filled in the way the plugin
/// fills them, so the tick and the plugin ask the same file.
pub fn manifest_url(base: &str) -> Result<Url, String> {
    let (target, arch) = target_arch();
    let template = endpoint(base)?;
    let url = str::replace(Url::as_str(&template), "%7B%7Btarget%7D%7D", target);
    str::replace(&url, "%7B%7Barch%7D%7D", arch)
        .parse()
        .map_err(|e| format!("update manifest: {e}"))
}

/// The plugin's `{{target}}` and `{{arch}}` for this Mac.
fn target_arch() -> (&'static str, &'static str) {
    let target = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        "arm" => "armv7",
        arch => arch,
    };
    (target, arch)
}

/// The channel's updates: the manifest, what it last named, and the updater it feeds.
pub struct StaticChannel {
    manifest: Url,
    running: String,
    updater: Arc<Updates>,
    /// The version the last manifest read named. A staged build installs only while it is this.
    latest: Mutex<Option<String>>,
    /// This build gets no updates: it was built without its channel's updater config. No entry
    /// point (the tick, Check now) then makes any request.
    updates_off: bool,
    /// A check is running. Another one (Check now during a tick) is skipped, so two reads can't
    /// land out of order in `latest`.
    checking: AtomicBool,
    emit: Box<dyn Fn(&View) + Send + Sync>,
}

/// Clears the in-flight flag however a check ends.
struct CheckGuard<'a>(&'a AtomicBool);

impl Drop for CheckGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl StaticChannel {
    /// `base` is the channel's download host, for example `https://dl.codesense.ai`. The updater
    /// is gated on the manifest from here on.
    pub fn new(
        base: &str,
        running: String,
        updater: Arc<Updates>,
        updates_off: bool,
        emit: Box<dyn Fn(&View) + Send + Sync>,
    ) -> Result<Arc<Self>, String> {
        let channel = Arc::new(Self {
            manifest: manifest_url(base)?,
            running,
            updater: updater.clone(),
            latest: Mutex::new(None),
            updates_off,
            checking: AtomicBool::new(false),
            emit,
        });
        let asking = Arc::downgrade(&channel);
        updater.set_gate(Box::new(move |version| {
            asking
                .upgrade()
                .is_some_and(|channel| channel.names(version))
        }));
        Ok(channel)
    }

    /// Whether the last manifest read named `version`.
    fn names(&self, version: &str) -> bool {
        lock(&self.latest).as_deref() == Some(version)
    }

    /// A newer build is staged or on offer. The tick then reads the manifest about hourly, so a
    /// build the channel pulls is dropped before the quit that would install it.
    pub fn update_pending(&self) -> bool {
        self.updater.staged_version().is_some()
            || lock(&self.latest)
                .as_deref()
                .is_some_and(|v| is_newer(v, &self.running))
    }

    /// One anonymous GET of the manifest: HTTPS only in a release build, no redirect, no
    /// header of its own, no query, and a size cap.
    async fn fetch_manifest(&self) -> Result<Manifest, &'static str> {
        let client = reqwest_mcp::Client::builder()
            .timeout(CHECK_TIMEOUT)
            .redirect(reqwest_mcp::redirect::Policy::none())
            .https_only(!cfg!(debug_assertions))
            .build()
            .map_err(|_| "the updater failed")?;
        let mut reply = client
            .get(self.manifest.clone())
            .send()
            .await
            .map_err(|_| "the update manifest could not be reached")?;
        if !reqwest_mcp::Response::status(&reply).is_success() {
            return Err("the update host refused the manifest");
        }
        let mut body = Vec::new();
        while let Some(chunk) = reply
            .chunk()
            .await
            .map_err(|_| "the update manifest could not be read")?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MANIFEST_MAX_BYTES {
                return Err("the update manifest is larger than allowed");
            }
        }
        let manifest: Manifest =
            serde_json::from_slice(&body).map_err(|_| "the update manifest is malformed")?;
        if parse_version(&manifest.version).is_none() {
            return Err("the update manifest names no version");
        }
        Ok(manifest)
    }

    /// One check: reads the manifest, drops a staged build it no longer names, and hands a newer
    /// version to the updater (download, verify, stage). Returns whether the manifest was read.
    /// A manifest that can't be read changes nothing; the next tick tries again. With updates
    /// off, or while another check runs, it asks nothing and returns false.
    pub async fn check<R: Runtime>(&self, app: &AppHandle<R>, explicit: bool) -> bool {
        if self.updates_off {
            self.refresh();
            return false;
        }
        if self.checking.swap(true, Ordering::SeqCst) {
            return false;
        }
        let _guard = CheckGuard(&self.checking);
        let manifest = match self.fetch_manifest().await {
            Ok(manifest) => manifest,
            Err(label) => {
                log::warn!(target: "license", "update check failed: {label}");
                return false;
            }
        };
        let version = manifest.version;
        *lock(&self.latest) = Some(version.clone());
        self.updater
            .clear_unless(Some(std::string::String::as_str(&version)));
        if !is_newer(&version, &self.running) {
            self.refresh();
            return true;
        }
        // A build known to fail at a restart asks for an install by hand at once.
        self.updater.note_offer(&version);
        self.refresh();
        let offer = Offer {
            version,
            notes: manifest.notes,
            explicit,
        };
        if !matches!(self.updater.run(app, offer).await, Outcome::Busy) {
            self.refresh();
        }
        true
    }

    /// The view `license_state` returns: the channel and the update fields. Nothing is gated.
    pub fn view(&self) -> View {
        let staged = self.updater.staged_info();
        let restart_ready = staged.is_some() && self.updater.restart_ready(false).is_ok();
        View {
            channel: CHANNEL,
            update: staged.map(|s| UpdateStatus {
                staged_version: s.version,
                notes: s.notes,
            }),
            update_attention: self.updater.attention(),
            updates_off: self.updates_off,
            restart_ready,
        }
    }

    /// Tells the UI the current view (`license://state`).
    pub fn refresh(&self) {
        (self.emit)(&self.view());
    }
}

/// One check, and the wall clock the tick loop measures a day and a wake by.
#[async_trait::async_trait]
pub trait UpdateCheck: Send + Sync {
    /// Runs one check. Returns true when the manifest was read.
    async fn check(&self) -> bool;
    /// The device clock, in UTC seconds.
    fn now(&self) -> i64;
    /// A newer build is staged or on offer.
    fn update_pending(&self) -> bool;
}

/// The update tick, on the old license scheduler's timing. Runs forever:
/// - a check at launch;
/// - on each tick (hourly, or every `max_age` when that is shorter), a check once `max_age` has
///   passed since the last check that read the manifest, or `PENDING_MAX_AGE` while an update is
///   pending, so a failed check is retried at the tick's cadence, never faster;
/// - one check on every wake from sleep, due or not.
///
/// `max_age` is a day, except in a staging build that sets `CODESENSE_CHECK_INTERVAL_SECS`.
pub async fn run_ticks(checker: Arc<dyn UpdateCheck>, max_age: Duration) {
    let tick = scheduler::tick_for(max_age);
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut probe = tokio::time::interval(scheduler::WAKE_PROBE.min(tick));
    probe.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_probe = (Instant::now(), checker.now());
    let mut last_read: Option<i64> = None;
    let mut launched = false;
    loop {
        let woke = tokio::select! {
            _ = tokio::time::Interval::tick(&mut ticker) => false,
            _ = tokio::time::Interval::tick(&mut probe) => {
                let (mono, wall) = (Instant::now(), checker.now());
                let mono_secs = i64::try_from(mono.duration_since(last_probe.0).as_secs())
                    .unwrap_or(i64::MAX);
                let gap = wall.saturating_sub(last_probe.1).saturating_sub(mono_secs);
                last_probe = (mono, wall);
                if gap < scheduler::SLEEP_GAP_SECS {
                    continue;
                }
                log::info!(target: "license", "woke from sleep; checking for an update");
                true
            }
        };
        let age = scheduler::effective_max_age(max_age, checker.update_pending());
        let due = !launched || woke || scheduler::check_due(last_read, checker.now(), age, false);
        launched = true;
        if due && checker.check().await {
            last_read = Some(checker.now());
        }
    }
}

/// The ticks' check: the channel's, through the app.
pub struct AppCheck<R: Runtime> {
    pub app: AppHandle<R>,
    pub channel: Arc<StaticChannel>,
}

#[async_trait::async_trait]
impl<R: Runtime> UpdateCheck for AppCheck<R> {
    async fn check(&self) -> bool {
        self.channel.check(&self.app, false).await
    }

    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn update_pending(&self) -> bool {
        self.channel.update_pending()
    }
}

/// In the update records folder: written once nothing is left to do, so later launches touch the
/// Keychain no more.
pub const CLEANUP_DONE_FILE: &str = "license-records-removed";
/// In the update records folder: how many launches the Keychain refused a delete.
pub const CLEANUP_REFUSALS_FILE: &str = "license-records-refusals";
/// The launches a refused Keychain delete is tried on. After that the records stay, unused.
pub const MAX_KEYCHAIN_REFUSALS: u32 = 3;

/// Removes the license records an older build stored: each service's accounts, and the fallback
/// files in `app_dir`. Local only: no network call. Idempotent. A Keychain that refuses a delete
/// (locked, or the person said no) is asked once per launch at most, on at most
/// `MAX_KEYCHAIN_REFUSALS` launches; then the cleanup stops and says so once. Its own records
/// live in `records_dir` (the updater's `updates/`). When that folder can't be written, the cap
/// can't be counted, so the Keychain isn't asked at all. Returns whether nothing is left to do.
pub fn remove_license_records(
    stores: &[(&str, &dyn CredentialStore)],
    app_dir: &Path,
    records_dir: &Path,
) -> bool {
    let done_marker = records_dir.join(CLEANUP_DONE_FILE);
    if std::path::Path::exists(&done_marker) {
        return true;
    }
    let refusals_file = records_dir.join(CLEANUP_REFUSALS_FILE);
    let mut refusals = std::result::Result::ok(std::fs::read_to_string(&refusals_file))
        .and_then(|text| std::result::Result::ok(text.trim().parse::<u32>()))
        .unwrap_or(0);
    let mut refused = false;
    // The count must be writable before the Keychain is asked, or a refusal could be asked again
    // at every launch with nothing to stop it.
    let countable = std::fs::create_dir_all(records_dir)
        .and_then(|()| std::fs::write(&refusals_file, refusals.to_string()))
        .map_err(|e| log::warn!(target: "license", "the update records can't be written ({e}); the old license records are left in the Keychain this launch"))
        .is_ok();
    // Past the cap the Keychain is not asked again, even while a file is still left.
    let stores_to_ask: &[(&str, &dyn CredentialStore)] =
        if countable && refusals < MAX_KEYCHAIN_REFUSALS {
            stores
        } else {
            &[]
        };
    'keychain: for (service, store) in stores_to_ask {
        // Every account the license kept in each service.
        let accounts = keychain::ALL_RECORDS
            .into_iter()
            .chain([keychain::DEACTIVATE_INCOMPLETE]);
        for account in accounts {
            if let Err(e) = store.delete(account) {
                log::warn!(target: "license", "could not remove the old license record {account} from {service}: {e}");
                refused = true;
                break 'keychain;
            }
        }
    }
    let mut files_left = false;
    let files = stores
        .iter()
        .map(|(service, _)| fallback_file(service))
        .chain([FALLBACK_FILE.to_string()]);
    for file in files {
        match std::fs::remove_file(app_dir.join(&file)) {
            Ok(()) => {}
            Err(e) if std::io::Error::kind(&e) == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!(target: "license", "could not remove the old license file {file}: {e}");
                files_left = true;
            }
        }
    }
    if !countable {
        return false;
    }
    if refused {
        refusals = refusals.saturating_add(1);
        if let Err(e) = std::fs::write(&refusals_file, refusals.to_string()) {
            log::warn!(target: "license", "could not record the refused Keychain delete: {e}");
        }
        if refusals < MAX_KEYCHAIN_REFUSALS {
            log::info!(target: "license", "the old license records are tried again at the next launch");
            return false;
        }
        log::warn!(target: "license", "the Keychain refused the old license records on {refusals} launches; they are left in place, unused, and not asked for again");
    }
    if files_left {
        return false;
    }
    if let Err(e) = std::fs::write(&done_marker, "") {
        log::warn!(target: "license", "could not record that the old license records are done: {e}");
    }
    true
}

#[cfg(all(test, not(cs_channel = "internal")))]
mod tests {
    //! SV-PBF-278 case table: one test per row, against a loopback manifest server.

    use std::sync::atomic::{AtomicI64, AtomicUsize};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::updates::tests::{
        app, app_with_pubkey, good, mount_file, Fixture, NEXT, RUNNING, SIGNED_PATH,
    };
    use super::super::updates::Attention;
    use super::*;
    use crate::commands::credential_store::{FakeStore, Secret};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const HOUR: u64 = 60 * 60;

    /// The manifest path the app asks on this Mac, as the plugin fills it in.
    fn manifest_path() -> String {
        manifest_url("http://127.0.0.1")
            .expect("the manifest url")
            .path()
            .to_string()
    }

    fn manifest(server: &MockServer, version: &str, sig: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": version,
            "notes": "Manifest notes",
            "pub_date": "2026-10-29T00:00:00Z",
            "url": format!("{}{SIGNED_PATH}", server.uri()),
            "signature": sig.trim(),
        }))
    }

    /// Replaces whatever the server serves with `reply` at the manifest path.
    async fn mount_manifest(server: &MockServer, reply: ResponseTemplate) {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path(manifest_path()))
            .respond_with(reply)
            .mount(server)
            .await;
    }

    async fn serve(server: &MockServer, version: &str, f: &Fixture) {
        mount_manifest(server, manifest(server, version, f.sig)).await;
        mount_file(server, f.bytes.to_vec(), None).await;
    }

    /// A channel on the fixture server, with an updater that installs into a temporary bundle.
    fn channel_with(
        server: &MockServer,
        dir: &tempfile::TempDir,
        updates_off: bool,
    ) -> (Arc<StaticChannel>, Arc<Updates>) {
        let bundle = dir.path().join("Codesense.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let updater = Arc::new(
            Updates::new(&server.uri(), RUNNING.into(), DAY)
                .unwrap()
                .installing_into(&bundle)
                .with_marker_dir(dir.path().join("updates")),
        );
        let channel = StaticChannel::new(
            &server.uri(),
            RUNNING.into(),
            updater.clone(),
            updates_off,
            Box::new(|_| {}),
        )
        .expect("the channel");
        (channel, updater)
    }

    fn channel(server: &MockServer, dir: &tempfile::TempDir) -> (Arc<StaticChannel>, Arc<Updates>) {
        channel_with(server, dir, false)
    }

    /// Every request the app made: none carries a credential, an install id or a query.
    async fn assert_anonymous(server: &MockServer) -> Vec<String> {
        let requests = server.received_requests().await.unwrap();
        for r in &requests {
            for name in ["authorization", "x-install-id", "cookie"] {
                assert!(r.headers.get(name).is_none(), "{} sent {name}", r.url);
            }
            assert_eq!(r.url.query(), None, "{} has a query", r.url);
        }
        requests
            .iter()
            .map(|r| format!("{} {}", r.method, r.url.path()))
            .collect()
    }

    /// The 8 accounts the story names, written out here so a record missing from the
    /// cleanup's list fails rows 2 and 3.
    const ACCOUNTS: [&str; 8] = [
        keychain::INSTALL_TOKEN,
        keychain::INSTALL_ID,
        keychain::LAST_JWS,
        keychain::LAST_SERVER_TIME,
        keychain::LOCK_REASON,
        keychain::PENDING_REVOKE,
        keychain::DEVICE_TIME,
        keychain::DEACTIVATE_INCOMPLETE,
    ];

    fn fallback_files() -> [String; 3] {
        [
            fallback_file(LICENSE_SERVICES[0]),
            fallback_file(LICENSE_SERVICES[1]),
            FALLBACK_FILE.to_string(),
        ]
    }

    /// Both channels' eight accounts, and the three fallback files.
    fn stored_license(dir: &Path) -> [FakeStore; 2] {
        let stores = [FakeStore::default(), FakeStore::default()];
        for store in &stores {
            for account in ACCOUNTS {
                store.set(account, &Secret::new("x")).unwrap();
            }
        }
        for file in fallback_files() {
            std::fs::write(dir.join(file), "{}").unwrap();
        }
        stores
    }

    fn stores_of(stores: &[FakeStore; 2]) -> [(&str, &dyn CredentialStore); 2] {
        [
            (LICENSE_SERVICES[0], &stores[0] as &dyn CredentialStore),
            (LICENSE_SERVICES[1], &stores[1] as &dyn CredentialStore),
        ]
    }

    /// A Keychain that refuses every delete, as a locked one does, and counts the asks.
    #[derive(Default)]
    struct Refusing {
        asks: AtomicUsize,
    }

    impl CredentialStore for Refusing {
        fn get(&self, _: &str) -> Result<Option<Secret>, String> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &Secret) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            self.asks.fetch_add(1, Ordering::SeqCst);
            Err("keychain delete failed: user canceled".into())
        }
    }

    /// Counts checks; the wall clock follows tokio's paused clock plus `slept`.
    struct Ticks {
        calls: AtomicUsize,
        ok: AtomicBool,
        pending: AtomicBool,
        start: tokio::time::Instant,
        slept: AtomicI64,
    }

    #[async_trait::async_trait]
    impl UpdateCheck for Ticks {
        async fn check(&self) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ok.load(Ordering::SeqCst)
        }
        fn now(&self) -> i64 {
            1_760_500_000
                + self.start.elapsed().as_secs() as i64
                + self.slept.load(Ordering::SeqCst)
        }
        fn update_pending(&self) -> bool {
            self.pending.load(Ordering::SeqCst)
        }
    }

    fn ticks() -> Arc<Ticks> {
        Arc::new(Ticks {
            calls: AtomicUsize::new(0),
            ok: AtomicBool::new(true),
            pending: AtomicBool::new(false),
            start: tokio::time::Instant::now(),
            slept: AtomicI64::new(0),
        })
    }

    /// Lets the tick loop's first look run (it fires at once).
    async fn launched(ticks: &Arc<Ticks>, max_age: Duration) -> tokio::task::JoinHandle<()> {
        let handle = tokio::spawn(run_ticks(ticks.clone(), max_age));
        tokio::task::yield_now().await;
        handle
    }

    /// Just past the next wake probe.
    const PAST_PROBE: Duration = Duration::from_secs(scheduler::WAKE_PROBE.as_secs() + 1);

    /// Row 1: a fresh install opens with no gate, and makes one anonymous manifest GET.
    #[tokio::test]
    async fn row_1_a_fresh_install_opens_and_makes_one_anonymous_manifest_get() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        mount_manifest(&server, manifest(&server, RUNNING, good().sig)).await;
        let (ch, updater) = channel(&server, &dir);
        let view = ch.view();
        assert!(view.update.is_none() && !view.updates_off, "nothing gates");
        assert!(ch.check(app.handle(), false).await, "the manifest was read");
        assert_eq!(
            assert_anonymous(&server).await,
            [format!("GET {}", manifest_path())],
            "one manifest GET"
        );
        assert_eq!(updater.staged_version(), None);
        assert!(!ch.update_pending());
    }

    /// Rows 2 and 3: stored license state, active or locked, is removed; other data stays.
    #[test]
    fn rows_2_and_3_stored_license_state_is_removed_and_data_kept() {
        // The cleanup names the services this build's license wrote.
        assert!(LICENSE_SERVICES.contains(&keychain::SERVICE));
        for lock in [None, Some("revoked")] {
            let dir = tempfile::tempdir().unwrap();
            let records = dir.path().join("updates");
            let stores = stored_license(dir.path());
            if let Some(reason) = lock {
                stores[0]
                    .set(keychain::LOCK_REASON, &Secret::new(reason))
                    .unwrap();
            }
            std::fs::create_dir_all(&records).unwrap();
            std::fs::write(dir.path().join("settings.json"), "{}").unwrap();
            assert!(remove_license_records(
                &stores_of(&stores),
                dir.path(),
                &records
            ));
            for store in &stores {
                for account in ACCOUNTS {
                    assert!(store.get(account).unwrap().is_none(), "{account} is left");
                }
            }
            for file in fallback_files() {
                assert!(!dir.path().join(&file).exists(), "{file} is left");
            }
            assert!(dir.path().join("settings.json").exists(), "data intact");
            assert!(records.join(CLEANUP_DONE_FILE).exists());
            // A later launch finds nothing to do and asks the Keychain nothing.
            let refusing = Refusing::default();
            let later: [(&str, &dyn CredentialStore); 1] = [(LICENSE_SERVICES[0], &refusing)];
            assert!(remove_license_records(&later, dir.path(), &records));
            assert_eq!(refusing.asks.load(Ordering::SeqCst), 0);
        }
    }

    /// Row 4: an install-failed record is honoured while the manifest names its build, and
    /// discarded once it doesn't.
    #[tokio::test]
    async fn row_4_restart_records_are_honoured_only_while_the_manifest_names_the_build() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(dir.path().join("updates")).unwrap();
        std::fs::write(dir.path().join("updates/restart-install-failed"), NEXT).unwrap();
        mount_manifest(&server, ResponseTemplate::new(404)).await;
        let (ch, _) = channel(&server, &dir);
        assert!(!ch.check(app.handle(), false).await);
        assert_eq!(ch.view().update_attention, None, "nothing on offer yet");
        mount_manifest(&server, manifest(&server, NEXT, good().sig)).await;
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(
            ch.view().update_attention,
            Some(Attention::InstallFailed),
            "honoured for the build the manifest names"
        );
        mount_manifest(&server, manifest(&server, RUNNING, good().sig)).await;
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(ch.view().update_attention, None, "discarded once not named");
    }

    /// Row 5: a newer version downloads, verifies and stages, with no credential.
    #[tokio::test]
    async fn row_5_a_newer_version_stages_with_no_credential() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        serve(&server, NEXT, &good()).await;
        let (ch, updater) = channel(&server, &dir);
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(updater.staged_version().as_deref(), Some(NEXT), "staged");
        let view = ch.view();
        let update = view.update.as_ref().expect("the staged build");
        assert_eq!(
            (update.staged_version.as_str(), update.notes.as_str()),
            (NEXT, "Manifest notes")
        );
        assert!(view.restart_ready, "Restart to update works");
        assert!(ch.update_pending(), "the tick reads hourly while it waits");
        let asked = assert_anonymous(&server).await;
        assert!(asked.contains(&format!("HEAD {SIGNED_PATH}")), "{asked:?}");
        assert!(asked.contains(&format!("GET {SIGNED_PATH}")), "{asked:?}");
    }

    /// Row 6: a manifest that can't be used offers nothing, stages nothing and never panics.
    #[tokio::test]
    async fn row_6_an_unusable_manifest_or_build_offers_nothing() {
        // Another key than the one the test builds are signed with.
        let other_key =
            serde_json::from_str::<serde_json::Value>(include_str!("../../tauri.conf.json"))
                .unwrap()
                .pointer("/plugins/updater/pubkey")
                .and_then(|k| k.as_str())
                .map(str::to_string)
                .expect("the internal updater key");
        let older = Fixture {
            bytes: include_bytes!("../../tests/fixtures/updater/update-0.0.30.app.tar.gz"),
            sig: include_str!("../../tests/fixtures/updater/update-0.0.30.app.tar.gz.sig"),
        };
        // (case, the manifest was read)
        for (case, read) in [
            ("unreachable", false),
            ("malformed", false),
            ("no version", false),
            ("too large", false),
            ("redirect", false),
            ("older", true),
            ("same", true),
            ("another version's signature", true),
            ("signed with another key", true),
        ] {
            let server = MockServer::start().await;
            let dir = tempfile::tempdir().unwrap();
            let app = if case == "signed with another key" {
                app_with_pubkey(&other_key)
            } else {
                app()
            };
            match case {
                "unreachable" => mount_manifest(&server, ResponseTemplate::new(503)).await,
                "malformed" => {
                    let body = ResponseTemplate::new(200).set_body_string("{not json");
                    mount_manifest(&server, body).await
                }
                "no version" => {
                    let body = serde_json::json!({ "version": "soon" });
                    let reply = ResponseTemplate::new(200).set_body_json(body);
                    mount_manifest(&server, reply).await
                }
                "too large" => {
                    let body = "x".repeat(MANIFEST_MAX_BYTES + 1);
                    let reply = ResponseTemplate::new(200).set_body_string(body);
                    mount_manifest(&server, reply).await
                }
                "redirect" => {
                    let reply = ResponseTemplate::new(302)
                        .insert_header("location", "https://example.com/latest.json");
                    mount_manifest(&server, reply).await
                }
                "older" => {
                    let reply = manifest(&server, "0.0.30", good().sig);
                    mount_manifest(&server, reply).await
                }
                "same" => {
                    let reply = manifest(&server, RUNNING, good().sig);
                    mount_manifest(&server, reply).await
                }
                // A genuine 0.0.30 build announced as 0.0.32: `requireSignedVersion` refuses it.
                "another version's signature" => serve(&server, NEXT, &older).await,
                // A genuine 0.0.32 build, but this app verifies with another key.
                "signed with another key" => serve(&server, NEXT, &good()).await,
                other => unreachable!("{other}"),
            }
            let (ch, updater) = channel(&server, &dir);
            assert_eq!(ch.check(app.handle(), false).await, read, "{case}: read");
            assert_eq!(updater.staged_version(), None, "{case}: nothing staged");
            assert!(ch.view().update.is_none(), "{case}");
        }
    }

    /// Row 7: the manifest drops a staged version: it is discarded, never installed. A read
    /// that fails keeps it.
    #[tokio::test]
    async fn row_7_a_build_the_manifest_drops_is_discarded() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        serve(&server, NEXT, &good()).await;
        let (ch, updater) = channel(&server, &dir);
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(
            updater.staged_version().as_deref(),
            Some(NEXT),
            "staged first"
        );
        mount_manifest(&server, ResponseTemplate::new(503)).await;
        assert!(!ch.check(app.handle(), false).await);
        assert_eq!(
            updater.staged_version().as_deref(),
            Some(NEXT),
            "a blip keeps it"
        );
        mount_manifest(&server, manifest(&server, RUNNING, good().sig)).await;
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(updater.staged_version(), None, "discarded");
        assert!(
            updater.install_on_exit().is_none(),
            "nothing installs at quit"
        );
        assert!(!ch.update_pending(), "back to daily reads");
    }

    /// SV-REL-32: a staged build the channel has since pulled is discarded whatever the manifest
    /// names instead: the running version, an older one, or another newer one (here a relabelled
    /// bundle, which is refused in turn). Nothing is left to restart onto or install at quit.
    #[tokio::test]
    async fn a_pulled_build_is_discarded_whatever_the_manifest_names_instead() {
        let older = Fixture {
            bytes: include_bytes!("../../tests/fixtures/updater/update-0.0.30.app.tar.gz"),
            sig: include_str!("../../tests/fixtures/updater/update-0.0.30.app.tar.gz.sig"),
        };
        for (case, version, f) in [
            ("the running version", RUNNING, good()),
            ("an older version", "0.0.30", older),
            ("another newer version", "0.0.33", good()),
        ] {
            let server = MockServer::start().await;
            let (app, dir) = (app(), tempfile::tempdir().unwrap());
            serve(&server, NEXT, &good()).await;
            let (ch, updater) = channel(&server, &dir);
            assert!(ch.check(app.handle(), false).await, "{case}");
            assert_eq!(updater.staged_version().as_deref(), Some(NEXT), "{case}");

            serve(&server, version, &f).await;
            assert!(ch.check(app.handle(), false).await, "{case}: read");

            assert_eq!(updater.staged_version(), None, "{case}: discarded");
            assert!(ch.view().update.is_none(), "{case}");
            assert!(updater.restart_ready(false).is_err(), "{case}: no restart");
            assert!(
                updater.install_on_exit().is_none(),
                "{case}: nothing at quit"
            );
        }
    }

    /// Row 7b: while a build is staged (no wake), the next hourly tick reads the manifest again,
    /// so a pulled build is discarded before the quit. Once nothing is pending, reads are daily.
    #[tokio::test(start_paused = true)]
    async fn row_7b_a_pending_update_rereads_the_manifest_at_the_next_hourly_tick() {
        let ticks = ticks();
        let handle = launched(&ticks, DAY).await;
        let calls = || ticks.calls.load(Ordering::SeqCst);
        assert_eq!(calls(), 1, "launch");
        ticks.pending.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(HOUR + 1)).await;
        assert_eq!(calls(), 2, "the next hourly tick re-reads");
        tokio::time::sleep(Duration::from_secs(HOUR)).await;
        assert_eq!(calls(), 3, "and the one after");
        ticks.pending.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(3 * HOUR)).await;
        assert_eq!(calls(), 3, "daily once nothing is pending");
        handle.abort();
    }

    /// The manifest gate refuses a build the last read didn't name: before any read, and after
    /// a read naming another version. Nothing is downloaded or staged.
    #[tokio::test]
    async fn the_manifest_gate_refuses_a_build_the_last_read_did_not_name() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        serve(&server, NEXT, &good()).await;
        let (ch, updater) = channel(&server, &dir);
        let offer = || Offer {
            version: NEXT.into(),
            notes: String::new(),
            explicit: false,
        };
        assert_eq!(
            updater.run(app.handle(), offer()).await,
            Outcome::Withdrawn,
            "no read yet"
        );
        mount_manifest(&server, manifest(&server, RUNNING, good().sig)).await;
        assert!(ch.check(app.handle(), false).await);
        assert_eq!(
            updater.run(app.handle(), offer()).await,
            Outcome::Withdrawn,
            "the last read names another version"
        );
        assert_eq!(updater.staged_version(), None);
        assert!(updater.restart_ready(false).is_err());
    }

    /// Row 8: a Keychain delete that fails is logged, asked once, and retried next launch.
    #[test]
    fn row_8_a_refused_keychain_delete_is_asked_once_and_retried_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("updates");
        let refusing = Refusing::default();
        let fine = FakeStore::default();
        fine.set(keychain::INSTALL_TOKEN, &Secret::new("x"))
            .unwrap();
        for file in fallback_files() {
            std::fs::write(dir.path().join(file), "{}").unwrap();
        }
        let stores: [(&str, &dyn CredentialStore); 2] = [
            (LICENSE_SERVICES[0], &refusing),
            (LICENSE_SERVICES[1], &fine),
        ];
        assert!(!remove_license_records(&stores, dir.path(), &records));
        assert_eq!(refusing.asks.load(Ordering::SeqCst), 1, "no prompt loop");
        assert!(
            fine.get(keychain::INSTALL_TOKEN).unwrap().is_some(),
            "no other Keychain call this launch"
        );
        for file in fallback_files() {
            assert!(!dir.path().join(&file).exists(), "{file} still removed");
        }
        assert!(
            !remove_license_records(&stores, dir.path(), &records),
            "retried"
        );
        assert_eq!(refusing.asks.load(Ordering::SeqCst), 2, "once per launch");
    }

    /// Row 8b: a Keychain that keeps refusing is asked on at most three launches, then never.
    #[test]
    fn row_8b_a_keychain_that_keeps_refusing_is_asked_on_three_launches_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("updates");
        let refusing = Refusing::default();
        let stores: [(&str, &dyn CredentialStore); 1] = [(LICENSE_SERVICES[0], &refusing)];
        assert!(
            !remove_license_records(&stores, dir.path(), &records),
            "launch 1"
        );
        assert!(
            !remove_license_records(&stores, dir.path(), &records),
            "launch 2"
        );
        assert!(
            remove_license_records(&stores, dir.path(), &records),
            "launch 3: gives up"
        );
        assert_eq!(
            refusing.asks.load(Ordering::SeqCst),
            MAX_KEYCHAIN_REFUSALS as usize
        );
        assert!(remove_license_records(&stores, dir.path(), &records));
        assert_eq!(
            refusing.asks.load(Ordering::SeqCst),
            MAX_KEYCHAIN_REFUSALS as usize,
            "never asked again"
        );
    }

    /// Row 8c: with the update records folder unwritable the refusal cap can't be counted, so
    /// a refusing Keychain isn't asked at all, at any launch: no loop. The files still go.
    #[test]
    fn row_8c_with_the_records_folder_unwritable_the_keychain_is_never_asked() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be: nothing can be written under it.
        let records = dir.path().join("updates");
        std::fs::write(&records, "not a folder").unwrap();
        for file in fallback_files() {
            std::fs::write(dir.path().join(file), "{}").unwrap();
        }
        let (beta, staging) = (Refusing::default(), Refusing::default());
        let stores: [(&str, &dyn CredentialStore); 2] = [
            (LICENSE_SERVICES[0], &beta),
            (LICENSE_SERVICES[1], &staging),
        ];
        for launch in 1..=MAX_KEYCHAIN_REFUSALS + 1 {
            assert!(
                !remove_license_records(&stores, dir.path(), &records),
                "launch {launch}"
            );
        }
        for refusing in [&beta, &staging] {
            assert_eq!(refusing.asks.load(Ordering::SeqCst), 0, "never asked");
        }
        for file in fallback_files() {
            assert!(!dir.path().join(&file).exists(), "{file} still removed");
        }
    }

    /// Row 9: nothing handles a `codesense://beta/activate` link: neither `lib.rs` nor any
    /// license file registers a link handler or listens for opened URLs. Setup also builds no
    /// license service, client, scheduler or deadline watch.
    #[test]
    fn row_9_setup_builds_no_link_handler_or_license_service() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src.join("lib.rs")];
        for entry in std::fs::read_dir(src.join("license")).expect("the license folder") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
        assert!(files.len() > 5, "{files:?}");
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap();
            // The license files end in a beta-only test module; `production_text` cuts the
            // plain `#[cfg(test)]` ones.
            let text = text
                .find("#[cfg(all(test")
                .map_or(text.as_str(), |at| &text[..at]);
            let shipped = crate::test_support::source_guard::production_text(text);
            for handler in [
                "on_open_url",
                "deeplink::install",
                "DeepLinkExt",
                "RunEvent::Opened",
            ] {
                assert!(
                    !shipped.contains(handler),
                    "{} has {handler}",
                    path.display()
                );
            }
        }
        let here = include_str!("mod.rs");
        let install = &here[here.find("pub fn install(").expect("install")..];
        let install = &install[..install.find("\n}\n").unwrap()];
        for gone in [
            "deeplink::",
            "Service::new",
            "Client::new",
            "scheduler::run",
            "watch_deadline",
        ] {
            assert!(!install.contains(gone), "install still uses {gone}");
        }
    }

    /// Row 10: an agent's feedback is refused with a clear message, and the Help item is gone.
    #[test]
    fn row_10_feedback_is_refused_and_its_menu_item_hidden() {
        let refused = crate::mcp::feedback_tool::availability().unwrap_err();
        assert_eq!(refused, crate::mcp::feedback_tool::FEEDBACK_REFUSED);
        assert!(refused.contains("nothing was sent"), "{refused}");
        let menu = include_str!("../menu.rs");
        assert!(!menu.contains("let help_sub"), "no Help → feedback item");
    }

    // Row 11 (the internal channel) is `lib.rs`'s `license_channel_tests`.

    /// Row 12: launch, daily, and one check on every wake; a failed check is retried at the
    /// hourly tick, never faster; a long sleep makes no burst.
    #[tokio::test(start_paused = true)]
    async fn row_12_launch_daily_and_one_check_per_wake() {
        let ticks = ticks();
        let handle = launched(&ticks, DAY).await;
        let calls = || ticks.calls.load(Ordering::SeqCst);
        assert_eq!(calls(), 1, "launch");
        tokio::time::sleep(Duration::from_secs(23 * HOUR)).await;
        assert_eq!(calls(), 1, "nothing more within the day");
        // Offline at the daily check: retried at each hourly tick, never faster.
        ticks.ok.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(HOUR + 60)).await;
        assert_eq!(calls(), 2, "daily");
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        assert_eq!(calls(), 2, "no spin while offline");
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        assert_eq!(calls(), 3, "retried at the next tick");
        ticks.ok.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(HOUR)).await;
        assert_eq!(calls(), 4, "back online");
        tokio::time::sleep(Duration::from_secs(2 * HOUR)).await;
        assert_eq!(calls(), 4, "daily again once a check succeeded");
        // Asleep for three days: one check on the wake, no burst.
        ticks.slept.fetch_add(3 * 24 * 60 * 60, Ordering::SeqCst);
        tokio::time::sleep(PAST_PROBE).await;
        assert_eq!(calls(), 5, "one check on the wake");
        tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        assert_eq!(calls(), 5, "no burst after the wake");
        // A short sleep (ten minutes, a check minutes ago) still checks once on the wake.
        ticks.slept.fetch_add(10 * 60, Ordering::SeqCst);
        tokio::time::sleep(PAST_PROBE).await;
        assert_eq!(calls(), 6, "once on every wake, due or not");
        handle.abort();
    }

    /// A staging build's shorter check age (`CODESENSE_CHECK_INTERVAL_SECS`) drives the tick.
    #[tokio::test(start_paused = true)]
    async fn a_shorter_check_age_ticks_at_that_age() {
        let ticks = ticks();
        let handle = launched(&ticks, Duration::from_secs(60)).await;
        assert_eq!(ticks.calls.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(61)).await;
        assert_eq!(ticks.calls.load(Ordering::SeqCst), 2, "a minute later");
        handle.abort();
    }

    /// Row 13: a build with updates off asks nothing from any entry: the tick's check and
    /// Check now.
    #[tokio::test]
    async fn row_13_with_updates_off_no_entry_makes_a_request() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        serve(&server, NEXT, &good()).await;
        let (ch, updater) = channel_with(&server, &dir, true);
        for explicit in [false, true] {
            assert!(
                !ch.check(app.handle(), explicit).await,
                "explicit {explicit}"
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        assert_eq!(updater.staged_version(), None);
        assert!(ch.view().updates_off);
    }

    /// Check now while a check runs asks nothing: two reads can't land out of order.
    #[tokio::test]
    async fn a_check_while_another_runs_asks_nothing() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        serve(&server, NEXT, &good()).await;
        let (ch, _) = channel(&server, &dir);
        ch.checking.store(true, Ordering::SeqCst);
        assert!(!ch.check(app.handle(), true).await);
        assert!(server.received_requests().await.unwrap().is_empty());
        ch.checking.store(false, Ordering::SeqCst);
        assert!(
            ch.check(app.handle(), true).await,
            "runs once the other ends"
        );
        assert!(
            !ch.checking.load(Ordering::SeqCst),
            "the flag is cleared after a check"
        );
    }
}
