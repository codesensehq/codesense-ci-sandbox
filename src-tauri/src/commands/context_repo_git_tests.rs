// SPDX-License-Identifier: MIT
use super::*;

use std::process::Command;

/// A context repo fixture driven through the real git CLI, so merges, rebases and
/// conflicts are exactly what an agent's git leaves behind.
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctx");
        std::fs::create_dir_all(&path).unwrap();
        let f = Fixture { _dir: dir, path };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.email", "me@example.com"]);
        f.git(&["config", "user.name", "Me"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.path.parent().unwrap())
            .env("GIT_EDITOR", "true")
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.path.join(rel)).ok()
    }

    fn commit(&self, rel: &str, text: &str, message: &str) -> String {
        self.write(rel, text);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Point `origin/<branch>` at `sha`, as a fetch would, with a remote configured.
    fn set_remote(&self, branch: &str, sha: &str) {
        if self.git(&["remote"]).is_empty() {
            self.git(&["remote", "add", "origin", "https://github.com/acme/ctx.git"]);
        }
        self.git(&["update-ref", &format!("refs/remotes/origin/{branch}"), sha]);
    }

    fn status(&self) -> ContextRepoStatus {
        context_repo_status_impl(&self.path).unwrap()
    }
}

const CLAUDE: &str = "\n\nCo-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>";

#[test]
fn status_reads_branch_upstream_and_counts() {
    let f = Fixture::new();
    let base = f.commit("a.md", "one\n", "base");
    f.git(&["checkout", "-q", "-b", "theirs"]);
    let t1 = f.commit("b.md", "b\n", "theirs 1");
    f.commit("c.md", "c\n", "theirs 2");
    let theirs_tip = f.git(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "theirs"]);
    f.commit("d.md", "d\n", "mine");
    f.set_remote("main", &theirs_tip);
    assert_ne!(base, t1);

    let s = f.status();
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert!(s.has_upstream);
    assert_eq!(s.upstream.as_deref(), Some("origin/main"));
    assert_eq!((s.ahead, s.behind), (1, 2));
    assert_eq!(s.sync.state, ContextSyncState::Ok);
    assert!(!s.sync.no_sender);
    assert!(s.conflicts.is_empty());
}

#[test]
fn status_detached_no_upstream_and_no_remote() {
    let f = Fixture::new();
    let first = f.commit("a.md", "one\n", "first");
    f.commit("a.md", "two\n", "second");

    // No remote at all: nothing will send these changes.
    let s = f.status();
    assert!(!s.has_upstream);
    assert!(s.sync.no_sender);

    // A branch GitHub has never seen.
    f.set_remote("main", &first);
    f.git(&["checkout", "-q", "-b", "feat-x"]);
    let s = f.status();
    assert_eq!(s.branch.as_deref(), Some("feat-x"));
    assert!(!s.has_upstream);
    assert!(!s.sync.no_sender);

    // Detached HEAD.
    f.git(&["checkout", "-q", "--detach", &first]);
    let s = f.status();
    assert_eq!(s.branch, None);
    assert_eq!(s.head_sha, first[..7]);
}

/// SW-VE-16: the status carries the `origin` remote's forge provider id so the file-viewer
/// toolbar can render `vs GitHub (origin/main)` without hardcoding GitHub. `None` when no
/// origin is configured; `Some("github")` for the canonical github.com case the fixture seeds;
/// `None` for a self-hosted host the forge table doesn't recognize.
#[test]
fn status_carries_origin_forge_provider_or_none() {
    let f = Fixture::new();
    let first = f.commit("a.md", "one\n", "first");

    // No remote at all → no provider.
    let s = f.status();
    assert_eq!(s.remote_provider, None);

    // `set_remote` configures origin at github.com → `github`.
    f.set_remote("main", &first);
    let s = f.status();
    assert_eq!(s.remote_provider.as_deref(), Some("github"));

    // Point origin at a self-hosted host the forge table doesn't recognize → `None`.
    f.git(&[
        "remote",
        "set-url",
        "origin",
        "https://corp.example/acme/ctx.git",
    ]);
    let s = f.status();
    assert_eq!(s.remote_provider, None);

    // Point origin at gitlab.com → `gitlab`.
    f.git(&[
        "remote",
        "set-url",
        "origin",
        "https://gitlab.com/acme/ctx.git",
    ]);
    let s = f.status();
    assert_eq!(s.remote_provider.as_deref(), Some("gitlab"));

    // Bitbucket is the third shipped forge in GIT_FORGE_HOSTS → `bitbucket`.
    f.git(&[
        "remote",
        "set-url",
        "origin",
        "https://bitbucket.org/acme/ctx.git",
    ]);
    let s = f.status();
    assert_eq!(s.remote_provider.as_deref(), Some("bitbucket"));

    // SSH form (`git@github.com:acme/ctx.git`) is what `git clone git@…` leaves behind for
    // keypair-authed clones. `normalize_source_repo_url` strips `git@` + replaces the SSH
    // colon so the host still resolves. The forge decision must not depend on scheme.
    f.git(&["remote", "set-url", "origin", "git@github.com:acme/ctx.git"]);
    let s = f.status();
    assert_eq!(s.remote_provider.as_deref(), Some("github"));
}

/// main and origin/main both changed `roadmap.md`; the merge stops on it, with `team.yaml`
/// from GitHub staged beside it.
fn merge_conflict() -> Fixture {
    let f = Fixture::new();
    let base = f.commit("roadmap.md", "Q3\n", "base");
    f.git(&["checkout", "-q", "-b", "gh"]);
    f.write("team.yaml", "name: acme\n");
    f.commit("roadmap.md", "Q4 moved\n", "maya: move the table");
    let gh = f.git(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "gh"]);
    f.commit(
        "roadmap.md",
        "Q4 table\n",
        &format!("Add the Q4 table{CLAUDE}"),
    );
    f.set_remote("main", &gh);
    f.git(&["merge", "-q", "origin/main"]);
    assert!(f.read("roadmap.md").unwrap().contains("<<<<<<<"));
    let _ = base;
    f
}

#[test]
fn a_merge_conflict_names_both_sides_the_last_writer_and_what_waits() {
    let f = merge_conflict();
    let s = f.status();
    assert_eq!(s.conflicts.len(), 1);
    let c = &s.conflicts[0];
    assert_eq!(c.path, "roadmap.md");
    assert_eq!(c.mine.who.as_deref(), Some("claude"));
    assert_eq!(c.mine.subject.as_deref(), Some("Add the Q4 table"));
    assert_eq!(c.theirs.who.as_deref(), Some("Me"));
    assert_eq!(c.last_writer.as_deref(), Some("claude"));
    assert_eq!(c.held_back, vec!["team.yaml".to_string()]);
    // A conflict is not a failed sync: the card settles it.
    assert_eq!(s.sync.state, ContextSyncState::Ok);
}

#[test]
fn resolving_use_mine_stages_this_macs_version() {
    let f = merge_conflict();
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseMine).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "Q4 table\n");
    assert!(f.status().conflicts.is_empty());
}

#[test]
fn resolving_use_theirs_stages_githubs_version() {
    let f = merge_conflict();
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseTheirs).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "Q4 moved\n");
    assert!(f.status().conflicts.is_empty());
    // The agent can finish the merge: nothing is left unmerged.
    f.git(&["commit", "-q", "--no-edit"]);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn resolving_keep_both_puts_githubs_copy_beside_mine() {
    let f = merge_conflict();
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::KeepBoth).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "Q4 table\n");
    assert_eq!(f.read("roadmap (GitHub).md").unwrap(), "Q4 moved\n");
    assert!(f.status().conflicts.is_empty());
}

#[test]
fn resolving_a_file_not_in_conflict_or_outside_the_repo_is_refused() {
    let f = merge_conflict();
    assert!(
        resolve_context_conflict_impl(&f.path, "team.yaml", ConflictResolution::UseMine).is_err()
    );
    assert!(
        resolve_context_conflict_impl(&f.path, "../x.md", ConflictResolution::UseMine).is_err()
    );
}

#[test]
fn a_rebase_that_stopped_conflicted_reads_mine_from_the_replayed_commit() {
    let f = Fixture::new();
    f.commit("roadmap.md", "Q3\n", "base");
    f.git(&["checkout", "-q", "-b", "gh"]);
    let gh = f.commit("roadmap.md", "theirs\n", "maya: theirs");
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "gh"]);
    f.commit("roadmap.md", "mine\n", &format!("mine{CLAUDE}"));
    f.set_remote("main", &gh);
    f.git(&["rebase", "origin/main"]);
    let s = f.status();
    assert_eq!(s.conflicts.len(), 1);
    assert_eq!(s.conflicts[0].mine.who.as_deref(), Some("claude"));
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseMine).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "mine\n");
}

#[test]
fn an_operation_in_progress_is_syncing_until_it_stalls() {
    let f = Fixture::new();
    f.commit("a.md", "a\n", "base");
    let rebase = f.path.join(".git/rebase-merge");
    std::fs::create_dir_all(&rebase).unwrap();
    std::fs::write(rebase.join("msgnum"), "3\n").unwrap();
    std::fs::write(rebase.join("end"), "7\n").unwrap();
    let repo = git2::Repository::open(&f.path).unwrap();

    let fresh = sync_state(&repo, &f.path, None, false, now_ms());
    assert_eq!(fresh.state, ContextSyncState::Syncing);
    assert_eq!(fresh.progress.as_deref(), Some("Rebasing 3 of 7"));

    let later = now_ms() + STALLED_AFTER.as_millis() as i64 + 60_000;
    let stalled = sync_state(&repo, &f.path, None, false, later);
    assert_eq!(stalled.state, ContextSyncState::Failed);
    assert_eq!(
        stalled.reason.as_deref(),
        Some("Rebasing 3 of 7 stopped partway")
    );
}

#[test]
fn a_reported_failure_stands_until_the_clone_hears_from_github_again() {
    let f = Fixture::new();
    let sha = f.commit("a.md", "a\n", "base");
    f.set_remote("main", &sha);
    let now = now_ms();
    record_sync_report(
        &f.path,
        ContextSyncReport {
            state: ContextSyncState::Retrying,
            reason: Some("The network dropped".into()),
            attempt: Some(2),
            max_attempts: Some(5),
            progress: None,
        },
        now + 60_000,
    );
    let s = f.status();
    assert_eq!(s.sync.state, ContextSyncState::Retrying);
    assert_eq!((s.sync.attempt, s.sync.max_attempts), (Some(2), Some(5)));

    // A later fetch supersedes it.
    record_sync_report(
        &f.path,
        ContextSyncReport {
            state: ContextSyncState::Failed,
            reason: Some("refused".into()),
            attempt: None,
            max_attempts: None,
            progress: None,
        },
        now - 3_600_000,
    );
    std::fs::write(f.path.join(".git/FETCH_HEAD"), "").unwrap();
    assert_eq!(f.status().sync.state, ContextSyncState::Ok);
}

#[test]
fn identity_reads_agents_from_trailers_and_bots() {
    let f = Fixture::new();
    f.commit("a.md", "a\n", &format!("Agent work{CLAUDE}"));
    f.commit(
        "b.md",
        "b\n",
        "Note\n\nActor-Kind: agent\nActor-Name: Barb\nActor-Avatar: pack:barb",
    );
    f.commit(
        "c.md",
        "c\n",
        "Pair\n\nCo-Authored-By: Sam <123+sam@users.noreply.github.com>",
    );
    let repo = git2::Repository::open(&f.path).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let barb = head.parent(0).unwrap();
    let claude = barb.parent(0).unwrap();
    assert_eq!(
        commit_identity(&head).agent,
        None,
        "a person co-author is not an agent"
    );
    assert_eq!(
        commit_identity(&barb).agent,
        Some(CommitAgent {
            name: "Barb".into(),
            avatar: Some("pack:barb".into())
        })
    );
    let c = commit_identity(&claude);
    assert_eq!(c.agent.map(|a| a.name).as_deref(), Some("claude"));
    assert_eq!(c.author, "Me");
}

/// A context repo shaped like the real one: main with a merged PR branch and a squash, an
/// open PR, a local branch GitHub never saw, a backup, and one commit not pushed.
fn history_fixture() -> Fixture {
    let f = Fixture::new();
    f.commit("team/a.md", "a\n", "Consolidate mockups");
    f.git(&["checkout", "-q", "-b", "gs/fixes"]);
    f.commit("team/fx.md", "fx\n", &format!("FX-7 done{CLAUDE}"));
    f.git(&["checkout", "-q", "main"]);
    f.commit("other/x.md", "x\n", "Store the notify hook");
    f.git(&[
        "merge",
        "-q",
        "--no-ff",
        "gs/fixes",
        "-m",
        "Merge pull request #22 from acme/gs/fixes\n\nFixes from the remote-machine test",
    ]);
    f.git(&["branch", "-D", "gs/fixes"]);
    f.commit("team/b.md", "b\n", "Migrate dashboards to runPrompt (#23)");
    f.git(&["checkout", "-q", "-b", "backup/mock"]);
    f.commit("team/m.md", "m\n", "Back up the mockup");
    f.git(&["checkout", "-q", "main"]);
    f.git(&["checkout", "-q", "-b", "gs/sync-routines"]);
    let open = f.commit("team/r.md", "r\n", &format!("Sync routine stories{CLAUDE}"));
    f.git(&["checkout", "-q", "main"]);
    let pushed = f.git(&["rev-parse", "HEAD"]);
    f.set_remote("main", &pushed);
    f.set_remote("gs/sync-routines", &open);
    f.set_remote("backup/mock", &f.git(&["rev-parse", "backup/mock"]));
    f.git(&["update-ref", "refs/remotes/pr/24", &open]);
    f.git(&["branch", "-D", "gs/sync-routines"]);
    f.git(&["checkout", "-q", "-b", "feat-left-nav"]);
    f.commit("team/l.md", "l\n", "Board and Pipeline panes");
    f.git(&["checkout", "-q", "main"]);
    f.commit("team/z.md", "z\n", "Explicit sync epics");
    f
}

fn page(f: &Fixture, branches: HistoryBranches, paths: Option<&[String]>) -> HistoryPage {
    context_repo_history_impl(&f.path, &branches, paths, 100, None).unwrap()
}

fn by_subject<'a>(p: &'a HistoryPage, subject: &str) -> &'a HistoryCommit {
    p.commits
        .iter()
        .find(|c| c.subject == subject)
        .unwrap_or_else(|| panic!("no commit {subject}"))
}

#[test]
fn history_draws_lanes_prs_agents_and_what_github_lacks() {
    let f = history_fixture();
    let p = page(&f, HistoryBranches::All, None);

    let newest = &p.commits[0];
    assert_eq!(newest.subject, "Explicit sync epics");
    assert_eq!(newest.lane, LaneKind::Main);
    assert!(newest.local_only, "not on GitHub yet");
    assert!(newest.by_you);
    assert!(newest.refs.iter().any(|r| r.kind == RefKind::Head));

    let merge = by_subject(&p, "Fixes from the remote-machine test");
    assert_eq!(merge.pr, Some(22));
    assert_eq!(merge.parents.len(), 2);
    assert_eq!(merge.lane, LaneKind::Main);

    let squash = by_subject(&p, "Migrate dashboards to runPrompt");
    assert_eq!(squash.pr, Some(23));
    assert!(!squash.local_only);

    let fx = by_subject(&p, "FX-7 done");
    assert_eq!(fx.lane, LaneKind::Merged);
    assert_eq!(fx.agent.as_ref().map(|a| a.name.as_str()), Some("claude"));

    let open = by_subject(&p, "Sync routine stories");
    assert_eq!(open.lane, LaneKind::Open);
    assert!(
        !open.refs.iter().any(|r| r.name == "pr/24"),
        "the PR head shows as its PR, not a branch"
    );
    assert_eq!(p.open_prs.len(), 1);
    assert_eq!(p.open_prs[0].number, 24);
    assert_eq!(p.open_prs[0].branch.as_deref(), Some("gs/sync-routines"));

    assert_eq!(
        by_subject(&p, "Board and Pipeline panes").lane,
        LaneKind::Local
    );
    assert!(by_subject(&p, "Board and Pipeline panes").local_only);
    assert_eq!(by_subject(&p, "Back up the mockup").lane, LaneKind::Backup);

    assert_eq!(p.branches[0], "main");
    for b in ["gs/sync-routines", "feat-left-nav", "backup/mock"] {
        assert!(p.branches.iter().any(|x| x == b), "{b} is listed");
    }
    assert_eq!(p.next_cursor, None);
}

#[test]
fn history_main_only_follows_first_parents() {
    let f = history_fixture();
    let p = page(&f, HistoryBranches::Main, None);
    assert!(p.commits.iter().all(|c| c.lane == LaneKind::Main));
    assert!(p.commits.iter().all(|c| c.parents.len() <= 1));
    assert!(p.commits.iter().any(|c| c.pr == Some(22)));
    assert!(!p.commits.iter().any(|c| c.subject == "FX-7 done"));
}

#[test]
fn history_one_branch_ends_where_it_left_main() {
    let f = history_fixture();
    let p = page(&f, HistoryBranches::Branch("gs/sync-routines".into()), None);
    assert_eq!(p.commits.len(), 2);
    assert_eq!(p.commits[0].subject, "Sync routine stories");
    let fork = &p.commits[1];
    assert!(fork.fork_point);
    assert!(fork.parents.is_empty());
}

#[test]
fn history_keeps_only_commits_touching_the_paths() {
    let f = history_fixture();
    let only = vec!["other".to_string()];
    let p = page(&f, HistoryBranches::All, Some(&only));
    let subjects: Vec<_> = p.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, vec!["Store the notify hook"]);
    assert_eq!(
        p.commits[0].files.as_deref(),
        Some(&["other/x.md".to_string()][..])
    );
}

#[test]
fn history_pages_with_a_cursor() {
    let f = history_fixture();
    let first = context_repo_history_impl(&f.path, &HistoryBranches::All, None, 3, None).unwrap();
    assert_eq!(first.commits.len(), 3);
    let cursor = first.next_cursor.clone().expect("more to come");
    let next = context_repo_history_impl(&f.path, &HistoryBranches::All, None, 100, Some(&cursor))
        .unwrap();
    let all = page(&f, HistoryBranches::All, None);
    assert_eq!(first.commits.len() + next.commits.len(), all.commits.len());
    assert_eq!(next.commits[0].sha, all.commits[3].sha);
}

#[test]
fn hand_off_and_action_texts_name_the_file_and_the_report_tool() {
    let text = conflict_hand_off_text(Path::new("/ctx"), "team/roadmap.md");
    assert!(text.contains("team/roadmap.md"));
    let retry = sync_action_text(Path::new("/ctx"), ContextSyncAction::Retry, Some("refused"));
    assert!(retry.contains("(refused)") && retry.contains("report_context_sync"));
    assert!(sync_action_text(Path::new("/ctx"), ContextSyncAction::SetUp, None).contains("remote"));
}

#[test]
fn keep_both_copy_sits_beside_the_file() {
    assert_eq!(
        theirs_copy_path("a/roadmap.html"),
        "a/roadmap (GitHub).html"
    );
    assert_eq!(theirs_copy_path("README"), "README (GitHub)");
}

fn index_mode(f: &Fixture, rel: &str) -> String {
    f.git(&["ls-files", "-s", "--", rel])
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn staged(f: &Fixture, rel: &str) -> String {
    f.git(&["show", &format!(":{rel}")])
}

#[cfg(unix)]
#[test]
fn settling_never_writes_through_a_symlink_in_the_working_tree() {
    let f = merge_conflict();
    let outside = f.path.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "secret\n").unwrap();
    std::fs::remove_file(f.path.join("roadmap.md")).unwrap();
    std::os::unix::fs::symlink(&outside, f.path.join("roadmap.md")).unwrap();
    std::os::unix::fs::symlink(&outside, f.path.join("roadmap (GitHub).md")).unwrap();

    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::KeepBoth).unwrap();

    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret\n");
    for rel in ["roadmap.md", "roadmap (GitHub).md"] {
        let meta = std::fs::symlink_metadata(f.path.join(rel)).unwrap();
        assert!(meta.file_type().is_file(), "{rel} is a regular file");
        assert_eq!(index_mode(&f, rel), "100644");
    }
    assert_eq!(f.read("roadmap.md").unwrap(), "Q4 table\n");
    assert_eq!(f.read("roadmap (GitHub).md").unwrap(), "Q4 moved\n");
}

#[cfg(unix)]
#[test]
fn settling_refuses_a_path_whose_folder_leads_out_of_the_repo() {
    let f = Fixture::new();
    f.commit("docs/plan.md", "base\n", "base");
    f.git(&["checkout", "-q", "-b", "gh"]);
    let gh = f.commit("docs/plan.md", "theirs\n", "theirs");
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "gh"]);
    f.commit("docs/plan.md", "mine\n", "mine");
    f.set_remote("main", &gh);
    f.git(&["merge", "-q", "origin/main"]);

    let outside = f.path.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("plan.md"), "keep\n").unwrap();
    std::fs::remove_dir_all(f.path.join("docs")).unwrap();
    std::os::unix::fs::symlink(&outside, f.path.join("docs")).unwrap();

    assert!(
        resolve_context_conflict_impl(&f.path, "docs/plan.md", ConflictResolution::UseMine)
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("plan.md")).unwrap(),
        "keep\n"
    );
}

#[cfg(unix)]
#[test]
fn a_conflicted_symlink_is_settled_as_a_symlink() {
    let f = Fixture::new();
    let link = f.path.join("current.md");
    std::os::unix::fs::symlink("a.md", &link).unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["checkout", "-q", "-b", "gh"]);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("b.md", &link).unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "theirs"]);
    let gh = f.git(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "gh"]);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("c.md", &link).unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "mine"]);
    f.set_remote("main", &gh);
    f.git(&["merge", "-q", "origin/main"]);
    assert_eq!(f.status().conflicts.len(), 1);

    resolve_context_conflict_impl(&f.path, "current.md", ConflictResolution::UseTheirs).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), PathBuf::from("b.md"));
    assert_eq!(index_mode(&f, "current.md"), "120000");
    assert!(f.status().conflicts.is_empty());
}

/// main and origin/main both changed `roadmap.md`; `git rebase origin/main` stops on it.
fn rebase_conflict() -> Fixture {
    let f = Fixture::new();
    f.commit("roadmap.md", "Q3\n", "base");
    f.git(&["checkout", "-q", "-b", "gh"]);
    let gh = f.commit("roadmap.md", "theirs\n", "maya: theirs");
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "gh"]);
    f.commit("roadmap.md", "mine\n", &format!("mine{CLAUDE}"));
    f.set_remote("main", &gh);
    f.git(&["rebase", "origin/main"]);
    assert!(f.path.join(".git/rebase-merge").is_dir());
    f
}

#[test]
fn during_a_rebase_use_theirs_and_keep_both_write_the_right_sides() {
    let f = rebase_conflict();
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseTheirs).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "theirs\n");
    assert_eq!(staged(&f, "roadmap.md"), "theirs");

    let f = rebase_conflict();
    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::KeepBoth).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "mine\n");
    assert_eq!(staged(&f, "roadmap.md"), "mine");
    assert_eq!(f.read("roadmap (GitHub).md").unwrap(), "theirs\n");
    assert_eq!(staged(&f, "roadmap (GitHub).md"), "theirs");
    assert!(f.status().conflicts.is_empty());
}

#[test]
fn a_stash_apply_conflict_reads_mine_from_the_stash() {
    let f = Fixture::new();
    f.commit("roadmap.md", "Q3\n", "base");
    f.write("roadmap.md", "mine\n");
    f.git(&["stash", "-q"]);
    f.commit("roadmap.md", "theirs\n", "theirs");
    f.git(&["stash", "apply", "-q"]);
    assert_eq!(f.status().conflicts.len(), 1);

    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseMine).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "mine\n");
    assert_eq!(staged(&f, "roadmap.md"), "mine");
}

#[test]
fn a_squash_merge_conflict_reads_mine_from_head() {
    let f = Fixture::new();
    f.commit("roadmap.md", "Q3\n", "base");
    f.git(&["checkout", "-q", "-b", "gh"]);
    f.commit("roadmap.md", "theirs\n", "theirs");
    f.git(&["checkout", "-q", "main"]);
    f.commit("roadmap.md", "mine\n", "mine");
    f.git(&["merge", "-q", "--squash", "gh"]);
    assert_eq!(f.status().conflicts.len(), 1);

    resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseMine).unwrap();
    assert_eq!(f.read("roadmap.md").unwrap(), "mine\n");
}

#[test]
fn a_conflict_with_no_sign_of_which_side_is_mine_is_not_settled_by_guessing() {
    let f = Fixture::new();
    f.commit("roadmap.md", "Q3\n", "base");
    f.git(&["checkout", "-q", "-b", "other"]);
    f.commit("roadmap.md", "other\n", "other");
    f.git(&["checkout", "-q", "main"]);
    f.write("roadmap.md", "local edit\n");
    f.git(&["checkout", "-q", "-m", "other"]);
    assert_eq!(f.status().conflicts.len(), 1);

    let err = resolve_context_conflict_impl(&f.path, "roadmap.md", ConflictResolution::UseMine)
        .unwrap_err();
    assert!(err.contains("agent"), "{err}");
    assert!(f.read("roadmap.md").unwrap().contains("<<<<<<<"));
}

#[test]
fn a_rebase_in_progress_keeps_the_branch_and_its_counts() {
    let f = rebase_conflict();
    let s = f.status();
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert!(s.has_upstream);
    assert_eq!(s.upstream.as_deref(), Some("origin/main"));
    assert_eq!((s.ahead, s.behind), (1, 1));
}
