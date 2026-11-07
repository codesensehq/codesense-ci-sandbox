// SPDX-License-Identifier: MIT
//! Read-only diff of a managed worktree against its fork point, so the desktop's
//! source diff reviews a remote workspace the way it reviews a local one.
//!
//! The desktop computes the local diff with libgit2; this side has only the
//! Git CLI, so the classification is rebuilt from `git diff --raw/--numstat`
//! plus the untracked listing.
//!
//! # The baseline rule
//!
//! One rule decides what a remote diff is taken against ([`resolve_baseline`]).
//! It is the desktop's (`resolve_source_baseline_tree` in `git_sync.rs`) with
//! one extra input, the base the workspace stands on (its *prep fork*: the
//! commit it was prepared from, or the base a base update moved it to), which
//! is always present on this host while a remote clone often has no local
//! `main`. It departs from the desktop in three named places: the prep fork
//! (rule 4), skipping rungs that are the branch itself (rule 3), and an unborn
//! branch diffing against the empty tree (rule 5). A named ref is limited to
//! the characters `valid_spec` accepts; any other reads as unresolved:
//!
//! 1. Base ref `HEAD`: the baseline is `HEAD` (uncommitted work only).
//! 2. A base ref the caller names that forks with `HEAD`: its fork point,
//!    exactly as on the desktop. A ref that does not resolve is skipped.
//! 3. Otherwise the *ladder fork*: the desktop's default ladder, in its order,
//!    `origin/main`, `main`, `@{upstream}`, `origin/HEAD`; the first rung that
//!    forks with `HEAD` wins. A rung that is the checked-out branch itself or
//!    a remote copy under the same name (a workspace branch named `main`,
//!    `git push -u origin <branch>`) is skipped: its fork point is the
//!    branch's own tip, which would hide committed work.
//! 4. The ladder fork beats the prep fork only when it is strictly newer (the
//!    prep fork is its proper ancestor): the branch merged, rebased or
//!    fast-forwarded onto a newer base, whose commits are not its work. A
//!    stale `origin/main` older than the prep fork loses.
//!    For the `@{upstream}` rung it must also have been reached by a merge
//!    (off `HEAD`'s first-parent line): an upstream under another name
//!    (`git push -u origin HEAD:review/x`) is usually the branch's own pushed
//!    work, and committing on top of it must not hide that work.
//! 5. Neither resolves: `HEAD`, so the diff is its uncommitted work; an
//!    unborn branch diffs against the empty tree (everything added).
//!
//! When a case is ambiguous the rule shows more rather than hiding work: a
//! branch named `main` that pulls upstream lists the pulled commits, since its
//! `origin/main` may equally be its own pushed work. `baseline_rule_cases` in
//! `files_tests.rs` pins every case above in one table.
use super::files::{observe_path, observe_path_for_view, relative_path, stat_path};
use super::git_output;
use codesense_pty_core::remote_files::{
    ChangeStatus, ChangedFile, FileReply, Observation, MAX_FILE_BYTES, MAX_PATHS,
};
use std::collections::HashMap;
use std::fs::File;
use std::time::{Duration, Instant};

/// Git listings (raw, numstat, untracked) are bounded like enumeration.
const LIST_BYTES: usize = 8 * 1024 * 1024;
/// Short answers (an object name, one ls-tree entry).
const SHORT_BYTES: usize = 64 * 1024;
/// A diff is interactive: give up rather than hold the operations lock long.
/// One budget covers the whole operation, every Git call in it.
const DIFF_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
thread_local! {
    /// Test seams: a shorter deadline and a smaller reply budget, so the
    /// public `changes`/`redline` paths can be driven into a mid-call kill or
    /// an oversized reply without a huge worktree.
    pub(crate) static TEST_DEADLINE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
    pub(crate) static TEST_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn diff_deadline() -> Instant {
    #[cfg(test)]
    if let Some(d) = TEST_DEADLINE.with(std::cell::Cell::get) {
        return Instant::now() + d;
    }
    Instant::now() + DIFF_DEADLINE
}

fn reply_budget() -> usize {
    #[cfg(test)]
    if let Some(b) = TEST_BUDGET.with(std::cell::Cell::get) {
        return b;
    }
    REPLY_BUDGET
}
/// Git's own binary heuristic looks for a NUL in the first 8000 bytes.
const BINARY_SNIFF_BYTES: usize = 8000;

fn git_text(
    root: &File,
    args: &[&str],
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<String, String> {
    let bytes = git_output(root, args, alive, deadline, SHORT_BYTES)?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_string())
        .map_err(|_| "invalid Git output".into())
}

/// A ref the caller may name: a branch, remote-tracking ref, `HEAD`-relative
/// spec or object name. Anything that could read as an option or carries
/// whitespace is refused rather than passed to Git.
fn valid_spec(spec: &str) -> bool {
    !spec.is_empty()
        && spec.len() <= 256
        && !spec.starts_with('-')
        && spec
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-@{}^~".contains(c))
}

/// The baseline a diff is taken against, and the `HEAD` it was resolved from.
pub(super) struct Baseline {
    /// A commit, or the empty tree for an unborn branch.
    pub(super) oid: String,
    /// `HEAD`'s commit, empty on an unborn branch.
    pub(super) head: String,
}

/// Whether ladder rung `full_name` (a full ref name) is the checked-out branch
/// itself or a remote-tracking copy of it (`git push -u origin <branch>`): its
/// fork point is the branch's own work, never where it forked. The copy must
/// carry the branch's whole name after `<remote>/`: `origin/hot/fix` is not a
/// copy of branch `fix`, nor `origin/release/main` of `main`.
fn names_the_branch(full_name: &str, branch: &str) -> bool {
    let Some(short) = branch.strip_prefix("refs/heads/") else {
        return false;
    };
    full_name == branch
        || full_name
            .strip_prefix("refs/remotes/")
            .and_then(|remote| remote.split_once('/'))
            .is_some_and(|(_, name)| name == short)
}

/// The default ladder, in the desktop's order. A base rung names where the
/// branch's base is; the upstream rung may instead name its own push target.
const LADDER: [(&str, Rung); 4] = [
    ("origin/main", Rung::Base),
    ("main", Rung::Base),
    ("@{upstream}", Rung::Upstream),
    ("origin/HEAD", Rung::Base),
];

#[derive(Clone, Copy, PartialEq)]
enum Rung {
    Base,
    Upstream,
}

/// The baseline commit (or the empty tree for an unborn branch) the diff is
/// taken against, by the module's baseline rule.
pub(super) fn resolve_baseline(
    root: &File,
    base_commit: Option<&str>,
    base_ref: Option<&str>,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<Baseline, String> {
    let git = |args: &[&str]| git_text(root, args, alive, deadline);
    let head = git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
        .ok()
        .filter(|oid| !oid.is_empty());
    let Some(head) = head else {
        super::out_of_time(alive, deadline)?;
        // Rule 5, unborn: everything in the worktree reads as added.
        return Ok(Baseline {
            oid: git(&["hash-object", "-t", "tree", "--stdin"])?,
            head: String::new(),
        });
    };
    let found = |oid: String| Baseline {
        oid,
        head: head.clone(),
    };
    // Rule 1.
    if base_ref == Some("HEAD") {
        return Ok(found(head.clone()));
    }
    let fork_point = |spec: &str| {
        git(&["merge-base", "HEAD", spec])
            .ok()
            .filter(|base| !base.is_empty())
    };
    // Rule 2.
    if let Some(base) = base_ref.filter(|r| valid_spec(r)).and_then(fork_point) {
        return Ok(found(base));
    }
    // Rule 3.
    let branch = git(&["symbolic-ref", "--quiet", "HEAD"]).ok();
    let is_the_branch = |rung: &str| {
        branch.as_deref().is_some_and(|branch| {
            git(&["rev-parse", "--symbolic-full-name", rung])
                .is_ok_and(|full| names_the_branch(&full, branch))
        })
    };
    let ladder = LADDER
        .into_iter()
        .filter(|(rung, _)| !is_the_branch(rung))
        .find_map(|(rung, kind)| fork_point(rung).map(|fork| (kind, fork)));
    let prepared = base_commit.filter(|c| valid_spec(c)).and_then(fork_point);
    Ok(found(match (prepared, ladder) {
        // Rule 4.
        (Some(prepared), Some((kind, ladder))) => {
            let strictly_newer = prepared != ladder
                && git(&["merge-base", "--is-ancestor", &prepared, &ladder]).is_ok();
            let reached_by_a_merge = || {
                kind == Rung::Base || !on_first_parent_line(root, &ladder, &head, alive, deadline)
            };
            if strictly_newer && reached_by_a_merge() {
                ladder
            } else {
                prepared
            }
        }
        (Some(base), None) | (None, Some((_, base))) => base,
        // Rule 5.
        (None, None) => head.clone(),
    }))
}

/// Whether `fork` lies on `HEAD`'s first-parent line (or is `HEAD`): the
/// branch reached it by committing on top of it (a push target it went on
/// from, or a fast-forward/rebase onto it), not by merging it in. A merged-in
/// upstream sits off that line (a merge's second parent). Unknown reads as
/// on the line, which keeps the prep fork rather than risk hiding work.
fn on_first_parent_line(
    root: &File,
    fork: &str,
    head: &str,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> bool {
    if fork == head {
        return true;
    }
    let range = format!("{fork}..{head}");
    let Ok(listing) = git_output(
        root,
        &["rev-list", "--first-parent", &range],
        alive,
        deadline,
        LIST_BYTES,
    ) else {
        // Unknown: keep the preparation commit rather than risk hiding work.
        return true;
    };
    let listing = String::from_utf8_lossy(&listing);
    let Some(oldest) = listing.split('\n').map(str::trim).rfind(|l| !l.is_empty()) else {
        return true;
    };
    git_text(
        root,
        &["rev-parse", &format!("{oldest}^1")],
        alive,
        deadline,
    )
    .map_or(true, |parent| parent == fork)
}

/// NUL-separated tokens; a token that is not UTF-8 (a path this protocol
/// cannot name) reads as `None`, so one odd file name never fails a listing
/// and paired records stay aligned.
fn split_z(bytes: &[u8]) -> Vec<Option<&str>> {
    bytes
        .split(|b| *b == 0)
        .filter(|t| !t.is_empty())
        .map(|t| std::str::from_utf8(t).ok())
        .collect()
}

struct RawEntry {
    path: String,
    old_path: Option<String>,
    status: ChangeStatus,
    mode_changed: bool,
}

/// Parse `git diff --raw -z`: a `:<src> <dst> <srcsha> <dstsha> <status>`
/// header, then one path (two for a rename or copy).
fn parse_raw(bytes: &[u8]) -> Result<Vec<RawEntry>, String> {
    let tokens = split_z(bytes);
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let header = tokens[i]
            .and_then(|t| t.strip_prefix(':'))
            .ok_or("unexpected Git raw diff output")?;
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.len() < 5 {
            return Err("unexpected Git raw diff header".into());
        }
        let (src_mode, dst_mode, status) = (fields[0], fields[1], fields[4]);
        let letter = status.chars().next().ok_or("empty Git diff status")?;
        let paired = matches!(letter, 'R' | 'C');
        let first = *tokens.get(i + 1).ok_or("truncated Git raw diff")?;
        let (path, old_path) = if paired {
            let second = *tokens.get(i + 2).ok_or("truncated Git raw diff")?;
            i += 3;
            (second, first)
        } else {
            i += 2;
            (first, None)
        };
        let absent = |mode: &str| mode.bytes().all(|b| b == b'0');
        // A path that is not UTF-8 cannot be opened through this protocol.
        let Some(path) = path.map(str::to_string) else {
            // A rename onto an unreadable name still removed its readable source.
            if letter == 'R' {
                if let Some(old) = old_path {
                    out.push(RawEntry {
                        path: old.to_string(),
                        old_path: None,
                        status: ChangeStatus::Deleted,
                        mode_changed: false,
                    });
                }
            }
            continue;
        };
        let old_path = old_path.map(str::to_string);
        let status = match letter {
            'A' | 'C' => ChangeStatus::Added,
            'D' => ChangeStatus::Deleted,
            // A rename whose source name can't be read has no baseline to pair
            // with: it reads as an add, as its redline does.
            'R' if old_path.is_none() => ChangeStatus::Added,
            _ => ChangeStatus::Modified,
        };
        out.push(RawEntry {
            path,
            // A copy reads as a plain add, as on the desktop (copies are off there).
            old_path: if letter == 'R' { old_path } else { None },
            status,
            mode_changed: !absent(src_mode) && !absent(dst_mode) && src_mode != dst_mode,
        });
    }
    Ok(out)
}

/// Parse `git diff --numstat -z` into `path → (added, removed)`; a binary
/// change (`-\t-`) maps to `None`.
fn parse_numstat(bytes: &[u8]) -> Result<HashMap<String, Option<(u32, u32)>>, String> {
    let tokens = split_z(bytes);
    let mut out = HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        let Some(token) = tokens[i] else {
            // A non-UTF-8 single path record: skip it.
            i += 1;
            continue;
        };
        let mut parts = token.splitn(3, '\t');
        let added = parts.next().ok_or("unexpected Git numstat output")?;
        let removed = parts.next().ok_or("unexpected Git numstat output")?;
        let rest = parts.next().unwrap_or("");
        let path = if rest.is_empty() {
            // A rename: the two paths follow as their own tokens.
            let dst = *tokens.get(i + 2).ok_or("truncated Git numstat")?;
            i += 3;
            let Some(dst) = dst else { continue };
            dst.to_string()
        } else {
            i += 1;
            rest.to_string()
        };
        let counts = match (added.parse::<u32>(), removed.parse::<u32>()) {
            (Ok(a), Ok(r)) => Some((a, r)),
            _ => None,
        };
        out.insert(path, counts);
    }
    Ok(out)
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

/// Lines in a new text file, counted the way Git's numstat does (a final
/// line without a newline still counts).
fn count_lines(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    let lines = newlines + usize::from(!bytes.ends_with(b"\n"));
    u32::try_from(lines).unwrap_or(u32::MAX)
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash = (*hash ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
    }
    *hash = (*hash ^ 0x1f).wrapping_mul(0x100_0000_01b3);
}

/// The diff only reads, so a Git call killed mid-run left nothing to
/// reconcile: under live authority it was a slow Git (a retryable timeout),
/// otherwise the authority retired (access loss), as `out_of_time` words it.
fn reworded(
    result: Result<FileReply, String>,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    result.map_err(|e| {
        if e != super::GIT_INTERRUPTED {
            e
        } else if alive() {
            super::GIT_TIMED_OUT.to_string()
        } else {
            super::AUTHORITY_RETIRED.to_string()
        }
    })
}

pub(crate) fn changes(
    root: &File,
    base_commit: Option<&str>,
    base_ref: Option<&str>,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    reworded(changes_inner(root, base_commit, base_ref, alive), alive)
}

fn changes_inner(
    root: &File,
    base_commit: Option<&str>,
    base_ref: Option<&str>,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    let deadline = diff_deadline();
    let Baseline {
        oid: baseline,
        head,
    } = resolve_baseline(root, base_commit, base_ref, alive, deadline)?;
    let diff_args = |format: &'static str| {
        [
            "--no-optional-locks",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-M",
            "-z",
            format,
        ]
    };
    let mut raw_args = diff_args("--raw").to_vec();
    raw_args.extend([baseline.as_str(), "--"]);
    let raw = parse_raw(&git_output(root, &raw_args, alive, deadline, LIST_BYTES)?)?;
    let mut stat_args = diff_args("--numstat").to_vec();
    stat_args.extend([baseline.as_str(), "--"]);
    let stats = parse_numstat(&git_output(root, &stat_args, alive, deadline, LIST_BYTES)?)?;

    let mut files: Vec<ChangedFile> = raw
        .into_iter()
        .map(|entry| {
            let counts = stats.get(&entry.path).copied().flatten();
            ChangedFile {
                added: counts.map(|(a, _)| a),
                removed: counts.map(|(_, r)| r),
                path: entry.path,
                status: entry.status,
                old_path: entry.old_path,
                mode_changed: entry.mode_changed,
            }
        })
        .collect();

    let untracked = git_output(
        root,
        &[
            "--no-optional-locks",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ],
        alive,
        deadline,
        LIST_BYTES,
    )?;
    let untracked = split_z(&untracked);
    // Too many paths for any reply: refuse before reading each new file. A
    // path already listed (or one `untracked_files` skips) adds no record.
    let listed: std::collections::HashSet<&str> = files
        .iter()
        .flat_map(|f| std::iter::once(f.path.as_str()).chain(f.old_path.as_deref()))
        .collect();
    let fresh = untracked
        .iter()
        .flatten()
        .filter(|p| relative_path(p).is_ok() && !listed.contains(**p))
        .count();
    if files.len().saturating_add(fresh) > MAX_PATHS {
        return Err("change list exceeds bounded result limit".into());
    }
    let (added, counted_all) = untracked_files(root, &files, untracked, alive, deadline)?;
    // Whether an untracked file got counted depends on where the deadline
    // fell, so its counts stay out of the stamp (its size and mtime are in).
    let uncounted_in_stamp: std::collections::HashSet<String> =
        added.iter().map(|f| f.path.clone()).collect();
    files.extend(added);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let stamp = change_stamp(
        root,
        &baseline,
        &head,
        &files,
        &uncounted_in_stamp,
        counted_all,
    );
    let reply = FileReply::Changes {
        baseline,
        files,
        stamp,
    };
    within_frame(&reply)?;
    Ok(reply)
}

/// The most of a frame a Changes reply may take, leaving room for the
/// envelope around it.
const REPLY_BUDGET: usize = codesense_host_proto::frame::MAX_FRAME_BYTES - 1024 * 1024;

/// A reply too large for one frame would fail the connection's writer, and
/// with it every session on the connection (and the watcher would retry it
/// every tick). Refuse it as a bounded read instead; the desktop reads
/// "exceeds" as "the change list is too large".
fn within_frame(reply: &FileReply) -> Result<(), String> {
    let FileReply::Changes { files, .. } = reply else {
        return Ok(());
    };
    if files.len() > MAX_PATHS {
        return Err("change list exceeds bounded result limit".into());
    }
    // An upper estimate of the encoded size: each record's escaped strings
    // plus a fixed allowance for its field names, numbers and punctuation.
    let escaped = |text: &str| -> usize {
        text.bytes()
            .map(|b| match b {
                b'"' | b'\\' => 2,
                0..=0x1f => 6,
                _ => 1,
            })
            .sum::<usize>()
            + 2
    };
    let size: usize = files
        .iter()
        .map(|f| escaped(&f.path) + f.old_path.as_deref().map_or(0, escaped) + 160)
        .sum();
    if size > reply_budget() {
        return Err("change list exceeds bounded result limit".into());
    }
    Ok(())
}

/// The watch signal: the baseline, the HEAD it was resolved from, every
/// listed record, and each file's size + mtime, so an edit that keeps line
/// counts still registers. Untracked counts stay out (`uncounted`), but
/// whether the deadline cut their counting short is in (`counted_all`), so
/// the complete listing that follows a cut-short one reads as a change and
/// the missing counts get fetched.
fn change_stamp(
    root: &File,
    baseline: &str,
    head: &str,
    files: &[ChangedFile],
    uncounted: &std::collections::HashSet<String>,
    counted_all: bool,
) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    fnv(&mut hash, &[u8::from(counted_all)]);
    fnv(&mut hash, baseline.as_bytes());
    fnv(&mut hash, head.as_bytes());
    for file in files {
        fnv(&mut hash, file.path.as_bytes());
        let counts = if uncounted.contains(&file.path) {
            (None, None)
        } else {
            (file.added, file.removed)
        };
        fnv(
            &mut hash,
            format!(
                "{:?}{:?}{:?}{:?}{}",
                file.status, counts.0, counts.1, file.old_path, file.mode_changed
            )
            .as_bytes(),
        );
        if let Some((len, mtime_ns)) = stat_path(root, &file.path) {
            fnv(&mut hash, &len.to_le_bytes());
            fnv(&mut hash, &mtime_ns.to_le_bytes());
        }
    }
    format!("{hash:016x}")
}

/// The untracked paths not already listed, each counted as a new file.
///
/// A path untracked since the baseline (`git rm --cached`), or the source of a
/// rename that has a new untracked file at its old name (`git mv a b; touch
/// a`), is already listed by the raw diff. The desktop's libgit2 diff lets the
/// baseline-to-index delta win there and lists the path once, so this does too
/// (and the review never shows two rows for one path).
///
/// Each file is read, so the operation's deadline and authority are checked
/// per file. Past the deadline the rest are listed uncounted (as an unreadable
/// file is), so a worktree full of untracked files still gets its Changes
/// without holding the worktree lock long; retired authority fails it. The
/// flag says whether the deadline left every file a chance to be counted.
fn untracked_files(
    root: &File,
    listed: &[ChangedFile],
    untracked: Vec<Option<&str>>,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(Vec<ChangedFile>, bool), String> {
    let listed: std::collections::HashSet<&str> = listed
        .iter()
        .flat_map(|f| std::iter::once(f.path.as_str()).chain(f.old_path.as_deref()))
        .collect();
    let mut out = Vec::new();
    let mut counting = true;
    for path in untracked.into_iter().flatten() {
        if relative_path(path).is_err() || listed.contains(path) {
            continue;
        }
        if !alive() {
            return Err(super::AUTHORITY_RETIRED.into());
        }
        counting = counting && Instant::now() < deadline;
        // An unreadable or oversized new file is still listed, just uncounted.
        let added = match counting.then(|| observe_path(root, path)) {
            Some(Ok((Observation::Regular { .. }, bytes))) if !looks_binary(&bytes) => {
                Some(count_lines(&bytes))
            }
            _ => None,
        };
        out.push(ChangedFile {
            path: path.to_string(),
            status: ChangeStatus::Added,
            old_path: None,
            added,
            removed: added.map(|_| 0),
            mode_changed: false,
        });
    }
    Ok((out, counting))
}

/// What the baseline holds at a path.
#[derive(Clone, Copy, PartialEq)]
enum BaselineEntry {
    /// A regular file's bytes.
    File,
    /// A symlink or other non-file entry: present, but not content.
    Opaque,
    /// A submodule's commit: the worktree holds a directory there.
    Gitlink,
}

/// The baseline side of one path, `None` when absent.
fn baseline_side(
    root: &File,
    baseline: &str,
    path: &str,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<Option<(Vec<u8>, BaselineEntry)>, String> {
    let listing = git_output(
        root,
        &[
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--full-tree",
            baseline,
            "--",
            path,
        ],
        alive,
        deadline,
        SHORT_BYTES,
    )?;
    let Some(entry) = split_z(&listing).into_iter().flatten().find(|entry| {
        entry
            .split_once('\t')
            .is_some_and(|(_, entry_path)| entry_path == path)
    }) else {
        return Ok(None);
    };
    let meta: Vec<&str> = entry
        .split_once('\t')
        .map(|(meta, _)| meta.split(' ').collect())
        .unwrap_or_default();
    match meta.as_slice() {
        // A symlink's target or a submodule's commit is not file content.
        [mode, "blob", _] if *mode == "120000" => Ok(Some((Vec::new(), BaselineEntry::Opaque))),
        [_, "commit", _] => Ok(Some((Vec::new(), BaselineEntry::Gitlink))),
        [_, "blob", oid] => {
            let bytes = git_output(
                root,
                &["cat-file", "blob", oid],
                alive,
                deadline,
                MAX_FILE_BYTES,
            )
            .map_err(|e| {
                if e.contains("exceeds") {
                    "baseline file exceeds bounded read limit".to_string()
                } else {
                    e
                }
            })?;
            Ok(Some((bytes, BaselineEntry::File)))
        }
        [_, "tree", _] => Ok(None),
        _ => Ok(Some((Vec::new(), BaselineEntry::Opaque))),
    }
}

pub(crate) fn redline(
    root: &File,
    base_commit: Option<&str>,
    path: &str,
    old_path: Option<&str>,
    base_ref: Option<&str>,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    reworded(
        redline_inner(root, base_commit, path, old_path, base_ref, alive),
        alive,
    )
}

fn redline_inner(
    root: &File,
    base_commit: Option<&str>,
    path: &str,
    old_path: Option<&str>,
    base_ref: Option<&str>,
    alive: &dyn Fn() -> bool,
) -> Result<FileReply, String> {
    relative_path(path)?;
    let old_path = old_path.filter(|p| !p.is_empty());
    if let Some(old) = old_path {
        relative_path(old)?;
    }
    let deadline = diff_deadline();
    let baseline = resolve_baseline(root, base_commit, base_ref, alive, deadline)?.oid;
    let before = baseline_side(root, &baseline, old_path.unwrap_or(path), alive, deadline)?;
    let entry = before.as_ref().map(|(_, entry)| *entry);
    let before_opaque = entry.is_some_and(|e| e != BaselineEntry::File);
    // A deleted file whose parent is now a file or symlink is simply gone, as
    // is a deleted file or link with a directory now at its name (Git lists
    // it deleted and the directory's files as untracked). A directory over a
    // submodule, or with no baseline at all, is that entry itself: opaque.
    let (after, after_opaque) = match observe_path_for_view(root, path)? {
        (Observation::Absent, _) => (None, false),
        (Observation::Directory, _)
            if matches!(entry, Some(BaselineEntry::File | BaselineEntry::Opaque)) =>
        {
            (None, false)
        }
        (Observation::Regular { .. }, bytes) => (Some(bytes), false),
        (Observation::Directory | Observation::Unsafe, _) => (Some(Vec::new()), true),
    };
    Ok(FileReply::Redline {
        before: before.map(|(bytes, _)| bytes),
        after,
        opaque: before_opaque || after_opaque,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_parse_pairs_renames_and_flags_mode_changes() {
        let raw = b":100644 100644 aaaa bbbb M\0src/a.rs\0:100644 100755 aaaa aaaa M\0run.sh\0:100644 100644 aaaa bbbb R087\0old.md\0new.md\0:000000 100644 0000 bbbb A\0added.txt\0:100644 000000 aaaa 0000 D\0gone.txt\0";
        let entries = parse_raw(raw).unwrap();
        let summary: Vec<_> = entries
            .iter()
            .map(|e| {
                (
                    e.path.as_str(),
                    e.status,
                    e.old_path.as_deref(),
                    e.mode_changed,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/a.rs", ChangeStatus::Modified, None, false),
                ("run.sh", ChangeStatus::Modified, None, true),
                ("new.md", ChangeStatus::Modified, Some("old.md"), false),
                ("added.txt", ChangeStatus::Added, None, false),
                ("gone.txt", ChangeStatus::Deleted, None, false),
            ]
        );
    }

    #[test]
    fn numstat_parse_keys_renames_by_new_path_and_marks_binary() {
        let stats =
            parse_numstat(b"3\t1\tsrc/a.rs\0-\t-\timg.png\x002\t0\t\0old.md\0new.md\0").unwrap();
        assert_eq!(stats["src/a.rs"], Some((3, 1)));
        assert_eq!(stats["img.png"], None);
        assert_eq!(stats["new.md"], Some((2, 0)));
    }

    #[test]
    fn a_non_utf8_path_is_skipped_without_failing_or_misaligning_the_listing() {
        let raw = b":100644 100644 aaaa bbbb M\0bad\xff.txt\0:100644 100644 aaaa bbbb R090\0old\xfe\0new.md\0:100644 100644 aaaa bbbb R095\0kept.md\0gone\xfd\0:100644 100644 aaaa bbbb M\0ok.rs\0";
        let entries = parse_raw(raw).unwrap();
        let paths: Vec<_> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.old_path.as_deref(), e.status))
            .collect();
        assert_eq!(
            paths,
            vec![
                // Renamed from an unreadable name: no baseline, so an add.
                ("new.md", None, ChangeStatus::Added),
                // Renamed onto an unreadable name: its readable source is gone.
                ("kept.md", None, ChangeStatus::Deleted),
                ("ok.rs", None, ChangeStatus::Modified),
            ]
        );
        let stats = parse_numstat(b"1\t1\tbad\xff.txt\x003\t1\tok.rs\0").unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats["ok.rs"], Some((3, 1)));
    }

    #[test]
    fn line_count_matches_numstat_for_a_missing_final_newline() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a\nb\n"), 2);
        assert_eq!(count_lines(b"a\nb"), 2);
    }

    #[test]
    fn caller_specs_that_could_read_as_options_are_refused() {
        assert!(valid_spec("origin/main"));
        assert!(valid_spec("HEAD~2"));
        assert!(valid_spec("feat/x@{upstream}"));
        assert!(!valid_spec("--output=/tmp/x"));
        assert!(!valid_spec("main; rm"));
        assert!(!valid_spec(""));
    }

    #[test]
    fn untracked_counting_stops_at_the_deadline_and_on_retired_authority() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("new.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.path().join("more.txt"), "c\n").unwrap();
        let root = File::open(dir.path()).unwrap();
        let later = Instant::now() + Duration::from_secs(30);
        let (listed, counted_all) =
            untracked_files(&root, &[], vec![Some("new.txt")], &|| true, later).unwrap();
        assert!(counted_all);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].added, Some(2));
        // Past the deadline every path is still listed, just uncounted.
        let (late, counted_all) = untracked_files(
            &root,
            &[],
            vec![Some("new.txt"), Some("more.txt")],
            &|| true,
            Instant::now(),
        )
        .unwrap();
        assert!(!counted_all);
        let late: Vec<_> = late.iter().map(|f| (f.path.as_str(), f.added)).collect();
        assert_eq!(late, vec![("new.txt", None), ("more.txt", None)]);
        assert!(untracked_files(&root, &[], vec![Some("new.txt")], &|| false, later).is_err());
        // Nothing left to read: nothing to time out.
        assert!(untracked_files(&root, &[], vec![], &|| false, Instant::now()).is_ok());
    }

    #[test]
    fn a_complete_count_after_a_cut_short_one_moves_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("new.txt"), "a\nb\n").unwrap();
        let root = File::open(dir.path()).unwrap();
        let later = Instant::now() + Duration::from_secs(30);
        let stamp_of = |deadline| {
            let (files, counted_all) =
                untracked_files(&root, &[], vec![Some("new.txt")], &|| true, deadline).unwrap();
            let uncounted = files.iter().map(|f| f.path.clone()).collect();
            change_stamp(&root, "base", "head", &files, &uncounted, counted_all)
        };
        let cut_short = stamp_of(Instant::now());
        let complete = stamp_of(later);
        assert_ne!(cut_short, complete, "the missing counts must be refetched");
        assert_eq!(
            complete,
            stamp_of(later),
            "a quiet worktree keeps its stamp"
        );
    }

    #[test]
    fn a_change_list_too_large_for_one_frame_is_a_bounded_error() {
        let file = |i: usize| ChangedFile {
            path: format!("node_modules/{}/{i:08}.js", "p".repeat(150)),
            status: ChangeStatus::Added,
            old_path: None,
            added: Some(1),
            removed: Some(0),
            mode_changed: false,
        };
        let reply = |n: usize| FileReply::Changes {
            baseline: "b".into(),
            files: (0..n).map(file).collect(),
            stamp: "s".into(),
        };
        assert!(within_frame(&reply(10)).is_ok());
        let err = within_frame(&reply(MAX_PATHS)).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn a_diff_killed_mid_call_reads_as_a_timeout_under_live_authority() {
        let interrupted = || Err(crate::remote_workspace::GIT_INTERRUPTED.to_string());
        assert_eq!(
            reworded(interrupted(), &|| true).unwrap_err(),
            crate::remote_workspace::GIT_TIMED_OUT
        );
        assert_eq!(
            reworded(interrupted(), &|| false).unwrap_err(),
            crate::remote_workspace::AUTHORITY_RETIRED
        );
        // Any other failure passes through untouched.
        assert_eq!(
            reworded(Err("Git refused".into()), &|| true).unwrap_err(),
            "Git refused"
        );
    }

    #[test]
    fn a_rung_naming_the_branch_or_its_pushed_copy_is_not_a_fork_point() {
        let branch = "refs/heads/feat/x";
        assert!(names_the_branch("refs/heads/feat/x", branch));
        assert!(names_the_branch("refs/remotes/origin/feat/x", branch));
        assert!(!names_the_branch("refs/remotes/origin/main", branch));
        assert!(!names_the_branch("refs/heads/main", branch));
        assert!(names_the_branch(
            "refs/remotes/origin/main",
            "refs/heads/main"
        ));
        assert!(!names_the_branch("refs/remotes/origin/main", "detached"));
        // Only the whole name after the remote is a copy of the branch.
        assert!(!names_the_branch(
            "refs/remotes/origin/hot/fix",
            "refs/heads/fix"
        ));
        assert!(!names_the_branch(
            "refs/remotes/origin/release/main",
            "refs/heads/main"
        ));
        assert!(names_the_branch(
            "refs/remotes/upstream/fix",
            "refs/heads/fix"
        ));
    }
}
