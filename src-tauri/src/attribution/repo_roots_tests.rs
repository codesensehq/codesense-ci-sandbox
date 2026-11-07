// SPDX-License-Identifier: MIT
//! SV-CDE-67: git-ai's daemon tracks only the attached clones (`daemon_repo_roots`), as one
//! table over the story's cases 1–8.
//!
//! Every row runs in its own temp CodeSense home with a fake git-ai that logs its calls;
//! nothing touches the real `~/.codesense` or `~/.git-ai`.

use super::launch::{prepare, reapply_config, Plan, Reapplied, Request};
use super::managed;
use super::testing::Launch;
use super::watcher::tests::AppOwner;
use super::{AttributionStatus, OffReason};
use crate::agents::resolve::tests::alice;
use codesense_pty_core::agent_cli::AgentCli;
use std::path::Path;

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

/// The managed git-ai's calls, without the `HOME=` suffix every logged call carries.
fn calls(fixture: &Launch) -> Vec<String> {
    fixture
        .calls()
        .into_iter()
        .map(|call| call.split(" HOME=").next().unwrap().to_owned())
        .collect()
}

/// `daemon_repo_roots` as the fake git-ai's config file would hold it after the last write:
/// `None` when the key was unset (every repo is tracked). Panics if it was never written.
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
    let parsed: serde_json::Value = serde_json::from_str(value).expect("a JSON array");
    let list: Vec<String> =
        serde_json::from_value(parsed).expect("an array of strings, never null");
    assert!(!list.is_empty(), "an empty list is written as no key");
    assert!(
        list.iter().all(|root| Path::new(root).is_absolute()),
        "{list:?}"
    );
    Some(list)
}

fn tracks(list: &[String], checkout: &Path) -> bool {
    managed::RepoRoots::Listed(list.to_vec()).daemon_tracks(checkout)
}

fn sets(fixture: &Launch) -> usize {
    calls(fixture)
        .iter()
        .filter(|c| c.starts_with("config set daemon_repo_roots "))
        .count()
}

/// The canonical common dir of the clone at `checkout`, as git-ai matches it.
fn common_dir(checkout: &Path) -> String {
    std::fs::canonicalize(checkout.join(".git"))
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn sentinel(fixture: &Launch) -> String {
    std::fs::read_to_string(fixture.home().join("git-ai/config-applied")).unwrap()
}

/// A managed-worktree record for `path`, as hydrating a story writes one.
fn register_worktree(fixture: &Launch, path: &Path) {
    let record = crate::install::worktree_index::WorktreeStoryRecord::new(
        path,
        "acme",
        crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
        "SV-2",
        fixture.home().join("story-2.md"),
    );
    crate::install::worktree_index::write_record(fixture.home(), &alice(), &record).unwrap();
}

fn git(repo: &Path, args: &[&str]) -> String {
    crate::attribution::git_ai::tests::git(repo, args)
}

/// Case 1: a newly attached clone is listed before the daemon starts, and a clone attached
/// while it runs is listed with no restart (git-ai reloads the file).
fn case_1_first_attach_is_listed_before_the_daemon_starts() {
    let fixture = Launch::new(AgentCli::Claude);
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let first = calls(&fixture);
    let listed = first
        .iter()
        .position(|c| c.starts_with("config set daemon_repo_roots "))
        .unwrap_or_else(|| panic!("{first:?}"));
    let started = first.iter().position(|c| c == "bg start").unwrap();
    assert!(listed < started, "{first:?}");
    assert_eq!(roots(&fixture), Some(vec![common_dir(&fixture.cwd)]));
    let _daemon = fixture.daemon_running();
    let other = fixture.attach_checkout("other", "https://github.com/acme/other.git");
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let mut expected = vec![common_dir(&fixture.cwd), common_dir(Path::new(&other))];
    expected.sort();
    assert_eq!(roots(&fixture), Some(expected));
    assert!(
        !calls_contain(&fixture, "bg restart"),
        "{:?}",
        calls(&fixture)
    );
}

fn calls_contain(fixture: &Launch, call: &str) -> bool {
    calls(fixture).iter().any(|c| c == call)
}

/// Case 2: a linked worktree of a listed clone shares its common dir: it is captured, and
/// the list still names only the clone.
fn case_2_a_linked_worktree_is_covered_by_its_clone() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    git(&fixture.cwd, &["commit", "--allow-empty", "-m", "base"]);
    let linked = fixture.home().join("linked");
    git(
        &fixture.cwd,
        &["worktree", "add", "-b", "linked", &linked.to_string_lossy()],
    );
    register_worktree(&fixture, &linked);
    assert_eq!(
        launch_in(&fixture, &linked).status,
        AttributionStatus::Active
    );
    let list = roots(&fixture).unwrap();
    assert_eq!(list, [common_dir(&fixture.cwd)]);
    assert!(tracks(&list, &linked));
}

/// Case 3: a repo an agent's test suite creates under `$TMPDIR` is never listed, so the daemon
/// ignores it; the list is a valid array, so git-ai never fails closed (no error log).
fn case_3_a_scratch_repo_is_not_listed() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let scratch = tempfile::tempdir().unwrap();
    git2::Repository::init(scratch.path()).unwrap();
    // Nested inside the clone's working tree too: its common dir is its own.
    let nested = fixture.cwd.join("target/fixture");
    git2::Repository::init(&nested).unwrap();
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let list = roots(&fixture).unwrap();
    assert_eq!(list, [common_dir(&fixture.cwd)]);
    assert!(!tracks(&list, scratch.path()));
    assert!(!tracks(&list, &nested));
    assert!(tracks(&list, &fixture.cwd));
}

/// Case 4: detach drops the clone from the list and changes the sentinel, through the
/// running app's watcher with no launch.
fn case_4_detach_removes_the_clone_and_changes_the_sentinel() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let other = fixture.attach_checkout("other", "https://github.com/acme/other.git");
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert_eq!(roots(&fixture).unwrap().len(), 2);
    let before = sentinel(&fixture);
    let app = AppOwner::start(fixture.home());
    app.drive();
    crate::commands::attachments::forget_attachment_record(fixture.home(), 1, &other).unwrap();
    assert!(app.refresh_pending(), "detach tells the watcher");
    app.drive();
    assert_eq!(roots(&fixture), Some(vec![common_dir(&fixture.cwd)]));
    assert_ne!(sentinel(&fixture), before);
    // Attach is the same path.
    let again = fixture.attach_checkout("again", "https://github.com/acme/again.git");
    assert!(app.refresh_pending(), "attach tells the watcher");
    app.drive();
    assert!(roots(&fixture)
        .unwrap()
        .contains(&common_dir(Path::new(&again))));
}

/// Case 5: two profiles' clones are one union, whichever profile applies it, with no
/// flip-flop between them.
fn case_5_two_profiles_write_one_union() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let bob = fixture.home().join("bob-api");
    git2::Repository::init(&bob)
        .unwrap()
        .remote("origin", "https://github.com/bob/api.git")
        .unwrap();
    crate::commands::attachments::upsert_attachment_record(
        fixture.home(),
        2,
        crate::commands::attachments::existing_checkout_attachment(&bob).unwrap(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let mut expected = vec![common_dir(&fixture.cwd), common_dir(&bob)];
    expected.sort();
    assert_eq!(roots(&fixture), Some(expected.clone()));
    let written = sets(&fixture);
    assert_eq!(reapply_config(fixture.home()), Reapplied::Now);
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert_eq!(
        sets(&fixture),
        written,
        "the same union is not written again"
    );
    assert_eq!(roots(&fixture), Some(expected));
}

/// Case 6: with no attached clone the key is omitted (git-ai tracks every repo), never `null`.
fn case_6_no_attached_clone_omits_the_key() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert!(roots(&fixture).is_some());
    crate::commands::attachments::forget_attachment_record(
        fixture.home(),
        1,
        &fixture.cwd.to_string_lossy(),
    )
    .unwrap();
    assert_eq!(reapply_config(fixture.home()), Reapplied::Now);
    assert_eq!(roots(&fixture), None);
    assert!(!calls(&fixture)
        .iter()
        .any(|c| c.contains("null") || c.ends_with("[]")));
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert_eq!(
        super::launch::doctor(fixture.home(), None).daemon_repo_roots,
        Some(0)
    );
}

/// Case 7: a clone attached through a symlink is listed by its canonical path.
fn case_7_a_symlinked_clone_is_listed_canonically() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let real = fixture.home().join("real-clone");
    git2::Repository::init(&real).unwrap();
    let link = fixture.home().join("link-clone");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    crate::commands::attachments::upsert_attachment_record(
        fixture.home(),
        1,
        crate::domain::Attachment {
            repo_path: link.to_string_lossy().into_owned(),
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        },
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    let list = roots(&fixture).unwrap();
    assert!(list.contains(&common_dir(&real)), "{list:?}");
    assert!(
        !list.iter().any(|root| root.contains("link-clone")),
        "{list:?}"
    );
    assert!(tracks(&list, &link));
}

/// Case 8: after a restart the list is rebuilt from the attachments as they are now, with no
/// launch: a clone forgotten while the app was closed is not left listed.
fn case_8_restart_rebuilds_the_list_from_attachments() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    fixture.attach_checkout("other", "https://github.com/acme/other.git");
    assert_eq!(
        launch_in(&fixture, &fixture.cwd).status,
        AttributionStatus::Active
    );
    assert_eq!(roots(&fixture).unwrap().len(), 2);
    // Edited while the app was closed: no watcher heard it.
    let path = crate::context_repo::path_to_user_attachments_file(fixture.home(), &alice());
    let mut data = crate::context_repo::attachments_io::read_attachments(&path).unwrap();
    data.attachments
        .retain(|row| !row.repo_path.ends_with("other"));
    crate::context_repo::attachments_io::write_attachments_atomic(&path, &data).unwrap();
    let app = AppOwner::start(fixture.home());
    assert!(app.refresh_pending(), "startup refreshes");
    app.drive();
    assert_eq!(roots(&fixture), Some(vec![common_dir(&fixture.cwd)]));
    let doctor = super::launch::doctor(fixture.home(), None);
    assert_eq!(doctor.daemon_repo_roots, Some(1));
}

/// I1 honesty: a managed worktree whose clone is not attached would be ignored by the daemon,
/// so its launch is off rather than wired to record nothing.
fn unattached_clone_launches_off_not_silently() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let stray = fixture.home().join("stray");
    git2::Repository::init(&stray)
        .unwrap()
        .remote("origin", "https://github.com/acme/api.git")
        .unwrap();
    register_worktree(&fixture, &stray);
    let plan = launch_in(&fixture, &stray);
    assert_eq!(
        plan.status,
        AttributionStatus::Off {
            reason: OffReason::RepoNotAttached
        }
    );
    assert!(plan.hook_command.is_none() && plan.env.is_empty());
}

/// A worktree created before its clone is attached captures from the next launch once the
/// clone is attached: the running app re-lists the roots, with no app or daemon restart.
fn attaching_the_clone_captures_the_next_launch_without_a_restart() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let app = AppOwner::start(fixture.home());
    app.drive();
    let clone = fixture.home().join("late");
    git2::Repository::init(&clone)
        .unwrap()
        .remote("origin", "https://github.com/acme/api.git")
        .unwrap();
    git(&clone, &["commit", "--allow-empty", "-m", "base"]);
    let worktree = fixture.home().join("late-wt");
    git(
        &clone,
        &["worktree", "add", "-b", "late", &worktree.to_string_lossy()],
    );
    register_worktree(&fixture, &worktree);
    assert_eq!(
        launch_in(&fixture, &worktree).status,
        AttributionStatus::Off {
            reason: OffReason::RepoNotAttached
        }
    );
    crate::commands::attachments::upsert_attachment_record(
        fixture.home(),
        1,
        crate::commands::attachments::existing_checkout_attachment(&clone).unwrap(),
        None,
        false,
    )
    .unwrap();
    assert!(app.refresh_pending(), "attach tells the watcher");
    app.drive();
    assert!(roots(&fixture).unwrap().contains(&common_dir(&clone)));
    let plan = launch_in(&fixture, &worktree);
    assert_eq!(plan.status, AttributionStatus::Active);
    assert!(plan.hook_command.is_some());
    assert!(
        !calls_contain(&fixture, "bg restart"),
        "{:?}",
        calls(&fixture)
    );
}

/// When no attached clone resolves, the list written before is kept; a recorded worktree whose
/// clone is not attached is still `repo_not_attached`, never a false `active`.
fn kept_list_still_reports_an_unattached_worktree() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    crate::commands::attachments::forget_attachment_record(
        fixture.home(),
        1,
        &fixture.cwd.to_string_lossy(),
    )
    .unwrap();
    crate::commands::attachments::upsert_attachment_record(
        fixture.home(),
        1,
        crate::domain::Attachment {
            repo_path: fixture.home().join("gone").to_string_lossy().into_owned(),
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        },
        None,
        false,
    )
    .unwrap();
    let plan = launch_in(&fixture, &fixture.cwd);
    assert_eq!(
        plan.status,
        AttributionStatus::Off {
            reason: OffReason::RepoNotAttached
        }
    );
    assert!(plan.hook_command.is_none() && plan.env.is_empty());
}

#[test]
fn sv_cde_67_case_table() {
    let cases: Vec<(usize, fn())> = vec![
        (1, case_1_first_attach_is_listed_before_the_daemon_starts),
        (2, case_2_a_linked_worktree_is_covered_by_its_clone),
        (3, case_3_a_scratch_repo_is_not_listed),
        (4, case_4_detach_removes_the_clone_and_changes_the_sentinel),
        (5, case_5_two_profiles_write_one_union),
        (6, case_6_no_attached_clone_omits_the_key),
        (7, case_7_a_symlinked_clone_is_listed_canonically),
        (8, case_8_restart_rebuilds_the_list_from_attachments),
        (101, unattached_clone_launches_off_not_silently),
        (
            102,
            attaching_the_clone_captures_the_next_launch_without_a_restart,
        ),
        (103, kept_list_still_reports_an_unattached_worktree),
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
