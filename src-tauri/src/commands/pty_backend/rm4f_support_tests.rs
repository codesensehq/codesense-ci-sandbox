// SPDX-License-Identifier: MIT
//! Backend consumer smoke: imports only the common fixture, never C3 scenarios.
use super::*;
use crate::commands::host_connection_manager::rm4f_support::OwnedManagerHarness;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires C2-owned A/B fresh slots"]
async fn rm4f_c2a_remote_backend_consumer() {
    let harness = OwnedManagerHarness::supplied().await.unwrap();
    harness.with_cleanup(async {
        for index in 0..2 { harness.connect(index).await.unwrap(); }
        let backends = harness.machines.ids.iter().map(|id| resolve_remote_backend(&harness.manager, id).unwrap()).collect::<Vec<_>>();
        for (index, backend) in backends.iter().enumerate() {
            assert!(backend.is_remote());
            assert!(!backend.allows_local_fail_open());
            assert_eq!(backend.machine_id, harness.machines.ids[index]);
            assert_eq!(backend.server_id, harness.machines.server_ids[index]);
            let target = backend.admission_target().expect("native Create/Attach admission");
            let token = backend.remote_token.as_ref().expect("real manager remote token");
            assert_eq!(target.member.machine_id, backend.machine_id);
            assert_eq!(target.namespace, token.client().namespace().into());
            let captured = harness.manager.capture_admitted_backend(target).unwrap();
            assert!(Arc::ptr_eq(&backend.client().unwrap(), &captured.client()));
            assert!(harness.manager.revalidate_backend(token).is_ok());
            assert!(Arc::ptr_eq(&backend.client().unwrap(), &token.client()));
        }
        harness.manager.disconnect(&harness.machines.ids[0]).await.unwrap();
        assert!(harness.manager.revalidate_backend(backends[0].remote_token.as_ref().unwrap()).is_err());
        assert!(harness.manager.capture_admitted_backend(backends[0].admission_target().unwrap()).is_err());
        assert!(harness.manager.capture_admitted_backend(backends[1].admission_target().unwrap()).is_ok());
        assert!(harness.manager.revalidate_backend(backends[1].remote_token.as_ref().unwrap()).is_ok());
        assert!(resolve_remote_backend(&harness.manager, &harness.machines.ids[0]).is_err());
        let other = backends[1].client().unwrap();
        crate::commands::await_blocking("B live control after A disconnect", move || {
            other.heartbeat(std::time::Duration::from_secs(5)).map_err(|error| error.to_string())
        }).await.expect("B must answer a real SSH control request after A disconnect");
        eprintln!("RM4F C2a: independent backend consumer resolved real A/B manager tokens; A disconnect invalidated only A; B answered real control request");
    }).await;
}
