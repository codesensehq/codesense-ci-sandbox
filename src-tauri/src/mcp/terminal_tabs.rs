// SPDX-License-Identifier: MIT
//! The terminal tabs a person sees, read live from every host's layout: each
//! tab's current name, the machine it runs on and its workspace. MCP lists
//! them by these names and resolves a readable address (`{tab, machine?,
//! workspace?}`) to one tab on the trusted side; nothing is cached, so a
//! rename by anyone is seen by the next call.
use std::collections::BTreeMap;

use crate::commands::machines::{RuntimeLayoutReply, RuntimeSessionTarget, LOCAL_MACHINE_ID};
use crate::commands::pty_layout::WORKSPACE_KEY_SEPARATOR;

/// One tab of one host's layout, as its tab bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveTab {
    pub machine_id: String,
    /// The machine's label (`This Mac` for this one).
    pub machine: String,
    pub workspace_key: String,
    /// The workspace's id (or story id) within its team.
    pub workspace: String,
    pub session: String,
    /// The tab's name now.
    pub name: String,
    /// The opaque handle MCP addresses it by, when it has one: a remote tab's,
    /// or a local tab MCP opened.
    pub handle: Option<String>,
    pub exited: bool,
    /// Where a rename goes: the tab's host as currently admitted.
    pub target: RuntimeSessionTarget,
    /// The saved agent a launch stamped on this tab, with its revision dropped.
    pub agent: Option<String>,
}

impl LiveTab {
    pub(crate) fn from_entry(
        target: &crate::commands::machines::RuntimeMemberTarget,
        machine: &str,
        key: &codesense_pty_core::domain::WorkspaceKey,
        tab: &codesense_pty_core::layout::TabEntry,
    ) -> Self {
        let key = key.as_str();
        Self {
            machine_id: target.member.machine_id.clone(),
            machine: machine.to_string(),
            workspace_key: key.to_string(),
            workspace: key
                .split_once(WORKSPACE_KEY_SEPARATOR)
                .map_or(key, |(_, form)| form)
                .to_string(),
            session: tab.session.as_str().to_string(),
            name: tab.name.clone(),
            handle: tab
                .tags
                .get("remoteHandle")
                .or_else(|| tab.tags.get("handle"))
                .cloned(),
            exited: tab.state.is_exited(),
            target: RuntimeSessionTarget {
                runtime: target.clone(),
                session: tab.session.clone(),
            },
            agent: tab
                .tags
                .get("remoteAgentKey")
                .or_else(|| {
                    tab.tags
                        .get(crate::commands::dashboard_launch::TAG_LAUNCH_AGENT_REVISION)
                })
                .map(|key| key.split('@').next().unwrap_or(key).to_string())
                .filter(|key| !key.is_empty()),
        }
    }
}

pub(crate) const THIS_MAC: &str = "This Mac";

/// Every tab of every live host layout. A machine that is offline has no
/// live layout: its tabs are absent here (`offline_machines` names it).
pub(crate) fn live_tabs(
    replies: &[RuntimeLayoutReply],
    labels: &BTreeMap<String, String>,
) -> Vec<LiveTab> {
    let mut tabs = Vec::new();
    for reply in replies {
        let RuntimeLayoutReply::Live {
            target, snapshot, ..
        } = reply
        else {
            continue;
        };
        let machine_id = target.member.machine_id.clone();
        let machine = if machine_id == LOCAL_MACHINE_ID {
            THIS_MAC.to_string()
        } else {
            labels
                .get(&machine_id)
                .cloned()
                .unwrap_or_else(|| machine_id.clone())
        };
        for (key, layout) in &snapshot.workspaces {
            for tab in &layout.tabs {
                tabs.push(LiveTab::from_entry(target, &machine, key, tab));
            }
        }
    }
    tabs
}

/// Architecture: workspace operations match the same layout key as close_workspace.
pub(crate) fn workspace_tab_indices(tabs: &[(&str, &str)], key: &str) -> Vec<usize> {
    tabs.iter()
        .enumerate()
        .filter(|(_, (workspace, _))| *workspace == key)
        .map(|(i, _)| i)
        .collect()
}

/// The machines whose layout could not be read live (offline, or held only as
/// its last known layout).
pub(crate) fn offline_machines(replies: &[RuntimeLayoutReply]) -> Vec<String> {
    replies
        .iter()
        .filter_map(|reply| match reply {
            RuntimeLayoutReply::Live { .. } => None,
            RuntimeLayoutReply::Historical { member, .. }
            | RuntimeLayoutReply::Unavailable { member, .. } => Some(member.machine_id.clone()),
        })
        .collect()
}

/// Every host's tabs as read just now, with the machines that could not be read.
pub(crate) struct LiveTabs {
    pub tabs: Vec<LiveTab>,
    pub offline: Vec<String>,
    pub live: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub(crate) activity: Vec<(
        crate::commands::machines::RuntimeMemberTarget,
        Result<Vec<codesense_pty_core::activity::Snapshot>, String>,
    )>,
}

impl LiveTabs {
    pub(crate) fn of(replies: &[RuntimeLayoutReply], labels: BTreeMap<String, String>) -> Self {
        Self {
            tabs: live_tabs(replies, &labels),
            offline: offline_machines(replies),
            live: replies
                .iter()
                .filter_map(|r| {
                    if let RuntimeLayoutReply::Live { target, .. } = r {
                        Some(target.member.machine_id.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            labels,
            activity: Vec::new(),
        }
    }

    /// Read every admitted host's layout now.
    pub(crate) async fn read(app: &tauri::AppHandle) -> Self {
        let labels = crate::commands::machines::machine_labels(app).await;
        let replies =
            match crate::commands::pty_layout::webview_member_layout_snapshots(app.clone()).await {
                Some(Ok(replies)) => replies,
                _ => Vec::new(),
            };
        Self::of(&replies, labels)
    }

    /// Read this Mac's own layouts now; no machine is asked.
    /// Its tabs carry no machine labels: this Mac's own need none.
    pub(crate) async fn read_local(app: &tauri::AppHandle) -> Self {
        let replies =
            match crate::commands::pty_layout::webview_local_layout_snapshots(app.clone()).await {
                Some(Ok(replies)) => replies,
                _ => Vec::new(),
            };
        Self::of(&replies, BTreeMap::new())
    }

    /// What a listed session's tab shows: found by its handle or session in a
    /// live layout. A remote tab (`remote`: its machine and workspace id) that
    /// no live layout holds is offline while its machine is, else ended.
    pub(crate) fn view(
        &self,
        handle: &str,
        session: Option<&str>,
        remote: Option<(&str, &str)>,
    ) -> Option<super::session_list::TabView> {
        use super::session_list::TabView;
        let machine = remote.map_or(LOCAL_MACHINE_ID, |(machine, _)| machine);
        let candidates: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tab.machine_id == machine)
            .cloned()
            .collect();
        if let Some(tab) = by_handle(&candidates, handle, session) {
            let activity = self
                .activity
                .iter()
                .find(|(target, _)| target == &tab.target.runtime)
                .map(|(_, result)| result);
            return Some(TabView {
                name: Some(tab.name.clone()),
                machine: tab.machine.clone(),
                machine_id: (tab.machine_id != LOCAL_MACHINE_ID).then(|| tab.machine_id.clone()),
                workspace_handle: None,
                workspace: Some(tab.workspace.clone()),
                session: Some(tab.session.clone()),
                alive: Some(!tab.exited),
                activity: activity.and_then(|result| activity_state(result, &tab.session)),
                activity_failed: activity
                    .is_some_and(|result| activity_failed(result, &tab.session)),
                subagents: activity.map_or(0, |result| subagents_open(result, &tab.session)),
                agent: tab.agent.clone(),
            });
        }
        let (machine_id, workspace) = remote?;
        Some(TabView {
            name: None,
            machine_id: (machine_id != LOCAL_MACHINE_ID).then(|| machine_id.to_string()),
            workspace_handle: None,
            machine: self
                .labels
                .get(machine_id)
                .cloned()
                .unwrap_or_else(|| machine_id.to_string()),
            workspace: Some(workspace.to_string()),
            session: None,
            activity: None,
            activity_failed: false,
            subagents: 0,
            agent: None,
            alive: if self.offline.iter().any(|m| m == machine_id) {
                None
            } else {
                Some(false)
            },
        })
    }
}

/// Missing invocation/unsupported owner retains legacy compatibility; a failed read
/// or an unobserved invocation must never invent a running/done claim.
fn activity_state(
    result: &Result<Vec<codesense_pty_core::activity::Snapshot>, String>,
    session: &str,
) -> Option<super::session_list::SessionState> {
    use super::session_list::SessionState;
    use codesense_pty_core::activity::State;
    let snapshots = match result {
        Ok(snapshots) => snapshots,
        Err(error) if error.contains("upgrade required") => return None,
        Err(_) => return Some(SessionState::Unknown),
    };
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.session == session)?;
    Some(if snapshot.exit.is_some() {
        SessionState::Exited
    } else if !snapshot.observed {
        SessionState::Unknown
    } else {
        match snapshot.state {
            State::Thinking | State::Working => SessionState::Running,
            State::NeedsPermission => SessionState::NeedsYou,
            State::Idle | State::Done | State::Errored | State::Crashed => SessionState::Idle,
        }
    })
}

/// Whether the owner's last observation of `session` is a turn that errored or crashed.
fn activity_failed(
    result: &Result<Vec<codesense_pty_core::activity::Snapshot>, String>,
    session: &str,
) -> bool {
    use codesense_pty_core::activity::State;
    result.as_ref().is_ok_and(|snapshots| {
        snapshots.iter().any(|s| {
            s.session == session
                && s.exit.is_none()
                && s.observed
                && matches!(s.state, State::Errored | State::Crashed)
        })
    })
}

/// How many subagents the owner's last observation of `session` says its agent runs (it
/// publishes none for an unobserved or exited invocation).
fn subagents_open(
    result: &Result<Vec<codesense_pty_core::activity::Snapshot>, String>,
    session: &str,
) -> u32 {
    result.as_ref().map_or(0, |snapshots| {
        snapshots
            .iter()
            .find(|s| s.session == session)
            .map_or(0, |s| s.subagents_open)
    })
}

/// The tab a terminal tool was pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Addressed {
    /// By a handle: the tool's own handle path takes it from here.
    Handle(String),
    /// A tab on this Mac with no handle, found by its current name.
    LocalSession(String),
}

/// Turn a tool's `handle` or readable address into the tab it means. Exactly
/// one of the two is given. A tab found by name answers to its handle when it
/// has one; a local tab without one answers to its session.
pub(crate) async fn address(
    app: &tauri::AppHandle,
    handle: Option<&str>,
    address: Option<TabAddress>,
) -> Result<Addressed, String> {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    use tauri::Manager;
    let handle = handle.map(str::trim).filter(|handle| !handle.is_empty());
    let address = match (handle, address) {
        (Some(handle), None) => return Ok(Addressed::Handle(handle.to_string())),
        (None, Some(address)) => address,
        (Some(_), Some(_)) => return Err("give a handle or a tab name, not both".into()),
        (None, None) => return Err("give the tab's handle, or its name as tab".into()),
    };
    let live = LiveTabs::read(app).await;
    let tab = resolve(&live.tabs, &address)?;
    let launch = app.try_state::<DashboardLaunchState>();
    if let Some(handle) = &tab.handle {
        let known = launch.as_ref().is_some_and(|launch| {
            launch.remote_shell_by_handle(handle).is_some() || launch.resolve(handle).is_some()
        });
        if known {
            return Ok(Addressed::Handle(handle.clone()));
        }
    }
    if tab.machine_id == LOCAL_MACHINE_ID {
        return Ok(Addressed::LocalSession(tab.session.clone()));
    }
    Err(format!(
        "{} has no handle this caller can use; address it by the handle list_sessions gives",
        describe(tab)
    ))
}

/// A tab addressed the way a person names it.
#[derive(Debug, Clone, Default)]
pub(crate) struct TabAddress {
    /// The tab's name as its tab bar shows it now, e.g. "Terminal 2".
    pub tab: String,
    /// The machine's label (or id) it runs on; "This Mac" for this one.
    pub machine: Option<String>,
    /// The workspace id (or story id) it is in.
    pub workspace: Option<String>,
}

fn describe(tab: &LiveTab) -> String {
    format!("\"{}\" on {} in {}", tab.name, tab.machine, tab.workspace)
}

/// The one live tab `address` names, matched on current names. None, or more
/// than one, is refused with the tabs a caller could mean.
pub(crate) fn resolve<'a>(
    tabs: &'a [LiveTab],
    address: &TabAddress,
) -> Result<&'a LiveTab, String> {
    let wanted = address.tab.trim();
    if wanted.is_empty() {
        return Err("tab must name a terminal tab".into());
    }
    let placed: Vec<&LiveTab> = tabs
        .iter()
        .filter(|tab| !tab.exited)
        .filter(|tab| {
            address
                .machine
                .as_deref()
                .map(str::trim)
                .is_none_or(|machine| {
                    tab.machine.eq_ignore_ascii_case(machine) || tab.machine_id == machine
                })
        })
        .filter(|tab| {
            address
                .workspace
                .as_deref()
                .map(str::trim)
                .is_none_or(|workspace| {
                    tab.workspace == workspace || tab.workspace_key == workspace
                })
        })
        .collect();
    let mut matches: Vec<&LiveTab> = placed
        .iter()
        .copied()
        .filter(|t| t.name == wanted)
        .collect();
    if matches.is_empty() {
        matches = placed
            .iter()
            .copied()
            .filter(|t| t.name.eq_ignore_ascii_case(wanted))
            .collect();
    }
    match matches.as_slice() {
        [one] => Ok(one),
        [] => {
            let names: Vec<String> = placed.iter().map(|t| describe(t)).collect();
            Err(if names.is_empty() {
                format!("no terminal tab is named \"{wanted}\" there, and no tab matches that machine and workspace")
            } else {
                format!(
                    "no terminal tab is named \"{wanted}\"; the tabs now are: {}",
                    names.join(", ")
                )
            })
        }
        several => Err(format!(
            "\"{wanted}\" names more than one tab; add machine or workspace: {}",
            several
                .iter()
                .map(|t| describe(t))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The live tab a handle (or the session a launch handle resolved to) names.
pub(crate) fn by_handle<'a>(
    tabs: &'a [LiveTab],
    handle: &str,
    session: Option<&str>,
) -> Option<&'a LiveTab> {
    let unique = |matches: Vec<&'a LiveTab>| {
        if matches.len() == 1 {
            matches.into_iter().next()
        } else {
            None
        }
    };
    let handles: Vec<_> = tabs
        .iter()
        .filter(|tab| tab.handle.as_deref() == Some(handle))
        .collect();
    if !handles.is_empty() {
        return unique(handles);
    }
    session.and_then(|session| unique(tabs.iter().filter(|tab| tab.session == session).collect()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::machines::{
        RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
        RuntimeNamespace,
    };
    use codesense_pty_core::domain::{PtySessionId, ServerId, WorkspaceKey};
    use codesense_pty_core::layout::{
        EntryState, LayoutSnapshot, Placement, TabEntry, Tags, ViewerLayout, WorkspaceLayout,
    };

    #[test]
    fn owner_activity_is_authoritative_and_unavailable_is_unknown() {
        use super::super::session_list::SessionState;
        use codesense_pty_core::activity::{Identity, Snapshot, State};
        use codesense_pty_core::agent_cli::AgentCli;
        let mut snapshot = Snapshot {
            session: "session".into(),
            identity: Identity {
                host_boot_id: "boot".into(),
                session_incarnation: "inc".into(),
                invocation_id: "run".into(),
            },
            revision: 1,
            turn_generation: 1,
            provider: AgentCli::Codex,
            state: State::NeedsPermission,
            observed: true,
            limited: false,
            exit: None,
            asks: vec![],
            reason: None,
            tool_open: None,
            activity_age_ms: None,
            subagents_open: 0,
        };
        assert_eq!(
            activity_state(&Ok(vec![snapshot.clone()]), "session"),
            Some(SessionState::NeedsYou)
        );
        snapshot.state = State::Done;
        assert_eq!(
            activity_state(&Ok(vec![snapshot.clone()]), "session"),
            Some(SessionState::Idle)
        );
        // Its subagents run beside its own idle state.
        snapshot.subagents_open = 2;
        assert_eq!(subagents_open(&Ok(vec![snapshot.clone()]), "session"), 2);
        assert_eq!(subagents_open(&Err("disconnected".into()), "session"), 0);
        snapshot.observed = false;
        assert_eq!(
            activity_state(&Ok(vec![snapshot]), "session"),
            Some(SessionState::Unknown)
        );
        assert_eq!(
            activity_state(&Err("disconnected".into()), "session"),
            Some(SessionState::Unknown)
        );
        assert_eq!(
            activity_state(&Err("host upgrade required".into()), "session"),
            None
        );
        assert_eq!(activity_state(&Ok(vec![]), "unmanaged"), None);
    }

    #[test]
    fn same_session_id_on_another_host_cannot_supply_a_listed_state() {
        let replies = vec![
            reply(
                LOCAL_MACHINE_ID,
                &[("team::local", &[("same", "local", None)])],
            ),
            reply(
                "remote",
                &[("team::remote", &[("same", "remote", Some("remote-handle"))])],
            ),
        ];
        let live = LiveTabs::of(&replies, BTreeMap::new());
        assert!(by_handle(&live.tabs, "unknown", Some("same")).is_none());
        assert_eq!(
            live.view("local-handle", Some("same"), None)
                .unwrap()
                .name
                .as_deref(),
            Some("local")
        );
        assert_eq!(
            live.view("remote-handle", Some("same"), Some(("remote", "remote")))
                .unwrap()
                .name
                .as_deref(),
            Some("remote")
        );
    }

    #[test]
    fn a_listed_tab_reads_its_own_hosts_subagent_count() {
        use super::super::session_list::SessionState;
        use codesense_pty_core::activity::{Identity, Snapshot, State};
        use codesense_pty_core::agent_cli::AgentCli;
        let snapshot = |session: &str| Snapshot {
            session: session.into(),
            identity: Identity {
                host_boot_id: "boot".into(),
                session_incarnation: "inc".into(),
                invocation_id: "run".into(),
            },
            revision: 1,
            turn_generation: 1,
            provider: AgentCli::Codex,
            state: State::Done,
            observed: true,
            limited: false,
            exit: None,
            asks: vec![],
            reason: None,
            tool_open: None,
            activity_age_ms: None,
            subagents_open: 2,
        };
        let replies = vec![reply(
            "remote",
            &[("team::remote", &[("sess", "worker", Some("w-handle"))])],
        )];
        let mut live = LiveTabs::of(&replies, BTreeMap::new());
        let target = live.tabs[0].target.runtime.clone();
        live.activity
            .push((target.clone(), Ok(vec![snapshot("sess")])));
        let view = live
            .view("w-handle", Some("sess"), Some(("remote", "remote")))
            .unwrap();
        assert_eq!(
            (view.activity, view.subagents),
            (Some(SessionState::Idle), 2)
        );
        live.activity = vec![(target, Ok(vec![snapshot("other")]))];
        let view = live
            .view("w-handle", Some("sess"), Some(("remote", "remote")))
            .unwrap();
        assert_eq!(view.subagents, 0);
    }

    /// A tab: its session, name and remote handle.
    type FixtureTab<'a> = (&'a str, &'a str, Option<&'a str>);

    pub(crate) fn reply(machine: &str, workspaces: &[(&str, &[FixtureTab])]) -> RuntimeLayoutReply {
        let mut layouts = BTreeMap::new();
        for (key, tabs) in workspaces {
            let mut layout = WorkspaceLayout::default();
            for (session, name, handle) in *tabs {
                let mut tags = Tags::new();
                if let Some(handle) = handle {
                    tags.insert("remoteHandle".into(), (*handle).into());
                }
                layout.tabs.push(TabEntry {
                    session: PtySessionId::new(*session),
                    name: (*name).into(),
                    role: None,
                    placement: Placement::Tab,
                    state: EntryState::Live,
                    tags,
                });
            }
            layouts.insert(WorkspaceKey::new(*key), layout);
        }
        RuntimeLayoutReply::Live {
            private_authority_epoch: None,
            target: RuntimeMemberTarget {
                member: RuntimeMemberAddress {
                    machine_id: machine.into(),
                    slot: RuntimeMemberSlot::Remote,
                },
                admission_revision: RuntimeCounter::new(1),
                namespace: RuntimeNamespace {
                    server_id: "s".into(),
                    epoch: "e".into(),
                    generation: RuntimeCounter::new(1),
                },
            },
            snapshot: Box::new(LayoutSnapshot {
                schema: 1,
                server_id: ServerId::new("s"),
                server_name: "s".into(),
                epoch: "e".into(),
                rev: 1,
                workspaces: layouts,
                viewer: ViewerLayout::default(),
            }),
        }
    }

    fn tabs() -> Vec<LiveTab> {
        let labels = BTreeMap::from([("droplet".to_string(), "codesense-test-1".to_string())]);
        live_tabs(
            &[
                reply(
                    LOCAL_MACHINE_ID,
                    &[("acme:default|root:repo", &[("local-1", "Terminal 2", None)])],
                ),
                reply(
                    "droplet",
                    &[(
                        "acme:default|root:repo",
                        &[
                            ("remote-shell-a", "Terminal 2", Some("remote-tab-a")),
                            ("remote-shell-b", "build watch", Some("remote-tab-b")),
                        ],
                    )],
                ),
            ],
            &labels,
        )
    }

    #[test]
    fn tabs_carry_their_current_name_machine_and_workspace() {
        let tabs = tabs();
        let remote = by_handle(&tabs, "remote-tab-b", None).unwrap();
        assert_eq!(remote.name, "build watch");
        assert_eq!(remote.machine, "codesense-test-1");
        assert_eq!(remote.workspace, "root:repo");
        assert_eq!(
            by_handle(&tabs, "x", Some("local-1")).unwrap().machine,
            THIS_MAC
        );
    }

    #[test]
    fn a_listed_remote_session_is_live_offline_or_ended_by_its_machines_layout() {
        let labels = BTreeMap::from([("droplet".to_string(), "codesense-test-1".to_string())]);
        let live = LiveTabs::of(
            &[reply(
                "droplet",
                &[(
                    "acme:default|root:repo",
                    &[("remote-shell-a", "Terminal 2", Some("remote-tab-a"))],
                )],
            )],
            labels.clone(),
        );
        let remote = Some(("droplet", "root:repo"));
        let running = live.view("remote-tab-a", None, remote).unwrap();
        assert_eq!(running.alive, Some(true));
        assert_eq!(running.name.as_deref(), Some("Terminal 2"));
        assert_eq!(running.machine, "codesense-test-1");
        // Gone from a live layout: it ended.
        assert_eq!(
            live.view("remote-tab-gone", None, remote).unwrap().alive,
            Some(false)
        );
        // Its machine not connected: offline, never "exited".
        let offline = LiveTabs::of(
            &[RuntimeLayoutReply::Unavailable {
                member: RuntimeMemberAddress {
                    machine_id: "droplet".into(),
                    slot: RuntimeMemberSlot::Remote,
                },
                admission_revision: RuntimeCounter::new(1),
            }],
            labels,
        );
        let view = offline.view("remote-tab-a", None, remote).unwrap();
        assert_eq!(view.alive, None);
        assert_eq!(view.workspace.as_deref(), Some("root:repo"));
        // A local session with no tab has no view.
        assert!(live.view("h", Some("local-x"), None).is_none());
    }

    #[test]
    fn list_sessions_shows_a_live_remote_tab_running_under_its_current_name() {
        use crate::commands::dashboard_launch::{derive_status, LaunchMeta, SessionRow};
        use crate::mcp::session_list::{session_list_rows_with_tabs, SessionState};
        // A remote tab's registry row has no local session: alone it reads exited.
        let row = SessionRow {
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            output_run_id: None,
            handle: "remote-tab-a".into(),
            session_id: None,
            kind: "registered".into(),
            alive: false,
            status: derive_status(false, None, false),
            backgrounded: false,
            started_with: None,
            started_at_ms: None,
            launcher: None,
            account_provenance: None,
            provenance: None,
            last_output_ms: None,
            meta: LaunchMeta {
                display_name: Some("name when opened".into()),
                ..LaunchMeta::default()
            },
            workspace: None,
            agent_revision: None,
            deadline_at_ms: None,
            expiring: false,
            machine_id: None,
            launch_team_key: None,
        };
        let labels = BTreeMap::from([("droplet".to_string(), "codesense-test-1".to_string())]);
        let live = LiveTabs::of(
            &[reply(
                "droplet",
                &[(
                    "acme:default|root:repo",
                    &[("remote-shell-a", "Terminal 2", Some("remote-tab-a"))],
                )],
            )],
            labels,
        );
        let listed = session_list_rows_with_tabs(
            vec![row],
            |_| None,
            None,
            |row| {
                live.view(
                    &row.handle,
                    row.session_id.as_deref(),
                    Some(("droplet", "root:repo")),
                )
            },
        );
        assert_eq!(listed[0].state, SessionState::Running);
        let wire = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(wire["displayName"], "Terminal 2");
        assert_eq!(wire["machine"], "codesense-test-1");
        assert_eq!(wire["workspace"], "root:repo");
        assert_eq!(wire["state"], "running");
    }

    #[test]
    fn a_readable_address_resolves_to_one_tab_or_refuses_with_the_candidates() {
        let tabs = tabs();
        let address = |tab: &str, machine: Option<&str>| TabAddress {
            tab: tab.into(),
            machine: machine.map(str::to_string),
            workspace: None,
        };
        let ambiguous = resolve(&tabs, &address("Terminal 2", None)).unwrap_err();
        assert!(ambiguous.contains("more than one"), "{ambiguous}");
        assert!(ambiguous.contains("on codesense-test-1"), "{ambiguous}");
        assert!(ambiguous.contains("on This Mac"), "{ambiguous}");

        let remote = resolve(&tabs, &address("Terminal 2", Some("codesense-test-1"))).unwrap();
        assert_eq!(remote.handle.as_deref(), Some("remote-tab-a"));
        assert_eq!(
            resolve(&tabs, &address("terminal 2", Some("this mac")))
                .unwrap()
                .session,
            "local-1"
        );

        // A name no tab has now is refused, listing the current names.
        let stale = resolve(&tabs, &address("Terminal 9", Some("droplet"))).unwrap_err();
        assert!(
            stale.contains("\"build watch\" on codesense-test-1"),
            "{stale}"
        );
    }
}
