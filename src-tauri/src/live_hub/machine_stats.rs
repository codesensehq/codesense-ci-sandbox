// SPDX-License-Identifier: MIT
//! Runtime observations shared by the Machines surface and tool reads.

use super::{publish, LiveHub, MACHINE_STATS};
use crate::terminal::session_server::{SessionServerClient, SessionServerState};
use codesense_host_proto::machine_stats::MachineStats;
use std::{collections::BTreeMap, sync::Arc};
use tauri::Manager;

pub(crate) fn client(app: &tauri::AppHandle, id: &str) -> Option<Arc<SessionServerClient>> {
    if id == crate::commands::machines::LOCAL_MACHINE_ID {
        app.try_state::<SessionServerState>()
            .and_then(|state| state.connected())
    } else {
        crate::commands::pty_backend::resolve_spawn_backend(app, id, "")
            .ok()?
            .client()
    }
}

pub(crate) fn note<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    sample: Option<MachineStats>,
    supported: bool,
) {
    let Some(hub) = app.try_state::<LiveHub>() else {
        return;
    };
    let mut row = sample
        .map(|s| serde_json::to_value(s).unwrap())
        .unwrap_or_else(|| serde_json::json!({}));
    row["id"] = id.into();
    row["machineStatsSupported"] = supported.into();
    let deltas = hub
        .model
        .lock()
        .unwrap()
        .projections
        .patch_view("machineStats", vec![(None, row)], vec![], None)
        .unwrap();
    publish(app, deltas);
}

pub(crate) fn reconcile(app: &tauri::AppHandle) {
    let hub = app.state::<LiveHub>();
    let watching = hub
        .model
        .lock()
        .unwrap()
        .projections
        .has_subscribers("machineStats");
    let mut desired = BTreeMap::new();
    if watching {
        let mut ids = std::collections::BTreeSet::from(["local".to_owned()]);
        if let Some(manager) =
            app.try_state::<crate::commands::host_connection_manager::HostConnectionManager>()
        {
            ids.extend(
                manager
                    .admission_baseline()
                    .rows
                    .into_iter()
                    .filter_map(|row| {
                        matches!(
                            row.state,
                            crate::commands::machines::RuntimeAdmissionState::Live { .. }
                        )
                        .then_some(row.member.machine_id)
                    }),
            );
        }
        for id in ids {
            if let Some(client) = client(app, &id) {
                desired.insert(id, client);
            }
        }
    }
    reconcile_watches(app, watching, desired);
}

fn reconcile_watches<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    watching: bool,
    desired: BTreeMap<String, Arc<SessionServerClient>>,
) {
    let hub = app.state::<LiveHub>();
    let mut model = hub.model.lock().unwrap();
    // A concurrent subscription change gets a new reconciliation wake.
    if watching != model.projections.has_subscribers("machineStats") {
        drop(model);
        hub.invalidate(MACHINE_STATS);
        return;
    }
    let previous = std::mem::replace(&mut model.machine_stats_watches, desired.clone());
    let changed = previous
        .iter()
        .filter(|(id, old)| desired.get(*id).is_none_or(|new| !Arc::ptr_eq(old, new)))
        .map(|(id, _)| id.clone())
        .chain(
            desired
                .keys()
                .filter(|id| !previous.contains_key(*id))
                .cloned(),
        )
        .collect();
    let deltas = model
        .projections
        .patch_view("machineStats", vec![], changed, None)
        .unwrap();
    drop(model);
    publish(app, deltas);
    for (id, old) in &previous {
        if desired.get(id).is_none_or(|new| !Arc::ptr_eq(old, new)) {
            old.set_machine_stats_watching(false);
            let old = old.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let _ = old.sync_machine_stats_watch();
            });
        }
    }
    for (id, next) in desired {
        if previous.get(&id).is_some_and(|old| Arc::ptr_eq(old, &next)) {
            continue;
        }
        next.set_machine_stats_watching(true);
        let handle = app.clone();
        tauri::async_runtime::spawn_blocking(move || match next.sync_machine_stats_watch() {
            Ok(sample) => {
                let hub = handle.state::<LiveHub>();
                let current = hub
                    .model
                    .lock()
                    .unwrap()
                    .machine_stats_watches
                    .get(&id)
                    .is_some_and(|c| Arc::ptr_eq(c, &next));
                if current {
                    note(&handle, &id, sample, next.supports_machine_stats());
                }
            }
            Err(error) => log::debug!("machine stats for {id}: {error}"),
        });
    }
}

pub(crate) async fn refresh_for_tool(
    app: &tauri::AppHandle,
    rows: &mut [crate::commands::machines::MachineSummary],
) {
    for row in rows {
        if !matches!(
            row.status,
            crate::commands::machines::MachineStatus::Connected { .. }
        ) {
            continue;
        }
        let source = client(app, &row.machine_id);
        refresh_row_for_tool(row, source).await;
    }
}

async fn refresh_row_for_tool(
    row: &mut crate::commands::machines::MachineSummary,
    client: Option<Arc<SessionServerClient>>,
) {
    // Listing may have attached an older observation. Only the client's freshness-checked
    // read may supply a tool result, including when the connection disappears during it.
    row.machine_stats = None;
    let Some(client) = client else {
        return;
    };
    let result = crate::commands::await_blocking("machine_stats.read", move || {
        let sample = client.read_machine_stats().map_err(|e| e.to_string())?;
        Ok((sample, client.supports_machine_stats()))
    })
    .await;
    if let Ok((sample, supported)) = result {
        row.machine_stats = sample;
        row.machine_stats_supported = Some(supported);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_session_server::testing::{wait_until, RecordingSink, TestServer};

    #[test]
    fn failed_tool_read_drops_the_lists_previous_observation() {
        use crate::commands::machines::{MachineCapabilities, MachineStatus, MachineSummary};
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let server = TestServer::start_stoppable_at(root.path());
        let connect = || {
            SessionServerClient::connect(
                server.root(),
                Arc::new(RecordingSink::default()),
                Arc::default(),
                None,
            )
            .unwrap()
        };
        let previous = connect().read_machine_stats().unwrap();
        assert!(previous.is_some());
        let client = connect();
        let mut row = MachineSummary {
            machine_stats: previous.clone(),
            machine_stats_supported: Some(true),
            machine_id: "local".into(),
            kind: "local",
            label: "This Mac".into(),
            scope: None,
            declaration: None,
            binding: None,
            can_publish: false,
            remote_mcp_control: false,
            status: MachineStatus::Connected {
                workspaces: Some(0),
                terminal_server_update: false,
                terminal_server_sessions: None,
            },
            attention: 0,
            capabilities: MachineCapabilities {
                agent_callbacks: true,
            },
            terminal_server: None,
        };
        // A fresh lookup must not turn this one-shot request into a watch.
        tauri::async_runtime::block_on(refresh_row_for_tool(&mut row, Some(client.clone())));
        assert!(row.machine_stats.is_some());
        assert!(!server.host().machine_stats_is_watched());
        // Reconnect with an empty client cache, then lose the connection during the list.
        let client = connect();
        server.stop();
        row.machine_stats = previous.clone();
        tauri::async_runtime::block_on(refresh_row_for_tool(&mut row, Some(client)));
        assert!(row.machine_stats.is_none());
        assert_eq!(row.machine_stats_supported, Some(true));
        row.machine_stats = previous;
        tauri::async_runtime::block_on(refresh_row_for_tool(&mut row, None));
        assert!(row.machine_stats.is_none());
    }

    #[test]
    fn subscriptions_control_real_server_watches_and_retire_disconnected_samples() {
        let server = TestServer::start();
        let client = SessionServerClient::connect(
            server.root(),
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(LiveHub::default());
        let handle = app.handle();
        let hub = app.state::<LiveHub>();
        assert!(!server.host().machine_stats_is_watched());
        super::super::subscribe(handle, "machineStats", "all", None, Some("frame"), None).unwrap();
        hub.model
            .lock()
            .unwrap()
            .projections
            .set_owner("frame", "window");
        reconcile_watches(
            handle,
            true,
            BTreeMap::from([("local".into(), client.clone())]),
        );
        assert!(wait_until(|| server.host().machine_stats_is_watched()));
        assert!(wait_until(|| !hub
            .model
            .lock()
            .unwrap()
            .projections
            .snapshot("machineStats", "all")
            .unwrap()
            .snapshot
            .is_empty()));
        let sample = client.cached_machine_stats().unwrap();
        assert!(sample.cpu_count > 0);
        assert!(sample.mem_used_bytes <= sample.mem_total_bytes);

        // A lost connection retires its observation even though This Mac's row stays present.
        reconcile_watches(handle, true, BTreeMap::new());
        assert!(hub
            .model
            .lock()
            .unwrap()
            .projections
            .snapshot("machineStats", "all")
            .unwrap()
            .snapshot
            .is_empty());
        assert!(wait_until(|| !server.host().machine_stats_is_watched()));

        reconcile_watches(handle, true, BTreeMap::from([("local".into(), client)]));
        assert!(wait_until(|| server.host().machine_stats_is_watched()));
        super::super::drop_window(handle, "window");
        reconcile_watches(handle, false, BTreeMap::new());
        assert!(wait_until(|| !server.host().machine_stats_is_watched()));
    }
}
