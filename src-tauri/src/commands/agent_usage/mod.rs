// SPDX-License-Identifier: MIT
pub mod coordinator;
pub mod scheduler;
pub mod supervisor;
pub mod types;

pub use coordinator::AgentUsageCoordinator;
pub use types::*;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use codesense_pty_core::agent_cli::{adapter, descriptor, Usage, UsageWindow};

use crate::agents::adapter::cli_of_provider;
use crate::commands::async_helpers::await_blocking;
use crate::commands::paths::codesense_home_dir;
use crate::domain::Principal;
use crate::install::agent_account_store::{self, AgentProvider};

pub const USAGE_EVENT: &str = "agent-usage-changed";

pub struct AgentUsageState {
    inner: Arc<AgentUsageInner>,
}

#[derive(Default)]
struct AgentUsageInner {
    coordinator: Mutex<AgentUsageCoordinator>,
    admission: Mutex<supervisor::PollAdmission>,
    /// Each status-line capability's account: its user, id and provider.
    status_routes: Mutex<HashMap<String, (u64, String, AgentProvider)>>,
    status_socket: Mutex<Option<std::path::PathBuf>>,
    last_publish: Mutex<HashMap<String, std::time::Instant>>,
    last_persist: Mutex<HashMap<String, std::time::Instant>>,
    publish_queue: Mutex<LatestPublishQueue>,
    poll_cancellations: Mutex<HashMap<String, CancellationToken>>,
    poll_backoff: Mutex<HashMap<String, (scheduler::RetryBackoff, std::time::Instant)>>,
    scheduler_started: AtomicBool,
    dashboard_visible: AtomicBool,
    visible_viewers: Mutex<HashSet<String>>,
    force_visible_refresh: AtomicBool,
    scheduler_user: AtomicU64,
    scheduler_wake: tokio::sync::Notify,
}

#[derive(Default)]
struct LatestPublishQueue {
    pending: HashMap<String, (u64, AgentUsageSnapshot)>,
    active: HashSet<String>,
}

impl LatestPublishQueue {
    fn enqueue(&mut self, github_user_id: u64, snapshot: AgentUsageSnapshot) -> bool {
        let account_id = snapshot.account_profile_id.clone();
        self.pending
            .insert(account_id.clone(), (github_user_id, snapshot));
        self.active.insert(account_id)
    }

    fn take(&mut self, account_id: &str) -> Option<(u64, AgentUsageSnapshot)> {
        self.pending.remove(account_id)
    }

    /// Returns true when a producer replaced the pending value while this
    /// publisher was active, so the same drain task should continue.
    fn finish_or_continue(&mut self, account_id: &str) -> bool {
        if self.pending.contains_key(account_id) {
            true
        } else {
            self.active.remove(account_id);
            false
        }
    }

    fn remove(&mut self, account_id: &str) {
        self.pending.remove(account_id);
    }
}

impl Default for AgentUsageState {
    fn default() -> Self {
        Self {
            inner: Arc::new(AgentUsageInner::default()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshReply {
    pub outcome: RefreshOutcome,
    pub refresh_state: RefreshState,
}

/// Canonical bounded reader for a persisted CN-20 usage snapshot. Corrupt,
/// oversized, or absent advisory telemetry is represented by `None`.
pub(crate) fn read_persisted_snapshot(
    root: &std::path::Path,
    principal: &Principal,
    account_id: &str,
) -> Option<AgentUsageSnapshot> {
    let path = crate::context_repo::path_to_user_home(root, principal)
        .join("agent-usage")
        .join(format!("{account_id}.json"));
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() <= types::MAX_ADAPTER_MESSAGE)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

#[tauri::command]
pub async fn read_agent_usage(
    app: AppHandle,
    github_user_id: u64,
) -> Result<Vec<AgentUsageSnapshot>, String> {
    let (account_ids, restored) = await_blocking("read_agent_usage.accounts", move || {
        let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
        let ids = agent_account_store::list_profiles(&root, &Principal::from(github_user_id))
            .map(|profiles| {
                profiles
                    .into_iter()
                    .map(|profile| profile.id)
                    .collect::<std::collections::HashSet<_>>()
            })
            .map_err(|error| error.to_string())?;
        let principal = Principal::from(github_user_id);
        let mut restored = Vec::new();
        for id in &ids {
            if let Some(snapshot) = read_persisted_snapshot(&root, &principal, id) {
                restored.push(snapshot);
            }
        }
        Ok((ids, restored))
    })
    .await?;
    let state = app.state::<AgentUsageState>();
    let mut coordinator = state.inner.coordinator.lock().unwrap();
    for snapshot in restored {
        let _ = coordinator.restore(snapshot);
    }
    Ok(coordinator
        .all()
        .into_iter()
        .filter(|snapshot| account_ids.contains(&snapshot.account_profile_id))
        .collect())
}

/// Update dashboard visibility and lazily start the single Rust-owned cadence
/// loop. The webview never owns provider timers or long-running invocations.
#[tauri::command]
pub fn set_agent_usage_visibility(
    app: AppHandle,
    github_user_id: u64,
    viewer_id: String,
    visible: bool,
) {
    let state = app.state::<AgentUsageState>();
    let visible = {
        let mut viewers = state.inner.visible_viewers.lock().unwrap();
        if visible {
            viewers.insert(viewer_id);
        } else {
            viewers.remove(&viewer_id);
        }
        !viewers.is_empty()
    };
    let was_visible = state
        .inner
        .dashboard_visible
        .swap(visible, Ordering::AcqRel);
    if visible && !was_visible {
        state
            .inner
            .force_visible_refresh
            .store(true, Ordering::Release);
    }
    state
        .inner
        .scheduler_user
        .store(github_user_id, Ordering::Release);
    state.inner.scheduler_wake.notify_one();
    if state.inner.scheduler_started.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(run_scheduler(app));
}

async fn run_scheduler(app: AppHandle) {
    let mut last_attempt = HashMap::<String, std::time::Instant>::new();
    loop {
        let wake = app.state::<AgentUsageState>().inner.clone();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            _ = wake.scheduler_wake.notified() => {}
        }
        let state = app.state::<AgentUsageState>();
        let github_user_id = state.inner.scheduler_user.load(Ordering::Acquire);
        if github_user_id == 0 {
            continue;
        }
        let visible = state.inner.dashboard_visible.load(Ordering::Acquire);
        let force_refresh = visible
            && state
                .inner
                .force_visible_refresh
                .swap(false, Ordering::AcqRel);
        let profiles = match await_blocking("agent_usage_scheduler.list", move || {
            let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
            agent_account_store::list_profiles(&root, &Principal::from(github_user_id))
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(profiles) => profiles,
            Err(_) => continue,
        };
        last_attempt.retain(|id, _| profiles.iter().any(|profile| profile.id == *id));
        for profile in profiles.into_iter().filter(|profile| {
            profile.is_attached()
                && profile.auth_state == agent_account_store::AgentAccountAuthState::Ready
        }) {
            let Some(interval) = scheduler::refresh_interval(usage_of(profile.provider), visible)
            else {
                continue;
            };
            let backoff = state
                .inner
                .poll_backoff
                .lock()
                .unwrap()
                .get(&profile.id)
                .map(|(_, retry_at)| *retry_at);
            if backoff.is_some_and(|retry_at| retry_at > std::time::Instant::now()) {
                continue;
            }
            if !force_refresh
                && backoff.is_none()
                && last_attempt
                    .get(&profile.id)
                    .is_some_and(|last| last.elapsed() < interval)
            {
                continue;
            }
            let account_id = profile.id.clone();
            if refresh_agent_usage(app.clone(), github_user_id, profile.id, profile.generation)
                .await
                .is_ok_and(|reply| {
                    matches!(
                        reply.outcome,
                        RefreshOutcome::Started
                            | RefreshOutcome::Coalesced
                            | RefreshOutcome::Cached
                    )
                })
            {
                last_attempt.insert(account_id, std::time::Instant::now());
            }
        }
    }
}

#[tauri::command]
pub async fn refresh_agent_usage(
    app: AppHandle,
    github_user_id: u64,
    account_id: String,
    expected_generation: u64,
) -> Result<RefreshReply, String> {
    let principal = Principal::from(github_user_id);
    let lookup_id = account_id.clone();
    let profile = await_blocking("refresh_agent_usage.validate", move || {
        let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
        agent_account_store::list_profiles(&root, &principal)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|profile| profile.id == lookup_id)
            .ok_or_else(|| "not-found: agent account".to_string())
    })
    .await?;
    if !profile.is_attached()
        || profile.generation != expected_generation
        || profile.auth_state != agent_account_store::AgentAccountAuthState::Ready
    {
        return Err("conflict: account is not attached and authentication-ready".into());
    }
    // Scheduler owns provider cadence. A CLI that reports on its status line has
    // none and therefore never launches merely to poll; a polled one has
    // visible/hidden intervals.
    if !polled(profile.provider) {
        return Ok(RefreshReply {
            outcome: RefreshOutcome::Cached,
            refresh_state: RefreshState::Idle,
        });
    }
    let state = app.state::<AgentUsageState>();
    if state
        .inner
        .poll_backoff
        .lock()
        .unwrap()
        .get(&account_id)
        .is_some_and(|(_, retry_at)| *retry_at > std::time::Instant::now())
    {
        return Ok(RefreshReply {
            outcome: RefreshOutcome::Cached,
            refresh_state: RefreshState::Backoff,
        });
    }
    let (outcome, refresh_state) = {
        let mut admission = state.inner.admission.lock().unwrap();
        let outcome = admission.request(&account_id, expected_generation);
        let refresh_state = admission.state(&account_id);
        (outcome, refresh_state)
    };
    if outcome == RefreshOutcome::Started {
        let cancellation = CancellationToken::new();
        state
            .inner
            .poll_cancellations
            .lock()
            .unwrap()
            .insert(account_id.clone(), cancellation.clone());
        let task_app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut current = profile;
            let mut cancellation = cancellation;
            'worker: loop {
                let result = refresh_polled(github_user_id, &current, &cancellation).await;
                let state = task_app.state::<AgentUsageState>();
                match result {
                    Ok(snapshot) => {
                        state.inner.poll_backoff.lock().unwrap().remove(&current.id);
                        let received_at = snapshot.usage_received_at;
                        tokio::time::sleep(scheduler::COALESCE_WINDOW).await;
                        let published = AgentUsageCoordinator::ingest(
                            &mut state.inner.coordinator.lock().unwrap(),
                            snapshot,
                            received_at,
                        )
                        .ok()
                        .flatten()
                        .cloned();
                        if let Some(published) = published {
                            enqueue_publish(
                                &task_app,
                                github_user_id,
                                state.inner.clone(),
                                published,
                            );
                        }
                    }
                    Err(_) if !cancellation.is_cancelled() => {
                        // A failed poll may mean the login is gone; look, without waiting
                        // for the account list to be opened.
                        super::agent_accounts::spawn_auth_recheck_after_poll_failure(
                            &task_app,
                            github_user_id,
                            &current.id,
                        );
                        let delay = {
                            let mut entries = state.inner.poll_backoff.lock().unwrap();
                            let (backoff, retry_at) =
                                entries.entry(current.id.clone()).or_insert_with(|| {
                                    (
                                        scheduler::RetryBackoff::default(),
                                        std::time::Instant::now(),
                                    )
                                });
                            let delay = backoff.fail();
                            *retry_at = std::time::Instant::now() + delay;
                            delay
                        };
                        let failed = state
                            .inner
                            .coordinator
                            .lock()
                            .unwrap()
                            .mark_refresh_failure(
                                &current.id,
                                current.provider,
                                current_epoch_seconds(),
                            );
                        if let Some(failed) = failed {
                            enqueue_publish(&task_app, github_user_id, state.inner.clone(), failed);
                        }
                        let wake = state.inner.clone();
                        tauri::async_runtime::spawn(async move {
                            tokio::time::sleep(delay).await;
                            wake.scheduler_wake.notify_one();
                        });
                    }
                    Err(_) => {}
                }
                state
                    .inner
                    .poll_cancellations
                    .lock()
                    .unwrap()
                    .remove(&current.id);
                let mut next = state.inner.admission.lock().unwrap().release(&current.id);
                let (profile, token) = loop {
                    let Some((next_id, queued_generation)) = next else {
                        break 'worker;
                    };
                    let queued_id = next_id.clone();
                    let queued_cancellation = CancellationToken::new();
                    state
                        .inner
                        .poll_cancellations
                        .lock()
                        .unwrap()
                        .insert(queued_id.clone(), queued_cancellation.clone());
                    let next_profile = await_blocking("refresh_agent_usage.dequeue", move || {
                        let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
                        agent_account_store::list_profiles(&root, &Principal::from(github_user_id))
                            .map_err(|error| error.to_string())?
                            .into_iter()
                            .find(|profile| {
                                profile.id == next_id
                                    && profile.generation == queued_generation
                                    && profile.is_attached()
                                    && polled(profile.provider)
                                    && profile.auth_state
                                        == agent_account_store::AgentAccountAuthState::Ready
                            })
                            .ok_or_else(|| "queued account is no longer eligible".to_string())
                    })
                    .await;
                    if let Ok(profile) = next_profile {
                        break (profile, queued_cancellation);
                    }
                    state
                        .inner
                        .poll_cancellations
                        .lock()
                        .unwrap()
                        .remove(&queued_id);
                    next = state.inner.admission.lock().unwrap().release(&queued_id);
                };
                current = profile;
                cancellation = token;
            }
        });
    }
    Ok(RefreshReply {
        outcome,
        refresh_state,
    })
}

async fn wait_for_publish_slot(inner: &AgentUsageInner, account_id: &str) {
    let delay = inner
        .last_publish
        .lock()
        .unwrap()
        .get(account_id)
        .and_then(|last| scheduler::PUBLISH_FLOOR.checked_sub(last.elapsed()));
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
    inner
        .last_publish
        .lock()
        .unwrap()
        .insert(account_id.to_string(), std::time::Instant::now());
}

/// Maintain a capacity-one pending slot per account. Bursty provider updates
/// replace an older pending value instead of accumulating stale publications.
fn enqueue_publish(
    app: &AppHandle,
    github_user_id: u64,
    inner: Arc<AgentUsageInner>,
    snapshot: AgentUsageSnapshot,
) {
    let account_id = snapshot.account_profile_id.clone();
    if !inner
        .publish_queue
        .lock()
        .unwrap()
        .enqueue(github_user_id, snapshot)
    {
        return;
    }
    let publish_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            wait_for_publish_slot(&inner, &account_id).await;
            let pending = inner.publish_queue.lock().unwrap().take(&account_id);
            let Some((github_user_id, snapshot)) = pending else {
                if inner
                    .publish_queue
                    .lock()
                    .unwrap()
                    .finish_or_continue(&account_id)
                {
                    continue;
                }
                break;
            };
            let _ = publish_app.emit(USAGE_EVENT, snapshot.clone());
            // Backoff is an ephemeral transport state. Keep the on-disk cache
            // as the last provider-confirmed value so a transient outage cannot
            // replace useful allowance data across a restart.
            if snapshot.refresh_state != RefreshState::Backoff {
                persist_snapshot_if_due(github_user_id, &inner, &snapshot).await;
            }
        }
    });
}

async fn persist_snapshot_if_due(
    github_user_id: u64,
    inner: &AgentUsageInner,
    snapshot: &AgentUsageSnapshot,
) {
    let due = inner
        .last_persist
        .lock()
        .unwrap()
        .get(&snapshot.account_profile_id)
        .is_none_or(|last| last.elapsed() >= scheduler::PERSIST_FLOOR);
    if !due {
        return;
    }
    let account_id = snapshot.account_profile_id.clone();
    let value = snapshot.clone();
    let persisted = await_blocking("persist_agent_usage", move || {
        let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
        let dir = crate::context_repo::path_to_user_home(&root, &Principal::from(github_user_id))
            .join("agent-usage");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        crate::content_engine::write_tmp_fsync_rename(
            &dir.join(format!("{}.json", value.account_profile_id)),
            &bytes,
        )
        .map_err(|e| e.to_string())
    })
    .await;
    if persisted.is_ok() {
        inner
            .last_persist
            .lock()
            .unwrap()
            .insert(account_id, std::time::Instant::now());
    }
}

pub fn cancel_account_work(app: &AppHandle, account_id: &str) {
    let state = app.state::<AgentUsageState>();
    if let Some(cancellation) = state
        .inner
        .poll_cancellations
        .lock()
        .unwrap()
        .get(account_id)
        .cloned()
    {
        cancellation.cancel();
    }
    state.inner.admission.lock().unwrap().cancel(account_id);
    state.inner.coordinator.lock().unwrap().remove(account_id);
    state.inner.last_publish.lock().unwrap().remove(account_id);
    state.inner.last_persist.lock().unwrap().remove(account_id);
    state.inner.publish_queue.lock().unwrap().remove(account_id);
    state.inner.poll_backoff.lock().unwrap().remove(account_id);
    state
        .inner
        .status_routes
        .lock()
        .unwrap()
        .retain(|_, (_, id, _)| id != account_id);
}

fn clear_status_socket_if_current(inner: &AgentUsageInner, failed_path: &std::path::Path) {
    let mut cached = inner.status_socket.lock().unwrap();
    if cached.as_deref() == Some(failed_path) {
        *cached = None;
    }
}

/// Register an account whose CLI reports usage on its status line with the single
/// per-process datagram receiver.
/// The returned capability and socket path stay backend/provider-home only.
pub fn register_status_line(
    app: &AppHandle,
    github_user_id: u64,
    account_id: &str,
    provider: AgentProvider,
) -> Result<(std::path::PathBuf, String), String> {
    use std::os::unix::net::UnixDatagram;
    if !matches!(usage_of(provider), Usage::StatusLine { .. }) {
        return Err("this account's CLI reports no usage on a status line".into());
    }
    let state = app.state::<AgentUsageState>();
    let capability = crate::http::random_hex_secret();
    state.inner.status_routes.lock().unwrap().insert(
        capability.clone(),
        (github_user_id, account_id.to_string(), provider),
    );
    if let Some(path) = state.inner.status_socket.lock().unwrap().clone() {
        return Ok((path, capability));
    }
    let runtime = codesense_home_dir()
        .ok_or("cannot resolve home directory")?
        .join("runtime")
        .join(format!("agent-usage-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let socket_path = runtime.join("status-line.sock");
    let _ = std::fs::remove_file(&socket_path);
    let socket = UnixDatagram::bind(&socket_path).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .map_err(|e| e.to_string())?;
    *state.inner.status_socket.lock().unwrap() = Some(socket_path.clone());
    let inner = state.inner.clone();
    let task_app = app.clone();
    let receiver_path = socket_path.clone();
    std::thread::Builder::new()
        .name("status-line-usage-datagram".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_ADAPTER_MESSAGE + 128];
            loop {
                let count = match socket.recv(&mut buffer) {
                    Ok(count) => count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue
                    }
                    Err(_) => {
                        clear_status_socket_if_current(&inner, &receiver_path);
                        let _ = std::fs::remove_file(&receiver_path);
                        break;
                    }
                };
                let Some(split) = buffer[..count].iter().position(|byte| *byte == b'\n') else {
                    continue;
                };
                let Ok(cap) = std::str::from_utf8(&buffer[..split]) else {
                    continue;
                };
                let Some((github_user_id, account_id, provider)) =
                    inner.status_routes.lock().unwrap().get(cap).cloned()
                else {
                    continue;
                };
                let Usage::StatusLine { source, parse } = usage_of(provider) else {
                    continue;
                };
                let payload = &buffer[split + 1..count];
                if payload.len() > MAX_ADAPTER_MESSAGE {
                    continue;
                }
                let now = current_epoch_seconds();
                let Ok(Some(windows)) = parse(payload) else {
                    continue;
                };
                let Ok(snapshot) = usage_snapshot(&account_id, provider, source, windows, now)
                else {
                    continue;
                };
                let published = AgentUsageCoordinator::ingest(
                    &mut inner.coordinator.lock().unwrap(),
                    snapshot,
                    now,
                )
                .ok()
                .flatten()
                .cloned();
                if let Some(snapshot) = published {
                    enqueue_publish(&task_app, github_user_id, inner.clone(), snapshot);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok((socket_path, capability))
}

/// How CodeSense learns `provider`'s accounts' allowance windows: through the CLI its
/// logins run.
fn usage_of(provider: AgentProvider) -> Usage {
    adapter(cli_of_provider(provider)).output().usage
}

/// Whether CodeSense asks `provider`'s CLI for its accounts' usage.
fn polled(provider: AgentProvider) -> bool {
    matches!(usage_of(provider), Usage::Poll(_))
}

/// `account_profile_id`'s snapshot of the allowance `windows` its CLI reported through
/// `source`.
fn usage_snapshot(
    account_profile_id: &str,
    provider: AgentProvider,
    source: &str,
    windows: Vec<UsageWindow>,
    received_at: i64,
) -> Result<AgentUsageSnapshot, String> {
    let allowance_windows = windows
        .into_iter()
        .map(|window| match window.used_percent {
            Some(used) => {
                AllowanceWindow::new(window.id, window.label, used, window.resets_at, true)
            }
            None => AllowanceWindow::unknown(window.id, window.label, true),
        })
        .collect::<Result<_, _>>()?;
    AgentUsageSnapshot {
        account_profile_id: account_profile_id.into(),
        provider,
        availability: AgentAvailability::Unknown,
        allowance_windows,
        sequence: 0,
        usage_received_at: received_at,
        source: source.into(),
        usage_freshness: UsageFreshness::Live,
        refresh_state: RefreshState::Idle,
    }
    .normalize()
}

/// Asks `profile`'s CLI for its allowance windows through its usage poll, in the
/// account's own home.
async fn refresh_polled(
    github_user_id: u64,
    profile: &agent_account_store::AgentAccountProfile,
    cancellation: &CancellationToken,
) -> Result<AgentUsageSnapshot, String> {
    let root = codesense_home_dir().ok_or("cannot resolve home directory")?;
    let home =
        agent_account_store::managed_home(&root, &Principal::from(github_user_id), &profile.id)
            .map_err(|error| error.to_string())?;
    let binary = descriptor(cli_of_provider(profile.provider)).binary.clone();
    refresh_polled_in(&binary, profile, &home, cancellation).await
}

/// [`refresh_polled`] with the CLI's `binary` and the account's `home` given.
async fn refresh_polled_in(
    binary: &str,
    profile: &agent_account_store::AgentAccountProfile,
    home: &std::path::Path,
    cancellation: &CancellationToken,
) -> Result<AgentUsageSnapshot, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    let descriptor = descriptor(cli_of_provider(profile.provider));
    let name = descriptor.name.as_str();
    let Usage::Poll(poll) = usage_of(profile.provider) else {
        return Err(format!("{name} reports its usage itself"));
    };
    if cancellation.is_cancelled() {
        return Err(format!("{name} usage refresh cancelled"));
    }
    let mut command = Command::new(binary);
    command
        .args(poll.argv)
        .env(descriptor.home_env.as_str(), home);
    super::agent_accounts::scrub_account_env(&mut command, descriptor);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("{name} CLI unavailable: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{name} usage stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{name} usage stdout unavailable"))?;
    stdin
        .write_all(poll.requests.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())?;
    let response = tokio::select! {
        _ = cancellation.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!("{name} usage refresh cancelled"));
        }
        response = timeout(Duration::from_secs(10), async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            if line.len() > MAX_ADAPTER_MESSAGE {
                return Err::<serde_json::Value, String>(format!(
                    "{name} usage message exceeds 64 KiB"
                ));
            }
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if value.get("id").and_then(|id| id.as_u64()) == Some(poll.reply_id) {
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| format!("{name} usage reply has no result"));
            }
        }
        Err::<serde_json::Value, String>(format!("{name} closed before replying"))
        }) => response
            .map_err(|_| format!("{name} usage request timed out"))??,
    };
    let _ = child.kill().await;
    let _ = child.wait().await;
    let windows = (poll.parse)(&response)?;
    usage_snapshot(
        &profile.id,
        profile.provider,
        poll.source,
        windows,
        current_epoch_seconds(),
    )
}

fn current_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn queued_snapshot(sequence: u64) -> AgentUsageSnapshot {
        AgentUsageSnapshot {
            account_profile_id: "account".into(),
            provider: agent_account_store::AgentProvider::Codex,
            availability: AgentAvailability::Unknown,
            allowance_windows: Vec::new(),
            sequence,
            usage_received_at: sequence as i64,
            source: "test".into(),
            usage_freshness: UsageFreshness::Live,
            refresh_state: RefreshState::Idle,
        }
    }

    #[test]
    fn a_failed_status_line_receiver_clears_only_its_cached_socket() {
        let inner = AgentUsageInner::default();
        let active = std::path::PathBuf::from("/tmp/active-status-line.sock");
        *inner.status_socket.lock().unwrap() = Some(active.clone());

        clear_status_socket_if_current(&inner, std::path::Path::new("/tmp/old.sock"));
        assert_eq!(*inner.status_socket.lock().unwrap(), Some(active.clone()));

        clear_status_socket_if_current(&inner, &active);
        assert!(inner.status_socket.lock().unwrap().is_none());
    }

    #[test]
    fn cx4_real_and_unknown_windows_flow_through_the_existing_coordinator() {
        let windows = vec![
            UsageWindow {
                id: "primary".into(),
                label: "Primary".into(),
                used_percent: Some(0.0),
                resets_at: None,
            },
            UsageWindow {
                id: "secondary".into(),
                label: "Secondary".into(),
                used_percent: None,
                resets_at: None,
            },
        ];
        let snapshot = usage_snapshot(
            "profile",
            AgentProvider::Codex,
            "codex_app_server",
            windows,
            100,
        )
        .unwrap();
        assert_eq!(snapshot.availability, AgentAvailability::Unknown);
        let mut coordinator = coordinator::AgentUsageCoordinator::default();
        let published = coordinator.ingest(snapshot, 100).unwrap().unwrap();
        let json = serde_json::to_value(published).unwrap();
        assert_eq!(
            json["allowanceWindows"][0]["usedPercentage"],
            serde_json::json!(0.0)
        );
        assert!(json["allowanceWindows"][1]["usedPercentage"].is_null());
        assert!(json["allowanceWindows"][1]["remainingPercentage"].is_null());
        assert_eq!(json["availability"], "unknown");
    }

    #[test]
    fn reported_windows_become_a_normalized_snapshot_of_the_account() {
        let window = |id: &str, used| UsageWindow {
            id: id.into(),
            label: id.into(),
            used_percent: Some(used),
            resets_at: None,
        };
        let snapshot = usage_snapshot(
            "a",
            AgentProvider::Codex,
            "codex_app_server",
            vec![window("b", 100.0), window("a", 25.0)],
            100,
        )
        .unwrap();
        assert_eq!(snapshot.availability, AgentAvailability::Exhausted);
        assert_eq!(snapshot.allowance_windows[0].key, "a");
        assert_eq!(snapshot.source, "codex_app_server");
        assert!(
            usage_snapshot("a", AgentProvider::Claude, "s", vec![window("a", 101.0)], 0).is_err()
        );
    }

    #[test]
    fn publish_queue_keeps_only_the_latest_pending_value() {
        let mut queue = LatestPublishQueue::default();
        assert!(queue.enqueue(1, queued_snapshot(1)));
        assert!(!queue.enqueue(1, queued_snapshot(2)));
        assert_eq!(queue.take("account").unwrap().1.sequence, 2);
        assert!(!queue.finish_or_continue("account"));
        assert!(queue.enqueue(1, queued_snapshot(3)));
    }

    /// The usage poll runs the CLI in the account's home without the credentials that CLI would
    /// rank above that home's login, so it reports the account's own allowance.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_poll_runs_the_real_cli_in_the_account_home_without_exported_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let record = dir.path().join("record");
        let bin = dir.path().join("fake-codex.sh");
        let Usage::Poll(poll) = usage_of(agent_account_store::AgentProvider::Codex) else {
            panic!("Codex is polled");
        };
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s' \"$CODEX_API_KEY|$OPENAI_API_KEY|$CODEX_HOME\" > '{}'\nread line\necho '{{\"id\":{},\"result\":{{}}}}'\n",
                record.display(),
                poll.reply_id
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["CODEX_API_KEY", "OPENAI_API_KEY"] {
            std::env::set_var(name, "exported-secret");
        }
        let profile = agent_account_store::AgentAccountProfile {
            schema_version: agent_account_store::SCHEMA_VERSION,
            id: "poll-account".into(),
            provider: agent_account_store::AgentProvider::Codex,
            label: "Poll".into(),
            auth_state: agent_account_store::AgentAccountAuthState::Ready,
            auth_checked_at: None,
            auth_error: None,
            identity: None,
            status_line: None,
            generation: 1,
            created_at: 1,
            updated_at: 1,
            detached_at: None,
            pending_lifecycle_events: Vec::new(),
        };
        let _ = refresh_polled_in(
            &bin.to_string_lossy(),
            &profile,
            &home,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("||{}", home.display())
        );
        for name in ["CODEX_API_KEY", "OPENAI_API_KEY"] {
            std::env::remove_var(name);
        }
    }
}
