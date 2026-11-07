// SPDX-License-Identifier: MIT
//! Stopping an account's installed terminal servers over SSH: the legacy
//! retirement (every server of a root) and the update fallback (one
//! server, by the pid it greeted with).
//!
//! The script reports one line per server, `server <pid> <sessions>
//! <running|stopped|alive>`, and for a named pid that is not stopped, why:
//! `gone <pid>` (no such process), `foreign <pid>` (not one of this root's
//! installed builds, or another account's), `denied <pid>` (the signal was
//! refused). A host without `/proc` exits 3: nothing can be checked there.

use crate::install::{Step, BINARY};

/// The script for the servers installed under `$HOME/<root>/host`. With
/// `pid`, only that process, after checking that it is this account's and
/// that its running image is one of the root's installed builds; a server
/// whose binary was replaced on disk still matches (`(deleted)` is ignored).
/// With `stop` false, a server that holds terminals is only reported.
pub fn stop_script(stop: bool, root: &str, pid: Option<u32>) -> Step {
    Step::new(format!(
        r#"
set -u
stop={stop}
only={only}
[ -d /proc/self ] || exit 3
# A zombie (reaped later by its parent) has stopped already.
alive() {{ [ -r "/proc/$1/stat" ] && [ "$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -c1)" != Z ]; }}
# A named server under a root that is not there is not this root's.
root=$(readlink -f "$HOME/{root}" 2>/dev/null) || {{
  if [ -n "$only" ]; then
    if alive "$only"; then echo "foreign $only"; else echo "gone $only"; fi
  fi
  exit 0
}}
ours() {{
  # The kernel's link is already absolute and resolved; plain readlink also
  # reads it when the binary was deleted (BusyBox's -f would not).
  real=$(readlink "/proc/$1/exe" 2>/dev/null) || return 1
  real=${{real% (deleted)}}
  case "$real" in "$root"/host/*/{binary}) return 0;; esac
  return 1
}}
# The terminals a server holds: its children on a tty (read from /proc, so
# no `ps` is needed).
sessions() {{
  # cat skips a process that exits mid-scan; awk then reads one stream.
  cat /proc/[0-9]*/stat 2>/dev/null | awk -v p="$1" '{{ sub(/^.*\) /, ""); split($0, f, " "); if (f[2] == p && f[5] != 0) n++ }} END {{ print n + 0 }}'
}}
stop_one() {{
  n=$(sessions "$1")
  if [ "$stop" != 1 ] && [ "$n" != 0 ]; then
    echo "server $1 $n running"
    return
  fi
  if ! kill -TERM "$1" 2>/dev/null; then
    alive "$1" && {{ echo "denied $1"; return; }}
  fi
  i=0
  while alive "$1" && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i+1)); done
  if alive "$1"; then
    kill -KILL "$1" 2>/dev/null
    i=0
    while alive "$1" && [ "$i" -lt 20 ]; do sleep 0.1; i=$((i+1)); done
  fi
  # `alive`: the stop signal was sent, but it still runs.
  if alive "$1"; then echo "server $1 $n alive"; else echo "server $1 $n stopped"; fi
}}
if [ -n "$only" ]; then
  alive "$only" || {{ echo "gone $only"; exit 0; }}
  owner=$(stat -c %u "/proc/$only" 2>/dev/null) || {{ echo "gone $only"; exit 0; }}
  [ "$owner" = "$(id -u)" ] && ours "$only" || {{ echo "foreign $only"; exit 0; }}
  stop_one "$only"
  exit 0
fi
[ -d "$root/host" ] || exit 0
uid=$(id -u)
for dir in /proc/[0-9]*; do
  pid=${{dir#/proc/}}
  [ "$pid" != "$$" ] || continue
  [ "$(stat -c %u "$dir" 2>/dev/null)" = "$uid" ] || continue
  ours "$pid" || continue
  stop_one "$pid"
done
"#,
        stop = u8::from(stop),
        only = pid.map(|pid| pid.to_string()).unwrap_or_default(),
        root = root,
        binary = BINARY,
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    fn run(home: &std::path::Path, step: &Step) -> String {
        let out = Command::new("sh")
            .arg("-c")
            .arg(&step.script)
            .env("HOME", home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn server(home: &std::path::Path, root: &str) -> Child {
        let dir = home.join(root).join("host/v1");
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(BINARY);
        if !binary.exists() {
            std::fs::copy("/bin/sleep", &binary).unwrap();
        }
        Command::new(&binary).arg("60").spawn().unwrap()
    }

    fn exited(child: &mut Child) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The fallback stops exactly the named server, and only when
    /// it is one of this root's builds; anything else is reported, untouched.
    #[test]
    fn agr19_a_named_server_is_stopped_only_when_it_is_this_roots() {
        let home = tempfile::tempdir().unwrap();
        let private = ".codesense/remote-private/v1";
        let mut target = server(home.path(), private);
        let mut bystander = server(home.path(), private);
        let mut legacy = server(home.path(), ".codesense");
        let mut stranger = Command::new("/bin/sleep").arg("60").spawn().unwrap();

        let report = run(home.path(), &stop_script(true, private, Some(target.id())));
        assert_eq!(report.trim(), format!("server {} 0 stopped", target.id()));
        assert!(exited(&mut target));
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "only the named pid"
        );
        assert!(legacy.try_wait().unwrap().is_none(), "never another root's");

        // Another root's server, or any other process: refused by name.
        let report = run(home.path(), &stop_script(true, private, Some(legacy.id())));
        assert_eq!(report.trim(), format!("foreign {}", legacy.id()));
        let report = run(
            home.path(),
            &stop_script(true, private, Some(stranger.id())),
        );
        assert_eq!(report.trim(), format!("foreign {}", stranger.id()));
        assert!(legacy.try_wait().unwrap().is_none());
        assert!(stranger.try_wait().unwrap().is_none());

        // A pid that already exited.
        let report = run(home.path(), &stop_script(true, private, Some(target.id())));
        assert_eq!(report.trim(), format!("gone {}", target.id()));

        // The legacy retirement: every server of its root, no other.
        let report = run(home.path(), &stop_script(true, ".codesense", None));
        assert_eq!(report.trim(), format!("server {} 0 stopped", legacy.id()));
        assert!(exited(&mut legacy));
        assert!(bystander.try_wait().unwrap().is_none());

        for child in [&mut bystander, &mut stranger] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }

    /// A server whose binary was replaced on disk is still that root's.
    #[test]
    fn agr19_a_replaced_binary_still_matches() {
        let home = tempfile::tempdir().unwrap();
        let mut target = server(home.path(), ".codesense");
        let binary = home.path().join(".codesense/host/v1").join(BINARY);
        std::fs::remove_file(&binary).unwrap();
        let report = run(
            home.path(),
            &stop_script(true, ".codesense", Some(target.id())),
        );
        assert_eq!(report.trim(), format!("server {} 0 stopped", target.id()));
        assert!(exited(&mut target));
    }
}
