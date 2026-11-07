// SPDX-License-Identifier: MIT
use super::*;
use crate::install::agent_account_store::AccountResolutionReason;
use crate::routines::test_support::{
    agent, alice, save_personal_agent, save_team_agent, write_org, ORG, TEAM,
};
use codesense_pty_core::agent_cli::AgentCli;

fn fixture() -> (tempfile::TempDir, Caller, AgentKey) {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1);
    let caller = Caller {
        home: home.path().into(),
        principal: alice(),
    };
    let key = save_team_agent(
        home.path(),
        &caller.principal,
        agent("unused-live", AgentCli::Codex),
    );
    (home, caller, key)
}
fn save(caller: &Caller, key: &AgentKey, name: &str) -> agents::events::AgentChanged {
    agents::events::recorder::record();
    let mut definition = service::read_current(&caller.home, &caller.principal, key).unwrap();
    let base_revision = definition.revision;
    definition.name = name.into();
    service::save(
        &caller.home,
        &caller.principal,
        service::SaveTarget::Update {
            key: key.clone(),
            base_revision,
        },
        definition,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    agents::events::recorder::last(&key.to_string())
}
fn output(name: &str, value: Value) {
    if std::env::var_os("UPDATE_AGENT_DEFINITION_FIXTURES").is_some() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../tests/fixtures/{name}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
    }
}

#[test]
fn service_save_fans_one_affected_read_into_native_detail_and_scoped_sdk_rows() {
    let (_home, caller, key) = fixture();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (before, _) = registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(key.to_string()),
            true,
            1,
        )
        .unwrap();
    let (page_before, _) = registry
        .open(
            &mut topics,
            &caller,
            "page",
            Scope::Page(Some((ORG.into(), TEAM.into()))),
            None,
            true,
            0,
        )
        .unwrap();
    registry.reads = 0;
    let change = save(&caller, &key, "Saved without any session");
    registry.note(&change.key, change.revision.is_none());
    registry.refresh().unwrap();
    let deltas = registry.project(&mut topics).unwrap();
    let after = topics.snapshot(TOPIC, &before.view_key).unwrap();
    let page_after = topics.snapshot(TOPIC, &page_before.view_key).unwrap();
    assert_eq!(registry.reads, 1);
    assert_eq!(registry.scans, 1);
    assert!(after
        .snapshot
        .iter()
        .any(|r| r["detail"]["definition"]["name"] == "Saved without any session"));
    assert!(page_after
        .snapshot
        .iter()
        .any(|r| r["row"]["name"] == "Saved without any session"));
    assert!(page_after.rev > page_before.rev);
    // The managed save's ordinary filesystem echo shares the loaded source.
    registry.note(&key.to_string(), false);
    registry.refresh().unwrap();
    assert!(registry.project(&mut topics).unwrap().is_empty());
    assert_eq!(registry.reads, 1);
    // The immutable history written by that save is not another definition authority.
    let team_dir = crate::context_repo::path_to_org_root(&caller.home, &caller.principal, &key.org)
        .unwrap()
        .join(&key.team);
    let revision = crate::shared::revisioned_defs::revision_path(
        &team_dir.join("config/agents").join(&key.id),
        change.revision.unwrap(),
    );
    assert!(revision.exists());
    assert!(key_at_path(&caller.principal, &team_dir, &key.org, &key.team, &revision).is_none());
    output(
        "agent-definitions-save",
        json!({"before":before,"after":after,"pageBefore":page_before,"pageAfter":page_after,"deltas":deltas}),
    );
}

#[test]
fn team_views_exclude_other_teams_private_definitions_and_account_data() {
    let (home, caller, key) = fixture();
    save_personal_agent(
        home.path(),
        &caller.principal,
        agent("private-live", AgentCli::Codex),
    );
    let root = crate::context_repo::path_to_org_root(home.path(), &caller.principal, ORG).unwrap();
    std::fs::write(
        root.join("teams.yml"),
        "teams:\n  - name: backend\n  - name: other\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("other/content")).unwrap();
    let other = service::save(
        home.path(),
        &caller.principal,
        service::SaveTarget::Create(service::Placement::Team {
            org: ORG.into(),
            team: "other".into(),
        }),
        agent("other-live", AgentCli::Codex),
        &crate::routines::Author::user("1"),
    )
    .unwrap()
    .key;
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (before, _) = registry
        .open(
            &mut topics,
            &caller,
            "page-a",
            Scope::Page(Some((ORG.into(), TEAM.into()))),
            Some(key.to_string()),
            true,
            0,
        )
        .unwrap();
    for row in &before.snapshot {
        if row["rowType"] == "compatibility" {
            continue;
        }
        assert_eq!(row["rowType"], "definition");
        assert!(row.get("account").is_none());
        assert!(row.get("detail").is_none());
        assert_eq!(row["row"].as_object().unwrap().len(), 8);
        assert!(row["row"].get("avatar").is_some());
        assert_ne!(row["row"]["scope"], "personal");
        assert_ne!(row["key"], other.to_string());
    }
    let change = save(&caller, &other, "Other team changed");
    registry.note(&change.key, false);
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    assert_eq!(
        topics.snapshot(TOPIC, &before.view_key).unwrap().rev,
        before.rev
    );
}

#[test]
fn archived_definition_keeps_running_session_name_and_runtime() {
    let (_home, caller, key) = fixture();
    save(&caller, &key, "Developer");
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 0)
        .unwrap();
    let before = registry.names()[&key.to_string()].clone();
    service::archive(
        &caller.home,
        &caller.principal,
        &key,
        &crate::routines::Author::user("alice"),
    )
    .unwrap();
    registry.note(&key.to_string(), false);
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    assert!(
        registry.catalogs[caller.principal.as_str()].sources[&key.to_string()]
            .row
            .archived
    );
    assert_eq!(registry.names()[&key.to_string()], before);
    assert_eq!(before, json!({"name":"Developer", "runtime":"codex"}));
}

#[test]
fn compatibility_notifications_preserve_role_status_callbacks_without_source_reads() {
    let (_home, caller, _) = fixture();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(
            &mut topics,
            &caller,
            "page",
            Scope::Page(None),
            None,
            true,
            0,
        )
        .unwrap();
    registry.reads = 0;
    registry.note_activity();
    let deltas = registry.project(&mut topics).unwrap();
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        deltas[0].upserts,
        vec![json!({"id":"compatibility","rowType":"compatibility","activityRevision":1})]
    );
    assert_eq!(registry.reads, 0);
    assert_eq!(registry.scans, 1);
}

#[test]
fn hidden_detail_is_lazy_and_real_hub_revision_gaps_recover() {
    let (_home, caller, key) = fixture();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (before, _) = registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(key.to_string()),
            true,
            1,
        )
        .unwrap();
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            None,
            false,
            2,
        )
        .unwrap();
    registry.reads = 0;
    let change = save(&caller, &key, "While hidden");
    registry.note(&change.key, false);
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    assert!(
        registry.catalogs[caller.principal.as_str()].sources[&key.to_string()]
            .detail
            .is_none()
    );
    let reads = registry.reads;
    let (revealed, _) = registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(key.to_string()),
            true,
            3,
        )
        .unwrap();
    assert_eq!(
        registry.reads, reads,
        "reveal resolves the source already read by the hub"
    );
    assert!(revealed
        .snapshot
        .iter()
        .any(|r| r["detail"]["definition"]["name"] == "While hidden"));
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            None,
            false,
            4,
        )
        .unwrap();
    let path = agents::roots::team_agents_dir(&caller.home, &caller.principal, &key.org, &key.team)
        .unwrap()
        .unwrap()
        .join(&key.id)
        .join("agent.yaml");
    for n in 0..=super::super::projection::RETAINED_DELTAS {
        let mut definition = service::read_current(&caller.home, &caller.principal, &key).unwrap();
        definition.name = format!("External change {n}");
        std::fs::write(&path, serde_yaml::to_string(&definition).unwrap()).unwrap();
        registry.note(&key.to_string(), false);
        registry.refresh().unwrap();
        registry.project(&mut topics).unwrap();
    }
    let recovered = topics
        .subscribe(TOPIC, &before.view_key, Some(before.rev))
        .unwrap();
    assert!(
        recovered.snapshot.is_some(),
        "a gap beyond retained real deltas returns a snapshot"
    );
    assert_eq!(registry.scans, 1);
}

#[tokio::test]
async fn active_account_cas_updates_default_eligibility_with_zero_yaml_reads() {
    use crate::routines::test_support::{launch_app, TempHome};
    use agent_account_store::{create_profile, set_auth_state, AgentAccountAuthState};
    let home = TempHome::new();
    let caller = Caller {
        home: home.codesense_home(),
        principal: alice(),
    };
    let key = save_team_agent(
        &caller.home,
        &caller.principal,
        agent("default-live", AgentCli::Codex),
    );
    let explicit = save_team_agent(
        &caller.home,
        &caller.principal,
        agent("bound-live", AgentCli::Codex),
    );
    let create = |label| {
        let p =
            create_profile(&caller.home, &caller.principal, AgentProvider::Codex, label).unwrap();
        set_auth_state(
            &caller.home,
            &caller.principal,
            &p.id,
            p.generation,
            AgentAccountAuthState::Ready,
            None,
        )
        .unwrap()
    };
    let first = create("First");
    let second = create("Second");
    agents::binding::set(&caller.home, &caller.principal, &explicit, &first.id).unwrap();
    agent_account_store::compare_and_set_active_account(
        &caller.home,
        &caller.principal,
        AgentProvider::Codex,
        0,
        Some(&first.id),
        Some(first.generation),
    )
    .unwrap();
    set_auth_state(
        &caller.home,
        &caller.principal,
        &first.id,
        first.generation,
        AgentAccountAuthState::AuthenticationRequired,
        None,
    )
    .unwrap();
    let app = launch_app(&caller.home);
    app.manage(super::super::LiveHub::default());
    let before = prepare(
        app.handle(),
        &caller,
        "native",
        Scope::Native,
        Some(key.to_string()),
        true,
        1,
    )
    .unwrap();
    let bound = before
        .snapshot
        .iter()
        .find(|r| r["id"] == format!("account:{explicit}"))
        .unwrap()
        .clone();
    // A broken Active account refuses, even for an Optional run: it never falls back to the
    // CLI's own login.
    let refused = before
        .snapshot
        .iter()
        .find(|r| r["id"] == format!("account:{key}"))
        .unwrap();
    assert_eq!(refused["account"]["spawn_eligible"], false);
    assert_eq!(refused["account"]["reason"], "auth-required");
    assert_eq!(refused["account"]["resolved_source"], "active");
    app.state::<super::super::LiveHub>()
        .model
        .lock()
        .unwrap()
        .agent_definitions
        .reads = 0;
    let changed = crate::commands::agent_accounts::set_active_agent_account_inner(
        app.handle().clone(),
        1,
        AgentProvider::Codex,
        1,
        Some(second.id.clone()),
        Some(second.generation),
    )
    .await
    .unwrap();
    let after = app
        .state::<super::super::LiveHub>()
        .snapshot(&before.view_key, TOPIC)
        .unwrap();
    let account = after
        .snapshot
        .iter()
        .find(|r| r["id"] == format!("account:{key}"))
        .unwrap();
    assert_eq!(account["account"]["source"], "runtime_default");
    assert_eq!(account["account"]["account_id"], second.id);
    assert_eq!(account["account"]["spawn_eligible"], true);
    assert_eq!(
        after
            .snapshot
            .iter()
            .find(|r| r["id"] == bound["id"])
            .unwrap(),
        &bound
    );
    let model = app.state::<super::super::LiveHub>();
    assert_eq!(model.model.lock().unwrap().agent_definitions.reads, 0);
    assert_eq!(model.model.lock().unwrap().agent_definitions.scans, 1);
    assert!(
        crate::commands::agent_accounts::set_active_agent_account_inner(
            app.handle().clone(),
            1,
            AgentProvider::Codex,
            0,
            Some(first.id.clone()),
            Some(first.generation)
        )
        .await
        .is_err()
    );
    assert_eq!(
        app.state::<super::super::LiveHub>()
            .snapshot(&before.view_key, TOPIC)
            .unwrap()
            .rev,
        after.rev
    );
    crate::commands::agent_accounts::set_active_agent_account_inner(
        app.handle().clone(),
        1,
        AgentProvider::Codex,
        changed.current.unwrap().revision,
        None,
        None,
    )
    .await
    .unwrap();
    output(
        "agent-definitions-account",
        json!({"before":before,"after":after,"key":key.to_string(),"explicit":explicit.to_string()}),
    );
}

#[test]
fn an_unpinned_row_refused_on_a_broken_active_is_eligible_once_the_account_heals() {
    use agent_account_store::{create_profile, set_auth_state, AgentAccountAuthState};
    let (_home, caller, _) = fixture();
    let profile = create_profile(
        &caller.home,
        &caller.principal,
        AgentProvider::Codex,
        "Work",
    )
    .unwrap();
    let ready = set_auth_state(
        &caller.home,
        &caller.principal,
        &profile.id,
        profile.generation,
        AgentAccountAuthState::Ready,
        None,
    )
    .unwrap();
    agent_account_store::compare_and_set_active_account(
        &caller.home,
        &caller.principal,
        AgentProvider::Codex,
        0,
        Some(&ready.id),
        Some(ready.generation),
    )
    .unwrap();
    // The login expires: Active is now broken.
    let expired = set_auth_state(
        &caller.home,
        &caller.principal,
        &ready.id,
        ready.generation,
        AgentAccountAuthState::AuthenticationRequired,
        None,
    )
    .unwrap();
    let unpinned = agents::builtin::key(AgentCli::Codex).to_string();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(unpinned.clone()),
            true,
            1,
        )
        .unwrap();
    let account = |registry: &Registry| {
        registry.catalogs[caller.principal.as_str()].sources[&unpinned]
            .row
            .account
            .clone()
    };
    let refused = account(&registry);
    assert!(!refused.spawn_eligible);
    assert_eq!(refused.bound_account_id, None);
    assert_eq!(refused.account_id, None);

    // Each way the account can mend reaches the row that names neither it nor a binding.
    let settle = |registry: &mut Registry, topics: &mut TopicRegistry| {
        registry.note_account();
        registry.refresh().unwrap();
        registry.project(topics).unwrap();
    };
    set_auth_state(
        &caller.home,
        &caller.principal,
        &expired.id,
        expired.generation,
        AgentAccountAuthState::Ready,
        None,
    )
    .unwrap();
    settle(&mut registry, &mut topics);
    let healed = account(&registry);
    assert!(
        healed.spawn_eligible,
        "a re-check or sign-in finds it ready"
    );
    assert_eq!(healed.account_id.as_deref(), Some(expired.id.as_str()));

    let current = agent_account_store::list_profiles(&caller.home, &caller.principal)
        .unwrap()
        .remove(0);
    let detached = agent_account_store::detach_profile(
        &caller.home,
        &caller.principal,
        &current.id,
        current.generation,
    )
    .unwrap();
    settle(&mut registry, &mut topics);
    let after_detach = account(&registry);
    assert!(!after_detach.spawn_eligible, "detached");
    assert_eq!(after_detach.reason, AccountResolutionReason::Detached);

    agent_account_store::reattach_profile(
        &caller.home,
        &caller.principal,
        &detached.id,
        detached.generation,
    )
    .unwrap();
    settle(&mut registry, &mut topics);
    let reattached = account(&registry);
    assert!(reattached.spawn_eligible, "reattached");
    assert_eq!(reattached.account_id.as_deref(), Some(current.id.as_str()));
}

#[test]
fn binding_write_refreshes_the_unattended_starters_inherited_account() {
    use agent_account_store::{create_profile, set_auth_state, AgentAccountAuthState};
    let (_home, caller, _) = fixture();
    let create = |label| {
        let profile =
            create_profile(&caller.home, &caller.principal, AgentProvider::Codex, label).unwrap();
        set_auth_state(
            &caller.home,
            &caller.principal,
            &profile.id,
            profile.generation,
            AgentAccountAuthState::Ready,
            None,
        )
        .unwrap()
    };
    let first = create("First");
    let second = create("Second");
    let attended = agents::builtin::key(AgentCli::Codex);
    let unattended = agents::builtin::unattended_key(AgentCli::Codex);
    agents::binding::set(&caller.home, &caller.principal, &attended, &first.id).unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(unattended.to_string()),
            true,
            1,
        )
        .unwrap();
    assert_eq!(
        registry.catalogs[caller.principal.as_str()].sources[&unattended.to_string()]
            .row
            .account
            .account_id
            .as_deref(),
        Some(first.id.as_str())
    );
    agents::events::recorder::record();
    agents::binding::set(&caller.home, &caller.principal, &attended, &second.id).unwrap();
    let change = agents::events::recorder::last(&attended.to_string());
    registry.note(&change.key, change.revision.is_none());
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    let source = &registry.catalogs[caller.principal.as_str()].sources[&unattended.to_string()];
    assert_eq!(
        source.row.account.account_id.as_deref(),
        Some(second.id.as_str())
    );
    assert_eq!(
        source
            .detail
            .as_ref()
            .unwrap()
            .resolved
            .account
            .account_id
            .as_deref(),
        Some(second.id.as_str())
    );
    assert_eq!(registry.scans, 1);
}

#[test]
fn file_edit_git_pull_and_private_edit_use_the_existing_shared_ingress() {
    use crate::content_engine::test_support::act_until;
    use crate::content_engine::watcher::{WatchSetup, Watcher};
    use crate::routines::test_support::launch_app;
    use std::{sync::Arc, time::Duration};
    let (home, caller, key) = fixture();
    let private = save_personal_agent(
        home.path(),
        &caller.principal,
        agent("private-edit", AgentCli::Codex),
    );
    let root = crate::context_repo::path_to_org_root(home.path(), &caller.principal, ORG).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Live test",
                "-c",
                "user.email=live-test@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&root, &["init", "-b", "main"]);
    git(
        &root,
        &[
            "add",
            "org.yml",
            "people.yml",
            "teams.yml",
            "backend/config/agents",
        ],
    );
    git(&root, &["commit", "-m", "Initial definitions"]);
    let remote = home.path().join("remote.git");
    git(
        home.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&root, &["push", "-u", "origin", "main"]);
    let peer = home.path().join("peer");
    git(
        home.path(),
        &["clone", remote.to_str().unwrap(), peer.to_str().unwrap()],
    );

    let app = launch_app(home.path());
    app.manage(super::super::LiveHub::default());
    let before = prepare(
        app.handle(),
        &caller,
        "native",
        Scope::Native,
        Some(key.to_string()),
        true,
        1,
    )
    .unwrap();
    let page = prepare(
        app.handle(),
        &caller,
        "frame",
        Scope::Page(Some((ORG.into(), TEAM.into()))),
        None,
        true,
        0,
    )
    .unwrap();
    let state = app.state::<crate::commands::watch::WatcherState>();
    let watcher =
        Arc::new(Watcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&state.dispatcher)).unwrap());
    *state.watcher.lock().unwrap() = Some(watcher);
    crate::commands::watch::register_worktree_move_watch(app.handle(), &state, &root, ORG);
    crate::commands::watch::register_team_context_watch(app.handle(), &state, &root, TEAM);
    crate::commands::scratch_watch::register_scratch_watch(app.handle(), &state, home.path(), 1);
    assert!(state
        .path_to_repo
        .lock()
        .unwrap()
        .values()
        .all(Vec::is_empty));
    let path = root.join(format!("{TEAM}/config/agents/{}/agent.yaml", key.id));
    let mut definition = service::read_current(home.path(), &caller.principal, &key).unwrap();
    let revision = definition.revision;
    definition.name = "External edit".into();
    let bytes = serde_yaml::to_string(&definition).unwrap();
    let pending = |key: &AgentKey| {
        app.state::<super::super::LiveHub>()
            .model
            .lock()
            .unwrap()
            .agent_definitions
            .pending
            .contains_key(&key.to_string())
    };
    act_until(
        Duration::from_millis(200),
        || std::fs::write(&path, &bytes).unwrap(),
        || pending(&key),
    );
    refresh(app.handle(), &caller).unwrap();
    let external = app
        .state::<super::super::LiveHub>()
        .snapshot(&before.view_key, TOPIC)
        .unwrap();
    assert!(external
        .snapshot
        .iter()
        .any(|r| r["detail"]["definition"]["name"] == "External edit"
            && r["detail"]["definition"]["revision"] == revision));
    output(
        "agent-definitions-file",
        json!({"before":before,"after":external}),
    );

    definition.name = "Pulled definition".into();
    let relative = format!("{TEAM}/config/agents/{}/agent.yaml", key.id);
    std::fs::write(
        peer.join(&relative),
        serde_yaml::to_string(&definition).unwrap(),
    )
    .unwrap();
    git(&peer, &["add", &relative]);
    git(&peer, &["commit", "-m", "Rename agent from peer"]);
    git(&peer, &["push", "origin", "main"]);
    // Return the test edit to its tracked baseline before an ordinary fast-forward pull.
    git(&root, &["restore", "--source=HEAD", "--", &relative]);
    git(&root, &["pull", "--ff-only", "origin", "main"]);
    act_until(Duration::from_millis(200), || {}, || pending(&key));
    refresh(app.handle(), &caller).unwrap();
    let pulled = app
        .state::<super::super::LiveHub>()
        .snapshot(&page.view_key, TOPIC)
        .unwrap();
    assert!(pulled
        .snapshot
        .iter()
        .any(|r| r["row"]["name"] == "Pulled definition"));
    assert_eq!(
        app.state::<super::super::LiveHub>()
            .model
            .lock()
            .unwrap()
            .agent_definitions
            .scans,
        1
    );

    let private_path =
        agents::roots::team_agents_dir(home.path(), &caller.principal, &private.org, &private.team)
            .unwrap()
            .unwrap()
            .join(&private.id)
            .join("agent.yaml");
    let mut private_def = service::read_current(home.path(), &caller.principal, &private).unwrap();
    private_def.name = "Private external edit".into();
    let bytes = serde_yaml::to_string(&private_def).unwrap();
    act_until(
        Duration::from_millis(200),
        || std::fs::write(&private_path, &bytes).unwrap(),
        || pending(&private),
    );
    let page_rev = pulled.rev;
    refresh(app.handle(), &caller).unwrap();
    assert_eq!(
        app.state::<super::super::LiveHub>()
            .snapshot(&page.view_key, TOPIC)
            .unwrap()
            .rev,
        page_rev
    );
    let native = app
        .state::<super::super::LiveHub>()
        .snapshot(&before.view_key, TOPIC)
        .unwrap();
    assert!(native
        .snapshot
        .iter()
        .any(|r| r["row"]["name"] == "Private external edit"));
}

#[test]
fn explicit_reads_follow_scratch_membership_and_newly_joined_roots() {
    use crate::context_repo::scratch_space;
    let (home, caller, _) = fixture();
    let private = save_personal_agent(
        home.path(),
        &caller.principal,
        agent("viewport", AgentCli::Codex),
    );
    scratch_space::set_active_team(home.path(), Some(TEAM));
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (before, _) = registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 1)
        .unwrap();
    assert!(before
        .snapshot
        .iter()
        .any(|r| r["key"] == private.to_string()));
    scratch_space::set_team_uses_scratch(home.path(), &caller.principal, ORG, TEAM, false).unwrap();
    let (without, _) = registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 2)
        .unwrap();
    assert!(!without
        .snapshot
        .iter()
        .any(|r| r["key"] == private.to_string()));
    scratch_space::set_team_uses_scratch(home.path(), &caller.principal, ORG, TEAM, true).unwrap();
    let root = crate::context_repo::path_to_org_root(home.path(), &caller.principal, ORG).unwrap();
    std::fs::write(
        root.join("teams.yml"),
        "teams:\n  - name: backend\n  - name: joined\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("joined/content")).unwrap();
    let joined = service::save(
        home.path(),
        &caller.principal,
        service::SaveTarget::Create(service::Placement::Team {
            org: ORG.into(),
            team: "joined".into(),
        }),
        agent("already-there", AgentCli::Codex),
        &crate::routines::Author::user("1"),
    )
    .unwrap()
    .key;
    let (after, _) = registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 3)
        .unwrap();
    assert!(after
        .snapshot
        .iter()
        .any(|r| r["key"] == private.to_string()));
    assert!(after
        .snapshot
        .iter()
        .any(|r| r["key"] == joined.to_string()));
    let scans = registry.scans;
    let change = save(&caller, &joined, "Keyed update");
    registry.note(&change.key, false);
    registry.refresh().unwrap();
    assert_eq!(
        registry.scans, scans,
        "definition pings never rescan the catalog"
    );
}

#[test]
fn existing_scratch_event_pushes_current_native_membership() {
    use crate::context_repo::scratch_space;
    use crate::routines::test_support::launch_app;
    use tauri::Emitter;
    let (home, caller, _) = fixture();
    let private = save_personal_agent(
        home.path(),
        &caller.principal,
        agent("scratch-event", AgentCli::Codex),
    );
    scratch_space::set_active_team(home.path(), Some(TEAM));
    let app = launch_app(home.path());
    app.manage(super::super::LiveHub::default());
    super::super::listen_sources(app.handle());
    let before = prepare(
        app.handle(),
        &caller,
        "native",
        Scope::Native,
        None,
        true,
        1,
    )
    .unwrap();
    assert!(before
        .snapshot
        .iter()
        .any(|r| r["key"] == private.to_string()));
    scratch_space::set_team_uses_scratch(home.path(), &caller.principal, ORG, TEAM, false).unwrap();
    app.emit(
        crate::commands::scratch_use::SCRATCH_USE_CHANGED_EVENT,
        crate::commands::scratch_use::ScratchUseChanged {
            org_slug: ORG.into(),
            team: TEAM.into(),
        },
    )
    .unwrap();
    refresh(app.handle(), &caller).unwrap();
    let after = app
        .state::<super::super::LiveHub>()
        .snapshot(&before.view_key, TOPIC)
        .unwrap();
    assert!(!after
        .snapshot
        .iter()
        .any(|r| r["key"] == private.to_string()));
}

#[test]
fn reselect_recomposes_startup_without_rereading_cached_definition() {
    let (home, caller, _) = fixture();
    let mut definition = agent("context-reselect", AgentCli::Codex);
    definition.startup = vec!["docs/new.md".into()];
    let key = save_team_agent(home.path(), &caller.principal, definition).to_string();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(key.clone()),
            true,
            1,
        )
        .unwrap();
    assert!(!registry.catalogs[caller.principal.as_str()].sources[&key]
        .detail
        .as_ref()
        .unwrap()
        .resolved
        .manifest
        .errors
        .is_empty());
    registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 2)
        .unwrap();
    let content =
        agents::roots::team_content_dir(home.path(), &caller.principal, ORG, TEAM).unwrap();
    std::fs::create_dir_all(content.join("docs")).unwrap();
    std::fs::write(content.join("docs/new.md"), "# New startup file\n").unwrap();
    registry.reads = 0;
    registry
        .open(
            &mut topics,
            &caller,
            "native",
            Scope::Native,
            Some(key.clone()),
            true,
            3,
        )
        .unwrap();
    let detail = registry.catalogs[caller.principal.as_str()].sources[&key]
        .detail
        .as_ref()
        .unwrap();
    assert!(
        detail.resolved.manifest.errors.is_empty(),
        "reselection resolves newly available startup files"
    );
    assert_eq!(registry.reads, 0);
}

#[tokio::test]
async fn lifecycle_rename_updates_bound_account_without_definition_reads() {
    use crate::routines::test_support::{launch_app, TempHome};
    let home = TempHome::new();
    let caller = Caller {
        home: home.codesense_home(),
        principal: alice(),
    };
    let key = save_team_agent(
        &caller.home,
        &caller.principal,
        agent("lifecycle-bound", AgentCli::Codex),
    )
    .to_string();
    let profile = agent_account_store::create_profile(
        &caller.home,
        &caller.principal,
        AgentProvider::Codex,
        "Before",
    )
    .unwrap();
    let profile = agent_account_store::set_auth_state(
        &caller.home,
        &caller.principal,
        &profile.id,
        profile.generation,
        agent_account_store::AgentAccountAuthState::Ready,
        None,
    )
    .unwrap();
    agents::binding::set(
        &caller.home,
        &caller.principal,
        &AgentKey::parse(&key).unwrap(),
        &profile.id,
    )
    .unwrap();
    let app = launch_app(&caller.home);
    app.manage(super::super::LiveHub::default());
    super::super::listen_sources(app.handle());
    let before = prepare(
        app.handle(),
        &caller,
        "native",
        Scope::Native,
        Some(key.clone()),
        true,
        1,
    )
    .unwrap();
    app.state::<super::super::LiveHub>()
        .model
        .lock()
        .unwrap()
        .agent_definitions
        .reads = 0;
    agent_account_store::rename_profile(
        &caller.home,
        &caller.principal,
        &profile.id,
        profile.generation,
        "Renamed",
    )
    .unwrap();
    for event in
        agent_account_store::pending_lifecycle_events(&caller.home, &caller.principal).unwrap()
    {
        use tauri::Emitter;
        app.emit(
            crate::commands::agent_accounts::LIFECYCLE_EVENT,
            crate::commands::agent_accounts::AgentAccountLifecycleChanged {
                event_id: event.id,
                account_id: event.account_id,
                provider: event.provider,
                generation: event.generation,
                state: event.state,
            },
        )
        .unwrap();
    }
    refresh(app.handle(), &caller).unwrap();
    let after = app
        .state::<super::super::LiveHub>()
        .snapshot(&before.view_key, TOPIC)
        .unwrap();
    assert_eq!(
        after
            .snapshot
            .iter()
            .find(|r| r["id"] == format!("account:{key}"))
            .unwrap()["account"]["label"],
        "Renamed"
    );
    assert_eq!(
        app.state::<super::super::LiveHub>()
            .model
            .lock()
            .unwrap()
            .agent_definitions
            .reads,
        0
    );
}

#[test]
fn recreated_agent_recovers_retained_archived_routine_facts() {
    let (home, caller, key) = fixture();
    let author = crate::routines::Author::user("1");
    let routine: crate::routines::RoutineDef = serde_json::from_value(json!({
        "id":"retained", "name":"Retained routine", "agent":key.id,
        "task":"Keep notes", "where":{"kind":"team_workspace"}, "policy":{"max_runtime":"30m"}
    }))
    .unwrap();
    crate::routines::service::save(
        home.path(),
        &caller.principal,
        crate::routines::service::SaveTarget::Create(crate::routines::Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        routine,
        &author,
    )
    .unwrap();
    crate::commands::agents::archive(&caller, &key.to_string(), &author, true).unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 1)
        .unwrap();
    let revision = service::read_current(home.path(), &caller.principal, &key)
        .unwrap()
        .revision;
    service::delete(
        home.path(),
        &caller.principal,
        &key,
        Some(revision),
        &crate::routines::agents::RoutineReferrers,
    )
    .unwrap();
    registry.note(&key.to_string(), false);
    registry.refresh().unwrap();
    assert!(!registry.catalogs[caller.principal.as_str()]
        .sources
        .contains_key(&key.to_string()));
    save_team_agent(
        home.path(),
        &caller.principal,
        agent(&key.id, AgentCli::Codex),
    );
    registry.note(&key.to_string(), false);
    registry.refresh().unwrap();
    assert_eq!(
        registry.catalogs[caller.principal.as_str()].sources[&key.to_string()]
            .row
            .archived_routines
            .len(),
        1
    );
}
