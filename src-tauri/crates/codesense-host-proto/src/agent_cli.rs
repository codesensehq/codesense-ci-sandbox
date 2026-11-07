// SPDX-License-Identifier: MIT
//! The one identity of an agent CLI CodeSense launches and observes.
//!
//! Everything else about a CLI (its binary, version floor, hooks and so on) is
//! its adapter's, in `codesense_pty_core::agent_cli`.
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// An agent CLI. A closed set: a new variant fails to compile until
/// `codesense_pty_core::agent_cli::adapter` has an adapter for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "&'static str", try_from = "String")]
pub enum AgentCli {
    Claude,
    Codex,
    OpenCode,
    Hermes,
}

impl AgentCli {
    /// Every available CLI, Claude first; the public user-facing enumeration.
    pub const ALL: [AgentCli; 4] = [
        AgentCli::Claude,
        AgentCli::Codex,
        AgentCli::OpenCode,
        AgentCli::Hermes,
    ];

    /// registered but not yet listed; empty when every CLI is listed.
    pub(crate) const UNLISTED: &'static [AgentCli] = &[];

    /// Every registered identity, including adapters not yet listed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn every_cli() -> impl Iterator<Item = AgentCli> {
        Self::ALL.into_iter().chain(Self::UNLISTED.iter().copied())
    }

    /// Its consecutive place in ALL followed by UNLISTED.
    const fn position(self) -> usize {
        match self {
            Self::Claude => 0,
            Self::Codex => 1,
            Self::OpenCode => 2,
            Self::Hermes => 3,
        }
    }

    /// `names` as messages list them, "claude, codex, opencode or hermes".
    pub fn join_names<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
        let names: Vec<_> = names.into_iter().collect();
        match names.split_last() {
            Some((last, [])) => (*last).to_string(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        }
    }

    /// The stable id, also the serde spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Hermes => "hermes",
        }
    }
}

const _: () = {
    let mut index = 0;
    while index < AgentCli::ALL.len() {
        assert!(AgentCli::ALL[index].position() == index);
        index += 1;
    }
    let mut unlisted = 0;
    while unlisted < AgentCli::UNLISTED.len() {
        assert!(AgentCli::UNLISTED[unlisted].position() == index);
        index += 1;
        unlisted += 1;
    }
};

impl fmt::Display for AgentCli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A name that is no agent CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAgentCli(pub String);

impl fmt::Display for UnknownAgentCli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if AgentCli::UNLISTED
            .iter()
            .any(|cli| AgentCli::as_str(*cli) == self.0)
        {
            return write!(f, "{} is not available yet", self.0);
        }
        write!(
            f,
            "unknown runtime {:?}; an agent runs on {}",
            self.0,
            AgentCli::join_names(AgentCli::ALL.map(AgentCli::as_str))
        )
    }
}

impl std::error::Error for UnknownAgentCli {}

impl FromStr for AgentCli {
    type Err = UnknownAgentCli;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|cli| cli.as_str() == s)
            .ok_or_else(|| UnknownAgentCli(s.to_string()))
    }
}

impl From<AgentCli> for &'static str {
    fn from(cli: AgentCli) -> Self {
        cli.as_str()
    }
}

impl TryFrom<String> for AgentCli {
    type Error = UnknownAgentCli;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// What a native launch runs: the user's shell or an agent CLI. On the wire it is
/// `"shell"` or the CLI's id, and a missing value is the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "&'static str", try_from = "String")]
pub enum NativeLaunch {
    #[default]
    Shell,
    Cli(AgentCli),
}

impl NativeLaunch {
    const SHELL: &'static str = "shell";

    /// The CLI it runs, or `None` for the shell.
    pub fn cli(self) -> Option<AgentCli> {
        match self {
            Self::Shell => None,
            Self::Cli(cli) => Some(cli),
        }
    }

    /// Whether it runs the shell. By reference, for `skip_serializing_if`.
    pub fn is_shell(&self) -> bool {
        *self == Self::Shell
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        self.cli().map_or(Self::SHELL, AgentCli::as_str)
    }
}

impl fmt::Display for NativeLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<NativeLaunch> for &'static str {
    fn from(launch: NativeLaunch) -> Self {
        launch.as_str()
    }
}

/// A name that is no native launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownNativeLaunch(pub String);

impl fmt::Display for UnknownNativeLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if AgentCli::UNLISTED
            .iter()
            .any(|cli| AgentCli::as_str(*cli) == self.0)
        {
            return write!(f, "{} is not available yet", self.0);
        }
        write!(
            f,
            "unknown launch {:?}; a launch is {}",
            self.0,
            NativeLaunch::SHELL
        )?;
        for cli in AgentCli::ALL {
            write!(f, ", {cli}")?;
        }
        Ok(())
    }
}

impl std::error::Error for UnknownNativeLaunch {}

impl FromStr for NativeLaunch {
    type Err = UnknownNativeLaunch;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == Self::SHELL {
            Ok(Self::Shell)
        } else {
            s.parse()
                .map(Self::Cli)
                .map_err(|_| UnknownNativeLaunch(s.to_string()))
        }
    }
}

impl TryFrom<String> for NativeLaunch {
    type Error = UnknownNativeLaunch;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_and_unlisted_partition_every_registered_variant_exactly_once() {
        let every: Vec<_> = AgentCli::every_cli().collect();
        for cli in AgentCli::every_cli() {
            assert_eq!(
                every.iter().filter(|candidate| **candidate == cli).count(),
                1
            );
            assert_eq!(
                AgentCli::ALL.contains(&cli) as usize + AgentCli::UNLISTED.contains(&cli) as usize,
                1
            );
        }
        assert_eq!(every.len(), AgentCli::ALL.len() + AgentCli::UNLISTED.len());
    }

    /// Every registered CLI is listed, so each one parses wherever the wire or a person
    /// names it: as an agent CLI and as a native launch, by name and through serde.
    #[test]
    fn every_registered_cli_is_listed_and_parses_at_every_wire_input() {
        for cli in AgentCli::every_cli() {
            assert!(AgentCli::ALL.contains(&cli), "{cli} is listed");
            let name = cli.as_str();
            let json = format!("\"{name}\"");
            assert_eq!(name.parse::<AgentCli>(), Ok(cli));
            assert_eq!(name.parse::<NativeLaunch>(), Ok(NativeLaunch::Cli(cli)));
            assert_eq!(serde_json::from_str::<AgentCli>(&json).unwrap(), cli);
            assert_eq!(
                serde_json::from_str::<NativeLaunch>(&json).unwrap(),
                NativeLaunch::Cli(cli)
            );
        }
    }

    #[test]
    fn join_names_lists_with_commas_and_a_last_or() {
        assert_eq!(AgentCli::join_names([]), "");
        assert_eq!(AgentCli::join_names(["Codex"]), "Codex");
        assert_eq!(
            AgentCli::join_names(["Codex", "OpenCode"]),
            "Codex or OpenCode"
        );
        assert_eq!(
            AgentCli::join_names(AgentCli::ALL.map(AgentCli::as_str)),
            "claude, codex, opencode or hermes"
        );
    }

    #[test]
    fn the_id_is_the_serde_spelling_and_round_trips() {
        assert_eq!(
            serde_json::from_str::<AgentCli>(r#""opencode""#).unwrap(),
            AgentCli::OpenCode
        );
        assert_eq!(AgentCli::OpenCode.as_str(), "opencode");
        assert_eq!(AgentCli::ALL[2], AgentCli::OpenCode);
        for cli in AgentCli::ALL {
            let json = serde_json::to_string(&cli).unwrap();
            assert_eq!(json, format!("\"{}\"", cli.as_str()));
            assert_eq!(serde_json::from_str::<AgentCli>(&json).unwrap(), cli);
            assert_eq!(cli.as_str().parse::<AgentCli>().unwrap(), cli);
        }
        let error = serde_json::from_str::<AgentCli>("\"gemini\"").unwrap_err();
        assert!(error.to_string().contains("gemini"), "{error}");
        assert_eq!(
            "shell".parse::<AgentCli>(),
            Err(UnknownAgentCli("shell".into()))
        );
    }

    #[test]
    fn a_launch_keeps_its_shell_spelling() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Launch {
            #[serde(default, skip_serializing_if = "NativeLaunch::is_shell")]
            launch: NativeLaunch,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            marker: Option<NativeLaunch>,
        }
        let shell = Launch {
            launch: NativeLaunch::Shell,
            marker: Some(NativeLaunch::Shell),
        };
        let json = serde_json::to_string(&shell).unwrap();
        assert_eq!(json, r#"{"marker":"shell"}"#);
        assert_eq!(serde_json::from_str::<Launch>(&json).unwrap(), shell);
        let claude = Launch {
            launch: NativeLaunch::Cli(AgentCli::Claude),
            marker: None,
        };
        let json = serde_json::to_string(&claude).unwrap();
        assert_eq!(json, r#"{"launch":"claude"}"#);
        assert_eq!(serde_json::from_str::<Launch>(&json).unwrap(), claude);
        let explicit: Launch = serde_json::from_str(r#"{"launch":"shell"}"#).unwrap();
        assert_eq!(explicit.launch, NativeLaunch::Shell);
        assert_eq!(
            serde_json::to_string(&NativeLaunch::Cli(AgentCli::Codex)).unwrap(),
            r#""codex""#
        );
        assert_eq!(
            serde_json::to_string(&NativeLaunch::Cli(AgentCli::OpenCode)).unwrap(),
            r#""opencode""#
        );
        let error = serde_json::from_str::<Launch>(r#"{"launch":"gemini"}"#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("a launch is shell, claude, codex"),
            "{error}"
        );
    }
}
