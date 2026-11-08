// SPDX-License-Identifier: MIT
use crate::dashboard_stream::frame::{
    FrameCallerKind, FrameGrant, FrameMessage, FramePolicy, FrameRecord, FrameStore, FrameTeam,
};
use serde::Deserialize;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintFrameArgs {
    pub document_path: String,
    pub content_root: String,
    pub asset_folder: Option<String>,
    pub slot: String,
    pub message: Option<FrameMessage>,
    pub parent_frame: Option<String>,
    pub builtin_team: Option<FrameTeam>,
    #[serde(default)]
    pub policy: FramePolicy,
}

#[tauri::command]
pub async fn mint_frame<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    mut args: MintFrameArgs,
) -> Result<FrameRecord, String> {
    let port = app
        .try_state::<crate::http::HttpApiPort>()
        .ok_or("HTTP server unavailable")?
        .0;
    let (records_home, local_principal) =
        crate::commands::route_to_context::resolve_ambient(&app).await?;
    let mut principal = "human".to_string();
    let team;
    let mut pane = None;
    let mut message_html = None;
    let mut caller_scope = "private";
    if let Some(message) = &args.message {
        caller_scope = "message";
        // Resolve the stored message once; its store and sender are the identity
        // used later. The HTTP URL cannot select a different team or sender.
        let (content_root, stored) =
            crate::http::routes::viewer::load_frame_message(&app, message).await?;
        if args.parent_frame.is_some() && (stored.to != "human" || stored.from == "human") {
            return Err("denied: message is not live human mail".into());
        }
        args.content_root = content_root;
        message_html = Some(stored.text);
        principal = stored.from;
        team = Some(FrameTeam {
            org: message.org.clone(),
            team: message.team.clone(),
        });
        args.document_path = format!(
            "inbox-message://{}/{}/{}",
            message.org, message.team, stored.id
        );
        args.asset_folder = Some(args.content_root.clone());
    } else if let Some((entry, binding)) =
        builtin_frame_context(&records_home, &local_principal.into(), &mut args)?
    {
        caller_scope = "built-in";
        pane = Some(entry);
        team = Some(binding);
    } else {
        let path = PathBuf::from(&args.document_path);
        crate::commands::files::validate_context_path(&path, &app)?;
        // Comparisons can show a committed file that has since been deleted.
        // Validate its existing ancestor instead; live documents must exist.
        let target = frame_document_target(&path, args.slot == "baseline")?;
        crate::commands::files::validate_context_path(&target, &app)?;
        crate::commands::files::validate_context_path(&PathBuf::from(&args.content_root), &app)?;
        let roots = crate::context_query::known_roots(&app);
        let active = super::pane_bridge::doc_context(&roots, &args.content_root);
        // A built-in lives outside every space: it belongs to the org it is listed in.
        let builtin = active
            .as_ref()
            .and_then(|l| l.org.as_deref())
            .and_then(|org| crate::panes::builtin_at(&path, org));
        let active_team = active
            .and_then(|l| l.org.zip(l.team))
            .map(|(org, team)| FrameTeam { org, team });
        let location = super::pane_bridge::frame_source_context(
            &roots,
            &args.document_path,
            &args.content_root,
        );
        if builtin.is_some() {
            caller_scope = "built-in";
            pane = builtin;
        } else if let Some(location) = &location {
            caller_scope = if location.team.is_some() {
                "team"
            } else {
                "private"
            };
            if let (Some(org), Some(dir)) = (
                &location.org,
                path.ancestors().find(|dir| {
                    dir.file_name()
                        .is_some_and(|n| n == crate::context_repo::PANES_DIR)
                }),
            ) {
                let scope = if location.team.is_some() {
                    crate::panes::PaneScope::Team
                } else {
                    crate::panes::PaneScope::Private
                };
                pane = crate::panes::list_layer(dir, scope, org, location.team.as_deref())
                    .into_iter()
                    .find(|p| PathBuf::from(&p.path) == path);
                if let Some(pane) = &pane {
                    if pane.status == crate::panes::PaneStatus::Rejected {
                        return Err(pane
                            .rejected_reason
                            .clone()
                            .unwrap_or_else(|| "Dashboard rejected".into()));
                    }
                }
            }
        }
        let source_team = if caller_scope == "built-in" {
            active_team.clone()
        } else {
            location
                .and_then(|l| l.org.zip(l.team))
                .map(|(org, team)| FrameTeam { org, team })
        };
        team = bound_team(&args.slot, source_team, active_team);
        args.asset_folder = Some(
            path.parent()
                .ok_or("Document has no folder")?
                .to_string_lossy()
                .into_owned(),
        );
    }
    inbox_network_policy(
        &mut args.policy,
        &args.slot,
        pane.as_ref().and_then(|p| p.surface.as_deref()),
    );
    let policy =
        crate::verbs::commands::read_policy(app.clone(), &local_principal.to_string()).await?;
    let dashboard = pane
        .as_ref()
        .map(|p| crate::panes::DashboardRef::key(&p.dashboard_ref))
        .unwrap_or_else(|| args.document_path.clone());
    let declared =
        declared_capabilities(pane.as_ref(), message_html.as_deref(), &args.document_path);
    let parent = args.parent_frame.clone();
    let frames = app.state::<std::sync::Arc<FrameStore>>();
    let mut record = mint_record(
        &app.state::<std::sync::Arc<FrameStore>>(),
        port,
        &policy,
        args,
        principal,
        dashboard,
        caller_scope,
        team,
        &declared,
    );
    let roots = crate::context_query::known_roots(&app);
    let location = super::pane_bridge::doc_context(&roots, &record.content_root);
    let org = record
        .grant
        .team
        .as_ref()
        .map(|t| t.org.clone())
        .or_else(|| location.and_then(|l| l.org));
    if let Some(org) = org {
        let binding = frame_records_binding(
            &records_home,
            &local_principal.into(),
            &org,
            record.grant.team.as_ref().map(|t| t.team.as_str()),
            caller_scope == "built-in",
        )?;
        let read_scope =
            crate::context_repo::caller_scope::for_page(&app, None, Some(&record.frame_id))?;
        record = frames.bind_records(&record.frame_id, binding.for_caller(read_scope))?;
    }
    if let Some(parent) = parent {
        match frames.link_child(&record.frame_id, &parent) {
            Ok(child) => Ok(child),
            Err(error) => {
                frames.release(&record.frame_id);
                Err(error)
            }
        }
    } else {
        Ok(record)
    }
}

/// Bind only the authenticated principal's bundled file; caller paths cannot supply its identity.
fn builtin_frame_context(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    args: &mut MintFrameArgs,
) -> Result<Option<(crate::panes::PaneEntry, FrameTeam)>, String> {
    let Some(binding) = args.builtin_team.clone() else {
        return Ok(None);
    };
    if args.parent_frame.is_some() {
        return Err("Built-in pane binding requires a slot host".into());
    }
    validate_builtin_team(home, principal, &binding)?;
    let listing = crate::panes::list_builtin_panes(home, principal, &binding.org, &binding.team)?;
    // The principal's own bundled pane, declared for this slot; the caller's path only selects it.
    let entry = listing
        .panes
        .into_iter()
        .find(|entry| {
            entry.path == args.document_path
                && entry
                    .declaration
                    .slots
                    .iter()
                    .any(|slot| *slot == args.slot)
        })
        .ok_or("Built-in pane does not belong to this principal or slot")?;
    args.content_root = listing.content_root;
    args.asset_folder = Some(entry.panes_dir.clone());
    Ok(Some((entry, binding)))
}

fn validate_builtin_team(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    binding: &FrameTeam,
) -> Result<(), String> {
    use crate::context_repo::{context_attachments, scratch_space};
    if scratch_space::is_scratch_team(&binding.team)
        && scratch_space::org_space(home, principal, &binding.org)
            .is_some_and(|space| space.root().is_dir())
    {
        return Ok(());
    }
    if let Ok(user_id) = principal.numeric_id() {
        if !matches!(
            super::route_to_context::resolve_local(
                home.to_path_buf(),
                user_id,
                binding.org.clone(),
                binding.team.clone(),
            ),
            super::route_to_context::ContextResolution::Unauthorized,
        ) {
            return Ok(());
        }
    }
    if let Some(attachment) = context_attachments::registered(home, principal, &binding.org)? {
        if context_attachments::available_root(&attachment).is_err()
            && crate::weft::weft_store_path(home, principal, &binding.org, &binding.team)?.is_file()
        {
            return Ok(());
        }
    }
    Err(format!(
        "unknown local team {}:{}",
        binding.org, binding.team
    ))
}

fn frame_records_binding(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    org: &str,
    team: Option<&str>,
    builtin: bool,
) -> Result<crate::records::binding::Binding, String> {
    match crate::records::binding::Binding::from_context(home, principal, org, team) {
        Ok(binding) => Ok(binding),
        Err(_) if builtin => {
            // Architecture: Inbox and private records do not require a mounted checkout.
            // Unavailable team collections remain unavailable; the grant keeps its team.
            let mut binding =
                crate::records::binding::Binding::from_context(home, principal, org, None)?;
            binding.team = team.map(str::to_owned);
            Ok(binding)
        }
        Err(error) => Err(error),
    }
}

fn mint_record(
    frames: &FrameStore,
    port: u16,
    policy: &crate::verbs::Policy,
    mut args: MintFrameArgs,
    principal: String,
    dashboard: String,
    source_scope: &str,
    team: Option<FrameTeam>,
    declared: &[String],
) -> FrameRecord {
    let effective_grant = mint_grant(
        policy,
        &mut args.policy,
        &args.slot,
        &dashboard,
        source_scope,
        declared,
    );
    let scope = grant_scope(args.policy.team_safe, source_scope);
    let kind = if args.slot == "message" || args.message.is_some() {
        FrameCallerKind::Message
    } else {
        match source_scope {
            "team" => FrameCallerKind::Team,
            "built-in" => FrameCallerKind::Builtin,
            _ => FrameCallerKind::Private,
        }
    };
    let grant = FrameGrant {
        frame_id: String::new(),
        principal,
        dashboard,
        slot: args.slot,
        team,
        scope: scope.into(),
    };
    let record = FrameStore::mint_with_grant(
        frames,
        port,
        grant,
        args.content_root,
        args.document_path,
        args.asset_folder.unwrap_or_default(),
        args.policy,
        args.message,
        effective_grant,
        kind,
    );
    frames.declare(&record.frame_id, declared);
    FrameRecord {
        declared: declared.to_vec(),
        ..record
    }
}

/// The grant's scope: `open` unless the saved policy is team-safe.
fn grant_scope(team_safe: bool, source_scope: &str) -> &'static str {
    if !team_safe {
        "open"
    } else if matches!(source_scope, "team" | "message") {
        "team"
    } else {
        "private"
    }
}

/// A dock frame follows the active team. On a team change the grant is computed fresh for
/// (principal, dashboard, dock, new team) from the saved policy, never unioned with the old
/// one, and the old token is revoked as the new one is minted. The frame keeps its origin.
#[tauri::command]
pub async fn rebind_frame_team(
    app: tauri::AppHandle,
    frame_id: String,
    token: String,
    org: String,
    team: String,
) -> Result<FrameRecord, String> {
    let (_, local_principal) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    let policy =
        crate::verbs::commands::read_policy(app.clone(), &local_principal.to_string()).await?;
    let frames = app.state::<std::sync::Arc<FrameStore>>();
    let record = rebind_record(&frames, &policy, &frame_id, &token, FrameTeam { org, team })?;
    if let Some(binding) = record.records_binding.clone() {
        let scope =
            crate::context_repo::caller_scope::for_page(&app, None, Some(&record.frame_id))?;
        return frames.bind_records(&record.frame_id, binding.for_caller(scope));
    }
    Ok(record)
}

fn rebind_record(
    frames: &FrameStore,
    policy: &crate::verbs::Policy,
    frame_id: &str,
    token: &str,
    active: FrameTeam,
) -> Result<FrameRecord, String> {
    let record = frames.by_id(frame_id).ok_or("denied: frame identity")?;
    if !matches!(record.grant.slot.as_str(), "dock" | "file-nav") {
        return Err("denied: this slot keeps its team".into());
    }
    let source_scope = match record.kind {
        FrameCallerKind::Team => "team",
        FrameCallerKind::Private => "private",
        FrameCallerKind::Builtin => "built-in",
        FrameCallerKind::Message => "message",
    };
    let mut frame_policy = record.policy.clone();
    let effective_grant = mint_grant(
        policy,
        &mut frame_policy,
        &record.grant.slot,
        &record.grant.dashboard,
        source_scope,
        &record.declared,
    );
    frames.rebind(
        frame_id,
        token,
        bound_team(&record.grant.slot, record.grant.team.clone(), Some(active)),
        grant_scope(frame_policy.team_safe, source_scope).into(),
        effective_grant,
        frame_policy,
    )
}

fn declared_capabilities(
    pane: Option<&crate::panes::PaneEntry>,
    message_html: Option<&str>,
    document_path: &str,
) -> Vec<String> {
    pane.map(|p| p.declaration.capabilities.clone())
        .unwrap_or_else(|| {
            let html = message_html.map(str::to_owned).unwrap_or_else(|| {
                crate::panes::read_head(&PathBuf::from(document_path)).unwrap_or_default()
            });
            crate::panes::DashboardDeclaration::from_html(&html).capabilities
        })
}

fn bound_team(
    slot: &str,
    source: Option<FrameTeam>,
    active: Option<FrameTeam>,
) -> Option<FrameTeam> {
    // The dock follows active context; fixed slots retain their source team.
    if matches!(slot, "dock" | "file-nav") {
        active
    } else {
        source
    }
}

fn mint_grant(
    policy: &crate::verbs::Policy,
    frame_policy: &mut FramePolicy,
    slot: &str,
    dashboard: &str,
    scope: &str,
    declared: &[String],
) -> Vec<String> {
    frame_policy.team_safe = policy.preset == crate::verbs::Preset::TeamSafe;
    policy.effective_grant(
        dashboard,
        slot,
        scope,
        declared,
        &crate::verbs::capabilities::full(),
    )
}

fn frame_document_target(path: &std::path::Path, baseline: bool) -> Result<PathBuf, String> {
    if baseline && !path.exists() {
        return path
            .ancestors()
            .find(|path| path.exists())
            .ok_or("Document has no existing ancestor")?
            .canonicalize()
            .map_err(|e| e.to_string());
    }
    let real = path.canonicalize().map_err(|e| e.to_string())?;
    if !real.is_file() {
        return Err("Document is not a file".into());
    }
    Ok(real)
}

#[tauri::command]
pub fn release_frame(app: tauri::AppHandle, frame_id: String) {
    crate::live_hub::agent_definitions::release(&app, &frame_id);
    FrameStore::release(&app.state::<std::sync::Arc<FrameStore>>(), &frame_id);
}

#[tauri::command]
pub async fn resolve_frame_grant(
    app: tauri::AppHandle,
    origin: String,
    token: String,
    message_ids: Option<Vec<i64>>,
) -> Result<FrameGrant, String> {
    let frames = app.state::<std::sync::Arc<FrameStore>>();
    let grant = FrameStore::resolve(&frames, &origin, &token).ok_or("denied: frame identity")?;
    if let Some(ids) = message_ids.filter(|_| grant.scope != "open" && grant.slot == "message") {
        let frame = frames.by_origin(&origin).ok_or("denied: frame identity")?;
        let message = frame.message.ok_or("denied: message thread")?;
        let (_, path) =
            crate::http::routes::viewer::frame_message_store_path(&app, &message).await?;
        super::async_helpers::await_blocking("frame_message_thread", move || {
            // RI-2-ALLOW: authoritative inbox ancestry is read off the async runtime.
            let store = crate::weft::WeftStore::open(path).map_err(|e| e.to_string())?;
            check_message_thread(&store, &message, &ids)
        })
        .await?;
    }
    Ok(grant)
}

fn check_message_thread(
    store: &crate::weft::WeftStore,
    message: &FrameMessage,
    ids: &[i64],
) -> Result<(), String> {
    let root = |mut id| -> Result<i64, String> {
        let mut seen = std::collections::HashSet::new();
        while seen.insert(id) {
            let row = crate::weft::inbox::inbox_get(store, id)
                .map_err(|e| e.to_string())?
                .ok_or("denied: message thread")?;
            if row.in_reply_to.is_none()
                || row
                    .in_reply_to_team
                    .is_some_and(|team| team.org != message.org || team.team != message.team)
            {
                return Ok(id);
            }
            id = row.in_reply_to.unwrap();
        }
        Err("denied: message thread".into())
    };
    let own = root(message.message_id)?;
    for id in ids {
        if root(*id)? != own {
            return Err("denied: message thread".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_frame_rejects_an_unknown_local_team() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let principal = crate::domain::Principal::from(42u64);
        let org = "acme/api";
        let root =
            crate::context_repo::context_attachments::tests::checkout(temp.path(), "volume", org);
        crate::context_repo::context_attachments::attach(&home, &principal, &root).unwrap();
        let descriptor = crate::panes::list_builtin_panes(&home, &principal, org, "default")
            .unwrap()
            .builtin_inbox
            .unwrap();
        for unavailable in [false, true] {
            if unavailable {
                std::fs::rename(&root, temp.path().join("unmounted-volume")).unwrap();
            }
            let mut args: MintFrameArgs = serde_json::from_value(serde_json::json!({
                "documentPath": descriptor.path,
                "contentRoot": descriptor.panes_dir, "slot": "right-pane",
                "builtinTeam": { "org": org, "team": "unknown-team" },
            }))
            .unwrap();
            assert!(builtin_frame_context(&home, &principal, &mut args)
                .unwrap_err()
                .contains("unknown local team"));
        }
    }

    #[test]
    fn builtin_frame_survives_an_unavailable_attached_checkout() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let principal = crate::domain::Principal::from(42u64);
        let org = "acme/api";
        let root =
            crate::context_repo::context_attachments::tests::checkout(temp.path(), "volume", org);
        crate::context_repo::context_attachments::attach(&home, &principal, &root).unwrap();
        std::fs::create_dir_all(root.join("default/content")).unwrap();
        let store = super::super::agent_inbox::team_store_path(
            super::super::route_to_context::resolve_local(
                home.clone(),
                42,
                org.into(),
                "default".into(),
            ),
            &super::super::dashboard_launch::LaunchTeam {
                org: org.into(),
                team: "default".into(),
            },
        )
        .unwrap();
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        // RI-2-ALLOW: synchronous unit-test fixture, outside Tauri command dispatch.
        drop(crate::weft::WeftStore::open(store).unwrap());
        std::fs::rename(&root, temp.path().join("unmounted-volume")).unwrap();
        assert!(crate::context_repo::path_to_org_root(&home, &principal, org).is_err());
        assert!(crate::panes::list_panes(&home, &principal, org, "default").is_err());
        let listing = crate::panes::list_builtin_panes(&home, &principal, org, "default").unwrap();
        let mut args: MintFrameArgs = serde_json::from_value(serde_json::json!({
            "documentPath": listing.builtin_inbox.unwrap().path,
            "contentRoot": root.join("default/content"), "slot": "right-pane",
            "builtinTeam": { "org": org, "team": "default" },
        }))
        .unwrap();
        let (entry, binding) = builtin_frame_context(&home, &principal, &mut args)
            .unwrap()
            .unwrap();
        assert!(PathBuf::from(&args.content_root).is_dir());
        assert!(!PathBuf::from(&args.content_root).starts_with(&root));
        let frames = FrameStore::default();
        let frame = mint_record(
            &frames,
            9321,
            &crate::verbs::Policy {
                preset: crate::verbs::Preset::TeamSafe,
                ..Default::default()
            },
            args,
            "human".into(),
            entry.dashboard_ref.key(),
            "built-in",
            Some(binding),
            &entry.declaration.capabilities,
        );
        let records = frame_records_binding(&home, &principal, org, Some("default"), true).unwrap();
        assert_eq!(records.org, org);
        assert_eq!(records.team.as_deref(), Some("default"));
        assert!(records.team_root.is_none());
        let frame = frames.bind_records(&frame.frame_id, records).unwrap();
        assert_eq!(frame.kind, FrameCallerKind::Builtin);
        assert_eq!(
            frame.grant.team,
            Some(FrameTeam {
                org: org.into(),
                team: "default".into()
            })
        );
        assert!(frame
            .effective_grant
            .contains(&"inbox:read:team".to_owned()));
    }

    #[tokio::test]
    async fn tab_only_builtin_is_refused_by_mint_frame_but_slot_builtins_still_mint() {
        let fixture = crate::context_repo::caller_scope::tests::AppFixture::new();
        fixture.app.manage(crate::http::HttpApiPort(9400));
        let listing = crate::panes::list_builtin_panes(&fixture.fixture.home, &crate::domain::Principal::from(1u64), "acme/ctx", "child").unwrap();
        let args = |id: &str, slot: &str| serde_json::from_value(serde_json::json!({
            "documentPath": listing.panes.iter().find(|pane| pane.id == id).unwrap().path,
            "contentRoot": listing.content_root, "slot": slot,
            "builtinTeam": {"org":"acme/ctx", "team":"child"},
        })).unwrap();
        for slot in ["right-pane", "dock", "file-nav"] {
            let error = mint_frame(fixture.app.handle().clone(), args("story-trail", slot)).await.unwrap_err();
            assert!(error.contains("Built-in pane does not belong to this principal or slot"), "{error}");
        }
        for (id, slot) in [("inbox", "right-pane"), ("agent-list", "dock"), ("files-tree", "file-nav"), ("files-tree", "right-pane")] {
            let frame = mint_frame(fixture.app.handle().clone(), args(id, slot)).await.unwrap();
            assert_eq!(frame.kind, FrameCallerKind::Builtin);
            assert_eq!(frame.grant.slot, slot);
        }
    }

    #[test]
    fn every_builtin_pane_mints_only_in_its_declared_slot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let principal = crate::domain::Principal::from(42u64);
        let org = "acme/api";
        let root =
            crate::context_repo::context_attachments::tests::checkout(temp.path(), "volume", org);
        crate::context_repo::context_attachments::attach(&home, &principal, &root).unwrap();
        std::fs::create_dir_all(root.join("default/content")).unwrap();
        let store = super::super::agent_inbox::team_store_path(
            super::super::route_to_context::resolve_local(
                home.clone(),
                42,
                org.into(),
                "default".into(),
            ),
            &super::super::dashboard_launch::LaunchTeam {
                org: org.into(),
                team: "default".into(),
            },
        )
        .unwrap();
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        // RI-2-ALLOW: synchronous unit-test fixture, outside Tauri command dispatch.
        drop(crate::weft::WeftStore::open(store).unwrap());
        let listing = crate::panes::list_builtin_panes(&home, &principal, org, "default").unwrap();
        let path = |id: &str| {
            listing
                .panes
                .iter()
                .find(|pane| pane.id == id)
                .unwrap_or_else(|| panic!("built-in {id} listed"))
                .path
                .clone()
        };
        let args = |path: &str, slot: &str, parent: Option<&str>| -> MintFrameArgs {
            let mut value = serde_json::json!({
                "documentPath": path,
                "contentRoot": root.join("default/content"), "slot": slot,
                "builtinTeam": { "org": org, "team": "default" },
            });
            if let Some(parent) = parent {
                value["parentFrame"] = serde_json::json!(parent);
            }
            serde_json::from_value(value).unwrap()
        };
        let frames = FrameStore::default();
        let policy = crate::verbs::Policy::default();
        // Every built-in mints in each slot it declares, through the real mint record path.
        for (id, slot) in [
            ("inbox", "right-pane"),
            ("agent-list", "dock"),
            ("files-tree", "file-nav"),
            ("files-tree", "right-pane"),
            ("notes", "right-pane"),
        ] {
            let mut minted = args(&path(id), slot, None);
            let (entry, binding) = builtin_frame_context(&home, &principal, &mut minted)
                .unwrap_or_else(|err| panic!("{id} in {slot}: {err}"))
                .unwrap();
            assert_eq!(entry.id, id);
            assert!(PathBuf::from(&minted.content_root).is_dir());
            let frame = mint_record(
                &frames,
                9400,
                &policy,
                minted,
                "human".into(),
                entry.dashboard_ref.key(),
                "built-in",
                Some(binding),
                &entry.declaration.capabilities,
            );
            assert_eq!(frame.kind, FrameCallerKind::Builtin, "{id} in {slot}");
            if id == "notes" {
                for grant in ["records:read:own", "records:write:own"] {
                    assert!(
                        frame.effective_grant.contains(&grant.to_owned()),
                        "notes keeps {grant}"
                    );
                }
            }
        }
        // A slot the pane doesn't declare, a nested frame, and a non-built-in path are refused.
        for (path, slot, parent) in [
            (path("agent-list"), "right-pane", None),
            (path("files-tree"), "dock", None),
            (path("inbox"), "file-nav", None),
            (path("files-tree"), "file-nav", Some("parent-frame")),
            (
                root.join("default/content/x.html")
                    .to_string_lossy()
                    .into_owned(),
                "file-nav",
                None,
            ),
        ] {
            assert!(
                builtin_frame_context(&home, &principal, &mut args(&path, slot, parent)).is_err(),
                "{path} in {slot} (parent {parent:?}) must be refused"
            );
        }
    }

    #[test]
    fn builtin_inbox_mark_unread_and_existing_verbs_are_granted_under_both_presets() {
        use crate::verbs::{capabilities, Decision, Dispatcher, Policy, Preset};
        use serde_json::json;

        let declared = declared_capabilities(
            None,
            Some(include_str!(
                "../../../src/features/shell/right-pane/__fixtures__/inbox-pane.html"
            )),
            "",
        );
        for preset in [Preset::TeamSafe, Preset::Open] {
            let policy = Policy {
                preset,
                ..Default::default()
            };
            let frames = FrameStore::default();
            let frame = mint_record(
                &frames,
                9321,
                &policy,
                serde_json::from_value(json!({
                    "documentPath": "/h/users/42/builtin-panes/panes/inbox.html",
                    "contentRoot": "/ctx", "slot": "right-pane"
                }))
                .unwrap(),
                "human".into(),
                "dashboard:built-in:acme/api/inbox".into(),
                "built-in",
                Some(FrameTeam {
                    org: "acme/api".into(),
                    team: "backend".into(),
                }),
                &declared,
            );
            assert_eq!(frame.kind, FrameCallerKind::Builtin);
            if preset == Preset::Open {
                assert_eq!(frame.effective_grant, capabilities::full());
            }
            let mut dispatcher = Dispatcher::default();
            for verb in [
                "context",
                "prefs",
                "setPrefs",
                "inbox",
                "inboxRollup",
                "roles",
                "humanStatus",
                "sessions",
                "subscribe",
                "unsubscribe",
                "markRead",
                "markUnread",
                "archive",
                "unarchive",
                "embedMessage",
                "openMessage",
                "openFile",
                "gotoAgent",
                "setHumanStatus",
                "sendAsHuman",
                "replyAsHuman",
                "commands.run",
            ] {
                let spec = crate::verbs::builtin_registry().get(verb).unwrap();
                assert!(
                    frame.effective_grant.contains(&spec.grant),
                    "{preset:?}: {verb}"
                );
                assert!(
                    matches!(
                        dispatcher.dispatch_frame(
                            &frame,
                            verb,
                            if verb == "commands.run" {
                                json!({"id":"inbox.new-message"})
                            } else {
                                json!({"topic":"sessions"})
                            },
                            &policy,
                            0
                        ),
                        Decision::Admit { .. }
                    ),
                    "{preset:?}: {verb} must run without a prompt"
                );
            }
        }
    }

    #[test]
    fn message_html_declarations_reach_native_admission() {
        let declared = declared_capabilities(
            None,
            Some(r#"<meta name="codesense:capabilities" content="sessions:read context:read">"#),
            "inbox-message://acme/a/1",
        );
        let policy = crate::verbs::Policy {
            preset: crate::verbs::Preset::TeamSafe,
            ..Default::default()
        };
        let args: MintFrameArgs = serde_json::from_value(serde_json::json!({
            "documentPath":"inbox-message://acme/a/1", "contentRoot":"/ctx", "slot":"message"
        }))
        .unwrap();
        let store = FrameStore::default();
        let record = mint_record(
            &store,
            9321,
            &policy,
            args,
            "agent".into(),
            "message".into(),
            "message",
            Some(FrameTeam {
                org: "acme".into(),
                team: "a".into(),
            }),
            &declared,
        );
        let stored = store.by_id(&record.frame_id).unwrap();
        assert!(stored.effective_grant.contains(&"sessions:read".into()));
        assert!(!stored.effective_grant.contains(&"sessions:send".into()));
        assert!(matches!(
            crate::verbs::Dispatcher::default().dispatch_frame(
                &stored,
                "context",
                serde_json::json!({}),
                &policy,
                0,
            ),
            crate::verbs::Decision::Admit { .. }
        ));
    }

    #[test]
    fn file_nav_follows_context_and_keeps_its_full_open_grant() {
        let store = FrameStore::default();
        let args: MintFrameArgs = serde_json::from_value(serde_json::json!({"documentPath":"/a/panes/tree.html","contentRoot":"/a/content","slot":"file-nav"})).unwrap();
        let policy = crate::verbs::Policy::default();
        let first = mint_record(
            &store,
            9321,
            &policy,
            args,
            "human".into(),
            "tree".into(),
            "team",
            Some(FrameTeam {
                org: "acme".into(),
                team: "a".into(),
            }),
            &crate::verbs::capabilities::full(),
        );
        let next = rebind_record(
            &store,
            &policy,
            &first.frame_id,
            &first.token,
            FrameTeam {
                org: "acme".into(),
                team: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(first.frame_id, next.frame_id);
        assert_eq!(next.grant.team.as_ref().unwrap().team, "b");
        assert!(next.effective_grant.iter().any(|c| c == "files:write"));
        assert_eq!(
            crate::live_hub::view_key::resolve(&next.grant).as_deref(),
            Some("team:acme/b")
        );
        assert!(store.resolve(&first.origin, &first.token).is_none());
    }

    #[test]
    fn dock_rebinds_to_the_active_team_without_widening_the_source_ceiling() {
        let source = FrameTeam {
            org: "acme".into(),
            team: "a".into(),
        };
        let active = FrameTeam {
            org: "acme".into(),
            team: "b".into(),
        };
        assert_eq!(
            bound_team("right-pane", Some(source.clone()), Some(active.clone())),
            Some(source.clone())
        );
        let policy = crate::verbs::Policy {
            preset: crate::verbs::Preset::TeamSafe,
            ..Default::default()
        };
        let store = FrameStore::default();
        let pane_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pane_dir.path().join("lib")).unwrap();
        let document_path = pane_dir.path().join("board.html");
        let asset_path = pane_dir.path().join("lib/chart.js");
        let document_path_text = document_path.to_string_lossy().into_owned();
        let asset_folder_text = pane_dir.path().to_string_lossy().into_owned();
        std::fs::write(&document_path, "<title>Board</title>").unwrap();
        std::fs::write(&asset_path, "export const chart = true;").unwrap();
        let make = |active: FrameTeam, scope: &str| {
            let args: MintFrameArgs = serde_json::from_value(serde_json::json!({
                "documentPath":document_path_text, "assetFolder":asset_folder_text,
                "contentRoot":format!("/{}/content", active.team), "slot":"dock",
                "policy":{"ownFolderOnly":true}
            }))
            .unwrap();
            mint_record(
                &store,
                9321,
                &policy,
                args,
                "human".into(),
                "board".into(),
                scope,
                bound_team("dock", Some(source.clone()), Some(active)),
                &crate::verbs::capabilities::full(),
            )
        };
        let first = make(source.clone(), "team");
        let second = make(active.clone(), "team");
        store.release(&first.frame_id);
        assert!(store.resolve(&first.origin, &first.token).is_none());
        let rebound = store.by_id(&second.frame_id).unwrap();
        assert_ne!(first.token, rebound.token);
        assert_eq!(rebound.grant.team, Some(active.clone()));
        assert_eq!(rebound.grant.scope, "team");
        assert_eq!(rebound.kind, FrameCallerKind::Team);
        assert_eq!(rebound.document_path, document_path_text);
        assert_eq!(rebound.asset_folder, asset_folder_text);
        assert_eq!(rebound.content_root, "/b/content");
        assert!(rebound.allows_path(&document_path));
        assert!(rebound.allows_path(&asset_path));
        assert_eq!(
            crate::live_hub::view_key::resolve(&rebound.grant).as_deref(),
            Some("team:acme/b")
        );
        assert!(rebound.effective_grant.contains(&"inbox:read:team".into()));
        assert!(!rebound
            .effective_grant
            .contains(&"inbox:read:all-teams".into()));
        let on_a = rebind_record(
            &store,
            &policy,
            &second.frame_id,
            &second.token,
            source.clone(),
        )
        .unwrap();
        assert_eq!(on_a.grant.team, Some(source.clone()));
        assert_eq!(
            crate::live_hub::view_key::resolve(&on_a.grant).as_deref(),
            Some("team:acme/a")
        );
        assert_eq!(
            on_a.content_root, "/b/content",
            "a team rebind keeps the root from the current mint"
        );
        assert!(on_a.allows_path(&document_path));
        assert!(on_a.allows_path(&asset_path));
        assert!(store.resolve(&second.origin, &second.token).is_none());
        let on_b = rebind_record(
            &store,
            &policy,
            &second.frame_id,
            &on_a.token,
            active.clone(),
        )
        .unwrap();
        assert_eq!(on_b.grant.team, Some(active.clone()));
        assert_eq!(on_b.document_path, document_path_text);
        assert_eq!(on_b.asset_folder, asset_folder_text);
        assert_eq!(on_b.content_root, "/b/content");
        assert!(on_b.allows_path(&document_path));
        assert!(on_b.allows_path(&asset_path));
        assert_eq!(
            crate::live_hub::view_key::resolve(&on_b.grant).as_deref(),
            Some("team:acme/b")
        );
        let private = make(active, "private");
        assert_eq!(private.grant.scope, "private");
        assert_eq!(private.kind, FrameCallerKind::Private);
        assert_eq!(private.effective_grant, rebound.effective_grant); // dock ceiling still applies
    }

    fn dock_frame(
        store: &FrameStore,
        policy: &crate::verbs::Policy,
        declared: &[String],
    ) -> FrameRecord {
        let args: MintFrameArgs = serde_json::from_value(serde_json::json!({
            "documentPath":"/a/panes/agents.html", "contentRoot":"/a/content", "slot":"dock"
        }))
        .unwrap();
        mint_record(
            store,
            9321,
            policy,
            args,
            "human".into(),
            "agents".into(),
            "team",
            Some(FrameTeam {
                org: "acme".into(),
                team: "a".into(),
            }),
            declared,
        )
    }

    #[test]
    fn rebind_recomputes_the_grant_for_the_new_team_and_revokes_the_old_token() {
        let store = FrameStore::default();
        let declared = vec!["sessions:read".to_string(), "inbox:read:team".to_string()];
        let first = dock_frame(&store, &crate::verbs::Policy::default(), &declared);
        // Open policy: the full set.
        assert_eq!(first.effective_grant, crate::verbs::capabilities::full());
        let b = FrameTeam {
            org: "acme".into(),
            team: "b".into(),
        };
        // The saved policy moved to team-safe since the mint: the rebind computes the grant
        // fresh from it, never keeping (or unioning) the old open grant.
        let team_safe = crate::verbs::Policy {
            preset: crate::verbs::Preset::TeamSafe,
            ..Default::default()
        };
        let rebound =
            rebind_record(&store, &team_safe, &first.frame_id, &first.token, b.clone()).unwrap();
        assert_eq!(rebound.frame_id, first.frame_id);
        assert_eq!(rebound.origin, first.origin);
        assert_ne!(rebound.token, first.token);
        assert!(store.resolve(&first.origin, &first.token).is_none());
        let grant = store.resolve(&rebound.origin, &rebound.token).unwrap();
        assert_eq!(grant.team, Some(b.clone()));
        assert_eq!(grant.scope, "team");
        assert!(rebound.policy.team_safe);
        let mut expected = declared.clone();
        expected.sort();
        let mut got = rebound.effective_grant.clone();
        got.sort();
        assert_eq!(got, expected);
        // The team-safe dock ceiling still holds after the rebind.
        assert!(rebound
            .effective_grant
            .iter()
            .all(|cap| crate::verbs::capabilities::slot_allows("dock", cap)));
        assert!(!rebound
            .effective_grant
            .contains(&"inbox:read:all-teams".to_string()));
        // A rebind with the revoked token loses.
        assert!(rebind_record(&store, &team_safe, &first.frame_id, &first.token, b).is_err());
    }

    #[test]
    fn rebind_under_team_safe_never_widens_past_the_dock_ceiling() {
        let store = FrameStore::default();
        let team_safe = crate::verbs::Policy {
            preset: crate::verbs::Preset::TeamSafe,
            ..Default::default()
        };
        let full = crate::verbs::capabilities::full();
        let first = dock_frame(&store, &team_safe, &full);
        let rebound = rebind_record(
            &store,
            &team_safe,
            &first.frame_id,
            &first.token,
            FrameTeam {
                org: "acme".into(),
                team: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(rebound.effective_grant, first.effective_grant);
        assert!(rebound
            .effective_grant
            .iter()
            .all(|cap| crate::verbs::capabilities::slot_allows("dock", cap)));
        assert!(rebound.effective_grant.len() < full.len());
    }

    #[test]
    fn only_a_follows_context_slot_rebinds() {
        let store = FrameStore::default();
        let args: MintFrameArgs = serde_json::from_value(serde_json::json!({
            "documentPath":"/a/panes/board.html", "contentRoot":"/a/content", "slot":"right-pane"
        }))
        .unwrap();
        let policy = crate::verbs::Policy::default();
        let pane = mint_record(
            &store,
            9321,
            &policy,
            args,
            "human".into(),
            "board".into(),
            "team",
            None,
            &[],
        );
        let team = FrameTeam {
            org: "acme".into(),
            team: "b".into(),
        };
        assert!(rebind_record(&store, &policy, &pane.frame_id, &pane.token, team).is_err());
        assert!(store.resolve(&pane.origin, &pane.token).is_some());
    }

    #[test]
    fn saved_team_safe_preset_controls_mint_without_a_caller_policy() {
        let mut args: MintFrameArgs = serde_json::from_value(serde_json::json!({ "documentPath":"/ctx/board.html", "contentRoot":"/ctx", "slot":"dock" })).unwrap();
        let state = serde_json::json!({"dashboardPolicy":{"preset":"team-safe"}});
        let policy = serde_json::from_value(state["dashboardPolicy"].clone()).unwrap();
        let grant = mint_grant(
            &policy,
            &mut args.policy,
            "dock",
            "board",
            "team",
            &crate::verbs::capabilities::full(),
        );
        assert!(args.policy.team_safe);
        assert!(grant.contains(&"sessions:read".into()));
        assert!(!grant.contains(&"inbox:read:all-teams".into()));
        let grant = mint_grant(
            &crate::verbs::Policy::default(),
            &mut args.policy,
            "dock",
            "board",
            "team",
            &[],
        );
        assert!(!args.policy.team_safe);
        assert_eq!(grant, crate::verbs::capabilities::full());
    }
    #[test]
    fn deleted_baseline_documents_use_an_existing_ancestor_but_live_documents_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deleted.html");
        assert_eq!(
            frame_document_target(&path, true).unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert!(frame_document_target(&path, false).is_err());
        assert!(frame_document_target(dir.path(), true).is_err());
    }
    #[tokio::test]
    async fn thread_authorization_follows_old_parents_outside_the_inbox_list() {
        let dir = tempfile::tempdir().unwrap();
        tokio::task::spawn_blocking(move || {
            let store = crate::weft::WeftStore::open(dir.path().join("inbox.sqlite3")).unwrap();
            let insert = |from, to, text, in_reply_to| {
                crate::weft::inbox::inbox_insert(
                    &store,
                    from,
                    to,
                    text,
                    in_reply_to,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap()
            };
            for _ in 0..502 {
                insert("agent", "human", "fixture", None);
            }
            assert_eq!(insert("agent", "human", "recent", Some(1)), 503);
            assert_eq!(insert("human", "agent", "reply", Some(503)), 504);
            assert!(!crate::weft::inbox::inbox_list(&store)
                .unwrap()
                .iter()
                .any(|row| row.id == 1));
            let message = FrameMessage {
                org: "acme".into(),
                team: "a".into(),
                message_id: 503,
            };
            assert!(check_message_thread(&store, &message, &[1, 503, 504]).is_ok());
            assert!(check_message_thread(&store, &message, &[502]).is_err());
        })
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod records_ceiling_tests {
    use super::*;
    use crate::verbs::{
        capabilities, check_frame_capability, Decision, Dispatcher, Policy, Preset,
    };
    use serde_json::json;

    #[test]
    fn centre_notes_are_refused_under_team_safe_and_allowed_under_open() {
        let home = tempfile::tempdir().unwrap();
        let binding = crate::records::binding::Binding::from_context(
            home.path(),
            &42u64.into(),
            "acme/ctx",
            None,
        )
        .unwrap();
        for preset in [Preset::TeamSafe, Preset::Open] {
            let policy = Policy {
                preset,
                ..Default::default()
            };
            for source in ["private", "team", "message"] {
                let frames = FrameStore::default();
                let args = serde_json::from_value(
                    json!({"documentPath":"/ctx/board.html","contentRoot":"/ctx","slot":"centre"}),
                )
                .unwrap();
                let frame = mint_record(
                    &frames,
                    9321,
                    &policy,
                    args,
                    "human".into(),
                    "board".into(),
                    source,
                    None,
                    &capabilities::full(),
                );
                let mut frame = frames
                    .bind_records(&frame.frame_id, binding.clone())
                    .unwrap();
                for cap in ["records:read:own", "records:write:own"] {
                    assert_eq!(
                        frame.effective_grant.iter().any(|g| g == cap),
                        preset == Preset::Open
                    );
                }
                if preset == Preset::Open {
                    assert_eq!(frame.effective_grant, capabilities::full());
                }
                // Admission also checks the ceiling if a broader stored grant remains.
                frame.effective_grant = capabilities::full();
                for (verb, args) in [
                    ("records.read", json!({"collection":"notes","id":"one"})),
                    (
                        "records.create",
                        json!({"collection":"notes","body":"Edited"}),
                    ),
                    ("subscribe", json!({"topic":"records","collection":"notes"})),
                ] {
                    assert_eq!(
                        check_frame_capability(&frame, verb, &args).is_ok(),
                        preset == Preset::Open
                    );
                    assert_eq!(
                        matches!(
                            Dispatcher::default().dispatch_frame(&frame, verb, args, &policy, 0),
                            Decision::Admit { .. }
                        ),
                        preset == Preset::Open
                    );
                }
            }
        }
        let safe = Policy {
            preset: Preset::TeamSafe,
            ..Default::default()
        };
        let all = capabilities::full();
        assert!(safe
            .effective_grant("board", "right-pane", "private", &all, &all)
            .contains(&"records:write:own".into()));
        let overridden: Policy = serde_json::from_value(
            json!({"preset":"team-safe","perDashboard":{"board":{"grant":all}}}),
        )
        .unwrap();
        assert!(!overridden
            .effective_grant("board", "centre", "private", &all, &all)
            .contains(&"records:read:own".into()));
    }
}

fn inbox_network_policy(policy: &mut FramePolicy, slot: &str, surface: Option<&str>) {
    policy.inbox_media = slot == "right-pane" && surface == Some("human-inbox");
    if policy.inbox_media {
        policy.no_outside_network = true;
        policy.allowed_hosts.clear();
    }
}
#[cfg(test)]
#[test]
fn only_inbox_pane_mints_without_outside_network() {
    let mut policy = FramePolicy {
        allowed_hosts: vec!["https://example.com".into()],
        ..Default::default()
    };
    inbox_network_policy(&mut policy, "message", Some("human-inbox"));
    assert!(!policy.no_outside_network);
    inbox_network_policy(&mut policy, "right-pane", Some("notes"));
    assert!(!policy.no_outside_network);
    inbox_network_policy(&mut policy, "right-pane", Some("human-inbox"));
    assert!(policy.no_outside_network);
    assert!(policy.allowed_hosts.is_empty());
}
