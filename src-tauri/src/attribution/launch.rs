// SPDX-License-Identifier: MIT
//! Switching capture on for one launch, and saying how that went.
//!
//! [`prepare_for_host`] never fails a launch: a missing, unsupported or broken git-ai, or a daemon
//! that will not start, comes back as an [`AttributionStatus`] with nothing wired.

use super::detect::{detect, Detection};
use super::managed::{ConfigError, Managed};
use super::prefs::{self, AttributionMode, AttributionPrefs, NotesMode};
use super::{AttributionState, AttributionStatus, OffReason};
use crate::agents::adapter::{desktop, Attribution};
use crate::domain::Principal;
use codesense_pty_core::agent_cli::AgentCli;
use std::ffi::OsStr;
use std::path::Path;

/// Events git-ai's checkpoint hooks run on in Codex (Claude's are `PreToolUse` and
/// `PostToolUse`; its `Stop` is not needed).
pub(crate) const CODEX_HOOK_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "Stop"];

/// Seconds a checkpoint hook may take. The hook hands off to the daemon in milliseconds.
pub(crate) const HOOK_TIMEOUT: u32 = 5;

pub(crate) struct Request<'a> {
    pub codesense_home: &'a Path,
    pub principal: &'a Principal,
    /// The directory the agent launches in.
    pub cwd: &'a Path,
    pub runtime: AgentCli,
}

/// What a launch adds for attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub status: AttributionStatus,
    pub git_ai_version: Option<String>,
    /// Added to the launch's environment (`GIT_TRACE2_EVENT`, or an unsupported CLI's
    /// `GIT_REFLOG_ACTION`).
    pub env: Vec<(String, String)>,
    /// The checkpoint hook command, for Claude's settings and Codex's `-c` hooks.
    pub hook_command: Option<String>,
}

impl Plan {
    fn without(status: AttributionStatus, git_ai_version: Option<String>) -> Self {
        Self {
            status,
            git_ai_version,
            env: Vec::new(),
            hook_command: None,
        }
    }

    /// This plan, found not to be wireable into the launch after all (`reason`): nothing is
    /// wired and the record says why.
    pub(crate) fn unwired(self, request: &Request<'_>, reason: &str) -> Self {
        let plan = Self::without(
            AttributionStatus::Error {
                message: reason.to_owned(),
            },
            self.git_ai_version,
        );
        store(request, &plan);
        plan
    }

    pub(crate) fn to_state(&self, now_ms: u64) -> AttributionState {
        AttributionState {
            status: self.status.clone(),
            git_ai_version: self.git_ai_version.clone(),
            checked_at: now_ms,
        }
    }
}

/// Plan for tests that run the current desktop core directly.
#[cfg(test)]
pub(crate) fn prepare(request: &Request<'_>, path_var: Option<&OsStr>) -> Plan {
    prepare_for_host(request, path_var, true)
}

/// Decide and wire attribution only when the selected host can trust its injected hooks.
/// The status is also stored on the worktree record when it has one.
pub(crate) fn prepare_for_host(
    request: &Request<'_>,
    path_var: Option<&OsStr>,
    attribution_hook_trust: bool,
) -> Plan {
    let record = crate::install::worktree_index::read_record(
        request.codesense_home,
        request.principal,
        request.cwd,
    )
    .ok()
    .flatten();
    // A context worktree has no story record, yet captures like a story worktree does. It
    // is judged by its checkout root: the launch directory is a folder inside it.
    let context_root = context_worktree_root(request);
    if Option::is_none(&record) && Option::is_none(&context_root) {
        return Plan::without(
            AttributionStatus::Off {
                reason: OffReason::NotManagedWorktree,
            },
            None,
        );
    }
    let plan = match &context_root {
        Some(root) if Option::is_none(&record) => plan_for(
            &Request {
                cwd: root,
                ..*request
            },
            path_var,
            attribution_hook_trust,
        ),
        _ => plan_for(request, path_var, attribution_hook_trust),
    };
    store(request, &plan);
    plan
}

fn canonical_or_same(path: &Path) -> std::path::PathBuf {
    Result::unwrap_or_else(std::fs::canonicalize(path), |_| Path::to_path_buf(path))
}

/// The checkout root of the context worktree this profile registered that holds the launch
/// directory.
fn context_worktree_root(request: &Request<'_>) -> Option<std::path::PathBuf> {
    let cwd = canonical_or_same(request.cwd);
    let rows = Result::ok(crate::install::worktree_index::context_records(
        request.codesense_home,
        request.principal,
    ))?;
    std::iter::Iterator::find_map(
        &mut <[crate::install::worktree_index::ContextWorktreeRecord]>::iter(&rows),
        |row| {
            let root = canonical_or_same(&row.root);
            bool::then_some(Path::starts_with(&cwd, &root), root)
        },
    )
}

/// Keep `plan`'s status on the worktree's record, and the story branch's head in its tips.
fn store(request: &Request<'_>, plan: &Plan) {
    let state = plan.to_state(now_ms());
    let stored = crate::install::worktree_index::update_record_with(
        request.codesense_home,
        request.principal,
        request.cwd,
        |current| {
            let mut record = current.ok_or("record gone")?;
            record.attribution = Some(state);
            Ok(record)
        },
    );
    // SV-CDE-82: a launch sees the story's tip; a busy tips writer skips it.
    if let Ok(record) = stored {
        crate::install::story_tips::sight_root(
            request.codesense_home,
            &record.story_id,
            request.cwd,
            crate::install::story_tips::Wait::Never,
        );
    }
}

fn plan_for(request: &Request<'_>, path_var: Option<&OsStr>, attribution_hook_trust: bool) -> Plan {
    if let Attribution::Unsupported(reason) = desktop(request.runtime).attribution {
        // Nothing captures this CLI, so its session's commits are kept out of git-ai's
        // untraced-commit fixup, which would credit them to the person.
        return Plan {
            env: super::unsupported::launch_env(reason),
            ..Plan::without(
                AttributionStatus::Unsupported {
                    reason: reason.into(),
                },
                None,
            )
        };
    }
    let notes = match repo_notes(request) {
        Ok(notes) => notes,
        Err(off) => return Plan::without(AttributionStatus::Off { reason: off }, None),
    };
    if let Some(status) = super::install::platform_status(request.codesense_home) {
        return Plan::without(status, None);
    }
    if codesense_pty_core::agent_cli::descriptor(request.runtime).attribution_hook_trust
        && !attribution_hook_trust
    {
        return Plan::without(
            AttributionStatus::Error {
                message: "The terminal server is outdated; restart it to enable line attribution."
                    .into(),
            },
            None,
        );
    }
    let managed = Managed::for_codesense_home(request.codesense_home);
    let detection = detect(request.codesense_home, path_var, &managed.daemon_env());
    if super::install::needs_install(request.codesense_home, &detection) {
        super::install::ensure_installed(
            request.codesense_home,
            path_var,
            super::install::Trigger::Launch,
        );
    }
    let unavailable = |fallback| super::install::status(request.codesense_home).unwrap_or(fallback);
    let (binary, version) = match detection {
        Detection::NotInstalled => {
            return Plan::without(unavailable(AttributionStatus::NotInstalled), None)
        }
        Detection::Broken { message, .. } => {
            return Plan::without(unavailable(AttributionStatus::Error { message }), None)
        }
        Detection::Found { path, raw, version } => {
            if let Some(status) =
                super::install::capture_status(request.codesense_home, &raw, version)
            {
                return Plan::without(status, Some(raw));
            }
            (path, raw)
        }
    };
    let configured = match managed.apply_config(&binary, &version, || {
        super::managed::lists(request.codesense_home)
    }) {
        // A binary that cannot keep notes local still captures a repo whose notes push.
        Err(ConfigError::LocalNotesUnsupported(_)) if notes == NotesMode::Push => Ok(()),
        Err(ConfigError::LocalNotesUnsupported(_)) => {
            return Plan::without(
                AttributionStatus::Off {
                    reason: OffReason::NotesWouldPush,
                },
                Some(version),
            )
        }
        Err(ConfigError::Failed(message)) => Err(message),
        // The daemon ignores a repo outside the roots just written (a worktree whose clone is
        // not attached), so wiring it would record nothing.
        Ok(lists) if !super::managed::RepoRoots::daemon_tracks(&lists.repo_roots, request.cwd) => {
            return Plan::without(
                AttributionStatus::Off {
                    reason: OffReason::RepoNotAttached,
                },
                Some(version),
            )
        }
        Ok(_) => Ok(()),
    };
    let ready = configured.and_then(|()| managed.ensure_daemon(&binary, &version));
    if let Err(message) = ready {
        let status = if managed.restart_pending(Some(&version)) {
            AttributionStatus::Off {
                reason: OffReason::NotesWouldPush,
            }
        } else {
            AttributionStatus::Error { message }
        };
        return Plan::without(status, Some(version));
    }
    Plan {
        status: AttributionStatus::Active,
        git_ai_version: Some(version),
        // Git's trace events go to the managed daemon only, and a user's own git-ai hook
        // cannot reach any daemon.
        env: vec![
            (
                "GIT_TRACE2_EVENT".into(),
                format!("af_unix:stream:{}", Path::display(&managed.trace_socket())),
            ),
            ("GIT_TRACE2_EVENT_NESTING".into(), "0".into()),
        ]
        .into_iter()
        .chain(USER_DAEMON_NEUTRALISED.map(|(key, value)| (key.to_owned(), value.to_owned())))
        .collect(),
        hook_command: Some(managed.checkpoint_command(&binary, AgentCli::as_str(request.runtime))),
    }
}

/// Where a user's own git-ai hook would look for its daemon inside a CodeSense session: a
/// path that can be neither found nor created, so that hook exits without a checkpoint.
/// CodeSense's own hook sets the managed values in its command, so exactly one counts.
pub(crate) const USER_DAEMON_NEUTRALISED: [(&str, &str); 3] = [
    ("GIT_AI_DAEMON_HOME", "/dev/null/git-ai"),
    ("GIT_AI_DAEMON_CONTROL_SOCKET", "/dev/null/git-ai.sock"),
    ("GIT_AI_DAEMON_TRACE_SOCKET", "/dev/null/git-ai-trace.sock"),
];

/// Where Claude and Codex keep hooks a user could have installed git-ai's checkpoint in.
pub(crate) struct HookSettings<'a> {
    pub user_home: &'a Path,
    /// `CLAUDE_CONFIG_DIR`, when set.
    pub claude_config_dir: Option<&'a Path>,
    /// `CODEX_HOME`, when set.
    pub codex_home: Option<&'a Path>,
    /// Checkouts whose project `.claude/settings*.json` a launch also merges.
    pub projects: &'a [std::path::PathBuf],
}

/// Whether the user ran git-ai's own `install-hooks`: a git-ai checkpoint hook that is not
/// CodeSense's in their Claude settings (user, `CLAUDE_CONFIG_DIR` or project) or Codex config.
/// Read only, for the doctor.
pub(crate) fn user_hooks_present(settings: &HookSettings<'_>) -> bool {
    let claude = Option::map_or_else(
        settings.claude_config_dir,
        || settings.user_home.join(".claude"),
        Path::to_path_buf,
    );
    let codex = Option::map_or_else(
        settings.codex_home,
        || settings.user_home.join(".codex"),
        Path::to_path_buf,
    );
    let mut files = vec![
        claude.join("settings.json"),
        claude.join("settings.local.json"),
        codex.join("config.toml"),
        codex.join("hooks.json"),
    ];
    for project in settings.projects {
        Vec::push(&mut files, project.join(".claude/settings.json"));
        Vec::push(&mut files, project.join(".claude/settings.local.json"));
    }
    let mut texts = std::iter::Iterator::filter_map(<[std::path::PathBuf]>::iter(&files), |path| {
        Result::ok(std::fs::read_to_string(path))
    });
    std::iter::Iterator::any(&mut texts, |text| {
        std::iter::Iterator::any(&mut str::lines(&text), |line| {
            str::contains(line, "git-ai")
                && str::contains(line, "checkpoint")
                && !str::contains(line, super::managed::HOOK_MARKER)
        })
    })
}

/// Where a repo's notes go, or why it is off. Capture is on for every managed worktree
/// unless the repo is set off; an identity or setting that cannot be read is off.
fn repo_notes(request: &Request<'_>) -> Result<NotesMode, OffReason> {
    let off = OffReason::NotesWouldPush;
    let (key, _) = prefs::repo_key_for_checkout(request.cwd).map_err(|_| off)?;
    let current = prefs::read(request.codesense_home, request.principal, &key).map_err(|_| off)?;
    if captures(&current) {
        Ok(AttributionPrefs::notes_mode(&current))
    } else {
        Err(OffReason::DisabledForRepo)
    }
}

/// Whether a repo's saved settings capture: every repo does unless it is set off.
pub(super) fn captures(prefs: &AttributionPrefs) -> bool {
    prefs.mode != Some(AttributionMode::Off)
}

/// What re-applying the managed config after a notes change did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "applied", rename_all = "snake_case")]
pub(crate) enum Reapplied {
    /// git-ai's config now holds the setting; the next push honours it.
    Now,
    /// No git-ai of CodeSense's own is installed yet. The setting is saved, and the config
    /// applies it before the daemon next starts.
    Pending,
    /// This git-ai refused the local-notes settings: it would push every repo's notes, so
    /// capture stays off for repos kept local.
    Unsupported { message: String },
    /// Applying failed; the setting is saved and the next launch applies it again.
    Failed { message: String },
}

/// Apply the managed config now, after a repo's notes setting changed, so the next push
/// already honours it. Any own copy takes it, so a move back to local reaches a daemon that is
/// still running on an older binary.
pub(crate) fn reapply_config(home: &Path) -> Reapplied {
    let managed = Managed::for_codesense_home(home);
    let Detection::Found { path, raw, .. } = detect(home, None, &managed.daemon_env()) else {
        return Reapplied::Pending;
    };
    match managed.apply_config(&path, &raw, || super::managed::lists(home)) {
        Ok(_) => Reapplied::Now,
        Err(ConfigError::LocalNotesUnsupported(message)) => Reapplied::Unsupported { message },
        Err(ConfigError::Failed(message)) => Reapplied::Failed { message },
    }
}

/// Generated capture environment is never replayed from a previous capture decision.
pub(crate) fn is_attribution_env(key: &str) -> bool {
    matches!(
        key,
        "GIT_TRACE2_EVENT" | "GIT_TRACE2_EVENT_NESTING" | "GIT_REFLOG_ACTION"
    ) || key.starts_with("GIT_AI_")
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Installation diagnostics for `describe_setup`. The first read acknowledges the
/// replacement notice in its receipt; it never writes config or starts the daemon.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Doctor {
    /// `active` here means git-ai is installed, supported and its daemon answers; a repo's
    /// own setting can still keep a worktree off.
    pub status: AttributionStatus,
    pub binary: Option<String>,
    pub git_ai_version: Option<String>,
    pub daemon_reachable: bool,
    pub warnings: Vec<String>,
    /// Last replacement receipt; retained across app restarts and manual reverts.
    pub previous_binary: Option<super::install::PreviousBinary>,
    pub supported_versions: &'static str,
    /// Last checkpoint sweep; in-memory since app start, separate from installation health.
    pub capture: super::catch_up::Capture,
    /// The notes ref capture writes (`refs/notes/sense`), kept on this machine.
    pub notes_ref: &'static str,
    /// Repos any local profile set `notes: push` (their remote keys); every other repo's notes
    /// stay local.
    pub notes_push_repos: Vec<String>,
    /// How many attached and context clones CodeSense lists in git-ai's `daemon_repo_roots` (0: no list,
    /// so every repo is tracked; `None` while none resolves and the list written before is kept).
    pub daemon_repo_roots: Option<usize>,
}

/// The doctor as `describe_setup` reads it: the first read acknowledges a pending replacement
/// notice.
pub(crate) fn doctor(codesense_home: &Path, path_var: Option<&OsStr>) -> Doctor {
    doctor_with(
        codesense_home,
        path_var,
        super::install::ReplacementNotice::Acknowledge,
    )
}

/// [`doctor`] with the notice mode a passive reader chooses ([`ReplacementNotice::Peek`]).
pub(crate) fn doctor_with(
    codesense_home: &Path,
    path_var: Option<&OsStr>,
    notice: super::install::ReplacementNotice,
) -> Doctor {
    let managed = Managed::for_codesense_home(codesense_home);
    let daemon_reachable = managed.daemon_reachable();
    let done = |status, binary: Option<&Path>, version: Option<String>| {
        let warnings = super::install::warning(codesense_home, notice)
            .into_iter()
            .chain(
                managed
                    .restart_pending(version.as_deref())
                    .then(|| "git-ai daemon restart pending; the next launch restarts it".into()),
            )
            .chain(matches!(&status, AttributionStatus::Unsupported { reason } if reason == "user_pinned")
                .then(|| super::install::USER_PIN_RECOVERY.to_string()))
            .chain(super::detect::user_copy_present(codesense_home, path_var)
                .then(|| "user_copy_present: a git-ai on PATH is left untouched and is never used for capture".into()))
            .chain(user_hooks_present(&HookSettings {
                user_home: &super::managed::UserGit::current().home,
                claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").as_deref().map(Path::new),
                codex_home: std::env::var_os("CODEX_HOME").as_deref().map(Path::new),
                projects: &prefs::all_attached_checkouts(codesense_home),
            })
            .then(|| "user_hooks_present: your own git-ai checkpoint hooks cannot reach a daemon in CodeSense sessions with capture on, so one checkpoint counts".into()))
            .collect();
        Doctor {
            status,
            binary: binary.map(|p| p.to_string_lossy().into_owned()),
            git_ai_version: version,
            daemon_reachable,
            warnings,
            previous_binary: super::install::previous_binary(codesense_home),
            supported_versions: super::install::expected_version(codesense_home),
            capture: super::catch_up::capture(codesense_home),
            notes_ref: super::managed::NOTES_REF,
            notes_push_repos: prefs::push_repos(codesense_home),
            daemon_repo_roots: super::managed::RepoRoots::listed_count(
                &super::managed::repo_roots(codesense_home),
            ),
        }
    };
    if let Some(status) = super::install::platform_status(codesense_home) {
        return done(status, None, None);
    }
    match detect(codesense_home, path_var, &managed.daemon_env()) {
        Detection::NotInstalled => done(
            super::install::status(codesense_home).unwrap_or(AttributionStatus::NotInstalled),
            None,
            None,
        ),
        Detection::Broken { path, message } => done(
            super::install::status(codesense_home).unwrap_or(AttributionStatus::Error { message }),
            Some(&path),
            None,
        ),
        Detection::Found { path, raw, version } => {
            if let Some(status) = super::install::capture_status(codesense_home, &raw, version) {
                return done(status, Some(&path), Some(raw));
            }
            if managed.restart_pending(Some(&raw)) {
                return done(
                    AttributionStatus::Off {
                        reason: OffReason::NotesWouldPush,
                    },
                    Some(&path),
                    Some(raw),
                );
            }
            if daemon_reachable {
                done(AttributionStatus::Active, Some(&path), Some(raw))
            } else {
                done(
                    AttributionStatus::Error {
                        message: "git-ai daemon is not running; it starts with the next launch"
                            .into(),
                    },
                    Some(&path),
                    Some(raw),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::resolve::tests::alice;
    use crate::attribution::testing::REMOTE;
    use crate::attribution::testing::{version_script, Launch};

    fn plan(fixture: &Launch, runtime: AgentCli) -> Plan {
        prepare(
            &Request {
                codesense_home: fixture.home(),
                principal: &alice(),
                cwd: &fixture.cwd,
                runtime,
            },
            None,
        )
    }

    fn off(reason: OffReason) -> AttributionStatus {
        AttributionStatus::Off { reason }
    }

    #[test]
    fn an_enabled_worktree_gets_the_declared_attribution_plan_for_every_cli() {
        for runtime in AgentCli::ALL {
            let (fact, cli) = match runtime {
                AgentCli::Claude => (Attribution::SettingsHooks, Some("claude")),
                AgentCli::Codex => (Attribution::ArgvHooks, Some("codex")),
                AgentCli::OpenCode => (Attribution::Unsupported("opencode"), None),
                AgentCli::Hermes => (Attribution::Unsupported("hermes"), None),
            };
            assert_eq!(desktop(runtime).attribution, fact, "{runtime:?}");
            let fixture = Launch::new(runtime);
            let _daemon = fixture.daemon_running();
            let actual = plan(&fixture, runtime);
            let expected = if let Some(cli) = cli {
                let managed = Managed::for_codesense_home(fixture.home());
                Plan {
                    status: AttributionStatus::Active,
                    git_ai_version: Some(super::super::install::PIN.expected_version.into()),
                    env: [
                        (
                            "GIT_TRACE2_EVENT",
                            format!("af_unix:stream:{}", managed.trace_socket().display()),
                        ),
                        ("GIT_TRACE2_EVENT_NESTING", "0".into()),
                        ("GIT_AI_DAEMON_HOME", "/dev/null/git-ai".into()),
                        (
                            "GIT_AI_DAEMON_CONTROL_SOCKET",
                            "/dev/null/git-ai.sock".into(),
                        ),
                        (
                            "GIT_AI_DAEMON_TRACE_SOCKET",
                            "/dev/null/git-ai-trace.sock".into(),
                        ),
                    ]
                    .map(|(key, value)| (key.to_owned(), value))
                    .to_vec(),
                    hook_command: Some(
                        managed.checkpoint_command(&fixture.home().join("bin/git-ai"), cli),
                    ),
                }
            } else {
                assert!(
                    crate::attribution::testing::logged_calls(&fixture.log()).is_empty(),
                    "unsupported capture must not even probe git-ai"
                );
                let Attribution::Unsupported(reason) = fact else {
                    unreachable!("{runtime:?} has no capture command, so it is unsupported")
                };
                Plan {
                    env: crate::attribution::unsupported::launch_env(reason),
                    ..Plan::without(
                        AttributionStatus::Unsupported {
                            reason: reason.into(),
                        },
                        None,
                    )
                }
            };
            assert_eq!(actual, expected, "{runtime:?}");
            assert_eq!(fixture.status(), Some(expected.status), "{runtime:?}");
        }
    }

    #[test]
    fn the_status_is_stored_on_the_record_and_survives_a_reload() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        assert_eq!(fixture.status(), None, "nothing before a launch");
        plan(&fixture, AgentCli::Claude);
        // Read straight from the index file, as a restarted app would.
        assert_eq!(fixture.status(), Some(AttributionStatus::Active));
        fixture.uninstall();
        plan(&fixture, AgentCli::Claude);
        assert_eq!(fixture.status(), Some(AttributionStatus::NotInstalled));
        let record =
            crate::install::worktree_index::read_record(fixture.home(), &alice(), &fixture.cwd)
                .unwrap()
                .unwrap();
        assert!(record.attribution.unwrap().checked_at > 0);
    }

    /// SV-CDE-82 (a): a launch into a story worktree records its story branch's head in the
    /// story's tips, so a later merge of that tip links after the branch is gone. Red on the
    /// base: a launch recorded no tip.
    #[test]
    fn a_launch_records_the_story_branch_head_in_the_storys_tips() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        let repo = git2::Repository::open(&fixture.cwd).unwrap();
        let sig = git2::Signature::new("t", "t@example.com", &git2::Time::new(1, 0)).unwrap();
        let tree = repo.find_tree(repo.treebuilder(None).unwrap().write().unwrap()).unwrap();
        let tip = repo
            .commit(Some("refs/heads/feat/SV-1-launch"), &sig, &sig, "work", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/feat/SV-1-launch").unwrap();
        plan(&fixture, AgentCli::Claude);
        let tips = crate::install::story_tips::read(fixture.home(), "SV-1");
        assert_eq!(tips.len(), 1, "{tips:?}");
        assert_eq!(tips[0].sha, tip.to_string());
        assert_eq!(tips[0].branch, "feat/SV-1-launch");
    }

    #[test]
    fn a_missing_git_ai_is_not_installed_and_wires_nothing() {
        let fixture = Launch::new(AgentCli::Claude);
        fixture.uninstall();
        let plan = plan(&fixture, AgentCli::Claude);
        assert_eq!(plan.status, AttributionStatus::NotInstalled);
        assert!(plan.env.is_empty() && plan.hook_command.is_none());
    }

    #[test]
    fn versions_outside_1_7_5_to_1_8_are_unsupported_and_wire_nothing() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        for (output, found) in [
            ("1.7.4", "1.7.4"),
            ("1.8.0", "1.8.0"),
            ("2.1.0", "2.1.0"),
            ("garbage", "garbage"),
        ] {
            fixture.install(&version_script(output));
            let plan = plan(&fixture, AgentCli::Claude);
            assert_eq!(
                plan.status,
                if matches!(output, "1.8.0" | "2.1.0") {
                    AttributionStatus::Unsupported {
                        reason: "newer".into(),
                    }
                } else {
                    AttributionStatus::UnsupportedVersion {
                        found: found.into(),
                    }
                },
                "{output}"
            );
            assert!(
                plan.hook_command.is_none() && plan.env.is_empty(),
                "{output}"
            );
        }
        for output in ["1.7.5", "1.7.9", "1.7.5 (debug)"] {
            fixture.install(&version_script(output));
            assert!(
                !matches!(
                    plan(&fixture, AgentCli::Claude).status,
                    AttributionStatus::Active
                ),
                "{output}"
            );
        }
        fixture.install(&version_script(super::super::install::PIN.expected_version));
        assert_eq!(
            plan(&fixture, AgentCli::Claude).status,
            AttributionStatus::Active
        );
    }

    #[test]
    fn a_broken_binary_or_a_daemon_that_will_not_start_is_an_error_with_the_reason() {
        let fixture = Launch::new(AgentCli::Claude);
        fixture.install("echo oops >&2; exit 4");
        let AttributionStatus::Error { message } = plan(&fixture, AgentCli::Claude).status else {
            panic!("a failing --version is an error");
        };
        assert!(message.contains("oops"), "{message}");
        fixture.install(&format!(
            "{}\ncase \"$1\" in bg) echo 'socket path too long' >&2; exit 1;; esac",
            version_script(super::super::install::PIN.expected_version)
        ));
        let plan = plan(&fixture, AgentCli::Claude);
        let AttributionStatus::Error { message } = &plan.status else {
            panic!("{:?}", plan.status);
        };
        assert!(message.contains("socket path too long"), "{message}");
        assert_eq!(
            plan.git_ai_version.as_deref(),
            Some(super::super::install::PIN.expected_version)
        );
        assert!(plan.hook_command.is_none() && plan.env.is_empty());
    }

    #[test]
    fn a_running_daemon_is_left_alone_and_a_stopped_one_is_started_once_per_gap() {
        let fixture = Launch::new(AgentCli::Claude);
        // Not yet started: the launch starts it.
        plan(&fixture, AgentCli::Claude);
        let starts = |f: &Launch| {
            f.calls()
                .iter()
                .filter(|c| c.starts_with("bg start"))
                .count()
        };
        assert_eq!(starts(&fixture), 1);
        // Already running: the next launch does not.
        let daemon = fixture.daemon_running();
        plan(&fixture, AgentCli::Codex);
        plan(&fixture, AgentCli::Claude);
        assert_eq!(starts(&fixture), 1);
        // Gone again (a restart): it is started again.
        drop(daemon);
        std::fs::remove_file(Managed::for_codesense_home(fixture.home()).control_socket()).unwrap();
        plan(&fixture, AgentCli::Claude);
        assert_eq!(starts(&fixture), 2);
        // The config went in once, for the first launch only.
        let sets = fixture
            .calls()
            .iter()
            .filter(|c| c.starts_with("config set telemetry_oss"))
            .count();
        assert_eq!(sets, 1);
    }

    #[test]
    fn two_launches_in_one_worktree_end_active_in_either_order() {
        for order in [
            [AgentCli::Claude, AgentCli::Codex],
            [AgentCli::Codex, AgentCli::Claude],
        ] {
            let fixture = Launch::new(AgentCli::Claude);
            let _daemon = fixture.daemon_running();
            let plans: Vec<Plan> = order.iter().map(|r| plan(&fixture, *r)).collect();
            assert!(
                plans.iter().all(|p| p.status == AttributionStatus::Active),
                "{order:?}"
            );
            assert_eq!(
                fixture.status(),
                Some(AttributionStatus::Active),
                "{order:?}"
            );
            let clis: Vec<bool> = plans
                .iter()
                .zip(order)
                .map(|(p, r)| {
                    p.hook_command.as_ref().unwrap().contains(match r {
                        AgentCli::Claude => "checkpoint claude",
                        _ => "checkpoint codex",
                    })
                })
                .collect();
            assert_eq!(clis, [true, true], "{order:?}");
        }
    }

    #[test]
    fn opencode_and_a_directory_that_is_no_managed_worktree_report_why_and_wire_nothing() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        let opencode = plan(&fixture, AgentCli::OpenCode);
        assert_eq!(
            opencode.status,
            AttributionStatus::Unsupported {
                reason: "opencode".into()
            }
        );
        assert!(opencode.hook_command.is_none());
        assert_eq!(
            opencode.env,
            crate::attribution::unsupported::launch_env("opencode")
        );
        let elsewhere = fixture.home().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let plan = prepare(
            &Request {
                codesense_home: fixture.home(),
                principal: &alice(),
                cwd: &elsewhere,
                runtime: AgentCli::Claude,
            },
            None,
        );
        assert_eq!(plan.status, off(OffReason::NotManagedWorktree));
        assert!(plan.hook_command.is_none());
        assert!(
            fixture.calls().is_empty(),
            "git-ai is not touched: {:?}",
            fixture.calls()
        );
    }

    #[test]
    fn a_repo_set_off_stays_off_and_never_touches_git_ai() {
        let fixture = Launch::new(AgentCli::Claude);
        fixture.set_prefs(crate::attribution::prefs::AttributionPrefs {
            mode: Some(AttributionMode::Off),
            notes: None,
        });
        let plan = plan(&fixture, AgentCli::Claude);
        assert_eq!(plan.status, off(OffReason::DisabledForRepo));
        assert!(plan.hook_command.is_none() && plan.env.is_empty());
        assert!(fixture.calls().is_empty());
        assert_eq!(fixture.status(), Some(off(OffReason::DisabledForRepo)));
    }

    #[test]
    fn the_doctor_reports_binary_version_and_whether_the_daemon_answers() {
        let fixture = Launch::new(AgentCli::Claude);
        let doctor = doctor(fixture.home(), None);
        assert_eq!(
            doctor.git_ai_version.as_deref(),
            Some(super::super::install::PIN.expected_version)
        );
        assert!(!doctor.daemon_reachable);
        assert!(matches!(doctor.status, AttributionStatus::Error { .. }));
        let _daemon = fixture.daemon_running();
        let doctor = super::doctor(fixture.home(), None);
        assert!(doctor.daemon_reachable);
        assert_eq!(doctor.status, AttributionStatus::Active);
        fixture.install(&version_script("1.8.0"));
        assert_eq!(
            super::doctor(fixture.home(), None).status,
            AttributionStatus::Unsupported {
                reason: "newer".into()
            }
        );
        fixture.uninstall();
        assert_eq!(
            super::doctor(fixture.home(), None).status,
            AttributionStatus::NotInstalled
        );
        assert!(
            fixture.calls().is_empty(),
            "the doctor starts and configures nothing"
        );
    }

    fn resave(fixture: &Launch, remote: &str) {
        git2::Repository::open(&fixture.cwd)
            .unwrap()
            .remote_set_url("origin", remote)
            .unwrap();
        crate::commands::attachments::upsert_attachment_record(
            fixture.home(),
            1,
            crate::domain::Attachment {
                repo_path: fixture.cwd.to_string_lossy().into_owned(),
                remote_url: Some(remote.into()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
            None,
            false,
        )
        .unwrap();
    }

    #[test]
    fn r2_the_same_repo_over_ssh_or_https_keeps_its_choices() {
        let fixture = Launch::new(AgentCli::Claude);
        let path = fixture.cwd.to_string_lossy().into_owned();
        fixture.set_prefs(AttributionPrefs {
            mode: Some(AttributionMode::Off),
            notes: None,
        });
        for remote in [
            "git@github.com:acme/api.git",
            "ssh://git@github.com/acme/api",
            "https://token@github.com/Acme/API.git",
        ] {
            resave(&fixture, remote);
            let prefs = fixture.prefs_of(&path);
            assert_eq!(prefs.mode, Some(AttributionMode::Off), "{remote}");
        }
    }

    #[test]
    fn r2_an_opt_out_on_one_checkout_stops_capture_from_another_in_either_direction() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        let a = fixture.cwd.to_string_lossy().into_owned();
        // Both checkouts resolve the same repository entry.
        let b = fixture.attach_checkout("second-checkout", REMOTE);
        assert_eq!(fixture.prefs_of(&b).mode, Some(AttributionMode::On));
        assert_eq!(
            plan(&fixture, AgentCli::Claude).status,
            AttributionStatus::Active
        );
        for (opt_out_on, other) in [(&a, &b), (&b, &a)] {
            let set = |choice| {
                crate::attribution::prefs::set_for_checkout(
                    fixture.home(),
                    &alice(),
                    opt_out_on,
                    Some(choice),
                    None,
                )
                .unwrap()
            };
            set(crate::attribution::prefs::AttributionChoice::Off);
            assert_eq!(fixture.prefs_of(other).mode, Some(AttributionMode::Off));
            // Launch from the other checkout reads the same repository entry.
            assert_eq!(
                plan(&fixture, AgentCli::Claude).status,
                off(OffReason::DisabledForRepo)
            );
            set(crate::attribution::prefs::AttributionChoice::On);
            assert_eq!(
                plan(&fixture, AgentCli::Claude).status,
                AttributionStatus::Active
            );
        }
    }

    #[test]
    fn r2_replacing_a_clone_keeps_the_repository_opt_out() {
        let fixture = Launch::new(AgentCli::Claude);
        let saved = AttributionPrefs {
            mode: Some(AttributionMode::Off),
            notes: None,
        };
        fixture.set_prefs(saved);
        let next = fixture.home().join("replacement");
        let repo = git2::Repository::init(&next).unwrap();
        repo.remote("origin", REMOTE).unwrap();
        let attachment = crate::commands::attachments::existing_checkout_attachment(&next).unwrap();
        crate::commands::attachments::upsert_attachment_record(
            fixture.home(),
            1,
            attachment,
            Some("github.com/acme/api"),
            false,
        )
        .unwrap();
        let record = crate::install::worktree_index::WorktreeStoryRecord::new(
            &next,
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "SV-1",
            fixture.home().join("story.md"),
        );
        crate::install::worktree_index::write_record(fixture.home(), &alice(), &record).unwrap();
        let during = prepare(
            &Request {
                codesense_home: fixture.home(),
                principal: &alice(),
                cwd: &next,
                runtime: AgentCli::Claude,
            },
            None,
        );
        assert_eq!(during.status, off(OffReason::DisabledForRepo));
        assert_eq!(fixture.prefs_of(next.to_str().unwrap()), saved);
    }

    #[test]
    fn r2_detaching_another_checkout_and_opting_out_keep_both_effects() {
        let fixture = Launch::new(AgentCli::Claude);
        let other = fixture.attach_checkout("other", "https://github.com/acme/other.git");
        let home = fixture.home().to_path_buf();
        let checkout = fixture.cwd.to_string_lossy().into_owned();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                crate::commands::attachments::forget_attachment_record(&home, 1, &other).unwrap();
            });
            scope.spawn(|| {
                barrier.wait();
                crate::attribution::prefs::set_for_checkout(
                    &home,
                    &alice(),
                    &checkout,
                    Some(crate::attribution::prefs::AttributionChoice::Off),
                    None,
                )
                .unwrap();
            });
        });
        let data = crate::context_repo::attachments_io::read_attachments(
            &crate::context_repo::path_to_user_attachments_file(&home, &alice()),
        )
        .unwrap();
        assert!(!data.attachments.iter().any(|a| a.repo_path == other));
        assert_eq!(fixture.prefs().mode, Some(AttributionMode::Off));
    }

    #[test]
    fn r2_reopening_preferences_keeps_the_saved_values() {
        let fixture = Launch::new(AgentCli::Claude);
        let saved = AttributionPrefs {
            mode: Some(AttributionMode::Off),
            notes: None,
        };
        fixture.set_prefs(saved);
        assert_eq!(fixture.prefs(), saved);
        assert_eq!(fixture.prefs_of(&fixture.cwd.to_string_lossy()), saved);
    }

    #[test]
    fn r2_only_the_repository_store_owns_preference_persistence() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let attachment = std::fs::read_to_string(src.join("domain/attachment.rs")).unwrap();
        let writers = std::fs::read_to_string(src.join("commands/attachments.rs")).unwrap();
        assert!(!attachment.contains("AttributionPrefs"));
        assert!(!writers.contains("AttributionPrefs"));
        let name = ["attribution-prefs", ".json"].concat();
        let owners: Vec<_> = walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs")
            })
            .filter(|entry| {
                std::fs::read_to_string(entry.path())
                    .unwrap()
                    .contains(&name)
            })
            .map(|entry| entry.into_path())
            .collect();
        assert_eq!(owners, vec![src.join("attribution/prefs.rs")]);
        let store = std::fs::read_to_string(src.join("attribution/prefs.rs")).unwrap();
        assert_eq!(store.matches("write_json_atomic(").count(), 1);
        let write = store.split("pub(crate) fn update(").nth(1).unwrap();
        let write = write.split("\n}\n").next().unwrap();
        assert!(write.contains("write_json_atomic("));
    }

    #[test]
    fn r3_a_remote_less_source_and_linked_worktree_share_capture_preferences() {
        let fixture = Launch::new(AgentCli::Claude);
        let _daemon = fixture.daemon_running();
        let repo = git2::Repository::open(&fixture.cwd).unwrap();
        repo.remote_delete("origin").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(".mcp.json")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        let linked = fixture.home().join("differently-named-linked-worktree");
        repo.worktree("linked", &linked, None).unwrap();
        prefs::set_for_checkout(
            fixture.home(),
            &alice(),
            &fixture.cwd.to_string_lossy(),
            Some(prefs::AttributionChoice::On),
            None,
        )
        .unwrap();
        let identity =
            crate::operations::git_remote::derive_repository_identity(&fixture.cwd).unwrap();
        let record = crate::install::worktree_index::WorktreeStoryRecord::new(
            &linked,
            "acme",
            identity,
            "SV-1",
            fixture.home().join("story.md"),
        );
        crate::install::worktree_index::write_record(fixture.home(), &alice(), &record).unwrap();
        let result = prepare(
            &Request {
                codesense_home: fixture.home(),
                principal: &alice(),
                cwd: &linked,
                runtime: AgentCli::Claude,
            },
            None,
        );
        assert_eq!(result.status, AttributionStatus::Active);
        assert!(result.env.iter().any(|(key, _)| key == "GIT_TRACE2_EVENT"));
        assert!(result.hook_command.is_some());
        assert_eq!(
            prefs::repo_key_for_checkout(&fixture.cwd).unwrap(),
            prefs::repo_key_for_checkout(&linked).unwrap()
        );
        assert_eq!(
            fixture.prefs_of(linked.to_str().unwrap()).mode,
            Some(AttributionMode::On)
        );
    }
}
