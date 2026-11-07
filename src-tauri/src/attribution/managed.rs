// SPDX-License-Identifier: MIT
//! The one git-ai home CodeSense manages, and the daemon that runs in it.
//!
//! git-ai is per user, not per repo: its installer edits the CLIs' user settings and the
//! global `~/.gitconfig`. CodeSense does none of that. It runs the daemon and every
//! checkpoint hook with `HOME` pointed at `<codesense home>/git-ai/home`, so git-ai's
//! config is separate from any personal `~/.git-ai`, and turns capture on per launch with
//! environment alone (`GIT_TRACE2_EVENT`) plus hooks in the launch's own settings.

use super::detect::{git_ai_command, run_bounded};
use sha2::{Digest, Sha256};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Bump when the managed config below changes, so it is applied again and the daemon, which
/// reads `notes_ref` once per process, restarts on it.
const CONFIG_REVISION: u32 = 2;

/// The notes ref CodeSense's git-ai writes (`refs/notes/sense`), apart from the `ai` ref a
/// user's own git-ai writes and pushes. Readers try it first.
pub(crate) const NOTES_REF: &str = "sense";

/// The managed git-ai config, applied with `git-ai config set` under the managed `HOME`.
/// Telemetry and update checks are off (git-ai's defaults send usage and logs to
/// usegitai.com), and prompt text never goes into a note.
const CONFIG: &[(&str, &str)] = &[
    ("telemetry_oss", "off"),
    ("disable_auto_updates", "true"),
    ("disable_version_checks", "true"),
    ("feature_flags.daemon_log_upload", "false"),
    ("feature_flags.token_usage_metrics", "false"),
    ("prompt_storage", "local"),
];

/// The fork's local-notes keys: notes go to their own ref and push only to
/// `notes_push_repositories`. An upstream git-ai refuses them.
const LOCAL_NOTES_CONFIG: &[(&str, &str)] = &[("notes_ref", NOTES_REF), ("notes_push", "never")];

/// The lists the managed config holds, read under the config lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Lists {
    /// The destinations notes may push to (`notes_push_repositories`).
    pub push: Vec<String>,
    /// What `daemon_repo_roots` holds: see [`repo_roots`].
    pub repo_roots: RepoRoots,
}

/// What git-ai's `daemon_repo_roots` is set to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum RepoRoots {
    /// No attached clone: the key is omitted, so the daemon tracks every repo.
    #[default]
    All,
    /// The canonical common dirs of the attached clones, sorted: the only repos it tracks.
    Listed(Vec<String>),
    /// Clones are attached (these paths) but none resolves: the list already written is kept,
    /// never widened to every repo.
    Keep(Vec<PathBuf>),
}

impl RepoRoots {
    /// How many roots CodeSense lists (0: no list); `None` while the previous list is kept.
    pub(crate) fn listed_count(&self) -> Option<usize> {
        match self {
            Self::All => Some(0),
            Self::Listed(roots) => Some(<[String]>::len(roots)),
            Self::Keep(_) => None,
        }
    }

    /// Whether the daemon tracks the repo at `checkout`: by path component, as git-ai matches
    /// its canonical common dir. With no list every repo is tracked. With a kept list, a repo
    /// under an attached clone is taken as listed; one under none of them is not.
    pub(crate) fn daemon_tracks(&self, checkout: &Path) -> bool {
        let under = |dirs: &crate::commands::context_git_watch::GitDirs, roots: &[PathBuf]| {
            std::iter::Iterator::any(&mut <[PathBuf]>::iter(roots), |root| {
                Path::starts_with(&dirs.common_dir, root)
            })
        };
        let resolved = crate::commands::context_git_watch::GitDirs::resolve(checkout);
        match self {
            Self::All => true,
            Self::Listed(roots) => {
                let roots: Vec<PathBuf> = std::iter::Iterator::collect(std::iter::Iterator::map(
                    <[String]>::iter(roots),
                    PathBuf::from,
                ));
                Option::is_some_and(resolved, |dirs| under(&dirs, &roots))
            }
            Self::Keep(clones) => {
                let clones: Vec<PathBuf> = std::iter::Iterator::collect(
                    std::iter::Iterator::flat_map(<[PathBuf]>::iter(clones), |clone| {
                        [
                            PathBuf::clone(clone),
                            Result::unwrap_or_else(std::fs::canonicalize(clone), |_| {
                                PathBuf::clone(clone)
                            }),
                        ]
                    }),
                );
                Option::is_none_or(resolved, |dirs| under(&dirs, &clones))
            }
        }
    }

    fn sentinel_part(&self) -> String {
        match self {
            Self::All => short_hash(b""),
            Self::Listed(roots) => short_hash(str::as_bytes(&<[String]>::join(roots, "\n"))),
            Self::Keep(_) => ToOwned::to_owned("keep"),
        }
    }
}

/// The managed config's lists for `codesense_home`, as they are now.
pub(crate) fn lists(codesense_home: &Path) -> Lists {
    Lists {
        push: super::prefs::push_destinations(codesense_home),
        repo_roots: repo_roots(codesense_home),
    }
}

/// The canonical common dir of every context clone (org root) any local profile has: context
/// worktrees are linked worktrees of these, so they are covered with them.
fn context_common_dirs(codesense_home: &Path) -> Vec<String> {
    std::iter::Iterator::collect(std::iter::Iterator::filter_map(
        IntoIterator::into_iter(crate::context_repo::org_enumeration::enumerate_all_org_roots(
            codesense_home,
        )),
        |root| {
            let dirs = crate::commands::context_git_watch::GitDirs::resolve(&root)?;
            Option::map(Path::to_str(&dirs.common_dir), str::to_owned)
        },
    ))
}

/// The canonical common dir of every existing checkout any local profile attached, plus the
/// context clones once a source repo is listed: the daemon tracks a repo only when its common
/// dir is under one. A linked worktree shares its clone's common dir, so it is covered; a
/// scratch repo an agent's tests create is not. With no source repo listed every repo is
/// tracked, the context clones included.
pub(crate) fn repo_roots(codesense_home: &Path) -> RepoRoots {
    let checkouts = super::prefs::all_attached_checkouts(codesense_home);
    let roots: std::collections::BTreeSet<String> = std::iter::Iterator::collect(
        std::iter::Iterator::filter_map(<[PathBuf]>::iter(&checkouts), |checkout| {
            let dirs = crate::commands::context_git_watch::GitDirs::resolve(checkout)?;
            Option::map(Path::to_str(&dirs.common_dir), str::to_owned)
        }),
    );
    if !<[PathBuf]>::is_empty(&checkouts) && std::collections::BTreeSet::is_empty(&roots) {
        log::warn!(
            target: "attribution",
            "none of {} attached clones resolves to a git repo; git-ai keeps its previous daemon_repo_roots",
            <[PathBuf]>::len(&checkouts)
        );
        return RepoRoots::Keep(checkouts);
    }
    if std::collections::BTreeSet::is_empty(&roots) {
        return RepoRoots::All;
    }
    let mut roots = roots;
    std::iter::Extend::extend(&mut roots, context_common_dirs(codesense_home));
    RepoRoots::Listed(std::iter::Iterator::collect(IntoIterator::into_iter(roots)))
}

/// Why the managed config is not fully applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigError {
    /// The binary refused the local-notes keys: it would push every repo's notes.
    LocalNotesUnsupported(String),
    Failed(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalNotesUnsupported(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

/// How long one `git-ai config set` may take.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 30 } else { 5 });

/// How long `git-ai bg start` may take to bring the daemon up.
const DAEMON_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 30 } else { 10 });

/// Marks a hook command as CodeSense's git-ai checkpoint: a trailing shell comment, so the
/// Claude merge and Codex hook trust can both recognize it.
pub(crate) const HOOK_MARKER: &str = codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER;

/// One launch at a time starts the daemon.
static DAEMON_START: Mutex<()> = Mutex::new(());

/// One writer at a time applies the managed config: its lists are cleared and re-added.
static CONFIG_WRITES: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Managed {
    root: PathBuf,
    socket_dir: PathBuf,
    /// The user's own Git environment, which git-ai's git subprocesses run in.
    user_git: UserGit,
    daemon_timeout: Duration,
}

impl Managed {
    pub(crate) fn at(root: PathBuf, socket_dir: PathBuf, user_git: UserGit) -> Self {
        Self {
            root,
            socket_dir,
            user_git,
            daemon_timeout: DAEMON_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_daemon_timeout(mut self, timeout: Duration) -> Self {
        self.daemon_timeout = timeout;
        self
    }

    /// The managed home of `codesense_home`. The daemon's sockets sit under `/tmp` because a
    /// unix socket path may not pass 104 bytes on macOS.
    pub(crate) fn for_codesense_home(codesense_home: &Path) -> Self {
        let tag = short_hash(codesense_home.as_os_str().as_encoded_bytes());
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        Self::at(
            codesense_home.join("git-ai"),
            PathBuf::from(format!("/tmp/cs-gai-{uid}-{tag}")),
            UserGit::current(),
        )
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn git_ai_home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub(crate) fn socket_dir(&self) -> &Path {
        &self.socket_dir
    }

    pub(crate) fn control_socket(&self) -> PathBuf {
        self.socket_dir.join("c.sock")
    }

    pub(crate) fn trace_socket(&self) -> PathBuf {
        self.socket_dir.join("t.sock")
    }

    /// What git-ai's daemon and checkpoint hooks run with.
    pub(crate) fn daemon_env(&self) -> Vec<(String, String)> {
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        let mut env = vec![("HOME".to_owned(), path(self.git_ai_home()))];
        env.extend([
            ("GIT_AI_DAEMON_HOME".into(), path(self.root.join("daemon"))),
            (
                "GIT_AI_DAEMON_CONTROL_SOCKET".into(),
                path(self.control_socket()),
            ),
            (
                "GIT_AI_DAEMON_TRACE_SOCKET".into(),
                path(self.trace_socket()),
            ),
        ]);
        env
    }

    /// The checkpoint hook command for `cli` (`claude` or `codex`). It carries its own
    /// environment, so the agent's process keeps the user's `HOME`.
    pub(crate) fn checkpoint_command(&self, binary: &Path, cli: &str) -> String {
        let assignments: Vec<String> = self
            .daemon_env()
            .into_iter()
            .map(|(key, value)| format!("{key}={}", codesense_pty_core::shell_quote(&value)))
            .collect();
        format!(
            "env {} {} checkpoint {cli} --hook-input stdin # {HOOK_MARKER}",
            assignments.join(" "),
            codesense_pty_core::shell_quote(&binary.to_string_lossy()),
        )
    }

    /// Whether the daemon answers on its control socket.
    pub(crate) fn daemon_reachable(&self) -> bool {
        std::os::unix::net::UnixStream::connect(self.control_socket()).is_ok()
    }

    fn prepare_dirs(&self) -> Result<(), String> {
        for dir in [self.git_ai_home(), self.root.join("daemon")] {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", Path::display(&dir)))?;
        }
        ensure_own_dir(&self.socket_dir)
    }

    /// Apply the managed config once per git-ai version, config revision, wrapper and lists,
    /// and return the lists it holds. `lists` reads the destinations notes may push to (every
    /// other push keeps them) and the repo roots the daemon tracks under the same lock as the
    /// write, so the last setting read is the one applied.
    pub(crate) fn apply_config(
        &self,
        binary: &Path,
        version: &str,
        lists: impl FnOnce() -> Lists,
    ) -> Result<Lists, ConfigError> {
        let _one_at_a_time = CONFIG_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let lists = lists();
        let Lists { push, repo_roots } = &lists;
        self.prepare_dirs().map_err(ConfigError::Failed)?;
        let sentinel = self.root.join("config-applied");
        let wrapper = self.root.join("git");
        let script = self.user_git.wrapper_script();
        let stamp = format!(
            "{CONFIG_REVISION} {version} {} {} {}",
            short_hash(script.as_bytes()),
            short_hash(push.join("\n").as_bytes()),
            RepoRoots::sentinel_part(repo_roots)
        );
        if std::fs::read_to_string(&sentinel).is_ok_and(|seen| seen == stamp) {
            return Ok(lists);
        }
        let run = |args: &[&str]| -> Result<(), String> {
            let mut command = git_ai_command(binary);
            command.arg("config").args(args).envs(self.daemon_env());
            match run_bounded(command, CONFIG_TIMEOUT) {
                Ok((status, _)) if status.success() => Ok(()),
                Ok((status, output)) => Err(format!(
                    "git-ai config {} exited {status}: {}",
                    args.join(" "),
                    output.trim()
                )),
                Err(error) => Err(format!("git-ai config {}: {error}", args.join(" "))),
            }
        };
        write_executable(&wrapper, &script).map_err(ConfigError::Failed)?;
        run(&["set", "git_path", &wrapper.to_string_lossy()]).map_err(ConfigError::Failed)?;
        for (key, value) in CONFIG {
            run(&["set", key, value]).map_err(ConfigError::Failed)?;
        }
        // Prompts are excluded from every repository. One replacing set (no `--add`), so the
        // list never grows and is never briefly empty.
        run(&["set", "exclude_prompts_in_repositories", "*"]).map_err(ConfigError::Failed)?;
        for (key, value) in LOCAL_NOTES_CONFIG {
            run(&["set", key, value]).map_err(ConfigError::LocalNotesUnsupported)?;
        }
        let _ = run(&["unset", "notes_push_repositories"]);
        for destination in push {
            run(&["set", "notes_push_repositories", destination, "--add"])
                .map_err(ConfigError::LocalNotesUnsupported)?;
        }
        // One write replaces the whole list, so the daemon's mtime reload never sees part of
        // it. No attached clone omits the key (every repo is tracked): git-ai drops every repo
        // for a `null` or malformed value, so neither is ever written.
        match repo_roots {
            RepoRoots::All => {
                run(&["unset", "daemon_repo_roots"]).map_err(ConfigError::Failed)?;
            }
            RepoRoots::Listed(roots) => {
                let roots = ToString::to_string(&serde_json::json!(roots));
                run(&["set", "daemon_repo_roots", &roots]).map_err(ConfigError::Failed)?;
            }
            RepoRoots::Keep(_) => {}
        }
        std::fs::write(&sentinel, stamp).map_err(|e| ConfigError::Failed(e.to_string()))?;
        Ok(lists)
    }

    /// What `daemon-binary-version` holds for a daemon started by `version` under this
    /// config revision. A daemon from another binary or revision is restarted.
    pub(crate) fn daemon_stamp(version: &str) -> String {
        format!("{version} config-{CONFIG_REVISION}")
    }

    /// The binary version used by the daemon we started. A successful replacement
    /// records the old version before rename, so this survives an app restart.
    pub(crate) fn restart_pending(&self, version: Option<&str>) -> bool {
        std::fs::read_to_string(self.root.join("daemon-binary-version"))
            .is_ok_and(|seen| Some(seen.trim().to_owned()) != version.map(Self::daemon_stamp))
            && self.daemon_reachable()
    }

    /// Start a missing daemon, or restart one whose recorded binary version changed.
    /// A matching reachable daemon avoids a process spawn on every launch.
    pub(crate) fn ensure_daemon(&self, binary: &Path, version: &str) -> Result<(), String> {
        if self.daemon_reachable() && !self.restart_pending(Some(version)) {
            return Ok(());
        }
        let _one_at_a_time = DAEMON_START.lock().unwrap_or_else(|e| e.into_inner());
        let restart = self.restart_pending(Some(version));
        if self.daemon_reachable() && !restart {
            return Ok(());
        }
        self.prepare_dirs()?;
        let action = if restart { "restart" } else { "start" };
        let mut command = git_ai_command(binary);
        command.args(["bg", action]).envs(self.daemon_env());
        match run_bounded(command, self.daemon_timeout) {
            Ok((status, _)) if status.success() => {
                std::fs::write(
                    self.root.join("daemon-binary-version"),
                    Self::daemon_stamp(version),
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            }
            // Another process won the start between the check and the call.
            _ if !restart && self.daemon_reachable() => Ok(()),
            Ok((status, output)) => Err(format!(
                "git-ai bg {action} exited {status}: {}",
                output.trim()
            )),
            Err(error) => Err(format!("git-ai bg {action}: {error}")),
        }
    }
}

/// The first four bytes of `bytes`' SHA-256, in hex.
fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn write_executable(path: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", Path::display(path)))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", Path::display(path)))
}

/// `dir`, created private and owned by this user: a path under `/tmp` another user (or a
/// symlink) could have claimed first is refused.
fn ensure_own_dir(dir: &Path) -> Result<(), String> {
    crate::commands::paths::ensure_private_dir(dir)?;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if meta.file_type().is_dir() && MetadataExt::uid(&meta) == uid {
        Ok(())
    } else {
        Err(format!(
            "{} is not a directory owned by this user",
            Path::display(dir)
        ))
    }
}

/// What Git needs to behave for git-ai exactly as it does for the user. git-ai runs under
/// its own `HOME`, which would hide the user's `~/.gitconfig`, XDG config, credential
/// helpers and their stores, so its `git_path` is a wrapper that puts all of it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserGit {
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME` when the user's environment sets it.
    pub xdg_config_home: Option<String>,
    /// `GIT_CONFIG_GLOBAL` when the user's environment sets it.
    pub config_global: Option<String>,
    /// The real git.
    pub git: PathBuf,
}

impl UserGit {
    pub(crate) fn current() -> Self {
        #[cfg(test)]
        if let Some(pinned) = pinned::get() {
            return pinned;
        }
        let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            config_global: var("GIT_CONFIG_GLOBAL"),
            git: resolve_git(),
        }
    }

    /// The wrapper script: the user's `HOME`, `XDG_CONFIG_HOME` and `GIT_CONFIG_GLOBAL` (or
    /// their absence) restored, then the real git.
    pub(crate) fn wrapper_script(&self) -> String {
        let q = |value: &str| codesense_pty_core::shell_quote(value);
        let mut script = String::from(
            "#!/bin/sh\n# Written by CodeSense: git-ai's git runs with the user's own Git configuration.\n",
        );
        script.push_str(&format!(
            "HOME={}\nexport HOME\n",
            q(&self.home.to_string_lossy())
        ));
        for (name, value) in [
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("GIT_CONFIG_GLOBAL", &self.config_global),
        ] {
            match value {
                Some(value) => script.push_str(&format!("{name}={}\nexport {name}\n", q(value))),
                None => script.push_str(&format!("unset {name}\n")),
            }
        }
        script.push_str(&format!("exec {} \"$@\"\n", q(&self.git.to_string_lossy())));
        script
    }
}

/// Tests that launch pin the user's Git environment for their thread, so other tests that
/// change `HOME` in the process cannot change what a launch computes.
#[cfg(test)]
pub(crate) mod pinned {
    use super::UserGit;
    use std::cell::RefCell;

    thread_local! {
        static PINNED: RefCell<Option<UserGit>> = const { RefCell::new(None) };
    }

    pub(crate) fn get() -> Option<UserGit> {
        PINNED.with(|p| p.borrow().clone())
    }

    pub(crate) struct Guard;

    pub(crate) fn pin(user: UserGit) -> Guard {
        PINNED.with(|p| *p.borrow_mut() = Some(user));
        Guard
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            PINNED.with(|p| *p.borrow_mut() = None);
        }
    }
}

/// The git CodeSense runs, as an absolute path where one can be found.
fn resolve_git() -> PathBuf {
    let git = PathBuf::from(crate::commands::worktree::git_bin());
    if git.is_absolute() {
        return git;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .map(|dir| dir.join(&git))
        .find(|candidate| candidate.is_file())
        .unwrap_or(git)
}

#[cfg(test)]
mod tests {
    use super::super::testing::Sandbox;
    use super::*;
    use std::process::Command;

    #[test]
    fn renamed_binaries_apply_config_and_start_the_daemon_as_git_ai() {
        let sandbox = Sandbox::new();
        let binary = sandbox.dir.path().join("git-ai-macos-arm64");
        super::super::testing::argv0_binary(&binary);
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap();
        sandbox.managed.ensure_daemon(&binary, "1.7.5").unwrap();
        let calls =
            std::fs::read_to_string(sandbox.managed.git_ai_home().join("argv0-calls")).unwrap();
        assert!(calls.contains("config set telemetry_oss off"), "{calls}");
        assert!(calls.lines().any(|line| line == "bg start"), "{calls}");
    }

    #[test]
    fn the_daemon_and_hooks_run_under_the_managed_home_with_short_sockets() {
        let sandbox = Sandbox::new();
        let env: std::collections::HashMap<_, _> =
            sandbox.managed.daemon_env().into_iter().collect();
        assert_eq!(env["HOME"], sandbox.managed.git_ai_home().to_string_lossy());
        // Git config is the wrapper's business, not the daemon's environment.
        assert!(!env.contains_key("GIT_CONFIG_GLOBAL"));
        for socket in ["GIT_AI_DAEMON_CONTROL_SOCKET", "GIT_AI_DAEMON_TRACE_SOCKET"] {
            assert!(env[socket].len() < 104, "{socket}: {}", env[socket]);
        }
    }

    #[test]
    fn the_real_socket_dir_is_short_and_differs_per_codesense_home() {
        let a = Managed::for_codesense_home(Path::new("/Users/someone/.codesense"));
        let b = Managed::for_codesense_home(Path::new("/Users/someone/sandbox/.codesense"));
        assert_ne!(a.socket_dir(), b.socket_dir());
        assert!(a.control_socket().to_string_lossy().len() < 60);
    }

    #[test]
    fn the_checkpoint_command_carries_its_own_environment_and_marker() {
        let sandbox = Sandbox::new();
        let command = sandbox
            .managed
            .checkpoint_command(Path::new("/opt/it's/git-ai"), "codex");
        assert!(command.starts_with("env HOME='"), "{command}");
        assert!(command.contains(" '/opt/it'\\''s/git-ai' checkpoint codex --hook-input stdin"));
        assert!(command.ends_with(&format!("# {HOOK_MARKER}")));
        assert!(!command.contains("GIT_TRACE2_EVENT"));
    }

    #[test]
    fn the_config_is_applied_once_per_version_under_the_managed_home() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("");
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap();
        let calls = sandbox.calls();
        let home = format!("HOME={}", sandbox.managed.git_ai_home().display());
        for expected in [
            format!(
                "config set git_path {}/git",
                sandbox.managed.root().display()
            )
            .as_str(),
            "config set telemetry_oss off",
            "config set disable_auto_updates true",
            "config set disable_version_checks true",
            "config set feature_flags.daemon_log_upload false",
            "config set feature_flags.token_usage_metrics false",
            "config set prompt_storage local",
            "config set exclude_prompts_in_repositories *",
        ] {
            assert!(
                calls.iter().any(|c| c == &format!("{expected} {home}")),
                "{expected}: {calls:?}"
            );
        }
        let applied = calls.len();
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap();
        assert_eq!(
            sandbox.calls().len(),
            applied,
            "a second launch re-applies nothing"
        );
        sandbox
            .managed
            .apply_config(&binary, "1.7.6", Lists::default)
            .unwrap();
        assert_eq!(
            sandbox.calls().len(),
            applied * 2,
            "a new version applies again"
        );
        // Each apply replaces the prompt exclusion in one write: it is never unset or
        // re-added, so no reapply opens a window without it.
        let calls = sandbox.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("config unset exclude_prompts")
                    || (c.contains("exclude_prompts_in_repositories") && c.contains("--add"))),
            "{calls:?}"
        );
    }

    #[test]
    fn clones_that_all_fail_to_resolve_keep_the_written_roots() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("");
        let listed = Lists {
            repo_roots: RepoRoots::Listed(vec!["/a/.git".into()]),
            ..Lists::default()
        };
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", || listed.clone())
            .unwrap();
        let keep = Lists {
            repo_roots: RepoRoots::Keep(vec![PathBuf::from("/gone")]),
            ..Lists::default()
        };
        let before = sandbox.calls().len();
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", || keep.clone())
            .unwrap();
        let calls = sandbox.calls();
        assert!(calls.len() > before, "a changed list applies again");
        assert!(
            !calls[before..]
                .iter()
                .any(|c| c.contains("daemon_repo_roots")),
            "the written list is kept, never unset: {calls:?}"
        );
        // Under a kept list: an unreadable repo is not known to be ignored, a repo under an
        // attached clone is taken as listed, and any other repo is not tracked.
        let repos = tempfile::tempdir().unwrap();
        let (attached, other) = (repos.path().join("attached"), repos.path().join("other"));
        git2::Repository::init(&attached).unwrap();
        git2::Repository::init(&other).unwrap();
        let kept = RepoRoots::Keep(vec![attached.clone()]);
        assert!(kept.daemon_tracks(Path::new("/anything")));
        assert!(kept.daemon_tracks(&attached));
        assert!(!kept.daemon_tracks(&other));
        assert_eq!(kept.listed_count(), None);
        // And repo_roots says Keep when attachments exist but none resolves.
        let home = tempfile::tempdir().unwrap();
        crate::commands::attachments::upsert_attachment_record(
            home.path(),
            1,
            crate::domain::Attachment {
                repo_path: home.path().join("gone").to_string_lossy().into_owned(),
                remote_url: None,
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            },
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            repo_roots(home.path()),
            RepoRoots::Keep(vec![home.path().join("gone")])
        );
        assert_eq!(
            repo_roots(tempfile::tempdir().unwrap().path()),
            RepoRoots::All
        );
    }

    #[test]
    fn a_failed_roots_unset_is_an_error_and_writes_no_sentinel() {
        let sandbox = Sandbox::new();
        let binary = sandbox
            .binary("if [ \"$1 $2 $3\" = 'config unset daemon_repo_roots' ]; then exit 3; fi");
        assert!(sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .is_err());
        assert!(!sandbox.managed.root().join("config-applied").exists());
    }

    #[test]
    fn a_failing_config_is_an_error() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("echo nope >&2; exit 2");
        let error = sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap_err()
            .to_string();
        assert!(error.contains("nope"), "{error}");
        // And it is retried, not remembered as done.
        assert!(sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .is_err());
    }

    #[test]
    fn a_running_daemon_is_not_started_again_and_a_missing_one_is_started_once() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("");
        std::fs::create_dir_all(sandbox.managed.socket_dir()).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(sandbox.managed.control_socket()).unwrap();
        sandbox.managed.ensure_daemon(&binary, "1.7.5").unwrap();
        assert!(sandbox.calls().is_empty(), "{:?}", sandbox.calls());
        drop(listener);
        std::fs::remove_file(sandbox.managed.control_socket()).unwrap();
        sandbox.managed.ensure_daemon(&binary, "1.7.5").unwrap();
        assert_eq!(
            sandbox.calls(),
            [format!(
                "bg start HOME={}",
                sandbox.managed.git_ai_home().display()
            )]
        );
    }

    #[test]
    fn a_daemon_that_fails_to_start_is_an_error_with_its_message() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("echo 'socket path too long' >&2; exit 1");
        let error = sandbox.managed.ensure_daemon(&binary, "1.7.5").unwrap_err();
        assert!(error.contains("socket path too long"), "{error}");
    }

    #[test]
    fn a_hung_daemon_start_times_out() {
        let mut sandbox = Sandbox::new();
        sandbox.managed = sandbox
            .managed
            .clone()
            .with_daemon_timeout(Duration::from_millis(500));
        // `exec` so the timeout kills the script itself, not a child of it.
        let binary = sandbox.binary("exec sleep 60");
        let started = std::time::Instant::now();
        let error = sandbox.managed.ensure_daemon(&binary, "1.7.5").unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_socket_dir_someone_else_could_hold_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ensure_own_dir(&link).is_err());
        assert!(ensure_own_dir(&target).is_ok());
    }

    fn user_git(dir: &Path) -> UserGit {
        UserGit {
            home: dir.join("realhome"),
            xdg_config_home: None,
            config_global: None,
            git: PathBuf::from("git"),
        }
    }

    #[test]
    fn the_wrapper_restores_the_users_environment_and_execs_the_real_git() {
        let dir = Path::new("/u");
        let plain = user_git(dir).wrapper_script();
        assert!(plain.starts_with("#!/bin/sh\n"));
        assert!(
            plain.contains("HOME='/u/realhome'\nexport HOME\n"),
            "{plain}"
        );
        // What git-ai's environment changed is undone when the user had none of it.
        assert!(
            plain.contains("unset XDG_CONFIG_HOME\n")
                && plain.contains("unset GIT_CONFIG_GLOBAL\n")
        );
        assert!(plain.ends_with("exec 'git' \"$@\"\n"), "{plain}");
        let pinned = UserGit {
            xdg_config_home: Some("/u/xdg".into()),
            config_global: Some("/u/it's.cfg".into()),
            git: PathBuf::from("/opt/homebrew/bin/git"),
            ..user_git(dir)
        }
        .wrapper_script();
        assert!(
            pinned.contains("XDG_CONFIG_HOME='/u/xdg'\nexport XDG_CONFIG_HOME\n"),
            "{pinned}"
        );
        assert!(
            pinned.contains("GIT_CONFIG_GLOBAL='/u/it'\\''s.cfg'\nexport GIT_CONFIG_GLOBAL\n"),
            "{pinned}"
        );
        assert!(pinned.ends_with("exec '/opt/homebrew/bin/git' \"$@\"\n"));
    }

    /// Run `git config --get key` through the wrapper in the environment git-ai's daemon
    /// has: a managed `HOME` and a stray `GIT_CONFIG_GLOBAL`.
    fn through_wrapper(user: &UserGit, scratch: &Path, key: &str) -> String {
        let wrapper = scratch.join("git");
        write_executable(&wrapper, &user.wrapper_script()).unwrap();
        let output = Command::new(&wrapper)
            .args(["config", "--get", key])
            .env("HOME", scratch.join("managed"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("XDG_CONFIG_HOME", scratch.join("managed-xdg"))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn a_home_credential_helper_and_an_xdg_identity_are_visible_through_the_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let user = user_git(dir.path());
        std::fs::create_dir_all(user.home.join(".config/git")).unwrap();
        std::fs::write(
            user.home.join(".gitconfig"),
            "[credential]\n\thelper = store\n",
        )
        .unwrap();
        std::fs::write(
            user.home.join(".config/git/config"),
            "[user]\n\tname = Dummy Xdg\n",
        )
        .unwrap();
        assert_eq!(
            through_wrapper(&user, dir.path(), "credential.helper"),
            "store"
        );
        assert_eq!(through_wrapper(&user, dir.path(), "user.name"), "Dummy Xdg");
        std::fs::write(
            user.home.join(".git-credentials"),
            "https://dummy-user:dummy-password@example.invalid\n",
        )
        .unwrap();
        let mut lookup = Command::new(dir.path().join("git"))
            .args(["credential", "fill"])
            .env("HOME", dir.path().join("managed"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("XDG_CONFIG_HOME", dir.path().join("managed-xdg"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        lookup
            .stdin
            .take()
            .unwrap()
            .write_all(b"protocol=https\nhost=example.invalid\n\n")
            .unwrap();
        let credentials = lookup.wait_with_output().unwrap();
        assert!(credentials.status.success());
        let credentials = String::from_utf8(credentials.stdout).unwrap();
        assert!(credentials.contains("username=dummy-user\n"));
        assert!(credentials.contains("password=dummy-password\n"));
        // Without the wrapper, the managed environment sees neither of these.
        let bare = Command::new("git")
            .args(["config", "--get", "user.name"])
            .env("HOME", dir.path().join("managed"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert_ne!(String::from_utf8_lossy(&bare.stdout).trim(), "Dummy Xdg");
    }

    #[test]
    fn a_global_config_the_user_pinned_is_the_one_the_wrapper_uses() {
        let dir = tempfile::tempdir().unwrap();
        let pinned = dir.path().join("pinned.cfg");
        std::fs::write(&pinned, "[user]\n\tname = Dummy Pinned\n").unwrap();
        let mut user = user_git(dir.path());
        std::fs::create_dir_all(&user.home).unwrap();
        std::fs::write(
            user.home.join(".gitconfig"),
            "[user]\n\tname = Dummy Home\n",
        )
        .unwrap();
        user.config_global = Some(pinned.to_string_lossy().into_owned());
        assert_eq!(
            through_wrapper(&user, dir.path(), "user.name"),
            "Dummy Pinned"
        );
    }

    #[test]
    fn the_wrapper_is_written_and_named_as_gits_path_and_rewritten_when_it_changes() {
        let sandbox = Sandbox::new();
        let binary = sandbox.binary("");
        sandbox
            .managed
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap();
        let wrapper = sandbox.managed.root().join("git");
        assert!(std::fs::read_to_string(&wrapper)
            .unwrap()
            .contains("/real/home"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&wrapper).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let mut moved = sandbox.managed.clone();
        moved.user_git.home = PathBuf::from("/other/home");
        let before = sandbox.calls().len();
        moved
            .apply_config(&binary, "1.7.5", Lists::default)
            .unwrap();
        assert!(
            sandbox.calls().len() > before,
            "a changed wrapper applies the config again"
        );
        assert!(std::fs::read_to_string(&wrapper)
            .unwrap()
            .contains("/other/home"));
    }
}
