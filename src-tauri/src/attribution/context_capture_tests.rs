// SPDX-License-Identifier: MIT
//! SV-CDE-85 PR2: a launch in a context worktree is captured like one in a story worktree,
//! and nothing about source-repo capture changes.
//!
//! Each case runs in its own temp CodeSense home with a fake git-ai that logs its calls.

use super::launch::{prepare, Plan, Request};
use super::managed;
use super::prefs::{self, AttributionMode};
use super::testing::Launch;
use super::{AttributionStatus, OffReason};
use crate::agents::resolve::tests::alice;
use codesense_pty_core::agent_cli::AgentCli;
use std::path::{Path, PathBuf};

fn git(repo: &Path, args: &[&str]) -> String {
    crate::attribution::git_ai::tests::git(repo, args)
}

fn launch_in(fixture: &Launch, cwd: &Path) -> Plan {
    prepare(
        &Request {
            codesense_home: fixture.home(),
            principal: &alice(),
            cwd,
            runtime: AgentCli::Claude,
        },
        None,
    )
}

fn calls(fixture: &Launch) -> Vec<String> {
    fixture
        .calls()
        .into_iter()
        .map(|call| call.split(" HOME=").next().unwrap().to_owned())
        .collect()
}

/// `daemon_repo_roots` as the last write left it; `None` when the key was unset.
fn roots(fixture: &Launch) -> Option<Vec<String>> {
    let calls = calls(fixture);
    let last = calls
        .iter()
        .rev()
        .find(|c| {
            c.starts_with("config set daemon_repo_roots ") || c == &"config unset daemon_repo_roots"
        })
        .unwrap_or_else(|| panic!("daemon_repo_roots never written: {calls:?}"));
    let value = last.strip_prefix("config set daemon_repo_roots ")?;
    Some(serde_json::from_str(value).expect("an array of strings"))
}

fn common_dir(checkout: &Path) -> String {
    std::fs::canonicalize(checkout.join(".git"))
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn sentinel(fixture: &Launch) -> String {
    std::fs::read_to_string(fixture.home().join("git-ai/config-applied")).unwrap()
}

/// The principal's context clone (an org root) and a registered linked worktree of it, with
/// the launch directory a folder inside the worktree, as a context launch has it.
fn context_worktree(fixture: &Launch) -> (PathBuf, PathBuf, PathBuf) {
    let clone = fixture.home().join("users/1/orgs/ctxorg");
    std::fs::create_dir_all(&clone).unwrap();
    std::fs::write(clone.join("org.yml"), "").unwrap();
    let repo = git2::Repository::init(&clone).unwrap();
    repo.remote("origin", "https://github.com/acme/ctx.git").unwrap();
    git(&clone, &["add", "org.yml"]);
    git(&clone, &["commit", "-m", "base"]);
    let root = fixture.home().join("users/1/context-worktrees/ctxorg/riverside/cde-63");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    git(&clone, &["worktree", "add", "-b", "riverside/cde-63", &root.to_string_lossy()]);
    let launch_cwd = root.join("riverside/content");
    std::fs::create_dir_all(&launch_cwd).unwrap();
    crate::install::worktree_index::write_context_record(
        fixture.home(),
        &alice(),
        &crate::install::worktree_index::ContextWorktreeRecord {
            owner: Some(crate::install::worktree_index::ContextWorktreeOwner::Person),
            schema_version: crate::install::worktree_index::SCHEMA_VERSION,
            kind: "context".into(),
            id: "cw_test".into(),
            org: "ctxorg".into(),
            team: "riverside".into(),
            name: "cde-63".into(),
            branch: "riverside/cde-63".into(),
            base_sha: String::new(),
            root: root.clone(),
            launch_cwd: launch_cwd.clone(),
            created_at: 1,
            last_used_at: 1,
            extras: Default::default(),
        },
    )
    .unwrap();
    (clone, root, launch_cwd)
}

/// Case 1: a launch in a context worktree is `Active`, the daemon lists the context clone
/// beside the attached clone, nothing restarts, and the launch is wired.
fn case_1_a_context_worktree_launch_is_captured() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let (clone, _root, cwd) = context_worktree(&fixture);
    let plan = launch_in(&fixture, &cwd);
    assert_eq!(plan.status, AttributionStatus::Active, "{:?}", calls(&fixture));
    assert!(plan.hook_command.is_some() && !plan.env.is_empty());
    let mut expected = vec![common_dir(&fixture.cwd), common_dir(&clone)];
    expected.sort();
    assert_eq!(roots(&fixture), Some(expected));
    assert!(!calls(&fixture).iter().any(|c| c == "bg restart"), "{:?}", calls(&fixture));
}

/// Case 2: the repo set off keeps a context worktree off, with nothing wired.
fn case_2_a_context_repo_set_off_is_not_captured() {
    let fixture = Launch::new(AgentCli::Claude);
    let (_clone, root, cwd) = context_worktree(&fixture);
    let (key, _) = prefs::repo_key_for_checkout(&root).unwrap();
    prefs::update(fixture.home(), &alice(), &key, |entry| entry.mode = Some(AttributionMode::Off))
        .unwrap();
    let plan = launch_in(&fixture, &cwd);
    assert_eq!(
        plan.status,
        AttributionStatus::Off { reason: OffReason::DisabledForRepo }
    );
    assert!(plan.hook_command.is_none() && plan.env.is_empty());
}

/// Case 3: only registered context worktrees join: a scratch repo and a story-less folder
/// stay `NotManagedWorktree` and are never listed.
fn case_3_a_scratch_repo_stays_unmanaged_and_unlisted() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let (_clone, _root, _cwd) = context_worktree(&fixture);
    let scratch = fixture.home().join("scratch");
    git2::Repository::init(&scratch).unwrap();
    let plan = launch_in(&fixture, &scratch);
    assert_eq!(
        plan.status,
        AttributionStatus::Off { reason: OffReason::NotManagedWorktree }
    );
    assert!(plan.hook_command.is_none());
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let list = roots(&fixture).unwrap();
    assert!(!managed::RepoRoots::Listed(list).daemon_tracks(&scratch));
}

/// Case 4: with no source repo attached the key stays omitted (every repo is tracked), and a
/// source repo's list is exactly what it was when no context clone exists.
fn case_4_source_repo_roots_are_unchanged() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert_eq!(roots(&fixture), Some(vec![common_dir(&fixture.cwd)]));
    let before = sentinel(&fixture);
    // A context clone appears: the list gains it and the sentinel changes.
    let (clone, _root, _cwd) = context_worktree(&fixture);
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let mut expected = vec![common_dir(&fixture.cwd), common_dir(&clone)];
    expected.sort();
    assert_eq!(roots(&fixture), Some(expected));
    assert_ne!(sentinel(&fixture), before);
    // Detach every source repo: no list at all, context clone or not.
    crate::commands::attachments::forget_attachment_record(
        fixture.home(),
        1,
        &fixture.cwd.to_string_lossy(),
    )
    .unwrap();
    assert_eq!(managed::repo_roots(fixture.home()), managed::RepoRoots::All);
}

/// Case 5: attached clones that no longer resolve keep the previous list, context clone or not.
fn case_5_a_kept_list_is_never_widened_by_a_context_clone() {
    let fixture = Launch::new(AgentCli::Claude);
    let (_clone, _root, _cwd) = context_worktree(&fixture);
    std::fs::remove_dir_all(fixture.cwd.join(".git")).unwrap();
    assert!(matches!(
        managed::repo_roots(fixture.home()),
        managed::RepoRoots::Keep(_)
    ));
}

#[test]
fn sv_cde_85_pr2_case_table() {
    let cases: Vec<(usize, fn())> = vec![
        (1, case_1_a_context_worktree_launch_is_captured),
        (2, case_2_a_context_repo_set_off_is_not_captured),
        (3, case_3_a_scratch_repo_stays_unmanaged_and_unlisted),
        (4, case_4_source_repo_roots_are_unchanged),
        (5, case_5_a_kept_list_is_never_widened_by_a_context_clone),
    ];
    let failed: Vec<String> = cases
        .into_iter()
        .filter_map(|(case, run)| {
            std::panic::catch_unwind(run).err().map(|panic| {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                format!("case {case}: {message}")
            })
        })
        .collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
