// SPDX-License-Identifier: MIT
//! The agent editor's commands over a temp home with real team content: the
//! save, preview and restore bodies through the real agent store, resolver
//! and MCP tool bodies, and the startup catalog over real content layers.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::path::Path;

use serde_json::json;

use super::*;
use crate::agents::content_reader::Node;
use crate::agents::manifest::EntryOrigin;
use crate::agents::roots;
use crate::domain::Principal;
use crate::routines::test_support::{
    agent, alice, copy_dir, save_personal_agent, write_org, TempHome, ORG, TEAM,
};
use codesense_pty_core::agent_cli::AgentCli;

fn caller_for(home: &Path, user_id: u64) -> Caller {
    Caller {
        home: home.to_path_buf(),
        principal: Principal::from(user_id),
    }
}

fn by(name: &str) -> Author {
    Author::user(name)
}

fn team_placement() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

fn draft(value: serde_json::Value) -> AgentDraft {
    serde_json::from_value(value).unwrap()
}

fn write(root: &Path, logical: &str, body: &[u8]) {
    let path = root.join(logical);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn team_content(home: &Path) -> PathBuf {
    roots::team_content_dir(home, &alice(), ORG, TEAM).unwrap()
}

fn saved(reply: AgentSaveReply) -> (String, u32) {
    match reply {
        AgentSaveReply::Saved { key, revision, .. } => (key, revision),
        other => panic!("expected a save, got {other:?}"),
    }
}

fn resolved(reply: AgentPreviewReply) -> ResolvedItem {
    match reply {
        AgentPreviewReply::Resolved { resolved, .. } => *resolved,
        other => panic!("expected a preview, got {other:?}"),
    }
}

/// The editor's whole loop over real content: a preview of a form that picks
/// a skill folder resolves its real files and its `requires` dependency and
/// writes nothing; the save then resolves to exactly the files the preview
/// showed; an edit against a stale revision is a conflict that writes
/// nothing; a restore writes the old definition as a new revision; a teammate
/// edits the agent and the history credits them.
#[test]
fn seam_preview_save_conflict_restore_round_trip() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let content = team_content(&home);
    write(
        &content,
        ".claude/skills/bmad-build/SKILL.md",
        b"---\nname: bmad-build\nrequires: [docs/guide.md]\n---\nBuild.\n",
    );
    write(&content, ".claude/skills/bmad-build/ref.md", b"reference\n");
    write(&content, "docs/guide.md", b"# Guide\n");
    let caller = caller_for(&home, 1);
    let before = service::list(&home, &alice(), true).unwrap().len();

    let form = json!({
        "name": "Builder",
        "runtime": "claude",
        "startup": [".claude/skills/bmad-build/"],
        "permissions": { "claude": { "allow": ["Read"], "deny": ["Bash(rm -rf:*)"] } }
    });
    let shown_preview = resolved(
        preview(
            &caller,
            team_placement(),
            draft(form.clone()),
            None,
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(
        service::list(&home, &alice(), true).unwrap().len(),
        before,
        "a preview writes nothing"
    );
    let declared = &shown_preview.manifest.entries[0];
    assert_eq!(declared.path.as_str(), ".claude/skills/bmad-build");
    assert_eq!(declared.origin, EntryOrigin::Declared);
    let logical: Vec<&str> = declared.files.iter().map(|f| f.logical.as_str()).collect();
    assert_eq!(
        logical,
        [
            ".claude/skills/bmad-build/SKILL.md",
            ".claude/skills/bmad-build/ref.md"
        ]
    );
    assert_eq!(
        declared.files[1].hash,
        crate::agents::manifest::content_hash(b"reference\n")
    );
    let dependency = &shown_preview.manifest.entries[1];
    assert_eq!(dependency.path.as_str(), "docs/guide.md");
    assert!(matches!(dependency.origin, EntryOrigin::Dependency { .. }));
    assert!(shown_preview
        .provides
        .skills
        .contains(&"bmad-build".to_string()));

    let (key, revision) = saved(
        save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(form.clone()),
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(key, format!("{ORG}/{TEAM}/builder"));
    assert_eq!(revision, 1);
    let shown = detail(&caller, &key).unwrap();
    assert_eq!(
        serde_json::to_value(&shown.resolved.manifest.entries).unwrap(),
        serde_json::to_value(&shown_preview.manifest.entries).unwrap(),
        "the saved agent resolves to exactly the previewed files"
    );
    assert_eq!(
        shown.resolved.manifest.totals,
        shown_preview.manifest.totals
    );

    // An edit preview of the saved agent keeps its id and key.
    let mut edit = form.clone();
    edit["description"] = json!("Builds things.");
    let edit_preview = resolved(
        preview(
            &caller,
            team_placement(),
            draft(json!({ "id": "builder", "name": "Builder", "runtime": "claude" })),
            None,
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(edit_preview.agent.key.to_string(), key);

    let (_, revision) = saved(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            draft(edit.clone()),
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(revision, 2);
    edit["description"] = json!("A stale edit.");
    assert_eq!(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            draft(edit),
            &by("alice"),
        )
        .unwrap(),
        AgentSaveReply::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(
        detail(&caller, &key).unwrap().definition.description,
        "Builds things."
    );

    // A stale restore writes nothing; the current base writes revision 3.
    assert_eq!(
        restore_revision(&caller, &key, 1, 1, &by("alice")).unwrap(),
        AgentSaveReply::Conflict {
            current_revision: 2
        }
    );
    let (_, revision) = saved(restore_revision(&caller, &key, 1, 2, &by("alice")).unwrap());
    assert_eq!(revision, 3);
    let restored = detail(&caller, &key).unwrap();
    assert_eq!(restored.definition.description, "");
    assert_eq!(restored.revisions.len(), 3);

    // Bob, who did not create it, edits it from his own checkout of the same
    // team; his revision is credited to him.
    let bob = Principal::from(2u64);
    write_org(&home, 2);
    let from = roots::team_agents_dir(&home, &alice(), ORG, TEAM)
        .unwrap()
        .unwrap();
    let to = roots::team_agents_dir(&home, &bob, ORG, TEAM)
        .unwrap()
        .unwrap();
    copy_dir(&from.join("builder"), &to.join("builder"));
    let bobs = caller_for(&home, 2);
    let (_, revision) = saved(
        save(
            &bobs,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 3,
            },
            draft(json!({ "name": "Builder", "runtime": "claude", "description": "Bob's" })),
            &by("bob"),
        )
        .unwrap(),
    );
    assert_eq!(revision, 4);
    let history = detail(&bobs, &key).unwrap().revisions;
    assert_eq!(history.last().unwrap().author, by("bob"));
}

/// A save with manifest errors goes through: errors are shown, never
/// enforced.
#[test]
fn a_save_with_manifest_errors_is_allowed() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let form = json!({ "name": "Lost", "runtime": "claude", "startup": ["docs/nowhere.md"] });
    let shown_preview = resolved(
        preview(
            &caller,
            team_placement(),
            draft(form.clone()),
            None,
            &by("a"),
        )
        .unwrap(),
    );
    assert!(shown_preview
        .manifest
        .errors
        .iter()
        .any(|e| e.path.as_ref().map(|p| p.as_str()) == Some("docs/nowhere.md")));
    let (key, _) = saved(
        save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(form),
            &by("a"),
        )
        .unwrap(),
    );
    assert_eq!(
        detail(&caller, &key).unwrap().definition.startup,
        ["docs/nowhere.md"]
    );
}

/// Both runtimes' permission blocks round-trip through the form: switching
/// runtime keeps the other block, and a block the form leaves out is removed.
#[test]
fn permissions_round_trip_and_switching_runtime_keeps_the_other_block() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let both = json!({
        "claude": { "allow": ["Read"], "deny": ["WebFetch"], "defaultMode": "acceptEdits" },
        "codex": { "approvalPolicy": "on-request", "sandboxMode": "read-only" }
    });
    let (key, _) = saved(
        save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(json!({ "name": "Perms", "runtime": "claude", "permissions": both })),
            &by("a"),
        )
        .unwrap(),
    );
    let (_, revision) = saved(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            draft(json!({ "name": "Perms", "runtime": "codex", "permissions": both })),
            &by("a"),
        )
        .unwrap(),
    );
    let shown = detail(&caller, &key).unwrap();
    let permissions = shown.definition.permissions.clone().unwrap();
    assert_eq!(shown.definition.runtime, AgentCli::Codex);
    assert_eq!(
        permissions.claude.as_ref().unwrap().default_mode.as_deref(),
        Some("acceptEdits")
    );
    assert_eq!(
        permissions.codex.as_ref().unwrap().sandbox_mode.as_deref(),
        Some("read-only")
    );
    let generated = &shown.resolved.manifest.generated[0];
    assert_eq!(generated.content["approval_policy"], "on-request");

    saved(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: revision,
            },
            draft(json!({
                "name": "Perms",
                "runtime": "claude",
                "permissions": { "claude": { "deny": ["mcp__codesense__signal_done"] } }
            })),
            &by("a"),
        )
        .unwrap(),
    );
    let shown = detail(&caller, &key).unwrap();
    let permissions = shown.definition.permissions.clone().unwrap();
    assert!(permissions.codex.is_none(), "a block left out is removed");
    // The required allow stays, the conflicting deny passes through, and the
    // launch warns that the agent cannot report back.
    let content = &shown.resolved.manifest.generated[0].content["permissions"];
    assert!(content["allow"]
        .as_array()
        .unwrap()
        .contains(&json!("mcp__codesense__signal_done")));
    assert_eq!(content["deny"], json!(["mcp__codesense__signal_done"]));
    assert!(!shown.resolved.warnings.is_empty());
}

/// What a hand edit or a newer build wrote beside the fields the editor
/// shows survives an editor save, which always sends every field: unknown
/// keys at the top level, under `mcp`, inside a runtime's permission block
/// (carried back by the form as it was read), and beside the runtime blocks,
/// even when the form clears every permission.
#[test]
fn an_editor_save_keeps_keys_it_does_not_show() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let (key, _) = saved(
        save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(json!({ "name": "Kept", "runtime": "claude" })),
            &by("a"),
        )
        .unwrap(),
    );
    let parsed = parse_key(&key).unwrap();
    let mut agent = service::read_current(&home, &alice(), &parsed).unwrap();
    let yaml = |s: &str| serde_yaml::from_str::<serde_yaml::Value>(s).unwrap();
    agent
        .extra
        .insert("future_field".into(), yaml("{depth: 2}"));
    agent.mcp.extra.insert("future_mcp".into(), yaml("on"));
    agent.permissions = Some(crate::agents::model::Permissions {
        claude: Some(crate::agents::model::ClaudePermissions {
            allow: vec!["Read".into()],
            additional_directories: vec!["../shared".into()],
            ..Default::default()
        }),
        codex: None,
        opencode: None,
        hermes: None,
        extra: [("gemini".to_string(), yaml("{mode: ask}"))].into(),
    });
    let written = service::save(
        &home,
        &alice(),
        service::SaveTarget::Update {
            key: parsed.clone(),
            base_revision: 1,
        },
        agent,
        &by("newer build"),
    )
    .unwrap();

    // The form as the editor sends it: the Claude block carries the keys it
    // does not show back as it read them.
    let (_, revision) = saved(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: written.agent.revision,
            },
            draft(json!({
                "name": "Kept renamed", "description": "", "runtime": "claude", "args": [],
                "launch": "foreground", "mcpServers": [], "startup": [], "defaultPrompt": "",
                "permissions": { "claude": {
                    "allow": ["Read", "Edit"], "additional_directories": ["../shared"]
                } }
            })),
            &by("a"),
        )
        .unwrap(),
    );
    let kept = service::read_current(&home, &alice(), &parsed).unwrap();
    assert_eq!(kept.name, "Kept renamed");
    assert_eq!(kept.extra["future_field"], yaml("{depth: 2}"));
    assert_eq!(kept.mcp.extra["future_mcp"], yaml("on"));
    let permissions = kept.permissions.as_ref().unwrap();
    let claude = permissions.claude.as_ref().unwrap();
    assert_eq!(claude.allow, ["Read", "Edit"]);
    assert_eq!(claude.additional_directories, ["../shared"]);
    assert_eq!(permissions.extra["gemini"], yaml("{mode: ask}"));

    // Every rule cleared: the form sends no permissions, and only the blocks
    // it shows go.
    saved(
        save(
            &caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: revision,
            },
            draft(json!({
                "name": "Kept renamed", "description": "", "runtime": "claude", "args": [],
                "launch": "foreground", "mcpServers": [], "startup": [], "defaultPrompt": "",
                "permissions": null
            })),
            &by("a"),
        )
        .unwrap(),
    );
    let cleared = service::read_current(&home, &alice(), &parsed).unwrap();
    assert_eq!(cleared.extra["future_field"], yaml("{depth: 2}"));
    let permissions = cleared.permissions.as_ref().unwrap();
    assert!(permissions.claude.is_none() && permissions.codex.is_none());
    assert_eq!(permissions.extra["gemini"], yaml("{mode: ask}"));
}

/// The preview lists the servers the team it resolves against offers (the
/// bridge aside): a team agent its own team's, a personal agent the team it
/// would be launched into, so the picker never offers the active team's
/// servers for another team's agent.
#[test]
fn the_preview_offers_the_servers_of_the_team_it_resolves_against() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let content = team_content(&home);
    write(
        &content,
        ".mcp.json",
        br#"{"mcpServers": {"slack": {"command": "slack-mcp"}, "codesense": {"command": "x"}}}"#,
    );
    let caller = caller_for(&home, 1);
    let servers = |reply: AgentPreviewReply| match reply {
        AgentPreviewReply::Resolved { servers, .. } => servers,
        other => panic!("expected a preview, got {other:?}"),
    };
    let form = || draft(json!({ "name": "Servers", "runtime": "claude" }));
    let team_agent = preview(&caller, team_placement(), form(), None, &by("a")).unwrap();
    assert_eq!(servers(team_agent), ["slack"]);
    let personal = Placement::Personal(PersonalOwner {
        github_user_id: 1,
        username: "alice".into(),
        display_name: "Alice".into(),
        avatar_url: None,
        org: "acme/api".into(),
    });
    let into_team = preview(
        &caller,
        personal.clone(),
        form(),
        Some(AgentTeamRef {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        &by("a"),
    )
    .unwrap();
    assert_eq!(servers(into_team), ["slack"]);
    let alone = preview(&caller, personal, form(), None, &by("a")).unwrap();
    assert!(servers(alone).is_empty());
}

/// The same malformed input through the editor's save and through the
/// `agent_create` tool body is refused with the same field and message.
#[test]
fn a_malformed_form_is_refused_exactly_as_the_mcp_tool_refuses_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    for (input, field) in [
        (
            json!({ "name": "Bad", "runtime": "claude", "permissions": { "claude": { "bogus": true } } }),
            "permissions.claude.bogus",
        ),
        (
            json!({ "name": "Bad", "runtime": "claude", "startup": ["../escape.md"] }),
            "startup",
        ),
        (json!({ "name": "Bad", "runtime": "gemini" }), "runtime"),
    ] {
        let tab = save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(input.clone()),
            &by("a"),
        )
        .unwrap();
        let params: AgentCreateParams = serde_json::from_value(input).unwrap();
        let AgentReply::Refused(tool) = agent_tools::create(
            &home,
            &alice(),
            team_placement(),
            &tab_caller(&by("a")),
            params,
        )
        .unwrap() else {
            panic!("the tool should refuse");
        };
        assert_eq!(tool.field.as_deref(), Some(field));
        assert_eq!(
            tab,
            AgentSaveReply::Invalid {
                field: tool.field,
                message: tool.message,
            }
        );
    }
    assert!(service::list(&home, &alice(), true)
        .unwrap()
        .iter()
        .all(|a| a.agent.name != "Bad"));
}

/// A reader that counts the directory listings it is asked for.
#[derive(Default)]
struct ListCounter {
    listed: RefCell<Vec<PathBuf>>,
}

impl ContentReader for ListCounter {
    fn node(&self, path: &Path) -> io::Result<Node> {
        FsContentReader.node(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.listed.borrow_mut().push(dir.to_path_buf());
        FsContentReader.list(dir)
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        FsContentReader.open(path)
    }
}

/// The catalog lists one level only: over a team with 2,000 nested docs it
/// reads each layer's directory exactly once, returns files and folders
/// merged by path (the personal layer winning), and leaves out what no
/// startup path can name.
#[test]
fn the_catalog_lists_one_level_per_layer_and_never_recurses() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    save_personal_agent(&home, &alice(), agent("mine", AgentCli::Claude));
    let team = team_content(&home);
    let personal = roots::personal_content_dir(&home, &alice(), "acme/api").unwrap();
    for i in 0..2_000 {
        write(
            &team,
            &format!("docs/area-{}/deep/nested/doc-{i}.md", i % 40),
            b"x",
        );
    }
    write(&team, "CLAUDE.md", b"team");
    write(&team, ".claude/skills/x/SKILL.md", b"skill");
    write(&team, ".DS_Store", b"cruft");
    std::fs::create_dir_all(team.join("node_modules/pkg")).unwrap();
    write(&personal, "CLAUDE.md", b"mine");
    write(&personal, "notes/today.md", b"mine");

    let layers = content_layers(
        &home,
        &alice(),
        &ManifestTarget {
            context_root: None,
            org: ORG.into(),
            team: TEAM.into(),
            repo: None,
        },
    );
    let reader = ListCounter::default();
    let root = catalog_in(&reader, &layers, None, CatalogPage::default()).unwrap();
    assert!(
        reader.listed.borrow().len() <= layers.len(),
        "at most one listing per layer; unshared layers are not listed: {:?}",
        reader.listed.borrow()
    );
    let paths: Vec<(&str, bool)> = root
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.is_dir))
        .collect();
    assert_eq!(
        paths,
        [
            (".claude", true),
            ("docs", true),
            ("notes", true),
            ("CLAUDE.md", false),
        ]
    );
    let claude_md = root.entries.iter().find(|e| e.path == "CLAUDE.md").unwrap();
    assert!(matches!(claude_md.layer, ContentLayer::Personal { .. }));
    let docs = root.entries.iter().find(|e| e.path == "docs").unwrap();
    assert!(matches!(docs.layer, ContentLayer::Team { .. }));
    assert!(!root.truncated);
    assert_eq!(root.dir, "");

    // Opening a folder lists that level alone.
    let reader = ListCounter::default();
    let docs = catalog_in(&reader, &layers, Some("./docs/"), CatalogPage::default()).unwrap();
    assert_eq!(docs.dir, "docs");
    assert_eq!(docs.entries.len(), 40);
    assert!(docs.entries.iter().all(|e| e.is_dir));
    assert_eq!(docs.entries[0].path, "docs/area-0");
    assert!(reader.listed.borrow().len() <= layers.len());

    // A folder reached through a symlink anywhere along the path is not listed.
    #[cfg(unix)]
    {
        let outside = temp.root().join("outside/sub");
        std::fs::create_dir_all(&outside).unwrap();
        write(&outside, "secret.md", b"x");
        std::os::unix::fs::symlink(temp.root().join("outside"), team.join("linked")).unwrap();
        let through = catalog_in(
            &ListCounter::default(),
            &layers,
            Some("linked/sub"),
            CatalogPage::default(),
        )
        .unwrap();
        assert!(through.entries.is_empty(), "{:?}", through.entries);
        let root = catalog_in(
            &ListCounter::default(),
            &layers,
            None,
            CatalogPage::default(),
        )
        .unwrap();
        assert!(root.entries.iter().all(|e| e.path != "linked"));
    }

    // An unsafe dir is refused, not listed.
    assert!(catalog_in(
        &ListCounter::default(),
        &layers,
        Some("../x"),
        CatalogPage::default()
    )
    .is_err());
}

/// A level with more than a page returns the first page and says where the
/// next starts; the next page and a filter reach every entry past the first
/// page, so the cap only pages the display and never limits what can be picked.
#[test]
fn a_huge_level_pages_and_filters_so_every_entry_is_reachable() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let team = team_content(&home);
    let count = CATALOG_LIMIT + 20;
    for i in 0..count {
        write(&team, &format!("flat/doc-{i:04}.md"), b"x");
    }
    let caller = caller_for(&home, 1);
    let team_ref = AgentTeamRef {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let first = startup_catalog(&caller, &team_ref, Some("flat"), CatalogPage::default()).unwrap();
    assert!(first.truncated);
    assert_eq!(first.total, count);
    assert_eq!(first.entries.len(), CATALOG_LIMIT);
    assert_eq!(first.entries[0].path, "flat/doc-0000.md");
    assert_eq!(first.next_offset, Some(CATALOG_LIMIT));

    let second = startup_catalog(
        &caller,
        &team_ref,
        Some("flat"),
        CatalogPage {
            offset: CATALOG_LIMIT,
            filter: None,
        },
    )
    .unwrap();
    assert!(!second.truncated);
    assert_eq!(second.next_offset, None);
    assert_eq!(second.entries.len(), 20);
    let last = format!("flat/doc-{:04}.md", count - 1);
    assert_eq!(second.entries.last().unwrap().path, last);

    let found = startup_catalog(
        &caller,
        &team_ref,
        Some("flat"),
        CatalogPage {
            offset: 0,
            filter: Some(&format!(" DOC-{:04} ", count - 1)),
        },
    )
    .unwrap();
    assert_eq!(found.total, 1);
    assert_eq!(found.entries.len(), 1);
    assert_eq!(found.entries[0].path, last);
}

/// No agent is special: a team agent saved with the id `controller` is created, edited and
/// launched through the same paths as any other, and `builtin/controller` names nothing.
#[test]
fn seam_a_team_agent_named_controller_is_an_ordinary_agent() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    team_content(&home);
    let alice_caller = caller_for(&home, 1);

    let (key, revision) = saved(
        save(
            &alice_caller,
            AgentFormTarget::Create(team_placement()),
            draft(json!({ "id": "controller", "name": "Ricky", "runtime": "claude" })),
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(
        (key.as_str(), revision),
        (format!("{ORG}/{TEAM}/controller").as_str(), 1)
    );
    let created = service::read_current(&home, &alice(), &AgentKey::parse(&key).unwrap()).unwrap();
    assert_eq!(created.name, "Ricky");
    assert!(created.startup.is_empty(), "nothing seeded from a starter");
    assert!(created.avatar.is_none(), "no crown or mascot face");

    // An edit is an ordinary update against its base revision.
    let (_, revision) = saved(
        save(
            &alice_caller,
            AgentFormTarget::Update {
                key: key.clone(),
                base_revision: 1,
            },
            draft(json!({ "name": "Julian", "runtime": "claude" })),
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(revision, 2);
    let launched = {
        let found =
            crate::agents::launch::find_agent(&home, &alice(), "controller", Some((ORG, TEAM)))
                .unwrap();
        crate::agents::launch::prepare(&home, &alice(), &found, Default::default()).unwrap()
    };
    assert_eq!(launched.agent.key.to_string(), key);
    assert_eq!(launched.name, "Julian");

    // There is no starter by that name to edit in place, and no `org`/`team` routing.
    let caller = tab_caller(&by("agent"));
    let starter: AgentUpdateParams =
        serde_json::from_value(json!({ "id": "builtin/controller", "baseRevision": 1 })).unwrap();
    match agent_tools::update(&home, &alice(), &caller, starter).unwrap() {
        AgentReply::Refused(refusal) => assert_eq!(refusal.code, RefusalCode::NotFound),
        AgentReply::Ok(_) => panic!("builtin/controller saved"),
    }
    let routed: AgentUpdateParams = serde_json::from_value(json!({
        "id": key, "baseRevision": 2, "org": ORG, "team": TEAM,
    }))
    .unwrap();
    match agent_tools::update(&home, &alice(), &caller, routed).unwrap() {
        AgentReply::Refused(refusal) => assert_eq!(refusal.code, RefusalCode::Invalid),
        AgentReply::Ok(_) => panic!("unknown `org`/`team` fields were accepted"),
    }
}

// ---- SW-XS-15: the left nav's agent context overlay -----------------------------

/// `{team}/repos/{host}/{owner}/{repo}/content/` for `owner/repo` on github.com.
fn repo_content(home: &Path, owner: &str, repo: &str) -> PathBuf {
    let org_root = crate::context_repo::path_to_org_root(home, &alice(), ORG).unwrap();
    crate::domain::SourceRepoSlug::new(owner, repo)
        .unwrap()
        .physical_path_key()
        .unwrap()
        .append_to(&org_root.join(TEAM).join("repos"))
        .join("content")
}

fn barb(home: &Path, caller: &Caller) -> String {
    let content = team_content(home);
    write(&content, "CLAUDE.md", b"# Team\n");
    write(
        &content,
        ".claude/skills/review/SKILL.md",
        b"---\nname: review\n---\n",
    );
    write(&content, "_bmad-output/plans/a.md", b"plan\n");
    write(&content, "docs/architecture/adr-1.md", b"adr\n");
    write(
        &repo_content(home, "alderco", "codesense"),
        "docs/architecture/repo-adr.md",
        b"repo adr\n",
    );
    let (key, revision) = saved(
        save(
            caller,
            AgentFormTarget::Create(team_placement()),
            draft(json!({ "name": "Barb", "runtime": "claude" })),
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(revision, 1);
    key
}

fn logicals(view: &AgentContextView) -> Vec<&str> {
    view.files.iter().map(|f| f.logical.as_str()).collect()
}

fn tier_of<'a>(view: &'a AgentContextView, logical: &str) -> &'a ContextTier {
    let file = view.files.iter().find(|f| f.logical == logical).unwrap();
    &view.roots[file.root.unwrap()].tier
}

fn edit(
    caller: &Caller,
    key: &str,
    base: u32,
    op: ContextOp,
    path: &str,
    repo: Option<&str>,
) -> AgentContextEditReply {
    // The nav shows the repo it edits for, unless a test says otherwise.
    context_edit(
        caller,
        ContextEditRequest {
            key,
            base_revision: base,
            op,
            path,
            dest: None,
            entries: &[],
            accepts: None,
            repo,
            view_repo: repo,
        },
        &by("alice"),
    )
    .unwrap()
}

fn saved_view(reply: AgentContextEditReply) -> AgentContextView {
    match reply {
        AgentContextEditReply::Saved { view, .. } => *view,
        other => panic!("expected a save, got {other:?}"),
    }
}

/// AC-1: the overlay is the agent's resolved want set for the repo — the runtime default
/// here, so `_bmad-output` is not in it — with every file's tier and the directory it lives
/// in (sent once per tier), and totals that count every file. With no repo it is what a
/// team-workspace launch gets: the runtime default, just as a repo launch does.
#[test]
fn xs15_context_view_is_the_agents_want_set_for_the_repo() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let view = context_view(&caller, &key, Some("alderco/codesense")).unwrap();
    assert_eq!(view.name, "Barb");
    assert_eq!(view.revision, 1);
    assert!(
        view.context.is_none(),
        "no stored context: the runtime default"
    );
    assert!(view.defaulted);
    assert_eq!(view.repo.as_deref(), Some("github.com/alderco/codesense"));
    let files = logicals(&view);
    assert!(files.contains(&"CLAUDE.md"), "{files:?}");
    assert!(
        files.contains(&".claude/skills/review/SKILL.md"),
        "{files:?}"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("_bmad-output")),
        "{files:?}"
    );
    assert!(!files.iter().any(|f| f.starts_with("docs/")), "{files:?}");
    assert_eq!(view.totals.files, view.files.len() as u64);
    let claude = view
        .files
        .iter()
        .find(|f| f.logical == "CLAUDE.md")
        .unwrap();
    assert_eq!(
        view.roots[claude.root.unwrap()].path.join("CLAUDE.md"),
        team_content(&home).join("CLAUDE.md"),
        "each file's tier root says where it lives, so the nav can open it"
    );
    assert_eq!(
        view.runtime_default
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        [
            "CLAUDE.md",
            ".claude/commands",
            ".claude/hooks",
            ".claude/agents",
            ".claude/skills"
        ]
    );

    let workspace = context_view(&caller, &key, None).unwrap();
    let default_paths: Vec<&str> = workspace
        .entries
        .iter()
        .filter(|e| e.source == crate::agents::manifest_resolve::ContextEntrySource::Default)
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(
        default_paths,
        [
            "CLAUDE.md",
            ".claude/commands",
            ".claude/hooks",
            ".claude/agents",
            ".claude/skills"
        ],
        "no repo: a team-workspace launch's default"
    );
}

/// AC-2 + AC-3: "Add to Barb's context for codesense only" writes the agent's override for
/// that repo as a new revision, and the reply carries the re-resolved preview: the folder
/// from both tiers, the repo tier's own file included. The same edit against the old
/// revision is a conflict that writes nothing.
#[test]
fn xs15_add_for_a_repo_only_writes_the_override_and_a_stale_base_conflicts() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let reply = edit(
        &caller,
        &key,
        1,
        ContextOp::Add,
        "docs/architecture",
        Some("alderco/codesense"),
    );
    let view = saved_view(reply);
    assert_eq!(view.revision, 2);
    let (overrides, problems) =
        service::repo_overrides(&home, &alice(), &parse_key(&key).unwrap()).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    let stored: Vec<(String, Vec<String>)> = overrides
        .into_iter()
        .map(|(repo, o)| (repo, o.add.into_iter().map(|e| e.path).collect()))
        .collect();
    assert_eq!(
        stored,
        [(
            "github.com/alderco/codesense".to_string(),
            vec!["docs/architecture".to_string()]
        )]
    );
    let files = logicals(&view);
    assert!(files.contains(&"docs/architecture/adr-1.md"), "{files:?}");
    assert!(
        files.contains(&"docs/architecture/repo-adr.md"),
        "{files:?}"
    );
    assert!(matches!(
        tier_of(&view, "docs/architecture/repo-adr.md"),
        ContextTier::Repo { repo, .. } if repo == "github.com/alderco/codesense"
    ));
    // Only for that repo: the team workspace does not get it.
    let team_view = context_view(&caller, &key, None).unwrap();
    assert!(!logicals(&team_view).iter().any(|f| f.starts_with("docs/")));
    // The context itself stayed the runtime default (a repo edit writes no `context`).
    assert!(team_view.context.is_none());

    // A teammate's view from before the edit: a conflict, nothing written.
    assert_eq!(
        edit(
            &caller,
            &key,
            1,
            ContextOp::Remove,
            "docs/architecture",
            Some("alderco/codesense")
        ),
        AgentContextEditReply::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(context_view(&caller, &key, None).unwrap().revision, 2);
    // And back out, against the saved revision.
    let view = saved_view(edit(
        &caller,
        &key,
        2,
        ContextOp::Remove,
        "docs/architecture",
        Some("alderco/codesense"),
    ));
    assert!(!logicals(&view).iter().any(|f| f.starts_with("docs/")));
}

/// A first team-wide edit writes the runtime default out explicitly plus the change, keeps
/// every other field, and an edit that cannot be made writes nothing.
#[test]
fn xs15_a_team_wide_edit_makes_the_context_explicit_and_keeps_the_rest() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let view = saved_view(edit(&caller, &key, 1, ContextOp::Add, "docs", None));
    assert_eq!(
        view.context
            .as_ref()
            .map(|c| c.iter().map(|e| e.path.as_str()).collect::<Vec<_>>()),
        Some(vec![
            "CLAUDE.md",
            ".claude/commands",
            ".claude/hooks",
            ".claude/agents",
            ".claude/skills",
            "docs"
        ])
    );
    assert!(!view.defaulted);
    let detail = detail(&caller, &key).unwrap();
    assert_eq!(detail.definition.name, "Barb");
    assert_eq!(detail.definition.runtime, AgentCli::Claude);

    assert!(matches!(
        edit(&caller, &key, 2, ContextOp::Remove, ".claude/skills/review", None),
        AgentContextEditReply::Blocked { reason } if reason.starts_with("Included through .claude/skills")
    ));
    assert_eq!(context_view(&caller, &key, None).unwrap().revision, 2);
}

/// Keys a newer build added — on a `context` entry and on a repo override — survive a nav
/// edit untouched.
#[test]
fn xs15_a_nav_edit_keeps_keys_a_newer_build_added() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);
    saved_view(edit(&caller, &key, 1, ContextOp::Add, "docs", None));
    saved_view(edit(
        &caller,
        &key,
        2,
        ContextOp::Remove,
        ".claude/skills/review",
        Some("alderco/codesense"),
    ));
    // A newer build writes keys this one does not know.
    let parsed = parse_key(&key).unwrap();
    let dir = team_content(&home)
        .parent()
        .unwrap()
        .join("config/agents/barb");
    let agent_yaml = dir.join("agent.yaml");
    let body = std::fs::read_to_string(&agent_yaml).unwrap();
    let body: String = body
        .lines()
        .map(|line| {
            if line.trim_start() == "- path: docs" {
                let indent = line.find("path").unwrap();
                format!("{line}\n{}weight: 3", " ".repeat(indent))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(body.contains("weight: 3"), "{body}");
    std::fs::write(&agent_yaml, body).unwrap();
    let override_yaml = dir.join("repos/github.com/alderco/codesense.yaml");
    let mut o = std::fs::read_to_string(&override_yaml).unwrap();
    o.push_str("note: keep me\n");
    std::fs::write(&override_yaml, o).unwrap();

    let revision = service::read_current(&home, &alice(), &parsed)
        .unwrap()
        .revision;
    saved_view(edit(
        &caller,
        &key,
        revision,
        ContextOp::Add,
        "_bmad-output/plans",
        Some("alderco/codesense"),
    ));
    saved_view(edit(
        &caller,
        &key,
        revision + 1,
        ContextOp::Add,
        "notes",
        None,
    ));
    let body = std::fs::read_to_string(&agent_yaml).unwrap();
    assert!(
        body.contains("weight: 3"),
        "the entry's unknown key survives: {body}"
    );
    let o = std::fs::read_to_string(&override_yaml).unwrap();
    assert!(
        o.contains("note: keep me"),
        "the override's unknown key survives: {o}"
    );
}

/// AC-4: a file moved "to codesense" lands in the repo's content tier, and the agent's want
/// set then lists it from that tier (host segment included).
#[test]
fn xs15_a_file_moved_to_the_repo_is_in_the_want_set_from_the_repo_tier() {
    use crate::commands::move_between_contexts::{
        move_between_contexts_impl, ContextEndpoint, MoveBetweenContextsOutcome,
        MoveBetweenContextsParams,
    };
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);
    write(&team_content(&home), "docs/runbooks/deploy.md", b"deploy\n");
    saved_view(edit(
        &caller,
        &key,
        1,
        ContextOp::Add,
        "docs/runbooks",
        Some("alderco/codesense"),
    ));

    let out = move_between_contexts_impl(
        &home,
        Some(ORG),
        &MoveBetweenContextsParams {
            from: ContextEndpoint::TeamAll {
                org_slug: ORG.into(),
                team_name: TEAM.into(),
            },
            to: ContextEndpoint::TeamRepo {
                org_slug: ORG.into(),
                team_name: TEAM.into(),
                code_repo_slug: crate::domain::SourceRepoSlug::new("alderco", "codesense").unwrap(),
            },
            logical_name: "docs/runbooks/deploy.md".into(),
            to_logical_name: None,
            user_id: 1,
            force: false,
            retain_source: false,
        },
    )
    .unwrap();
    assert_eq!(out, MoveBetweenContextsOutcome::Moved);
    assert!(repo_content(&home, "alderco", "codesense")
        .join("docs/runbooks/deploy.md")
        .is_file());

    let view = context_view(&caller, &key, Some("alderco/codesense")).unwrap();
    assert!(matches!(
        tier_of(&view, "docs/runbooks/deploy.md"),
        ContextTier::Repo { repo, .. } if repo == "github.com/alderco/codesense"
    ));
}

/// N1: a team-wide edit made while the nav shows a repo replies with the view being shown
/// (the repo's), not a team-workspace view the nav would have to drop.
#[test]
fn xs15_a_team_wide_edit_replies_with_the_view_being_shown() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let reply = context_edit(
        &caller,
        ContextEditRequest {
            key: &key,
            base_revision: 1,
            op: ContextOp::Add,
            path: "docs",
            dest: None,
            entries: &[],
            accepts: None,
            repo: None,
            view_repo: Some("alderco/codesense"),
        },
        &by("alice"),
    )
    .unwrap();
    let view = saved_view(reply);
    assert_eq!(view.repo.as_deref(), Some("github.com/alderco/codesense"));
    assert!(logicals(&view).contains(&"docs/architecture/adr-1.md"));
    assert!(logicals(&view).contains(&"docs/architecture/repo-adr.md"));
}

/// A definition the edit would make unsavable is an `invalid` answer that writes nothing.
#[test]
fn xs15_an_edit_the_definition_refuses_is_invalid_and_writes_nothing() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let reply = edit(&caller, &key, 1, ContextOp::Add, "docs/.git/config", None);
    assert!(
        matches!(reply, AgentContextEditReply::Invalid { .. }),
        "{reply:?}"
    );
    let view = context_view(&caller, &key, None).unwrap();
    assert_eq!(view.revision, 1);
    assert!(view.context.is_none());
}

// ---- SW-XS-16: the agent editor's Context section ------------------------------

/// "Customize" writes the runtime default out as the agent's own list; accepting the
/// `projections.yml` suggestion adds its entries (a `dest` kept) in ONE revision made against
/// `baseRevision`, and the same accept against the old revision is a conflict.
#[test]
fn xs16_customize_and_accept_entries_save_against_the_base_revision() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);

    let request = |base: u32, op: ContextOp, entries: &[ContextEntry]| {
        context_edit(
            &caller,
            ContextEditRequest {
                key: &key,
                base_revision: base,
                op,
                path: "",
                dest: None,
                entries,
                accepts: None,
                repo: None,
                view_repo: Some("alderco/codesense"),
            },
            &by("alice"),
        )
        .unwrap()
    };
    let view = saved_view(request(1, ContextOp::Customize, &[]));
    assert_eq!(view.revision, 2);
    assert!(!view.defaulted);
    assert_eq!(
        view.context.as_ref().map(Vec::len),
        Some(5),
        "the runtime default, written out"
    );

    let accepted = [ContextEntry::with_dest("_bmad-output/plans", "plans")];
    let view = saved_view(request(2, ContextOp::AddEntries, &accepted));
    assert_eq!(view.revision, 3);
    assert_eq!(
        view.context.as_ref().and_then(|c| c.last()),
        Some(&accepted[0])
    );
    assert!(
        view.files.iter().any(|f| f.destination == "plans/a.md"),
        "the dest remaps where the files land"
    );
    assert_eq!(
        request(2, ContextOp::AddEntries, &accepted),
        AgentContextEditReply::Conflict {
            current_revision: 3
        }
    );
}

/// The editor's per-repo list can drop any `remove` pattern, a real glob included, and add
/// a path with a `dest` for one repo only.
#[test]
fn xs16_a_repo_override_drops_a_pattern_and_adds_with_a_dest() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);
    let repo = Some("alderco/codesense");
    let run = |base: u32, op: ContextOp, path: &str, dest: Option<&str>| {
        context_edit(
            &caller,
            ContextEditRequest {
                key: &key,
                base_revision: base,
                op,
                path,
                dest,
                entries: &[],
                accepts: None,
                repo,
                view_repo: repo,
            },
            &by("alice"),
        )
        .unwrap()
    };
    saved_view(run(1, ContextOp::Remove, ".claude/skills/review", None));
    let view = saved_view(run(2, ContextOp::DropRemove, ".claude/skills/review", None));
    assert!(view.repo_overrides.iter().all(|o| o.remove.is_empty()));
    assert!(logicals(&view).contains(&".claude/skills/review/SKILL.md"));
    let view = saved_view(run(3, ContextOp::Add, "docs/architecture", Some("arch")));
    assert_eq!(
        view.repo_overrides[0].add,
        [ContextEntry::with_dest("docs/architecture", "arch")]
    );
    assert!(view.files.iter().any(|f| f.destination == "arch/adr-1.md"));
    assert!(matches!(
        run(4, ContextOp::DropRemove, "nothing/*", None),
        AgentContextEditReply::Blocked { .. }
    ));

    // The base covers `docs/architecture` too once `docs` is in the context: dropping
    // the repo's add must not leave the repo removing it (SHOULD-1).
    let view = saved_view(
        context_edit(
            &caller,
            ContextEditRequest {
                key: &key,
                base_revision: 4,
                op: ContextOp::Add,
                path: "docs",
                dest: None,
                entries: &[],
                accepts: None,
                repo: None,
                view_repo: repo,
            },
            &by("alice"),
        )
        .unwrap(),
    );
    assert_eq!(view.revision, 5);
    let view = saved_view(run(5, ContextOp::DropAdd, "docs/architecture", None));
    assert!(
        view.repo_overrides
            .iter()
            .all(|o| o.add.is_empty() && o.remove.is_empty()),
        "{:?}",
        view.repo_overrides
    );
    assert!(view
        .files
        .iter()
        .any(|f| f.destination == "docs/architecture/adr-1.md"));
}

/// M1 + SHOULD-3: the view carries the `projections.yml` suggestion, and accepting a
/// nested entry that lands elsewhere (the projection's own default `dest`) adds it and
/// retires the suggestion in the same reply: no second read, no accept loop.
#[test]
fn xs16_the_view_carries_the_suggestion_and_accepting_retires_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let key = barb(&home, &caller);
    let config = crate::context_repo::path_to_team_config_dir(
        &crate::context_repo::path_to_org_root(&home, &alice(), ORG).unwrap(),
        TEAM,
    );
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("projections.yml"),
        "projections:\n  - {key: review, source_subdir: .claude/skills/review}\n",
    )
    .unwrap();

    let view = context_view(&caller, &key, None).unwrap();
    let suggestion = view.context_suggestion.expect("a suggestion");
    let entries: Vec<ContextEntry> = suggestion
        .entries
        .iter()
        .map(|e| ContextEntry::with_dest(e.path.clone(), e.dest.clone().unwrap()))
        .collect();
    assert_eq!(
        entries,
        [ContextEntry::with_dest(".claude/skills/review", "review")]
    );
    let view = saved_view(
        context_edit(
            &caller,
            ContextEditRequest {
                key: &key,
                base_revision: 1,
                op: ContextOp::AddEntries,
                path: "",
                dest: None,
                entries: &entries,
                accepts: Some(&suggestion.fingerprint),
                repo: None,
                view_repo: None,
            },
            &by("alice"),
        )
        .unwrap(),
    );
    assert!(view
        .context
        .as_ref()
        .unwrap()
        .contains(&ContextEntry::with_dest(".claude/skills/review", "review")));
    assert_eq!(view.context_suggestion, None);
    assert!(view
        .files
        .iter()
        .any(|f| f.destination == "review/SKILL.md"));

    // SHOULD-1 (r2): one-time. Removing the accepted entry does not bring the same
    // suggestion back; a changed projections.yml is a new one.
    let view = saved_view(edit(
        &caller,
        &key,
        2,
        ContextOp::Remove,
        ".claude/skills/review",
        None,
    ));
    assert!(!view
        .context
        .as_ref()
        .unwrap()
        .iter()
        .any(|e| e.path == ".claude/skills/review"));
    assert_eq!(view.context_suggestion, None, "accepting dismissed it");
    std::fs::write(
        config.join("projections.yml"),
        "projections:\n  - {key: review, source_subdir: .claude/skills/review}\n  - {key: kb, source_subdir: kb}\n",
    )
    .unwrap();
    let fresh = context_view(&caller, &key, None)
        .unwrap()
        .context_suggestion
        .expect("a changed projections.yml is a new suggestion");
    assert_ne!(fresh.fingerprint, suggestion.fingerprint);
}

/// r3 LOW-1: an IPC entry's `keep` carries only the keys beside `path`/`dest`; either of
/// those inside it is refused by name, and unknown top-level keys are refused too.
#[test]
fn xs16_an_ipc_entry_refuses_path_or_dest_inside_keep() {
    let entry: ContextEntryIpc = serde_json::from_value(json!({
        "path": "docs",
        "dest": "kb",
        "keep": { "weight": 3 }
    }))
    .unwrap();
    let entry = entry.into_entry().unwrap();
    assert_eq!(entry.dest.as_deref(), Some("kb"));
    assert_eq!(entry.extra.get("weight"), Some(&serde_yaml::Value::from(3)));

    for key in ["path", "dest"] {
        let bad: ContextEntryIpc = serde_json::from_value(json!({
            "path": "docs",
            "keep": { key: "elsewhere" }
        }))
        .unwrap();
        let err = bad.into_entry().unwrap_err();
        assert!(err.contains(&format!("`{key}`")), "{err}");
    }
    assert!(
        serde_json::from_value::<ContextEntryIpc>(json!({ "path": "docs", "weight": 3 })).is_err(),
        "an unknown key outside `keep` is refused"
    );
}

#[tokio::test]
async fn saved_default_drives_shared_registration_startup_inspection_and_normal_release() {
    use super::super::role_startup::{
        self,
        tests::{session_row, StartupHost},
    };
    use crate::mcp::session_list::SessionState;
    use crate::weft::{role_inbox as inbox, role_state};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    let (key, _) = saved(
        save(
            &caller,
            AgentFormTarget::Create(team_placement()),
            draft(json!({"name":"Review bot", "runtime":"claude", "role":"reviewer"})),
            &by("alice"),
        )
        .unwrap(),
    );
    let host = StartupHost::new(home.join("startup.sqlite3"));
    let store = host.store();
    let now = chrono::Utc::now().timestamp();
    for (text, hash) in [("first", "a"), ("second", "b")] {
        inbox::send_role_message_commit(
            &store, "human", "reviewer", text, None, None, None, hash, now, None, None, None,
        )
        .unwrap();
    }
    let team = super::super::dashboard_launch::LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let registry = super::super::dashboard_launch::DashboardLaunchState::new(8);
    let mut rows = Vec::new();
    for (session, choice, expected) in [
        ("session-a", None, Some("reviewer")),
        ("session-b", None, Some("reviewer")),
        ("no-role", Some(None), None),
        ("override", Some(Some("qa".into())), Some("qa")),
    ] {
        // Resolve the persisted default for every launch, just as an ordinary saved-agent start does.
        let launch =
            crate::agents::launch::prepare_named(&home, &alice(), &key, Default::default())
                .unwrap();
        let role =
            crate::queue_roles::effective_role(launch.queue_role.as_deref(), choice.as_ref())
                .unwrap();
        assert_eq!(role.as_deref(), expected);
        registry.begin(session).unwrap();
        let (store, registry) = (&store, &registry);
        role_startup::start_argv(
            &host,
            Some(&team),
            role.as_deref(),
            session,
            launch.runtime,
            launch.launch.args.clone(),
            Some("/review queued work"),
            |plan| async move {
                // Only the provider is simulated. Production has already registered, inspected
                // pending mail and built the startup argv before this callback can run.
                assert!(plan.in_argv);
                assert!(plan.pty_prompt.is_none());
                let task = plan.launch_args.last().unwrap();
                assert!(task.starts_with("/review queued work"));
                let queues = role_state::role_state_read(&store, None, now, 0).unwrap();
                match expected {
                    Some(role) => {
                        let queue = queues.iter().find(|q| q.role == role).unwrap();
                        assert!(queue.registrations.iter().any(|r| r.instance_id == session
                            && r.session_id.as_deref() == Some(session)
                            && r.live));
                        assert!(task.contains(&format!("You hold role {role}")));
                        if role == "reviewer" {
                            assert!(task.contains("2 messages are pending"));
                            assert_eq!((queue.counts.pending, queue.counts.claimed), (2, 0));
                        }
                    }
                    None => {
                        assert!(!task.contains("You hold role"));
                        assert!(!queues
                            .iter()
                            .flat_map(|q| &q.registrations)
                            .any(|r| r.instance_id == session));
                    }
                }
                registry.bind(session, session, Some("mcp"))?;
                Ok(())
            },
        )
        .await
        .unwrap();
        rows.push(session_row(session, SessionState::Running));
    }
    let queues = role_state::role_state_read(&store, Some("reviewer"), now, 0).unwrap();
    assert_eq!(queues[0].registrations.iter().filter(|r| r.live).count(), 2);
    let generation = |session: &str| {
        queues[0]
            .registrations
            .iter()
            .find(|r| r.instance_id == session)
            .unwrap()
            .generation
    };
    let a = inbox::claim_role_messages(
        &store,
        "reviewer",
        "session-a",
        generation("session-a"),
        1,
        now,
    )
    .unwrap();
    let b = inbox::claim_role_messages(
        &store,
        "reviewer",
        "session-b",
        generation("session-b"),
        1,
        now,
    )
    .unwrap();
    assert_ne!(a[0].message_id, b[0].message_id);
    let running = super::super::role_inbox::renew_store(&store, now, &rows).unwrap();
    assert_eq!(running.renewed, 3);
    for row in &mut rows {
        registry.release_for_session(&row.handle);
        row.state = SessionState::Exited;
    }
    // The app's normal renewal tick uses these same session-list liveness decisions.
    let report = super::super::role_inbox::renew_store(&store, now, &rows).unwrap();
    assert_eq!(report.released, 3);
    let queue = role_state::role_state_read(&store, Some("reviewer"), now, 0).unwrap();
    assert_eq!(queue[0].counts.pending, 2);
    assert!(queue[0].registrations.iter().all(|r| !r.live));
    let reloaded = crate::agents::service::list(&home, &alice(), false)
        .unwrap()
        .into_iter()
        .find(|a| a.key.to_string() == key)
        .unwrap();
    assert_eq!(reloaded.agent.role.as_deref(), Some("reviewer"));
}

const MODEL_ENDPOINT_REQUIRED: &str =
    "Hermes needs a model. Set a local endpoint in Edit → Runs on. API-key providers come later.";

#[test]
fn seam_hermes_editor_preview_and_save_follow_its_model_endpoint() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let caller = caller_for(&home, 1);
    for (model, allowed) in [
        (json!(null), false),
        (
            json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"local"}),
            true,
        ),
    ] {
        let form = json!({"name": format!("Hermes preview {allowed}"), "runtime":"hermes", "context":[], "startup":[], "startupSettings":{"model":model}});
        let shown = serde_json::to_value(resolved(
            preview(
                &caller,
                team_placement(),
                draft(form.clone()),
                None,
                &by("alice"),
            )
            .unwrap(),
        ))
        .unwrap();
        assert_eq!(shown["launch_admission"]["allowed"], allowed, "{shown}");
        if !allowed {
            assert_eq!(shown["launch_admission"]["reason"], MODEL_ENDPOINT_REQUIRED);
        }
        let saved = serde_json::to_value(
            save(
                &caller,
                AgentFormTarget::Create(team_placement()),
                draft(form),
                &by("alice"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["outcome"], "saved", "{saved}");
    }
}

#[test]
fn registered_model_endpoint_preview_blocks_missing_and_allows_only_keyless_local_models() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    for (model, allowed) in [
        (json!(null), false),
        (
            json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"local"}),
            true,
        ),
        (
            json!({"provider":"custom", "base_url":"http://localhost:11434/v1", "default":"local"}),
            true,
        ),
        (
            json!({"provider":"custom", "base_url":"http://[::1]:11434/v1", "default":"local"}),
            true,
        ),
        (
            json!({"provider":"openai", "base_url":"http://127.0.0.1:11434/v1", "default":"local"}),
            false,
        ),
        (
            json!({"provider":"custom", "base_url":"https://api.openai.com/v1", "default":"local"}),
            false,
        ),
        (
            json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1"}),
            false,
        ),
    ] {
        let mut definition = agent("registered-model", AgentCli::Hermes);
        definition.set_startup_settings(Some(
            serde_json::from_value(json!({"model":model})).unwrap(),
        ));
        let shown = crate::agents::resolve::preview_agent(
            &home,
            &alice(),
            &definition,
            &team_placement(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(shown.launch_admission.allowed, allowed, "{model}");
        assert_eq!(
            shown.launch_admission.reason.as_deref(),
            (!allowed).then_some(MODEL_ENDPOINT_REQUIRED)
        );
        assert!(
            shown.manifest.errors.is_empty(),
            "{:?}",
            shown.manifest.errors
        );
    }
}

#[test]
fn registered_model_endpoint_inline_settings_persist_and_native_model_overrides_win() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut definition = agent("registered-inline-model", AgentCli::Hermes);
    definition.revision = 1;
    definition.args = vec!["--model=first".into(), "-m".into(), "final".into()];
    definition.set_startup_settings(Some(serde_json::from_value(json!({"model":{"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"configured"}})).unwrap()));
    let shown = crate::agents::resolve::preview_agent(
        &home,
        &alice(),
        &definition,
        &team_placement(),
        Default::default(),
    )
    .unwrap();
    assert!(shown.launch_admission.allowed);
    assert_eq!(shown.launch_admission.model.unwrap()["default"], "final");
    let key = crate::agents::service::save_typed_for_test(
        &home,
        &alice(),
        &definition,
        &team_placement(),
    )
    .unwrap();
    let stored = service::read_current(&home, &alice(), &key).unwrap();
    assert_eq!(
        stored.startup_settings().unwrap()["model"]["default"],
        "configured"
    );
}

#[test]
fn seam_model_endpoint_admission_does_not_require_models_for_other_clis() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    for cli in AgentCli::ALL.into_iter().filter(|cli| {
        crate::agents::adapter::desktop(*cli)
            .model_endpoint
            .is_none()
    }) {
        let runtime = AgentCli::as_str(cli);
        let shown = resolved(preview(
            &caller_for(&home, 1), team_placement(),
            draft(json!({ "name": "Existing CLI", "runtime": runtime, "context": [], "startup": [] })),
            None, &by("alice"),
        ).unwrap());
        let json = serde_json::to_value(shown).unwrap();
        assert_eq!(
            json["launch_admission"]["allowed"], true,
            "{runtime}: {json}"
        );
    }
}
