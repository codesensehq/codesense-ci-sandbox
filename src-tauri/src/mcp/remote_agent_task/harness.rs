// SPDX-License-Identifier: MIT
//! Private official app-server owner. No app-server RPC endpoint is exposed to callers.
use codesense_pty_core::agent_cli::{descriptor, AgentCli, Descriptor, VERSION_OUTPUT_MAX};
use serde_json::{json, Value};
use std::{
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

const MAX_MESSAGE: usize = 1024 * 1024;
pub(super) type Validate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
pub(super) async fn check(validate: &Validate) -> Result<(), String> {
    let validate = validate.clone();
    crate::commands::await_blocking("remote_task_binding", move || validate()).await
}
pub(crate) struct Environment {
    pub id: String,
    pub url: String,
    pub token: String,
    pub cwd: String,
}
impl Environment {
    /// The sole environment a thread and each of its turns select: the captured remote
    /// workspace, so no turn falls back to a default or local one.
    fn selection(&self) -> Value {
        json!([{"environmentId":self.id,"cwd":self.cwd,"runtimeWorkspaceRoots":[self.cwd]}])
    }
    /// Refuses an `environment/info` answer whose working directory is not the captured
    /// remote cwd. Codex reports it as a canonical file URI.
    fn reported_by(&self, info: &Value) -> Result<(), String> {
        let reported = info
            .get("cwd")
            .and_then(Value::as_str)
            .and_then(|uri| url::Url::parse(uri).ok())
            .filter(|uri| {
                uri.scheme() == "file" && matches!(uri.host_str(), None | Some("localhost"))
            })
            .and_then(|uri| uri.to_file_path().ok());
        if reported.as_deref() != Some(Path::new(&self.cwd)) {
            return Err("remote environment reports another workspace".into());
        }
        Ok(())
    }
    /// Refuses a started thread that does not select exactly the captured remote
    /// environment, cwd and roots.
    fn selected_by(&self, thread: &Value) -> Result<(), String> {
        if thread.pointer("/thread/environments") != Some(&self.selection()) {
            return Err("thread selects another environment than the remote workspace".into());
        }
        Ok(())
    }
}
pub(super) struct McpConnection {
    pub url: String,
    pub token: String,
    pub projection: crate::mcp::local_mcp_relay::native::Projection,
}
#[cfg(test)]
impl McpConnection {
    pub(super) fn new(url: String, token: String) -> Self {
        Self {
            url,
            token,
            projection: Default::default(),
        }
    }
}
pub(super) struct Harness {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    sequence: u64,
    #[cfg(test)]
    pub(super) synthetic_fixture: bool,
    #[cfg(test)]
    pub(super) fixture_diagnostics: bool,
    pending: std::collections::VecDeque<Value>,
    pending_bytes: usize,
    allowed_mcp: std::collections::HashSet<String>,
    validate: Validate,
    cancelled: Arc<AtomicBool>,
}
impl Harness {
    pub(super) async fn spawn(
        cwd: &Path,
        home: Option<&Path>,
        validate: Validate,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        check(&validate).await?;
        let mut command = tokio::process::Command::new(&descriptor(AgentCli::Codex).binary);
        command.env_clear();
        for name in [
            "HOME",
            "PATH",
            "TMPDIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "CODEX_HOME",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(test)]
        command.env("CODESENSE_AUTH1_LOCAL_SENTINEL", "synthetic-local-only");
        command.process_group(0);
        command
            .args(["app-server", "--listen", "stdio://"])
            .args([
                "-c",
                "shell_environment_policy.inherit=\"none\"",
                "-c",
                "mcp_servers={}",
                "-c",
                "features.multi_agent=false",
                "-c",
                "features.multi_agent_mode=false",
                "-c",
                "features.multi_agent_v2=false",
                "-c",
                "features.collab=false",
                "-c",
                "features.hooks=false",
                "-c",
                "features.codex_hooks=false",
                "-c",
                "features.plugin_hooks=false",
                "-c",
                "features.plugins=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.connectors=false",
                "-c",
                "features.computer_use=false",
                "-c",
                "features.browser_use=false",
                "-c",
                "features.js_repl=false",
                "-c",
                "features.code_mode=false",
                "-c",
                "features.shell_snapshot=false",
                "-c",
                "notify=[]",
                "-c",
                "allow_login_shell=false",
                "-c",
                "shell_environment_policy.experimental_use_profile=false",
            ])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Never change native subscription selection into inherited API billing.
        for name in [
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_BASE_URL",
        ] {
            command.env_remove(name);
        }
        if let Some(home) = home {
            command.env("CODEX_HOME", home);
        }
        check(&validate).await?;
        let child = command
            .spawn()
            .map_err(|_| "official Codex app-server unavailable")?;
        Self::over(child, validate, cancelled)
    }
    /// The owner of an app-server `child` speaking on its stdio.
    fn over(
        mut child: Child,
        validate: Validate,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let input = child.stdin.take().ok_or("app-server input unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("app-server output unavailable")?);
        Ok(Self {
            child,
            input: Some(input),
            output,
            sequence: 0,
            #[cfg(test)]
            synthetic_fixture: false,
            #[cfg(test)]
            fixture_diagnostics: false,
            pending: Default::default(),
            pending_bytes: 0,
            allowed_mcp: Default::default(),
            validate,
            cancelled,
        })
    }
    async fn write(&mut self, value: Value) -> Result<(), String> {
        check(&self.validate).await?;
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "invalid private protocol value")?;
        if bytes.len() > MAX_MESSAGE {
            return Err("private protocol message exceeds bound".into());
        }
        bytes.push(b'\n');
        tokio::time::timeout(
            Duration::from_secs(10),
            self.input
                .as_mut()
                .ok_or("app-server input closed")?
                .write_all(&bytes),
        )
        .await
        .map_err(|_| "app-server write outcome unknown")?
        .map_err(|_| "app-server write outcome unknown".to_string())
    }
    async fn next_wire(&mut self) -> Result<Value, String> {
        let mut bytes = Vec::new();
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err("task cancellation requested".into());
            }
            check(&self.validate).await?;
            let available = match tokio::time::timeout(
                Duration::from_millis(250),
                self.output.fill_buf(),
            )
            .await
            {
                Err(_) => continue,
                Ok(Err(_)) => return Err("app-server output unavailable".into()),
                Ok(Ok([])) => return Err("app-server ended before task settlement".into()),
                Ok(Ok(bytes)) => bytes,
            };
            let consumed = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |n| n + 1);
            if bytes.len() + consumed > MAX_MESSAGE {
                return Err("app-server output exceeds bound".into());
            }
            let complete = available[consumed - 1] == b'\n';
            bytes.extend_from_slice(&available[..consumed]);
            self.output.consume(consumed);
            if !complete {
                continue;
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "invalid app-server output")?;
            if value["method"] == "mcpServer/startupStatus/updated"
                && matches!(
                    value["params"]["status"].as_str(),
                    Some("starting" | "ready")
                )
                && value["params"]["name"]
                    .as_str()
                    .is_none_or(|name| !self.allowed_mcp.contains(name))
            {
                return Err("official runtime exposed an MCP server outside the task grant".into());
            }
            // Approval and host-side requests are never delegated to remote callers.
            if value.get("id").is_some() && value.get("method").is_some() {
                self.write(json!({"id":value["id"],"error":{"code":-32601,"message":"unsupported task request"}})).await?;
                bytes.clear();
                continue;
            }
            return Ok(value);
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.sequence += 1;
        let id = self.sequence;
        self.write(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let value = self.next_wire().await?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if value.get("error").is_some() {
                        return Err(format!("official runtime refused {method}"));
                    }
                    return value
                        .get("result")
                        .cloned()
                        .ok_or_else(|| "missing app-server result".into());
                }
                let size = value.to_string().len();
                if self.pending.len() >= 256 || self.pending_bytes + size > MAX_MESSAGE {
                    return Err("app-server notification queue exceeds bound".into());
                }
                self.pending_bytes += size;
                self.pending.push_back(value);
            }
        })
        .await
        .map_err(|_| "app-server request outcome unknown")?
    }
    pub(super) async fn run(
        &mut self,
        environment: Environment,
        mcp: &McpConnection,
        prompt: &str,
        mut event: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        self.call("initialize",json!({"clientInfo":{"name":"codesense_remote_task","version":"1"},"capabilities":{"experimentalApi":true}})).await?;
        self.write(json!({"method":"initialized"})).await?;
        self.call("environment/add",json!({"environmentId":environment.id,"execServerUrl":environment.url,"authBearerToken":environment.token,"connectTimeoutMs":10000})).await?;
        let info = self
            .call("environment/info", json!({"environmentId":environment.id}))
            .await?;
        environment.reported_by(&info)?;
        // Empty TOML maps merge with native config. Disable every effective server by ID.
        let effective = self
            .call("config/read", json!({"includeLayers":false}))
            .await?;
        if effective
            .pointer("/config/shell_environment_policy/set")
            .and_then(Value::as_object)
            .is_some_and(|v| !v.is_empty())
        {
            return Err("local environment overrides are unavailable for remote tasks".into());
        }
        #[cfg(test)]
        let synthetic = self.synthetic_fixture;
        #[cfg(not(test))]
        let synthetic = false;
        if !synthetic {
            let account = self
                .call("account/read", json!({"refreshToken":false}))
                .await?;
            if account.pointer("/account/type").and_then(Value::as_str) != Some("chatgpt")
                || effective
                    .pointer("/config/model_provider")
                    .and_then(Value::as_str)
                    .is_some_and(|p| p != "openai")
            {
                return Err("capabilityUnavailable: remote tasks require native ChatGPT account authentication".into());
            }
        }
        if effective
            .pointer("/config/mcp_servers")
            .and_then(Value::as_object)
            .is_some_and(|native| {
                native.contains_key("codesense_remote_task")
                    || mcp
                        .projection
                        .servers
                        .keys()
                        .any(|id| native.contains_key(id))
            })
        {
            return Err(
                "capabilityUnavailable: selected MCP server collides with native configuration"
                    .into(),
            );
        }
        let mut servers = serde_json::Map::new();
        if let Some(configured) = effective
            .pointer("/config/mcp_servers")
            .and_then(Value::as_object)
        {
            for name in configured.keys() {
                servers.insert(name.clone(), json!({"enabled":false}));
            }
        }
        // The official runtime can synthesize this authenticated built-in after config/read.
        // An explicit disabled registration vetoes that later compatibility overlay.
        servers.insert(
            "codex_apps".into(),
            json!({"url":"http://127.0.0.1:9","enabled":false}),
        );
        servers.extend(mcp.projection.servers.clone());
        servers.insert("codesense_remote_task".into(),json!({"url":mcp.url,"environment_id":"local","required":true,"default_tools_approval_mode":"approve","http_headers":{"Authorization":format!("Bearer {}",mcp.token)},"enabled":true}));
        self.allowed_mcp = mcp
            .projection
            .servers
            .keys()
            .cloned()
            .chain(std::iter::once("codesense_remote_task".into()))
            .collect();
        let thread=self.call("thread/start",json!({"environments":environment.selection(),"approvalPolicy":"never","sandbox":"workspace-write","ephemeral":true,"config":{"mcp_servers":servers}})).await?;
        environment.selected_by(&thread)?;
        #[cfg(test)]
        if self.fixture_diagnostics {
            eprintln!(
                "OWNED FIXTURE THREAD CWD {}",
                thread.pointer("/thread/cwd").unwrap_or(&Value::Null)
            );
        }
        let id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or("missing owned thread identity")?;
        self.call(
            "turn/start",
            json!({"threadId":id,"input":[{"type":"text","text":prompt}],"environments":environment.selection()}),
        )
        .await?;
        let mut streamed = std::collections::HashMap::<String, String>::new();
        loop {
            let value = match self.pending.pop_front() {
                Some(value) => {
                    self.pending_bytes = self.pending_bytes.saturating_sub(value.to_string().len());
                    value
                }
                None => self.next_wire().await?,
            };
            #[cfg(test)]
            if self.fixture_diagnostics && value["method"] == "item/completed" {
                let item = &value["params"]["item"];
                if matches!(
                    item["type"].as_str(),
                    Some("commandExecution" | "mcpToolCall")
                ) {
                    eprintln!(
                        "OWNED FIXTURE TOOL type={} status={} exit={} output={}",
                        item["type"],
                        item["status"],
                        item["exitCode"],
                        item["aggregatedOutput"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .take(1200)
                            .collect::<String>()
                    );
                }
            }
            #[cfg(test)]
            if self.fixture_diagnostics {
                if value["method"] == "mcpServer/startupStatus/updated" {
                    let p = &value["params"];
                    eprintln!(
                        "OWNED FIXTURE MCP START name={} status={} error={}",
                        p["name"],
                        p["status"],
                        p["error"]
                            .to_string()
                            .chars()
                            .take(1200)
                            .collect::<String>()
                    );
                }
                if value["method"] == "item/completed"
                    && value["params"]["item"]["type"] == "mcpToolCall"
                {
                    eprintln!(
                        "OWNED FIXTURE MCP ERROR {}",
                        value["params"]["item"]["error"]
                            .to_string()
                            .chars()
                            .take(1200)
                            .collect::<String>()
                    );
                }
            }
            match value.get("method").and_then(Value::as_str) {
                Some("item/agentMessage/delta") => {
                    if let Some(text) = value.pointer("/params/delta").and_then(Value::as_str) {
                        if let Some(id) = value.pointer("/params/itemId").and_then(Value::as_str) {
                            streamed.entry(id.to_string()).or_default().push_str(text);
                        }
                        check(&self.validate).await?;
                        event(text)?;
                    }
                }
                Some("item/completed")
                    if value.pointer("/params/item/type").and_then(Value::as_str)
                        == Some("agentMessage") =>
                {
                    if let (Some(id), Some(text)) = (
                        value.pointer("/params/item/id").and_then(Value::as_str),
                        value.pointer("/params/item/text").and_then(Value::as_str),
                    ) {
                        let prefix = streamed.remove(id).unwrap_or_default();
                        let suffix = text
                            .strip_prefix(&prefix)
                            .ok_or("runtime final message changed after streaming")?;
                        if !suffix.is_empty() {
                            check(&self.validate).await?;
                            event(suffix)?;
                        }
                    }
                }
                Some("turn/completed") => {
                    return match value.pointer("/params/turn/status").and_then(Value::as_str) {
                        Some("completed") => Ok(()),
                        _ => Err("official task did not complete".into()),
                    };
                }
                _ => {}
            }
        }
    }
    pub(super) async fn stop(&mut self) -> bool {
        // EOF lets the official owner retire its separately grouped MCP children.
        drop(self.input.take());
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_ok_and(|v| v.is_ok_and(|status| status.success()))
        {
            return true;
        }
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        // Forced parent termination cannot confirm native MCP child cleanup.
        false
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

/// Refuses a local official CLI outside its qualified release, naming the release it
/// needs. Reads at most one byte past [`VERSION_OUTPUT_MAX`] and then closes the pipe,
/// so an over-long answer cannot keep the probe waiting.
pub(super) async fn verify_version(
    mut cli: tokio::process::Command,
    agent: AgentCli,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    let Descriptor { name, release, .. } = descriptor(agent);
    let unavailable = || {
        format!(
            "{}; official {name} unavailable",
            release.remote_task_refusal()
        )
    };
    let mut child = cli
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| unavailable())?;
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let probe = async {
        let mut bytes = Vec::new();
        // `take` consumes the pipe, so it closes before `wait`.
        stdout
            .take(VERSION_OUTPUT_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, std::io::Error>((child.wait().await?, bytes))
    };
    let (status, bytes) = tokio::time::timeout(Duration::from_secs(5), probe)
        .await
        .map_err(|_| {
            format!(
                "{}; {name} version check timed out",
                release.remote_task_refusal()
            )
        })?
        .map_err(|_| unavailable())?;
    if !status.success() || !release.output_qualified(&bytes) {
        return Err(release.remote_task_refusal());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{verify_version, Environment, VERSION_OUTPUT_MAX};
    use codesense_pty_core::agent_cli::{descriptor, AgentCli};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn remote() -> Environment {
        Environment {
            id: "codesense-task".into(),
            url: "ws://127.0.0.1:1".into(),
            token: "token".into(),
            cwd: "/home/dev/auth 1/work".into(),
        }
    }

    // The answers Codex 0.159.3 and 0.162.1 gave for a 0.163.0 exec-server on the droplet.
    #[test]
    fn environment_info_must_report_the_captured_cwd() {
        let remote = remote();
        let info =
            |cwd: serde_json::Value| json!({"shell":{"name":"bash","path":"/bin/bash"},"cwd":cwd});
        remote
            .reported_by(&info(json!("file:///home/dev/auth%201/work")))
            .unwrap();
        for wrong in [
            json!("file:///home/dev/auth%201"),
            json!("file:///home/dev/auth%201/work/sub"),
            json!("file://other/home/dev/auth%201/work"),
            json!("https:///home/dev/auth%201/work"),
            json!("/home/dev/auth 1/work"),
            json!(null),
        ] {
            assert!(remote.reported_by(&info(wrong.clone())).is_err(), "{wrong}");
        }
        assert!(remote.reported_by(&json!({})).is_err());
    }

    /// Runs a task against a stand-in app-server answering each method from `answers`,
    /// and returns the outcome with the `turn/start` params it was sent.
    async fn run_against(
        answers: serde_json::Value,
    ) -> (Result<(), String>, Option<serde_json::Value>) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("app-server.py");
        let turn = dir.path().join("turn.json");
        std::fs::write(
            &script,
            r#"import json, sys
answers, log = json.loads(sys.argv[1]), sys.argv[2]
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message["method"]
    if method == "turn/start":
        open(log, "w").write(json.dumps(message["params"]))
    print(json.dumps({"id": message["id"], "result": answers.get(method, {})}), flush=True)
    if method == "turn/start":
        print(json.dumps({"method": "turn/completed", "params": {"turn": {"status": "completed"}}}), flush=True)
"#,
        )
        .unwrap();
        let child = tokio::process::Command::new("python3")
            .arg("-I")
            .arg(&script)
            .arg(answers.to_string())
            .arg(&turn)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut harness =
            super::Harness::over(child, std::sync::Arc::new(|| Ok(())), Default::default())
                .unwrap();
        harness.synthetic_fixture = true;
        let mcp = super::McpConnection::new("http://127.0.0.1:9/mcp".into(), "token".into());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            harness.run(remote(), &mcp, "prompt", |_| Ok(())),
        )
        .await
        .expect("stand-in app-server answered");
        assert!(harness.stop().await);
        let sent = std::fs::read(&turn)
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap());
        (result, sent)
    }

    #[tokio::test]
    async fn a_turn_runs_only_in_the_captured_remote_environment() {
        let exact = json!([{"environmentId":"codesense-task","cwd":"/home/dev/auth 1/work","runtimeWorkspaceRoots":["/home/dev/auth 1/work"]}]);
        let answers = |cwd: &str, environments: &serde_json::Value| {
            json!({
                "environment/info": {"shell":{"name":"bash","path":"/bin/bash"},"cwd":cwd},
                "config/read": {"config":{}},
                "thread/start": {"thread":{"id":"thread","environments":environments}},
                "turn/start": {"turn":{"id":"turn","status":"inProgress"}},
            })
        };
        let (result, sent) = run_against(answers("file:///home/dev/auth%201/work", &exact)).await;
        result.unwrap();
        let sent = sent.expect("turn started");
        assert_eq!(sent["environments"], exact);

        let (result, sent) = run_against(answers("file:///home/dev", &exact)).await;
        assert!(result.unwrap_err().contains("another workspace"));
        assert!(sent.is_none());

        let local = json!([{"environmentId":"local","cwd":"/","runtimeWorkspaceRoots":["/"]}]);
        let (result, sent) = run_against(answers("file:///home/dev/auth%201/work", &local)).await;
        assert!(result.unwrap_err().contains("another environment"));
        assert!(sent.is_none());
    }

    #[test]
    fn thread_must_select_only_the_captured_environment() {
        let remote = remote();
        let selected = |environments: serde_json::Value| {
            remote.selected_by(&json!({"thread":{"id":"t","environments":environments}}))
        };
        let exact = remote.selection();
        selected(exact.clone()).unwrap();
        let mut extra = exact.clone();
        extra
            .as_array_mut()
            .unwrap()
            .push(json!({"environmentId":"local","cwd":"/","runtimeWorkspaceRoots":["/"]}));
        let mut roots = exact.clone();
        roots[0]["runtimeWorkspaceRoots"] = json!(["/home/dev/auth 1/work", "/"]);
        let mut cwd = exact.clone();
        cwd[0]["cwd"] = json!("/home/dev");
        let mut id = exact;
        id[0]["environmentId"] = json!("local");
        for wrong in [extra, roots, cwd, id, json!([]), json!(null)] {
            assert!(selected(wrong.clone()).is_err(), "{wrong}");
        }
    }

    #[tokio::test]
    async fn local_version_gate_admits_the_qualified_range_only() {
        let dir = tempfile::tempdir().unwrap();
        let codex = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            verify_version(tokio::process::Command::new(path), AgentCli::Codex)
        };
        let supported = descriptor(AgentCli::Codex).release.supported;
        codex("current", "echo 'codex-cli 0.162.1'").await.unwrap();
        let refused = codex("pinned", "echo 'codex-cli 0.158.0'")
            .await
            .unwrap_err();
        assert!(refused.contains(supported), "{refused}");
        assert!(codex("failing", "echo 'codex-cli 0.162.1'; exit 1")
            .await
            .is_err());
        let long = format!("echo 'codex-cli 0.162.1{}'", " ".repeat(VERSION_OUTPUT_MAX));
        assert!(codex("long", &long).await.is_err());
        let missing = tokio::process::Command::new(dir.path().join("missing"));
        let missing = verify_version(missing, AgentCli::Codex).await.unwrap_err();
        assert!(missing.contains(supported), "{missing}");
    }
}
