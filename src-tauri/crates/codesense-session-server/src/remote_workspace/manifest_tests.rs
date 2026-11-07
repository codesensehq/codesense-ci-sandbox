// SPDX-License-Identifier: MIT
use super::*;
use crate::remote_workspace::tests::{fixture, prepared};
use codesense_pty_core::remote_files::FileTarget;

struct Fixture {
    root: tempfile::TempDir,
    target: FileTarget,
    cwd: std::path::PathBuf,
}
fn workspace() -> Fixture {
    let (root, request) = fixture();
    let result = prepared(root.path(), request.clone());
    assert_eq!(result.status, "prepared", "{result:?}");
    Fixture {
        target: FileTarget {
            owner_id: 41,
            origin_key: request.origin_key,
            request_id: request.request_id,
            handle: result.handle.unwrap(),
        },
        cwd: request.worktree_path.into(),
        root,
    }
}
struct Answer {
    reply: ManifestReply,
    parts: Vec<ManifestPart>,
}
impl Answer {
    fn entries(&self) -> Vec<ManifestEntry> {
        self.parts
            .iter()
            .flat_map(|part| part.entries.clone())
            .collect()
    }
    fn entry(&self, path: &str) -> ManifestEntry {
        self.entries()
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry {path}"))
    }
}
fn manifest(fixture: &Fixture, prefixes: &[&str], limit: usize) -> Answer {
    let mut parts = Vec::new();
    let reply = handle(
        fixture.root.path(),
        41,
        ManifestRequest {
            target: fixture.target.clone(),
            prefixes: prefixes.iter().map(|prefix| prefix.to_string()).collect(),
            limit,
            workspace_id: None,
            read: Vec::new(),
        },
        || true,
        &mut |part| {
            parts.push(part);
            true
        },
    )
    .unwrap();
    Answer { reply, parts }
}
fn write(fixture: &Fixture, path: &str, bytes: &[u8]) {
    let path = fixture.cwd.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
/// Files written now are racy against a walk that starts at once; real
/// workspaces were written before the walk. Wait out the fine granularity.
fn settle() {
    std::thread::sleep(Duration::from_millis(120));
}
fn blake(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// 2,000 files under 3 prefixes; the first manifest hashes all
/// 2,000, the second hashes 0 (every file a cache hit).
#[test]
fn two_thousand_files_hash_once_then_answer_from_the_cache() {
    let fixture = workspace();
    for index in 0..2_000 {
        let prefix = [".claude/skills", "docs", "_bmad"][index % 3];
        write(
            &fixture,
            &format!("{prefix}/d{}/f{index}.md", index % 17),
            format!("file {index}").as_bytes(),
        );
    }
    write(&fixture, "outside/not-declared.md", b"never listed");
    settle();
    let first = manifest(&fixture, &["docs", ".claude/skills", "_bmad"], 10_000);
    assert!(first.reply.complete, "{:?}", first.reply);
    assert_eq!(first.reply.entries, 2_000);
    assert_eq!(first.reply.progress.hashed, 2_000);
    assert_eq!(first.reply.progress.cache_hits, 0);
    assert_eq!(first.entries().len(), 2_000);
    assert_eq!(first.entry("docs/d1/f1.md").hash, Some(blake(b"file 1")));
    let second = manifest(&fixture, &["docs", ".claude/skills", "_bmad"], 10_000);
    assert_eq!(second.reply.progress.hashed, 0, "{:?}", second.reply);
    assert_eq!(second.reply.progress.cache_hits, 2_000);
    assert_eq!(second.entries(), first.entries());
}

/// A same-size edit inside one timestamp tick keeps the cache key
/// (same inode, size and whole-second times); the racy-clean rule still
/// re-hashes it, so the edit is detected.
#[test]
fn a_same_size_edit_inside_one_timestamp_tick_is_detected() {
    let fixture = workspace();
    COARSE_TIMESTAMPS.with(|flag| flag.set(true));
    write(&fixture, "docs/racy.md", b"aaaa");
    let first = manifest(&fixture, &["docs"], 100);
    assert_eq!(first.entry("docs/racy.md").hash, Some(blake(b"aaaa")));
    // Same inode, same size, within the same second.
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.cwd.join("docs/racy.md"))
        .unwrap()
        .write_all(b"bbbb")
        .unwrap();
    let second = manifest(&fixture, &["docs"], 100);
    COARSE_TIMESTAMPS.with(|flag| flag.set(false));
    assert_eq!(second.entry("docs/racy.md").hash, Some(blake(b"bbbb")));
}

/// A cold 6,000-file manifest streams progress while it walks, then
/// its entries in bounded parts, and completes.
#[test]
fn a_cold_six_thousand_file_manifest_streams_progress_and_completes() {
    let fixture = workspace();
    for index in 0..6_000 {
        write(
            &fixture,
            &format!("docs/d{}/f{index}.md", index % 50),
            &[b'x'; 64],
        );
    }
    let answer = manifest(&fixture, &["docs"], 10_000);
    assert!(answer.reply.complete);
    assert_eq!(answer.reply.entries, 6_000);
    let progress_only = answer
        .parts
        .iter()
        .filter(|part| part.entries.is_empty())
        .count();
    assert!(progress_only >= 5, "progress streamed while walking");
    // Progress only grows.
    let scanned: Vec<_> = answer
        .parts
        .iter()
        .map(|part| part.progress.scanned)
        .collect();
    assert!(scanned.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(answer
        .parts
        .iter()
        .all(|part| part.entries.len() <= ENTRIES_PER_PART));
    assert_eq!(answer.entries().len(), 6_000);
}

/// Symlinks are reported and never followed, a nested
/// repository is skipped, other types and bad names fail per item, modes are
/// normalized, and index paths carry `tracked`.
#[test]
fn the_walk_reports_without_following_and_flags_tracked_paths() {
    let fixture = workspace();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), b"outside").unwrap();
    write(&fixture, "docs/plain.md", b"plain");
    write(&fixture, "docs/run.sh", b"#!/bin/sh\n");
    std::fs::set_permissions(
        fixture.cwd.join("docs/run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o750),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.cwd.join("docs/link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.cwd.join("linked-prefix")).unwrap();
    write(&fixture, "docs/vendor/.git/HEAD", b"ref: refs/heads/main\n");
    write(&fixture, "docs/vendor/inner.md", b"inner");
    write(&fixture, "docs/cafe\u{301}.md", b"nfd");
    write(&fixture, "docs/tracked.md", b"tracked");
    let status = std::process::Command::new("git")
        .args(["add", "docs/tracked.md"])
        .current_dir(&fixture.cwd)
        .status()
        .unwrap();
    assert!(status.success());
    let fifo = std::ffi::CString::new(fixture.cwd.join("docs/pipe").to_string_lossy().into_owned())
        .unwrap();
    // SAFETY: a valid path for a new fifo.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    settle();
    let answer = manifest(&fixture, &["docs", "linked-prefix"], 100);
    assert!(answer.reply.complete, "{:?}", answer.reply);
    assert_eq!(answer.entry("docs/plain.md").mode, 0o644);
    assert_eq!(answer.entry("docs/run.sh").mode, 0o755);
    assert_eq!(answer.entry("docs/link").kind, EntryKind::Symlink);
    assert_eq!(answer.entry("linked-prefix").kind, EntryKind::Symlink);
    assert!(answer
        .entries()
        .iter()
        .all(|entry| !entry.path.contains("secret")));
    assert_eq!(answer.entry("docs/vendor").kind, EntryKind::NestedRepo);
    assert!(answer
        .entries()
        .iter()
        .all(|entry| entry.path != "docs/vendor/inner.md"));
    assert_eq!(answer.entry("docs/pipe").kind, EntryKind::Other);
    // NFC-compared, not rejected: listed by its NFC form, with its name on disk.
    let nfd = answer.entry("docs/caf\u{e9}.md");
    assert_eq!(nfd.kind, EntryKind::File);
    assert_eq!(nfd.hash, Some(blake(b"nfd")));
    assert_eq!(nfd.disk_name.as_deref(), Some("cafe\u{301}.md"));
    assert!(answer.entry("docs/tracked.md").tracked);
    assert!(!answer.entry("docs/plain.md").tracked);
}

#[test]
fn a_manifest_over_its_limit_is_incomplete() {
    let fixture = workspace();
    for index in 0..10 {
        write(&fixture, &format!("docs/{index}.md"), b"x");
    }
    let answer = manifest(&fixture, &["docs"], 4);
    assert!(!answer.reply.complete);
    assert!(answer.reply.incomplete_reason.is_some());
    assert_eq!(answer.entries().len(), 4);
}

/// A cache that does not parse, or from another filesystem, is discarded:
/// every file is hashed again and the answer stays right.
#[test]
fn a_corrupt_or_foreign_cache_is_discarded_and_rebuilt() {
    let fixture = workspace();
    write(&fixture, "docs/a.md", b"a");
    write(&fixture, "docs/b.md", b"b");
    settle();
    manifest(&fixture, &["docs"], 100);
    let (path, _) = cache_path(fixture.root.path(), fixture.cwd.to_str().unwrap());
    assert!(
        path.exists(),
        "cache written next to the workspace receipts"
    );
    std::fs::write(&path, b"{ not json").unwrap();
    let rebuilt = manifest(&fixture, &["docs"], 100);
    assert_eq!(rebuilt.reply.progress.hashed, 2);
    assert_eq!(rebuilt.entry("docs/a.md").hash, Some(blake(b"a")));
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cache["fs_id"] = serde_json::json!("another:filesystem");
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
    assert_eq!(manifest(&fixture, &["docs"], 100).reply.progress.hashed, 2);
    // No temporary is left behind.
    let leftovers = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(leftovers, 0);
}

/// A file that changes between its two `fstat`s is hashed again; one that
/// keeps changing is `unstable` and never cached.
#[test]
fn a_file_changing_while_hashed_is_unstable_and_not_cached() {
    let fixture = workspace();
    write(&fixture, "docs/log.txt", b"start");
    write(&fixture, "docs/calm.md", b"calm");
    settle();
    let log = fixture.cwd.join("docs/log.txt");
    DURING_HASH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path: &str| {
            if path == "docs/log.txt" {
                let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
                file.write_all(b"+").unwrap();
            }
        }))
    });
    let answer = manifest(&fixture, &["docs"], 100);
    DURING_HASH.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(answer.entry("docs/log.txt").kind, EntryKind::Unstable);
    assert_eq!(answer.entry("docs/log.txt").hash, None);
    assert_eq!(answer.entry("docs/calm.md").kind, EntryKind::File);
    settle();
    let calm = manifest(&fixture, &["docs"], 100);
    assert_eq!(calm.entry("docs/log.txt").kind, EntryKind::File);
    assert_eq!(calm.reply.progress.hashed, 1, "only the unstable file");
}

/// The manifest is a read: it never waits on the workspace's write lock.
#[test]
fn a_manifest_runs_while_the_workspace_lock_is_held() {
    let fixture = workspace();
    write(&fixture, "docs/a.md", b"a");
    let _held = locks::workspace(fixture.cwd.to_str().unwrap(), &|| true).unwrap();
    let answer = manifest(&fixture, &["docs"], 100);
    assert!(answer.reply.complete);
}

#[test]
fn invalid_prefixes_and_limits_are_refused() {
    let fixture = workspace();
    for prefixes in [vec!["../escape"], vec![".git"], vec!["/abs"], vec![""]] {
        let refused = handle(
            fixture.root.path(),
            41,
            ManifestRequest {
                target: fixture.target.clone(),
                prefixes: prefixes.iter().map(|p| p.to_string()).collect(),
                limit: 10,
                workspace_id: None,
                read: Vec::new(),
            },
            || true,
            &mut |_| true,
        );
        assert!(refused.is_err(), "{prefixes:?}");
    }
    assert!(handle(
        fixture.root.path(),
        41,
        ManifestRequest {
            target: fixture.target.clone(),
            prefixes: vec!["docs".into()],
            limit: 0,
            workspace_id: None,
            read: Vec::new(),
        },
        || true,
        &mut |_| true,
    )
    .is_err());
}

/// The walk holds one directory open at a time, so a tree wider than the
/// process's open-file limit is walked whole.
#[test]
fn a_tree_of_1200_sibling_directories_is_walked_whole() {
    let fixture = workspace();
    for index in 0..1_200 {
        write(&fixture, &format!("docs/d{index}/f.md"), b"x");
    }
    settle();
    let answer = manifest(&fixture, &["docs"], 10_000);
    assert!(answer.reply.complete, "{:?}", answer.reply);
    assert_eq!(answer.reply.entries, 1_200);
}

/// Only a vanished entry is silent; a directory the walk cannot read
/// makes the manifest incomplete (removals are never computed from it).
#[test]
fn an_unreadable_directory_makes_the_manifest_incomplete() {
    let fixture = workspace();
    write(&fixture, "docs/open/a.md", b"a");
    write(&fixture, "docs/locked/b.md", b"b");
    let locked = fixture.cwd.join("docs/locked");
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let answer = manifest(&fixture, &["docs"], 100);
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert!(!answer.reply.complete);
    assert!(answer
        .reply
        .incomplete_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("docs/locked")));
    assert_eq!(answer.entry("docs/open/a.md").kind, EntryKind::File);
}

/// An entry carried over while another prefix was walked keeps its own
/// verification time, so a later same-tick edit is still detected.
#[test]
fn a_carried_over_racy_entry_is_still_rehashed() {
    let fixture = workspace();
    COARSE_TIMESTAMPS.with(|flag| flag.set(true));
    write(&fixture, "a/racy.md", b"aaaa");
    write(&fixture, "b/other.md", b"other");
    manifest(&fixture, &["a"], 100);
    // Ten (virtual) seconds later another prefix is walked; the cache is
    // rewritten, carrying `a`'s entry along.
    CLOCK_OFFSET_NS.with(|offset| offset.set(10_000_000_000));
    manifest(&fixture, &["b"], 100);
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.cwd.join("a/racy.md"))
        .unwrap()
        .write_all(b"bbbb")
        .unwrap();
    let again = manifest(&fixture, &["a"], 100);
    CLOCK_OFFSET_NS.with(|offset| offset.set(0));
    COARSE_TIMESTAMPS.with(|flag| flag.set(false));
    assert_eq!(again.entry("a/racy.md").hash, Some(blake(b"bbbb")));
}

/// A walk that keeps overlapping writes answers busy, but the stable
/// hashes it computed are kept: the retry hashes nothing again.
#[test]
fn hashes_survive_a_busy_answer() {
    let fixture = workspace();
    for index in 0..5 {
        write(&fixture, &format!("docs/{index}.md"), b"stable");
    }
    settle();
    let worktree = fixture.cwd.to_string_lossy().into_owned();
    // A write lands in this worktree during every attempt.
    let writing = std::rc::Rc::new(std::cell::RefCell::new(None));
    let open = writing.clone();
    EACH_ATTEMPT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &str| {
            *open.borrow_mut() = Some(locks::writing(&worktree));
        }))
    });
    let close = writing.clone();
    DURING_HASH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &str| {
            close.borrow_mut().take();
        }))
    });
    let busy = handle(
        fixture.root.path(),
        41,
        ManifestRequest {
            target: fixture.target.clone(),
            prefixes: vec!["docs".into()],
            limit: 100,
            workspace_id: None,
            read: Vec::new(),
        },
        || true,
        &mut |_| true,
    );
    DURING_HASH.with(|hook| *hook.borrow_mut() = None);
    EACH_ATTEMPT.with(|hook| *hook.borrow_mut() = None);
    writing.borrow_mut().take();
    assert_eq!(busy.unwrap_err(), locks::BUSY);
    let retry = manifest(&fixture, &["docs"], 100);
    assert_eq!(retry.reply.progress.hashed, 0, "{:?}", retry.reply);
    assert_eq!(retry.reply.progress.cache_hits, 5);
}

/// Slow hashing still sends a part at least every few seconds, so the
/// desktop's idle timeout never fires on a walk that is working.
#[test]
fn slow_hashing_sends_heartbeat_parts() {
    let fixture = workspace();
    for index in 0..3 {
        write(&fixture, &format!("docs/{index}.md"), b"slow");
    }
    settle();
    DURING_HASH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|_: &str| {
            std::thread::sleep(Duration::from_millis(1_200))
        }))
    });
    let started = Instant::now();
    let mut stamps = vec![started];
    let mut progress_only = 0;
    handle(
        fixture.root.path(),
        41,
        ManifestRequest {
            target: fixture.target.clone(),
            prefixes: vec!["docs".into()],
            limit: 100,
            workspace_id: None,
            read: Vec::new(),
        },
        || true,
        &mut |part| {
            if part.entries.is_empty() {
                progress_only += 1;
            }
            stamps.push(Instant::now());
            true
        },
    )
    .unwrap();
    DURING_HASH.with(|hook| *hook.borrow_mut() = None);
    assert!(progress_only >= 1, "a heartbeat while hashing");
    let widest = stamps
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap();
    assert!(widest <= Duration::from_secs(5), "{widest:?}");
}

/// A file larger than one batch member is listed, hashed and
/// flagged, so the diff reports it instead of failing the batch.
#[test]
fn an_oversize_file_is_flagged() {
    let fixture = workspace();
    let big = vec![7u8; codesense_pty_core::remote_files::MAX_FILE_BYTES + 1];
    write(&fixture, "docs/big.bin", &big);
    write(&fixture, "docs/small.md", b"small");
    let answer = manifest(&fixture, &["docs"], 100);
    assert!(answer.entry("docs/big.bin").oversize);
    assert_eq!(answer.entry("docs/big.bin").hash, Some(blake(&big)));
    assert!(!answer.entry("docs/small.md").oversize);
}

/// A manifest that names a workspace reports that workspace's own
/// exclude block (its paths, or "hand-edited"), so Step 2 repairs a dropped
/// block at no extra call.
#[test]
fn a_manifest_naming_a_workspace_reports_its_exclude_block() {
    use codesense_pty_core::remote_manifest::WorkspaceExclude;
    let fixture = workspace();
    write(&fixture, "docs/a.md", b"a");
    let ask = |id: Option<&str>| {
        handle(
            fixture.root.path(),
            41,
            ManifestRequest {
                target: fixture.target.clone(),
                prefixes: vec!["docs".into()],
                limit: 100,
                workspace_id: id.map(str::to_string),
                read: Vec::new(),
            },
            || true,
            &mut |_| true,
        )
        .unwrap()
        .workspace_exclude
    };
    assert_eq!(ask(None), None);
    assert_eq!(
        ask(Some("ws-1")),
        Some(WorkspaceExclude::Paths { paths: vec![] })
    );
    let exclude = fixture
        .cwd
        .parent()
        .unwrap()
        .join("clone/.git/info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(
        &exclude,
        "# mine\n# >>> codesense managed workspace ws-1\n/docs/a.md\n# <<< codesense managed workspace ws-1\n",
    )
    .unwrap();
    assert_eq!(
        ask(Some("ws-1")),
        Some(WorkspaceExclude::Paths {
            paths: vec!["docs/a.md".into()]
        })
    );
    std::fs::write(
        &exclude,
        "# >>> codesense managed workspace ws-1\ndocs/*\n# <<< codesense managed workspace ws-1\n",
    )
    .unwrap();
    assert!(matches!(
        ask(Some("ws-1")),
        Some(WorkspaceExclude::HandEdited { .. })
    ));
}

#[test]
fn deleted_worktree_manifest_returns_missing_code_without_parts() {
    let fixture = workspace();
    std::fs::remove_dir_all(&fixture.cwd).unwrap();
    let error = handle(
        fixture.root.path(),
        41,
        ManifestRequest {
            target: fixture.target,
            prefixes: vec!["docs".into()],
            limit: 100,
            workspace_id: None,
            read: vec![],
        },
        || true,
        &mut |_| panic!("a missing workspace cannot stream files"),
    )
    .unwrap_err();
    assert!(error.starts_with("REMOTE_WORKSPACE_MISSING:"), "{error}");
}
