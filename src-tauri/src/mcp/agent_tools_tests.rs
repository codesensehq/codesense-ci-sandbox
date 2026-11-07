// SPDX-License-Identifier: MIT
//! Seam tests for the agent MCP tools: each body is driven with the arguments
//! as an agent sends them (JSON through the tool's own params type), against a
//! real temp home, real org layouts and the real agent store and resolver, and
//! its reply is checked as the JSON the agent reads.
//!
//! The `#[tool]` methods themselves need the desktop runtime; what they add on
//! top of these bodies (who the caller is, which team) is covered by the
//! handler's registration tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;
use crate::agents::manifest::content_hash;
use crate::agents::permissions::{CLAUDE_REQUIRED_ALLOW, CLAUDE_SETTINGS_ID};
use crate::agents::NoReferrers;
use crate::context_repo::{path_to_org_root, path_to_team_content_dir};
use crate::routines::model::AuthorKind;
use crate::routines::PersonalOwner;
use codesense_pty_core::agent_cli::AgentCli;

#[test]
fn hermes_is_listed_in_agent_create_and_model_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({
            "name": "Hermes preview", "runtime": "hermes"
        })),
    ));
    assert_eq!(
        created["id"], "acme/api/backend/hermes-preview",
        "{created}"
    );
    let schema =
        serde_json::to_value(rmcp::schemars::schema_for!(crate::agents::model::Agent)).unwrap();
    assert_eq!(
        schema["properties"]["runtime"]["enum"],
        json!(AgentCli::ALL.map(AgentCli::as_str))
    );
    assert!(schema["properties"]["runtime"]["enum"]
        .as_array()
        .unwrap()
        .contains(&json!("hermes")));
}

/// The starters' ids in list order: the runtime starters, then their unattended twins.
fn starter_ids() -> Vec<String> {
    AgentCli::ALL
        .map(|cli| crate::agents::builtin::key(cli).to_string())
        .into_iter()
        .chain(AgentCli::ALL.map(|cli| crate::agents::builtin::unattended_key(cli).to_string()))
        .collect()
}

/// The starters' display names, sorted.
fn starter_names() -> Vec<String> {
    let mut names: Vec<String> = AgentCli::ALL
        .into_iter()
        .flat_map(|cli| {
            let starter = crate::agents::adapter::desktop(cli).starter;
            [starter.name, starter.unattended_name]
        })
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

const ORG: &str = "acme/api";
const TEAM: &str = "backend";

#[test]
fn codex_bridge_tools_round_trip_and_preview_the_effective_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let tools = json!(["who_am_i", "read_messages", "reply", "signal_done"]);
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Bridge reviewer", "runtime": "codex", "mcpBridgeTools": tools
        }),
    ));
    let id = saved["id"].as_str().unwrap();
    let got = ok(get(home, &alice(), params(json!({"id": id}))));
    assert_eq!(got["definition"]["mcp"]["bridge_tools"], tools);
    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({"agent": id})),
    ));
    assert_eq!(previewed["launch"]["bridge_tools"], tools);
    let mut effective: Vec<_> = tools.as_array().unwrap().clone();
    effective.sort_by_key(|v| v.as_str().unwrap().to_owned());
    assert_eq!(
        previewed["launch"]["effective_bridge_tools"],
        json!(effective)
    );
    assert!(!previewed["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["code"] == "bridge_tools_excluded"));
    assert!(previewed["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["code"] == "bridge_tools_local_only"
            && w["message"]
                .as_str()
                .unwrap()
                .contains("remote preparation refuses")));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": 1, "description": "unchanged tool grant"
        })),
    ));
    let unchanged = ok(get(home, &alice(), params(json!({"id": id}))));
    assert_eq!(unchanged["definition"]["mcp"]["bridge_tools"], tools);
    for (revision, replacement) in [(2, json!([])), (3, Value::Null)] {
        ok(update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({
                "id": id, "baseRevision": revision, "mcpBridgeTools": replacement
            })),
        ));
        let p = ok(preview(
            home,
            &alice(),
            team(),
            &local(&alice()),
            params(json!({"agent": id})),
        ));
        assert_eq!(p["launch"]["bridge_tools"], replacement);
        if replacement.is_array() {
            assert_eq!(p["launch"]["effective_bridge_tools"], json!([]));
            let warnings = p["warnings"].to_string();
            for tool in ["signal_done", "read_messages", "reply"] {
                assert!(warnings.contains(tool), "{warnings}");
            }
        } else {
            assert!(!p["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["code"] == "bridge_tools_local_only"));
            assert!(p["launch"]["effective_bridge_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "who_am_i"));
        }
    }
}

#[test]
fn bridge_tool_validation_and_unknown_names_never_expand_the_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    for (runtime, tools) in [
        ("claude", json!(["who_am_i"])),
        ("codex", json!(["*"])),
        ("codex", json!(["mcp__codesense__who_am_i"])),
        ("codex", json!(["reply", "reply"])),
        ("codex", json!([""])),
    ] {
        let error = refused_with(
            create_as(
                home,
                &alice(),
                team(),
                json!({
                    "name": "Invalid bridge", "runtime": runtime, "mcpBridgeTools": tools
                }),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "mcpBridgeTools");
    }
    let p = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({
            "name": "Unknown bridge tool", "runtime": "codex", "mcpBridgeTools": ["future_tool"]
        })),
    ));
    assert_eq!(p["launch"]["effective_bridge_tools"], json!([]));
    assert!(p["warnings"].to_string().contains("future_tool"));
    ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Future tool", "runtime": "codex", "mcpBridgeTools": ["future_tool"]
        }),
    ));
    let dispatch = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({
            "name": "Named dispatch", "runtime": "codex", "mcpBridgeTools": ["codesense_call_tool", "who_am_i"]
        })),
    ));
    assert!(dispatch["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["code"] == "bridge_tool_dispatch"
            && w["message"].as_str().unwrap().contains("inner tool")));
}

fn alice() -> Principal {
    Principal::from(1u64)
}

fn bob() -> Principal {
    Principal::from(2u64)
}

/// A locally-cloned org for `user`, with `backend` synced and team content
/// holding a skill, a command, `CLAUDE.md` and an MCP config with `slack`.
fn write_org(home: &Path, user: &Principal) -> PathBuf {
    let org_root = path_to_org_root(home, user, ORG).unwrap();
    crate::context_repo::scratch_space::set_active_org(home, Some(ORG));
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {ORG}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 1\n    github_username: alice\n    display_name: Alice A\n  - github_user_id: 2\n    github_username: bob\n",
    )
    .unwrap();
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n"),
    )
    .unwrap();
    let content = path_to_team_content_dir(&org_root, TEAM);
    for (logical, body) in [
        (".claude/skills/bmad-build/SKILL.md", "# Build\n"),
        (".claude/commands/bmad-help.md", "Help.\n"),
        ("CLAUDE.md", "# Team\n"),
        (
            ".mcp.json",
            r#"{"mcpServers": {"slack": {"command": "slack-mcp"}}}"#,
        ),
    ] {
        let path = content.join(logical);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    org_root
}

fn team() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

fn personal(user: &Principal, name: &str) -> Placement {
    Placement::Personal(PersonalOwner {
        github_user_id: user.as_str().parse().unwrap(),
        username: name.into(),
        display_name: name.into(),
        avatar_url: None,
        org: "acme/api".into(),
    })
}

/// The local user, with no session.
fn local(user: &Principal) -> AgentCaller {
    AgentCaller::new(user, None)
}

/// The arguments as an agent sends them.
fn params<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the agent's arguments parse")
}

/// The reply as the agent reads it.
fn wire<T: Serialize>(reply: Reply<T>) -> Value {
    serde_json::to_value(reply.expect("not a hard failure")).unwrap()
}

fn ok<T: Serialize>(reply: Reply<T>) -> Value {
    let wire = wire(reply);
    assert_eq!(wire["ok"], true, "{wire}");
    wire["data"].clone()
}

fn refused_with<T: Serialize>(reply: Reply<T>, code: &str) -> Value {
    let wire = wire(reply);
    assert_eq!(wire["ok"], false, "{wire}");
    assert_eq!(wire["error"]["code"], code, "{wire}");
    wire["error"].clone()
}

fn create_as(
    home: &Path,
    user: &Principal,
    placement: Placement,
    args: Value,
) -> Reply<AgentSaved> {
    create(home, user, placement, &local(user), params(claude(args)))
}

/// Draft arguments with `runtime: claude` when they name none: a caller
/// CodeSense did not launch always passes its runtime.
fn claude(mut args: Value) -> Value {
    if args.get("runtime").is_none() {
        args["runtime"] = json!("claude");
    }
    args
}

/// Every file under `root` with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_path_buf(), std::fs::read(e.path()).unwrap()))
        .collect()
}

/// The paths added, removed or rewritten between two snapshots.
fn changed(
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = after
        .iter()
        .filter(|(path, bytes)| before.get(*path) != Some(bytes))
        .map(|(path, _)| path.clone())
        .collect();
    out.extend(before.keys().filter(|p| !after.contains_key(*p)).cloned());
    out
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn team_agents(home: &Path, user: &Principal) -> PathBuf {
    roots::team_agents_dir(home, user, ORG, TEAM)
        .unwrap()
        .unwrap()
}

/// Create a personal agent, see it listed, read its manifest with the real
/// hashes of the content it starts with, update it past a stale base, archive
/// it and read it back: the whole agent loop through the real store and
/// resolver.
#[test]
fn seam_create_list_get_update_conflict_archive_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // A personal agent is filed under an org the user has on this machine.
    write_org(home, &alice());

    let created = ok(create_as(
        home,
        &alice(),
        personal(&alice(), "alice"),
        json!({ "name": "Notes helper", "startup": ["CLAUDE.md"], "description": "Keeps notes." }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id, "_personal/1/acme/api/_scratch/notes-helper");
    assert_eq!(created["revision"], 1);
    assert_eq!(created["preview"]["agent"]["key"], id.as_str());
    assert_eq!(created["preview"]["launch"]["binary"], "claude");
    assert!(created.get("dry_run").is_none());

    // Content the agent starts with, written after the create.
    let body = b"# Mine\n";
    let content = roots::personal_content_dir(home, &alice(), "acme/api").unwrap();
    std::fs::write(content.join("CLAUDE.md"), body).unwrap();

    let listed = ok(list(
        home,
        &alice(),
        params(json!({ "includeManifest": true })),
    ));
    let rows = listed.as_array().unwrap();
    // Then the starters.
    let starters = starter_ids();
    assert_eq!(rows.len(), 1 + starters.len(), "{listed}");
    let listed_ids: Vec<&str> = rows[1..]
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed_ids, starters);
    for (row, cli) in rows[1..].iter().zip(AgentCli::ALL) {
        assert_eq!(
            row["name"],
            crate::agents::adapter::desktop(cli).starter.name
        );
    }
    for (row, cli) in rows[1 + AgentCli::ALL.len()..].iter().zip(AgentCli::ALL) {
        assert_eq!(
            row["name"],
            crate::agents::adapter::desktop(cli).starter.unattended_name
        );
    }
    let row = &rows[0];
    assert_eq!(row["id"], id.as_str());
    assert_eq!(row["name"], "Notes helper");
    assert_eq!(row["description"], "Keeps notes.");
    assert_eq!(row["runtime"], "claude");
    assert_eq!(row["launch"], "foreground");
    assert_eq!(row["mcp_servers"], json!([]));
    assert_eq!(row["startup_count"], 1);
    assert_eq!(row["scope"], "personal");
    assert_eq!(row["archived"], false);
    assert_eq!(row["revision"], 1);
    assert_eq!(row["manifest"]["files"], 1);
    assert_eq!(row["manifest"]["bytes"], body.len());
    assert!(row["manifest"]["error_count"].is_u64());
    // Without the flag the list resolves nothing.
    let plain = ok(list(home, &alice(), params(json!({}))));
    assert!(plain[0].get("manifest").is_none());

    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["definition"]["startup"], json!(["CLAUDE.md"]));
    assert_eq!(got["revisions"][0]["n"], 1);
    assert_eq!(
        got["revisions"][0]["author"],
        json!({ "kind": "user", "name": "1" })
    );
    let entry = &got["resolved"]["manifest"]["entries"][0];
    assert_eq!(entry["path"], "CLAUDE.md");
    assert_eq!(entry["files"][0]["hash"], content_hash(body).as_str());
    assert_eq!(got["resolved"]["manifest_applies_at_launch"], false);
    assert_eq!(got["resolved"]["account"]["source"], "runtime_default");

    // A stale base changes nothing; the fresh one goes through.
    let first = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "Keeps better notes." })),
    ));
    assert_eq!(first["revision"], 2);
    let conflict = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 1, "name": "Late edit" })),
        ),
        "conflict",
    );
    assert_eq!(conflict["current_revision"], 2);
    let fresh = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "name": "Notes" })),
    ));
    assert_eq!(fresh["revision"], 3);
    assert_eq!(
        ok(get(home, &alice(), params(json!({ "id": id }))))["definition"]["name"],
        "Notes"
    );

    let archived = ok(delete(
        home,
        &alice(),
        &local(&alice()),
        &NoReferrers,
        params(json!({ "id": id })),
    ));
    assert_eq!(
        archived,
        json!({ "id": id, "archived": true, "revision": 4, "used_by": [] })
    );
    assert!(ok(list(home, &alice(), params(json!({}))))
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["scope"] == "builtin"));
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["archived"], true);
    assert_eq!(got["resolved"]["archived"], true);
    assert_eq!(
        ok(list(
            home,
            &alice(),
            params(json!({ "includeArchived": true }))
        ))
        .as_array()
        .unwrap()
        .len(),
        1 + starter_ids().len(),
        "the archived agent, then the starters"
    );

    // Unarchive and restore, with the same stale-edit guard.
    refused_with(
        unarchive(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 3 })),
        ),
        "conflict",
    );
    let back = ok(unarchive(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 4 })),
    ));
    assert_eq!(back, json!({ "id": id, "revision": 5, "archived": false }));
    let restored = ok(restore(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "revision": 1, "baseRevision": 5 })),
    ));
    assert_eq!(restored["revision"], 6);
    assert_eq!(
        ok(get(home, &alice(), params(json!({ "id": id }))))["definition"]["name"],
        "Notes helper"
    );
}

/// The triage example's permissions are kept verbatim in the definition and
/// reach the generated Claude settings with CodeSense's required allows. An
/// unknown key for the runtime is a field error that writes nothing.
#[test]
fn seam_permissions_pass_through_verbatim_and_unknown_keys_name_the_field() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let allow = ["Read", "Grep", "Glob", "mcp__slack__*"];
    let deny = ["Edit", "Write", "Bash"];

    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Slack triage",
            "runtime": "claude",
            "mcpServers": ["slack"],
            "startup": ["CLAUDE.md", ".claude/skills/bmad-build/"],
            "permissions": { "claude": { "allow": allow, "deny": deny, "defaultMode": "default" } },
        }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id, format!("{ORG}/{TEAM}/slack-triage"));

    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    let claude = &got["definition"]["permissions"]["claude"];
    assert_eq!(claude["allow"], json!(allow));
    assert_eq!(claude["deny"], json!(deny));
    assert_eq!(claude["default_mode"], "default");
    let manifest = &got["resolved"]["manifest"];
    assert_eq!(manifest["errors"], json!([]), "{manifest}");
    let settings = manifest["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == CLAUDE_SETTINGS_ID)
        .expect("a generated Claude settings entry");
    assert_eq!(settings["target"], "launch_settings");
    let generated_allow: Vec<&str> = settings["content"]["permissions"]["allow"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for rule in allow.iter().chain(CLAUDE_REQUIRED_ALLOW) {
        assert!(
            generated_allow.contains(rule),
            "{rule} in {generated_allow:?}"
        );
    }
    assert_eq!(settings["content"]["permissions"]["deny"], json!(deny));

    let before = snapshot(home);
    let error = refused_with(
        create_as(
            home,
            &alice(),
            team(),
            json!({ "name": "Bad", "permissions": { "claude": { "foo": ["x"] } } }),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "permissions.claude.foo");
    let error = refused_with(
        create_as(
            home,
            &alice(),
            team(),
            json!({ "name": "Bad", "runtime": "codex", "permissions": { "codex": { "approvalPolicy": "on-request", "sandboxMode": "workspace-write", "foo": 1 } } }),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "permissions.codex.foo");
    let error = refused_with(
        create_as(
            home,
            &alice(),
            team(),
            json!({ "name": "Bad", "permissions": { "gemini": {} } }),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "permissions.gemini");
    assert_eq!(before, snapshot(home), "a refused create writes nothing");
}

/// Every validation error is a typed `invalid` naming the input, and writes
/// nothing.
#[test]
fn validation_errors_name_their_field_and_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let before = snapshot(home);
    for (args, field) in [
        (json!({ "name": "X", "runtime": "my-pack" }), "runtime"),
        (json!({ "name": "X", "launch": "sideways" }), "launch"),
        (json!({ "name": "X", "id": "Bad Id" }), "id"),
        (json!({ "name": "  " }), "name"),
        (
            json!({ "name": "X", "startup": ["/etc/passwd"] }),
            "startup",
        ),
        (json!({ "name": "X", "startup": ["a/../b"] }), "startup"),
        (
            json!({ "name": "X", "startup": [".claude/skills/*"] }),
            "startup",
        ),
        (
            json!({ "name": "X", "startup": ["CLAUDE.md", "CLAUDE.md"] }),
            "startup",
        ),
        (
            json!({ "name": "X", "mcpServers": ["codesense"] }),
            "mcpServers",
        ),
        (json!({ "name": "X", "args": ["|"] }), "args"),
    ] {
        let error = refused_with(create_as(home, &alice(), team(), args.clone()), "invalid");
        assert_eq!(error["field"], field, "{args}: {error}");
        assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    // A taken id is the caller's to fix too.
    ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage" }),
    ));
    let after_first = snapshot(home);
    let error = refused_with(
        create_as(
            home,
            &alice(),
            team(),
            json!({ "name": "Other", "id": "triage" }),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "id");
    assert_eq!(after_first, snapshot(home));
    assert_ne!(before, after_first);
    // Without an id, a name that is taken gets the next free one.
    let second = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage" }),
    ));
    assert_eq!(second["id"], format!("{ORG}/{TEAM}/triage-2"));
}

/// A preview refuses what a create would refuse in the body, and a key the
/// tools don't take is refused by name instead of dropped.
#[test]
fn preview_validates_like_create_and_unknown_keys_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    for (draft, field) in [
        (json!({ "name": "X", "startup": ["../secrets"] }), "startup"),
        (
            json!({ "name": "X", "mcpServers": ["codesense"] }),
            "mcpServers",
        ),
        (json!({ "name": " " }), "name"),
        (
            json!({ "name": "X", "permissions": { "claude": { "foo": [] } } }),
            "permissions.claude.foo",
        ),
        (
            json!({ "name": "X", "mcp": { "servers": ["slack"] } }),
            "mcp",
        ),
    ] {
        let error = refused_with(
            preview(
                home,
                &alice(),
                team(),
                &local(&alice()),
                params(claude(draft.clone())),
            ),
            "invalid",
        );
        assert_eq!(error["field"], field, "{draft}: {error}");
    }
    let error = refused_with(
        create_as(
            home,
            &alice(),
            team(),
            json!({ "name": "X", "mcp": { "servers": ["slack"] } }),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "mcp");
    assert!(error["message"].as_str().unwrap().contains("mcpServers"));
    // A normalized path previews exactly as it will be saved.
    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(json!({ "name": "X", "startup": ["./CLAUDE.md"] }))),
    ));
    assert_eq!(previewed["manifest"]["entries"][0]["path"], "CLAUDE.md");
    let created = ok(create_as(home, &alice(), team(), json!({ "name": "Typo" })));
    let error = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": created["id"], "baseRevision": 1, "defaultprompt": "go" })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "defaultprompt");
}

/// A dry run returns the reply a create would and writes nothing; so does a
/// preview.
#[test]
fn dry_run_and_preview_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let draft = json!({
        "name": "Builder",
        "startup": ["CLAUDE.md", ".claude/skills/bmad-build/", ".claude/commands/bmad-help.md"],
        "mcpServers": ["slack"],
        "defaultPrompt": "/bmad-help",
    });

    let before = snapshot(home);
    let mut args = draft.clone();
    args["dryRun"] = json!(true);
    let dry = ok(create_as(home, &alice(), team(), args));
    assert_eq!(before, snapshot(home), "a dry run writes nothing");
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["id"], format!("{ORG}/{TEAM}/builder"));
    assert_eq!(dry["revision"], 1);
    assert_eq!(dry["preview"]["manifest"]["errors"], json!([]));
    assert_eq!(dry["preview"]["provides"]["commands"], json!(["bmad-help"]));
    assert_eq!(dry["preview"]["warnings"], json!([]));

    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(draft.clone())),
    ));
    assert_eq!(before, snapshot(home), "a preview writes nothing");
    assert_eq!(
        previewed["manifest"]["entries"],
        dry["preview"]["manifest"]["entries"]
    );

    // A personal preview before the personal space exists creates nothing.
    let previewed = ok(preview(
        home,
        &alice(),
        personal(&alice(), "alice"),
        &local(&alice()),
        params(claude(json!({ "name": "Mine" }))),
    ));
    assert_eq!(
        previewed["agent"]["key"],
        "_personal/1/acme/api/_scratch/mine"
    );
    assert_eq!(before, snapshot(home));

    // A dry run checks what the create checks: an unsynced team is refused.
    let error = refused_with(
        create_as(
            home,
            &alice(),
            Placement::Team {
                org: ORG.into(),
                team: "frontend".into(),
            },
            json!({ "name": "X", "dryRun": true }),
        ),
        "invalid",
    );
    assert!(error["message"].as_str().unwrap().contains("not synced"));
    assert_eq!(before, snapshot(home));

    // A real create of the same draft matches the dry run.
    let created = ok(create_as(home, &alice(), team(), draft));
    assert_eq!(created["id"], dry["id"]);
    assert_eq!(
        created["preview"]["manifest"]["entries"],
        dry["preview"]["manifest"]["entries"]
    );
}

/// Alice creates a team agent; Bob, with Alice's revision as his base, edits
/// it from his own checkout, and the new revision is credited to Bob.
#[test]
fn seam_a_teammate_updates_a_team_agent_and_is_credited() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    write_org(home, &bob());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage" }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    // The git pull.
    copy_tree(&team_agents(home, &alice()), &team_agents(home, &bob()));

    let edited = ok(update(
        home,
        &bob(),
        &local(&bob()),
        params(
            json!({ "id": id, "baseRevision": created["revision"], "description": "Bob's wording" }),
        ),
    ));
    assert_eq!(edited["revision"], 2);
    let got = ok(get(home, &bob(), params(json!({ "id": id }))));
    assert_eq!(
        got["revisions"][1]["author"],
        json!({ "kind": "user", "name": "2" })
    );
    assert_eq!(
        got["created_by"], "Alice A",
        "created_by is attribution, kept"
    );

    // Through a launched session, the revision is credited to that agent.
    let session = CallerSession {
        account_runtime: Some(AgentCli::Codex),
        profile_id: None,
        agent: None,
    };
    let launched = AgentCaller::new(&bob(), Some(&session));
    assert_eq!(launched.author.kind, AuthorKind::Agent);
    assert_eq!(launched.author.name, "codex");
    ok(update(
        home,
        &bob(),
        &launched,
        params(json!({ "id": id, "baseRevision": 2, "name": "Triage bot" })),
    ));
    let got = ok(get(home, &bob(), params(json!({ "id": id }))));
    assert_eq!(
        got["revisions"][2]["author"],
        json!({ "kind": "agent", "name": "codex" })
    );
}

/// A new agent defaults to the calling session's runtime, else `claude`.
#[test]
fn a_new_agent_defaults_to_the_callers_runtime_or_requires_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let codex_session = CallerSession {
        account_runtime: Some(AgentCli::Codex),
        profile_id: None,
        agent: None,
    };
    let from_codex = AgentCaller::new(&alice(), Some(&codex_session));
    assert_eq!(from_codex.session_runtime, Some(AgentCli::Codex));
    let created = ok(create(
        home,
        &alice(),
        team(),
        &from_codex,
        params(json!({ "name": "Reviewer" })),
    ));
    assert_eq!(created["preview"]["manifest"]["runtime"], "codex");
    assert_eq!(created["preview"]["launch"]["binary"], "codex");

    // A session that recorded no account, or no session at all, cannot tell
    // which CLI is calling: `runtime` is then required, never guessed.
    let unbound = CallerSession {
        account_runtime: None,
        profile_id: Some("default".into()),
        agent: None,
    };
    let from_unbound = AgentCaller::new(&alice(), Some(&unbound));
    assert_eq!(from_unbound.session_runtime, None);
    assert_eq!(local(&alice()).session_runtime, None);
    assert_eq!(local(&alice()).author, Author::user("1"));
    let before = snapshot(home);
    for caller in [&from_unbound, &local(&alice())] {
        let error = refused_with(
            create(
                home,
                &alice(),
                team(),
                caller,
                params(json!({ "name": "Guess" })),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "runtime", "{error}");
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("`claude`, `codex` or `opencode`"));
        let error = refused_with(
            preview(
                home,
                &alice(),
                team(),
                caller,
                params(json!({ "name": "Guess" })),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "runtime", "{error}");
    }
    assert_eq!(before, snapshot(home), "a refused create writes nothing");
    let named = ok(create(
        home,
        &alice(),
        team(),
        &from_unbound,
        params(json!({ "name": "Named", "runtime": "codex" })),
    ));
    assert_eq!(named["preview"]["launch"]["binary"], "codex");
}

/// Unknown input keys are refused by name, all at once, with what to send
/// instead; keys the stored `agent.yaml` carries that this build does not know
/// survive every update untouched.
#[test]
fn unknown_input_is_refused_but_unknown_stored_keys_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Keeper" }),
    ));
    let id = created["id"].as_str().unwrap().to_string();

    // A definition pasted straight from agent_get names every offending key.
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    let mut copy = got["definition"].clone();
    copy["id"] = json!("keeper-copy");
    let error = refused_with(create_as(home, &alice(), team(), copy), "invalid");
    let message = error["message"].as_str().unwrap();
    for key in ["created_by", "mcp", "revision"] {
        assert!(message.contains(&format!("`{key}`")), "{message}");
    }
    assert!(message.contains("mcpServers"), "{message}");
    // With those dropped and renamed, the copy goes through.
    let mut copy = got["definition"].clone();
    let fields = copy.as_object_mut().unwrap();
    for key in ["created_by", "revision", "archived_at"] {
        fields.remove(key);
    }
    let servers = fields.remove("mcp").unwrap()["servers"].clone();
    fields.insert("mcpServers".into(), servers);
    fields.insert("id".into(), json!("keeper-copy"));
    ok(create_as(home, &alice(), team(), copy));

    // A key a newer build wrote stays in the file through an update.
    let yaml = team_agents(home, &alice()).join("keeper/agent.yaml");
    let mut stored = std::fs::read_to_string(&yaml).unwrap();
    stored.push_str("future_field:\n  nested: true\n");
    std::fs::write(&yaml, stored).unwrap();
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "kept" })),
    ));
    let after = std::fs::read_to_string(&yaml).unwrap();
    assert!(after.contains("future_field:"), "{after}");
    assert!(after.contains("nested: true"), "{after}");
    assert!(after.contains("description: kept"), "{after}");
}

/// Absent patch fields are unchanged, `[]` clears a list, a permissions block
/// replaces that runtime's block whole, and `null` drops every permission.
#[test]
fn update_patches_only_what_it_names() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Dev",
            "args": ["--model", "opus"],
            "mcpServers": ["slack"],
            "startup": ["CLAUDE.md"],
            "permissions": {
                "claude": { "allow": ["Edit"], "deny": ["Bash(git push --force:*)"] },
                "codex": { "approvalPolicy": "on-request", "sandboxMode": "workspace-write" },
            },
        }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    let def =
        |home: &Path| ok(get(home, &alice(), params(json!({ "id": id }))))["definition"].clone();

    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(
            json!({ "id": id, "baseRevision": 1, "mcpServers": [], "permissions": { "claude": { "allow": ["Read"] } } }),
        ),
    ));
    let d = def(home);
    assert_eq!(
        d["args"],
        json!(["--model", "opus"]),
        "absent fields are unchanged"
    );
    assert_eq!(d["startup"], json!(["CLAUDE.md"]));
    assert_eq!(d["mcp"]["servers"], json!([]), "[] clears the list");
    assert_eq!(
        d["permissions"]["claude"],
        json!({ "allow": ["Read"] }),
        "the block is replaced whole"
    );
    assert_eq!(
        d["permissions"]["codex"],
        json!({ "approval_policy": "on-request", "sandbox_mode": "workspace-write" }),
        "the other runtime's block is kept"
    );

    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "permissions": { "codex": null } })),
    ));
    assert!(def(home)["permissions"].get("codex").is_none());

    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 3, "permissions": null, "launch": "headless" })),
    ));
    let d = def(home);
    assert!(
        d.get("permissions").is_none(),
        "null drops every permission: {d}"
    );
    assert_eq!(d["launch"], "headless");

    let error = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 4, "runtime": "gemini" })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "runtime");
    refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": "acme/api/backend/nope", "baseRevision": 1, "name": "x" })),
        ),
        "not_found",
    );
    let error = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": "nope", "baseRevision": 1 })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "id");
}

struct OneRoutine;

impl AgentReferrers for OneRoutine {
    fn referrers(&self, _: &Path, _: &Principal, _: &AgentKey) -> Vec<String> {
        vec!["acme/api/backend/standup".into()]
    }
}

/// Archive reports who still uses the agent; a hard delete is refused while
/// anything does, and goes through once nothing does.
#[test]
fn hard_delete_waits_for_referrers_and_archive_reports_them() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage" }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    let dir = team_agents(home, &alice()).join("triage");

    let before = snapshot(home);
    let error = refused_with(
        delete(
            home,
            &alice(),
            &local(&alice()),
            &OneRoutine,
            params(json!({ "id": id, "hard": true })),
        ),
        "in_use",
    );
    assert_eq!(error["referrers"], json!(["acme/api/backend/standup"]));
    assert_eq!(before, snapshot(home), "a refused delete writes nothing");

    let archived = ok(delete(
        home,
        &alice(),
        &local(&alice()),
        &OneRoutine,
        params(json!({ "id": id })),
    ));
    assert_eq!(archived["used_by"], json!(["acme/api/backend/standup"]));
    assert!(dir.exists());

    let deleted = ok(delete(
        home,
        &alice(),
        &local(&alice()),
        &NoReferrers,
        params(json!({ "id": id, "hard": true })),
    ));
    assert_eq!(deleted, json!({ "id": id, "deleted": true }));
    assert!(!dir.exists());
    refused_with(
        get(home, &alice(), params(json!({ "id": id }))),
        "not_found",
    );
}

/// Two users on one Mac: neither lists, reads, changes or deletes the
/// other's personal agent, even by its exact id.
#[test]
fn seam_personal_agents_stay_private_across_two_users() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    write_org(home, &bob());
    let alices = ok(create_as(
        home,
        &alice(),
        personal(&alice(), "alice"),
        json!({ "name": "Triage" }),
    ));
    let bobs = ok(create_as(
        home,
        &bob(),
        personal(&bob(), "bob"),
        json!({ "name": "Triage" }),
    ));
    let alice_id = alices["id"].as_str().unwrap().to_string();
    assert_eq!(alice_id, "_personal/1/acme/api/_scratch/triage");
    assert_eq!(bobs["id"], "_personal/2/acme/api/_scratch/triage");

    let bob_sees = ok(list(
        home,
        &bob(),
        params(json!({ "includeArchived": true })),
    ));
    let ids: Vec<&str> = bob_sees
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = ["_personal/2/acme/api/_scratch/triage".to_string()]
        .into_iter()
        .chain(starter_ids())
        .collect();
    assert_eq!(ids, expected);

    let before = snapshot(home);
    refused_with(
        get(home, &bob(), params(json!({ "id": alice_id }))),
        "not_found",
    );
    refused_with(
        update(
            home,
            &bob(),
            &local(&bob()),
            params(json!({ "id": alice_id, "baseRevision": 1, "name": "Mine now" })),
        ),
        "not_found",
    );
    refused_with(
        delete(
            home,
            &bob(),
            &local(&bob()),
            &NoReferrers,
            params(json!({ "id": alice_id, "hard": true })),
        ),
        "not_found",
    );
    assert_eq!(before, snapshot(home));
    // Nobody creates in someone else's personal space.
    refused_with(
        create_as(
            home,
            &bob(),
            personal(&alice(), "alice"),
            json!({ "name": "Sneaky" }),
        ),
        "invalid",
    );
}

/// `scope` narrows the list to the caller's own agents or the team's.
#[test]
fn list_scopes_narrow_to_mine_or_team() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    write_org(home, &bob());
    ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Alice team" }),
    ));
    copy_tree(&team_agents(home, &alice()), &team_agents(home, &bob()));
    ok(create_as(
        home,
        &bob(),
        team(),
        json!({ "name": "Bob team" }),
    ));
    ok(create_as(
        home,
        &bob(),
        personal(&bob(), "bob"),
        json!({ "name": "Bob own" }),
    ));

    let names = |scope: &str| -> Vec<String> {
        let rows = ok(list(home, &bob(), params(json!({ "scope": scope }))));
        let mut names: Vec<String> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };
    let mut all: Vec<String> = ["Alice team", "Bob own", "Bob team"]
        .map(str::to_string)
        .into_iter()
        .chain(starter_names())
        .collect();
    all.sort();
    assert_eq!(names("all"), all);
    assert_eq!(names("mine"), ["Bob own", "Bob team"]);
    assert_eq!(names("team"), ["Alice team", "Bob team"]);
    assert_eq!(names("builtin"), starter_names());
    let error = refused_with(
        list(home, &bob(), params(json!({ "scope": "theirs" }))),
        "invalid",
    );
    assert_eq!(error["field"], "scope");
}

/// Every write the tools make lands under `config/agents/`, and nothing ever
/// touches the Weft ledger: no notice, post, inbox or queue entry.
#[test]
fn every_write_lands_under_config_agents_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    // The personal space exists before the audit starts: creating it is the
    // personal team's own setup, not an agent write.
    ok(create_as(
        home,
        &alice(),
        personal(&alice(), "alice"),
        json!({ "name": "Seed" }),
    ));

    let before = snapshot(home);
    let me = local(&alice());
    let team_id = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage", "startup": ["CLAUDE.md"] }),
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mine = ok(create_as(
        home,
        &alice(),
        personal(&alice(), "alice"),
        json!({ "name": "Mine" }),
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Dry", "dryRun": true }),
    ));
    ok(preview(
        home,
        &alice(),
        team(),
        &me,
        params(claude(json!({ "name": "Peek" }))),
    ));
    ok(list(
        home,
        &alice(),
        params(json!({ "includeManifest": true, "includeArchived": true })),
    ));
    ok(get(home, &alice(), params(json!({ "id": team_id }))));
    ok(update(
        home,
        &alice(),
        &me,
        params(json!({ "id": team_id, "baseRevision": 1, "name": "Triage 2" })),
    ));
    ok(delete(
        home,
        &alice(),
        &me,
        &NoReferrers,
        params(json!({ "id": team_id })),
    ));
    ok(unarchive(
        home,
        &alice(),
        &me,
        params(json!({ "id": team_id, "baseRevision": 3 })),
    ));
    ok(restore(
        home,
        &alice(),
        &me,
        params(json!({ "id": team_id, "revision": 1, "baseRevision": 4 })),
    ));
    ok(delete(
        home,
        &alice(),
        &me,
        &NoReferrers,
        params(json!({ "id": mine, "hard": true })),
    ));

    let after = snapshot(home);
    let changed = changed(&before, &after);
    assert!(!changed.is_empty());
    for path in &changed {
        let shown = path.to_string_lossy();
        assert!(
            shown.contains("/config/agents/")
                || path.parent() == Some(home)
                    && path.file_name().is_some_and(|n| matches!(
                        n.to_str(),
                        Some("usage.sqlite" | "usage.sqlite-wal" | "usage.sqlite-shm")
                    )),
            "wrote outside config/agents or the local spend store: {shown}"
        );
    }
    let weft = walkdir::WalkDir::new(home)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with("weft.sqlite3"));
    assert!(!weft, "nothing touches the Weft ledger");
}

/// A starter is listed and read like any agent, every change to it is
/// `read_only` and writes nothing, and get-then-create copies it into an
/// ordinary agent the caller can edit.
#[test]
fn a_starter_is_read_only_through_the_tools_and_copies_by_get_then_create() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let got = ok(get(
        home,
        &alice(),
        params(json!({ "id": "builtin/claude" })),
    ));
    assert_eq!(got["scope"], "builtin");
    assert_eq!(got["created_by"], "CodeSense");
    let revision = got["revision"].as_u64().unwrap();
    assert_eq!(got["resolved"]["agent"]["key"], "builtin/claude", "{got}");

    let before = snapshot(home);
    let message = "built-in agent — Copy to Mine to customize";
    let refusal = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": "builtin/claude", "baseRevision": revision, "name": "Mine" })),
        ),
        "read_only",
    );
    assert_eq!(refusal["message"], message);
    // An input that would not validate is still `read_only`: copying is the fix.
    refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(
                json!({ "id": "builtin/claude", "baseRevision": revision, "runtime": "gemini" }),
            ),
        ),
        "read_only",
    );
    for hard in [false, true] {
        refused_with(
            delete(
                home,
                &alice(),
                &local(&alice()),
                &NoReferrers,
                params(json!({ "id": "builtin/codex", "hard": hard })),
            ),
            "read_only",
        );
    }
    refused_with(
        restore(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": "builtin/claude", "revision": 1, "baseRevision": revision })),
        ),
        "read_only",
    );
    refused_with(
        unarchive(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": "builtin/claude", "baseRevision": revision })),
        ),
        "read_only",
    );
    assert_eq!(before, snapshot(home), "a refused change writes nothing");

    // Copy: the definition minus what CodeSense sets, with a fresh id.
    let definition = &got["definition"];
    let copy = ok(create_as(
        home,
        &alice(),
        personal(&alice(), "alice"),
        json!({
            "id": "my-claude",
            "name": definition["name"],
            "description": definition["description"],
            "runtime": definition["runtime"],
            "startup": definition["startup"],
        }),
    ));
    assert_eq!(copy["id"], "_personal/1/acme/api/_scratch/my-claude");
    assert_eq!(definition["startup"], json!(["CLAUDE.md"]));
    let copied = ok(get(home, &alice(), params(json!({ "id": copy["id"] }))));
    assert_eq!(copied["definition"]["startup"], definition["startup"]);
    assert_eq!(
        copied["resolved"]["manifest"]["entries"], got["resolved"]["manifest"]["entries"],
        "the copy starts with what the starter starts with"
    );
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": copy["id"], "baseRevision": 1, "args": ["--model", "opus"] })),
    ));
}

#[test]
fn an_agent_carries_a_default_face_that_update_changes_or_clears() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Reviewer", "avatar": " builtin-funny:skeptical-goat " }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    let def =
        |home: &Path| ok(get(home, &alice(), params(json!({ "id": id }))))["definition"].clone();
    assert_eq!(def(home)["avatar"], "builtin-funny:skeptical-goat");

    // An update that does not name it keeps it.
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "Reviews" })),
    ));
    assert_eq!(def(home)["avatar"], "builtin-funny:skeptical-goat");

    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "avatar": "" })),
    ));
    assert!(def(home).get("avatar").is_none());
}

/// An invalid `context` is refused by `agent_update` (and
/// `agent_create`) with a field error, and nothing is written.
#[test]
fn ac5_an_invalid_context_is_refused_with_a_field_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(home, &alice(), team(), json!({ "name": "Barb" })));
    let id = saved["id"].as_str().unwrap().to_string();
    let before = snapshot(home);
    let too_many: Vec<Value> = (0..33)
        .map(|i| json!({ "path": format!("d{i}") }))
        .collect();
    for context in [
        json!([{ "path": ".." }]),
        json!([{ "path": "a/../b" }]),
        json!([{ "path": "/etc" }]),
        json!([{ "path": "docs", "dest": "../up" }]),
        Value::Array(too_many),
    ] {
        let error = refused_with(
            update(
                home,
                &alice(),
                &local(&alice()),
                params(json!({ "id": id, "baseRevision": 1, "context": context })),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "context", "{context}: {error}");
        let error = refused_with(
            create_as(
                home,
                &alice(),
                team(),
                json!({ "name": "Other", "context": context }),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "context", "{context}: {error}");
    }
    for overrides in [
        json!([{ "repo": "alderco/codesense", "remove": ["../x"] }]),
        json!([{ "repo": "alderco/codesense", "add": [{ "path": "/abs" }] }]),
        json!([{ "repo": "not-a-repo", "add": [{ "path": "docs" }] }]),
    ] {
        let error = refused_with(
            update(
                home,
                &alice(),
                &local(&alice()),
                params(json!({ "id": id, "baseRevision": 1, "repoOverrides": overrides })),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "repoOverrides", "{overrides}: {error}");
    }
    assert_eq!(before, snapshot(home));
}

/// `context` and repo overrides round-trip through create, update
/// and get; a `null` context goes back to the runtime default.
#[test]
fn context_and_repo_overrides_round_trip_through_the_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Barb",
            "context": [{ "path": "./docs/" }, { "path": "_bmad-output/x", "dest": "plans" }],
            "repoOverrides": [{ "repo": "alderco/codesense", "add": [{ "path": "docs/architecture" }], "remove": [".claude/skills/legacy-*"] }],
        }),
    ));
    assert_eq!(
        saved["context"],
        json!([{ "path": "docs" }, { "path": "_bmad-output/x", "dest": "plans" }])
    );
    assert_eq!(
        saved["repo_overrides"],
        json!([{ "repo": "github.com/alderco/codesense", "add": [{ "path": "docs/architecture" }], "remove": [".claude/skills/legacy-*"] }])
    );
    let id = saved["id"].as_str().unwrap().to_string();
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["definition"]["context"], saved["context"]);
    assert_eq!(got["repo_overrides"], saved["repo_overrides"]);
    let yaml =
        std::fs::read_to_string(team_agents(home, &alice()).join("barb/agent.yaml")).unwrap();
    assert!(yaml.contains("context:"), "{yaml}");

    // An override with nothing in it is removed; the context can be cleared.
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id,
            "baseRevision": 1,
            "context": null,
            "repoOverrides": [{ "repo": "github.com/alderco/codesense" }],
        })),
    ));
    assert_eq!(updated["revision"], 2);
    assert!(
        updated.get("context").is_none_or(Value::is_null),
        "{updated}"
    );
    assert_eq!(updated["repo_overrides"], json!([]));
    assert!(!team_agents(home, &alice())
        .join("barb/repos/github.com/alderco/codesense.yaml")
        .exists());
    let yaml =
        std::fs::read_to_string(team_agents(home, &alice()).join("barb/agent.yaml")).unwrap();
    assert!(
        !yaml.contains("context"),
        "the default is never written: {yaml}"
    );
}

/// `agent_preview {agent, repo}` returns the resolved want set:
/// files, bytes, the tier of each file, and warnings.
#[test]
fn preview_of_a_saved_agent_in_a_repo_returns_the_want_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org_root = write_org(home, &alice());
    let content = path_to_team_content_dir(&org_root, TEAM);
    for (logical, body) in [
        ("docs/a.md", "team"),
        ("docs/b.md", "team"),
        ("_bmad-output/p.md", "plan"),
    ] {
        let path = content.join(logical);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let repo_content = crate::domain::SourceRepoSlug::new("alderco", "codesense")
        .unwrap()
        .physical_path_key()
        .unwrap()
        .append_to(&org_root.join(TEAM).join("repos"))
        .join("content");
    std::fs::create_dir_all(repo_content.join("docs")).unwrap();
    std::fs::write(repo_content.join("docs/a.md"), "repo!").unwrap();
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Barb", "context": [{ "path": "docs" }, { "path": "missing" }] }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();
    let before = snapshot(home);
    let data = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({ "agent": id, "repo": "alderco/codesense" })),
    ));
    let context = &data["context"];
    assert_eq!(context["repo"], "github.com/alderco/codesense");
    assert_eq!(context["totals"], json!({ "files": 2, "bytes": 9 }));
    let files = context["files"].as_array().unwrap();
    assert_eq!(files[0]["destination"], "docs/a.md");
    assert_eq!(files[0]["tier"]["kind"], "repo");
    assert_eq!(files[1]["tier"]["kind"], "team");
    assert_eq!(context["warnings"][0]["code"], "missing");
    assert_eq!(context["warnings"][0]["path"], "missing");
    // The launch preview is still there, as before.
    assert!(data["manifest"].is_object(), "{data}");
    assert_eq!(before, snapshot(home), "a preview writes nothing");

    // A draft previews its own context and overrides too.
    let data = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(json!({
            "name": "Draft",
            "context": [{ "path": "docs", "dest": "d" }],
            "repoOverrides": [{ "repo": "alderco/codesense", "remove": ["d/b.md"] }],
            "repo": "alderco/codesense",
        }))),
    ));
    let files: Vec<&str> = data["context"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["destination"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["d/a.md"]);
}

/// A bare path is accepted wherever an entry is (`context`,
/// `repoOverrides[].add`), and is stored in the object form.
#[test]
fn bare_path_entries_are_accepted_and_stored_as_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Barb",
            "context": ["docs", { "path": "_bmad-output/x", "dest": "plans" }],
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs/architecture"], "remove": [".claude/skills/legacy-*"] }],
        }),
    ));
    assert_eq!(
        saved["context"],
        json!([{ "path": "docs" }, { "path": "_bmad-output/x", "dest": "plans" }])
    );
    assert_eq!(
        saved["repo_overrides"][0]["add"],
        json!([{ "path": "docs/architecture" }])
    );
    let dir = team_agents(home, &alice()).join("barb");
    let agent_yaml = std::fs::read_to_string(dir.join("agent.yaml")).unwrap();
    assert!(agent_yaml.contains("- path: docs"), "{agent_yaml}");
    let override_yaml =
        std::fs::read_to_string(dir.join("repos/github.com/alderco/codesense.yaml")).unwrap();
    assert!(
        override_yaml.contains("- path: docs/architecture"),
        "{override_yaml}"
    );

    // And through agent_update.
    let id = saved["id"].as_str().unwrap().to_string();
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "context": ["notes"] })),
    ));
    assert_eq!(updated["context"], json!([{ "path": "notes" }]));
    // A bare string that is not a valid path is still a field error.
    let error = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 2, "context": [".."] })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "context");
}

/// Overrides are part of the revision: snapshotted with it, covered by
/// its hash, refused on a stale base, and restored with it.
#[test]
fn repo_overrides_are_part_of_the_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(home, &alice(), team(), json!({ "name": "Barb" })));
    let id = saved["id"].as_str().unwrap().to_string();
    let hash_of = |home: &Path| {
        let got = ok(get(home, &alice(), params(json!({ "id": id }))));
        got["resolved"]["agent"]["definition_hash"].clone()
    };
    let h1 = hash_of(home);
    let r2 = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": 1,
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs"] }],
        })),
    ));
    assert_eq!(r2["revision"], 2);
    let h2 = hash_of(home);
    assert_ne!(h1, h2, "the overrides are covered by the hash");
    let dir = team_agents(home, &alice()).join("barb");
    let snapshot = std::fs::read_to_string(dir.join("revisions/2.yml")).unwrap();
    assert!(
        snapshot.contains("github.com/alderco/codesense"),
        "{snapshot}"
    );

    // A stale base changes nothing, overrides included.
    let before = snapshot_dir(&dir);
    let stale = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({
                "id": id, "baseRevision": 1,
                "repoOverrides": [{ "repo": "alderco/codesense", "add": ["other"] }],
            })),
        ),
        "conflict",
    );
    assert_eq!(stale["current_revision"], 2);
    assert_eq!(before, snapshot_dir(&dir));

    // Restoring revision 1 takes the overrides away again; 2 brings them back.
    ok(restore(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "revision": 1, "baseRevision": 2 })),
    ));
    assert!(!dir.join("repos/github.com/alderco/codesense.yaml").exists());
    let r3 = std::fs::read_to_string(dir.join("revisions/3.yml")).unwrap();
    assert!(!r3.contains("repo_overrides"), "{r3}");
    ok(restore(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "revision": 2, "baseRevision": 3 })),
    ));
    assert!(dir
        .join("repos/github.com/alderco/codesense.yaml")
        .is_file());
    let r4 = std::fs::read_to_string(dir.join("revisions/4.yml")).unwrap();
    assert!(r4.contains("github.com/alderco/codesense"), "{r4}");
}

fn snapshot_dir(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    snapshot(dir)
}

/// A bad override file on disk is skipped and reported; it never fails a
/// save or a read.
#[test]
fn a_bad_override_file_is_skipped_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(home, &alice(), team(), json!({ "name": "Barb" })));
    let id = saved["id"].as_str().unwrap().to_string();
    let bad = team_agents(home, &alice()).join("barb/repos/github.com/x/y.yaml");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, "add: {not: [a list}\n").unwrap();
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["repo_overrides"], json!([]));
    assert!(got["repo_override_errors"][0]
        .as_str()
        .is_some_and(|m| m.contains("y.yaml")));
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "d" })),
    ));
    assert_eq!(updated["revision"], 2);
    assert!(updated["preview_error"]
        .as_str()
        .is_some_and(|m| m.contains("y.yaml")));
}

/// `agent_preview {agent}` takes no draft fields, and `org`+`team` name
/// the team it resolves against. `name` stays required to create.
#[test]
fn preview_of_a_saved_agent_refuses_draft_fields_and_create_requires_a_name() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(home, &alice(), team(), json!({ "name": "Barb" })));
    let id = saved["id"].as_str().unwrap().to_string();
    let error = refused_with(
        preview(
            home,
            &alice(),
            team(),
            &local(&alice()),
            params(json!({ "agent": id, "startup": ["CLAUDE.md"] })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "startup");
    let data = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({ "agent": id, "org": ORG, "team": TEAM })),
    ));
    assert_eq!(data["context"]["defaulted"], true);

    let create_schema =
        serde_json::to_value(rmcp::schemars::schema_for!(AgentCreateParams)).unwrap();
    assert!(
        create_schema["required"]
            .as_array()
            .is_some_and(|r| r.contains(&json!("name"))),
        "{create_schema}"
    );
    let preview_schema =
        serde_json::to_value(rmcp::schemars::schema_for!(AgentPreviewParams)).unwrap();
    assert!(!preview_schema["required"]
        .as_array()
        .is_some_and(|r| r.contains(&json!("name"))));
}

/// Tool input refuses unknown keys in a context entry or an override.
#[test]
fn unknown_keys_in_context_input_are_refused_by_name() {
    let bad: Result<AgentUpdateParams, _> = serde_json::from_value(json!({
        "id": "x", "baseRevision": 1, "context": [{ "path": "docs", "dst": "d" }],
    }));
    assert!(bad.unwrap_err().to_string().contains("dst"));
    let bad: Result<AgentUpdateParams, _> = serde_json::from_value(json!({
        "id": "x", "baseRevision": 1, "repoOverrides": [{ "repo": "a/b", "adds": ["docs"] }],
    }));
    assert!(bad.unwrap_err().to_string().contains("adds"));
}

// `allowWake` is a typed field of the definition: set on
// create, kept by an update that does not name it, changed by one that does,
// carried by a copy (get then create), stored as `allowWake`, and a
// non-boolean is refused.
#[test]
fn allow_wake_round_trips_through_create_update_and_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Night shift", "args": ["--dangerously-skip-permissions"], "allowWake": true }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    let def = |id: &str| ok(get(home, &alice(), params(json!({ "id": id }))))["definition"].clone();
    assert_eq!(def(&id)["allowWake"], true);
    let yaml = std::fs::read_to_string(
        team_agents(home, &alice())
            .join(id.rsplit('/').next().unwrap())
            .join("agent.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("allowWake: true"), "{yaml}");
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "nights" })),
    ));
    assert_eq!(
        def(&id)["allowWake"],
        true,
        "an update that does not name it keeps it"
    );
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "allowWake": false })),
    ));
    assert!(
        def(&id).get("allowWake").is_none(),
        "off is the default and not stored"
    );
    // A copy carries it: get, then create its definition's fields.
    let copy = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Night shift 2", "allowWake": true }),
    ));
    assert_eq!(def(copy["id"].as_str().unwrap())["allowWake"], true);
    assert!(serde_json::from_value::<AgentCreateParams>(
        json!({ "name": "x", "runtime": "claude", "allowWake": "yes" })
    )
    .is_err());
}

// `resumeAfterRestart` is a typed field, off by default: set
// on create, kept by an update that does not name it, turned off (and no
// longer stored) by one that does.
#[test]
fn agr19_resume_after_restart_round_trips_through_create_and_update() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Triage", "resumeAfterRestart": true }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    let def = |id: &str| ok(get(home, &alice(), params(json!({ "id": id }))))["definition"].clone();
    assert_eq!(def(&id)["resumeAfterRestart"], true);
    let yaml = std::fs::read_to_string(
        team_agents(home, &alice())
            .join(id.rsplit('/').next().unwrap())
            .join("agent.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("resumeAfterRestart: true"), "{yaml}");
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "inbox" })),
    ));
    assert_eq!(def(&id)["resumeAfterRestart"], true);
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "resumeAfterRestart": false })),
    ));
    assert!(def(&id).get("resumeAfterRestart").is_none());
    let plain = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Plain" }),
    ));
    assert!(def(plain["id"].as_str().unwrap())
        .get("resumeAfterRestart")
        .is_none());
}

/// `agent_get` reports the stored `context` and what a launch gets: the explicit
/// list, or the runtime default while there is none. Set through `agent_update`,
/// read back through `agent_get`, and `agent_preview {agent}` resolves it.
#[test]
fn agent_get_reports_stored_and_effective_context() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(home, &alice(), team(), json!({ "name": "Barb" })));
    let id = saved["id"].as_str().unwrap().to_string();

    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["context"], Value::Null, "{got}");
    assert_eq!(got["context_defaulted"], true);
    let default: Vec<Value> = crate::agents::context::default_context(AgentCli::Claude, false)
        .into_iter()
        .map(|e| json!({ "path": e.path }))
        .collect();
    assert_eq!(got["effective_context"], json!(default));

    // Set → get.
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": 1,
            "context": ["docs", { "path": "_bmad-output/x", "dest": "plans" }],
        })),
    ));
    assert_eq!(updated["revision"], 2);
    let explicit = json!([{ "path": "docs" }, { "path": "_bmad-output/x", "dest": "plans" }]);
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["context"], explicit);
    assert_eq!(got["effective_context"], explicit);
    assert_eq!(got["context_defaulted"], false);

    // The saved agent's preview resolves that context, not the default.
    let preview = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({ "agent": id })),
    ));
    assert_eq!(preview["context"]["defaulted"], false, "{preview}");
    let sources: Vec<(&str, &str)> = preview["context"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] == "context")
        .map(|e| (e["path"].as_str().unwrap(), e["source"].as_str().unwrap()))
        .collect();
    assert_eq!(
        sources,
        [("docs", "context"), ("_bmad-output/x", "context")]
    );

    // A stale base is a conflict and changes nothing.
    let dir = team_agents(home, &alice()).join("barb");
    let before = snapshot_dir(&dir);
    let stale = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 1, "context": ["other"] })),
        ),
        "conflict",
    );
    assert_eq!(stale["current_revision"], 2);
    assert_eq!(before, snapshot_dir(&dir));

    // An invalid entry is refused by field, with the rule it breaks.
    let error = refused_with(
        update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 2, "context": ["docs/../secrets"] })),
        ),
        "invalid",
    );
    assert_eq!(error["field"], "context");
    assert!(error["message"].as_str().unwrap().contains(".."), "{error}");
    assert_eq!(before, snapshot_dir(&dir));

    // `[]` is an explicit empty context: a launch gets none, not the default.
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "context": [] })),
    ));
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["context"], json!([]));
    assert_eq!(got["effective_context"], json!([]));
    assert_eq!(got["context_defaulted"], false);
}

/// Replacing `context` or a repo override through `agent_update` keeps the keys a
/// newer build wrote on the entries it keeps and on the override, as the editor's
/// in-place edits do.
#[test]
fn a_context_replace_keeps_keys_a_newer_build_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Barb",
            "context": ["docs", { "path": "docs", "dest": "mirror" }, "notes"],
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs/architecture"] }],
        }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();
    let dir = team_agents(home, &alice()).join("barb");
    // A newer build's keys, on an entry (the remapped one of two sharing a
    // path), on an override entry and on the override.
    let yaml_path = dir.join("agent.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    let yaml = yaml.replacen("  dest: mirror\n", "  dest: mirror\n  future_entry: 7\n", 1);
    assert!(yaml.contains("future_entry"), "{yaml}");
    std::fs::write(&yaml_path, yaml).unwrap();
    let o_path = dir.join("repos/github.com/alderco/codesense.yaml");
    let o = std::fs::read_to_string(&o_path).unwrap();
    let o = o.replacen(
        "- path: docs/architecture\n",
        "- path: docs/architecture\n  future_add: x\n",
        1,
    );
    std::fs::write(&o_path, format!("{o}future_override: true\n")).unwrap();

    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": 1,
            "context": ["./docs/", "plans", { "path": "docs", "dest": "mirror" }],
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs/architecture", "more"], "remove": ["x/*"] }],
        })),
    ));
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    assert!(!yaml.contains("notes"), "{yaml}");
    // The key stayed on the entry it was written on, nowhere else.
    let stored: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let entries: Vec<(String, Option<String>, Option<i64>)> = stored["context"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["path"].as_str().unwrap().to_string(),
                e.get("dest").and_then(|d| d.as_str()).map(str::to_string),
                e.get("future_entry").and_then(|v| v.as_i64()),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("docs".to_string(), None, None),
            ("plans".to_string(), None, None),
            ("docs".to_string(), Some("mirror".to_string()), Some(7)),
        ],
        "{yaml}"
    );
    let o = std::fs::read_to_string(&o_path).unwrap();
    assert!(o.contains("future_add: x"), "{o}");
    assert!(o.contains("future_override: true"), "{o}");
    assert!(o.contains("more"), "{o}");

    // `{repo}` alone still removes the override, extra keys or not.
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(
            json!({ "id": id, "baseRevision": 2, "repoOverrides": [{ "repo": "alderco/codesense" }] }),
        ),
    ));
    assert!(!o_path.exists());
}

/// `agent_get` also reports what a team-workspace launch gets: the explicit
/// `context`, or the runtime default (shared with repo launches), computed
/// by the same function the launch path uses. Checked
/// against `agent_preview {agent, teamWorkspace:true}` for both runtimes.
#[test]
fn agent_get_reports_the_team_workspace_context() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    for (name, runtime, kind) in [
        ("Barb", "claude", AgentCli::Claude),
        ("Bubbles", "codex", AgentCli::Codex),
    ] {
        let saved = ok(create_as(
            home,
            &alice(),
            team(),
            json!({ "name": name, "runtime": runtime }),
        ));
        let id = saved["id"].as_str().unwrap().to_string();
        let got = ok(get(home, &alice(), params(json!({ "id": id }))));
        let paths = |entries: Vec<crate::agents::context::ContextEntry>| -> Value {
            json!(entries
                .into_iter()
                .map(|e| json!({ "path": e.path }))
                .collect::<Vec<_>>())
        };
        assert_eq!(
            got["effective_context"],
            paths(crate::agents::context::default_context(kind, false)),
            "{runtime}: {got}"
        );
        assert_eq!(
            got["effective_context_team_workspace"],
            paths(crate::agents::context::default_context(kind, true)),
            "{runtime}: {got}"
        );
        assert_eq!(
            got["effective_context"], got["effective_context_team_workspace"],
            "{runtime}"
        );

        // The launch path's team-workspace default is the same list.
        let preview = ok(preview(
            home,
            &alice(),
            team(),
            &local(&alice()),
            params(json!({ "agent": id, "teamWorkspace": true })),
        ));
        let defaults: Vec<Value> = preview["context"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["source"] == "default")
            .map(|e| json!({ "path": e["path"] }))
            .collect();
        assert_eq!(
            json!(defaults),
            got["effective_context_team_workspace"],
            "{runtime}: {preview}"
        );

        // An explicit list is what both launches get.
        ok(update(
            home,
            &alice(),
            &local(&alice()),
            params(json!({ "id": id, "baseRevision": 1, "context": ["docs"] })),
        ));
        let got = ok(get(home, &alice(), params(json!({ "id": id }))));
        assert_eq!(got["effective_context"], json!([{ "path": "docs" }]));
        assert_eq!(
            got["effective_context_team_workspace"],
            json!([{ "path": "docs" }])
        );
    }
}

/// `context: null` on create is the same as omitting it (the runtime default),
/// and every reply says `context: null` while it is defaulted, as agent_get does.
#[test]
fn a_null_context_is_the_default_in_every_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Barb", "context": null }),
    ));
    assert!(
        saved.as_object().unwrap().contains_key("context"),
        "{saved}"
    );
    assert_eq!(saved["context"], Value::Null);
    let id = saved["id"].as_str().unwrap().to_string();
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["context"], Value::Null);
    assert_eq!(got["context_defaulted"], true);

    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "context": ["docs"] })),
    ));
    assert_eq!(updated["context"], json!([{ "path": "docs" }]));
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "context": null })),
    ));
    assert!(
        updated.as_object().unwrap().contains_key("context"),
        "{updated}"
    );
    assert_eq!(updated["context"], Value::Null);
    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["context_defaulted"], true);
}

/// agent_get's `context` is `{path, dest?}` only: a newer build's keys stay in
/// `definition`, so passing `context` straight back to agent_update works and
/// keeps those keys.
#[test]
fn agent_get_context_round_trips_through_agent_update() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Barb",
            "context": ["docs", { "path": "notes", "dest": "n" }],
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs/architecture"] }],
        }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();
    let o_path = team_agents(home, &alice()).join("barb/repos/github.com/alderco/codesense.yaml");
    let o = std::fs::read_to_string(&o_path).unwrap();
    let o = o.replacen(
        "- path: docs/architecture\n",
        "- path: docs/architecture\n  future_add: x\n",
        1,
    );
    assert!(o.contains("future_add"), "{o}");
    std::fs::write(&o_path, o).unwrap();
    let yaml_path = team_agents(home, &alice()).join("barb/agent.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    let yaml = yaml.replacen("- path: docs\n", "- path: docs\n  future_entry: 7\n", 1);
    assert!(yaml.contains("future_entry"), "{yaml}");
    std::fs::write(&yaml_path, yaml).unwrap();

    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    let context = got["context"].clone();
    assert_eq!(
        context,
        json!([{ "path": "docs" }, { "path": "notes", "dest": "n" }]),
        "{got}"
    );
    assert_eq!(got["effective_context"], context);
    assert_eq!(got["definition"]["context"][0]["future_entry"], 7, "{got}");

    assert_eq!(
        got["repo_overrides"][0]["add"],
        json!([{ "path": "docs/architecture" }]),
        "{got}"
    );

    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": got["revision"],
            "context": context,
            "repoOverrides": got["repo_overrides"],
        })),
    ));
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["context"], context);
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    assert!(yaml.contains("future_entry: 7"), "{yaml}");
    let o = std::fs::read_to_string(&o_path).unwrap();
    assert!(o.contains("future_add: x"), "{o}");
}

/// An override a newer build stored with only its own keys (no `add`, no
/// `remove`) is left out of agent_get's list, so passing that list back to
/// agent_update cannot remove it (`{repo}` alone removes an override).
#[test]
fn an_override_with_only_newer_keys_survives_a_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let saved = ok(create_as(
        home,
        &alice(),
        team(),
        json!({
            "name": "Barb",
            "repoOverrides": [{ "repo": "alderco/codesense", "add": ["docs"] }],
        }),
    ));
    let id = saved["id"].as_str().unwrap().to_string();
    let o_path = team_agents(home, &alice()).join("barb/repos/github.com/alderco/codesense.yaml");
    std::fs::write(&o_path, "future_override: true\n").unwrap();

    let got = ok(get(home, &alice(), params(json!({ "id": id }))));
    assert_eq!(got["repo_overrides"], json!([]), "{got}");
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({
            "id": id, "baseRevision": got["revision"],
            "context": got["context"],
            "repoOverrides": got["repo_overrides"],
        })),
    ));
    let o = std::fs::read_to_string(&o_path).unwrap();
    assert!(o.contains("future_override: true"), "{o}");
}

/// `env` is set on create, read back by get (as stored and in the launch it
/// resolves to), previewed, replaced whole by an update (`{}` or `null` clears it), kept
/// by an update that does not name it, part of each revision, and refused
/// with a field error when a launch could not carry it.
#[test]
fn env_round_trips_through_the_tools_and_is_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let env = json!({ "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1", "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD": "1" });
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({ "name": "Isolated", "env": env }),
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["preview"]["launch"]["env"], env);
    let got = |id: &str| ok(get(home, &alice(), params(json!({ "id": id }))));
    let first = got(&id);
    assert_eq!(first["definition"]["env"], env);
    assert_eq!(first["resolved"]["launch"]["env"], env);
    let yaml = std::fs::read_to_string(
        team_agents(home, &alice())
            .join(id.rsplit('/').next().unwrap())
            .join("agent.yaml"),
    )
    .unwrap();
    assert!(
        yaml.contains("CLAUDE_CODE_DISABLE_AUTO_MEMORY: '1'"),
        "{yaml}"
    );

    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(json!({ "name": "X", "env": { "A_B": "c" } }))),
    ));
    assert_eq!(previewed["launch"]["env"], json!({ "A_B": "c" }));
    let saved = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(json!({ "agent": id })),
    ));
    assert_eq!(saved["launch"]["env"], env);

    // An update that does not name it keeps it; one that does replaces it whole,
    // as a new revision with a new definition hash.
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 1, "description": "isolated" })),
    ));
    assert_eq!(got(&id)["definition"]["env"], env);
    let before = got(&id)["resolved"]["agent"]["definition_hash"].clone();
    let updated = ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 2, "env": { "ONLY": "this" } })),
    ));
    assert_eq!(updated["revision"], 3);
    let now = got(&id);
    assert_eq!(now["definition"]["env"], json!({ "ONLY": "this" }));
    assert_ne!(now["resolved"]["agent"]["definition_hash"], before);
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 3, "env": {} })),
    ));
    assert!(got(&id)["definition"].get("env").is_none());
    // `null` clears it too; omitting it leaves it as it is.
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 4, "env": { "AGAIN": "1" } })),
    ));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 5, "name": "Isolated, renamed" })),
    ));
    assert_eq!(got(&id)["definition"]["env"], json!({ "AGAIN": "1" }));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({ "id": id, "baseRevision": 6, "env": null })),
    ));
    assert!(got(&id)["definition"].get("env").is_none());

    // What a launch could not carry, and a value that is not a string, is refused, naming
    // `env`, and writes nothing.
    let before = snapshot(home);
    let too_many: serde_json::Map<String, Value> =
        (0..65).map(|i| (format!("V{i}"), json!("x"))).collect();
    for env in [
        json!({ "1BAD": "x" }),
        json!({ "HAS-DASH": "x" }),
        json!({ "": "x" }),
        json!({ "NUL": "a\u{0}b" }),
        json!({ "LONG": "x".repeat(4097) }),
        Value::Object(too_many),
        json!({ "NUMBER": 1 }),
        json!({ "FLAG": true }),
        json!({ "NOTHING": null }),
        json!({ "LIST": ["a"] }),
    ] {
        let error = refused_with(
            create_as(
                home,
                &alice(),
                team(),
                json!({ "name": "Bad env", "env": env }),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "env", "{error}");
        let error = refused_with(
            update(
                home,
                &alice(),
                &local(&alice()),
                params(json!({ "id": id, "baseRevision": 7, "env": env })),
            ),
            "invalid",
        );
        assert_eq!(error["field"], "env", "{error}");
    }
    assert_eq!(before, snapshot(home));
}

#[test]
fn default_role_updates_keep_set_and_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let made = ok(create_as(
        home,
        &alice(),
        Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        json!({"name":"Role bot", "role":"reviewer"}),
    ));
    let id = made["id"].as_str().unwrap();
    let get_role =
        || ok(get(home, &alice(), params(json!({"id":id}))))["definition"]["role"].clone();
    assert_eq!(get_role(), json!("reviewer"));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({"id":id,"baseRevision":1,"description":"keeps its queue"})),
    ));
    assert_eq!(get_role(), json!("reviewer"));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({"id":id,"baseRevision":2,"role":"qa"})),
    ));
    assert_eq!(get_role(), json!("qa"));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({"id":id,"baseRevision":3,"role":null})),
    ));
    assert!(get_role().is_null());
}

#[test]
fn cap_raises_by_mcp_and_file_stay_capped_until_native_accept_and_lowering_ratchets() {
    use crate::session_usage::{
        event::Tokens,
        spend_caps::{self, Budget},
        tests::event,
        UsageStore,
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_org(home, &alice());
    let created = ok(create_as(
        home,
        &alice(),
        team(),
        json!({"name":"Capped","budgetUsd":{"perDay":1.0}}),
    ));
    let id = created["id"].as_str().unwrap();
    let key = AgentKey::parse(id).unwrap();
    let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
    usage
        .attribute("cap-session", ORG, Some(TEAM), id, "agent-handle", None)
        .unwrap();
    let mut call = event(
        "cost",
        "cap-session",
        "claude-opus-5-5",
        chrono::Utc::now().timestamp_millis(),
        Tokens::default(),
    );
    call.reported_cost_usd = Some(2.0);
    usage.ingest(&[call]).unwrap();
    assert!(spend_caps::refuse_agent(home, &alice(), &key)
        .unwrap_err()
        .contains("cap $1.0000"));
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({"id":id,"baseRevision":1,"budgetUsd":{"perDay":10.0}})),
    ));
    let got = ok(get(home, &alice(), params(json!({"id":id}))));
    assert_eq!(got["budgetPending"], true);
    assert_eq!(got["spend"]["state"], "reached");
    assert!(
        crate::agents::launch::prepare(home, &alice(), &key, Default::default())
            .unwrap_err()
            .contains("cap $1.0000")
    );
    let path = team_agents(home, &alice()).join("capped/agent.yaml");
    let mut agent: Agent = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    agent.budget_usd = Some(Budget {
        per_day: Some(0.5),
        per_week: None,
    });
    std::fs::write(&path, serde_yaml::to_string(&agent).unwrap()).unwrap();
    assert!(spend_caps::refuse_agent(home, &alice(), &key)
        .unwrap_err()
        .contains("cap $0.5000"));
    agent.budget_usd = Some(Budget {
        per_day: Some(20.0),
        per_week: None,
    });
    std::fs::write(&path, serde_yaml::to_string(&agent).unwrap()).unwrap();
    assert!(spend_caps::refuse_agent(home, &alice(), &key)
        .unwrap_err()
        .contains("cap $0.5000"));
    assert!(spend_caps::accept_current(
        home,
        &alice(),
        "agent",
        id,
        Some(Budget {
            per_day: Some(10.0),
            per_week: None
        })
    )
    .is_err());
    spend_caps::accept_current(
        home,
        &alice(),
        "agent",
        id,
        Some(Budget {
            per_day: Some(20.0),
            per_week: None,
        }),
    )
    .unwrap();
    assert!(spend_caps::refuse_agent(home, &alice(), &key).is_ok());
    ok(update(
        home,
        &alice(),
        &local(&alice()),
        params(json!({"id":id,"baseRevision":2,"budgetUsd":null})),
    ));
    assert_eq!(
        ok(get(home, &alice(), params(json!({"id":id}))))["budgetPending"],
        true
    );
    spend_caps::accept_current(home, &alice(), "agent", id, None).unwrap();
    assert_eq!(
        ok(get(home, &alice(), params(json!({"id":id}))))["budgetPending"],
        false
    );
}

/// Through agent_preview, a parent kit team's skill is provided
/// to a child team's agent only once the parent shares it. With no
/// `config/sharing.yml` the preview names the parent instead.
#[test]
fn ghi3_agent_preview_provides_a_parent_skill_once_shared() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let org_root = write_org(home, &alice());
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n    parent_team: kit\n  - name: kit\n"),
    )
    .unwrap();
    let skill = path_to_team_content_dir(&org_root, "kit").join(".claude/skills/kit/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(skill, "# Kit\n").unwrap();
    let draft = json!({ "name": "Kit user", "startup": [".claude/skills/kit/"] });
    let shares_nothing = crate::context_repo::inheritance::shares_nothing_warning("kit");
    let names_kit = |previewed: &Value| {
        previewed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["message"] == shares_nothing.as_str())
    };

    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(draft.clone())),
    ));
    assert_eq!(previewed["provides"]["skills"], json!([]), "{previewed}");
    assert!(names_kit(&previewed), "{previewed}");

    std::fs::create_dir_all(org_root.join("kit/config")).unwrap();
    std::fs::write(
        org_root.join("kit/config/sharing.yml"),
        "share: ['content/**']\n",
    )
    .unwrap();
    let previewed = ok(preview(
        home,
        &alice(),
        team(),
        &local(&alice()),
        params(claude(draft)),
    ));
    assert_eq!(
        previewed["provides"]["skills"],
        json!(["kit"]),
        "{previewed}"
    );
    assert_eq!(previewed["manifest"]["errors"], json!([]), "{previewed}");
    assert!(!names_kit(&previewed), "{previewed}");
}
