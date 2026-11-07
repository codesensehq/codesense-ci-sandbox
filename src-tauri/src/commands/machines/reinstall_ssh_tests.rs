// SPDX-License-Identifier: MIT
/// SW-AGR-19 PR-3: the confirmed reinstall over real SSH, against the sshd
/// container: a private-account machine's running server (with a live
/// session) is stopped by the pid it greeted with, this build is installed,
/// and a fresh connection admits a new server under the same pinned server
/// id. A pid that is not one of the account's servers stops nothing.
#[test]
#[ignore = "needs an sshd test container"]
fn agr19_reinstall_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use super::reinstall::{stop_and_install, ReinstallResult};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_session_server::testing::{registration, sh, RecordingSink};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let mut draft = input(key_file(), None);
    draft.mode = Some(MachineMode::PrivateAccount);
    draft.username = Some("dev".into());
    let first = test_connection(&environment, &store, None, &draft).unwrap();
    draft.trusted_host_key_fp = first.steps[0].fingerprint.clone();
    let saved_draft = save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
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
    let id = saved.machine_id.clone();
    runtime.block_on(async {
        let connect = || async {
            let transport = open_pinned_saved(&environment, &store, &id).unwrap();
            let admitted = RemoteTransportOwner::from_pinned(transport)
                .into_pending()
                .admit(Duration::from_secs(5), CancellationToken::new())
                .await
                .unwrap();
            SessionServerClient::from_remote_admitted(
                admitted,
                Arc::new(RecordingSink::default()),
                Arc::<Ledger>::default(),
                None,
            )
            .await
            .unwrap()
        };
        let before = connect().await;
        let old = before.domain_client();
        let (server_id, epoch, pid) = (
            old.namespace().server_id.clone(),
            old.namespace().epoch.clone(),
            old.server_pid(),
        );
        let session = PtySessionId::new(format!("agr19-{}", uuid::Uuid::new_v4().simple()));
        let mut intent = sh("sleep 120");
        intent.shell = Some("/bin/bash".into());
        intent.cwd = "/home/dev".into();
        old.spawn(intent, registration(session.as_str(), None, false))
            .unwrap();
        assert!(old.has_session(&session).unwrap());
        drop(old);
        drop(before);

        // A pid that is not one of the account's servers: refused by name,
        // nothing stopped, nothing installed.
        let foreign = stop_and_install(&environment, &store, &id, Some(1));
        assert!(
            matches!(&foreign, Err(ReinstallResult::Failed { old_stopped: false, repair }) if repair.contains("not one of this machine's terminal servers")),
            "{foreign:?}"
        );

        // The server it greeted with: stopped (its session with it), this
        // build installed.
        let sessions = stop_and_install(&environment, &store, &id, Some(pid)).unwrap();
        assert!(sessions >= 1, "the live session ended with the old server");
        docker_as_dev(&format!("! kill -0 {pid} 2>/dev/null"));

        let after = connect().await;
        let next = after.domain_client();
        assert_eq!(next.namespace().server_id, server_id, "the pinned server id survives");
        assert_ne!(next.namespace().epoch, epoch);
        assert_ne!(next.server_pid(), pid);
        assert!(!next.has_session(&session).unwrap());
        // This build's server: its stamp is this app's.
        assert_eq!(
            next.build_stamp().map(|stamp| stamp.version.clone()),
            Some(super::terminal_server_info::app_stamp().version)
        );
        // The machine's own record is unchanged.
        assert_eq!(
            summary_of(&environment, &id).unwrap().declaration.map(|d| d.host),
            saved.declaration.map(|d| d.host)
        );
    });
}
