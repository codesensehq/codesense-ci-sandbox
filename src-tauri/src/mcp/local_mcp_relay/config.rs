// SPDX-License-Identifier: MIT
//! Trusted profile parsing. Desktop values and remote literals have separate execution paths.
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

// Deliberately no Debug: commands/arguments, URLs, and headers may contain secrets.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Endpoint {
    Remote(codesense_host_proto::reverse_mcp::RemoteStdioEntry),
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Server {
    pub(crate) endpoint: Endpoint,
    pub(super) enabled_tools: Option<BTreeSet<String>>,
    pub(super) disabled_tools: BTreeSet<String>,
}
pub(crate) type Grant = BTreeMap<String, Server>;

fn expand(raw: &str, env: &impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut remaining = raw;
    let mut result = String::new();
    while let Some(at) = remaining.find("${") {
        result.push_str(&remaining[..at]);
        let rest = &remaining[at + 2..];
        let end = rest
            .find('}')
            .ok_or("invalid local MCP environment expansion")?;
        let key = &rest[..end];
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("unsupported local MCP environment expansion".into());
        }
        result.push_str(&env(key).ok_or("local MCP environment value unavailable")?);
        remaining = &rest[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}
fn strings(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("invalid MCP string list".into())
            })
            .collect(),
        _ => Err("invalid MCP string list".into()),
    }
}
fn string_map(value: Option<&Value>) -> Result<BTreeMap<String, String>, String> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(values)) => values
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    v.as_str().ok_or("invalid MCP string map")?.into(),
                ))
            })
            .collect(),
        _ => Err("invalid MCP string map".into()),
    }
}

pub(crate) fn parse(
    root: &Value,
    key: &str,
    bridge: &str,
    allow: Option<&[String]>,
    cwd: &std::path::Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Grant, String> {
    parse_selection(root, key, Some(bridge), allow, cwd, env)
}

/// The local managed launch already selected its canonical scope, including its
/// ordinary CodeSense stdio bridge. No remote authority is introduced here.
pub(crate) fn parse_local_scope(
    root: &Value,
    key: &str,
    cwd: &std::path::Path,
) -> Result<Grant, String> {
    let grant = parse_selection(root, key, None, None, cwd, |key| std::env::var(key).ok())?;
    if grant
        .values()
        .any(|server| matches!(server.endpoint, Endpoint::Remote(_)))
    {
        return Err("local Codex scope requires desktop MCP endpoints".into());
    }
    Ok(grant)
}

fn parse_selection(
    root: &Value,
    key: &str,
    bridge: Option<&str>,
    allow: Option<&[String]>,
    cwd: &std::path::Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Grant, String> {
    let map = root
        .get(key)
        .and_then(Value::as_object)
        .ok_or("local MCP server map missing")?;
    let mut grant = Grant::new();
    for (id, value) in map {
        if (bridge == Some(id.as_str()) || id == "codesense")
            && value.get("executionLocation").and_then(Value::as_str) == Some("remoteWorkspace")
        {
            return Err("reserved remote MCP server identity".into());
        }
        if bridge.is_some_and(|bridge| id == bridge || id == "codesense")
            || allow.is_some_and(|ids| !ids.iter().any(|s| s.trim() == id))
        {
            continue;
        }
        let row = value
            .as_object()
            .ok_or("invalid local MCP server configuration")?;
        if row.get("executionLocation").and_then(Value::as_str) == Some("remoteWorkspace") {
            let entry = parse_remote(id, row)?;
            if let Some(entry) = entry {
                let generated = entry.id.clone();
                if grant
                    .insert(
                        generated,
                        Server {
                            endpoint: Endpoint::Remote(entry),
                            enabled_tools: None,
                            disabled_tools: BTreeSet::new(),
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate generated MCP server identity".into());
                }
            }
            continue;
        }
        if row
            .get("executionLocation")
            .is_some_and(|v| v.as_str() != Some("desktop"))
        {
            return Err("unsupported MCP execution location".into());
        }
        // Policy/auth keys outside this finite subset are never silently discarded.
        const FIELDS: &[&str] = &[
            "executionLocation",
            "type",
            "command",
            "args",
            "env",
            "cwd",
            "url",
            "headers",
            "http_headers",
            "env_http_headers",
            "bearer_token_env_var",
            "enabled",
            "enabled_tools",
            "disabled_tools",
        ];
        if row.keys().any(|k| !FIELDS.contains(&k.as_str())) {
            return Err("unsupported local MCP configuration or authentication field".into());
        }
        match row.get("enabled") {
            Some(Value::Bool(false)) => continue,
            None | Some(Value::Bool(true)) => {}
            _ => return Err("invalid MCP enabled policy".into()),
        }
        let tools = row
            .get("enabled_tools")
            .map(|v| strings(Some(v)).map(|s| s.into_iter().collect()))
            .transpose()?;
        let disabled_tools = strings(row.get("disabled_tools"))?.into_iter().collect();
        let kind = row
            .get("type")
            .map(|v| v.as_str().ok_or("invalid MCP transport type"))
            .transpose()?;
        let endpoint = if let Some(command) = row.get("command") {
            if !matches!(kind, None | Some("stdio"))
                || [
                    "url",
                    "headers",
                    "http_headers",
                    "env_http_headers",
                    "bearer_token_env_var",
                ]
                .iter()
                .any(|k| row.contains_key(*k))
            {
                return Err("mixed or unsupported MCP transport".into());
            }
            let command = expand(command.as_str().ok_or("invalid MCP command")?, &env)?;
            if command.is_empty() {
                return Err("empty MCP command".into());
            }
            let args = strings(row.get("args"))?
                .iter()
                .map(|s| expand(s, &env))
                .collect::<Result<_, _>>()?;
            let child_env = string_map(row.get("env"))?
                .into_iter()
                .map(|(k, v)| Ok((k, expand(&v, &env)?)))
                .collect::<Result<_, String>>()?;
            let local_cwd = match row.get("cwd") {
                None => cwd.to_path_buf(),
                Some(v) => {
                    let p = PathBuf::from(expand(v.as_str().ok_or("invalid MCP cwd")?, &env)?);
                    if p.is_absolute() {
                        p
                    } else {
                        cwd.join(p)
                    }
                }
            };
            Endpoint::Stdio {
                command,
                args,
                env: child_env,
                cwd: local_cwd,
            }
        } else {
            if !matches!(kind, None | Some("http") | Some("streamable-http"))
                || ["args", "env", "cwd"].iter().any(|k| row.contains_key(*k))
            {
                return Err("unsupported MCP HTTP transport".into());
            }
            let url = expand(
                row.get("url")
                    .and_then(Value::as_str)
                    .ok_or("MCP URL missing")?,
                &env,
            )?;
            let parsed = url::Url::parse(&url).map_err(|_| "invalid MCP URL")?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err("unsupported MCP URL authentication".into());
            }
            if row.contains_key("headers") && row.contains_key("http_headers") {
                return Err("duplicate MCP header configuration".into());
            }
            let mut headers = string_map(row.get("headers").or_else(|| row.get("http_headers")))?
                .into_iter()
                .map(|(k, v)| Ok((k.to_ascii_lowercase(), expand(&v, &env)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            for (name, key) in string_map(row.get("env_http_headers"))? {
                if headers
                    .insert(
                        name.to_ascii_lowercase(),
                        env(&key).ok_or("local MCP authentication required")?,
                    )
                    .is_some()
                {
                    return Err("duplicate MCP header".into());
                }
            }
            if let Some(key) = row.get("bearer_token_env_var") {
                let token = env(key.as_str().ok_or("invalid MCP bearer variable")?)
                    .ok_or("local MCP authentication required")?;
                if headers
                    .insert("authorization".into(), format!("Bearer {token}"))
                    .is_some()
                {
                    return Err("duplicate MCP authorization".into());
                }
            }
            for (name, value) in &headers {
                if [
                    "host",
                    "content-length",
                    "content-type",
                    "accept",
                    "mcp-session-id",
                    "mcp-protocol-version",
                    "last-event-id",
                ]
                .contains(&name.as_str())
                {
                    return Err("reserved MCP transport header".into());
                }
                reqwest_mcp::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "invalid MCP header")?;
                reqwest_mcp::header::HeaderValue::from_str(value)
                    .map_err(|_| "invalid MCP header value")?;
            }
            Endpoint::Http { url, headers }
        };
        #[cfg(not(unix))]
        if matches!(endpoint, Endpoint::Stdio { .. }) {
            return Err("local MCP stdio relay unsupported on this platform".into());
        }
        if grant.contains_key(id) {
            return Err("duplicate generated MCP server identity".into());
        }
        grant.insert(
            id.clone(),
            Server {
                endpoint,
                enabled_tools: tools,
                disabled_tools,
            },
        );
    }
    let (remote, desktop): (Vec<_>, Vec<_>) = grant
        .iter()
        .partition(|(_, server)| matches!(server.endpoint, Endpoint::Remote(_)));
    let entries: Vec<_> = remote
        .into_iter()
        .filter_map(|(_, server)| match &server.endpoint {
            Endpoint::Remote(entry) => Some(entry.clone()),
            _ => None,
        })
        .collect();
    let ids: Vec<_> = desktop.into_iter().map(|(id, _)| id.clone()).collect();
    codesense_host_proto::reverse_mcp::validate_remote_entries(&ids, &entries)?;
    Ok(grant)
}

fn parse_remote(
    id: &str,
    row: &serde_json::Map<String, Value>,
) -> Result<Option<codesense_host_proto::reverse_mcp::RemoteStdioEntry>, String> {
    use codesense_host_proto::reverse_mcp::{remote_server_id, RemoteStdioEntry};
    const FIELDS: &[&str] = &[
        "executionLocation",
        "type",
        "command",
        "args",
        "cwd",
        "env",
        "envRefs",
        "enabled",
    ];
    if row.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Err("unsupported remote MCP field or tool filter".into());
    }
    match row.get("enabled") {
        Some(Value::Bool(false)) => return Ok(None),
        None | Some(Value::Bool(true)) => {}
        _ => return Err("invalid remote MCP enabled policy".into()),
    }
    if row
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("stdio"))
    {
        return Err("remote workspace MCP supports stdio only".into());
    }
    let entry = RemoteStdioEntry {
        id: remote_server_id(id)?,
        logical_name: id.into(),
        command: row
            .get("command")
            .and_then(Value::as_str)
            .ok_or("remote MCP command missing")?
            .into(),
        args: strings(row.get("args"))?,
        cwd: row
            .get("cwd")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("invalid remote MCP cwd")
            })
            .transpose()?,
        env: string_map(row.get("env"))?,
        env_refs: string_map(row.get("envRefs"))?,
    };
    entry.validate()?;
    Ok(Some(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_entries_never_expand_desktop_values_and_reject_policy_loss() {
        let mut root = serde_json::json!({"mcpServers": {
            "desktop": {"command":"python3", "env":{"TOKEN":"${DESKTOP_TOKEN}"}},
            "browser": {"executionLocation":"remoteWorkspace", "command":"/opt/node", "args":["/opt/mcp/cli.js"], "env":{"MODE":"test"}, "envRefs":{"TOKEN":"REMOTE_TOKEN"}}
        }});
        let parse_profile = |root: &Value| {
            parse(
                root,
                "mcpServers",
                "codesense",
                None,
                std::path::Path::new("/desktop"),
                |key| {
                    assert_eq!(key, "DESKTOP_TOKEN");
                    Some("desktop-secret-sentinel".into())
                },
            )
        };
        let grant = parse_profile(&root).unwrap();
        let id = codesense_host_proto::reverse_mcp::remote_server_id("browser").unwrap();
        let Endpoint::Remote(entry) = &grant[&id].endpoint else {
            panic!("remote expected")
        };
        let encoded = serde_json::to_string(entry).unwrap();
        assert!(!encoded.contains("desktop-secret-sentinel"));
        assert!(!encoded.contains("/desktop"));
        assert_eq!(entry.env_refs["TOKEN"], "REMOTE_TOKEN");
        root["mcpServers"]["browser"]["command"] = serde_json::json!("/different/node");
        for (field, value) in [
            ("enabled_tools", serde_json::json!(["safe"])),
            ("disabled_tools", serde_json::json!(["unsafe"])),
            ("url", serde_json::json!("http://secret.invalid")),
            ("env", serde_json::json!({"TOKEN":"${DESKTOP_TOKEN}"})),
        ] {
            let mut unsupported = root.clone();
            unsupported["mcpServers"]["browser"][field] = value;
            let error = parse_profile(&unsupported).err().unwrap();
            assert!(!error.contains("DESKTOP_TOKEN") && !error.contains("secret.invalid"));
        }
        root["mcpServers"][&id] = serde_json::json!({"command":"python3"});
        assert!(parse_profile(&root).is_err());
    }

    #[test]
    fn local_mcp_config_preserves_scope_and_refuses_dropped_policy() {
        let root = serde_json::json!({"mcpServers":{"codesense":{"command":"never"},"private":{"command":"python3","args":["${ARG}"],"env":{"SECRET":"${TOKEN}"},"enabled_tools":["safe"]},"off":{"command":"no","enabled":false}}});
        let env = |key: &str| match key {
            "ARG" => Some("local".into()),
            "TOKEN" => Some("desktop-only".into()),
            _ => None,
        };
        let all = parse(
            &root,
            "mcpServers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            env,
        )
        .unwrap();
        assert_eq!(all.len(), 1);
        assert!(all["private"]
            .enabled_tools
            .as_ref()
            .unwrap()
            .contains("safe"));
        assert!(parse(
            &root,
            "mcpServers",
            "codesense",
            Some(&[]),
            std::path::Path::new("/tmp"),
            env
        )
        .unwrap()
        .is_empty());
        let changed = serde_json::json!({"mcpServers":{"private":{"command":"python3","alwaysAllow":["danger"]}}});
        assert!(parse(
            &changed,
            "mcpServers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            env
        )
        .is_err());
    }
    #[test]
    fn local_mcp_http_auth_is_desktop_resolved_or_explicitly_refused() {
        let root = serde_json::json!({"mcp_servers":{"server":{"url":"http://127.0.0.1:1/mcp","bearer_token_env_var":"TOKEN"}}});
        assert!(parse(
            &root,
            "mcp_servers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            |_| None
        )
        .is_err());
        let grant = parse(
            &root,
            "mcp_servers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            |_| Some("sentinel".into()),
        )
        .unwrap();
        let Endpoint::Http { headers, .. } = &grant["server"].endpoint else {
            panic!("HTTP expected")
        };
        assert_eq!(headers["authorization"], "Bearer sentinel");
        let oauth = serde_json::json!({"mcp_servers":{"server":{"url":"https://example.test","oauth":true}}});
        assert!(parse(
            &oauth,
            "mcp_servers",
            "codesense",
            None,
            std::path::Path::new("/tmp"),
            |_| None
        )
        .is_err());
    }
}
