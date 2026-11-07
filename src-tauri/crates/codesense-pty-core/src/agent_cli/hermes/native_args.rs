// SPDX-License-Identifier: MIT
//! The pinned native option meanings consumed by managed Hermes launches.

#[derive(Debug, Default, PartialEq, Eq)]
pub struct NativeArguments<'a> {
    pub yolo: bool,
    pub tui: bool,
    pub toolsets: Option<Toolsets<'a>>,
    pub model: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub bypasses: Vec<&'a str>,
    model_slots: Vec<ModelSlot<'a>>,
}

#[derive(Debug, PartialEq, Eq)]
struct ModelSlot<'a> {
    index: usize,
    separate_value: Option<usize>,
    prefix: Option<&'a str>,
}

impl NativeArguments<'_> {
    /// Rewrite only slots identified by this resolver, retaining grouped boolean prefixes.
    pub fn model_edit(&self, arguments: &[String]) -> Option<(Vec<String>, usize)> {
        if self.model_slots.is_empty() {
            return None;
        }
        let mut args = Vec::new();
        let mut value_index = None;
        for (index, argument) in arguments.iter().enumerate() {
            if self
                .model_slots
                .iter()
                .any(|slot| slot.separate_value == Some(index))
            {
                continue;
            }
            if let Some(slot) = self.model_slots.iter().find(|slot| slot.index == index) {
                if let Some(prefix) = slot.prefix {
                    args.push(prefix.to_owned());
                }
                if value_index.is_none() {
                    args.push("--model".into());
                    value_index = Some(args.len());
                    args.push(self.model.unwrap_or_default().into());
                }
            } else {
                args.push(argument.clone());
            }
        }
        value_index.map(|index| (args, index))
    }

    pub fn check_managed(&self) -> Result<(), &'static str> {
        if self.bypasses.is_empty() {
            Ok(())
        } else {
            Err("CodeSense-managed Hermes does not support safe-mode, ignore-user-config, ignore-rules or profile arguments")
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Toolsets<'a> {
    pub names: Vec<&'a str>,
    pub composite: bool,
}
impl<'a> Toolsets<'a> {
    fn new(value: &'a str) -> Self {
        let mut names = Vec::new();
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let composite = names.iter().any(|name| matches!(*name, "all" | "*"));
        Self { names, composite }
    }
}

#[derive(Clone, Copy)]
enum Meaning {
    Yolo,
    Tui,
    Toolsets,
    Model,
    Provider,
    BaseUrl,
    ApiKey,
    Bypass,
    IgnoredSwitch,
}
enum Parser {
    Argparse {
        minimum: &'static str,
        short: Option<&'static str>,
        value: bool,
    },
    Profile,
}
struct Flag {
    name: &'static str,
    parser: Parser,
    meaning: Meaning,
}
const FLAGS: &[Flag] = &[
    Flag::argparse("--yolo", "--y", None, false, Meaning::Yolo),
    Flag::argparse("--tui", "--tu", None, false, Meaning::Tui),
    Flag::argparse("--toolsets", "--to", Some("-t"), true, Meaning::Toolsets),
    Flag::argparse("--model", "--mo", Some("-m"), true, Meaning::Model),
    Flag::argparse("--provider", "--pr", None, true, Meaning::Provider),
    Flag::argparse("--base-url", "--b", None, true, Meaning::BaseUrl),
    Flag::argparse("--api-key", "--ap", None, true, Meaning::ApiKey),
    Flag::argparse("--safe-mode", "--sa", None, false, Meaning::Bypass),
    Flag::argparse(
        "--ignore-user-config",
        "--ignore-u",
        None,
        false,
        Meaning::Bypass,
    ),
    Flag::argparse("--ignore-rules", "--ignore-r", None, false, Meaning::Bypass),
    Flag::argparse(
        "--verbose",
        "--v",
        Some("-v"),
        false,
        Meaning::IgnoredSwitch,
    ),
    Flag::argparse(
        "--quiet",
        "--qui",
        Some("-Q"),
        false,
        Meaning::IgnoredSwitch,
    ),
    Flag::argparse(
        "--worktree",
        "--w",
        Some("-w"),
        false,
        Meaning::IgnoredSwitch,
    ),
    Flag {
        name: "--profile",
        parser: Parser::Profile,
        meaning: Meaning::Bypass,
    },
];

impl Flag {
    const fn argparse(
        name: &'static str,
        minimum: &'static str,
        short: Option<&'static str>,
        value: bool,
        meaning: Meaning,
    ) -> Self {
        Self {
            name,
            parser: Parser::Argparse {
                minimum,
                short,
                value,
            },
            meaning,
        }
    }

    /// Recognized option, its optional inline value, and whether it consumes a value.
    fn read<'a>(&self, argument: &'a str) -> Option<(Option<&'a str>, bool)> {
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        match &self.parser {
            Parser::Profile => {
                if name == self.name || name == "-p" {
                    return Some((inline, true));
                }
                argument
                    .strip_prefix("-p")
                    .filter(|value| !value.is_empty())
                    .map(|value| (Some(value), true))
            }
            Parser::Argparse { minimum, value, .. } => (name.len() >= minimum.len()
                && self.name.starts_with(name))
            .then_some((inline, *value)),
        }
    }
}

// Native short booleans may precede a short value flag in one argv element.
// Stop as soon as a value flag consumes the rest; never inspect that value as options.
fn read_argument(argument: &str) -> Option<(Meaning, (Option<&str>, bool), Option<&str>)> {
    if let Some(found) = FLAGS
        .iter()
        .find_map(|flag| flag.read(argument).map(|value| (flag.meaning, value)))
    {
        return Some((found.0, found.1, None));
    }
    let mut tail = argument.strip_prefix('-')?;
    while !tail.is_empty() {
        let letter = tail.get(..1)?;
        let flag = FLAGS.iter().find(|flag| {
            matches!(&flag.parser,
            Parser::Argparse { short: Some(short), .. } if short.strip_prefix('-') == Some(letter))
        })?;
        tail = tail.get(1..)?;
        match &flag.parser {
            Parser::Argparse { value: true, .. } => {
                let inline = (!tail.is_empty()).then(|| tail.strip_prefix('=').unwrap_or(tail));
                let prefix_end = argument.len() - tail.len() - 1;
                let prefix = (prefix_end > 1).then(|| &argument[..prefix_end]);
                return Some((flag.meaning, (inline, true), prefix));
            }
            _ => {}
        }
    }
    Some((Meaning::IgnoredSwitch, (None, false), None))
}

/// Scalar selections use the last native occurrence. Bypass refusal scans every
/// supplied element, including separator tails, before the shared launch fold splits argv.
pub fn native_arguments(arguments: &[String]) -> NativeArguments<'_> {
    let mut result = NativeArguments::default();
    let mut options = true;
    let mut consumed = None;
    for (index, argument) in arguments.iter().enumerate() {
        if argument == "--" {
            options = false;
        }
        let Some((meaning, (inline, takes_value), prefix)) = read_argument(argument) else {
            continue;
        };
        if matches!(meaning, Meaning::Bypass) {
            result.bypasses.push(argument);
            continue;
        }
        if !options || consumed == Some(index) {
            continue;
        }
        let value = if takes_value {
            inline.or_else(|| {
                consumed = Some(index + 1);
                arguments.get(index + 1).map(String::as_str)
            })
        } else {
            None
        };
        match meaning {
            Meaning::Yolo => result.yolo = true,
            Meaning::Tui => result.tui = true,
            Meaning::Toolsets => {
                result.toolsets = value
                    .map(Toolsets::new)
                    .filter(|selection| !selection.names.is_empty())
            }
            Meaning::Model => {
                result.model = value;
                result.model_slots.push(ModelSlot {
                    index,
                    separate_value: (inline.is_none() && arguments.get(index + 1).is_some())
                        .then_some(index + 1),
                    prefix,
                });
            }
            Meaning::Provider => result.provider = value,
            Meaning::BaseUrl => result.base_url = value,
            Meaning::ApiKey => result.api_key = value,
            Meaning::Bypass => unreachable!(),
            Meaning::IgnoredSwitch => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn hermes_native_arguments_resolve_aliases_joined_values_and_last_selection() {
        for yolo in ["--y", "--yo", "--yol", "--yolo"] {
            assert!(native_arguments(&args(&[yolo])).yolo, "{yolo}");
        }
        for option in [
            "--to",
            "--too",
            "--tool",
            "--tools",
            "--toolse",
            "--toolset",
            "--toolsets",
            "-t",
        ] {
            let separate = args(&[option, "web"]);
            assert_eq!(native_arguments(&separate).toolsets.unwrap().names, ["web"]);
            let inline = args(&[&format!("{option}=web")]);
            assert_eq!(native_arguments(&inline).toolsets.unwrap().names, ["web"]);
        }
        let arguments = args(&[
            "-tall",
            "--tools",
            " web,web,,file ",
            "-mfirst",
            "--model=second",
            "-m",
            "last",
            "--pro",
            "custom",
            "--b=http://localhost:9000/v1",
        ]);
        let resolved = native_arguments(&arguments);
        assert_eq!(
            resolved.toolsets.unwrap(),
            Toolsets {
                names: vec!["web", "file"],
                composite: false
            }
        );
        assert_eq!(resolved.model, Some("last"));
        assert_eq!(resolved.provider, Some("custom"));
        assert_eq!(resolved.base_url, Some("http://localhost:9000/v1"));
        assert!(resolved.bypasses.is_empty());
        for selection in ["all", "*", "web,all"] {
            let arguments = args(&["-t", selection]);
            assert!(native_arguments(&arguments).toolsets.unwrap().composite);
        }
    }

    #[test]
    fn hermes_native_model_edit_removes_every_resolved_alias_and_preserves_group_prefixes() {
        let arguments = args(&["--model=first", "--mo=second", "-vQmtiny", "--yo"]);
        let native = native_arguments(&arguments);
        assert_eq!(native.model, Some("tiny"));
        let (mut rewritten, slot) = native.model_edit(&arguments).unwrap();
        assert_eq!(rewritten, ["--model", "tiny", "-vQ", "--yo"]);
        rewritten[slot] = "chosen".into();
        let after = native_arguments(&rewritten);
        assert_eq!(after.model, Some("chosen"));
        assert_eq!(after.model_slots.len(), 1);
        assert!(after.yolo);
        let no_model = args(&["--yo"]);
        assert!(native_arguments(&no_model).model_edit(&no_model).is_none());
        let literal = args(&["--query=explain --mo=literal -mtiny"]);
        assert!(native_arguments(&literal).model_edit(&literal).is_none());
    }

    #[test]
    fn hermes_native_arguments_last_empty_toolsets_restore_configured_selection() {
        for reset in ["", " , , "] {
            let arguments = args(&["-t", "all", &format!("--toolsets={reset}")]);
            assert!(native_arguments(&arguments).toolsets.is_none());
        }
    }

    #[test]
    fn hermes_native_arguments_group_only_pinned_short_booleans_before_values() {
        for (argument, model, toolsets) in [
            ("-vtweb", None, Some("web")),
            ("-Qmtiny", Some("tiny"), None),
            ("-vQwtall", None, Some("all")),
            ("-vmtall", Some("tall"), None),
            ("-vQqtall", None, None),
        ] {
            let arguments = args(&[argument]);
            let resolved = native_arguments(&arguments);
            assert_eq!(resolved.model, model, "{argument}");
            assert_eq!(
                resolved
                    .toolsets
                    .as_ref()
                    .and_then(|selection| selection.names.first().copied()),
                toolsets,
                "{argument}"
            );
        }
    }

    #[test]
    fn hermes_native_arguments_keep_bypass_parser_kinds_and_literal_query_distinct() {
        for option in [
            "--sa",
            "--safe",
            "--safe-mode",
            "--ignore-u",
            "--ignore-user-config",
            "--ignore-r",
            "--ignore-rules",
            "--profile",
            "--profile=x",
            "-p",
            "-px",
            "-p=x",
        ] {
            let arguments = args(&["chat", "--", option, "x"]);
            let resolved = native_arguments(&arguments);
            assert_eq!(resolved.bypasses, [option], "{option}");
            assert!(resolved.check_managed().is_err());
        }
        let arguments = args(&[
            "--pro",
            "custom",
            "--accept-hooks",
            "--query=explain --yolo --safe-mode -t all --profile x",
        ]);
        let resolved = native_arguments(&arguments);
        assert!(!resolved.yolo && resolved.toolsets.is_none() && resolved.bypasses.is_empty());
        assert!(resolved.check_managed().is_ok());
        assert_eq!(resolved.provider, Some("custom"));
        let arguments = args(&["--api=not-projected"]);
        assert_eq!(native_arguments(&arguments).api_key, Some("not-projected"));
        let arguments = args(&["--", "--yo", "-tall"]);
        let resolved = native_arguments(&arguments);
        assert!(!resolved.yolo && resolved.toolsets.is_none());
    }
}
