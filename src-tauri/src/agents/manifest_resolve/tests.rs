// SPDX-License-Identifier: MIT
//! Seam tests for the manifest resolver: real temp homes, real org layouts
//! with a parent team, the real agent store, and a counting content reader.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::context_repo::scratch_space::{self, SCRATCH_TEAM};
use crate::context_repo::{path_to_org_root, path_to_team_config_dir, path_to_team_content_dir};
use crate::domain::Principal;
use crate::routines::model::Author;
use crate::routines::PersonalOwner;

use super::super::content_reader::CountingReader;
use super::super::keys::AgentKey;
use super::super::manifest::{
    content_hash, entry_hash, json_hash, AgentRevisionRef, ContentLayer, EntryOrigin,
    EssentialEntry, GeneratedEntry, GeneratedTarget, ManifestEntry, ManifestErrorCode,
    ManifestFile, ManifestTotals, StartupManifest,
};
use super::super::model::{Agent, LaunchMode, McpAccess};
use super::super::paths::StartupPath;
use super::super::service::{self, Placement, SaveTarget};
use super::*;
use codesense_pty_core::agent_cli::AgentCli;

const ORG: &str = "acme/api";
const TEAM: &str = "backend";
const PARENT: &str = "platform";
const UID: u64 = 1;

fn me() -> Principal {
    Principal::from(UID)
}

/// A locally-cloned org whose `backend` team has `platform` as its parent.
fn write_org(home: &Path) -> PathBuf {
    let org_root = path_to_org_root(home, &me(), ORG).unwrap();
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {ORG}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 1\n    github_username: alice\n",
    )
    .unwrap();
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n    parent_team: {PARENT}\n  - name: {PARENT}\n"),
    )
    .unwrap();
    for team in [TEAM, PARENT] {
        std::fs::create_dir_all(path_to_team_content_dir(&org_root, team)).unwrap();
    }
    std::fs::create_dir_all(org_root.join(PARENT).join("config")).unwrap();
    std::fs::write(
        org_root.join(PARENT).join("config/sharing.yml"),
        "share: ['content/**', 'panes/**']\n",
    )
    .unwrap();
    org_root
}

fn team_content(home: &Path, team: &str) -> PathBuf {
    path_to_team_content_dir(&path_to_org_root(home, &me(), ORG).unwrap(), team)
}

/// The target org's scratch-space content dir, created.
fn personal_content(home: &Path) -> PathBuf {
    scratch_space::ensure_org_space(home, &me(), ORG)
        .unwrap()
        .0
        .content_dir()
}

fn write(root: &Path, logical: &str, body: &[u8]) {
    let path = root.join(logical);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[cfg(unix)]
fn chmod(root: &Path, logical: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join(logical), std::fs::Permissions::from_mode(mode)).unwrap();
}

fn agent(runtime: AgentCli, startup: &[&str]) -> Agent {
    Agent {
        role: None,
        id: "builder".into(),
        name: "Builder".into(),
        description: String::new(),
        created_by: "1".into(),
        runtime,
        args: Vec::new(),
        launch: LaunchMode::Foreground,
        mcp: McpAccess::default(),
        startup: startup.iter().map(|s| s.to_string()).collect(),
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 1,
        archived_at: None,
        extra: BTreeMap::new(),
        env: Default::default(),
    }
}

fn reference() -> AgentRevisionRef {
    AgentRevisionRef {
        key: AgentKey::new(ORG, TEAM, "builder"),
        revision: 1,
        definition_hash: content_hash(b"agent.yaml"),
    }
}

fn target() -> ManifestTarget {
    ManifestTarget {
        context_root: None,
        org: ORG.into(),
        team: TEAM.into(),
        repo: None,
    }
}

fn team_layer(team: &str) -> ContentLayer {
    ContentLayer::Team {
        org: ORG.into(),
        team: team.into(),
    }
}

fn personal_layer() -> ContentLayer {
    ContentLayer::Personal {
        account: UID.to_string(),
        org: Some(ORG.into()),
    }
}

fn path(raw: &str) -> StartupPath {
    StartupPath::parse(raw).unwrap()
}

fn resolve(home: &Path, agent: &Agent) -> StartupManifest {
    resolve_manifest(
        home,
        &me(),
        agent,
        &reference(),
        target(),
        Vec::new(),
        Vec::new(),
    )
}

/// A Codex agent: no CodeSense-required content entry, so a test sees only
/// what it declares.
fn resolve_codex(home: &Path, startup: &[&str]) -> StartupManifest {
    resolve(home, &agent(AgentCli::Codex, startup))
}

fn codes(manifest: &StartupManifest) -> Vec<(Option<String>, ManifestErrorCode)> {
    manifest
        .errors
        .iter()
        .map(|e| (e.path.as_ref().map(ToString::to_string), e.code))
        .collect()
}

fn entry<'a>(manifest: &'a StartupManifest, raw: &str) -> &'a ManifestEntry {
    manifest
        .entries
        .iter()
        .find(|e| e.path.as_str() == raw)
        .unwrap_or_else(|| panic!("no entry {raw} in {:#?}", manifest.entries))
}

fn mf(logical: &str, body: &[u8], mode: u32) -> ManifestFile {
    ManifestFile {
        logical: logical.into(),
        destination: logical.into(),
        hash: content_hash(body),
        bytes: body.len() as u64,
        mode,
        layer: None,
    }
}

/// What lifecycle bootstrap does with a manifest: copy exactly
/// `content_root(layer)/logical` to `destination`, checking `hash`, never
/// looking anywhere else. A mismatch stops with nothing substituted.
fn apply_exact(
    home: &Path,
    principal: &Principal,
    manifest: &StartupManifest,
    workspace: &Path,
) -> Result<(), String> {
    assert!(manifest.errors.is_empty(), "{:?}", manifest.errors);
    for entry in &manifest.entries {
        let root = entry
            .layer
            .content_root(home, principal)
            .map_err(|e| e.to_string())?;
        for file in &entry.files {
            let bytes = std::fs::read(root.join(&file.logical)).map_err(|e| e.to_string())?;
            if content_hash(&bytes) != file.hash {
                return Err(format!("{} changed since the manifest", file.logical));
            }
            let dest = workspace.join(&file.destination);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, bytes).unwrap();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The seams
// ---------------------------------------------------------------------------

/// An agent saved through the real store resolves against real team content
/// into exact entries, in order, with every origin, layer, hash and mode.
#[cfg(unix)]
#[test]
fn seam_saved_agent_resolves_to_exact_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    let skill = b"---\nname: bmad-build\nrequires: [.claude/commands/bmad-help.md]\n---\nBuild.\n";
    write(&team, ".claude/skills/bmad-build/SKILL.md", skill);
    write(
        &team,
        ".claude/skills/bmad-build/step-01.md",
        b"Step one.\n",
    );
    write(&team, ".claude/commands/bmad-help.md", b"Help.\n");
    write(&team, ".claude/hooks/codesense-notify.sh", b"#!/bin/sh\n");
    chmod(&team, ".claude/hooks/codesense-notify.sh", 0o755);
    write(&team, "CLAUDE.md", b"# Team\n");
    for logical in [
        ".claude/skills/bmad-build/SKILL.md",
        ".claude/skills/bmad-build/step-01.md",
        ".claude/commands/bmad-help.md",
        "CLAUDE.md",
    ] {
        chmod(&team, logical, 0o644);
    }

    let saved = service::save(
        home,
        &me(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        agent(
            AgentCli::Claude,
            &["./.claude/skills/bmad-build/", "CLAUDE.md"],
        ),
        &Author::user("alice"),
    )
    .unwrap();
    let pinned = service::get_pinned(home, &me(), &saved.key, 1).unwrap();
    let target = ManifestTarget::for_launch(&saved.key, None);
    assert_eq!(target, self::target());
    let generated = GeneratedEntry {
        id: "claude-permissions".into(),
        target: GeneratedTarget::LaunchSettings,
        content: serde_json::json!({"permissions": {"deny": ["Bash(rm:*)"]}}),
        hash: "stale".into(),
        required: Vec::new(),
        path: None,
        merge_keys: Default::default(),
    };
    let manifest = resolve_manifest(
        home,
        &me(),
        &pinned.agent,
        &pinned.reference,
        target,
        vec![generated.clone()],
        Vec::new(),
    );

    let build = vec![
        mf(".claude/skills/bmad-build/SKILL.md", skill, 0o644),
        mf(
            ".claude/skills/bmad-build/step-01.md",
            b"Step one.\n",
            0o644,
        ),
    ];
    let help = vec![mf(".claude/commands/bmad-help.md", b"Help.\n", 0o644)];
    let claude_md = vec![mf("CLAUDE.md", b"# Team\n", 0o644)];
    let expected = vec![
        ManifestEntry {
            path: path(".claude/skills/bmad-build"),
            origin: EntryOrigin::Declared,
            layer: team_layer(TEAM),
            entry_hash: entry_hash(&build),
            files: build,
        },
        ManifestEntry {
            path: path(".claude/commands/bmad-help.md"),
            origin: EntryOrigin::Dependency {
                of: path(".claude/skills/bmad-build"),
            },
            layer: team_layer(TEAM),
            entry_hash: entry_hash(&help),
            files: help,
        },
        ManifestEntry {
            path: path("CLAUDE.md"),
            origin: EntryOrigin::Declared,
            layer: team_layer(TEAM),
            entry_hash: entry_hash(&claude_md),
            files: claude_md,
        },
    ];
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.entries, expected);
    assert_eq!(manifest.agent, pinned.reference);
    assert_eq!(manifest.runtime, AgentCli::Claude);
    assert_eq!(
        manifest.totals,
        ManifestTotals {
            files: 4,
            bytes: (skill.len() + 10 + 6 + 7) as u64,
        }
    );
    assert_eq!(
        manifest.essentials,
        vec![
            EssentialEntry {
                id: "codesense-bridge".into(),
                reason: "CodeSense MCP bridge — required by CodeSense".into(),
            },
            EssentialEntry {
                id: "codesense-notify-settings".into(),
                reason: "CodeSense notification hook settings — required by CodeSense".into(),
            },
        ]
    );
    // Generated entries are placed as given, hashed from their content.
    assert_eq!(
        manifest.generated,
        vec![GeneratedEntry {
            hash: json_hash(&generated.content),
            ..generated
        }]
    );
    assert_eq!(manifest.manifest_hash, manifest.compute_hash());
}

/// Four different copies of one skill: personal, the target team, its parent
/// hop and a source repo. The manifest names the personal copy; applying it
/// exactly yields the personal bytes, and once the personal file changes the
/// hash check fails rather than falling back to another layer.
#[test]
fn seam_bootstrap_applies_the_recorded_layer_and_never_re_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    let repo = home.join("users/1/sources/acme/api");
    for (root, body) in [
        (personal.clone(), "personal"),
        (team_content(home, TEAM), "team"),
        (team_content(home, PARENT), "parent"),
        (repo.clone(), "repo"),
    ] {
        write(&root, ".claude/skills/x/SKILL.md", body.as_bytes());
    }

    let manifest = resolve_codex(home, &[".claude/skills/x/"]);
    assert_eq!(manifest.errors, Vec::new());
    let x = entry(&manifest, ".claude/skills/x");
    assert_eq!(x.layer, personal_layer());
    assert_eq!(x.files.len(), 1);
    assert_eq!(x.files[0].logical, ".claude/skills/x/SKILL.md");
    assert_eq!(x.files[0].hash, content_hash(b"personal"));

    let workspace = tmp.path().join("workspace");
    apply_exact(home, &me(), &manifest, &workspace).unwrap();
    assert_eq!(
        std::fs::read(workspace.join(".claude/skills/x/SKILL.md")).unwrap(),
        b"personal"
    );

    write(&personal, ".claude/skills/x/SKILL.md", b"personal, edited");
    let fresh = tmp.path().join("fresh");
    let err = apply_exact(home, &me(), &manifest, &fresh).unwrap_err();
    assert!(err.contains(".claude/skills/x/SKILL.md"), "{err}");
    assert!(!fresh.join(".claude/skills/x/SKILL.md").exists());
}

/// With no personal copy the target team wins over its parent hop; with no
/// target copy the parent hop wins and is named. The chain comes from the
/// org's real `teams.yml`.
#[test]
fn seam_team_chain_hops_resolve_nearest_first_and_name_the_hop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    personal_content(home);
    write(&team_content(home, TEAM), "docs/guide.md", b"team");
    write(&team_content(home, PARENT), "docs/guide.md", b"parent");
    write(
        &team_content(home, PARENT),
        "docs/only-parent.md",
        b"parent",
    );

    let manifest = resolve_codex(home, &["docs/guide.md", "docs/only-parent.md"]);
    assert_eq!(manifest.errors, Vec::new());
    let guide = entry(&manifest, "docs/guide.md");
    assert_eq!(guide.layer, team_layer(TEAM));
    assert_eq!(guide.files[0].hash, content_hash(b"team"));
    let inherited = entry(&manifest, "docs/only-parent.md");
    assert_eq!(inherited.layer, team_layer(PARENT));
    assert_eq!(
        inherited.layer.content_root(home, &me()).unwrap(),
        team_content(home, PARENT)
    );
}

/// The target org's scratch space is the first layer and all-orgs the next;
/// another org's scratch content never layers into this org.
#[test]
fn the_target_orgs_scratch_space_layers_first_and_never_another_orgs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, TEAM), "docs/guide.md", b"team");
    write(&personal_content(home), "docs/guide.md", b"mine");
    let (other, _) = scratch_space::ensure_org_space(home, &me(), "other/org").unwrap();
    write(&other.content_dir(), "docs/secret.md", b"other org");
    write(
        &scratch_space::scratch_all_orgs_root(home, &me()),
        "prompts/carry.md",
        b"everywhere",
    );

    let manifest = resolve_codex(
        home,
        &["docs/guide.md", "prompts/carry.md", "docs/secret.md"],
    );
    assert_eq!(
        codes(&manifest),
        vec![(Some("docs/secret.md".into()), ManifestErrorCode::Missing)],
        "org B's scratch file is invisible here"
    );
    assert_eq!(manifest.entries[0].layer, personal_layer());
    assert_eq!(manifest.entries[0].files[0].hash, content_hash(b"mine"));
    assert_eq!(
        manifest.entries[1].layer,
        ContentLayer::Personal {
            account: UID.to_string(),
            org: None,
        }
    );
}

/// SLW-PT-20: one rule — what a team's worktree sees is what its agents start with.
/// "Use team's" at a path makes an agent launched for THAT team start with the team's
/// file (a directory holding it resolves from the team); another team still gets the
/// scratch file; the switch off drops every scratch layer for that team.
#[test]
fn use_teams_and_the_switch_apply_to_an_agents_startup_files_for_that_team() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, TEAM), "CLAUDE.md", b"team rules");
    write(
        &team_content(home, TEAM),
        ".claude/skills/review/SKILL.md",
        b"team review",
    );
    write(&team_content(home, TEAM), "docs/guide.md", b"team guide");
    let mine = personal_content(home);
    write(&mine, "CLAUDE.md", b"my rules");
    write(&mine, ".claude/skills/review/SKILL.md", b"my review");
    write(&mine, ".claude/skills/review/mine.md", b"my extra step");
    write(
        &team_content(home, TEAM),
        ".claude/skills/review/team-only.md",
        b"team extra",
    );
    write(&mine, "docs/guide.md", b"my guide");

    scratch_space::set_team_file_uses_team(home, &me(), ORG, TEAM, "CLAUDE.md", true).unwrap();
    scratch_space::set_team_file_uses_team(
        home,
        &me(),
        ORG,
        TEAM,
        ".claude/skills/review/SKILL.md",
        true,
    )
    .unwrap();
    let manifest = resolve_codex(
        home,
        &["CLAUDE.md", ".claude/skills/review/", "docs/guide.md"],
    );
    assert!(codes(&manifest).is_empty(), "{:?}", codes(&manifest));
    let by_path = |p: &str| {
        manifest
            .entries
            .iter()
            .find(|e| e.path == path(p))
            .unwrap()
            .clone()
    };
    assert_eq!(by_path("CLAUDE.md").layer, team_layer(TEAM));
    assert_eq!(
        by_path("CLAUDE.md").files[0].hash,
        content_hash(b"team rules")
    );
    // The folder merges per file, as the worktree overlay does: the team's SKILL.md ("Use
    // team's"), the team-only file, and the scratch-only file.
    let folder = by_path(".claude/skills/review/");
    let files: Vec<(String, String, ContentLayer)> = folder
        .files
        .iter()
        .map(|f| {
            (
                f.logical.clone(),
                f.hash.clone(),
                f.layer.clone().unwrap_or_else(|| folder.layer.clone()),
            )
        })
        .collect();
    assert_eq!(
        files,
        vec![
            (
                ".claude/skills/review/SKILL.md".into(),
                content_hash(b"team review"),
                team_layer(TEAM)
            ),
            (
                ".claude/skills/review/mine.md".into(),
                content_hash(b"my extra step"),
                personal_layer()
            ),
            (
                ".claude/skills/review/team-only.md".into(),
                content_hash(b"team extra"),
                team_layer(TEAM)
            ),
        ]
    );
    assert_eq!(
        by_path("docs/guide.md").layer,
        personal_layer(),
        "every other path stays mine"
    );

    // Another team of the org keeps the scratch files.
    let other = resolve_manifest(
        home,
        &me(),
        &agent(AgentCli::Codex, &["CLAUDE.md"]),
        &reference(),
        ManifestTarget {
            context_root: None,
            org: ORG.into(),
            team: PARENT.into(),
            repo: None,
        },
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(other.entries[0].layer, personal_layer());

    // The switch off: no scratch layer at all for this team.
    scratch_space::set_team_uses_scratch(home, &me(), ORG, TEAM, false).unwrap();
    let off = resolve_codex(home, &["docs/guide.md"]);
    assert_eq!(off.entries[0].layer, team_layer(TEAM));
    assert_eq!(off.entries[0].files[0].hash, content_hash(b"team guide"));
}

/// A personal agent with no launch target resolves in the personal space
/// alone; given a target team it resolves there too.
#[test]
fn a_personal_agent_resolves_personal_alone_unless_given_a_target() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let saved = service::save(
        home,
        &me(),
        SaveTarget::Create(Placement::Personal(PersonalOwner {
            github_user_id: UID,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        })),
        agent(AgentCli::Codex, &["docs/team-only.md"]),
        &Author::user("alice"),
    )
    .unwrap();
    write(&team_content(home, TEAM), "docs/team-only.md", b"team");
    let pinned = service::get_pinned(home, &me(), &saved.key, 1).unwrap();

    let alone = ManifestTarget::for_launch(&saved.key, None);
    assert_eq!(
        (alone.org.as_str(), alone.team.as_str()),
        (ORG, SCRATCH_TEAM)
    );
    let manifest = resolve_manifest(
        home,
        &me(),
        &pinned.agent,
        &pinned.reference,
        alone,
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        codes(&manifest),
        vec![(Some("docs/team-only.md".into()), ManifestErrorCode::Missing)]
    );

    let manifest = resolve_manifest(
        home,
        &me(),
        &pinned.agent,
        &pinned.reference,
        ManifestTarget::for_launch(&saved.key, Some(target())),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.entries[0].layer, team_layer(TEAM));
}

/// The receiving side reads a manifest off the wire with the frozen types,
/// and every source it names exists and hashes as recorded.
#[test]
fn seam_a_serialized_manifest_round_trips_and_every_source_matches_its_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    write(&personal, "notes/me.md", b"mine");
    write(&team_content(home, TEAM), ".claude/skills/x/SKILL.md", b"x");
    write(
        &team_content(home, TEAM),
        ".claude/hooks/codesense-notify.sh",
        b"#!/bin/sh\n",
    );
    write(&team_content(home, PARENT), "docs/a/b.md", b"b");

    let manifest = resolve(
        home,
        &agent(
            AgentCli::Claude,
            &["notes/me.md", ".claude/skills/x", "docs/a"],
        ),
    );
    assert_eq!(manifest.errors, Vec::new());
    let wire = serde_json::to_string(&manifest).unwrap();
    let received: StartupManifest = serde_json::from_str(&wire).unwrap();
    assert_eq!(received, manifest);
    assert_eq!(received.manifest_hash, received.compute_hash());
    let layers: Vec<_> = received.entries.iter().map(|e| e.layer.clone()).collect();
    // The team's hook file is not one the agent starts with.
    assert_eq!(
        layers,
        vec![personal_layer(), team_layer(TEAM), team_layer(PARENT)]
    );
    for entry in &received.entries {
        let root = entry.layer.content_root(home, &me()).unwrap();
        for file in &entry.files {
            let bytes = std::fs::read(root.join(&file.logical)).unwrap();
            assert_eq!(content_hash(&bytes), file.hash, "{}", file.logical);
            assert_eq!(bytes.len() as u64, file.bytes);
            assert_eq!(file.destination, file.logical);
        }
    }
}

/// 2,000 unrelated documents in the team content are never touched: the
/// resolver reads only what the agent starts with.
#[test]
fn resolution_reads_only_the_startup_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    for i in 0..2000 {
        write(
            &team,
            &format!("docs/bulk/{:02}/doc-{i}.md", i % 40),
            b"unrelated",
        );
    }
    write(
        &team,
        ".claude/skills/x/SKILL.md",
        b"---\nrequires: [docs/one.md]\n---\n",
    );
    write(&team, ".claude/skills/x/ref/notes.md", b"notes");
    write(&team, "docs/one.md", b"one");

    let reader = CountingReader::default();
    let manifest = resolve_manifest_with(
        &reader,
        home,
        &me(),
        &agent(AgentCli::Codex, &[".claude/skills/x/"]),
        &reference(),
        target(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.totals.files, 3);
    let bulk = team.join("docs/bulk");
    let strays: Vec<_> = reader
        .touched()
        .into_iter()
        .filter(|p| p.starts_with(&bulk))
        .collect();
    assert!(strays.is_empty(), "unrelated paths touched: {strays:?}");
    let mut opened = reader.opened();
    opened.sort();
    assert_eq!(
        opened,
        vec![
            team.join(".claude/skills/x/SKILL.md"),
            team.join(".claude/skills/x/ref/notes.md"),
            team.join("docs/one.md"),
        ]
    );
}

// ---------------------------------------------------------------------------
// Expansion and destinations
// ---------------------------------------------------------------------------

/// A file is one entry file; a directory is every regular file under it,
/// recursively, sorted. The destination is always the logical path, even when
/// the team's `projections.yml` remaps that subtree for source repos, and the
/// runtime never filters a path.
#[test]
fn files_and_directories_expand_to_files_at_their_own_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "kit/z.md", b"z");
    write(&team, "kit/a/b/c.txt", b"c");
    write(&team, "kit/a/a.md", b"a");
    write(&team, ".claude/commands/x.md", b"x");
    let config = path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), TEAM);
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("projections.yml"),
        "projections:\n  - key: kit\n    source_subdir: kit\n    dest: tools/elsewhere\n",
    )
    .unwrap();

    let manifest = resolve_codex(home, &["kit", ".claude/commands/x.md"]);
    assert_eq!(manifest.errors, Vec::new());
    let kit = entry(&manifest, "kit");
    let logicals: Vec<_> = kit.files.iter().map(|f| f.logical.as_str()).collect();
    assert_eq!(logicals, vec!["kit/a/a.md", "kit/a/b/c.txt", "kit/z.md"]);
    for file in manifest.entries.iter().flat_map(|e| &e.files) {
        assert_eq!(file.destination, file.logical);
    }
    // A Codex agent naming a Claude command gets it, at that path.
    assert_eq!(
        entry(&manifest, ".claude/commands/x.md").files[0].destination,
        ".claude/commands/x.md"
    );
    assert_eq!(manifest.essentials.len(), 1);
    assert_eq!(manifest.essentials[0].id, "codesense-bridge");
    assert_eq!(manifest.totals, ManifestTotals { files: 4, bytes: 4 });
}

/// A declared path inside an earlier declared directory is refused naming
/// both; a requirement another entry already carries is dropped without a
/// word.
#[test]
fn overlapping_entries_have_one_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(
        &team,
        ".claude/skills/x/SKILL.md",
        b"---\nrequires: [.claude/skills/x/ref.md, docs/d.md]\n---\n",
    );
    write(&team, ".claude/skills/x/ref.md", b"ref");
    write(&team, "docs/d.md", b"---\nrequires: [kit/one.md]\n---\n");
    write(&team, "kit/one.md", b"one");

    let manifest = resolve_codex(home, &[".claude/skills/x/", ".claude/skills/x/SKILL.md"]);
    assert_eq!(
        codes(&manifest),
        vec![(
            Some(".claude/skills/x/SKILL.md".into()),
            ManifestErrorCode::Invalid
        )]
    );
    let message = &manifest.errors[0].message;
    assert!(
        message.contains("\".claude/skills/x/SKILL.md\""),
        "{message}"
    );
    assert!(message.contains("\".claude/skills/x\""), "{message}");
    assert!(manifest.entries.is_empty());

    let manifest = resolve_codex(home, &[".claude/skills/x/", "kit"]);
    assert_eq!(manifest.errors, Vec::new());
    let paths: Vec<_> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec![".claude/skills/x", "docs/d.md", "kit"]);
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlinks_and_excluded_paths_escape_the_content() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    let outside = tmp.path().join("outside");
    write(&outside, "secret.md", b"secret");
    std::os::unix::fs::symlink(&outside, team.join("linked")).unwrap();
    write(&team, "kit/ok.md", b"ok");
    std::os::unix::fs::symlink(outside.join("secret.md"), team.join("kit/link.md")).unwrap();
    write(&team, "conf/.env", b"TOKEN=1");
    write(&team, "conf/private/x.md", b"x");
    // A symlink in the personal layer stops resolution there; it never falls
    // through to the team copy.
    let personal = personal_content(home);
    write(&team, "shadow/a.md", b"team");
    std::os::unix::fs::symlink(&outside, personal.join("shadow")).unwrap();
    crate::install::projection_excludes::write_patterns_to(
        &path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), TEAM),
        vec!["conf/private/**".into()],
    )
    .unwrap();

    let manifest = resolve_codex(
        home,
        &[
            "linked/secret.md",
            "kit",
            "conf/.env",
            "conf/private/x.md",
            "shadow/a.md",
        ],
    );
    assert_eq!(
        codes(&manifest),
        vec![
            (
                Some("linked/secret.md".into()),
                ManifestErrorCode::EscapesContent
            ),
            (Some("kit".into()), ManifestErrorCode::EscapesContent),
            (Some("conf/.env".into()), ManifestErrorCode::EscapesContent),
            (
                Some("conf/private/x.md".into()),
                ManifestErrorCode::EscapesContent
            ),
            (
                Some("shadow/a.md".into()),
                ManifestErrorCode::EscapesContent
            ),
        ]
    );
    assert!(manifest.errors[1].message.contains("kit/link.md"));
    assert!(manifest.entries.is_empty());
}

#[test]
fn absent_or_empty_paths_are_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    std::fs::create_dir_all(team_content(home, TEAM).join("empty/inner")).unwrap();

    let manifest = resolve_codex(home, &["nowhere.md", "empty"]);
    assert_eq!(
        codes(&manifest),
        vec![
            (Some("nowhere.md".into()), ManifestErrorCode::Missing),
            (Some("empty".into()), ManifestErrorCode::Missing),
        ]
    );
    // A hand-edited definition with an unparseable path is invalid, not a panic.
    let manifest = resolve_codex(home, &["../escape.md"]);
    assert_eq!(codes(&manifest), vec![(None, ManifestErrorCode::Invalid)]);
}

fn mark(home: &Path, team: &str, logical: &str, array_keys: BTreeMap<String, String>) {
    crate::install::merge_marks::set_merge_mark(
        &path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), team),
        logical,
        array_keys,
    )
    .unwrap();
}

fn generated<'a>(manifest: &'a StartupManifest, id: &str) -> &'a GeneratedEntry {
    manifest
        .generated
        .iter()
        .find(|g| g.id == id)
        .unwrap_or_else(|| panic!("no generated {id} in {:#?}", manifest.generated))
}

/// SW-XS-14 AC3: a merge-marked `.mcp.json` present in two layers resolves to
/// one generated entry holding the deep merge (the nearer layer wins a shared
/// key), not an error, and no ordinary entry carries the file.
#[test]
fn merge_marked_file_across_two_layers_is_a_generated_deep_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(
        &team_content(home, PARENT),
        ".mcp.json",
        br#"{"mcpServers":{"slack":{"command":"slack"}},"note":"parent"}"#,
    );
    write(
        &team_content(home, TEAM),
        ".mcp.json",
        br#"{"mcpServers":{"jira":{"command":"jira"}},"note":"team"}"#,
    );
    mark(home, TEAM, ".mcp.json", BTreeMap::new());

    let manifest = resolve_codex(home, &[".mcp.json"]);

    assert_eq!(manifest.errors, Vec::new());
    assert!(
        manifest
            .entries
            .iter()
            .all(|e| e.files.iter().all(|f| f.logical != ".mcp.json")),
        "no ordinary entry carries the merge-marked file"
    );
    let merged = generated(&manifest, "merge:.mcp.json");
    assert_eq!(
        merged.content,
        serde_json::json!({
            "mcpServers": {"slack": {"command": "slack"}, "jira": {"command": "jira"}},
            "note": "team"
        })
    );
    assert_eq!(merged.hash, json_hash(&merged.content));
}

/// A merge-marked file inside a declared directory leaves the rest of the
/// directory as an ordinary entry; a mark in a parent team's config applies to
/// the child's copy too; declared array keys union the arrays.
#[test]
fn merge_marked_file_inside_a_declared_directory_splits_out() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(
        &team_content(home, PARENT),
        ".claude/settings.json",
        br#"{"hooks":{"Stop":[{"command":"a"}]}}"#,
    );
    write(
        &team_content(home, TEAM),
        ".claude/settings.json",
        br#"{"hooks":{"Stop":[{"command":"b"}]}}"#,
    );
    write(
        &team_content(home, TEAM),
        ".claude/skills/s/SKILL.md",
        b"skill",
    );
    mark(
        home,
        PARENT,
        ".claude/settings.json",
        BTreeMap::from([("hooks.Stop".to_string(), "command".to_string())]),
    );

    let manifest = resolve_codex(home, &[".claude"]);

    assert_eq!(manifest.errors, Vec::new());
    let files: Vec<&str> = entry(&manifest, ".claude")
        .files
        .iter()
        .map(|f| f.logical.as_str())
        .collect();
    assert_eq!(files, vec![".claude/skills/s/SKILL.md"]);
    let merged = generated(&manifest, "merge:.claude/settings.json");
    assert_eq!(
        merged.content,
        serde_json::json!({"hooks": {"Stop": [{"command": "a"}, {"command": "b"}]}})
    );
}

/// A merge-marked file that is not JSON, YAML or TOML cannot be merged: that
/// is a definition error naming it.
#[test]
fn merge_marked_unstructured_file_is_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, TEAM), "notes.md", b"# hi");
    // The mark API refuses an unstructured path, so this is a hand-edited store.
    let config = path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), TEAM);
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("merge-marked.yaml"), "marks:\n  notes.md: {}\n").unwrap();

    let manifest = resolve_codex(home, &["notes.md"]);
    assert_eq!(
        codes(&manifest),
        vec![(Some("notes.md".into()), ManifestErrorCode::Invalid)]
    );
}

/// Size never refuses an entry: a file larger than any chunk and a directory
/// of hundreds of files resolve whole, each hashed exactly as bootstrap
/// checks it.
#[test]
fn large_files_and_directories_resolve_without_a_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    let big = vec![7u8; 5 * 1024 * 1024 + 3];
    write(&team, "big.bin", &big);
    for i in 0..300 {
        write(&team, &format!("many/f{i:03}.md"), b"x");
    }
    let manifest = resolve_codex(home, &["big.bin", "many"]);
    assert_eq!(manifest.errors, Vec::new());
    let carried = &entry(&manifest, "big.bin").files[0];
    assert_eq!(carried.hash, content_hash(&big));
    assert_eq!(carried.bytes, big.len() as u64);
    assert_eq!(entry(&manifest, "many").files.len(), 300);
    assert_eq!(manifest.totals.files, 301);
    assert_eq!(manifest.totals.bytes, big.len() as u64 + 300);
}

// ---------------------------------------------------------------------------
// Dependency closure
// ---------------------------------------------------------------------------

/// Requirements resolve transitively, each right after the entry that needs
/// it, once. Any Markdown file can declare them.
#[test]
fn requires_close_transitively_in_order_and_dedupe() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", b"---\nrequires: [b.md, c]\n---\n");
    write(&team, "b.md", b"---\nrequires:\n  - ./d.md\n---\n");
    write(&team, "c/one.md", b"---\nrequires: [d.md]\n---\n");
    write(&personal, "d.md", b"d");
    write(&team, "e.md", b"---\nrequires: [b.md]\n---\n");

    let manifest = resolve_codex(home, &["a.md", "e.md"]);
    assert_eq!(manifest.errors, Vec::new());
    let shape: Vec<_> = manifest
        .entries
        .iter()
        .map(|e| (e.path.to_string(), e.origin.clone(), e.layer.clone()))
        .collect();
    let dep = |of: &str| EntryOrigin::Dependency { of: path(of) };
    assert_eq!(
        shape,
        vec![
            ("a.md".into(), EntryOrigin::Declared, team_layer(TEAM)),
            ("b.md".into(), dep("a.md"), team_layer(TEAM)),
            ("d.md".into(), dep("b.md"), personal_layer()),
            ("c".into(), dep("a.md"), team_layer(TEAM)),
            ("e.md".into(), EntryOrigin::Declared, team_layer(TEAM)),
        ]
    );
}

#[test]
fn a_requirement_cycle_is_one_error_and_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", b"---\nrequires: [b.md]\n---\n");
    write(&team, "b.md", b"---\nrequires: [c.md]\n---\n");
    write(&team, "c.md", b"---\nrequires: [a.md]\n---\n");

    let manifest = resolve_codex(home, &["a.md"]);
    assert_eq!(
        codes(&manifest),
        vec![(Some("a.md".into()), ManifestErrorCode::DependencyCycle)]
    );
    assert!(manifest.errors[0].message.contains("a.md, b.md, c.md"));
    assert_eq!(manifest.entries.len(), 3);
}

#[test]
fn malformed_requires_are_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "scalar.md", b"---\nrequires: b.md\n---\n");
    write(&team, "escape.md", b"---\nrequires: [../b.md]\n---\n");
    write(&team, "glob.md", b"---\nrequires: [docs/*.md]\n---\n");
    write(&team, "broken.md", b"---\nrequires: [unclosed\n---\n");
    // Unrelated frontmatter, even unreadable, declares nothing.
    write(&team, "plain.md", b"---\ntitle: [oops\n---\nbody\n");
    write(&team, "no-frontmatter.md", b"requires: [b.md]\n");

    let manifest = resolve_codex(
        home,
        &[
            "scalar.md",
            "escape.md",
            "glob.md",
            "broken.md",
            "plain.md",
            "no-frontmatter.md",
        ],
    );
    assert_eq!(
        codes(&manifest),
        vec![
            (Some("scalar.md".into()), ManifestErrorCode::Invalid),
            (Some("escape.md".into()), ManifestErrorCode::Invalid),
            (Some("glob.md".into()), ManifestErrorCode::Invalid),
            (Some("broken.md".into()), ManifestErrorCode::Invalid),
        ]
    );
}

// ---------------------------------------------------------------------------
// Essentials, identity
// ---------------------------------------------------------------------------

/// SW-AN-49: the notify hook ships with CodeSense's own install
/// ([`codesense_pty_core::notify_hook`]), so a Claude agent on a team with no
/// `.claude/` in content resolves without errors, and a team's hooks resolve
/// as ordinary content.
#[test]
fn claude_resolve_no_longer_requires_the_notify_hook_in_content() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "CLAUDE.md", b"c");

    let manifest = resolve(home, &agent(AgentCli::Claude, &["CLAUDE.md"]));
    assert_eq!(manifest.errors, Vec::new(), "no error on missing hook");
    assert!(
        !manifest
            .entries
            .iter()
            .any(|e| e.path.as_str().starts_with(".claude/hooks")),
        "no hook entry is required"
    );

    // A declared `.claude/hooks/` directory still resolves the team's content
    // normally; the manifest never adds its own notify-hook entry on top.
    write(&team, ".claude/hooks/team-hook.sh", b"#!/bin/sh\n");
    let manifest = resolve(home, &agent(AgentCli::Claude, &[".claude/hooks/"]));
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(manifest.entries[0].origin, EntryOrigin::Declared);
    assert_eq!(
        manifest.totals,
        ManifestTotals {
            files: 1,
            bytes: 10
        }
    );
}

/// Unchanged content resolves to byte-identical JSON; one changed byte in one
/// skill file changes that file's hash, its entry's hash and the manifest
/// hash, and nothing else.
#[test]
fn resolution_is_deterministic_and_a_byte_moves_only_its_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, ".claude/skills/x/SKILL.md", b"skill");
    write(&team, ".claude/skills/x/step.md", b"step");
    write(&team, "CLAUDE.md", b"root");
    let def = agent(AgentCli::Codex, &[".claude/skills/x", "CLAUDE.md"]);

    let first = resolve(home, &def);
    let second = resolve(home, &def);
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );

    write(&team, ".claude/skills/x/step.md", b"stEp");
    let changed = resolve(home, &def);
    assert_ne!(changed.manifest_hash, first.manifest_hash);
    let mut expected = first.clone();
    expected.entries[0].files[1].hash = content_hash(b"stEp");
    expected.entries[0].entry_hash = entry_hash(&expected.entries[0].files);
    expected.manifest_hash = changed.manifest_hash.clone();
    assert_eq!(changed, expected);
    assert_ne!(changed.entries[0].entry_hash, first.entries[0].entry_hash);
    assert_eq!(changed.entries[1], first.entries[1]);
}

/// Two declared entries that require each other are a loop just as when one
/// pulls the other in; a file requiring a sibling in its own entry is not.
#[test]
fn a_requirement_cycle_is_found_whether_its_entries_are_declared_or_pulled_in() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", b"---\nrequires: [b.md]\n---\n");
    write(&team, "b.md", b"---\nrequires: [a.md]\n---\n");
    write(&team, "kit/one.md", b"---\nrequires: [kit/two.md]\n---\n");
    write(&team, "kit/two.md", b"two");

    for startup in [&["a.md"][..], &["a.md", "b.md"][..], &["b.md", "a.md"][..]] {
        let manifest = resolve_codex(home, startup);
        let codes: Vec<_> = manifest.errors.iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            vec![ManifestErrorCode::DependencyCycle],
            "{startup:?}"
        );
    }
    let manifest = resolve_codex(home, &["kit"]);
    assert_eq!(manifest.errors, Vec::new());
}

/// A directory carries what projection would: OS and editor cruft, build and
/// VCS subtrees and projection-excluded files inside it are left out rather
/// than failing the entry.
#[test]
fn a_directory_leaves_out_cruft_and_excluded_files() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "kit/SKILL.md", b"skill");
    write(&team, "kit/.DS_Store", b"finder");
    write(&team, "kit/node_modules/dep/index.js", b"js");
    write(&team, "kit/.git/HEAD", b"ref");
    write(&team, "kit/.env", b"TOKEN=1");

    let reader = CountingReader::default();
    let manifest = resolve_manifest_with(
        &reader,
        home,
        &me(),
        &agent(AgentCli::Codex, &["kit"]),
        &reference(),
        target(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(manifest.errors, Vec::new());
    let logicals: Vec<_> = manifest.entries[0]
        .files
        .iter()
        .map(|f| f.logical.as_str())
        .collect();
    assert_eq!(logicals, vec!["kit/SKILL.md"]);
    let pruned = team.join("kit/node_modules");
    assert!(
        !reader.touched().iter().any(|p| p.starts_with(&pruned)),
        "an ignored subtree is pruned, not walked"
    );
}

/// A requirement whose files an earlier entry already carries is neither
/// re-read nor counted toward the size limit.
#[test]
fn a_covered_requirement_is_not_read_again() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/a.md", b"---\nrequires: [docs/b.md]\n---\n");
    write(&team, "docs/b.md", b"b");

    let reader = CountingReader::default();
    let manifest = resolve_manifest_with(
        &reader,
        home,
        &me(),
        &agent(AgentCli::Codex, &["docs"]),
        &reference(),
        target(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.entries.len(), 1);
    let mut opened = reader.opened();
    opened.sort();
    assert_eq!(opened, vec![team.join("docs/a.md"), team.join("docs/b.md")]);
}

/// Every loop is reported, one error per group of entries that require each
/// other, however the depth-first walk happens to meet them.
#[test]
fn each_requirement_loop_group_is_reported_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", b"---\nrequires: [b.md, c.md]\n---\n");
    write(&team, "b.md", b"---\nrequires: [a.md]\n---\n");
    write(&team, "c.md", b"---\nrequires: [b.md, a.md]\n---\n");
    write(&team, "x.md", b"---\nrequires: [y.md]\n---\n");
    write(&team, "y.md", b"---\nrequires: [x.md]\n---\n");

    let manifest = resolve_codex(home, &["a.md", "x.md"]);
    assert_eq!(
        codes(&manifest),
        vec![
            (Some("a.md".into()), ManifestErrorCode::DependencyCycle),
            (Some("x.md".into()), ManifestErrorCode::DependencyCycle),
        ]
    );
    assert!(manifest.errors[0].message.contains("a.md, b.md, c.md"));
}

/// A long requirement chain resolves without deep recursion.
#[test]
fn a_long_requirement_chain_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    let n = 3000;
    for i in 0..n {
        let body = if i + 1 < n {
            format!("---\nrequires: [chain/f{}.md]\n---\n", i + 1)
        } else {
            "end".to_string()
        };
        write(&team, &format!("chain/f{i}.md"), body.as_bytes());
    }
    let manifest = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn({
            let home = home.to_path_buf();
            move || resolve_codex(&home, &["chain/f0.md"])
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(manifest.errors, Vec::new());
    assert_eq!(manifest.entries.len(), n);
}

/// A personal copy of a directory shadows the team's whole directory, but a
/// file the personal copy lacks can still be required from the team: it
/// becomes its own entry, from the team layer, and nothing the personal copy
/// carries is replaced.
#[test]
fn a_partial_personal_override_can_require_the_rest_from_the_team() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    write(
        &personal,
        "kit/SKILL.md",
        b"---\nrequires: [kit/extra.md, kit/SKILL.md]\n---\n",
    );
    let team = team_content(home, TEAM);
    write(&team, "kit/SKILL.md", b"team skill");
    write(&team, "kit/extra.md", b"team only");

    let manifest = resolve_codex(home, &["kit"]);
    assert_eq!(manifest.errors, Vec::new());
    let entries: Vec<_> = manifest
        .entries
        .iter()
        .map(|e| (e.path.to_string(), e.layer.clone(), e.origin.clone()))
        .collect();
    // SLW-PT-20: the folder merges per file, as the worktree does — the scratch SKILL.md
    // and the team's extra.md both arrive in the one entry.
    assert_eq!(
        entries,
        vec![("kit".into(), personal_layer(), EntryOrigin::Declared)]
    );
    let kit = entry(&manifest, "kit");
    let files: Vec<_> = kit
        .files
        .iter()
        .map(|f| (f.logical.clone(), f.layer_in(&kit.layer).clone()))
        .collect();
    assert_eq!(
        files,
        vec![
            ("kit/SKILL.md".into(), personal_layer()),
            ("kit/extra.md".into(), team_layer(TEAM)),
        ]
    );
}

/// A file the entry's own layer holds but excludes from projection is not
/// fetched from another layer instead.
#[test]
fn a_requirement_excluded_in_its_entrys_layer_is_not_taken_from_another() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(
        &team,
        "kit/SKILL.md",
        b"---\nrequires: [kit/private.md]\n---\n",
    );
    write(&team, "kit/private.md", b"team private");
    write(&team_content(home, PARENT), "kit/private.md", b"parent");
    crate::install::projection_excludes::write_patterns_to(
        &path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), TEAM),
        vec!["kit/private.md".into()],
    )
    .unwrap();

    let manifest = resolve_codex(home, &["kit"]);
    assert_eq!(
        codes(&manifest),
        vec![(
            Some("kit/private.md".into()),
            ManifestErrorCode::EscapesContent
        )]
    );
    assert_eq!(manifest.entries.len(), 1);
}

/// A Claude agent that starts with its own personal hooks directory still
/// gets the team's hook when the personal directory lacks it.
#[test]
fn a_personal_hooks_directory_still_gets_the_teams_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(
        &personal_content(home),
        ".claude/hooks/mine.sh",
        b"#!/bin/sh\n",
    );
    write(
        &team_content(home, TEAM),
        ".claude/hooks/team-hook.sh",
        b"#!/bin/sh\n",
    );

    let manifest = resolve(home, &agent(AgentCli::Claude, &[".claude/hooks"]));
    assert_eq!(manifest.errors, Vec::new());
    // SLW-PT-20: the hooks folder merges per file — mine plus the team's hook.
    let hooks = entry(&manifest, ".claude/hooks");
    assert_eq!(hooks.layer, personal_layer());
    let notify = hooks
        .files
        .iter()
        .find(|f| f.logical == ".claude/hooks/team-hook.sh")
        .expect("the team's hook");
    assert_eq!(notify.layer_in(&hooks.layer), &team_layer(TEAM));
}

/// Cruft and VCS data are never carried, even when named directly or
/// required.
#[test]
fn vcs_data_and_cruft_are_never_carried_even_when_named() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "vendor/lib/.git/hooks/post-checkout", b"#!/bin/sh\n");
    write(&team, "vendor/lib/.hg/store/data", b"hg");
    write(
        &team,
        "vendor/lib/README.md",
        b"---\nrequires: [vendor/lib/.git/hooks/post-checkout]\n---\n",
    );

    let manifest = resolve_codex(home, &["vendor"]);
    assert_eq!(
        codes(&manifest),
        vec![(
            Some("vendor/lib/.git/hooks/post-checkout".into()),
            ManifestErrorCode::EscapesContent
        )]
    );
    let logicals: Vec<_> = manifest.entries[0]
        .files
        .iter()
        .map(|f| f.logical.as_str())
        .collect();
    assert_eq!(logicals, vec!["vendor/lib/README.md"]);
}

/// A requirement directory and a file inside it may come from different
/// layers, in either order: each entry keeps its own layer, every file has
/// one owner, and the order of the requirements does not change the files.
#[test]
fn a_directory_and_a_file_inside_it_keep_their_own_layers_in_either_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    write(&personal, "docs/guide.md", b"mine");
    let team = team_content(home, TEAM);
    write(&team, "docs/guide.md", b"team");
    write(&team, "docs/x.md", b"x");
    write(&team, "a.md", b"---\nrequires: [docs, docs/x.md]\n---\n");
    write(&team, "b.md", b"---\nrequires: [docs/x.md, docs]\n---\n");

    for declared in ["a.md", "b.md"] {
        let manifest = resolve_codex(home, &[declared]);
        assert_eq!(manifest.errors, Vec::new(), "{declared}");
        let mut files: Vec<_> = manifest
            .entries
            .iter()
            .filter(|e| e.path.as_str().starts_with("docs"))
            .flat_map(|e| {
                e.files.iter().map(|f| {
                    (
                        f.logical.clone(),
                        f.layer_in(&e.layer).clone(),
                        f.hash.clone(),
                    )
                })
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            files,
            vec![
                (
                    "docs/guide.md".into(),
                    personal_layer(),
                    content_hash(b"mine")
                ),
                ("docs/x.md".into(), team_layer(TEAM), content_hash(b"x")),
            ],
            "{declared}"
        );
    }
}

/// Destinations that a case-insensitive workspace would merge, or that put
/// one file beneath another, are refused, across entries and inside one.
/// (Each spelling lives in its own layer so both resolve on any file system.)
#[test]
fn destinations_that_differ_in_case_or_nest_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let personal = personal_content(home);
    let team = team_content(home, TEAM);
    write(&personal, "Guide.md", b"mine");
    write(&team, "guide.md", b"team");
    write(&personal, "notes", b"a file");
    write(&team, "Notes/inner.md", b"beneath a case twin");

    let manifest = resolve_codex(home, &["Guide.md", "guide.md"]);
    assert_eq!(
        codes(&manifest),
        vec![(Some("guide.md".into()), ManifestErrorCode::Invalid)]
    );
    assert!(manifest.errors[0].message.contains("differ only in case"));
    assert_eq!(manifest.entries.len(), 1);

    let manifest = resolve_codex(home, &["notes", "Notes/inner.md"]);
    assert_eq!(
        codes(&manifest),
        vec![(Some("Notes/inner.md".into()), ManifestErrorCode::Invalid)]
    );
    assert!(manifest.errors[0].message.contains("lies inside"));
}

/// A file whose name is not a content path (a glob character, a backslash)
/// fails its directory entry here, rather than the whole manifest at
/// bootstrap.
#[cfg(unix)]
#[test]
fn a_directory_file_that_is_not_a_content_path_is_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "kit/ok.md", b"ok");
    write(&team, "kit/draft[1].md", b"bracketed");
    write(&team, "other/a\\b.md", b"backslash");

    let manifest = resolve_codex(home, &["kit", "other"]);
    assert_eq!(
        codes(&manifest),
        vec![
            (Some("kit".into()), ManifestErrorCode::Invalid),
            (Some("other".into()), ManifestErrorCode::Invalid),
        ]
    );
    assert!(manifest.errors[0].message.contains("kit/draft[1].md"));
    assert!(manifest.entries.is_empty());
}

/// A content layer reached through a symlink below the CodeSense home is
/// refused, as bootstrap refuses it, and is never read.
#[cfg(unix)]
#[test]
fn a_symlinked_layer_root_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let elsewhere = tmp.path().join("elsewhere");
    write(&elsewhere, "CLAUDE.md", b"elsewhere");
    let team = team_content(home, TEAM);
    std::fs::remove_dir(&team).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &team).unwrap();
    write(&team_content(home, PARENT), "CLAUDE.md", b"parent");

    let manifest = resolve_codex(home, &["CLAUDE.md"]);
    assert_eq!(manifest.errors.len(), 1, "{:?}", manifest.errors);
    assert_eq!(manifest.errors[0].code, ManifestErrorCode::EscapesContent);
    assert_eq!(manifest.errors[0].path, None);
    assert_eq!(entry(&manifest, "CLAUDE.md").layer, team_layer(PARENT));
}

/// A path counts as present only as spelled on disk, so a different case
/// cannot slip past the layer's excludes on a case-insensitive file system.
#[test]
fn a_path_resolves_only_as_spelled_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "secrets/token.md", b"secret");
    write(&team, "Docs/Guide.md", b"guide");
    crate::install::projection_excludes::write_patterns_to(
        &path_to_team_config_dir(&path_to_org_root(home, &me(), ORG).unwrap(), TEAM),
        vec!["secrets/**".into()],
    )
    .unwrap();

    let manifest = resolve_codex(
        home,
        &["Secrets/token.md", "docs/guide.md", "Docs/Guide.md"],
    );
    assert_eq!(
        codes(&manifest),
        vec![
            (Some("Secrets/token.md".into()), ManifestErrorCode::Missing),
            (Some("docs/guide.md".into()), ManifestErrorCode::Missing),
        ]
    );
    assert_eq!(entry(&manifest, "Docs/Guide.md").files.len(), 1);
}

/// SW-CR-8 AC-5: a startup path missing from every content layer BUT present
/// under the retired `{team}/install/` tree is reported with a message that
/// names the install location and the heal action — not the bare "in no
/// content layer" that stops the user from acting.
#[test]
fn missing_path_under_retired_install_is_self_describing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org_root = write_org(home);
    let install = crate::context_repo::path_to_team_install_dir(&org_root, TEAM);
    std::fs::create_dir_all(install.join(".claude/hooks")).unwrap();
    std::fs::write(install.join(".claude/hooks/x.sh"), b"#!/bin/sh\n").unwrap();

    let manifest = resolve_codex(home, &[".claude/hooks/x.sh"]);
    let error = manifest
        .errors
        .iter()
        .find(|e| e.code == ManifestErrorCode::Missing)
        .expect("missing error");
    assert!(
        error.message.contains("retired install/ layer"),
        "message names the retired install layer: {}",
        error.message
    );
    assert!(
        error.message.contains("heal action"),
        "message names the heal action: {}",
        error.message
    );
    assert!(
        error.message.contains("install"),
        "message names the install dir: {}",
        error.message
    );
}

/// SW-CR-8 R2 NIT 4: a startup path shaped as a directory (e.g. an agent
/// declares `.claude/hooks/` whole) is also self-described when the retired
/// install twin carries it as a directory, not just when it carries a file.
#[test]
fn missing_dir_shape_path_under_retired_install_is_self_describing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org_root = write_org(home);
    let install = crate::context_repo::path_to_team_install_dir(&org_root, TEAM);
    std::fs::create_dir_all(install.join(".claude/hooks")).unwrap();
    // Dir-shape startup path: the whole `.claude/hooks/` lives under install,
    // nothing under content. The resolver must still name install/.
    std::fs::write(install.join(".claude/hooks/x.sh"), b"#!/bin/sh\n").unwrap();

    let manifest = resolve_codex(home, &[".claude/hooks/"]);
    let error = manifest
        .errors
        .iter()
        .find(|e| e.code == ManifestErrorCode::Missing)
        .expect("missing error");
    assert!(
        error.message.contains("retired install/ layer"),
        "dir-shape path still self-describes: {}",
        error.message
    );
}

/// A missing path NOT under install/ keeps the plain `"is in no content
/// layer"` message — the self-describing branch only fires when the retired
/// install twin actually has the file.
#[test]
fn missing_path_not_under_install_keeps_plain_message() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);

    let manifest = resolve_codex(home, &["docs/absent.md"]);
    let error = manifest
        .errors
        .iter()
        .find(|e| e.code == ManifestErrorCode::Missing)
        .expect("missing error");
    assert!(
        error.message.contains("is in no content layer"),
        "plain message: {}",
        error.message
    );
    assert!(
        !error.message.contains("retired install"),
        "no install mention: {}",
        error.message
    );
}

/// A refused requirement carries no edge, so it cannot make a requirement
/// loop out of the entry it would have lain inside.
#[test]
fn a_refused_requirement_makes_no_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", b"---\nrequires: [skills/x.md]\n---\n");
    write(&team, "skills/SKILL.md", b"---\nrequires: [a.md]\n---\n");

    let manifest = resolve_codex(home, &["a.md", "skills"]);
    assert_eq!(
        codes(&manifest),
        vec![(Some("skills/x.md".into()), ManifestErrorCode::Missing)]
    );
}

#[cfg(unix)]
#[test]
fn external_attached_layer_rejects_an_intermediate_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("state");
    let old = write_org(&home);
    let checkout = tmp.path().join("checkout");
    std::fs::rename(old, &checkout).unwrap();
    git2::Repository::init(&checkout).unwrap();
    crate::context_repo::context_attachments::attach(&home, &me(), &checkout).unwrap();
    let outside = tmp.path().join("outside-team");
    write(&outside.join("content"), "CLAUDE.md", b"outside-secret");
    let team = path_to_team_content_dir(&checkout, TEAM);
    std::fs::remove_dir_all(team.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, team.parent().unwrap()).unwrap();
    write(&team_content(&home, PARENT), "CLAUDE.md", b"parent");
    let manifest = resolve_codex(&home, &["CLAUDE.md"]);
    assert!(manifest
        .errors
        .iter()
        .any(|error| error.code == ManifestErrorCode::EscapesContent));
    assert_eq!(entry(&manifest, "CLAUDE.md").layer, team_layer(PARENT));
}

#[test]
fn inherited_startup_and_context_tiers_do_not_carry_private_files() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::remove_file(root.join("child/content/secret.md")).unwrap();
    std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    let agent = agent(AgentCli::Claude, &["shared.md", "secret.md"]);
    let manifest = resolve_manifest(
        home.path(),
        &me(),
        &agent,
        &reference(),
        target.clone(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(
        manifest.entries[0].layer,
        ContentLayer::Team {
            org: "acme/ctx".into(),
            team: "parent".into()
        }
    );
    assert_eq!(
        manifest.entries[0].files[0].hash,
        content_hash(b"parent-shared.md-bytes")
    );
    assert_eq!(manifest.errors[0].code, ManifestErrorCode::Missing);
    let resolution = resolve_context(
        home.path(),
        &me(),
        ContextRequest {
            agent: &agent,
            target: &target,
            repo_override: None,
            team_workspace: false,
        },
    );
    assert!(resolution.files.iter().all(|f| f.logical != "secret.md"));
    assert!(resolution
        .files
        .iter()
        .any(|f| f.source == root.join("parent/content/shared.md")));
}

#[test]
fn inherited_merge_and_catalog_use_only_admitted_files() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::write(
        root.join("parent/config/merge-marked.yaml"),
        "marks:\n  .mcp.json: {}\n",
    )
    .unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    assert!(
        merged_workspace_file(home.path(), &me(), &target, ".mcp.json").is_err(),
        "an unshared parent file cannot hand down its merge mark"
    );
    std::fs::write(
        root.join("child/config/merge-marked.yaml"),
        "marks:\n  .mcp.json: {}\n",
    )
    .or_else(|_| {
        std::fs::create_dir_all(root.join("child/config"))?;
        std::fs::write(
            root.join("child/config/merge-marked.yaml"),
            "marks:\n  .mcp.json: {}\n",
        )
    })
    .unwrap();
    std::fs::remove_file(root.join("child/content/secret.md")).unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    let merged = merged_workspace_file(home.path(), &me(), &target, ".mcp.json").unwrap();
    assert!(merged.content["mcpServers"].get("child-srv").is_some());
    assert!(merged.content["mcpServers"].get("parent-srv").is_none());
    assert!(merged.content["mcpServers"].get("grand-srv").is_none());
    let catalog = crate::commands::agents::catalog_in(
        &FsContentReader,
        &content_layers(home.path(), &me(), &target),
        None,
        crate::commands::agents::CatalogPage::default(),
    )
    .unwrap();
    assert!(catalog.entries.iter().any(|f| f.path == "shared.md"));
    assert!(catalog.entries.iter().all(|f| f.path != "secret.md"));
}

#[test]
fn inherited_context_suggestion_never_names_unshared_paths() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::write(root.join("parent/config/projections.yml"), "projections:\n  - key: private\n    source_subdir: secret.md\n  - key: shared\n    source_subdir: shared.md\n").unwrap();
    let key = AgentKey::new("acme/ctx", "child", "builder");
    let agent = agent(AgentCli::Claude, &[]);
    let suggestion =
        crate::agents::context_suggestion::suggestion_for(home.path(), &me(), &key, &agent)
            .unwrap();
    assert!(suggestion.entries.iter().any(|e| e.path == "shared.md"));
    assert!(!format!("{suggestion:?}").contains("secret.md"));
}

#[cfg(unix)]
#[test]
fn startup_alias_matrix_remains_stricter_than_central_read_admission() {
    let fixture = crate::context_repo::inheritance::alias_fixture();
    let view = crate::context_repo::inheritance::team_view(&fixture.root, "acme/ctx", "child");
    for case in &fixture.cases {
        assert_eq!(
            view.read_path(&case.content).is_ok(),
            case.admitted,
            "{}",
            case.id
        );
        let logical = format!("{}.md", case.id);
        let request = agent(AgentCli::Codex, &[logical.as_str()]);
        let manifest = resolve_manifest(
            fixture.home.path(),
            &me(),
            &request,
            &reference(),
            ManifestTarget::team("acme/ctx", "child"),
            Vec::new(),
            Vec::new(),
        );
        assert!(manifest.entries.is_empty(), "{}", case.id);
        assert!(!manifest.errors.is_empty(), "{}", case.id);
    }
}
