// SPDX-License-Identifier: MIT
async fn activation_adopt(
    environment: &Arc<Env>,
    store: &Arc<FakeStore>,
    id: &str,
    client: crate::terminal::session_server::ManagedRemoteClient,
) -> Arc<crate::commands::host_connection_manager::HostConnectionManager> {
    use crate::commands::host_connection_manager::compatible_activation_fixture;
    let open_env = environment.clone();
    let open_store = store.clone();
    let proof_env = environment.clone();
    let proof_store = store.clone();
    let validate_env = environment.clone();
    compatible_activation_fixture::adopt(
        manager_record(environment, id).unwrap().unwrap(),
        client,
        Arc::new(move |id| open_pinned_saved(&open_env, open_store.as_ref(), &id)),
        Arc::new(move |id| legacy_host_proof_saved(&proof_env, proof_store.as_ref(), &id)),
        Arc::new(move |id, generation| {
            revalidate_transport_generation(&validate_env, id, generation)
        }),
    )
    .await
}

async fn activation_replace(
    environment: &Arc<Env>,
    store: &Arc<FakeStore>,
    id: &str,
    manager: Arc<crate::commands::host_connection_manager::HostConnectionManager>,
    remote_root: &str,
) {
    use crate::commands::host_connection_manager::{
        compatible_activation_fixture as fixture, ReplacementResult,
    };
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_session_server::testing::{registration, sh};
    let before = manager.connected_backend(id).unwrap();
    let client = before.client();
    let old_namespace = client.namespace().clone();
    let session = PtySessionId::new(format!("activation-{}", uuid::Uuid::new_v4()));
    let mut intent = sh(&format!(
        "while IFS= read -r line; do printf '%s\\n' \"$line\" > '{remote_root}/responsive'; done"
    ));
    intent.shell = Some("/bin/bash".into());
    intent.cwd = "/home/dev".into();
    client
        .spawn(intent, registration(session.as_str(), None, false))
        .unwrap();
    client.claim_writable_typed(&session).unwrap();
    let cancelled = fixture::prepare(&manager, id).await;
    assert_eq!(cancelled.count, None);
    // Cancellation consumes no authority: leave this token unexecuted and prove
    // the same daemon and live session still accept input before a fresh prompt.
    manager.revalidate_backend(&before).unwrap();
    client
        .write(&session, b"CANCEL_STILL_RESPONSIVE\r")
        .unwrap();
    docker_as_dev(&format!("i=0; until grep -q CANCEL_STILL_RESPONSIVE '{remote_root}/responsive' 2>/dev/null; do i=$((i+1)); test $i -lt 100 || exit 1; sleep .05; done"));
    assert_eq!(client.namespace(), &old_namespace);
    docker_as_dev(&format!(
        r#"
set -eu
mkdir -p "$HOME/.claude" "$HOME/.codex"
(set -C; printf '%s\n' '{{"fixture":"activation-only"}}' > "$HOME/.claude/.credentials.json") 2>/dev/null || test -f "$HOME/.claude/.credentials.json"
(set -C; printf '%s\n' '{{"fixture":"activation-only"}}' > "$HOME/.codex/auth.json") 2>/dev/null || test -f "$HOME/.codex/auth.json"
setsid sleep 180 >/dev/null 2>&1 & echo $! > '{remote_root}/sentinel.pid'
p="$HOME/.codesense/remote-private/v1"
sha256sum "$p/owner.json" "$p/pty-server-id" "$HOME/.claude/.credentials.json" "$HOME/.codex/auth.json" > '{remote_root}/preserved.sha256'
find "$p/remote-workspaces" '{remote_root}/work' -type f -exec sha256sum {{}} \; >> '{remote_root}/preserved.sha256'
"#
    ));
    let prompt = fixture::prepare(&manager, id).await;
    let fresh = format!("activation-new-{}", uuid::Uuid::new_v4());
    let result = fixture::restart(
        &manager,
        id,
        prompt.server_token,
        |next| {
            let mut intent = sh("sleep 60");
            intent.shell = Some("/bin/bash".into());
            intent.cwd = "/home/dev".into();
            next.spawn(intent, registration(&fresh, None, false))
                .unwrap();
            assert!(next.has_session(&PtySessionId::new(&fresh)).unwrap());
        },
        |id, proof, token, epoch| {
            replace_pinned_saved(environment, store.as_ref(), &id, &proof, &token, &epoch)
                .map(|_| ())
        },
    )
    .await;
    assert_eq!(
        result,
        ReplacementResult::Replaced {
            count: None,
            restored: None
        }
    );
    let after = manager.connected_backend(id).unwrap();
    let next = after.client();
    assert_eq!(next.namespace().server_id, old_namespace.server_id);
    assert_ne!(next.namespace().epoch, old_namespace.epoch);
    assert!(next.supports_selected_context_batch());
    assert!(!next.has_session(&session).unwrap());
    docker_as_dev(&format!("set -eu; sha256sum -c '{remote_root}/preserved.sha256' >/dev/null; kill -0 \"$(cat '{remote_root}/sentinel.pid')\"; kill \"$(cat '{remote_root}/sentinel.pid')\""));
    // The completion boundary retained the genuine empty admission census even
    // though this new session was started after publication, before completion.
    assert!(next.has_session(&PtySessionId::new(&fresh)).unwrap());
    next.kill(&PtySessionId::new(&fresh)).unwrap();
    println!("COMPATIBLE_ACTIVATION_CORE_SSH_PASS cancelled_session_responsive=true stable_server_id=true new_epoch=true enrollment_workspace_receipts_logins_preserved=true unrelated_sentinel_alive=true installed_batch_capability=true");
}
