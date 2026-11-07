// SPDX-License-Identifier: MIT
//! Seam tests for the agent model and service: real temp homes, real org
//! layouts, the real accounts store and the real projection path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use crate::context_repo::scratch_space::SCRATCH_TEAM;
use crate::context_repo::{path_to_org_root, path_to_team_content_dir};
use crate::domain::Principal;
use crate::install::agent_account_store::{create_profile, AgentProvider};
use crate::routines::model::{Author, AuthorKind};
use crate::routines::PersonalOwner;

use super::binding::{self, BindingError};
use super::defs;
use super::keys::AgentKey;
use super::manifest::{
    content_hash, entry_hash, json_hash, AgentRevisionRef, ContentLayer, EntryOrigin,
    EssentialEntry, GeneratedEntry, GeneratedTarget, ManifestEntry, ManifestError,
    ManifestErrorCode, ManifestFile, ManifestTotals, StartupManifest,
};
use super::model::{Agent, LaunchMode, McpAccess, ValidationError};
use super::paths::{StartupPath, StartupPathError};
use super::roots;
use super::service::{self, AgentError, AgentReferrers, NoReferrers, Placement, SaveTarget};
use super::AgentScope;
use codesense_pty_core::agent_cli::AgentCli;

const ORG: &str = "acme/api";
const TEAM: &str = "backend";

const PEOPLE_YML: &str = r#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice A
  - github_user_id: 2
    github_username: bob
"#;
const TEAMS_YML: &str = r#"
teams:
  - name: backend
  - name: frontend
"#;

fn alice() -> Principal {
    Principal::from(1u64)
}

fn bob() -> Principal {
    Principal::from(2u64)
}

/// A locally-cloned org for `user_id`, with `backend` synced.
fn write_org(home: &Path, user_id: u64, slug: &str) -> PathBuf {
    let org_root = path_to_org_root(home, &Principal::from(user_id), slug).unwrap();
    if slug == ORG {
        crate::context_repo::scratch_space::set_active_org(home, Some(ORG));
    }
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {slug}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(org_root.join("people.yml"), PEOPLE_YML).unwrap();
    std::fs::write(org_root.join("teams.yml"), TEAMS_YML).unwrap();
    std::fs::create_dir_all(path_to_team_content_dir(&org_root, TEAM)).unwrap();
    org_root
}

fn team() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

fn personal(user_id: u64, name: &str) -> Placement {
    Placement::Personal(PersonalOwner {
        github_user_id: user_id,
        username: name.into(),
        display_name: name.into(),
        avatar_url: None,
        org: "acme/api".into(),
    })
}

fn agent(id: &str) -> Agent {
    Agent {
        role: None,
        id: id.into(),
        name: format!("Agent {id}"),
        description: "Sweeps Slack and files actionable stories.".into(),
        created_by: String::new(),
        runtime: AgentCli::Claude,
        args: vec!["--model".into(), "opus".into()],
        launch: LaunchMode::Foreground,
        mcp: McpAccess {
            bridge_tools: None,
            servers: vec!["slack".into(), "github".into()],
            extra: BTreeMap::new(),
        },
        startup: vec![
            "CLAUDE.md".into(),
            "./.claude/skills/bmad-build/".into(),
            ".claude//commands/bmad-help.md".into(),
        ],
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 0,
        archived_at: None,
        extra: BTreeMap::new(),
        env: Default::default(),
    }
}

fn key(id: &str) -> AgentKey {
    AgentKey::new(ORG, TEAM, id)
}

fn create(home: &Path, who: &Principal, id: &str) -> Agent {
    service::save(
        home,
        who,
        SaveTarget::Create(team()),
        agent(id),
        &Author::user(who.as_str()),
    )
    .unwrap()
    .agent
}

fn agent_dir(home: &Path, who: &Principal, id: &str) -> PathBuf {
    roots::team_agents_dir(home, who, ORG, TEAM)
        .unwrap()
        .unwrap()
        .join(id)
}

/// [`service::list`] without the starters, which every list ends with.
fn saved(
    home: &Path,
    who: &Principal,
    include_archived: bool,
) -> Result<Vec<super::DiscoveredAgent>, AgentError> {
    service::list(home, who, include_archived)
        .map(|found| found.into_iter().filter(|a| !a.key.is_builtin()).collect())
}

fn yaml(text: &str) -> Agent {
    serde_yaml::from_str(text).unwrap()
}

// ---------------------------------------------------------------------------
// Schema and validation
// ---------------------------------------------------------------------------

const FULL_YAML: &str = r#"
id: triage
name: Triage
description: Sweeps Slack and files actionable stories.
created_by: "1234567"
runtime: claude
args: ["--model", "opus"]
env:
  CLAUDE_CODE_DISABLE_AUTO_MEMORY: "1"
  CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD: "1"
  MODE: "0755"
launch: headless
mcp:
  servers: [slack, github]
startup:
  - CLAUDE.md
  - .claude/skills/bmad-build
permissions:
  claude:
    allow: ["Bash(npm test)", "mcp__slack__*"]
    deny: ["Bash(git push --force:*)"]
    default_mode: acceptEdits
  codex:
    approval_policy: on-request
    sandbox_mode: workspace-write
    future_codex_key: 1
default_prompt: "go"
revision: 3
account: claude-work
future_field: {nested: true}
"#;

#[test]
fn definition_round_trips_unknown_keys_and_an_account_key_applies_nowhere() {
    let def = yaml(FULL_YAML);
    def.validate().unwrap();
    assert_eq!(def.launch, LaunchMode::Headless);
    assert!(def.extra.contains_key("account"));
    assert!(def.extra.contains_key("future_field"));
    // `env` is a field of its own, never carried as an unknown key; each value is
    // the string as written.
    assert!(!def.extra.contains_key("env"));
    assert_eq!(
        def.env,
        BTreeMap::from([
            (
                "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string(),
                "1".to_string()
            ),
            (
                "CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string(),
                "1".to_string()
            ),
            ("MODE".to_string(), "0755".to_string()),
        ])
    );

    let again: Agent = serde_yaml::from_str(&serde_yaml::to_string(&def).unwrap()).unwrap();
    assert_eq!(again, def, "every key survives a write");
    let claude = again.permissions.as_ref().unwrap().claude.as_ref().unwrap();
    assert_eq!(claude.allow, vec!["Bash(npm test)", "mcp__slack__*"]);
    assert_eq!(claude.deny, vec!["Bash(git push --force:*)"]);
    assert_eq!(claude.default_mode.as_deref(), Some("acceptEdits"));
    // The other runtime's block, unknown key included, is carried untouched
    // and ignored: it would not validate on its own runtime.
    let codex = again.permissions.as_ref().unwrap().codex.as_ref().unwrap();
    assert!(codex.extra.contains_key("future_codex_key"));
    let on_codex = Agent {
        runtime: AgentCli::Codex,
        ..again.clone()
    };
    assert!(matches!(
        on_codex.validate(),
        Err(ValidationError::UnknownPermissionKey { .. })
    ));

    // Defaults: foreground, bridge-only MCP, nothing at startup, no env (and
    // none written back).
    let bare = yaml("id: x\nname: X\nruntime: codex\n");
    assert_eq!(bare.launch, LaunchMode::Foreground);
    assert!(bare.mcp.servers.is_empty() && bare.startup.is_empty() && bare.env.is_empty());
    assert!(!serde_yaml::to_string(&bare).unwrap().contains("env"));
}

/// An `env` value written by hand as anything but a string still reads: the definition fails
/// validation naming the variable ("quote it"), never parses as a whole-file error, and the value
/// is never taken as its text, so no save rewrites `0o755` as `"493"` or `1.10` as `"1.1"`.
#[test]
fn an_env_value_that_is_not_a_string_is_a_validation_error_naming_it() {
    for value in [
        "1", "true", "", "~", "null", "0o755", "1.10", "[a]", "{a: b}",
    ] {
        let def = yaml(&format!(
            "id: x\nname: X\nruntime: claude\nrevision: 1\nenv:\n  GOOD: \"ok\"\n  BAD: {value}\n"
        ));
        let refused = def
            .validate()
            .expect_err(&format!("{value:?} reads as {:?}", def.env));
        assert_eq!(
            refused,
            ValidationError::EnvValueNotString("BAD".into()),
            "{value:?}"
        );
        assert!(refused.to_string().contains("quote it"), "{refused}");
        assert_eq!(def.env.get("GOOD").map(String::as_str), Some("ok"));
        assert!(
            def.env.get("BAD").is_none(),
            "{value:?} is never read as text"
        );
        assert!(!def.extra.contains_key("env"));
    }
    // A plain `0755` is a YAML 1.2 string: it reads as written, never as a number.
    let def = yaml("id: x\nname: X\nruntime: claude\nrevision: 1\nenv:\n  MODE: 0755\n");
    def.validate().unwrap();
    assert_eq!(def.env.get("MODE").map(String::as_str), Some("0755"));
}

#[test]
fn an_unknown_runtime_is_refused_naming_it() {
    let err = serde_yaml::from_str::<Agent>("id: x\nname: X\nruntime: my-pack\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("my-pack"), "{err}");
    let err = "gemini".parse::<AgentCli>().unwrap_err().to_string();
    assert_eq!(
        err,
        format!(
            "unknown runtime \"gemini\"; an agent runs on {}",
            AgentCli::join_names(AgentCli::ALL.map(AgentCli::as_str))
        )
    );
}

#[test]
fn the_bridge_tools_error_names_every_enforcing_runtime() {
    let enforcing: Vec<&str> = AgentCli::ALL
        .into_iter()
        .map(crate::agents::adapter::desktop)
        .filter(|desktop| desktop.enforces_bridge_tool_allowlist)
        .map(|desktop| desktop.starter.name)
        .collect();
    assert!(enforcing.len() > 1);
    assert_eq!(
        ValidationError::BridgeToolsUnsupported.to_string(),
        format!(
            "mcp.bridge_tools is supported only for {} agents",
            AgentCli::join_names(enforcing)
        )
    );
}

#[test]
fn bridge_tool_allowlist_validation_follows_the_runtime_capability() {
    for tools in [vec![], vec!["signal_done".to_string()]] {
        let mut definition = agent("bridge-reviewer");
        definition.revision = 1;
        definition.mcp.bridge_tools = Some(tools);
        assert_eq!(
            definition.validate(),
            Err(ValidationError::BridgeToolsUnsupported)
        );
        definition.runtime = AgentCli::Codex;
        assert_eq!(definition.validate(), Ok(()));
    }
}

#[test]
fn every_validation_error_is_typed() {
    let saved = |mut a: Agent| {
        a.revision = 1;
        a.validate()
    };
    let cases: Vec<(Agent, ValidationError)> = vec![
        (
            Agent {
                id: "Bad_Id".into(),
                ..agent("x")
            },
            ValidationError::InvalidId("Bad_Id".into()),
        ),
        (
            Agent {
                id: "a".repeat(65),
                ..agent("x")
            },
            ValidationError::InvalidId("a".repeat(65)),
        ),
        (
            Agent {
                name: " ".into(),
                ..agent("x")
            },
            ValidationError::EmptyName,
        ),
        (
            Agent {
                args: vec!["".into()],
                ..agent("x")
            },
            ValidationError::InvalidArg("".into()),
        ),
        (
            Agent {
                args: vec!["a\0b".into()],
                ..agent("x")
            },
            ValidationError::InvalidArg("a\0b".into()),
        ),
        (
            Agent {
                args: vec!["echo".into(), "&&".into()],
                ..agent("x")
            },
            ValidationError::InvalidArg("&&".into()),
        ),
        (
            Agent {
                args: vec!["|".into()],
                ..agent("x")
            },
            ValidationError::InvalidArg("|".into()),
        ),
        (
            Agent {
                env: BTreeMap::from([("1ST".to_string(), "x".to_string())]).into(),
                ..agent("x")
            },
            ValidationError::InvalidEnvName("1ST".into()),
        ),
        (
            Agent {
                env: BTreeMap::from([("A-B".to_string(), "x".to_string())]).into(),
                ..agent("x")
            },
            ValidationError::InvalidEnvName("A-B".into()),
        ),
        (
            Agent {
                env: BTreeMap::from([("N".repeat(129), "x".to_string())]).into(),
                ..agent("x")
            },
            ValidationError::InvalidEnvName("N".repeat(129)),
        ),
        (
            Agent {
                env: BTreeMap::from([("NUL".to_string(), "a\0b".to_string())]).into(),
                ..agent("x")
            },
            ValidationError::InvalidEnvValue("NUL".into()),
        ),
        (
            Agent {
                env: BTreeMap::from([("LONG".to_string(), "x".repeat(4097))]).into(),
                ..agent("x")
            },
            ValidationError::InvalidEnvValue("LONG".into()),
        ),
        (
            Agent {
                env: (0..65)
                    .map(|i| (format!("V{i}"), "x".to_string()))
                    .collect(),
                ..agent("x")
            },
            ValidationError::TooManyEnvVars(65),
        ),
        (
            Agent {
                mcp: McpAccess {
                    bridge_tools: None,
                    servers: vec!["".into()],
                    extra: BTreeMap::new(),
                },
                ..agent("x")
            },
            ValidationError::InvalidMcpServer("".into()),
        ),
        (
            Agent {
                mcp: McpAccess {
                    bridge_tools: None,
                    servers: vec!["slack".into(), "slack".into()],
                    extra: BTreeMap::new(),
                },
                ..agent("x")
            },
            ValidationError::DuplicateMcpServer("slack".into()),
        ),
        (
            Agent {
                mcp: McpAccess {
                    bridge_tools: None,
                    servers: vec!["codesense".into()],
                    extra: BTreeMap::new(),
                },
                ..agent("x")
            },
            ValidationError::BridgeServerListed,
        ),
        (
            Agent {
                startup: vec!["/etc/hosts".into()],
                ..agent("x")
            },
            ValidationError::InvalidStartupPath(StartupPathError::Absolute("/etc/hosts".into())),
        ),
        (
            Agent {
                startup: vec!["docs/../../x".into()],
                ..agent("x")
            },
            ValidationError::InvalidStartupPath(StartupPathError::ParentSegment(
                "docs/../../x".into(),
            )),
        ),
        (
            Agent {
                startup: vec![".claude/skills/*".into()],
                ..agent("x")
            },
            ValidationError::InvalidStartupPath(StartupPathError::Glob(".claude/skills/*".into())),
        ),
        (
            Agent {
                startup: vec![".git/HEAD".into()],
                ..agent("x")
            },
            ValidationError::InvalidStartupPath(StartupPathError::Reserved {
                path: ".git/HEAD".into(),
                dir: ".git".into(),
            }),
        ),
        (
            Agent {
                startup: vec!["a/b".into(), "./a//b/".into()],
                ..agent("x")
            },
            ValidationError::DuplicateStartupPath("a/b".into()),
        ),
        (
            Agent {
                startup: vec![
                    ".claude/skills/bmad-build/".into(),
                    ".claude/skills/bmad-build/SKILL.md".into(),
                ],
                ..agent("x")
            },
            ValidationError::OverlappingStartupPath {
                path: ".claude/skills/bmad-build/SKILL.md".into(),
                covered_by: ".claude/skills/bmad-build".into(),
            },
        ),
        (
            Agent {
                startup: vec![
                    ".claude/skills/bmad-build/SKILL.md".into(),
                    ".claude/skills/bmad-build".into(),
                ],
                ..agent("x")
            },
            ValidationError::OverlappingStartupPath {
                path: ".claude/skills/bmad-build".into(),
                covered_by: ".claude/skills/bmad-build/SKILL.md".into(),
            },
        ),
    ];
    for (def, expected) in cases {
        assert_eq!(saved(def), Err(expected.clone()), "{expected}");
    }
    assert_eq!(agent("x").validate(), Err(ValidationError::InvalidRevision));
    // Sibling paths that only share a prefix are not nested.
    assert!(saved(Agent {
        startup: vec!["a/b".into(), "a/bc".into()],
        ..agent("x")
    })
    .is_ok());
    // An env at its limits saves: 64 variables, a 128-byte name, a 4096-byte
    // value, an empty value. Any name a launch can carry is allowed.
    let mut env: BTreeMap<String, String> = (0..62)
        .map(|i| (format!("V{i}"), "x".to_string()))
        .collect();
    env.insert("N".repeat(128), "x".repeat(4096));
    env.insert("_EMPTY".into(), String::new());
    assert!(saved(Agent {
        env: env.into(),
        ..agent("x")
    })
    .is_ok());
    assert!(saved(Agent {
        env: BTreeMap::from([("CODESENSE_TAB_SESSION_ID".to_string(), "x".to_string())]).into(),
        ..agent("x")
    })
    .is_ok());
}

#[test]
fn permissions_are_shape_checked_for_the_own_runtime_only() {
    let claude = |block: &str| {
        let mut a = yaml(&format!(
            "id: x\nname: X\nruntime: claude\nrevision: 1\npermissions:\n{block}"
        ));
        a.revision = 1;
        a.validate()
    };
    // Every key of Claude's native `permissions` block passes through
    // verbatim, under either spelling of the camelCase ones.
    let native = yaml(
        "id: x\nname: X\nruntime: claude\nrevision: 1\npermissions:\n  claude:\n    allow: [\"Bash(npm test)\"]\n    ask: [\"Bash(git push:*)\"]\n    deny: [\"WebFetch\"]\n    additionalDirectories: [\"../docs\"]\n    defaultMode: acceptEdits\n    disableBypassPermissionsMode: disable\n",
    );
    assert_eq!(native.validate(), Ok(()));
    let block = native
        .permissions
        .as_ref()
        .unwrap()
        .claude
        .as_ref()
        .unwrap();
    assert_eq!(block.ask, vec!["Bash(git push:*)".to_string()]);
    assert_eq!(block.additional_directories, vec!["../docs".to_string()]);
    assert_eq!(block.default_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(
        block.disable_bypass_permissions_mode.as_deref(),
        Some("disable")
    );
    assert!(block.extra.is_empty());
    let snake = yaml(
        "id: x\nname: X\nruntime: claude\nrevision: 1\npermissions:\n  claude:\n    ask: [\"Bash(git push:*)\"]\n    default_mode: plan\n",
    );
    assert_eq!(
        snake
            .permissions
            .as_ref()
            .unwrap()
            .claude
            .as_ref()
            .unwrap()
            .default_mode
            .as_deref(),
        Some("plan")
    );
    let back: Agent = serde_yaml::from_str(&serde_yaml::to_string(&native).unwrap()).unwrap();
    assert_eq!(back, native);
    assert_eq!(
        claude("  claude:\n    ask: [y, y]\n"),
        Err(ValidationError::DuplicatePermission("y".into()))
    );
    assert_eq!(
        claude("  claude:\n    allow: [x]\n    sandbox: true\n"),
        Err(ValidationError::UnknownPermissionKey {
            runtime: AgentCli::Claude,
            key: "sandbox".into(),
        })
    );
    assert_eq!(
        claude("  claude:\n    allow: [\"Bash(ls)\", \"Bash(ls)\"]\n"),
        Err(ValidationError::DuplicatePermission("Bash(ls)".into()))
    );
    assert_eq!(
        claude("  claude:\n    deny: [\"\"]\n"),
        Err(ValidationError::InvalidPermission("".into()))
    );
    assert_eq!(
        claude("  claude:\n    default_mode: \"\"\n"),
        Err(ValidationError::InvalidPermission("".into()))
    );
    // The other runtime's block is never checked: switching runtime loses
    // nothing and refuses nothing.
    assert!(claude("  codex:\n    sandbox_mode: \"\"\n    unheard_of: 1\n").is_ok());
    // An unknown runtime block is carried, not refused.
    assert!(claude("  gemini:\n    anything: 1\n").is_ok());

    let codex = yaml(
        "id: x\nname: X\nruntime: codex\nrevision: 1\npermissions:\n  codex:\n    approval_policy: on-request\n    network: true\n",
    );
    assert_eq!(
        codex.validate(),
        Err(ValidationError::UnknownPermissionKey {
            runtime: AgentCli::Codex,
            key: "network".into(),
        })
    );
}

#[test]
fn escalation_is_shown_for_bypass_flags_and_bypassing_permissions() {
    let with = |runtime: &str, rest: &str| {
        yaml(&format!("id: x\nname: X\nruntime: {runtime}\n{rest}")).is_escalating()
    };
    assert!(!with("claude", ""));
    assert!(with("claude", "args: [--dangerously-skip-permissions]\n"));
    assert!(with(
        "codex",
        "args: [--dangerously-bypass-approvals-and-sandbox]\n"
    ));
    assert!(with(
        "claude",
        "permissions:\n  claude:\n    default_mode: bypassPermissions\n"
    ));
    assert!(!with(
        "claude",
        "permissions:\n  claude:\n    default_mode: acceptEdits\n"
    ));
    assert!(with(
        "codex",
        "permissions:\n  codex:\n    sandbox_mode: danger-full-access\n    approval_policy: never\n"
    ));
    assert!(!with(
        "codex",
        "permissions:\n  codex:\n    sandbox_mode: danger-full-access\n    approval_policy: on-request\n"
    ));
    // The same modes set by a CLI flag in args, which wins over the block.
    assert!(with(
        "claude",
        "args: [--permission-mode, bypassPermissions]\n"
    ));
    assert!(with(
        "claude",
        "args: [\"--permission-mode=bypassPermissions\"]\n"
    ));
    assert!(!with(
        "claude",
        "args: [--permission-mode, plan]\npermissions:\n  claude:\n    default_mode: bypassPermissions\n"
    ));
    assert!(with(
        "codex",
        "args: [--sandbox, danger-full-access, -a, never]\n"
    ));
    assert!(with(
        "codex",
        "args: [\"--sandbox=danger-full-access\"]\npermissions:\n  codex:\n    approval_policy: never\n"
    ));
    assert!(!with("codex", "args: [-s, danger-full-access]\n"));
    // Only the agent's own runtime block counts.
    assert!(!with(
        "codex",
        "permissions:\n  claude:\n    default_mode: bypassPermissions\n"
    ));
}

// ---------------------------------------------------------------------------
// Store: revisions, conflict, attribution, archive, delete
// ---------------------------------------------------------------------------

#[test]
fn create_writes_revision_one_with_normalized_startup_in_the_team_config_dir() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let def = create(home.path(), &alice(), "triage");

    assert_eq!(def.revision, 1);
    assert_eq!(def.created_by, "1");
    assert_eq!(
        def.startup,
        vec![
            "CLAUDE.md",
            ".claude/skills/bmad-build",
            ".claude/commands/bmad-help.md"
        ]
    );
    let dir = agent_dir(home.path(), &alice(), "triage");
    assert!(dir.ends_with("backend/config/agents/triage"));
    assert_eq!(defs::read_def(&dir).unwrap().unwrap(), def);
    let rev = defs::read_revision(&dir, 1).unwrap().unwrap();
    assert_eq!(rev.agent, def);
    assert_eq!(rev.author.kind, AuthorKind::User);

    // An id is never reused, and creating someone else's `created_by` is
    // overwritten by the caller.
    let again = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(team()),
        agent("triage"),
        &Author::user("1"),
    );
    assert_eq!(again.unwrap_err(), AgentError::AlreadyExists(key("triage")));
    let forged = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(team()),
        Agent {
            created_by: "99".into(),
            ..agent("other")
        },
        &Author::user("1"),
    )
    .unwrap();
    assert_eq!(forged.agent.created_by, "1");

    // A team nobody synced is not a place to write.
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: "nobody/synced".into(),
                team: TEAM.into()
            }),
            agent("x"),
            &Author::user("1"),
        ),
        Err(AgentError::InvalidLocation(_))
    ));
}

// SW-CR-1: `builtin` is reserved for starter agents. The slug validator
// rejects the bare `builtin`; `check_org` catches a team-shaped
// `builtin/<x>` org that would otherwise sail through the relaxed gate.
// SW-CR-5: the reservation is case-insensitive (ASCII) so a mixed-case
// `Builtin` / `Builtin/foo` / `_Personal/foo` can't alias onto the
// reserved namespace on a case-folding filesystem.
#[test]
fn create_refuses_a_team_whose_org_is_the_reserved_builtin_prefix() {
    let home = tempfile::tempdir().unwrap();
    for org in [
        "builtin",
        "builtin/foo",
        "Builtin",
        "BUILTIN/foo",
        "_Personal/foo",
    ] {
        let err = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: org.into(),
                team: TEAM.into(),
            }),
            agent("triage"),
            &Author::user("1"),
        )
        .unwrap_err();
        assert!(
            matches!(err, AgentError::InvalidLocation(_)),
            "{org}: {err:?}"
        );
    }
}

#[test]
fn revisions_conflict_and_restore_cycle() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let v1 = create(home.path(), &alice(), "triage");
    let edit = |base: u32, name: &str| {
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Update {
                key: key("triage"),
                base_revision: base,
            },
            Agent {
                name: name.into(),
                ..v1.clone()
            },
            &Author::agent("controller"),
        )
    };
    let v2 = edit(1, "Second").unwrap().agent;
    assert_eq!(v2.revision, 2);

    // A stale base writes nothing.
    assert_eq!(
        edit(1, "Stale").unwrap_err(),
        AgentError::Conflict {
            current_revision: 2
        }
    );
    let dir = agent_dir(home.path(), &alice(), "triage");
    assert!(defs::read_revision(&dir, 3).unwrap().is_none());

    // Restore writes revision n+1 with revision 1's body.
    let v3 = service::restore(
        home.path(),
        &alice(),
        &key("triage"),
        1,
        2,
        &Author::user("1"),
    )
    .unwrap()
    .agent;
    assert_eq!(v3.revision, 3);
    assert_eq!(v3.name, v1.name);
    assert_eq!(
        service::restore(
            home.path(),
            &alice(),
            &key("triage"),
            1,
            2,
            &Author::user("1")
        )
        .unwrap_err(),
        AgentError::Conflict {
            current_revision: 3
        }
    );
    assert!(matches!(
        service::restore(
            home.path(),
            &alice(),
            &key("triage"),
            9,
            3,
            &Author::user("1")
        ),
        Err(AgentError::RevisionNotFound { revision: 9, .. })
    ));

    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    let authors: Vec<_> = detail
        .revisions
        .iter()
        .map(|r| (r.revision, r.author.name.as_str()))
        .collect();
    assert_eq!(authors, vec![(1, "1"), (2, "controller"), (3, "1")]);
    assert_eq!(detail.revisions[1].author.kind, AuthorKind::Agent);
    assert_eq!(detail.created_by_display, "Alice A");

    // The id cannot be changed by an update.
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Update {
                key: key("triage"),
                base_revision: 3
            },
            agent("renamed"),
            &Author::user("1"),
        ),
        Err(AgentError::InvalidLocation(_))
    ));
}

#[test]
fn two_concurrent_saves_with_one_base_give_exactly_one_conflict() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let v1 = create(home.path(), &alice(), "triage");
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<_> = (0..2)
        .map(|i| {
            let home = home.path().to_path_buf();
            let barrier = barrier.clone();
            let body = Agent {
                name: format!("writer {i}"),
                ..v1.clone()
            };
            std::thread::spawn(move || {
                barrier.wait();
                service::save(
                    &home,
                    &alice(),
                    SaveTarget::Update {
                        key: key("triage"),
                        base_revision: 1,
                    },
                    body,
                    &Author::user("1"),
                )
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(AgentError::Conflict {
                    current_revision: 2
                })
            )
        })
        .count();
    assert_eq!((ok, conflicts), (1, 1));
}

/// Alice's team agent reaches Bob by git; Bob sees it, edits it with no owner
/// check (attributed to him, `created_by` still Alice), and copies it.
#[test]
fn seam_a_teammate_lists_edits_and_copies_a_synced_team_agent() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    write_org(home.path(), 2, ORG);
    let alices = create(home.path(), &alice(), "triage");

    // Simulate the git pull: Alice's definition dir lands in Bob's clone.
    let from = agent_dir(home.path(), &alice(), "triage");
    let to = agent_dir(home.path(), &bob(), "triage");
    for entry in walkdir::WalkDir::new(&from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(&from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }

    let listed = saved(home.path(), &bob(), false).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, key("triage"));
    assert_eq!(listed[0].scope, AgentScope::Team);
    assert_eq!(
        (listed[0].org.as_str(), listed[0].team.as_str()),
        (ORG, TEAM)
    );
    assert_eq!(listed[0].created_by_display, "Alice A");

    let edited = service::save(
        home.path(),
        &bob(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: alices.revision,
        },
        Agent {
            description: "Bob's wording".into(),
            ..alices.clone()
        },
        &Author::user("bob"),
    )
    .unwrap()
    .agent;
    assert_eq!(edited.revision, 2);
    assert_eq!(edited.created_by, "1", "created_by is attribution, kept");
    let rev = defs::read_revision(&to, 2).unwrap().unwrap();
    assert_eq!(rev.author, Author::user("bob"));

    // Archive, unarchive, restore and delete carry no owner check either.
    service::archive(home.path(), &bob(), &key("triage"), &Author::user("bob")).unwrap();
    service::unarchive(home.path(), &bob(), &key("triage"), 3, &Author::user("bob")).unwrap();
    service::restore(
        home.path(),
        &bob(),
        &key("triage"),
        1,
        4,
        &Author::user("bob"),
    )
    .unwrap();

    let copy = service::copy_to_mine(
        home.path(),
        &bob(),
        &key("triage"),
        personal(2, "bob"),
        &Author::user("bob"),
    )
    .unwrap();
    assert_eq!(
        copy.key.to_string(),
        "_personal/2/acme/api/_scratch/triage-copy"
    );
    assert_eq!(copy.agent.created_by, "2");
    assert_eq!(copy.agent.revision, 1);
    let copy2 = service::copy_to_mine(
        home.path(),
        &bob(),
        &key("triage"),
        personal(2, "bob"),
        &Author::user("bob"),
    )
    .unwrap();
    assert_eq!(copy2.key.id, "triage-copy-2");

    service::delete(home.path(), &bob(), &key("triage"), None, &NoReferrers).unwrap();
    assert!(!to.exists());
}

struct OneRoutine;

impl AgentReferrers for OneRoutine {
    fn referrers(&self, _: &Path, _: &Principal, _: &AgentKey) -> Vec<String> {
        vec!["acme/api/backend/standup".into()]
    }
}

#[test]
fn archiving_keeps_an_env_value_written_as_a_non_string() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    create(home.path(), &alice(), "triage");
    let file = agent_dir(home.path(), &alice(), "triage").join("agent.yaml");
    let yaml = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        format!("{yaml}env:\n  KEPT: \"yes\"\n  CLAUDE_CODE_DISABLE_AUTO_MEMORY: 1\n"),
    )
    .unwrap();
    for archived in [true, false] {
        let agent = if archived {
            service::archive(home.path(), &alice(), &key("triage"), &Author::user("1"))
        } else {
            service::unarchive(home.path(), &alice(), &key("triage"), 2, &Author::user("1"))
        }
        .unwrap()
        .agent;
        // Written without validation, the value is carried as written, still refused.
        assert_eq!(
            agent.env.first_not_string(),
            Some("CLAUDE_CODE_DISABLE_AUTO_MEMORY")
        );
        assert_eq!(agent.env.get("KEPT").map(String::as_str), Some("yes"));
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(
            written.contains("CLAUDE_CODE_DISABLE_AUTO_MEMORY: 1"),
            "{written}"
        );
    }
}

#[test]
fn archive_hides_from_list_while_references_keep_resolving_and_delete_waits_for_referrers() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    create(home.path(), &alice(), "triage");
    let live_at_1 =
        std::fs::read(agent_dir(home.path(), &alice(), "triage").join("agent.yaml")).unwrap();
    let pinned_live = service::get_pinned(home.path(), &alice(), &key("triage"), 1).unwrap();
    assert_eq!(
        pinned_live.reference.definition_hash,
        content_hash(&live_at_1)
    );
    let archived = service::archive(home.path(), &alice(), &key("triage"), &Author::user("1"))
        .unwrap()
        .agent;
    assert!(archived.is_archived());
    assert_eq!(archived.revision, 2);
    // Archiving twice is a no-op.
    assert_eq!(
        service::archive(home.path(), &alice(), &key("triage"), &Author::user("1"))
            .unwrap()
            .agent
            .revision,
        2
    );

    assert!(saved(home.path(), &alice(), false).unwrap().is_empty());
    assert_eq!(saved(home.path(), &alice(), true).unwrap().len(), 1);
    assert!(service::get(home.path(), &alice(), &key("triage"))
        .unwrap()
        .agent
        .is_archived());
    let pinned = service::get_pinned(home.path(), &alice(), &key("triage"), 1).unwrap();
    assert!(!pinned.agent.is_archived());
    assert_eq!(pinned.reference.to_string(), "acme/api/backend/triage@1");
    // Read back from history, revision 1 keeps the hash it had while live.
    assert_eq!(pinned.reference, pinned_live.reference);
    assert!(matches!(
        service::get_pinned(home.path(), &alice(), &key("triage"), 7),
        Err(AgentError::RevisionNotFound { revision: 7, .. })
    ));
    assert_eq!(
        service::get_pinned(home.path(), &alice(), &key("nope"), 1).unwrap_err(),
        AgentError::NotFound(key("nope"))
    );

    // Its id stays taken while archived.
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            agent("triage"),
            &Author::user("1"),
        )
        .unwrap_err(),
        AgentError::AlreadyExists(key("triage"))
    );

    // Delete is refused while referenced, and writes nothing.
    assert_eq!(
        service::used_by(home.path(), &alice(), &key("triage"), &OneRoutine).unwrap(),
        vec!["acme/api/backend/standup"]
    );
    assert_eq!(
        service::delete(home.path(), &alice(), &key("triage"), None, &OneRoutine).unwrap_err(),
        AgentError::InUse {
            referrers: vec!["acme/api/backend/standup".into()],
            owned: Vec::new(),
        }
    );
    let dir = agent_dir(home.path(), &alice(), "triage");
    assert!(dir.join("agent.yaml").exists());

    // A stale base is a conflict and changes nothing.
    assert_eq!(
        service::unarchive(home.path(), &alice(), &key("triage"), 1, &Author::user("1"))
            .unwrap_err(),
        AgentError::Conflict {
            current_revision: 2
        }
    );
    service::unarchive(home.path(), &alice(), &key("triage"), 2, &Author::user("1")).unwrap();
    assert_eq!(saved(home.path(), &alice(), false).unwrap().len(), 1);
    service::delete(home.path(), &alice(), &key("triage"), None, &NoReferrers).unwrap();
    assert!(!dir.exists());
    assert_eq!(
        service::get(home.path(), &alice(), &key("triage")).unwrap_err(),
        AgentError::NotFound(key("triage"))
    );
}

/// Two local users each save a personal `triage`: two agents with nothing in
/// common, and neither key resolves for the other.
#[test]
fn seam_same_named_personal_agents_of_two_users_share_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut keys = Vec::new();
    for (who, name) in [(alice(), "alice"), (bob(), "bob")] {
        // A personal agent is filed under an org the user has on this machine.
        write_org(home.path(), who.as_str().parse().unwrap(), ORG);
        let saved = service::save(
            home.path(),
            &who,
            SaveTarget::Create(personal(who.as_str().parse().unwrap(), name)),
            agent("triage"),
            &Author::user(name),
        )
        .unwrap();
        keys.push(saved.key);
    }
    assert_eq!(keys[0].to_string(), "_personal/1/acme/api/_scratch/triage");
    assert_eq!(keys[1].to_string(), "_personal/2/acme/api/_scratch/triage");
    assert_eq!(
        service::get(home.path(), &bob(), &keys[0]).unwrap_err(),
        AgentError::NotFound(keys[0].clone())
    );
    assert_eq!(
        service::get(home.path(), &alice(), &keys[1]).unwrap_err(),
        AgentError::NotFound(keys[1].clone())
    );
    let alices = saved(home.path(), &alice(), false).unwrap();
    assert_eq!(alices.len(), 1);
    assert_eq!(alices[0].key, keys[0]);
    assert_eq!(alices[0].scope, AgentScope::Personal);
    // Nobody creates in someone else's personal space, and no team org can
    // render like a personal key.
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(personal(2, "bob")),
            agent("sneaky"),
            &Author::user("alice"),
        ),
        Err(AgentError::InvalidLocation(_))
    ));
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: SCRATCH_TEAM.into(),
            }),
            agent("sneaky"),
            &Author::user("alice"),
        ),
        Err(AgentError::InvalidLocation(_))
    ));
    assert_eq!(
        roots::personal_agents_dir(home.path(), &alice(), ORG).unwrap(),
        roots::team_agents_dir(home.path(), &alice(), ORG, SCRATCH_TEAM).unwrap()
    );
    assert!(roots::personal_agents_dir(home.path(), &alice(), ORG)
        .unwrap()
        .unwrap()
        .join("triage/agent.yaml")
        .exists());
}

// ---------------------------------------------------------------------------
// Per-user provider-login binding
// ---------------------------------------------------------------------------

#[test]
fn a_binding_is_local_per_user_and_must_match_the_agents_runtime() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    write_org(home.path(), 2, ORG);
    create(home.path(), &alice(), "triage");
    let claude = create_profile(home.path(), &alice(), AgentProvider::Claude, "Work").unwrap();
    let codex = create_profile(home.path(), &alice(), AgentProvider::Codex, "Codex").unwrap();

    assert_eq!(
        binding::set(home.path(), &alice(), &key("triage"), &codex.id).unwrap_err(),
        BindingError::ProviderMismatch {
            account_id: codex.id.clone(),
            provider: AgentProvider::Codex,
            runtime: AgentCli::Claude,
        }
    );
    assert_eq!(
        binding::set(home.path(), &alice(), &key("triage"), "missing").unwrap_err(),
        BindingError::AccountNotFound("missing".into())
    );
    let file = binding::bindings_file(home.path(), &alice());
    assert!(!file.exists(), "a refused set writes nothing");
    assert!(matches!(
        binding::set(home.path(), &alice(), &key("nope"), &claude.id),
        Err(BindingError::Agent(AgentError::NotFound(_)))
    ));

    binding::set(home.path(), &alice(), &key("triage"), &claude.id).unwrap();
    assert_eq!(
        binding::get(home.path(), &alice(), &key("triage"))
            .unwrap()
            .unwrap()
            .account_id,
        claude.id
    );
    // Bob's binding of the same team agent is his own.
    assert_eq!(
        binding::get(home.path(), &bob(), &key("triage")).unwrap(),
        None
    );
    // Machine-local: never under any team dir, never in the definition.
    let org_root = path_to_org_root(home.path(), &alice(), ORG).unwrap();
    assert!(!file.starts_with(&org_root));
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({ "acme/api/backend/triage": { "account_id": claude.id } })
    );
    let def =
        std::fs::read_to_string(agent_dir(home.path(), &alice(), "triage").join("agent.yaml"))
            .unwrap();
    assert!(!def.contains(&claude.id));

    // A binding to an account that no longer exists is reported, not dropped.
    std::fs::write(
        &file,
        r#"{ "acme/api/backend/triage": { "account_id": "gone" } }"#,
    )
    .unwrap();
    let stale = binding::get(home.path(), &alice(), &key("triage"))
        .unwrap()
        .unwrap();
    assert_eq!(stale.account_id, "gone");
    assert!(!stale.account_exists);

    assert!(binding::clear(home.path(), &alice(), &key("triage")).unwrap());
    assert!(!binding::clear(home.path(), &alice(), &key("triage")).unwrap());
    assert_eq!(
        binding::get(home.path(), &alice(), &key("triage")).unwrap(),
        None
    );
}

// ---------------------------------------------------------------------------
// Frozen seam types
// ---------------------------------------------------------------------------

fn file(logical: &str, destination: &str, body: &[u8], mode: u32) -> ManifestFile {
    ManifestFile {
        logical: logical.into(),
        destination: destination.into(),
        hash: content_hash(body),
        bytes: body.len() as u64,
        mode,
        layer: None,
    }
}

fn path(raw: &str) -> StartupPath {
    StartupPath::parse(raw).unwrap()
}

fn golden_manifest() -> StartupManifest {
    let skill_files = vec![
        file(
            ".claude/skills/bmad-build/SKILL.md",
            ".claude/skills/bmad-build/SKILL.md",
            b"# build",
            0o644,
        ),
        file(
            ".claude/skills/bmad-build/steps/one.md",
            ".claude/skills/bmad-build/steps/one.md",
            b"step one",
            0o644,
        ),
    ];
    let notes = vec![file("notes/mine.md", "notes/mine.md", b"mine", 0o644)];
    let review = vec![file(
        ".claude/skills/review/SKILL.md",
        ".claude/skills/review/SKILL.md",
        b"# review",
        0o644,
    )];
    let hook = vec![file(
        ".claude/hooks/codesense-notify.sh",
        ".claude/hooks/codesense-notify.sh",
        b"#!/bin/sh\n",
        0o755,
    )];
    let claude_settings = serde_json::json!({
        "allow": ["Bash(npm test)", "mcp__codesense__*"],
        "deny": ["Bash(git push --force:*)"],
        "defaultMode": "acceptEdits",
    });
    let codex_config = serde_json::json!({
        "approval_policy": "never",
        "sandbox_mode": "workspace-write",
    });
    let entries = vec![
        ManifestEntry {
            path: path(".claude/skills/bmad-build/"),
            origin: EntryOrigin::Declared,
            layer: ContentLayer::Team {
                org: ORG.into(),
                team: TEAM.into(),
            },
            entry_hash: entry_hash(&skill_files),
            files: skill_files,
        },
        ManifestEntry {
            path: path(".claude/skills/review"),
            origin: EntryOrigin::Dependency {
                of: path(".claude/skills/bmad-build"),
            },
            layer: ContentLayer::Team {
                org: ORG.into(),
                team: "platform".into(),
            },
            entry_hash: entry_hash(&review),
            files: review,
        },
        ManifestEntry {
            path: path("notes/mine.md"),
            origin: EntryOrigin::Declared,
            layer: ContentLayer::Personal {
                account: "1".into(),
                org: Some(ORG.into()),
            },
            entry_hash: entry_hash(&notes),
            files: notes,
        },
        ManifestEntry {
            path: path(".claude/hooks/codesense-notify.sh"),
            origin: EntryOrigin::Required,
            layer: ContentLayer::Team {
                org: ORG.into(),
                team: TEAM.into(),
            },
            entry_hash: entry_hash(&hook),
            files: hook,
        },
    ];
    let totals = entries
        .iter()
        .flat_map(|e| &e.files)
        .fold(ManifestTotals::default(), |t, f| ManifestTotals {
            files: t.files + 1,
            bytes: t.bytes + f.bytes,
        });
    let mut manifest = StartupManifest {
        agent: AgentRevisionRef {
            key: key("triage"),
            revision: 3,
            definition_hash: content_hash(b"revision 3 bytes"),
        },
        runtime: AgentCli::Claude,
        entries,
        essentials: vec![EssentialEntry {
            id: "codesense-bridge".into(),
            reason: "CodeSense MCP bridge".into(),
        }],
        generated: vec![
            GeneratedEntry {
                id: "claude-permissions".into(),
                target: GeneratedTarget::LaunchSettings,
                hash: json_hash(&claude_settings),
                content: claude_settings,
                required: vec!["mcp__codesense__*".into()],
                path: None,
                merge_keys: Default::default(),
            },
            GeneratedEntry {
                id: "codex-permissions".into(),
                target: GeneratedTarget::LaunchSettings,
                hash: json_hash(&codex_config),
                content: codex_config,
                required: Vec::new(),
                path: None,
                merge_keys: Default::default(),
            },
        ],
        totals,
        errors: vec![
            ManifestError {
                path: Some(path("docs/gone.md")),
                code: ManifestErrorCode::Missing,
                message: "docs/gone.md exists in no layer".into(),
            },
            ManifestError {
                path: None,
                code: ManifestErrorCode::Invalid,
                message: "MCP server \"jira\" is not in this team's MCP config".into(),
            },
        ],
        manifest_hash: String::new(),
    };
    manifest.manifest_hash = manifest.compute_hash();
    manifest
}

/// The wire shape the context lifecycle consumes. A renamed or reshaped field
/// fails here; change the fixture only with the consumer's agreement.
#[test]
fn seam_startup_manifest_matches_the_golden_json() {
    let manifest = golden_manifest();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("testdata/startup_manifest.golden.json")).unwrap();
    assert_eq!(
        serde_json::to_value(&manifest).unwrap(),
        golden,
        "actual:\n{}",
        serde_json::to_string_pretty(&manifest).unwrap()
    );
    let back: StartupManifest = serde_json::from_value(golden).unwrap();
    assert_eq!(back, manifest);
    assert_eq!(back.compute_hash(), back.manifest_hash);

    assert_eq!(manifest.agent.to_string(), "acme/api/backend/triage@3");
    let r: AgentRevisionRef = serde_json::from_value(serde_json::json!({
        "key": "_personal/7/acme/api/_scratch/mine",
        "revision": 2,
        "definition_hash": content_hash(b"x"),
    }))
    .unwrap();
    assert_eq!(r.to_string(), "_personal/7/acme/api/_scratch/mine@2");

    // The entry hash depends on the files, not their order.
    let mut files = manifest.entries[0].files.clone();
    files.reverse();
    assert_eq!(entry_hash(&files), manifest.entries[0].entry_hash);
    assert!(manifest.entries[0].entry_hash.starts_with("blake3:"));
    assert_eq!(manifest.entries[0].entry_hash.len(), "blake3:".len() + 64);
}

#[test]
fn seam_content_root_maps_each_layer_to_its_content_dir() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path(), 1, ORG);
    let me = alice();
    assert_eq!(
        ContentLayer::Team {
            org: ORG.into(),
            team: TEAM.into()
        }
        .content_root(home.path(), &me)
        .unwrap(),
        path_to_team_content_dir(&org_root, TEAM)
    );
    // A parent hop of the team chain is just another team of the org.
    assert_eq!(
        ContentLayer::Team {
            org: ORG.into(),
            team: "frontend".into()
        }
        .content_root(home.path(), &me)
        .unwrap(),
        path_to_team_content_dir(&org_root, "frontend")
    );
    service::save(
        home.path(),
        &me,
        SaveTarget::Create(personal(1, "alice")),
        agent("mine"),
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(
        ContentLayer::Personal {
            account: "1".into(),
            org: Some(ORG.into()),
        }
        .content_root(home.path(), &me)
        .unwrap(),
        crate::context_repo::scratch_space::org_space(home.path(), &me, ORG)
            .unwrap()
            .content_dir()
    );
    assert_eq!(
        ContentLayer::Personal {
            account: "1".into(),
            org: None,
        }
        .content_root(home.path(), &me)
        .unwrap(),
        crate::context_repo::scratch_space::scratch_all_orgs_root(home.path(), &me),
        "an org-less personal layer is all-orgs"
    );
    // A layer read off the wire never becomes a path outside the tree, and the
    // personal space is only ever a personal layer.
    for layer in [
        ContentLayer::Team {
            org: ORG.into(),
            team: "../../escape".into(),
        },
        ContentLayer::Team {
            org: "../x".into(),
            team: TEAM.into(),
        },
        ContentLayer::Team {
            org: ORG.into(),
            team: SCRATCH_TEAM.into(),
        },
        ContentLayer::Personal {
            account: "../1".into(),
            org: Some(ORG.into()),
        },
        ContentLayer::Personal {
            account: "".into(),
            org: Some(ORG.into()),
        },
        ContentLayer::Personal {
            account: "1".into(),
            org: Some("../x".into()),
        },
        // Another org's scratch space has never been created for this user.
        ContentLayer::Personal {
            account: "1".into(),
            org: Some("other/org".into()),
        },
        // Another user's personal space never resolves.
        ContentLayer::Personal {
            account: "2".into(),
            org: Some(ORG.into()),
        },
    ] {
        assert!(
            matches!(
                layer.content_root(home.path(), &me),
                Err(AgentError::InvalidLocation(_))
            ),
            "{layer:?}"
        );
    }
}

/// A pulled definition with no revision files stays pinnable after a local
/// edit, a snapshot left by an interrupted save is not a revision, and a
/// restored body passes the same checks as a save.
#[test]
fn hand_written_and_interrupted_revisions_resolve_truthfully() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let dir = agent_dir(home.path(), &alice(), "pulled");
    std::fs::create_dir_all(&dir).unwrap();
    let pulled = "id: pulled\nname: Pulled\ncreated_by: \"2\"\nruntime: claude\nrevision: 1\n";
    std::fs::write(dir.join("agent.yaml"), pulled).unwrap();
    let before = service::get_pinned(home.path(), &alice(), &key("pulled"), 1).unwrap();
    assert_eq!(
        before.reference.definition_hash,
        content_hash(pulled.as_bytes())
    );

    let current = service::get(home.path(), &alice(), &key("pulled")).unwrap();
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("pulled"),
            base_revision: 1,
        },
        Agent {
            name: "Edited".into(),
            ..current.agent
        },
        &Author::user("1"),
    )
    .unwrap();
    let after = service::get_pinned(home.path(), &alice(), &key("pulled"), 1).unwrap();
    assert_eq!(after.agent.name, "Pulled", "revision 1 still resolves");
    assert_eq!(
        after.reference, before.reference,
        "a pinned revision keeps its hash once it moves into history"
    );
    let history = service::get(home.path(), &alice(), &key("pulled")).unwrap();
    assert_eq!(history.revisions.len(), 2);
    assert_eq!(history.revisions[0].author, Author::user("2"));

    // An orphan snapshot past the live revision is not served.
    std::fs::copy(dir.join("revisions/2.yml"), dir.join("revisions/3.yml")).unwrap();
    assert!(matches!(
        service::get_pinned(home.path(), &alice(), &key("pulled"), 3),
        Err(AgentError::RevisionNotFound { revision: 3, .. })
    ));
    assert!(matches!(
        service::restore(
            home.path(),
            &alice(),
            &key("pulled"),
            3,
            2,
            &Author::user("1")
        ),
        Err(AgentError::RevisionNotFound { revision: 3, .. })
    ));
    assert_eq!(
        service::get(home.path(), &alice(), &key("pulled"))
            .unwrap()
            .revisions
            .len(),
        2
    );

    // A stored revision whose body this build refuses cannot be restored.
    let bad = "revision: 1\nauthor: {kind: user, name: \"2\"}\nat: \"2026-01-01T00:00:00Z\"\n\
               agent: {id: pulled, name: Pulled, runtime: claude, revision: 1, startup: [\"/abs\"]}\n";
    std::fs::write(dir.join("revisions/1.yml"), bad).unwrap();
    assert!(matches!(
        service::restore(
            home.path(),
            &alice(),
            &key("pulled"),
            1,
            2,
            &Author::user("1")
        ),
        Err(AgentError::Validation(ValidationError::InvalidStartupPath(
            _
        )))
    ));
    // Discovery lists only keys the service can resolve.
    std::fs::create_dir_all(agent_dir(home.path(), &alice(), "Bad_Id")).unwrap();
    std::fs::write(
        agent_dir(home.path(), &alice(), "Bad_Id").join("agent.yaml"),
        "id: Bad_Id\nname: B\nruntime: claude\nrevision: 1\n",
    )
    .unwrap();
    let listed = saved(home.path(), &alice(), true).unwrap();
    assert!(listed.iter().all(|a| a.key.id != "Bad_Id"));
}

#[test]
fn json_hash_ignores_object_key_order() {
    let a: serde_json::Value =
        serde_json::from_str(r#"{"b":1,"a":{"y":[1,{"q":2,"p":3}],"x":"s"}}"#).unwrap();
    let mut map = serde_json::Map::new();
    map.insert(
        "a".into(),
        serde_json::json!({"x": "s", "y": [1, {"p": 3, "q": 2}]}),
    );
    map.insert("b".into(), serde_json::json!(1));
    assert_eq!(json_hash(&a), json_hash(&serde_json::Value::Object(map)));
    assert_eq!(
        json_hash(&serde_json::json!({"b": 1, "a": 2})),
        content_hash(br#"{"a":2,"b":1}"#)
    );
}

#[test]
fn agents_in_the_codesense_docs_are_read_only() {
    let home = tempfile::tempdir().unwrap();
    let docs = crate::context_repo::codesense_docs::bundle_info();
    let who = Principal::from(1u64);
    crate::context_repo::codesense_docs::ensure_placed(
        home.path(),
        &who,
        include_bytes!("../../resources/codesense-docs.tar.gz"),
    )
    .unwrap();
    let key = AgentKey::new(&docs.slug, &docs.team, "docs-helper");
    let error = service::archive(home.path(), &who, &key, &Author::user("1"))
        .expect_err("a docs agent write must be refused")
        .to_string();
    assert!(
        error.contains(crate::context_repo::codesense_docs::READ_ONLY_MESSAGE),
        "{error}"
    );
}

#[test]
fn deleting_an_agent_of_the_codesense_docs_is_refused_and_keeps_its_folder() {
    let home = tempfile::tempdir().unwrap();
    let docs = crate::context_repo::codesense_docs::bundle_info();
    let who = Principal::from(1u64);
    let placed = crate::context_repo::codesense_docs::ensure_placed(
        home.path(),
        &who,
        include_bytes!("../../resources/codesense-docs.tar.gz"),
    )
    .unwrap();
    let key = AgentKey::new(&docs.slug, &docs.team, "docs-helper");
    let error = service::delete(home.path(), &who, &key, None, &service::NoReferrers)
        .expect_err("deleting a docs agent must be refused")
        .to_string();
    assert!(
        error.contains(crate::context_repo::codesense_docs::READ_ONLY_MESSAGE),
        "{error}"
    );
    assert!(placed
        .attachment
        .root
        .join("getting-started/config/agents/docs-helper/agent.yaml")
        .is_file());
    // Its routines live under it (SW-AGR-10); the refusal must hold for an
    // agent dir with `routines/` too, and leave them.
    assert!(placed
        .attachment
        .root
        .join(
            "getting-started/config/agents/docs-helper/routines/update-codesense-docs/routine.yml"
        )
        .is_file());
}

/// A background launch that names no agent runs the unattended starter, on the login the
/// user bound to its ordinary twin; a binding of its own wins.
#[test]
fn an_unattended_starter_runs_on_its_twins_account_until_it_has_its_own() {
    use super::builtin;
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let work = create_profile(home.path(), &alice(), AgentProvider::Claude, "Work").unwrap();
    let own = create_profile(home.path(), &alice(), AgentProvider::Claude, "Own").unwrap();
    let unattended = builtin::unattended_key(AgentCli::Claude);
    assert_eq!(
        builtin::attended_twin(&unattended),
        Some(builtin::key(AgentCli::Claude))
    );
    assert_eq!(
        builtin::attended_twin(&builtin::key(AgentCli::Claude)),
        None
    );
    let account = || {
        super::launch::prepare(home.path(), &alice(), &unattended, Default::default())
            .unwrap()
            .account_override
    };
    assert_eq!(account(), None);

    binding::set(
        home.path(),
        &alice(),
        &builtin::key(AgentCli::Claude),
        &work.id,
    )
    .unwrap();
    assert_eq!(account(), Some(work.id.clone()));

    binding::set(home.path(), &alice(), &unattended, &own.id).unwrap();
    assert_eq!(account(), Some(own.id));
}

/// Every `agent.yaml` in the CodeSense docs source (the docs repo's own agent and the
/// recipe examples users copy into their team) parses as an agent and passes the checks a
/// save runs, with no field the schema doesn't know.
#[test]
fn every_agent_yaml_in_the_codesense_docs_is_a_valid_agent() {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.file_name().is_some_and(|n| n == defs::AGENT_FILE) {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../codesense-docs");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            f.parent()
                .and_then(Path::file_name)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for expected in ["docs-helper", "codesense-setup", "machine-setup", "manager"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing: {names:?}"
        );
    }
    for file in &files {
        let raw = std::fs::read_to_string(file).unwrap();
        let agent: Agent = serde_yaml::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", file.display()));
        assert!(
            agent.extra.is_empty(),
            "{} has unknown fields: {:?}",
            file.display(),
            agent.extra.keys().collect::<Vec<_>>()
        );
        agent
            .validate()
            .unwrap_or_else(|e| panic!("{} is invalid: {e:?}", file.display()));
        assert_eq!(
            Some(agent.id.as_str()),
            file.parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str()),
            "{}: id must match its folder",
            file.display()
        );
    }
}

/// A personal agent never reaches a workspace: its `config/` is no content
/// tier, so the engine's hydrate (defaults and `skills`) carries the personal
/// space's content and never the agent (ported to the preparation engine,
/// SW-XS-8b review r1).
#[test]
fn seam_personal_agent_is_never_hydrated() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let me = Principal::from(42u64);
    write_org(home, 42, ORG);
    service::save(
        home,
        &me,
        SaveTarget::Create(personal(42, "me")),
        agent("private-triage"),
        &Author::user("me"),
    )
    .unwrap();
    assert!(roots::personal_agents_dir(home, &me, ORG)
        .unwrap()
        .unwrap()
        .join("private-triage/agent.yaml")
        .exists());
    let personal_content = roots::personal_content_dir(home, &me, ORG).unwrap();
    std::fs::create_dir_all(personal_content.join("skills")).unwrap();
    std::fs::write(personal_content.join("skills/s.md"), "personal skill").unwrap();

    let target = crate::agents::ManifestTarget::team(ORG, TEAM);
    let (workspace, paths) =
        crate::agents::prepare::step3::tests::hydrate_defaults(home, &me, &target);

    assert!(
        workspace.path().join("skills/s.md").exists(),
        "the personal space's content is hydrated"
    );
    let leaked: Vec<_> = paths
        .iter()
        .filter(|p| {
            p.components().any(|c| c.as_os_str() == defs::AGENTS_DIR)
                || p.file_name().is_some_and(|n| n == defs::AGENT_FILE)
        })
        .collect();
    assert!(leaked.is_empty(), "personal agent leaked: {leaked:?}");
}

#[test]
fn saved_telemetry_overrides_are_refused_with_an_engine_owned_message() {
    for name in [
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "OTEL_SDK_DISABLED",
        "OTEL_LOGS_EXPORTER",
        "CLAUDE_CODE_ENABLE_TELEMETRY",
        "DISABLE_TELEMETRY",
    ] {
        let why = super::model::check_env(&[(name.to_string(), "override".into())].into())
            .unwrap_err()
            .to_string();
        assert!(why.contains("engine-owned usage telemetry"), "{why}");
        assert!(why.contains(name));
    }
}
