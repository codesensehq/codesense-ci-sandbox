// SPDX-License-Identifier: MIT
#[test]
#[ignore = "owned real SSH: official local harness and native remote executor"]
fn remote_agent_executor_over_real_ssh() {
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
    .unwrap();
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
    let remote_root = format!("/home/dev/auth1-{marker}");
    docker_as_dev(&format!("mkdir -m700 '{remote_root}'"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
        let admitted = RemoteTransportOwner::from_pinned(transport)
            .into_pending()
            .admit(Duration::from_secs(5), CancellationToken::new())
            .await
            .unwrap();
        let managed = SessionServerClient::from_remote_admitted(
            admitted,
            Arc::new(RecordingSink::default()),
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
        let backend = manager.capture_admitted_backend(&target).unwrap();
        let client = backend.client();
        let gate = || {
            manager
                .revalidate_backend(&backend)
                .map_err(|error| format!("{error:?}"))
        };

        let key = uuid::Uuid::new_v4().to_string();
        let source = BuildSource {
            url: url.into(),
            repository_key: repository_key.clone(),
            base_ref: "main".into(),
            seed: Some(seed.clone()),
        };
        let alive_manager = manager.clone();
        let alive_backend = backend.clone();
        let (path, bundle) = build(
            root.path(),
            &key,
            &key,
            source,
            move || alive_manager.revalidate_backend(&alive_backend).is_ok(),
            |commit| {
                gate()?;
                client.inspect_workspace_repository(InspectRepository {
                    owner_id: 41,
                    repository_key: repository_key.clone(),
                    clone_url: url.into(),
                    repo_path: format!("{remote_root}/repo"),
                    commit: commit.into(),
                })
            },
        )
        .unwrap();
        let request = PrepareWorkspace {
            request_id: key,
            owner_id: 41,
            origin_key: format!("{:x}", Sha256::digest(marker.as_bytes())),
            repository_key: repository_key.clone(),
            clone_url: url.into(),
            repo_path: format!("{remote_root}/repo"),
            worktree_path: format!("{remote_root}/work"),
            base_ref: "main".into(),
            branch: "hydrate-work".into(),
            bundle,
        };
        let prepared = dispatch(&client, request.clone(), &path, gate).unwrap();
        assert_eq!(prepared.status, "prepared", "{prepared:?}");
        let owner = manager.clone();
        let captured = backend.clone();
        let valid: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::new(move || {
            owner
                .revalidate_backend(&captured)
                .map_err(|e| format!("{e:?}"))
        });
        let target = codesense_pty_core::remote_files::FileTarget {
            owner_id: 41,
            origin_key: request.origin_key.clone(),
            request_id: request.request_id.clone(),
            handle: prepared.handle.unwrap(),
        };
        let resolved = crate::mcp::remote_workspace_target::ResolvedWorkspace {
            context: crate::commands::route_to_context::ActiveContext { codesense_home: root.path().to_path_buf(), user_id: 41, org_slug: "fixture/context".into(), team_name: Some("default".into()) },
            client: client.clone(),
            cwd: request.worktree_path.clone(),
            target,
            source_repo: crate::domain::SourceRepoSlug::new("fixture","workspace-objects").unwrap(),
            team_key: "default".into(),
            workspace_id: "fixture".into(),
            machine_id: saved.machine_id.clone(),
            server_id: client.server_id().into(),
            prepared_commit: request.bundle.commit.clone(),
            prepared_branch: request.branch.clone(),
            prepared_request: request.clone(),
            base_ref: request.base_ref.clone(),
            base_commit: None,
            validate: valid.clone(),
        };
        let endpoint = crate::mcp::remote_executor::open(resolved, uuid::Uuid::new_v4(), valid)
            .await
            .unwrap();
        let claude=std::env::var("CODESENSE_REMOTE_AUTH_CLAUDE_PROOF").as_deref()==Ok("1");
        if claude {
            crate::mcp::remote_agent_task::prove_claude_harness(&endpoint, root.path()).await.unwrap();
        } else {
            crate::mcp::remote_agent_task::prove_harness(&endpoint, root.path()).await.unwrap();
        }
        let remote_read=|path:&str| {let output=Command::new("docker").args(["exec","--user","dev",&var("CODESENSE_REMOTE_HOST_CONTAINER"),"cat",path]).output().unwrap();assert!(output.status.success(),"remote fixture file missing: {path}");output.stdout};
        assert_eq!(remote_read(&format!("{}/agent-proof.txt",request.worktree_path)),b"owned-remote-task-proof");
        let synthetic=std::env::var("CODESENSE_REMOTE_AUTH_ACCOUNT_PROOF").as_deref()!=Ok("1");
        if claude {
            assert_eq!(remote_read(&format!("{}/claude-command-proof.txt",request.worktree_path)),b"owned claude command");
        }
        if synthetic && !claude {
            assert_eq!(remote_read(&format!("{}/native-proof.txt",request.worktree_path)),b"owned native patch\n");
            // This PID belongs to the native sandbox namespace, not Docker's /proc.
            let native_pid=String::from_utf8(remote_read(&format!("{}/executor-child.pid",request.worktree_path))).unwrap().trim().parse::<u32>().unwrap();
            assert!(native_pid > 0);
        }
        assert_eq!(endpoint.close().await,crate::mcp::remote_executor::CloseOutcome::Confirmed);
        // Native harness shutdown may already have retired its foreground command.
        // Inspect the exact unique worktree in the host namespace after final close,
        // rather than interpreting the sandbox's PID as a host PID.
        let check=Command::new("docker").args(["exec","--user","dev",&var("CODESENSE_REMOTE_HOST_CONTAINER"),"python3","-c",r#"import pathlib,sys,time
root=pathlib.Path(sys.argv[1])
def live():
 result=[]
 for proc in pathlib.Path('/proc').iterdir():
  if not proc.name.isdecimal(): continue
  try:
   if (proc/'cwd').resolve(strict=True)==root and (proc/'stat').read_text().split(') ')[1].split()[0] != 'Z': result.append(proc.name)
  except (FileNotFoundError,PermissionError,ProcessLookupError): pass
 return result
for _ in range(20):
 remaining=live()
 if not remaining: break
 time.sleep(.05)
assert not remaining, 'executor processes survived confirmed close: '+str(remaining)
"#,&request.worktree_path]).output().unwrap();
        assert!(check.status.success(),"{}",String::from_utf8_lossy(&check.stderr));
        manager.disconnect(&saved.machine_id).await.unwrap();
    });
}
