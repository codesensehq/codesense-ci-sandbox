// SPDX-License-Identifier: MIT
//! SW-XS-14: a team's `projections.yml` becomes a one-time **suggestion** per
//! team agent ("Add these to ‹Agent›'s context?"), never applied silently.
//!
//! A team-wide projection (`projections.yml`, in the team chain's `config/`)
//! used to decide what every workspace got. The agent's own `context` decides
//! that now (SPEC §12.1), so each entry the projection named becomes a
//! suggested `context` entry (`source_subdir` → `path`, `dest` → `dest`) for
//! every team agent that does not already have it. The agent editor shows it
//! (SW-XS-16); accepting goes through `agent_update` like any edit, so nothing
//! here writes an agent file.
//!
//! "One-time": a suggestion the user dismisses stays dismissed for that
//! agent, recorded per user beside their org clones, keyed by a fingerprint of
//! what was suggested. A changed `projections.yml` (a new fingerprint) is a new
//! suggestion. Once every suggested entry is in the agent's `context` there is
//! nothing to suggest.
//!
//! SW-XS-8 removed the legacy projection engine, so this module is the only
//! reader of `projections.yml`; nothing delivers content from it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context_repo::path_to_user_home;
use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::Principal;

use super::context::{normalize_entries, ContextEntry};
use super::keys::AgentKey;
use super::manifest::{json_hash, ContentLayer};
use super::manifest_resolve::ManifestTarget;
use super::model::Agent;

/// Where a suggestion came from.
pub const SUGGESTION_SOURCE: &str = "projections.yml";

/// The per-user record of dismissed suggestions, under the user's home.
const DISMISSED_FILE: &str = "agent-context-suggestions.json";

/// One suggested `context` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuggestedEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
}

/// The suggestion for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextSuggestion {
    /// Always [`SUGGESTION_SOURCE`].
    pub source: &'static str,
    /// The entries to add, in `projections.yml` order (nearest team first).
    pub entries: Vec<SuggestedEntry>,
    /// Identifies exactly this suggestion; dismissing names it.
    pub fingerprint: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DismissedFile {
    /// Agent key → the fingerprint the user dismissed.
    #[serde(default)]
    dismissed: BTreeMap<String, String>,
}

/// The suggestion for `agent` (saved as `key`), for a launch by `principal`:
/// `None` for a starter or a personal agent, when the team chain has no
/// `projections.yml` entry the agent lacks, or when this exact suggestion was
/// dismissed. Reads only; never fails (an unreadable file suggests nothing).
pub fn suggestion_for(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    agent: &Agent,
) -> Option<ContextSuggestion> {
    suggestion_for_caller(home, principal, key, agent, &Default::default())
}

pub(crate) fn suggestion_for_caller(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    agent: &Agent,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Option<ContextSuggestion> {
    if key.is_builtin() || key.is_personal() || key.team.is_empty() || is_scratch_team(&key.team) {
        return None;
    }
    // What the agent gets now: its own list, or the default that accepting would write
    // out first. An entry is had when one of these delivers it at the same place
    // (`places_like`), the same test accepting applies, so an accepted suggestion
    // is never offered again.
    let have: Vec<ContextEntry> = match agent.context.as_deref() {
        // A stored context that does not validate suggests nothing (rather than all).
        Some(own) => normalize_entries(own).ok()?,
        None => super::context::default_context(agent.runtime, false),
    };
    let mut seen_keys = HashSet::new();
    let mut entries: Vec<SuggestedEntry> = Vec::new();
    // The fingerprint covers the raw team-chain specs, before any filtering, so
    // a dismissal holds while `projections.yml` is unchanged, whatever the
    // agent's own `context` does meanwhile.
    let mut raw: Vec<serde_json::Value> = Vec::new();
    let target = ManifestTarget::team(key.org.clone(), key.team.clone());
    for (layer, content_root) in
        super::manifest_resolve::content_layers_with(caller, home, principal, &target)
    {
        let ContentLayer::Team { team, .. } = &layer else {
            continue;
        };
        // First-wins per projection key, nearest team first (as projection
        // resolved them).
        for spec in content_root.projections_admitted(|path| caller.visible(path)) {
            raw.push(serde_json::json!({
                "team": team,
                "key": spec.key,
                "source_subdir": spec.source_subdir,
                "dest": spec.dest,
            }));
            if !seen_keys.insert(spec.key.clone()) {
                continue;
            }
            let Some(entry) = suggested(&spec.source_subdir, spec.dest.as_deref()) else {
                continue;
            };
            if have.iter().any(|h| h.places_like(&entry)) {
                continue;
            }
            let suggested = SuggestedEntry {
                path: entry.path,
                dest: entry.dest,
            };
            if !entries.contains(&suggested) {
                entries.push(suggested);
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    let fingerprint = json_hash(&serde_json::Value::Array(raw));
    if dismissed(home, principal)
        .ok()
        .and_then(|f| f.dismissed.get(&key.to_string()).cloned())
        == Some(fingerprint.clone())
    {
        return None;
    }
    Some(ContextSuggestion {
        source: SUGGESTION_SOURCE,
        entries,
        fingerprint,
    })
}

/// Record that the user dismissed `fingerprint` for `key`. Writes only the
/// user's own record, never the agent. Serialized in-process, and a record
/// that exists but cannot be read is refused rather than overwritten.
pub fn dismiss(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    fingerprint: &str,
) -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut file = dismissed(home, principal)?;
    file.dismissed
        .insert(key.to_string(), fingerprint.to_string());
    crate::shared::json_io::write_json_atomic(&dismissed_path(home, principal), &file)
        .map_err(|e| format!("cannot record the dismissed suggestion: {e}"))
}

/// A `projections.yml` entry as a normalized `context` entry: `dest` defaults
/// to the last component of `source_subdir` (the projection's own rule), and
/// is dropped when it equals the path. A whole-tree entry (empty
/// `source_subdir`) or one the context rules refuse suggests nothing.
fn suggested(source_subdir: &str, dest: Option<&str>) -> Option<ContextEntry> {
    let path = source_subdir.trim_matches('/');
    if path.is_empty() {
        return None;
    }
    let dest = dest
        .map(|d| d.trim_matches('/').to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string());
    let entry = if dest == path {
        ContextEntry::new(path)
    } else {
        ContextEntry::with_dest(path, dest)
    };
    normalize_entries(&[entry]).ok()?.into_iter().next()
}

fn dismissed_path(home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(home, principal).join(DISMISSED_FILE)
}

/// The user's dismissal record: empty when absent, an error when it exists but
/// cannot be read or parsed.
fn dismissed(home: &Path, principal: &Principal) -> Result<DismissedFile, String> {
    let path = dismissed_path(home, principal);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not a valid record: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DismissedFile::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::resolve::tests::{alice, draft, save, write_org, ORG, TEAM};
    use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
    use codesense_pty_core::agent_cli::AgentCli;

    fn projections(home: &Path, team: &str, yaml: &str) {
        let config = path_to_team_config_dir(&path_to_org_root(home, &alice(), ORG).unwrap(), team);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("projections.yml"), yaml).unwrap();
    }

    fn agent_file(home: &Path, key: &AgentKey) -> Vec<u8> {
        let config = path_to_team_config_dir(&path_to_org_root(home, &alice(), ORG).unwrap(), TEAM);
        std::fs::read(config.join("agents").join(&key.id).join("agent.yaml")).unwrap()
    }

    fn entry(path: &str, dest: Option<&str>) -> SuggestedEntry {
        SuggestedEntry {
            path: path.into(),
            dest: dest.map(Into::into),
        }
    }

    /// AC2: each team agent gets the suggestion; no agent file changes.
    #[test]
    fn projections_become_a_suggestion_and_no_agent_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        projections(
            home,
            TEAM,
            "projections:\n\
             \x20 - {key: docs, source_subdir: docs}\n\
             \x20 - {key: plans, source_subdir: _bmad-output/agent-workspaces, dest: plans}\n\
             \x20 - {key: guides, source_subdir: guides/team}\n\
             \x20 - {key: everything, source_subdir: ''}\n",
        );
        projections(
            home,
            "platform",
            "projections:\n\
             \x20 - {key: docs, source_subdir: parent-docs}\n\
             \x20 - {key: runbooks, source_subdir: runbooks}\n",
        );
        let parent =
            crate::agents::roots::team_content_dir(home, &alice(), ORG, "platform").unwrap();
        std::fs::create_dir_all(parent.join("runbooks")).unwrap();
        std::fs::write(parent.join("runbooks/on-call.md"), "shared runbook").unwrap();
        let barb = save(home, &alice(), draft("barb", AgentCli::Claude));
        let bubbles = save(home, &alice(), draft("bubbles", AgentCli::Codex));
        let before = (agent_file(home, &barb), agent_file(home, &bubbles));

        for key in [&barb, &bubbles] {
            let agent = crate::agents::service::get(home, &alice(), key)
                .unwrap()
                .agent;
            let got = suggestion_for(home, &alice(), key, &agent).expect("a suggestion");
            assert_eq!(got.source, "projections.yml");
            assert_eq!(
                got.entries,
                vec![
                    entry("docs", None),
                    entry("_bmad-output/agent-workspaces", Some("plans")),
                    entry("guides/team", Some("team")),
                    entry("runbooks", None),
                ]
            );
        }
        assert_eq!(
            (agent_file(home, &barb), agent_file(home, &bubbles)),
            before,
            "suggesting never writes an agent"
        );
    }

    /// Entries the agent already has are not suggested; with all of them,
    /// there is no suggestion.
    #[test]
    fn entries_the_agent_has_are_not_suggested() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        projections(
            home,
            TEAM,
            "projections:\n  - {key: docs, source_subdir: docs}\n  - {key: kb, source_subdir: kb}\n",
        );
        let mut agent = draft("barb", AgentCli::Claude);
        agent.context = Some(vec![ContextEntry::new("docs")]);
        let key = save(home, &alice(), agent.clone());
        let got = suggestion_for(home, &alice(), &key, &agent).unwrap();
        assert_eq!(got.entries, vec![entry("kb", None)]);

        agent.context = Some(vec![ContextEntry::new("docs"), ContextEntry::new("kb")]);
        assert_eq!(suggestion_for(home, &alice(), &key, &agent), None);
    }

    /// M1 (SW-XS-16): a nested entry with `projections.yml`'s default `dest` (its last
    /// segment) is suggested to a defaulted agent even though `.claude/skills` holds its
    /// files, because it lands elsewhere; accepting it (the editor's `add_entries`) adds
    /// it, and then nothing is suggested again: no accept loop.
    #[test]
    fn an_accepted_nested_dest_entry_is_not_suggested_again() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        projections(
            home,
            TEAM,
            "projections:\n  - {key: foo, source_subdir: .claude/skills/foo}\n  - {key: skills, source_subdir: .claude/skills, dest: .claude/skills}\n",
        );
        let mut agent = draft("barb", AgentCli::Claude);
        let key = save(home, &alice(), agent.clone());
        let got = suggestion_for(home, &alice(), &key, &agent).expect("a suggestion");
        assert_eq!(
            got.entries,
            vec![entry(".claude/skills/foo", Some("foo"))],
            "`.claude/skills` placed the same is had through the default; the nested remap is not"
        );
        let accepted: Vec<ContextEntry> = got
            .entries
            .iter()
            .map(|e| match &e.dest {
                Some(d) => ContextEntry::with_dest(e.path.clone(), d.clone()),
                None => ContextEntry::new(e.path.clone()),
            })
            .collect();
        let crate::agents::context_edit::ContextEdit::Context(next) =
            crate::agents::context_edit::plan_add_entries(&agent, &accepted)
        else {
            panic!("accepting adds the entry");
        };
        assert!(next.contains(&ContextEntry::with_dest(".claude/skills/foo", "foo")));
        agent.context = Some(next);
        assert_eq!(suggestion_for(home, &alice(), &key, &agent), None);
    }

    /// One-time: a dismissed suggestion stays dismissed until it changes.
    #[test]
    fn a_dismissed_suggestion_stays_dismissed_until_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        projections(
            home,
            TEAM,
            "projections:\n  - {key: docs, source_subdir: docs}\n",
        );
        let agent = draft("barb", AgentCli::Claude);
        let key = save(home, &alice(), agent.clone());
        let first = suggestion_for(home, &alice(), &key, &agent).unwrap();

        dismiss(home, &alice(), &key, &first.fingerprint).unwrap();
        assert_eq!(suggestion_for(home, &alice(), &key, &agent), None);

        projections(
            home,
            TEAM,
            "projections:\n  - {key: docs, source_subdir: docs}\n  - {key: kb, source_subdir: kb}\n",
        );
        let changed = suggestion_for(home, &alice(), &key, &agent).unwrap();
        assert_ne!(changed.fingerprint, first.fingerprint);
    }

    /// S8: the fingerprint is over the raw `projections.yml` specs, so a
    /// dismissal holds when the agent's own `context` changes the filtered set.
    #[test]
    fn a_dismissal_survives_the_agent_taking_some_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        projections(
            home,
            TEAM,
            "projections:\n  - {key: docs, source_subdir: docs}\n  - {key: kb, source_subdir: kb}\n  - {key: ops, source_subdir: ops}\n",
        );
        let mut agent = draft("barb", AgentCli::Claude);
        let key = save(home, &alice(), agent.clone());
        let first = suggestion_for(home, &alice(), &key, &agent).unwrap();
        dismiss(home, &alice(), &key, &first.fingerprint).unwrap();

        agent.context = Some(vec![ContextEntry::new("docs")]);
        assert_eq!(suggestion_for(home, &alice(), &key, &agent), None);
    }

    /// A dismissal record that exists but is corrupt is never overwritten.
    #[test]
    fn a_corrupt_record_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        let path = dismissed_path(home, &alice());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        let key = AgentKey::new(ORG, TEAM, "barb");
        assert!(dismiss(home, &alice(), &key, "f").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    #[test]
    fn no_projections_file_or_a_starter_suggests_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_org(home, &alice());
        let agent = draft("barb", AgentCli::Claude);
        let key = save(home, &alice(), agent.clone());
        assert_eq!(suggestion_for(home, &alice(), &key, &agent), None);

        projections(
            home,
            TEAM,
            "projections:\n  - {key: docs, source_subdir: docs}\n",
        );
        let starter = AgentKey::parse("builtin/claude").unwrap();
        assert_eq!(suggestion_for(home, &alice(), &starter, &agent), None);
    }
}
