// SPDX-License-Identifier: MIT
//! Projection into a private LOCAL official CLI. Never serialized into remote config or receipts.
use super::config::{Endpoint, Grant};
use crate::install::agent_account_store::AgentProvider;
use serde_json::{json, Map, Value};
#[derive(Default)]
pub(crate) struct Projection {
    pub servers: Map<String, Value>,
    pub allowed: Vec<String>,
    pub denied: Vec<String>,
}
const RESERVED: &str = "codesense_remote_task";
fn literal(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
pub(crate) fn project(grant: &Grant, provider: AgentProvider) -> Result<Projection, String> {
    let mut result = Projection::default();
    for (id, server) in grant {
        if matches!(id.as_str(), RESERVED | "codex_apps")
            || !literal(id)
            || (provider == AgentProvider::Claude && id.contains("__"))
        {
            return Err("MCP server name is reserved or unsupported for native projection".into());
        }
        let mut row = match &server.endpoint {
            Endpoint::Remote(_) => {
                return Err("remote workspace MCP requires a native remote launch".into())
            }
            Endpoint::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                if provider == AgentProvider::Codex {
                    json!({"command":command,"args":args,"env":env,"cwd":cwd})
                } else {
                    // The fixed wrapper preserves the captured cwd without shell interpolation.
                    let mut argv = vec![
                        "-c".to_string(),
                        "cd \"$1\" && shift && exec \"$@\"".into(),
                        "codesense-mcp".into(),
                        cwd.to_string_lossy().into_owned(),
                        command.clone(),
                    ];
                    argv.extend(args.iter().cloned());
                    json!({"type":"stdio","command":"/bin/sh","args":argv,"env":env})
                }
            }
            Endpoint::Http { url, headers } => {
                if provider == AgentProvider::Codex {
                    json!({"url":url,"http_headers":headers})
                } else {
                    json!({"type":"http","url":url,"headers":headers})
                }
            }
        };
        if provider == AgentProvider::Codex {
            row["enabled"] = json!(true);
            row["environment_id"] = json!("local");
            row["required"] = json!(true);
            row["default_tools_approval_mode"] = json!("approve");
            if let Some(enabled) = &server.enabled_tools {
                row["enabled_tools"] = json!(enabled);
            }
            row["disabled_tools"] = json!(server.disabled_tools);
        } else {
            let prefix = format!("mcp__{id}__");
            match &server.enabled_tools {
                None => result.allowed.push(format!("{prefix}*")),
                Some(names) => {
                    for name in names {
                        if !literal(name) {
                            return Err("MCP tool policy cannot be projected exactly".into());
                        }
                        result.allowed.push(format!("{prefix}{name}"));
                    }
                }
            }
            for name in &server.disabled_tools {
                if !literal(name) {
                    return Err("MCP tool policy cannot be projected exactly".into());
                }
                result.denied.push(format!("{prefix}{name}"));
            }
        }
        result.servers.insert(id.clone(), row);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    fn native_policy_preserves_allow_deny_and_local_placement() {
        let grant = BTreeMap::from([(
            "github".into(),
            super::super::config::Server {
                endpoint: Endpoint::Http {
                    url: "https://example.invalid/mcp".into(),
                    headers: BTreeMap::from([(
                        "Authorization".into(),
                        "synthetic-local-only".into(),
                    )]),
                },
                enabled_tools: Some(BTreeSet::from(["read".into(), "write".into()])),
                disabled_tools: BTreeSet::from(["write".into()]),
            },
        )]);
        let codex = project(&grant, AgentProvider::Codex).unwrap();
        assert_eq!(codex.servers["github"]["environment_id"], "local");
        assert_eq!(
            codex.servers["github"]["default_tools_approval_mode"],
            "approve"
        );
        assert_eq!(codex.servers["github"]["required"], true);
        assert_eq!(codex.servers["github"]["disabled_tools"], json!(["write"]));
        let claude = project(&grant, AgentProvider::Claude).unwrap();
        assert_eq!(
            claude.allowed,
            vec!["mcp__github__read", "mcp__github__write"]
        );
        assert_eq!(claude.denied, vec!["mcp__github__write"]);
        let mut bad = grant;
        let server = bad.remove("github").unwrap();
        bad.insert("codesense_remote_task__extra".into(), server.clone());
        assert!(project(&bad, AgentProvider::Claude).is_err());
        bad.clear();
        bad.insert("codex_apps".into(), server.clone());
        assert!(project(&bad, AgentProvider::Codex).is_err());
        bad.clear();
        bad.insert(RESERVED.into(), server);
        assert!(project(&bad, AgentProvider::Codex).is_err());
    }
}
