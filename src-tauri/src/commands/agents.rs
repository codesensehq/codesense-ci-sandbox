// SPDX-License-Identifier: MIT
//! The Agents tab's commands: thin wrappers over [`crate::agents::service`],
//! [`crate::agents::resolve`] and [`crate::agents::binding`].
//!
//! Each command resolves who is asking and runs one testable body inside
//! `await_blocking`, so no filesystem work happens on the async runtime. Keys
//! travel as the rendered strings the service serializes. Every write the
//! service or a binding makes emits `agents:changed` itself, so the tab
//! re-reads on that event rather than on a reply.
//!
//! There is no write authorization: anyone can archive, delete or copy any
//! agent they can see. A stale base revision and a delete refused because
//! routines still use the agent are answers, not errors.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::agents::binding;
use crate::agents::content_reader::{ContentReader, FsContentReader, NodeKind};
use crate::agents::context::ContextEntry;
use crate::agents::context_edit::{self, ContextEdit, ContextOp};
use crate::agents::defs::RevisionRecord;
use crate::agents::launch_check::LaunchWarning;
use crate::agents::manifest::ManifestTotals;
use crate::agents::manifest::{AgentRevisionRef, ContentLayer};
use crate::agents::manifest_resolve::{
    content_layers, context_tier_roots, not_carried, ContextTier, ContextWarning, ManifestTarget,
    ResolvedContextEntry,
};
use crate::agents::model::{Agent, LaunchMode};
use crate::agents::resolve::{account_preview, ResolveOptions};
use crate::agents::service::SaveTarget;
use crate::agents::service::{self, AgentError, Placement};
use crate::agents::{AgentKey, AgentScope, StartupPath};
use crate::commands::await_blocking;
use crate::commands::routines::{author, caller, personal_owner, Caller};
use crate::commands::workspace_open::{
    open_agent, AppOpenWorkspacePort, OpenRequest, OpenStatus, OpenWorkspacePort,
};
use crate::domain::SourceRepoSlug;
use crate::mcp::agent_tools::{
    self, AccountItem, AgentCaller, AgentCreateParams, AgentDraft, AgentPreviewParams, AgentReply,
    AgentRestoreParams, AgentSaved, AgentUpdateParams, Refusal, RefusalCode, ResolvedItem,
};
use crate::routines::agents::RoutineReferrers;
use crate::routines::lifecycle::{self, OwnedRoutine};
use crate::routines::Author;
#[cfg(test)]
#[allow(unused_imports)]
use crate::routines::PersonalOwner;
use codesense_pty_core::agent_cli::AgentCli;

/// One listed agent with what the tab shows beside it.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRow {
    pub key: String,
    pub name: String,
    pub description: String,
    pub runtime: AgentCli,
    pub launch: LaunchMode,
    pub scope: AgentScope,
    pub org: String,
    pub team: String,
    /// The creator's display name. Attribution only.
    pub created_by: String,
    /// The caller created it. Only credits it: anyone can change any agent.
    pub created_by_you: bool,
    pub archived: bool,
    pub revision: u32,
    pub default_prompt: String,
    /// The face the agent definition picked (`pack:id`), or `None` to let the
    /// renderer pool-deal a stable face from the agent id. Mirrors `Agent::avatar`
    /// so the inbox sender renderer can show the configured face without a second
    /// round-trip to the agent detail.
    pub avatar: Option<String>,
    pub role: Option<String>,
    /// The model its args name (`--model`, `--model=` or `-m`); `None` is the CLI default.
    pub model: Option<String>,
    /// The account this user would launch it with, and why when it cannot.
    pub account: AccountItem,
    /// The routines (that the caller can see) naming it, sorted by key.
    pub used_by: Vec<String>,
    /// Its own active routines (SW-AGR-1): what archiving it archives too, so
    /// the confirm lists them.
    pub routines: Vec<OwnedRoutine>,
    /// The routines its archive archived that are still archived: offered
    /// back (a "Restore routines" action) until restored.
    pub archived_routines: Vec<OwnedRoutine>,
    /// How many live sessions run it, at any revision.
    pub running_now: usize,
}

/// Native model args take precedence over non-secret inline startup model settings.
fn model_arg(agent: &Agent) -> Option<String> {
    let endpoint = crate::agents::adapter::desktop(agent.runtime).model_endpoint;
    let model = match endpoint {
        Some(port) => (port.model_arguments)(&agent.args).model,
        None => crate::session_usage::record::requested_model(&agent.args).map(str::to_owned),
    };
    model.or_else(|| {
        let settings = endpoint.and_then(|_| agent.startup_settings())?;
        serde_json::Value::as_str(settings.get("model")?.get("default")?)
            .filter(|model| !std::primitive::str::is_empty(std::primitive::str::trim(model)))
            .map(str::to_owned)
    })
}

/// One agent with its history and what a launch of it would run.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDetailView {
    pub key: String,
    pub scope: AgentScope,
    pub created_by: String,
    pub created_by_you: bool,
    /// The definition exactly as stored.
    pub definition: Agent,
    /// Every revision, oldest first, each with its author.
    pub revisions: Vec<RevisionRecord>,
    pub resolved: ResolvedItem,
}

/// What an archive, unarchive or copy did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentWriteReply {
    Saved {
        key: String,
        revision: u32,
        archived: bool,
        /// Archive: the routines archived with it. Unarchive: the routines its
        /// archive archived, still archived, to offer back
        /// (`agent_restore_routines`).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        routines: Vec<OwnedRoutine>,
    },
    Conflict {
        current_revision: u32,
    },
}

/// What a delete did. Routines still using the agent refuse it, listed so the
/// tab can show them; nothing was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentDeleteReply {
    Deleted {
        key: String,
    },
    InUse {
        referrers: Vec<String>,
        /// Those of `referrers` that are its own routines (the rest only wait
        /// on it finishing).
        owned: Vec<String>,
    },
    Conflict {
        current_revision: u32,
    },
}

/// The agent as the editor's form sends it: exactly the fields the
/// `agent_create` tool takes, plus where it lives. `key` names the agent an
/// edit changes; a create places it by `scope` (`personal`, the default, or
/// `team` with `org` and `team`). The form always carries every field, so an
/// edit replaces the whole definition, both runtimes' permission blocks
/// included (a block left out is removed).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentForm {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(flatten)]
    pub draft: AgentDraft,
}

/// A team, by org and name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentTeamRef {
    pub org: String,
    pub team: String,
}

/// What a save or a restore did. A stale base revision is an answer, not an
/// error: the editor keeps the user's edit and offers a reload. An input the
/// tool body refuses names the field, exactly as the MCP tools do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentSaveReply {
    Saved {
        key: String,
        revision: u32,
        /// What a launch of the saved agent would be warned about; shown,
        /// never enforced.
        warnings: Vec<LaunchWarning>,
    },
    Conflict {
        current_revision: u32,
    },
    Invalid {
        field: Option<String>,
        message: String,
    },
}

/// What the editor's live preview resolved, or which input stops the draft
/// from resolving at all.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentPreviewReply {
    Resolved {
        resolved: Box<ResolvedItem>,
        /// The MCP servers the team the preview resolved against offers, the
        /// bridge aside: what the editor's server picker lists.
        servers: Vec<String>,
    },
    Invalid {
        field: Option<String>,
        message: String,
    },
}

/// Where an editor save writes.
#[derive(Debug, Clone)]
pub(crate) enum AgentFormTarget {
    Create(Placement),
    Update { key: String, base_revision: u32 },
}

/// One file or folder the startup picker offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupCatalogEntry {
    /// The plain content-relative path a startup entry stores.
    pub path: String,
    pub is_dir: bool,
    /// The layer it comes from: the first one, in resolution order, that has
    /// it, which is the one a launch would use.
    pub layer: ContentLayer,
}

/// One directory level of a team's content, across its layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartupCatalog {
    /// The directory listed, normalized; empty for the content root.
    pub dir: String,
    /// This page of the level: folders first, then files, each by name.
    pub entries: Vec<StartupCatalogEntry>,
    /// How many names at this level match the filter, across every page.
    pub total: usize,
    /// More entries follow this page.
    pub truncated: bool,
    /// The `offset` that asks for the next page, while one follows.
    pub next_offset: Option<usize>,
}

/// The most entries one catalog page returns. A display page only: the next
/// page, a filter or a typed path reaches everything past it.
pub const CATALOG_LIMIT: usize = 500;

/// Which page of a catalog level to list, and which names to keep.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CatalogPage<'a> {
    /// How many matching names (in name order) to skip.
    pub offset: usize,
    /// Keep only names containing this, ignoring case.
    pub filter: Option<&'a str>,
}

/// The agent "Describe it to an agent" opens: a visible team-workspace agent
/// started with a plain request. The `agent_*` MCP tool descriptions carry
/// everything it needs, so it depends on no skill being in the team.
const AUTHORING_AGENT_NAME: &str = "Agent author";
const AUTHORING_PROMPT: &str = "Help me create a CodeSense agent. Use the `agent_*` MCP tools; `agent_create` explains the fields.";

/// Whether the authoring agent is up, and what to tell the user when it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentAuthoring {
    pub opened: bool,
    pub message: String,
}

fn parse_key(key: &str) -> Result<AgentKey, String> {
    AgentKey::parse(key).ok_or_else(|| format!("not an agent key: {key:?}"))
}

fn written(saved: Result<service::SavedAgent, AgentError>) -> Result<AgentWriteReply, String> {
    match saved {
        Ok(saved) => Ok(AgentWriteReply::Saved {
            key: saved.key.to_string(),
            revision: saved.agent.revision,
            archived: saved.agent.is_archived(),
            routines: Vec::new(),
        }),
        Err(AgentError::Conflict { current_revision }) => {
            Ok(AgentWriteReply::Conflict { current_revision })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// How many of `revisions` (each a live session's `"<key>@<revision>"` stamp)
/// run each agent, whatever the revision.
pub(crate) fn running_by_agent<'a>(
    revisions: impl IntoIterator<Item = &'a str>,
) -> HashMap<AgentKey, usize> {
    let mut counts = HashMap::new();
    for key in revisions.into_iter().filter_map(AgentRevisionRef::key_of) {
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// Every agent the caller can see, in discovery order (the tab orders them),
/// with its account line, the routines using it and how many sessions run it.
/// `running` is each live session's agent stamp.
pub(crate) fn list(
    caller: &Caller,
    include_archived: bool,
    running: &HashMap<AgentKey, usize>,
) -> Result<Vec<AgentRow>, String> {
    let found = service::list(&caller.home, &caller.principal, include_archived)
        .map_err(|e| e.to_string())?;
    // An unreadable routine store shows as "used by none" here; a delete still
    // reads it again and refuses.
    let used_by = RoutineReferrers::by_agent(&caller.home, &caller.principal)
        .inspect_err(
            |e| log::warn!(target: "agents", "routines unreadable while listing agents: {e}"),
        )
        .unwrap_or_default();
    let mut owned = lifecycle::by_agent(&caller.home, &caller.principal)
        .inspect_err(
            |e| log::warn!(target: "agents", "routines unreadable while listing agents: {e}"),
        )
        .unwrap_or_default();
    Ok(found
        .into_iter()
        .map(|found| {
            let account = account_preview(
                &caller.home,
                &caller.principal,
                &found.key,
                found.agent.runtime,
            );
            let lifecycle::AgentRoutines {
                active,
                archived_with,
            } = owned.remove(&found.key).unwrap_or_default();
            AgentRow {
                routines: active,
                archived_routines: archived_with,
                key: found.key.to_string(),
                created_by_you: found.agent.created_by == caller.principal.as_str(),
                name: found.agent.name.clone(),
                description: found.agent.description.clone(),
                runtime: found.agent.runtime,
                launch: found.agent.launch,
                scope: found.scope,
                org: found.org,
                team: found.team,
                created_by: found.created_by_display,
                archived: found.agent.is_archived(),
                revision: found.agent.revision,
                default_prompt: found.agent.default_prompt.clone(),
                avatar: found.agent.avatar.clone(),
                role: found.agent.role.clone(),
                model: model_arg(&found.agent),
                account: AccountItem::from(&account),
                used_by: used_by.get(&found.key).cloned().unwrap_or_default(),
                running_now: running.get(&found.key).copied().unwrap_or(0),
            }
        })
        .collect())
}

#[cfg(test)]
pub(crate) fn detail(caller: &Caller, key: &str) -> Result<AgentDetailView, String> {
    let key = parse_key(key)?;
    let (detail, reference) =
        service::get_snapshot(&caller.home, &caller.principal, &key).map_err(|e| e.to_string())?;
    let account = account_preview(&caller.home, &caller.principal, &key, detail.agent.runtime);
    detail_from_snapshot(caller, detail, reference, account)
}

pub(crate) fn detail_from_snapshot(
    caller: &Caller,
    detail: service::AgentDetail,
    reference: crate::agents::AgentRevisionRef,
    account: crate::agents::resolve::AgentAccountChoice,
) -> Result<AgentDetailView, String> {
    let definition = detail.agent;
    let resolved = crate::agents::resolve::compose_with_account(
        &caller.home,
        &caller.principal,
        &definition,
        reference,
        ResolveOptions::default(),
        account,
    );
    Ok(AgentDetailView {
        key: detail.key.to_string(),
        scope: detail.scope,
        created_by: detail.created_by_display,
        created_by_you: definition.created_by == caller.principal.as_str(),
        definition,
        revisions: detail.revisions,
        resolved: ResolvedItem::from(resolved),
    })
}

pub(crate) fn row_from_detail(
    caller: &Caller,
    detail: &service::AgentDetail,
    before: Option<&AgentRow>,
    account: AccountItem,
) -> AgentRow {
    let a = &detail.agent;
    AgentRow {
        key: detail.key.to_string(),
        name: a.name.clone(),
        description: a.description.clone(),
        runtime: a.runtime,
        launch: a.launch,
        scope: detail.scope,
        org: detail.key.org.clone(),
        team: detail.key.team.clone(),
        created_by: detail.created_by_display.clone(),
        created_by_you: a.created_by == caller.principal.as_str(),
        archived: a.is_archived(),
        revision: a.revision,
        default_prompt: a.default_prompt.clone(),
        avatar: a.avatar.clone(),
        role: a.role.clone(),
        model: model_arg(a),
        account,
        used_by: before.map(|r| r.used_by.clone()).unwrap_or_default(),
        routines: before.map(|r| r.routines.clone()).unwrap_or_default(),
        archived_routines: before
            .map(|r| r.archived_routines.clone())
            .unwrap_or_default(),
        running_now: before.map_or(0, |r| r.running_now),
    }
}

pub(crate) fn refresh_facts<'a>(caller: &Caller, rows: impl Iterator<Item = &'a mut AgentRow>) {
    let used = RoutineReferrers::by_agent(&caller.home, &caller.principal).unwrap_or_default();
    let owned = lifecycle::by_agent(&caller.home, &caller.principal).unwrap_or_default();
    for row in rows {
        let Some(key) = AgentKey::parse(&row.key) else {
            continue;
        };
        row.used_by = used.get(&key).cloned().unwrap_or_default();
        let routines = owned.get(&key).cloned().unwrap_or_default();
        row.routines = routines.active;
        row.archived_routines = routines.archived_with;
    }
}

/// Archive an agent and, once the user confirmed (`archive_routines`), its
/// routines with it. An agent with active routines and no confirm is
/// refused, naming them, and nothing changes.
pub(crate) fn archive(
    caller: &Caller,
    key: &str,
    author: &Author,
    archive_routines: bool,
) -> Result<AgentWriteReply, String> {
    let key = parse_key(key)?;
    // A starter is read-only: the service refuses it, whatever runs on it.
    let owned = if crate::agents::builtin::is_starter(&key) {
        Vec::new()
    } else {
        lifecycle::active_owned(&caller.home, &caller.principal, &key).map_err(|e| e.to_string())?
    };
    if !owned.is_empty() && !archive_routines {
        let names: Vec<&str> = owned.iter().map(|r| r.name.as_str()).collect();
        return Err(format!(
            "archiving this agent also archives its routines ({}): confirm first",
            names.join(", ")
        ));
    }
    let reply = written(service::archive(
        &caller.home,
        &caller.principal,
        &key,
        author,
    ))?;
    let archived = lifecycle::archive_owned(&caller.home, &caller.principal, &key, author)
        .map_err(|e| e.to_string())?;
    Ok(with_routines(reply, archived))
}

/// Bring an archived agent back, offering the routines its archive archived.
pub(crate) fn unarchive(
    caller: &Caller,
    key: &str,
    base_revision: u32,
    author: &Author,
) -> Result<AgentWriteReply, String> {
    let key = parse_key(key)?;
    let reply = written(service::unarchive(
        &caller.home,
        &caller.principal,
        &key,
        base_revision,
        author,
    ))?;
    let offered = lifecycle::archived_with(&caller.home, &caller.principal, &key)
        .map_err(|e| e.to_string())?;
    Ok(with_routines(reply, offered))
}

/// Unarchive the routines `key`'s archive archived (the offer unarchive made).
pub(crate) fn restore_routines(
    caller: &Caller,
    key: &str,
    author: &Author,
) -> Result<Vec<OwnedRoutine>, String> {
    lifecycle::restore_owned(&caller.home, &caller.principal, &parse_key(key)?, author)
        .map_err(|e| e.to_string())
}

fn with_routines(reply: AgentWriteReply, routines: Vec<OwnedRoutine>) -> AgentWriteReply {
    match reply {
        AgentWriteReply::Saved {
            key,
            revision,
            archived,
            ..
        } => AgentWriteReply::Saved {
            key,
            revision,
            archived,
            routines,
        },
        other => other,
    }
}

pub(crate) fn delete(
    caller: &Caller,
    key: &str,
    base_revision: u32,
) -> Result<AgentDeleteReply, String> {
    let key = parse_key(key)?;
    match service::delete(
        &caller.home,
        &caller.principal,
        &key,
        Some(base_revision),
        &RoutineReferrers,
    ) {
        Ok(()) => Ok(AgentDeleteReply::Deleted {
            key: key.to_string(),
        }),
        Err(AgentError::InUse { referrers, owned }) => {
            Ok(AgentDeleteReply::InUse { referrers, owned })
        }
        Err(AgentError::Conflict { current_revision }) => {
            Ok(AgentDeleteReply::Conflict { current_revision })
        }
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn copy_to_mine(
    caller: &Caller,
    key: &str,
    placement: Placement,
    author: &Author,
) -> Result<AgentWriteReply, String> {
    written(service::copy_to_mine(
        &caller.home,
        &caller.principal,
        &parse_key(key)?,
        placement,
        author,
    ))
}

/// This user's machine-local settings for one agent, as the tab shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentBindingView {
    /// The bound account; `None` runs the runtime's default account.
    pub account_id: Option<String>,
    /// False when the bound account no longer exists.
    pub account_exists: bool,
    pub capture_raw_terminal: bool,
}

/// Change what is given and leave the rest, all or nothing: bind an account,
/// set the raw-terminal consent, or both.
pub(crate) fn binding_set(
    caller: &Caller,
    key: &str,
    account_id: Option<&str>,
    capture_raw_terminal: Option<bool>,
) -> Result<AgentBindingView, String> {
    let key = parse_key(key)?;
    let (home, principal) = (&caller.home, &caller.principal);
    let entry = binding::update(
        home,
        principal,
        &key,
        binding::BindingChange {
            account_id,
            capture_raw_terminal,
        },
    )
    .map_err(|e| e.to_string())?;
    // The change is saved; an unreadable account store only leaves the existence check off.
    let account = binding::get(home, principal, &key).ok().flatten();
    Ok(AgentBindingView {
        account_exists: account.as_ref().is_some_and(|a| a.account_exists),
        account_id: entry.account_id,
        capture_raw_terminal: entry.capture_raw_terminal,
    })
}

pub(crate) fn binding_clear(caller: &Caller, key: &str) -> Result<bool, String> {
    binding::clear(&caller.home, &caller.principal, &parse_key(key)?).map_err(|e| e.to_string())
}

/// The tool caller a tab write is: the local user, credited as `author`, with
/// no session to default a runtime from.
fn tab_caller(author: &Author) -> AgentCaller {
    AgentCaller {
        author: author.clone(),
        session_runtime: None,
    }
}

/// `permissions` as a whole-definition replacement: each runtime block the
/// form leaves out is removed, so what the form shows is what is saved. Keys
/// beside the runtime blocks, which the form never shows, are kept.
fn whole_permissions(value: Option<serde_json::Value>) -> serde_json::Value {
    match value {
        None | Some(serde_json::Value::Null) => whole_permissions(Some(serde_json::json!({}))),
        Some(serde_json::Value::Object(mut blocks)) => {
            for runtime in AgentCli::ALL {
                blocks
                    .entry(runtime.as_str())
                    .or_insert(serde_json::Value::Null);
            }
            serde_json::Value::Object(blocks)
        }
        // Not an object: the tool body refuses it, naming the field.
        Some(other) => other,
    }
}

/// The editor's form as an update: each field it sends replaces the agent's
/// (an empty one clears it), one it leaves out is kept, and `permissions` is
/// always whole.
fn update_params(key: String, base_revision: u32, draft: AgentDraft) -> AgentUpdateParams {
    AgentUpdateParams {
        id: key,
        base_revision,
        name: Some(draft.name),
        description: draft.description,
        runtime: draft.runtime,
        args: draft.args,
        startup_settings: Some(draft.startup_settings),
        env: draft.env.map(Some),
        launch: draft.launch,
        mcp_servers: draft.mcp_servers,
        mcp_bridge_tools: draft.mcp_bridge_tools,
        startup: draft.startup,
        context: draft.context.map(Some),
        repo_overrides: draft.repo_overrides,
        permissions: Some(Some(whole_permissions(draft.permissions))),
        default_prompt: draft.default_prompt,
        // The editor shows no face: an edit keeps the one the agent has.
        avatar: draft.avatar,
        role: Some(draft.role),
        allow_wake: draft.allow_wake,
        resume_after_restart: draft.resume_after_restart,
        budget_usd: draft.budget_usd,
        session_id: None,
        unknown: draft.unknown,
    }
}

fn refused_save(refusal: Refusal) -> Result<AgentSaveReply, String> {
    match refusal.code {
        RefusalCode::Conflict => Ok(AgentSaveReply::Conflict {
            current_revision: refusal.current_revision.unwrap_or_default(),
        }),
        RefusalCode::Invalid => Ok(AgentSaveReply::Invalid {
            field: refusal.field,
            message: refusal.message,
        }),
        RefusalCode::NotFound
        | RefusalCode::InUse
        | RefusalCode::ReadOnly
        | RefusalCode::NeedsConfirm => Err(refusal.message),
    }
}

/// What the editor shows for a save the tool bodies answered.
fn save_reply(reply: AgentReply<AgentSaved>) -> Result<AgentSaveReply, String> {
    match reply {
        AgentReply::Ok(AgentSaved {
            id,
            revision,
            preview,
            ..
        }) => Ok(AgentSaveReply::Saved {
            key: id,
            revision,
            warnings: preview.map(|p| p.warnings).unwrap_or_default(),
        }),
        AgentReply::Refused(refusal) => refused_save(refusal),
    }
}

/// An editor save through the same bodies the `agent_create` and
/// `agent_update` tools run, so the form and an agent are parsed and
/// validated identically. Manifest errors never stop a save.
pub(crate) fn save(
    caller: &Caller,
    target: AgentFormTarget,
    draft: AgentDraft,
    author: &Author,
) -> Result<AgentSaveReply, String> {
    let tool_caller = tab_caller(author);
    let reply = match target {
        AgentFormTarget::Create(placement) => agent_tools::create(
            &caller.home,
            &caller.principal,
            placement,
            &tool_caller,
            AgentCreateParams {
                draft,
                scope: None,
                org: None,
                team: None,
                dry_run: false,
                session_id: None,
            },
        )?,
        AgentFormTarget::Update { key, base_revision } => agent_tools::update(
            &caller.home,
            &caller.principal,
            &tool_caller,
            update_params(key, base_revision, draft),
        )?,
    };
    save_reply(reply)
}

/// What the unsaved `draft` would resolve to at `placement`, against `target`
/// when given (else the agent's own team). Writes nothing.
pub(crate) fn preview(
    caller: &Caller,
    placement: Placement,
    draft: AgentDraft,
    target: Option<AgentTeamRef>,
    author: &Author,
) -> Result<AgentPreviewReply, String> {
    preview_at(caller, placement, draft, target, author, None)
}

pub(crate) fn preview_at(
    caller: &Caller,
    placement: Placement,
    draft: AgentDraft,
    target: Option<AgentTeamRef>,
    author: &Author,
    cwd: Option<String>,
) -> Result<AgentPreviewReply, String> {
    let target_team = target.map(|t| (t.org, t.team));
    let reply = agent_tools::preview_with(
        &caller.home,
        &caller.principal,
        placement,
        &tab_caller(author),
        AgentPreviewParams {
            draft,
            agent: None,
            repo: None,
            cwd,
            team_workspace: false,
            scope: None,
            org: None,
            team: None,
            session_id: None,
        },
        ResolveOptions { target_team },
    )?;
    Ok(match reply {
        AgentReply::Ok(resolved) => {
            // SW-AN-50: `offered_mcp_servers` is already on the resolved view;
            // mirror it onto the preview reply so the editor keeps its field.
            let servers = resolved.offered_mcp_servers.clone();
            AgentPreviewReply::Resolved {
                resolved: Box::new(resolved),
                servers,
            }
        }
        AgentReply::Refused(refusal) => AgentPreviewReply::Invalid {
            field: refusal.field,
            message: refusal.message,
        },
    })
}

/// Make revision `revision`'s definition current again as a new revision,
/// against `base_revision`.
pub(crate) fn restore_revision(
    caller: &Caller,
    key: &str,
    revision: u32,
    base_revision: u32,
    author: &Author,
) -> Result<AgentSaveReply, String> {
    let reply = agent_tools::restore(
        &caller.home,
        &caller.principal,
        &tab_caller(author),
        AgentRestoreParams {
            id: key.to_string(),
            revision,
            base_revision,
            session_id: None,
        },
    )?;
    match reply {
        AgentReply::Ok(changed) => Ok(AgentSaveReply::Saved {
            key: changed.id,
            revision: changed.revision,
            warnings: Vec::new(),
        }),
        AgentReply::Refused(refusal) => refused_save(refusal),
    }
}

/// SW-XS-15: one directory a want-set file can come from, on this Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentContextRootView {
    pub tier: ContextTier,
    pub path: PathBuf,
}

/// SW-XS-15: one file of an agent's want set. `root` indexes
/// [`AgentContextView::roots`] (its tier and directory); `None` when its tier
/// has no directory here. Inherited files carry their admitted source and tier
/// individually; their ancestor directory is not exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentContextFileView {
    pub destination: String,
    pub logical: String,
    pub bytes: u64,
    pub root: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<ContextTier>,
}

/// SW-XS-15: one stored per-repo override of an agent's context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRepoOverrideView {
    /// `host/owner/repo`.
    pub repo: String,
    pub add: Vec<ContextEntry>,
    pub remove: Vec<String>,
}

/// SW-XS-15: what the left nav's "Context for ‹Agent› · in ‹Repo›" overlay
/// shows: the agent's want set for `repo` (every tier resolved, as
/// `agent_preview {agent, repo}` resolves it; with no repo, a team-workspace
/// launch's), plus what the menu needs to predict an edit, at `revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentContextView {
    pub key: String,
    pub name: String,
    /// What an edit's `base_revision` must be.
    pub revision: u32,
    /// The stored `context`; `None` when the runtime default applies.
    pub context: Option<Vec<ContextEntry>>,
    /// What a team-wide edit writes out first while `context` is `None`: the
    /// runtime's default (`default_context(runtime, false)`).
    pub runtime_default: Vec<ContextEntry>,
    pub startup: Vec<String>,
    pub repo_overrides: Vec<AgentRepoOverrideView>,
    /// `host/owner/repo` when resolved for a repo.
    pub repo: Option<String>,
    pub defaulted: bool,
    pub entries: Vec<ResolvedContextEntry>,
    pub roots: Vec<AgentContextRootView>,
    /// At most [`NAV_CONTEXT_MAX_FILES`]; `truncated` when cut.
    pub files: Vec<AgentContextFileView>,
    pub truncated: bool,
    /// Every file, however many are listed.
    pub totals: ManifestTotals,
    pub warnings: Vec<ContextWarning>,
    /// SW-XS-16: the one-time `projections.yml` suggestion (XS-14) for this agent at
    /// this revision, so the editor needs no second read; `None` once accepted,
    /// dismissed, or when there is nothing to suggest.
    pub context_suggestion: Option<crate::agents::context_suggestion::ContextSuggestion>,
}

/// The most want-set files the nav overlay is sent. Totals count them all.
pub const NAV_CONTEXT_MAX_FILES: usize = 20_000;

fn parse_repo_arg(repo: Option<&str>) -> Result<Option<SourceRepoSlug>, String> {
    match repo.map(str::trim).filter(|r| !r.is_empty()) {
        Some(raw) => Ok(Some(
            crate::agents::context::parse_repo(raw).map_err(|e| e.to_string())?,
        )),
        None => Ok(None),
    }
}

/// SW-XS-15: `key`'s want set for `repo` (`owner/repo` or `host/owner/repo`).
/// With no repo it is what a team-workspace launch gets (the team-workspace
/// default applies while the agent has no `context`). Writes nothing.
pub(crate) fn context_view(
    caller: &Caller,
    key: &str,
    repo: Option<&str>,
) -> Result<AgentContextView, String> {
    let key = parse_key(key)?;
    let repo = parse_repo_arg(repo)?;
    let team_workspace = repo.is_none();
    let agent =
        service::read_current(&caller.home, &caller.principal, &key).map_err(|e| e.to_string())?;
    let target = ManifestTarget {
        repo,
        ..ManifestTarget::for_launch(&key, None)
    };
    let resolution = service::resolve_agent_context(
        &caller.home,
        &caller.principal,
        &key,
        &target,
        team_workspace,
    )
    .map_err(|e| e.to_string())?
    .capped(NAV_CONTEXT_MAX_FILES);
    let (overrides, _) = service::repo_overrides(&caller.home, &caller.principal, &key)
        .map_err(|e| e.to_string())?;
    let roots: Vec<AgentContextRootView> =
        context_tier_roots(&caller.home, &caller.principal, &target)
            .into_iter()
            .filter_map(|(tier, root)| {
                root.own_path().map(|path| AgentContextRootView {
                    tier,
                    path: path.to_path_buf(),
                })
            })
            .collect();
    let files = resolution
        .files
        .into_iter()
        .map(|file| {
            let root = roots.iter().position(|r| r.tier == file.tier);
            AgentContextFileView {
                source: root.is_none().then_some(file.source),
                tier: root.is_none().then_some(file.tier),
                root,
                destination: file.destination,
                logical: file.logical,
                bytes: file.bytes,
            }
        })
        .collect();
    let name = if agent.name.trim().is_empty() {
        agent.id.clone()
    } else {
        agent.name.clone()
    };
    let context_suggestion = crate::agents::context_suggestion::suggestion_for(
        &caller.home,
        &caller.principal,
        &key,
        &agent,
    );
    Ok(AgentContextView {
        key: key.to_string(),
        name,
        revision: agent.revision,
        context: agent.context.clone(),
        runtime_default: crate::agents::context::default_context(agent.runtime, false),
        startup: agent.startup.clone(),
        repo_overrides: overrides
            .into_iter()
            .map(|(repo, o)| AgentRepoOverrideView {
                repo,
                add: o.add,
                remove: o.remove,
            })
            .collect(),
        repo: resolution.repo,
        defaulted: resolution.defaulted,
        entries: resolution.entries,
        roots,
        files,
        truncated: resolution.truncated,
        totals: resolution.totals,
        warnings: resolution.warnings,
        context_suggestion,
    })
}

/// SW-XS-15: one nav edit, as `agent_context_update` takes it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextEditRequest<'a> {
    pub key: &'a str,
    pub base_revision: u32,
    pub op: ContextOp,
    pub path: &'a str,
    /// SW-XS-16: where an `add` lands, when not at its own path.
    pub dest: Option<&'a str>,
    /// SW-XS-16: what an `add_entries` adds.
    pub entries: &'a [ContextEntry],
    /// SW-XS-16: the `projections.yml` suggestion (its fingerprint) an `add_entries`
    /// accepts. Once the save lands it is recorded as dismissed, so the suggestion is
    /// one-time (SPEC §12.2) even if an accepted entry is later removed.
    pub accepts: Option<&'a str>,
    /// "… for ‹Repo› only": the repo whose override the edit changes.
    pub repo: Option<&'a str>,
    /// The repo the nav is showing, which the reply's view is resolved for.
    pub view_repo: Option<&'a str>,
}

/// SW-XS-15: what one nav edit did. `Saved` carries the want set re-resolved
/// once after the write, so the overlay needs no second read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentContextEditReply {
    Saved {
        key: String,
        revision: u32,
        view: Box<AgentContextView>,
    },
    /// Changed since `base_revision`: nothing was written.
    Conflict { current_revision: u32 },
    /// The edit cannot be made here; nothing was written.
    Blocked { reason: String },
    /// The edited definition does not save.
    Invalid { message: String },
}

/// SW-XS-15: one "Add to / Remove from ‹Agent›'s context" edit from the left
/// nav (`repo` given: "… for ‹Repo› only"), applied on the server to the
/// agent's stored definition so every key it does not touch, a newer build's
/// included, is written back as it was. A team-wide edit while the agent has
/// no `context` writes the runtime default out explicitly first. The save is a
/// compare-and-swap on `base_revision`. The reply's view is resolved for
/// `view_repo`: the repo the nav is showing, whatever the edit's `repo`.
pub(crate) fn context_edit(
    caller: &Caller,
    edit: ContextEditRequest<'_>,
    author: &Author,
) -> Result<AgentContextEditReply, String> {
    let ContextEditRequest {
        key,
        base_revision,
        op,
        path,
        dest,
        entries,
        accepts,
        repo,
        view_repo,
    } = edit;
    let parsed = parse_key(key)?;
    let slug = parse_repo_arg(repo)?;
    let agent = service::read_current(&caller.home, &caller.principal, &parsed)
        .map_err(|e| e.to_string())?;
    if agent.revision != base_revision {
        return Ok(AgentContextEditReply::Conflict {
            current_revision: agent.revision,
        });
    }
    let current = match &slug {
        Some(slug) => Some(
            service::repo_override(&caller.home, &caller.principal, &parsed, slug)
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let current_ref = current.as_ref().map(Option::as_ref);
    let edit = match op {
        ContextOp::Add => context_edit::plan_add(&agent, path, dest, current_ref),
        ContextOp::Remove => context_edit::plan(&agent, op, path, current_ref),
        ContextOp::Customize if slug.is_none() => context_edit::plan_customize(&agent),
        ContextOp::AddEntries if slug.is_none() => context_edit::plan_add_entries(&agent, entries),
        ContextOp::DropRemove if slug.is_some() => {
            context_edit::plan_drop_remove(&agent, path, current_ref.flatten())
        }
        ContextOp::DropAdd if slug.is_some() => {
            context_edit::plan_drop_add(&agent, path, current_ref.flatten())
        }
        ContextOp::Customize | ContextOp::AddEntries => {
            return Err("this context edit is team-wide; it takes no repo".into());
        }
        ContextOp::DropRemove | ContextOp::DropAdd => {
            return Err("this context edit changes a repo's override; it needs a repo".into());
        }
    };
    let (agent, overrides) = match (edit, slug) {
        (ContextEdit::Blocked(reason), _) => {
            return Ok(AgentContextEditReply::Blocked { reason });
        }
        (ContextEdit::Context(context), _) => (
            Agent {
                context: Some(context),
                ..agent
            },
            Vec::new(),
        ),
        (ContextEdit::Override(o), Some(slug)) => (agent, vec![(slug, o)]),
        (ContextEdit::Override(_), None) => {
            return Err("a repo override edit needs a repo".into());
        }
    };
    let saved = match service::save_with_overrides(
        &caller.home,
        &caller.principal,
        SaveTarget::Update {
            key: parsed,
            base_revision,
        },
        agent,
        &overrides,
        author,
    ) {
        Ok(saved) => saved,
        Err(AgentError::Conflict { current_revision }) => {
            return Ok(AgentContextEditReply::Conflict { current_revision });
        }
        Err(AgentError::Validation(e)) => {
            return Ok(AgentContextEditReply::Invalid {
                message: e.to_string(),
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    if let (ContextOp::AddEntries, Some(fingerprint)) = (op, accepts) {
        // The agent is saved; failing to record the dismissal only means the (now
        // satisfied) suggestion could be offered again later.
        if let Err(e) = crate::agents::context_suggestion::dismiss(
            &caller.home,
            &caller.principal,
            &saved.key,
            fingerprint,
        ) {
            log::warn!("agent_context_update: could not record the accepted suggestion: {e}");
        }
    }
    // Re-resolve the view the nav is SHOWING (`view_repo`), which a team-wide edit
    // made while a repo is picked does not name.
    let view = context_view(caller, &saved.key.to_string(), view_repo)?;
    Ok(AgentContextEditReply::Saved {
        key: saved.key.to_string(),
        revision: saved.agent.revision,
        view: Box::new(view),
    })
}

/// One page of one level of `team`'s content under `dir` (the content root
/// when empty), across every layer a launch into that team resolves through.
pub(crate) fn startup_catalog(
    caller: &Caller,
    team: &AgentTeamRef,
    dir: Option<&str>,
    page: CatalogPage<'_>,
) -> Result<StartupCatalog, String> {
    let layers = content_layers(
        &caller.home,
        &caller.principal,
        &ManifestTarget {
            context_root: None,
            org: team.org.clone(),
            team: team.team.clone(),
            repo: None,
        },
    );
    catalog_in(&FsContentReader, &layers, dir, page)
}

/// [`startup_catalog`] over `layers` (in precedence order), read through
/// `reader`. Each layer's directory is listed once and nothing below it is
/// read: what a folder holds is listed only when the user opens it. A path in
/// several layers is the first layer's. What no startup path can name (a
/// reserved directory, a glob character, projection cruft) and anything that
/// is neither a file nor a folder (a symlink, a socket) is left out, as is a
/// folder reached through a symlink anywhere along `dir`. Only the page's
/// [`CATALOG_LIMIT`] names (by name, after `page.filter`) are statted, so a
/// huge folder costs one listing per layer, not one stat per entry.
pub(crate) fn catalog_in<R: Clone + Into<crate::context_repo::inheritance::ContentRoot>>(
    reader: &dyn ContentReader,
    layers: &[(ContentLayer, R)],
    dir: Option<&str>,
    page: CatalogPage<'_>,
) -> Result<StartupCatalog, String> {
    let layers: Vec<_> = layers
        .iter()
        .map(|(id, root)| (id.clone(), root.clone().into()))
        .collect();
    let filter = page
        .filter
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_lowercase);
    let dir = match dir.map(str::trim).filter(|d| !d.is_empty() && *d != "/") {
        Some(raw) => Some(StartupPath::parse(raw).map_err(|e| e.to_string())?),
        None => None,
    };
    // Every candidate name, first layer first, with the folder it was listed in.
    let mut names: BTreeMap<
        String,
        (
            String,
            &ContentLayer,
            &crate::context_repo::inheritance::ContentRoot,
        ),
    > = BTreeMap::new();
    for (layer, root) in &layers {
        let Some(base) = real_dir(reader, root, dir.as_ref()) else {
            continue;
        };
        let listed = match root.list(reader, &base) {
            Ok(listed) => listed,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(target: "agents", "startup catalog: cannot list {}: {e}", base);
                }
                continue;
            }
        };
        for name in listed {
            let Some(name) = name.to_str() else { continue };
            let rel = match &dir {
                Some(dir) => format!("{dir}/{name}"),
                None => name.to_string(),
            };
            if names.contains_key(&rel) || not_carried(&rel) {
                continue;
            }
            if filter
                .as_deref()
                .is_some_and(|f| !name.to_lowercase().contains(f))
            {
                continue;
            }
            if StartupPath::parse(&rel).map_or(true, |p| p.as_str() != rel) {
                continue;
            }
            names.insert(rel, (name.to_string(), layer, root));
        }
    }
    let total = names.len();
    let mut ordered: Vec<(
        String,
        (
            String,
            &ContentLayer,
            &crate::context_repo::inheritance::ContentRoot,
        ),
    )> = names.into_iter().collect();
    ordered.sort_by(|(a, _), (b, _)| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    let end = page.offset.saturating_add(CATALOG_LIMIT).min(total);
    let next_offset = (end < total).then_some(end);
    let mut entries: Vec<StartupCatalogEntry> = ordered
        .into_iter()
        .skip(page.offset)
        .take(CATALOG_LIMIT)
        .filter_map(|(path, (_name, layer, base))| {
            let is_dir = match base.node(reader, &path).map(|n| n.kind) {
                Ok(NodeKind::Dir) => true,
                Ok(NodeKind::File) => false,
                _ => return None,
            };
            Some(StartupCatalogEntry {
                path,
                is_dir,
                layer: layer.clone(),
            })
        })
        .collect();
    // Folders first; the name order within each group is kept.
    entries.sort_by_key(|e| !e.is_dir);
    Ok(StartupCatalog {
        dir: dir.map(|d| d.to_string()).unwrap_or_default(),
        entries,
        total,
        truncated: next_offset.is_some(),
        next_offset,
    })
}

/// `root` joined with `dir`, when every step along it is a real directory of
/// this layer (none of them a symlink).
fn real_dir(
    reader: &dyn ContentReader,
    root: &crate::context_repo::inheritance::ContentRoot,
    dir: Option<&StartupPath>,
) -> Option<String> {
    let mut at = String::new();
    for segment in dir.map(|d| d.as_str().split('/')).into_iter().flatten() {
        if !at.is_empty() {
            at.push('/');
        }
        at.push_str(segment);
        if !root
            .node(reader, &at)
            .is_ok_and(|n| n.kind == NodeKind::Dir)
        {
            return None;
        }
    }
    Some(at)
}

/// The open "Describe it to an agent" asks for: the visible team-workspace
/// launch agents use, primed with the authoring request.
pub(crate) fn authoring_request() -> OpenRequest {
    OpenRequest {
        target: "team-workspace".to_string(),
        name: Some(AUTHORING_AGENT_NAME.to_string()),
        initial_prompt: Some(AUTHORING_PROMPT.to_string()),
        mode: Some("headed".to_string()),
        ..OpenRequest::default()
    }
}

/// Where a form's agent lives (an edit: its key's space; a create: its scope),
/// and the id an edit keeps.
async fn form_placement(app: &AppHandle, form: &AgentForm) -> Result<Placement, String> {
    if let Some(key) = form.key.as_deref() {
        let key = parse_key(key)?;
        return Ok(if key.is_personal() {
            // An edit stays in the org the agent is filed under.
            let identity = super::scratch_identity::resolve_scratch_identity(app).await?;
            Placement::Personal(identity.owner_in(key.org))
        } else {
            Placement::Team {
                org: key.org,
                team: key.team,
            }
        });
    }
    match form.scope.as_deref().map(str::trim).unwrap_or("personal") {
        "personal" => Ok(Placement::Personal(personal_owner(app).await?)),
        "team" => match (form.org.clone(), form.team.clone()) {
            (Some(org), Some(team)) => Ok(Placement::Team { org, team }),
            _ => Err("a team agent needs its org and team".to_string()),
        },
        other => Err(format!("scope {other:?} must be personal or team")),
    }
}

/// Every agent the caller can see, with its account line, the routines using
/// it and how many live sessions run it. Archived agents only with
/// `include_archived`.
#[tauri::command]
pub async fn agents_list(
    app: AppHandle,
    include_archived: Option<bool>,
) -> Result<Vec<AgentRow>, String> {
    let caller = caller(&app).await?;
    await_blocking("agents_list", move || {
        crate::live_hub::agent_definitions::list(&app, &caller, include_archived.unwrap_or(false))
    })
    .await
}

/// One agent: its definition, revisions, and what a launch of it would run
/// for this user (account preview included).
#[tauri::command]
pub async fn agent_detail(app: AppHandle, key: String) -> Result<AgentDetailView, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_detail", move || {
        crate::live_hub::agent_definitions::detail(&app, &caller, &key).map_err(|e| e.to_string())
    })
    .await
}

/// Archive an agent: hidden from lists and pickers, still resolving for what
/// uses it.
#[tauri::command]
pub async fn agent_archive(
    app: AppHandle,
    key: String,
    archive_routines: Option<bool>,
) -> Result<AgentWriteReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("agent_archive", move || {
        archive(&caller, &key, &author, archive_routines.unwrap_or(false))
    })
    .await
}

/// Unarchive the routines an agent's archive archived, after its unarchive
/// offered them.
#[tauri::command]
pub async fn agent_restore_routines(
    app: AppHandle,
    key: String,
) -> Result<Vec<OwnedRoutine>, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("agent_restore_routines", move || {
        restore_routines(&caller, &key, &author)
    })
    .await
}

/// Bring an archived agent back, against the revision the tab last read.
#[tauri::command]
pub async fn agent_unarchive(
    app: AppHandle,
    key: String,
    base_revision: u32,
) -> Result<AgentWriteReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("agent_unarchive", move || {
        unarchive(&caller, &key, base_revision, &author)
    })
    .await
}

/// Delete an agent for good, unless routines still use it.
#[tauri::command]
pub async fn agent_delete(
    app: AppHandle,
    key: String,
    base_revision: u32,
) -> Result<AgentDeleteReply, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_delete", move || delete(&caller, &key, base_revision)).await
}

/// Copy any visible agent into the caller's personal space, or into a team the caller
/// can write to when a `team` is given. `team` absent (default) stays personal — the
/// Agents tab's `Use template` picker calls this with no team for a personal clone and
/// with the team's org+name for a team-scoped clone.
#[tauri::command]
pub async fn agent_copy_to_mine(
    app: AppHandle,
    key: String,
    team: Option<AgentTeamRef>,
) -> Result<AgentWriteReply, String> {
    let caller = caller(&app).await?;
    let placement = match team {
        None => Placement::Personal(personal_owner(&app).await?),
        Some(AgentTeamRef { org, team }) => Placement::Team { org, team },
    };
    let author = author(&app, &caller).await;
    await_blocking("agent_copy_to_mine", move || {
        copy_to_mine(&caller, &key, placement, &author)
    })
    .await
}

/// Run an agent with one of this user's accounts, and/or give or withdraw this
/// user's raw-terminal consent for it. Only this user's machine-local binding
/// changes, never the shared definition.
#[tauri::command]
pub async fn agent_binding_set(
    app: AppHandle,
    key: String,
    account_id: Option<String>,
    capture_raw_terminal: Option<bool>,
) -> Result<AgentBindingView, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_binding_set", move || {
        binding_set(&caller, &key, account_id.as_deref(), capture_raw_terminal)
    })
    .await
}

/// Go back to this user's default account for the agent's runtime.
#[tauri::command]
pub async fn agent_binding_clear(app: AppHandle, key: String) -> Result<bool, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_binding_clear", move || binding_clear(&caller, &key)).await
}

/// Create an agent from the editor's form (no `key`), or replace the agent
/// `key` names with it, against `base_revision`.
#[tauri::command]
pub async fn agent_save(
    app: AppHandle,
    form: AgentForm,
    base_revision: Option<u32>,
) -> Result<AgentSaveReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    let target = match form.key.clone() {
        Some(key) => AgentFormTarget::Update {
            key,
            base_revision: base_revision
                .ok_or_else(|| "an edit needs the revision it was made against".to_string())?,
        },
        None => AgentFormTarget::Create(form_placement(&app, &form).await?),
    };
    await_blocking("agent_save", move || {
        save(&caller, target, form.draft, &author)
    })
    .await
}

/// What the editor's unsaved form would resolve to, against `team` when
/// given. Writes nothing.
#[tauri::command]
pub async fn agent_preview(
    app: AppHandle,
    form: AgentForm,
    team: Option<AgentTeamRef>,
    cwd: Option<String>,
) -> Result<AgentPreviewReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    let placement = form_placement(&app, &form).await?;
    let mut draft = form.draft;
    if let Some(key) = form.key.as_deref() {
        draft.id = Some(parse_key(key)?.id);
    }
    let mut reply = await_blocking("agent_preview", move || {
        preview_at(&caller, placement, draft, team, &author, cwd)
    })
    .await?;
    if let AgentPreviewReply::Resolved { resolved, .. } = &mut reply {
        super::deps::note_cli_version(&app, resolved);
    }
    Ok(reply)
}

/// Make an older revision of an agent current again, as a new revision.
#[tauri::command]
pub async fn agent_restore_revision(
    app: AppHandle,
    key: String,
    revision: u32,
    base_revision: u32,
) -> Result<AgentSaveReply, String> {
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("agent_restore_revision", move || {
        restore_revision(&caller, &key, revision, base_revision, &author)
    })
    .await
}

/// SW-XS-15: the left nav's "Context for ‹Agent› · in ‹Repo›" overlay: the
/// agent's want set for `repo`. Writes nothing.
#[tauri::command]
pub async fn agent_context_preview(
    app: AppHandle,
    key: String,
    repo: Option<String>,
) -> Result<AgentContextView, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_context_preview", move || {
        context_view(&caller, &key, repo.as_deref())
    })
    .await
}

/// SW-XS-16: one `add_entries` entry as the editor sends it. Strict: any key other than
/// these is refused by name. `keep` carries keys a newer build stored on an entry, so
/// "Undo" of a remove puts the entry back exactly as it was.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEntryIpc {
    pub path: String,
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub keep: std::collections::BTreeMap<String, serde_yaml::Value>,
}

impl ContextEntryIpc {
    /// The entry, or why it is refused: `keep` carries only keys the entry has beside
    /// `path`/`dest`, so either one inside it is refused by name rather than letting
    /// the flattened write emit it twice.
    pub(crate) fn into_entry(self) -> Result<ContextEntry, String> {
        if let Some(key) = ["path", "dest"]
            .into_iter()
            .find(|k| self.keep.contains_key(*k))
        {
            return Err(format!(
                "context entry {:?}: `{key}` belongs beside `keep`, not inside it",
                self.path
            ));
        }
        Ok(ContextEntry {
            path: self.path,
            dest: self.dest,
            extra: self.keep,
        })
    }
}

/// SW-XS-15: "Add to / Remove from ‹Agent›'s context" (and "… for ‹Repo›
/// only") from the left nav, as one operation applied on the server against
/// `base_revision`. SW-XS-16 (the agent editor's Context section): an `add`
/// may carry a `dest`; `customize`, `add_entries` (with `entries`) and
/// `drop_remove` (with `repo`) take no or another payload, so `path` is
/// optional.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_context_update(
    app: AppHandle,
    key: String,
    base_revision: u32,
    op: ContextOp,
    path: Option<String>,
    dest: Option<String>,
    entries: Option<Vec<ContextEntryIpc>>,
    accepts: Option<String>,
    repo: Option<String>,
    view_repo: Option<String>,
) -> Result<AgentContextEditReply, String> {
    let entries: Vec<ContextEntry> = entries
        .unwrap_or_default()
        .into_iter()
        .map(ContextEntryIpc::into_entry)
        .collect::<Result<_, _>>()?;
    let caller = caller(&app).await?;
    let author = author(&app, &caller).await;
    await_blocking("agent_context_update", move || {
        context_edit(
            &caller,
            ContextEditRequest {
                key: &key,
                base_revision,
                op,
                path: path.as_deref().unwrap_or_default(),
                dest: dest.as_deref(),
                entries: &entries,
                accepts: accepts.as_deref(),
                repo: repo.as_deref(),
                view_repo: view_repo.as_deref(),
            },
            &author,
        )
    })
    .await
}

/// One page of one directory level of `team`'s content under `dir`, for the
/// startup picker: from `offset`, keeping names that contain `filter`. Never
/// recurses.
#[tauri::command]
pub async fn agent_startup_catalog(
    app: AppHandle,
    team: AgentTeamRef,
    dir: Option<String>,
    offset: Option<usize>,
    filter: Option<String>,
) -> Result<StartupCatalog, String> {
    let caller = caller(&app).await?;
    await_blocking("agent_startup_catalog", move || {
        startup_catalog(
            &caller,
            &team,
            dir.as_deref(),
            CatalogPage {
                offset: offset.unwrap_or_default(),
                filter: filter.as_deref(),
            },
        )
    })
    .await
}

/// Open the authoring agent in the UI-active team's workspace through `port`.
pub(crate) async fn open_authoring_agent(
    port: &dyn OpenWorkspacePort,
) -> Result<AgentAuthoring, String> {
    let (active, host) = port.targeted().await?;
    let result = open_agent(host.as_ref(), &active, &authoring_request()).await?;
    Ok(AgentAuthoring {
        opened: matches!(result.status, OpenStatus::Opened | OpenStatus::AlreadyLive),
        message: result.message,
    })
}

/// Open a visible agent in the active team's workspace, asked to help the user
/// create an agent, so the user can describe one in words.
#[tauri::command]
pub async fn agent_open_authoring_agent(app: AppHandle) -> Result<AgentAuthoring, String> {
    open_authoring_agent(&AppOpenWorkspacePort { app }).await
}

/// The agent CLIs the terminal dot floor recognizes, with their attention channel.
#[tauri::command]
pub async fn list_agent_signals() -> Result<Vec<crate::agents::adapter::AgentSignalInfo>, String> {
    Ok(crate::agents::adapter::builtin_agent_signals())
}

#[cfg(test)]
mod editor_tests;
#[cfg(test)]
mod tests;
