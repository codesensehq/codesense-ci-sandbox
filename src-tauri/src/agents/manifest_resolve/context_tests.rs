// SPDX-License-Identifier: MIT
//! SW-XS-13: the context resolver over real temp homes, a real org with a
//! parent team, a per-repo tier, and the real agent store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::context_repo::{path_to_org_root, path_to_team_content_dir};
use crate::domain::{Principal, SourceRepoSlug};
use crate::routines::model::Author;

use super::super::super::context::{ContextEntry, RepoOverride};
use super::super::super::keys::AgentKey;
use super::super::super::model::{Agent, LaunchMode, McpAccess};
use super::super::super::service::{self, Placement, SaveTarget};
use super::super::{resolve_manifest, ManifestTarget};
use super::*;
use codesense_pty_core::agent_cli::AgentCli;

const ORG: &str = "acme/api";
const TEAM: &str = "backend";
const PARENT: &str = "platform";

fn me() -> Principal {
    Principal::from(1u64)
}

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
        "share: ['content/**']\n",
    )
    .unwrap();
    org_root
}

fn team_content(home: &Path, team: &str) -> PathBuf {
    path_to_team_content_dir(&path_to_org_root(home, &me(), ORG).unwrap(), team)
}

fn codesense() -> SourceRepoSlug {
    SourceRepoSlug::new("alderco", "codesense").unwrap()
}

fn other_repo() -> SourceRepoSlug {
    SourceRepoSlug::new("alderco", "website").unwrap()
}

/// `{team}/repos/{host}/{owner}/{repo}/content/` for `slug`.
fn repo_content(home: &Path, slug: &SourceRepoSlug) -> PathBuf {
    let org_root = path_to_org_root(home, &me(), ORG).unwrap();
    slug.physical_path_key()
        .unwrap()
        .append_to(&org_root.join(TEAM).join("repos"))
        .join("content")
}

fn write(root: &Path, logical: &str, body: &str) {
    let path = root.join(logical);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn agent(runtime: AgentCli, startup: &[&str], context: Option<Vec<ContextEntry>>) -> Agent {
    Agent {
        role: None,
        id: "barb".into(),
        name: "Barb".into(),
        description: String::new(),
        created_by: "1".into(),
        runtime,
        args: Vec::new(),
        launch: LaunchMode::Foreground,
        mcp: McpAccess::default(),
        startup: startup.iter().map(|s| s.to_string()).collect(),
        context,
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

fn target(repo: Option<SourceRepoSlug>) -> ManifestTarget {
    ManifestTarget {
        context_root: None,
        org: ORG.into(),
        team: TEAM.into(),
        repo,
    }
}

fn preview(
    home: &Path,
    agent: &Agent,
    repo: Option<SourceRepoSlug>,
    repo_override: Option<&RepoOverride>,
) -> ContextResolution {
    resolve_context(
        home,
        &me(),
        ContextRequest {
            agent,
            target: &target(repo),
            repo_override,
            team_workspace: false,
        },
    )
}

fn destinations(r: &ContextResolution) -> BTreeSet<String> {
    r.files.iter().map(|f| f.destination.clone()).collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The team's content as this team actually has it: agent dirs, plans,
/// browser captures and the junk that used to be projected with them.
fn write_team_like_this_one(home: &Path) {
    let team = team_content(home, TEAM);
    for logical in [
        "CLAUDE.md",
        ".claude/commands/bmad-help.md",
        ".claude/hooks/team-hook.sh",
        ".claude/agents/reviewer.md",
        ".claude/skills/bmad-build/SKILL.md",
        // Junk under the default prefixes.
        ".claude/skills/bmad-build/wvbuild-lock",
        ".claude/skills/bmad-build/wv-ci-gate",
        ".claude/commands/wv-ci-gate",
        // Junk beside them.
        ".claude/settings.local.json",
        ".claude/scheduled_tasks.lock",
        "weft.sqlite3",
        "weft.sqlite3-wal",
        // Never in a default.
        "_bmad-output/08/plan.md",
        ".playwright-mcp/page.yml",
        ".opencode/x.md",
        "_lab/x.md",
        "docs/a.md",
    ] {
        write(&team, logical, "x");
    }
}

/// AC1: a claude agent with no `context`, previewed for codesense, wants the
/// root doc and the four `.claude` sets, and nothing else.
#[test]
fn ac1_no_context_resolves_to_the_claude_default_without_plans_or_junk() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write_team_like_this_one(home);
    let barb = agent(AgentCli::Claude, &[], None);
    let got = preview(home, &barb, Some(codesense()), None);
    assert!(got.defaulted);
    assert_eq!(got.repo.as_deref(), Some("github.com/alderco/codesense"));
    assert_eq!(
        destinations(&got),
        set(&[
            "CLAUDE.md",
            ".claude/commands/bmad-help.md",
            ".claude/hooks/team-hook.sh",
            ".claude/agents/reviewer.md",
            ".claude/skills/bmad-build/SKILL.md",
        ])
    );
    assert_eq!(got.totals.files, 5);
    assert_eq!(got.totals.bytes, 5);
    assert!(got
        .entries
        .iter()
        .all(|e| e.source == ContextEntrySource::Default));
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
    // Resolving never writes the default back.
    assert_eq!(barb.context, None);
}

/// SW-XS-5 (SPEC §11.5): the default want set does not depend on
/// designations. A team `designations.yml` that marks `.claude` inherit-only and
/// the plans and docs as context (team-wide and for this repo) changes nothing:
/// the agent with no `context` still gets exactly the claude default.
#[test]
fn xs5_the_default_want_set_ignores_designations() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write_team_like_this_one(home);
    write(
        &team_content(home, TEAM),
        "designations.yml",
        "context:\n  - _bmad-output\n  - docs\ninstall:\n  - .claude\nby-source:\n  alderco/codesense:\n    context:\n      - _lab\n    install:\n      - .claude\npin:\n  - docs/a.md\n",
    );
    let barb = agent(AgentCli::Claude, &[], None);
    let got = preview(home, &barb, Some(codesense()), None);
    assert!(got.defaulted);
    assert_eq!(
        destinations(&got),
        set(&[
            "CLAUDE.md",
            ".claude/commands/bmad-help.md",
            ".claude/hooks/team-hook.sh",
            ".claude/agents/reviewer.md",
            ".claude/skills/bmad-build/SKILL.md",
        ])
    );
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
}

/// AC2: a `dest` remaps the prefix.
#[test]
fn ac2_dest_remaps_files_under_the_new_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/sub/b.md", "b");
    write(&team, "_bmad-output/y.md", "no");
    let a = agent(
        AgentCli::Claude,
        &[],
        Some(vec![ContextEntry::with_dest("_bmad-output/x", "plans")]),
    );
    let got = preview(home, &a, None, None);
    assert!(!got.defaulted);
    assert_eq!(destinations(&got), set(&["plans/a.md", "plans/sub/b.md"]));
    let file = got
        .files
        .iter()
        .find(|f| f.destination == "plans/a.md")
        .unwrap();
    assert_eq!(file.logical, "_bmad-output/x/a.md");
    assert_eq!(file.bytes, 2);
    assert_eq!(got.entries[0].files, 2);
    assert_eq!(got.entries[0].bytes, 3);
}

/// AC3: a repo override adds and removes for its repo only.
#[test]
fn ac3_a_repo_override_applies_to_its_repo_and_no_other() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    for logical in [
        "CLAUDE.md",
        ".claude/skills/legacy-a/SKILL.md",
        ".claude/skills/legacy-b/SKILL.md",
        ".claude/skills/current/SKILL.md",
        "docs/architecture/spine.md",
        "docs/other.md",
    ] {
        write(&team, logical, "x");
    }
    let saved = service::save(
        home,
        &me(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        agent(AgentCli::Claude, &[], None),
        &Author::user("1"),
    )
    .unwrap();
    // Overrides are part of the revision: written by an update on a base.
    let saved = service::save_with_overrides(
        home,
        &me(),
        SaveTarget::Update {
            key: saved.key.clone(),
            base_revision: 1,
        },
        saved.agent.clone(),
        &[(
            codesense(),
            RepoOverride {
                add: vec![ContextEntry::new("docs/architecture")],
                remove: vec![".claude/skills/legacy-*".into()],
                ..RepoOverride::default()
            },
        )],
        &Author::user("1"),
    )
    .unwrap();
    let for_repo = |slug: SourceRepoSlug| {
        service::resolve_agent_context(home, &me(), &saved.key, &target(Some(slug)), false).unwrap()
    };

    let cs = for_repo(codesense());
    assert_eq!(
        destinations(&cs),
        set(&[
            "CLAUDE.md",
            ".claude/skills/current/SKILL.md",
            "docs/architecture/spine.md",
        ])
    );
    assert!(cs
        .entries
        .iter()
        .any(|e| e.path == "docs/architecture" && e.source == ContextEntrySource::RepoAdd));

    let web = for_repo(other_repo());
    assert_eq!(
        destinations(&web),
        set(&[
            "CLAUDE.md",
            ".claude/skills/current/SKILL.md",
            ".claude/skills/legacy-a/SKILL.md",
            ".claude/skills/legacy-b/SKILL.md",
        ])
    );
    // The override is a file beside the definition, keyed host/owner/repo.
    let dir = crate::agents::roots::team_agents_dir(home, &me(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join("barb");
    assert!(dir
        .join("repos/github.com/alderco/codesense.yaml")
        .is_file());
    assert!(!dir.join("repos/github.com/alderco/website.yaml").exists());
}

/// AC4: the repo tier wins per file over the team, and the team still
/// supplies what the repo tier lacks.
#[test]
fn ac4_the_repo_tier_wins_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/a.md", "team");
    write(&team, "docs/b.md", "team");
    write(&repo_content(home, &codesense()), "docs/a.md", "repo!");
    let a = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("docs")]));

    let got = preview(home, &a, Some(codesense()), None);
    let by: BTreeMap<&str, &ContextFile> = got
        .files
        .iter()
        .map(|f| (f.destination.as_str(), f))
        .collect();
    assert_eq!(
        by["docs/a.md"].tier,
        ContextTier::Repo {
            org: ORG.into(),
            team: TEAM.into(),
            repo: "github.com/alderco/codesense".into(),
        }
    );
    assert_eq!(by["docs/a.md"].bytes, 5);
    assert_eq!(
        by["docs/b.md"].tier,
        ContextTier::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }
    );
    // Without a repo, or for another repo, the team's copy is the source.
    for repo in [None, Some(other_repo())] {
        let got = preview(home, &a, repo, None);
        assert!(got
            .files
            .iter()
            .all(|f| matches!(f.tier, ContextTier::Team { .. })));
    }
}

/// A missing prefix is a warning, never an error; the rest still resolves.
#[test]
fn a_missing_prefix_is_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, TEAM), "docs/a.md", "x");
    let a = agent(
        AgentCli::Claude,
        &[],
        Some(vec![ContextEntry::new("nope"), ContextEntry::new("docs")]),
    );
    let got = preview(home, &a, None, None);
    assert_eq!(destinations(&got), set(&["docs/a.md"]));
    assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
    assert_eq!(got.warnings[0].code, ContextWarningCode::Missing);
    assert_eq!(got.warnings[0].path.as_deref(), Some("nope"));
}

/// The team chain and the parent team fill in, nearest first; startup paths
/// are always part of the want set.
#[test]
fn startup_joins_the_want_set_and_the_chain_resolves_nearest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, PARENT), "docs/a.md", "parent");
    write(&team_content(home, PARENT), "docs/p.md", "parent");
    write(&team_content(home, TEAM), "docs/a.md", "team");
    write(&team_content(home, TEAM), "notes/n.md", "n");
    let a = agent(
        AgentCli::Claude,
        &["notes/n.md"],
        Some(vec![ContextEntry::new("docs")]),
    );
    let got = preview(home, &a, None, None);
    assert_eq!(
        destinations(&got),
        set(&["docs/a.md", "docs/p.md", "notes/n.md"])
    );
    let tier = |d: &str| {
        got.files
            .iter()
            .find(|f| f.destination == d)
            .unwrap()
            .tier
            .clone()
    };
    assert_eq!(
        tier("docs/a.md"),
        ContextTier::Team {
            org: ORG.into(),
            team: TEAM.into()
        }
    );
    assert_eq!(
        tier("docs/p.md"),
        ContextTier::Team {
            org: ORG.into(),
            team: PARENT.into()
        }
    );
    assert!(got
        .entries
        .iter()
        .any(|e| e.path == "notes/n.md" && e.source == ContextEntrySource::Startup));
}

/// The team's `projection-excludes.yml` filters context too.
#[test]
fn projection_excludes_filter_the_want_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org_root = write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/a.md", "x");
    write(&team, "docs/secret.md", "x");
    write(&team, "docs/.env", "x");
    crate::install::projection_excludes::write_patterns_to(
        &org_root.join(TEAM).join("config"),
        vec!["docs/secret.md".into()],
    )
    .unwrap();
    let a = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("docs")]));
    let got = preview(home, &a, None, None);
    assert_eq!(destinations(&got), set(&["docs/a.md"]));
}

/// The codex and team-workspace defaults.
#[test]
fn codex_and_team_workspace_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    for logical in [
        "AGENTS.md",
        "CLAUDE.md",
        ".agents/skills/x/SKILL.md",
        ".claude/commands/c.md",
        ".claude/skills/s/SKILL.md",
        ".claude/agents/a.md",
    ] {
        write(&team, logical, "x");
    }
    let codex = agent(AgentCli::Codex, &[], None);
    assert_eq!(
        destinations(&preview(home, &codex, None, None)),
        set(&["AGENTS.md", ".agents/skills/x/SKILL.md"])
    );
    let claude = agent(AgentCli::Claude, &[], None);
    let tw = resolve_context(
        home,
        &me(),
        ContextRequest {
            agent: &claude,
            target: &target(None),
            repo_override: None,
            team_workspace: true,
        },
    );
    assert_eq!(
        destinations(&tw),
        set(&[
            "CLAUDE.md",
            ".claude/commands/c.md",
            ".claude/skills/s/SKILL.md",
            ".claude/agents/a.md"
        ])
    );
}

/// AC6: `startup` keeps its manifest and error semantics; `context` and a
/// repo on the target never change the startup manifest.
#[test]
fn ac6_startup_manifest_is_unchanged_by_context_and_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "CLAUDE.md", "x");
    write(&team, ".claude/skills/review/SKILL.md", "x");
    write(&team, ".claude/hooks/team-hook.sh", "x");
    write(&repo_content(home, &codesense()), "CLAUDE.md", "repo");
    let reference = crate::agents::manifest::AgentRevisionRef {
        key: AgentKey::new(ORG, TEAM, "barb"),
        revision: 1,
        definition_hash: "h".into(),
    };
    let run = |a: &Agent, repo: Option<SourceRepoSlug>| {
        resolve_manifest(
            home,
            &me(),
            a,
            &reference,
            target(repo),
            Vec::new(),
            Vec::new(),
        )
    };
    let plain = agent(
        AgentCli::Claude,
        &["CLAUDE.md", ".claude/skills/review", "missing.md"],
        None,
    );
    let base = run(&plain, None);
    let paths: Vec<String> = base.entries.iter().map(|e| e.path.to_string()).collect();
    // The team's hook file is not a declared startup path, so it is not an entry.
    assert_eq!(paths, ["CLAUDE.md", ".claude/skills/review"]);
    assert!(base
        .entries
        .iter()
        .all(|e| matches!(e.layer, crate::agents::manifest::ContentLayer::Team { .. })));
    // A missing startup path is still an error, not a warning.
    assert_eq!(base.errors.len(), 1);
    assert_eq!(
        base.errors[0].code,
        crate::agents::manifest::ManifestErrorCode::Missing
    );

    let with_context = Agent {
        context: Some(vec![ContextEntry::new("docs")]),
        ..plain.clone()
    };
    for (a, repo) in [
        (&plain, Some(codesense())),
        (&with_context, None),
        (&with_context, Some(codesense())),
    ] {
        let got = run(a, repo);
        assert_eq!(got.entries, base.entries);
        assert_eq!(got.errors, base.errors);
    }
}

/// Ordinary lockfiles are content; only the exact junk is dropped.
#[test]
fn lockfiles_are_content_and_only_exact_junk_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    for logical in [
        "app/Cargo.lock",
        "app/pnpm-lock.yaml",
        "app/src/main.rs",
        "app/wvbuild-lock",
        "app/x/wv-ci-gate",
    ] {
        write(&team, logical, "x");
    }
    let a = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("app")]));
    assert_eq!(
        destinations(&preview(home, &a, None, None)),
        set(&["app/Cargo.lock", "app/pnpm-lock.yaml", "app/src/main.rs"])
    );
}

/// H1: a file that would land on a managed, VCS or excluded path is skipped
/// with a warning, even when the stored `dest` predates the save check.
#[test]
fn files_landing_on_unsafe_destinations_are_skipped_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "cfg/settings.json", "{}");
    write(&team, "cfg/settings.local.json", "{}");
    write(&team, "cfg/notes.md", "x");
    write(&team, "hooks/pre-commit", "x");
    // Stored by hand (a save would refuse these dests).
    let a = agent(
        AgentCli::Claude,
        &[],
        Some(vec![
            ContextEntry::with_dest("cfg", ".claude"),
            ContextEntry::with_dest("hooks", "vendor/.git/hooks"),
        ]),
    );
    let got = resolve_context(
        home,
        &me(),
        ContextRequest {
            agent: &a,
            target: &target(None),
            repo_override: None,
            team_workspace: false,
        },
    );
    // The hand-written unsafe dest makes the list invalid as a whole.
    assert!(got
        .warnings
        .iter()
        .any(|w| w.code == ContextWarningCode::Invalid));
    let a = agent(
        AgentCli::Claude,
        &[],
        Some(vec![ContextEntry::with_dest("cfg", ".claude")]),
    );
    let got = preview(home, &a, None, None);
    assert_eq!(destinations(&got), set(&[".claude/notes.md"]));
    let warning = got
        .warnings
        .iter()
        .find(|w| w.code == ContextWarningCode::UnsafeDestination)
        .expect("an unsafe-destination warning");
    assert!(warning.message.contains("2 file(s)"), "{}", warning.message);
}

/// H2 at resolution: different files for one workspace path (any case), or a
/// file where another entry has a directory, are one `collision` warning per
/// pair of entries; the earlier keeps them.
#[test]
fn colliding_files_warn_and_the_earlier_entry_keeps_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/a.md", "docs");
    write(&team, "D/A.md", "other");
    write(&team, "single.md", "file");
    write(&team, "notes/x.md", "x");
    let a = agent(
        AgentCli::Claude,
        &["D/A.md", "single.md"],
        Some(vec![
            ContextEntry::with_dest("docs", "d"),
            ContextEntry::with_dest("notes", "single.md/sub"),
        ]),
    );
    let got = preview(home, &a, None, None);
    let collisions: Vec<&ContextWarning> = got
        .warnings
        .iter()
        .filter(|w| w.code == ContextWarningCode::Collision)
        .collect();
    assert_eq!(collisions.len(), 2, "{:?}", got.warnings);
    assert!(
        collisions[0].message.contains("docs"),
        "{}",
        collisions[0].message
    );
    let by: BTreeMap<&str, &str> = got
        .files
        .iter()
        .map(|f| (f.destination.as_str(), f.logical.as_str()))
        .collect();
    // Startup resolves first, so its `D/A.md` keeps the path.
    assert_eq!(by.get("D/A.md"), Some(&"D/A.md"));
    assert!(!by.contains_key("d/a.md"));
    // The same file reached twice (default + startup) is no collision.
    let b = agent(
        AgentCli::Claude,
        &["docs/a.md"],
        Some(vec![ContextEntry::new("docs")]),
    );
    assert!(preview(home, &b, None, None).warnings.is_empty());
}

/// L3: `remove` never drops a startup path; the size cap lists 500 files
/// while totals count all.
#[test]
fn remove_spares_startup_and_the_listing_caps() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/keep.md", "x");
    write(&team, "docs/pinned.md", "x");
    let a = agent(
        AgentCli::Claude,
        &["docs/pinned.md"],
        Some(vec![ContextEntry::new("docs")]),
    );
    let o = RepoOverride {
        remove: vec!["docs/**".into()],
        ..RepoOverride::default()
    };
    let got = preview(home, &a, Some(codesense()), Some(&o));
    assert_eq!(destinations(&got), set(&["docs/pinned.md"]));

    for i in 0..12 {
        write(&team, &format!("many/f{i:02}.md"), "x");
    }
    let m = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("many")]));
    let capped = preview(home, &m, None, None).capped(5);
    assert!(capped.truncated);
    assert_eq!(capped.files.len(), 5);
    assert_eq!(capped.totals.files, 12);
}

/// L1: names that are not usable content paths are counted in a warning.
#[test]
fn unusable_names_are_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "docs/ok.md", "x");
    write(&team, "docs/what[1].md", "x");
    write(&team, "docs/star*.md", "x");
    let a = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("docs")]));
    let got = preview(home, &a, None, None);
    assert_eq!(destinations(&got), set(&["docs/ok.md"]));
    let skipped = got
        .warnings
        .iter()
        .find(|w| w.code == ContextWarningCode::Skipped)
        .expect("a skipped warning");
    assert!(
        skipped.message.starts_with("2 name(s)"),
        "{}",
        skipped.message
    );
}

/// M3: an invalid `context` never blocks a launch yet: it is a launch
/// warning, not a manifest error.
#[test]
fn an_invalid_context_is_a_launch_warning_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    write(&team_content(home, TEAM), "CLAUDE.md", "x");
    let bad = agent(
        AgentCli::Claude,
        &["CLAUDE.md"],
        Some(vec![ContextEntry::new("../escape")]),
    );
    let resolved = crate::agents::preview_agent(
        home,
        &me(),
        &bad,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        crate::agents::ResolveOptions::default(),
    )
    .unwrap();
    assert!(
        resolved
            .manifest
            .errors
            .iter()
            .all(|e| !e.message.contains("context")),
        "{:?}",
        resolved.manifest.errors
    );
    assert!(resolved
        .warnings
        .iter()
        .any(|w| w.code == crate::agents::launch_check::LaunchWarningCode::InvalidContext));
}

/// Startup resolves before context: a remapped entry cannot take a startup
/// path (the root doc stays the team's own); collisions are grouped per pair.
#[test]
fn startup_paths_win_over_a_remapped_context_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "CLAUDE.md", "root");
    write(&team, "other/CLAUDE.md", "imposter");
    write(&team, ".claude/skills/review/SKILL.md", "mine");
    write(&team, ".claude/skills/review/extra.md", "mine");
    write(&team, "imports/review/SKILL.md", "theirs");
    write(&team, "imports/review/extra.md", "theirs");
    let a = agent(
        AgentCli::Claude,
        &["CLAUDE.md", ".claude/skills/review"],
        Some(vec![
            ContextEntry::with_dest("other/CLAUDE.md", "CLAUDE.md"),
            ContextEntry::with_dest("imports/review", ".claude/skills/review"),
        ]),
    );
    let got = preview(home, &a, None, None);
    let logical = |d: &str| {
        got.files
            .iter()
            .find(|f| f.destination == d)
            .unwrap()
            .logical
            .clone()
    };
    assert_eq!(logical("CLAUDE.md"), "CLAUDE.md");
    assert_eq!(
        logical(".claude/skills/review/SKILL.md"),
        ".claude/skills/review/SKILL.md"
    );
    // One warning per pair of entries, however many files collide.
    let collisions: Vec<&ContextWarning> = got
        .warnings
        .iter()
        .filter(|w| w.code == ContextWarningCode::Collision)
        .collect();
    assert_eq!(collisions.len(), 2, "{collisions:?}");
    assert!(collisions[1]
        .message
        .starts_with("2 file(s) of imports/review"));
}

/// L4: a startup file's `requires:` come along; a missing startup root doc
/// is silent; the root doc named by startup and the default is one entry.
#[test]
fn startup_follows_requires_and_the_root_doc_is_one_quiet_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "a.md", "---\nrequires: [lib/b.md]\n---\nA\n");
    write(&team, "lib/b.md", "B");
    let a = agent(AgentCli::Claude, &["CLAUDE.md", "a.md"], Some(vec![]));
    let got = preview(home, &a, None, None);
    assert_eq!(destinations(&got), set(&["a.md", "lib/b.md"]));
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);

    write(&team, "CLAUDE.md", "root");
    let b = agent(AgentCli::Claude, &["CLAUDE.md"], None);
    let got = preview(home, &b, None, None);
    let roots: Vec<&ResolvedContextEntry> = got
        .entries
        .iter()
        .filter(|e| e.path == "CLAUDE.md")
        .collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].files, 1);
    assert_eq!(roots[0].source, ContextEntrySource::Startup);
}

/// L10: a symlinked prefix is only `unreadable`, never also `missing`.
#[cfg(unix)]
#[test]
fn a_symlinked_prefix_is_only_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let team = team_content(home, TEAM);
    write(&team, "real/a.md", "x");
    std::os::unix::fs::symlink(team.join("real"), team.join("link")).unwrap();
    let a = agent(AgentCli::Claude, &[], Some(vec![ContextEntry::new("link")]));
    let got = preview(home, &a, None, None);
    let codes: Vec<ContextWarningCode> = got.warnings.iter().map(|w| w.code).collect();
    assert_eq!(codes, [ContextWarningCode::Unreadable]);
}

/// A failed save puts every override file back byte for byte (malformed
/// ones too) and removes only what it added; a restore leaves no stale
/// malformed file.
#[test]
fn a_failed_save_restores_override_bytes_and_restore_clears_malformed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home);
    let author = Author::user("1");
    let saved = service::save(
        home,
        &me(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        agent(AgentCli::Claude, &[], None),
        &author,
    )
    .unwrap();
    let add = |path: &str| RepoOverride {
        add: vec![ContextEntry::new(path)],
        ..RepoOverride::default()
    };
    let saved = service::save_with_overrides(
        home,
        &me(),
        SaveTarget::Update {
            key: saved.key.clone(),
            base_revision: 1,
        },
        saved.agent.clone(),
        &[(codesense(), add("docs"))],
        &author,
    )
    .unwrap();
    let dir = crate::agents::roots::team_agents_dir(home, &me(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join("barb");
    let good = dir.join("repos/github.com/alderco/codesense.yaml");
    let malformed = dir.join("repos/github.com/x/broken.yaml");
    std::fs::create_dir_all(malformed.parent().unwrap()).unwrap();
    std::fs::write(&malformed, "add: {not: [closed\n").unwrap();
    let good_bytes = std::fs::read(&good).unwrap();
    // Revision 3 cannot be written: a directory sits where its file goes.
    std::fs::create_dir_all(dir.join("revisions/3.yml/blocker")).unwrap();
    let failed = service::save_with_overrides(
        home,
        &me(),
        SaveTarget::Update {
            key: saved.key.clone(),
            base_revision: 2,
        },
        saved.agent.clone(),
        &[(codesense(), add("notes")), (other_repo(), add("docs"))],
        &author,
    );
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&good).unwrap(), good_bytes);
    assert_eq!(
        std::fs::read_to_string(&malformed).unwrap(),
        "add: {not: [closed\n"
    );
    assert!(!dir.join("repos/github.com/alderco/website.yaml").exists());

    std::fs::remove_dir_all(dir.join("revisions/3.yml")).unwrap();
    service::restore(home, &me(), &saved.key, 1, 2, &author).unwrap();
    assert!(!good.exists());
    assert!(
        !malformed.exists(),
        "restore leaves no stale malformed file"
    );
}

/// L6: an override filed in non-canonical case is reported (with the name to
/// rename it to), not silently relied on.
#[test]
fn a_non_canonical_override_name_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "repos/GitHub.com/AlderCo/CodeSense.yaml",
        "add: [docs]\n",
    );
    let (found, problems) = crate::agents::defs::read_repo_overrides(dir).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].contains("repos/github.com/alderco/codesense.yaml"),
        "{problems:?}"
    );
}

/// SW-FX-125: the context repo (the org folder, which encloses the team
/// content) keeps git-ignored output out of a bulk context prefix.
fn git_in(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn carried_under_x(
    home: &Path,
    startup: &[&str],
    context: Option<Vec<ContextEntry>>,
) -> ContextResolution {
    let a = agent(AgentCli::Claude, startup, context);
    preview(home, &a, None, None)
}

fn x_context() -> Option<Vec<ContextEntry>> {
    Some(vec![ContextEntry::with_dest("_bmad-output/x", "plans")])
}

#[test]
fn sw_fx_125_ignored_context_files_are_not_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".gitignore", "evidence/\n*.log\n");
    let team = team_content(home, TEAM);
    assert!(
        team.starts_with(&org),
        "the team content lives in the context repo"
    );
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/evidence/shot.txt", "run output");
    write(&team, "_bmad-output/x/run.log", "log");
    let got = carried_under_x(home, &[], x_context());
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert_eq!(carried, set(&["_bmad-output/x/a.md"]));
    assert!(got.warnings.is_empty(), "{:?}", got.warnings);
}

#[test]
fn sw_fx_125_nested_gitignore_and_info_exclude_both_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".git/info/exclude", "private.md\n");
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/.gitignore", "secret.md\n");
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/secret.md", "no");
    write(&team, "_bmad-output/x/private.md", "no");
    let got = carried_under_x(home, &[], x_context());
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert_eq!(
        carried,
        set(&["_bmad-output/x/.gitignore", "_bmad-output/x/a.md"])
    );
}

#[test]
fn sw_fx_125_a_tracked_file_matching_an_ignore_rule_is_still_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".gitignore", "*.log\n");
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/run.log", "log");
    let run_log = team.join("_bmad-output/x/run.log");
    git_in(&org, &["add", "-f", "--", run_log.to_str().unwrap()]);
    let got = carried_under_x(home, &[], x_context());
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert_eq!(
        carried,
        set(&["_bmad-output/x/a.md", "_bmad-output/x/run.log"])
    );
}

#[test]
fn sw_fx_125_startup_paths_keep_git_ignored_files() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".gitignore", "*.log\n");
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/run.log", "log");
    let got = carried_under_x(home, &["_bmad-output/x/run.log"], Some(vec![]));
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert!(carried.contains("_bmad-output/x/run.log"), "{carried:?}");
}

#[test]
fn sw_fx_125_a_tracked_file_under_an_ignored_folder_is_still_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".gitignore", "evidence/\n");
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/evidence/shot.txt", "run output");
    write(&team, "_bmad-output/x/evidence/keep.md", "kept");
    let keep = team.join("_bmad-output/x/evidence/keep.md");
    git_in(&org, &["add", "-f", "--", keep.to_str().unwrap()]);
    let got = carried_under_x(home, &[], x_context());
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert_eq!(
        carried,
        set(&["_bmad-output/x/a.md", "_bmad-output/x/evidence/keep.md"])
    );
}

#[test]
fn sw_fx_125_a_named_ignored_file_is_not_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org = write_org(home);
    git_in(&org, &["init", "-q"]);
    write(&org, ".gitignore", "*.log\n");
    let team = team_content(home, TEAM);
    write(&team, "_bmad-output/x/a.md", "aa");
    write(&team, "_bmad-output/x/run.log", "log");
    let got = carried_under_x(
        home,
        &[],
        Some(vec![
            ContextEntry::new("_bmad-output/x/run.log"),
            ContextEntry::new("_bmad-output/x/a.md"),
        ]),
    );
    let carried: BTreeSet<String> = got.files.iter().map(|f| f.logical.clone()).collect();
    assert_eq!(carried, set(&["_bmad-output/x/a.md"]));
}

/// SV-GHI-3: the org managed under `home`, or (`attached`) moved to a user
/// checkout and attached, with the parent's kit (root doc, a skill, references)
/// and no `config/sharing.yml` in the parent.
fn kit_org(tmp: &Path, attached: bool) -> PathBuf {
    let home = tmp.join("state");
    let org_root = write_org(&home);
    if attached {
        let checkout = tmp.join("checkout");
        std::fs::rename(&org_root, &checkout).unwrap();
        git2::Repository::init(&checkout).unwrap();
        crate::context_repo::context_attachments::attach(&home, &me(), &checkout).unwrap();
    }
    let parent = team_content(&home, PARENT);
    write(&parent, "CLAUDE.md", "kit");
    write(&parent, ".claude/skills/kit/SKILL.md", "kit-skill");
    write(&parent, "references/r.md", "ref");
    std::fs::remove_file(parent.parent().unwrap().join("config/sharing.yml")).unwrap();
    home
}

fn kit_agent() -> Agent {
    agent(
        AgentCli::Claude,
        &[],
        Some(
            ["CLAUDE.md", ".claude/skills", "references", "nowhere.md"]
                .into_iter()
                .map(ContextEntry::new)
                .collect(),
        ),
    )
}

/// SV-GHI-3: a parent with no `config/sharing.yml` shares nothing and says
/// so once as a layer warning; every missing path carries the same hint, so a
/// path the parent holds reads exactly like one nobody holds. Sharing it
/// resolves the kit from the parent. Managed and attached orgs agree.
#[test]
fn ghi3_a_parent_without_sharing_says_so_without_an_oracle() {
    let mut seen = Vec::new();
    for attached in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let home = kit_org(tmp.path(), attached);
        let got = preview(&home, &kit_agent(), None, None);
        let layer: Vec<_> = got
            .warnings
            .iter()
            .filter(|w| w.code == ContextWarningCode::Layer)
            .map(|w| w.message.clone())
            .collect();
        assert_eq!(
            layer,
            [crate::context_repo::inheritance::shares_nothing_warning(
                PARENT
            )],
            "attached={attached}"
        );
        let missing: Vec<_> = got
            .warnings
            .iter()
            .filter(|w| w.code == ContextWarningCode::Missing)
            .collect();
        assert_eq!(missing.len(), 4, "attached={attached}: {missing:?}");
        let hint = format!(
            "is in no content tier; parent team {PARENT} shares nothing (no config/sharing.yml)"
        );
        for w in &missing {
            let path = w.path.as_deref().unwrap();
            assert_eq!(w.message, format!("{path} {hint}"), "attached={attached}");
        }
        // The oracle check: the parent holds CLAUDE.md, nobody holds
        // nowhere.md, and the two read byte-identically past the path.
        let past_path = |p: &str| {
            let w = missing
                .iter()
                .find(|w| w.path.as_deref() == Some(p))
                .unwrap();
            w.message.strip_prefix(p).unwrap().to_string()
        };
        assert_eq!(past_path("CLAUDE.md"), past_path("nowhere.md"));
        assert!(got.files.is_empty(), "{:?}", destinations(&got));
        seen.push(got.warnings.clone());

        let parent = team_content(&home, PARENT);
        std::fs::write(
            parent.parent().unwrap().join("config/sharing.yml"),
            "share: ['content/**']\n",
        )
        .unwrap();
        let got = preview(&home, &kit_agent(), None, None);
        assert_eq!(
            got.warnings
                .iter()
                .filter(|w| w.code != ContextWarningCode::Missing)
                .count(),
            0,
            "attached={attached}: {:?}",
            got.warnings
        );
        assert_eq!(
            got.warnings
                .iter()
                .map(|w| w.path.as_deref())
                .collect::<Vec<_>>(),
            [Some("nowhere.md")],
            "attached={attached}"
        );
        assert!(got.warnings[0].message.ends_with("is in no content tier"));
        assert_eq!(
            destinations(&got),
            set(&[
                "CLAUDE.md",
                ".claude/skills/kit/SKILL.md",
                "references/r.md"
            ])
        );
        assert!(got.files.iter().all(|f| f.tier
            == ContextTier::Team {
                org: ORG.into(),
                team: PARENT.into()
            }));
        seen.push(got.warnings);
    }
    assert_eq!(seen[0], seen[2], "managed and attached warn alike");
    assert_eq!(seen[1], seen[3], "managed and attached resolve alike");
}
