// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::agent_state::SessionTurnState;
use crate::commands::dashboard_launch::{session_row, DashboardLaunchState};
use crate::domain::Principal;
use crate::mcp::session_list::{session_list_rows, SessionListRow};

const ORG: &str = "acme/widgets-context";

pub(crate) type RegisteredSpawnProbe =
    std::sync::Arc<dyn Fn(&Launch, &str) -> Result<(), String> + Send + Sync>;

/// The [`OpenHost`] of the tests: the REAL launch registry (so reserve, idempotent
/// focus, the spawn cap and launcher normalization are the shipped code) and the real
/// worker-dir staging on a temp home; only the PTY spawn is replaced by a bind to a
/// minted session id.
struct FakeHost {
    registry: DashboardLaunchState,
    home: PathBuf,
    principal: Principal,
    content_roots: HashMap<String, PathBuf>,
    /// Story id → the org and team the story belongs to.
    story_teams: HashMap<String, ActiveTeam>,
    /// The org hint of every story lookup, in order.
    story_org_hints: Mutex<Vec<Option<String>>>,
    /// When set, every story lookup fails with this reason.
    story_error: Option<String>,
    /// Every `(org, team)` a content root was looked up for.
    lookups: Mutex<Vec<(String, String)>>,
    /// When set, the spine fails every launch with this error.
    launch_error: Mutex<Option<LaunchError>>,
    /// Pause the next spawn after its destination has been prepared.
    launch_gate: Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), LaunchError>>>>,
    startup_submit: Mutex<codesense_pty_core::domain::SubmitOutcome>,
    launches: Mutex<Vec<Launch>>,
    reveals: Mutex<Vec<(String, bool)>>,
    next_session: AtomicUsize,
    /// How many times a worker dir was staged.
    stagings: AtomicUsize,
    /// When set, the next staging waits on it before writing (holds an open mid-stage).
    stage_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    claims: OpenClaims,
    /// Stories whose worktree a story launch has created and set up.
    story_worktrees: Mutex<std::collections::HashSet<String>>,
    /// How many story worktrees were created (and set up).
    worktree_creations: AtomicUsize,
    /// Story id → where its workspace lives.
    places: Mutex<HashMap<String, WorkspacePlace>>,
    /// Every launch on a machine.
    remote_launches: Mutex<Vec<RemoteLaunch>>,
    /// When set, a launch on a machine answers it as already running there.
    remote_already_live: Mutex<Option<String>>,
    /// When set, a launch on a machine fails with this reason.
    remote_error: Mutex<Option<String>>,
    /// What showing an already-running session does (its tab, unless a test says otherwise).
    show_outcome: Mutex<RevealOutcome>,
    /// When set, what changes every agent this host prepares.
    prepared: Mutex<Option<fn(&mut AgentLaunch)>>,
    registered_test_launch: Mutex<Option<AgentLaunch>>,
    registered_spawn_probe: Mutex<Option<RegisteredSpawnProbe>>,
}

impl FakeHost {
    fn new(home: &Path, cap: usize) -> Self {
        let content_root = home.join("context-repo/default/content");
        std::fs::create_dir_all(content_root.join(".claude/commands")).unwrap();
        FakeHost {
            registry: DashboardLaunchState::new(cap),
            home: home.to_path_buf(),
            principal: Principal::from(42u64),
            content_roots: HashMap::from([("default".to_string(), content_root)]),
            // Every open launches an agent prepared in its story's own team.
            story_teams: ["SW-1", "SW-9", "SW-COLD"]
                .into_iter()
                .map(|story| (story.to_string(), active()))
                .collect(),
            story_org_hints: Mutex::new(Vec::new()),
            story_error: None,
            lookups: Mutex::new(Vec::new()),
            launch_error: Mutex::new(None),
            launch_gate: Mutex::new(None),
            startup_submit: Mutex::new(codesense_pty_core::domain::SubmitOutcome {
                submitted: true,
            }),
            launches: Mutex::new(Vec::new()),
            reveals: Mutex::new(Vec::new()),
            next_session: AtomicUsize::new(1),
            stagings: AtomicUsize::new(0),
            stage_gate: Mutex::new(None),
            claims: OpenClaims::default(),
            story_worktrees: Mutex::new(std::collections::HashSet::new()),
            worktree_creations: AtomicUsize::new(0),
            places: Mutex::new(HashMap::new()),
            remote_launches: Mutex::new(Vec::new()),
            remote_already_live: Mutex::new(None),
            remote_error: Mutex::new(None),
            prepared: Mutex::new(None),
            registered_test_launch: Mutex::new(None),
            registered_spawn_probe: Mutex::new(None),
            show_outcome: Mutex::new(RevealOutcome::Shown(ShownAs::Tab)),
        }
    }

    fn remote_launches(&self) -> Vec<RemoteLaunch> {
        self.remote_launches.lock().unwrap().clone()
    }

    fn place(&self, story: &str, place: WorkspacePlace) {
        self.places.lock().unwrap().insert(story.to_string(), place);
    }

    fn content_root(&self) -> &Path {
        &self.content_roots["default"]
    }

    fn launches(&self) -> Vec<Launch> {
        self.launches.lock().unwrap().clone()
    }

    fn reveals(&self) -> Vec<(String, bool)> {
        self.reveals.lock().unwrap().clone()
    }

    fn lookups(&self) -> Vec<(String, String)> {
        self.lookups.lock().unwrap().clone()
    }

    fn worker(&self, slug: &str) -> PathBuf {
        crate::context_repo::path_to_user_home(&self.home, &self.principal)
            .join("context-worktrees")
            .join(ORG)
            .join("default")
            .join(slug)
            .join("default/content")
    }

    fn session_of(&self, handle: &str) -> String {
        self.registry.resolve(handle).expect("handle is bound")
    }

    /// `list_sessions` over this registry, every session alive.
    fn list(&self, turns: &SessionTurnState, launched_by: Option<&str>) -> Vec<SessionListRow> {
        let rows = self
            .registry
            .snapshot()
            .into_iter()
            .map(|s| session_row(s, true))
            .collect();
        let by = launched_by.map(|raw| self.registry.normalize_launched_by(raw));
        session_list_rows(rows, |sid| turns.last(sid), by.as_deref())
    }
}

#[async_trait::async_trait]
impl OpenHost for FakeHost {
    async fn story_workspace_place(
        &self,
        _team: &ActiveTeam,
        story_id: &str,
    ) -> Option<WorkspacePlace> {
        self.places.lock().unwrap().get(story_id).cloned()
    }

    async fn launch_remote(&self, launch: RemoteLaunch) -> Result<RemoteLaunched, String> {
        if let Some(error) = self.remote_error.lock().unwrap().clone() {
            return Err(error);
        }
        let machine = launch.machine_id.to_uppercase();
        self.remote_launches.lock().unwrap().push(launch);
        if let Some(handle) = self.remote_already_live.lock().unwrap().clone() {
            return Ok(RemoteLaunched {
                handle,
                fresh: false,
                machine,
            });
        }
        let n = self.next_session.fetch_add(1, Ordering::SeqCst);
        Ok(RemoteLaunched {
            handle: format!("remote-tab-{n}"),
            fresh: true,
            machine,
        })
    }

    async fn story_team(&self, story_id: &str, org: Option<&str>) -> Result<ActiveTeam, String> {
        self.story_org_hints
            .lock()
            .unwrap()
            .push(org.map(str::to_string));
        if let Some(error) = &self.story_error {
            return Err(error.clone());
        }
        // An org hint looks only in that org.
        self.story_teams
            .get(story_id)
            .filter(|team| org.is_none_or(|org| team.org == org))
            .cloned()
            .ok_or_else(|| format!("unknown story {story_id:?}"))
    }

    fn is_live(&self, handle: &str) -> bool {
        self.registry.has_handle(handle)
    }

    async fn claim_open(&self, handle: &str) -> OpenClaim {
        self.claims.claim(handle).await
    }

    async fn team_content_root(&self, org: &str, team: &str) -> Result<PathBuf, String> {
        self.lookups
            .lock()
            .unwrap()
            .push((org.to_string(), team.to_string()));
        self.content_roots
            .get(team)
            .cloned()
            .ok_or_else(|| format!("unknown team; no content for team '{team}'"))
    }

    async fn prepare_context_worktree(
        &self,
        scope: &ActiveTeam,
        content: PathBuf,
        name: &str,
        requested: Option<&str>,
        create: bool,
        agent: &AgentLaunch,
    ) -> Result<context::ContextDestination, String> {
        let gate = self.stage_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        self.stagings.fetch_add(1, Ordering::SeqCst);
        if crate::content_engine::git_worktree::GitWorktree::discover(&content)?.is_none() {
            let root = content.parent().unwrap().parent().unwrap();
            // Keep the test fixture's checkout shape identical to the production context repo.
            let team_content = root.join(&scope.team).join("content");
            if team_content != content {
                std::fs::create_dir_all(&team_content).unwrap();
                for e in walkdir::WalkDir::new(&content)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file())
                {
                    let dest = team_content.join(e.path().strip_prefix(&content).unwrap());
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::copy(e.path(), dest).unwrap();
                }
            }
            if !team_content.join("README.md").exists() {
                std::fs::write(team_content.join("README.md"), "team fixture").unwrap();
            }
            for args in [
                vec!["init", "-b", "main"],
                vec!["config", "user.name", "Test"],
                vec!["config", "user.email", "test@example.test"],
                vec!["add", "."],
                vec!["commit", "--allow-empty", "-m", "fixture"],
            ] {
                let out = std::process::Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
        AppOpenHost::prepare_context_at(
            &self.home,
            &self.principal,
            scope,
            &content,
            name,
            requested,
            create,
            agent,
            &|_| {},
        )
    }
    async fn rollback_context_worktree(&self, destination: &context::ContextDestination) {
        context::rollback(&self.home, &self.principal, destination);
    }

    async fn prepare_agent(
        &self,
        reference: &str,
        team: &ActiveTeam,
        prompt: Option<&str>,
        mode: Option<adapter::LaunchMode>,
    ) -> Result<AgentLaunch, String> {
        if let Some(agent) = self.registered_test_launch.lock().unwrap().clone() {
            assert_eq!(
                reference,
                agent.agent.key.to_string(),
                "registered fixture selection"
            );
            return Ok(agent);
        }
        let mut agent = open_workspace_prepare(
            &self.home,
            &self.principal,
            reference,
            Some((team.org.clone(), team.team.clone())),
            prompt,
            mode,
        )?;
        if let Some(change) = *self.prepared.lock().unwrap() {
            change(&mut agent);
        }
        Ok(agent)
    }

    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, LaunchError> {
        let gate = self.launch_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.await.unwrap()?;
        }
        if let Some(error) = self.launch_error.lock().unwrap().clone() {
            return Err(error);
        }
        let handle = match &launch.target {
            LaunchTarget::Story {
                story_id, handle, ..
            } => {
                // A story's worktree is prepared the way the spine prepares it: under the
                // per-story lock, created and set up when it is cold. Creating yields, so
                // an unserialized second launch would see it cold too.
                crate::commands::mcp_workspace::with_story_prepare_lock(story_id, async {
                    let cold = !self.story_worktrees.lock().unwrap().contains(story_id);
                    if cold {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        self.worktree_creations.fetch_add(1, Ordering::SeqCst);
                        self.story_worktrees
                            .lock()
                            .unwrap()
                            .insert(story_id.clone());
                    }
                })
                .await;
                handle.clone()
            }
            LaunchTarget::Dir { handle, .. } => handle.clone(),
        };
        let mut record = launch.record.clone();
        if let LaunchTarget::Story { story_id, .. } = &launch.target {
            let team = &self.story_teams[story_id];
            record.meta.team.get_or_insert(LaunchTeam {
                org: team.org.clone(),
                team: team.team.clone(),
            });
        }
        let fresh = self.registry.begin_with_provenance(
            &handle,
            Some(launch.provenance.clone()),
            record,
        )?;
        let mut warnings = vec![];
        if fresh {
            let n = self.next_session.fetch_add(1, Ordering::SeqCst);
            let probe = self.registered_spawn_probe.lock().unwrap().clone();
            if let Some(probe) = probe {
                probe(&launch, &format!("sess-{n}")).map_err(LaunchError::Other)?;
            }
            self.registry
                .bind(&handle, &format!("sess-{n}"), Some("mcp"))
                .unwrap();
            if let LaunchTarget::Dir { cwd, .. } = &launch.target {
                self.registry
                    .set_worktree_dir_if_absent(&handle, cwd.clone());
            }
            let plan = super::super::mcp_workspace::plan_initial_prompt(
                launch.agent.runtime,
                launch.agent.launch.args.clone(),
                launch.initial_prompt.as_deref(),
            );
            if plan.pty_prompt.is_some() {
                warnings = super::super::mcp_workspace::startup_submission(Ok(*self
                    .startup_submit
                    .lock()
                    .unwrap()))
                .1;
            }
            self.launches.lock().unwrap().push(launch);
        }
        Ok(LaunchOutcome {
            handle,
            reconciled: None,
            fresh,
            warnings,
        })
    }

    fn reveal(&self, handle: &str, focus: bool) {
        self.reveals
            .lock()
            .unwrap()
            .push((handle.to_string(), focus));
    }

    async fn show(&self, handle: &str) -> RevealOutcome {
        self.reveal(handle, true);
        self.show_outcome.lock().unwrap().clone()
    }
}

fn active() -> ActiveTeam {
    ActiveTeam {
        org: ORG.to_string(),
        team: "default".to_string(),
    }
}

fn team_workspace(name: &str, launched_by: Option<&str>) -> OpenRequest {
    OpenRequest {
        target: "team-workspace".into(),
        name: Some(name.into()),
        initial_prompt: Some("/manager sprint 1".into()),
        launched_by: launched_by.map(str::to_string),
        ..Default::default()
    }
}

const SPRINT_1: &str = "_worker/acme/widgets-context/default/sprint-1";

/// Check the facts the real open and reservation pass to session and task capture.
fn assert_registry_story(registry: &DashboardLaunchState, handle: &str, expected: Option<&str>) {
    let snapshot = registry.snapshot().into_iter().find(|s| s.handle == handle).unwrap();
    assert_eq!(snapshot.meta.story.as_deref(), expected);
    let session = registry.resolve(handle).expect("handle is bound");
    let write = crate::commands::dashboard_launch::compose_launch_write(
        registry,
        &session,
        handle,
        Default::default(),
    ).unwrap();
    assert_eq!(write.record.story_id.as_deref(), expected);
    if expected.is_none() {
        assert_eq!(write.record.unavailable["storyId"], "the launch belongs to no story");
    }
    let dir = tempfile::tempdir().unwrap();
    let store = crate::session_usage::UsageStore::open(&dir.path().join("usage.sqlite")).unwrap();
    store.write_session_record(&write.record).unwrap();
    let task = crate::session_usage::task_capture::capture_launch_task(
        &store,
        &crate::session_usage::task_capture::Capture {
            session_id: &session,
            record: &write.record,
            inputs: &write.task,
            routine: write.routine.as_ref(),
            launcher_task: write.launcher.as_deref(),
            story_file: None,
            now_ms: 1,
        },
    ).unwrap();
    assert_eq!(task.story_id.as_deref(), expected);
    assert_eq!(store.read_task(&session).unwrap().unwrap().story_id.as_deref(), expected);
}

fn assert_launch_story(host: &FakeHost, handle: &str, expected: Option<&str>) {
    assert_registry_story(&host.registry, handle, expected);
}

async fn open_story_lead(host: &FakeHost) -> String {
    let result = open_agent(
        host,
        &active(),
        &OpenRequest {
            target: "story".into(),
            story_id: Some("SW-1".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    host.session_of("SW-1")
}

#[tokio::test]
async fn cde36_team_workspace_explicit_story_reaches_the_record_and_task() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            story_id: Some("SW-1".into()),
            ..team_workspace("Story worker", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert_launch_story(&host, result.handle.as_deref().unwrap(), Some("SW-1"));
}

#[tokio::test]
async fn cde36_team_workspace_story_attribution_does_not_follow_the_story_to_a_machine() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    host.place("SW-1", WorkspacePlace::Machine("box".into()));
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            story_id: Some("SW-1".into()),
            worktree: Some("Story content".into()),
            create_worktree: true,
            ..team_workspace("Story review", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened, "{}", result.message);
    assert!(host.remote_launches().is_empty(), "storyId only attributes the local session");
    let handle = result.handle.as_deref().unwrap();
    let launches = host.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].target, LaunchTarget::Dir {
        handle: handle.into(),
        cwd: host.worker("story-content"),
        scope: active(),
    });
    assert_eq!(result.worktree.as_ref().unwrap().name, "Story content");
    assert_launch_story(&host, handle, Some("SW-1"));
}

#[tokio::test]
async fn cde36_team_workspace_remote_story_allows_local_headless_and_max_runtime() {
    use crate::commands::mcp_workspace::SessionView;
    for (mode, budget) in [
        ("headless", None),
        ("headless", Some(60_000)),
        ("headed", Some(60_000)),
    ] {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::new(home.path(), 4);
        host.place("SW-1", WorkspacePlace::Machine("box".into()));
        let result = open_agent(
            &host,
            &active(),
            &OpenRequest {
                story_id: Some("SW-1".into()),
                worktree: Some("Story content".into()),
                create_worktree: true,
                mode: Some(mode.into()),
                max_runtime_ms: budget,
                ..team_workspace("Story review", None)
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result.status,
            OpenStatus::Opened,
            "mode={mode}, budget={budget:?}: {}",
            result.message
        );
        assert!(host.remote_launches().is_empty());
        let launches = host.launches();
        assert_eq!(launches.len(), 1);
        assert!(matches!(&launches[0].target, LaunchTarget::Dir { cwd, .. } if cwd == &host.worker("story-content")));
        assert_eq!(launches[0].record.max_runtime_ms, budget);
        assert_eq!(launches[0].view, if mode == "headless" { SessionView::Background } else { SessionView::Tab { focus: true } });
        assert_launch_story(&host, result.handle.as_deref().unwrap(), Some("SW-1"));
    }
}

#[tokio::test]
async fn cde36_team_workspace_inherits_the_story_its_launcher_opened() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let lead = open_story_lead(&host).await;
    let result = open_agent(
        &host,
        &active(),
        &team_workspace("Story worker", Some(&lead)),
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert_launch_story(&host, result.handle.as_deref().unwrap(), Some("SW-1"));
}

#[tokio::test]
async fn cde36_team_workspace_explicit_story_wins_over_its_launchers_story() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let lead = open_story_lead(&host).await;
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            story_id: Some("SW-9".into()),
            ..team_workspace("Other story", Some(&lead))
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert_launch_story(&host, result.handle.as_deref().unwrap(), Some("SW-9"));
}

#[tokio::test]
async fn cde36_team_workspace_never_guesses_a_story_from_its_name_or_launcher() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let parent = open_agent(&host, &active(), &team_workspace("SW-1 planning", None))
        .await.unwrap();
    let parent_handle = parent.handle.as_deref().unwrap();
    assert_launch_story(&host, parent_handle, None);
    let parent_session = host.session_of(parent_handle);
    let child = open_agent(
        &host,
        &active(),
        &team_workspace("SW-1 worker", Some(&parent_session)),
    )
    .await
    .unwrap();
    assert_launch_story(&host, child.handle.as_deref().unwrap(), None);
}

#[tokio::test]
async fn open_workspace_team_workspace_launches_in_context_worktree() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    std::fs::write(
        host.content_root().join(".claude/commands/manager.md"),
        b"team manager",
    )
    .unwrap();

    let result = open_agent(&host, &active(), &team_workspace("Sprint 1", Some("S1")))
        .await
        .unwrap();

    assert_eq!(result.status, OpenStatus::Opened);
    assert_eq!(result.handle.as_deref(), Some(SPRINT_1));
    assert_eq!(result.name.as_deref(), Some("Sprint 1"));
    assert_eq!(result.message, "Opened Sprint 1.");

    let launches = host.launches();
    let [launch] = launches.as_slice() else {
        panic!("one launch, got {launches:?}");
    };
    let worker = host.worker("sprint-1");
    assert_eq!(
        launch.target,
        LaunchTarget::Dir {
            handle: SPRINT_1.into(),
            cwd: worker.clone(),
            scope: active(),
        }
    );
    assert_ne!(worker, host.content_root(), "never the team content root");
    assert!(!launch
        .agent
        .launch
        .args
        .iter()
        .any(|arg| arg == "--add-dir"));
    assert_eq!(result.worktree.as_ref().unwrap().name, "Sprint 1");
    assert_eq!(launch.initial_prompt.as_deref(), Some("/manager sprint 1"));
    assert_eq!(launch.agent.launch.id, "builtin/claude");
    assert_eq!(
        launch.record.started_with.as_deref(),
        Some("/manager sprint 1")
    );

    // The worker dir can host an agent, even one typed by hand: the status hooks wired
    // and the team's command staged.
    assert!(worker.join(".claude/settings.local.json").is_file());
    assert_eq!(
        std::fs::read(worker.join(".claude/commands/manager.md")).unwrap(),
        b"team manager"
    );
    // Nothing was written into the team content root.
    assert!(!host.content_root().join(".mcp.json").exists());

    // No mode → the pill appears without stealing focus.
    assert_eq!(host.reveals(), vec![(SPRINT_1.to_string(), false)]);
}

#[tokio::test]
async fn open_workspace_without_an_agent_launches_the_preferred_builtin_and_modes_still_work() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);

    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("headed".into()),
            ..team_workspace("Sprint 1", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    let launch = &host.launches()[0];
    // No agent named: the preferred built-in runs, with its own argv (no bypass flag) and
    // its revision on the launch's provenance.
    let stamp = format!("builtin/claude@{}", crate::agents::builtin::revision());
    assert_eq!(launch.agent.revision_label(), stamp);
    assert_eq!(launch.agent.launch.binary, "claude");
    assert_eq!(launch.agent.launch.args, Vec::<String>::new());
    assert_eq!(
        launch.agent.launch.mode,
        crate::agents::adapter::LaunchMode::Interactive
    );
    assert_eq!(launch.provenance.agent.as_deref(), Some(stamp.as_str()));
    assert!(!launch
        .agent
        .launch
        .args
        .iter()
        .any(|arg| arg == "--add-dir"));
    assert_eq!(host.reveals(), vec![(SPRINT_1.to_string(), true)]);

    let headless = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("headless".into()),
            ..team_workspace("Sprint 2", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(headless.status, OpenStatus::Opened);
    assert_eq!(host.reveals().len(), 1, "headless reveals nothing");
    // A background open nobody watches runs the unattended built-in instead.
    let background = &host.launches()[1].agent;
    assert_eq!(
        background.launch.mode,
        crate::agents::adapter::LaunchMode::Headless
    );
    assert_eq!(
        background.revision_label(),
        format!(
            "builtin/claude-unattended@{}",
            crate::agents::builtin::revision()
        )
    );
    assert_eq!(background.launch.args, ["--dangerously-skip-permissions"]);
}

#[tokio::test]
async fn open_workspace_statuses_opened_alreadylive_busy_invalid() {
    let home = tempfile::tempdir().unwrap();
    // A spawn cap of one: the second distinct session saturates it.
    let host = FakeHost::new(home.path(), 1);
    let open = |req: OpenRequest| {
        let host = &host;
        async move { open_agent(host, &active(), &req).await.unwrap() }
    };

    let opened = open(team_workspace("Sprint 1", Some("S1"))).await;
    assert_eq!(opened.status, OpenStatus::Opened);

    // Same name again, from anyone → the same session, no second launch, and the
    // original launcher stays.
    let again = open(team_workspace("sprint 1!", Some("S2"))).await;
    assert_eq!(again.status, OpenStatus::AlreadyLive);
    assert_eq!(again.handle.as_deref(), Some(SPRINT_1));
    assert_eq!(again.message, "sprint 1! is already running.");
    assert_eq!(host.launches().len(), 1);
    let snap = host.registry.snapshot();
    assert_eq!(snap[0].meta.launched_by.as_deref(), Some("S1"));

    // The shipped cap is saturated → a plain `busy`, not an error.
    let busy = open(team_workspace("Sprint 2", None)).await;
    assert_eq!(busy.status, OpenStatus::Busy);
    assert!(
        busy.message.contains("too many agent sessions"),
        "{}",
        busy.message
    );

    // Input the resolver cannot use → `invalid`, and nothing launches.
    for (req, reason) in [
        (
            OpenRequest {
                agent: Some("no-such-agent".into()),
                ..team_workspace("Sprint 3", None)
            },
            "no agent \"no-such-agent\"",
        ),
        (
            OpenRequest {
                target: "epic".into(),
                ..Default::default()
            },
            "unknown target",
        ),
        (
            OpenRequest {
                name: None,
                ..team_workspace("x", None)
            },
            "name is required",
        ),
        (
            OpenRequest {
                team: Some("design".into()),
                ..team_workspace("Sprint 3", None)
            },
            "unknown team",
        ),
        (
            OpenRequest {
                target: "story".into(),
                ..Default::default()
            },
            "story_id is required",
        ),
        (
            OpenRequest {
                mode: Some("sideways".into()),
                ..team_workspace("Sprint 3", None)
            },
            "unknown workspace mode",
        ),
    ] {
        let invalid = open(req).await;
        assert_eq!(invalid.status, OpenStatus::Invalid, "{reason}");
        assert!(
            invalid.message.contains(reason),
            "{reason}: {}",
            invalid.message
        );
    }
    assert_eq!(
        host.launches().len(),
        1,
        "no invalid open launched anything"
    );
}

#[tokio::test]
async fn invalid_mode_is_an_input_error_not_a_refusal() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("sideways".into()),
            ..team_workspace("Sprint 1", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Invalid);
    assert_eq!(
        result.message,
        "Not opened: unknown workspace mode 'sideways' — use headed | visible-unfocused | headless."
    );
}

#[tokio::test]
async fn team_with_parent_components_is_invalid_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut host = FakeHost::new(home.path(), 4);
    let evil = [
        "../escape",
        "..",
        "a/b",
        "/abs",
        "./x",
        "default/../../x",
        "default/",
        "default/.",
    ];
    // A host that would resolve any of these, so only the open's own check stops them.
    for team in evil {
        host.content_roots
            .insert(team.to_string(), host.content_root().to_path_buf());
    }
    host.story_teams.insert("SW-1".into(), active());

    for team in evil {
        for req in [
            team_workspace("Sprint 1", None),
            OpenRequest {
                target: "story".into(),
                story_id: Some("SW-1".into()),
                agent: Some("builtin/codex".into()),
                ..Default::default()
            },
        ] {
            let result = open_agent(
                &host,
                &active(),
                &OpenRequest {
                    team: Some(team.into()),
                    ..req
                },
            )
            .await
            .unwrap();
            assert_eq!(result.status, OpenStatus::Invalid, "{team}");
            assert_eq!(
                result.message,
                format!("Not opened: unknown team {team:?}.")
            );
        }
    }
    assert!(host.launches().is_empty());
    assert!(host.lookups().is_empty(), "no path was built from the team");
    assert!(!home.path().join("users").exists(), "no dir was written");
    assert!(!home.path().join("escape").exists());
}

/// Two opens of the same name racing: the one still staging holds the name, so the
/// other neither restages under it nor launches; it reads `alreadyLive`.
#[tokio::test]
async fn concurrent_same_name_opens_stage_once() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let (release, gate) = tokio::sync::oneshot::channel();
    *host.stage_gate.lock().unwrap() = Some(gate);

    let req = team_workspace("Sprint 1", None);
    let team = active();
    let (first, second) = tokio::join!(open_agent(&host, &team, &req), async {
        let second = open_agent(&host, &team, &req);
        tokio::pin!(second);
        assert!(
            futures_util::poll!(&mut second).is_pending(),
            "an in-flight open is not already live"
        );
        let _ = release.send(());
        second.await
    });

    assert_eq!(first.unwrap().status, OpenStatus::Opened);
    assert_eq!(second.unwrap().status, OpenStatus::AlreadyLive);
    assert_eq!(host.stagings.load(Ordering::SeqCst), 1, "staged once");
    assert_eq!(host.launches().len(), 1);
    assert!(host.worker("sprint-1").is_dir());

    // The name is free again once the open settles: a reopen is the live session.
    let again = open_agent(&host, &active(), &req).await.unwrap();
    assert_eq!(again.status, OpenStatus::AlreadyLive);
    assert_eq!(host.stagings.load(Ordering::SeqCst), 1);
}

/// A failed launch releases the name: the next open stages and launches afresh.
#[tokio::test]
async fn a_failed_open_releases_its_name() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let req = team_workspace("Sprint 1", None);
    *host.launch_error.lock().unwrap() = Some(LaunchError::Other("no agent CLI".into()));
    open_agent(&host, &active(), &req).await.unwrap_err();
    *host.launch_error.lock().unwrap() = None;
    let opened = open_agent(&host, &active(), &req).await.unwrap();
    assert_eq!(opened.status, OpenStatus::Opened);
    assert_eq!(host.stagings.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn launch_failure_is_typed_and_leaves_no_new_worker_dir() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);

    // Any failure other than the cap is the tool error, even one whose text is "busy".
    for message in ["unavailable: no agent CLI configured", "busy"] {
        *host.launch_error.lock().unwrap() = Some(LaunchError::Other(message.into()));
        let error = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
            .await
            .unwrap_err();
        assert_eq!(error, message);
        assert!(!host.worker("sprint-1").exists(), "the new dir is removed");
    }

    // The cap is a plain `busy` result, and its dir is removed too.
    *host.launch_error.lock().unwrap() = Some(LaunchError::Busy);
    let busy = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    assert_eq!(busy.status, OpenStatus::Busy);
    assert!(!host.worker("sprint-1").exists());

    // A dir an earlier session left is not this open's to remove.
    std::fs::create_dir_all(host.worker("sprint-1")).unwrap();
    open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    assert!(host.worker("sprint-1").is_dir());
    assert!(host.launches().is_empty());
}

#[tokio::test]
async fn busy_open_leaves_no_orphan_worker_dir() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 1);
    let opened = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    assert_eq!(opened.status, OpenStatus::Opened);
    let busy = open_agent(&host, &active(), &team_workspace("Sprint 2", None))
        .await
        .unwrap();
    assert_eq!(busy.status, OpenStatus::Busy);
    assert!(host.worker("sprint-1").is_dir());
    assert!(!host.worker("sprint-2").exists());
}

#[tokio::test]
async fn already_live_open_does_not_restage_the_running_agents_dir() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    let worker = host.worker("sprint-1");
    std::fs::write(worker.join(".mcp.json"), b"the running agent's").unwrap();
    std::fs::write(
        host.content_root().join(".claude/commands/new.md"),
        b"added later",
    )
    .unwrap();

    let again = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    assert_eq!(again.status, OpenStatus::AlreadyLive);
    assert_eq!(
        std::fs::read(worker.join(".mcp.json")).unwrap(),
        b"the running agent's"
    );
    assert!(!worker.join(".claude/commands/new.md").exists());
}

#[tokio::test]
async fn a_story_agent_resolves_in_the_storys_own_team() {
    let home = tempfile::tempdir().unwrap();
    let mut host = FakeHost::new(home.path(), 4);
    let design = ActiveTeam {
        org: "acme/other-context".into(),
        team: "design".into(),
    };
    host.story_teams.insert("SW-9".into(), design.clone());

    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            target: "story".into(),
            story_id: Some("SW-9".into()),
            // The app's selected team is `default`; the story still launches in its own.
            // A `team` naming any other team is refused.
            team: Some("design".into()),
            agent: Some("builtin/codex".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    let launch = &host.launches()[0];
    assert_eq!(launch.agent.launch.binary, "codex");
    assert_eq!(
        launch.agent.startup_context.target,
        crate::agents::ManifestTarget {
            context_root: None,
            org: design.org,
            team: design.team,
            repo: None,
        }
    );

    let unknown = open_agent(
        &host,
        &active(),
        &OpenRequest {
            target: "story".into(),
            story_id: Some("SW-404".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(unknown.status, OpenStatus::Invalid);
    assert!(
        unknown.message.contains("unknown story"),
        "{}",
        unknown.message
    );
}

#[tokio::test]
async fn already_live_headed_open_brings_the_session_forward() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    for (mode, status) in [
        (Some("headed"), OpenStatus::AlreadyLive),
        (None, OpenStatus::AlreadyLive),
        (Some("visible-unfocused"), OpenStatus::AlreadyLive),
    ] {
        let again = open_agent(
            &host,
            &active(),
            &OpenRequest {
                mode: mode.map(str::to_string),
                ..team_workspace("Sprint 1", None)
            },
        )
        .await
        .unwrap();
        assert_eq!(again.status, status);
    }
    assert_eq!(
        host.reveals(),
        vec![(SPRINT_1.to_string(), false), (SPRINT_1.to_string(), true)],
        "only the headed reopen takes focus"
    );
}

/// The app side of the verb, recording what the verb asked of it.
#[derive(Default)]
struct FakePort {
    registry: DashboardLaunchState,
    targeted_error: Option<String>,
    story_launches: Mutex<Vec<(String, Option<String>)>>,
    /// The org hint of every legacy story launch, in order.
    story_orgs: Mutex<Vec<Option<String>>>,
    story_error: Option<String>,
    story_views: Mutex<Vec<crate::commands::mcp_workspace::SessionView>>,
    reveals: Mutex<Vec<(String, bool)>>,
    machine_opens: Mutex<Vec<(String, String, bool)>>,
    // The viewer's refusal of a machine open (SW-FX-131), when it refused one.
    machine_error: Option<String>,
}

#[async_trait::async_trait]
impl OpenWorkspacePort for FakePort {
    async fn open_on_machine(
        &self,
        story_id: &str,
        machine_id: &str,
        focus: bool,
    ) -> Result<(), String> {
        self.machine_opens.lock().unwrap().push((
            story_id.to_string(),
            machine_id.to_string(),
            focus,
        ));
        match &self.machine_error {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    async fn launch_story(
        &self,
        story_id: &str,
        org: Option<&str>,
        initial_prompt: Option<&str>,
        view: crate::commands::mcp_workspace::SessionView,
    ) -> Result<crate::commands::mcp_workspace::LaunchOutcome, String> {
        self.story_orgs
            .lock()
            .unwrap()
            .push(org.map(str::to_string));
        self.story_views.lock().unwrap().push(view);
        self.story_launches
            .lock()
            .unwrap()
            .push((story_id.to_string(), initial_prompt.map(str::to_string)));
        if let Some(error) = &self.story_error {
            return Err(error.clone());
        }
        let fresh = self.registry.begin_with_provenance(
            story_id,
            None,
            crate::commands::mcp_workspace::story_launch_record(story_id, None),
        ).map_err(|error| error.to_string())?;
        if fresh {
            self.registry.bind(story_id, "legacy-session", Some("mcp")).unwrap();
        }
        Ok(crate::commands::mcp_workspace::LaunchOutcome {
            handle: story_id.to_string(),
            reconciled: None,
            fresh,
            warnings: vec![],
        })
    }

    async fn targeted(&self) -> Result<(ActiveTeam, Box<dyn OpenHost>), String> {
        match &self.targeted_error {
            Some(error) => Err(error.clone()),
            None => panic!("a legacy open never resolves the targeted host"),
        }
    }

    fn reveal(&self, handle: &str, focus: bool) {
        self.reveals
            .lock()
            .unwrap()
            .push((handle.to_string(), focus));
    }
}

fn params(json: serde_json::Value) -> crate::mcp::handler::OpenWorkspaceParams {
    serde_json::from_value(json).expect("params deserialize")
}

#[tokio::test]
async fn a_story_opens_on_a_machine_through_the_viewer_never_locally() {
    let port = FakePort::default();
    let result = open_workspace_call(
        &port,
        params(serde_json::json!({"story_id": "SW-1", "machineId": "box"})),
    )
    .await
    .unwrap();
    // SW-FX-131 AC1: the call returns once the viewer confirmed the open, never "opening".
    assert_eq!(result["status"], "opened");
    assert_eq!(result["machineId"], "box");
    assert_eq!(
        *port.machine_opens.lock().unwrap(),
        vec![("SW-1".to_string(), "box".to_string(), true)]
    );
    assert!(port.story_launches.lock().unwrap().is_empty());
    assert!(port.reveals.lock().unwrap().is_empty());

    let unfocused = FakePort::default();
    open_workspace_call(
        &unfocused,
        params(serde_json::json!({"story_id": "SW-1", "machine_id": "box", "mode": "visible-unfocused"})),
    )
    .await
    .unwrap();
    assert!(!unfocused.machine_opens.lock().unwrap()[0].2);

    // With a target the launch goes through the targeted open (which launches the agent
    // on the machine), never the viewer-only open.
    let refused = FakePort {
        targeted_error: Some("no team is selected".into()),
        ..FakePort::default()
    };
    let targeted = open_workspace_call(
        &refused,
        params(serde_json::json!({"story_id": "SW-1", "machine_id": "box", "target": "story"})),
    )
    .await
    .unwrap();
    assert_eq!(targeted["status"], "invalid");
    assert!(targeted["message"]
        .as_str()
        .unwrap()
        .contains("no team is selected"));
    assert!(open_workspace_call(
        &refused,
        params(serde_json::json!({"story_id": "SW-1", "machine_id": "box", "headed": false})),
    )
    .await
    .is_err());
    assert!(refused.machine_opens.lock().unwrap().is_empty());
}

/// SW-FX-131 AC1/AC4: a machine open the viewer cannot complete fails the call with its reason.
#[tokio::test]
async fn a_failing_machine_open_returns_its_reason_as_the_tool_error() {
    let port = FakePort {
        machine_error: Some("SW-1's workspace couldn't be created on the machine: no clone".into()),
        ..FakePort::default()
    };
    let error = open_workspace_call(
        &port,
        params(serde_json::json!({"story_id": "SW-1", "machineId": "box"})),
    )
    .await
    .unwrap_err();
    assert!(error.contains("no clone"), "{error}");
    assert_eq!(port.machine_opens.lock().unwrap().len(), 1);
}

/// SW-FX-131 AC2: a viewer that never answers turns the stall into a failure.
#[tokio::test(start_paused = true)]
async fn a_machine_open_with_no_viewer_reply_times_out_as_unconfirmed() {
    let registry = MachineOpenRegistry::default();
    let (_request_id, reply) = registry.register();
    let error = await_machine_open(reply, MACHINE_OPEN_TIMEOUT, "SW-1", "box")
        .await
        .unwrap_err();
    assert!(error.starts_with("unconfirmed:"), "{error}");
    assert!(error.contains("10 minutes"), "{error}");
}

/// SW-FX-131 AC3: the viewer's reply, success or failure, is what the waiting call sees.
#[tokio::test]
async fn a_viewer_reply_resolves_the_waiting_machine_open() {
    let registry = MachineOpenRegistry::default();
    let timeout = std::time::Duration::from_secs(1);

    let (id, reply) = registry.register();
    registry.take(&id).unwrap().send(Ok(())).unwrap();
    assert_eq!(
        await_machine_open(reply, timeout, "SW-1", "box").await,
        Ok(())
    );

    let (id, reply) = registry.register();
    registry
        .take(&id)
        .unwrap()
        .send(Err("the story can't be resolved".into()))
        .unwrap();
    assert_eq!(
        await_machine_open(reply, timeout, "SW-1", "box").await,
        Err("unavailable: the story can't be resolved".into())
    );
    // Consume-once: a duplicate reply finds nothing.
    assert!(registry.take(&id).is_none());
}

/// SW-FX-131: a cancelled open leaves no sender behind to be resolved later.
#[test]
fn an_abandoned_machine_open_is_removed_from_the_registry() {
    let registry = std::sync::Arc::new(MachineOpenRegistry::default());
    let (id, _reply) = registry.register();
    drop(PendingMachineOpen {
        registry: registry.clone(),
        request_id: id.clone(),
    });
    assert!(registry.take(&id).is_none());
}

#[test]
fn a_story_already_living_elsewhere_is_not_opened_on_a_machine() {
    use super::machine_open_conflict;
    let viewer = serde_json::json!({
        "workspaces": { "acme:core": [
            { "id": "sw-1-slug", "kind": "story", "storyId": "SW-1", "cwd": "/wt/sw-1" },
            { "id": "sw-2-slug", "kind": "story", "storyId": "SW-2", "cwd": "",
              "location": { "kind": "machine", "machineId": "box", "requestId": "r" } }
        ]},
        "softClosedWorkspaces": { "acme:core": { "sw-3-slug": { "workspace":
            { "id": "sw-3-slug", "kind": "story", "storyId": "SW-3", "cwd": "",
              "location": { "kind": "machine", "machineId": "other", "requestId": "q" } } } } }
    });
    let conflict =
        |story: &str, machine: &str| machine_open_conflict(&viewer, "acme:core", story, machine);
    assert!(conflict("SW-1", "box").unwrap().contains("This Mac"));
    assert_eq!(conflict("SW-2", "box"), None);
    assert!(conflict("SW-2", "other").unwrap().contains("machine box"));
    assert!(conflict("SW-3", "box").unwrap().contains("machine other"));
    assert_eq!(conflict("SW-4", "box"), None);
    assert_eq!(
        machine_open_conflict(&viewer, "acme:infra", "SW-1", "box"),
        None
    );
}

#[tokio::test]
async fn a_named_story_launch_is_listed_and_tabbed_under_its_name() {
    // Seen live: open_workspace {target:"story", name:"demo"} answered name "demo" while
    // its tab and list_sessions said "Terminal 10".
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let request = OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        name: Some("demo".into()),
        agent: Some("builtin/claude".into()),
        ..Default::default()
    };
    let result = open_agent(&host, &active(), &request).await.unwrap();
    assert_eq!(result.handle.as_deref(), Some("SW-1#demo"));
    let launch = host.launches().remove(0);
    // The spine names the tab from the record (`agent_open_tab(.., tab_name)` with
    // `tab_name = record.meta.display_name`).
    assert_eq!(launch.record.meta.display_name.as_deref(), Some("demo"));
    let rows = host.list(&SessionTurnState::default(), None);
    let row = rows.iter().find(|r| r.handle == "SW-1#demo").unwrap();
    assert_eq!(row.display_name, "demo");
    // A tab named what it was launched as lists under that name too.
    let rows = crate::mcp::session_list::session_list_rows_with_tabs(
        host.registry
            .snapshot()
            .into_iter()
            .map(|s| session_row(s, true))
            .collect(),
        |_| None,
        None,
        |_| {
            Some(crate::mcp::session_list::TabView {
                name: launch.record.meta.display_name.clone(),
                machine_id: None,
                workspace_handle: None,
                machine: "This Mac".into(),
                workspace: None,
                session: None,
                alive: Some(true),
                activity: None,
                activity_failed: false,
                subagents: 0,
                agent: None,
            })
        },
    );
    assert_eq!(rows[0].display_name, "demo");
}

#[tokio::test]
async fn cde36_open_workspace_legacy_story_call_is_unchanged() {
    // `{story_id}` alone: headed by default, the story focused, the shipped shape.
    let port = FakePort::default();
    let result = open_workspace_call(&port, params(serde_json::json!({"story_id": "SW-1"})))
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!({"handle": "SW-1", "mode": "headed", "headed": true, "reconciled": null})
    );
    assert_eq!(
        *port.story_launches.lock().unwrap(),
        vec![("SW-1".to_string(), None)]
    );
    assert_registry_story(&port.registry, "SW-1", Some("SW-1"));
    assert_eq!(
        *port.reveals.lock().unwrap(),
        vec![("SW-1".to_string(), true)]
    );

    // The shipped mode options.
    for (extra, wire, reveal) in [
        (
            serde_json::json!({"mode": "visible-unfocused"}),
            "visible-unfocused",
            Some(false),
        ),
        (serde_json::json!({"headed": false}), "headless", None),
        (serde_json::json!({"headed": true}), "headed", Some(true)),
    ] {
        let port = FakePort::default();
        let mut call = serde_json::json!({"story_id": "SW-1", "initial_prompt": "/review"});
        call.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let result = open_workspace_call(&port, params(call)).await.unwrap();
        assert_eq!(result["mode"], wire);
        assert_registry_story(&port.registry, "SW-1", Some("SW-1"));
        assert_eq!(
            *port.story_launches.lock().unwrap(),
            vec![("SW-1".to_string(), Some("/review".to_string()))]
        );
        assert_eq!(
            port.reveals
                .lock()
                .unwrap()
                .first()
                .map(|(_, focus)| *focus),
            reveal
        );
    }

    // The shipped errors, word for word.
    let port = FakePort::default();
    let bad_mode = open_workspace_call(
        &port,
        params(serde_json::json!({"story_id": "SW-1", "mode": "sideways"})),
    )
    .await
    .unwrap_err();
    assert_eq!(
        bad_mode,
        "denied: unknown workspace mode 'sideways' — use headed | visible-unfocused | headless"
    );
    assert_eq!(
        open_workspace_call(&port, params(serde_json::json!({})))
            .await
            .unwrap_err(),
        "story_id is required"
    );
    assert!(port.story_launches.lock().unwrap().is_empty());
    let busy_port = FakePort {
        story_error: Some("busy".into()),
        ..Default::default()
    };
    assert_eq!(
        open_workspace_call(&busy_port, params(serde_json::json!({"story_id": "SW-1"})))
            .await
            .unwrap_err(),
        "busy"
    );
    assert!(busy_port.reveals.lock().unwrap().is_empty());
}

#[tokio::test]
async fn open_workspace_targeted_maps_a_context_error_to_invalid() {
    let port = FakePort {
        targeted_error: Some(
            "no active team selected — open codesense and pick a team first".into(),
        ),
        ..Default::default()
    };
    let result = open_workspace_call(
        &port,
        params(serde_json::json!({"target": "team-workspace", "name": "Sprint 1"})),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "status": "invalid",
            "handle": null,
            "name": null,
            "message": "Not opened: no active team selected — open codesense and pick a team first.",
        })
    );
    assert!(port.story_launches.lock().unwrap().is_empty());
    assert!(port.reveals.lock().unwrap().is_empty());
}

#[tokio::test]
async fn open_workspace_story_target_uses_the_story_id_as_handle() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            target: "story".into(),
            story_id: Some("SW-1".into()),
            launched_by: Some("S1".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert_eq!(result.handle.as_deref(), Some("SW-1"));
    assert_eq!(result.name.as_deref(), Some("SW-1"));
    let launch = &host.launches()[0];
    assert_eq!(
        launch.target,
        LaunchTarget::Story {
            story_id: "SW-1".into(),
            handle: "SW-1".into(),
            team: active(),
        }
    );
    assert!(!launch
        .agent
        .launch
        .args
        .iter()
        .any(|arg| arg == "--add-dir"));
    assert_eq!(launch.record.meta.story.as_deref(), Some("SW-1"));
    assert_launch_story(&host, "SW-1", Some("SW-1"));
    assert_eq!(host.reveals(), vec![("SW-1".to_string(), false)]);
}

/// A team workspace records its named team, so its inbox follows it when the UI team
/// changes. A story leaves it to the story launch spine, which records the story's own
/// team.
#[tokio::test]
async fn launch_records_the_team_it_runs_in() {
    use crate::commands::dashboard_launch::LaunchTeam;
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    open_agent(
        &host,
        &active(),
        &OpenRequest {
            target: "story".into(),
            story_id: Some("SW-9".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let teams: Vec<_> = host
        .launches()
        .iter()
        .map(|l| l.record.meta.team.clone())
        .collect();
    assert_eq!(
        teams,
        vec![
            Some(LaunchTeam {
                org: ORG.to_string(),
                team: "default".to_string(),
            }),
            None,
        ]
    );
}

#[tokio::test]
async fn team_workspace_handle_is_readable_by_shipped_read_verbs() {
    use codesense_pty_core::domain::{
        PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
    };
    use codesense_pty_core::layout::ServerIdentity;
    use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner, PtySpec};
    use codesense_pty_core::{SessionMeta, SessionRegistration, TerminalHost};

    struct NoopSink;
    impl TerminalEventSink for NoopSink {
        fn emit(&self, _event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            Ok(())
        }
    }

    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let opened = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    let handle = opened.handle.unwrap();

    // The read verbs resolve a handle through the launch registry, then read the PTY
    // bound to it. Put a real PTY behind the session the open bound.
    let session_id = host
        .registry
        .resolve(&handle)
        .expect("the handle resolves to its session, like any story handle");
    let terminal = TerminalHost::new(
        ServerIdentity::ephemeral("test"),
        std::sync::Arc::new(NoopSink),
        tokio::runtime::Handle::current(),
    );
    terminal.attach_echo_emulator();
    let mut command = portable_pty::CommandBuilder::new("sh");
    command.args(["-c", "printf 'sprint one ready'; sleep 5"]);
    let session = LocalPtyRunner
        .spawn(PtySpec {
            command,
            size: portable_pty::PtySize::default(),
        })
        .await
        .unwrap();
    terminal
        .register(
            session,
            SessionRegistration {
                session_id: session_id.clone(),
                meta: SessionMeta::default(),
                home_cwd: None,
                osc_enabled: false,
                is_pool_member: false,
                scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
            },
        )
        .unwrap();

    // Both read verbs read the session's emulator text from its owning host
    // (`pty_backend::visible_text`).
    let id = PtySessionId::new(&session_id);
    let mut visible = String::new();
    for _ in 0..100 {
        visible = codesense_pty_core::domain::visible_text(&terminal, &id, u32::MAX)
            .unwrap()
            .text;
        if visible.contains("sprint one ready") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        visible.contains("sprint one ready"),
        "read_terminal_tab / wv_read_recent read: {visible:?}"
    );
    terminal.reap_non_persisting().unwrap();
}

fn named_story(name: Option<&str>, launched_by: Option<&str>) -> OpenRequest {
    OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        name: name.map(str::to_string),
        launched_by: launched_by.map(str::to_string),
        ..Default::default()
    }
}

#[tokio::test]
async fn named_story_sessions_open_as_tabs_in_the_story_workspace() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 8);
    let manager = open_agent(&host, &active(), &named_story(None, None))
        .await
        .unwrap();
    assert_eq!(manager.handle.as_deref(), Some("SW-1"));
    let manager_session = host.session_of("SW-1");

    let mut opened = Vec::new();
    for name in ["worker-1", "Worker 2", "reviewer"] {
        let result = open_agent(
            &host,
            &active(),
            &named_story(Some(name), Some(&manager_session)),
        )
        .await
        .unwrap();
        assert_eq!(result.status, OpenStatus::Opened, "{name}");
        assert_eq!(result.name.as_deref(), Some(name));
        opened.push(result.handle.unwrap());
    }
    assert_eq!(opened, ["SW-1#worker-1", "SW-1#worker-2", "SW-1#reviewer"]);

    // Every named session launches into the same story (same worktree and workspace),
    // records the story and nests under the manager.
    let launches = host.launches();
    assert_eq!(launches.len(), 4);
    for (launch, handle) in launches[1..].iter().zip(&opened) {
        assert_eq!(
            launch.target,
            LaunchTarget::Story {
                story_id: "SW-1".into(),
                handle: handle.clone(),
                team: active(),
            }
        );
        assert_eq!(launch.record.meta.story.as_deref(), Some("SW-1"));
    }
    let turns = SessionTurnState::default();
    let mine = host.list(&turns, Some(&manager_session));
    assert_eq!(
        mine.iter().map(|r| r.handle.as_str()).collect::<Vec<_>>(),
        ["SW-1#reviewer", "SW-1#worker-1", "SW-1#worker-2"]
    );
    assert!(mine.iter().all(|r| r.story.as_deref() == Some("SW-1")));

    // The same name again is the running session; the story's own session is untouched.
    let again = open_agent(
        &host,
        &active(),
        &named_story(Some("worker-1"), Some(&manager_session)),
    )
    .await
    .unwrap();
    assert_eq!(again.status, OpenStatus::AlreadyLive);
    assert_eq!(again.handle.as_deref(), Some("SW-1#worker-1"));
    assert_eq!(again.message, "worker-1 is already running.");
    let relabel = open_agent(
        &host,
        &active(),
        &OpenRequest {
            role: Some("reviewer".into()),
            ..named_story(Some("worker-1"), Some(&manager_session))
        },
    )
    .await
    .unwrap();
    assert_eq!(relabel.status, OpenStatus::AlreadyLive);
    assert!(
        relabel.message.contains("role and avatar were not changed"),
        "{}",
        relabel.message
    );
    let bare = open_agent(&host, &active(), &named_story(None, None))
        .await
        .unwrap();
    assert_eq!(bare.status, OpenStatus::AlreadyLive);
    assert_eq!(bare.handle.as_deref(), Some("SW-1"));
    assert_eq!(host.launches().len(), 4);

    let unusable = open_agent(&host, &active(), &named_story(Some("##"), None))
        .await
        .unwrap();
    assert_eq!(unusable.status, OpenStatus::Invalid);
}

#[tokio::test]
async fn concurrent_named_launches_into_a_cold_story_prepare_it_once() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 8);
    let cold = |name: &str| OpenRequest {
        story_id: Some("SW-COLD".into()),
        ..named_story(Some(name), Some("S1"))
    };
    let (one, two, team) = (cold("worker-1"), cold("worker-2"), active());
    let (a, b) = tokio::join!(
        open_agent(&host, &team, &one),
        open_agent(&host, &team, &two)
    );
    assert_eq!(a.unwrap().status, OpenStatus::Opened);
    assert_eq!(b.unwrap().status, OpenStatus::Opened);
    assert_eq!(host.worktree_creations.load(Ordering::SeqCst), 1);
    assert_eq!(host.launches().len(), 2);
    assert!(host.registry.has_handle("SW-COLD#worker-1"));
    assert!(host.registry.has_handle("SW-COLD#worker-2"));
}

#[test]
fn launch_labels_drop_control_and_invisible_format_characters() {
    use crate::commands::dashboard_launch::launch_label;
    assert_eq!(
        launch_label(Some("re\u{202E}view\u{200B}er\u{E0041}\u{FEFF}")).as_deref(),
        Some("reviewer")
    );
    // Emoji, including joined sequences and variation selectors, stay.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(launch_label(Some(family)).as_deref(), Some(family));
    assert_eq!(
        launch_label(Some("\u{1F989}")).as_deref(),
        Some("\u{1F989}")
    );
    assert_eq!(
        launch_label(Some("\u{2764}\u{FE0F}")).as_deref(),
        Some("\u{2764}\u{FE0F}")
    );
    assert_eq!(launch_label(Some("\u{200B}\u{202E}")), None);
    assert_eq!(launch_label(None), None);
}

#[test]
fn story_session_handles_keep_the_story_grammar() {
    assert_eq!(story_session_handle("SW-1", None).as_deref(), Some("SW-1"));
    assert_eq!(
        story_session_handle("SW-1", Some("QA / Bot #2")).as_deref(),
        Some("SW-1#qa-bot-2")
    );
    assert_eq!(story_session_handle("SW-1", Some("—")), None);
    let long = story_session_handle("SW-1", Some(&"ab-".repeat(40))).unwrap();
    let label = long.strip_prefix("SW-1#").unwrap();
    assert!(label.len() <= STORY_SESSION_LABEL_MAX, "{label}");
    assert!(!label.ends_with('-'));
}

#[tokio::test]
async fn role_and_avatar_are_recorded_and_listed() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 8);
    let reviewer = OpenRequest {
        role: Some("  reviewer\u{7}\n".into()),
        avatar: Some("owl".into()),
        ..named_story(Some("reviewer"), Some("S1"))
    };
    open_agent(&host, &active(), &reviewer).await.unwrap();
    let long = "x".repeat(200);
    let worker = OpenRequest {
        role: Some(long.clone()),
        avatar: Some("   ".into()),
        ..team_workspace("Sprint 1", None)
    };
    open_agent(&host, &active(), &worker).await.unwrap();
    open_agent(&host, &active(), &team_workspace("Plain", None))
        .await
        .unwrap();

    let launches = host.launches();
    assert_eq!(launches[0].record.meta.role.as_deref(), Some("reviewer"));
    assert_eq!(launches[0].record.meta.avatar.as_deref(), Some("owl"));
    assert_eq!(launches[1].record.meta.role.as_deref(), Some(&long[..64]));
    assert_eq!(launches[1].record.meta.avatar, None);

    // The roster row and list_sessions carry the same values.
    let snapshot = host.registry.snapshot();
    let row = |handle: &str| {
        session_row(
            snapshot
                .iter()
                .find(|s| s.handle == handle)
                .unwrap()
                .clone(),
            true,
        )
    };
    let roster = serde_json::to_value(row("SW-1#reviewer")).unwrap();
    assert_eq!(roster["role"], "reviewer");
    assert_eq!(roster["avatar"], "owl");
    let plain = serde_json::to_value(row(&team_workspace_handle(ORG, "default", "plain"))).unwrap();
    assert_eq!(plain["role"], serde_json::Value::Null);
    assert_eq!(plain["avatar"], serde_json::Value::Null);

    let turns = SessionTurnState::default();
    let rows = host.list(&turns, None);
    let listed = |name: &str| {
        serde_json::to_value(rows.iter().find(|r| r.display_name == name).unwrap()).unwrap()
    };
    let reviewer = listed("reviewer");
    assert_eq!(reviewer["role"], roster["role"]);
    assert_eq!(reviewer["avatar"], roster["avatar"]);
    assert!(listed("Plain").get("role").is_none());
}

/// Every field the roster row serializes is declared on the TS `AgentSessionRow`.
#[tokio::test]
async fn roster_row_fields_are_declared_on_the_ts_row() {
    let ts = include_str!("../../../../src/tauri/commands.ts");
    let start = ts
        .find("export interface AgentSessionRow {")
        .expect("AgentSessionRow is declared");
    let body = &ts[start..start + ts[start..].find("\n}\n").unwrap()];
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let req = OpenRequest {
        role: Some("worker".into()),
        avatar: Some("owl".into()),
        ..named_story(Some("worker-1"), Some("S1"))
    };
    open_agent(&host, &active(), &req).await.unwrap();
    let row = session_row(host.registry.snapshot().remove(0), true);
    let wire = serde_json::to_value(row).unwrap();
    let keys: Vec<&String> = wire.as_object().unwrap().keys().collect();
    for key in ["role", "avatar", "launchedBy", "displayName", "story"] {
        assert!(wire.get(key).is_some(), "the row serializes {key}");
    }
    for key in keys {
        assert!(
            body.contains(&format!("  {key}?:")) || body.contains(&format!("  {key}:")),
            "AgentSessionRow declares {key}"
        );
    }
}

#[tokio::test]
async fn launched_session_launches_grandchild_and_list_shows_chain() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 8);

    // You → A. A's own session → B. B's own session → C. No depth limit applies.
    let a = open_agent(&host, &active(), &team_workspace("A", None))
        .await
        .unwrap();
    let a_session = host.session_of(a.handle.as_deref().unwrap());
    let b = open_agent(&host, &active(), &team_workspace("B", Some(&a_session)))
        .await
        .unwrap();
    let b_session = host.session_of(b.handle.as_deref().unwrap());
    let c = open_agent(&host, &active(), &team_workspace("C", Some(&b_session)))
        .await
        .unwrap();
    for opened in [&a, &b, &c] {
        assert_eq!(opened.status, OpenStatus::Opened);
    }

    // Every child runs in its own dir, which carries the hand-launch config.
    for launch in host.launches() {
        let LaunchTarget::Dir { cwd, .. } = &launch.target else {
            panic!("team-workspace launches run in a dir");
        };
        assert!(
            cwd.join(".claude/settings.local.json").is_file(),
            "{}",
            cwd.display()
        );
    }
    // All three are unfocused pills.
    assert!(host.reveals().iter().all(|(_, focus)| !focus));

    let turns = SessionTurnState::default();
    let rows = host.list(&turns, None);
    let by_name = |name: &str| rows.iter().find(|r| r.display_name == name).unwrap();
    assert_eq!(by_name("A").launched_by, None);
    assert_eq!(by_name("B").launched_by, a.handle);
    assert_eq!(by_name("C").launched_by, b.handle);

    // Filtering by a session id or by its handle selects the same launches.
    let by_session = host.list(&turns, Some(&a_session));
    let by_handle = host.list(&turns, a.handle.as_deref());
    assert_eq!(by_session, by_handle);
    assert_eq!(by_session.len(), 1);
    assert_eq!(by_session[0].display_name, "B");
}

#[tokio::test]
async fn list_sessions_rows_states_and_launched_by_filter() {
    use crate::mcp::session_list::SessionState;

    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 8);
    let launcher = open_agent(&host, &active(), &team_workspace("Launcher", None))
        .await
        .unwrap();
    let s1 = host.session_of(launcher.handle.as_deref().unwrap());
    let by_s1 = open_agent(&host, &active(), &team_workspace("Child", Some(&s1)))
        .await
        .unwrap();
    open_agent(&host, &active(), &team_workspace("Mine", None))
        .await
        .unwrap();

    let turns = SessionTurnState::default();
    turns.record_hook(&s1, "Stop");
    let child_session = host.session_of(by_s1.handle.as_deref().unwrap());
    turns.record_hook(&child_session, "PermissionRequest");

    let rows = host.list(&turns, None);
    let state = |name: &str| rows.iter().find(|r| r.display_name == name).unwrap().state;
    assert_eq!(state("Launcher"), SessionState::Idle);
    assert_eq!(state("Child"), SessionState::NeedsYou);
    assert_eq!(
        state("Mine"),
        SessionState::Running,
        "no hook yet reads running"
    );

    let child = rows.iter().find(|r| r.display_name == "Child").unwrap();
    let wire = serde_json::to_value(child).unwrap();
    assert_eq!(wire["launchedBy"], serde_json::json!(launcher.handle));
    assert_eq!(wire["state"], "needs-you");
    assert!(wire["workspace"]
        .as_str()
        .unwrap()
        .ends_with("context-worktrees/acme/widgets-context/default/launcher/default/content"));

    // An exited session reads `exited` whatever its last hook said.
    let exited = session_row(host.registry.snapshot()[0].clone(), false);
    assert_eq!(
        session_list_rows(vec![exited], |sid| turns.last(sid), None)[0].state,
        SessionState::Exited
    );

    let only_s1 = host.list(&turns, Some(&s1));
    assert_eq!(
        only_s1
            .iter()
            .map(|r| r.display_name.as_str())
            .collect::<Vec<_>>(),
        ["Child"]
    );
}

#[test]
fn new_copy_says_no_weft() {
    for (file, src) in [
        ("workspace_open.rs", include_str!("../workspace_open.rs")),
        ("session_list.rs", include_str!("../../mcp/session_list.rs")),
    ] {
        assert!(!src.to_lowercase().contains("weft"), "{file} mentions Weft");
    }
    for tool in ["open_workspace", "list_sessions"] {
        let description = crate::mcp::handler::tests::tool_description(tool)
            .unwrap_or_else(|| panic!("{tool} is a registered tool"));
        assert!(!description.is_empty());
        assert!(
            !description.to_lowercase().contains("weft"),
            "the {tool} description mentions Weft"
        );
    }
}

#[test]
fn worker_slug_reduces_free_names() {
    assert_eq!(worker_slug("Sprint 1").as_deref(), Some("sprint-1"));
    assert_eq!(worker_slug("  sprint 1! ").as_deref(), Some("sprint-1"));
    assert_eq!(
        worker_slug("Q3 / release—prep").as_deref(),
        Some("q3-release-prep")
    );
    assert_eq!(worker_slug("!!!"), None);
    // A slug is always one plain dir segment, whatever the name.
    for name in ["..", "../..", "/etc/passwd", "a/../../b", ".", "~", "C:\\x"] {
        if let Some(slug) = worker_slug(name) {
            assert!(is_single_path_segment(&slug), "{name:?} → {slug:?}");
            assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        }
    }
    assert_eq!(worker_slug("/etc/passwd").as_deref(), Some("etc-passwd"));
    assert_eq!(worker_slug("../.."), None);
}

#[test]
fn only_one_plain_segment_is_a_team() {
    for ok in ["default", "Sprint Team", "a.b"] {
        assert!(is_single_path_segment(ok), "{ok}");
    }
    for bad in [
        "default/",
        "default/.",
        "",
        "..",
        ".",
        "../x",
        "a/b",
        "/abs",
        "./x",
        "x/..",
    ] {
        assert!(!is_single_path_segment(bad), "{bad}");
    }
}

#[tokio::test]
async fn legacy_open_threads_visibility_to_host_layout() {
    use crate::commands::mcp_workspace::SessionView;
    for (mode, expected) in [
        ("headed", SessionView::Tab { focus: true }),
        ("visible-unfocused", SessionView::Tab { focus: false }),
        ("headless", SessionView::Background),
    ] {
        let port = FakePort::default();
        open_workspace_call(
            &port,
            params(serde_json::json!({"story_id": "SW-1", "mode": mode})),
        )
        .await
        .unwrap();
        assert_eq!(*port.story_views.lock().unwrap(), vec![expected]);
    }
}

#[tokio::test]
async fn targeted_open_threads_visibility_to_host_layout() {
    use crate::commands::mcp_workspace::SessionView;
    for (mode, expected) in [
        ("headed", SessionView::Tab { focus: true }),
        ("visible-unfocused", SessionView::Tab { focus: false }),
        ("headless", SessionView::Background),
    ] {
        let home = tempfile::tempdir().unwrap();
        let host = FakeHost::new(home.path(), 4);
        let request = OpenRequest {
            target: "story".to_string(),
            story_id: Some("SW-1".to_string()),
            mode: Some(mode.to_string()),
            ..Default::default()
        };
        open_agent(
            &host,
            &ActiveTeam {
                org: "acme".to_string(),
                team: "default".to_string(),
            },
            &request,
        )
        .await
        .unwrap();
        assert_eq!(host.launches.lock().unwrap()[0].view, expected);
    }
}

#[test]
fn worker_reveal_uses_the_host_home_not_the_display_handle() {
    let home = crate::commands::mcp_workspace::LaunchHome::new(
        ORG,
        Some("other-team"),
        "worker-workspace",
        "agent",
        Path::new("/managed/worker"),
        crate::commands::mcp_workspace::SessionView::Tab { focus: true },
    );
    let layout = codesense_pty_core::layout::WorkspaceLayout {
        descriptor: home.descriptor,
        ..Default::default()
    };
    let payload = reveal_payload("a display handle", true, &home.workspace, &layout).unwrap();
    assert_eq!(payload["teamKey"], format!("{ORG}:other-team"));
    assert_eq!(payload["workspaceId"], "worker-workspace");
    assert_eq!(payload["cwd"], "/managed/worker");
    assert_eq!(payload["target"], "team-workspace");
}

#[tokio::test]
async fn concurrent_failed_launch_never_reports_already_live() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    *host.launch_error.lock().unwrap() = Some(LaunchError::Other("no agent CLI".into()));
    let (release, gate) = tokio::sync::oneshot::channel();
    *host.stage_gate.lock().unwrap() = Some(gate);
    let req = team_workspace("Sprint 1", None);
    let team = active();
    let (first, second) = tokio::join!(open_agent(&host, &team, &req), async {
        let second = open_agent(&host, &team, &req);
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        let _ = release.send(());
        second.await
    });
    assert_eq!(first.unwrap_err(), "no agent CLI");
    assert_eq!(second.unwrap_err(), "no agent CLI");
    assert!(host.launches().is_empty());
}

#[tokio::test]
async fn a_failed_creator_settles_before_another_launch_adopts_its_worktree() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let team = active();
    let first_request = team_workspace("First", None);
    let second_request = team_workspace("Second", None);
    let (release, gate) = tokio::sync::oneshot::channel();
    *host.launch_gate.lock().unwrap() = Some(gate);
    let first = open_agent(&host, &team, &first_request);
    tokio::pin!(first);
    assert!(futures_util::poll!(&mut first).is_pending());
    let created = crate::install::worktree_index::context_records(&host.home, &host.principal)
        .unwrap()
        .pop()
        .unwrap();
    assert!(created.launch_cwd.is_dir());
    let second = open_agent(&host, &team, &second_request);
    tokio::pin!(second);
    assert!(
        futures_util::poll!(&mut second).is_pending(),
        "another launch must not adopt a destination its creator can still roll back"
    );
    release
        .send(Err(LaunchError::Other("no agent CLI".into())))
        .unwrap();
    assert_eq!(first.await.unwrap_err(), "no agent CLI");
    assert!(!created.root.exists());
    let result = second.await.unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    let records =
        crate::install::worktree_index::context_records(&host.home, &host.principal).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(result.worktree.unwrap().id, records[0].id);
    assert!(records[0].launch_cwd.is_dir());
    assert_eq!(host.launches().len(), 1);
}

#[tokio::test]
async fn retained_worktree_launches_do_not_wait_for_another_agents_spawn() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let team = active();
    open_agent(&host, &team, &team_workspace("Owner", None))
        .await
        .unwrap();
    let first_request = team_workspace("First", None);
    let second_request = team_workspace("Second", None);
    let (release, gate) = tokio::sync::oneshot::channel();
    *host.launch_gate.lock().unwrap() = Some(gate);
    let first = open_agent(&host, &team, &first_request);
    tokio::pin!(first);
    assert!(futures_util::poll!(&mut first).is_pending());
    let second = open_agent(&host, &team, &second_request);
    tokio::pin!(second);
    let second = match futures_util::poll!(&mut second) {
        std::task::Poll::Ready(result) => result.unwrap(),
        std::task::Poll::Pending => panic!("a retained worktree must allow independent spawns"),
    };
    assert_eq!(second.status, OpenStatus::Opened);
    release
        .send(Err(LaunchError::Other("no agent CLI".into())))
        .unwrap();
    assert_eq!(first.await.unwrap_err(), "no agent CLI");
    let records =
        crate::install::worktree_index::context_records(&host.home, &host.principal).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(second.worktree.unwrap().id, records[0].id);
    assert!(records[0].launch_cwd.is_dir());
}

#[test]
fn callers_cannot_inject_trusted_account_policy() {
    assert!(
        serde_json::from_value::<crate::commands::agent_accounts::ResolveAgentAccountInput>(
            serde_json::json!({"trustedPolicy": {"requirement": "none"}})
        )
        .is_err()
    );
}

#[test]
fn retained_dead_binding_can_relaunch_but_unknown_host_cannot() {
    let state = DashboardLaunchState::new(2);
    assert!(state.begin("worker").unwrap());
    state.bind("worker", "session", None).unwrap();
    assert!(retained_handle_is_live(&state, "worker", |_| false));
    assert_eq!(state.resolve("worker").as_deref(), Some("session"));
    assert!(!retained_handle_is_live(&state, "worker", |_| true));
    assert!(state.begin("worker").unwrap());
}

#[tokio::test]
async fn a_codex_starter_keeps_the_codex_account_policy() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let req = OpenRequest {
        agent: Some("builtin/codex".into()),
        ..team_workspace("Sprint 1", None)
    };
    open_agent(&host, &active(), &req).await.unwrap();
    let launch = host.launches().pop().unwrap();
    assert_eq!(
        launch.agent.account_policy.runtime,
        codesense_pty_core::agent_cli::AgentCli::Codex
    );
    assert_eq!(
        launch.agent.account_input().runtime,
        Some(codesense_pty_core::agent_cli::AgentCli::Codex)
    );
}

/// Opens with a named agent, through the real `open_workspace_call` and the real
/// `prepare` over a temp store; only the PTY spawn is the recording host's.
fn story_on(machine: Option<&str>) -> OpenRequest {
    OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        name: Some("Worker 1".into()),
        initial_prompt: Some("Fix the flaky test".into()),
        launched_by: Some("SW-1#lead".into()),
        role: Some("worker".into()),
        avatar: Some("builtin-funny:skeptical-goat".into()),
        machine_id: machine.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn a_story_launch_runs_where_its_workspace_lives() {
    let mac = Some(WorkspacePlace::ThisMac);
    let boxed = Some(WorkspacePlace::Machine("box".into()));
    // No machine asked for: where it lives, This Mac for a new one.
    assert_eq!(story_machine("SW-1", None, None), Ok(None));
    assert_eq!(story_machine("SW-1", mac.clone(), None), Ok(None));
    assert_eq!(
        story_machine("SW-1", boxed.clone(), None),
        Ok(Some("box".into()))
    );
    // A machine asked for creates a new one there, or must be where it lives.
    assert_eq!(
        story_machine("SW-1", None, Some("box")),
        Ok(Some("box".into()))
    );
    assert_eq!(
        story_machine("SW-1", boxed.clone(), Some("box")),
        Ok(Some("box".into()))
    );
    assert!(story_machine("SW-1", mac.clone(), Some("box"))
        .unwrap_err()
        .contains("already lives on This Mac"));
    assert!(story_machine("SW-1", boxed.clone(), Some("other"))
        .unwrap_err()
        .contains("already lives on machine box"));
    // `local` is This Mac.
    assert_eq!(story_machine("SW-1", None, Some("local")), Ok(None));
    assert_eq!(story_machine("SW-1", mac, Some("local")), Ok(None));
    assert!(story_machine("SW-1", boxed, Some("local")).is_err());
}

#[tokio::test]
async fn a_story_launch_on_a_machine_goes_through_the_remote_path_with_its_identity() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);

    let result = open_agent(&host, &active(), &story_on(Some("box")))
        .await
        .unwrap();

    assert_eq!(result.status, OpenStatus::Opened, "{}", result.message);
    assert_eq!(result.handle.as_deref(), Some("remote-tab-1"));
    assert_eq!(result.name.as_deref(), Some("Worker 1"));
    assert_eq!(result.message, "Opened Worker 1 on BOX.");
    // Nothing launched on This Mac, nothing revealed locally.
    assert!(host.launches().is_empty());
    assert!(host.reveals().is_empty());
    let launches = host.remote_launches();
    assert_eq!(launches.len(), 1);
    let launch = &launches[0];
    assert_eq!(launch.machine_id, "box");
    assert_eq!(launch.story_id, "SW-1");
    assert_eq!(launch.team, active());
    assert_eq!(launch.name, "Worker 1");
    assert_eq!(
        launch.agent_key,
        crate::agents::builtin::default_key(false).to_string()
    );
    // The first prompt is the launch's own, delivered on the machine.
    assert_eq!(launch.initial_prompt.as_deref(), Some("Fix the flaky test"));
    assert_eq!(launch.launched_by.as_deref(), Some("SW-1#lead"));
    assert_eq!(launch.role.as_deref(), Some("worker"));
    assert_eq!(
        launch.avatar.as_deref(),
        Some("builtin-funny:skeptical-goat")
    );
    // A targeted open with no mode stays unfocused.
    assert!(!launch.focus);
}

#[tokio::test]
async fn a_story_living_on_a_machine_launches_there_without_naming_it() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    host.place("SW-1", WorkspacePlace::Machine("box".into()));
    let result = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("headed".into()),
            ..story_on(None)
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert!(host.launches().is_empty());
    let launch = &host.remote_launches()[0];
    assert_eq!(launch.machine_id, "box");
    assert!(launch.focus);
}

#[tokio::test]
async fn a_machine_that_is_not_where_the_workspace_lives_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    host.place("SW-1", WorkspacePlace::ThisMac);
    let result = open_agent(&host, &active(), &story_on(Some("box")))
        .await
        .unwrap();
    assert_eq!(result.status, OpenStatus::Invalid);
    assert!(result.message.contains("already lives on This Mac"));
    assert!(host.launches().is_empty());
    assert!(host.remote_launches().is_empty());

    // `local` keeps it on This Mac.
    let local = open_agent(&host, &active(), &story_on(Some("local")))
        .await
        .unwrap();
    assert_eq!(local.status, OpenStatus::Opened);
    assert_eq!(local.handle.as_deref(), Some("SW-1#worker-1"));
    assert!(host.remote_launches().is_empty());
}

#[tokio::test]
async fn a_team_folder_or_background_launch_is_not_offered_on_a_machine() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let folder = open_agent(
        &host,
        &active(),
        &OpenRequest {
            machine_id: Some("box".into()),
            ..team_workspace("Sprint 1", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(folder.status, OpenStatus::Invalid);
    assert!(folder.message.contains("a team folder runs on This Mac"));

    let headless = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("headless".into()),
            ..story_on(Some("box"))
        },
    )
    .await
    .unwrap();
    assert_eq!(headless.status, OpenStatus::Invalid);
    assert!(headless.message.contains("omit headless"));
    assert!(host.launches().is_empty());
    assert!(host.remote_launches().is_empty());
}

#[tokio::test]
async fn an_agent_already_running_on_the_machine_is_already_live() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    *host.remote_already_live.lock().unwrap() = Some("remote-tab-old".into());
    let result = open_agent(&host, &active(), &story_on(Some("box")))
        .await
        .unwrap();
    assert_eq!(result.status, OpenStatus::AlreadyLive);
    assert_eq!(result.handle.as_deref(), Some("remote-tab-old"));

    *host.remote_already_live.lock().unwrap() = None;
    *host.remote_error.lock().unwrap() = Some("agentUnavailable: claude is not installed".into());
    let failed = open_agent(&host, &active(), &story_on(Some("box"))).await;
    assert_eq!(
        failed.unwrap_err(),
        "agentUnavailable: claude is not installed"
    );
}

#[test]
fn waiting_for_a_workspace_on_a_machine_reads_the_viewer() {
    let wait = |viewer: serde_json::Value, asked: bool, seen: bool| {
        machine_workspace_wait(&viewer, "acme:core", "SW-1", "box", asked, seen)
    };
    let row = |location: serde_json::Value, status: &str| {
        serde_json::json!({ "workspaces": { "acme:core": [
            { "id": "SW-1-fix", "kind": "story", "storyId": "SW-1", "cwd": "", "status": status,
              "statusError": "copy failed", "location": location }
        ]}})
    };
    let creating = serde_json::json!({ "kind": "machine", "machineId": "box", "requestId": "r" });
    let ready = serde_json::json!({ "kind": "machine", "machineId": "box", "requestId": "r",
        "handle": "h1", "cwd": "/home/dev/wt/sw-1" });
    // None yet: ask the viewer once, then wait for it.
    assert_eq!(
        wait(serde_json::json!({}), false, false),
        MachineWorkspaceWait::Ask
    );
    assert_eq!(
        wait(serde_json::json!({}), true, false),
        MachineWorkspaceWait::Pending
    );
    assert_eq!(
        wait(row(creating.clone(), "creating"), true, true),
        MachineWorkspaceWait::Pending
    );
    assert_eq!(
        wait(row(ready.clone(), "ready"), true, true),
        MachineWorkspaceWait::Ready {
            workspace_id: "SW-1-fix".into(),
            handle: "h1".into(),
            cwd: "/home/dev/wt/sw-1".into()
        }
    );
    assert!(matches!(
        wait(row(creating, "error"), true, true),
        MachineWorkspaceWait::Failed(why) if why.contains("copy failed")
    ));
    // Closed while it was being created.
    assert!(matches!(
        wait(serde_json::json!({}), true, true),
        MachineWorkspaceWait::Failed(_)
    ));
    // Living on This Mac is never launched on the machine.
    assert!(matches!(
        wait(row(serde_json::Value::Null, "ready"), false, false),
        MachineWorkspaceWait::Failed(why) if why.contains("This Mac")
    ));
    // A minimized one is brought back first.
    let minimized = serde_json::json!({ "softClosedWorkspaces": { "acme:core": { "SW-1-fix":
        { "workspace": { "id": "SW-1-fix", "kind": "story", "storyId": "SW-1", "location": ready } } } } });
    assert_eq!(
        wait(minimized.clone(), false, true),
        MachineWorkspaceWait::Ask
    );
    assert_eq!(wait(minimized, true, true), MachineWorkspaceWait::Pending);
}

pub(crate) mod agent_opens {
    use std::sync::Arc;

    use super::*;
    use crate::agents::model::Agent;
    use crate::agents::resolve::tests::{alice, content, draft, save, team_content};
    use crate::install::agent_account_store::AgentProvider;
    use codesense_pty_core::agent_cli::AgentCli;

    const AGENT_ORG: &str = crate::agents::resolve::tests::ORG;
    const AGENT_TEAM: &str = crate::agents::resolve::tests::TEAM;

    /// A recording host shared with the test, so the verb can own its box.
    pub(super) struct SharedHost(Arc<FakeHost>);

    #[async_trait::async_trait]
    impl OpenHost for SharedHost {
        async fn story_workspace_place(
            &self,
            team: &ActiveTeam,
            story_id: &str,
        ) -> Option<WorkspacePlace> {
            self.0.story_workspace_place(team, story_id).await
        }
        async fn launch_remote(&self, launch: RemoteLaunch) -> Result<RemoteLaunched, String> {
            self.0.launch_remote(launch).await
        }
        async fn story_team(
            &self,
            story_id: &str,
            org: Option<&str>,
        ) -> Result<ActiveTeam, String> {
            self.0.story_team(story_id, org).await
        }
        async fn team_content_root(&self, org: &str, team: &str) -> Result<PathBuf, String> {
            self.0.team_content_root(org, team).await
        }
        fn is_live(&self, handle: &str) -> bool {
            self.0.is_live(handle)
        }
        async fn claim_open(&self, handle: &str) -> OpenClaim {
            self.0.claim_open(handle).await
        }
        async fn prepare_context_worktree(
            &self,
            scope: &ActiveTeam,
            content: PathBuf,
            name: &str,
            requested: Option<&str>,
            create: bool,
            agent: &AgentLaunch,
        ) -> Result<context::ContextDestination, String> {
            self.0
                .prepare_context_worktree(scope, content, name, requested, create, agent)
                .await
        }
        async fn rollback_context_worktree(&self, destination: &context::ContextDestination) {
            self.0.rollback_context_worktree(destination).await
        }
        async fn prepare_agent(
            &self,
            reference: &str,
            team: &ActiveTeam,
            prompt: Option<&str>,
            mode: Option<adapter::LaunchMode>,
        ) -> Result<AgentLaunch, String> {
            self.0.prepare_agent(reference, team, prompt, mode).await
        }
        async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, LaunchError> {
            self.0.launch(launch).await
        }
        fn reveal(&self, handle: &str, focus: bool) {
            self.0.reveal(handle, focus)
        }
        async fn show(&self, handle: &str) -> RevealOutcome {
            self.0.show(handle).await
        }
    }

    pub(super) struct HostPort(pub(super) Arc<FakeHost>);

    #[async_trait::async_trait]
    impl OpenWorkspacePort for HostPort {
        async fn launch_story(
            &self,
            _story_id: &str,
            _org: Option<&str>,
            _initial_prompt: Option<&str>,
            _view: crate::commands::mcp_workspace::SessionView,
        ) -> Result<crate::commands::mcp_workspace::LaunchOutcome, String> {
            panic!("an agent open never takes the legacy story path")
        }
        async fn targeted(&self) -> Result<(ActiveTeam, Box<dyn OpenHost>), String> {
            let active = ActiveTeam {
                org: AGENT_ORG.to_string(),
                team: AGENT_TEAM.to_string(),
            };
            Ok((active, Box::new(SharedHost(self.0.clone()))))
        }
        fn reveal(&self, _handle: &str, _focus: bool) {}
    }

    /// A team with a saved agent, and the host and port that open it.
    fn setup(home: &Path, agent: Agent) -> (Arc<FakeHost>, HostPort, crate::agents::AgentKey) {
        let team = team_content(home, &alice());
        let repo = git2::Repository::init(team.parent().unwrap().parent().unwrap()).unwrap();
        let key = save(home, &alice(), agent);
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.test").unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &[],
        )
        .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let mut host = FakeHost::new(home, 4);
        host.principal = alice();
        host.content_roots
            .insert(AGENT_TEAM.to_string(), content(home, &alice(), AGENT_TEAM));
        host.story_teams.insert(
            "SW-9".to_string(),
            ActiveTeam {
                org: AGENT_ORG.to_string(),
                team: AGENT_TEAM.to_string(),
            },
        );
        let host = Arc::new(host);
        (host.clone(), HostPort(host), key)
    }

    #[tokio::test]
    async fn hermes_is_not_a_remote_launch_choice_through_open_workspace() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let home = tempfile::tempdir().unwrap();
        // Listed, but its desktop adapter launches it on this Mac only.
        assert!(!crate::agents::adapter::desktop(AgentCli::Hermes).supports_remote_launch);
        // A Hermes with its model, so only the remote launch can refuse it.
        let mut agent = draft("hermes-preview", AgentCli::Hermes);
        agent.startup.clear();
        agent.context = Some(Vec::new());
        agent.mcp.servers.clear();
        agent.set_startup_settings(Some(std::collections::BTreeMap::from([("model".into(), serde_json::json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"local-model"}))])));
        let (host, port, key) = setup(home.path(), agent);
        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "storyId": "SW-9", "agent": key.to_string(), "machineId": "box"
            })),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "invalid", "{result}");
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("does not support remote launches yet"),
            "{result}"
        );
        assert!(host.launches().is_empty());
        assert!(host.remote_launches().is_empty());
        assert_eq!(host.worktree_creations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_explicit_org_is_the_org_the_story_is_checked_and_launched_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, key) = setup(
            tmp.path(),
            Agent {
                default_prompt: "Review the story.".into(),
                ..draft("reviewer", AgentCli::Codex)
            },
        );
        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "story_id": "SW-9", "agent": key.to_string(), "org": AGENT_ORG,
            })),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "opened", "{result}");
        let hints = host.story_org_hints.lock().unwrap().clone();
        assert!(!hints.is_empty());
        assert!(
            hints.iter().all(|hint| hint.as_deref() == Some(AGENT_ORG)),
            "{hints:?}"
        );
        let launch = host.launches().remove(0);
        assert!(
            matches!(&launch.target, LaunchTarget::Story { team, .. } if team.org == AGENT_ORG),
            "{:?}",
            launch.target
        );

        // The story is looked up in the named org only.
        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "story_id": "SW-9", "name": "other", "org": "elsewhere/org",
            })),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "invalid", "{result}");
        assert_eq!(host.launches().len(), 1, "nothing more launched");
    }

    #[tokio::test]
    async fn a_legacy_open_refuses_a_team_other_than_the_storys() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("dev", AgentCli::Claude));
        let error = open_workspace_call(
            &port,
            params(serde_json::json!({"story_id": "SW-9", "team": "frontend"})),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            format!("invalid: story SW-9 is in team {AGENT_TEAM}, not frontend")
        );
        assert!(host.launches().is_empty());
    }

    pub(crate) async fn registered_producers(
        launch: &AgentLaunch,
        probe: RegisteredSpawnProbe,
    ) -> Vec<(String, Launch)> {
        let (host, port, _) = setup(
            &launch.startup_context.home,
            draft("registered-seat", AgentCli::Claude),
        );
        let key = &launch.agent.key;
        *host.registered_test_launch.lock().unwrap() = Some(launch.clone());
        *host.registered_spawn_probe.lock().unwrap() = Some(probe);
        let mut inputs = Vec::new();
        for (site, request) in [
            (
                "open_workspace team",
                serde_json::json!({
                    "target": "team-workspace", "name": "MCP managed launch",
                    "agent": key.to_string(), "worktree": "hermes-mcp-parity",
                    "createWorktree": true
                }),
            ),
            (
                "open_workspace story",
                serde_json::json!({
                    "target": "story", "storyId": "SW-9", "name": "Story managed launch",
                    "agent": key.to_string()
                }),
            ),
        ] {
            let result = open_workspace_call(&port, params(request)).await.unwrap();
            assert_eq!(result["status"], "opened", "{site}: {result}");
            inputs.push((site.into(), host.launches().last().unwrap().clone()));
        }
        let result = launch_from_ui(
            &port,
            UiLaunchArgs {
                target: "team-workspace".into(),
                name: Some("UI launch".into()),
                agent: Some(key.to_string()),
                worktree: Some("hermes-ui-parity".into()),
                create_worktree: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.status, OpenStatus::Opened, "{}", result.message);
        inputs.push((
            "agent_launch_open / Launch sheet".into(),
            host.launches().last().unwrap().clone(),
        ));
        host.place("SW-9", WorkspacePlace::Machine("test-machine".into()));
        let remote = open_agent(
            &SharedHost(host.clone()),
            &ActiveTeam {
                org: AGENT_ORG.into(),
                team: AGENT_TEAM.into(),
            },
            &OpenRequest {
                target: "story".into(),
                story_id: Some("SW-9".into()),
                name: Some("Remote refusal".into()),
                agent: Some(key.to_string()),
                machine_id: Some("test-machine".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(remote.status, OpenStatus::Invalid);
        assert!(
            remote.message.contains("does not support remote launches"),
            "{}",
            remote.message
        );
        assert!(host.remote_launches().is_empty());
        inputs
    }

    #[cfg(feature = "dashboard-publish")]
    #[tokio::test]
    async fn hosted_agent_launch_keeps_source_caller_before_preparation() {
        use crate::context_repo::caller_scope::tests::Fixture;
        use crate::dashboard_publish::{actions, state::Scope};
        let f = Fixture::new();
        let me = crate::domain::Principal::from(1u64);
        let hidden = f.agent("parent", "runner");
        let own = f.agent("child", "runner");
        let mut host = FakeHost::new(&f.home, 8);
        host.principal = me.clone();
        host.content_roots
            .insert("child".into(), f.path("child", "content"));
        host.content_roots
            .insert("parent".into(), f.path("parent", "content"));
        let host = Arc::new(host);
        let port = HostPort(host.clone());
        let a = |agent: String| {
            serde_json::from_value(serde_json::json!({"verb":"launchAgent","who":"owner","agent":agent,"prompt":"Fix {id}","mode":"headless"})).unwrap()
        };
        let scope = Scope {
            org: "acme/ctx".into(),
            team: "child".into(),
        };
        let denied = actions::launch(
            &port,
            &a(hidden.to_string()),
            "BLOCKED",
            &scope,
            "caller",
            false,
            (&f.home, &me, &f.scope),
        )
        .await
        .unwrap_err();
        assert_eq!(
            denied,
            actions::launch(
                &port,
                &a("acme/ctx/parent/absent".into()),
                "BLOCKED",
                &scope,
                "caller",
                false,
                (&f.home, &me, &f.scope)
            )
            .await
            .unwrap_err()
        );
        assert_eq!(host.stagings.load(Ordering::SeqCst), 0);
        assert!(host.launches().is_empty());
        // An admitted own definition gets as far as the real preparation callback.
        // This fixture deliberately stops there rather than creating a workspace.
        let (tx, rx) = tokio::sync::oneshot::channel();
        *host.stage_gate.lock().unwrap() = Some(rx);
        let own_action = a(own.to_string());
        let operation = actions::launch(
            &port,
            &own_action,
            "OWN",
            &scope,
            "caller",
            false,
            (&f.home, &me, &f.scope),
        );
        tokio::pin!(operation);
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        assert!(std::future::Future::poll(operation.as_mut(), &mut context).is_pending());
        assert!(
            host.stage_gate.lock().unwrap().is_none(),
            "admitted definition reached preparation"
        );
        drop(operation);
        drop(tx);
        assert!(f
            .scope
            .read_admitted(&f.path("parent", "content/shared.md"))
            .is_ok());
    }

    #[cfg(feature = "dashboard-publish")]
    #[tokio::test]
    async fn dashboard_publish_actions_launch_through_the_port_and_fall_back_without_a_worktree() {
        use crate::dashboard_publish::{actions, state::Scope};
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let a = serde_json::from_value(serde_json::json!({"verb":"launchAgent","who":"owner","agent":"triage","prompt":"Fix {id}","mode":"headless"})).unwrap();
        let scope = Scope {
            org: AGENT_ORG.into(),
            team: AGENT_TEAM.into(),
        };
        for (id, has_worktree) in [("SW-9", true), ("BUG-1", false)] {
            let result = actions::launch(
                &port,
                &a,
                id,
                &scope,
                "triage",
                has_worktree,
                (
                    tmp.path(),
                    &alice(),
                    &crate::context_repo::caller_scope::CallerScope::Human,
                ),
            )
            .await
            .unwrap();
            assert!(result["handle"].is_string());
            let launch = host.launches().pop().unwrap();
            assert_eq!(
                launch.initial_prompt.as_deref(),
                Some(format!("Fix {id}").as_str())
            );
            assert_eq!(launch.record.meta.role.as_deref(), Some("dash:triage"));
            assert_eq!(
                launch.agent.launch.mode,
                crate::agents::adapter::LaunchMode::Headless
            );
            assert_eq!(
                matches!(launch.target, LaunchTarget::Story { .. }),
                has_worktree
            );
        }
        assert_eq!(host.remote_launches().len(), 0);
    }
    #[tokio::test]
    async fn max_runtime_call_arms_the_bound_launch_and_reopen_preserves_its_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        for (name, budget) in [("bounded", Some("30m")), ("unbounded", None)] {
            let call = |budget: Option<&str>| {
                params(serde_json::json!({
                    "target": "story", "storyId": "SW-9", "name": name,
                    "agent": "triage", "maxRuntime": budget,
                }))
            };
            let opened = open_workspace_call(&port, call(budget)).await.unwrap();
            assert_eq!(opened["status"], "opened", "{opened}");
            let handle = opened["handle"].as_str().unwrap();
            let before = host
                .registry
                .snapshot()
                .into_iter()
                .find(|row| row.handle == handle)
                .unwrap();
            let launch = host.launches().into_iter().last().unwrap();
            assert_eq!(launch.record.max_runtime_ms, budget.map(|_| 30 * 60_000));
            assert_eq!(before.deadline_at_ms.is_some(), budget.is_some());
            if let Some(deadline) = before.deadline_at_ms {
                let tags = host
                    .registry
                    .launch_tags_for_session(before.session_id.as_deref().unwrap())
                    .unwrap();
                let bound_at: u64 = tags["launchBoundAt"].as_deref().unwrap().parse().unwrap();
                assert_eq!(deadline, bound_at + 30 * 60_000);
            }

            let reopened = open_workspace_call(&port, call(Some("2h"))).await.unwrap();
            assert_eq!(reopened["status"], "alreadyLive", "{reopened}");
            assert!(reopened["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["message"] == "maxRuntime ignored: session already live"));
            let after = host
                .registry
                .snapshot()
                .into_iter()
                .find(|row| row.handle == handle)
                .unwrap();
            assert_eq!(after.session_id, before.session_id);
            assert_eq!(after.deadline_at_ms, before.deadline_at_ms);
        }
        assert_eq!(host.launches().len(), 2, "reopening never launches again");
    }

    #[tokio::test]
    async fn max_runtime_call_rejects_invalid_and_unsupported_requests_before_launch() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        for budget in ["0m", "25h", "abc", "1.5h", "30s"] {
            let result = open_workspace_call(
                &port,
                params(serde_json::json!({
                    "target": "story", "storyId": "SW-9", "agent": "triage",
                    "maxRuntime": budget,
                })),
            )
            .await
            .unwrap();
            assert_eq!(result["status"], "invalid", "{result}");
        }
        let legacy = open_workspace_call(
            &port,
            params(serde_json::json!({
                "storyId": "SW-9", "maxRuntime": "30m",
            })),
        )
        .await
        .unwrap();
        assert_eq!(legacy["status"], "invalid", "{legacy}");
        assert!(legacy["message"]
            .as_str()
            .unwrap()
            .contains("maxRuntime requires target"));

        let explicit_remote = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "storyId": "SW-9", "machineId": "box",
                "agent": "triage", "maxRuntime": "30m",
            })),
        )
        .await
        .unwrap();
        assert_eq!(explicit_remote["status"], "invalid", "{explicit_remote}");
        assert!(explicit_remote["message"]
            .as_str()
            .unwrap()
            .contains("This Mac only"));

        host.place("SW-9", WorkspacePlace::Machine("box".into()));
        let resolved_remote = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "storyId": "SW-9", "agent": "triage",
                "maxRuntime": "30m",
            })),
        )
        .await
        .unwrap();
        assert_eq!(resolved_remote["status"], "invalid", "{resolved_remote}");
        assert!(resolved_remote["message"]
            .as_str()
            .unwrap()
            .contains("This Mac only"));
        assert!(host.launches().is_empty());
        assert!(host.remote_launches().is_empty());
        assert!(host.registry.snapshot().is_empty());
        assert_eq!(host.worktree_creations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_ui_launch_opens_a_focused_named_tab_under_its_manager() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let args = UiLaunchArgs {
            queue_role: None,
            target: "story".into(),
            story_id: Some("SW-9".into()),
            name: Some("Repro bot".into()),
            agent: Some("triage".into()),
            initial_prompt: Some("Reproduce the timeout.".into()),
            launched_by: Some("SW-9#lead".into()),
            role: Some("qa".into()),
            avatar: Some("builtin-funny:skeptical-goat".into()),
            machine_id: None,
            org: None,
            team: None,
            worktree: None,
            create_worktree: None,
        };

        let result = launch_from_ui(&port, args).await.unwrap();

        assert_eq!(result.status, OpenStatus::Opened);
        assert_eq!(result.handle.as_deref(), Some("SW-9#repro-bot"));
        let launch = &host.launches()[0];
        assert_eq!(
            launch.initial_prompt.as_deref(),
            Some("Reproduce the timeout.")
        );
        assert_eq!(launch.record.meta.launched_by.as_deref(), Some("SW-9#lead"));
        assert_eq!(launch.record.meta.role.as_deref(), Some("qa"));
        assert_eq!(
            launch.record.meta.avatar.as_deref(),
            Some("builtin-funny:skeptical-goat")
        );
        assert_eq!(
            launch.record.meta.display_name.as_deref(),
            Some("Repro bot")
        );
        // The person is watching: the new tab is shown and takes focus.
        assert_eq!(host.reveals(), vec![("SW-9#repro-bot".to_string(), true)]);
    }

    #[tokio::test]
    async fn a_ui_launch_on_a_machine_runs_the_saved_agent_there_focused() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, key) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let args = UiLaunchArgs {
            queue_role: None,
            target: "story".into(),
            story_id: Some("SW-9".into()),
            name: Some("Repro bot".into()),
            agent: Some("triage".into()),
            initial_prompt: Some("Reproduce the timeout.".into()),
            launched_by: Some("SW-9#lead".into()),
            role: Some("qa".into()),
            avatar: None,
            machine_id: Some("box".into()),
            org: None,
            team: None,
            worktree: None,
            create_worktree: None,
        };
        let result = launch_from_ui(&port, args).await.unwrap();
        assert_eq!(result.status, OpenStatus::Opened, "{}", result.message);
        assert!(host.launches().is_empty());
        let launch = &host.remote_launches()[0];
        assert_eq!(launch.agent_key, key.to_string());
        assert_eq!(launch.machine_id, "box");
        assert_eq!(
            launch.initial_prompt.as_deref(),
            Some("Reproduce the timeout.")
        );
        // The person is watching.
        assert!(launch.focus);
    }

    #[tokio::test]
    async fn an_agent_open_on_a_machine_over_mcp_takes_the_targeted_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "story_id": "SW-9", "machine_id": "box",
                "agent": "triage", "name": "Worker", "mode": "headed"
            })),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "opened");
        assert_eq!(host.remote_launches()[0].machine_id, "box");
        assert!(host.launches().is_empty());
    }

    #[tokio::test]
    async fn a_ui_launch_without_a_usable_target_is_invalid_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let result = launch_from_ui(
            &port,
            UiLaunchArgs {
                target: "team-workspace".into(),
                ..UiLaunchArgs::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.status, OpenStatus::Invalid);
        assert!(
            result.message.contains("name is required"),
            "{}",
            result.message
        );
        assert!(host.launches().is_empty());
    }

    #[tokio::test]
    async fn the_verb_opens_named_story_sessions_with_role_and_avatar() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let open = |name: &str, role: &str, avatar: Option<&str>| {
            params(serde_json::json!({
                "target": "story", "story_id": "SW-9", "name": name,
                "role": role, "avatar": avatar, "launched_by": "S1",
            }))
        };
        for (name, role, avatar) in [
            ("worker-1", "worker", None),
            ("worker-2", "worker", None),
            ("reviewer", "reviewer", Some("owl")),
        ] {
            let result = open_workspace_call(&port, open(name, role, avatar))
                .await
                .unwrap();
            assert_eq!(result["status"], "opened", "{name}");
            assert_eq!(result["handle"], format!("SW-9#{name}"));
        }
        let again = open_workspace_call(&port, open("worker-1", "worker", None))
            .await
            .unwrap();
        assert_eq!(again["status"], "alreadyLive");

        let rows = host.list(&SessionTurnState::default(), Some("S1"));
        let wire: Vec<_> = rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        assert_eq!(wire.len(), 3);
        assert!(wire.iter().all(|r| r["story"] == "SW-9"));
        let reviewer = wire
            .iter()
            .find(|r| r["handle"] == "SW-9#reviewer")
            .unwrap();
        assert_eq!(
            (&reviewer["role"], &reviewer["avatar"]),
            (&"reviewer".into(), &"owl".into())
        );
        let worker = wire
            .iter()
            .find(|r| r["handle"] == "SW-9#worker-1")
            .unwrap();
        assert_eq!(worker["role"], "worker");
        assert!(worker.get("avatar").is_none());
    }

    #[tokio::test]
    async fn an_agent_open_launches_the_prepared_agent_and_returns_its_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, key) = setup(tmp.path(), draft("triage", AgentCli::Claude));

        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "team-workspace",
                "name": "Triage",
                "agent": "triage",
                "initial_prompt": "/missing please",
            })),
        )
        .await
        .unwrap();

        assert_eq!(result["status"], "opened");
        assert_eq!(result["warnings"][0]["code"], "command_not_in_manifest");
        assert!(result["warnings"][0]["message"]
            .as_str()
            .unwrap()
            .contains("/missing"));
        let launches = host.launches();
        let [launch] = launches.as_slice() else {
            panic!("one launch, got {launches:?}");
        };
        let agent = &launch.agent;
        let profile = &agent.launch;
        assert_eq!(profile.id, key.to_string());
        assert_eq!(profile.binary, "claude");
        assert_eq!(profile.args, vec!["--model", "opus"]);
        assert_eq!(profile.mcp_allow, Some(vec!["slack".to_string()]));
        assert_eq!(agent.account_policy.provider(), Some(AgentProvider::Claude));
        assert_eq!(agent.revision_label(), "acme/api/backend/triage@1");
        let provenance = &launch.provenance;
        assert_eq!(
            provenance.agent.as_deref(),
            Some("acme/api/backend/triage@1")
        );
        assert_eq!(provenance.manifest_hash, Some(agent.manifest_hash.clone()));
        assert!(!launch
            .agent
            .launch
            .args
            .iter()
            .any(|arg| arg == "--add-dir"));
        assert_eq!(launch.initial_prompt.as_deref(), Some("/missing please"));
    }

    /// A starter opens through the same spine as any agent: its runtime's
    /// defaults, no MCP server beyond the bridge, and its fixed revision on
    /// the launch's provenance. A bare `claude` finds it when no team or
    /// personal agent has that id.
    #[tokio::test]
    async fn a_starter_opens_by_key_or_bare_id_and_records_its_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let stamp = format!("builtin/claude@{}", crate::agents::builtin::revision());

        for (name, agent) in [("Starter", "builtin/claude"), ("Bare", "claude")] {
            let result = open_workspace_call(
                &port,
                params(serde_json::json!({
                    "target": "team-workspace", "name": name, "agent": agent,
                })),
            )
            .await
            .unwrap();
            assert_eq!(result["status"], "opened", "{result}");
            let launches = host.launches();
            let launch = launches.last().unwrap();
            let profile = &launch.agent.launch;
            assert_eq!(profile.id, "builtin/claude");
            assert_eq!(profile.binary, "claude");
            assert_eq!(profile.mcp_allow, Some(Vec::new()));
            assert_eq!(
                launch.agent.account_policy.provider(),
                Some(AgentProvider::Claude)
            );
            assert_eq!(launch.agent.revision_label(), stamp);
            assert_eq!(launch.provenance.agent.as_deref(), Some(stamp.as_str()));
            assert_eq!(launch.initial_prompt, None, "the user types the task");
        }
    }

    #[tokio::test]
    async fn a_codex_agent_opens_a_story_on_its_own_runtime_and_account_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, key) = setup(
            tmp.path(),
            Agent {
                default_prompt: "Review the story.".into(),
                ..draft("reviewer", AgentCli::Codex)
            },
        );

        let result = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "story", "story_id": "SW-9", "agent": key.to_string(),
            })),
        )
        .await
        .unwrap();

        assert_eq!(result["status"], "opened");
        let launch = host.launches().remove(0);
        assert_eq!(
            launch.target,
            LaunchTarget::Story {
                story_id: "SW-9".into(),
                handle: "SW-9".into(),
                team: ActiveTeam {
                    org: AGENT_ORG.to_string(),
                    team: AGENT_TEAM.to_string(),
                },
            }
        );
        // No prompt of its own: the agent starts with its default one.
        assert_eq!(launch.initial_prompt.as_deref(), Some("Review the story."));
        assert_eq!(launch.agent.launch.binary, "codex");
        assert_eq!(
            launch.agent.account_policy.provider(),
            Some(AgentProvider::Codex)
        );
        assert_eq!(
            launch.provenance.agent.as_deref(),
            Some("acme/api/backend/reviewer@1")
        );
    }

    #[tokio::test]
    async fn unknown_agents_and_targetless_agents_are_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));

        let unknown = open_workspace_call(
            &port,
            params(serde_json::json!({
                "target": "team-workspace", "name": "Triage", "agent": "nope",
            })),
        )
        .await
        .unwrap();
        assert_eq!(unknown["status"], "invalid");
        assert!(unknown["message"]
            .as_str()
            .unwrap()
            .contains("no agent \"nope\""));

        // A retired pack/profile recipe is an unknown argument, never silently dropped for
        // the default agent.
        for retired in ["pack", "profile"] {
            let mut call = serde_json::json!({
                "target": "story", "story_id": "SW-9", "agent": "triage",
            });
            call[retired] = serde_json::json!("codex");
            assert!(
                serde_json::from_value::<crate::mcp::handler::OpenWorkspaceParams>(call).is_err(),
                "{retired} is refused"
            );
        }

        let targetless = open_workspace_call(
            &port,
            params(serde_json::json!({"story_id": "SW-9", "agent": "triage"})),
        )
        .await
        .unwrap();
        assert_eq!(targetless["status"], "invalid");
        assert!(host.launches().is_empty());
    }

    /// The caller's explicit mode decides the agent's mode (and so its generated
    /// permissions); with none, the agent's declared mode applies.
    #[tokio::test]
    async fn an_agent_open_runs_in_the_callers_mode_else_the_agents_own() {
        use crate::agents::adapter::LaunchMode;
        use crate::agents::model::LaunchMode as Declared;
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(
            tmp.path(),
            Agent {
                launch: Declared::Headless,
                ..draft("triage", AgentCli::Claude)
            },
        );
        let cases = [
            (None, None, LaunchMode::Headless),
            (Some("headed"), None, LaunchMode::Interactive),
            (Some("headless"), None, LaunchMode::Headless),
            (None, Some(true), LaunchMode::Interactive),
        ];
        for (i, (mode, headed, expected)) in cases.into_iter().enumerate() {
            let mut call = serde_json::json!({
                "target": "team-workspace", "name": format!("Triage {i}"), "agent": "triage",
            });
            if let Some(mode) = mode {
                call["mode"] = serde_json::json!(mode);
            }
            if let Some(headed) = headed {
                call["headed"] = serde_json::json!(headed);
            }
            let result = open_workspace_call(&port, params(call)).await.unwrap();
            assert_eq!(result["status"], "opened", "{mode:?} {headed:?}: {result}");
            let launch = host.launches().pop().unwrap();
            let agent = &launch.agent;
            assert_eq!(agent.launch.mode, expected, "{mode:?} {headed:?}");
            let default_mode = &agent.generated[0].content["permissions"]["defaultMode"];
            assert_eq!(
                default_mode.as_str() == Some("dontAsk"),
                expected == LaunchMode::Headless,
                "{mode:?} {headed:?}: {default_mode}"
            );
        }
    }

    #[tokio::test]
    async fn an_open_without_an_agent_records_the_preferred_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let (host, port, _) = setup(tmp.path(), draft("triage", AgentCli::Claude));
        let result = open_workspace_call(
            &port,
            params(serde_json::json!({"target": "team-workspace", "name": "Plain"})),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "opened");
        assert!(result.get("warnings").is_none());
        let launch = host.launches().remove(0);
        let stamp = format!("builtin/claude@{}", crate::agents::builtin::revision());
        assert_eq!(launch.agent.revision_label(), stamp);
        assert_eq!(launch.provenance.agent.as_deref(), Some(stamp.as_str()));
    }
}

/// "Describe it to an agent" (agents and routines) launches through the real open path with
/// nothing but CodeSense in the team: no skill file anywhere, and the first prompt is a plain
/// request naming the MCP tools that carry the whole contract.
mod authoring_opens {
    use super::*;
    use std::sync::Arc;

    struct Shared(Arc<FakeHost>);

    #[async_trait::async_trait]
    impl OpenHost for Shared {
        async fn story_team(
            &self,
            story_id: &str,
            org: Option<&str>,
        ) -> Result<ActiveTeam, String> {
            self.0.story_team(story_id, org).await
        }
        fn is_live(&self, handle: &str) -> bool {
            self.0.is_live(handle)
        }
        async fn claim_open(&self, handle: &str) -> OpenClaim {
            self.0.claim_open(handle).await
        }
        async fn team_content_root(&self, org: &str, team: &str) -> Result<PathBuf, String> {
            self.0.team_content_root(org, team).await
        }
        async fn prepare_context_worktree(
            &self,
            scope: &ActiveTeam,
            content: PathBuf,
            name: &str,
            requested: Option<&str>,
            create: bool,
            agent: &AgentLaunch,
        ) -> Result<context::ContextDestination, String> {
            self.0
                .prepare_context_worktree(scope, content, name, requested, create, agent)
                .await
        }
        async fn rollback_context_worktree(&self, destination: &context::ContextDestination) {
            self.0.rollback_context_worktree(destination).await
        }
        async fn prepare_agent(
            &self,
            reference: &str,
            team: &ActiveTeam,
            prompt: Option<&str>,
            mode: Option<adapter::LaunchMode>,
        ) -> Result<AgentLaunch, String> {
            self.0.prepare_agent(reference, team, prompt, mode).await
        }
        async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, LaunchError> {
            self.0.launch(launch).await
        }
        fn reveal(&self, handle: &str, focus: bool) {
            self.0.reveal(handle, focus)
        }
        async fn show(&self, handle: &str) -> RevealOutcome {
            self.0.show(handle).await
        }
    }

    struct Port(Arc<FakeHost>);

    #[async_trait::async_trait]
    impl OpenWorkspacePort for Port {
        async fn launch_story(
            &self,
            _story_id: &str,
            _org: Option<&str>,
            _initial_prompt: Option<&str>,
            _view: crate::commands::mcp_workspace::SessionView,
        ) -> Result<crate::commands::mcp_workspace::LaunchOutcome, String> {
            panic!("an authoring open never takes the legacy story path")
        }
        async fn targeted(&self) -> Result<(ActiveTeam, Box<dyn OpenHost>), String> {
            Ok((active(), Box::new(Shared(self.0.clone()))))
        }
        fn reveal(&self, _handle: &str, _focus: bool) {}
    }

    fn no_skill_anywhere(host: &FakeHost) {
        let skills = host.content_root().join(".claude/skills");
        assert!(!skills.exists(), "the team has no skills at all");
    }

    #[tokio::test]
    async fn agent_authoring_opens_with_no_skill_and_a_plain_prompt() {
        let home = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::new(home.path(), 4));
        no_skill_anywhere(&host);
        let reply = crate::commands::agents::open_authoring_agent(&Port(host.clone()))
            .await
            .unwrap();
        assert!(reply.opened, "{reply:?}");
        let launches = host.launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(
            launches[0].initial_prompt.as_deref(),
            Some(
                "Help me create a CodeSense agent. Use the `agent_*` MCP tools; \
                 `agent_create` explains the fields."
            )
        );
    }

    #[tokio::test]
    async fn routine_authoring_opens_with_no_skill_and_a_plain_prompt() {
        let home = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::new(home.path(), 4));
        no_skill_anywhere(&host);
        let reply = crate::commands::routines::open_authoring_agent(&Port(host.clone()))
            .await
            .unwrap();
        assert!(reply.opened, "{reply:?}");
        let launches = host.launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(
            launches[0].initial_prompt.as_deref(),
            Some(
                "Help me create a CodeSense routine. Use the `routine_*` MCP tools; \
                 `routine_create` explains the fields."
            )
        );
    }
}

#[tokio::test]
async fn a_headed_reopen_reports_what_showing_the_running_session_did() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 4);
    let opened = open_agent(&host, &active(), &team_workspace("Sprint 1", None))
        .await
        .unwrap();
    assert_eq!(
        opened.reveal, None,
        "a fresh open reports no reveal of its own"
    );

    // A live session no workspace shows is adopted into a tab, and the caller is told so.
    *host.show_outcome.lock().unwrap() = RevealOutcome::Shown(ShownAs::Adopted);
    let headed = || OpenRequest {
        mode: Some("headed".into()),
        ..team_workspace("Sprint 1", None)
    };
    let again = open_agent(&host, &active(), &headed()).await.unwrap();
    assert_eq!(again.status, OpenStatus::AlreadyLive);
    let json = serde_json::to_value(&again).unwrap();
    assert_eq!(json["shown"], "adopted");
    assert!(json.get("notShown").is_none());

    // One that can't be shown says why, in the result and its message: never a silent success.
    *host.show_outcome.lock().unwrap() = RevealOutcome::NotShown("its machine is offline".into());
    let offline = open_agent(&host, &active(), &headed()).await.unwrap();
    let json = serde_json::to_value(&offline).unwrap();
    assert_eq!(json["notShown"], "its machine is offline");
    assert!(json.get("shown").is_none());
    assert_eq!(
        offline.message,
        "Sprint 1 is already running. It could not be shown: its machine is offline."
    );

    // A background reopen shows nothing, so it reports nothing.
    let background = open_agent(
        &host,
        &active(),
        &OpenRequest {
            mode: Some("headless".into()),
            ..team_workspace("Sprint 1", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(background.reveal, None);
    let json = serde_json::to_value(&background).unwrap();
    assert!(json.get("shown").is_none() && json.get("notShown").is_none());
}

#[test]
fn reveal_of_a_tabless_live_session_adopts_it_or_says_why_not() {
    let (payload, outcome) = tabless_reveal("SW-1", "shell-7", Some("acme:default".into()), true);
    assert_eq!(outcome, RevealOutcome::Shown(ShownAs::Adopted));
    let payload = payload.expect("the viewer is asked to adopt it");
    assert_eq!(payload["target"], "session");
    assert_eq!(payload["sessionId"], "shell-7");
    assert_eq!(payload["teamKey"], "acme:default");
    assert_eq!(payload["handle"], "SW-1");
    assert_eq!(payload["focus"], true);

    let (payload, outcome) = tabless_reveal("SW-1", "shell-7", None, true);
    assert!(payload.is_none());
    match outcome {
        RevealOutcome::NotShown(reason) => assert!(reason.contains("which team"), "{reason}"),
        other => panic!("expected notShown, got {other:?}"),
    }
}

#[test]
fn reveal_of_a_headless_run_opens_its_run_output() {
    let (payload, outcome) = headless_reveal(
        "SW-1",
        "headless-1",
        Some("run-headless-1".into()),
        Some("acme:default".into()),
    );
    assert_eq!(outcome, RevealOutcome::Shown(ShownAs::RunOutput));
    let payload = payload.unwrap();
    assert_eq!(payload["target"], "runOutput");
    assert_eq!(payload["runId"], "run-headless-1");
    let (payload, outcome) =
        headless_reveal("SW-1", "headless-1", None, Some("acme:default".into()));
    assert!(payload.is_none());
    assert!(matches!(outcome, RevealOutcome::NotShown(_)));
}

// ──────────────────────────────────────────────────────────────────

/// AC-1: a well-formed `maxRuntime` resolves to the exact budget in ms.
#[test]
fn resolve_max_runtime_accepts_minute_and_hour_strings() {
    assert_eq!(resolve_max_runtime(Some("1m"), &None), Ok(Some(60_000)));
    assert_eq!(
        resolve_max_runtime(Some("30m"), &None),
        Ok(Some(30 * 60_000))
    );
    assert_eq!(
        resolve_max_runtime(Some("24h"), &None),
        Ok(Some(24 * 3_600_000))
    );
    // Absent or blank → unbounded (today's behaviour).
    assert_eq!(resolve_max_runtime(None, &None), Ok(None));
    assert_eq!(resolve_max_runtime(Some("   "), &None), Ok(None));
}

/// AC-2: a value out of range or syntactically bad surfaces as a legible refusal.
#[test]
fn resolve_max_runtime_refuses_bad_values_before_anything_spawns() {
    for bad in ["30s", "0m", "25h", "abc", "1.5h", "30"] {
        let result = resolve_max_runtime(Some(bad), &None);
        assert!(result.is_err(), "expected refusal for {bad:?}: {result:?}");
    }
}

/// AC-3: `maxRuntime` + a non-local `machineId` is refused with a message that
/// names the local requirement; `machineId=local` is accepted.
#[test]
fn resolve_max_runtime_is_this_mac_only() {
    let remote = Some("remote-box".to_string());
    let err = resolve_max_runtime(Some("30m"), &remote).unwrap_err();
    assert!(err.contains("This Mac only"), "{err}");
    assert_eq!(
        resolve_max_runtime(Some("30m"), &Some("local".to_string())),
        Ok(Some(30 * 60_000)),
    );
    // Without the budget, a remote launch is fine.
    assert_eq!(resolve_max_runtime(None, &remote), Ok(None));
}

/// AC-9: an `alreadyLive` result with a caller-supplied `maxRuntime` includes
/// the "session already live" warning. Without `maxRuntime` the warning is
/// absent — today's behaviour on an idempotent reopen.
#[test]
fn already_live_result_warns_when_max_runtime_is_sent_at_an_already_live_session() {
    use crate::agents::launch_check::LaunchWarningCode;
    let req = OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        max_runtime_ms: Some(30 * 60_000),
        ..OpenRequest::default()
    };
    let result = already_live_result("SW-1".into(), "SW-1".into(), &req);
    assert_eq!(result.status, OpenStatus::AlreadyLive);
    let warning = result
        .warnings
        .iter()
        .find(|w| matches!(w.code, LaunchWarningCode::MaxRuntimeIgnored))
        .expect("maxRuntime warning on an alreadyLive result");
    assert!(warning.message.contains("already live"), "{warning:?}");

    let bare = OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        ..OpenRequest::default()
    };
    let bare_result = already_live_result("SW-1".into(), "SW-1".into(), &bare);
    assert!(
        bare_result
            .warnings
            .iter()
            .all(|w| !matches!(w.code, LaunchWarningCode::MaxRuntimeIgnored)),
        "{:?}",
        bare_result.warnings
    );
}

mod context_production {
    use super::*;
    use crate::agents::resolve::tests::{
        alice, content, draft, save, team_content, write, ORG, TEAM,
    };
    use codesense_pty_core::agent_cli::AgentCli;

    #[tokio::test]
    async fn production_host_prepares_nested_cwd_and_bootstrap_keeps_it_after_scope_setup() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let principal = alice();
        let team = team_content(home, &principal);
        let parent = content(home, &principal, "platform");
        write(&parent, ".claude/skills/parent/SKILL.md", b"parent skill");
        write(&parent, "AGENTS.md", b"Codex parent instructions");
        write(
            &parent,
            ".agents/skills/parent/SKILL.md",
            b"Codex parent skill",
        );
        write(&parent, "private-document.md", b"not layered");
        let key = save(home, &principal, draft("context-agent", AgentCli::Claude));
        let clone = team.parent().unwrap().parent().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.test"],
            vec!["add", "."],
            vec!["commit", "-m", "fixture"],
        ] {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(clone)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        // A dirty canonical parent must not leak into the branch cut from main.
        write(
            &parent,
            ".claude/skills/parent/SKILL.md",
            b"canonical dirty parent",
        );
        write(
            &parent,
            ".claude/skills/canonical-only/SKILL.md",
            b"not in main",
        );
        let agent = open_workspace_prepare(
            home,
            &principal,
            &key.to_string(),
            Some((ORG.into(), TEAM.into())),
            Some("/resume"),
            None,
        )
        .unwrap();
        let scope = ActiveTeam {
            org: ORG.into(),
            team: TEAM.into(),
        };
        let snapshot = (
            std::fs::read(clone.join(".git/HEAD")).unwrap(),
            std::fs::read(clone.join(".git/index")).unwrap(),
            std::fs::read(parent.join(".claude/skills/parent/SKILL.md")).unwrap(),
        );
        let destination = AppOpenHost::prepare_context_at(
            home,
            &principal,
            &scope,
            &team,
            "Agent",
            Some("Sprint 1"),
            true,
            &agent,
            &|_| {},
        )
        .unwrap();
        let branch_destination = context::resolve(
            home,
            &principal,
            &scope,
            &team,
            "Branch",
            Some("Branch"),
            true,
        )
        .unwrap();
        write(
            &branch_destination.record.root.join("platform/content"),
            ".claude/skills/branch-only/SKILL.md",
            b"branch skill",
        );
        context::layer(home, &principal, &branch_destination, &agent, &|_| {}).unwrap();
        assert_eq!(
            std::fs::read(
                branch_destination
                    .record
                    .launch_cwd
                    .join(".claude/skills/branch-only/SKILL.md")
            )
            .unwrap(),
            b"branch skill"
        );

        let cwd = &destination.record.launch_cwd;
        assert_eq!(std::fs::read(cwd.join("CLAUDE.md")).unwrap(), b"# Team\n");
        assert_eq!(
            std::fs::read(cwd.join(".claude/skills/parent/SKILL.md")).unwrap(),
            b"parent skill"
        );
        assert_eq!(
            std::fs::read(cwd.join("AGENTS.md")).unwrap(),
            b"Codex parent instructions"
        );
        assert_eq!(
            std::fs::read(cwd.join(".agents/skills/parent/SKILL.md")).unwrap(),
            b"Codex parent skill"
        );
        assert!(!cwd.join("private-document.md").exists());
        assert!(!cwd.join(".claude/skills/canonical-only/SKILL.md").exists());
        assert!(cwd.join(".claude/settings.local.json").is_file());
        let managed = crate::agents::prepare::ManagedRecord::read(
            home,
            &principal,
            &crate::content_engine::context_lifecycle::local_identity(cwd),
        )
        .unwrap();
        assert_eq!(
            managed.entries[".claude/settings.local.json"].source_layer,
            "generated"
        );
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
        let bootstrap = crate::agents::product_bootstrap::prepare_with_resources(&agent, cwd, None)
            .await
            .unwrap();
        assert_eq!(bootstrap.canonical_root(), Some(cwd.as_path()));
        bootstrap.validate().unwrap();
        let scope_session = format!("context-test-{}", uuid::Uuid::new_v4());
        let scoped = crate::agents::launch::project_and_apply_for_cli(
            &agent,
            &scope_session,
            cwd,
            None,
            None,
            str::to_string,
        );
        assert!(scoped.is_ok(), "{scoped:?}");
        crate::commands::mcp_scope::remove_scope_config(&scope_session);
        bootstrap.validate().unwrap();
        assert_eq!(bootstrap.canonical_root(), Some(cwd.as_path()));
        assert_eq!(
            snapshot,
            (
                std::fs::read(clone.join(".git/HEAD")).unwrap(),
                std::fs::read(clone.join(".git/index")).unwrap(),
                std::fs::read(parent.join(".claude/skills/parent/SKILL.md")).unwrap()
            )
        );
        // Opening another tab never refreshes a shared inherited snapshot.
        write(
            &destination.record.root.join("platform/content"),
            ".claude/skills/parent/SKILL.md",
            b"new parent skill",
        );
        let inherited = std::fs::read(cwd.join(".claude/skills/parent/SKILL.md")).unwrap();
        let codex_key = save(
            home,
            &principal,
            draft("codex-context-agent", AgentCli::Codex),
        );
        let codex = open_workspace_prepare(
            home,
            &principal,
            &codex_key.to_string(),
            Some((ORG.into(), TEAM.into())),
            None,
            None,
        )
        .unwrap();
        let shared = AppOpenHost::prepare_context_at(
            home,
            &principal,
            &scope,
            &team,
            "Second",
            Some(&destination.record.id),
            false,
            &codex,
            &|_| {},
        )
        .unwrap();
        assert_eq!(shared.record.launch_cwd, *cwd);
        assert!(!shared.created);
        assert_eq!(
            std::fs::read(cwd.join(".agents/skills/parent/SKILL.md")).unwrap(),
            b"Codex parent skill"
        );
        assert_eq!(
            std::fs::read(cwd.join(".claude/skills/parent/SKILL.md")).unwrap(),
            inherited
        );
    }
}

#[tokio::test]
async fn queue_launch_choices_inherit_clear_and_override_without_relabeling_the_parent() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 5);
    *host.prepared.lock().unwrap() = Some(|agent| agent.queue_role = Some("reviewer".into()));
    let parent = team_workspace("Parent", None);
    open_agent(&host, &active(), &parent).await.unwrap();
    for (name, choice, expected) in [
        ("Inherited", None, Some("reviewer")),
        ("No role", Some(None), None),
        ("Override", Some(Some("qa".into())), Some("qa")),
    ] {
        let mut req = team_workspace(name, Some("sess-1"));
        req.queue_role = choice;
        open_agent(&host, &active(), &req).await.unwrap();
        let launches = host.launches.lock().unwrap();
        let launch = launches.last().unwrap();
        assert_eq!(launch.record.meta.queue_role.as_deref(), expected);
        assert_eq!(launch.agent.queue_role.as_deref(), expected);
    }
    let parent = host
        .registry
        .snapshot()
        .into_iter()
        .find(|s| s.meta.display_name.as_deref() == Some("Parent"))
        .unwrap();
    assert_eq!(parent.meta.queue_role.as_deref(), Some("reviewer"));
}

#[tokio::test]
async fn an_unconfirmed_large_no_role_task_keeps_one_live_launch_and_returns_a_warning() {
    let home = tempfile::tempdir().unwrap();
    let host = FakeHost::new(home.path(), 1);
    host.startup_submit.lock().unwrap().submitted = false;
    let mut request = team_workspace("Large task", None);
    request.queue_role = Some(None);
    request.initial_prompt = Some("x".repeat(64 * 1024 + 1));
    let result = open_agent(&host, &active(), &request).await.unwrap();
    assert_eq!(result.status, OpenStatus::Opened);
    assert!(result
        .warnings
        .iter()
        .any(|w| w.code == LaunchWarningCode::StartupDeliveryUnconfirmed));
    let handle = result.handle.as_deref().unwrap();
    let session = host.registry.resolve(handle).unwrap();
    assert_eq!(host.registry.snapshot().len(), 1);
    let launches = host.launches.lock().unwrap();
    assert_eq!(launches.len(), 1);
    assert!(launches[0].agent.queue_role.is_none());
    let plan = super::super::mcp_workspace::plan_initial_prompt(
        launches[0].agent.runtime,
        launches[0].agent.launch.args.clone(),
        launches[0].initial_prompt.as_deref(),
    );
    assert!(!plan.in_argv);
    assert_eq!(plan.pty_prompt.as_deref().unwrap().len(), 64 * 1024 + 1);
    drop(launches);
    let reopened = open_agent(&host, &active(), &request).await.unwrap();
    assert_eq!(reopened.status, OpenStatus::AlreadyLive);
    assert_eq!(
        host.registry.resolve(handle).as_deref(),
        Some(session.as_str())
    );
    assert_eq!(host.registry.snapshot().len(), 1);
    assert_eq!(host.launches.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn child_open_of_shared_parent_story_gates_bare_and_full_agent_before_any_launch() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture};
    use std::sync::Arc;
    let f = AppFixture::new();
    f.session("child-open", Some("child"));
    let principal = Principal::from(1u64);
    let parent_agent = f.fixture.agent("parent", "reviewer");
    let shared_story = f.fixture.write(
        "parent",
        "content/shared.md",
        "---
type: story
story_id: SHARED
source_repo: acme/api
---
Shared story
",
    );
    let scope = caller_scope::for_session(f.app.handle(), Some("child-open"), None).await;
    assert!(scope.existing(&shared_story).is_ok());
    let mut host = FakeHost::new(&f.fixture.home, 4);
    host.principal = principal.clone();
    host.story_teams.insert(
        "SHARED".into(),
        ActiveTeam {
            org: "acme/ctx".into(),
            team: "parent".into(),
        },
    );
    let host = Arc::new(host);
    let port = agent_opens::HostPort(host.clone());
    let request = |reference: &str| {
        params(
            serde_json::json!({"target":"story", "storyId":"SHARED", "name":"review", "agent":reference}),
        )
    };
    let bare = open_workspace_call_for(
        &port,
        request("reviewer"),
        Some((&f.fixture.home, &principal, &scope)),
    )
    .await
    .unwrap_err();
    let full = open_workspace_call_for(
        &port,
        request(&parent_agent.to_string()),
        Some((&f.fixture.home, &principal, &scope)),
    )
    .await
    .unwrap_err();
    // The story's team is not one the child can launch into.
    assert_eq!(bare, not_openable("SHARED", "child"));
    assert_eq!(bare, full);
    std::fs::remove_file(
        f.fixture
            .path("parent", "config/agents/reviewer/agent.yaml"),
    )
    .unwrap();
    assert_eq!(
        bare,
        open_workspace_call_for(
            &port,
            request("reviewer"),
            Some((&f.fixture.home, &principal, &scope))
        )
        .await
        .unwrap_err()
    );
    // Authority is checked even when the caller leaves the agent at its default,
    // explicitly names a builtin, or uses the legacy story-open shape. A story open
    // says so in the same words as for a story that does not exist.
    for (json, refusal) in [
        (
            serde_json::json!({"target":"story", "storyId":"SHARED", "name":"default"}),
            not_openable("SHARED", "child"),
        ),
        (
            serde_json::json!({"target":"story", "storyId":"SHARED", "name":"builtin", "agent":"builtin/claude"}),
            not_openable("SHARED", "child"),
        ),
        (
            serde_json::json!({"storyId":"SHARED"}),
            not_openable("SHARED", "child"),
        ),
        (
            serde_json::json!({"target":"team-workspace", "team":"parent", "name":"parent"}),
            caller_scope::MISSING.to_string(),
        ),
        (
            serde_json::json!({"target":"story", "storyId":"ABSENT", "name":"default"}),
            not_openable("ABSENT", "child"),
        ),
    ] {
        assert_eq!(
            open_workspace_call_for(
                &port,
                params(json),
                Some((&f.fixture.home, &principal, &scope))
            )
            .await
            .unwrap_err(),
            refusal
        );
    }
    assert!(host.launches().is_empty());
    assert!(host.registry.snapshot().is_empty());
    for team in ["child", "sibling"] {
        let key = f.fixture.agent(team, "reviewer");
        assert_eq!(
            crate::agents::launch::find_agent_for(
                &f.fixture.home,
                &principal,
                "reviewer",
                Some(("acme/ctx", team)),
                &scope
            )
            .unwrap(),
            key
        );
    }
}

#[tokio::test]
async fn team_workspace_preflight_and_open_default_to_recorded_child_across_active_orgs() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture, CallerScope};
    use tauri::Manager;
    let f = AppFixture::new();
    f.session("own-workspace", Some("child"));
    // This consumer prepares a real context worktree, so give it a real Git checkout.
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.test"],
        vec!["add", "org.yml", "teams.yml", "people.yml", "child/content"],
        vec!["commit", "-m", "fixture"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&f.fixture.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let scope = caller_scope::for_session(f.app.handle(), Some("own-workspace"), None).await;
    let principal = Principal::from(1u64);
    let mut host = FakeHost::new(&f.fixture.home, 4);
    host.principal = principal.clone();
    host.content_roots
        .insert("child".into(), f.fixture.path("child", "content"));
    let host = std::sync::Arc::new(host);
    let port = agent_opens::HostPort(host.clone());
    for org in ["acme/ctx", "other/ctx"] {
        f.app
            .state::<crate::mcp::state::ActiveTeamState>()
            .update(crate::mcp::state::ActiveTeam {
                org_slug: org.into(),
                team_name: "parent".into(),
            });
        for requested in [None, Some("child")] {
            let context = team_workspace_preflight(f.app.handle(), &scope, requested)
                .await
                .unwrap();
            assert_eq!(context.org_slug, "acme/ctx");
            assert_eq!(context.team_name.as_deref(), Some("child"));
            let result = open_workspace_call_for(
                &port,
                params(
                    serde_json::json!({"target":"team-workspace","team":requested,"name":"own"}),
                ),
                Some((&f.fixture.home, &principal, &scope)),
            )
            .await
            .unwrap();
            assert!(
                matches!(result["status"].as_str(), Some("opened" | "alreadyLive")),
                "{result}"
            );
        }
        assert_eq!(
            team_workspace_preflight(f.app.handle(), &scope, Some("acme/ctx:parent"))
                .await
                .unwrap_err(),
            caller_scope::MISSING
        );
        assert_eq!(
            team_workspace_preflight(f.app.handle(), &scope, Some("acme/ctx:missing"))
                .await
                .unwrap_err(),
            caller_scope::MISSING
        );
    }
    f.app
        .state::<crate::mcp::state::ActiveTeamState>()
        .update(crate::mcp::state::ActiveTeam {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        });
    assert_eq!(
        team_workspace_preflight(f.app.handle(), &CallerScope::Human, None)
            .await
            .unwrap()
            .team_name
            .as_deref(),
        Some("parent")
    );
}

#[tokio::test]
async fn a_story_open_refuses_a_team_other_than_the_storys() {
    let tmp = tempfile::tempdir().unwrap();
    let host = FakeHost::new(tmp.path(), 4);
    let request = OpenRequest {
        target: "story".into(),
        story_id: Some("SW-1".into()),
        team: Some("other".into()),
        ..Default::default()
    };
    let result = open_agent(&host, &active(), &request).await.unwrap();
    assert_eq!(result.status, OpenStatus::Invalid);
    assert_eq!(
        result.message,
        "Not opened: story SW-1 is in team default, not other."
    );
    assert!(host.launches().is_empty());
}

#[tokio::test]
async fn a_team_agent_looks_its_story_up_in_its_own_org_whatever_org_is_active() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture};
    use std::sync::Arc;
    let f = AppFixture::new();
    f.session("own-org", Some("child"));
    let principal = Principal::from(1u64);
    let scope = caller_scope::for_session(f.app.handle(), Some("own-org"), None).await;
    let mut host = FakeHost::new(&f.fixture.home, 4);
    host.principal = principal.clone();
    // The caller's view is acme/ctx; the port's active team is another org.
    host.story_teams.insert(
        "SHARED".into(),
        ActiveTeam {
            org: "acme/ctx".into(),
            team: "parent".into(),
        },
    );
    host.story_teams.insert(
        "ELSEWHERE".into(),
        ActiveTeam {
            org: "other/org".into(),
            team: "child".into(),
        },
    );
    let host = Arc::new(host);
    let port = agent_opens::HostPort(host.clone());
    // Targeted and legacy opens both ask for the caller's own org. A story outside
    // its view (another team, another org) is not found either way.
    for (json, id) in [
        (serde_json::json!({"target":"story", "storyId":"SHARED", "name":"dev"}), "SHARED"),
        (serde_json::json!({"storyId":"SHARED"}), "SHARED"),
        (serde_json::json!({"target":"story", "storyId":"ELSEWHERE", "name":"dev"}), "ELSEWHERE"),
        (serde_json::json!({"storyId":"ELSEWHERE"}), "ELSEWHERE"),
    ] {
        let error = open_workspace_call_for(
            &port,
            params(json),
            Some((&f.fixture.home, &principal, &scope)),
        )
        .await
        .unwrap_err();
        assert_eq!(error, not_openable(id, "child"));
    }
    let hints = host.story_org_hints.lock().unwrap().clone();
    assert_eq!(hints, vec![Some("acme/ctx".to_string()); 4]);
    // An org outside its view is refused before any lookup: a story open in the same
    // words as a story it cannot see.
    for (json, refusal) in [
        (
            serde_json::json!({"target":"story", "storyId":"ELSEWHERE", "name":"dev", "org":"other/org"}),
            not_openable("ELSEWHERE", "child"),
        ),
        (
            serde_json::json!({"storyId":"ELSEWHERE", "org":"other/org"}),
            not_openable("ELSEWHERE", "child"),
        ),
        (
            serde_json::json!({"target":"team-workspace", "name":"dev", "org":"other/org"}),
            caller_scope::MISSING.to_string(),
        ),
    ] {
        let error = open_workspace_call_for(
            &port,
            params(json),
            Some((&f.fixture.home, &principal, &scope)),
        )
        .await
        .unwrap_err();
        assert_eq!(error, refusal);
    }
    assert_eq!(host.story_org_hints.lock().unwrap().len(), 4);
    assert!(host.launches().is_empty());
}

/// A team agent's refusal for a story it cannot open, whether the story is missing,
/// in a team it cannot launch into, or in another org.
fn not_openable(story_id: &str, team: &str) -> String {
    format!(
        "No story {story_id} that the {team} team can open. Check the story id, and that \
         the story's team is {team} or a team {team} can launch into."
    )
}

#[tokio::test]
async fn a_team_agents_story_open_says_why_without_naming_what_it_cannot_see() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture};
    use std::sync::Arc;
    let f = AppFixture::new();
    f.session("why", Some("child"));
    let principal = Principal::from(1u64);
    let scope = caller_scope::for_session(f.app.handle(), Some("why"), None).await;
    let candidate = format!(
        "unavailable: story file candidate at {} does not carry story_id 'SV-404'\nVerify its frontmatter.",
        f.fixture.path("parent", "content/x.md").display()
    );
    for (error, expected) in [
        // Reasons that name the story could describe one in a team the agent cannot see.
        (
            "unavailable: no managed story with id 'SV-404'".to_string(),
            not_openable("SV-404", "child"),
        ),
        (
            "unavailable: ambiguous story id 'SV-404' (2 indexed)".to_string(),
            not_openable("SV-404", "child"),
        ),
        (candidate, not_openable("SV-404", "child")),
        (
            caller_scope::MISSING.to_string(),
            not_openable("SV-404", "child"),
        ),
        // Reasons about the agent's own context or an index reach it as they are.
        (
            "cannot resolve context for acme/ctx:child (Unauthorized)".to_string(),
            "cannot resolve context for acme/ctx:child (Unauthorized)".to_string(),
        ),
        (
            "unavailable: frontmatter cache not ready".to_string(),
            "unavailable: frontmatter cache not ready".to_string(),
        ),
    ] {
        let mut host = FakeHost::new(&f.fixture.home, 4);
        host.principal = principal.clone();
        host.story_error = Some(error.clone());
        let host = Arc::new(host);
        let port = agent_opens::HostPort(host.clone());
        for json in [
            serde_json::json!({"target":"story", "storyId":"SV-404", "name":"dev"}),
            serde_json::json!({"storyId":"SV-404"}),
        ] {
            let refusal = open_workspace_call_for(
                &port,
                params(json),
                Some((&f.fixture.home, &principal, &scope)),
            )
            .await
            .unwrap_err();
            assert_eq!(refusal, expected, "{error}");
            assert_ne!(refusal, caller_scope::MISSING);
        }
        assert!(host.launches().is_empty());
    }
}

#[tokio::test]
async fn a_team_agent_cannot_tell_a_hidden_story_from_a_missing_one() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture};
    use std::sync::Arc;
    let f = AppFixture::new();
    f.session("leak", Some("child"));
    let principal = Principal::from(1u64);
    let scope = caller_scope::for_session(f.app.handle(), Some("leak"), None).await;
    let mut host = FakeHost::new(&f.fixture.home, 4);
    host.principal = principal.clone();
    // HIDDEN exists, in a team the child cannot launch into.
    host.story_teams.insert(
        "HIDDEN".into(),
        ActiveTeam {
            org: "acme/ctx".into(),
            team: "parent".into(),
        },
    );
    let host = Arc::new(host);
    let port = agent_opens::HostPort(host.clone());
    for (hidden, missing) in [
        (
            serde_json::json!({"target":"story", "storyId":"HIDDEN", "name":"dev"}),
            serde_json::json!({"target":"story", "storyId":"NOWHERE", "name":"dev"}),
        ),
        (
            serde_json::json!({"storyId":"HIDDEN"}),
            serde_json::json!({"storyId":"NOWHERE"}),
        ),
    ] {
        let caller = Some((f.fixture.home.as_path(), &principal, &scope));
        let hidden = open_workspace_call_for(&port, params(hidden), caller)
            .await
            .unwrap_err();
        let missing = open_workspace_call_for(&port, params(missing), caller)
            .await
            .unwrap_err();
        assert_eq!(hidden, not_openable("HIDDEN", "child"));
        assert_eq!(missing, not_openable("NOWHERE", "child"));
        assert_eq!(
            hidden.replace("HIDDEN", "ID"),
            missing.replace("NOWHERE", "ID")
        );
    }
    assert!(host.launches().is_empty());
}

#[tokio::test]
async fn a_persons_story_open_keeps_its_reason() {
    use crate::context_repo::caller_scope::CallerScope;
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(1u64);
    let mut host = FakeHost::new(home.path(), 4);
    host.story_error = Some("unavailable: ambiguous story id 'X' (2 indexed)".into());
    let port = agent_opens::HostPort(std::sync::Arc::new(host));
    for json in [
        serde_json::json!({"target":"story", "storyId":"X", "name":"dev"}),
        serde_json::json!({"storyId":"X"}),
    ] {
        let refusal = open_workspace_call_for(
            &port,
            params(json),
            Some((home.path(), &principal, &CallerScope::Human)),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal, "unavailable: ambiguous story id 'X' (2 indexed)");
    }
}

#[tokio::test]
async fn target_story_without_story_id_says_so() {
    use crate::context_repo::caller_scope::{self, tests::AppFixture};
    let f = AppFixture::new();
    f.session("no-id", Some("child"));
    let principal = Principal::from(1u64);
    let scope = caller_scope::for_session(f.app.handle(), Some("no-id"), None).await;
    let host = FakeHost::new(&f.fixture.home, 4);
    let port = agent_opens::HostPort(std::sync::Arc::new(host));
    for json in [
        serde_json::json!({"target":"story", "name":"dev"}),
        serde_json::json!({"target":"story", "storyId":" ", "name":"dev"}),
    ] {
        let result = open_workspace_call_for(
            &port,
            params(json),
            Some((&f.fixture.home, &principal, &scope)),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "invalid", "{result}");
        assert_eq!(
            result["message"],
            "Not opened: target: story needs story_id."
        );
    }
}

#[tokio::test]
async fn a_person_names_the_org_of_a_legacy_story_open() {
    let port = FakePort::default();
    open_workspace_call(
        &port,
        params(serde_json::json!({"story_id": "SW-1", "org": "acme/b"})),
    )
    .await
    .unwrap();
    open_workspace_call(&port, params(serde_json::json!({"story_id": "SW-1"})))
        .await
        .unwrap();
    assert_eq!(
        *port.story_orgs.lock().unwrap(),
        vec![Some("acme/b".to_string()), None]
    );
    // An org is a slug, never a path.
    let error = open_workspace_call(
        &port,
        params(serde_json::json!({"story_id": "SW-1", "org": "acme/../x"})),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "invalid: unknown org \"acme/../x\"");
    assert_eq!(port.story_orgs.lock().unwrap().len(), 2);
}
