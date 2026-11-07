// SPDX-License-Identifier: MIT
//! Environment isolation shared by fixture Git runners.
use std::process::Command;

/// Keep fixture Git away from the agent's tracing and git-ai daemon, and from the reflog
/// action an unsupported CLI's session sets, including environment overrides already
/// installed on this command.
pub fn strip_attribution_env(command: &mut Command) -> &mut Command {
    let keys: Vec<_> = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_owned()))
        .filter(|key| {
            let key = key.as_encoded_bytes();
            key.starts_with(b"GIT_TRACE2")
                || key.starts_with(b"GIT_AI")
                || key == b"GIT_REFLOG_ACTION"
        })
        .collect();
    for key in keys {
        command.env_remove(key);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_inherited_and_explicit_prefixes_preserving_other_environment() {
        const CHILD: &str = "CODESENSE_TEST_ATTRIBUTION_ENV_CHILD";
        const FAMILY: &[&str] = &[
            "GIT_TRACE2",
            "GIT_TRACE2_EVENT",
            "GIT_TRACE2_EVENT_NESTING",
            "GIT_TRACE2_PERF",
            "GIT_TRACE2_FUTURE",
            "GIT_AI",
            "GIT_AI_HOME",
            "GIT_AI_FUTURE",
            "GIT_AIFUTURE",
            "GIT_REFLOG_ACTION",
        ];
        if std::env::var_os(CHILD).is_some() {
            for explicit in [false, true] {
                let mut command = Command::new("env");
                if explicit {
                    for key in FAMILY {
                        command.env(key, "explicit fixture value");
                    }
                    command.env("CODESENSE_FIXTURE_PRESERVED", "explicit");
                }
                let output = strip_attribution_env(&mut command).output().unwrap();
                assert!(output.status.success());
                let output = String::from_utf8(output.stdout).unwrap();
                for key in FAMILY {
                    assert!(
                        !output
                            .lines()
                            .any(|line| line.starts_with(&format!("{key}="))),
                        "{key} survived: {output}"
                    );
                }
                let preserved = if explicit { "explicit" } else { "inherited" };
                assert!(output
                    .lines()
                    .any(|line| line == format!("CODESENSE_FIXTURE_PRESERVED={preserved}")));
            }
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(CHILD, "1")
            .env("CODESENSE_FIXTURE_PRESERVED", "inherited")
            .args(["--exact", "test_support::tests::strips_inherited_and_explicit_prefixes_preserving_other_environment", "--nocapture"]);
        for key in FAMILY {
            child.env(key, "owned fixture value");
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{output:?}"
        );
    }
}
