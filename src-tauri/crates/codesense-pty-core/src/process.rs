// SPDX-License-Identifier: MIT
//! The process census the runtime's idle / agent-liveness probes share — one
//! copy, consumed by the session registry (`is_idle`), the host's batched
//! agent-liveness commands, and the host's memory sampler.

/// Returns `true` if `pid` has at least one live descendant process — i.e. the
/// shell is running a foreground or background command rather than sitting at a
/// bare prompt. This is the single source of truth for the idle/running
/// distinction (tab close, the idle-eviction guard, the status dot); nothing
/// else re-implements process inspection.
///
/// A direct-child check is equivalent to a full descendant walk here: when an
/// intermediate process exits, the OS reparents its children away from our
/// shell (to `launchd`/`init`), so any *still-live* descendant implies a still
/// live direct child of the shell. This also correctly counts a backgrounded
/// `dev-server &` as "running" (idle eviction must never evict it), unlike a
/// foreground-process-group check.
///
/// Called on demand (tab close, eviction checks) — never in a hot loop — so the
/// one-shot process enumeration cost is amortized away.
pub(crate) fn has_live_descendant(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    // We only read parent PIDs, which `ProcessRefreshKind::new()` always
    // retrieves — so skip the per-process cpu/memory/disk syscalls that
    // `System::new_all()` would do (this query runs on every tab-close /
    // eviction check).
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
    let root = Pid::from_u32(pid);
    sys.processes().values().any(|p| p.parent() == Some(root))
}

/// Prompt helpers a shell keeps running as background children between commands.
/// They are part of the prompt, not the user's work, so they never make a shell
/// read as busy. Matched as a name prefix (the real binary carries a platform
/// suffix, e.g. `gitstatusd-darwin-arm64`, powerlevel10k's git status daemon).
const PROMPT_HELPERS: &[&str] = &["gitstatusd"];

fn is_prompt_helper(name: &str) -> bool {
    PROMPT_HELPERS.iter().any(|h| name.starts_with(h))
}

/// What a shell is running: the job in the terminal's foreground (the user is
/// running a command — `claude`, a build, `vim`), and its background jobs
/// (`dev-server &`), prompt helpers excluded. Both empty = a bare prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningJobs {
    /// The foreground job's name, when the terminal's foreground process group
    /// is not the shell's own.
    pub foreground: Option<String>,
    /// Names of the shell's background child jobs, deduplicated.
    pub background: Vec<String>,
}

/// A process's `(process group, terminal foreground process group)`, read from
/// the OS process table — pid-based, so it answers for a session hosted by any
/// terminal host (in-process or the session-server sidecar). `None` when the
/// process is gone or the platform can't say.
#[cfg(target_os = "macos")]
fn pgid_and_tpgid(pid: u32) -> Option<(i32, i32)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a correctly sized, writable `proc_bsdinfo`.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some((info.pbi_pgid as i32, info.e_tpgid as i32))
}

#[cfg(target_os = "linux")]
fn pgid_and_tpgid(pid: u32) -> Option<(i32, i32)> {
    // `/proc/<pid>/stat`: `pid (comm) state ppid pgrp session tty_nr tpgid …`;
    // `comm` may hold spaces/parens, so split after the LAST `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f.get(2)?.parse().ok()?, f.get(5)?.parse().ok()?))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pgid_and_tpgid(_pid: u32) -> Option<(i32, i32)> {
    None
}

/// One process under a shell: `(pid, parent pid, name, process group)`.
type Proc = (u32, u32, String, Option<i32>);

/// The jobs of each shell (see [`RunningJobs`]). One process-table scan answers
/// the whole batch; a shell with no pid (gone, or not started) reads idle.
pub fn running_jobs(shell_pids: &[Option<u32>]) -> Vec<RunningJobs> {
    running_jobs_known(shell_pids)
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect()
}

/// [`running_jobs`], `None` for a shell whose jobs cannot be told: no pid, or
/// a pid the process table does not hold (an unreadable or raced table). An
/// unreadable table never reads as a bare prompt (a wake must never
/// take it for an agent that quit).
pub fn running_jobs_known(shell_pids: &[Option<u32>]) -> Vec<Option<RunningJobs>> {
    if shell_pids.iter().all(Option::is_none) {
        return vec![None; shell_pids.len()];
    }
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
    let listed: std::collections::HashSet<u32> =
        sys.processes().keys().map(|pid| pid.as_u32()).collect();
    let table: Vec<(u32, u32, String)> = sys
        .processes()
        .iter()
        .filter_map(|(pid, p)| {
            Some((
                pid.as_u32(),
                p.parent()?.as_u32(),
                p.name().to_string_lossy().into_owned(),
            ))
        })
        .collect();
    shell_pids
        .iter()
        .map(|shell| {
            let shell = (*shell).filter(|pid| listed.contains(pid))?;
            let subtree: Vec<Proc> = descendants(&table, shell)
                .into_iter()
                .map(|(pid, ppid, name)| (pid, ppid, name, pgid_and_tpgid(pid).map(|(g, _)| g)))
                .collect();
            Some(classify_jobs(shell, pgid_and_tpgid(shell), &subtree))
        })
        .collect()
}

/// Every live process under `root`, at any depth.
fn descendants(table: &[(u32, u32, String)], root: u32) -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(cur) = stack.pop() {
        for (pid, ppid, name) in table {
            if *ppid == cur && !out.iter().any(|(p, _, _)| p == pid) {
                out.push((*pid, *ppid, name.clone()));
                stack.push(*pid);
            }
        }
    }
    out
}

/// Pure half of [`running_jobs`]. `groups` is the shell's `(pgid, tpgid)`;
/// `subtree` every process under it.
///
/// - Foreground: the terminal's foreground group is not the shell's → the job
///   leading that group (else any process in it).
/// - Background: the shell's direct children outside the foreground group,
///   prompt helpers excluded.
/// - Groups unknown → fail toward "running": any non-helper child reads as a
///   foreground job, so the default close keeps it rather than killing it.
fn classify_jobs(shell: u32, groups: Option<(i32, i32)>, subtree: &[Proc]) -> RunningJobs {
    let children = subtree
        .iter()
        .filter(|(_, ppid, name, _)| *ppid == shell && !is_prompt_helper(name));
    let Some((shell_pgid, tpgid)) = groups else {
        return RunningJobs {
            foreground: children.map(|(_, _, name, _)| name.clone()).next(),
            background: Vec::new(),
        };
    };
    let foreground = (tpgid > 0 && tpgid != shell_pgid).then(|| {
        subtree
            .iter()
            .find(|(pid, ..)| *pid as i32 == tpgid)
            .or_else(|| subtree.iter().find(|(.., g)| *g == Some(tpgid)))
            .map_or_else(
                || "a foreground job".to_string(),
                |(_, _, name, _)| name.clone(),
            )
    });
    let mut background: Vec<String> = Vec::new();
    for (_, _, name, pgid) in children {
        if *pgid != Some(tpgid) && !background.contains(name) {
            background.push(name.clone());
        }
    }
    RunningJobs {
        foreground,
        background,
    }
}

/// Interpreters an agent CLI may run under as a script (`#!/usr/bin/env node`,
/// a Python entry point): for these the script — argv's first non-flag entry
/// after the interpreter — names the program. Matched on the lowercased basename,
/// `python` as a prefix (`python3`, `python3.12`, macOS's `Python`).
fn is_interpreter(basename: &str) -> bool {
    matches!(basename, "node" | "nodejs" | "bun") || basename.starts_with("python")
}

/// A path's last component, lowercased.
fn basename_lower(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
}

/// Does a process run the agent CLI `needle_lower` (already lowercased) as its
/// EXECUTABLE? Pure so it unit-tests the shim shapes without a live process tree.
///
/// Matches only what names the program, never an arbitrary path segment or
/// argument (`node /x/claude/server.js`, `cat CLAUDE.md`, a cwd under
/// `~/claude/` are not the agent):
/// - the process name, the resolved exe's basename, or argv[0]'s basename;
/// - under a script interpreter (argv[0] `node`, `bun`, `python3`, …), the
///   basename of the script it runs — an npm shim `node /usr/local/bin/claude` —
///   with a `.js`/`.mjs`/`.cjs`/`.py` extension ignored.
pub fn agent_signature_matches(
    name: &str,
    exe: Option<&str>,
    cmd: &[&str],
    needle_lower: &str,
) -> bool {
    if needle_lower.is_empty() {
        return false;
    }
    if name.to_lowercase() == needle_lower || exe.is_some_and(|e| basename_lower(e) == needle_lower)
    {
        return true;
    }
    let Some(argv0) = cmd.first().map(|a| basename_lower(a)) else {
        return false;
    };
    if argv0 == needle_lower {
        return true;
    }
    let interpreted = is_interpreter(&argv0)
        || is_interpreter(&name.to_lowercase())
        || exe.is_some_and(|e| is_interpreter(&basename_lower(e)));
    if !interpreted {
        return false;
    }
    cmd.iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .is_some_and(|script| {
            let base = basename_lower(script);
            let stem = [".js", ".mjs", ".cjs", ".py"]
                .iter()
                .find_map(|ext| base.strip_suffix(ext))
                .unwrap_or(&base);
            stem == needle_lower
        })
}

/// A ONE-shot snapshot of the process table — the expensive
/// `System::new()` + `refresh_processes_specifics(All, …with_exe/with_cmd)` (the
/// slow macOS `KERN_PROCARGS2` scan) + child-adjacency build — captured once and
/// then queried per-root by [`ProcessIndex::has_named_process`]. This lets the
/// batched liveness command amortize a single enumeration across N sessions
/// instead of paying one full scan per session.
pub struct ProcessIndex {
    /// Parent → children adjacency over every live process.
    children: std::collections::HashMap<u32, Vec<u32>>,
    /// pid → (process name, exe path, argv) — everything the matcher inspects.
    info: std::collections::HashMap<u32, ProcInfo>,
}

struct ProcInfo {
    name: String,
    exe: Option<String>,
    cmd: Vec<String>,
    /// Resident set size in bytes (0 if the refresh didn't capture it).
    /// Populated by the additive `.with_memory()` refresh; unused by the
    /// descendant matcher, read by the memory sampler (its forensic line and the
    /// renderer-pressure read).
    memory_bytes: u64,
}

impl ProcessIndex {
    /// Whether the snapshot holds `pid` (an unreadable or raced table may not).
    pub fn has_process(&self, pid: u32) -> bool {
        self.info.contains_key(&pid)
    }

    /// Build one snapshot of the full process table. No memory
    /// refresh — byte-identical to the pre-RF-perf-5 descendant matcher, and the
    /// always-on agent-liveness callers never pay for RSS they don't read.
    pub fn build() -> Self {
        Self::build_inner(false)
    }

    /// RF-perf-5: like [`build`] but also captures per-process RSS. Callers:
    /// RF-perf-5's forensic memory line (behind `memory_enabled()`) and — since
    /// RF-perf-7 — the renderer-watchdog's every-~60 s pressure sample, which runs
    /// this **ungated** (the renderer RSS is the production pressure signal; FE
    /// heap is engine-gated to `unavailable` on WebKit). One snapshot feeds both,
    /// on the watchdog thread, off the UI/render thread.
    pub fn build_with_memory() -> Self {
        Self::build_inner(true)
    }

    fn build_inner(refresh_memory: bool) -> Self {
        use std::collections::HashMap;
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

        let mut refresh = ProcessRefreshKind::new()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always);
        if refresh_memory {
            refresh = refresh.with_memory();
        }
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        let procs = sys.processes();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut info: HashMap<u32, ProcInfo> = HashMap::new();
        for (cpid, proc_) in procs {
            let cpid = cpid.as_u32();
            if let Some(parent) = proc_.parent() {
                children.entry(parent.as_u32()).or_default().push(cpid);
            }
            info.insert(
                cpid,
                ProcInfo {
                    name: proc_.name().to_string_lossy().into_owned(),
                    exe: proc_.exe().map(|p| p.to_string_lossy().into_owned()),
                    cmd: proc_
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect(),
                    memory_bytes: proc_.memory(),
                },
            );
        }
        ProcessIndex { children, info }
    }

    /// RF-perf-5: iterate `(pid, name, exe, memory_bytes)` over the snapshot for
    /// the gated memory sampler. Read-only view; the sampler classifies WebKit
    /// helpers and sums their RSS off this without a second process-table scan.
    pub fn iter_procs(&self) -> impl Iterator<Item = (u32, &str, Option<&str>, u64)> {
        self.info.iter().map(|(&pid, info)| {
            (
                pid,
                info.name.as_str(),
                info.exe.as_deref(),
                info.memory_bytes,
            )
        })
    }

    /// Is the agent `agent_name` running as the PTY's FOREGROUND job? The PTY
    /// root itself counts (a native launch runs the agent in place of a shell);
    /// otherwise a matching process anywhere under the root must belong to the
    /// terminal's foreground process group — a `claude &` in the background of
    /// an interactive shell is the shell's job, not an agent tab.
    pub fn has_named_process(&self, root_pid: u32, agent_name: &str) -> bool {
        self.has_foreground_named_process(root_pid, agent_name, pgid_and_tpgid)
    }

    /// [`Self::has_named_process`] with the `(pgid, tpgid)` reader injected, so the
    /// foreground rule unit-tests over a synthetic tree. When the root's groups
    /// can't be read (unsupported platform, process racing exit), any matching
    /// process under the root counts.
    fn has_foreground_named_process(
        &self,
        root_pid: u32,
        agent_name: &str,
        groups: impl Fn(u32) -> Option<(i32, i32)>,
    ) -> bool {
        use std::collections::HashSet;

        if agent_name.is_empty() {
            return false;
        }
        let needle = agent_name.to_lowercase();
        let matches = |pid: u32| {
            self.info.get(&pid).is_some_and(|p| {
                let args: Vec<&str> = p.cmd.iter().map(String::as_str).collect();
                agent_signature_matches(&p.name, p.exe.as_deref(), &args, &needle)
            })
        };
        if matches(root_pid) {
            return true;
        }
        let foreground = groups(root_pid)
            .map(|(_, tpgid)| tpgid)
            .filter(|tpgid| *tpgid > 0);
        // Child adjacency DFS: the foreground job may sit under a wrapper.
        let mut stack = vec![root_pid];
        let mut seen: HashSet<u32> = HashSet::new();
        while let Some(cur) = stack.pop() {
            let Some(kids) = self.children.get(&cur) else {
                continue;
            };
            for &kid in kids {
                if !seen.insert(kid) {
                    continue;
                }
                if matches(kid)
                    && foreground.is_none_or(|tpgid| groups(kid).is_some_and(|(g, _)| g == tpgid))
                {
                    return true;
                }
                stack.push(kid);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A shell the process table does not hold is unknown, never
    // a bare prompt (which a wake would read as an agent that quit).
    #[test]
    fn a_shell_missing_from_the_process_table_is_unknown_not_idle() {
        let own = std::process::id();
        let gone = u32::MAX - 7;
        let known = running_jobs_known(&[Some(own), Some(gone), None]);
        assert!(
            known[0].is_some(),
            "this test's own process is in the table"
        );
        assert_eq!(known[1], None);
        assert_eq!(known[2], None);
        let index = ProcessIndex::build();
        assert!(index.has_process(own));
        assert!(!index.has_process(gone));
    }

    /// The agent matcher keys on what names the program: the process name, the
    /// exe basename, argv[0], or the script an interpreter runs — never an
    /// arbitrary argument or path segment.
    #[test]
    fn agent_signature_matches_the_executable_only() {
        // direct binary: name == claude
        assert!(agent_signature_matches(
            "claude",
            Some("/opt/homebrew/bin/claude"),
            &["claude"],
            "claude"
        ));
        // native install: the process is named for its version file, argv[0] is `claude`
        assert!(agent_signature_matches(
            "2.0.14",
            Some("/Users/me/.local/share/claude/versions/2.0.14"),
            &["claude", "--resume"],
            "claude"
        ));
        // npm shim: `#!/usr/bin/env node` runs `node /usr/local/bin/claude`
        assert!(agent_signature_matches(
            "node",
            Some("/usr/local/bin/node"),
            &["node", "/usr/local/bin/claude", "--interactive"],
            "claude"
        ));
        // interpreter flags before the script are skipped; a `.js` stem counts
        assert!(agent_signature_matches(
            "node",
            Some("/usr/local/bin/node"),
            &["/usr/local/bin/node", "--no-warnings", "/x/bin/codex.js"],
            "codex"
        ));
        // a python entry point
        assert!(agent_signature_matches(
            "Python",
            Some("/opt/homebrew/bin/python3.12"),
            &["/opt/homebrew/bin/python3.12", "/Users/me/.local/bin/aider"],
            "aider"
        ));
        // case-insensitive
        assert!(agent_signature_matches("Claude", None, &[], "claude"));
        // a node server that merely lives under a `claude/` directory is not the agent
        assert!(!agent_signature_matches(
            "node",
            Some("/usr/local/bin/node"),
            &["node", "/x/claude/server.js"],
            "claude"
        ));
        // an argument naming the CLI is not the CLI (only the interpreter's script is)
        assert!(!agent_signature_matches(
            "node",
            Some("/usr/local/bin/node"),
            &["node", "/x/tool.js", "claude"],
            "claude"
        ));
        assert!(!agent_signature_matches(
            "man",
            Some("/usr/bin/man"),
            &["man", "claude"],
            "claude"
        ));
        // a bare zsh prompt — no claude anywhere
        assert!(!agent_signature_matches(
            "zsh",
            Some("/bin/zsh"),
            &["-zsh"],
            "claude"
        ));
        // a file or dir mentioning the name is not the agent
        assert!(!agent_signature_matches(
            "cat",
            Some("/bin/cat"),
            &["cat", "CLAUDE.md"],
            "claude"
        ));
        assert!(!agent_signature_matches(
            "less",
            Some("/usr/bin/less"),
            &["less", "/Users/me/claude/settings.json"],
            "claude"
        ));
        // empty needle never matches
        assert!(!agent_signature_matches("claude", None, &["claude"], ""));
    }

    fn proc_info(name: &str, exe: Option<&str>, cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            name: name.to_string(),
            exe: exe.map(str::to_string),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            memory_bytes: 0,
        }
    }

    /// A synthetic shell tree: `(pid, parent, info, pgid)` per process; the
    /// terminal's foreground group is `tpgid`.
    fn tree(
        procs: Vec<(u32, u32, ProcInfo, i32)>,
        tpgid: i32,
    ) -> (ProcessIndex, impl Fn(u32) -> Option<(i32, i32)>) {
        use std::collections::HashMap;
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut info = HashMap::new();
        let mut groups = HashMap::new();
        for (pid, ppid, p, pgid) in procs {
            children.entry(ppid).or_default().push(pid);
            info.insert(pid, p);
            groups.insert(pid, (pgid, tpgid));
        }
        (ProcessIndex { children, info }, move |pid| {
            groups.get(&pid).copied()
        })
    }

    fn zsh() -> ProcInfo {
        proc_info("zsh", Some("/bin/zsh"), &["-zsh"])
    }

    /// Only the PTY's foreground job makes a tab an agent.
    #[test]
    fn an_agent_tab_needs_the_cli_as_the_foreground_job() {
        let claude = || proc_info("claude", Some("/opt/homebrew/bin/claude"), &["claude"]);
        // foreground claude under an interactive shell → agent
        let (index, groups) = tree(vec![(10, 1, zsh(), 10), (11, 10, claude(), 11)], 11);
        assert!(index.has_foreground_named_process(10, "claude", &groups));
        // `claude &` at the prompt: its own group, the shell's group in front → not agent
        let (index, groups) = tree(vec![(10, 1, zsh(), 10), (11, 10, claude(), 11)], 10);
        assert!(!index.has_foreground_named_process(10, "claude", &groups));
        // a background claude while `vim` holds the terminal → not agent
        let (index, groups) = tree(
            vec![
                (10, 1, zsh(), 10),
                (11, 10, claude(), 11),
                (12, 10, proc_info("vim", None, &["vim"]), 12),
            ],
            12,
        );
        assert!(!index.has_foreground_named_process(10, "claude", &groups));
        // foreground `node /x/claude/server.js` → not agent
        let server = proc_info(
            "node",
            Some("/usr/local/bin/node"),
            &["node", "/x/claude/server.js"],
        );
        let (index, groups) = tree(vec![(10, 1, zsh(), 10), (11, 10, server, 11)], 11);
        assert!(!index.has_foreground_named_process(10, "claude", &groups));
        // foreground npm shim `node …/bin/claude` → agent
        let shim = proc_info(
            "node",
            Some("/usr/local/bin/node"),
            &["node", "/usr/local/lib/node_modules/.bin/claude"],
        );
        let (index, groups) = tree(vec![(10, 1, zsh(), 10), (11, 10, shim, 11)], 11);
        assert!(index.has_foreground_named_process(10, "claude", &groups));
        // foreground codex: the npm launcher spawns the native binary in its group → agent
        let (index, groups) = tree(
            vec![
                (10, 1, zsh(), 10),
                (
                    11,
                    10,
                    proc_info("node", None, &["node", "/usr/local/bin/codex"]),
                    11,
                ),
                (
                    12,
                    11,
                    proc_info("codex", Some("/x/vendor/codex"), &["/x/vendor/codex"]),
                    11,
                ),
            ],
            11,
        );
        assert!(index.has_foreground_named_process(10, "codex", &groups));
        assert!(!index.has_foreground_named_process(10, "claude", &groups));
        // a native launch: the PTY root is the agent itself
        let (index, groups) = tree(vec![(10, 1, claude(), 10)], 10);
        assert!(index.has_foreground_named_process(10, "claude", &groups));
        // groups unreadable → any matching process under the root counts
        let (index, _) = tree(vec![(10, 1, zsh(), 10), (11, 10, claude(), 11)], 10);
        assert!(index.has_foreground_named_process(10, "claude", |_| None));
        // empty name never matches
        assert!(!index.has_foreground_named_process(10, "", |_| None));
    }

    /// A shell running a command has a live descendant.
    /// Short sleep so any post-kill reparented child self-reaps quickly.
    #[test]
    fn idle_detects_running_command() {
        use std::process::{Command, Stdio};
        // The trailing `; true` defeats the shell's single-command exec
        // optimization (`sh -c "sleep 2"` would `exec` sleep and leave no
        // child), forcing `sh` to fork `sleep` — exactly how an interactive PTY
        // shell runs a foreground job.
        let mut parent = Command::new("sh")
            .args(["-c", "sleep 2; true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        std::thread::sleep(std::time::Duration::from_millis(250)); // let sh fork sleep
        let running = has_live_descendant(parent.id());
        let _ = parent.kill();
        let _ = parent.wait();
        assert!(running, "sh running `sleep` must report a live descendant");
    }

    fn job(pid: u32, ppid: u32, name: &str, pgid: i32) -> Proc {
        (pid, ppid, name.to_string(), Some(pgid))
    }

    /// The foreground group names the job; background children outside it are
    /// listed; prompt helpers never count.
    #[test]
    fn classify_splits_foreground_background_and_helpers() {
        let subtree = vec![
            job(11, 10, "claude", 11),
            job(12, 11, "node", 11),
            job(13, 10, "sleep", 13),
            job(14, 10, "gitstatusd-darwin-arm64", 14),
        ];
        assert_eq!(
            classify_jobs(10, Some((10, 11)), &subtree),
            RunningJobs {
                foreground: Some("claude".into()),
                background: vec!["sleep".into()],
            }
        );
        // At the prompt (foreground group = the shell's own): not foreground.
        assert_eq!(
            classify_jobs(10, Some((10, 10)), &subtree[2..]),
            RunningJobs {
                foreground: None,
                background: vec!["sleep".into()],
            }
        );
        // Unknown groups → a non-helper child reads as foreground (fail safe).
        assert_eq!(
            classify_jobs(10, None, &subtree[2..]).foreground,
            Some("sleep".into())
        );
        assert_eq!(
            classify_jobs(10, None, &subtree[3..]),
            RunningJobs::default()
        );
    }

    /// An interactive shell in a real PTY, driven by typing into it.
    struct PtyShell {
        pid: u32,
        writer: Box<dyn std::io::Write + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        _master: Box<dyn portable_pty::MasterPty + Send>,
        _reader: std::thread::JoinHandle<()>,
    }

    impl PtyShell {
        fn spawn() -> Self {
            use portable_pty::{native_pty_system, CommandBuilder, PtySize};
            let pair = native_pty_system()
                .openpty(PtySize::default())
                .expect("openpty");
            let mut cmd = CommandBuilder::new("/bin/bash");
            cmd.args(["--norc", "--noprofile", "-i"]);
            let child = pair.slave.spawn_command(cmd).expect("spawn bash");
            let pid = child.process_id().expect("pid");
            let mut reader = pair.master.try_clone_reader().expect("reader");
            // Drain output so the shell never blocks on a full PTY buffer.
            let drain = std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while matches!(std::io::Read::read(&mut reader, &mut buf), Ok(n) if n > 0) {}
            });
            let writer = pair.master.take_writer().expect("writer");
            PtyShell {
                pid,
                writer,
                child,
                _master: pair.master,
                _reader: drain,
            }
        }

        fn type_line(&mut self, line: &str) {
            self.writer
                .write_all(format!("{line}\n").as_bytes())
                .unwrap();
            self.writer.flush().unwrap();
        }

        fn wait_for(&self, want: impl Fn(&RunningJobs) -> bool) -> RunningJobs {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let jobs = running_jobs(&[Some(self.pid)]).remove(0);
                if want(&jobs) || std::time::Instant::now() > deadline {
                    return jobs;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    impl Drop for PtyShell {
        fn drop(&mut self) {
            let _ = self.child.kill();
        }
    }

    /// A foreground `sleep` makes the shell read as running, naming it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_foreground_command_reads_as_running() {
        let mut sh = PtyShell::spawn();
        sh.type_line("sleep 30");
        // (A snapshot racing the fork may briefly see the group before the child.)
        let jobs = sh.wait_for(|j| j.foreground.as_deref() == Some("sleep"));
        assert_eq!(jobs.foreground.as_deref(), Some("sleep"));
    }

    /// At the prompt with a prompt helper and a `sleep &` in the background: not
    /// foreground, and the background lists only `sleep`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_prompt_with_a_helper_and_a_background_job_is_not_foreground() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("gitstatusd-test");
        std::fs::copy("/bin/sleep", &helper).unwrap();
        let mut sh = PtyShell::spawn();
        sh.type_line(&format!("{} 30 &", helper.display()));
        sh.type_line("sleep 30 &");
        let jobs = sh.wait_for(|j| j.background.contains(&"sleep".to_string()));
        // Give a stray helper time to (wrongly) show up before asserting.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let jobs = running_jobs(&[Some(sh.pid)]).pop().unwrap_or(jobs);
        assert_eq!(
            jobs,
            RunningJobs {
                foreground: None,
                background: vec!["sleep".into()],
            }
        );
    }

    /// `sleep` linked as `claude`, standing in for the CLI in a real PTY (argv[0]
    /// names it; a copied system binary would be killed by macOS code signing).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn fake_claude() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &bin).unwrap();
        (dir, bin)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn wait_agent(sh: &PtyShell, want: bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = ProcessIndex::build().has_named_process(sh.pid, "claude");
            if got == want || std::time::Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// A `claude` typed at the prompt is the tab's foreground job: an agent.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_foreground_cli_in_a_real_pty_is_an_agent() {
        let (_dir, bin) = fake_claude();
        let mut sh = PtyShell::spawn();
        sh.type_line(&format!("{} 30", bin.display()));
        assert!(wait_agent(&sh, true));
    }

    /// `claude &` leaves the shell in the foreground: not an agent.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_background_cli_in_a_real_pty_is_not_an_agent() {
        let (_dir, bin) = fake_claude();
        let mut sh = PtyShell::spawn();
        sh.type_line(&format!("{} 30 &", bin.display()));
        let jobs = sh.wait_for(|j| !j.background.is_empty());
        assert!(!jobs.background.is_empty(), "the background job started");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!ProcessIndex::build().has_named_process(sh.pid, "claude"));
    }

    /// A bare leaf process (no children) is idle.
    #[test]
    fn idle_true_for_leaf_process() {
        use std::process::{Command, Stdio};
        let mut leaf = Command::new("sleep")
            .arg("2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        std::thread::sleep(std::time::Duration::from_millis(250));
        let running = has_live_descendant(leaf.id());
        let _ = leaf.kill();
        let _ = leaf.wait();
        assert!(!running, "a bare `sleep` has no descendants");
    }
}
