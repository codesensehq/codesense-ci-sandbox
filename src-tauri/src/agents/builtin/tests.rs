// SPDX-License-Identifier: MIT
//! Seam tests for the starter agents: the real service, resolver and launch
//! prepare over a real temp home and team content.

use crate::agents::launch::{self, PrepareOptions};
use crate::agents::manifest::{EntryOrigin, ManifestErrorCode};
use crate::agents::resolve::tests::{alice, content, draft, save, team_content, write, ORG, TEAM};
use crate::agents::service::{self, AgentError, NoReferrers, Placement, SaveTarget};
use crate::agents::{resolve_agent, AgentKey, AgentScope, ResolveOptions};
use crate::routines::model::Author;
use crate::routines::PersonalOwner;
use codesense_pty_core::agent_cli::descriptor;
use codesense_pty_core::agent_cli::AgentCli;

use super::*;

fn target() -> ResolveOptions {
    ResolveOptions {
        target_team: Some((ORG.into(), TEAM.into())),
    }
}

fn mine() -> Placement {
    Placement::Personal(PersonalOwner {
        github_user_id: 1,
        username: "alice".into(),
        display_name: "Alice".into(),
        avatar_url: None,
        org: "acme/api".into(),
    })
}

fn team_placement() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

fn by_alice() -> Author {
    Author::user("1")
}

#[test]
fn keys_render_as_builtin_slash_id_and_never_as_a_team_key() {
    let claude = key(AgentCli::Claude);
    assert_eq!(claude.to_string(), "builtin/claude");
    assert_eq!(AgentKey::parse("builtin/claude"), Some(claude.clone()));
    assert!(claude.is_builtin() && claude.resolves_for(&alice()));
    assert!(!claude.is_personal());
    assert_eq!(
        serde_json::to_string(&key(AgentCli::Codex)).unwrap(),
        "\"builtin/codex\""
    );
    // A team org named like it is still a team key, and no other shape parses.
    let team = AgentKey::parse("builtin/x/backend/claude").unwrap();
    assert!(!team.is_builtin());
    assert_eq!(AgentKey::parse("builtin/"), None);
    assert_eq!(get(&AgentKey::builtin("gemini")), None);

    // A GitHub owner named `builtin` keeps its team keys: an org slug is
    // `owner/repo`, so a team key always has four segments, and it renders
    // and parses back as the team key it is.
    let owned = AgentKey::parse("builtin/tools/backend/claude").unwrap();
    assert_eq!(
        (owned.org.as_str(), owned.team.as_str(), owned.id.as_str()),
        ("builtin/tools", "backend", "claude")
    );
    assert!(!owned.is_builtin() && !is_starter(&owned));
    assert_eq!(owned.to_string(), "builtin/tools/backend/claude");
    // Three segments name no starter, and `builtin` alone is no org a team
    // or an agent can live in.
    let short = AgentKey::parse("builtin/backend/claude").unwrap();
    assert!(!short.is_builtin());
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        service::get(tmp.path(), &alice(), &short),
        Err(AgentError::InvalidLocation(_))
    ));
    // A routine key in the starters' shape names no routine.
    let routine = crate::routines::RoutineKey::parse("builtin/claude").unwrap();
    assert!(crate::routines::service::get(tmp.path(), &alice(), &routine).is_err());
}

#[test]
fn the_revision_rises_with_the_app_version_and_never_collides() {
    let versions = [
        ("0.0.1", 1),
        ("0.0.13", 13),
        ("0.0.999", 999),
        ("0.1.0", 1_000),
        ("0.1.2", 1_002),
        ("0.2.0", 2_000),
        ("1.0.0", 1_000_000),
        ("1.2.3", 1_002_003),
        ("2.0.0", 2_000_000),
    ];
    for (version, expected) in versions {
        assert_eq!(revision_of(version), expected, "{version}");
    }
    let revisions: Vec<u32> = versions.iter().map(|(v, _)| revision_of(v)).collect();
    assert!(
        revisions.windows(2).all(|w| w[0] < w[1]),
        "every later release has a higher revision: {revisions:?}"
    );
    // A pre-release or build suffix leaves the number alone (the definition
    // hash tells those apart); a field past 999 is held, never spilling over.
    assert_eq!(revision_of("1.2.3-beta.1"), 1_002_003);
    assert_eq!(revision_of("1.2.3+build.7"), 1_002_003);
    assert_eq!(revision_of("0.1000.0"), 999_000);
    assert!(revision_of("0.1000.0") < revision_of("1.0.0"));
    // Never 0, which no agent revision is.
    assert_eq!(revision_of("0.0.0"), 1);
    assert_eq!(revision(), revision_of(env!("CARGO_PKG_VERSION")));

    // Whatever an older build stamped, the stamp parses and reads back; a
    // revision no build up to this one stamped is not found.
    let tmp = tempfile::tempdir().unwrap();
    let claude = key(AgentCli::Claude);
    for bad in [0, revision() + 1] {
        assert!(matches!(
            service::get_pinned(tmp.path(), &alice(), &claude, bad),
            Err(AgentError::RevisionNotFound { .. })
        ));
    }
    for (version, _) in versions
        .iter()
        .filter(|(v, _)| revision_of(v) <= revision())
    {
        let stamp = format!("builtin/claude@{}", revision_of(version));
        assert_eq!(
            crate::agents::AgentRevisionRef::key_of(&stamp),
            Some(key(AgentCli::Claude))
        );
        let pinned = service::get_pinned(
            tmp.path(),
            &alice(),
            &key(AgentCli::Claude),
            revision_of(version),
        )
        .unwrap();
        assert_eq!(pinned.reference.to_string(), stamp);
        assert_eq!(pinned.agent, get(&key(AgentCli::Claude)).unwrap());
    }
}

#[test]
fn the_service_lists_and_reads_the_starters_after_everyone_elses_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    save(home, &alice(), draft("triage", AgentCli::Claude));

    let listed = service::list(home, &alice(), false).unwrap();
    let keys: Vec<String> = listed.iter().map(|a| a.key.to_string()).collect();
    let expected: Vec<String> = ["acme/api/backend/triage".to_string()]
        .into_iter()
        .chain(AgentCli::ALL.map(|cli| key(cli).to_string()))
        .chain(AgentCli::ALL.map(|cli| unattended_key(cli).to_string()))
        .collect();
    assert_eq!(keys, expected);
    for (found, runtime) in listed[1..].iter().zip(AgentCli::ALL) {
        let name = crate::agents::adapter::desktop(runtime).starter.name;
        assert_eq!(found.scope, AgentScope::Builtin);
        assert_eq!(found.created_by_display, CREATOR);
        let agent = &found.agent;
        assert_eq!((agent.runtime, agent.name.as_str()), (runtime, name));
        assert!(agent.args.is_empty() && agent.mcp.servers.is_empty());
        assert_eq!(agent.startup, [descriptor(runtime).root_doc.as_str()]);
        assert!(agent.permissions.is_none());
        assert!(agent.default_prompt.is_empty() && !agent.is_archived());
        assert_eq!(agent.revision, revision());
        agent.validate().unwrap();
    }
    // Nothing is written for them.
    assert!(!home.join("builtin").exists());

    let detail = service::get(home, &alice(), &key(AgentCli::Codex)).unwrap();
    assert_eq!(detail.scope, AgentScope::Builtin);
    assert_eq!(detail.revisions.len(), 1);
    let pinned = service::get_pinned(home, &alice(), &key(AgentCli::Codex), revision()).unwrap();
    assert_eq!(
        pinned.reference.to_string(),
        format!("builtin/codex@{}", revision())
    );
    assert_eq!(
        pinned.reference.definition_hash,
        detail.revisions[0].definition_hash
    );
    // An earlier build's stamp still resolves after an upgrade, to this build's
    // definition.
    let earlier = service::get_pinned(home, &alice(), &key(AgentCli::Codex), 1).unwrap();
    assert_eq!(earlier.reference.revision, 1);
    assert_eq!(earlier.agent, detail.agent);
}

#[test]
fn every_change_to_a_starter_is_read_only_and_copy_to_mine_is_editable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let claude = key(AgentCli::Claude);
    let read_only = Err::<(), _>(AgentError::ReadOnly(claude.clone()));
    let current = get(&claude).unwrap();

    let update = service::save(
        home,
        &alice(),
        SaveTarget::Update {
            key: claude.clone(),
            base_revision: current.revision,
        },
        Agent {
            name: "Mine".into(),
            ..current.clone()
        },
        &by_alice(),
    );
    assert_eq!(update.map(|_| ()), read_only);
    // Even with a body that would not validate: copying it is the fix.
    let broken = service::save(
        home,
        &alice(),
        SaveTarget::Update {
            key: claude.clone(),
            base_revision: current.revision,
        },
        Agent {
            startup: vec!["../x".into()],
            ..current.clone()
        },
        &by_alice(),
    );
    assert_eq!(broken.map(|_| ()), read_only);
    assert_eq!(
        service::archive(home, &alice(), &claude, &by_alice()).map(|_| ()),
        read_only
    );
    assert_eq!(
        service::unarchive(home, &alice(), &claude, current.revision, &by_alice()).map(|_| ()),
        read_only
    );
    assert_eq!(
        service::restore(home, &alice(), &claude, 1, current.revision, &by_alice()).map(|_| ()),
        read_only
    );
    assert_eq!(
        service::delete(home, &alice(), &claude, None, &NoReferrers),
        read_only
    );
    assert_eq!(
        AgentError::ReadOnly(claude.clone()).to_string(),
        "built-in agent — Copy to Mine to customize"
    );

    let copy = service::copy_to_mine(home, &alice(), &claude, mine(), &by_alice()).unwrap();
    assert_eq!(
        copy.key.to_string(),
        "_personal/1/acme/api/_scratch/claude-copy"
    );
    assert_eq!(copy.agent.revision, 1);
    assert_eq!(copy.agent.created_by, "1");
    assert_eq!(copy.agent.runtime, AgentCli::Claude);
    assert_eq!(copy.agent.startup, ["CLAUDE.md"]);
    let edited = service::save(
        home,
        &alice(),
        SaveTarget::Update {
            key: copy.key.clone(),
            base_revision: 1,
        },
        Agent {
            args: vec!["--model".into(), "opus".into()],
            ..copy.agent
        },
        &by_alice(),
    )
    .unwrap();
    assert_eq!(edited.agent.revision, 2);
    // The starter itself is unchanged.
    assert_eq!(get(&claude).unwrap(), current);
}

#[test]
fn a_starter_starts_with_the_essentials_and_the_teams_root_doc_when_it_has_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());

    let claude = resolve_agent(home, &alice(), &key(AgentCli::Claude), target()).unwrap();
    assert_eq!(
        claude.agent.to_string(),
        format!("builtin/claude@{}", revision())
    );
    let entries: Vec<(&str, &EntryOrigin)> = claude
        .manifest
        .entries
        .iter()
        .map(|e| (e.path.as_str(), &e.origin))
        .collect();
    // SW-AN-49: the notify hook ships with CodeSense's own install; it never
    // appears as a manifest entry.
    assert_eq!(entries, [("CLAUDE.md", &EntryOrigin::Declared)]);
    let essentials: Vec<&str> = claude
        .manifest
        .essentials
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(
        essentials,
        ["codesense-bridge", "codesense-notify-settings"]
    );
    assert!(
        claude.manifest.errors.is_empty(),
        "{:?}",
        claude.manifest.errors
    );
    assert_eq!(claude.launch.mcp_allow, Some(Vec::new()));
    // SW-AN-50: the only warning on a starter against this team is the
    // silent-empty one — the team offers `slack`, the starter carries nothing,
    // and the UI surfaces that so the user knows to copy the starter and grant
    // access. Any other warning on a starter is still a bug.
    let codes: Vec<_> = claude.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        vec![crate::agents::launch_check::LaunchWarningCode::NoMcpServersOffered],
        "{:?}",
        claude.warnings
    );

    // Codex reads AGENTS.md, and a team without it is no error.
    let codex = resolve_agent(home, &alice(), &key(AgentCli::Codex), target()).unwrap();
    assert!(codex.manifest.entries.is_empty());
    assert!(
        codex.manifest.errors.is_empty(),
        "{:?}",
        codex.manifest.errors
    );
    // SW-AN-50: a Codex MCP config lives at `.codex/config.toml`; without one
    // the codex starter has nothing offered and no warning fires. Write one,
    // and the codex starter surfaces the same silent-empty warning.
    assert!(
        !codex
            .warnings
            .iter()
            .any(|w| w.code == crate::agents::launch_check::LaunchWarningCode::NoMcpServersOffered),
        "codex starter without a .codex/config.toml should NOT warn: {:?}",
        codex.warnings
    );
    write(
        &content(home, &alice(), TEAM),
        ".codex/config.toml",
        b"[mcp_servers.slack]\ncommand = \"slack-mcp\"\n",
    );
    let codex = resolve_agent(home, &alice(), &key(AgentCli::Codex), target()).unwrap();
    assert_eq!(codex.offered_mcp_servers, vec!["slack".to_string()]);
    assert!(
        codex
            .warnings
            .iter()
            .any(|w| w.code == crate::agents::launch_check::LaunchWarningCode::NoMcpServersOffered),
        "codex starter should also surface the silent-empty warning: {:?}",
        codex.warnings
    );
    write(&content(home, &alice(), TEAM), "AGENTS.md", b"# Agents\n");
    let codex = resolve_agent(home, &alice(), &key(AgentCli::Codex), target()).unwrap();
    let paths: Vec<&str> = codex
        .manifest
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(paths, ["AGENTS.md"]);
    assert_eq!(
        codex
            .manifest
            .essentials
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["codesense-bridge"]
    );

    // A misspelled root doc is not the root doc.
    std::fs::remove_file(content(home, &alice(), TEAM).join("CLAUDE.md")).unwrap();
    write(&content(home, &alice(), TEAM), "claude.md", b"# lower\n");
    let claude = resolve_agent(home, &alice(), &key(AgentCli::Claude), target()).unwrap();
    assert!(claude
        .manifest
        .entries
        .iter()
        .all(|e| e.path.as_str() != "CLAUDE.md" && e.path.as_str() != "claude.md"));
    assert!(!claude
        .manifest
        .errors
        .iter()
        .any(|e| e.code == ManifestErrorCode::Missing && e.message.contains("CLAUDE.md")));

    // With no target, a starter resolves against the personal space alone.
    let untargeted = resolve_agent(
        home,
        &alice(),
        &key(AgentCli::Codex),
        ResolveOptions::default(),
    )
    .unwrap();
    assert!(untargeted.manifest.entries.is_empty());
}

#[test]
fn a_launch_finds_a_starter_by_key_or_bare_id_and_records_its_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let options = || PrepareOptions {
        target_team: Some((ORG.into(), TEAM.into())),
        ..Default::default()
    };

    let by_key = launch::prepare_named(home, &alice(), "builtin/codex", options()).unwrap();
    let by_id = launch::prepare_named(home, &alice(), "codex", options()).unwrap();
    let stamp = format!("builtin/codex@{}", revision());
    assert_eq!(by_key.revision_label(), stamp);
    assert_eq!(by_id.revision_label(), stamp);
    assert_eq!(by_key.launch.binary, "codex");
    assert_eq!(by_key.default_prompt, None);
    assert_eq!(
        by_key.provenance(None, "h").agent.as_deref(),
        Some(stamp.as_str())
    );

    // A team agent with that id wins over the starter.
    save(home, &alice(), draft("codex", AgentCli::Codex));
    let team = launch::prepare_named(home, &alice(), "codex", options()).unwrap();
    assert_eq!(team.revision_label(), "acme/api/backend/codex@1");
}

/// A copy names the starter's startup outright, and resolves to the very same
/// manifest wherever it launches: with the team's root doc where the team has
/// it, and without it, never refused, where it does not.
#[test]
fn a_copy_resolves_exactly_like_its_starter_with_or_without_the_root_doc() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let entries = |key: &AgentKey| -> Vec<(String, EntryOrigin)> {
        let resolved = resolve_agent(home, &alice(), key, target()).unwrap();
        assert!(
            resolved.manifest.errors.is_empty(),
            "{key}: {:?}",
            resolved.manifest.errors
        );
        resolved
            .manifest
            .entries
            .iter()
            .map(|e| (e.path.to_string(), e.origin.clone()))
            .collect()
    };
    for runtime in AgentCli::ALL {
        let starter = key(runtime);
        let copy = service::copy_to_mine(home, &alice(), &starter, mine(), &by_alice()).unwrap();
        assert_eq!(copy.agent.startup, [descriptor(runtime).root_doc.as_str()]);

        let doc = content(home, &alice(), TEAM).join(descriptor(runtime).root_doc.as_str());
        std::fs::remove_file(&doc).ok();
        let without = entries(&starter);
        assert!(without
            .iter()
            .all(|(p, _)| p != descriptor(runtime).root_doc.as_str()));
        assert_eq!(entries(&copy.key), without);

        write(
            &content(home, &alice(), TEAM),
            descriptor(runtime).root_doc.as_str(),
            b"# Rules\n",
        );
        let with = entries(&starter);
        assert_eq!(
            with[0],
            (
                descriptor(runtime).root_doc.as_str().to_string(),
                EntryOrigin::Declared
            )
        );
        assert_eq!(entries(&copy.key), with);
    }

    // Only a missing root doc is left out: one that is there but unusable is
    // still an error, so the team's rules are never silently dropped.
    write(
        &content(home, &alice(), TEAM),
        "CLAUDE.md",
        b"---\nrequires: [../outside.md]\n---\n# Rules\n",
    );
    let claude = resolve_agent(home, &alice(), &key(AgentCli::Claude), target()).unwrap();
    assert!(
        claude
            .manifest
            .errors
            .iter()
            .any(|e| e.path.as_ref().is_some_and(|p| p.as_str() == "CLAUDE.md")),
        "{:?}",
        claude.manifest.errors
    );

    // Only the root doc is optional: any other missing path is still refused.
    let mut other = draft("notes", AgentCli::Claude);
    other.startup = vec!["CLAUDE.md".into(), "docs/gone.md".into()];
    let notes = save(home, &alice(), other);
    let resolved = resolve_agent(home, &alice(), &notes, target()).unwrap();
    let missing: Vec<&str> = resolved
        .manifest
        .errors
        .iter()
        .filter(|e| e.code == ManifestErrorCode::Missing)
        .filter_map(|e| e.path.as_ref().map(|p| p.as_str()))
        .collect();
    assert_eq!(missing, ["docs/gone.md"]);
}

/// No starter is special: an agent a team saves with the id `controller` is an ordinary
/// team agent, found by a bare-id launch like any other, and nothing ships under that id.
#[test]
fn an_agent_named_controller_is_an_ordinary_team_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let options = || PrepareOptions {
        target_team: Some((ORG.into(), TEAM.into())),
        ..Default::default()
    };
    let shipped = AgentKey::builtin("controller");
    assert!(!is_starter(&shipped));
    assert!(get(&shipped).is_none());
    assert!(all().iter().all(|(key, _)| key.id != "controller"));
    assert!(launch::prepare_named(home, &alice(), "controller", options()).is_err());

    let saved = save(home, &alice(), draft("controller", AgentCli::Claude));
    let team = launch::prepare_named(home, &alice(), "controller", options()).unwrap();
    assert_eq!(team.revision_label(), "acme/api/backend/controller@1");
    // Archived through the same path as any team agent, never refused as a starter.
    assert_eq!(saved.to_string(), "acme/api/backend/controller");
    service::archive(home, &alice(), &saved, &by_alice()).unwrap();
}

/// The unattended starters are the runtime starters with every permission check off: what
/// a background launch that names no agent runs, so it never waits on a prompt nobody
/// sees. They are listed, read-only, copyable, and their launch carries the bypass posture
/// in its argv and per-launch config.
#[test]
fn the_unattended_starters_run_without_asking_and_are_read_only_built_ins() {
    use crate::agents::adapter::LaunchMode as Mode;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());

    let listed = service::list(home, &alice(), false).unwrap();
    for (runtime, id, name) in [
        (AgentCli::Claude, "claude-unattended", "Claude (unattended)"),
        (AgentCli::Codex, "codex-unattended", "Codex (unattended)"),
    ] {
        let key = unattended_key(runtime);
        assert_eq!(key.to_string(), format!("builtin/{id}"));
        assert!(is_starter(&key));
        let found = listed.iter().find(|a| a.key == key).expect("listed");
        assert_eq!(found.scope, AgentScope::Builtin);
        let agent = &found.agent;
        assert_eq!((agent.runtime, agent.name.as_str()), (runtime, name));
        assert!(agent.description.contains("without asking for permission"));
        assert!(agent.description.contains("unattended"));
        assert_eq!(agent.startup, [descriptor(runtime).root_doc.as_str()]);
        agent.validate().unwrap();

        // Read-only, and Copy to Mine makes an ordinary editable agent of it.
        assert_eq!(
            service::archive(home, &alice(), &key, &by_alice()).map(|_| ()),
            Err(AgentError::ReadOnly(key.clone()))
        );
        let copy = service::copy_to_mine(home, &alice(), &key, mine(), &by_alice()).unwrap();
        assert_eq!(copy.agent.permissions, agent.permissions);
        assert_eq!(copy.agent.args, agent.args);
    }

    // Through the real launch prepare, headless and interactive alike, the posture holds.
    for mode in [Mode::Headless, Mode::Interactive] {
        let options = || PrepareOptions {
            target_team: Some((ORG.into(), TEAM.into())),
            mode_override: Some(mode),
            ..PrepareOptions::default()
        };
        let claude =
            launch::prepare(home, &alice(), &unattended_key(AgentCli::Claude), options()).unwrap();
        assert_eq!(
            claude.revision_label(),
            format!("builtin/claude-unattended@{}", revision())
        );
        assert_eq!(claude.launch.args, ["--dangerously-skip-permissions"]);
        assert_eq!(
            claude.generated[0].content["permissions"]["defaultMode"], "bypassPermissions",
            "{mode:?}"
        );
        let codex =
            launch::prepare(home, &alice(), &unattended_key(AgentCli::Codex), options()).unwrap();
        assert!(codex.launch.args.is_empty());
        assert_eq!(codex.generated[0].content["approval_policy"], "never");
        assert_eq!(
            codex.generated[0].content["sandbox_mode"], "danger-full-access",
            "{mode:?}"
        );
    }
}

#[test]
fn the_default_starter_is_unattended_only_for_a_background_launch() {
    assert_eq!(default_key(true).to_string(), "builtin/claude-unattended");
    assert_eq!(default_key(false).to_string(), "builtin/claude");
    assert_eq!(
        default_key_for(AgentCli::Codex, true).to_string(),
        "builtin/codex-unattended"
    );
    assert_eq!(
        default_key_for(AgentCli::Codex, false),
        key(AgentCli::Codex)
    );
}

#[test]
fn copy_to_mine_with_a_team_placement_writes_the_clone_to_the_team() {
    // The directive on #1870 was `copy_to_mine` with Placement::Team should write
    // the clone to the team's content, not personal scratch. Prove it on a starter.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());

    let claude = key(AgentCli::Claude);
    let copy = service::copy_to_mine(home, &alice(), &claude, team_placement(), &by_alice())
        .expect("team-scoped copy_to_mine saves");
    assert_eq!(copy.agent.revision, 1);
    // The returned key reflects the team placement (org/team/id), not personal scratch.
    assert_eq!(copy.key.org, ORG);
    assert_eq!(copy.key.team, TEAM);
    assert_eq!(copy.key.id, "claude-copy");
    assert!(!copy.key.is_personal() && !copy.key.is_builtin());

    // The team's agents list surfaces the clone.
    let listed = service::list(home, &alice(), false).unwrap();
    assert!(
        listed.iter().any(|row| row.key == copy.key),
        "team clone shows in the team's list"
    );
}

/// SW-XS-13: no starter carries an explicit `context`, so the runtime's (or
/// the team workspace's) default applies where it launches.
#[test]
fn starters_carry_no_explicit_context() {
    for (key, agent) in all() {
        assert_eq!(agent.context, None, "{key}");
        agent.validate().unwrap();
    }
}
