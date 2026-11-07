// SPDX-License-Identifier: MIT
//! Reverse streams use their own kind+channel namespace on the admitted transport.
//!
//! This module is also the one codec for the daemon's local MCP broker socket:
//! the mode byte a peer opens with, the newline-terminated audience challenge,
//! newline-delimited MCP messages, and the bridge configuration file.
use crate::frame::{Frame, Kind};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// How a host says a native launch's CLI is not on the account's login PATH,
/// for a terminal server that answers it only when the launch starts. The one
/// wording both sides share: the host writes it, the desktop recognizes it.
pub const NATIVE_RUNTIME_MISSING: &str =
    "is unavailable on this machine; install it in the remote account's login PATH";

/// Bounded, nonsecret saved-Agent launch inputs. Remote account credentials and
/// auth-home selection remain entirely owned by the remote runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentStartup {
    pub args: Vec<String>,
    pub settings: serde_json::Value,
    pub prompt: Option<String>,
    /// The saved Agent's own environment variables, set on the CLI under every
    /// variable CodeSense sets for the launch.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

/// The most environment variables a saved Agent sets.
pub const MAX_AGENT_ENV_VARS: usize = 64;
/// The longest name of a saved Agent's environment variable, in bytes.
pub const MAX_AGENT_ENV_NAME: usize = 128;
/// The longest value of a saved Agent's environment variable, in bytes.
pub const MAX_AGENT_ENV_VALUE: usize = 4096;

/// A portable environment variable name: `[A-Za-z_][A-Za-z0-9_]*`, at most
/// [`MAX_AGENT_ENV_NAME`] bytes.
pub fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_AGENT_ENV_NAME
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

/// An environment variable value a launch can carry: at most
/// [`MAX_AGENT_ENV_VALUE`] bytes, with no NUL.
pub fn is_env_value(value: &str) -> bool {
    value.len() <= MAX_AGENT_ENV_VALUE && !value.contains('\0')
}
/// What one CLI's saved-Agent startup may carry beyond the bounds every startup keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupRules {
    /// The CLI's name in a refusal.
    pub name: &'static str,
    /// Flags refused besides [`RESERVED_STARTUP_FLAGS`]: switches this CLI lacks, so a
    /// startup naming them is not one it can run.
    pub refused: &'static [&'static str],
    /// The only top-level settings keys a startup may carry.
    pub settings_keys: &'static [&'static str],
    /// Whether every setting must be a string.
    pub string_settings: bool,
}

impl StartupRules {
    /// Whether `settings` holds only keys these rules allow, each a string where they
    /// must be.
    pub fn admits(&self, settings: &serde_json::Map<String, serde_json::Value>) -> bool {
        settings.iter().all(|(key, value)| {
            self.settings_keys.contains(&key.as_str())
                && (!self.string_settings || value.is_string())
        })
    }
}

/// Switches that can escape the verified workspace or replace the mandatory bridge or
/// settings, refused for every CLI rather than silently weakening a saved definition.
pub const RESERVED_STARTUP_FLAGS: &[&str] = &[
    "--mcp-config",
    "--settings",
    "--add-dir",
    "--cd",
    "-C",
    "--config",
    "-c",
    "--resume",
    "-r",
    "--continue",
    "--session-id",
    "--fork-session",
];

impl AgentStartup {
    /// Whether this startup is bounded and leaves CodeSense's own scope in place, for a
    /// CLI that runs by `rules`.
    pub fn validate(&self, rules: &StartupRules) -> Result<(), String> {
        let invalid = || "invalid or unbounded saved-Agent launch inputs".to_string();
        if self.args.len() > 128
            || self
                .args
                .iter()
                .any(|arg| arg.len() > 4096 || arg.contains('\0'))
            || self
                .prompt
                .as_ref()
                .is_some_and(|p| p.len() > 64 * 1024 || p.contains('\0'))
            || !self.settings.is_object()
            || self.env.len() > MAX_AGENT_ENV_VARS
            || self
                .env
                .iter()
                .any(|(name, value)| !is_env_name(name) || !is_env_value(value))
            || serde_json::to_vec(&self.settings)
                .map_err(|e| e.to_string())?
                .len()
                > 64 * 1024
        {
            return Err(invalid());
        }
        let reserved = RESERVED_STARTUP_FLAGS.iter().chain(rules.refused);
        if let Some(arg) = self.args.iter().find(|arg| {
            arg.starts_with("-C")
                || (arg.starts_with("-c") && !arg.starts_with("--"))
                || reserved.clone().any(|key| {
                    arg.strip_prefix(key)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
                })
        }) {
            // Name the flag, not the whole value: an inline `--settings` JSON can be long.
            let flag = arg.split('=').next().unwrap_or(arg.as_str());
            return Err(format!(
                "saved Agent argument {flag:?} would override required workspace, settings or MCP scope"
            ));
        }
        let settings = self.settings.as_object().ok_or_else(invalid)?;
        if !rules.admits(settings) {
            return Err(format!("unsupported {} saved-Agent settings", rules.name));
        }
        Ok(())
    }
}

/// Trusted nonsecret remote configuration. Environment references are resolved only
/// by the remote session owner; Debug intentionally omits all command values.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteStdioEntry {
    pub id: String,
    pub logical_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub env_refs: std::collections::BTreeMap<String, String>,
}
impl std::fmt::Debug for RemoteStdioEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteStdioEntry { configuration: [redacted] }")
    }
}
/// Injective encoding, with a bounded logical name, prevents normalized-name collisions.
pub fn remote_server_id(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 56 || name.chars().any(char::is_control) {
        return Err("invalid remote MCP logical name".into());
    }
    let mut id = String::from("remote-");
    use std::fmt::Write;
    for byte in name.bytes() {
        write!(id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(id)
}
impl RemoteStdioEntry {
    pub fn validate(&self) -> Result<(), String> {
        fn text(value: &str) -> bool {
            value.len() <= 4096 && !value.contains('\0') && !value.contains("${")
        }
        fn variable(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
        }
        if self.id != remote_server_id(&self.logical_name)?
            || self.command.is_empty()
            || !text(&self.command)
            || self.args.len() > 128
            || self.args.iter().any(|arg| !text(arg))
            || self
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd.is_empty() || !text(cwd))
            || self.env.len() + self.env_refs.len() > 64
            || self
                .env
                .iter()
                .any(|(key, value)| !variable(key) || !text(value))
            || self.env_refs.iter().any(|(key, value)| {
                !variable(key) || !variable(value) || self.env.contains_key(key)
            })
        {
            return Err("invalid or unbounded remote MCP configuration".into());
        }
        let executable = self.command.rsplit('/').next().unwrap_or("");
        if matches!(
            executable,
            "sudo"
                | "su"
                | "doas"
                | "pkexec"
                | "runuser"
                | "setpriv"
                | "npx"
                | "npm"
                | "pnpm"
                | "yarn"
                | "bunx"
                | "uvx"
                | "docker"
                | "podman"
                | "ssh"
        ) || self.args.iter().any(|arg| {
            ["--cdp-endpoint", "--browser-endpoint", "--extension"]
                .iter()
                .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
        }) {
            return Err("remote MCP requires an installed same-account stdio program without external browser attachment".into());
        }
        Ok(())
    }
}
/// Reject collisions across both locations before any launch artifacts are produced.
pub fn validate_remote_entries(
    desktop_ids: &[String],
    entries: &[RemoteStdioEntry],
) -> Result<(), String> {
    let mut ids: std::collections::BTreeSet<String> = selected_servers(desktop_ids.to_vec())?
        .into_iter()
        .collect();
    for entry in entries {
        entry.validate()?;
        if !ids.insert(entry.id.clone()) {
            return Err("duplicate or reserved remote MCP server identity".into());
        }
    }
    if ids.len() > MAX_SERVERS {
        return Err("MCP server selection exceeds limit".into());
    }
    Ok(())
}

pub const CHUNK_BYTES: usize = 16 * 1024;
pub const LINE_BYTES: usize = 1024 * 1024;
pub const QUEUE_CHUNKS: usize = 4;
pub const MAX_SERVERS: usize = 16;
pub const MAX_SERVER_ID_BYTES: usize = 128;
pub const MAX_STREAMS: usize = 32;
pub const MAX_SESSION_STREAMS: usize = 32;
pub const CODESENSE_SERVER: &str = "codesense";
pub const SOCKET_ENV: &str = "CODESENSE_MCP_SOCK";
pub const BROKER_ENV: &str = "CODESENSE_MCP_BROKER_FD";
pub const AUDIENCE: &str = "codesense-mcp";

/// The JSON-RPC error code a desktop-relayed tool answers while no desktop is connected
/// (the machine answers it itself).
pub const DESKTOP_OFFLINE_CODE: i64 = -32001;
/// That error's `data.type`.
pub const DESKTOP_OFFLINE: &str = "desktop_offline";

/// The first byte on the broker socket: a bootstrapping shell collecting its
/// session capability.
pub const MODE_BOOTSTRAP: u8 = b'B';
/// A bridge reacquiring its session capability by process lineage.
pub const MODE_REACQUIRE: u8 = b'R';
/// A bridge presenting its capability to open an MCP stream.
pub const MODE_MCP: u8 = b'M';
/// A bridge selecting a server after completing the same capability challenge.
pub const MODE_MCP_SERVER: u8 = b'S';
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Open {
    pub session: String,
    pub binding_nonce: String,
    #[serde(default = "default_server", skip_serializing_if = "is_default_server")]
    pub server_id: String,
}

pub fn default_server() -> String {
    CODESENSE_SERVER.into()
}
pub fn is_default_server(id: &str) -> bool {
    id == CODESENSE_SERVER
}
pub fn is_default_servers(ids: &[String]) -> bool {
    ids.len() == 1 && is_default_server(&ids[0])
}
pub fn default_servers() -> Vec<String> {
    vec![default_server()]
}
pub fn validate_server_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_SERVER_ID_BYTES || id.chars().any(char::is_control) {
        return Err("invalid MCP server selector".into());
    }
    Ok(())
}
/// Normalize only selection, never desktop command/configuration or credentials.
pub fn selected_servers(ids: Vec<String>) -> Result<Vec<String>, String> {
    if ids.len() > MAX_SERVERS {
        return Err("MCP server selection exceeds limit".into());
    }
    let mut selected = std::collections::BTreeSet::new();
    selected.insert(default_server());
    for id in ids {
        validate_server_id(&id)?;
        selected.insert(id);
    }
    if selected.len() > MAX_SERVERS {
        return Err("MCP server selection exceeds limit".into());
    }
    Ok(selected.into_iter().collect())
}

/// Remote projection contains only installed bridge commands and opaque names.
pub fn bridge_configuration(command: &str, socket: &str) -> serde_json::Value {
    server_configuration(command, socket, default_servers()).expect("fixed server is valid")
}
pub fn server_configuration(
    command: &str,
    socket: &str,
    ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    let mut servers = serde_json::Map::new();
    for id in selected_servers(ids)? {
        let args = if is_default_server(&id) {
            vec!["--remote-socket", socket]
        } else {
            vec!["--remote-socket", socket, "--server-id", &id]
        };
        servers.insert(
            id.clone(),
            serde_json::json!({ "command": command, "args": args }),
        );
    }
    Ok(serde_json::json!({"mcpServers": servers}))
}
pub fn write_bridge_configuration(
    output: &mut impl Write,
    command: &str,
    socket: &str,
) -> Result<(), String> {
    write_server_configuration(output, command, socket, default_servers())
}
pub fn write_server_configuration(
    output: &mut impl Write,
    command: &str,
    socket: &str,
    ids: Vec<String>,
) -> Result<(), String> {
    serde_json::to_writer(output, &server_configuration(command, socket, ids)?)
        .map_err(|e| e.to_string())
}
pub fn write_server_selector(output: &mut impl Write, id: &str) -> Result<(), String> {
    validate_server_id(id)?;
    serde_json::to_writer(&mut *output, id).map_err(|e| e.to_string())?;
    output.write_all(b"\n").map_err(|e| e.to_string())
}
pub fn read_server_selector<R: Read>(
    stream: &mut R,
    before: impl FnMut(&mut R) -> Result<(), String>,
) -> Result<String, String> {
    let mut before = before;
    let line = read_message(stream, MAX_SERVER_ID_BYTES * 6 + 3, |r| before(r))?;
    let id: String = serde_json::from_str(&line).map_err(|_| "invalid MCP server selector")?;
    validate_server_id(&id)?;
    Ok(id)
}

/// Sends the broker mode byte a peer opens with.
pub fn write_mode(stream: &mut impl Write, mode: u8) -> std::io::Result<()> {
    stream.write_all(&[mode])
}

/// Sends the one-use challenge line `"<AUDIENCE> <challenge>\n"`.
pub fn write_challenge(stream: &mut impl Write, challenge: &str) -> std::io::Result<()> {
    stream.write_all(format!("{AUDIENCE} {challenge}\n").as_bytes())
}

/// Delivers one `MCP_DATA` payload verbatim to the local MCP stream.
pub fn write_payload(stream: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(payload)
}

/// The `MCP_OPEN` frame announcing `open` on `channel`.
pub fn open_frame(channel: u32, open: &Open) -> Result<Frame, String> {
    Ok(Frame {
        channel,
        ..Frame::json(Kind::MCP_OPEN, open)?
    })
}

/// Reads one newline-terminated message of at most `limit` bytes (newline
/// included), byte by byte so nothing past the newline is consumed. `before`
/// runs ahead of every byte, e.g. to re-arm a deadline on `stream`.
pub fn read_message<R: Read>(
    stream: &mut R,
    limit: usize,
    mut before: impl FnMut(&mut R) -> Result<(), String>,
) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        before(stream)?;
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
        if bytes.len() >= limit {
            return Err("MCP line exceeds limit".into());
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes).map_err(|_| "MCP line is not UTF8".into());
        }
    }
}

#[cfg(test)]
mod tests {
    fn remote_entry() -> super::RemoteStdioEntry {
        super::RemoteStdioEntry {
            id: super::remote_server_id("browser").unwrap(),
            logical_name: "browser".into(),
            command: "/opt/node".into(),
            args: vec!["/opt/mcp/cli.js".into()],
            cwd: None,
            env: Default::default(),
            env_refs: Default::default(),
        }
    }
    #[test]
    fn remote_projection_is_bounded_redacted_and_collision_checked() {
        let mut entry = remote_entry();
        assert!(
            super::validate_remote_entries(&super::default_servers(), &[entry.clone()]).is_ok()
        );
        assert!(super::validate_remote_entries(&[entry.id.clone()], &[entry.clone()]).is_err());
        assert!(super::validate_remote_entries(&[], &[entry.clone(), entry.clone()]).is_err());
        entry.env_refs.insert("TOKEN".into(), "REMOTE_TOKEN".into());
        assert!(entry.validate().is_ok());
        entry.env.insert("TOKEN".into(), "literal".into());
        assert!(entry.validate().is_err());
        entry = remote_entry();
        entry.args = vec!["x".into(); 129];
        assert!(entry.validate().is_err());
        entry = remote_entry();
        entry.command = "npx".into();
        assert!(entry.validate().is_err());
        entry.command = "sudo".into();
        assert!(entry.validate().is_err());
        entry = remote_entry();
        entry.args.push("--cdp-endpoint=http://localhost".into());
        assert!(entry.validate().is_err());
        assert!(!format!("{entry:?}").contains("localhost"));
        let mut value = serde_json::to_value(remote_entry()).unwrap();
        value["headers"] = serde_json::json!({"secret":"x"});
        assert!(serde_json::from_value::<super::RemoteStdioEntry>(value).is_err());
        assert_ne!(
            super::remote_server_id("a-b").unwrap(),
            super::remote_server_id("a_b").unwrap()
        );
    }

    #[test]
    fn message_reads_stop_at_the_newline_and_enforce_the_limit() {
        let mut input: &[u8] = b"{\"id\":1}\nrest";
        let line = super::read_message(&mut input, 64, |_| Ok(())).unwrap();
        assert_eq!(line, "{\"id\":1}\n");
        assert_eq!(input, b"rest");
        let mut long: &[u8] = b"abcdef\n";
        assert!(super::read_message(&mut long, 3, |_| Ok(())).is_err());
    }

    #[test]
    fn open_frame_carries_its_channel() {
        let open = super::Open {
            session: "s".into(),
            binding_nonce: "n".into(),
            server_id: super::default_server(),
        };
        let frame = super::open_frame(7, &open).unwrap();
        assert_eq!(frame.kind, crate::Kind::MCP_OPEN);
        assert_eq!(frame.channel, 7);
        let decoded: super::Open = frame.decode_json().unwrap();
        assert_eq!(decoded.session, "s");
    }

    #[test]
    fn selected_projection_preserves_names_and_contains_no_desktop_config() {
        let ids = vec!["issue tracker / personal".into(), "codesense".into()];
        let value = super::server_configuration("/bridge", "/socket", ids).unwrap();
        let servers = value["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers["issue tracker / personal"]["args"][3],
            "issue tracker / personal"
        );
        assert!(servers.values().all(|v| v.as_object().unwrap().len() == 2));
        assert!(super::selected_servers(vec!["bad\nname".into()]).is_err());
        assert!(super::selected_servers((0..16).map(|n| format!("server-{n}")).collect()).is_err());
        let mut encoded = Vec::new();
        super::write_server_selector(&mut encoded, "a \"quoted\" name").unwrap();
        assert_eq!(
            super::read_server_selector(&mut encoded.as_slice(), |_| Ok(())).unwrap(),
            "a \"quoted\" name"
        );
        let old: super::Open =
            serde_json::from_str(r#"{"session":"s","bindingNonce":"n"}"#).unwrap();
        assert_eq!(old.server_id, "codesense");
        assert!(serde_json::to_value(old).unwrap().get("serverId").is_none());
    }

    #[test]
    fn reverse_mcp_projection_is_bridge_only() {
        let value = super::bridge_configuration("/owned/codesense-bridge", "/owned/mcp.sock");
        assert_eq!(value["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(
            value["mcpServers"]["codesense"]["command"],
            "/owned/codesense-bridge"
        );
        assert!(value["mcpServers"]["codesense"].get("env").is_none());
    }
}
