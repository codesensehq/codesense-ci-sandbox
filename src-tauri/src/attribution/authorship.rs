// SPDX-License-Identifier: MIT
//! The git-ai authorship note (`authorship/3.x`), as `git notes --ref=<sense or ai> show <commit>`
//! prints it:
//!
//! ```text
//! a.txt
//!   s_014f8695c01cc2::t_754527e57d0ef8 1-2,7
//! d.txt
//!   h_b0791c3fb8befd 3
//! ---
//! {"schema_version":"authorship/3.0.0","sessions":{"s_…":{"agent_id":{"tool":…}}},…}
//! ```
//!
//! The attestation maps each file's line ranges to an author key. `s_<hash>::t_<hash>` is
//! an agent session and the checkpoint trace, `h_<hash>` a human, and a bare 16-hex key is
//! the older prompt-hash form.

use serde::Deserialize;
use std::collections::HashMap;

/// Who an attestation key names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Author {
    /// `s_…::t_…`: the session key and checkpoint trace.
    Session(String, String),
    /// `h_…`: a human.
    Human,
    /// A 16-hex prompt hash from an older note.
    Prompt(String),
}

impl Author {
    pub(crate) fn parse(key: &str) -> Option<Self> {
        if let Some((session, trace)) = key.split_once("::") {
            return (session.starts_with("s_") && trace.starts_with("t_"))
                .then(|| Self::Session(session.to_owned(), trace.to_owned()));
        }
        if key.starts_with("h_") && key.len() > 2 {
            return Some(Self::Human);
        }
        (key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self::Prompt(key.to_owned()))
    }
}

/// One attestation line: the author of some lines of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub author: Author,
    /// 1-based inclusive `(start, end)` ranges.
    pub ranges: Vec<(u32, u32)>,
}

impl Entry {
    fn covers_line(&self, line: u32) -> bool {
        self.ranges.iter().any(|&(a, b)| (a..=b).contains(&line))
    }
}

/// An agent as the note's metadata names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct AgentId {
    pub tool: String,
    pub id: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Deserialize)]
struct SessionMeta {
    agent_id: AgentId,
}

#[derive(Debug, Deserialize)]
struct PromptMeta {
    agent_id: AgentId,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    schema_version: String,
    #[serde(default)]
    sessions: HashMap<String, SessionMeta>,
    #[serde(default)]
    prompts: HashMap<String, PromptMeta>,
}

/// A parsed note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Authorship {
    files: Vec<(String, Vec<Entry>)>,
    sessions: HashMap<String, AgentId>,
    prompts: HashMap<String, AgentId>,
}

/// Who wrote a line, as a note says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    Agent(AgentId, Option<String>),
    Human,
}

impl Authorship {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let (attestation, metadata) = match text.strip_prefix("---\n") {
            Some(metadata) => ("", metadata),
            None => text
                .split_once("\n---\n")
                .ok_or("note has no attestation separator")?,
        };
        let metadata: Metadata =
            serde_json::from_str(metadata).map_err(|e| format!("note metadata: {e}"))?;
        if !metadata.schema_version.starts_with("authorship/3.") {
            return Err(format!(
                "unsupported note schema {}",
                metadata.schema_version
            ));
        }
        let mut files: Vec<(String, Vec<Entry>)> = Vec::new();
        for line in attestation.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(entry) = line.strip_prefix("  ") {
                let (_, entries) = files.last_mut().ok_or("entry before any file")?;
                entries.push(parse_entry(entry)?);
            } else {
                files.push((unquote(line)?, Vec::new()));
            }
        }
        Ok(Self {
            files,
            sessions: metadata
                .sessions
                .into_iter()
                .map(|(key, meta)| (key, meta.agent_id))
                .collect(),
            prompts: metadata
                .prompts
                .into_iter()
                .map(|(key, meta)| (key, meta.agent_id))
                .collect(),
        })
    }

    pub(crate) fn normalize_sessions(
        &mut self,
        store: &crate::session_usage::UsageStore,
    ) -> Result<(), String> {
        let Self {
            sessions, prompts, ..
        } = self;
        for agent in sessions.values_mut().chain(prompts.values_mut()) {
            agent.id = crate::session_usage::session_read::resolve_session(
                store,
                Some(&agent.tool),
                &agent.id,
            )?
            .session_id;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn attested_files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|(path, _)| path.as_str())
    }

    /// Native session ids actually named by attested agent entries in this note.
    pub(crate) fn session_ids(&self) -> Vec<String> {
        let Self {
            files,
            sessions,
            prompts,
            ..
        } = self;
        files
            .iter()
            .flat_map(|(_, entries)| entries)
            .filter_map(|entry| match &entry.author {
                Author::Session(key, _) => sessions.get(key),
                Author::Prompt(key) => prompts.get(key),
                Author::Human => None,
            })
            .map(|agent| agent.id.clone())
            .filter(|id| !std::string::String::is_empty(id))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Who wrote `line` (1-based) of `file` in the commit this note is on. `None` when the
    /// attestation does not cover the line, or covers it with a key the metadata cannot
    /// resolve: an unresolved line is unknown, never human.
    pub(crate) fn origin_of_line(&self, file: &str, line: u32) -> Option<Origin> {
        let (_, entries) = self.files.iter().find(|(path, _)| path == file)?;
        let entry = entries.iter().find(|entry| entry.covers_line(line))?;
        match &entry.author {
            Author::Human => Some(Origin::Human),
            Author::Session(key, trace) => self
                .sessions
                .get(key)
                .cloned()
                .map(|agent| Origin::Agent(agent, Some(trace.clone()))),
            Author::Prompt(key) => self
                .prompts
                .get(key)
                .cloned()
                .map(|agent| Origin::Agent(agent, None)),
        }
    }
}

fn parse_entry(text: &str) -> Result<Entry, String> {
    let (key, ranges) = text
        .trim()
        .split_once(' ')
        .ok_or_else(|| format!("malformed attestation entry {text:?}"))?;
    let author = Author::parse(key).ok_or_else(|| format!("unknown attestation key {key:?}"))?;
    let ranges = ranges
        .trim()
        .split(',')
        .map(parse_range)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Entry { author, ranges })
}

fn parse_range(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("malformed line range {text:?}");
    let (start, end) = match text.trim().split_once('-') {
        Some((a, b)) => (a, b),
        None => (text.trim(), text.trim()),
    };
    let start: u32 = start.parse().map_err(|_| bad())?;
    let end: u32 = end.parse().map_err(|_| bad())?;
    (start >= 1 && end >= start)
        .then_some((start, end))
        .ok_or_else(bad)
}

/// A file line is the path, or a JSON string when the path needed quoting.
fn unquote(line: &str) -> Result<String, String> {
    if line.starts_with('"') {
        serde_json::from_str(line).map_err(|e| format!("malformed quoted path {line:?}: {e}"))
    } else {
        Ok(line.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "a.txt\n  s_1::t_1 1-3,7\n  h_9 4\nb c.txt\n  abcdef0123456789 2\n---\n{\"schema_version\":\"authorship/3.0.0\",\"prompts\":{\"abcdef0123456789\":{\"agent_id\":{\"tool\":\"codex\",\"id\":\"old\",\"model\":\"gpt\"}}},\"sessions\":{\"s_1\":{\"agent_id\":{\"tool\":\"claude\",\"id\":\"sess\",\"model\":\"unknown\"}}}}";

    #[test]
    fn every_key_shape_resolves_and_a_range_list_covers_each_line() {
        let note = Authorship::parse(NOTE).unwrap();
        let claude = |line| match note.origin_of_line("a.txt", line) {
            Some(Origin::Agent(agent, _)) => Some((agent.tool, agent.id)),
            _ => None,
        };
        for line in [1, 2, 3, 7] {
            assert_eq!(
                claude(line),
                Some(("claude".into(), "sess".into())),
                "{line}"
            );
        }
        assert_eq!(note.origin_of_line("a.txt", 4), Some(Origin::Human));
        assert_eq!(
            note.origin_of_line("b c.txt", 2),
            Some(Origin::Agent(
                AgentId {
                    tool: "codex".into(),
                    id: "old".into(),
                    model: "gpt".into()
                },
                None
            ))
        );
    }

    #[test]
    fn a_line_the_note_does_not_cover_is_not_resolved() {
        let note = Authorship::parse(NOTE).unwrap();
        for (file, line) in [("a.txt", 5), ("a.txt", 6), ("a.txt", 8), ("other.txt", 1)] {
            assert_eq!(note.origin_of_line(file, line), None, "{file}:{line}");
        }
    }

    #[test]
    fn a_session_the_metadata_lacks_is_unresolved_not_human() {
        let text = "a.txt\n  s_gone::t_1 1\n---\n{\"schema_version\":\"authorship/3.0.0\"}";
        assert_eq!(
            Authorship::parse(text).unwrap().origin_of_line("a.txt", 1),
            None
        );
    }

    #[test]
    fn malformed_notes_are_refused() {
        for text in [
            "",
            "a.txt\n  s_1::t_1 1\n",
            "a.txt\n  s_1::t_1 x\n---\n{\"schema_version\":\"authorship/3.0.0\"}",
            "a.txt\n  s_1::t_1 3-1\n---\n{\"schema_version\":\"authorship/3.0.0\"}",
            "a.txt\n  zzz 1\n---\n{\"schema_version\":\"authorship/3.0.0\"}",
            "  s_1::t_1 1\n---\n{\"schema_version\":\"authorship/3.0.0\"}",
            "a.txt\n---\n{\"schema_version\":\"authorship/4.0.0\"}",
            "a.txt\n---\nnot json",
        ] {
            assert!(Authorship::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn a_quoted_path_is_unquoted() {
        let text =
            "\"d\\u00e9j\\\"a.txt\"\n  h_1 1\n---\n{\"schema_version\":\"authorship/3.1.0\"}";
        let note = Authorship::parse(text).unwrap();
        assert_eq!(
            note.origin_of_line("d\u{e9}j\"a.txt", 1),
            Some(Origin::Human)
        );
    }
}
