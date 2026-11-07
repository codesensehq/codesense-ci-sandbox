// SPDX-License-Identifier: MIT
//! SW-XS-4: nothing adopts worktree edits into team content automatically.
//!
//! Each test drives a stimulus that SURVIVES the removal — a real file edit under
//! a real watcher, the window-focus handler, the OSC-133 `CommandEnd` handler,
//! the workspace-open command, team activation (the team main-clone watch), and
//! the app's sync timers under an injected (paused tokio) clock advanced past the
//! slowest 300 s backstop — against a live worktree whose projected file carries a
//! genuine local edit, with a registered `WaveCoordinator` and the wave intake
//! probe (the enqueue counter). Each asserts the team content is byte-unchanged,
//! zero waves were enqueued for the worktree, and no `conflict-detected` event
//! fired. Before SW-XS-4 every one of these stimuli reverse-synced the edit (the
//! red run switched the old reverse default to `Live` so the old code was at its
//! most eager; that setting no longer exists).
//!
//! The explicit path is the control: `reverse_sync_worktree`'s legacy form still
//! publishes the edit.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{Listener, Manager};

use crate::content_engine::layers::LayerId;
use crate::content_engine::lockfile::{Lockfile, MaterializedEntry};
use crate::content_engine::sync::wave::intake_probe;
use crate::content_engine::sync::WaveCoordinator;
use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
use crate::content_engine::ResolverContext;
use crate::domain::{Principal, SourceRepoSlug};

const USER_ID: u64 = 42;
const ORG: &str = "acme/api";
const TEAM: &str = "default";
const LOGICAL: &str = "_bmad-output/impl/epic-1/story.md";
const SOURCE_BYTES: &[u8] = b"team content\n";
const EDIT_BYTES: &[u8] = b"local worktree edit\n";

struct Fixture {
    // Field order is drop order: the app goes before `$HOME` is restored.
    app: tauri::App<tauri::test::MockRuntime>,
    _home_env: HomeEnv,
    _home: tempfile::TempDir,
    _worktree: tempfile::TempDir,
    home: PathBuf,
    _ctx: ResolverContext,
    slug: SourceRepoSlug,
    content_dir: PathBuf,
    worktree: PathBuf,
    source_path: PathBuf,
    dest_path: PathBuf,
    conflict_events: Arc<AtomicUsize>,
    context_events: Arc<AtomicUsize>,
}

/// Points `$HOME` at the fixture (the app resolves `~/.codesense` from it) under
/// the process env lock, restoring it on drop.
struct HomeEnv {
    _env: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl HomeEnv {
    fn set(root: &Path) -> Self {
        let env = crate::content_engine::test_support::env_guard();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", root);
        Self {
            _env: env,
            previous,
        }
    }
}

impl Drop for HomeEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn canonical(dir: &tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(dir.path()).unwrap()
}

fn backdate(path: &Path) {
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

/// A live legacy worktree of the active team: the team source holds
/// `SOURCE_BYTES`, the worktree's projected copy was materialized from it and then
/// edited locally to `EDIT_BYTES` (source stable → before SW-XS-4, a clean
/// promotion candidate for every reverse trigger).
fn fixture() -> Fixture {
    let home_dir = tempfile::TempDir::new().unwrap();
    let worktree_dir = tempfile::TempDir::new().unwrap();
    let home_env = HomeEnv::set(&canonical(&home_dir));
    let home = crate::commands::paths::codesense_home_dir().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let worktree = canonical(&worktree_dir);
    let ctx = ResolverContext {
        context_root: None,
        codesense_home: home.clone(),
        user_id: USER_ID,
        org_slug: ORG.to_string(),
        team_name: Some(TEAM.to_string()),
    };
    let slug = SourceRepoSlug::new("alderco", "codesense").unwrap();
    let org_root = home.join("users/42/orgs/acme/api");
    std::fs::create_dir_all(&org_root).unwrap();
    std::fs::write(org_root.join("org.yml"), "name: api\n").unwrap();
    let content_dir = org_root.join(TEAM).join("content");
    let source_path = content_dir.join(LOGICAL);
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, SOURCE_BYTES).unwrap();

    let dest_path = worktree.join(LOGICAL);
    std::fs::create_dir_all(dest_path.parent().unwrap()).unwrap();
    std::fs::write(&dest_path, EDIT_BYTES).unwrap();
    backdate(&dest_path);

    let lock = Lockfile {
        schema_version: 3,
        org_slug: ctx.org_slug.clone(),
        team_name: ctx.team_name.clone(),
        code_repo_path: worktree.clone(),
        entries: vec![MaterializedEntry {
            last_materialized_size: None,
            logical_name: LOGICAL.to_string(),
            dest_path: dest_path.clone(),
            source_layer: LayerId::TeamProjection,
            source_path: source_path.clone(),
            last_materialized_hash: *blake3::hash(SOURCE_BYTES).as_bytes(),
            last_materialized_mtime_ns: 0,
            projection_key: Some("_bmad-output".to_string()),
            source_oid: None,
        }],
    };
    SyncStateStore::open(default_store_path(&home))
        .unwrap()
        .upsert(USER_ID, &slug, &lock)
        .unwrap();
    // The worktree is a codesense-managed clone of the team's source repo (the
    // write guard only lets the sync engine touch managed targets).
    let attachments =
        crate::context_repo::path_to_user_attachments_file(&home, &Principal::from(USER_ID));
    std::fs::create_dir_all(attachments.parent().unwrap()).unwrap();
    crate::context_repo::attachments_io::write_attachments_atomic(
        &attachments,
        &crate::domain::Attachments {
            attachments: vec![crate::domain::Attachment {
                repo_path: worktree.to_string_lossy().into_owned(),
                remote_url: Some("git@github.com:alderco/codesense.git".to_string()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: true,
            }],
        },
    )
    .unwrap();
    // The story is indexed, so it is in the team's wanted set.
    let mut cache = crate::install::frontmatter_cache::FrontmatterCache::new();
    cache.update_entry(
        source_path.clone(),
        crate::install::frontmatter_cache::CacheEntry {
            path: source_path.clone(),
            dir_team: TEAM.to_string(),
            fields: crate::commands::pipeline_format::FrontmatterFields {
                team: Some(TEAM.to_string()),
                source_repo: Some("alderco/codesense".to_string()),
                status: Some("in-progress".to_string()),
                ..Default::default()
            },
            metadata_parse_error: None,
            open_tags: std::collections::BTreeMap::new(),
            mtime: 1,
        },
    );

    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(crate::commands::watch::WatcherState::default());
    app.manage(crate::install::frontmatter_cache::FrontmatterCache::shared(
        cache,
    ));
    app.manage(crate::commands::local_profile::LocalProfileState::for_test(
        &home,
        USER_ID,
        crate::commands::local_profile::MachineOwner::LegacyGithub { id: USER_ID },
    ));
    let coord = Arc::new(WaveCoordinator::new());
    coord.stash_active_projection_on_team_change(&ctx, std::slice::from_ref(&slug));
    app.manage(coord);

    let conflict_events = Arc::new(AtomicUsize::new(0));
    {
        let seen = Arc::clone(&conflict_events);
        app.listen_any("conflict-detected", move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
    }
    let context_events = Arc::new(AtomicUsize::new(0));
    {
        let seen = Arc::clone(&context_events);
        app.listen_any("context-file-change", move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
    }

    Fixture {
        app,
        _home_env: home_env,
        _home: home_dir,
        _worktree: worktree_dir,
        home,
        _ctx: ctx,
        slug,
        content_dir,
        worktree,
        source_path,
        dest_path,
        conflict_events,
        context_events,
    }
}

impl Fixture {
    fn handle(&self) -> tauri::AppHandle<tauri::test::MockRuntime> {
        self.app.handle().clone()
    }

    fn principal(&self) -> String {
        Principal::from(USER_ID).as_str().to_string()
    }

    fn watch_config(&self) -> crate::commands::watch::WatchConfig {
        crate::commands::watch::WatchConfig {
            repo_path: self.worktree.to_string_lossy().into_owned(),
            context_repo_path: self.content_dir.to_string_lossy().into_owned(),
        }
    }

    /// Positive control: keep editing a team content file until the watcher
    /// delivers `context-file-change` (FSEvents can drop or coalesce the first
    /// write right after a watch is attached, and is slow under load).
    async fn await_control_event(&self) {
        let control = self.content_dir.join("_bmad-output/control.md");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut n = 0u32;
        while self.context_events.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            n += 1;
            std::fs::write(&control, format!("control {n}")).unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    fn intake(&self) -> usize {
        intake_probe::count(&self.worktree)
    }

    /// Wait out `window` (returning early the moment anything is enqueued or the
    /// team source changes), then assert the stimulus adopted nothing.
    fn assert_nothing_adopted(&self, trigger: &str, window: Duration) {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            if self.intake() > 0 || std::fs::read(&self.source_path).unwrap() != SOURCE_BYTES {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            self.intake(),
            0,
            "{trigger}: a wave was enqueued for the worktree — an automatic reverse trigger survived"
        );
        assert_eq!(
            std::fs::read(&self.source_path).unwrap(),
            SOURCE_BYTES,
            "{trigger}: the worktree edit was adopted into team content"
        );
        assert_eq!(
            std::fs::read(&self.dest_path).unwrap(),
            EDIT_BYTES,
            "{trigger}: the worktree edit itself was touched"
        );
        assert_eq!(
            self.conflict_events.load(Ordering::SeqCst),
            0,
            "{trigger}: a conflict-detected event fired"
        );
    }
}

/// R1: a real edit in the worktree, observed by the real watcher. The positive
/// control (a team content edit delivering `context-file-change`) proves the
/// watcher is live, so the absence of a reverse enqueue is not a dead watcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_worktree_edit_under_live_watcher_adopts_nothing() {
    let fx = fixture();
    crate::commands::watch::start_watching(
        fx.handle(),
        Some(fx.principal()),
        vec![fx.watch_config()],
        1,
    )
    .await
    .expect("start the real watcher");

    // The stimulus: an editor/agent writes the projected file in the worktree…
    std::fs::write(&fx.dest_path, b"second local edit\n").unwrap();
    // …and a brand-new file under the projected prefix.
    std::fs::write(fx.dest_path.with_file_name("new-story.md"), b"new\n").unwrap();
    // Positive control: a team content edit reaches the same watcher.
    fx.await_control_event().await;
    assert!(
        fx.context_events.load(Ordering::SeqCst) > 0,
        "control: the watcher never delivered the team content edit"
    );
    std::fs::write(&fx.dest_path, EDIT_BYTES).unwrap();
    tokio::task::block_in_place(|| {
        fx.assert_nothing_adopted("R1 watcher", Duration::from_millis(1500));
    });
}

/// R2/R3/R4: the app's sync timers (drain loop, entry sweep, new-file sweep)
/// under an injected clock advanced well past the slowest 300 s idle backstop.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn r2_r3_r4_timers_past_300s_adopt_nothing() {
    let fx = fixture();
    // A brand-new file under the projected prefix, for the new-file sweep.
    std::fs::write(fx.dest_path.with_file_name("new-story.md"), b"new\n").unwrap();
    crate::spawn_sync_timers(&fx.handle());
    for _ in 0..12 {
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        // Let any blocking sweep the tick spawned run on its real thread.
        std::thread::sleep(Duration::from_millis(40));
        tokio::task::yield_now().await;
        if fx.intake() > 0 {
            break;
        }
    }
    fx.assert_nothing_adopted("R2-R4 timers (360 s)", Duration::from_millis(200));
}

/// R5: the window regains focus.
#[test]
fn r5_focus_regain_adopts_nothing() {
    let fx = fixture();
    crate::on_window_focus_changed(&fx.handle(), false);
    crate::on_window_focus_changed(&fx.handle(), true);
    fx.assert_nothing_adopted("R5 focus regain", Duration::from_millis(1500));
}

/// R6: a terminal command finishes (OSC-133 `CommandEnd`) inside the worktree.
#[test]
fn r6_osc133_command_end_adopts_nothing() {
    let fx = fixture();
    crate::on_osc133_command_end(&fx.handle(), "session-in-the-worktree");
    fx.assert_nothing_adopted("R6 OSC-133", Duration::from_millis(1500));
}

/// R7: the workspace is opened (`lazy_attach_workspace`, the FE's open command).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r7_workspace_open_adopts_nothing() {
    let fx = fixture();
    crate::commands::watch::start_watching(fx.handle(), Some(fx.principal()), Vec::new(), 1)
        .await
        .expect("start the real watcher");
    crate::commands::watch::lazy_attach_workspace(
        fx.handle(),
        Some(fx.principal()),
        fx.worktree.to_string_lossy().into_owned(),
        "alderco/codesense".to_string(),
        fx.watch_config(),
    )
    .await
    .expect("open the workspace");
    tokio::task::block_in_place(|| {
        fx.assert_nothing_adopted("R7 workspace open", Duration::from_millis(1500));
    });
}

/// R9: the team's main clone (attached source repo) is edited while the team is
/// active — team activation used to put an always-on reverse watch on it.
/// Driven through the real `set_active_team_for_wave` (the team content watch it
/// attaches is the positive control).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r9_team_main_clone_edit_adopts_nothing() {
    let fx = fixture();
    // The fixture's worktree doubles as the team's attached main clone.
    crate::commands::watch::start_watching(fx.handle(), Some(fx.principal()), Vec::new(), 1)
        .await
        .expect("start the real watcher");
    // The real team switch (the FE's `set_active_team_for_wave`).
    fx.app.manage(crate::mcp::state::ActiveTeamState::default());
    crate::commands::content_engine::set_active_team_for_wave(
        fx.handle(),
        fx.home.to_string_lossy().into_owned(),
        USER_ID,
        ORG.to_string(),
        TEAM.to_string(),
        vec![fx.slug.clone()],
    )
    .await
    .expect("activate the team");

    std::fs::write(&fx.dest_path, b"main clone edit\n").unwrap();
    fx.await_control_event().await;
    assert!(
        fx.context_events.load(Ordering::SeqCst) > 0,
        "control: the team content watch never delivered an edit"
    );
    std::fs::write(&fx.dest_path, EDIT_BYTES).unwrap();
    tokio::task::block_in_place(|| {
        fx.assert_nothing_adopted("R9 team main clone", Duration::from_millis(1500));
    });
}

/// R11: a both-diverged file (team source AND worktree edited since the last
/// sync) under every surviving stimulus: no conflict row is recorded and no
/// `conflict-detected` event fires — nothing even looks at it.
#[test]
fn r11_diverged_file_under_every_stimulus_raises_no_conflict() {
    let fx = fixture();
    std::fs::write(&fx.source_path, b"team edited too\n").unwrap();
    crate::on_window_focus_changed(&fx.handle(), true);
    crate::on_osc133_command_end(&fx.handle(), "session-in-the-worktree");
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline && fx.intake() == 0 {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(fx.intake(), 0, "R11: a stimulus enqueued a wave");
    assert_eq!(
        fx.conflict_events.load(Ordering::SeqCst),
        0,
        "R11: a conflict-detected event fired"
    );
    assert_eq!(
        std::fs::read(&fx.source_path).unwrap(),
        b"team edited too\n"
    );
    assert_eq!(std::fs::read(&fx.dest_path).unwrap(), EDIT_BYTES);
}
