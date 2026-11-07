// SPDX-License-Identifier: MIT
use super::*;
use crate::commands::local_profile::{LocalProfileState, MachineOwner};
pub(super) async fn fixture(home: &Path) -> (LocalProfileState, PrivateRegistry, Registry) {
    let manager = LocalProfileState::for_test(home, 41, MachineOwner::LegacyGithub { id: 41 });
    let legacy = Registry::new(home.to_path_buf(), Principal::from(41_u64));
    let private = PrivateRegistry::new(legacy.clone(), &manager.snapshot().unwrap()).unwrap();
    (manager, private, legacy)
}

fn legacy_declaration(id: &str) -> Declaration {
    Declaration {
        machine_id: id.into(),
        label: "shared host".into(),
        host: "host.example".into(),
        port: 22,
        jump_host: None,
        default_base_dir: None,
        pinned_host_key: Some("ssh-ed25519 test".into()),
        pinned_server_id: None,
    }
}

pub(super) fn declaration(id: &str) -> PrivateDeclaration {
    PrivateDeclaration::from_legacy(legacy_declaration(id)).unwrap()
}

#[tokio::test]
async fn private_storage_is_invisible_to_old_reader_and_writer() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    assert!(legacy.find("shared-host").unwrap().is_none());
    assert!(legacy.bindings().unwrap().is_empty());
    let path = private
        .dir(&Scope::Personal)
        .unwrap()
        .join("shared-host.yaml");
    let original = std::fs::read(&path).unwrap();
    let mut old = legacy_declaration("shared-host");
    old.label = "late old save".into();
    legacy.write(&Scope::Personal, &old).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn private_only_teams_are_discovered_and_cross_scope_duplicates_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, _legacy) = fixture(temp.path()).await;
    let team = Scope::Team {
        org_slug: "owner/repo".into(),
        team: "default".into(),
    };
    std::fs::create_dir_all(
        private
            .dir(&team)
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
    )
    .unwrap();
    private.publish(&team, &declaration("shared-host")).unwrap();
    assert_eq!(private.find("shared-host").unwrap().unwrap().0, team);
    let other = private.dir(&Scope::Personal).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("shared-host.yaml"),
        serde_yaml::to_string(&declaration("shared-host")).unwrap(),
    )
    .unwrap();
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn malformed_and_removed_private_records_never_reactivate_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let path = private
        .dir(&Scope::Personal)
        .unwrap()
        .join("shared-host.yaml");
    std::fs::write(&path, b"malformed: [").unwrap();
    assert!(private.find("shared-host").is_err());
    std::fs::remove_file(&path).unwrap();
    legacy
        .write(&Scope::Personal, &legacy_declaration("shared-host"))
        .unwrap();
    assert!(private.find("shared-host").is_err());
    let reopened = PrivateRegistry::new(legacy, &_manager.snapshot().unwrap()).unwrap();
    assert!(reopened.find("shared-host").is_err());
}

#[tokio::test]
async fn private_registry_rejects_mismatched_principal_and_retired_lease() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, private, _legacy) = fixture(temp.path()).await;
    let snapshot = manager.snapshot().unwrap();
    assert!(PrivateRegistry::new(
        Registry::new(temp.path().into(), Principal::local_guest()),
        &snapshot
    )
    .is_err());
    manager.revoke();
    assert!(private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .is_err());
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn migration_preserves_old_writer_after_revision_observation() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    let original = legacy_declaration("shared-host");
    legacy.write(&Scope::Personal, &original).unwrap();
    let old_path = legacy
        .dir(&Scope::Personal)
        .unwrap()
        .join("shared-host.yaml");
    let original_bytes = std::fs::read(&old_path).unwrap();
    let migration = private
        .prepare_migration(&Scope::Personal, "shared-host")
        .unwrap();
    let mut late_save = original;
    late_save.label = "changed after observation".into();
    legacy.write(&Scope::Personal, &late_save).unwrap();
    let changed_bytes = std::fs::read(&old_path).unwrap();
    assert!(private.publish_migration(migration).is_err());
    assert_eq!(std::fs::read(&old_path).unwrap(), changed_bytes);
    assert!(private
        .dir(&Scope::Personal)
        .unwrap()
        .join("shared-host.yaml")
        .is_file());
    let backup_dir = legacy.user_file("machine-private-migrations-v1");
    let backup_path = std::fs::read_dir(backup_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup: MigrationBackup =
        serde_json::from_slice(&std::fs::read(backup_path).unwrap()).unwrap();
    assert_eq!(backup.declaration, original_bytes);
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn interrupted_private_publication_retains_durable_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let (_, private, _) = fixture(temp.path()).await;
    let bytes = serde_yaml::to_string(&declaration("shared-host")).unwrap();
    let mut state = private.state().unwrap();
    state.migrated_mode_refusals.insert(
        "shared-host".into(),
        Refusal {
            transaction_id: uuid::Uuid::new_v4().to_string(),
            declaration_revision: digest(bytes.as_bytes()),
            published: false,
        },
    );
    private.save_state(&state).unwrap();
    assert!(private.find("shared-host").is_err());
    let path = private
        .dir(&Scope::Personal)
        .unwrap()
        .join("shared-host.yaml");
    private.write(&path, bytes.as_bytes(), true).unwrap();
    // Crash after the file lands but before the completion ledger: still refused.
    assert!(private.find("shared-host").is_err());
    std::fs::remove_file(path).unwrap();
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn private_binding_cas_preserves_refusal_and_separate_legacy_data() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let bytes = std::fs::read(
        private
            .dir(&Scope::Personal)
            .unwrap()
            .join("shared-host.yaml"),
    )
    .unwrap();
    let binding = PrivateBinding {
        username: Some("owner".into()),
        key_path: None,
        credential_ref: None,
        setup: PrivateSetup::Ready {
            account: PrivateAccount {
                uid: 1001,
                canonical_home: "/home/owner".into(),
                root_version: 1,
                enrollment_id: uuid::Uuid::new_v4().to_string(),
            },
            server_id: uuid::Uuid::new_v4().to_string(),
        },
        host_key: Some("ssh-ed25519 test".into()),
        declaration_revision: digest(&bytes),
        revision: 1,
        auto_connect: false,
        remote_mcp_control: false,
        hidden: false,
        restore_auto_connect: false,
        open_tabs: false,
    };
    private
        .save_binding("shared-host", binding.clone(), None)
        .unwrap();
    assert!(legacy.bindings().unwrap().is_empty());
    assert!(private
        .save_binding("shared-host", binding.clone(), None)
        .is_err());
    assert_eq!(private.binding("shared-host").unwrap().unwrap().revision, 1);
    private.remove_binding("shared-host", 1).unwrap();
    assert!(private.binding("shared-host").unwrap().is_none());
    assert!(private
        .state()
        .unwrap()
        .migrated_mode_refusals
        .contains_key("shared-host"));
    let mut invalid = binding;
    if let PrivateSetup::Ready { account, .. } = &mut invalid.setup {
        account.canonical_home = "/home/../other".into();
    }
    assert!(private.save_binding("shared-host", invalid, None).is_err());
}

#[tokio::test]
async fn interrupted_migration_backup_alone_reserves_machine_id() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    legacy
        .write(&Scope::Personal, &legacy_declaration("shared-host"))
        .unwrap();
    let migration = private
        .prepare_migration(&Scope::Personal, "shared-host")
        .unwrap();
    let path = legacy
        .user_file("machine-private-migrations-v1")
        .join(format!("shared-host.{}.json", migration.transaction_id));
    PreparedWrite::new(&path, &serde_json::to_vec(&migration.backup).unwrap(), true)
        .unwrap()
        .persist()
        .unwrap();
    legacy.delete(&Scope::Personal, "shared-host").unwrap();
    assert!(private.find("shared-host").is_err());
    assert!(private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .is_err());
}

#[tokio::test]
async fn private_declaration_cannot_import_legacy_pin_or_unknown_schema_fields() {
    let mut legacy = legacy_declaration("shared-host");
    legacy.pinned_server_id = Some("legacy-pin".into());
    assert!(PrivateDeclaration::from_legacy(legacy).is_err());
    let mut wire = serde_json::to_value(declaration("shared-host")).unwrap();
    wire["machine"]["pinned_server_id"] = "shared-authority".into();
    assert!(serde_json::from_value::<PrivateDeclaration>(wire).is_err());
}

#[tokio::test]
async fn private_setup_draft_has_no_daemon_authority_or_automatic_connection() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, _legacy) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let bytes = std::fs::read(
        private
            .dir(&Scope::Personal)
            .unwrap()
            .join("shared-host.yaml"),
    )
    .unwrap();
    let mut draft = PrivateBinding {
        username: Some("owner".into()),
        key_path: None,
        credential_ref: None,
        setup: PrivateSetup::NeedsEnrollment,
        host_key: Some("ssh-ed25519 test".into()),
        declaration_revision: digest(&bytes),
        revision: 1,
        auto_connect: true,
        remote_mcp_control: false,
        hidden: false,
        restore_auto_connect: false,
        open_tabs: false,
    };
    assert!(private
        .save_binding("shared-host", draft.clone(), None)
        .is_err());
    draft.auto_connect = false;
    private.save_binding("shared-host", draft, None).unwrap();
    assert!(matches!(
        private.binding("shared-host").unwrap().unwrap().setup,
        PrivateSetup::NeedsEnrollment
    ));
}

pub(super) fn draft(revision: String) -> PrivateBinding {
    PrivateBinding {
        username: Some("owner".into()),
        key_path: None,
        credential_ref: None,
        setup: PrivateSetup::NeedsEnrollment,
        host_key: Some("ssh-ed25519 test".into()),
        declaration_revision: revision,
        revision: 1,
        auto_connect: false,
        remote_mcp_control: false,
        hidden: false,
        restore_auto_connect: false,
        open_tabs: false,
    }
}

#[tokio::test]
async fn private_binding_replacement_never_reuses_deleted_generation() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, private, legacy) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let (_, _, revision) = private.capture("shared-host").unwrap().unwrap();
    let first = draft(revision);
    private
        .save_binding("shared-host", first.clone(), None)
        .unwrap();
    private.remove_binding("shared-host", 1).unwrap();
    assert!(private
        .save_binding("shared-host", first.clone(), None)
        .is_err());
    let private = PrivateRegistry::new(legacy.clone(), &manager.snapshot().unwrap()).unwrap();
    let mut replacement = first;
    replacement.revision = private
        .binding_generation("shared-host")
        .unwrap()
        .checked_add(1)
        .unwrap();
    private
        .save_binding("shared-host", replacement.clone(), None)
        .unwrap();
    assert!(private.remove_binding("shared-host", 1).is_err());
    replacement.revision = 3;
    assert!(private
        .save_binding("shared-host", replacement, Some(1))
        .is_err());
    let reopened = PrivateRegistry::new(legacy, &manager.snapshot().unwrap()).unwrap();
    assert_eq!(
        reopened.binding("shared-host").unwrap().unwrap().revision,
        2
    );
}

#[tokio::test]
async fn private_binding_checks_current_snapshot_pin_and_digest() {
    let temp = tempfile::tempdir().unwrap();
    let (_, private, _) = fixture(temp.path()).await;
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let (_, _, revision) = private.capture("shared-host").unwrap().unwrap();
    let valid = draft(revision);
    let mut bad_pin = valid.clone();
    bad_pin.host_key = Some("another key".into());
    assert!(private.save_binding("shared-host", bad_pin, None).is_err());
    let mut bad_hash = valid.clone();
    bad_hash.declaration_revision = "0".repeat(64);
    assert!(private.save_binding("shared-host", bad_hash, None).is_err());
    private
        .save_binding("shared-host", valid.clone(), None)
        .unwrap();
    let mut changed = declaration("shared-host");
    changed.machine.host = "replacement.example".into();
    std::fs::write(
        private
            .dir(&Scope::Personal)
            .unwrap()
            .join("shared-host.yaml"),
        serde_yaml::to_string(&changed).unwrap(),
    )
    .unwrap();
    assert!(private.binding("shared-host").is_err());
    let mut stale = valid;
    stale.revision = 2;
    assert!(private.save_binding("shared-host", stale, Some(1)).is_err());
}

#[tokio::test]
async fn private_lock_survives_a_fork_inheriting_its_close_on_exec_descriptor() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    const CHILD: &str = "CODESENSE_PRIVATE_LOCK_FORK_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Exec first so the deliberately delayed fork cannot inherit locks or
        // pipes owned by unrelated tests running in this process.
        let thread = std::thread::current();
        let name = thread.name().expect("libtest names its test thread");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}\n{stderr}");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    let held = private.lock().unwrap();
    let held_fd = held.as_raw_fd();
    // SAFETY: held owns this descriptor, and F_GETFD takes no third argument.
    let flags = unsafe { libc::fcntl(held_fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);

    let (wait, mut release) = UnixStream::pair().unwrap();
    let wait_fd = wait.as_raw_fd();
    let release_fd = release.as_raw_fd();
    // SAFETY: the child only closes, reads and exits through async-signal-safe
    // syscalls. It models the interval between an SSH fork and its exec.
    let child = unsafe { libc::fork() };
    if child == 0 {
        unsafe {
            libc::close(release_fd);
            let mut byte = 0_u8;
            let read = libc::read(wait_fd, (&mut byte as *mut u8).cast(), 1);
            libc::_exit(if read == 1 { 0 } else { 1 });
        }
    }
    assert!(child > 0, "fork: {}", std::io::Error::last_os_error());
    drop(wait);
    drop(held);

    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(legacy.user_file("machine-private-v1.lock"))
        .unwrap();
    let inherited_lock = matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock));
    drop(contender);
    let reaper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let released = release.write_all(b"x");
        let mut status = 0;
        loop {
            // SAFETY: child is the exact child this test forked; status is writable.
            let waited = unsafe { libc::waitpid(child, &mut status, 0) };
            if waited < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            assert_eq!(waited, child);
            break;
        }
        assert!(released.is_ok(), "release forked child: {released:?}");
        assert_eq!(status, 0, "forked child exited unsuccessfully");
    });
    let acquired = private.lock();
    // Reap even when acquisition failed, so the red regression leaves no child.
    reaper.join().unwrap();
    assert!(inherited_lock, "the fork must retain the parent's lock");
    assert!(acquired.is_ok(), "lock after child release: {acquired:?}");
}

#[tokio::test]
async fn private_incomplete_scopes_and_busy_lock_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let (_, private, legacy) = fixture(temp.path()).await;
    let lock = private.lock().unwrap();
    assert!(private.lock().is_err());
    drop(lock);
    private
        .publish(&Scope::Personal, &declaration("shared-host"))
        .unwrap();
    let orgs = path_to_user_home(&legacy.home, &legacy.principal).join("orgs");
    // Personal machines live in the profile's own directory, so `orgs/` may not
    // exist yet: create it so an unreadable team scope can be simulated.
    std::fs::create_dir_all(&orgs).unwrap();
    std::fs::rename(&orgs, orgs.with_file_name("orgs-preserved")).unwrap();
    std::fs::write(orgs, "not a directory").unwrap();
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn private_pending_migration_cannot_be_completed_by_foreign_declaration() {
    let temp = tempfile::tempdir().unwrap();
    let (_, private, legacy) = fixture(temp.path()).await;
    legacy
        .write(&Scope::Personal, &legacy_declaration("shared-host"))
        .unwrap();
    let migration = private
        .prepare_migration(&Scope::Personal, "shared-host")
        .unwrap();
    let path = legacy
        .user_file("machine-private-migrations-v1")
        .join(format!("shared-host.{}.json", migration.transaction_id));
    PreparedWrite::new(&path, &serde_json::to_vec(&migration.backup).unwrap(), true)
        .unwrap()
        .persist()
        .unwrap();
    assert!(private
        .prepare_migration(&Scope::Personal, "shared-host")
        .is_err());
    assert!(private.publish_migration(migration).is_err());
    legacy.delete(&Scope::Personal, "shared-host").unwrap();
    let target = private.dir(&Scope::Personal).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("shared-host.yaml"),
        serde_yaml::to_string(&declaration("shared-host")).unwrap(),
    )
    .unwrap();
    assert!(private.find("shared-host").is_err());
}

#[tokio::test]
async fn private_write_commit_revocation_preserves_captured_destination() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, private, _) = fixture(temp.path()).await;
    let destination = private.personal_path();
    let prepared = PreparedWrite::new(&destination, b"captured old owner", false).unwrap();
    let committed = private.lease.commit(|| prepared).unwrap();
    manager.revoke();
    committed.persist().unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"captured old owner");
    let late = PreparedWrite::new(&destination, b"unauthorized late effect", false).unwrap();
    assert!(private.lease.commit(|| late).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"captured old owner");
}

#[tokio::test]
async fn explicit_conversion_keeps_the_id_and_host_key_and_backs_up_the_legacy_record() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    let mut original = legacy_declaration("shared-host");
    original.pinned_server_id = Some("legacy-server".into());
    legacy.write(&Scope::Personal, &original).unwrap();
    let original_bytes = std::fs::read(
        legacy
            .dir(&Scope::Personal)
            .unwrap()
            .join("shared-host.yaml"),
    )
    .unwrap();

    private
        .convert_legacy(&Scope::Personal, "shared-host")
        .unwrap();

    let (scope, converted) = private.find("shared-host").unwrap().unwrap();
    assert_eq!(scope, Scope::Personal);
    assert_eq!(converted.machine.label, original.label);
    assert_eq!(converted.machine.pinned_host_key, original.pinned_host_key);
    assert!(legacy.find("shared-host").unwrap().is_none());
    let backups: Vec<_> = std::fs::read_dir(legacy.user_file("machine-private-migrations-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let backup: MigrationBackup =
        serde_json::from_slice(&std::fs::read(&backups[0]).unwrap()).unwrap();
    assert_eq!(backup.declaration, original_bytes);

    // An old writer recreating the legacy record meets the duplicate refusal,
    // and asking again does not paper over it.
    legacy.write(&Scope::Personal, &original).unwrap();
    assert!(private.find("shared-host").is_err());
    assert!(private
        .convert_legacy(&Scope::Personal, "shared-host")
        .is_err());
}

#[tokio::test]
async fn an_interrupted_conversion_finishes_when_asked_again() {
    let temp = tempfile::tempdir().unwrap();
    let (_manager, private, legacy) = fixture(temp.path()).await;
    let original = legacy_declaration("shared-host");
    legacy.write(&Scope::Personal, &original).unwrap();
    // Crash after the private declaration is published, before the legacy
    // record is removed: the id stays refused and the legacy file intact.
    let migration = private
        .prepare_migration(&Scope::Personal, "shared-host")
        .unwrap();
    let bytes = serde_yaml::to_string(&migration.declaration)
        .unwrap()
        .into_bytes();
    std::fs::create_dir_all(legacy.user_file("machine-private-migrations-v1")).unwrap();
    std::fs::write(
        legacy
            .user_file("machine-private-migrations-v1")
            .join(format!("shared-host.{}.json", migration.transaction_id)),
        serde_json::to_vec(&migration.backup).unwrap(),
    )
    .unwrap();
    let mut state = private.state().unwrap();
    state.migrated_mode_refusals.insert(
        "shared-host".into(),
        Refusal {
            transaction_id: migration.transaction_id.clone(),
            declaration_revision: digest(&bytes),
            published: false,
        },
    );
    private.save_state(&state).unwrap();
    let target = private.dir(&Scope::Personal).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("shared-host.yaml"), &bytes).unwrap();
    assert!(private.find("shared-host").is_err());
    assert!(legacy.find("shared-host").unwrap().is_some());

    private
        .convert_legacy(&Scope::Personal, "shared-host")
        .unwrap();

    assert_eq!(
        private
            .find("shared-host")
            .unwrap()
            .unwrap()
            .1
            .machine
            .pinned_host_key,
        original.pinned_host_key
    );
    assert!(legacy.find("shared-host").unwrap().is_none());
}
