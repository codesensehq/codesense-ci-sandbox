// SPDX-License-Identifier: MIT
//! Account-owned local harnesses with exact remote execution and opaque task handles.
mod account;
mod claude;
mod claude_executor;
mod claude_tools;
mod harness;
mod mcp_endpoint;
mod registry;
mod runtime;
use super::{remote_authority::RemoteAuthority, remote_executor, remote_workspace_target};
use crate::commands::route_to_context::ActiveContext;
use registry::{Receipt, Store};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

mod types;
pub(crate) use types::{CancelParams, Outcome, ReadParams, StartParams};
struct Live {
    cancel: Arc<AtomicBool>,
    validate: harness::Validate,
}
fn live() -> &'static Mutex<HashMap<String, Arc<Live>>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Arc<Live>>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}
fn provider(value: &str) -> Result<crate::install::agent_account_store::AgentProvider, String> {
    crate::install::agent_account_store::AgentProvider::parse(value)
        .ok_or_else(|| "capabilityUnavailable: unsupported remote task provider".into())
}
fn root(ctx: &ActiveContext) -> PathBuf {
    crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
        .join("remote-agent-tasks-v1")
}
fn key(ctx: &ActiveContext, id: uuid::Uuid) -> String {
    format!("{}:{id}", ctx.user_id)
}
fn task_id(handle: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(
        handle
            .strip_prefix("remote-agent-task-")
            .ok_or("invalid task handle")?,
    )
    .map_err(|_| "invalid task handle".into())
}
fn outcome(
    id: uuid::Uuid,
    receipt: Receipt,
    cursor: usize,
    active: bool,
) -> Result<Outcome, String> {
    if cursor > receipt.text.len() || !receipt.text.is_char_boundary(cursor) {
        return Err("invalid task cursor".into());
    }
    let status = if !active
        && matches!(
            receipt.status.as_str(),
            "starting" | "running" | "cancelRequested"
        ) {
        "unknown".into()
    } else {
        receipt.status
    };
    Ok(Outcome {
        task_handle: format!("remote-agent-task-{id}"),
        status,
        cursor: receipt.text.len().to_string(),
        text: receipt.text[cursor..].to_string(),
        message: receipt.message,
    })
}
pub(crate) async fn start(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    params: StartParams,
    caller: Option<RemoteAuthority>,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> Result<Outcome, String> {
    let id = uuid::Uuid::parse_str(&params.request_id).map_err(|_| "requestId must be a UUID")?;
    let provider = provider(&params.provider)?;
    if params.prompt.trim().is_empty() || params.prompt.len() > 65536 {
        return Err("prompt must contain 1–65536 bytes".into());
    }
    if params.account_id.is_some() != params.account_generation.is_some() {
        return Err("accountId and accountGeneration must be supplied together".into());
    }
    let root = root(&ctx);
    let taskkey = key(&ctx, id);
    // Correlation precedes connectivity: an unavailable executor cannot create a second task.
    if root.join(id.to_string()).exists() {
        let store = Store::open(&root, id)?;
        if let Some(receipt) = store.load()? {
            if receipt.params != params {
                return Err("requestId already belongs to another immutable task".into());
            }
            drop(store);
            authorize(&app, &ctx, &receipt, caller.as_ref()).await?;
            let active = live()
                .lock()
                .map_err(|_| "task registry unavailable")?
                .contains_key(&taskkey);
            return outcome(id, receipt, 0, active);
        }
    }
    let resolved = remote_workspace_target::resolve(
        app.clone(),
        ctx.clone(),
        None,
        &params.workspace_handle,
        caller.clone(),
    )
    .await?;
    scope.require_launch_key(&ctx.codesense_home, &ctx.principal(), &resolved.team_key)?;
    // A provider task names no agent, so it runs with CodeSense alone.
    let grant = std::sync::Arc::new(super::local_mcp_relay::config::Grant::new());
    let projection = super::local_mcp_relay::native::project(&grant, provider)?;
    let account = account::Account::resolve(
        ctx.codesense_home.clone(),
        ctx.principal(),
        provider,
        params.account_id.clone(),
        params.account_generation,
    )?;
    let account_check = account.clone();
    let target_check = resolved.validate.clone();
    let validate: harness::Validate = Arc::new(move || {
        target_check()?;
        account_check.validate()
    });
    harness::check(&validate).await?;
    let qualified = runtime::verify(provider).await?;
    let receipt = Receipt {
        params: params.clone(),
        team: resolved.team_key.clone(),
        workspace: resolved.workspace_id.clone(),
        machine: resolved.machine_id.clone(),
        server: resolved.server_id.clone(),
        account: account.provenance(),
        status: "starting".into(),
        text: String::new(),
        message: None,
    };
    let dir = {
        let store = Store::open(&root, id)?;
        if store.load()?.is_some() {
            return Err("task start raced; retry same request".into());
        }
        store.save(&receipt)?;
        store.dir.clone()
    };
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut tasks = live().lock().map_err(|_| "task registry unavailable")?;
        if tasks.len() >= 16 {
            return Err("remote task capacity reached; intent retained".into());
        }
        tasks.insert(
            taskkey.clone(),
            Arc::new(Live {
                cancel: cancel.clone(),
                validate: validate.clone(),
            }),
        );
    }
    let taskroot = root.clone();
    let task_app = app.clone();
    let original_validate = validate.clone();
    let execution_cancel = cancel.clone();
    let validate: harness::Validate = Arc::new(move || {
        if execution_cancel.load(Ordering::Acquire) {
            return Err("task cancelled".into());
        }
        original_validate()
    });
    let task_authority = RemoteAuthority::for_task(
        app.clone(),
        ctx.user_id,
        &resolved,
        validate.clone(),
        grant,
        format!("remote-agent-task-{id}"),
    );
    tokio::spawn(async move {
        let mut receipt = receipt;
        let result=async {
            let endpoint=remote_executor::open(resolved,id,validate.clone()).await?;
            let mut stopped=true;
            let result=async {
                let mcp=if provider==crate::install::agent_account_store::AgentProvider::Claude {
                    let client=claude_executor::Client::connect(&endpoint,validate.clone()).await?;
                    mcp_endpoint::Endpoint::serve(move || claude_tools::Handler{base:super::handler::CodesenseHandler::new_remote(task_app.clone(),task_authority.clone()),authority:task_authority.clone(),client:client.clone()}).await?
                }else{mcp_endpoint::Endpoint::open(task_app,task_authority).await?};
                let connection=harness::McpConnection{url:mcp.url.clone(),token:mcp.token.clone(),projection};
                let mut process=runtime::Process::spawn(qualified,&dir,account.home.as_deref(),&connection,&params.prompt,validate.clone(),cancel.clone()).await?;
                let result=async {
                    receipt.status="running".into();Store::open(&taskroot,id)?.save(&receipt)?;
                    let environment=harness::Environment{id:endpoint.environment_id().into(),url:endpoint.ws_url().into(),token:endpoint.auth_token().into(),cwd:endpoint.remote_cwd().into()};
                    tokio::select!{
                        result=process.run(environment,&connection,&params.prompt,|text|{if receipt.text.len()+text.len()>registry::MAX_OUTPUT{return Err("task output exceeds bound".into());}receipt.text.push_str(text);Store::open(&taskroot,id)?.save(&receipt)})=>result,
                        _=endpoint.closed()=>Err("remote executor unavailable; no local fallback".into()),
                    }
                }.await;
                stopped=process.stop().await;
                result
            }.await;
            let closed=endpoint.close().await;
            if !stopped||!matches!(closed,remote_executor::CloseOutcome::Confirmed){return Err("task cleanup outcome unknown".into());}
            if cancel.load(Ordering::Acquire){receipt.status="cancelled".into();return Ok(());}
            result
        }.await;
        match result {
            Ok(()) => {
                if receipt.status != "cancelled" {
                    receipt.status = "completed".into();
                }
            }
            Err(error) => {
                receipt.status = "unknown".into();
                receipt.message = Some(error);
            }
        }
        if let Ok(store) = Store::open(&taskroot, id) {
            let _ = store.save(&receipt);
        }
        if let Ok(mut tasks) = live().lock() {
            tasks.remove(&taskkey);
        }
    });
    read(
        app,
        ctx,
        ReadParams {
            task_handle: format!("remote-agent-task-{id}"),
            cursor: None,
        },
        caller,
    )
    .await
}
async fn authorize(
    app: &tauri::AppHandle,
    ctx: &ActiveContext,
    receipt: &Receipt,
    caller: Option<&RemoteAuthority>,
) -> Result<(), String> {
    let account = account::Account::resolve(
        ctx.codesense_home.clone(),
        ctx.principal(),
        provider(&receipt.params.provider)?,
        receipt.params.account_id.clone(),
        receipt.params.account_generation,
    )?;
    account.validate()?;
    if account.provenance() != receipt.account {
        return Err("task account was retired or changed".into());
    }
    let resolved = remote_workspace_target::resolve(
        app.clone(),
        ctx.clone(),
        Some(&receipt.machine),
        &receipt.params.workspace_handle,
        caller.cloned(),
    )
    .await?;
    if resolved.team_key != receipt.team
        || resolved.workspace_id != receipt.workspace
        || resolved.server_id != receipt.server
    {
        return Err("task authority changed".into());
    }
    harness::check(&resolved.validate).await
}
pub(crate) async fn read(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    params: ReadParams,
    caller: Option<RemoteAuthority>,
) -> Result<Outcome, String> {
    let id = task_id(&params.task_handle)?;
    let receipt = {
        let store = Store::open(&root(&ctx), id)?;
        store.load()?.ok_or("task unavailable")?
    };
    authorize(&app, &ctx, &receipt, caller.as_ref()).await?;
    let active = live()
        .lock()
        .map_err(|_| "task registry unavailable")?
        .get(&key(&ctx, id))
        .cloned();
    if let Some(active) = &active {
        harness::check(&active.validate).await?;
    }
    let cursor = params
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| "invalid task cursor")?;
    let mut result = outcome(id, receipt, cursor, active.is_some())?;
    if active.is_some_and(|task| task.cancel.load(Ordering::Acquire))
        && matches!(result.status.as_str(), "starting" | "running")
    {
        result.status = "cancelRequested".into();
    }
    Ok(result)
}
pub(crate) async fn cancel(
    app: tauri::AppHandle,
    ctx: ActiveContext,
    params: CancelParams,
    caller: Option<RemoteAuthority>,
) -> Result<Outcome, String> {
    let id = task_id(&params.task_handle)?;
    let receipt = {
        let store = Store::open(&root(&ctx), id)?;
        store.load()?.ok_or("task unavailable")?
    };
    authorize(&app, &ctx, &receipt, caller.as_ref()).await?;
    if let Some(active) = live()
        .lock()
        .map_err(|_| "task registry unavailable")?
        .get(&key(&ctx, id))
        .cloned()
    {
        active.cancel.store(true, Ordering::Release);
    }
    read(
        app,
        ctx,
        ReadParams {
            task_handle: params.task_handle,
            cursor: None,
        },
        caller,
    )
    .await
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
pub(crate) use fixture::prove_harness;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_task_inputs_never_accept_credentials_or_endpoints() {
        let value = serde_json::json!({"requestId":uuid::Uuid::new_v4(),"workspaceHandle":"bound","provider":"codex","prompt":"work","token":"secret"});
        assert!(serde_json::from_value::<StartParams>(value).is_err());
        assert!(serde_json::from_value::<ReadParams>(
            serde_json::json!({"taskHandle":"opaque","url":"http://desktop"})
        )
        .is_err());
    }
    #[test]
    fn ambiguous_inflight_receipt_never_claims_running_after_restart() {
        let id = uuid::Uuid::new_v4();
        let receipt = Receipt {
            params: StartParams {
                request_id: id.to_string(),
                workspace_handle: "bound".into(),
                provider: "codex".into(),
                account_id: None,
                account_generation: None,
                prompt: "work".into(),
            },
            team: "t".into(),
            workspace: "w".into(),
            machine: "m".into(),
            server: "s".into(),
            account: serde_json::json!({}),
            status: "running".into(),
            text: "é done".into(),
            message: None,
        };
        assert_eq!(
            outcome(id, receipt.clone(), 0, false).unwrap().status,
            "unknown"
        );
        assert!(outcome(id, receipt, 1, false).is_err());
    }
}

#[cfg(test)]
mod claude_fixture;
#[cfg(test)]
pub(crate) use claude_fixture::prove_claude_harness;

#[cfg(test)]
mod profile_fixture;
