// SPDX-License-Identifier: MIT
//! SV-CDE-73: an unsupported CLI's commits are never credited to the person.
//!
//! Every row runs real `git` in a temp repo with the environment the launch plan gives the
//! session, and reads the reflog git wrote the way git-ai's untraced-commit fixup does.

use super::git_ai::tests::rebuild;
use super::launch::{prepare, Plan, Request};
use super::ranges::AttributionSource;
use super::testing::{git_as, Launch};
use super::unsupported::{passes, Reflogs, REASON};
use crate::agents::resolve::tests::alice;
use codesense_pty_core::agent_cli::AgentCli;
use serde_json::json;
use std::path::Path;

/// The `HEAD` records git-ai's untraced-commit fixup credits to the person: those whose message
/// starts with one of these (alfypops/git-ai `src/daemon/ref_cursor.rs`
/// `UNTRACED_COMMIT_REFLOG_PREFIXES`, the same at `v1.7.5-cs.2` and at
/// `d0325fa3d251b538d0e8a69348e6b3546a1c7723`, the `v1.7.5-cs.3` pin's source). Every other
/// record is settled once and never claimed.
const FIXUP_CLAIMS: [&str; 3] = ["commit:", "commit (initial):", "commit (merge):"];

fn plan(fixture: &Launch, runtime: AgentCli) -> Plan {
    prepare(
        &Request {
            codesense_home: fixture.home(),
            principal: &alice(),
            cwd: &fixture.cwd,
            runtime,
        },
        None,
    )
}

/// The CLI that made `commit`, from one fresh read of the reflogs.
fn committed_by(repo: &Path, commit: &str) -> Option<String> {
    Reflogs::read(repo).committed_by(repo, commit)
}

fn head(repo: &Path) -> String {
    git_as(repo, &[], &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Commit `lines` lines in `path` as a session with `env`.
fn commit(
    repo: &Path,
    env: &[(String, String)],
    path: &str,
    lines: usize,
    subject: &str,
) -> String {
    let text: String = (1..=lines).map(|n| format!("{path} {n}\n")).collect();
    std::fs::write(repo.join(path), text).unwrap();
    git_as(repo, env, &["add", path]);
    git_as(repo, env, &["commit", "-qm", subject]);
    head(repo)
}

/// Whether the fixup would credit the person with the `HEAD` record that moved to `sha` in
/// `git_dir`'s reflog.
fn fixup_claims(git_dir: &Path, sha: &str) -> bool {
    let log = std::fs::read_to_string(git_dir.join("logs/HEAD")).unwrap();
    let message = log
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(header, _)| header.split(' ').nth(1) == Some(sha))
        .map(|(_, message)| message)
        .last()
        .unwrap_or_else(|| panic!("no HEAD record for {sha} in {log}"));
    FIXUP_CLAIMS
        .iter()
        .any(|prefix| message.starts_with(prefix))
}

/// What git-ai's fixup does with `sha`, the `HEAD` commit, which added `lines` lines to
/// `path`: credit the person in a note when it claims the record, as it did for OpenCode's
/// commit in SV-CDE-71 (`h_efb682ecbfe328 1-5`).
pub(crate) fn run_fixup(repo: &Path, sha: &str, path: &str, lines: usize) {
    if !fixup_claims(&repo.join(".git"), sha) {
        return;
    }
    let note = format!(
        "{path}\n  h_efb682ecbfe328 1-{lines}\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"git_ai_version\":\"1.7.5-cs.2\",\"base_commit_sha\":\"{sha}\",\"prompts\":{{}},\"humans\":{{\"h_efb682ecbfe328\":{{\"author\":\"Fixture <fixture@example.com>\"}}}}}}"
    );
    git_as(repo, &[], &["notes", "--ref=ai", "add", "-m", &note, sha]);
}

/// The environment the launch plan gives an OpenCode session in a managed worktree.
pub(crate) fn opencode_session_env() -> Vec<(String, String)> {
    let fixture = Launch::new(AgentCli::OpenCode);
    plan(&fixture, AgentCli::OpenCode).env
}

/// Commit `lines` lines in `path` as a session with `env`, then let the fixup run.
pub(crate) fn commit_and_fixup(
    repo: &Path,
    env: &[(String, String)],
    path: &str,
    lines: usize,
    subject: &str,
) -> String {
    let sha = commit(repo, env, path, lines, subject);
    run_fixup(repo, &sha, path, lines);
    sha
}

#[test]
fn an_unsupported_session_commit_is_left_by_the_fixup_and_names_its_cli() {
    for (runtime, cli) in [
        (AgentCli::OpenCode, "opencode"),
        (AgentCli::Hermes, "hermes"),
    ] {
        let fixture = Launch::new(runtime);
        let env = plan(&fixture, runtime).env;
        let repo = fixture.cwd.as_path();
        let git_dir = repo.join(".git");

        let initial = commit(repo, &env, "first.txt", 2, "first");
        let work = commit(repo, &env, "work.txt", 5, "SV-CDE-71: probe");
        for sha in [&initial, &work] {
            assert!(!fixup_claims(&git_dir, sha), "{cli}: {sha}");
            assert_eq!(
                committed_by(repo, sha).as_deref(),
                Some(cli),
                "{cli}: {sha}"
            );
        }

        // The person's own commit, in their shell with no session environment (AC4).
        let own = commit(repo, &[], "own.txt", 1, "mine");
        assert!(fixup_claims(&git_dir, &own), "{cli}");
        assert_eq!(committed_by(repo, &own), None, "{cli}");

        // The managed worktree is a linked worktree of the attached clone: its records are
        // found from the clone.
        let linked = fixture.home().join("linked");
        git_as(
            repo,
            &[],
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/linked",
                linked.to_str().unwrap(),
            ],
        );
        let there = commit(&linked, &env, "there.txt", 3, "there");
        let linked_git_dir = repo.join(".git/worktrees/linked");
        assert!(!fixup_claims(&linked_git_dir, &there), "{cli}");
        assert_eq!(committed_by(repo, &there).as_deref(), Some(cli), "{cli}");
        // After the worktree is gone, its branch's log still says whose the commit is.
        git_as(
            repo,
            &[],
            &["worktree", "remove", "--force", linked.to_str().unwrap()],
        );
        assert_eq!(committed_by(repo, &there).as_deref(), Some(cli), "{cli}");
        // Once no reflog records it, nothing names the CLI (the lines stay unattributed).
        git_as(repo, &[], &["branch", "-D", "feat/linked"]);
        assert_eq!(committed_by(repo, &there), None, "{cli}");
    }
}

/// A commit the session moved to without creating it never names the CLI: a fast-forward
/// to someone else's commit, an amend, a rebase pick and a merge. One it made by copying
/// does: a cherry-pick, a revert and a `git am`.
#[test]
fn only_a_commit_the_session_created_names_the_cli() {
    let fixture = Launch::new(AgentCli::OpenCode);
    let env = plan(&fixture, AgentCli::OpenCode).env;
    let repo = fixture.cwd.as_path();
    commit(repo, &[], "base.txt", 1, "base");
    git_as(repo, &[], &["checkout", "-qb", "upstream"]);
    let theirs = commit(repo, &[], "theirs.txt", 1, "theirs");
    git_as(repo, &[], &["checkout", "-q", "-"]);
    git_as(repo, &env, &["merge", "-q", "--ff-only", "upstream"]);
    assert_eq!(head(repo), theirs);
    assert_eq!(committed_by(repo, &theirs), None, "fast-forward");

    let made = commit(repo, &env, "made.txt", 1, "made");
    assert_eq!(committed_by(repo, &made).as_deref(), Some("opencode"));
    // Same subject, new tree: the amend's record moves from `made`, not from its parent.
    std::fs::write(repo.join("made.txt"), "amended\n").unwrap();
    git_as(repo, &env, &["commit", "-qa", "--amend", "-m", "made"]);
    assert_ne!(head(repo), made);
    assert_eq!(committed_by(repo, &head(repo)), None, "amend");

    git_as(repo, &[], &["checkout", "-qb", "side", "upstream"]);
    commit(repo, &[], "side.txt", 1, "side");
    git_as(repo, &[], &["checkout", "-q", "-"]);
    git_as(repo, &env, &["rebase", "-q", "side"]);
    assert_eq!(committed_by(repo, &head(repo)), None, "rebase pick");
    git_as(repo, &[], &["checkout", "-qb", "other", "side"]);
    commit(repo, &[], "other.txt", 1, "other");
    git_as(repo, &[], &["checkout", "-q", "-"]);
    git_as(
        repo,
        &env,
        &["merge", "-q", "--no-ff", "-m", "joined", "other"],
    );
    assert_eq!(committed_by(repo, &head(repo)), None, "merge");
    assert!(!fixup_claims(&repo.join(".git"), &head(repo)), "merge");

    // Copies are the session's commits: named, and never claimed for the person.
    git_as(repo, &[], &["checkout", "-qb", "donor", "upstream"]);
    let donor = commit(repo, &[], "donor.txt", 1, "donor");
    git_as(repo, &[], &["checkout", "-q", "-"]);
    git_as(repo, &env, &["cherry-pick", &donor]);
    let picked = head(repo);
    git_as(repo, &env, &["revert", "--no-edit", &picked]);
    let reverted = head(repo);
    let patch = git_as(repo, &[], &["format-patch", "-1", &donor, "--stdout"]);
    let patch_file = fixture.home().join("donor.patch");
    std::fs::write(&patch_file, patch).unwrap();
    git_as(repo, &[], &["checkout", "-qb", "applied", "upstream"]);
    git_as(repo, &env, &["am", "-q", patch_file.to_str().unwrap()]);
    let applied = head(repo);
    for (label, sha) in [
        ("cherry-pick", &picked),
        ("revert", &reverted),
        ("am", &applied),
    ] {
        assert_eq!(
            committed_by(repo, sha).as_deref(),
            Some("opencode"),
            "{label}"
        );
        assert!(!fixup_claims(&repo.join(".git"), sha), "{label}");
    }
}

/// git squeezes whitespace in the subject it writes to a reflog; the commit still names the CLI.
#[test]
fn a_subject_with_runs_of_whitespace_still_names_the_cli() {
    let fixture = Launch::new(AgentCli::OpenCode);
    let env = plan(&fixture, AgentCli::OpenCode).env;
    let repo = fixture.cwd.as_path();
    let sha = commit(repo, &env, "spaced.txt", 1, "two  spaces\tand a tab ");
    assert_eq!(committed_by(repo, &sha).as_deref(), Some("opencode"));
}

/// Pinned, interim behaviour: the marker replaces a session's `checkout: moving from` records,
/// so `git checkout -` afterwards goes back to the last branch switched to outside the
/// session, not to the one the session left. Use explicit branch names in such sessions.
#[test]
fn checkout_dash_after_a_session_returns_to_the_last_branch_switched_to_outside_it() {
    let fixture = Launch::new(AgentCli::OpenCode);
    let env = plan(&fixture, AgentCli::OpenCode).env;
    let repo = fixture.cwd.as_path();
    commit(repo, &[], "base.txt", 1, "base");
    for branch in ["a", "b", "c"] {
        git_as(repo, &[], &["branch", branch]);
    }
    git_as(repo, &[], &["checkout", "-q", "a"]);
    git_as(repo, &[], &["checkout", "-q", "b"]);
    git_as(repo, &env, &["checkout", "-q", "c"]);
    git_as(repo, &[], &["checkout", "-q", "-"]);
    let branch = git_as(repo, &[], &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "a", "not b, which the session left");
}

/// One read of the reflogs serves every noteless commit `why_line` meets in a call.
#[test]
fn why_line_reads_the_reflogs_once_for_every_noteless_commit() {
    let home = tempfile::tempdir().unwrap();
    let repo = rebuild();
    super::why_line::tests::attach(home.path(), repo.path());
    let env = opencode_session_env();
    let mut text = String::new();
    for n in 1..=3 {
        text.push_str(&format!("line {n}\n"));
        std::fs::write(repo.path().join("grown.txt"), &text).unwrap();
        git_as(repo.path(), &env, &["add", "grown.txt"]);
        git_as(repo.path(), &env, &["commit", "-qm", &format!("grow {n}")]);
    }
    let before = passes::of(repo.path());
    let result = why_line(home.path(), "grown.txt", "HEAD", 1, 3);
    let ranges = result["ranges"].as_array().unwrap();
    assert_eq!(ranges.len(), 3, "{result}");
    assert!(
        ranges.iter().all(|range| range["tool"] == "opencode"),
        "{result}"
    );
    assert_eq!(passes::of(repo.path()) - before, 1);
}

fn why_line(home: &Path, path: &str, rev: &str, start: u32, end: u32) -> serde_json::Value {
    serde_json::to_value(
        super::why_line::resolve_core(
            home,
            &crate::domain::Principal::from(1),
            &super::why_line::Params {
                repo: "acme/api".into(),
                rev: Some(rev.into()),
                story: None,
                path: path.into(),
                start,
                end,
            },
            &crate::context_repo::caller_scope::CallerScope::Human,
        )
        .unwrap(),
    )
    .unwrap()
}

/// A Claude-noted commit amended or rebased in an unsupported session reads exactly as it
/// would have without the session's environment, whether or not git copies notes on rewrite.
#[test]
fn rewrites_in_an_unsupported_session_attribute_as_they_would_without_it() {
    let fixture = Launch::new(AgentCli::OpenCode);
    let marked = plan(&fixture, AgentCli::OpenCode).env;
    let fixed = |env: &[(String, String)]| -> Vec<(String, String)> {
        env.iter()
            .cloned()
            .chain(
                ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"]
                    .map(|key| (key.to_owned(), "2026-10-29T12:00:00Z".to_owned())),
            )
            .collect()
    };
    for rewrite_ref in [false, true] {
        let mut seen = Vec::new();
        for env in [fixed(&[]), fixed(&marked)] {
            let repo = rebuild();
            let repo = repo.path();
            if rewrite_ref {
                git_as(repo, &[], &["config", "notes.rewriteRef", "refs/notes/ai"]);
            }
            let subject = git_as(repo, &[], &["log", "-1", "--format=%B"]);
            git_as(
                repo,
                &env,
                &["commit", "-q", "--amend", "-m", subject.trim()],
            );
            let amended = head(repo);
            git_as(repo, &[], &["checkout", "-qb", "side", "HEAD~2"]);
            git_as(repo, &[], &["commit", "-q", "--allow-empty", "-m", "side"]);
            git_as(repo, &[], &["checkout", "-q", "main"]);
            git_as(repo, &env, &["rebase", "-q", "side"]);
            let rebased = head(repo);
            // The fixture's own commits differ per rebuild, so ranges are compared without them.
            let read = |rev: &str, path: &str| {
                let mut ranges = super::git_ai::GitAiSource::default()
                    .ranges(repo, rev, path)
                    .unwrap();
                ranges.iter_mut().for_each(|range| range.commit = None);
                ranges
            };
            seen.push((
                [&amended, &rebased].map(|sha| {
                    (
                        committed_by(repo, sha),
                        fixup_claims(&repo.join(".git"), sha),
                    )
                }),
                read(&amended, "a.txt"),
                read(&rebased, "a.txt"),
                read(&rebased, "b.txt"),
            ));
        }
        assert_eq!(seen[0], seen[1], "rewriteRef {rewrite_ref}");
        let exact = seen[0]
            .2
            .iter()
            .any(|range| range.basis == super::ranges::Basis::Exact);
        assert_eq!(exact, rewrite_ref, "a copied note is still read");
    }
}

/// `why_line` on a commit an unsupported session made, after the fixup ran: unattributed, the
/// CLI and why, never human and never just a story association. The person's own and
/// Claude's lines are unchanged.
#[test]
fn why_line_reports_an_unsupported_session_commit_as_its_cli_never_human() {
    let home = tempfile::tempdir().unwrap();
    let repo = rebuild();
    super::why_line::tests::attach(home.path(), repo.path());
    let env = opencode_session_env();
    let probe = commit_and_fixup(
        repo.path(),
        &env,
        "opencode.txt",
        5,
        "SV-CDE-71: opencode attribution probe",
    );
    let own = commit_and_fixup(repo.path(), &[], "own.txt", 2, "SV-CDE-71: by hand");
    let read =
        |path: &str, rev: &str, start: u32, end: u32| why_line(home.path(), path, rev, start, end);

    let ranges = read("opencode.txt", &probe, 1, 5)["ranges"].clone();
    assert_eq!(ranges.as_array().unwrap().len(), 1, "{ranges}");
    let range = &ranges[0];
    assert_eq!(
        (&range["start"], &range["end"], &range["commit"]),
        (&json!(1), &json!(5), &json!(probe))
    );
    assert_eq!(range["basis"], "unknown");
    assert_eq!(range["tool"], "opencode");
    assert_eq!(range["reason"], REASON);
    assert!(range["story"].is_null() && range.get("candidateSessions").is_none());

    // The person's own commit is still theirs (AC4).
    let mine = &read("own.txt", &own, 1, 2)["ranges"][0];
    assert_eq!(
        (&mine["tool"], &mine["reason"]),
        (&json!("human"), &json!("human"))
    );
    // Fixture notes: Claude's line and the person's line read as before.
    assert_eq!(read("a.txt", "HEAD", 2, 2)["ranges"][0]["basis"], "exact");
    assert_eq!(read("d.txt", "HEAD", 3, 3)["ranges"][0]["reason"], "human");
}
