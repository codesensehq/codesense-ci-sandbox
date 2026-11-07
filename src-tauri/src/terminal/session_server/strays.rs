// SPDX-License-Identifier: MIT
//! SW-AGR-19 PR-7: stray terminal servers on This Mac. Every
//! `codesense-session-server` the user runs, other than the one this app is
//! attached to, is listed with its executable, its `--root` (canonical), when
//! it started and — when its root's socket answers as that very process — how
//! many sessions it holds. Each is classified against this app's bundled
//! server and this instance's root. A stray stops (SIGTERM, then SIGKILL) only
//! after its own confirmation, and only if the same process (pid and start
//! time) still serves the same root when the signal is sent.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) use codesense_pty_core::activity::HELPER_NAME;

use super::{read_server_identity, ConnectError, SIDECAR_BIN};

/// How long a stray has to exit after SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// How long a SIGKILLed stray has to be gone.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// One `codesense-session-server` process, as `ps` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProcess {
    pub pid: u32,
    pub uid: u32,
    /// When it started, exactly as `ps -o lstart` prints it: with the pid,
    /// it names this process and no later one that reuses the pid.
    pub started: String,
    pub exe: String,
    pub root: String,
}

/// How a stray relates to this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StrayKind {
    /// This app's own server, serving another root (a leaked test fixture,
    /// another profile).
    Bundled,
    /// Another CodeSense build's server (a dev worktree, an older install)
    /// serving another root.
    OtherBuild,
    /// Another server on this instance's own root (it cannot be serving it:
    /// the attached one holds the lock).
    OwnRoot,
}

/// A stray terminal server, for the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrayServer {
    pub pid: u32,
    /// Its start time as `ps` prints it: with the pid, its identity.
    pub started: String,
    /// The same, as epoch milliseconds (local time), for the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    pub exe: String,
    pub root: String,
    pub kind: StrayKind,
    /// The sessions it holds, when its root's socket answered as this very
    /// process; `None` when that could not be read.
    pub sessions: Option<u32>,
}

/// Which stray to stop: the exact process the person confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrayTarget {
    pub pid: u32,
    pub started: String,
    pub root: String,
}

/// How a stop ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum StrayStop {
    /// It exited (`killed`: only after SIGKILL).
    Stopped { killed: bool },
    /// It is no longer the process that was confirmed (gone, or its pid now
    /// names another process): nothing was signalled.
    Gone,
    /// It is the server this app is attached to: never stopped here.
    Attached,
    /// It did not exit even after SIGKILL, or could not be signalled.
    Failed { message: String },
}

/// The options a terminal server itself takes.
const SERVER_OPTIONS: [&str; 3] = ["--root ", "--resources ", "--emulator "];

/// The options a terminal server takes, as argv items.
pub(crate) const SERVER_ARGS: [&str; 3] = ["--root", "--resources", "--emulator"];

/// The fixed width of `ps -o lstart`: `Thu Oct  8 11:35:00 2026`.
const LSTART_WIDTH: usize = 24;

/// One line of `ps -Aww -o pid=,uid=,lstart=,args=` as a terminal server, or
/// `None` for any other process (a server's sub-modes — `attach`, the MCP
/// supervisors, `private-serve` — included).
pub fn parse_ps_line(line: &str) -> Option<ServerProcess> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (uid, rest) = rest.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if rest.len() < LSTART_WIDTH || !rest.is_char_boundary(LSTART_WIDTH) {
        return None;
    }
    let (started, args) = rest.split_at(LSTART_WIDTH);
    let args = args.trim();
    // The executable — the first mention of the server's name that runs up to
    // its options without a space (a suffixed name such as
    // `codesense-session-server-aarch64-apple-darwin` is the bundled
    // sidecar's) — then only the server's own options.
    let (exe_end, options) = args.match_indices(SIDECAR_BIN).find_map(|(at, _)| {
        let after = &args[at + SIDECAR_BIN.len()..];
        let options_at = after.find(" --")?;
        (!after[..options_at].contains(' ')).then(|| {
            (
                at + SIDECAR_BIN.len() + options_at,
                &after[options_at + 1..],
            )
        })
    })?;
    let exe = &args[..exe_end];
    if !SERVER_OPTIONS
        .iter()
        .any(|option| options.starts_with(option))
    {
        return None;
    }
    // `--root`'s value runs to the next of the server's own options, in any
    // order.
    let (_, root) = options.split_once("--root ")?;
    let root_end = SERVER_OPTIONS
        .iter()
        .filter_map(|option| root.find(&format!(" {option}")))
        .min()
        .unwrap_or(root.len());
    let root = root[..root_end].trim();
    if root.is_empty() {
        return None;
    }
    Some(ServerProcess {
        pid: pid.parse().ok()?,
        uid: uid.parse().ok()?,
        started: started.trim().to_string(),
        exe: exe.to_string(),
        root: root.to_string(),
    })
}

/// The executable `pid` runs, as the kernel says (never its argv, which a
/// wrapper — `caffeinate`, `time`, a debugger — shares).
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` is valid for its length for the call.
        let len = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        if len <= 0 {
            return None;
        }
        buf.truncate(len as usize);
        Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
}

/// Whether `exe` is a terminal server's executable: its file name is the
/// server's (the bundled sidecar's carries a target suffix).
pub fn is_server_exe(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SIDECAR_BIN))
}

/// The executable a terminal server runs, from the kernel's path `kernel`,
/// the path in its argv it was `started_from`, and its `--root`; `None` when
/// the process is not a terminal server.
///
/// Each server hard-links its own binary to `<root>/activity-helper`, and
/// macOS may name a hard-linked executable by whichever of its links was last
/// opened: a running server can read as any server's helper link, its own or
/// another root's. So a kernel path counts when:
/// - its file name is the server's;
/// - it is the same file as the server path the process was started from
///   (then the server is named by that path); or
/// - it is the helper in the server's own root, or another root's link to that
///   same file, though the path it was started from is no longer that file (an
///   update replaced it while it ran).
///
/// A wrapper that names the server in its argv (`caffeinate`, `time`) runs
/// another file, so it is none of these. A helper process is refused before
/// this rule: by [`server_exe`] in the census, by `runs_sidecar` when ending one.
pub(crate) fn server_exe_of(
    kernel: &Path,
    started_from: &Path,
    root: Option<&str>,
) -> Option<PathBuf> {
    if is_server_exe(kernel) {
        return Some(kernel.to_path_buf());
    }
    // A relative path would be read against this process's directory, not the
    // server's, so only an absolute argv path and root are compared.
    if started_from.is_absolute() && is_server_exe(started_from) && same_file(started_from, kernel)
    {
        return Some(started_from.to_path_buf());
    }
    // Its own helper (the link it made to the binary it runs), or another
    // root's link to that same file: two servers left on an old build after an
    // update share it.
    let own_helper = kernel.file_name().is_some_and(|name| name == HELPER_NAME)
        && started_from.is_absolute()
        && is_server_exe(started_from)
        && root
            .filter(|root| Path::new(root).is_absolute())
            .is_some_and(|root| {
                let own = Path::new(root).join(HELPER_NAME);
                kernel
                    .parent()
                    .is_some_and(|dir| canonical(&dir.to_string_lossy()) == canonical(root))
                    || same_file(kernel, &own)
            });
    own_helper.then(|| kernel.to_path_buf())
}

/// [`server_exe_of`] for a process `ps` listed: its argv path and root. `ps`
/// prints argv as one line, so a helper process whose wrapped command names a
/// server (a prompt, say) can parse as one; a process whose argv starts with the
/// helper is never a server.
fn server_exe(process: &ServerProcess, kernel: PathBuf) -> Option<PathBuf> {
    // The parsed executable of a server is its path alone; a helper's runs on
    // into its own arguments. A path may hold spaces, so the helper is found by
    // its name followed by one.
    let helper_then_args = format!("/{HELPER_NAME} ");
    let bare_helper = format!("{HELPER_NAME} ");
    if process.exe.contains(&helper_then_args) || process.exe.starts_with(&bare_helper) {
        return None;
    }
    server_exe_of(&kernel, Path::new(&process.exe), Some(&process.root))
}

/// Whether two paths name the same file: the same path, or another hard link
/// to it (a server read through its `activity-helper` link).
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// `ps` with the start time in a fixed format and paths as they are.
fn ps() -> Option<String> {
    let out = std::process::Command::new("/bin/ps")
        .env_remove("LC_ALL")
        .env("LC_TIME", "C")
        .env("LC_CTYPE", "en_US.UTF-8")
        .args(["-Aww", "-o", "pid=,uid=,lstart=,args="])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every terminal server process `uid` runs, as `ps` lists them now: only
/// processes whose executable really is a terminal server (a wrapper that
/// names one in its arguments is not), with that executable.
pub fn server_processes(uid: u32) -> Vec<ServerProcess> {
    let Some(listed) = ps() else {
        return Vec::new();
    };
    servers_listed(&listed, uid, process_exe)
}

/// The terminal servers `uid` runs in `ps` output `listed`, each named by
/// [`server_exe`] from the kernel's path `exe_of` gives for its pid.
fn servers_listed(
    listed: &str,
    uid: u32,
    exe_of: impl Fn(u32) -> Option<PathBuf>,
) -> Vec<ServerProcess> {
    listed
        .split('\n')
        .filter_map(parse_ps_line)
        .filter(|process| process.uid == uid)
        .filter_map(|mut process| {
            let exe = server_exe(&process, exe_of(process.pid)?)?;
            process.exe = exe.to_string_lossy().into_owned();
            Some(process)
        })
        .collect()
}

/// Whether a listed `process` is named by its own root's activity helper: a
/// server whose started-from path is no longer the file it runs (see
/// [`server_exe_of`]).
#[cfg(test)]
fn named_by_own_helper(process: &ServerProcess) -> bool {
    let exe = Path::new(&process.exe);
    exe.file_name().is_some_and(|name| name == HELPER_NAME)
        && server_exe_of(exe, exe, Some(&process.root)).is_some()
}

/// `ps -o lstart` (`Thu Oct  8 11:35:00 2026`, local time) as epoch ms.
pub fn lstart_ms(started: &str) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(
        &started.split_whitespace().collect::<Vec<_>>().join(" "),
        "%a %b %d %H:%M:%S %Y",
    )
    .ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.timestamp_millis())
}

fn canonical(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// How `process` relates to this app: `None` for this instance's own server
/// — the one attached, or (while the app is reconnecting, or the server is
/// incompatible) the one holding this instance's lock (`owner_pid`). That one
/// is restarted or updated through its confirmed path, never stopped here.
pub fn classify(
    process: &ServerProcess,
    attached_pid: Option<u32>,
    owner_pid: Option<u32>,
    bundled: Option<&Path>,
    own_root: Option<&Path>,
) -> Option<StrayKind> {
    if Some(process.pid) == attached_pid || Some(process.pid) == owner_pid {
        return None;
    }
    let root = canonical(&process.root);
    if own_root.is_some_and(|own| canonical(&own.to_string_lossy()) == root) {
        return Some(StrayKind::OwnRoot);
    }
    let exe = canonical(&process.exe);
    Some(
        if bundled.is_some_and(|bundled| {
            canonical(&bundled.to_string_lossy()) == exe || same_file(bundled, &exe)
        }) {
            StrayKind::Bundled
        } else {
            StrayKind::OtherBuild
        },
    )
}

/// The sessions the server serving `root` holds, when it is process `pid`.
fn sessions_of(root: &Path, pid: u32) -> Option<u32> {
    match read_server_identity(root, |_| true) {
        Ok(Some(identity)) if identity.pid == pid => u32::try_from(identity.session_ids.len()).ok(),
        Err(ConnectError::Incompatible(skew)) if skew.host_pid == Some(pid) => skew.live_sessions,
        _ => None,
    }
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Every stray terminal server this user runs.
pub fn strays(
    attached_pid: Option<u32>,
    bundled: Option<&Path>,
    own_root: Option<&Path>,
) -> Vec<StrayServer> {
    let owner = own_root.and_then(codesense_pty_core::protocol::host_pid);
    let found: Vec<(ServerProcess, StrayKind)> = server_processes(current_uid())
        .into_iter()
        .filter_map(|process| {
            let kind = classify(&process, attached_pid, owner, bundled, own_root)?;
            Some((process, kind))
        })
        .collect();
    // Each stray's socket is asked at once, so a wedged one never holds up
    // the others.
    std::thread::scope(|scope| {
        let asked: Vec<_> = found
            .into_iter()
            .map(|(process, kind)| {
                scope.spawn(move || {
                    let root = canonical(&process.root);
                    StrayServer {
                        sessions: sessions_of(&root, process.pid),
                        started_at_ms: lstart_ms(&process.started),
                        pid: process.pid,
                        started: process.started,
                        exe: process.exe,
                        root: root.to_string_lossy().into_owned(),
                        kind,
                    }
                })
            })
            .collect();
        asked
            .into_iter()
            .filter_map(|asking| asking.join().ok())
            .collect()
    })
}

/// Whether `pid` names a live process (one this user may not signal counts
/// as alive: never taken for gone).
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks the pid exists.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether the confirmed process still runs as it was: the same pid, start
/// time and root.
fn still_the_same(target: &StrayTarget) -> bool {
    server_processes(current_uid()).iter().any(|process| {
        process.pid == target.pid
            && process.started == target.started
            && canonical(&process.root) == canonical(&target.root)
    })
}

fn wait_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}

/// Stop the confirmed stray: SIGTERM, then SIGKILL — only if it is still the
/// same process serving the same root, and never the attached server.
pub fn stop(target: &StrayTarget, attached_pid: Option<u32>, own_root: Option<&Path>) -> StrayStop {
    let owner = own_root.and_then(codesense_pty_core::protocol::host_pid);
    if Some(target.pid) == attached_pid || Some(target.pid) == owner || target.pid <= 1 {
        return StrayStop::Attached;
    }
    if !still_the_same(target) {
        return StrayStop::Gone;
    }
    let signal = |sig| {
        // SAFETY: the pid was just checked to be the confirmed process.
        unsafe { libc::kill(target.pid as libc::pid_t, sig) == 0 }
    };
    if !signal(libc::SIGTERM) {
        return if alive(target.pid) {
            StrayStop::Failed {
                message: std::io::Error::last_os_error().to_string(),
            }
        } else {
            StrayStop::Gone
        };
    }
    if wait_gone(target.pid, TERM_GRACE) {
        return StrayStop::Stopped { killed: false };
    }
    // Still the same process before the harder signal.
    if !still_the_same(target) {
        return if alive(target.pid) {
            StrayStop::Failed {
                message: "it did not exit, and is no longer the confirmed process".to_string(),
            }
        } else {
            StrayStop::Stopped { killed: false }
        };
    }
    signal(libc::SIGKILL);
    if wait_gone(target.pid, KILL_GRACE) {
        StrayStop::Stopped { killed: true }
    } else {
        StrayStop::Failed {
            message: "it did not exit".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agr19_a_ps_line_reads_as_a_server_with_its_root() {
        let line = "  763 501 Sat Oct  3 09:12:44 2026     /Users/me/codesense/target/release/bundle/macos/CodeSense.app/Contents/MacOS/codesense-session-server --resources /Users/me/r --root /Users/me/Library/Application Support/codesense";
        assert_eq!(
            parse_ps_line(line),
            Some(ServerProcess {
                pid: 763,
                uid: 501,
                started: "Sat Oct  3 09:12:44 2026".into(),
                exe: "/Users/me/codesense/target/release/bundle/macos/CodeSense.app/Contents/MacOS/codesense-session-server".into(),
                root: "/Users/me/Library/Application Support/codesense".into(),
            })
        );
        // The bundled sidecar's suffixed name.
        let bundled = parse_ps_line(
            "9 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server-aarch64-apple-darwin --root /tmp/x",
        )
        .unwrap();
        assert_eq!(
            bundled.exe,
            "/A/codesense-session-server-aarch64-apple-darwin"
        );
        assert_eq!(bundled.root, "/tmp/x");
    }

    #[test]
    fn agr19_a_root_naming_the_server_or_options_in_any_order_still_parse() {
        let named = parse_ps_line(
            "9 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server --root /tmp/codesense-session-server-fixture",
        )
        .unwrap();
        assert_eq!(named.exe, "/A/codesense-session-server");
        assert_eq!(named.root, "/tmp/codesense-session-server-fixture");
        let reordered = parse_ps_line(
            "9 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server --root /Users/me/My Root --emulator /A/emu --resources /A/r",
        )
        .unwrap();
        assert_eq!(reordered.root, "/Users/me/My Root");
    }

    #[test]
    fn agr19_a_wrapper_naming_the_server_is_not_a_server() {
        // Its argv names the server, so `ps` parsing alone would take it for
        // one: its real executable says otherwise.
        let wrapped = parse_ps_line(
            "9 501 Thu Oct  8 11:35:00 2026 caffeinate -i /A/codesense-session-server --root /tmp/x",
        );
        if let Some(wrapped) = wrapped {
            assert!(
                !is_server_exe(Path::new("/usr/bin/caffeinate")),
                "{wrapped:?}"
            );
        }
        assert!(is_server_exe(Path::new("/A/codesense-session-server")));
        assert!(is_server_exe(Path::new(
            "/A/codesense-session-server-aarch64-apple-darwin"
        )));
        assert!(!is_server_exe(Path::new("/usr/bin/rg")));
        // This test process's own executable is no terminal server.
        assert!(process_exe(std::process::id()).is_some_and(|exe| !is_server_exe(&exe)));
        // Each listed server is named by a server executable, or by its own
        // root's helper when its bundled file was replaced while it ran.
        assert!(server_processes(unsafe { libc::getuid() })
            .iter()
            .all(|process| is_server_exe(Path::new(&process.exe)) || named_by_own_helper(process)));
    }

    #[test]
    fn agr19_a_server_read_through_its_own_activity_helper_link_is_still_that_server() {
        // A server hard-links itself to `<root>/activity-helper`; the kernel may
        // then name the running server by that link.
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir
            .path()
            .join("codesense-session-server-aarch64-apple-darwin");
        std::fs::write(&bundled, b"server").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let helper = root.join(HELPER_NAME);
        std::fs::hard_link(&bundled, &helper).unwrap();
        // As `parse_ps_line` reads it: the executable is the path in argv.
        let process = ServerProcess {
            pid: 9,
            uid: 501,
            started: "Thu Oct  8 11:35:00 2026".into(),
            exe: bundled.to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
        };
        // Read through its own helper link, it is named by the path it started from.
        assert_eq!(server_exe(&process, helper.clone()), Some(bundled.clone()));
        assert_eq!(server_exe(&process, bundled.clone()), Some(bundled.clone()));
        assert_eq!(
            classify(&process, None, None, Some(&bundled), None),
            Some(StrayKind::Bundled)
        );
        // Another root's helper link to the same binary: the kernel may name it
        // by that link too. Still the server, named by its started-from path.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::hard_link(&bundled, elsewhere.join(HELPER_NAME)).unwrap();
        assert_eq!(
            server_exe(&process, elsewhere.join(HELPER_NAME)),
            Some(bundled.clone())
        );
        // Another file's helper outside the server's root, or a wrapper: not a server.
        let stranger = dir.path().join("stranger");
        std::fs::create_dir(&stranger).unwrap();
        std::fs::write(stranger.join(HELPER_NAME), b"something else").unwrap();
        assert_eq!(server_exe(&process, stranger.join(HELPER_NAME)), None);
        assert_eq!(
            server_exe(&process, PathBuf::from("/usr/bin/caffeinate")),
            None
        );
        // The helper in its root, but no longer the file argv names (the bundled
        // file was replaced by an update): still the server, named by its helper,
        // and the bundled build only if that file is the bundled one.
        let replaced = ServerProcess {
            exe: dir.path().join("gone").join("codesense-session-server-aarch64-apple-darwin").to_string_lossy().into_owned(),
            ..process.clone()
        };
        assert_eq!(server_exe(&replaced, helper.clone()), Some(helper.clone()));
        let read_as_helper = ServerProcess {
            exe: helper.to_string_lossy().into_owned(),
            ..process.clone()
        };
        assert_eq!(
            classify(&read_as_helper, None, None, Some(&bundled), None),
            Some(StrayKind::Bundled)
        );
        let other_root = dir.path().join("other");
        std::fs::create_dir(&other_root).unwrap();
        std::fs::write(other_root.join(HELPER_NAME), b"server").unwrap();
        let other_build = ServerProcess {
            exe: other_root.join(HELPER_NAME).to_string_lossy().into_owned(),
            root: other_root.to_string_lossy().into_owned(),
            ..process
        };
        assert_eq!(
            classify(&other_build, None, None, Some(&bundled), None),
            Some(StrayKind::OtherBuild)
        );
        // The helper's own argv never reads as a server.
        assert!(parse_ps_line(&format!(
            "9 501 Thu Oct  8 11:35:00 2026 {} --activity-run claude -- claude",
            helper.display()
        ))
        .is_none());
    }

    #[test]
    fn agr19_the_census_keeps_a_server_the_kernel_names_by_its_helper_link() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir
            .path()
            .join("codesense-session-server-aarch64-apple-darwin");
        std::fs::write(&bundled, b"server").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let helper = root.join(HELPER_NAME);
        std::fs::hard_link(&bundled, &helper).unwrap();
        let listed = format!(
            "  41 501 Thu Oct  8 11:35:00 2026 {} --root {}\n  42 501 Thu Oct  8 11:35:00 2026 /usr/bin/caffeinate -i {} --root {}\n",
            bundled.display(),
            root.display(),
            bundled.display(),
            root.display(),
        );
        let exe_of = |pid: u32| match pid {
            41 => Some(helper.clone()),
            42 => Some(PathBuf::from("/usr/bin/caffeinate")),
            _ => None,
        };
        let servers = servers_listed(&listed, 501, exe_of);
        assert_eq!(servers.len(), 1, "{servers:?}");
        assert_eq!(servers[0].pid, 41);
        assert_eq!(servers[0].exe, bundled.to_string_lossy());
        // Another account's server is not listed.
        assert!(servers_listed(&listed, 502, exe_of).is_empty());
        // A helper process whose wrapped command names a server is not one.
        let helper_run = format!(
            "  43 501 Thu Oct  8 11:35:00 2026 {} --activity-run claude -- claude run {} --root {}\n",
            helper.display(),
            bundled.display(),
            root.display(),
        );
        assert!(servers_listed(&helper_run, 501, |_| Some(helper.clone())).is_empty());
        // Under a root whose path holds a space too.
        let spaced = dir.path().join("Application Support");
        std::fs::create_dir(&spaced).unwrap();
        let spaced_helper = spaced.join(HELPER_NAME);
        std::fs::hard_link(&bundled, &spaced_helper).unwrap();
        let spaced_run = format!(
            "  44 501 Thu Oct  8 11:35:00 2026 {} --activity-run claude -- claude run {} --root {}\n",
            spaced_helper.display(),
            bundled.display(),
            spaced.display(),
        );
        assert!(servers_listed(&spaced_run, 501, |_| Some(spaced_helper.clone())).is_empty());
        // Started by its bare name too.
        let bare_run = format!(
            "  46 501 Thu Oct  8 11:35:00 2026 {HELPER_NAME} --activity-run claude -- claude run {} --root {}\n",
            bundled.display(),
            root.display(),
        );
        assert!(servers_listed(&bare_run, 501, |_| Some(helper.clone())).is_empty());
        // A server under that root is still listed.
        let spaced_server = format!(
            "  45 501 Thu Oct  8 11:35:00 2026 {} --root {}\n",
            bundled.display(),
            spaced.display(),
        );
        assert_eq!(
            servers_listed(&spaced_server, 501, |_| Some(spaced_helper.clone())).len(),
            1
        );
    }

    #[test]
    fn agr19_a_start_time_reads_as_a_moment() {
        let ms = lstart_ms("Thu Oct  8 11:35:00 2026").unwrap();
        let back = chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .with_timezone(&chrono::Local);
        assert_eq!(
            back.format("%Y-%m-%d %H:%M").to_string(),
            "2026-10-08 11:35"
        );
        assert_eq!(lstart_ms("not a time"), None);
    }

    #[test]
    fn agr19_other_processes_and_server_sub_modes_are_not_servers() {
        for line in [
            "1 501 Thu Oct  8 11:35:00 2026 /usr/bin/vim notes --root /tmp/x",
            "2 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server attach --root /tmp/x",
            "3 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server --mcp-stdio-supervisor --root /tmp/x",
            "4 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server private-serve --root /tmp/x",
            "5 501 Thu Oct  8 11:35:00 2026 /A/codesense-session-server",
            "6 501 short",
            "",
        ] {
            assert_eq!(parse_ps_line(line), None, "{line}");
        }
    }

    #[test]
    fn agr19_a_stray_is_classified_against_this_app() {
        let process = |pid: u32, exe: &str, root: &str| ServerProcess {
            pid,
            uid: 501,
            started: "t".into(),
            exe: exe.into(),
            root: root.into(),
        };
        let bundled = Path::new("/App/codesense-session-server");
        let own = Path::new("/home/.codesense");
        let classify = |p: &ServerProcess| classify(p, Some(1), Some(9), Some(bundled), Some(own));
        // This instance's lock holder while the app is not attached to it.
        assert_eq!(
            classify(&process(
                9,
                "/App/codesense-session-server",
                "/home/.codesense"
            )),
            None
        );
        assert_eq!(
            classify(&process(
                1,
                "/App/codesense-session-server",
                "/home/.codesense"
            )),
            None
        );
        assert_eq!(
            classify(&process(2, "/App/codesense-session-server", "/tmp/fixture")),
            Some(StrayKind::Bundled)
        );
        assert_eq!(
            classify(&process(
                3,
                "/dev/target/debug/codesense-session-server",
                "/tmp/fixture"
            )),
            Some(StrayKind::OtherBuild)
        );
        assert_eq!(
            classify(&process(
                4,
                "/dev/codesense-session-server",
                "/home/.codesense"
            )),
            Some(StrayKind::OwnRoot)
        );
    }

    #[test]
    fn agr19_a_stop_never_signals_the_attached_server_or_a_changed_process() {
        assert_eq!(
            stop(
                &StrayTarget {
                    pid: 42,
                    started: "x".into(),
                    root: "/r".into()
                },
                Some(42),
                None
            ),
            StrayStop::Attached
        );
        // A pid that does not name a server with that start time: nothing is
        // signalled (this test's own pid is not a terminal server).
        assert_eq!(
            stop(
                &StrayTarget {
                    pid: std::process::id(),
                    started: "Thu Jan  1 00:00:00 1970".into(),
                    root: "/r".into()
                },
                None,
                None
            ),
            StrayStop::Gone
        );
    }
}
