// SPDX-License-Identifier: MIT
//! Which slash-command names an agent starts with, derived from the files its
//! startup manifest places.
//!
//! Startup entries are plain content paths, never classified when the agent
//! is saved. This is the one place a path becomes a `/name`, read off every
//! resolved file of every entry (declared, dependency or required), so a
//! directory entry such as `.claude/skills/` counts everything under it:
//!
//! - **claude**: `.claude/commands/<x>.md` provides command `<x>`, a nested
//!   `.claude/commands/<ns>/<x>.md` provides `<ns>:<x>` and also `<x>` (the
//!   name Claude Code gives it today, showing the subdirectory only in the
//!   command's description), and `.claude/skills/<x>/SKILL.md` provides
//!   skill `<x>`.
//! - **codex**: `.agents/skills/<x>/SKILL.md` provides skill `<x>`. Codex has
//!   no project command directory, and `.claude/…` files provide nothing to
//!   it (they are still placed).
//!
//! Nothing else provides a name.

use std::collections::BTreeSet;

use serde::Serialize;

use super::manifest::{entry_hash, ManifestFile, StartupManifest};
use codesense_pty_core::agent_cli::AgentCli;

/// The slash-command names a launch of the agent will have, each list sorted
/// and without duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentProvides {
    pub commands: Vec<String>,
    pub skills: Vec<String>,
}

impl AgentProvides {
    /// Whether `/name` resolves to a command or a skill this agent starts with.
    pub fn has(&self, name: &str) -> bool {
        self.commands.iter().any(|c| c == name) || self.skills.iter().any(|s| s == name)
    }
}

/// The names `manifest`'s files provide to `runtime`.
pub fn derive(runtime: AgentCli, manifest: &StartupManifest) -> AgentProvides {
    let mut commands = BTreeSet::new();
    let mut skills = BTreeSet::new();
    let descriptor = codesense_pty_core::agent_cli::descriptor(runtime);
    let skills_dir = descriptor.skills_dir.as_str();
    for file in manifest.entries.iter().flat_map(|e| &e.files) {
        let path = file.destination.as_str();
        if let Some(stem) = descriptor
            .commands_dir
            .as_deref()
            .and_then(|dir| path.strip_prefix(dir))
            .and_then(command_stem)
        {
            commands.insert(stem.join(":"));
            if let Some(plain) = stem.last().filter(|_| stem.len() > 1) {
                commands.insert(plain.to_string());
            }
            continue;
        }
        if let Some(name) = path.strip_prefix(skills_dir).and_then(skill_name) {
            skills.insert(name.to_string());
        }
    }
    AgentProvides {
        commands: commands.into_iter().collect(),
        skills: skills.into_iter().collect(),
    }
}

/// A skill the manifest provides and the hash of its own files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillHash {
    pub name: String,
    /// `None` when no manifest file backs the skill.
    pub hash: Option<String>,
}

/// The hash of skill `name`: [`entry_hash`] over the manifest files under `<skills_dir><name>/`,
/// wherever they came from. A skills directory placed as one entry still gives each skill its
/// own hash, and a changed file changes only its skill's. `None` when no manifest file is under
/// the skill.
pub fn skill_hash(runtime: AgentCli, manifest: &StartupManifest, name: &str) -> Option<String> {
    let prefix = format!(
        "{}{name}/",
        codesense_pty_core::agent_cli::descriptor(runtime).skills_dir
    );
    let files: Vec<ManifestFile> = manifest
        .entries
        .iter()
        .flat_map(|e| &e.files)
        .filter(|f| f.destination.starts_with(&prefix))
        .cloned()
        .collect();
    (!files.is_empty()).then(|| entry_hash(&files))
}

/// Every skill `manifest` provides to `runtime`, each with its own hash.
pub fn skill_hashes(runtime: AgentCli, manifest: &StartupManifest) -> Vec<SkillHash> {
    derive(runtime, manifest)
        .skills
        .into_iter()
        .map(|name| SkillHash {
            hash: skill_hash(runtime, manifest, &name),
            name,
        })
        .collect()
}

/// `x.md` → `[x]`, `ns/x.md` → `[ns, x]`.
fn command_stem(rest: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = rest.strip_suffix(".md")?.split('/').collect();
    (!parts.iter().any(|p| p.is_empty())).then_some(parts)
}

/// `x/SKILL.md` → `x`; anything else under the skills dir names nothing.
fn skill_name(rest: &str) -> Option<&str> {
    let (dir, file) = rest.split_once('/')?;
    (!dir.is_empty() && file == "SKILL.md").then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::keys::AgentKey;
    use crate::agents::manifest::{
        content_hash, AgentRevisionRef, ContentLayer, EntryOrigin, ManifestEntry, ManifestFile,
        ManifestTotals,
    };
    use crate::agents::paths::StartupPath;

    fn manifest(runtime: AgentCli, entries: &[(&str, &[&str])]) -> StartupManifest {
        StartupManifest {
            agent: AgentRevisionRef {
                key: AgentKey::new("acme/api", "backend", "a"),
                revision: 1,
                definition_hash: content_hash(b""),
            },
            runtime,
            entries: entries
                .iter()
                .map(|(path, files)| ManifestEntry {
                    path: StartupPath::parse(path).unwrap(),
                    origin: EntryOrigin::Declared,
                    layer: ContentLayer::Team {
                        org: "acme/api".into(),
                        team: "backend".into(),
                    },
                    entry_hash: String::new(),
                    files: files
                        .iter()
                        .map(|f| ManifestFile {
                            logical: f.to_string(),
                            destination: f.to_string(),
                            hash: content_hash(b""),
                            bytes: 0,
                            mode: 0o644,
                            layer: None,
                        })
                        .collect(),
                })
                .collect(),
            essentials: Vec::new(),
            generated: Vec::new(),
            totals: ManifestTotals::default(),
            errors: Vec::new(),
            manifest_hash: String::new(),
        }
    }

    fn provides(runtime: AgentCli, entries: &[(&str, &[&str])]) -> AgentProvides {
        derive(runtime, &manifest(runtime, entries))
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_come_from_the_resolved_paths() {
        let p = provides(
            AgentCli::Claude,
            &[(
                ".claude/commands/bmad-help.md",
                &[".claude/commands/bmad-help.md"],
            )],
        );
        assert_eq!(p.commands, names(&["bmad-help"]));
        assert_eq!(p.skills, names(&[]));

        let p = provides(
            AgentCli::Claude,
            &[(
                ".claude/skills/bmad-build",
                &[
                    ".claude/skills/bmad-build/SKILL.md",
                    ".claude/skills/bmad-build/ref/notes.md",
                ],
            )],
        );
        assert_eq!(p.skills, names(&["bmad-build"]));
        assert_eq!(p.commands, names(&[]));

        // A whole skills dir provides every skill dir holding a SKILL.md.
        let p = provides(
            AgentCli::Claude,
            &[(
                ".claude/skills",
                &[
                    ".claude/skills/b/SKILL.md",
                    ".claude/skills/a/SKILL.md",
                    ".claude/skills/c/notes.md",
                    ".claude/skills/README.md",
                    ".claude/skills/a/deep/SKILL.md",
                ],
            )],
        );
        assert_eq!(p.skills, names(&["a", "b"]));

        let p = provides(
            AgentCli::Claude,
            &[(".claude/skills/x/notes.md", &[".claude/skills/x/notes.md"])],
        );
        assert_eq!(p, AgentProvides::default());
    }

    #[test]
    fn nested_commands_are_namespaced_and_lists_are_sorted_and_deduped() {
        let p = provides(
            AgentCli::Claude,
            &[
                (
                    ".claude/commands",
                    &[
                        ".claude/commands/zeta.md",
                        ".claude/commands/ops/deploy.md",
                        ".claude/commands/notes.txt",
                    ],
                ),
                (".claude/skills/zeta", &[".claude/skills/zeta/SKILL.md"]),
                (".agents/skills/zeta", &[".agents/skills/zeta/SKILL.md"]),
            ],
        );
        assert_eq!(p.commands, names(&["deploy", "ops:deploy", "zeta"]));
        assert_eq!(p.skills, names(&["zeta"]));
    }

    #[test]
    fn each_runtime_reads_only_its_own_dirs() {
        let entries: &[(&str, &[&str])] = &[
            (".agents/skills/y", &[".agents/skills/y/SKILL.md"]),
            (".claude/skills/x", &[".claude/skills/x/SKILL.md"]),
            (".claude/commands/c.md", &[".claude/commands/c.md"]),
        ];
        let codex = provides(AgentCli::Codex, entries);
        assert_eq!(codex.skills, names(&["y"]));
        assert_eq!(codex.commands, names(&[]));
        let claude = provides(AgentCli::Claude, entries);
        assert_eq!(claude.skills, names(&["x"]));
        assert_eq!(claude.commands, names(&["c"]));
    }

    /// A manifest whose files carry the given content hashes.
    fn hashed(runtime: AgentCli, entries: &[(&str, &[(&str, &str)])]) -> StartupManifest {
        let mut m = manifest(runtime, &[]);
        m.entries = entries
            .iter()
            .map(|(path, files)| ManifestEntry {
                path: StartupPath::parse(path).unwrap(),
                origin: EntryOrigin::Declared,
                layer: ContentLayer::Team {
                    org: "acme/api".into(),
                    team: "backend".into(),
                },
                entry_hash: String::new(),
                files: files
                    .iter()
                    .map(|(f, hash)| ManifestFile {
                        logical: f.to_string(),
                        destination: f.to_string(),
                        hash: hash.to_string(),
                        bytes: 0,
                        mode: 0o644,
                        layer: None,
                    })
                    .collect(),
            })
            .collect();
        m
    }

    #[test]
    fn a_per_skill_directory_entry_gives_that_skill_a_hash() {
        let m = hashed(
            AgentCli::Claude,
            &[(
                ".claude/skills/bmad-build",
                &[
                    (".claude/skills/bmad-build/SKILL.md", "h-skill"),
                    (".claude/skills/bmad-build/ref/notes.md", "h-notes"),
                ],
            )],
        );
        let hashes = skill_hashes(AgentCli::Claude, &m);
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].name, "bmad-build");
        assert!(hashes[0].hash.is_some());
    }

    #[test]
    fn a_shared_skills_entry_still_gives_each_skill_its_own_hash_and_a_change_moves_only_one() {
        let files = |a: &'static str| -> Vec<(&'static str, &'static str)> {
            vec![
                (".claude/skills/a/SKILL.md", a),
                (".claude/skills/a/deep/ref.md", "a-ref"),
                (".claude/skills/b/SKILL.md", "b1"),
                (".claude/skills/README.md", "readme"),
            ]
        };
        let before = files("a1");
        let after = files("a2");
        let by_name = |files: &[(&str, &str)]| {
            skill_hashes(
                AgentCli::Claude,
                &hashed(AgentCli::Claude, &[(".claude/skills", files)]),
            )
        };
        let (b0, a0) = (by_name(&before), by_name(&after));
        assert_eq!(
            b0.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_ne!(b0[0].hash, b0[1].hash, "one hash per skill, not per entry");
        assert!(b0.iter().all(|s| s.hash.is_some()));
        assert_ne!(b0[0].hash, a0[0].hash, "the changed skill's hash moves");
        assert_eq!(b0[1].hash, a0[1].hash, "the other skill's hash does not");
    }

    #[test]
    fn a_skill_with_no_manifest_file_has_no_hash() {
        let m = hashed(
            AgentCli::Claude,
            &[(".claude/skills/a", &[(".claude/skills/a/SKILL.md", "h")])],
        );
        assert_eq!(skill_hash(AgentCli::Claude, &m, "ghost"), None);
        // The other runtime's directory backs nothing for this one.
        assert_eq!(skill_hash(AgentCli::Codex, &m, "a"), None);
    }
}
