// SPDX-License-Identifier: MIT
#[test]
#[ignore = "owned real SSH: production local-object bundle and admitted workspace dispatch"]
fn remote_workspace_local_objects_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::mcp::remote_workspace::{dispatch, PrepareRemoteWorkspaceParams};
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
    let data: Vec<u8> = (0..3 * 1024 * 1024)
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
    let first = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    std::fs::write(seed.join("desktop-only.txt"), "uncommitted local data").unwrap();
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let remote_root = format!("/home/dev/w1-{marker}");
    docker_as_dev(&format!("mkdir -m700 '{remote_root}'"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
        let admitted = RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5), CancellationToken::new()).await.unwrap();
        let managed = SessionServerClient::from_remote_admitted(admitted, Arc::new(RecordingSink::default()), Arc::<Ledger>::default(), None).await.unwrap();
        let namespace = managed.domain_client().namespace().clone();
        let manager = crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id, managed).await;
        let target = manager.target_for_namespace(&namespace).unwrap();
        let backend = manager.capture_admitted_backend(&target).unwrap();
        let client = backend.client();
        let gate = || manager.revalidate_backend(&backend).map_err(|error| format!("{error:?}"));
        let mut transferred = Vec::new();
        for number in 0..3 {
            if number == 2 {
                let parent = repo.find_commit(first).unwrap();
                repo.commit(Some("refs/heads/main"), &sig, &sig, "next", &tree, &[&parent]).unwrap();
            }
            let key = uuid::Uuid::new_v4().to_string();
            let source = BuildSource { url: url.into(), repository_key: repository_key.clone(), base_ref: "main".into(), seed: Some(seed.clone()) };
            let manager_alive = manager.clone();
            let backend_alive = backend.clone();
            let (path, bundle) = build(root.path(), &key, &key, source, move || manager_alive.revalidate_backend(&backend_alive).is_ok(), |commit| {
                gate()?;
                client.inspect_workspace_repository(InspectRepository { owner_id: 41, repository_key: repository_key.clone(), clone_url: url.into(), repo_path: format!("{remote_root}/repo"), commit: commit.into() })
            }).unwrap();
            let selected = repo.head().unwrap().target().unwrap().to_string();
            assert_eq!(bundle.commit, selected);
            transferred.push(bundle.size);
            let params: PrepareRemoteWorkspaceParams = serde_json::from_value(serde_json::json!({"requestId":key,"machineId":saved.machine_id,"teamKey":"org:team","workspaceId":"work","sourceRepo":{"owner":"fixture","repo":"workspace-objects","provider":"github","host":"github.com"},"baseRef":"main","branch":format!("work-{number}"),"repoPath":format!("{remote_root}/repo"),"worktreePath":format!("{remote_root}/work-{number}")})).unwrap();
            let request = PrepareWorkspace { request_id: params.request_id, owner_id: 41, origin_key: format!("{:x}", Sha256::digest(marker.as_bytes())), repository_key: repository_key.clone(), clone_url: url.into(), repo_path: params.repo_path, worktree_path: params.worktree_path, base_ref: params.base_ref, branch: params.branch, bundle };
            let result = dispatch(&client, request.clone(), &path, gate).unwrap();
            assert_eq!(result.status, "prepared", "{result:?}");
            assert_eq!(result.resolved_commit.as_deref(), Some(selected.as_str()));
            assert!(!path.exists(), "desktop settled payload reclaimed");
            let repeated = dispatch(&client, request.clone(), &path, gate).unwrap();
            assert_eq!(repeated.status, "alreadyPrepared", "{repeated:?}");
            assert_eq!(repeated.handle, result.handle);
            let content_digest = format!("{:x}", Sha256::digest(&data));
            docker_as_dev(&format!("test \"$(sha256sum '{}/tracked.bin' | cut -d ' ' -f 1)\" = '{}'", request.worktree_path, content_digest));
            docker_as_dev(&format!("test -f '{}/tracked.bin' && test ! -e '{}/desktop-only.txt' && test ! -e '{}/.codesense' && test \"$(git -C '{}' rev-parse HEAD)\" = '{}'", request.worktree_path, request.worktree_path, request.worktree_path, request.worktree_path, selected));
        }
        assert!(transferred[0] > 3 * 1024 * 1024, "multi-MiB bundle traversed the production writer");
        assert_eq!(transferred[1], 0);
        assert!(transferred[2] < transferred[0] / 4, "{transferred:?}");
        assert_eq!(std::fs::read_to_string(seed.join("desktop-only.txt")).unwrap(), "uncommitted local data");
        manager.disconnect(&saved.machine_id).await.unwrap();
        println!("REMOTE_WORKSPACE_SHARED_PREPARE_PASS root={remote_root} transfer_bytes={transferred:?} local_dirty_preserved=true hydration_sideeffects=false");
    });
}

/// A story workspace created "on a machine" over a link with internet round
/// trips: its paths come from the account home the machine reports (the
/// missing `~/codesense/...` levels are made there), its first open copies the
/// local objects, a repeat sends no bundle, and a tab opened at the workspace's
/// cwd is inside the worktree.
#[test]
#[ignore = "needs an sshd test container"]
fn a_story_workspace_lives_at_the_machine_home_over_a_high_latency_link() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::mcp::remote_story_workspace::story_workspace_paths;
    use crate::mcp::remote_workspace::{dispatch_reporting, PrepareProgress};
    use crate::mcp::remote_workspace_bundle::{build, BuildSource};
    use crate::terminal::session_server::tests::remote_tab_runs;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::remote_workspace::{InspectRepository, PrepareWorkspace};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::RecordingSink;
    use sha2::{Digest, Sha256};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(44));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(44);
    environment.private_authority = Some(authority);
    let params: crate::mcp::machine_tools::DraftOperationParams = serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"Story workspace","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
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
    let url = "https://github.com/fixture/story-workspace.git";
    repo.remote("origin", url).unwrap();
    let repository_key = crate::context_repo::canonical_remote_key(url).unwrap();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("README.md", repo.blob(b"story workspace\n").unwrap(), 0o100644)
        .unwrap();
    let tree = repo.find_tree(tree.write().unwrap()).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    repo.commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let story = format!("SW-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let branch = format!("story/{}", story.to_ascii_lowercase());
    let source = crate::domain::SourceRepoSlug::new("fixture", "story-workspace").unwrap();

    // A fixture image built before it carried git (its rebuild needs the
    // registry) gets git the same way the Dockerfile installs it.
    docker_as_root(
        "command -v git >/dev/null || (apt-get update -qq && apt-get install -y -qq --no-install-recommends git >/dev/null)",
    );
    // A ~/codesense an umask-0002 shell made: group-writable, healed at prepare.
    docker_as_dev("mkdir -p \"$HOME/codesense\" && chmod 775 \"$HOME/codesense\"");
    let _latency = InjectedLatency::add();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let started = Instant::now();
        let transport = open_pinned_saved(&environment, &store, &saved.machine_id).unwrap();
        let admitted = RemoteTransportOwner::from_pinned(transport)
            .into_pending()
            .admit(Duration::from_secs(10), CancellationToken::new())
            .await
            .unwrap();
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
        let backend = manager.capture_admitted_backend(&target).unwrap();
        let client = backend.client();
        let gate = || {
            manager
                .revalidate_backend(&backend)
                .map_err(|error| format!("{error:?}"))
        };
        let home = client.account_home().unwrap();
        let (repo_path, worktree_path) =
            story_workspace_paths(home.to_str().unwrap(), &source, "acme:default", &story).unwrap();
        assert!(worktree_path.starts_with("/home/dev/codesense/worktrees/acme-default-"));
        assert!(worktree_path.ends_with(&format!("/{story}")));
        let team_dir = worktree_path.rsplit_once('/').unwrap().0.to_string();
        let key = uuid::Uuid::new_v4().to_string();
        let alive_manager = manager.clone();
        let alive_backend = backend.clone();
        let inspect_repo = repo_path.clone();
        let (path, bundle) = build(
            root.path(),
            &key,
            &key,
            BuildSource {
                url: url.into(),
                repository_key: repository_key.clone(),
                base_ref: "main".into(),
                seed: Some(seed.clone()),
            },
            move || alive_manager.revalidate_backend(&alive_backend).is_ok(),
            |commit| {
                gate()?;
                client.inspect_workspace_repository(InspectRepository {
                    owner_id: 44,
                    repository_key: repository_key.clone(),
                    clone_url: url.into(),
                    repo_path: inspect_repo.clone(),
                    commit: commit.into(),
                })
            },
        )
        .unwrap();
        let request = PrepareWorkspace {
            request_id: key,
            owner_id: 44,
            origin_key: format!("{:x}", Sha256::digest(story.as_bytes())),
            repository_key: repository_key.clone(),
            clone_url: url.into(),
            repo_path: repo_path.clone(),
            worktree_path: worktree_path.clone(),
            base_ref: "main".into(),
            branch: branch.clone(),
            bundle,
        };
        let seen = Mutex::new(Vec::new());
        let record = |progress: PrepareProgress| seen.lock().unwrap().push(progress);
        let prepared = dispatch_reporting(&client, request.clone(), &path, gate, &record).unwrap();
        let created = started.elapsed();
        assert_eq!(prepared.status, "prepared", "{prepared:?}");
        assert_eq!(prepared.cwd.as_deref(), Some(worktree_path.as_str()));
        let first: Vec<PrepareProgress> = std::mem::take(&mut *seen.lock().unwrap());
        assert!(
            matches!(first.iter().rev().find(|p| matches!(p, PrepareProgress::Copying { .. })),
                Some(PrepareProgress::Copying { sent, total }) if sent == total && *total > 0),
            "the bundle was copied with progress: {first:?}"
        );
        assert_eq!(first.last(), Some(&PrepareProgress::CreatingWorktree));
        assert!(created < Duration::from_secs(120), "first create took {created:?}");

        let reopened_at = Instant::now();
        let again = dispatch_reporting(&client, request.clone(), &path, gate, &record).unwrap();
        let reopened = reopened_at.elapsed();
        assert_eq!(again.status, "alreadyPrepared", "{again:?}");
        assert_eq!(again.handle, prepared.handle);
        assert!(
            seen.lock().unwrap().is_empty(),
            "a second open sends no bundle: {:?}",
            seen.lock().unwrap()
        );
        assert!(reopened < Duration::from_secs(5), "reopen took {reopened:?}");

        let output = remote_tab_runs(
            client.clone(),
            events,
            "story-workspace",
            SpawnIntent {
                remote_launch: Default::default(),
                command: None,
                argv: None,
                shell: None,
                cwd: prepared.cwd.clone().unwrap(),
                cols: 120,
                rows: 24,
                shell_integration: true,
                starship_preset: None,
                api_port: None,
                env_remove: vec![],
                env: vec![],
                agent_env: Vec::new(),
                login_path: None,
            },
            "printf 'WT_%s\\n' \"$(pwd)\"; git -c color.ui=never status --short --branch",
            &format!("## {branch}"),
        );
        assert!(output.contains(&format!("WT_{worktree_path}")), "{output}");
        docker_as_dev(&format!(
            "test \"$(stat -c %a '{team_dir}')\" = 700 && test -f '{worktree_path}/README.md'"
        ));
        docker_as_dev("test \"$(stat -c %a \"$HOME/codesense\")\" = 755");

        // An agent launched there: a missing CLI is known before anything
        // starts, and the agent's startup arguments reach the CLI.
        {
            use codesense_host_proto::reverse_mcp::AgentStartup;
            use crate::terminal::session_server::NativeMcpSelection;
            use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
            use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
            use codesense_session_server::testing::{registration, wait_until};
            let cwd = prepared.cwd.clone().unwrap();
            docker_as_dev("rm -f \"$HOME/.local/bin/claude\"");
            assert_eq!(
                client.native_runtime_installed(&cwd, NativeLaunch::Cli(AgentCli::Claude)).unwrap(),
                Some(false),
                "a machine without Claude says so"
            );
            let script = "#!/bin/sh\ncase \"$*\" in *--help*) echo '--mcp-config --strict-mcp-config --settings'; exit 0;; esac\nprintf '%s\\n' \"$@\" > \"$PWD/profile-argv\"\necho PROFILE_ARGS_PASS\nsleep 30\n";
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(script);
            docker_as_dev(&format!("mkdir -p \"$HOME/.local/bin\" && printf '%s' '{encoded}' | base64 -d > \"$HOME/.local/bin/claude\" && chmod 700 \"$HOME/.local/bin/claude\""));
            assert_eq!(
                client.native_runtime_installed(&cwd, NativeLaunch::Cli(AgentCli::Claude)).unwrap(),
                Some(true)
            );
            let session = PtySessionId::new(format!("profile-args-{}", uuid::Uuid::new_v4()));
            client
                .prepare_native_remote(
                    &session,
                    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()),
                    NativeMcpSelection {
                        server_ids: vec![],
                        remote_entries: vec![],
                        agent_startup: Some(AgentStartup {
                            args: vec!["--model".into(), "opus".into()],
                            settings: serde_json::json!({}),
                            prompt: None,
                            env: Default::default(),
                        }),
                    },
                    NativeLaunch::Cli(AgentCli::Claude),
                    Arc::new(|_, _| {}),
                )
                .unwrap();
            client
                .spawn(
                    SpawnIntent {
                        remote_launch: Some(NativeLaunch::Cli(AgentCli::Claude)),
                        command: None,
                        argv: None,
                        shell: None,
                        cwd: cwd.clone(),
                        cols: 120,
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
            assert!(wait_until(|| client
                .read_terminal_text(&session, u32::MAX, codesense_pty_core::emulator::TextMode::Normal)
                .is_ok_and(|text| text.text.contains("PROFILE_ARGS_PASS"))));
            for flag in ["--model", "--strict-mcp-config", "--settings"] {
                docker_as_dev(&format!("grep -qx -- '{flag}' '{cwd}/profile-argv'"));
            }
        }
        // Work committed on the machine comes home: the commit lands in the
        // local clone as the machine's ref and the story branch fast-forwards.
        let fetched = {
            use crate::mcp::remote_workspace_fetch::{fetch_into_clone, FetchSource, LocalBranch};
            let base = repo.head().unwrap().target().unwrap();
            repo.branch(&branch, &repo.find_commit(base).unwrap(), false)
                .unwrap();
            docker_as_dev(&format!(
                "cd '{worktree_path}' && printf 'made on the machine\\n' > agent.txt && git add agent.txt && git -c user.name=Fixture -c user.email=fixture@example.invalid commit -q -m 'agent work on the machine'"
            ));
            let made = Command::new("docker")
                .args(["exec", "--user", "dev", &var("CODESENSE_REMOTE_HOST_CONTAINER"), "git", "-C", &worktree_path, "rev-parse", "HEAD"])
                .output()
                .unwrap();
            assert!(made.status.success());
            let made = String::from_utf8(made.stdout).unwrap().trim().to_string();
            let valid = || {
                manager
                    .revalidate_backend(&backend)
                    .map_err(|error| format!("{error:?}"))
            };
            let fetched_at = Instant::now();
            let report = fetch_into_clone(FetchSource {
                host: client.as_ref(),
                target: codesense_pty_core::remote_files::FileTarget {
                    owner_id: 44,
                    origin_key: request.origin_key.clone(),
                    request_id: request.request_id.clone(),
                    handle: prepared.handle.clone().unwrap(),
                },
                machine_id: saved.machine_id.clone(),
                prepared_commit: request.bundle.commit.clone(),
                base_commit: None,
                prepared_branch: request.branch.clone(),
                clone: seed.clone(),
                valid: &valid,
            })
            .unwrap();
            let took = fetched_at.elapsed();
            assert_eq!(report.commit, made);
            assert_eq!(report.branch, branch);
            assert_eq!(report.commits.len(), 1, "{report:?}");
            assert_eq!(report.commits[0].subject, "agent work on the machine");
            assert_eq!(report.local_branch_state, LocalBranch::FastForwarded);
            let local = git2::Repository::open(&seed).unwrap();
            let oid = git2::Oid::from_str(&made).unwrap();
            assert_eq!(
                local.find_reference(&report.remote_ref).unwrap().target(),
                Some(oid)
            );
            assert_eq!(
                local
                    .find_reference(&format!("refs/heads/{branch}"))
                    .unwrap()
                    .target(),
                Some(oid)
            );
            let tree = local.find_commit(oid).unwrap().tree().unwrap();
            let blob = local
                .find_blob(tree.get_name("agent.txt").unwrap().id())
                .unwrap();
            assert_eq!(blob.content(), b"made on the machine\n");
            assert!(tree.get_name("profile-argv").is_none(), "uncommitted files stay on the machine");
            assert_eq!(local.head().unwrap().target(), Some(base), "the local checkout is untouched");
            took
        };
        manager.disconnect(&saved.machine_id).await.unwrap();
        println!(
            "REMOTE_STORY_WORKSPACE_PASS rtt_ms={} create={created:?} reopen={reopened:?} fetch_home={fetched:?}",
            2 * WAN_ONE_WAY_MS
        );
    });
}
