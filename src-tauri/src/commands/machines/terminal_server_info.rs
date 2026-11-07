// SPDX-License-Identifier: MIT
//! SW-AGR-19: what each machine's running terminal server is, next to the app.
//!
//! Every terminal server is the `codesense-session-server` crate, whose own
//! version (`0.1.0`) never moves, so a server is placed against the app by its
//! [`BuildStamp`] (the app release, commit and build time `build.rs` stamps;
//! hello minor 16) and, for one older than the stamp, by its installed digest
//! (remote) or by its executable (This Mac). The relation is computed from the
//! live client on every summary — list, connect, retry — so no reload can
//! drop it.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use codesense_host_proto::BuildStamp;
use codesense_session_server::install;
use serde::Serialize;

/// How a running terminal server stands against this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerRelation {
    /// This app's own build.
    Same,
    /// Built before this app: offer the update.
    Older,
    /// Built after this app (a newer desktop installed it): left alone, never
    /// downgraded.
    Newer,
    /// This Mac only: another CodeSense build (a dev app, another install)
    /// runs the server on this root. Shown, never counted as out of date, so
    /// two apps sharing a root never replace each other's server in turn.
    OtherBuild,
}

impl ServerRelation {
    pub fn up_to_date(self) -> bool {
        matches!(self, Self::Same | Self::Newer)
    }
}

/// A connected machine's terminal server, as Machines and `list_machines`
/// show it. Absent for a machine that is not connected (no live hello).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalServerInfo {
    /// The app release the server was built for: its stamp's, this app's when
    /// it is this app's build, else unknown (an older build).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// This app's release.
    pub app_version: String,
    /// `relation` is `same` or `newer`.
    pub up_to_date: bool,
    pub relation: ServerRelation,
    /// The raw version the server announced (`0.1.0`, `0.1.0+{digest}`).
    pub announced_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built_at_ms: Option<u64>,
    /// The installed build's digest (remote installs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_digest: Option<String>,
    /// This Mac: the running server's executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_path: Option<String>,
    /// This Mac: the server executable this app ships (and would launch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_server_path: Option<String>,
    /// This Mac: when the running server started (ms since the epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// This Mac: the running server's pid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// This app is a development build: show build date, commit and path.
    pub dev_build: bool,
}

/// This app's own build stamp. The app links the session-server crate, so
/// this is the stamp a sidecar built alongside it carries.
pub fn app_stamp() -> BuildStamp {
    install::build_stamp()
}

fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['+', '-']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// A stamped server against this app's stamp: the release first; at one
/// release, the same commit is the same build. Builds of different (or
/// unknown) commits are ordered by build time when both know it; two builds
/// of one release with different commits and no times are taken as older,
/// since this app's build is the one it would install. Too little to tell
/// them apart (no commit on either side, no times) reads as the same release.
pub fn stamp_relation(server: &BuildStamp, app: &BuildStamp) -> ServerRelation {
    match (semver(&server.version), semver(&app.version)) {
        (Some(theirs), Some(ours)) if theirs > ours => return ServerRelation::Newer,
        (Some(theirs), Some(ours)) if theirs < ours => return ServerRelation::Older,
        _ => {}
    }
    if let (Some(theirs), Some(ours)) = (&server.commit, &app.commit) {
        if theirs == ours {
            return ServerRelation::Same;
        }
    }
    let commits_differ = matches!((&server.commit, &app.commit), (Some(a), Some(b)) if a != b);
    match (server.built_at_ms, app.built_at_ms) {
        (Some(theirs), Some(ours))
            if server.commit.is_some() != app.commit.is_some() || commits_differ =>
        {
            match theirs.cmp(&ours) {
                Ordering::Less => ServerRelation::Older,
                Ordering::Greater => ServerRelation::Newer,
                Ordering::Equal if commits_differ => ServerRelation::Older,
                Ordering::Equal => ServerRelation::Same,
            }
        }
        _ if commits_differ => ServerRelation::Older,
        _ => ServerRelation::Same,
    }
}

/// A remote server: by its stamp; without one (older than minor 16) a build
/// this app ships is the same, a higher protocol minor is newer, and anything
/// else predates the stamp, so is older than this app.
pub fn remote_relation(
    shipped: &HashSet<String>,
    announced_version: &str,
    proto_minor: u16,
    stamp: Option<&BuildStamp>,
    app: &BuildStamp,
) -> ServerRelation {
    if install::bundle_version_of(announced_version).is_some_and(|build| shipped.contains(&build)) {
        return ServerRelation::Same;
    }
    if let Some(stamp) = stamp {
        return stamp_relation(stamp, app);
    }
    let announced_newer = matches!(
        (semver(announced_version), semver(install::package_version())),
        (Some(theirs), Some(ours)) if theirs > ours
    );
    if proto_minor > codesense_host_proto::PROTO_MINOR || announced_newer {
        ServerRelation::Newer
    } else {
        ServerRelation::Older
    }
}

/// What This Mac knows of its running server's executable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalExecutable {
    /// Whether the running executable is the one this app ships; `None` when
    /// either path could not be read.
    pub same_path: Option<bool>,
}

/// This Mac's server. The same executable path proves little after an
/// in-place app update (the old server keeps running from the replaced path),
/// so the stamp decides; a server older than the stamp is older than this app.
/// Another path is another build sharing the root — unless its stamp is newer.
pub fn local_relation(
    stamp: Option<&BuildStamp>,
    app: &BuildStamp,
    exe: &LocalExecutable,
) -> ServerRelation {
    match (exe.same_path, stamp) {
        (Some(false), Some(stamp)) => match stamp_relation(stamp, app) {
            ServerRelation::Newer => ServerRelation::Newer,
            _ => ServerRelation::OtherBuild,
        },
        (Some(false), None) => ServerRelation::OtherBuild,
        (_, Some(stamp)) => stamp_relation(stamp, app),
        // No stamp: it predates 0.0.23, so it is older than this app whatever
        // its path (installers keep old modification and birth times, so a
        // file's age proves nothing).
        (_, None) => ServerRelation::Older,
    }
}

/// The version shown for a server: its stamp's release, or this app's when it
/// is this app's build.
fn shown_version(
    stamp: Option<&BuildStamp>,
    relation: ServerRelation,
    app: &BuildStamp,
) -> Option<String> {
    stamp
        .map(|stamp| stamp.version.clone())
        .or_else(|| (relation == ServerRelation::Same).then(|| app.version.clone()))
}

/// The digest part of an announced `{version}+{digest}`.
fn digest_of(announced_version: &str) -> Option<String> {
    install::bundle_version_of(announced_version)
        .and_then(|build| build.rsplit_once('-').map(|(_, digest)| digest.to_string()))
}

pub fn remote_info(
    shipped: &HashSet<String>,
    announced_version: &str,
    proto_minor: u16,
    stamp: Option<&BuildStamp>,
) -> TerminalServerInfo {
    let app = app_stamp();
    let relation = remote_relation(shipped, announced_version, proto_minor, stamp, &app);
    TerminalServerInfo {
        server_version: shown_version(stamp, relation, &app),
        app_version: app.version.clone(),
        up_to_date: relation.up_to_date(),
        relation,
        announced_version: announced_version.to_string(),
        build_commit: stamp.and_then(|stamp| stamp.commit.clone()),
        built_at_ms: stamp.and_then(|stamp| stamp.built_at_ms),
        build_digest: digest_of(announced_version),
        server_path: None,
        app_server_path: None,
        started_at_ms: None,
        pid: None,
        dev_build: cfg!(debug_assertions),
    }
}

/// The running process `pid`'s executable and start time (ms), if readable.
fn process_facts(pid: u32) -> (Option<PathBuf>, Option<u64>) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pid == 0 {
        return (None, None);
    }
    let id = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[id]),
        true,
        ProcessRefreshKind::new().with_exe(UpdateKind::Always),
    );
    let Some(process) = system.process(id) else {
        return (None, None);
    };
    (
        process.exe().map(Path::to_path_buf),
        Some(process.start_time() * 1000).filter(|ms| *ms > 0),
    )
}

/// `path` resolved, so a symlinked or firmlinked spelling compares equal.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether two executables are the same file: one path once resolved, or one
/// app moved by Gatekeeper's App Translocation (the same bundle-relative
/// path under a read-only randomized mount).
fn same_executable(running: &Path, shipped: &Path) -> bool {
    let (running, shipped) = (canonical(running), canonical(shipped));
    // The same file under another name too: a server hard-links its binary to
    // `<root>/activity-helper`, and the kernel may name it by that link.
    if running == shipped || crate::terminal::session_server::strays::same_file(&running, &shipped)
    {
        return true;
    }
    let translocated = |path: &Path| {
        path.to_string_lossy()
            .contains("/AppTranslocation/")
            .then(|| bundle_relative(path))
            .flatten()
    };
    let same_place = match (translocated(&running), translocated(&shipped)) {
        (Some(a), _) => bundle_relative(&shipped).is_some_and(|b| a == b),
        (_, Some(b)) => bundle_relative(&running).is_some_and(|a| a == b),
        _ => false,
    };
    // Another copy of the app with the same bundle name is not this one: the
    // translocated file must be byte-for-byte the same size and age.
    same_place && same_file_facts(&running, &shipped)
}

/// The path this Mac's running server is shown by: the shipped path when the
/// kernel names it by an `activity-helper` hard link to that same file, else
/// the path the kernel gives.
fn shown_server_path(running: PathBuf, shipped: Option<&Path>) -> PathBuf {
    use crate::terminal::session_server::strays::{same_file, HELPER_NAME};
    match shipped {
        Some(shipped)
            if running.file_name().is_some_and(|name| name == HELPER_NAME)
                && same_file(&running, shipped) =>
        {
            shipped.to_path_buf()
        }
        _ => running,
    }
}

fn same_file_facts(a: &Path, b: &Path) -> bool {
    let facts = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.len(), metadata.modified().ok()))
    };
    matches!((facts(a), facts(b)), (Some(a), Some(b)) if a == b)
}

/// `Name.app/Contents/…`: a path from its app bundle on.
fn bundle_relative(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let at = text.find(".app/Contents/")?;
    let start = text[..at].rfind('/').map_or(0, |slash| slash + 1);
    Some(text[start..].to_string())
}

pub fn local_info(
    announced_version: &str,
    pid: u32,
    stamp: Option<&BuildStamp>,
    shipped_exe: Option<&Path>,
) -> TerminalServerInfo {
    let app = app_stamp();
    let (running_exe, started_at_ms) = process_facts(pid);
    let exe = LocalExecutable {
        same_path: running_exe
            .as_deref()
            .zip(shipped_exe)
            .map(|(running, shipped)| same_executable(running, shipped)),
    };
    let relation = local_relation(stamp, &app, &exe);
    TerminalServerInfo {
        server_version: shown_version(stamp, relation, &app),
        app_version: app.version.clone(),
        up_to_date: relation.up_to_date(),
        relation,
        announced_version: announced_version.to_string(),
        build_commit: stamp.and_then(|stamp| stamp.commit.clone()),
        built_at_ms: stamp.and_then(|stamp| stamp.built_at_ms),
        build_digest: digest_of(announced_version),
        server_path: running_exe
            .map(|path| shown_server_path(path, shipped_exe))
            .map(|path| path.to_string_lossy().into_owned()),
        app_server_path: shipped_exe.map(|path| path.to_string_lossy().into_owned()),
        started_at_ms,
        pid: Some(pid).filter(|pid| *pid != 0),
        dev_build: cfg!(debug_assertions),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(version: &str, commit: Option<&str>, built_at_ms: Option<u64>) -> BuildStamp {
        BuildStamp {
            version: version.into(),
            commit: commit.map(str::to_string),
            built_at_ms,
        }
    }

    fn app() -> BuildStamp {
        stamp("0.0.23", Some("aaaaaaaaaaaa"), Some(2_000))
    }

    #[test]
    fn agr19_a_stamp_orders_by_release_then_commit_then_build_time() {
        let app = app();
        assert_eq!(stamp_relation(&app, &app), ServerRelation::Same);
        assert_eq!(
            stamp_relation(&stamp("0.0.22", Some("aaaaaaaaaaaa"), Some(9_000)), &app),
            ServerRelation::Older
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.24", None, None), &app),
            ServerRelation::Newer
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.23", Some("bbbbbbbbbbbb"), Some(1_000)), &app),
            ServerRelation::Older,
            "same release, another commit built earlier"
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.23", Some("bbbbbbbbbbbb"), Some(3_000)), &app),
            ServerRelation::Newer
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.23", Some("aaaaaaaaaaaa"), Some(1)), &app),
            ServerRelation::Same,
            "one commit built twice (app and sidecar) is one build"
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.23", None, None), &app),
            ServerRelation::Same
        );
        // Another commit and no build times to order them: not this app's
        // build, so older (the update installs this app's).
        assert_eq!(
            stamp_relation(
                &stamp("0.0.23", Some("bbbbbbbbbbbb"), None),
                &stamp("0.0.23", Some("aaaaaaaaaaaa"), None)
            ),
            ServerRelation::Older
        );
        // A commit known on one side only: the build times order them.
        assert_eq!(
            stamp_relation(&stamp("0.0.23", None, Some(1_000)), &app),
            ServerRelation::Older
        );
        assert_eq!(
            stamp_relation(&stamp("0.0.23", None, Some(3_000)), &app),
            ServerRelation::Newer
        );
    }

    #[test]
    fn agr19_a_remote_server_is_placed_by_digest_then_stamp_then_protocol() {
        let app = app();
        let digest = "0123456789ab";
        let announced =
            install::announced_version(Some(&format!("{}-{digest}", install::package_version())));
        let shipped: HashSet<String> = [format!("{}-{digest}", install::package_version())].into();
        let none = HashSet::new();
        let minor = codesense_host_proto::PROTO_MINOR;
        // A build this app ships is this app's, stamped or not.
        assert_eq!(
            remote_relation(&shipped, &announced, minor - 1, None, &app),
            ServerRelation::Same
        );
        // Unshipped and unstamped: it predates the stamp, so it is older…
        assert_eq!(
            remote_relation(&none, &announced, minor - 1, None, &app),
            ServerRelation::Older
        );
        assert_eq!(
            remote_relation(&none, "0.1.0", minor, None, &app),
            ServerRelation::Older
        );
        // …unless it speaks a newer protocol.
        assert_eq!(
            remote_relation(&none, &announced, minor + 1, None, &app),
            ServerRelation::Newer
        );
        // A newer package version (a future crate bump) is newer too.
        assert_eq!(
            remote_relation(&none, "99.0.0+cccccccccccc", minor, None, &app),
            ServerRelation::Newer
        );
        // Stamped: the stamp decides.
        let older = stamp("0.0.22", None, None);
        assert_eq!(
            remote_relation(&none, &announced, minor, Some(&older), &app),
            ServerRelation::Older
        );
        assert_eq!(
            remote_relation(&none, &announced, minor, Some(&app), &app),
            ServerRelation::Same
        );
    }

    #[test]
    fn agr19_this_mac_needs_this_apps_executable_and_build() {
        let app = app();
        let here = LocalExecutable {
            same_path: Some(true),
        };
        let elsewhere = LocalExecutable {
            same_path: Some(false),
        };
        // The app's own sidecar.
        assert_eq!(
            local_relation(Some(&app), &app, &here),
            ServerRelation::Same
        );
        // In-place app update: the old server still runs from the replaced
        // path; its older stamp gives it away.
        let old = stamp("0.0.22", Some("cccccccccccc"), Some(1));
        assert_eq!(
            local_relation(Some(&old), &app, &here),
            ServerRelation::Older
        );
        // Older than the stamp, at this app's own path: older, always (an
        // installer keeps file times, so a file's age proves nothing).
        assert_eq!(local_relation(None, &app, &here), ServerRelation::Older);
        // Another path: another build sharing the root, never "out of date"…
        assert_eq!(
            local_relation(Some(&app), &app, &elsewhere),
            ServerRelation::OtherBuild
        );
        assert_eq!(
            local_relation(Some(&old), &app, &elsewhere),
            ServerRelation::OtherBuild
        );
        assert_eq!(
            local_relation(None, &app, &elsewhere),
            ServerRelation::OtherBuild
        );
        // …unless it is newer: never offered as a downgrade.
        let newer = stamp("0.0.24", None, None);
        assert_eq!(
            local_relation(Some(&newer), &app, &elsewhere),
            ServerRelation::Newer
        );
        // Paths unreadable: the stamp alone.
        let unknown = LocalExecutable::default();
        assert_eq!(
            local_relation(Some(&app), &app, &unknown),
            ServerRelation::Same
        );
        assert_eq!(local_relation(None, &app, &unknown), ServerRelation::Older);
        assert!(!ServerRelation::OtherBuild.up_to_date());
        assert!(ServerRelation::Newer.up_to_date());
    }

    #[test]
    fn agr19_shown_version_and_digest() {
        let app = app();
        assert_eq!(
            shown_version(None, ServerRelation::Same, &app).as_deref(),
            Some("0.0.23")
        );
        assert_eq!(shown_version(None, ServerRelation::Older, &app), None);
        let old = stamp("0.0.22", None, None);
        assert_eq!(
            shown_version(Some(&old), ServerRelation::Older, &app).as_deref(),
            Some("0.0.22")
        );
        let announced = install::announced_version(Some(&format!(
            "{}-0123456789ab",
            install::package_version()
        )));
        assert_eq!(digest_of(&announced).as_deref(), Some("0123456789ab"));
        assert_eq!(digest_of("0.1.0"), None);
    }

    #[test]
    fn agr19_translocated_and_linked_executables_are_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let place = |root: &str, bytes: &[u8]| {
            let path = dir
                .path()
                .join(root)
                .join("CodeSense.app/Contents/MacOS/codesense-session-server");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
            path
        };
        let shipped = place("Applications", b"server");
        let moved = place("var/T/AppTranslocation/ABCD/d", b"server");
        assert!(same_executable(&moved, &shipped));
        assert!(same_executable(&shipped, &moved));
        // Another copy of the app (another build) translocated from Downloads.
        let other = place("var/T/AppTranslocation/EFGH/d", b"another server");
        assert!(!same_executable(&other, &shipped));
        assert!(!same_executable(
            Path::new("/Users/me/codesense/target/debug/codesense-session-server"),
            &shipped
        ));
        let real = dir.path().join("server");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_executable(&link, &real));
        // The server read through its activity-helper hard link is the shipped one.
        let helper = dir
            .path()
            .join(crate::terminal::session_server::strays::HELPER_NAME);
        std::fs::hard_link(&shipped, &helper).unwrap();
        assert!(same_executable(&helper, &shipped));
        // A copy is not.
        let copy = dir.path().join("copy");
        std::fs::copy(&shipped, &copy).unwrap();
        assert!(!same_executable(&copy, &shipped));
    }

    /// This Mac's server, named by its helper link, is shown by the shipped path;
    /// any other path is shown as the kernel gives it.
    #[test]
    fn a_server_named_by_its_helper_link_is_shown_by_the_shipped_path() {
        use crate::terminal::session_server::strays::HELPER_NAME;
        let dir = tempfile::tempdir().unwrap();
        let shipped = dir.path().join("codesense-session-server");
        std::fs::write(&shipped, b"server").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let helper = root.join(HELPER_NAME);
        std::fs::hard_link(&shipped, &helper).unwrap();
        assert_eq!(shown_server_path(helper.clone(), Some(&shipped)), shipped);
        assert_eq!(shown_server_path(helper.clone(), None), helper);
        let other_root = dir.path().join("other");
        std::fs::create_dir(&other_root).unwrap();
        let other_helper = other_root.join(HELPER_NAME);
        std::fs::write(&other_helper, b"older server").unwrap();
        assert_eq!(
            shown_server_path(other_helper.clone(), Some(&shipped)),
            other_helper
        );
        let elsewhere = dir.path().join("codesense-session-server-old");
        std::fs::hard_link(&shipped, &elsewhere).unwrap();
        assert_eq!(
            shown_server_path(elsewhere.clone(), Some(&shipped)),
            elsewhere
        );
    }

    /// The real process facts for this test process: an executable and a
    /// start time; a dead pid reads as nothing.
    #[test]
    fn agr19_process_facts_read_the_executable_and_start() {
        let (exe, started) = process_facts(std::process::id());
        assert!(exe.is_some());
        assert!(started.is_some_and(|ms| ms > 1_600_000_000_000));
        assert_eq!(process_facts(0), (None, None));
    }

    /// A real-machine check of This Mac: `AGR19_REAL_PID=<pid of the running
    /// codesense-session-server> AGR19_SHIPPED=<the app's bundled server>
    /// cargo test … agr19_real_this_mac -- --ignored --nocapture` prints the
    /// info the Machines row would show.
    #[test]
    #[ignore = "reads a real running terminal server"]
    fn agr19_real_this_mac() {
        let pid: u32 = std::env::var("AGR19_REAL_PID").unwrap().parse().unwrap();
        let shipped = std::env::var("AGR19_SHIPPED").unwrap();
        let info = local_info("0.1.0", pid, None, Some(Path::new(&shipped)));
        println!("{}", serde_json::to_string_pretty(&info).unwrap());
    }
}
