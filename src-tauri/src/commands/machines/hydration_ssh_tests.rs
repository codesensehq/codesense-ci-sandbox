// SPDX-License-Identifier: MIT
#[test]
#[ignore = "owned real SSH: SW-XS-8 hydrate_worktree and reverse_sync_worktree on the preparation engine"]
fn remote_explicit_ops_on_engine_over_real_ssh() {
    remote_hydration_fixture(false, false, false, false);
}

#[test]
#[ignore = "owned real SSH: SW-XS-8 explicit engine keeps its state across a terminal-server restart"]
fn remote_selected_context_over_real_ssh() {
    remote_hydration_fixture(true, false, false, false);
}

#[test]
#[ignore = "owned SSH: genuine pinned compatible restart through production manager command core"]
fn remote_compatible_daemon_activation_over_real_ssh() {
    remote_hydration_fixture(true, true, false, false);
}

#[test]
#[ignore = "owned real SSH: saved-Agent selected startup followed by fake native CLI"]
fn remote_saved_agent_startup_over_real_ssh() {
    remote_hydration_fixture(true, false, true, false);
}

fn remote_hydration_fixture(selected: bool, activation: bool, startup: bool, routine: bool) {
    remote_hydration_fixture_with(selected, activation, startup, routine, false);
}

/// `wake`: the SW-AGR-7 wake and timer proof instead of the SW-AGR-2b launch
/// proof (`routine` must be set too).
fn remote_hydration_fixture_with(
    selected: bool,
    activation: bool,
    startup: bool,
    routine: bool,
    wake: bool,
) {
    if startup {
        docker_as_root("apt-get update -qq && apt-get install -y -qq --no-install-recommends git python3 >/dev/null");
    }
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::mcp::remote_workspace::dispatch;
    use crate::mcp::remote_workspace_bundle::{build, BuildSource};
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::remote_workspace::{InspectRepository, PrepareWorkspace};
    use codesense_session_server::testing::RecordingSink;
    use sha2::{Digest, Sha256};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let params:crate::mcp::machine_tools::DraftOperationParams=serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"Workspace fixture","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
    let mut draft: MachineDraftInput = params.input.into();
    draft.trusted_host_key_fp = test_connection(&environment, &store, None, &draft)
        .unwrap()
        .steps[0]
        .fingerprint
        .clone();
    let saved = save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
    private_setup::enroll(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    install_server(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    save(
        &environment,
        &store,
        Some(&saved.machine_id),
        &draft,
        &Scope::Personal,
        true,
    )
    .unwrap_or_else(|error| {
        if startup {
            docker_as_root("tail -c 8192 /home/dev/.codesense/remote-private/v1/logs/pty-server.log 2>/dev/null || true");
        }
        panic!("private fixture connect failed: {error}");
    });
    let seed = root.path().join("source");
    let repo = git2::Repository::init(&seed).unwrap();
    let url = "https://github.com/fixture/workspace-objects.git";
    repo.remote("origin", url).unwrap();
    let repository_key = crate::context_repo::canonical_remote_key(url).unwrap();
    let mut random = 17u32;
    let data: Vec<u8> = (0..256 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("tracked.bin", repo.blob(&data).unwrap(), 0o100644)
        .unwrap();
    let tree_id = tree.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let _first = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    std::fs::write(seed.join("desktop-only.txt"), "uncommitted local data").unwrap();
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let remote_root = format!("/home/dev/hy1-{marker}");
    docker_as_dev(&format!("mkdir -m700 '{remote_root}'"));
    let environment = Arc::new(environment);
    let store = Arc::new(store);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport = open_pinned_saved(&environment, store.as_ref(), &saved.machine_id).unwrap();
        let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
        let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
        let namespace = managed.domain_client().namespace().clone();
        let manager = if activation {
            activation_adopt(&environment, &store, &saved.machine_id, managed).await
        } else {
            crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await
        };
        let target = manager.target_for_namespace(&namespace).unwrap();
        let backend = manager.capture_admitted_backend(&target).unwrap();
        let client = backend.client();
        let gate = || manager.revalidate_backend(&backend).map_err(|error| format!("{error:?}"));

        let key = uuid::Uuid::new_v4().to_string();
        let source = BuildSource { url: url.into(), repository_key: repository_key.clone(), base_ref: "main".into(), seed: Some(seed.clone()) };
        let alive_manager = manager.clone(); let alive_backend = backend.clone();
        let (path, bundle) = build(root.path(), &key, &key, source, move || alive_manager.revalidate_backend(&alive_backend).is_ok(), |commit| {
            gate()?;
            client.inspect_workspace_repository(InspectRepository { owner_id:41, repository_key:repository_key.clone(), clone_url:url.into(), repo_path:format!("{remote_root}/repo"), commit:commit.into() })
        }).unwrap();
        let request = PrepareWorkspace { request_id:key, owner_id:41, origin_key:format!("{:x}",Sha256::digest(marker.as_bytes())), repository_key, clone_url:url.into(), repo_path:format!("{remote_root}/repo"),worktree_path:format!("{remote_root}/work"),base_ref:"main".into(),branch:"hydrate-work".into(),bundle };
        let prepared = dispatch(&client, request.clone(), &path, gate).unwrap();
        assert_eq!(prepared.status,"prepared","{prepared:?}");
        let prepared_cwd = prepared.cwd.clone().unwrap_or_else(|| request.worktree_path.clone());
        use crate::content_engine::destination::{DestinationIdentity, RemoteDestinationIdentity};
        let durable_request = request.clone();
        let identity = RemoteDestinationIdentity { owner_id:41,machine_id:saved.machine_id.clone(),server_id:client.namespace().server_id.clone(),workspace_handle:prepared.handle.clone().unwrap() };
        let file_target = codesense_pty_core::remote_files::FileTarget { owner_id:41,origin_key:request.origin_key,request_id:request.request_id,handle:prepared.handle.unwrap() };
        let guard_manager=manager.clone();let guard_backend=backend.clone();
        let lose_ack=Arc::new(std::sync::atomic::AtomicBool::new(true));
        let held_receipt=Arc::new(std::sync::Mutex::new(None));
        let io=crate::mcp::remote_destination::from_admitted_for_test(identity.clone(),file_target.clone(),client.clone(),Arc::new(move||guard_manager.revalidate_backend(&guard_backend).map_err(|e|format!("{e:?}"))),lose_ack.clone(),held_receipt.clone());
        assert_eq!(io.identity(),&DestinationIdentity::Remote(identity.clone()));
        let ctx=crate::content_engine::ResolverContext {context_root: None,
codesense_home:environment.codesense_home.clone(),user_id:41,org_slug:"fixture/context".into(),team_name:Some("default".into())};
        let slug=crate::domain::SourceRepoSlug::new("fixture","workspace-objects").unwrap();
        let org_root=crate::context_repo::path_to_org_root(&ctx.codesense_home,&ctx.principal(),&ctx.org_slug).unwrap();
        let content=crate::context_repo::path_to_team_content_dir(&org_root,"default");
        std::fs::create_dir_all(content.join("docs")).unwrap();
        let source_path=content.join("docs/current.md");
        let context_git=git2::Repository::init(&org_root).unwrap();
        std::fs::write(&source_path,"old committed context\n").unwrap();
        let context_relative=source_path.strip_prefix(&org_root).unwrap();
        let mut context_index=context_git.index().unwrap();context_index.add_path(context_relative).unwrap();context_index.write().unwrap();
        let context_tree_id=context_index.write_tree().unwrap();let context_tree=context_git.find_tree(context_tree_id).unwrap();
        context_git.commit(Some("HEAD"),&sig,&sig,"old context baseline",&context_tree,&[]).unwrap();
        std::fs::write(&source_path,"saved local context, never committed\n").unwrap();
        let committed_blob=context_git.find_blob(context_tree.get_path(context_relative).unwrap().id()).unwrap();
        assert_eq!(committed_blob.content(),b"old committed context\n");
        assert!(context_git.status_file(context_relative).unwrap().is_wt_modified());
        let scope_path=crate::commands::layout_resolver::resolve_worktree_config_path(&ctx.codesense_home,Some(&ctx.principal()),&org_root,"default",&slug).unwrap();
        std::fs::create_dir_all(scope_path.parent().unwrap()).unwrap();
        let config=crate::commands::worktree::WorktreeConfig::default();
        std::fs::write(scope_path,serde_yaml::to_string(&config).unwrap()).unwrap();
        // SW-XS-8: a real org layout, so context resolves through the team.
        std::fs::write(org_root.join("org.yml"),format!("slug: {}\ndisplay_name: SSH fixture\ncreated_at: \"2026-01-01T00:00:00Z\"\n",ctx.org_slug)).unwrap();
        std::fs::write(org_root.join("teams.yml"),"teams:\n  - name: default\n").unwrap();
        std::fs::write(org_root.join("people.yml"),"people: []\n").unwrap();
        let cache=crate::install::frontmatter_cache::FrontmatterCache::new();
        let suppression=Arc::new(crate::content_engine::dispatcher::Dispatcher::new(50,100,60_000,500)).suppression_handle();
        if routine {
            // SW-AGR-12: the workspace's durable receipt, as its preparation
            // writes it. No viewer holds the workspace: it is closed in the UI.
            let durable_handle = crate::mcp::remote_workspace_target::write_receipt_for_test(
                &crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal()).join("remote-workspace-intents-v1"),
                crate::mcp::remote_workspace::PrepareRemoteWorkspaceParams {
                    request_id: durable_request.request_id.clone(),
                    machine_id: target.member.machine_id.clone(),
                    team_key: format!("{}:default", ctx.org_slug),
                    workspace_id: "objects".into(),
                    source_repo: slug.clone(),
                    base_ref: "main".into(),
                    branch: durable_request.branch.clone(),
                    repo_path: durable_request.repo_path.clone(),
                    worktree_path: durable_request.worktree_path.clone(),
                },
                durable_request.clone(),
                &client.namespace().server_id,
                Some("generic"),
            );
            assert_eq!(durable_handle, file_target.handle, "the receipt names the handle the machine gave");
            // And a stale receipt that sorts first: its worktree was never
            // (or no longer is) prepared on the machine.
            let mut gone = durable_request.clone();
            gone.request_id = uuid::Uuid::new_v4().to_string();
            gone.worktree_path = format!("{remote_root}/gone");
            crate::mcp::remote_workspace_target::write_receipt_for_test(
                &crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal()).join("remote-workspace-intents-v1"),
                crate::mcp::remote_workspace::PrepareRemoteWorkspaceParams {
                    request_id: gone.request_id.clone(),
                    machine_id: target.member.machine_id.clone(),
                    team_key: format!("{}:default", ctx.org_slug),
                    workspace_id: "aaa-gone".into(),
                    source_repo: slug.clone(),
                    base_ref: "main".into(),
                    branch: gone.branch.clone(),
                    repo_path: gone.repo_path.clone(),
                    worktree_path: gone.worktree_path.clone(),
                },
                gone,
                &client.namespace().server_id,
                Some("generic"),
            );
            assert_eq!(durable_request.worktree_path, prepared_cwd, "and the worktree it reported");
        }
        if routine && wake {
            *held_receipt.lock().unwrap() = None;
            lose_ack.store(false, std::sync::atomic::Ordering::SeqCst);
            let machine_id = target.member.machine_id.clone();
            let ssh_host = Arc::new(SshHost { machine: machine_id.clone(), client: std::sync::Mutex::new(Some(client.clone())) });
            let window = Arc::new(SshWindow::default());
            let service = Arc::new(crate::delivery::DeliveryService::new(ssh_host.clone(), window.clone(), crate::delivery::store::RecordDir::at(None), "this-mac".into()));
            let record_home = ctx.codesense_home.clone();
            service.subscribe(Arc::new(move |outcome| {
                let store = crate::routines::state::RoutineStateStore::open(&record_home).unwrap();
                match crate::routines::wake::env_for(&record_home) {
                    Some(env) => crate::routines::wake::record_routed(&store, outcome, env.as_ref()),
                    None => crate::routines::wake::record_routed(&store, outcome, &crate::routines::wake::NoNotify),
                }
            }));
            service.sync(&machine_id).unwrap();
            let (mut state, wake_machine) = remote_routine_wake_proof(&ctx, &slug, io.clone(), client.clone(), &target, &prepared_cwd, &file_target.handle, &content, ssh_host.clone(), service.clone()).await;
            // The desktop goes away (an app quit): its link to the machine ends.
            state.left_at = std::time::SystemTime::now();
            *ssh_host.client.lock().unwrap() = None;
            manager.disconnect(&saved.machine_id).await.unwrap();
            tokio::time::sleep(Duration::from_secs(40)).await;
            // Back: a new connection, as a relaunched app makes.
            let transport = open_pinned_saved(&environment, store.as_ref(), &saved.machine_id).unwrap();
            let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
            let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
            let back = crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await;
            let namespace = back.connected_backend(&saved.machine_id).unwrap().client().namespace().clone();
            let back_target = back.target_for_namespace(&namespace).unwrap();
            let back_client = back.capture_admitted_backend(&back_target).unwrap().client();
            *ssh_host.client.lock().unwrap() = Some(back_client.clone());
            remote_routine_wake_after_reconnect(back_client, &prepared_cwd, &machine_id, &state, &service, &window, &wake_machine).await;
            back.disconnect(&saved.machine_id).await.unwrap();
            return;
        }
        if routine {
            *held_receipt.lock().unwrap() = None;
            lose_ack.store(false, std::sync::atomic::Ordering::SeqCst);
            if std::env::var_os("CODESENSE_MACHINE_ARMED_PROOF").is_some() {
                // SW-AGR-8: armed on the machine, firing with this Mac away.
                let machine = saved.machine_id.clone();
                let leaving = saved.machine_id.clone();
                let (away, back) = (manager.clone(), (environment.clone(), store.clone(), saved.machine_id.clone()));
                machine_armed_proof(&ctx, &slug, io.clone(), client.clone(), &machine, &prepared_cwd, &file_target.handle, &content,
                    || async move { away.disconnect(&leaving).await.unwrap(); },
                    || async move {
                        let (environment, store, machine) = back;
                        let transport = open_pinned_saved(&environment, store.as_ref(), &machine).unwrap();
                        let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
                        let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
                        let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(&machine, managed).await;
                        let client = manager.connected_backend(&machine).unwrap().client();
                        std::mem::forget(manager);
                        client
                    }).await;
                return;
            }
            remote_routine_launch_proof(&ctx, &slug, io.clone(), client.clone(), &target, &prepared_cwd, &file_target.handle, &content).await;
            manager.disconnect(&saved.machine_id).await.unwrap();
            return;
        }
        if startup {
            remote_saved_agent_startup_proof(&ctx, &slug, io.as_ref(), &client, &target, &request.worktree_path, &content, &suppression, &held_receipt).await;
            manager.disconnect(&saved.machine_id).await.unwrap();
            assert!(io.validate_authority().is_err());
            return;
        }
        if selected {
            // SW-XS-8: the explicit engine across a terminal-server restart (or
            // a compatible activation): on the new connection the managed
            // record and the machine's hash cache still hold, so one team edit
            // is the only transfer.
            let _ = (&cache, &suppression);
            let principal = ctx.principal();
            let target = crate::agents::ManifestTarget { context_root: None,
org: ctx.org_slug.clone(), team: "default".into(), repo: Some(slug.clone()) };
            for name in ["batch-proof-a", "batch-proof-b", "batch-proof-c"] {
                std::fs::write(content.join(format!("docs/{name}.md")), format!("selected recovery {name}")).unwrap();
            }
            let want = crate::agents::prepare::step3::want_for_selection(&ctx.codesense_home, &principal, &target, &[], &["docs".into()]).unwrap();
            assert_eq!(want.files.len(), 4);
            let legacy = || Ok(Vec::new());
            let run = |io: &dyn crate::content_engine::destination::DestinationIo, operation: &str| {
                crate::agents::prepare::step3::hydrate(crate::agents::prepare::step3::Hydrate {
                    home: &ctx.codesense_home, principal: &principal,
                    destination: crate::agents::prepare::Destination::Remote { io },
                    want: &want, operation, workspace: request.worktree_path.clone(), agent: "ssh".into(),
                    prune: false, overwrite_modified: false, progress: &|_| {}, validate: &|| io.validate_authority(), cancel: None, legacy: &legacy,
                })
                .unwrap()
            };
            let first = run(io.as_ref(), "6f1e2c3a-0000-4000-8000-000000000011");
            assert_eq!(first.status, crate::agents::prepare::PrepareStatus::Ready, "{first:?}");
            assert_eq!(first.written.len(), 4);
            let identity = io.identity().clone();
            let (manager, io) = {
                let next_manager = if activation {
                    activation_replace(&environment, &store, &saved.machine_id, manager.clone(), &remote_root).await;
                    assert!(io.validate_authority().is_err());
                    manager.clone()
                } else {
                    manager.disconnect(&saved.machine_id).await.unwrap();
                    assert!(io.validate_authority().is_err());
                    // This ignored fixture only runs in the explicitly owned container.
                    // Keep the daemon's durable workspace and transfer receipts intact.
                    docker_as_dev("pkill -KILL -f '[c]odesense-session-server' || true");
                    let transport = open_pinned_saved(&environment, store.as_ref(), &saved.machine_id).unwrap();
                    let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
                    let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
                    crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await
                };
                let namespace = next_manager.connected_backend(&saved.machine_id).unwrap().client().namespace().clone();
                let target = next_manager.target_for_namespace(&namespace).unwrap();
                let next_backend = next_manager.capture_admitted_backend(&target).unwrap();
                let next_client = next_backend.client();
                assert_eq!(next_client.namespace().server_id, client.namespace().server_id, "daemon restart preserves stable server identity");
                let guard_manager = next_manager.clone(); let guard_backend = next_backend.clone();
                let DestinationIdentity::Remote(remote_identity) = identity.clone() else { unreachable!() };
                *held_receipt.lock().unwrap() = None;
                let next_io = crate::mcp::remote_destination::from_admitted_for_test(remote_identity, file_target.clone(), next_client.clone(), Arc::new(move || guard_manager.revalidate_backend(&guard_backend).map_err(|e| format!("{e:?}"))), lose_ack.clone(), held_receipt.clone());
                (next_manager, next_io)
            };
            // After the restart (or activation) one team edit is the only
            // transfer: the managed record and the machine's hash cache held.
            std::fs::write(content.join("docs/batch-proof-b.md"), "edited after the restart").unwrap();
            let recovered = run(io.as_ref(), "6f1e2c3a-0000-4000-8000-000000000012");
            assert_eq!(recovered.status, crate::agents::prepare::PrepareStatus::Ready, "{recovered:?}");
            assert_eq!(recovered.written, vec!["docs/batch-proof-b.md"]);
            let record = crate::agents::prepare::ManagedRecord::load(&ctx.codesense_home, &principal, &identity);
            assert!(record.pending.is_empty());
            assert_eq!(record.entries.len(), 4);
            docker_as_dev(&format!("test \"$(cat '{}/docs/current.md')\" = 'saved local context, never committed'", request.worktree_path));
            docker_as_dev(&format!("test -z \"$(git -C '{}' status --porcelain --untracked-files=all)\" && test \"$(git -C '{}' rev-parse HEAD)\" = '{}'", request.worktree_path, request.worktree_path, _first));
            println!("REMOTE_ENGINE_RESTART_PASS root={remote_root} activation={activation} recovered={}", serde_json::to_string(&recovered.written).unwrap());
            manager.disconnect(&saved.machine_id).await.unwrap();
            return;
        }
        if xs26_perf_requested() {
            remote_explicit_sync_perf(&ctx, &slug, io.as_ref(), &request.worktree_path, &content);
            manager.disconnect(&saved.machine_id).await.unwrap();
            return;
        }
        // SW-XS-8: hydrate_worktree and reverse_sync_worktree run on the
        // preparation engine over real SSH: one manifest and one batch to
        // hydrate, one manifest and one streamed read to publish.
        let _ = (&cache, &suppression);
        let principal = ctx.principal();
        let target = crate::agents::ManifestTarget { context_root: None,
org: ctx.org_slug.clone(), team: "default".into(), repo: Some(slug.clone()) };
        let want = crate::agents::prepare::step3::want_for_selection(&ctx.codesense_home, &principal, &target, &[], &["docs".into()]).unwrap();
        assert_eq!(want.files.iter().map(|f| f.dest.as_str()).collect::<Vec<_>>(), vec!["docs/current.md"]);
        let legacy = || Ok(Vec::new());
        let validate = || io.validate_authority();
        let hydrate = |operation: &str| crate::agents::prepare::step3::hydrate(crate::agents::prepare::step3::Hydrate {
            home: &ctx.codesense_home, principal: &principal,
            destination: crate::agents::prepare::Destination::Remote { io: io.as_ref() },
            want: &want, operation, workspace: request.worktree_path.clone(), agent: "ssh".into(),
            prune: false, overwrite_modified: false, progress: &|_| {}, validate: &validate, cancel: None, legacy: &legacy,
        }).unwrap();
        // One manifest and one batch; the copy is managed and hidden from git.
        let _ = (&lose_ack, &held_receipt);
        let first = hydrate("6f1e2c3a-0000-4000-8000-000000000001");
        assert_eq!(first.status, crate::agents::prepare::PrepareStatus::Ready, "{first:?}");
        assert_eq!(first.written, vec!["docs/current.md"]);
        // Nothing changed: nothing is sent again.
        let second = hydrate("6f1e2c3a-0000-4000-8000-000000000002");
        assert!(second.written.is_empty(), "{second:?}");
        docker_as_dev(&format!("test \"$(cat '{}/docs/current.md')\" = 'saved local context, never committed'", request.worktree_path));
        docker_as_dev(&format!("test -z \"$(git -C '{}' status --porcelain --untracked-files=all)\"", request.worktree_path));
        let identity_any = io.identity().clone();
        let record = crate::agents::prepare::ManagedRecord::load(&ctx.codesense_home, &principal, &identity_any);
        assert!(record.entries["docs/current.md"].origin.starts_with("step3:"));
        // reverse_sync_worktree: preview, then publish the machine's edit.
        docker_as_dev(&format!("printf 'remote agent update\\n' > '{}/docs/current.md'", request.worktree_path));
        let expect = std::collections::BTreeMap::new();
        let publish_request = |operation: &'static str| crate::agents::prepare::publish::PublishRequest {
            home: &ctx.codesense_home, principal: &principal,
            destination: crate::agents::prepare::Destination::Remote { io: io.as_ref() },
            target: &target, paths: &[], sets: &[], to: None, expect: &expect, include_deletes: false, legacy: None, operation,
            workspace: request.worktree_path.clone(), agent: "ssh".into(), progress: &|_| {}, validate: &validate, cancel: None,
        };
        let preview = crate::agents::prepare::publish::preview(&publish_request("preview")).unwrap();
        assert_eq!(preview.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), vec!["docs/current.md"]);
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), "saved local context, never committed\n", "a preview writes nothing");
        let published = crate::agents::prepare::publish::publish(&publish_request("6f1e2c3a-0000-4000-8000-000000000003")).unwrap();
        assert_eq!(published.status, crate::agents::prepare::publish::PublishStatus::Published, "{published:?}");
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), "remote agent update\n");
        // Both sides changed since the copy: a conflict, never written.
        std::fs::write(&source_path, "desktop conflict\n").unwrap();
        docker_as_dev(&format!("printf 'remote conflict\\n' > '{}/docs/current.md'", request.worktree_path));
        let conflict = crate::agents::prepare::publish::publish(&publish_request("6f1e2c3a-0000-4000-8000-000000000004")).unwrap();
        assert_eq!(conflict.conflicts.len(), 1, "{conflict:?}");
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), "desktop conflict\n");
        docker_as_dev(&format!("test \"$(cat '{}/docs/current.md')\" = 'remote conflict' && test ! -e '{}/desktop-only.txt'", request.worktree_path, request.worktree_path));
        manager.disconnect(&saved.machine_id).await.unwrap();
        assert!(io.validate_authority().is_err());
        println!("REMOTE_EXPLICIT_OPS_ON_ENGINE_PASS root={remote_root} idempotent=true git_status_clean=true preview=true publish=true conflict_preserved=true");
    });
}

#[allow(clippy::too_many_arguments)]
async fn remote_saved_agent_startup_proof(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: &dyn crate::content_engine::destination::DestinationIo,
    client: &crate::terminal::session_server::SessionServerClient,
    runtime_target: &crate::commands::machines::RuntimeMemberTarget,
    cwd: &str,
    content: &std::path::Path,
    suppression: &crate::content_engine::dispatcher::SuppressionHandle,
    held: &std::sync::Mutex<Option<uuid::Uuid>>,
) {
    use crate::agents::{remote_launch, service, Placement};
    use crate::terminal::session_server::NativeMcpSelection;
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    use codesense_pty_core::{
        domain::{PtySessionId, TerminalDomain},
        spawn::SpawnIntent,
    };
    use codesense_session_server::testing::{registration, wait_until};
    let org_root =
        crate::context_repo::path_to_org_root(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug)
            .unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!(
            "slug: {}\ndisplay_name: SSH fixture\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
            ctx.org_slug
        ),
    )
    .unwrap();
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {}\n", ctx.team_name.as_deref().unwrap()),
    )
    .unwrap();
    std::fs::write(org_root.join("people.yml"), "people: []\n").unwrap();
    std::fs::create_dir_all(content.join(".claude/hooks")).unwrap();
    std::fs::write(
        content.join(".claude/hooks/codesense-notify.sh"),
        "#!/bin/sh\n",
    )
    .unwrap();
    let mut definition = crate::agents::resolve::tests::draft(
        "ssh-startup",
        codesense_pty_core::agent_cli::AgentCli::Claude,
    );
    definition.startup = vec!["docs/current.md".into()];
    definition.mcp.servers.clear();
    definition.default_prompt = "--literal-prompt".into();
    let key = service::save(
        &ctx.codesense_home,
        &ctx.principal(),
        service::SaveTarget::Create(Placement::Team {
            org: ctx.org_slug.clone(),
            team: ctx.team_name.clone().unwrap(),
        }),
        definition,
        &crate::routines::model::Author::user(ctx.principal().as_str()),
    )
    .unwrap()
    .key;
    let captured = remote_launch::prepare(
        &ctx.codesense_home,
        &ctx.principal(),
        &key,
        ctx.org_slug.clone(),
        ctx.team_name.clone().unwrap(),
        "fixture-prepared".into(),
    )
    .unwrap();
    let session = PtySessionId::new(format!("saved-startup-{}", uuid::Uuid::new_v4()));
    let intent = uuid::Uuid::new_v4();
    let state = crate::commands::dashboard_launch::DashboardLaunchState::new(41);
    let receipt_root = ctx.codesense_home.join("proof-launch-intents");
    let mut record = crate::commands::pty_layout::RemoteShellRecord {
        workspace_story_id: None,
        agent: Some(captured.clone()),
        phase: serde_json::from_value(serde_json::json!("preparing")).unwrap(),
        handle: format!("remote-tab-{intent}"),
        owner_id: 41,
        request_id: intent,
        machine_id: runtime_target.member.machine_id.clone(),
        team_key: format!("{}:{}", ctx.org_slug, ctx.team_name.as_deref().unwrap()),
        workspace_id: "fixture".into(),
        workspace: crate::commands::pty_layout::workspace_key("fixture/context:default", "fixture"),
        cwd: cwd.into(),
        name: None,
        mcp_profile_revision: None,
        mcp_inventory: vec![],
        hooks: false,
        launch: NativeLaunch::Cli(AgentCli::Claude),
        permission_mode: Default::default(),
        target: RuntimeSessionTarget {
            runtime: runtime_target.clone(),
            session: session.clone(),
        },
        status: "pending",
        identity: Default::default(),
    };
    state
        .reserve_remote_shell_durable(&receipt_root, record.clone())
        .unwrap();
    // SW-XS-7: Step 2 is one manifest diff and at most one batch; the Apply
    // checks every pre-hash on the machine, so there is no separate verify.
    // SW-XS-8 (XS-14 handoff): a merge-marked workspace file reaches a remote
    // launch through the engine, managed and excluded like any context file.
    let merged = crate::agents::manifest::GeneratedEntry {
        id: "workspace-file:.mcp.json".into(),
        target: crate::agents::manifest::GeneratedTarget::WorkspaceFile,
        content: serde_json::json!({"mcpServers": {"proof": {"command": "true"}}}),
        hash: String::new(),
        required: vec![],
        path: Some(".mcp.json".into()),
        merge_keys: Default::default(),
    };
    let step_two = |manifest: &crate::agents::StartupManifest, launch: NativeLaunch| {
        remote_launch::prepare_workspace(
            ctx,
            slug,
            io,
            launch,
            Some((manifest, "ssh")),
            std::slice::from_ref(&merged),
            &|_| {},
        )
        .map(|_| ())
    };
    *held.lock().unwrap() = None;
    let _ = suppression;
    step_two(&captured.manifest, NativeLaunch::Cli(AgentCli::Claude)).unwrap();
    docker_as_dev(&format!(
        "grep -q '\"proof\"' '{cwd}/.mcp.json' && git -C '{cwd}' check-ignore -q .mcp.json"
    ));
    docker_as_dev(&format!(
        "printf 'intervening edit' > '{cwd}/docs/current.md'"
    ));
    // An edit made after preparation is kept (modified), never overwritten.
    step_two(&captured.manifest, NativeLaunch::Cli(AgentCli::Claude)).unwrap();
    docker_as_dev(&format!("test \"$(cat '{cwd}/docs/current.md')\" = 'intervening edit' && printf 'saved local context, never committed\\n' > '{cwd}/docs/current.md'"));
    docker_as_dev(&format!(
        "test ! -e '{cwd}/docs/unselected.md' && mkdir -p /home/dev/.local/bin"
    ));
    // No provider credentials or network inference. The fake CLI checks the
    // actual daemon-generated argv/settings/cwd after real SSH selected effects.
    let script = r#"#!/usr/bin/python3
import sys,os,json,subprocess
if '--help' in sys.argv:
 print('--mcp-config --strict-mcp-config --settings');sys.exit(0)
assert open('docs/current.md').read() == 'saved local context, never committed\n'
assert not os.path.exists('docs/unselected.md')
assert sys.argv[-2:] == ['--', '--literal-prompt']
settings=json.load(open(sys.argv[sys.argv.index('--settings')+1]))
assert 'permissions' in settings and 'hooks' in settings
# Like Claude, run every hook the event carries. The inbox waiter (the host's own
# helper) waits on this host for mail (none comes), so it is still waiting when cut off.
commands=[hook['command'] for group in settings['hooks']['Stop'] for hook in group['hooks']]
assert sum('--inbox-wait' in command for command in commands) == 1, commands
for command in commands:
  if '--inbox-wait' in command:
    # Run as Claude runs it: its shell's parent is this agent (its `$PPID`).
    try:
      subprocess.run(['sh','-c',command],env=dict(os.environ,CLAUDE_PROJECT_DIR=os.getcwd()),timeout=3)
      assert False, 'the waiter ended without mail'
    except subprocess.TimeoutExpired:
      pass
  else:
    subprocess.run(command,shell=True,check=True,env=dict(os.environ,CLAUDE_PROJECT_DIR=os.getcwd()))
assert os.environ['HOME'].startswith('/home/')
assert 'CODEX_HOME' not in os.environ
activity={k:os.environ[k] for k in ('CODESENSE_ACTIVITY_HOST','CODESENSE_ACTIVITY_SOCKET','CODESENSE_ACTIVITY_TOKEN')}
with open('native-proof.json','w') as output: json.dump({'cwd':os.getcwd(),'argv':sys.argv,'settings':settings,'activity':activity},output)
print('SAVED_AGENT_NATIVE_PASS',flush=True)
"#;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    docker_as_dev(&format!("printf '%s' '{encoded}' | base64 -d > /home/dev/.local/bin/claude && chmod 700 /home/dev/.local/bin/claude"));
    let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = callbacks.clone();
    let lease = client.private_lease().unwrap().clone();
    let serve = Arc::new(move |server: String, stream: tokio::io::DuplexStream| {
        assert_eq!(server, "codesense");
        let observed = observed.clone();
        let lease = lease.clone();
        tauri::async_runtime::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (read, mut write) = tokio::io::split(stream);
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                lease.validate().unwrap();
                let input: serde_json::Value = serde_json::from_str(&line).unwrap();
                let Some(id) = input.get("id") else {
                    continue;
                };
                let result = if input["method"] == "initialize" {
                    serde_json::json!({"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}})
                } else {
                    // An inbox waiter waits on the machine's host: no call reaches here.
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    serde_json::json!({"content":[]})
                };
                let response =
                    serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string() + "\n";
                if write.write_all(response.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    });
    client
        .prepare_native_remote(
            &session,
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            NativeMcpSelection {
                server_ids: vec![],
                remote_entries: vec![],
                agent_startup: Some(captured.startup.clone()),
            },
            NativeLaunch::Cli(AgentCli::Claude),
            serve,
        )
        .unwrap();
    remote_launch::drive_prepared(
        record.preparing(),
        async {
            step_two(&captured.manifest, NativeLaunch::Cli(AgentCli::Claude))?;
            client
                .preflight_native_remote(&session, cwd, NativeLaunch::Cli(AgentCli::Claude))
                .map_err(|e| e.to_string())
        },
        |()| async {
            assert!(state
                .begin_remote_spawn(&receipt_root, &mut record)
                .unwrap());
            client
                .spawn(
                    SpawnIntent {
                        remote_launch: Some(NativeLaunch::Cli(AgentCli::Claude)),
                        command: None,
                        argv: None,
                        shell: None,
                        cwd: cwd.into(),
                        cols: 80,
                        rows: 24,
                        shell_integration: false,
                        starship_preset: None,
                        api_port: None,
                        env_remove: vec![],
                        env: vec![],
                        agent_env: Vec::new(),
                        login_path: None,
                    },
                    registration(session.as_str(), None, false),
                )
                .unwrap();
            Ok(())
        },
    )
    .await
    .unwrap();
    state
        .refresh_remote_spawn_phase(&receipt_root, &mut record)
        .unwrap();
    let repeated = remote_launch::drive_prepared(
        record.preparing(),
        async { panic!("post-attempt Attach must not replay selected preparation") },
        |(): ()| async { panic!("post-attempt Attach must not respawn") },
    )
    .await;
    assert!(repeated.unwrap_err().contains("already attempted"));
    assert!(wait_until(|| client
        .read_terminal_text(
            &session,
            u32::MAX,
            codesense_pty_core::emulator::TextMode::Normal
        )
        .is_ok_and(|text| text
            .text
            .contains("SAVED_AGENT_NATIVE_PASS"))));
    // Waiting held no desktop stream.
    assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 0);
    docker_as_dev(&format!("test -f '{cwd}/native-proof.json'"));
    // The same waiter is unusable outside the agent's process lineage, even with its
    // token: the host refuses it at once and it never wakes the agent.
    docker_as_dev(&format!("python3 -c 'import json,os,subprocess; p=json.load(open(\"{cwd}/native-proof.json\")); assert os.access(p[\"activity\"][\"CODESENSE_ACTIVITY_HOST\"]+\"/activity-helper\",os.X_OK); command=[h[\"command\"] for g in p[\"settings\"][\"hooks\"][\"Stop\"] for h in g[\"hooks\"] if \"--inbox-wait\" in h[\"command\"]][0]; assert subprocess.run([\"sh\",\"-c\",command],env=dict(os.environ,**p[\"activity\"]),timeout=5).returncode == 0'"));
    assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 0);

    let mut definition = crate::agents::resolve::tests::draft(
        "ssh-codex",
        codesense_pty_core::agent_cli::AgentCli::Codex,
    );
    definition.startup = vec!["docs/current.md".into()];
    definition.mcp.servers.clear();
    definition.default_prompt = "--codex-prompt".into();
    definition.permissions = Some(crate::agents::model::Permissions {
        codex: Some(crate::agents::model::CodexPermissions {
            approval_policy: Some("on-request".into()),
            sandbox_mode: Some("workspace-write".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let key = service::save(
        &ctx.codesense_home,
        &ctx.principal(),
        service::SaveTarget::Create(Placement::Team {
            org: ctx.org_slug.clone(),
            team: ctx.team_name.clone().unwrap(),
        }),
        definition,
        &crate::routines::model::Author::user(ctx.principal().as_str()),
    )
    .unwrap()
    .key;
    let codex = remote_launch::prepare(
        &ctx.codesense_home,
        &ctx.principal(),
        &key,
        ctx.org_slug.clone(),
        ctx.team_name.clone().unwrap(),
        "fixture-prepared".into(),
    )
    .unwrap();
    let session = PtySessionId::new(format!("saved-codex-{}", uuid::Uuid::new_v4()));
    record.request_id = uuid::Uuid::new_v4();
    record.handle = format!("remote-tab-{}", record.request_id);
    record.agent = Some(codex.clone());
    record.launch = NativeLaunch::Cli(AgentCli::Codex);
    record.target.session = session.clone();
    record.phase = serde_json::from_value(serde_json::json!("preparing")).unwrap();
    state
        .reserve_remote_shell_durable(&receipt_root, record.clone())
        .unwrap();
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let provision = || {
        client.prepare_native_remote(
            &session,
            nonce.clone(),
            NativeMcpSelection {
                server_ids: vec![],
                remote_entries: vec![],
                agent_startup: Some(codex.startup.clone()),
            },
            NativeLaunch::Cli(AgentCli::Codex),
            Arc::new(|_, _| {}),
        )
    };
    provision().unwrap();
    assert!(
        client
            .preflight_native_remote(&session, cwd, NativeLaunch::Cli(AgentCli::Codex))
            .is_err(),
        "missing remote CLI is a pre-spawn refusal"
    );
    state
        .refresh_remote_spawn_phase(&receipt_root, &mut record)
        .unwrap();
    assert!(record.preparing());
    let codex_script = r#"#!/usr/bin/python3
import sys,os,json
if '--help' in sys.argv:
 print('--no-daemon --config');sys.exit(0)
if sys.argv[1:4] == ['--no-daemon','mcp','list']:
 print('[]');sys.exit(0)
assert open('docs/current.md').read() == 'saved local context, never committed\n'
assert sys.argv[-2:] == ['--','--codex-prompt']
assert any(arg.startswith('approval_policy=') for arg in sys.argv)
assert any(arg.startswith('sandbox_mode=') for arg in sys.argv)
assert any(arg.startswith('mcp_servers={') and 'codesense' in arg for arg in sys.argv)
assert 'CODEX_HOME' not in os.environ
json.dump({'cwd':os.getcwd(),'argv':sys.argv,'home':os.environ['HOME']},open('codex-proof.json','w'))
print('SAVED_CODEX_NATIVE_PASS',flush=True)
"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(codex_script);
    docker_as_dev(&format!("printf '%s' '{encoded}' | base64 -d > /home/dev/.local/bin/codex && chmod 700 /home/dev/.local/bin/codex"));
    provision().unwrap();
    let codex_request = record.request_id;
    remote_launch::drive_prepared(
        record.preparing(),
        async {
            let _ = codex_request;
            step_two(&codex.manifest, NativeLaunch::Cli(AgentCli::Codex))?;
            client
                .preflight_native_remote(&session, cwd, NativeLaunch::Cli(AgentCli::Codex))
                .map_err(|e| e.to_string())
        },
        |()| async {
            assert!(state
                .begin_remote_spawn(&receipt_root, &mut record)
                .unwrap());
            client
                .spawn(
                    SpawnIntent {
                        remote_launch: Some(NativeLaunch::Cli(AgentCli::Codex)),
                        command: None,
                        argv: None,
                        shell: None,
                        cwd: cwd.into(),
                        cols: 80,
                        rows: 24,
                        shell_integration: false,
                        starship_preset: None,
                        api_port: None,
                        env_remove: vec![],
                        env: vec![],
                        agent_env: Vec::new(),
                        login_path: None,
                    },
                    registration(session.as_str(), None, false),
                )
                .map_err(|e| e.message)?;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert!(
        wait_until(|| client
            .read_terminal_text(
                &session,
                u32::MAX,
                codesense_pty_core::emulator::TextMode::Normal
            )
            .is_ok_and(|text| text.text.contains("SAVED_CODEX_NATIVE_PASS"))),
        "Codex fake CLI output: {:?}",
        client
            .read_terminal_text(
                &session,
                100,
                codesense_pty_core::emulator::TextMode::Normal
            )
            .map(|text| text.text)
    );
    println!("REMOTE_SAVED_AGENT_STARTUP_PASS: actual SSH selected receipt recovery and native settings; fake provider CLI, no inference");
}
