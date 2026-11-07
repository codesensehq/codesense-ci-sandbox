// SPDX-License-Identifier: MIT
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::registry::{Binding, Declaration, Registry, Scope};
use super::*;

#[tokio::test]
async fn a_stalled_open_names_the_step_it_is_in() {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (entered, reached) = std::sync::mpsc::channel::<()>();
    let attempt = PinnedSshOpenAttempt::spawn(move |progress| {
        progress.enter("checking the host key");
        progress.enter("verifying the private account");
        entered.send(()).unwrap();
        let _ = wait.recv();
        Err(PinnedSshOpenError::Other("released".into()))
    });
    tokio::task::spawn_blocking(move || reached.recv().unwrap())
        .await
        .unwrap();
    assert_eq!(attempt.step(), "verifying the private account");
    let steps = attempt.progress.timings();
    assert_eq!(
        steps.iter().map(|(step, _)| *step).collect::<Vec<_>>(),
        [
            "starting",
            "checking the host key",
            "verifying the private account"
        ]
    );
    release.send(()).unwrap();
    attempt.cancel_and_reap().await.unwrap();
}

#[test]
fn rm5aa_boxed_live_layout_preserves_wire_shape() {
    let wire = serde_json::json!({
        "mode": "live",
        "member": {"machineId": "local", "slot": "sidecar"},
        "admissionRevision": "9007199254740993",
        "namespace": {"serverId": "persisted", "epoch": "lifetime", "generation": "18446744073709551615"},
        "snapshot": {
            "schema": 1, "serverId": "persisted", "serverName": "local",
            "epoch": "lifetime", "rev": 7, "workspaces": {},
            "viewer": {"active": {}, "views": {}, "hidden": []}
        }
    });
    let decoded: RuntimeLayoutReply = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(&decoded, RuntimeLayoutReply::Live { snapshot, .. } if snapshot.rev == 7));
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    let mut private_wire = wire;
    private_wire["privateAuthorityEpoch"] = "18446744073709551615".into();
    let private: RuntimeLayoutReply = serde_json::from_value(private_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(private).unwrap(), private_wire);
}

#[test]
fn rm5aa_runtime_admission_u64_dto_roundtrips_losslessly() {
    for value in [9, 10, 9_007_199_254_740_993, u64::MAX] {
        let namespace = crate::terminal::session_server::EventNamespace::for_test(
            "persisted-server",
            "runtime-epoch",
            value,
        );
        let row = RuntimeAdmissionRow::live(
            RuntimeMemberAddress::local(RuntimeMemberSlot::Sidecar),
            value,
            &namespace,
        );
        let encoded = serde_json::to_value(&row).unwrap();
        assert_eq!(encoded["revision"], value.to_string());
        assert_eq!(encoded["namespace"]["generation"], value.to_string());
        let decoded: RuntimeAdmissionRow = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, row);
        let mut private = row;
        private.private_authority_epoch = Some(value.to_string());
        let encoded = serde_json::to_value(&private).unwrap();
        assert_eq!(encoded["privateAuthorityEpoch"], value.to_string());
        assert_eq!(
            serde_json::from_value::<RuntimeAdmissionRow>(encoded).unwrap(),
            private
        );
    }
    for malformed in ["", "-1", "+1", "01", "1.0", "18446744073709551616"] {
        assert!(serde_json::from_value::<RuntimeCounter>(serde_json::json!(malformed)).is_err());
    }
    assert!(RuntimeCounter::new(9) < RuntimeCounter::new(10));
    assert!(serde_json::from_value::<RuntimeCounter>(serde_json::json!(9)).is_err());
}

#[test]
fn rm5aa_runtime_membership_dto_is_separate_from_personal_binding() {
    let namespace = crate::terminal::session_server::EventNamespace::for_test("real", "epoch", 3);
    let row = RuntimeAdmissionRow::live(
        RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
        5,
        &namespace,
    );
    let encoded = serde_json::to_value(row).unwrap();
    assert_eq!(
        encoded["member"],
        serde_json::json!({"machineId":"local","slot":"inProcess"})
    );
    assert_eq!(encoded["state"], "live");
    assert!(encoded.get("binding").is_none());
    let target = RuntimeMemberTarget {
        member: RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
        admission_revision: RuntimeCounter::new(5),
        namespace: (&namespace).into(),
    };
    let reference = RuntimeSessionReference::Admitted(Box::new(RuntimeSessionTarget {
        runtime: target,
        session: codesense_pty_core::domain::PtySessionId::new("same-wire-id"),
    }));
    let encoded = serde_json::to_value(&reference).unwrap();
    let decoded = serde_json::from_value::<RuntimeSessionReference>(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    // Historical presentation data cannot be forged into an executable target.
    // Keep wire-only serialization checks with the DTOs, not the framing path.
    let mut historical = encoded;
    historical["namespace"] = serde_json::Value::Null;
    historical["mode"] = serde_json::json!("historical");
    assert!(serde_json::from_value::<RuntimeSessionReference>(historical).is_err());
    assert!(serde_json::from_value::<RuntimeMemberAddress>(
        serde_json::json!({"machineId":"local","slot":"remote"})
    )
    .is_err());
    assert!(serde_json::from_value::<RuntimeMemberAddress>(
        serde_json::json!({"machineId":"remote-machine","slot":"sidecar"})
    )
    .is_err());
}

#[test]
fn rm4d_machine_status_serializes_phase_contract_in_camel_case() {
    assert_eq!(
        serde_json::to_value(MachineStatus::TerminalServerFrozen {
            count: None,
            replacement_supported: false,
        })
        .unwrap(),
        serde_json::json!({"state":"terminalServerFrozen","count":null,"replacementSupported":false})
    );
    assert_eq!(
        serde_json::to_value(MachineStatus::ReplacementFailed {
            old_stopped: true,
            count: Some(2),
            repair: "repair".into(),
        })
        .unwrap(),
        serde_json::json!({"state":"replacementFailed","oldStopped":true,"count":2,"repair":"repair"})
    );
}

#[test]
fn rm4d_pid_reuse_after_probe() {
    let proof = LegacyHostProof::test_proof("server-a");
    if std::env::var("RM4D_INJECT").as_deref() == Ok("rm4d_pid_reuse_after_probe") {
        assert_eq!(
            revalidate_replacement_process_proof(&proof, &proof),
            Err("replacement process identity changed after probe".into())
        );
    } else {
        let mut reused = proof.clone();
        reused.process_start += 1;
        assert!(revalidate_replacement_process_proof(&proof, &proof).is_ok());
        assert!(revalidate_replacement_process_proof(&proof, &reused).is_err());
    }
}

#[test]
fn rm4c_legacy_proof_requires_pinned_identity_manifest_digest_and_process() {
    let digest = "a".repeat(64);
    let output = format!(
        "server_id=server-a\nversion=0.0.1-v1\nmanifest={digest}\npid=42\nstart=99\ninode=8:123\nexecutable=/home/dev/.codesense/host/0.0.1-v1/codesense-session-server\nroot=/home/dev/.codesense\nuid=1000\n"
    );
    let proof = parse_legacy_host_proof(output.as_bytes(), "server-a").expect("sealed proof");
    assert_eq!(proof.server_id(), "server-a");
    for unverified in [
        output.replace("server-a", "server-b"),
        output.replace("pid=42", "pid=0"),
        output.replace("start=99", "start=0"),
        output.replace("inode=8:123", "inode=bad"),
        output.replace("executable=/", "executable=relative/"),
        output.replace("root=/", "root=relative/"),
        output.replace(&digest, "short"),
        output.replace("version=0.0.1-v1", "version=../escape"),
    ] {
        assert!(parse_legacy_host_proof(unverified.as_bytes(), "server-a").is_none());
    }
}

#[test]
fn live_manager_status_shapes_match_the_typescript_contract() {
    assert_eq!(
        serde_json::to_value(MachineStatus::Connecting).unwrap(),
        serde_json::json!({ "state": "connecting" })
    );
    assert_eq!(
        serde_json::to_value(MachineStatus::Handshaking).unwrap(),
        serde_json::json!({ "state": "handshaking" })
    );
    assert_eq!(
        serde_json::to_value(MachineStatus::Disconnected).unwrap(),
        serde_json::json!({ "state": "disconnected" })
    );
    assert_eq!(
        serde_json::to_value(MachineStatus::ConnectionFailed {
            last_error: "ssh ended".into(),
        })
        .unwrap(),
        serde_json::json!({ "state": "connectionFailed", "lastError": "ssh ended" })
    );
}

#[test]
fn command_summary_is_overlaid_from_live_manager_state() {
    let mut summary = MachineSummary {
        machine_id: "box-123456".into(),
        kind: "ssh",
        label: "Box".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Connected {
            workspaces: Some(7),
            terminal_server_update: false,
            terminal_server_sessions: None,
        },
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    };
    apply_manager_state(
        &mut summary,
        super::super::host_connection_manager::HostConnectionState::Connected {
            server_id: "server-a".into(),
        },
    );
    assert_eq!(
        summary.status,
        MachineStatus::Connected {
            workspaces: Some(7),
            terminal_server_update: false,
            terminal_server_sessions: None,
        }
    );
    apply_manager_state(
        &mut summary,
        super::super::host_connection_manager::HostConnectionState::Unreachable {
            last_error: "ssh ended".into(),
        },
    );
    assert_eq!(
        summary.status,
        MachineStatus::ConnectionFailed {
            last_error: "ssh ended".into()
        }
    );
}

/// SW-AGR-19 (AC10): a list reload re-applies the manager's state, which
/// knows nothing of the server's build; the server read from the live client
/// is applied after it, so a stale remote server keeps its update flag and
/// This Mac (whose update is its own flow) shows its server without one.
#[test]
fn agr19_a_reload_keeps_the_terminal_server_and_its_update_flag() {
    use std::collections::HashMap;
    use terminal_server_info::{remote_info, ServerRelation};
    let summary_for = |id: &str, kind: &'static str, status: MachineStatus| MachineSummary {
        machine_id: id.into(),
        kind,
        label: id.into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status,
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    };
    let connected = || MachineStatus::Connected {
        workspaces: Some(2),
        terminal_server_update: false,
        terminal_server_sessions: None,
    };
    let mut summaries = vec![
        summary_for(LOCAL_MACHINE_ID, "local", connected()),
        summary_for("stale-box", "ssh", connected()),
        summary_for("fresh-box", "ssh", connected()),
        summary_for("idle-box", "ssh", MachineStatus::Idle),
    ];
    for summary in &mut summaries[1..3] {
        apply_manager_state(
            summary,
            super::super::host_connection_manager::HostConnectionState::Connected {
                server_id: "server".into(),
            },
        );
    }
    let none = std::collections::HashSet::new();
    let minor = codesense_host_proto::PROTO_MINOR;
    let stale = remote_info(&none, "0.1.0+bbbbbbbbbbbb", minor - 1, None);
    let fresh = remote_info(
        &none,
        "0.1.0",
        minor,
        Some(&terminal_server_info::app_stamp()),
    );
    let mut local = stale.clone();
    local.relation = ServerRelation::Older;
    let infos: HashMap<_, _> = [
        (LOCAL_MACHINE_ID.to_string(), (local, Some(9))),
        ("stale-box".to_string(), (stale.clone(), Some(3))),
        ("fresh-box".to_string(), (fresh.clone(), Some(5))),
        ("idle-box".to_string(), (stale.clone(), Some(1))),
    ]
    .into();
    apply_terminal_servers(&mut summaries, &infos);
    assert_eq!(
        summaries[1].status,
        MachineStatus::Connected {
            workspaces: Some(2),
            terminal_server_update: true,
            terminal_server_sessions: Some(3),
        }
    );
    assert_eq!(summaries[1].terminal_server.as_ref(), Some(&stale));
    assert_eq!(
        summaries[2].status,
        MachineStatus::Connected {
            workspaces: Some(2),
            terminal_server_update: false,
            terminal_server_sessions: None,
        }
    );
    assert!(summaries[2]
        .terminal_server
        .as_ref()
        .is_some_and(|info| info.up_to_date));
    // SW-AGR-19 PR-5: This Mac's older server is flagged too (its update
    // is its own flow); no session count rides on it.
    assert_eq!(
        summaries[0].status,
        MachineStatus::Connected {
            workspaces: Some(2),
            terminal_server_update: true,
            terminal_server_sessions: None,
        },
    );
    assert!(summaries[0].terminal_server.is_some());
    assert_eq!(
        summaries[3].terminal_server, None,
        "not connected: no server"
    );
    let wire = serde_json::to_value(&summaries[1]).unwrap();
    assert_eq!(wire["terminalServer"]["relation"], "older");
    assert_eq!(wire["terminalServer"]["upToDate"], false);
    assert_eq!(wire["terminalServer"]["buildDigest"], "bbbbbbbbbbbb");
    assert!(serde_json::to_value(&summaries[3])
        .unwrap()
        .get("terminalServer")
        .is_none());
}

/// SW-AGR-19 (AC10), behaviourally: a machines list read twice (a reload)
/// from the registry, projected through the live manager (which resets the
/// update flag) and then given its terminal server, keeps the flag each time.
#[tokio::test]
async fn agr19_reloading_the_list_keeps_a_stale_servers_update_flag() {
    use std::collections::HashMap;
    let manager = super::super::host_connection_manager::HostConnectionManager::default();
    let record = super::super::host_connection_manager::MachineManagerRecord {
        machine_id: "box-stale".into(),
        scope: None,
        revision: None,
        fingerprint: "stale".into(),
        set_up: true,
        auto_connect: true,
        open_tabs: false,
    };
    manager
        .observe_mutation(
            manager.reserve_observation(),
            "box-stale".into(),
            Some(record),
        )
        .await;
    manager.set_state_for_test(
        "box-stale",
        super::super::host_connection_manager::HostConnectionState::Connected {
            server_id: "server".into(),
        },
    );
    let stale = terminal_server_info::remote_info(
        &Default::default(),
        "0.1.0+bbbbbbbbbbbb",
        codesense_host_proto::PROTO_MINOR - 1,
        None,
    );
    let infos: HashMap<_, _> = [("box-stale".to_string(), (stale.clone(), None))].into();
    for _reload in 0..2 {
        // What the registry reads: a set-up machine, not connected by itself.
        let mut summaries = vec![MachineSummary {
            machine_id: "box-stale".into(),
            kind: "ssh",
            label: "Stale".into(),
            scope: None,
            declaration: None,
            binding: None,
            can_publish: false,
            remote_mcp_control: false,
            status: MachineStatus::Idle,
            attention: 0,
            capabilities: MachineCapabilities {
                agent_callbacks: false,
            },
            machine_stats: None,
            machine_stats_supported: None,
            terminal_server: None,
        }];
        apply_projection_snapshot(&manager, &mut summaries);
        apply_terminal_servers(&mut summaries, &infos);
        assert!(matches!(
            summaries[0].status,
            MachineStatus::Connected {
                terminal_server_update: true,
                ..
            }
        ));
        assert_eq!(summaries[0].terminal_server.as_ref(), Some(&stale));
    }
}

/// SW-AGR-19 (AC10): every summary path reads the terminal server after the
/// manager's state (which resets the update flag), so no reload drops it.
#[test]
fn agr19_every_summary_path_reads_the_terminal_server_last() {
    let source = include_str!("mod.rs");
    let body = |name: &str| {
        let start = source.find(name).unwrap();
        let end = start + source[start..].find("\n}\n").unwrap();
        &source[start..end]
    };
    let list = body("pub async fn list_machines(");
    let projected = list.find("apply_projection_snapshot(").unwrap();
    let annotated = list
        .find("annotate_terminal_servers(&app, &mut summaries, false)")
        .unwrap();
    assert!(projected < annotated);
    let one = body("pub(crate) async fn manager_machine_summary(");
    let applied = one.find("apply_manager_state(").unwrap();
    let annotated = one.find("annotate_terminal_servers(").unwrap();
    assert!(applied < annotated);
}

#[test]
fn manager_connected_without_a_census_is_explicitly_unknown() {
    let mut summary = MachineSummary {
        machine_id: "box-unknown".into(),
        kind: "ssh",
        label: "Box".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Idle,
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    };
    apply_manager_state(
        &mut summary,
        super::super::host_connection_manager::HostConnectionState::Connected {
            server_id: "server-a".into(),
        },
    );
    assert_eq!(
        summary.status,
        MachineStatus::Connected {
            workspaces: None,
            terminal_server_update: false,
            terminal_server_sessions: None,
        }
    );
}

#[tokio::test]
async fn list_overlay_observes_state_change_after_registry_read_and_reload_keeps_failure() {
    let manager = super::super::host_connection_manager::HostConnectionManager::default();
    manager
        .observe_mutation(
            manager.reserve_observation(),
            "box-123456".into(),
            Some(
                super::super::host_connection_manager::MachineManagerRecord {
                    machine_id: "box-123456".into(),
                    scope: None,
                    revision: None,
                    fingerprint: "same".into(),
                    set_up: true,
                    auto_connect: false,
                    open_tabs: false,
                },
            ),
        )
        .await;
    let mut summaries = vec![MachineSummary {
        machine_id: "box-123456".into(),
        kind: "ssh",
        label: "Box".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Idle,
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    }];
    manager.set_failure_for_test("box-123456", "ssh ended");
    manager
        .observe_mutation(
            manager.reserve_observation(),
            "box-123456".into(),
            Some(
                super::super::host_connection_manager::MachineManagerRecord {
                    machine_id: "box-123456".into(),
                    scope: None,
                    revision: None,
                    fingerprint: "same".into(),
                    set_up: true,
                    auto_connect: false,
                    open_tabs: false,
                },
            ),
        )
        .await;
    overlay_manager_states(&manager, &mut summaries);
    assert_eq!(
        summaries[0].status,
        MachineStatus::ConnectionFailed {
            last_error: "ssh ended".into()
        }
    );
}

#[tokio::test]
async fn registry_removal_after_list_read_suppresses_the_stale_remote_ghost() {
    let manager = super::super::host_connection_manager::HostConnectionManager::default();
    let record = super::super::host_connection_manager::MachineManagerRecord {
        machine_id: "box-ghost".into(),
        scope: None,
        revision: None,
        fingerprint: "ghost".into(),
        set_up: true,
        auto_connect: false,
        open_tabs: false,
    };
    manager
        .observe_mutation(
            manager.reserve_observation(),
            "box-ghost".into(),
            Some(record),
        )
        .await;
    let mut summaries = vec![MachineSummary {
        machine_id: "box-ghost".into(),
        kind: "ssh",
        label: "Ghost".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Idle,
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    }];
    manager
        .observe_mutation(manager.reserve_observation(), "box-ghost".into(), None)
        .await;
    apply_projection_snapshot(&manager, &mut summaries);
    assert!(summaries.is_empty());
}

#[test]
fn valid_registry_row_not_yet_observed_by_manager_is_preserved() {
    let manager = super::super::host_connection_manager::HostConnectionManager::default();
    let mut summaries = vec![MachineSummary {
        machine_id: "box-new".into(),
        kind: "ssh",
        label: "New".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Idle,
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    }];
    apply_projection_snapshot(&manager, &mut summaries);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].machine_id, "box-new");
}
use crate::commands::credential_store::FakeStore;

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

fn team() -> Scope {
    Scope::Team {
        org_slug: "acme/context".into(),
        team: "platform".into(),
    }
}

/// A CodeSense home for `user`, with the acme team's context on disk.
fn env_for(root: &Path, user: u64) -> Env {
    let home = root.join(format!("home-{user}"));
    let codesense_home = home.join(".codesense");
    let registry = Registry::new(codesense_home.clone(), Principal::from(user));
    let team_dir = registry.dir(&team()).unwrap();
    std::fs::create_dir_all(team_dir.parent().unwrap()).unwrap();
    Env {
        private_authority: None,
        registry,
        codesense_home,
        home: Some(home),
        artifacts: root.join("artifacts"),
        shell_init: root.join("shell-init"),
        askpass_program: PathBuf::from("/bin/false"),
        ssh_program: PathBuf::from("ssh"),
    }
}

fn declaration(id: &str) -> Declaration {
    Declaration {
        machine_id: id.into(),
        label: "gpu-box".into(),
        host: "gpu.int".into(),
        port: 22,
        jump_host: None,
        default_base_dir: Some("~/src".into()),
        pinned_host_key: Some(KEY.into()),
        pinned_server_id: None,
    }
}

#[test]
fn manager_projection_skips_a_malformed_declaration_and_names_it() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    env.registry
        .write(&Scope::Personal, &declaration("good-123456"))
        .unwrap();
    let dir = env.registry.dir(&Scope::Personal).unwrap();
    std::fs::write(dir.join("bad-123456.yaml"), "machine_id: [not yaml").unwrap();

    let scan = env.registry.all_declarations().unwrap();
    let found: Vec<_> = scan
        .found
        .iter()
        .map(|(_, d)| d.machine_id.as_str())
        .collect();
    assert_eq!(found, ["good-123456"]);
    let why = &scan.unreadable["bad-123456"];
    assert!(why.contains("bad-123456.yaml"), "{why}");
    // The list reads the same files the same way.
    let listed: Vec<_> = env
        .registry
        .declarations(&Scope::Personal)
        .unwrap()
        .into_iter()
        .map(|d| d.machine_id)
        .collect();
    assert_eq!(listed, ["good-123456"]);
}

/// A profile with no machine access says so, not "check account enrollment",
/// which enrollment could never fix.
#[test]
fn private_setup_without_machine_access_says_so() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        ..draft(MachineSignIn::Agent)
    };
    let store = FakeStore::default();
    for error in [
        private_setup::test(&env, &store, None, &input).unwrap_err(),
        private_setup::enroll(&env, &store, None, &input).unwrap_err(),
    ] {
        assert_eq!(
            error,
            "machine access is unavailable for this profile; set up machine access in Settings"
        );
    }
}

fn draft(sign_in: MachineSignIn) -> MachineDraftInput {
    MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "Dev Box".into(),
        host: "devbox.int".into(),
        port: 2222,
        username: Some("quinn".into()),
        jump_host: None,
        sign_in,
        trusted_host_key_fp: None,
    }
}

fn keychain_sign_in(account: &str) -> MachineSignIn {
    MachineSignIn::KeychainPassphrase {
        key_path: "~/.ssh/id_ed25519".into(),
        credential_ref: CredentialRef {
            provider: "ssh".into(),
            account: account.into(),
        },
    }
}

/// Every file under `dir`, as text.
fn tree_text(dir: &Path) -> String {
    let mut out = String::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            out.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    out
}

/// A declaration serializes to exactly its secret-free fields: no user name,
/// key, passphrase, credential or token can be written into a layer.
#[test]
fn a_declaration_carries_no_secret_field() {
    let yaml = serde_yaml::to_value(declaration("gpu-box-1a2b3c")).unwrap();
    let mut keys: Vec<String> = yaml
        .as_mapping()
        .unwrap()
        .keys()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "default_base_dir",
            "host",
            "jump_host",
            "label",
            "machine_id",
            "pinned_host_key",
            "pinned_server_id",
            "port"
        ]
    );
    for key in &keys {
        for secret in [
            "user",
            "pass",
            "secret",
            "credential",
            "token",
            "identity",
            "key_path",
        ] {
            assert!(!key.contains(secret), "{key} looks like a secret");
        }
    }
    // A layer file that carries one anyway is read without it.
    let tampered = format!(
        "{}username: root\npassphrase: hunter2\n",
        serde_yaml::to_string(&declaration("gpu-box-1a2b3c")).unwrap()
    );
    let read: Declaration = serde_yaml::from_str(&tampered).unwrap();
    assert!(!format!("{read:?}").contains("hunter2"));
}

/// SLW-PT-19: a personal machine lives in the profile's own directory — never a
/// context repo and never an org's scratch space — so it is there in every org.
/// Sharing moves the declaration into the team layer with no sign-in material;
/// unsharing moves it back home.
#[test]
fn a_personal_machine_follows_the_profile_and_round_trips_share_unshare() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    store.set("handle-7", &Secret::new("pw")).unwrap();
    let personal_dir = env.registry.dir(&Scope::Personal).unwrap();
    let profile_home =
        crate::context_repo::path_to_user_home(&env.codesense_home, &Principal::from(1u64));
    assert_eq!(personal_dir.parent(), Some(profile_home.as_path()));
    assert!(!personal_dir.starts_with(profile_home.join("orgs")));
    assert!(
        !personal_dir.starts_with(crate::context_repo::scratch_space::scratch_root(
            &env.codesense_home,
            &Principal::from(1u64)
        ))
    );

    let saved = save(
        &env,
        &store,
        None,
        &draft(keychain_sign_in("handle-7")),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let id = saved.machine_id.clone();
    assert_eq!(saved.scope, Some(Scope::Personal));

    let shared = save(
        &env,
        &store,
        Some(&id),
        &draft(keychain_sign_in("handle-7")),
        &team(),
        false,
    )
    .unwrap();
    assert_eq!(shared.scope, Some(team()));
    assert!(env
        .registry
        .declarations(&Scope::Personal)
        .unwrap()
        .is_empty());
    let team_layer = env.registry.dir(&team()).unwrap();
    let published = tree_text(team_layer.parent().unwrap().parent().unwrap());
    for private in ["quinn", "id_ed25519", "handle-7"] {
        assert!(
            !published.contains(private),
            "{private} reached the team layer"
        );
    }

    let unshared = unshare(&env, &id).unwrap();
    assert_eq!(unshared.scope, Some(Scope::Personal));
    assert!(env.registry.declarations(&team()).unwrap().is_empty());
    assert_eq!(
        env.registry.declarations(&Scope::Personal).unwrap().len(),
        1
    );
    assert_eq!(
        env.registry.bindings().unwrap()[&id]
            .credential_ref
            .as_ref()
            .unwrap()
            .account,
        "handle-7",
        "the sign-in stays in the profile's own bindings throughout"
    );
}

/// Sharing a machine writes only its declaration into the team layer; the
/// user, key path and keychain handle stay in the publisher's own bindings.
#[test]
fn a_shared_machine_publishes_no_credential() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    store.set("handle-1", &Secret::new("pw")).unwrap();
    let saved = save(
        &env,
        &store,
        None,
        &draft(keychain_sign_in("handle-1")),
        &team(),
        false,
    )
    .unwrap();
    assert_eq!(saved.scope, Some(team()));
    let team_layer = env.registry.dir(&team()).unwrap();
    let published = tree_text(team_layer.parent().unwrap().parent().unwrap());
    assert!(published.contains("devbox.int"));
    for private in ["quinn", "id_ed25519", "handle-1"] {
        assert!(
            !published.contains(private),
            "{private} reached the team layer"
        );
    }
    let binding = &env.registry.bindings().unwrap()[&saved.machine_id];
    assert_eq!(binding.username.as_deref(), Some("quinn"));
    assert_eq!(binding.credential_ref.as_ref().unwrap().account, "handle-1");
}

/// A draft (closing mid-flow) is a personal "Needs setup" row under every
/// team, its binding outside every context repo.
#[test]
fn a_draft_is_personal_and_needs_setup_everywhere() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let input = draft(MachineSignIn::KeyFile {
        key_path: "~/.ssh/id_rsa".into(),
    });
    let saved = save(&env, &store, None, &input, &Scope::Personal, false).unwrap();
    assert!(
        saved.machine_id.starts_with("dev-box-"),
        "{}",
        saved.machine_id
    );
    assert!(registry::is_machine_id(&saved.machine_id));
    assert_eq!(saved.status, MachineStatus::NeedsCredential);
    assert!(!saved.binding.as_ref().unwrap().auto_connect);
    assert_eq!(
        saved.binding.as_ref().unwrap().key_path.as_deref(),
        Some("~/.ssh/id_rsa")
    );

    for (org, team) in [
        ("acme/context", Some("platform")),
        ("other/org", Some("web")),
        ("x/y", None),
    ] {
        let listed = machines_for(&env, org, team, 3).unwrap();
        assert_eq!(listed[0].machine_id, LOCAL_MACHINE_ID, "This Mac first");
        assert_eq!(
            listed[0].status,
            MachineStatus::Connected {
                workspaces: Some(3),
                terminal_server_update: false,
                terminal_server_sessions: None,
            }
        );
        assert!(listed.iter().any(|m| m.machine_id == saved.machine_id));
    }
    let orgs = env.codesense_home.join("users/1/orgs");
    assert!(
        !tree_text(&orgs).contains("id_rsa"),
        "the key path never enters a layer"
    );
}

#[test]
fn auto_connect_changes_only_through_its_explicit_personal_setting() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let saved = save(
        &env,
        &store,
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert!(!saved.binding.as_ref().unwrap().auto_connect);

    env.registry
        .update_bindings(|all| all.get_mut(&saved.machine_id).unwrap().set_up = true)
        .unwrap();

    set_auto_connect(&env, &saved.machine_id, true).unwrap();
    let changed_reach = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        host: "new-address.int".into(),
        ..draft(MachineSignIn::Agent)
    };
    let resaved = save(
        &env,
        &store,
        Some(&saved.machine_id),
        &changed_reach,
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert!(
        resaved.binding.as_ref().unwrap().auto_connect,
        "re-enrollment preserves the explicit personal preference"
    );

    // A changed reach requires enrollment again before any personal setting
    // can be edited; completing that admission does not itself change the
    // already-preserved preference.
    env.registry
        .update_bindings(|all| all.get_mut(&saved.machine_id).unwrap().set_up = true)
        .unwrap();
    set_auto_connect(&env, &saved.machine_id, false).unwrap();
    assert!(
        !summary_of(&env, &saved.machine_id)
            .unwrap()
            .binding
            .unwrap()
            .auto_connect
    );
}

#[test]
fn auto_connect_refuses_local_missing_and_needs_setup_bindings() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    assert!(set_auto_connect(&env, LOCAL_MACHINE_ID, true)
        .unwrap_err()
        .contains("This Mac"));
    env.registry
        .write(&Scope::Personal, &declaration("gpu-box-1a2b3c"))
        .unwrap();
    assert!(set_auto_connect(&env, "gpu-box-1a2b3c", true)
        .unwrap_err()
        .contains("Set up"));
    let draft = save(
        &env,
        &store,
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert!(set_auto_connect(&env, &draft.machine_id, true)
        .unwrap_err()
        .contains("Set up"));
}

#[test]
fn pinned_transport_is_raw_duplex_and_explicit_close_reaps_its_process_group() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("fake-ssh");
    let descendant_file = root.path().join("descendant");
    std::fs::write(
        &program,
        "#!/bin/sh\nsleep 30 &\necho $! > \"$DESCENDANT_FILE\"\ncat\nwait\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = install::Ssh::new(&program, Vec::new()).with_env(vec![(
        "DESCENDANT_FILE".into(),
        descendant_file.as_os_str().to_owned(),
    )]);
    let (reader, mut writer, child_guard) = spawn_pinned(ssh, "1.0.0", None).unwrap();
    let child_pid = child_guard.child.as_ref().unwrap().id() as libc::pid_t;
    writeln!(writer, "protocol-frame").unwrap();
    writer.flush().unwrap();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).unwrap();
    assert_eq!(line, "protocol-frame\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let descendant: libc::pid_t = loop {
        if let Ok(text) = std::fs::read_to_string(&descendant_file) {
            if let Ok(pid) = text.trim().parse() {
                break pid;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake ssh did not write a descendant pid to {descendant_file:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    drop(writer);
    assert_eq!(
        unsafe { libc::kill(child_pid, 0) },
        0,
        "dropping protocol I/O must not steal guard ownership"
    );
    child_guard.close_and_reap().unwrap();
    assert_ne!(
        unsafe { libc::kill(child_pid, 0) },
        0,
        "SSH child was not reaped"
    );
    // A zombie still answers kill(pid, 0), and the new parent that reaps an orphan can run
    // after close returns, so wait for the pid to disappear rather than checking once.
    assert!(
        crate::content_engine::test_support::eventually(std::time::Duration::from_secs(5), || {
            unsafe { libc::kill(descendant, 0) != 0 }
        }),
        "SSH descendant survived explicit close"
    );
}

#[test]
fn pinned_transport_spawn_failure_returns_without_a_guard_or_pipe() {
    let ssh = install::Ssh::new("/definitely/not/a/codesense-ssh", Vec::new());
    let error = match spawn_pinned(ssh, "1.0.0", None) {
        Ok(_) => panic!("a missing SSH executable unexpectedly spawned"),
        Err(error) => error,
    };
    assert!(error.contains("start SSH attach"), "{error}");
}

#[test]
fn pinned_transport_handles_natural_exit_and_drop_cancellation() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let exited = root.path().join("exited-ssh");
    std::fs::write(&exited, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exited, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, _, guard) =
        spawn_pinned(install::Ssh::new(&exited, Vec::new()), "1.0.0", None).unwrap();
    guard.close_and_reap().unwrap();

    let sleeping = root.path().join("sleeping-ssh");
    std::fs::write(&sleeping, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&sleeping, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (reader, writer, guard) =
        spawn_pinned(install::Ssh::new(&sleeping, Vec::new()), "1.0.0", None).unwrap();
    let pid = guard.child.as_ref().unwrap().id() as libc::pid_t;
    drop(reader);
    drop(writer);
    drop(guard);
    for _ in 0..100 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("cancelled SSH child {pid} was not reaped");
}

#[test]
fn bounded_drop_fallback_eventually_reaps_after_returning() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let sleeping = root.path().join("sleeping-ssh");
    std::fs::write(&sleeping, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&sleeping, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, _, guard) =
        spawn_pinned(install::Ssh::new(&sleeping, Vec::new()), "1.0.0", None).unwrap();
    let pid = guard.child.as_ref().unwrap().id() as libc::pid_t;
    DEFER_DROP_REAP.with(|defer| defer.set(true));
    drop(guard);
    DEFER_DROP_REAP.with(|defer| defer.set(false));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "detached Drop reaper did not reap SSH child {pid} within 2 seconds"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn process_guard_owns_and_cleans_up_askpass() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let askpass_root = root.path().join("askpass");
    let key = root.path().join("key");
    std::fs::write(&key, "test").unwrap();
    let askpass = Askpass::serve(&askpass_root, Path::new("/bin/false"), &key, "secret").unwrap();
    let program = root.path().join("fake-ssh");
    std::fs::write(&program, "#!/bin/sh\ncat\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, _, guard) = spawn_pinned(
        install::Ssh::new(&program, Vec::new()),
        "1.0.0",
        Some(askpass),
    )
    .unwrap();
    assert_eq!(std::fs::read_dir(&askpass_root).unwrap().count(), 1);
    guard.close_and_reap().unwrap();
    assert_eq!(std::fs::read_dir(&askpass_root).unwrap().count(), 0);
}

#[test]
fn transport_snapshot_refuses_partial_setup() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    env.registry
        .write(&Scope::Personal, &declaration(id))
        .unwrap();
    assert!(transport_snapshot(&env, id)
        .unwrap_err()
        .contains("sign-in setup"));
    env.registry
        .update_bindings(|all| {
            all.insert(id.into(), Binding::default());
        })
        .unwrap();
    assert!(transport_snapshot(&env, id)
        .unwrap_err()
        .contains("needs setup"));
}

fn assert_waits_for_machine_transaction(machine_id: &str, action: impl FnOnce() + Send + 'static) {
    let transaction = save_transaction(machine_id);
    let held = transaction.lock().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        action();
        tx.send(()).unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("machine operation worker did not start");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(30))
            .is_err(),
        "machine operation escaped the transaction lock"
    );
    drop(held);
    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("machine operation did not resume after transaction commit");
    worker.join().unwrap();
}

#[test]
fn attach_save_setter_remove_and_unshare_share_one_machine_transaction() {
    let root = tempfile::tempdir().unwrap();
    let env = std::sync::Arc::new(env_for(root.path(), 1));
    let store = std::sync::Arc::new(FakeStore::default());
    let id = "gpu-box-1a2b3c";
    let mut decl = declaration(id);
    decl.pinned_server_id = Some("server".into());
    env.registry.write(&Scope::Personal, &decl).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                id.into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths().pin(id, &HostKey::parse(KEY).unwrap()).unwrap();

    let snapshot_env = env.clone();
    assert_waits_for_machine_transaction(id, move || {
        transport_snapshot(&snapshot_env, id).unwrap();
    });
    let revalidate_env = env.clone();
    let generation = transport_snapshot(&env, id).unwrap().generation;
    assert_waits_for_machine_transaction(id, move || {
        revalidate_transport_generation(&revalidate_env, id, &generation).unwrap();
    });
    let setter_env = env.clone();
    assert_waits_for_machine_transaction(id, move || {
        set_auto_connect(&setter_env, id, true).unwrap();
    });
    let save_env = env.clone();
    let save_store = store.clone();
    let matching = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: decl.label,
        host: decl.host,
        port: decl.port,
        username: None,
        jump_host: decl.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    assert_waits_for_machine_transaction(id, move || {
        save(
            &save_env,
            save_store.as_ref(),
            Some(id),
            &matching,
            &Scope::Personal,
            false,
        )
        .unwrap();
    });
    let remove_env = env.clone();
    let remove_store = store.clone();
    assert_waits_for_machine_transaction(id, move || {
        remove(&remove_env, remove_store.as_ref(), id).unwrap();
    });

    let team_id = "team-box-1a2b3c";
    env.registry.write(&team(), &declaration(team_id)).unwrap();
    let unshare_env = env.clone();
    assert_waits_for_machine_transaction(team_id, move || {
        unshare(&unshare_env, team_id).unwrap();
    });
}

#[test]
fn generation_revalidation_tracks_save_setting_scope_and_removal() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let id = "gpu-box-1a2b3c";
    let mut decl = declaration(id);
    decl.pinned_server_id = Some("server".into());
    env.registry.write(&Scope::Personal, &decl).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                id.into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths().pin(id, &HostKey::parse(KEY).unwrap()).unwrap();

    let current = transport_snapshot(&env, id).unwrap().generation;
    assert_eq!(revalidate_transport_generation(&env, id, &current), Ok(()));
    set_auto_connect(&env, id, true).unwrap();
    assert_eq!(
        revalidate_transport_generation(&env, id, &current),
        Err(PinnedTransportGenerationError::Stale)
    );

    let before_save = transport_snapshot(&env, id).unwrap().generation;
    let changed = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "renamed".into(),
        host: decl.host.clone(),
        port: decl.port,
        username: None,
        jump_host: decl.jump_host.clone(),
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    save(&env, &store, Some(id), &changed, &Scope::Personal, false).unwrap();
    assert_eq!(
        revalidate_transport_generation(&env, id, &before_save),
        Err(PinnedTransportGenerationError::Stale)
    );

    let before_remove = transport_snapshot(&env, id).unwrap().generation;
    remove(&env, &store, id).unwrap();
    assert!(matches!(
        revalidate_transport_generation(&env, id, &before_remove),
        Err(PinnedTransportGenerationError::Missing(_))
    ));

    let team_id = "team-box-1a2b3c";
    let mut team_decl = declaration(team_id);
    team_decl.pinned_server_id = Some("server".into());
    env.registry.write(&team(), &team_decl).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                team_id.into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths()
        .pin(team_id, &HostKey::parse(KEY).unwrap())
        .unwrap();
    let before_unshare = transport_snapshot(&env, team_id).unwrap().generation;
    unshare(&env, team_id).unwrap();
    assert_eq!(
        revalidate_transport_generation(&env, team_id, &before_unshare),
        Err(PinnedTransportGenerationError::Stale)
    );
}

#[test]
fn stale_transport_generation_is_closed_before_publication() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut declaration = declaration(id);
    declaration.pinned_server_id = Some("expected-server".into());
    env.registry.write(&Scope::Personal, &declaration).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                id.into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths().pin(id, &HostKey::parse(KEY).unwrap()).unwrap();
    let snapshot = transport_snapshot(&env, id).unwrap();

    let program = root.path().join("fake-ssh");
    std::fs::write(&program, "#!/bin/sh\ncat\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (reader, writer, guard) =
        spawn_pinned(install::Ssh::new(&program, Vec::new()), "1.0.0", None).unwrap();
    let child_pid = guard.child.as_ref().unwrap().id() as libc::pid_t;
    let env = std::sync::Arc::new(env);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publish_env = env.clone();
    let publishing = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        publish_pinned_transport(
            &publish_env,
            id,
            "expected-server".into(),
            snapshot.generation,
            reader,
            writer,
            guard,
        )
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("publication interleaving did not reach its pause");
    let changed = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "replacement".into(),
        host: declaration.host,
        port: declaration.port,
        username: Some("replacement-user".into()),
        jump_host: declaration.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    save(
        &env,
        &FakeStore::default(),
        Some(id),
        &changed,
        &Scope::Personal,
        false,
    )
    .unwrap();
    release_tx.send(()).unwrap();
    let error = publishing
        .join()
        .unwrap()
        .err()
        .expect("a replaced binding must not publish its transport");
    assert!(error.contains("stale machine binding"), "{error}");
    assert_ne!(
        unsafe { libc::kill(child_pid, 0) },
        0,
        "stale SSH child was not reaped"
    );
}

/// A team's declaration syncs through the team's repo; a teammate sees the
/// machine as "Needs setup" with no binding — the publisher's never travels —
/// until they bind their own sign-in.
#[test]
fn a_teammate_sees_a_shared_machine_as_needs_setup() {
    let root = tempfile::tempdir().unwrap();
    let publisher = env_for(root.path(), 1);
    let mut published_declaration = declaration("gpu-box-1a2b3c");
    published_declaration.pinned_server_id = Some("publisher-server-id".into());
    publisher
        .registry
        .write(&team(), &published_declaration)
        .unwrap();
    publisher
        .registry
        .update_bindings(|all| {
            all.insert(
                "gpu-box-1a2b3c".into(),
                Binding {
                    username: Some("quinn".into()),
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    assert_eq!(
        machines_for(&publisher, "acme/context", Some("platform"), 0).unwrap()[1].status,
        MachineStatus::Idle
    );

    // Sync: the org's context repo is committed and cloned by a teammate.
    let git = |dir: &Path, args: &[&str]| {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    let org_root = |env: &Env, user: u64| {
        crate::context_repo::path_to_org_root(
            &env.codesense_home,
            &Principal::from(user),
            "acme/context",
        )
        .unwrap()
    };
    let source = org_root(&publisher, 1);
    git(&source, &["init", "-q"]);
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-q", "-m", "machines"]);
    let teammate = env_for(root.path(), 2);
    let target = org_root(&teammate, 2);
    std::fs::remove_dir_all(&target).unwrap();
    git(
        target.parent().unwrap(),
        &["clone", "-q", source.to_str().unwrap(), "context"],
    );

    let seen = machines_for(&teammate, "acme/context", Some("platform"), 0).unwrap();
    let gpu = seen
        .iter()
        .find(|m| m.machine_id == "gpu-box-1a2b3c")
        .unwrap();
    assert_eq!(gpu.status, MachineStatus::NeedsCredential);
    assert_eq!(gpu.binding, None, "the publisher's binding never syncs");
    assert_eq!(gpu.scope, Some(team()));
    assert!(gpu
        .declaration
        .as_ref()
        .unwrap()
        .pinned_host_key_fp
        .as_ref()
        .unwrap()
        .starts_with("SHA256:"));
    assert_eq!(
        gpu.declaration
            .as_ref()
            .unwrap()
            .pinned_server_id
            .as_deref(),
        Some("publisher-server-id")
    );

    teammate
        .registry
        .update_bindings(|all| {
            all.insert(
                "gpu-box-1a2b3c".into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    set_auto_connect(&teammate, "gpu-box-1a2b3c", true).unwrap();
    assert!(teammate.registry.bindings().unwrap()["gpu-box-1a2b3c"].auto_connect);
    assert!(
        !publisher.registry.bindings().unwrap()["gpu-box-1a2b3c"].auto_connect,
        "a member's personal preference must not change the publisher's"
    );
    teammate
        .registry
        .update_bindings(|all| {
            let binding = all.get_mut("gpu-box-1a2b3c").unwrap();
            binding.set_up = false;
            binding.auto_connect = false;
        })
        .unwrap();

    // Hiding it is the teammate's own, and it still needs setting up.
    set_hidden(&teammate, "gpu-box-1a2b3c", true).unwrap();
    let hidden = machines_for(&teammate, "acme/context", Some("platform"), 0).unwrap();
    let gpu = hidden
        .iter()
        .find(|m| m.machine_id == "gpu-box-1a2b3c")
        .unwrap();
    assert!(gpu.binding.as_ref().unwrap().hidden);
    assert_eq!(gpu.status, MachineStatus::NeedsCredential);
    assert!(!publisher.registry.bindings().unwrap()["gpu-box-1a2b3c"].hidden);
}

#[test]
fn unshare_moves_the_declaration_home_and_remove_forgets_everything() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    store.set("handle-9", &Secret::new("pw")).unwrap();
    env.registry
        .write(&team(), &declaration("gpu-box-1a2b3c"))
        .unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                "gpu-box-1a2b3c".into(),
                Binding {
                    credential_ref: Some(CredentialRef {
                        provider: "ssh".into(),
                        account: "handle-9".into(),
                    }),
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    let pin = env.paths().known_hosts_dir.join("codesense-gpu-box-1a2b3c");
    std::fs::create_dir_all(pin.parent().unwrap()).unwrap();
    std::fs::write(&pin, "pin").unwrap();

    let unshared = unshare(&env, "gpu-box-1a2b3c").unwrap();
    assert_eq!(unshared.scope, Some(Scope::Personal));
    assert!(env.registry.declarations(&team()).unwrap().is_empty());
    assert!(
        unshare(&env, "gpu-box-1a2b3c").is_err(),
        "a personal machine isn't shared"
    );

    remove(&env, &store, "gpu-box-1a2b3c").unwrap();
    assert!(env.registry.find("gpu-box-1a2b3c").unwrap().is_none());
    assert!(!env
        .registry
        .bindings()
        .unwrap()
        .contains_key("gpu-box-1a2b3c"));
    assert!(
        store.get("handle-9").unwrap().is_none(),
        "the keychain entry is gone"
    );
    assert!(!pin.exists(), "the host-key pin is gone");
    assert!(remove(&env, &store, "gpu-box-1a2b3c").is_err());
    assert!(set_hidden(&env, "../escape", true).is_err());
}

#[test]
fn unshare_never_overwrites_a_colliding_personal_declaration() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let mut personal = declaration("gpu-box-1a2b3c");
    personal.label = "personal".into();
    let mut shared = declaration("gpu-box-1a2b3c");
    shared.label = "shared".into();
    env.registry.write(&Scope::Personal, &personal).unwrap();
    env.registry.write(&team(), &shared).unwrap();

    assert!(unshare(&env, "gpu-box-1a2b3c").is_err());
    assert_eq!(
        env.registry.declarations(&Scope::Personal).unwrap()[0].label,
        "personal"
    );
    assert_eq!(
        env.registry.declarations(&team()).unwrap()[0].label,
        "shared"
    );
}

#[tokio::test]
async fn cancelled_open_attempt_awaits_post_spawn_normal_reap() {
    let (pid_tx, pid_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let attempt = PinnedSshOpenAttempt::test_spawn(move || {
        let (transport, pid) = test_pinned_transport(std::path::Path::new("/bin/cat"))?;
        pid_tx.send(pid).unwrap();
        release_rx.recv().unwrap();
        Ok(transport)
    });
    let pid = pid_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    let cleanup = tokio::spawn(attempt.cancel_and_reap());
    tokio::task::yield_now().await;
    assert!(!cleanup.is_finished(), "timeout waits for the owned worker");
    release_tx.send(()).unwrap();
    cleanup.await.unwrap().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn remove_refuses_unreadable_bindings_before_deleting_the_declaration() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    env.registry
        .write(&Scope::Personal, &declaration("gpu-box-1a2b3c"))
        .unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert("gpu-box-1a2b3c".into(), Binding::default());
        })
        .unwrap();
    let bindings = walkdir::WalkDir::new(&env.codesense_home)
        .into_iter()
        .flatten()
        .map(|entry| entry.into_path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "machine-bindings.json")
        })
        .unwrap();
    std::fs::write(&bindings, "not json").unwrap();

    assert!(remove(&env, &store, "gpu-box-1a2b3c").is_err());
    assert!(env.registry.find("gpu-box-1a2b3c").unwrap().is_some());
}

/// Binding a new passphrase forgets the old one.
#[test]
fn a_replaced_passphrase_leaves_the_keychain() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    store.set("old", &Secret::new("a")).unwrap();
    store.set("new", &Secret::new("b")).unwrap();
    let first = save(
        &env,
        &store,
        None,
        &draft(keychain_sign_in("old")),
        &Scope::Personal,
        false,
    )
    .unwrap();
    save(
        &env,
        &store,
        Some(&first.machine_id),
        &draft(keychain_sign_in("new")),
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert!(store.get("old").unwrap().is_none());
    assert_eq!(store.get("new").unwrap().unwrap().expose(), "b");
}

/// The dialog's answer goes to the keychain only once it unlocks the key;
/// only a handle comes back.
#[test]
fn a_captured_passphrase_is_kept_only_when_it_unlocks_the_key() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let cancelled = capture_passphrase(&env, &store, "~/k", |_| Ok(None), |_, _| Ok(())).unwrap();
    assert_eq!(cancelled, None);

    let wrong = capture_passphrase(
        &env,
        &store,
        "~/k",
        |_| Ok(Some(Secret::new("nope"))),
        |_, _| Err("That passphrase doesn't unlock it".into()),
    );
    assert!(wrong.is_err());

    let mut asked = None;
    let handle = capture_passphrase(
        &env,
        &store,
        "~/k",
        |key| {
            asked = Some(key.to_path_buf());
            Ok(Some(Secret::new("correct horse")))
        },
        |_, secret| {
            assert_eq!(secret.expose(), "correct horse");
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(asked, Some(env.home.clone().unwrap().join("k")));
    assert_eq!(handle.provider, "ssh");
    assert!(!handle.account.contains("correct"));
    assert_eq!(
        store.get(&handle.account).unwrap().unwrap().expose(),
        "correct horse"
    );
    assert!(!serde_json::to_string(&handle).unwrap().contains("correct"));
}

/// The IPC input carries a keychain handle, never a passphrase field.
#[test]
fn the_sign_in_input_has_no_passphrase_field() {
    let json = serde_json::json!({
        "method": "keychainPassphrase",
        "keyPath": "~/.ssh/id",
        "credentialRef": { "provider": "ssh", "account": "h" },
    });
    let parsed: MachineSignIn = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, keychain_sign_in("h").with_key_path("~/.ssh/id"));
    let old =
        serde_json::json!({ "method": "keychainPassphrase", "keyPath": "k", "passphrase": "x" });
    assert!(serde_json::from_value::<MachineSignIn>(old).is_err());
}

impl MachineSignIn {
    fn with_key_path(self, key: &str) -> Self {
        match self {
            Self::KeychainPassphrase { credential_ref, .. } => Self::KeychainPassphrase {
                key_path: key.into(),
                credential_ref,
            },
            other => other,
        }
    }
}

/// The summary's JSON is the UI's `MachineSummary`.
#[test]
fn a_summary_serializes_as_the_ui_contract() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    env.registry
        .write(&team(), &declaration("gpu-box-1a2b3c"))
        .unwrap();
    let listed = machines_for(&env, "acme/context", Some("platform"), 2).unwrap();
    let json = serde_json::to_value(&listed).unwrap();
    assert_eq!(json[0]["kind"], "local");
    assert_eq!(
        json[0]["status"],
        serde_json::json!({ "state": "connected", "workspaces": 2 })
    );
    let gpu = &json[1];
    assert_eq!(gpu["machineId"], "gpu-box-1a2b3c");
    assert_eq!(gpu["kind"], "ssh");
    assert_eq!(
        gpu["scope"],
        serde_json::json!({ "kind": "team", "orgSlug": "acme/context", "team": "platform" })
    );
    assert_eq!(gpu["declaration"]["jumpHost"], serde_json::Value::Null);
    assert!(gpu["declaration"]["pinnedHostKeyFp"]
        .as_str()
        .unwrap()
        .starts_with("SHA256:"));
    assert_eq!(
        gpu["status"],
        serde_json::json!({ "state": "needsCredential" })
    );
    assert_eq!(gpu["canPublish"], true);
    assert_eq!(gpu["capabilities"]["agentCallbacks"], false);

    let personal: Scope =
        serde_json::from_value(serde_json::json!({ "kind": "personal" })).unwrap();
    assert_eq!(personal, Scope::Personal);
}

/// A scope never names a path outside the user's orgs.
#[test]
fn a_scope_cannot_escape_its_layer() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    for (org, team) in [
        ("../..", "x"),
        ("acme/context", ".."),
        ("acme/context", "a/b"),
        ("", "t"),
    ] {
        let scope = Scope::Team {
            org_slug: org.into(),
            team: team.into(),
        };
        assert!(env.registry.dir(&scope).is_err(), "{org} {team}");
    }
    let bad = Declaration {
        machine_id: "../../x".into(),
        ..declaration("x")
    };
    assert!(env.registry.write(&Scope::Personal, &bad).is_err());
    assert!(!env.registry.can_publish(&Scope::Team {
        org_slug: "nobody/here".into(),
        team: "t".into()
    }));
}

/// A malformed or option-shaped declaration from a layer is skipped, never
/// handed to `ssh`.
#[test]
fn a_bad_declaration_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    env.registry
        .write(&team(), &declaration("gpu-box-1a2b3c"))
        .unwrap();
    let dir = env.registry.dir(&team()).unwrap();
    std::fs::write(
        dir.join("evil-1a2b3c.yaml"),
        "machine_id: evil-1a2b3c\nlabel: e\nhost: -oProxyCommand=touch /tmp/x\nport: 22\n",
    )
    .unwrap();
    std::fs::write(dir.join("junk-1a2b3c.yaml"), "not: [yaml").unwrap();
    std::fs::write(
        dir.join("other-name.yaml"),
        serde_yaml::to_string(&declaration("gpu-box-9z9z9z")).unwrap(),
    )
    .unwrap();
    let ids: Vec<String> = env
        .registry
        .declarations(&team())
        .unwrap()
        .into_iter()
        .map(|d| d.machine_id)
        .collect();
    assert_eq!(ids, ["gpu-box-1a2b3c"]);
}

/// A saved machine moved to a new address is a new machine as far as trust
/// goes: its old host key and terminal-server pins no longer apply.
#[test]
fn a_moved_machine_drops_its_old_pins() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let mut original = declaration("gpu-box-1a2b3c");
    original.pinned_server_id = Some("srv-old".into());
    env.registry.write(&Scope::Personal, &original).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                "gpu-box-1a2b3c".into(),
                Binding {
                    set_up: true,
                    auto_connect: true,
                    observed_server_id: Some("srv-old".into()),
                    username: Some("quinn".into()),
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    let old = declaration("gpu-box-1a2b3c");
    let same = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        host: old.host.clone(),
        port: old.port,
        ..draft(MachineSignIn::Agent)
    };
    assert!(pinned_key(Some(&old), &same).unwrap().is_some());
    let moved = draft(MachineSignIn::Agent);
    assert!(
        pinned_key(Some(&old), &moved).unwrap().is_none(),
        "a new address is a first sight"
    );

    let kept = save(
        &env,
        &store,
        Some("gpu-box-1a2b3c"),
        &same,
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert_eq!(
        kept.status,
        MachineStatus::Idle,
        "a re-save keeps a set-up machine set up"
    );
    let other_user = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        username: Some("root".into()),
        ..same.clone()
    };
    let changed = save(
        &env,
        &store,
        Some("gpu-box-1a2b3c"),
        &other_user,
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert_eq!(
        changed.status,
        MachineStatus::NeedsCredential,
        "a new sign-in is checked again"
    );
    let saved = save(
        &env,
        &store,
        Some("gpu-box-1a2b3c"),
        &moved,
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert_eq!(saved.declaration.as_ref().unwrap().pinned_host_key_fp, None);
    assert_eq!(saved.status, MachineStatus::NeedsCredential);
    let binding = &env.registry.bindings().unwrap()["gpu-box-1a2b3c"];
    assert_eq!(binding.observed_server_id, None);
    assert_eq!(binding.accepted_identity.as_ref().unwrap().host, moved.host);
}

/// Re-saving an unchanged team machine (a teammate setting it up) leaves the
/// team's declaration file alone.
#[test]
fn setting_up_a_shared_machine_rewrites_nothing_teammates_see() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let mut decl = declaration("gpu-box-1a2b3c");
    decl.pinned_host_key = None;
    env.registry.write(&team(), &decl).unwrap();
    let file = env
        .registry
        .dir(&team())
        .unwrap()
        .join("gpu-box-1a2b3c.yaml");
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: decl.label.clone(),
        host: decl.host.clone(),
        port: decl.port,
        ..draft(MachineSignIn::Agent)
    };
    save(&env, &store, Some("gpu-box-1a2b3c"), &input, &team(), false).unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
}

/// The checklist report, the install result and the form input cross IPC
/// in the UI's own field names.
#[test]
fn the_checklist_and_install_serialize_as_the_ui_contract() {
    use codesense_session_server::check::{InstallOffer, Report, Step as CheckStep};
    let report = Report {
        steps: vec![CheckStep {
            id: StepId::HostKey,
            status: Status::NeedsTrust,
            detail: "first sight".into(),
            next_steps: vec![NextStep::UseJumpHost],
            fingerprint: Some("SHA256:x".into()),
        }],
        install: Some(InstallOffer {
            size_bytes: 3,
            target_dir: "~/.codesense/host/v".into(),
            commands: vec!["ssh …".into()],
        }),
        file_view_only: true,
        host_key: None,
        version: None,
    };
    assert_eq!(
        serde_json::to_value(report_dto(&report)).unwrap(),
        serde_json::json!({
            "steps": [{
                "id": "hostKey",
                "status": "needsTrust",
                "detail": "first sight",
                "nextSteps": ["useJumpHost"],
                "fingerprint": "SHA256:x",
            }],
            "installPlan": { "sizeBytes": 3, "targetDir": "~/.codesense/host/v", "commands": ["ssh …"] },
            "fileViewOnly": true,
        })
    );
    let installed = InstallTerminalServerResult::Installed {
        ok: true,
        version: "v".into(),
        logout_warning: Some("w".into()),
    };
    assert_eq!(
        serde_json::to_value(installed).unwrap(),
        serde_json::json!({ "ok": true, "version": "v", "logoutWarning": "w" })
    );
    let failed = InstallTerminalServerResult::Failed {
        ok: false,
        code: "interrupted",
        message: "m".into(),
    };
    assert_eq!(
        serde_json::to_value(failed).unwrap(),
        serde_json::json!({ "ok": false, "code": "interrupted", "message": "m" })
    );
    let input: MachineDraftInput = serde_json::from_value(serde_json::json!({
        "label": "l", "host": "h", "port": 22, "username": "u", "jumpHost": "j",
        "signIn": { "method": "keyFile", "keyPath": "~/k" },
        "trustedHostKeyFp": "SHA256:t",
    }))
    .unwrap();
    assert_eq!(input.jump_host.as_deref(), Some("j"));
    assert_eq!(input.trusted_host_key_fp.as_deref(), Some("SHA256:t"));
}

/// Only a broken connection is worth resuming; a refused sign-in is not.
#[test]
fn an_install_failure_says_whether_to_resume() {
    let refused = "`set -e` failed (exit status: 255): dev@box: Permission denied (publickey).";
    assert_eq!(install_failure_code(refused), "failed");
    let dropped = "upload of \"/b\" failed (exit status: 255): Connection reset by peer";
    assert_eq!(install_failure_code(dropped), "interrupted");
    let hung = "\"ssh\" did not finish within 120 s";
    assert_eq!(install_failure_code(hung), "interrupted");
    let jump = "`set -e` failed (exit status: 255): Host key verification failed.";
    assert_eq!(install_failure_code(jump), "failed");
    let full = "upload of \"/b\" failed (exit status: 1): No space left on device";
    assert_eq!(install_failure_code(full), "failed");
}

/// A declaration someone committed with a secret in it is refused, not read.
#[test]
fn a_declaration_carrying_a_secret_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let dir = env.registry.dir(&team()).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let clean = serde_yaml::to_string(&declaration("gpu-box-1a2b3c")).unwrap();
    for smuggled in [
        "username: root\n",
        "extra:\n  pw: hunter2\n",
        "auth: {}\n",
        "extra: !x {pw: hunter2}\n",
        "!x pw: hunter2\n",
    ] {
        std::fs::write(
            dir.join("gpu-box-1a2b3c.yaml"),
            format!("{clean}{smuggled}"),
        )
        .unwrap();
        assert!(
            env.registry.declarations(&team()).unwrap().is_empty(),
            "{smuggled}"
        );
    }
    std::fs::write(dir.join("gpu-box-1a2b3c.yaml"), clean).unwrap();
    assert_eq!(env.registry.declarations(&team()).unwrap().len(), 1);
}

/// A passphrase kept but never bound (the panel moved on) leaves the
/// keychain after a day; a bound one stays with its binding.
#[test]
fn an_unbound_passphrase_is_forgotten_after_a_day() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    for account in ["stale", "fresh", "bound"] {
        store.set(account, &Secret::new("pw")).unwrap();
    }
    let day_ago = now_secs() - UNBOUND_PASSPHRASE_SECS - 1;
    env.registry
        .update_issued_passphrases(|issued| {
            issued.insert("stale".into(), day_ago);
            issued.insert("bound".into(), day_ago);
            issued.insert("fresh".into(), now_secs());
        })
        .unwrap();
    save(
        &env,
        &store,
        None,
        &draft(keychain_sign_in("bound")),
        &Scope::Personal,
        false,
    )
    .unwrap();
    assert!(
        store.get("stale").unwrap().is_none(),
        "the dropped one is gone"
    );
    assert!(
        store.get("fresh").unwrap().is_some(),
        "one still in a panel stays"
    );
    assert!(
        store.get("bound").unwrap().is_some(),
        "a bound one belongs to its machine"
    );
    let issued = env
        .registry
        .update_issued_passphrases(|issued| issued.clone())
        .unwrap();
    assert_eq!(issued.keys().collect::<Vec<_>>(), ["fresh"]);
}

/// Concurrent binding changes are never lost.
#[test]
fn concurrent_binding_changes_all_land() {
    let root = tempfile::tempdir().unwrap();
    let env = std::sync::Arc::new(env_for(root.path(), 1));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let env = env.clone();
            std::thread::spawn(move || {
                env.registry
                    .update_bindings(|all| {
                        all.insert(format!("m-{n}"), Binding::default());
                    })
                    .unwrap();
            })
        })
        .collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(env.registry.bindings().unwrap().len(), 8);
}

#[test]
fn concurrent_declaration_writes_never_share_a_staging_file() {
    let root = tempfile::tempdir().unwrap();
    let env = std::sync::Arc::new(env_for(root.path(), 1));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let env = env.clone();
            std::thread::spawn(move || {
                let mut decl = declaration("gpu-box-1a2b3c");
                decl.label = format!("writer-{n}");
                env.registry.write(&Scope::Personal, &decl).unwrap();
            })
        })
        .collect();
    threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());
    let declarations = env.registry.declarations(&Scope::Personal).unwrap();
    assert_eq!(declarations.len(), 1);
    assert!(declarations[0].label.starts_with("writer-"));
}

#[test]
fn concurrent_saves_commit_one_coherent_machine_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let env = std::sync::Arc::new(env_for(root.path(), 1));
    let store = std::sync::Arc::new(FakeStore::default());
    env.registry
        .write(&Scope::Personal, &declaration("gpu-box-1a2b3c"))
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let env = env.clone();
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut input = draft(MachineSignIn::Agent);
                input.label = format!("writer-{n}");
                input.host = format!("host-{n}.int");
                input.username = Some(format!("user-{n}"));
                barrier.wait();
                save(
                    &env,
                    store.as_ref(),
                    Some("gpu-box-1a2b3c"),
                    &input,
                    &Scope::Personal,
                    false,
                )
                .unwrap();
            })
        })
        .collect();
    threads
        .into_iter()
        .for_each(|thread| thread.join().unwrap());

    let declaration = env.registry.find("gpu-box-1a2b3c").unwrap().unwrap().1;
    let binding = env
        .registry
        .bindings()
        .unwrap()
        .remove("gpu-box-1a2b3c")
        .unwrap();
    let n = declaration.label.strip_prefix("writer-").unwrap();
    assert_eq!(declaration.host, format!("host-{n}.int"));
    let expected_user = format!("user-{n}");
    assert_eq!(binding.username.as_deref(), Some(expected_user.as_str()));
}

#[test]
fn a_late_save_failure_rolls_back_and_reports_rollback_failure() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let original = save(
        &env,
        &store,
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let before_decl = env.registry.find(&original.machine_id).unwrap().unwrap().1;
    let before_binding = env.registry.bindings().unwrap()[&original.machine_id].clone();
    let changed = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "Changed".into(),
        username: Some("other-user".into()),
        ..draft(MachineSignIn::Agent)
    };

    env.registry.fail_next_declaration_write_for_test();
    let error = save(
        &env,
        &store,
        Some(&original.machine_id),
        &changed,
        &Scope::Personal,
        false,
    )
    .unwrap_err();
    assert!(
        error.contains("injected declaration write failure"),
        "{error}"
    );
    assert_eq!(
        env.registry.find(&original.machine_id).unwrap().unwrap().1,
        before_decl
    );
    assert_eq!(
        env.registry.bindings().unwrap()[&original.machine_id],
        before_binding
    );

    env.registry.fail_next_declaration_write_for_test();
    env.registry.fail_binding_update_on_for_test(2);
    let error = save(
        &env,
        &store,
        Some(&original.machine_id),
        &changed,
        &Scope::Personal,
        false,
    )
    .unwrap_err();
    assert!(error.contains("rollback also failed"), "{error}");
    assert!(error.contains("injected binding update failure"), "{error}");
}

#[test]
fn a_pin_snapshot_failure_prevents_every_save_mutation() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let store = FakeStore::default();
    let original = save(
        &env,
        &store,
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let before_decl = env.registry.find(&original.machine_id).unwrap().unwrap().1;
    let before_binding = env.registry.bindings().unwrap()[&original.machine_id].clone();
    let pin = env.paths().pin_file(&original.machine_id);
    std::fs::create_dir_all(pin.parent().unwrap()).unwrap();
    // A directory at the pin path deterministically injects a snapshot read
    // failure without relying on platform-specific permission semantics.
    std::fs::create_dir(&pin).unwrap();
    let changed = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "Must not commit".into(),
        username: Some("must-not-commit".into()),
        ..draft(MachineSignIn::Agent)
    };

    let error = save(
        &env,
        &store,
        Some(&original.machine_id),
        &changed,
        &Scope::Personal,
        false,
    )
    .unwrap_err();
    assert!(error.contains("read existing host-key pin"), "{error}");
    assert!(pin.is_dir(), "the unreadable pin placeholder was changed");
    assert_eq!(
        env.registry.find(&original.machine_id).unwrap().unwrap().1,
        before_decl
    );
    assert_eq!(
        env.registry.bindings().unwrap()[&original.machine_id],
        before_binding
    );
}

/// The app's own operations over real SSH: the Add-machine flow as the UI
/// drives it, against an sshd test container.
pub(super) mod container {
    include!("mcp_ssh_tests.rs");
    include!("workspace_ssh_tests.rs");
    include!("hydration_ssh_tests.rs");
    include!("explicit_sync_perf_ssh_tests.rs");
    include!("routine_launch_ssh_tests.rs");
    include!("routine_wake_ssh_tests.rs");
    include!("machine_arm_ssh_tests.rs");
    include!("compatible_activation_ssh_fixture.rs");
    include!("executor_ssh_tests.rs");
    include!("reverse_mcp_ssh_tests.rs");
    include!("mcp_relay_ssh_tests.rs");
    include!("bridge_reconnect_ssh_tests.rs");
    include!("inbox_wait_ssh_tests.rs");
    include!("reinstall_ssh_tests.rs");
    use super::*;
    use std::sync::Arc;

    const NEEDS: &str = "needs an sshd test container";

    fn var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} unset: {NEEDS}"))
    }

    fn docker_as_dev(script: &str) {
        let mut command = Command::new("docker");
        command.arg("exec");
        if let Ok(token) = std::env::var("RM4F_OWNER_TOKEN") {
            assert!(token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()));
            command.args(["--env", &format!("RM4F_OWNER_TOKEN={token}")]);
        }
        let status = command
            .args([
                &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                "su",
                "--whitelist-environment=RM4F_OWNER_TOKEN",
                "dev",
                "-c",
                script,
            ])
            .status()
            .unwrap();
        assert!(status.success(), "{script}");
    }

    fn docker_as_root(script: &str) {
        let status = Command::new("docker")
            .args([
                "exec",
                &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                "sh",
                "-c",
                script,
            ])
            .status()
            .unwrap();
        assert!(status.success(), "{script}");
    }

    struct ContainerFile(&'static str);

    impl Drop for ContainerFile {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                    "rm",
                    "-f",
                    self.0,
                ])
                .status();
        }
    }

    /// A user's app environment, with this build's terminal server.
    pub(in crate::commands::machines) fn env(root: &Path) -> Env {
        let mut env = env_for(root, 7);
        if let Some(program) = std::env::var_os("RM4F_SSH_PROGRAM") {
            env.ssh_program = PathBuf::from(program);
        }
        env.artifacts = PathBuf::from(var("CODESENSE_REMOTE_HOST_ARTIFACTS"));
        env.shell_init =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/shell-integration");
        env.askpass_program = PathBuf::from(var("CODESENSE_REMOTE_HOST_ASKPASS"));
        env
    }

    fn input(sign_in: MachineSignIn, trusted: Option<String>) -> MachineDraftInput {
        input_on_port(
            sign_in,
            trusted,
            var("CODESENSE_REMOTE_HOST_PORT").parse().unwrap(),
        )
    }

    fn input_on_port(
        sign_in: MachineSignIn,
        trusted: Option<String>,
        port: u16,
    ) -> MachineDraftInput {
        MachineDraftInput {
            mode: Some(MachineMode::Legacy),
            label: "container".into(),
            host: std::env::var("CODESENSE_REMOTE_HOST_HOST")
                .unwrap_or_else(|_| "127.0.0.1".into()),
            port,
            username: Some("dev".into()),
            jump_host: None,
            sign_in,
            trusted_host_key_fp: trusted,
        }
    }

    fn key_file() -> MachineSignIn {
        MachineSignIn::KeyFile {
            key_path: var("CODESENSE_REMOTE_HOST_KEY"),
        }
    }

    fn status_of<'a>(report: &'a MachineTestReportDto, id: &str) -> &'a str {
        report.steps.iter().find(|s| s.id == id).unwrap().status
    }

    #[test]
    #[ignore = "needs two sshd test containers"]
    fn rm4e_two_ssh_hosts_reusing_ids_remain_isolated() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::commands::pty_host::{StreamHost, StreamIds};
        use crate::terminal::session_server::{
            Ledger, NamespacedTerminalEvent, NamespacedTerminalEventSink, SessionServerClient,
        };
        use codesense_pty_core::domain::{
            PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, ViewportId,
        };
        use codesense_session_server::testing::{registration, sh, wait_until};
        use std::sync::Mutex;
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        #[derive(Default)]
        struct RoutingSink {
            streams: StreamIds,
            sent: Mutex<Vec<(u64, String)>>,
        }
        impl NamespacedTerminalEventSink for RoutingSink {
            fn emit(&self, event: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
                if let TerminalDomainEvent::StreamOutput { stream, data, seq } = event.event {
                    self.streams.deliver_output(
                        StreamHost::remote_namespace(&event.namespace),
                        stream,
                        data,
                        seq,
                        &mut |local, data, _| {
                            self.sent.lock().unwrap().push((local, data.into()));
                            true
                        },
                    );
                }
                Ok(())
            }
        }
        impl RoutingSink {
            fn output(&self, local: u64) -> String {
                self.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(to, _)| *to == local)
                    .map(|(_, data)| data.as_str())
                    .collect()
            }
        }

        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = Arc::new(env(root.path()));
        let store = Arc::new(FakeStore::default());
        let ports = [
            var("CODESENSE_REMOTE_HOST_PORT").parse().unwrap(),
            var("CODESENSE_REMOTE_HOST_PORT_2").parse().unwrap(),
        ];
        let mut saved = Vec::new();
        for (index, port) in ports.into_iter().enumerate() {
            let mut draft = input_on_port(key_file(), None, port);
            if index == 1 {
                if let Ok(host) = std::env::var("CODESENSE_REMOTE_HOST_HOST_2") {
                    draft.host = host;
                }
                if let Ok(key_path) = std::env::var("CODESENSE_REMOTE_HOST_KEY_2") {
                    draft.sign_in = MachineSignIn::KeyFile { key_path };
                }
            }
            let first = test_connection(&env, store.as_ref(), None, &draft).unwrap();
            let mut trusted = draft;
            trusted.trusted_host_key_fp = first.steps[1].fingerprint.clone();
            install_server(&env, store.as_ref(), None, &trusted).unwrap();
            saved.push(save(&env, store.as_ref(), None, &trusted, &Scope::Personal, true).unwrap());
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut managed = Vec::new();
            let routing = Arc::new(RoutingSink::default());
            for machine in &saved {
                let admitted = RemoteTransportOwner::from_pinned(
                    open_pinned_saved(&env, store.as_ref(), &machine.machine_id).unwrap(),
                )
                .into_pending()
                .admit(Duration::from_secs(5), CancellationToken::new())
                .await
                .unwrap();
                let client = SessionServerClient::from_remote_admitted(
                    admitted,
                    routing.clone(),
                    Arc::<Ledger>::default(),
                    None,
                )
                .await
                .unwrap();
                client.publication_permit().publish();
                managed.push(client);
            }
            assert_ne!(
                managed[0].domain_client().namespace().server_id,
                managed[1].domain_client().namespace().server_id,
                "the two verified SSH hosts must have distinct server identities"
            );
            for (index, client) in managed.iter().enumerate() {
                let sentinel = if index == 0 { "RM4E_A" } else { "RM4E_B" };
                let mut intent = sh(&format!(
                    "printf '{sentinel}_OPEN\\n'; read line; printf '{sentinel}_LIVE\\n'; read line; printf '{sentinel}_RECONNECTED\\n'; sleep 30"
                ));
                intent.shell = Some("/bin/bash".into());
                intent.cwd = "/tmp".into();
                client
                    .domain_client()
                    .spawn(intent, registration("same-session", Some("rm4e"), false))
                    .unwrap();
            }
            let viewport = ViewportId::new("rm4e-shared-viewport");
            let subscribed = managed
                .iter()
                .map(|client| {
                    client
                        .domain_client()
                        .subscribe(&PtySessionId::new("same-session"), &viewport)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                subscribed[0].stream_id, subscribed[1].stream_id,
                "the two independent servers must reuse the intended wire stream id"
            );
            let locals = managed
                .iter()
                .zip(&subscribed)
                .map(|(client, result)| {
                    routing.streams.attach(
                        StreamHost::remote_namespace(client.domain_client().namespace()),
                        &PtySessionId::new("same-session"),
                        &viewport,
                        result,
                    )
                })
                .collect::<Vec<_>>();
            assert_ne!(locals[0], locals[1]);
            for client in &managed {
                client
                    .domain_client()
                    .write(&PtySessionId::new("same-session"), b"live\r")
                    .unwrap();
            }
            assert!(wait_until(|| routing.output(locals[0]).contains("RM4E_A_LIVE")));
            assert!(wait_until(|| routing.output(locals[1]).contains("RM4E_B_LIVE")));
            assert!(!routing.output(locals[0]).contains("RM4E_B"));
            assert!(!routing.output(locals[1]).contains("RM4E_A"));
            let old_namespace = managed[0].domain_client().namespace().clone();
            let first = managed.remove(0);
            first.close_awaited().await;
            let admitted = RemoteTransportOwner::from_pinned(
                open_pinned_saved(&env, store.as_ref(), &saved[0].machine_id).unwrap(),
            )
            .into_pending()
            .admit(Duration::from_secs(5), CancellationToken::new())
            .await
            .unwrap();
            let replacement = SessionServerClient::from_remote_admitted(
                admitted,
                routing.clone(),
                Arc::<Ledger>::default(),
                None,
            )
            .await
            .unwrap();
            replacement.publication_permit().publish();
            assert_ne!(replacement.domain_client().namespace(), &old_namespace);
            let new_namespace = replacement.domain_client().namespace().clone();
            let resumed = routing.streams.resume_from_to(
                StreamHost::remote_namespace(&old_namespace),
                StreamHost::remote_namespace(&new_namespace),
                || true,
                |session, viewport| replacement.domain_client().subscribe(session, viewport),
                |stream| {
                    replacement.domain_client().unsubscribe(stream).unwrap();
                },
                &mut |local, item| {
                    if let crate::commands::pty_host::StreamItem::Output { data, .. } = item {
                        routing.sent.lock().unwrap().push((local, data.into()));
                    }
                    true
                },
            );
            assert!(resumed.is_empty(), "the retained stream must migrate cleanly");
            replacement
                .domain_client()
                .write(&PtySessionId::new("same-session"), b"go\r")
                .unwrap();
            assert!(wait_until(|| routing
                .output(locals[0])
                .contains("RM4E_A_RECONNECTED")));
            assert!(!routing.output(locals[1]).contains("RM4E_A"));
            assert!(!routing.output(locals[0]).contains("RM4E_B"));
            replacement.close_awaited().await;
            managed.pop().unwrap().close_awaited().await;
        });
    }

    #[test]
    #[ignore = "needs an sshd test container"]
    fn rm4d_container_supported_cooperative_replace_and_readmit() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::terminal::session_server::{Ledger, SessionServerClient};
        use codesense_session_server::testing::{registration, sh, RecordingSink};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        docker_as_dev(
            "pkill -KILL -f '[c]odesense-session-server' || true; rm -rf \"$HOME/.codesense\"",
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = Arc::new(env(root.path()));
        let store = Arc::new(FakeStore::default());
        let first = test_connection(&env, store.as_ref(), None, &input(key_file(), None)).unwrap();
        let trusted = input(key_file(), first.steps[1].fingerprint.clone());
        install_server(&env, store.as_ref(), None, &trusted).unwrap();
        let saved = save(&env, store.as_ref(), None, &trusted, &Scope::Personal, true).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        // Prove both the image installation and its dynamic loader before the
        // production PTY path is involved. The failure branch leaves actionable
        // path/ownership evidence in the authoritative Docker log.
        docker_as_dev(
            "helper=/usr/local/bin/rm4d-process-fixture; if ! test -x \"$helper\"; then echo 'RM4d fixture missing/not executable:' >&2; ls -la /usr/local/bin >&2; exit 1; fi; rm -f /tmp/rm4d-preflight; \"$helper\" term-exit /tmp/rm4d-preflight & p=$!; i=0; while ! grep -q '^READY ' /tmp/rm4d-preflight 2>/dev/null && [ $i -lt 40 ]; do i=$((i+1)); sleep .05; done; if ! grep -q '^READY ' /tmp/rm4d-preflight 2>/dev/null; then echo \"RM4d fixture could not execute at $helper\" >&2; ls -l \"$helper\" >&2; kill -KILL $p 2>/dev/null || true; exit 1; fi; kill -TERM $p; wait $p",
        );
        docker_as_dev(
            "rm -f /tmp/rm4d-events /tmp/rm4d-sentinel.pid; setsid /usr/local/bin/rm4d-process-fixture sentinel /tmp/rm4d-events >/dev/null 2>&1 & echo $! > /tmp/rm4d-sentinel.pid",
        );
        let (server_id, old_epoch) = runtime.block_on(async {
            let pending = RemoteTransportOwner::from_pinned(
                open_pinned_saved(&env, store.as_ref(), &saved.machine_id).unwrap(),
            )
            .into_pending();
            let admitted = pending
                .admit(Duration::from_secs(5), CancellationToken::new())
                .await
                .unwrap();
            let identity = (
                admitted.verified_server_id().to_string(),
                admitted.verified_epoch().to_string(),
            );
            let events = Arc::new(RecordingSink::default());
            let managed = SessionServerClient::from_remote_admitted(
                admitted,
                events,
                Arc::<Ledger>::default(),
                None,
            )
            .await
            .unwrap();
            managed.publication_permit().publish();
            let client = managed.domain_client();
            for (id, mode, pool) in [
                ("rm4d-term-exit", "term-exit", false),
                ("rm4d-term-delay", "term-delay", false),
                ("rm4d-term-ignore", "term-ignore", false),
                ("rm4d-warm", "term-ignore", true),
            ] {
                let mut intent = sh(&format!(
                    "exec /usr/local/bin/rm4d-process-fixture {mode} /tmp/rm4d-events"
                ));
                // The helper's `sh()` defaults model an in-process test host.
                // Pin the real remote image's known shell and cwd so the PTY
                // child cannot inherit a caller-machine shell or missing cwd.
                intent.shell = Some("/bin/bash".into());
                intent.cwd = "/tmp".into();
                client
                    .spawn(intent, registration(id, Some("rm4d-persisting"), pool))
                    .unwrap()
                    .expect("fixture child pid");
            }
            drop(managed);
            identity
        });
        docker_as_dev(
            "i=0; while [ \"$(grep -c '^READY ' /tmp/rm4d-events 2>/dev/null || true)\" -lt 5 ] && [ $i -lt 100 ]; do i=$((i+1)); sleep .05; done; test \"$(grep -c '^READY ' /tmp/rm4d-events)\" -eq 5",
        );
        let proof = legacy_host_proof_saved(&env, store.as_ref(), &saved.machine_id)
            .unwrap()
            .unwrap();
        assert_eq!(proof.expected_server_id(), server_id);
        replace_pinned_saved(
            &env,
            store.as_ref(),
            &saved.machine_id,
            &proof,
            "0123456789abcdef",
            &old_epoch,
        )
        .unwrap();
        docker_as_dev(
            "test \"$(grep -c '^TERM$' /tmp/rm4d-events)\" -ge 3; kill -0 \"$(cat /tmp/rm4d-sentinel.pid)\"; test \"$(pgrep -f '^/usr/local/bin/rm4d-process-fixture (term-exit|term-delay|term-ignore)' | wc -l)\" -eq 0",
        );
        runtime.block_on(async {
            let pending = RemoteTransportOwner::from_pinned(
                open_pinned_saved(&env, store.as_ref(), &saved.machine_id).unwrap(),
            )
            .into_pending();
            let admitted = pending
                .admit(Duration::from_secs(5), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(admitted.verified_server_id(), server_id);
            assert_ne!(admitted.verified_epoch(), old_epoch);
            assert_eq!(admitted.verified_census().live_sessions, 0);
            admitted.close_awaited().await;
        });
    }

    #[test]
    #[ignore = "owned two-account SSH fixture; injected backend owners, not signed-in profile qualification"]
    fn private_account_setup_and_own_session_over_real_ssh() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::terminal::session_server::tests::remote_tab_connector_contract;
        use crate::terminal::session_server::{Ledger, SessionServerClient};
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::testing::RecordingSink;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let store = FakeStore::default();
        let mut ids = Vec::new();
        for (owner, user) in [(41, "dev"), (42, "peer")] {
            let mut environment = env(root.path());
            environment.registry =
                Registry::new(environment.codesense_home.clone(), Principal::from(owner));
            let (authority, _retire) =
                crate::commands::auth::private_authority::test_snapshot(owner);
            environment.private_authority = Some(authority);
            let mut draft = input(key_file(), None);
            draft.mode = Some(MachineMode::PrivateAccount);
            draft.username = Some(user.into());
            if user == "peer" {
                draft.sign_in = MachineSignIn::KeyFile {
                    key_path: var("P1_PEER_KEY"),
                };
            }
            let first = test_connection(&environment, &store, None, &draft).unwrap();
            draft.trusted_host_key_fp = first.steps[0].fingerprint.clone();
            assert!(draft.trusted_host_key_fp.is_some());
            let needs = test_connection(&environment, &store, None, &draft).unwrap();
            assert!(needs
                .steps
                .iter()
                .any(|s| s.next_steps.contains(&"enroll-private-account")));
            let saved_draft =
                save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
            assert!(environment
                .registry
                .all_declarations()
                .unwrap()
                .found
                .is_empty());
            private_setup::enroll(&environment, &store, Some(&saved_draft.machine_id), &draft)
                .unwrap();
            install_server(&environment, &store, Some(&saved_draft.machine_id), &draft).unwrap();
            let saved = save(
                &environment,
                &store,
                Some(&saved_draft.machine_id),
                &draft,
                &Scope::Personal,
                true,
            )
            .unwrap();
            runtime.block_on(async {
                let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
                assert!(transport.private_lease.is_some());
                let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
                ids.push(admitted.verified_server_id().to_string());
                let events = Arc::new(RecordingSink::default());
                let managed = SessionServerClient::from_remote_admitted(admitted, events.clone(), Arc::<Ledger>::default(), None).await.unwrap();
                let namespace = managed.domain_client().namespace().clone();
                let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await;
                let target = manager.target_for_namespace(&namespace).unwrap();
                let exact = manager.capture_admitted_backend(&target).unwrap();
                let output = remote_tab_connector_contract(exact.client(), events, &format!("private-{owner}"), SpawnIntent {
                    remote_launch: Default::default(),
                    command: None, argv: None, shell: None, cwd: format!("/home/{user}"), cols: 80, rows: 24,
                    shell_integration: true, starship_preset: None, api_port: None, env_remove: vec![], env: vec![], agent_env: Vec::new(),
                    login_path: None,
                }, "printf 'PRIVATE_READY\\nCWD=%s\\nINIT=%s\\n' \"$PWD\" \"$__CODESENSE_SHELL_INIT\"; read line; printf 'GOT=%s\\n' \"$line\"; sleep 60");
                assert!(output.contains(&format!("INIT=/home/{user}/.codesense/remote-private/v1/host/")), "{output}");
                manager.disconnect(&saved.machine_id).await.unwrap();
                assert!(manager.capture_admitted_backend(&target).is_err());
                let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
                let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
                assert_eq!(admitted.verified_server_id(), ids.last().unwrap());
                let old_epoch = admitted.verified_epoch().to_string();
                admitted.close_awaited().await;
                let proof = legacy_host_proof_saved(&environment, &store, &saved.machine_id).unwrap().unwrap();
                replace_pinned_saved(&environment, &store, &saved.machine_id, &proof, &uuid::Uuid::new_v4().simple().to_string(), &old_epoch).unwrap();
                let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
                let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
                assert_eq!(admitted.verified_server_id(), ids.last().unwrap());
                assert_ne!(admitted.verified_epoch(), old_epoch);
                admitted.close_awaited().await;
            });
            let mut peer = draft.clone();
            peer.username = Some(if user == "dev" { "peer" } else { "dev" }.into());
            // Once both accounts are enrolled, the wrong owner must get no report.
            if owner == 42 {
                peer.sign_in = key_file(); // Valid dev credential, wrong backend owner.
                assert!(test_connection(&environment, &store, None, &peer).is_err());
            }
        }
        assert_ne!(ids[0], ids[1]);
    }

    /// One-way delay the test adds to the container's link: about a 250 ms
    /// round trip, a distant cloud region rather than localhost.
    const WAN_ONE_WAY_MS: u32 = 125;

    struct InjectedLatency;

    impl InjectedLatency {
        fn add() -> Self {
            docker_as_root(&format!(
                "tc qdisc replace dev eth0 root netem delay {WAN_ONE_WAY_MS}ms"
            ));
            Self
        }
    }

    impl Drop for InjectedLatency {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                    "tc",
                    "qdisc",
                    "del",
                    "dev",
                    "eth0",
                    "root",
                ])
                .status();
        }
    }

    /// SSH sessions with umask 0002, the default on hosts with user private
    /// groups (Ubuntu), while this guard lives. Only a line this guard added
    /// is removed again.
    struct GroupWritableUmask {
        added: bool,
    }

    const PAM_UMASK_LINE: &str = "session optional pam_umask.so umask=0002";

    impl GroupWritableUmask {
        fn set() -> Self {
            let present = Command::new("docker")
                .args([
                    "exec",
                    &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                    "grep",
                    "-q",
                    "pam_umask",
                    "/etc/pam.d/sshd",
                ])
                .status()
                .unwrap()
                .success();
            if !present {
                docker_as_root(&format!("echo '{PAM_UMASK_LINE}' >> /etc/pam.d/sshd"));
            }
            Self { added: !present }
        }
    }

    impl Drop for GroupWritableUmask {
        fn drop(&mut self) {
            if !self.added {
                return;
            }
            let _ = Command::new("docker")
                .args([
                    "exec",
                    &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                    "sed",
                    "-i",
                    &format!("\\|^{PAM_UMASK_LINE}$|d"),
                    "/etc/pam.d/sshd",
                ])
                .status();
        }
    }

    /// A ready private-account machine on a umask-0002 host reached over real
    /// internet round trips: an install whose runtime files are group-writable
    /// still updates, then connects within the connection manager's
    /// deadlines, opens a tab and reads its output.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn private_account_connects_over_a_high_latency_link() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::commands::host_connection_manager::{
            CONNECT_DEADLINE, HANDSHAKE_DEADLINE, UPLOAD_GRACE,
        };
        use crate::terminal::session_server::tests::remote_tab_runs;
        use crate::terminal::session_server::{Ledger, SessionServerClient};
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::private_account::UPLOAD_STEP;
        use codesense_session_server::testing::RecordingSink;
        use std::time::Instant;
        use tokio_util::sync::CancellationToken;

        docker_as_dev(
            "pkill -KILL -u dev -f '[p]rivate-serve' || true; rm -rf \"$HOME/.codesense/remote-private\"",
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let store = FakeStore::default();
        let owner = 43;
        let mut environment = env(root.path());
        environment.registry =
            Registry::new(environment.codesense_home.clone(), Principal::from(owner));
        let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(owner);
        environment.private_authority = Some(authority);
        let _umask = GroupWritableUmask::set();
        let umask = Command::new("ssh")
            .args([
                "-p",
                &var("CODESENSE_REMOTE_HOST_PORT"),
                "-i",
                &var("CODESENSE_REMOTE_HOST_KEY"),
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "dev@127.0.0.1",
                "umask",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&umask.stdout).trim(), "0002");
        let mut draft = input(key_file(), None);
        // A new machine with default settings: the mode that opens tabs.
        draft.mode = None;
        let first = test_connection(&environment, &store, None, &draft).unwrap();
        draft.trusted_host_key_fp = first.steps[0].fingerprint.clone();
        let saved_draft =
            save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
        private_setup::enroll(&environment, &store, Some(&saved_draft.machine_id), &draft).unwrap();
        install_server(&environment, &store, Some(&saved_draft.machine_id), &draft).unwrap();
        let saved = save(
            &environment,
            &store,
            Some(&saved_draft.machine_id),
            &draft,
            &Scope::Personal,
            true,
        )
        .unwrap();

        // Start the server, then leave its runtime files the way an install
        // made before the server set its own umask did: group-writable.
        runtime.block_on(async {
            let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
            let admitted = RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(HANDSHAKE_DEADLINE, CancellationToken::new())
                .await
                .unwrap();
            let epoch = admitted.verified_epoch().to_string();
            admitted.close_awaited().await;
            docker_as_dev(
                "cd \"$HOME/.codesense/remote-private/v1\" && for f in pty-server.lock pty-server-id logs/pty-server.log; do test \"$(stat -c %a $f)\" = 600 || exit 1; done && test \"$(stat -c %a emulator)\" = 700 && chmod 664 pty-server.lock pty-server-id logs/pty-server.log && chmod 775 emulator",
            );
            // Update terminal server: the proof and the replacement succeed.
            let proof = legacy_host_proof_saved(&environment, &store, &saved.machine_id)
                .unwrap()
                .unwrap();
            replace_pinned_saved(
                &environment,
                &store,
                &saved.machine_id,
                &proof,
                &uuid::Uuid::new_v4().simple().to_string(),
                &epoch,
            )
            .unwrap();
        });
        docker_as_dev(
            "cd \"$HOME/.codesense/remote-private/v1\" && for f in pty-server.lock pty-server-id logs/pty-server.log; do test \"$(stat -c %a $f)\" = 600 || exit 1; done && test \"$(stat -c %a emulator)\" = 700",
        );

        let _latency = InjectedLatency::add();
        runtime.block_on(async {
            let started = Instant::now();
            let progress = OpenProgress::default();
            let transport =
                open_pinned_saved_with(&environment, &store, &saved.machine_id, &progress).unwrap();
            let opened = started.elapsed();
            let steps = progress.timings();
            eprintln!("private open steps: {steps:?}");
            assert!(
                opened < CONNECT_DEADLINE,
                "opening took {opened:?}, over the {CONNECT_DEADLINE:?} connect deadline: {steps:?}"
            );
            assert!(
                !steps.iter().any(|(step, _)| *step == UPLOAD_STEP),
                "an installed machine re-sent its bootstrap helper: {steps:?}"
            );
            let admitted = RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(HANDSHAKE_DEADLINE, CancellationToken::new())
                .await
                .unwrap();
            eprintln!(
                "private connect at ~{}ms RTT: open {opened:?}, hello {:?}",
                2 * WAN_ONE_WAY_MS,
                started.elapsed() - opened
            );
            let events = Arc::new(RecordingSink::default());
            let managed = SessionServerClient::from_remote_admitted(
                admitted,
                events.clone(),
                Arc::<Ledger>::default(),
                None,
            )
            .await
            .unwrap();
            let namespace = managed.domain_client().namespace().clone();
            let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(
                &saved.machine_id,
                managed,
            )
            .await;
            let target = manager.target_for_namespace(&namespace).unwrap();
            let exact = manager.capture_admitted_backend(&target).unwrap();
            let output = remote_tab_runs(
                exact.client(),
                events,
                "private-wan",
                SpawnIntent {
                    remote_launch: Default::default(),
                    command: None,
                    argv: None,
                    shell: None,
                    cwd: "/home/dev".into(),
                    cols: 80,
                    rows: 24,
                    shell_integration: true,
                    starship_preset: None,
                    api_port: None,
                    env_remove: vec![],
                    env: vec![],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                "printf 'PRIVATE_%s\\n' READY",
                "PRIVATE_READY",
            );
            assert!(output.contains("PRIVATE_READY"), "{output}");
            manager.disconnect(&saved.machine_id).await.unwrap();
        });

        // After an app update the installed helper no longer matches this
        // build: the connect re-sends it, inside its upload grace.
        let updated = tempfile::tempdir_in("/tmp").unwrap();
        simulate_app_update(&environment.artifacts, updated.path());
        environment.artifacts = updated.path().into();
        runtime.block_on(async {
            let started = Instant::now();
            let progress = OpenProgress::default();
            let transport =
                open_pinned_saved_with(&environment, &store, &saved.machine_id, &progress).unwrap();
            let opened = started.elapsed();
            let steps = progress.timings();
            eprintln!("private open steps after an update: {steps:?}");
            assert!(
                steps.iter().any(|(step, _)| *step == UPLOAD_STEP),
                "{steps:?}"
            );
            assert!(
                opened < CONNECT_DEADLINE + UPLOAD_GRACE,
                "opening after an update took {opened:?}: {steps:?}"
            );
            RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(HANDSHAKE_DEADLINE, CancellationToken::new())
                .await
                .unwrap()
                .close_awaited()
                .await;
        });
    }

    /// The artifacts of a newer build: every terminal-server binary differs
    /// (and so does its recorded sha256), everything else is the same file.
    fn simulate_app_update(artifacts: &Path, updated: &Path) {
        for entry in std::fs::read_dir(artifacts).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let server =
                name.starts_with("codesense-session-server-") && name.ends_with("-linux-musl");
            if name.starts_with("codesense-session-server-") && name.ends_with("-linux-musl.sha256")
            {
                continue;
            }
            if server {
                let target = updated.join(&name);
                let mut bytes = std::fs::read(&path).unwrap();
                bytes.push(0);
                std::fs::write(&target, bytes).unwrap();
                std::fs::set_permissions(&target, std::fs::metadata(&path).unwrap().permissions())
                    .unwrap();
                let sha = codesense_session_server::install::sha256_of(&target).unwrap();
                std::fs::write(
                    updated.join(format!("{name}.sha256")),
                    format!("{sha}  {name}\n"),
                )
                .unwrap();
            } else {
                std::os::unix::fs::symlink(&path, updated.join(&name)).unwrap();
            }
        }
    }

    /// A connected-capable legacy machine switches to a private account in
    /// place: testing it writes nothing, enrolling converts it, and once
    /// ready it keeps its id, host key and background reconnect, and opens.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn a_legacy_machine_switches_to_a_private_account_in_place() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::commands::host_connection_manager::HANDSHAKE_DEADLINE;
        use tokio_util::sync::CancellationToken;

        docker_as_dev(
            "pkill -KILL -u dev -f '[p]rivate-serve' || true; rm -rf \"$HOME/.codesense/remote-private\"",
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let store = FakeStore::default();
        let owner = 44;
        let mut environment = env(root.path());
        environment.registry =
            Registry::new(environment.codesense_home.clone(), Principal::from(owner));
        let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(owner);
        environment.private_authority = Some(authority);

        let first = test_connection(&environment, &store, None, &input(key_file(), None)).unwrap();
        let legacy = input(key_file(), first.steps[1].fingerprint.clone());
        install_server(&environment, &store, None, &legacy).unwrap();
        let saved = save(&environment, &store, None, &legacy, &Scope::Personal, true).unwrap();
        let id = saved.machine_id.clone();
        let pin = saved.declaration.unwrap().pinned_host_key_fp;
        environment
            .registry
            .update_bindings(|bindings| bindings.get_mut(&id).unwrap().auto_connect = true)
            .unwrap();
        // Connecting the legacy machine leaves its shared-account server running.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let transport = open_pinned_saved(&environment, &store, &id).unwrap();
            RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(HANDSHAKE_DEADLINE, CancellationToken::new())
                .await
                .unwrap()
                .close_awaited()
                .await;
        });
        docker_as_dev("pgrep -u dev -f \"$HOME/.codesense/host/\" >/dev/null");

        let mut private = legacy.clone();
        private.mode = Some(MachineMode::PrivateAccount);
        private.trusted_host_key_fp = None;
        let report = test_connection(&environment, &store, Some(&id), &private).unwrap();
        assert!(report
            .steps
            .iter()
            .any(|s| s.next_steps.contains(&"enroll-private-account")));
        assert!(!private_setup::claims(&environment, &id).unwrap());

        private_setup::enroll(&environment, &store, Some(&id), &private).unwrap();
        assert!(private_setup::claims(&environment, &id).unwrap());
        install_server(&environment, &store, Some(&id), &private).unwrap();
        let ready = save(
            &environment,
            &store,
            Some(&id),
            &private,
            &Scope::Personal,
            true,
        )
        .unwrap();
        assert_eq!(ready.machine_id, id);
        let declaration = ready.declaration.unwrap();
        assert_eq!(declaration.mode, MachineMode::PrivateAccount);
        assert_eq!(declaration.pinned_host_key_fp, pin);
        assert!(ready.binding.unwrap().auto_connect);

        // The switched machine stops the legacy server it left behind (it holds
        // no terminals, so without asking), and only that one.
        assert_eq!(
            legacy_retire::retire_saved(&environment, &store, &id, false).unwrap(),
            legacy_retire::LegacyServerRetirement::Stopped { sessions: 0 }
        );
        docker_as_dev("! pgrep -u dev -f \"$HOME/.codesense/host/\" >/dev/null");
        assert_eq!(
            legacy_retire::retire_saved(&environment, &store, &id, false).unwrap(),
            legacy_retire::LegacyServerRetirement::NotRunning
        );

        runtime.block_on(async {
            let transport = open_pinned_saved(&environment, &store, &id).unwrap();
            assert!(transport.private_lease.is_some());
            RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(HANDSHAKE_DEADLINE, CancellationToken::new())
                .await
                .unwrap()
                .close_awaited()
                .await;
        });
    }

    /// SW-RM-4 AC1/AC7: RM-3's sole pinned connector feeds the same client
    /// contract as local UDS, then normal close consumes and reaps its SSH
    /// process group.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn terminal_domain_contract_runs_over_rm3_ssh_stdio() {
        use crate::commands::host_connection::RemoteTransportOwner;
        use crate::terminal::session_server::tests::terminal_domain_connector_contract;
        use crate::terminal::session_server::{Ledger, SessionServerClient};
        use codesense_pty_core::spawn::SpawnIntent;
        use codesense_session_server::testing::RecordingSink;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        docker_as_dev(
            "pkill -KILL -f '[c]odesense-session-server' || true; rm -rf \"$HOME/.codesense\"; mkdir -p /tmp/rm4-contract",
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = Arc::new(env(root.path()));
        let store = Arc::new(FakeStore::default());
        let first = test_connection(&env, store.as_ref(), None, &input(key_file(), None)).unwrap();
        let trusted = input(key_file(), first.steps[1].fingerprint.clone());
        install_server(&env, store.as_ref(), None, &trusted).unwrap();
        let saved = save(&env, store.as_ref(), None, &trusted, &Scope::Personal, true).unwrap();
        let machine_id = saved.machine_id;

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let transport = open_pinned_saved(&env, store.as_ref(), &machine_id).unwrap();
            let generation = transport.generation.clone();
            let ssh_pid = transport.child_guard.child.as_ref().unwrap().id() as libc::pid_t;
            revalidate_transport_generation(&env, &machine_id, &generation).unwrap();
            let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
            let admitted = pending
                .admit(Duration::from_secs(5), CancellationToken::new())
                .await
                .unwrap();
            revalidate_transport_generation(&env, &machine_id, &generation).unwrap();

            let events = Arc::new(RecordingSink::default());
            let managed = SessionServerClient::from_remote_admitted(
                admitted,
                events.clone(),
                Arc::<Ledger>::default(),
                None,
            )
            .await
            .unwrap();
            managed.publication_permit().publish();
            let client = managed.domain_client();
            let output = terminal_domain_connector_contract(
                client,
                events,
                "contract-ssh",
                SpawnIntent {
                    remote_launch: Default::default(),
                    command: None,
                    argv: None,
                    shell: None,
                    cwd: "/tmp/rm4-contract".into(),
                    cols: 80,
                    rows: 24,
                    shell_integration: true,
                    starship_preset: None,
                    api_port: None,
                    env_remove: Vec::new(),
                    env: vec![("RM4_AUTHORIZED".into(), "allowed".into())],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                "printf 'READY\\nSHELL=%s\\nCWD=%s\\nAUTH=%s\\nINIT=%s\\n' \"$SHELL\" \"$PWD\" \"$RM4_AUTHORIZED\" \"$__CODESENSE_SHELL_INIT\"; read line; printf 'GOT=%s\\n' \"$line\"; sleep 60",
            );
            assert!(output.contains("SHELL=/"), "{output}");
            assert!(output.contains("CWD=/tmp/rm4-contract"), "{output}");
            assert!(output.contains("AUTH=allowed"), "{output}");
            assert!(
                output.contains("INIT=/home/dev/.codesense/host/")
                    && output.contains("/resources/shell-integration/codesense-shell-init."),
                "{output}"
            );
            assert!(
                !output.contains(env!("CARGO_MANIFEST_DIR"))
                    && !output.contains(root.path().to_string_lossy().as_ref()),
                "desktop paths leaked into remote spawn: {output}"
            );
            managed.close_awaited().await;

            assert_eq!(unsafe { libc::kill(ssh_pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(ssh_pid, &mut status, libc::WNOHANG) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        });
    }

    /// Trust → install → Connect saves a pinned machine; a changed terminal
    /// server at it is refused and changes nothing.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn the_add_machine_flow_connects_and_pins() {
        docker_as_dev(
            "pkill -KILL -f '[c]odesense-session-server' || true; rm -rf \"$HOME/.codesense\"",
        );
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = env(root.path());
        let store = FakeStore::default();

        let first = test_connection(&env, &store, None, &input(key_file(), None)).unwrap();
        assert_eq!(status_of(&first, "hostKey"), "needsTrust");
        let fp = first.steps[1].fingerprint.clone().unwrap();
        let trusted = input(key_file(), Some(fp.clone()));
        let report = test_connection(&env, &store, None, &trusted).unwrap();
        assert_eq!(status_of(&report, "auth"), "ok");
        assert_eq!(status_of(&report, "terminalServer"), "warn");
        assert!(report.install_plan.is_some());
        assert!(
            save(&env, &store, None, &trusted, &Scope::Personal, true).is_err(),
            "Connect refuses a machine whose check has not passed"
        );

        let installed = install_server(&env, &store, None, &trusted).unwrap();
        assert!(
            matches!(installed, InstallTerminalServerResult::Installed { .. }),
            "{installed:?}"
        );
        let ready = test_connection(&env, &store, None, &trusted).unwrap();
        assert!(
            ready.steps.iter().all(|s| s.status == "ok"),
            "{:?}",
            ready.steps
        );

        let saved = save(&env, &store, None, &trusted, &Scope::Personal, true).unwrap();
        assert_eq!(saved.status, MachineStatus::Idle);
        assert!(
            !saved.binding.as_ref().unwrap().auto_connect,
            "Connect enrolls but never opts into background reconnect"
        );
        assert_eq!(
            saved
                .declaration
                .as_ref()
                .unwrap()
                .pinned_host_key_fp
                .as_deref(),
            Some(fp.as_str())
        );
        let declaration = env.registry.find(&saved.machine_id).unwrap().unwrap().1;
        let server = declaration
            .pinned_server_id
            .clone()
            .expect("the terminal server is pinned");

        // The saved machine is checked against its pin, with no trust needed,
        // and a check never rewrites that pin.
        let pin = env.paths().pin_file(&saved.machine_id);
        let pinned_before = std::fs::read_to_string(&pin).unwrap();
        let pinned = test_connection(
            &env,
            &store,
            Some(&saved.machine_id),
            &input(key_file(), None),
        )
        .unwrap();
        assert_eq!(status_of(&pinned, "hostKey"), "ok");
        assert_eq!(std::fs::read_to_string(&pin).unwrap(), pinned_before);

        docker_as_dev("pkill -KILL -f '[c]odesense-session-server' || true; rm -f \"$HOME/.codesense/pty-server-id\"");
        let err = save(
            &env,
            &store,
            Some(&saved.machine_id),
            &trusted,
            &Scope::Personal,
            true,
        )
        .unwrap_err();
        assert!(err.contains(&server), "{err}");
        assert_eq!(
            env.registry
                .find(&saved.machine_id)
                .unwrap()
                .unwrap()
                .1
                .pinned_server_id
                .as_deref(),
            Some(server.as_str()),
            "a refused identity changes nothing"
        );
    }

    #[test]
    #[ignore = "needs an sshd test container"]
    fn rm4c_container_v1_control_proof_opens_no_terminal_stream() {
        if std::env::var_os("RM4F_SCENARIO_SLOT").is_some() {
            // The owned adapter has saved quiescent caller state outside this
            // empty slot. Never reset a supplied machine.
            docker_as_dev("test ! -e \"$HOME/.codesense\"");
        } else {
            // Preserve the legacy sequential rm4d gate's existing lifecycle.
            docker_as_dev(
                "pkill -KILL -f '[c]odesense-session-server' || true; rm -rf \"$HOME/.codesense\"",
            );
        }
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = Arc::new(env(root.path()));
        let store = Arc::new(FakeStore::default());
        let first = test_connection(&env, store.as_ref(), None, &input(key_file(), None)).unwrap();
        let trusted = input(
            key_file(),
            Some(first.steps[1].fingerprint.clone().unwrap()),
        );
        install_server(&env, store.as_ref(), None, &trusted).unwrap();
        let saved = save(&env, store.as_ref(), None, &trusted, &Scope::Personal, true).unwrap();
        let server_id = env
            .registry
            .find(&saved.machine_id)
            .unwrap()
            .unwrap()
            .1
            .pinned_server_id
            .unwrap();
        docker_as_dev(&format!(
            r#"
H="$HOME/.codesense/host"; V=rm4c-v1-fixture; D="$H/$V"
mkdir -p "$D"
cp /bin/sleep "$D/codesense-session-server"
printf '1\n' > "$D/.codesense-protocol-major"
cd "$D"; sha256sum codesense-session-server .codesense-protocol-major > .codesense-manifest
ln -sfn "$V" "$H/current"; printf '%s\n' '{server_id}' > "$HOME/.codesense/pty-server-id"
: > "$HOME/rm4c-terminal-client-bytes"
: > "$HOME/rm4c-terminal-attach-invocations"
"$D/codesense-session-server" 300 </dev/null >/dev/null 2>&1 & echo $! > "$HOME/rm4c-mismatched-pid"
pid=$(cat "$HOME/rm4c-mismatched-pid"); deadline=$(( $(date +%s) + 5 ))
while test "$(readlink /proc/$pid/exe)" != "$D/codesense-session-server"; do test "$(date +%s)" -lt "$deadline" || exit 1; sleep .05; done
cp /usr/local/bin/rm4c-sentinel "$D/codesense-session-server.new"
mv "$D/codesense-session-server.new" "$D/codesense-session-server"
sha256sum codesense-session-server .codesense-protocol-major > .codesense-manifest
pid=$(cat "$HOME/rm4c-mismatched-pid"); deadline=$(( $(date +%s) + 5 ))
while ! test -e "/proc/$pid/exe"; do test "$(date +%s)" -lt "$deadline" || exit 1; sleep .05; done
"#
        ));
        let mismatch = legacy_host_proof_saved(&env, store.as_ref(), &saved.machine_id)
            .expect_err("argv path alone must not prove a replaced executable inode");
        assert!(
            mismatch.contains("exited Some(28)"),
            "unexpected identity rejection: {mismatch}"
        );
        docker_as_dev(
            r#"kill "$(cat "$HOME/rm4c-mismatched-pid")"; D="$HOME/.codesense/host/rm4c-v1-fixture"; "$D/codesense-session-server" </dev/null >/dev/null 2>&1 & pid=$!; echo "$pid" > "$HOME/rm4c-sentinel-pid"; deadline=$(( $(date +%s) + 5 )); while test "$(readlink /proc/$pid/exe)" != "$D/codesense-session-server"; do test "$(date +%s)" -lt "$deadline" || exit 1; sleep .05; done"#,
        );
        let open_env = env.clone();
        let open_store = store.clone();
        let proof_env = env.clone();
        let proof_store = store.clone();
        let state = tokio::runtime::Runtime::new().unwrap().block_on(
            crate::commands::host_connection_manager::test_production_legacy_path(
                saved.machine_id.clone(),
                Arc::new(move |machine_id| {
                    open_pinned_saved(&open_env, open_store.as_ref(), &machine_id)
                }),
                Arc::new(move |machine_id| {
                    legacy_host_proof_saved(&proof_env, proof_store.as_ref(), &machine_id)
                }),
            ),
        );
        assert!(
            matches!(
                state,
                crate::commands::host_connection_manager::HostConnectionState::UnsupportedLegacy { .. }
            ),
            "unexpected manager state: {state:?}"
        );
        let bytes = std::process::Command::new("docker")
            .args([
                "exec",
                &var("CODESENSE_REMOTE_HOST_CONTAINER"),
                "sh",
                "-lc",
                "wc -c < /home/dev/rm4c-terminal-client-bytes; wc -l < /home/dev/rm4c-terminal-attach-invocations",
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&bytes.stdout)
                .lines()
                .collect::<Vec<_>>(),
            ["0", "0"]
        );
    }

    #[test]
    #[ignore = "needs an sshd test container"]
    fn rm4d_container_unsupported_v1_has_zero_dispatch_or_action() {
        // Reuse the deployed-v1 pinned-control fixture: it drives the actual
        // production classification path to UnsupportedLegacy and asserts the
        // terminal transport/attach sent exactly zero bytes/invocations.
        rm4c_container_v1_control_proof_opens_no_terminal_stream();
    }

    /// A passphrase kept under a keychain handle signs in through the askpass.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn a_keychain_handle_signs_in() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = env(root.path());
        let store = FakeStore::default();
        let handle = passphrase::store(
            &store,
            &Secret::new(var("CODESENSE_REMOTE_HOST_PASSPHRASE")),
        )
        .unwrap();
        let sign_in = MachineSignIn::KeychainPassphrase {
            key_path: var("CODESENSE_REMOTE_HOST_LOCKED_KEY"),
            credential_ref: handle,
        };
        let fp = test_connection(&env, &store, None, &input(sign_in.clone(), None))
            .unwrap()
            .steps[1]
            .fingerprint
            .clone();
        let report = test_connection(&env, &store, None, &input(sign_in, fp)).unwrap();
        assert_eq!(status_of(&report, "auth"), "ok", "{:?}", report.steps);
    }

    /// Real ssh output crosses the command DTO as the exact disk-full failure
    /// the Add-machine UI renders.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn real_disk_pressure_is_an_ipc_checklist_failure() {
        docker_as_root(
            r#"printf '%s\n' '#!/bin/sh' 'echo "Filesystem 1024-blocks Used Available Capacity Mounted on"' 'echo "fs 100000 99000 1024 99% /"' > /usr/local/bin/df; chmod 755 /usr/local/bin/df"#,
        );
        let _df = ContainerFile("/usr/local/bin/df");
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let env = env(root.path());
        let store = FakeStore::default();
        let first = test_connection(&env, &store, None, &input(key_file(), None)).unwrap();
        let fp = first.steps[1].fingerprint.clone();
        let report = test_connection(&env, &store, None, &input(key_file(), fp)).unwrap();
        let data = report
            .steps
            .iter()
            .find(|step| step.id == "dataDir")
            .unwrap();
        assert_eq!(data.status, "fail", "{:?}", report.steps);
        assert!(data.detail.contains("disk is full"), "{}", data.detail);
        assert_eq!(data.next_steps, ["retry"]);
    }

    /// Preparation and discovery use the real sshd. The wrapper drops the
    /// first upload exactly where a network interruption would; the command
    /// maps ssh's 255/reset into the IPC `interrupted` code consumed by Resume.
    #[test]
    #[ignore = "needs an sshd test container"]
    fn interrupted_real_install_maps_to_resume_ipc() {
        docker_as_dev("rm -rf \"$HOME/.codesense\"");
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let mut env = env(root.path());
        let wrapper = root.path().join("ssh-drop-upload");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\ncase \"$*\" in *'cat >'*) echo 'Connection reset by peer' >&2; exit 255;; esac\nexec ssh \"$@\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.ssh_program = wrapper;
        let store = FakeStore::default();
        let first = test_connection(&env, &store, None, &input(key_file(), None)).unwrap();
        let fp = first.steps[1].fingerprint.clone();
        let trusted = input(key_file(), fp);
        let result = install_server(&env, &store, None, &trusted).unwrap();
        assert!(
            matches!(
                result,
                InstallTerminalServerResult::Failed {
                    code: "interrupted",
                    ..
                }
            ),
            "{result:?}"
        );
    }
}

#[test]
fn private_draft_keeps_opaque_keychain_handle_and_survives_sweep() {
    let root = tempfile::tempdir().unwrap();
    let mut env = env_for(root.path(), 41);
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    env.private_authority = Some(authority);
    let store = FakeStore::default();
    let handle = capture_passphrase(
        &env,
        &store,
        "/tmp/private-key",
        |_| Ok(Some(Secret::new("private fixture passphrase"))),
        |_, _| Ok(()),
    )
    .unwrap()
    .unwrap();
    let mut input = draft(MachineSignIn::KeychainPassphrase {
        key_path: "/tmp/private-key".into(),
        credential_ref: handle.clone(),
    });
    input.mode = Some(MachineMode::PrivateAccount);
    let saved = save(&env, &store, None, &input, &Scope::Personal, false).unwrap();
    assert_ne!(handle.account, saved.machine_id);
    env.registry
        .update_issued_passphrases(|issued| {
            issued.insert(handle.account.clone(), 0);
        })
        .unwrap();
    sweep_passphrases(&env, &store, None);
    assert!(passphrase::load(&store, &handle).is_ok());
    assert!(env.registry.all_declarations().unwrap().found.is_empty());
}

#[tokio::test]
async fn private_machine_authority_uses_the_registered_app_state_wrapper() {
    use crate::commands::local_profile::{LocalProfileState, MachineOwner};
    let temp = tempfile::tempdir().unwrap();
    let manager =
        LocalProfileState::for_test(temp.path(), 41, MachineOwner::LegacyGithub { id: 41 });
    let app = tauri::test::mock_builder()
        .manage(manager)
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    assert_eq!(
        private_authority_for_app(app.handle())
            .await
            .unwrap()
            .owner_id(),
        41
    );
}

#[test]
fn private_reserved_shadow_refuses_without_usable_authority_and_actions_work() {
    let root = tempfile::tempdir().unwrap();
    let mut env = env_for(root.path(), 41);
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    env.private_authority = Some(authority);
    let store = FakeStore::default();
    let mut input = draft(MachineSignIn::Agent);
    input.mode = Some(MachineMode::PrivateAccount);
    let saved = save(&env, &store, None, &input, &team(), false).unwrap();
    set_hidden(&env, &saved.machine_id, true).unwrap();
    assert!(
        summary_of(&env, &saved.machine_id)
            .unwrap()
            .binding
            .unwrap()
            .hidden
    );
    set_auto_connect(&env, &saved.machine_id, false).unwrap();
    assert!(set_auto_connect(&env, &saved.machine_id, true).is_err());
    assert_eq!(
        unshare(&env, &saved.machine_id).unwrap().scope,
        Some(Scope::Personal)
    );
    let authority = env.private_authority.take();
    let unavailable = private_setup::summaries(&env).unwrap();
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0].machine_id, saved.machine_id);
    assert!(unavailable[0].binding.is_none());
    assert!(!unavailable[0].remote_mcp_control);
    assert!(matches!(
        unavailable[0].status,
        MachineStatus::ConnectionFailed { .. }
    ));
    assert!(private_setup::credential_accounts(&env).unwrap().is_empty());
    assert!(open_pinned_saved(&env, &store, &saved.machine_id).is_err());
    let mut shadow = declaration(&saved.machine_id);
    shadow.pinned_server_id = Some("legacy-shadow-sentinel".into());
    env.registry.write(&Scope::Personal, &shadow).unwrap();
    assert!(private_setup::claims(&env, &saved.machine_id).unwrap());
    assert!(open_pinned_saved(&env, &store, &saved.machine_id).is_err());
    assert!(existing(&env, Some(&saved.machine_id)).is_err());
    env.registry
        .delete(&Scope::Personal, &saved.machine_id)
        .unwrap();
    env.private_authority = authority;
    remove(&env, &store, &saved.machine_id).unwrap();
    assert!(private_setup::claims(&env, &saved.machine_id).unwrap());
    assert!(manager_record(&env, &saved.machine_id).unwrap().is_none());
    assert!(private_setup::summaries(&env).unwrap().is_empty());
}

#[test]
fn remote_mcp_control_is_personal_ready_only_and_revokes_binding_generation() {
    use registry::private::{PrivateAccount, PrivateRegistry, PrivateSetup};
    let root = tempfile::tempdir().unwrap();
    let mut env = env_for(root.path(), 41);
    let (authority, retire) = crate::commands::auth::private_authority::test_snapshot(41);
    env.private_authority = Some(authority);
    let store = FakeStore::default();
    let mut input = draft(MachineSignIn::Agent);
    input.mode = Some(MachineMode::PrivateAccount);
    let saved = save(&env, &store, None, &input, &Scope::Personal, false).unwrap();
    let id = &saved.machine_id;
    assert!(!saved.remote_mcp_control);
    assert!(private_setup::set_remote_mcp_control(&env, id, true).is_err());
    assert!(private_setup::authorize_remote_mcp_control(&env, id).is_err());
    let private = PrivateRegistry::new(
        env.registry.clone(),
        env.private_authority.as_ref().unwrap(),
    )
    .unwrap();
    private.pin_draft(id, "ssh-ed25519 test").unwrap();
    let mut binding = private.binding(id).unwrap().unwrap();
    let old_revision = binding.revision;
    // Existing pre-permission documents remain valid and fail closed.
    let mut old_document = serde_json::to_value(&binding).unwrap();
    old_document
        .as_object_mut()
        .unwrap()
        .remove("remote_mcp_control");
    let old_binding: registry::private::PrivateBinding =
        serde_json::from_value(old_document).unwrap();
    assert!(!old_binding.remote_mcp_control);
    binding.setup = PrivateSetup::Ready {
        account: PrivateAccount {
            uid: 1001,
            canonical_home: "/home/owner".into(),
            root_version: 1,
            enrollment_id: uuid::Uuid::new_v4().to_string(),
        },
        server_id: uuid::Uuid::new_v4().to_string(),
    };
    binding.host_key = Some("ssh-ed25519 test".into());
    binding.revision += 1;
    private
        .save_binding(id, binding, Some(old_revision))
        .unwrap();
    let before = private_setup::generation(&env, id).unwrap();
    private_setup::set_remote_mcp_control(&env, id, true).unwrap();
    private_setup::authorize_remote_mcp_control(&env, id).unwrap();
    let enabled = private.binding(id).unwrap().unwrap();
    assert!(enabled.remote_mcp_control);
    assert!(!enabled.auto_connect);
    assert_ne!(before, private_setup::generation(&env, id).unwrap());
    assert!(summary_of(&env, id).unwrap().remote_mcp_control);
    let declaration_bytes = serde_json::to_value(private.capture(id).unwrap().unwrap().1).unwrap();
    assert!(!declaration_bytes.to_string().contains("remote_mcp_control"));
    private_setup::set_remote_mcp_control(&env, id, false).unwrap();
    assert!(private.binding(id).unwrap().unwrap().revision > enabled.revision);
    assert!(private_setup::authorize_remote_mcp_control(&env, id).is_err());
    private_setup::set_remote_mcp_control(&env, id, true).unwrap();
    retire();
    assert!(private_setup::authorize_remote_mcp_control(&env, id).is_err());
    assert!(private_setup::set_remote_mcp_control(&env, id, true).is_err());
    assert!(!this_mac(0).remote_mcp_control);
}

fn heard_machines_changed(
    app: &tauri::App<tauri::test::MockRuntime>,
) -> Arc<std::sync::Mutex<Vec<MachinesChanged>>> {
    use tauri::Listener;
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = heard.clone();
    app.listen_any(MACHINES_CHANGED_EVENT, move |event| {
        into.lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    heard
}

fn changed(scope: Option<Scope>, machine_id: &str) -> MachinesChanged {
    MachinesChanged {
        scope,
        machine_id: machine_id.into(),
    }
}

/// A registry write reaches both the connection manager and the UI, and so
/// does every connection-state transition the manager then makes.
#[tokio::test]
async fn registry_writes_and_connection_transitions_announce_machines_changed() {
    use super::super::host_connection_manager::HostConnectionManager;
    let root = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(HostConnectionManager::default());
    let manager = app.state::<HostConnectionManager>();
    let forward = app.handle().clone();
    manager.subscribe(Arc::new(move |event| {
        forward_manager_event(&forward, &event)
    }));
    let heard = heard_machines_changed(&app);

    // Saved over MCP: the manager registers it and the UI hears its layer.
    let env = env_for(root.path(), 7);
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &team(),
        false,
    )
    .unwrap();
    let id = saved.machine_id.clone();
    let (scope, _) = observe_registry_write(
        &manager,
        manager.reserve_observation(),
        env_for(root.path(), 7),
        id.clone(),
    )
    .await
    .unwrap();
    announce_registry_write(app.handle(), None, scope, &id, None);
    assert!(manager.is_member(&id));
    assert!(heard.lock().unwrap().contains(&changed(Some(team()), &id)));

    // A binding-only write (hide) is announced too.
    heard.lock().unwrap().clear();
    set_hidden(&env, &id, true).unwrap();
    let (scope, _) = observe_registry_write(
        &manager,
        manager.reserve_observation(),
        env_for(root.path(), 7),
        id.clone(),
    )
    .await
    .unwrap();
    announce_registry_write(app.handle(), None, scope, &id, None);
    assert!(heard.lock().unwrap().contains(&changed(Some(team()), &id)));

    // A connection transition carries the machine's layer as well.
    heard.lock().unwrap().clear();
    manager.disconnect(&id).await.unwrap();
    assert!(
        heard.lock().unwrap().contains(&changed(Some(team()), &id)),
        "{:?}",
        heard.lock().unwrap()
    );

    // Unsharing moves it: both the team list and every personal list re-read.
    heard.lock().unwrap().clear();
    let prior = current_scope(&env, &id);
    unshare(&env, &id).unwrap();
    let (scope, _) = observe_registry_write(
        &manager,
        manager.reserve_observation(),
        env_for(root.path(), 7),
        id.clone(),
    )
    .await
    .unwrap();
    announce_registry_write(app.handle(), prior, scope, &id, None);
    let heard_now = heard.lock().unwrap().clone();
    assert!(
        heard_now.contains(&changed(Some(team()), &id)),
        "{heard_now:?}"
    );
    assert!(
        heard_now.contains(&changed(Some(Scope::Personal), &id)),
        "{heard_now:?}"
    );

    // Removing a machine the manager never knew is still announced.
    heard.lock().unwrap().clear();
    announce_registry_write(
        app.handle(),
        Some(Scope::Personal),
        None,
        "gone-123456",
        None,
    );
    assert_eq!(
        *heard.lock().unwrap(),
        vec![changed(Some(Scope::Personal), "gone-123456")]
    );
}

/// A registry write the connection manager announces itself is not
/// announced a second time: one write, one list re-read per layer.
#[tokio::test]
async fn a_registry_write_is_announced_once_per_layer() {
    use super::super::host_connection_manager::HostConnectionManager;
    let root = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(HostConnectionManager::default());
    let manager = app.state::<HostConnectionManager>();
    let forward = app.handle().clone();
    manager.subscribe(Arc::new(move |event| {
        forward_manager_event(&forward, &event)
    }));
    let heard = heard_machines_changed(&app);

    let env = env_for(root.path(), 7);
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &team(),
        false,
    )
    .unwrap();
    let id = saved.machine_id.clone();
    let mark = machines_changed_mark(app.handle());
    let (scope, _) = observe_registry_write(
        &manager,
        manager.reserve_observation(),
        env_for(root.path(), 7),
        id.clone(),
    )
    .await
    .unwrap();
    // The manager registered the new member and announced it already.
    assert!(heard.lock().unwrap().contains(&changed(Some(team()), &id)));
    announce_registry_write(app.handle(), None, scope, &id, Some(mark));
    let heard_now = heard.lock().unwrap().clone();
    assert_eq!(
        heard_now
            .iter()
            .filter(|event| **event == changed(Some(team()), &id))
            .count(),
        1,
        "{heard_now:?}"
    );

    // A write the manager has nothing to say about is still announced.
    heard.lock().unwrap().clear();
    let mark = machines_changed_mark(app.handle());
    announce_registry_write(app.handle(), Some(team()), None, "gone-123456", Some(mark));
    assert_eq!(
        *heard.lock().unwrap(),
        vec![changed(Some(team()), "gone-123456")]
    );
}

/// A teammate's machine arriving through git (the registry watcher sees a
/// new team file) is announced in its team's layer and then listed.
#[tokio::test]
async fn a_team_machine_arriving_through_the_registry_watcher_is_announced_and_listed() {
    use super::super::host_connection_manager::HostConnectionManager;
    let root = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(HostConnectionManager::default());
    let manager = app.state::<HostConnectionManager>();
    let forward = app.handle().clone();
    manager.subscribe(Arc::new(move |event| {
        forward_manager_event(&forward, &event)
    }));
    let heard = heard_machines_changed(&app);
    let env = env_for(root.path(), 7);

    // The pull writes the file; nothing in this app saved it.
    env.registry
        .write(&team(), &declaration("arrived-123456"))
        .unwrap();
    let (records, unreadable) = manager_records(&env).unwrap();
    manager
        .observe_external_keeping(
            manager.reserve_observation(),
            records,
            ["arrived-123456".to_string()].into(),
            true,
            Vec::new(),
            &unreadable,
        )
        .await;

    assert!(
        heard
            .lock()
            .unwrap()
            .contains(&changed(Some(team()), "arrived-123456")),
        "{:?}",
        heard.lock().unwrap()
    );
    assert!(manager.is_member("arrived-123456"));
    assert!(machines_for(&env, "acme/context", Some("platform"), 0)
        .unwrap()
        .iter()
        .any(|summary| summary.machine_id == "arrived-123456"));
}

#[test]
fn an_unreadable_machine_file_is_named_in_its_teams_list_not_hidden() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    env.registry
        .write(&team(), &declaration("good-123456"))
        .unwrap();
    let dir = env.registry.dir(&team()).unwrap();
    std::fs::write(dir.join("bad-123456.yaml"), "machine_id: [not yaml").unwrap();
    std::fs::write(dir.join("notes.txt"), "not a machine").unwrap();

    let files = unreadable_machine_files(&env, "acme/context", Some("platform")).unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].machine_id, "bad-123456");
    assert_eq!(files[0].scope, team());
    assert!(files[0].path.ends_with("bad-123456.yaml"), "{files:?}");
    assert!(!files[0].error.is_empty());
    assert!(
        unreadable_machine_file(&env, "acme/context", Some("platform"), "good-123456").is_err()
    );
    // Another team's list does not name it.
    assert!(
        unreadable_machine_files(&env, "acme/context", Some("other"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn only_a_plain_unreadable_file_is_opened() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let dir = env.registry.dir(&team()).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    // A teammate commits links that fail to parse, pointing at things the
    // default handler would run.
    let script = root.path().join("evil.command");
    std::fs::write(&script, "#!/bin/sh\necho pwned\n").unwrap();
    let app = root.path().join("Evil.app");
    std::fs::create_dir(&app).unwrap();
    std::os::unix::fs::symlink(&script, dir.join("evil-123456.yaml")).unwrap();
    std::os::unix::fs::symlink(&app, dir.join("evilapp-123456.yaml")).unwrap();
    std::fs::create_dir(dir.join("folder-123456.yaml")).unwrap();
    std::fs::write(dir.join("bad-123456.yaml"), "machine_id: [not yaml").unwrap();

    let files = unreadable_machine_files(&env, "acme/context", Some("platform")).unwrap();
    let ids: Vec<_> = files.iter().map(|f| f.machine_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "bad-123456",
            "evil-123456",
            "evilapp-123456",
            "folder-123456"
        ]
    );
    for file in &files {
        let opened = plain_file(Path::new(&file.path));
        if file.machine_id == "bad-123456" {
            assert!(opened.is_ok(), "{opened:?}");
        } else {
            assert!(opened.is_err(), "{} was let through", file.machine_id);
        }
    }
}

#[test]
fn a_machine_whose_file_stays_unreadable_runs_out_its_grace() {
    let grace = UnreadableGrace::default();
    let start = std::time::Instant::now();
    let period = std::time::Duration::from_secs(30);
    let bad: std::collections::BTreeSet<String> = ["bad-123456".to_string()].into();

    assert_eq!(
        grace.note(&bad, start).fresh,
        vec!["bad-123456".to_string()]
    );
    // Seen again: still the same grace period, not a new one.
    assert!(grace.note(&bad, start + period / 2).fresh.is_empty());
    assert!(!grace.expired("bad-123456", start + period / 2, period));
    assert!(grace.expired("bad-123456", start + period, period));

    // Repaired before the end: forgotten, so it never disconnects.
    assert!(grace.refresh(&Default::default()).is_empty());
    assert!(!grace.expired("bad-123456", start + period * 2, period));
}

/// Unreadable past its grace, disconnected, then repaired byte for byte (the
/// manager sees the same fingerprint as before): it reconnects in the
/// background again, because the disconnect was not the person's.
#[tokio::test]
async fn a_grace_disconnect_lifts_once_the_file_reads_again() {
    use super::super::host_connection_manager::{HostConnectionManager, MachineManagerRecord};
    let manager = HostConnectionManager::default();
    let record = MachineManagerRecord {
        machine_id: "box-123456".into(),
        scope: Some(team()),
        revision: None,
        fingerprint: "same".into(),
        set_up: true,
        auto_connect: true,
        open_tabs: false,
    };
    manager
        .observe_external(
            manager.reserve_observation(),
            vec![record.clone()],
            ["box-123456".to_string()].into(),
            true,
            Vec::new(),
        )
        .await;
    assert_eq!(manager.auto_connect_candidates(), ["box-123456"]);

    // Unreadable: kept as it was, then its grace runs out.
    let grace = UnreadableGrace::default();
    let bad: std::collections::BTreeSet<String> = ["box-123456".to_string()].into();
    let start = std::time::Instant::now();
    grace.note(&bad, start);
    manager
        .observe_external_keeping(
            manager.reserve_observation(),
            Vec::new(),
            Default::default(),
            true,
            Vec::new(),
            &bad,
        )
        .await;
    assert!(grace.expired("box-123456", start + UNREADABLE_GRACE, UNREADABLE_GRACE));
    grace.mark_disconnected("box-123456");
    manager.disconnect("box-123456").await.unwrap();
    assert!(manager.auto_connect_candidates().is_empty());

    // Repaired with the same bytes: the manager sees no change...
    manager
        .observe_external(
            manager.reserve_observation(),
            vec![record],
            ["box-123456".to_string()].into(),
            true,
            Vec::new(),
        )
        .await;
    assert!(manager.auto_connect_candidates().is_empty());
    // ...but the grace ledger does, and lifts the suppression.
    let note = grace.note(&Default::default(), start + UNREADABLE_GRACE * 2);
    assert_eq!(note.repaired, vec!["box-123456".to_string()]);
    resume_repaired(&manager, &note.repaired);
    assert_eq!(manager.auto_connect_candidates(), ["box-123456"]);
}

/// After the app (and so the manager) restarts, rebuilding from the registry
/// gives connect every machine the list shows.
#[tokio::test]
async fn registry_and_manager_agree_after_a_frontend_restart() {
    use super::super::host_connection_manager::HostConnectionManager;
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let store = FakeStore::default();
    for (label, scope) in [("personal box", Scope::Personal), ("team box", team())] {
        let mut input = draft(MachineSignIn::Agent);
        input.label = label.into();
        save(&env, &store, None, &input, &scope, false).unwrap();
    }
    let listed: Vec<String> = machines_for(&env, "acme/context", Some("platform"), 0)
        .unwrap()
        .into_iter()
        .filter(|machine| machine.machine_id != LOCAL_MACHINE_ID)
        .map(|machine| machine.machine_id)
        .collect();
    assert_eq!(listed.len(), 2);

    // The manager a fresh start holds before (or without) the registry.
    let manager = HostConnectionManager::default();
    assert!(listed.iter().all(|id| !manager.is_member(id)));
    reconcile_manager(&manager, env_for(root.path(), 7))
        .await
        .unwrap();
    assert!(listed.iter().all(|id| manager.is_member(id)), "{listed:?}");

    // A second re-register (another reload) changes nothing.
    reconcile_manager(&manager, env_for(root.path(), 7))
        .await
        .unwrap();
    assert!(listed.iter().all(|id| manager.is_member(id)));
}

/// `connect_machine` / `retry_machine` as the app runs them, minus the app:
/// the shared entry, the registry half of `ensure_manager_member`, and a
/// manager whose SSH opens are refused (so reaching "ssh refused" means the
/// machine was registered and an attempt began).
async fn connect_via_entry(
    manager: &super::super::host_connection_manager::HostConnectionManager,
    root: &Path,
    machine_id: &str,
    origin: super::super::host_connection_manager::ConnectOrigin,
) -> Result<MachineSummary, String> {
    super::super::host_connection_manager::connection_command(
        manager,
        None,
        ensure_member(manager, env_for(root, 7), machine_id),
        machine_id.to_owned(),
        origin,
        || async { Err("no summary in this test".to_string()) },
    )
    .await
}

/// One malformed declaration (say, from a git sync) must not cost every
/// other machine its connect: the list skips it, and so does connect.
#[tokio::test]
async fn a_bad_unrelated_file_still_lets_a_healthy_machine_connect_and_retry() {
    use super::super::host_connection_manager::{ConnectOrigin, HostConnectionManager};
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let dir = env.registry.dir(&team()).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad-123456.yaml"), "machine_id: [not yaml").unwrap();

    let listed: Vec<String> = machines_for(&env, "acme/context", Some("platform"), 0)
        .unwrap()
        .into_iter()
        .filter(|machine| machine.machine_id != LOCAL_MACHINE_ID)
        .map(|machine| machine.machine_id)
        .collect();
    assert_eq!(listed, std::slice::from_ref(&saved.machine_id));

    for origin in [ConnectOrigin::ExplicitForeground, ConnectOrigin::Retry] {
        // A fresh manager: what an app restart holds.
        let manager = HostConnectionManager::refusing_ssh();
        let result = connect_via_entry(&manager, root.path(), &saved.machine_id, origin).await;
        assert_eq!(result, Err("ssh refused".into()), "{origin:?}");
        assert!(manager.is_member(&saved.machine_id));
    }
}

/// When the broad read fails outright (a layer that can't be listed),
/// connect still registers the requested machine from its own record.
#[tokio::test]
async fn a_failed_broad_read_falls_back_to_the_requested_machine() {
    use super::super::host_connection_manager::{ConnectOrigin, HostConnectionManager};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let locked = env.registry.dir(&team()).unwrap();
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let broad = env.registry.all_declarations();

    let manager = HostConnectionManager::refusing_ssh();
    let result = connect_via_entry(
        &manager,
        root.path(),
        &saved.machine_id,
        ConnectOrigin::ExplicitForeground,
    )
    .await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(broad.is_err(), "the layer must be unlistable for this test");
    assert_eq!(result, Err("ssh refused".into()));
    assert!(manager.is_member(&saved.machine_id));
}

/// When the requested machine's own file is the bad one, connect names it.
#[tokio::test]
async fn a_bad_requested_file_gets_a_reason_naming_the_file() {
    use super::super::host_connection_manager::{ConnectOrigin, HostConnectionManager};
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let dir = env.registry.dir(&Scope::Personal).unwrap();
    std::fs::write(dir.join("bad-123456.yaml"), "machine_id: [not yaml").unwrap();

    for origin in [ConnectOrigin::ExplicitForeground, ConnectOrigin::Retry] {
        let manager = HostConnectionManager::refusing_ssh();
        let reason = connect_via_entry(&manager, root.path(), "bad-123456", origin)
            .await
            .unwrap_err();
        assert!(reason.contains("can't be read"), "{reason}");
        assert!(reason.contains("bad-123456.yaml"), "{reason}");
        assert!(!reason.contains("not registered"), "{reason}");
    }
    let manager = HostConnectionManager::refusing_ssh();
    let reason = connect_via_entry(
        &manager,
        root.path(),
        "never-123456",
        ConnectOrigin::ExplicitForeground,
    )
    .await
    .unwrap_err();
    assert!(reason.contains("isn't saved"), "{reason}");
}

/// A corrupt file arriving later leaves the machine as the manager knew it.
#[tokio::test]
async fn a_reconcile_never_reads_an_unreadable_file_as_a_removal() {
    use super::super::host_connection_manager::HostConnectionManager;
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 7);
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &draft(MachineSignIn::Agent),
        &Scope::Personal,
        false,
    )
    .unwrap();
    let manager = HostConnectionManager::default();
    reconcile_manager(&manager, env_for(root.path(), 7))
        .await
        .unwrap();
    assert!(manager.is_member(&saved.machine_id));
    let path = env
        .registry
        .dir(&Scope::Personal)
        .unwrap()
        .join(format!("{}.yaml", saved.machine_id));
    std::fs::write(&path, "machine_id: [not yaml").unwrap();
    reconcile_manager(&manager, env_for(root.path(), 7))
        .await
        .unwrap();
    assert!(manager.is_member(&saved.machine_id));
}

/// A saved, set-up legacy machine `id` at `input`'s address with background
/// reconnect and Hide on.
fn save_legacy_machine(env: &Env, id: &str, input: &MachineDraftInput) {
    env.registry
        .write(
            &Scope::Personal,
            &Declaration {
                machine_id: id.into(),
                label: input.label.clone(),
                host: input.host.clone(),
                port: input.port,
                jump_host: None,
                default_base_dir: None,
                pinned_host_key: Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExM".into()),
                pinned_server_id: Some("legacy-server".into()),
            },
        )
        .unwrap();
    env.registry
        .update_bindings(|bindings| {
            bindings.insert(
                id.into(),
                Binding {
                    username: input.username.clone(),
                    key_path: Some("/tmp/legacy-key".into()),
                    auto_connect: true,
                    hidden: true,
                    set_up: true,
                    ..Default::default()
                },
            );
        })
        .unwrap();
}

fn legacy_test_env(root: &Path) -> Env {
    let mut env = env_for(root, 41);
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    std::mem::forget(_retire);
    env.private_authority = Some(authority);
    env
}

#[test]
fn saving_a_legacy_machine_as_a_private_account_converts_it_in_place() {
    let root = tempfile::tempdir().unwrap();
    let mut env = env_for(root.path(), 41);
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    env.private_authority = Some(authority);
    let store = FakeStore::default();
    let mut input = draft(MachineSignIn::KeyFile {
        key_path: "/tmp/legacy-key".into(),
    });
    let id = "dev-box-legacy";
    save_legacy_machine(&env, id, &input);

    // A saved legacy record set up as a private account never needs repair.
    input.mode = Some(MachineMode::PrivateAccount);
    let saved = save(&env, &store, Some(id), &input, &Scope::Personal, false).unwrap();

    assert_eq!(saved.machine_id, id);
    let declaration = saved.declaration.unwrap();
    assert_eq!(declaration.mode, MachineMode::PrivateAccount);
    assert_eq!(
        declaration.pinned_host_key_fp,
        HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExM"
        )
        .ok()
        .map(|key| key.fingerprint())
    );
    let binding = saved.binding.unwrap();
    assert!(binding.hidden);
    // Background reconnect waits for the private setup to be ready.
    assert!(!binding.auto_connect);
    assert!(env.registry.find(id).unwrap().is_none());
    assert!(private_setup::claims(&env, id).unwrap());
}

#[test]
fn a_missing_mode_is_private_for_a_new_machine_and_the_saved_mode_for_a_legacy_one() {
    let root = tempfile::tempdir().unwrap();
    let env = legacy_test_env(root.path());
    let input: MachineDraftInput = serde_json::from_value(serde_json::json!({
        "label": "Dev Box", "host": "devbox.int", "port": 2222,
        "username": "quinn", "jumpHost": null,
        "signIn": { "method": "keyFile", "keyPath": "/tmp/legacy-key" }, "trustedHostKeyFp": null,
    }))
    .unwrap();
    assert_eq!(input.mode, None);
    assert_eq!(
        effective_mode(&env, None, &input).unwrap(),
        MachineMode::PrivateAccount
    );
    save_legacy_machine(&env, "dev-box-legacy", &input);
    assert_eq!(
        effective_mode(&env, Some("dev-box-legacy"), &input).unwrap(),
        MachineMode::Legacy
    );
}

#[test]
fn an_mcp_save_of_a_legacy_machine_without_a_mode_keeps_it_legacy() {
    let root = tempfile::tempdir().unwrap();
    let env = legacy_test_env(root.path());
    let store = FakeStore::default();
    let params: crate::mcp::machine_tools::DraftParams =
        serde_json::from_value(serde_json::json!({
            "label": "Dev Box", "host": "devbox.int", "port": 2222, "username": "quinn",
            "signIn": { "method": "keyFile", "keyPath": "/tmp/legacy-key" },
        }))
        .unwrap();
    let input = MachineDraftInput::from(params);
    let id = "dev-box-legacy";
    save_legacy_machine(&env, id, &input);
    let saved = save(&env, &store, Some(id), &input, &Scope::Personal, false).unwrap();
    assert_eq!(saved.declaration.unwrap().mode, MachineMode::Legacy);
    assert!(env.registry.find(id).unwrap().is_some());
    assert!(!private_setup::claims(&env, id).unwrap());
}

#[test]
fn testing_a_legacy_machine_as_a_private_account_never_converts_it() {
    let root = tempfile::tempdir().unwrap();
    let env = legacy_test_env(root.path());
    let store = FakeStore::default();
    let mut input = draft(MachineSignIn::KeyFile {
        key_path: "/tmp/legacy-key".into(),
    });
    // Nothing listens here: the check fails fast, and must still write nothing.
    input.host = "127.0.0.1".into();
    input.port = 9;
    let id = "dev-box-legacy";
    save_legacy_machine(&env, id, &input);
    let bindings = env.registry.bindings().unwrap();
    input.mode = Some(MachineMode::PrivateAccount);
    let _ = test_connection(&env, &store, Some(id), &input);
    assert!(env.registry.find(id).unwrap().is_some());
    assert_eq!(env.registry.bindings().unwrap(), bindings);
    assert!(!private_setup::claims(&env, id).unwrap());

    let mut moved = input.clone();
    moved.port = 10;
    let error = test_connection(&env, &store, Some(id), &moved).unwrap_err();
    assert!(error.contains("Undo the address change"), "{error}");
}

#[test]
fn a_retried_save_of_a_new_machine_under_its_minted_id_never_adds_a_twin() {
    let root = tempfile::tempdir().unwrap();
    let mut env = env_for(root.path(), 41);
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    env.private_authority = Some(authority);
    let store = FakeStore::default();
    let mut input = draft(MachineSignIn::Agent);
    input.mode = Some(MachineMode::PrivateAccount);
    let id = registry::new_machine_id("droplet");
    let first = save(&env, &store, Some(&id), &input, &Scope::Personal, false).unwrap();
    let again = save(&env, &store, Some(&id), &input, &Scope::Personal, false).unwrap();
    assert_eq!(first.machine_id, id);
    assert_eq!(again.machine_id, id);
    assert_eq!(private_setup::summaries(&env).unwrap().len(), 1);
    assert!(save(&env, &store, Some("../x"), &input, &Scope::Personal, false).is_err());
}

#[test]
fn an_agent_key_signs_in_through_a_public_key_file_naming_only_it() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let line =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl quinn@mac";
    let keys = || codesense_session_server::ssh::parse_agent_keys(line);
    let kept = agent_key_dtos(Some(home.path()), keys())[0]
        .identity_file
        .clone()
        .unwrap();
    assert!(kept.contains("/.codesense/agent-keys/"), "{kept}");
    assert_eq!(std::fs::read_to_string(&kept).unwrap().trim(), line);
    let mode = std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(
        agent_key_dtos(Some(home.path()), keys())[0].identity_file,
        Some(kept),
        "the same key keeps one file"
    );

    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::write(home.path().join(".ssh/work.pub"), format!("{line}\n")).unwrap();
    std::fs::write(
        home.path().join(".ssh/other.pub"),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherKeyOtherKeyOtherKeyOtherKeyOtherKeyOt x\n",
    )
    .unwrap();
    assert_eq!(
        agent_key_dtos(Some(home.path()), keys())[0].identity_file,
        Some(
            home.path()
                .join(".ssh/work.pub")
                .to_string_lossy()
                .into_owned()
        ),
        "the user's own public key file is used when it names the key"
    );
}

/// SW-AGR-19 (replacing SW-XS-6's capability check): a remote server this
/// app ships is its own; one predating the build stamp is older than this
/// app; a newer protocol is newer. The flag serializes for the UI.
#[test]
fn only_an_older_unshipped_terminal_server_prompts_an_update() {
    use std::collections::HashSet;
    use terminal_server_info::{remote_info, ServerRelation};
    let version = codesense_session_server::install::package_version();
    let shipped: HashSet<String> = [format!("{version}-aaaaaaaaaaaa")].into_iter().collect();
    let announce = |digest: &str| {
        codesense_session_server::install::announced_version(Some(&format!("{version}-{digest}")))
    };
    let current = codesense_host_proto::PROTO_MINOR;
    assert!(current >= 16, "the build stamp arrived at minor 16");
    let shipped_build = remote_info(&shipped, &announce("aaaaaaaaaaaa"), current - 1, None);
    assert_eq!(shipped_build.relation, ServerRelation::Same);
    assert!(shipped_build.up_to_date);
    assert_eq!(
        shipped_build.server_version.as_deref(),
        Some(terminal_server_info::app_stamp().version.as_str())
    );
    let other = remote_info(&shipped, &announce("bbbbbbbbbbbb"), current - 1, None);
    assert_eq!(other.relation, ServerRelation::Older);
    assert_eq!(other.server_version, None);
    assert_eq!(other.build_digest.as_deref(), Some("bbbbbbbbbbbb"));
    assert_eq!(
        remote_info(&shipped, "99.0.0+cccccccccccc", current + 1, None).relation,
        ServerRelation::Newer
    );
    let status = MachineStatus::Connected {
        workspaces: Some(1),
        terminal_server_update: true,
        terminal_server_sessions: Some(3),
    };
    assert_eq!(
        serde_json::to_value(&status).unwrap(),
        serde_json::json!({"state": "connected", "workspaces": 1, "terminalServerUpdate": true, "terminalServerSessions": 3})
    );
}

#[test]
fn changed_team_identity_is_refused_before_ssh_opens() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut decl = declaration(id);
    decl.pinned_server_id = Some("accepted-server".into());
    env.registry.write(&team(), &decl).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                id.into(),
                Binding {
                    set_up: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths().pin(id, &HostKey::parse(KEY).unwrap()).unwrap();
    transport_snapshot(&env, id).unwrap();
    decl.host = "changed.int".into();
    env.registry.write(&team(), &decl).unwrap();
    let error = transport_snapshot(&env, id).unwrap_err();
    assert!(
        error.contains("Machine identity changed in team config"),
        "{error}"
    );
}

fn accepted_team_machine(env: &Env, id: &str) -> Declaration {
    let mut decl = declaration(id);
    decl.pinned_server_id = Some("accepted-server".into());
    env.registry.write(&team(), &decl).unwrap();
    env.registry
        .update_bindings(|all| {
            all.insert(
                id.into(),
                Binding {
                    set_up: true,
                    auto_connect: true,
                    ..Binding::default()
                },
            );
        })
        .unwrap();
    env.paths().pin(id, &HostKey::parse(KEY).unwrap()).unwrap();
    transport_snapshot(env, id).unwrap();
    decl
}

fn identity_review(env: &Env, id: &str) -> String {
    let dto = serde_json::to_value(summary_of(env, id).unwrap()).unwrap();
    dto["declaration"]["identityChange"]["revision"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn every_accepted_identity_field_blocks_connect_setup_launch_and_shared_mcp() {
    for field in ["host", "port", "jump", "key", "server"] {
        let root = tempfile::tempdir().unwrap();
        let env = env_for(root.path(), 1);
        let store = FakeStore::default();
        let id = "gpu-box-1a2b3c";
        let mut decl = accepted_team_machine(&env, id);
        match field {
            "host" => decl.host = "different.int".into(),
            "port" => decl.port = 2222,
            "jump" => decl.jump_host = Some("bastion.int".into()),
            "key" => decl.pinned_host_key = Some(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExM"
                    .into(),
            ),
            "server" => decl.pinned_server_id = Some("new-server".into()),
            _ => unreachable!(),
        }
        env.registry.write(&team(), &decl).unwrap();
        assert!(
            transport_snapshot(&env, id)
                .unwrap_err()
                .contains(identity::REFUSAL),
            "{field}"
        );
        assert!(
            open_pinned_saved(&env, &store, id)
                .err()
                .unwrap()
                .to_string()
                .contains(identity::REFUSAL),
            "{field}"
        );
        let input = MachineDraftInput {
            mode: Some(MachineMode::Legacy),
            host: decl.host.clone(),
            port: decl.port,
            jump_host: decl.jump_host.clone(),
            ..draft(MachineSignIn::Agent)
        };
        assert!(test_connection(&env, &store, Some(id), &input)
            .unwrap_err()
            .contains(identity::REFUSAL));
        assert!(install_server(&env, &store, Some(id), &input)
            .unwrap_err()
            .contains(identity::REFUSAL));
        assert!(save(&env, &store, Some(id), &input, &team(), true)
            .unwrap_err()
            .contains(identity::REFUSAL));
        // Read through the actual registry list, with the declaration's team lens.
        let Scope::Team {
            org_slug,
            team: team_name,
        } = team()
        else {
            unreachable!()
        };
        let list = machines_for(&env, &org_slug, Some(&team_name), 0).unwrap();
        assert!(!terminal_machine_visible(&list, id, &org_slug, &team_name));
        assert!(private_setup::authorize_remote_mcp_control(&env, id).is_err());
        assert!(!manager_record(&env, id).unwrap().unwrap().set_up);
        let machine = list.iter().find(|m| m.machine_id == id).unwrap();
        assert!(matches!(
            machine.status,
            MachineStatus::ConnectionFailed { .. }
        ));
        assert!(!machine.capabilities.agent_callbacks);
    }
}

#[test]
fn upgrade_adopts_once_and_confirm_alone_replaces_acceptance() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut decl = accepted_team_machine(&env, id);
    let original = env.registry.bindings().unwrap()[id]
        .accepted_identity
        .clone();
    decl.pinned_server_id = Some("new-server".into());
    env.registry.write(&team(), &decl).unwrap();
    let reopened = env_for(root.path(), 1);
    assert_eq!(
        reopened.registry.bindings().unwrap()[id].accepted_identity,
        original
    );
    let revision = identity_review(&env, id);
    identity::resolve(&env, id, &revision, false).unwrap();
    assert_eq!(
        env.registry.bindings().unwrap()[id].accepted_identity,
        original
    );
    assert!(identity::resolve(&env, id, "stale", true).is_err());
    identity::resolve(&env, id, &revision, true).unwrap();
    let saved = transport_snapshot(&env, id).unwrap();
    assert_eq!(
        saved
            .binding
            .accepted_identity
            .unwrap()
            .server_id
            .as_deref(),
        Some("new-server")
    );
    assert!(manager_record(&env, id).unwrap().unwrap().set_up);
    decl.host = "changed-again.int".into();
    env.registry.write(&team(), &decl).unwrap();
    assert!(identity::resolve(&env, id, &revision, true).is_err());
    assert!(transport_snapshot(&env, id).is_err());
}

#[test]
fn observed_server_identity_refuses_without_becoming_the_accepted_pin() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    accepted_team_machine(&env, id);
    let binding = env.registry.bindings().unwrap().remove(id).unwrap();
    assert!(identity::verify_server(&env, id, &binding, "other-server")
        .unwrap_err()
        .contains(identity::REFUSAL));
    let now = env.registry.bindings().unwrap().remove(id).unwrap();
    assert_eq!(now.accepted_identity, binding.accepted_identity);
    assert_eq!(now.observed_server_id.as_deref(), Some("other-server"));
    assert!(transport_snapshot(&env, id).is_err());
    let dto = serde_json::to_value(summary_of(&env, id).unwrap()).unwrap();
    assert_eq!(
        dto["declaration"]["identityChange"]["observedServerId"],
        "other-server"
    );
    identity::resolve(&env, id, &identity_review(&env, id), true).unwrap();
    let snapshot = transport_snapshot(&env, id).unwrap();
    assert_eq!(
        snapshot.binding.accepted_identity,
        binding.accepted_identity
    );
    assert!(identity::verify_server(&env, id, &snapshot.binding, "other-server").is_err());
}

#[test]
fn label_and_folder_edits_do_not_require_identity_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut decl = accepted_team_machine(&env, id);
    decl.label = "A new label".into();
    decl.default_base_dir = Some("/different/folder".into());
    env.registry.write(&team(), &decl).unwrap();
    transport_snapshot(&env, id).unwrap();
    assert!(summary_of(&env, id)
        .unwrap()
        .declaration
        .unwrap()
        .identity_change
        .is_none());
}

#[test]
fn identity_review_shows_the_declarations_git_author_and_date_when_available() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut decl = accepted_team_machine(&env, id);
    decl.host = "new.int".into();
    env.registry.write(&team(), &decl).unwrap();
    let dir = env.registry.dir(&team()).unwrap();
    let repo = git2::Repository::init(&dir).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(&format!("{id}.yaml"))).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let author = git2::Signature::new(
        "Teammate",
        "team@example.com",
        &git2::Time::new(1_761_048_000, 0),
    )
    .unwrap();
    repo.commit(
        Some("HEAD"),
        &author,
        &author,
        "Change machine endpoint",
        &tree,
        &[],
    )
    .unwrap();
    let dto = serde_json::to_value(summary_of(&env, id).unwrap()).unwrap();
    let change = &dto["declaration"]["identityChange"];
    assert_eq!(change["author"], "Teammate");
    assert!(change["date"].as_str().unwrap().starts_with("2025-10-21"));
    assert_eq!(change["accepted"]["host"], "gpu.int");
    assert_eq!(change["declared"]["host"], "new.int");
}

#[tokio::test]
async fn identity_edit_and_confirmation_invalidate_the_existing_hub_connection() {
    use super::super::host_connection_manager::{HostConnectionManager, HostConnectionState};
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    let mut decl = accepted_team_machine(&env, id);
    let manager = HostConnectionManager::default();
    reconcile_manager(&manager, env_for(root.path(), 1))
        .await
        .unwrap();
    decl.pinned_server_id = Some("new-server".into());
    env.registry.write(&team(), &decl).unwrap();
    reconcile_manager(&manager, env_for(root.path(), 1))
        .await
        .unwrap();
    assert!(!manager.auto_connect_candidates().contains(&id.to_string()));
    let mut summary = summary_of(&env, id).unwrap();
    apply_manager_state(
        &mut summary,
        HostConnectionState::Connected {
            server_id: "old".into(),
        },
    );
    assert!(matches!(
        summary.status,
        MachineStatus::ConnectionFailed { .. }
    ));
    identity::resolve(&env, id, &identity_review(&env, id), true).unwrap();
    reconcile_manager(&manager, env_for(root.path(), 1))
        .await
        .unwrap();
    assert!(manager.auto_connect_candidates().contains(&id.to_string()));
}

#[test]
fn native_endpoint_edit_accepts_its_own_change_while_an_agent_cannot() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 1);
    let id = "gpu-box-1a2b3c";
    accepted_team_machine(&env, id);
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        host: "my-new.int".into(),
        ..draft(MachineSignIn::Agent)
    };
    assert!(identity::validate_saved(&env, id, Some(&input))
        .unwrap_err()
        .contains(identity::REFUSAL));
    let saved = save(
        &env,
        &FakeStore::default(),
        Some(id),
        &input,
        &team(),
        false,
    )
    .unwrap();
    assert!(matches!(saved.status, MachineStatus::NeedsCredential));
    assert!(saved.declaration.unwrap().identity_change.is_none());
    let binding = env.registry.bindings().unwrap().remove(id).unwrap();
    assert_eq!(binding.accepted_identity.unwrap().host, "my-new.int");
    assert!(!binding.set_up);
    // A later team edit is still distinct from the native edit just accepted.
    let (scope, mut decl) = env.registry.find(id).unwrap().unwrap();
    decl.host = "from-team-git.int".into();
    env.registry.write(&scope, &decl).unwrap();
    assert!(identity::validate_saved(&env, id, None).is_err());
}

fn externally_convert_accepted_machine(
    env: &Env,
    id: &str,
    mutate: impl FnOnce(&mut Declaration),
) -> Declaration {
    let mut destination = accepted_team_machine(env, id);
    destination.pinned_server_id = None;
    mutate(&mut destination);
    let private = registry::private::PrivateRegistry::new(
        env.registry.clone(),
        env.private_authority.as_ref().unwrap(),
    )
    .unwrap();
    let dir = private.dir(&team()).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let declaration =
        registry::private::PrivateDeclaration::from_legacy(destination.clone()).unwrap();
    env.registry.delete(&team(), id).unwrap();
    std::fs::write(
        dir.join(format!("{id}.yaml")),
        serde_yaml::to_string(&declaration).unwrap(),
    )
    .unwrap();
    destination
}

#[test]
fn externally_converted_private_destination_is_refused_before_ssh_or_binding_adoption() {
    use std::os::unix::fs::PermissionsExt;
    for field in ["host", "port", "jump", "key"] {
        for operation in ["test", "enroll", "install", "save"] {
            let root = tempfile::tempdir().unwrap();
            let env = legacy_test_env(root.path());
            let id = "converted-team-machine";
            let destination = externally_convert_accepted_machine(&env, id, |declaration| {
                match field {
                "host" => declaration.host = "changed.int".into(),
                "port" => declaration.port = 2223,
                "jump" => declaration.jump_host = Some("changed-jump.int".into()),
                "key" => declaration.pinned_host_key = Some(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExM"
                    .into(),
                ),
                _ => unreachable!(),
            }
            });
            let before = binding_bytes(&env);
            let called = root.path().join("ssh-invoked");
            let program = root.path().join("record-ssh");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
                    called.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let env = Env {
                ssh_program: program,
                ..env
            };
            let input = MachineDraftInput {
                mode: Some(MachineMode::PrivateAccount),
                host: destination.host.clone(),
                port: destination.port,
                jump_host: destination.jump_host.clone(),
                ..draft(MachineSignIn::Agent)
            };
            let store = FakeStore::default();
            let error = match operation {
                "test" => test_connection(&env, &store, Some(id), &input).unwrap_err(),
                "enroll" => private_setup::enroll(&env, &store, Some(id), &input).unwrap_err(),
                "install" => private_setup::install(&env, &store, Some(id), &input).unwrap_err(),
                "save" => save(&env, &store, Some(id), &input, &team(), false).unwrap_err(),
                _ => unreachable!(),
            };
            assert!(
                !called.exists(),
                "{operation} attempted SSH before identity acceptance: {error}"
            );
            assert!(error.contains(identity::REFUSAL), "{operation}: {error}");
            assert_eq!(binding_bytes(&env), before);
            let private = registry::private::PrivateRegistry::new(
                env.registry.clone(),
                env.private_authority.as_ref().unwrap(),
            )
            .unwrap();
            assert!(private.binding(id).unwrap().is_none());
        }
    }
}

fn binding_bytes(env: &Env) -> Vec<u8> {
    std::fs::read(env.registry.watch_root().join("machine-bindings.json")).unwrap()
}

#[test]
fn converted_private_identity_is_visible_and_only_reviewed_destination_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let env = legacy_test_env(root.path());
    let id = "converted-review";
    let destination = externally_convert_accepted_machine(&env, id, |d| {
        d.host = "new-private.int".into();
        d.port = 2223;
        d.jump_host = Some("new-jump.int".into());
    });
    let before = binding_bytes(&env);
    let summary = summary_of(&env, id).unwrap();
    let dto = serde_json::to_value(&summary).unwrap();
    let change = &dto["declaration"]["identityChange"];
    assert_eq!(change["accepted"]["host"], "gpu.int");
    assert_eq!(change["declared"]["host"], destination.host);
    assert!(change["accepted"]["serverId"].is_null());
    assert!(matches!(
        summary.status,
        MachineStatus::ConnectionFailed { .. }
    ));
    assert!(!private_setup::record(&env, id).unwrap().unwrap().set_up);
    let reviewed = identity_review(&env, id);
    identity::resolve(&env, id, &reviewed, false).unwrap();
    assert_eq!(binding_bytes(&env), before);
    identity::resolve(&env, id, &reviewed, true).unwrap();
    identity::validate_saved(&env, id, None).unwrap();
    assert!(summary_of(&env, id)
        .unwrap()
        .declaration
        .unwrap()
        .identity_change
        .is_none());
    let accepted = env
        .registry
        .bindings()
        .unwrap()
        .remove(id)
        .unwrap()
        .accepted_identity
        .unwrap();
    assert_eq!(accepted.host, destination.host);
    assert!(accepted.server_id.is_none());
}

#[test]
fn unchanged_external_conversion_and_established_private_keep_private_setup_semantics() {
    let root = tempfile::tempdir().unwrap();
    let env = legacy_test_env(root.path());
    let id = "unchanged-conversion";
    let destination = externally_convert_accepted_machine(&env, id, |_| {});
    let before = binding_bytes(&env);
    identity::validate_saved(&env, id, None).unwrap();
    assert!(summary_of(&env, id)
        .unwrap()
        .declaration
        .unwrap()
        .identity_change
        .is_none());
    assert_eq!(binding_bytes(&env), before);
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        host: destination.host,
        port: destination.port,
        jump_host: destination.jump_host,
        ..draft(MachineSignIn::Agent)
    };
    save(
        &env,
        &FakeStore::default(),
        Some(id),
        &input,
        &team(),
        false,
    )
    .unwrap();
    // Private setup does not inherit the legacy daemon identity.
    let private = registry::private::PrivateRegistry::new(
        env.registry.clone(),
        env.private_authority.as_ref().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        private.binding(id).unwrap().unwrap().setup,
        registry::private::PrivateSetup::NeedsEnrollment
    ));
    assert_eq!(binding_bytes(&env), before);
    identity::validate_saved(&env, id, Some(&input)).unwrap();
    // A machine created directly as private has no legacy accepted pin to adopt.
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &input,
        &Scope::Personal,
        false,
    )
    .unwrap();
    identity::validate_saved(&env, &saved.machine_id, Some(&input)).unwrap();
    assert!(saved.declaration.unwrap().identity_change.is_none());
}

#[test]
fn agent_receiving_seams_refuse_confirmation_and_endpoint_edits_without_binding_writes() {
    use crate::mcp::machine_tools::{ResolveMachineIdentityParams, SaveMachineParams};
    for private in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let env = legacy_test_env(root.path());
        let id = "agent-receiving-seam";
        let destination = if private {
            externally_convert_accepted_machine(&env, id, |d| d.host = "changed.int".into())
        } else {
            let mut d = accepted_team_machine(&env, id);
            d.host = "changed.int".into();
            env.registry.write(&team(), &d).unwrap();
            d
        };
        let params: ResolveMachineIdentityParams = serde_json::from_value(serde_json::json!({
            "machineId": id, "revision": identity_review(&env, id), "accept": true,
        }))
        .unwrap();
        let before = binding_bytes(&env);
        let error =
            identity::resolve_for_agent(&env, &params.machine_id, &params.revision, params.accept)
                .unwrap_err();
        assert!(error.starts_with("denied:"), "{error}");
        assert!(error.contains("humans only"), "{error}");
        assert_eq!(binding_bytes(&env), before);
        identity::resolve_for_agent(&env, id, &params.revision, false).unwrap();
        assert_eq!(binding_bytes(&env), before);

        // The person can accept. A later agent save still cannot accept a new
        // endpoint through the native editor's deliberate-edit behavior.
        identity::resolve(&env, id, &params.revision, true).unwrap();
        let before = binding_bytes(&env);
        let params: SaveMachineParams = serde_json::from_value(serde_json::json!({
            "machineId": id,
            "input": {
                "mode": if private { "private-account" } else { "legacy" },
                "label": "gpu-box", "host": "agent-changed.int", "port": destination.port,
                "username": "quinn", "jumpHost": destination.jump_host,
                "signIn": { "method": "agent" },
            },
            "share": { "kind": "personal" }, "connect": false,
        }))
        .unwrap();
        let input = MachineDraftInput::from(params.input);
        let error = save_for_agent(
            &env,
            &FakeStore::default(),
            params.machine_id.as_deref(),
            &input,
            &Scope::from(params.share),
            params.connect,
        )
        .unwrap_err();
        assert!(error.contains(identity::REFUSAL), "{error}");
        assert_eq!(binding_bytes(&env), before);
    }
}

/// The native checklist runs unchanged against a local SSH executable. Its
/// probe waits on a FIFO so the stale agent save queues before native commit.
fn native_checklist_ssh(env: &mut Env, root: &Path, pause_probe: bool) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let triple = "x86_64-unknown-linux-musl";
    std::fs::create_dir_all(&env.artifacts).unwrap();
    std::fs::create_dir_all(&env.shell_init).unwrap();
    for name in [
        install::artifact_name(triple),
        install::bridge_artifact_name(triple),
        install::emulator_artifact_name(triple),
    ] {
        let path = env.artifacts.join(name);
        std::fs::write(&path, b"fixture artifact").unwrap();
        std::fs::write(
            install::digest_file(&path),
            format!("{:x}  fixture\n", Sha256::digest(b"fixture artifact")),
        )
        .unwrap();
    }
    std::fs::write(env.artifacts.join(install::EMULATOR_NOTICES), "MIT").unwrap();
    for name in install::SHELL_INIT_SCRIPTS {
        std::fs::write(env.shell_init.join(name), "fixture shell init").unwrap();
    }
    let bundle = install::Bundle::load(&env.artifacts, &env.shell_init, triple).unwrap();
    let probe = root.join("probe-output");
    std::fs::write(
        &probe,
        format!(
            "who=quinn\ndata=ok\nfree=999999\nuname=Linux x86_64\ninstalled={}\n",
            bundle.version
        ),
    )
    .unwrap();
    let hello = codesense_host_proto::Hello {
        proto_major: codesense_host_proto::PROTO_MAJOR,
        proto_minor: codesense_host_proto::PROTO_MINOR,
        agent_version: bundle.version,
        server_id: "human-accepted-server".into(),
        server_name: "native-save".into(),
        epoch: "fixture".into(),
        rev: 0,
        instance_id: "fixture".into(),
        capabilities: vec![],
        viewer_id: None,
        pid: 1,
        emulator: None,
        build: None,
        client_id: None,
    };
    let hello_path = root.join("hello");
    codesense_host_proto::write_frame(
        &mut std::fs::File::create(&hello_path).unwrap(),
        &hello.frame().unwrap(),
    )
    .unwrap();
    let pending = root.join("checklist-pending");
    let release = root.join("release-checklist");
    assert!(Command::new("mkfifo")
        .arg(&release)
        .status()
        .unwrap()
        .success());
    let wait = if pause_probe {
        format!(
            "read release < {}",
            install::shell_quote(&release.to_string_lossy())
        )
    } else {
        ":".into()
    };
    let program = root.join("checklist-ssh");
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
case "$*" in
  *ssh-keyscan*) printf '%s %s\n' fixture '{KEY}';;
  *-G*) exit 0;;
  *who=*) touch {pending}; {wait}; cat {probe};;
  *) cat {hello};;
esac
"#,
            pending = install::shell_quote(&pending.to_string_lossy()),
            wait = wait,
            probe = install::shell_quote(&probe.to_string_lossy()),
            hello = install::shell_quote(&hello_path.to_string_lossy())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.ssh_program = program;
    (pending, release)
}

#[test]
fn agent_save_queued_behind_native_checklist_cannot_revert_human_acceptance() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut env = env_for(root.path(), 41);
    let id = "pending-human-save";
    let original = accepted_team_machine(&env, id);
    let mut expected = original.clone();
    expected.host = "human-new.int".into();
    expected.jump_host = Some("fixture-jump".into());
    expected.pinned_server_id = Some("human-accepted-server".into());
    let mut expected_bindings = env.registry.bindings().unwrap();
    let binding = expected_bindings.get_mut(id).unwrap();
    binding.accepted_identity = Some(registry::AcceptedIdentity::from(&expected));
    binding.observed_server_id = expected.pinned_server_id.clone();
    let expected_binding_bytes = serde_json::to_vec_pretty(&expected_bindings).unwrap();
    let expected_declaration_bytes = serde_yaml::to_string(&expected).unwrap().into_bytes();
    let declaration_path = env
        .registry
        .dir(&team())
        .unwrap()
        .join(format!("{id}.yaml"));
    let (pending, release) = native_checklist_ssh(&mut env, root.path(), true);
    let env = Arc::new(env);
    let native_env = env.clone();
    let native_input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: expected.label.clone(),
        host: expected.host.clone(),
        port: expected.port,
        username: None,
        jump_host: expected.jump_host.clone(),
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: Some(HostKey::parse(KEY).unwrap().fingerprint()),
    };
    let native = std::thread::spawn(move || {
        save(
            &native_env,
            &FakeStore::default(),
            Some(id),
            &native_input,
            &team(),
            true,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pending.exists() {
        if native.is_finished() {
            panic!(
                "native save failed before its checklist: {:?}",
                native.join().unwrap().err()
            );
        }
        assert!(
            Instant::now() < deadline,
            "native checklist did not reach probe"
        );
        std::thread::yield_now();
    }
    // Native owns one Arc; this test owns one. A third proves the agent
    // reached the shared transaction while the native checklist is pending.
    let transaction = save_transaction(id);
    assert_eq!(Arc::strong_count(&transaction), 2);
    let agent_env = env.clone();
    let stale_input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: original.label,
        host: original.host,
        port: original.port,
        username: None,
        jump_host: original.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    let agent = std::thread::spawn(move || {
        save_for_agent(
            &agent_env,
            &FakeStore::default(),
            Some(id),
            &stale_input,
            &team(),
            false,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while Arc::strong_count(&transaction) < 3 {
        assert!(
            !agent.is_finished(),
            "agent did not wait for pending native save"
        );
        assert!(
            Instant::now() < deadline,
            "agent did not reach shared save transaction"
        );
        std::thread::yield_now();
    }
    std::fs::write(release, "continue\n").unwrap();
    native.join().unwrap().unwrap();
    let result = agent.join().unwrap();
    assert_eq!(
        std::fs::read(declaration_path).unwrap(),
        expected_declaration_bytes,
        "stale agent save changed the person's declaration"
    );
    assert_eq!(
        binding_bytes(&env),
        expected_binding_bytes,
        "stale agent save changed the person's binding"
    );
    assert!(result.unwrap_err().contains(identity::REFUSAL));
}

#[test]
fn rejected_agent_save_operation_propagates_refusal_without_writing_either_file() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 41);
    let id = "agent-endpoint-save";
    let original = accepted_team_machine(&env, id);
    let path = env
        .registry
        .dir(&team())
        .unwrap()
        .join(format!("{id}.yaml"));
    let before_declaration = std::fs::read(&path).unwrap();
    let before_binding = binding_bytes(&env);
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: original.label,
        host: "agent-new.int".into(),
        port: original.port,
        username: None,
        jump_host: original.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    assert!(save_for_agent(
        &env,
        &FakeStore::default(),
        Some(id),
        &input,
        &team(),
        false
    )
    .unwrap_err()
    .contains(identity::REFUSAL));
    assert_eq!(std::fs::read(path).unwrap(), before_declaration);
    assert_eq!(binding_bytes(&env), before_binding);
}

#[test]
fn private_agent_save_uses_the_shared_machine_transaction_without_reentry() {
    let root = tempfile::tempdir().unwrap();
    let env = std::sync::Arc::new(legacy_test_env(root.path()));
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        ..draft(MachineSignIn::Agent)
    };
    let saved = save(
        &env,
        &FakeStore::default(),
        None,
        &input,
        &Scope::Personal,
        false,
    )
    .unwrap();
    let id = saved.machine_id.clone();
    assert_waits_for_machine_transaction(&saved.machine_id, move || {
        save_for_agent(
            &env,
            &FakeStore::default(),
            Some(&id),
            &input,
            &Scope::Personal,
            false,
        )
        .unwrap();
    });
}

#[test]
fn agent_settings_save_preserves_the_accepted_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 41);
    let id = "agent-settings-save";
    let declaration = accepted_team_machine(&env, id);
    let before = binding_bytes(&env);
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: "Renamed by agent".into(),
        host: declaration.host,
        port: declaration.port,
        username: None,
        jump_host: declaration.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    let saved = save_for_agent(
        &env,
        &FakeStore::default(),
        Some(id),
        &input,
        &team(),
        false,
    )
    .unwrap();
    assert_eq!(saved.label, "Renamed by agent");
    assert_eq!(binding_bytes(&env), before);
}

fn swap_key_after_save_validation(env: &Env, declaration: &Declaration) -> String {
    let registry = env.registry.clone();
    let mut changed = declaration.clone();
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExMTExMTExMTExMTExMTExMTExMTExMTExMTExMTExM";
    changed.pinned_host_key = Some(key.into());
    AFTER_SAVE_IDENTITY_VALIDATION.with(|slot| {
        assert!(slot
            .borrow_mut()
            .replace(Box::new(move || registry.write(&team(), &changed).unwrap()))
            .is_none());
    });
    key.into()
}

fn assert_git_pin_change_remains_unaccepted(env: &Env, id: &str, changed_key: &str) {
    let declaration = env.registry.find(id).unwrap().unwrap().1;
    assert_eq!(declaration.pinned_host_key.as_deref(), Some(changed_key));
    let binding = env.registry.bindings().unwrap().remove(id).unwrap();
    assert_eq!(
        binding.accepted_identity.unwrap().host_key.as_deref(),
        Some(KEY)
    );
    let summary = summary_of(env, id).unwrap();
    assert!(matches!(
        summary.status,
        MachineStatus::ConnectionFailed { .. }
    ));
    let dto = serde_json::to_value(summary).unwrap();
    let change = &dto["declaration"]["identityChange"];
    assert_eq!(
        change["accepted"]["hostKeyFingerprint"],
        HostKey::parse(KEY).unwrap().fingerprint()
    );
    assert_eq!(
        change["declared"]["hostKeyFingerprint"],
        HostKey::parse(changed_key).unwrap().fingerprint()
    );
}

#[test]
fn agent_save_uses_the_validated_snapshot_when_git_changes_the_pin() {
    let root = tempfile::tempdir().unwrap();
    let env = env_for(root.path(), 41);
    let id = "agent-snapshot-save";
    let declaration = accepted_team_machine(&env, id);
    let before = binding_bytes(&env);
    let changed_key = swap_key_after_save_validation(&env, &declaration);
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label,
        host: declaration.host,
        port: declaration.port,
        username: None,
        jump_host: declaration.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    let saved = save_for_agent(
        &env,
        &FakeStore::default(),
        Some(id),
        &input,
        &team(),
        false,
    )
    .unwrap();
    assert_eq!(
        binding_bytes(&env),
        before,
        "git update was silently accepted after validation"
    );
    assert!(saved.declaration.unwrap().identity_change.is_some());
    assert_git_pin_change_remains_unaccepted(&env, id, &changed_key);
}

#[test]
fn first_agent_setup_accepts_only_the_validated_snapshot_when_git_changes_the_pin() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut env = env_for(root.path(), 41);
    let id = "first-agent-snapshot-setup";
    let mut declaration = declaration(id);
    declaration.jump_host = Some("fixture-jump".into());
    declaration.pinned_server_id = Some("human-accepted-server".into());
    env.registry.write(&team(), &declaration).unwrap();
    native_checklist_ssh(&mut env, root.path(), false);
    let changed_key = swap_key_after_save_validation(&env, &declaration);
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label,
        host: declaration.host,
        port: declaration.port,
        username: None,
        jump_host: declaration.jump_host,
        sign_in: MachineSignIn::Agent,
        trusted_host_key_fp: None,
    };
    let saved = save_for_agent(&env, &FakeStore::default(), Some(id), &input, &team(), true)
        .expect("initial setup must use validated A despite a later git pin update");
    assert!(saved.declaration.unwrap().identity_change.is_some());
    assert_git_pin_change_remains_unaccepted(&env, id, &changed_key);
}
