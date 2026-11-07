// SPDX-License-Identifier: MIT
//! The push watcher's timing and state (SW-PB-22e-1): the K2 debounce, the K1
//! interval, the K12 retry, single flight, and the K13 choice between a checkin
//! and a push. It makes no network call. Each due tick builds the payload
//! through the payload builder (the single exit, DR-14) and queues one
//! [`Request`] for the HTTP client (`client.rs`, `push.rs`). The client takes
//! requests with [`Watcher::next_action`] and reports each result with
//! [`Watcher::request_done`], which drops a result whose id is no longer in
//! flight (a cancelled request). The record write stays with the caller.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use super::manifest::{self, normalize_rel, ManifestError, PublishState};
use super::payload::{self, FolderReader, Mode, Payload, PayloadError};
use super::state::{LastError, LocalState, PublishRecord, SyncStatus};

/// K2: a trailing 60-second debounce after the last change.
pub const DEBOUNCE: Duration = Duration::from_secs(60);
/// K1: the checkin interval while published (DP-D32, ASSUMES).
pub const TICK_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// K12: the retry delays after a failed request. Then the next K1 tick.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(240),
];

/// What the HTTP client must do next. At most one is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The local sha equals the record's `last_sha`: refresh metadata, then R7.
    Checkin,
    /// K13: the sha differs, or the dashboard is publishing. The push sequence.
    /// Every push refreshes metadata with R1. `publish` also sends R2 first,
    /// before the content upload.
    Push {
        publish: bool,
        payload: Payload,
        title: String,
        visibility: &'static str,
    },
}

/// One action, tagged with an id so its outcome can be matched to it. A
/// request cancelled by an unpublish gets no outcome applied (22e-1 review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub action: Action,
}

/// The result the client reports for one action. The watcher does not record
/// a sha as pushed: the caller writes the record on `Success` only, so after a
/// failure the next tick still sees a changed sha and pushes again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// A 5xx, 429, timeout or connection error: retry at K12, then the next tick.
    Failed(LastError),
    /// E5, E6 on a published dashboard, or another refusal: no retry. The next
    /// K1 tick decides.
    NoRetry(LastError),
    /// E1: pushes stop until the token is replaced (AC-6).
    AuthFailed,
    /// E6 while the dashboard is unpublished: the owner took it down, so the
    /// result is discarded silently (AC-5).
    Ignored,
}

#[derive(Debug)]
pub enum TickError {
    Manifest(ManifestError),
    Payload(PayloadError),
}

impl From<ManifestError> for TickError {
    fn from(e: ManifestError) -> Self {
        TickError::Manifest(e)
    }
}

impl From<PayloadError> for TickError {
    fn from(e: PayloadError) -> Self {
        TickError::Payload(e)
    }
}

/// The kind of request, without its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Checkin,
    Push,
}

impl Action {
    fn kind(&self) -> Kind {
        match self {
            Action::Checkin => Kind::Checkin,
            Action::Push { .. } => Kind::Push,
        }
    }
}

#[derive(Debug)]
pub struct Watcher {
    /// The trailing debounce or retry deadline, when one is pending.
    debounce_until: Option<Instant>,
    /// The next K1 tick.
    next_tick: Instant,
    /// The id and kind of the request in flight, when there is one.
    in_flight: Option<(u64, Kind)>,
    /// The id the next request gets.
    next_id: u64,
    /// A change arrived while a request was in flight.
    dirty: bool,
    /// Failed requests since the last success or the last exhausted K12 run.
    retries: usize,
    /// The kind that failed and is due to run again at the K12 deadline.
    retry: Option<Kind>,
    actions: VecDeque<Request>,
}

/// `publishing` and `published` are the states the watcher works in (AC-9).
/// An auth failure pauses them until the token is replaced (AC-6).
fn watching(record: &PublishRecord) -> bool {
    matches!(record.state, LocalState::Publishing | LocalState::Published)
        && record.sync_status != SyncStatus::AuthFailed
}

impl Watcher {
    pub fn new(now: Instant) -> Watcher {
        Watcher {
            debounce_until: None,
            next_tick: now + TICK_INTERVAL,
            in_flight: None,
            next_id: 1,
            dirty: false,
            retries: 0,
            retry: None,
            actions: VecDeque::new(),
        }
    }

    /// Drop all pending work: queued actions, the request in flight, the
    /// debounce, the retry and the dirty flag. Used when the dashboard is not
    /// watched, so nothing queued before an unpublish goes out.
    fn clear_pending(&mut self) {
        self.actions.clear();
        self.in_flight = None;
        self.dirty = false;
        self.debounce_until = None;
        self.retry = None;
        self.retries = 0;
    }

    /// An allowlisted file changed: restart the trailing debounce. A path that
    /// is not on the allowlist is not part of the payload, so it is ignored.
    pub fn file_changed(&mut self, now: Instant, rel: &str, allowlist: &[String]) {
        let Ok(rel) = normalize_rel(rel) else {
            return;
        };
        if allowlist.iter().any(|f| *f == rel) {
            self.request_tick(now + DEBOUNCE);
        }
    }

    /// Publish intent (the record moved to `publishing`): tick at once.
    pub fn publish_requested(&mut self, now: Instant) {
        self.request_tick(now);
    }

    /// A failed build keeps the pending change, and tries again after the
    /// debounce, not on every pass of the loop.
    fn retry_later(&mut self, now: Instant, interval_due: bool) {
        if self.debounce_until.is_some() {
            self.debounce_until = Some(now + DEBOUNCE);
        }
        if interval_due {
            self.next_tick = now + TICK_INTERVAL;
        }
    }

    fn request_tick(&mut self, at: Instant) {
        if self.in_flight.is_some() {
            self.dirty = true;
        } else {
            self.debounce_until = Some(at);
        }
    }

    /// Run the timers at `now`. A due tick builds the payload and queues one
    /// action. Draft and unpublished records are left alone: no action, and no
    /// file is read (AC-9). A failed build keeps the pending work, so the next
    /// poll retries it.
    pub fn poll(
        &mut self,
        now: Instant,
        record: &PublishRecord,
        folder: &Path,
    ) -> Result<(), TickError> {
        if record.sync_status == SyncStatus::AuthFailed {
            // Paused: no tick is spent, so a replaced token resumes at once.
            self.clear_pending();
            return Ok(());
        }
        // Only a published record pauses: a publishing one sends R2 again,
        // which is what publishes it on the server.
        if record.state == LocalState::Published
            && record.last_error == Some(LastError::DashboardNotPublished)
        {
            // The server says it is unpublished while this Mac says published.
            // Re-sending would upload every file and fail at commit, each time.
            // Paused until the owner unpublishes and publishes again.
            self.clear_pending();
            return Ok(());
        }
        if !watching(record) {
            self.clear_pending();
            self.next_tick = now + TICK_INTERVAL;
            return Ok(());
        }
        if self.in_flight.is_some() {
            return Ok(());
        }
        let debounced = self.debounce_until.is_some_and(|t| t <= now);
        let interval_due = self.next_tick <= now;
        if !debounced && !interval_due {
            return Ok(());
        }

        let manifest = match manifest::load(folder) {
            Ok(manifest) => manifest,
            Err(e) => {
                self.retry_later(now, interval_due);
                return Err(e.into());
            }
        };
        // The manifest is the off switch too (DR-13). A record can still say
        // publishing after a crash between its save and the manifest write. A
        // slug edited after publishing also stops pushes: the record's slug is
        // the one on the server.
        if manifest.state != PublishState::Published || manifest.slug != record.slug {
            self.clear_pending();
            return Ok(());
        }
        let built = match payload::build_approved(&manifest, Mode::Published, &FolderReader { folder }, record) {
            Ok(built) => built,
            Err(e) => {
                self.retry_later(now, interval_due);
                return Err(e.into());
            }
        };
        // The pending work is consumed only now that the payload is built.
        self.debounce_until = None;
        if interval_due {
            self.next_tick = now + TICK_INTERVAL;
        }

        let publish = record.state == LocalState::Publishing;
        if publish || record.last_sha.as_deref() != Some(&built.payload.manifest_sha) {
            self.start(Action::Push {
                publish,
                payload: built.payload,
                title: manifest.title.clone(),
                visibility: manifest.visibility.as_str(),
            });
        } else if interval_due || self.retry == Some(Kind::Checkin) {
            self.start(Action::Checkin);
        }
        // A debounced run with an unchanged sha needs no call (DR-14).
        Ok(())
    }

    /// The next action for the HTTP client. The record is checked again here
    /// too: if the dashboard left `publishing` or `published` after the action
    /// was queued, the action is dropped unsent.
    pub fn next_action(&mut self, record: &PublishRecord) -> Option<Request> {
        if !watching(record) {
            self.clear_pending();
            return None;
        }
        self.actions.pop_front()
    }

    /// The client finished the request in flight, with `outcome`. With nothing
    /// in flight this does nothing. A failed request is retried as the same
    /// kind of request at K12. A change that came during the request runs one
    /// more tick after a fresh debounce window.
    /// Returns `false` and does nothing when `id` is not the request in flight:
    /// a cancelled request's late result is dropped.
    pub fn request_done(&mut self, now: Instant, id: u64, outcome: Outcome) -> bool {
        let Some((in_flight, kind)) = self.in_flight else {
            return false;
        };
        if in_flight != id {
            return false;
        }
        self.in_flight = None;
        let retry_at = match outcome {
            Outcome::Failed(_) => match RETRY_DELAYS.get(self.retries) {
                Some(delay) => {
                    self.retries += 1;
                    self.retry = Some(kind);
                    Some(now + *delay)
                }
                None => {
                    // K12 is spent: the next K1 tick starts a new run.
                    self.retries = 0;
                    self.retry = None;
                    None
                }
            },
            Outcome::Success | Outcome::NoRetry(_) | Outcome::AuthFailed | Outcome::Ignored => {
                self.retries = 0;
                self.retry = None;
                None
            }
        };
        let dirty_at = std::mem::take(&mut self.dirty).then(|| now + DEBOUNCE);
        self.debounce_until = retry_at.into_iter().chain(dirty_at).max();
        true
    }

    /// An unpublish cancels the request in flight and everything queued, so a
    /// late result is dropped (AC-5 owner review). Returns a fresh id for the
    /// unpublish call itself.
    pub fn cancel_for_unpublish(&mut self) -> u64 {
        self.clear_pending();
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // Explicit updates enqueue a validated push without the periodic source-hash comparison.
    pub(super) fn start(&mut self, action: Action) {
        let id = self.next_id;
        self.next_id += 1;
        self.in_flight = Some((id, action.kind()));
        self.retry = None;
        self.actions.push_back(Request { id, action });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_publish::manifest::MANIFEST_FILE;
    use crate::dashboard_publish::state::{Event, SyncStatus};
    use std::fs;

    const PAGE: &str = "<h1>AI usage</h1>";

    fn dashboard() -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("index.html"), PAGE).unwrap();
        fs::write(
            folder.path().join(MANIFEST_FILE),
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: published\n  visibility: private\n  files:\n    - index.html\n",
        )
        .unwrap();
        folder
    }

    fn allowlist() -> Vec<String> {
        vec!["index.html".into()]
    }

    fn record(state: LocalState, last_sha: Option<String>) -> PublishRecord {
        let mut r = PublishRecord::new("ai-usage");
        r.state = state;
        r.last_sha = last_sha;
        r
    }

    fn sha_of(folder: &Path) -> String {
        let manifest = manifest::load(folder).unwrap();
        payload::build(
            &manifest,
            Mode::Published,
            &FolderReader { folder },
        )
        .unwrap()
        .payload
        .manifest_sha
    }

    /// Take every queued action, as the client would.
    fn drain(w: &mut Watcher, rec: &PublishRecord) -> Vec<Action> {
        std::iter::from_fn(|| w.next_action(rec))
            .map(|r| r.action)
            .collect()
    }

    fn poll(w: &mut Watcher, t0: Instant, s: u64, rec: &PublishRecord, folder: &Path) {
        w.poll(t0 + secs(s), rec, folder)
            .unwrap();
    }

    /// Report the outcome of the request in flight.
    fn done(w: &mut Watcher, at: Instant, outcome: Outcome) {
        let (id, _) = w.in_flight.expect("a request is in flight");
        assert!(w.request_done(at, id, outcome));
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn five_changes_inside_the_window_give_one_tick() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        let allow = allowlist();
        for i in 0..5 {
            w.file_changed(t0 + secs(10 * i), "index.html", &allow);
        }
        poll(&mut w, t0, 99, &rec, folder.path());
        assert!(
            drain(&mut w, &rec).is_empty(),
            "the window is still open at 99 s"
        );

        poll(&mut w, t0, 100, &rec, folder.path());
        let actions = drain(&mut w, &rec);
        assert_eq!(actions.len(), 1, "one tick for five changes: {actions:?}");
        assert!(matches!(actions[0], Action::Push { publish: false, .. }));
    }

    #[test]
    fn a_change_during_a_push_gives_exactly_one_more_tick_after_it_completes() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        let allow = allowlist();
        w.file_changed(t0, "index.html", &allow);
        poll(&mut w, t0, 60, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "first push starts");

        // The change arrives while the first push is in flight: no second push
        // may start, however often the timers run.
        w.file_changed(t0 + secs(61), "index.html", &allow);
        for s in [62, 90, 500] {
            poll(&mut w, t0, s, &rec, folder.path());
        }
        assert!(drain(&mut w, &rec).is_empty(), "no concurrent push");

        done(&mut w, t0 + secs(70), Outcome::Success);
        poll(&mut w, t0, 129, &rec, folder.path());
        assert!(
            drain(&mut w, &rec).is_empty(),
            "the fresh debounce is still open"
        );
        poll(&mut w, t0, 130, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "exactly one more push");

        done(&mut w, t0 + secs(131), Outcome::Success);
        poll(&mut w, t0, 400, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "no third push");
    }

    #[test]
    fn a_debounced_tick_with_an_unchanged_sha_makes_no_call() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, Some(sha_of(folder.path())));
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty());
    }

    #[test]
    fn a_change_outside_the_allowlist_starts_no_tick() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        let allow = allowlist();
        w.file_changed(t0, "notes.txt", &allow);
        w.file_changed(t0, "../index.html", &allow);
        poll(&mut w, t0, 61, &rec, folder.path());
        assert!(
            drain(&mut w, &rec).is_empty(),
            "a non-allowlisted change ticked"
        );
    }

    #[test]
    fn a_build_error_keeps_the_pending_change_and_the_next_poll_retries_it() {
        // No dashboard.yml yet: the build fails on the first tick.
        let folder = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        w.file_changed(t0, "index.html", &allowlist());
        assert!(w
            .poll(t0 + secs(60), &rec, folder.path())
            .is_err());
        assert!(drain(&mut w, &rec).is_empty());

        // The owner fixes the folder. The pending change is still there, and it
        // is tried again a minute after the failure, not on the next pass.
        let fixed = dashboard();
        poll(&mut w, t0, 61, &rec, fixed.path());
        assert!(drain(&mut w, &rec).is_empty(), "retried before the debounce");
        poll(&mut w, t0, 121, &rec, fixed.path());
        assert!(matches!(
            drain(&mut w, &rec).as_slice(),
            [Action::Push { publish: false, .. }]
        ));
    }

    #[test]
    fn an_interval_tick_with_an_unchanged_sha_is_a_checkin_only() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, Some(sha_of(folder.path())));
        poll(&mut w, t0, 899, &rec, folder.path());
        assert!(
            drain(&mut w, &rec).is_empty(),
            "the interval is not due at 899 s"
        );

        poll(&mut w, t0, 900, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec), vec![Action::Checkin]);
    }

    #[test]
    fn an_interval_tick_with_a_changed_sha_is_a_push() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, Some("stale".into()));
        poll(&mut w, t0, 900, &rec, folder.path());
        match drain(&mut w, &rec).as_slice() {
            [Action::Push {
                publish, payload, ..
            }] => {
                assert!(!publish);
                assert_eq!(payload.manifest_sha, sha_of(folder.path()));
            }
            other => panic!("expected one push, got {other:?}"),
        }
    }

    #[test]
    fn a_publish_intent_pushes_at_once_with_the_publish_flag() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        w.publish_requested(t0);
        poll(&mut w, t0, 0, &rec, folder.path());
        assert!(matches!(
            drain(&mut w, &rec).as_slice(),
            [Action::Push { publish: true, .. }]
        ));
    }

    #[test]
    fn a_failed_push_retries_at_k12_without_recording_the_sha() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        // last_sha stays None: the failed push is not recorded as pushed.
        let rec = record(LocalState::Published, None);
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1);
        done(
            &mut w,
            t0 + secs(70),
            Outcome::Failed(LastError::ServerError),
        );

        // Retry 1 at +1 minute, then 2 and 4 minutes later.
        poll(&mut w, t0, 129, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 1 is not due yet");
        poll(&mut w, t0, 130, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "retry 1 pushes again");
        done(
            &mut w,
            t0 + secs(131),
            Outcome::Failed(LastError::ServerError),
        );

        poll(&mut w, t0, 250, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 2 is not due yet");
        poll(&mut w, t0, 251, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "retry 2 pushes");
        done(
            &mut w,
            t0 + secs(252),
            Outcome::Failed(LastError::ServerError),
        );

        poll(&mut w, t0, 491, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 3 is not due yet");
        poll(&mut w, t0, 492, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "retry 3 pushes");
        done(
            &mut w,
            t0 + secs(493),
            Outcome::Failed(LastError::ServerError),
        );

        // K12 is spent: no more retries before the next K1 tick.
        poll(&mut w, t0, 800, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "no retry after the third");
    }

    #[test]
    fn a_failed_checkin_retries_at_k12_then_waits_for_the_next_tick() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        // The sha is unchanged, so the interval run is a checkin.
        let rec = record(LocalState::Published, Some(sha_of(folder.path())));
        poll(&mut w, t0, 900, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec), vec![Action::Checkin]);
        done(
            &mut w,
            t0 + secs(910),
            Outcome::Failed(LastError::NoResponse),
        );

        // Retry 1 at +1 minute, retry 2 at +2 minutes, retry 3 at +4 minutes.
        poll(&mut w, t0, 969, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 1 is not due yet");
        poll(&mut w, t0, 970, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec), vec![Action::Checkin], "retry 1");
        done(
            &mut w,
            t0 + secs(971),
            Outcome::Failed(LastError::NoResponse),
        );

        poll(&mut w, t0, 1090, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 2 is not due yet");
        poll(&mut w, t0, 1091, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec), vec![Action::Checkin], "retry 2");
        done(
            &mut w,
            t0 + secs(1092),
            Outcome::Failed(LastError::NoResponse),
        );

        poll(&mut w, t0, 1331, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "retry 3 is not due yet");
        poll(&mut w, t0, 1332, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec), vec![Action::Checkin], "retry 3");
        done(
            &mut w,
            t0 + secs(1333),
            Outcome::Failed(LastError::NoResponse),
        );

        // K12 is spent: nothing until the next K1 tick (at 1800 s).
        poll(&mut w, t0, 1700, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "no retry after the third");
        poll(&mut w, t0, 1800, &rec, folder.path());
        assert_eq!(
            drain(&mut w, &rec),
            vec![Action::Checkin],
            "the next K1 tick"
        );

        // A new run after the next tick gets its own K12 schedule.
        done(
            &mut w,
            t0 + secs(1801),
            Outcome::Failed(LastError::NoResponse),
        );
        poll(&mut w, t0, 1861, &rec, folder.path());
        assert_eq!(
            drain(&mut w, &rec),
            vec![Action::Checkin],
            "a fresh retry 1"
        );
    }

    #[test]
    fn a_push_queued_before_an_unpublish_is_dropped_by_poll_even_undequeued() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let mut rec = PublishRecord::new("ai-usage");
        rec.apply(Event::Publish).unwrap();
        w.publish_requested(t0);
        poll(&mut w, t0, 0, &rec, folder.path());

        // The owner unpublishes. Nothing dequeues before the next poll.
        rec.state = LocalState::Unpublished;
        poll(&mut w, t0, 1, &rec, folder.path());
        // Even if the record is back in a watched state, the stale push is gone.
        rec.state = LocalState::Published;
        assert!(drain(&mut w, &rec).is_empty(), "the stale push went out");

        // The watcher is not stuck: a later change pushes normally.
        w.file_changed(t0 + secs(100), "index.html", &allowlist());
        poll(&mut w, t0, 160, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1);
    }

    #[test]
    fn a_no_retry_outcome_waits_for_the_next_tick() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1);
        done(
            &mut w,
            t0 + secs(70),
            Outcome::NoRetry(LastError::RemoteVersionChanged),
        );

        poll(&mut w, t0, 200, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "E5 is not retried");
    }

    #[test]
    fn an_action_queued_before_an_unpublish_is_dropped_unsent() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let published = record(LocalState::Published, None);
        let unpublished = record(LocalState::Unpublished, None);
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &published, folder.path());

        assert_eq!(w.next_action(&unpublished), None, "nothing is sent");
        // The in-flight state is cleared too, so a later publish is not blocked.
        w.file_changed(t0 + secs(100), "index.html", &allowlist());
        poll(&mut w, t0, 160, &published, folder.path());
        assert_eq!(drain(&mut w, &published).len(), 1);
    }

    #[test]
    fn a_cancelled_request_result_is_dropped() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let published = record(LocalState::Published, None);
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &published, folder.path());
        let request = w.next_action(&published).expect("a push");

        // The owner unpublishes while the request is in flight. The request is
        // cancelled, so its late success must not be applied.
        let unpublished = record(LocalState::Unpublished, None);
        poll(&mut w, t0, 61, &unpublished, folder.path());
        assert!(!w.request_done(t0 + secs(62), request.id, Outcome::Success));

        // A result carrying a stale id is dropped even while another request
        // is in flight.
        w.file_changed(t0 + secs(100), "index.html", &allowlist());
        poll(&mut w, t0, 160, &published, folder.path());
        let next = w.next_action(&published).expect("the next push");
        assert_ne!(next.id, request.id);
        assert!(!w.request_done(t0 + secs(161), request.id, Outcome::Success));
        assert!(w.request_done(t0 + secs(162), next.id, Outcome::Success));
    }

    #[test]
    fn an_auth_failure_pauses_every_tick_until_the_record_is_ok_again() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let mut rec = record(LocalState::Published, None);
        w.file_changed(t0, "index.html", &allowlist());
        poll(&mut w, t0, 60, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1);
        done(&mut w, t0 + secs(61), Outcome::AuthFailed);

        rec.sync_status = SyncStatus::AuthFailed;
        w.file_changed(t0 + secs(100), "index.html", &allowlist());
        poll(&mut w, t0, 900, &rec, folder.path());
        assert!(drain(&mut w, &rec).is_empty(), "a call while auth_failed");

        // The token is replaced: the pending change is pushed at once, because
        // the overdue interval tick runs now.
        rec.sync_status = SyncStatus::Idle;
        poll(&mut w, t0, 901, &rec, folder.path());
        assert!(matches!(
            drain(&mut w, &rec).as_slice(),
            [Action::Push { publish: false, .. }]
        ));
    }

    #[test]
    fn a_request_done_with_nothing_in_flight_does_nothing() {
        let folder = dashboard();
        let t0 = Instant::now();
        let mut w = Watcher::new(t0);
        let rec = record(LocalState::Published, None);
        // A stray failure arms no retry.
        assert!(!w.request_done(t0, 0, Outcome::Failed(LastError::ServerError)));
        poll(&mut w, t0, 60, &rec, folder.path());
        assert!(
            drain(&mut w, &rec).is_empty(),
            "a stray failure armed a retry"
        );

        // A stray success does not clear a real push in flight.
        w.file_changed(t0 + secs(100), "index.html", &allowlist());
        poll(&mut w, t0, 160, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1);
        assert!(!w.request_done(t0 + secs(161), 0, Outcome::Success));
        done(&mut w, t0 + secs(162), Outcome::Success);
        w.file_changed(t0 + secs(170), "index.html", &allowlist());
        poll(&mut w, t0, 230, &rec, folder.path());
        assert_eq!(drain(&mut w, &rec).len(), 1, "one real push, one real tick");
    }

    #[test]
    fn draft_and_unpublished_ticks_make_no_call_and_read_no_file() {
        // No dashboard.yml: any read of the folder would fail the poll.
        let folder = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        for state in [LocalState::Draft, LocalState::Unpublished] {
            let mut w = Watcher::new(t0);
            let rec = record(state, None);
            w.file_changed(t0, "index.html", &allowlist());
            poll(&mut w, t0, 61, &rec, folder.path());
            poll(&mut w, t0, 901, &rec, folder.path());
            assert!(drain(&mut w, &rec).is_empty(), "{state:?} made a call");
        }
    }

    #[test]
    fn the_watcher_gets_its_files_only_from_the_payload_builder() {
        let src = include_str!("watch.rs");
        let src = src.split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("payload::build_approved("));
        for direct in ["fs::", "File::open", "read_dir", "read_to_string"] {
            assert!(!src.contains(direct), "the watcher reads files: {direct}");
        }
    }

    #[test]
    fn the_watcher_imports_no_remote_or_routine_module() {
        let src = include_str!("watch.rs");
        let src = src.split("#[cfg(test)]").next().unwrap();
        for line in src.lines().filter(|l| l.trim_start().starts_with("use ")) {
            for needle in ["remote", "routine"] {
                assert!(!line.contains(needle), "watcher imports {needle}: {line}");
            }
        }
    }
}
