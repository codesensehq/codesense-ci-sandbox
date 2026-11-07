// SPDX-License-Identifier: MIT
//! The app's CLI-helper mode must not change a terminal's inherited budget.
#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::process::Command;

#[test]
fn app_activity_helper_preserves_the_incoming_limit() {
    for soft in [256, 1024] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codesense"));
        command
            .args([
                "--activity-run",
                "codex",
                "--",
                "/bin/sh",
                "-c",
                "ulimit -Sn",
            ])
            .env_clear();
        // SAFETY: only rlimit syscalls run in the exec'd child; the test
        // harness retains its limit and the helper never starts the app UI.
        unsafe {
            command.pre_exec(move || {
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                limit.rlim_cur = soft;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            soft.to_string()
        );
    }
}
