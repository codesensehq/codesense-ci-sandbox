// SPDX-License-Identifier: MIT
//! Commits made while capture cannot be wired for a CLI are never credited to the person.
//!
//! git-ai's daemon credits the person with every untraced commit it finds in a worktree's
//! `HEAD` reflog as `commit: <subject>` (its untraced-commit fixup), which is right for a
//! commit the person made in their own shell and wrong for one an agent made. A launch whose
//! CLI has no capture sets `GIT_REFLOG_ACTION` for its session, so that session's commits are
//! recorded as `<REFLOG_ACTION> <cli>: <subject>` instead: the fixup leaves them, no note is
//! written, and their lines stay unattributed. [`Reflogs`] reads the CLI back while a reflog
//! still holds the record.
//!
//! The marker is interim, until these CLIs have real capture. Git takes `GIT_REFLOG_ACTION` for
//! every command the session runs, so its checkouts are recorded as `<REFLOG_ACTION> <cli>`
//! rather than `checkout: moving from <a> to <b>`. `git checkout -`, `git switch -` and `@{-N}`
//! read only the latter, so after such a session they go back to the last branch switched to
//! outside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The reflog action an unsupported CLI's session commits under, before the CLI's name.
pub(crate) const REFLOG_ACTION: &str = "codesense-unsupported-cli";

/// The reason a line committed in such a session reports, next to the CLI as its tool.
pub(crate) const REASON: &str = "unsupported_cli";

/// The reason a whole commit of `cli`'s reports where there is no tool to name it in.
pub(crate) fn reason(cli: &str) -> String {
    format!("{REASON}: {cli}")
}

/// What a launch of unsupported `cli` adds to its environment.
pub(crate) fn launch_env(cli: &str) -> Vec<(String, String)> {
    vec![(
        "GIT_REFLOG_ACTION".to_owned(),
        format!("{REFLOG_ACTION} {cli}"),
    )]
}

/// One marked reflog record: the commit it moved from and the CLI and subject it names.
struct Marked {
    old: String,
    cli: String,
    subject: String,
}

/// The marked records of one repo's reflogs, read once for a whole read of its attribution:
/// each worktree's `HEAD` and every branch, by the commit each record moved to.
#[derive(Default)]
pub(crate) struct Reflogs(HashMap<String, Vec<Marked>>);

impl Reflogs {
    pub(crate) fn read(repo: &Path) -> Self {
        let Some(dirs) = crate::commands::context_git_watch::GitDirs::resolve(repo) else {
            return Self::default();
        };
        count_pass(&dirs.common_dir);
        Self(marked_records(&dirs.common_dir))
    }

    /// The unsupported CLI whose session made `commit` in `repo`: a record whose subject is
    /// the commit's and whose previous value is the commit's parent. An amend, a rebase pick
    /// or a fast-forward moves from some other commit, so it never names the CLI. A
    /// cherry-pick, revert or `git am` in the session does: those commits are the session's,
    /// though the lines may have been written elsewhere, and they are never the person's.
    pub(crate) fn committed_by(&self, repo: &Path, commit: &str) -> Option<String> {
        let records = HashMap::get(&self.0, commit)?;
        let repo = Result::ok(git2::Repository::open(repo))?;
        let id = Result::ok(git2::Oid::from_str(commit))?;
        let found = Result::ok(git2::Repository::find_commit(&repo, id))?;
        let message = String::from_utf8_lossy(git2::Commit::message_bytes(&found)).into_owned();
        let subject = squeezed(message.lines().next().unwrap_or_default());
        let parent = Result::ok(git2::Commit::parent_id(&found, 0)).map(|id| id.to_string());
        let created = |record: &&Marked| {
            record.subject == subject
                && match &parent {
                    Some(parent) => record.old == *parent,
                    None => Iterator::all(&mut str::bytes(&record.old), |b| b == b'0'),
                }
        };
        Iterator::find(&mut <[Marked]>::iter(records), created).map(|record| record.cli.clone())
    }
}

/// A subject as git writes it into a reflog: every run of whitespace one space, none at
/// either end.
fn squeezed(subject: &str) -> String {
    let words: Vec<&str> = Iterator::collect(str::split_whitespace(subject));
    <[&str]>::join(&words, " ")
}

/// Tests count each read of a common dir's reflogs.
#[cfg(not(test))]
fn count_pass(_common_dir: &Path) {}

#[cfg(test)]
fn count_pass(common_dir: &Path) {
    *HashMap::entry(&mut *passes::lock(), PathBuf::from(common_dir)).or_default() += 1;
}

/// How many times each common dir's reflogs were read.
#[cfg(test)]
pub(crate) mod passes {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex, MutexGuard};

    static PASSES: LazyLock<Mutex<HashMap<PathBuf, usize>>> = LazyLock::new(Default::default);

    pub(super) fn lock() -> MutexGuard<'static, HashMap<PathBuf, usize>> {
        PASSES.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn of(repo: &Path) -> usize {
        let dirs = crate::commands::context_git_watch::GitDirs::resolve(repo).unwrap();
        lock().get(&dirs.common_dir).copied().unwrap_or(0)
    }
}

/// Every marked record in the reflogs under `common_dir`, by the commit it moved to.
fn marked_records(common_dir: &Path) -> HashMap<String, Vec<Marked>> {
    let mut files = vec![common_dir.join("logs/HEAD")];
    if let Ok(worktrees) = std::fs::read_dir(common_dir.join("worktrees")) {
        files.extend(
            worktrees
                .flatten()
                .map(|entry| std::fs::DirEntry::path(&entry).join("logs/HEAD")),
        );
    }
    branch_logs(&common_dir.join("logs/refs/heads"), &mut files);
    let needle = format!("\t{REFLOG_ACTION} ");
    let mut marked: HashMap<String, Vec<Marked>> = HashMap::new();
    for file in files {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        if !text.contains(&needle) {
            continue;
        }
        for line in text.lines() {
            if let Some((new, record)) = parse(line) {
                marked.entry(new).or_default().push(record);
            }
        }
    }
    marked
}

fn branch_logs(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = std::fs::DirEntry::path(&entry);
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => branch_logs(&path, files),
            Ok(kind) if kind.is_file() => files.push(path),
            _ => {}
        }
    }
}

/// `<old> <new> <identity> <time> <zone>\t<REFLOG_ACTION> <cli>: <subject>`, as `(new, record)`.
fn parse(line: &str) -> Option<(String, Marked)> {
    let (header, message) = line.split_once('\t')?;
    let mut ids = header.split(' ');
    let (old, new) = (ids.next()?, ids.next()?);
    let (cli, subject) = message
        .strip_prefix(REFLOG_ACTION)?
        .strip_prefix(' ')?
        .split_once(": ")?;
    let named = !str::is_empty(cli)
        && Iterator::all(&mut str::bytes(cli), |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
        });
    named.then(|| {
        (
            new.to_owned(),
            Marked {
                old: old.to_owned(),
                cli: cli.to_owned(),
                subject: subject.to_owned(),
            },
        )
    })
}
