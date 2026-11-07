// SPDX-License-Identifier: MIT
//! Whether a machine's host outlives the SSH session that started it.
//!
//! The host detaches into its own session, so closing the SSH connection never
//! signals it. systemd-logind can still end it: with `KillUserProcesses=yes`
//! it kills every process of a login session's scope when the session ends,
//! detached or not. That loss must never be silent, so a client probes for it
//! and shows it, with the commands that keep the host alive: lingering (the
//! user's service manager outlives their sessions) and starting the relay in a
//! scope of that manager, outside the login session. A host already running
//! inside a login session's scope stays at risk until it is restarted that
//! way. A machine that runs systemd but whose logind cannot be asked, or does
//! not say, is `Unknown`, never safe.

use crate::install::{shell_word, Ssh, Step};

/// Whether the host survives its user logging out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Survival {
    /// Nothing ends it: no systemd, or logind leaves this user's processes be.
    Survives,
    /// logind kills the user's processes when their last session ends.
    EndsAtLogout {
        user: String,
        /// Whether the user's service manager lingers after logout.
        lingering: bool,
        /// Whether systemd runs (elogind machines have no `systemd-run`).
        systemd: bool,
    },
    /// logind's policy could not be read — logind would not say, or the probe
    /// itself failed: the host may end at logout. `user` is empty, and
    /// `systemd` false, when the probe failed.
    Unknown { user: String, systemd: bool },
}

impl Survival {
    /// The commands that together keep the host alive across logout, to run
    /// on the machine in order — every one of them: with
    /// `KillUserProcesses=yes` lingering alone saves nothing, it only keeps
    /// the service manager the relay's scope runs under. None when the host
    /// survives already, and none without systemd (elogind): only an
    /// administrator can exempt the user there (`KillExcludeUsers=`).
    pub fn remedies(&self, attach_command: &str) -> Vec<String> {
        let (user, lingering, systemd) = match self {
            Self::Survives => return Vec::new(),
            Self::EndsAtLogout {
                user,
                lingering,
                systemd,
            } => (user, *lingering, *systemd),
            Self::Unknown { user, systemd } => (user, false, *systemd),
        };
        let user = if user.is_empty() {
            "\"$(id -un)\"".to_string()
        } else {
            shell_word(user)
        };
        if !systemd {
            return Vec::new();
        }
        let mut remedies = Vec::new();
        if !lingering {
            remedies.push(format!("loginctl enable-linger {user}"));
        }
        remedies.push(format!(
            "systemd-run --user --scope --quiet {attach_command}"
        ));
        remedies
    }
}

/// The probe: the user, whether systemd runs (`/run/systemd/system`, as
/// `sd_booted` tells), and logind's policy — `loginctl show-session` with no
/// session shows the manager's properties, `show-user` the user's.
pub fn probe_step() -> Step {
    Step::new(
        "u=\"$(id -un 2>/dev/null || id -u)\"\n\
         printf 'user=%s\\n' \"$u\"\n\
         [ -d /run/systemd/system ] && echo systemd=yes\n\
         if loginctl show-session -p KillUserProcesses -p KillOnlyUsers -p KillExcludeUsers 2>/dev/null; then\n\
         \x20 echo logind=read\n\
         \x20 loginctl show-user \"$u\" -p Linger 2>/dev/null\n\
         fi\n\
         exit 0",
    )
}

/// Read the probe's output.
pub fn parse(output: &str) -> Survival {
    let mut user = String::new();
    let (mut systemd, mut read) = (false, false);
    let (mut kill, mut lingering) = (None, false);
    let (mut only, mut exclude) = (Vec::new(), Vec::new());
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let names = || value.split_whitespace().map(String::from).collect();
        match key {
            "user" => user = value.to_string(),
            "systemd" => systemd = value == "yes",
            "logind" => read = value == "read",
            "KillUserProcesses" => kill = Some(value == "yes"),
            "KillOnlyUsers" => only = names(),
            "KillExcludeUsers" => exclude = names(),
            "Linger" => lingering = value == "yes",
            _ => {}
        }
    }
    if !read {
        // No logind answered: without systemd nothing ends the host at logout.
        return if systemd {
            Survival::Unknown { user, systemd }
        } else {
            Survival::Survives
        };
    }
    let Some(kill) = kill else {
        // logind answered without its policy.
        return Survival::Unknown { user, systemd };
    };
    let killed = kill && (only.is_empty() || only.contains(&user)) && !exclude.contains(&user);
    if killed {
        Survival::EndsAtLogout {
            user,
            lingering,
            systemd,
        }
    } else {
        Survival::Survives
    }
}

/// Probe the machine `ssh` reaches.
pub fn probe(ssh: &Ssh) -> Result<Survival, String> {
    ssh.run(&probe_step()).map(|output| parse(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KILLING: &str =
        "user=dev\nsystemd=yes\nKillUserProcesses=yes\nKillOnlyUsers=\nKillExcludeUsers=root\nlogind=read\nLinger=no\n";

    #[test]
    fn without_systemd_the_host_survives() {
        assert_eq!(parse("user=dev\n"), Survival::Survives);
    }

    /// systemd runs but logind would not answer: never reported as safe.
    #[test]
    fn an_unreadable_logind_is_unknown_not_safe() {
        let survival = parse("user=dev\nsystemd=yes\n");
        assert_eq!(
            survival,
            Survival::Unknown {
                user: "dev".into(),
                systemd: true
            }
        );
        assert_eq!(survival.remedies("relay")[0], "loginctl enable-linger dev");
    }

    /// logind that answers without its policy is not taken as safe; elogind
    /// without systemd is surfaced with no user-level remedy.
    #[test]
    fn a_silent_policy_is_unknown_and_elogind_has_no_user_fix() {
        let silent = "user=dev\nsystemd=yes\nlogind=read\n";
        assert_eq!(
            parse(silent),
            Survival::Unknown {
                user: "dev".into(),
                systemd: true
            }
        );
        let silent_elogind = parse("user=dev\nlogind=read\n");
        assert!(matches!(
            silent_elogind,
            Survival::Unknown { systemd: false, .. }
        ));
        assert!(silent_elogind.remedies("relay").is_empty());
        let elogind = parse(&KILLING.replace("systemd=yes\n", ""));
        assert!(matches!(elogind, Survival::EndsAtLogout { .. }), "surfaced");
        assert!(elogind.remedies("relay").is_empty(), "no user-level fix");
    }

    #[test]
    fn logind_that_keeps_user_processes_is_survived() {
        let out = KILLING.replace("KillUserProcesses=yes", "KillUserProcesses=no");
        assert_eq!(parse(&out), Survival::Survives);
    }

    /// `KillUserProcesses=yes` ends the host at logout, lingering or not: the
    /// state is surfaced, with what fixes it.
    #[test]
    fn kill_user_processes_is_surfaced_with_its_remedies() {
        let survival = parse(KILLING);
        assert_eq!(
            survival,
            Survival::EndsAtLogout {
                user: "dev".into(),
                lingering: false,
                systemd: true,
            }
        );
        assert_eq!(
            survival.remedies("relay"),
            [
                "loginctl enable-linger dev",
                "systemd-run --user --scope --quiet relay"
            ]
        );

        let lingering = parse(&KILLING.replace("Linger=no", "Linger=yes"));
        assert_eq!(
            lingering.remedies("relay"),
            ["systemd-run --user --scope --quiet relay"]
        );
        assert!(Survival::Survives.remedies("relay").is_empty());
        let odd = Survival::EndsAtLogout {
            user: "a b;c".into(),
            lingering: false,
            systemd: true,
        };
        assert_eq!(odd.remedies("relay")[0], "loginctl enable-linger 'a b;c'");
        let unread = Survival::Unknown {
            user: String::new(),
            systemd: true,
        };
        assert_eq!(
            unread.remedies("relay")[0],
            "loginctl enable-linger \"$(id -un)\""
        );
    }

    #[test]
    fn excluded_or_unlisted_users_are_not_killed() {
        let excluded = KILLING.replace("KillExcludeUsers=root", "KillExcludeUsers=root dev");
        assert_eq!(parse(&excluded), Survival::Survives);
        let only_others = KILLING.replace("KillOnlyUsers=", "KillOnlyUsers=alice");
        assert_eq!(parse(&only_others), Survival::Survives);
        let only_dev = KILLING.replace("KillOnlyUsers=", "KillOnlyUsers=alice dev");
        assert!(matches!(parse(&only_dev), Survival::EndsAtLogout { .. }));
    }
}
