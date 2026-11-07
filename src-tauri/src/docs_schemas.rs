// SPDX-License-Identifier: MIT
//! Export the authoring schemas from the serde models, never from a second field list.
use codesense_pty_core::agent_cli::AgentCli;
use rmcp::schemars::{schema_for, JsonSchema};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const SOURCES: &[&str] = &[
    "src-tauri/src/agents/model.rs",
    "src-tauri/src/agents/context.rs",
    "src-tauri/src/agents/adapter.rs",
    "src-tauri/crates/codesense-host-proto/src/agent_cli.rs",
    "src-tauri/src/routines/model.rs",
    "src-tauri/src/context_query/model.rs",
    "src-tauri/src/queue_roles.rs",
    "src-tauri/src/session_usage/spend_caps.rs",
    "src-tauri/src/docs_schemas.rs",
];

fn schema<T: JsonSchema>() -> Value {
    // Input schemas include serde-defaulted fields even when serializing omits them.
    serde_json::to_value(schema_for!(T)).unwrap()
}

/// Export registered identities and listing/remote facts for frontend consumers. Keeping
/// unlisted metadata lets promotion change only the backend registry and regenerated data.
fn agent_cli_metadata(listed: &[AgentCli]) -> Vec<Value> {
    AgentCli::every_cli()
        .map(|cli| {
            let desktop = crate::agents::adapter::desktop(cli);
            json!({
                "runtime": AgentCli::as_str(cli),
                "binary": codesense_pty_core::agent_cli::descriptor(cli).binary,
                "listed": listed.contains(&cli),
                "supportsRemoteLaunch": desktop.supports_remote_launch,
                "preview": desktop.preview,
                "modelEndpoint": desktop.model_endpoint.is_some(),
                "permissionsDisplayOnly": desktop.permission_display.is_some(),
            })
        })
        .collect()
}

#[test]
fn frontend_registry_preserves_unlisted_metadata_and_promotes_from_all() {
    let registered: Vec<_> = AgentCli::every_cli().collect();
    for listed in [&AgentCli::ALL[..], &registered[..]] {
        let rows = agent_cli_metadata(listed);
        let visible: Vec<_> = rows
            .iter()
            .filter(|row| row["listed"] == true)
            .map(|row| row["runtime"].as_str().unwrap())
            .collect();
        assert_eq!(
            visible,
            listed
                .iter()
                .copied()
                .map(AgentCli::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(rows.len(), registered.len());
        for (cli, row) in registered.iter().zip(&rows) {
            assert_eq!(
                row["supportsRemoteLaunch"],
                crate::agents::adapter::desktop(*cli).supports_remote_launch
            );
            let desktop = crate::agents::adapter::desktop(*cli);
            assert_eq!(row["preview"], desktop.preview);
            assert_eq!(row["modelEndpoint"], desktop.model_endpoint.is_some());
            assert_eq!(
                row["permissionsDisplayOnly"],
                desktop.permission_display.is_some()
            );
            assert_eq!(
                row["binary"],
                codesense_pty_core::agent_cli::descriptor(*cli).binary
            );
        }
    }
    let rows = agent_cli_metadata(&AgentCli::ALL);
    let hermes = rows
        .iter()
        .find(|row| row["runtime"] == AgentCli::as_str(AgentCli::Hermes))
        .unwrap();
    assert_eq!(hermes["listed"], true);
}

#[test]
fn export_model_schemas() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let sources = SOURCES.iter().map(|source| source.to_string()).chain(
        AgentCli::every_cli()
            .map(|cli| format!("src-tauri/src/agents/adapter/{}.rs", AgentCli::as_str(cli))),
    );
    let hashes: serde_json::Map<String, Value> = sources
        .map(|source| {
            let bytes = std::fs::read(root.join(&source)).unwrap();
            (
                source.to_string(),
                json!(format!("{:x}", Sha256::digest(bytes))),
            )
        })
        .collect();
    let fixture = json!({
        "sources": hashes,
        "agentClis": agent_cli_metadata(&AgentCli::ALL),
        "schemas": {
            "agent-yaml": schema::<crate::agents::model::Agent>(),
            "routine-yml": schema::<crate::routines::model::RoutineDef>(),
            "role-yaml": schema::<crate::queue_roles::RoleConfig>()
        }
    });
    let rendered = format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap());
    let path = root.join("src-tauri/src/docs_schemas.generated.json");
    if std::env::var_os("UPDATE_DOCS_SCHEMAS").is_some() {
        std::fs::write(path, rendered).unwrap();
    } else {
        assert_eq!(
            std::fs::read_to_string(path).expect("run with UPDATE_DOCS_SCHEMAS=1 to export"),
            rendered,
            "serde model schemas drifted; export the real Rust fixture, then pnpm gen:docs"
        );
    }
}

#[test]
fn schemas_accept_custom_yaml_wire_shapes() {
    let agent = schema::<crate::agents::model::Agent>();
    for context in [
        json!(["docs"]),
        json!([{"path":"docs", "dest":"reference", "future":42}]),
        json!(null),
    ] {
        let wire = json!({"id":"reviewer","name":"Reviewer","runtime":"codex", "context":context,
            "env":{"CHANNEL":"review"},"future":{"nested":[true]}});
        assert!(jsonschema::is_valid(&agent, &wire), "{wire}");
        let parsed: crate::agents::model::Agent = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed.env.get("CHANNEL").unwrap(), "review");
    }
    let routine = schema::<crate::routines::model::RoutineDef>();
    let wire = json!({"id":"review","name":"Review","owner":"alice","agent":"reviewer","task":"Review",
        "where":{"kind":"team_workspace","future":true},"policy":{"max_runtime":"5m"},"revision":1,
        "variables":{"n":{"type":"number","default":4}},
        "triggers":[{"event":{"kind":"metadata_change","metadata":{"selection":{"where":{"kinds":["story"]}},
            "target":{"key":"status","op":"eq","value":"review"}}}}]});
    assert!(jsonschema::is_valid(&routine, &wire), "{routine}");
    let parsed: crate::routines::model::RoutineDef =
        serde_yaml::from_str(&serde_yaml::to_string(&wire).unwrap()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.variables["n"].default.as_deref(), Some("4"));
}

#[test]
fn docs_context_query_order() {
    use crate::context_query::{
        engine::{DiskBodyReader, Viewer},
        index::{IndexRoot, IndexState},
        model::{ContextQuery, Layer},
    };
    let docs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("codesense-docs/getting-started/content");
    let mut state = IndexState::default();
    state.reconcile(vec![IndexRoot {
        dir: docs,
        org: None,
        team: None,
        layer: Layer::AllOrgs,
    }]);
    let snapshot = state.snapshot(1, true);
    for (text, expected_page) in [
        ("metadata trigger", "arc-g-routines/trigger-cookbook.html"),
        ("command routine", "arc-g-routines/command-routines.html"),
        ("role_save", "arc-f-inboxes-and-triage/roles.html"),
    ] {
        let q: ContextQuery = serde_json::from_value(json!({"text":text,"limit":10000})).unwrap();
        let result = snapshot
            .query(&q, &Viewer::default(), &DiskBodyReader)
            .unwrap();
        let html: Vec<_> = result
            .rows
            .iter()
            .filter(|r| r.rel_path.ends_with(".html"))
            .map(|r| &r.rel_path)
            .collect();
        assert!(
            html.iter().any(|path| path.as_str() == expected_page),
            "{text} must find {expected_page}; HTML results: {html:?}"
        );
        println!(
            "DOC_QUERY {text} (total={}, truncated={}): {}",
            result.total,
            result.text_scan_truncated,
            serde_json::to_string(&html).unwrap()
        );
    }
}
