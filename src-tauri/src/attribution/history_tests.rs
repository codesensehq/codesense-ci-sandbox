// SPDX-License-Identifier: MIT
//! Fixtures are temp repos only: a bare origin, and a clone of it on an unrelated old branch,
//! dirty, with local `refs/notes/sense`; remotes are local files or loopback listeners.
use super::*;
use crate::attribution::git_ai::tests::git;
use crate::attribution::ranges::{HISTORY_PAST_SHALLOW, PATH_NOT_IN_REVISION};
use crate::attribution::why_line::{self, tests as wl};
use crate::context_repo::caller_scope::CallerScope;
use crate::domain::Principal;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const STORY: &str = "SV-29";
pub(crate) const BRANCH: &str = "feat/SV-29-trail";
/// History older than the story, so a deepen that covers the story leaves a boundary.
const ANCIENT: usize = 150;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/attribution/testdata/git_ai_1_7_5")
        .join(name)
}

fn copy_tree(from: &str, to: &Path) {
    for entry in std::fs::read_dir(testdata(from)).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

fn sha(repo: &Path, rev: &str) -> String {
    git(repo, &["rev-parse", rev]).trim().to_owned()
}

pub(crate) struct Trail {
    _root: tempfile::TempDir,
    pub work: PathBuf,
    pub origin: PathBuf,
    pub clone: PathBuf,
    /// The story's merge on origin's default branch, and the commit it landed.
    pub merge: String,
    pub feature: String,
}

impl Trail {
    /// Origin's default branch is `default`; the story `SV-29` merged as PR #7 (its c1 commit
    /// has a real git-ai note, kept only in the clone's `refs/notes/sense`), then one more
    /// commit landed. The clone is on `old-wip` (no `a.txt` there), dirty, and attached as
    /// `acme/api` in `home`; `depth` makes it shallow. `refs/remotes/origin/HEAD` is removed.
    /// The story file says SV-29 is done.
    pub(crate) fn new(home: &Path, default: &str, depth: Option<u32>) -> Self {
        Self::build(home, default, depth, ANCIENT, |_| {})
    }

    /// [`Self::new`] with `ancient` older commits, and `more` run in the work repo on the
    /// default branch after the story landed, before origin is made from it.
    pub(crate) fn build(
        home: &Path,
        default: &str,
        depth: Option<u32>,
        ancient: usize,
        more: impl FnOnce(&Path),
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (work, origin, clone) = (
            root.path().join("work"),
            root.path().join("origin.git"),
            root.path().join("clone"),
        );
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", default]);
        {
            let repo = git2::Repository::open(&work).unwrap();
            let sig =
                git2::Signature::new("Fixture", "fixture@example.com", &git2::Time::new(1_000, 0))
                    .unwrap();
            let mut parent: Option<git2::Oid> = None;
            for n in 0..ancient {
                let blob = repo.blob(format!("{n}\n").as_bytes()).unwrap();
                let mut tree = repo.treebuilder(None).unwrap();
                tree.insert("ancient.txt", blob, 0o100644).unwrap();
                let tree = repo.find_tree(tree.write().unwrap()).unwrap();
                let parents: Vec<_> = parent
                    .iter()
                    .map(|p| repo.find_commit(*p).unwrap())
                    .collect();
                parent = Some(
                    repo.commit(
                        Some("HEAD"),
                        &sig,
                        &sig,
                        &format!("ancient {n}"),
                        &tree,
                        &parents.iter().collect::<Vec<_>>(),
                    )
                    .unwrap(),
                );
            }
        }
        git(&work, &["reset", "-q", "--hard"]);
        copy_tree("c0", &work);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "initial"]);
        git(&work, &["tag", "v0"]);
        git(&work, &["branch", "stale"]);
        git(&work, &["checkout", "-qb", "old-wip"]);
        std::fs::write(work.join("old.txt"), "old\n").unwrap();
        git(&work, &["add", "old.txt"]);
        git(&work, &["commit", "-qm", "old wip"]);
        git(&work, &["checkout", "-q", default]);
        git(&work, &["checkout", "-qb", BRANCH]);
        copy_tree("c1", &work);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "c1-attributed"]);
        git(
            &work,
            &[
                "notes",
                "--ref=sense",
                "add",
                "-F",
                testdata("c1.note").to_str().unwrap(),
            ],
        );
        let feature = sha(&work, "HEAD");
        git(&work, &["checkout", "-q", default]);
        git(
            &work,
            &[
                "merge",
                "--no-ff",
                "-q",
                "-m",
                &format!("Merge pull request #7 from acme/{BRANCH}"),
                BRANCH,
            ],
        );
        let merge = sha(&work, "HEAD");
        std::fs::write(work.join("later.txt"), "later\n").unwrap();
        git(&work, &["add", "later.txt"]);
        git(&work, &["commit", "-qm", "later work"]);
        more(&work);
        git(
            root.path(),
            &[
                "clone",
                "-q",
                "--bare",
                work.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let url = format!("file://{}", origin.display());
        let mut args = vec!["clone", "-q"];
        let depth = depth.map(|depth| format!("--depth={depth}"));
        if let Some(depth) = &depth {
            args.extend([depth.as_str(), "--no-single-branch"]);
        }
        args.extend([url.as_str(), clone.to_str().unwrap()]);
        git(root.path(), &args);
        git(&clone, &["checkout", "-qb", "old-wip", "origin/old-wip"]);
        // Dirty: a modified tracked file, a staged new file and an untracked one.
        std::fs::write(clone.join("base.txt"), "edited by the user\n").unwrap();
        std::fs::write(clone.join("staged.txt"), "staged\n").unwrap();
        git(&clone, &["add", "staged.txt"]);
        std::fs::write(clone.join("untracked.txt"), "untracked\n").unwrap();
        git(
            &clone,
            &[
                "fetch",
                "-q",
                "--no-write-fetch-head",
                work.to_str().unwrap(),
                "refs/notes/sense:refs/notes/sense",
            ],
        );
        git(
            &clone,
            &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
        );
        assert!(git2::Repository::open(&clone)
            .unwrap()
            .find_reference(&format!("refs/remotes/origin/{default}"))
            .is_ok());
        // Bind under the configured identity, then point origin back at the local bare repo.
        git(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/acme/api.git",
            ],
        );
        wl::org(home);
        wl::bind(home, &clone);
        story_file(home, STORY, "done");
        git(&clone, &["remote", "set-url", "origin", &url]);
        Self {
            _root: root,
            work,
            origin,
            clone,
            merge,
            feature,
        }
    }

    /// Origin moves on: a new commit and tag on its default branch, and branch `stale` deleted.
    pub(crate) fn advance_origin(&self) -> String {
        std::fs::write(self.work.join("after.txt"), "after\n").unwrap();
        git(&self.work, &["add", "after.txt"]);
        git(&self.work, &["commit", "-qm", "after"]);
        git(&self.work, &["tag", "v1"]);
        git(
            &self.work,
            &["push", "-q", "origin", "HEAD", "v1", ":stale"],
        );
        sha(&self.work, "HEAD")
    }

    pub(crate) fn source(&self) -> HistorySource {
        HistorySource::at(&self.clone).unwrap()
    }

    pub(crate) fn set_origin(&self, url: &str) {
        git(&self.clone, &["remote", "set-url", "origin", url]);
    }
}

/// A story file found by its id in the attached org.
pub(crate) fn story_file(home: &Path, id: &str, status: &str) {
    let org = crate::context_repo::path_to_org_root(home, &Principal::from(1u64), "acme").unwrap();
    std::fs::create_dir_all(org.join("dev/content")).unwrap();
    std::fs::write(
        org.join(format!("dev/content/{id}.md")),
        format!("---\ntype: story\nstory_id: {id}\nsource_repo: acme/api\nstatus: {status}\n---\n## Purpose\nTrail.\n"),
    )
    .unwrap();
}

/// Commit `file` on a new `branch` off the current branch, then return to it.
pub(crate) fn branch_commit(work: &Path, branch: &str, file: &str) -> String {
    let back = git(work, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();
    git(work, &["checkout", "-qb", branch]);
    std::fs::write(work.join(file), format!("{branch}\n")).unwrap();
    git(work, &["add", file]);
    git(work, &["commit", "-qm", &format!("{branch} work")]);
    let head = sha(work, "HEAD");
    git(work, &["checkout", "-q", &back]);
    head
}

/// Every byte the user owns in the clone: the working tree (tracked, staged, untracked), the
/// index, HEAD, config, local refs, tags and notes. Only remote-tracking refs (and their
/// reflogs), objects and `shallow` are left out; `packed-refs` keeps its non-remote lines.
pub(crate) fn user_state(clone: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if [
                ".git/objects/",
                ".git/refs/remotes/",
                ".git/logs/refs/remotes/",
            ]
            .iter()
            .any(|skip| format!("{rel}/").starts_with(skip))
                || rel == ".git/shallow"
            {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if rel == ".git/packed-refs" {
                let text = std::fs::read_to_string(&path).unwrap();
                let kept: String = text
                    .lines()
                    .filter(|l| !l.contains(" refs/remotes/"))
                    .map(|l| format!("{l}\n"))
                    .collect();
                out.insert(rel, kept.into_bytes());
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(clone, clone, &mut out);
    let refs = git(
        clone,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let refs: String = refs
        .lines()
        .filter(|l| !l.starts_with("refs/remotes/"))
        .map(|l| format!("{l}\n"))
        .collect();
    out.insert("for-each-ref (non-remote)".into(), refs.into_bytes());
    out
}

pub(crate) fn assert_same_state(
    before: &BTreeMap<String, Vec<u8>>,
    after: &BTreeMap<String, Vec<u8>>,
    what: &str,
) {
    let changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .collect();
    assert!(
        changed.is_empty(),
        "{what} wrote the user's state: {changed:?}"
    );
}

/// A loopback listener that accepts and never answers; it counts connections.
pub(crate) struct Blackhole {
    pub url: String,
    pub hits: Arc<AtomicUsize>,
}

pub(crate) fn blackhole() -> Blackhole {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "git://127.0.0.1:{}/api.git",
        listener.local_addr().unwrap().port()
    );
    let hits = Arc::new(AtomicUsize::new(0));
    let (counted, held) = (hits.clone(), Arc::new(Mutex::new(Vec::new())));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            held.lock().unwrap().push(stream);
        }
    });
    Blackhole { url, hits }
}

/// A loopback HTTP remote that demands credentials on every request.
fn needs_auth() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/acme/api.git",
        listener.local_addr().unwrap().port()
    );
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    url
}

pub(crate) fn why(home: &Path, story: Option<&str>, rev: Option<&str>) -> Value {
    let params = why_line::Params {
        repo: "acme/api".into(),
        rev: rev.map(Into::into),
        story: story.map(Into::into),
        path: "a.txt".into(),
        start: 1,
        end: 3,
    };
    why_line::resolve_core(home, &Principal::from(1), &params, &CallerScope::Human).unwrap()
}

pub(crate) async fn refresh_story(home: &Path, story: Option<&str>) -> Value {
    let params = RefreshParams {
        repo: "acme/api".into(),
        story_id: story.map(Into::into),
        timeout_ms: Some(60_000),
    };
    refresh(home, &Principal::from(1), &params, &CallerScope::Human)
        .await
        .unwrap()
}

fn assert_attributed(answer: &Value) {
    let ranges = answer["ranges"]
        .as_array()
        .unwrap_or_else(|| panic!("no ranges: {answer}"));
    assert!(!ranges.is_empty(), "{answer}");
    assert!(
        ranges.iter().all(|range| range["basis"] == "exact"),
        "{answer}"
    );
    assert!(
        ranges.iter().any(|range| range["tool"] == "claude"),
        "{answer}"
    );
}

/// Run `f` on a thread, failing (instead of hanging) if it takes longer than `limit`.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit)
        .expect("did not finish within its bound")
}

// AC3
#[tokio::test]
async fn refresh_trail_fetch_and_reads_leave_the_users_working_state_byte_identical() {
    for depth in [None, Some(1)] {
        let home = tempfile::tempdir().unwrap();
        let trail = Trail::new(home.path(), "main", depth);
        // The user's own config prunes; pruning may change only remote-tracking refs.
        git(&trail.clone, &["config", "fetch.prune", "true"]);
        git(&trail.clone, &["branch", "keep-me", "origin/stale"]);
        let new_head = trail.advance_origin();
        let before = user_state(&trail.clone);

        let refreshed = refresh_story(home.path(), Some(STORY)).await;
        assert!(refreshed.get("status").is_none(), "{refreshed}");
        assert_same_state(&before, &user_state(&trail.clone), "refresh_history");

        assert_attributed(&why(home.path(), Some(STORY), None));
        why(home.path(), None, None);
        assert_same_state(&before, &user_state(&trail.clone), "a trail read");

        trail
            .source()
            .fetch(home.path(), Some(STORY), TRAIL_FETCH_BOUND)
            .unwrap();
        assert_same_state(&before, &user_state(&trail.clone), "the trail-open fetch");

        // The fetch did update remote-tracking refs, and only those.
        assert_eq!(sha(&trail.clone, "refs/remotes/origin/main"), new_head);
        let refs = git(&trail.clone, &["for-each-ref", "--format=%(refname)"]);
        assert!(
            !refs.contains("refs/remotes/origin/stale"),
            "pruned: {refs}"
        );
        assert!(
            refs.contains("refs/heads/keep-me"),
            "local branches untouched: {refs}"
        );
        assert!(!refs.contains("refs/tags/v1"), "no tags: {refs}");
        assert!(!trail.clone.join(".git/FETCH_HEAD").exists());
        assert_eq!(
            git(&trail.clone, &["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/old-wip"
        );
    }
}

// AC1 (why_line)
#[test]
fn why_line_for_a_story_reads_its_merge_not_the_checkouts_old_dirty_head() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let answer = why(home.path(), Some(STORY), None);
    assert_ne!(answer["reason"], PATH_NOT_IN_REVISION, "{answer}");
    assert_attributed(&answer);
    assert_eq!(answer["resolvedCommit"], trail.merge.as_str());
}

// AC4
#[test]
fn an_omitted_rev_is_merged_sha_then_branch_head_then_origin_default_never_head() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let pick = |story: Option<&str>| {
        let answer = why(home.path(), story, None);
        (
            answer["revSource"].as_str().map(str::to_owned),
            answer["resolvedCommit"].as_str().map(str::to_owned),
            answer,
        )
    };
    // 1. A merged story: its merge.
    let (source, commit, answer) = pick(Some(STORY));
    assert_eq!(
        (source.as_deref(), commit.as_deref()),
        (Some("mergedSha"), Some(trail.merge.as_str())),
        "{answer}"
    );
    // 2. An unmerged story: origin's branch, then a local branch of the same story ahead of it.
    git(
        &trail.work,
        &["checkout", "-qb", "feat/SV-31-open", &trail.merge],
    );
    std::fs::write(trail.work.join("open.txt"), "open\n").unwrap();
    git(&trail.work, &["add", "open.txt"]);
    git(&trail.work, &["commit", "-qm", "SV-31 work"]);
    git(&trail.work, &["push", "-q", "origin", "feat/SV-31-open"]);
    git(
        &trail.clone,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    let remote = sha(&trail.clone, "refs/remotes/origin/feat/SV-31-open");
    let (source, commit, answer) = pick(Some("SV-31"));
    assert_eq!(
        (source.as_deref(), commit.as_deref()),
        (Some("branch"), Some(remote.as_str())),
        "{answer}"
    );
    git(
        &trail.clone,
        &["update-ref", "refs/heads/feat/SV-31-open", &trail.feature],
    );
    let (source, commit, _) = pick(Some("SV-31"));
    assert_eq!(
        (source.as_deref(), commit.as_deref()),
        (Some("branch"), Some(trail.feature.as_str()))
    );
    // 3. No story: origin/main without origin/HEAD, then what origin/HEAD names.
    let main = sha(&trail.clone, "refs/remotes/origin/main");
    let (source, commit, _) = pick(None);
    assert_eq!(
        (source.as_deref(), commit.as_deref()),
        (Some("defaultBranch"), Some(main.as_str()))
    );
    git(
        &trail.clone,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{BRANCH}"),
        ],
    );
    let (source, commit, _) = pick(None);
    assert_eq!(
        (source.as_deref(), commit.as_deref()),
        (Some("defaultBranch"), Some(trail.feature.as_str()))
    );
    // Never HEAD, unless asked for: the checkout's branch has no a.txt.
    let head = why(home.path(), None, Some("HEAD"));
    assert_eq!(head["reason"], PATH_NOT_IN_REVISION, "{head}");
}

// AC2 (why_line)
#[tokio::test]
async fn a_shallow_copy_names_the_boundary_until_refresh_deepens_just_enough() {
    let home = tempfile::tempdir().unwrap();
    // More history than one merge scan reads, so "enough" is less than all of it.
    let trail = Trail::build(home.path(), "main", Some(1), SCAN_LIMIT + 200, |_| {});
    assert!(trail.source().is_shallow());
    // The merge is past the boundary; the branch head (itself a boundary) answers per range.
    let branch = why(home.path(), Some(STORY), None);
    assert_eq!(branch["revSource"], "branch", "{branch}");
    let ranges = branch["ranges"]
        .as_array()
        .unwrap_or_else(|| panic!("{branch}"));
    assert!(
        ranges.iter().all(|r| r["reason"] == HISTORY_PAST_SHALLOW),
        "{branch}"
    );
    // Without the branch, the merge is simply unknowable here.
    git(
        &trail.clone,
        &["update-ref", "-d", &format!("refs/remotes/origin/{BRANCH}")],
    );
    let blocked = why(home.path(), Some(STORY), None);
    assert_eq!(blocked["status"], "unavailable", "{blocked}");
    assert_eq!(blocked["reason"], HISTORY_PAST_SHALLOW, "{blocked}");
    // At the default branch tip, blame folds every line into the boundary commit.
    let tip = why(home.path(), None, None);
    let ranges = tip["ranges"].as_array().unwrap_or_else(|| panic!("{tip}"));
    assert!(
        ranges
            .iter()
            .all(|r| r["basis"] == "unknown" && r["reason"] == HISTORY_PAST_SHALLOW),
        "{tip}"
    );

    let refreshed = refresh_story(home.path(), Some(STORY)).await;
    assert!(
        refreshed.get("status").is_none() && refreshed.get("reason").is_none(),
        "{refreshed}"
    );
    assert!(!refreshed["deepened"].is_null(), "{refreshed}");
    assert_eq!(
        refreshed["shallow"], true,
        "deepened just enough, not the whole history: {refreshed}"
    );
    let answer = why(home.path(), Some(STORY), None);
    assert_attributed(&answer);
    assert_eq!(answer["resolvedCommit"], trail.merge.as_str());
}

// AC5: ordinary reads
#[test]
fn ordinary_reads_of_present_history_make_no_network_call() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let hole = blackhole();
    trail.set_origin(&hole.url);
    let started = Instant::now();
    assert_attributed(&why(home.path(), Some(STORY), None));
    why(home.path(), None, None);
    why(home.path(), None, Some(&trail.feature));
    assert!(!trail.source().story_past_boundary(home.path(), STORY));
    assert_eq!(
        hole.hits.load(Ordering::SeqCst),
        0,
        "a read reached the network"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

// AC5: the trail-open fetch is bounded
#[test]
fn the_trail_open_fetch_gives_up_at_its_bound_on_a_blackholed_remote() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", Some(1));
    let hole = blackhole();
    trail.set_origin(&hole.url);
    let (source, home_path) = (trail.source(), home.path().to_owned());
    let started = Instant::now();
    let result = within(Duration::from_secs(20), move || {
        source.fetch(&home_path, Some(STORY), Duration::from_secs(2))
    });
    assert_eq!(result.unwrap_err(), FETCH_TIMED_OUT);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
    assert!(
        hole.hits.load(Ordering::SeqCst) >= 1,
        "the fetch did try origin"
    );
    assert_eq!(
        why(home.path(), Some(STORY), None)["ranges"][0]["reason"],
        HISTORY_PAST_SHALLOW
    );
}

// AC5: refresh_history returns the new historyAsOf
#[tokio::test]
async fn refresh_history_returns_the_new_history_as_of_and_sets_a_missing_origin_head() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let before = trail.source().history_as_of(home.path());
    assert!(before["fetchedAt"].is_null());
    let new_head = trail.advance_origin();
    let refreshed = refresh_story(home.path(), None).await;
    assert_eq!(refreshed["repo"], "acme/api");
    assert_eq!(
        refreshed["historyAsOf"]["originHead"],
        new_head.as_str(),
        "{refreshed}"
    );
    assert!(
        refreshed["historyAsOf"]["fetchedAt"].as_i64().is_some(),
        "{refreshed}"
    );
    assert_eq!(
        refreshed["historyAsOf"],
        trail.source().history_as_of(home.path())
    );
    assert_eq!(
        why(home.path(), None, None)["historyAsOf"],
        refreshed["historyAsOf"]
    );
    assert_eq!(
        git(&trail.clone, &["symbolic-ref", "refs/remotes/origin/HEAD"]).trim(),
        "refs/remotes/origin/main"
    );
}

// Lead: never prompt
#[test]
fn a_remote_that_needs_credentials_fails_fast_without_prompting() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    trail.set_origin(&needs_auth());
    let (source, home_path) = (trail.source(), home.path().to_owned());
    let started = Instant::now();
    let result = within(Duration::from_secs(20), move || {
        source.fetch(&home_path, None, Duration::from_secs(15))
    });
    assert_eq!(result.unwrap_err(), FETCH_NEEDS_CREDENTIALS);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

// Lead: a held lock
#[test]
fn a_held_ref_lock_skips_the_fetch_as_busy_and_reads_still_answer() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    trail.advance_origin();
    let lock = trail.clone.join(".git/refs/remotes/origin/main.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    assert_eq!(
        trail
            .source()
            .fetch(home.path(), None, Duration::from_secs(30))
            .unwrap_err(),
        FETCH_REPO_BUSY
    );
    assert!(lock.exists(), "the user's lock is left alone");
    assert_attributed(&why(home.path(), Some(STORY), None));
}

// AC6
#[tokio::test]
async fn a_fetch_keeps_cache_entries_for_unchanged_commits() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let first = why(home.path(), Some(STORY), None);
    assert_attributed(&first);
    let cache = || {
        super::super::why_line_cache::Cache::open(&home.path().join("caches/why-line.sqlite3"))
            .unwrap()
    };
    let key = super::super::why_line_cache::Key {
        repo: "github.com/acme/api",
        commit: &trail.merge,
        path: "a.txt",
        notes: first["notesRef"].as_str(),
    };
    let cached = cache().get(&key).unwrap();
    assert!(cached.is_some());
    trail.advance_origin();
    refresh_story(home.path(), Some(STORY)).await;
    assert_eq!(
        cache().get(&key).unwrap(),
        cached,
        "the fetch kept the unchanged commit's entry"
    );
    assert_eq!(
        why(home.path(), Some(STORY), None)["ranges"],
        first["ranges"]
    );
}

// D1 (a): attach fetches once, bounded, in the background.
#[test]
fn attaching_fetches_origin_once_in_the_background() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", None);
    let new_head = trail.advance_origin();
    fetch_after_attach(home.path(), &trail.clone);
    let deadline = Instant::now() + Duration::from_secs(30);
    while trail.source().history_as_of(home.path())["originHead"] != new_head.as_str()
        || !trail.source().history_as_of(home.path())["fetchedAt"].is_i64()
    {
        assert!(Instant::now() < deadline, "attach fetch never landed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn merge_subjects_match_merge_and_squash_shapes_of_the_story_only() {
    let none = BTreeSet::new();
    assert_eq!(
        story_merge_subject(
            "Merge pull request #7 from acme/feat/SV-29-trail",
            STORY,
            &none
        ),
        Some((7, Some("feat/SV-29-trail")))
    );
    assert_eq!(
        story_merge_subject("SV-29: land it (#8)", STORY, &none),
        Some((8, None))
    );
    assert_eq!(
        story_merge_subject(
            "Merge pull request #9 from acme/feat/SV-30-other",
            STORY,
            &none
        ),
        None
    );
    assert_eq!(
        story_merge_subject(
            "Merge pull request #9 from acme/odd",
            STORY,
            &BTreeSet::from(["odd".to_owned()])
        ),
        Some((9, Some("odd")))
    );
    assert_eq!(
        story_merge_subject("Revert \"SV-29: land it (#8)\"", STORY, &none),
        None
    );
}

// Never assume `main`. Default branches `master` and `release`, with and without
// `refs/remotes/origin/HEAD`, with origin reachable and offline.
#[tokio::test]
async fn the_default_branch_comes_from_origin_head_never_an_assumed_name() {
    for default in ["master", "release"] {
        for (origin_head, online) in [(true, true), (true, false), (false, true), (false, false)] {
            let case = format!("{default} origin/HEAD={origin_head} online={online}");
            let home = tempfile::tempdir().unwrap();
            let trail = Trail::new(home.path(), default, None);
            let tip = sha(&trail.clone, &format!("refs/remotes/origin/{default}"));
            if origin_head {
                git(
                    &trail.clone,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        &format!("refs/remotes/origin/{default}"),
                    ],
                );
            }
            if !online {
                trail.set_origin(&blackhole().url);
            }
            let params = RefreshParams {
                repo: "acme/api".into(),
                story_id: None,
                timeout_ms: Some(3_000),
            };
            let refreshed = refresh(
                home.path(),
                &Principal::from(1),
                &params,
                &CallerScope::Human,
            )
            .await
            .unwrap();
            assert_eq!(
                refreshed.get("status").is_none(),
                online,
                "{case}: {refreshed}"
            );
            let answer = why(home.path(), None, None);
            if origin_head || online || default == "master" {
                assert_eq!(answer["revSource"], "defaultBranch", "{case}: {answer}");
                assert_eq!(answer["resolvedCommit"], tip.as_str(), "{case}: {answer}");
                // The story's merge walk uses the same resolver.
                assert_eq!(
                    why(home.path(), Some(STORY), None)["resolvedCommit"],
                    trail.merge.as_str(),
                    "{case}"
                );
            } else {
                assert_eq!(answer["reason"], NO_DEFAULT_BRANCH, "{case}: {answer}");
            }
        }
    }
}

// Review M1: a two-PR story whose first PR is past the boundary is never a silent partial.
#[tokio::test]
async fn a_landed_story_with_a_merge_past_the_boundary_says_older_merges_may_be_missing() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::build(home.path(), "main", Some(1), ANCIENT, |work| {
        branch_commit(work, "feat/SV-29-cp2", "cp2.txt");
        git(
            work,
            &[
                "merge",
                "--no-ff",
                "-q",
                "-m",
                "Merge pull request #8 from acme/feat/SV-29-cp2",
                "feat/SV-29-cp2",
            ],
        );
    });
    let source = trail.source();
    assert!(
        source.story_past_boundary(home.path(), STORY),
        "CP1 is past the boundary"
    );
    // Only CP2's merge is visible, and the newest merge is still the right revision.
    let answer = why(home.path(), Some(STORY), None);
    assert_eq!(answer["revSource"], "mergedSha", "{answer}");
    let refreshed = refresh_story(home.path(), Some(STORY)).await;
    assert!(refreshed.get("reason").is_none(), "{refreshed}");
    assert!(
        !source.story_past_boundary(home.path(), STORY),
        "the scan window is inside the boundary now"
    );
    let repo = source.open().unwrap();
    let walk = HistorySource::story_merges(home.path(), &repo, STORY, &BTreeSet::new()).unwrap();
    assert_eq!(walk.merges.len(), 2, "both PRs: {:?}", walk.merges);
}

// Review M3: a story still in progress reads its branch on a shallow copy, with no fetch.
#[tokio::test]
async fn an_in_progress_story_on_a_shallow_copy_answers_from_its_branch_without_fetching() {
    let home = tempfile::tempdir().unwrap();
    let mut head = String::new();
    let trail = Trail::build(home.path(), "main", Some(1), ANCIENT, |work| {
        head = branch_commit(work, "feat/SV-31-open", "open.txt");
    });
    story_file(home.path(), "SV-31", "in-progress");
    let hole = blackhole();
    trail.set_origin(&hole.url);
    let answer = why(home.path(), Some("SV-31"), None);
    assert_eq!(answer["revSource"], "branch", "{answer}");
    assert_eq!(answer["resolvedCommit"], head.as_str());
    let ranges = answer["ranges"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"));
    assert!(
        ranges.iter().all(|r| r["reason"] == HISTORY_PAST_SHALLOW),
        "per range: {answer}"
    );
    // refresh_history for it is a plain fetch: no deepen.
    trail.set_origin(&format!("file://{}", trail.origin.display()));
    let refreshed = refresh_story(home.path(), Some("SV-31")).await;
    assert!(refreshed["deepened"].is_null(), "{refreshed}");
    assert_eq!(refreshed["shallow"], true);
    trail.set_origin(&hole.url);
    assert_eq!(hole.hits.load(Ordering::SeqCst), 0, "no read fetched");
}

// Review lows: an explicit rev on a shallow copy.
#[test]
fn an_explicit_rev_on_a_shallow_copy_is_honoured_or_not_local() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", Some(1));
    let missing = why(home.path(), None, Some(&trail.merge));
    assert_eq!(
        missing["reason"],
        crate::attribution::ranges::REVISION_NOT_LOCAL,
        "{missing}"
    );
    let tip = sha(&trail.clone, "refs/remotes/origin/main");
    let answer = why(home.path(), None, Some(&tip));
    assert_eq!(answer["revSource"], "explicit");
    assert_eq!(
        answer["ranges"][0]["reason"], HISTORY_PAST_SHALLOW,
        "{answer}"
    );
}

// Review lows: a deepen that fails after the first fetch is a partial, with fetchedAt.
#[tokio::test]
async fn a_refresh_whose_deepen_fails_reports_partial_and_records_the_fetch() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", Some(1));
    std::fs::write(trail.clone.join(".git/shallow.lock"), "").unwrap();
    let refreshed = refresh_story(home.path(), Some(STORY)).await;
    std::fs::remove_file(trail.clone.join(".git/shallow.lock")).unwrap();
    assert_eq!(refreshed["status"], "partial", "{refreshed}");
    assert_eq!(refreshed["reason"], FETCH_REPO_BUSY, "{refreshed}");
    assert!(
        refreshed["historyAsOf"]["fetchedAt"].is_i64(),
        "{refreshed}"
    );
}

// Review lows: a linked worktree reads and records through the shared common dir.
#[test]
fn a_linked_worktree_shares_its_checkouts_history() {
    let home = tempfile::tempdir().unwrap();
    let trail = Trail::new(home.path(), "main", Some(1));
    let linked = trail.clone.parent().unwrap().join("linked");
    git(
        &trail.clone,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "origin/main",
        ],
    );
    let (main, side) = (trail.source(), HistorySource::at(&linked).unwrap());
    assert!(side.is_shallow());
    assert_eq!(side.boundary(), main.boundary());
    assert_eq!(side.record_key(), main.record_key());
    side.fetch(home.path(), None, Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        main.history_as_of(home.path()),
        side.history_as_of(home.path())
    );
}

#[test]
fn a_status_is_done_whatever_its_case() {
    for status in ["done", "Done", " MERGED "] {
        assert!(status_is_done(status), "{status}");
    }
    for status in ["in-progress", "review", ""] {
        assert!(!status_is_done(status), "{status}");
    }
}

/// SV-CDE-82 (I1, review r5 M1-M3, L2): one linkage rule for every merge walk, over one
/// tip set (the story's live branch heads and its recorded tips). Rows:
/// - a custom subject links by `M^2` = the live head, then by the recorded tip once the branch
///   is gone, and the why_line walk (`story_merges`) finds the same merge;
/// - the number is GitHub's `#N` or a trailing `(#N)`, never the first `#`; else the one local
///   PR head ref at `M^2`; two refs are ambiguous; no number always has a reason;
/// - a subject naming another branch, a `develop` → `main` merge, and a tip already on the
///   first parent's line never link; a tip that is also another story's branch head is
///   unlinked with [`TIP_SHARED`], both ways;
/// - a two-parent merge under the story's squash-style subject is a merge, not a squash.
/// Red on the base: only subjects linked (no tips), and the first `#` was the number.
#[test]
fn a_merge_links_by_its_second_parent_in_every_walk() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let sig = git2::Signature::new("t", "t@example.com", &git2::Time::new(1000, 0)).unwrap();
    let blob = |text: &str| {
        let mut builder = repo.treebuilder(None).unwrap();
        builder
            .insert("f", repo.blob(text.as_bytes()).unwrap(), 0o100644)
            .unwrap();
        builder.write().unwrap()
    };
    let commit = |reference: Option<&str>, subject: &str, parents: &[git2::Oid]| {
        let tree = repo.find_tree(blob(subject)).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|p| repo.find_commit(*p).unwrap())
            .collect();
        repo.commit(
            reference,
            &sig,
            &sig,
            subject,
            &tree,
            &parents.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let root = commit(Some("refs/heads/main"), "root", &[]);
    let tip = commit(Some("refs/heads/feat/SV-29-x"), "work", &[root]);
    let merged = commit(
        None,
        "Merge pull request #2769: SV-29 work (fixes #12)",
        &[root, tip],
    );
    repo.reference("refs/remotes/origin/main", merged, true, "merge")
        .unwrap();
    let merge = repo.find_commit(merged).unwrap();
    let none = BTreeSet::new();
    let tips = || StoryTips::for_story(home.path(), &repo, STORY, &none);
    let link = |commit: &git2::Commit<'_>| story_merge(&repo, commit, STORY, &none, &tips());

    assert_eq!(
        link(&merge),
        Some(Ok(MergeLink {
            number: Some(2769),
            number_reason: None,
            branch: Some("feat/SV-29-x".into()),
            by_tip: true,
            tip_recorded: false,
            merge: true,
        }))
    );
    let walk = HistorySource::story_merges(home.path(), &repo, STORY, &none).unwrap();
    assert_eq!(
        walk.merges,
        vec![merged],
        "the why_line walk sees the same merge"
    );

    // Recorded, then the branch goes: only the recorded tip links, in every walk.
    let checkout = tempfile::tempdir().unwrap();
    let branch = repo.find_reference("refs/heads/feat/SV-29-x").unwrap();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&branch));
    let wt = checkout.path().join("wt");
    repo.worktree("sv29", &wt, Some(&options)).unwrap();
    drop(branch);
    assert!(crate::install::story_tips::sight_root(
        home.path(),
        STORY,
        &wt,
        crate::install::story_tips::Wait::LastChance
    ));
    std::fs::remove_dir_all(&wt).unwrap();
    repo.find_reference("refs/heads/feat/SV-29-x")
        .unwrap()
        .delete()
        .unwrap();
    assert!(matches!(
        link(&merge),
        Some(Ok(MergeLink { by_tip: true, .. }))
    ));
    let walk = HistorySource::story_merges(home.path(), &repo, STORY, &none).unwrap();
    assert_eq!(walk.merges, vec![merged]);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        story_merge(
            &repo,
            &merge,
            STORY,
            &none,
            &StoryTips::for_story(empty.path(), &repo, STORY, &none)
        ),
        None,
        "no tip recorded: nothing links"
    );

    // Numbers: a local PR head ref, then two of them.
    let plain = repo
        .find_commit(commit(None, "Land the work", &[root, tip]))
        .unwrap();
    assert!(matches!(
        link(&plain),
        Some(Ok(MergeLink {
            number: None,
            number_reason: Some(NO_PR_NUMBER),
            ..
        }))
    ));
    repo.reference("refs/pull/9/head", tip, true, "pr").unwrap();
    assert!(matches!(
        link(&plain),
        Some(Ok(MergeLink {
            number: Some(9),
            ..
        }))
    ));
    repo.reference("refs/remotes/pr/10", tip, true, "pr")
        .unwrap();
    assert!(matches!(
        link(&plain),
        Some(Ok(MergeLink {
            number: None,
            number_reason: Some(AMBIGUOUS_PR_REFS),
            ..
        }))
    ));

    // Subjects that name another branch, and develop -> main, never link by tip.
    for subject in [
        "Merge pull request #5 from acme/feat/SV-30-other",
        "Merge branch 'develop'",
    ] {
        let other = repo
            .find_commit(commit(None, subject, &[root, tip]))
            .unwrap();
        assert_eq!(link(&other), None, "{subject}");
    }
    let develop = commit(Some("refs/heads/develop"), "dev", &[root]);
    let dev_merge = repo
        .find_commit(commit(None, "Release", &[root, develop]))
        .unwrap();
    assert_eq!(link(&dev_merge), None, "develop is no story branch");
    // A tip already on the first parent's line had nothing of its own to merge.
    let behind = repo
        .find_commit(commit(None, "Sync", &[tip, root]))
        .unwrap();
    assert_eq!(
        story_merge(
            &repo,
            &behind,
            STORY,
            &none,
            &StoryTips(BTreeMap::from([(root, ("feat/SV-29-x".to_owned(), true))]))
        ),
        None
    );

    // Shared tip: B stacked at A's tip makes A's custom-subject merge unlinked, both ways.
    repo.reference("refs/heads/feat/SV-29-x", tip, true, "restore")
        .unwrap();
    repo.reference("refs/heads/feat/SV-31-stacked", tip, true, "stack")
        .unwrap();
    assert_eq!(link(&merge), Some(Err(TIP_SHARED)));
    let b_tips = StoryTips::for_story(home.path(), &repo, "SV-31", &none);
    assert_eq!(
        story_merge(&repo, &merge, "SV-31", &none, &b_tips),
        Some(Err(TIP_SHARED))
    );
    let default = repo
        .find_commit(commit(
            None,
            "Merge pull request #5 from acme/feat/SV-29-x",
            &[root, tip],
        ))
        .unwrap();
    assert!(
        matches!(
            link(&default),
            Some(Ok(MergeLink {
                number: Some(5),
                ..
            }))
        ),
        "the subject still links"
    );
    repo.find_reference("refs/heads/feat/SV-31-stacked")
        .unwrap()
        .delete()
        .unwrap();

    // The story's squash-style subject: a squash with one parent, a merge with two.
    let squash = repo
        .find_commit(commit(None, "SV-29: land it (#8)", &[root]))
        .unwrap();
    assert_eq!(
        link(&squash),
        Some(Ok(MergeLink {
            number: Some(8),
            number_reason: None,
            branch: None,
            by_tip: false,
            tip_recorded: false,
            merge: false
        }))
    );
    let titled = repo
        .find_commit(commit(None, "SV-29: land it (#8)", &[root, tip]))
        .unwrap();
    assert_eq!(
        link(&titled),
        Some(Ok(MergeLink {
            number: Some(8),
            number_reason: None,
            branch: Some("feat/SV-29-x".into()),
            by_tip: true,
            tip_recorded: true,
            merge: true,
        }))
    );
    // The same squash-style title on a merge whose second parent is no tip: a merge with no
    // branch, so the trail still reads M^1..M^2.
    let loose = commit(Some("refs/heads/feat/SV-29-loose"), "loose", &[root]);
    repo.find_reference("refs/heads/feat/SV-29-loose")
        .unwrap()
        .delete()
        .unwrap();
    let unnamed = repo
        .find_commit(commit(None, "SV-29: land it (#9)", &[root, loose]))
        .unwrap();
    assert_eq!(
        link(&unnamed),
        Some(Ok(MergeLink {
            number: Some(9),
            number_reason: None,
            branch: None,
            by_tip: false,
            tip_recorded: false,
            merge: true,
        }))
    );
}

/// Review r8 M1: one rule for the branch a merge subject names, in all three default forms;
/// a title that merely contains " from " names no branch.
#[test]
fn a_merge_subject_names_its_branch_in_each_default_form() {
    for (subject, branch) in [
        (
            "Merge pull request #5 from acme/feat/SV-29-x",
            Some("feat/SV-29-x"),
        ),
        (
            "Merge pull request #5 from acme/feat/SV-29-x\n\nbody",
            Some("feat/SV-29-x"),
        ),
        ("Merge branch 'develop'", Some("develop")),
        (
            "Merge branch 'feat/SV-29-x' into main",
            Some("feat/SV-29-x"),
        ),
        (
            "Merge remote-tracking branch 'origin/release'",
            Some("release"),
        ),
        ("Merge pull request #2769: move the data from A to B", None),
        ("Land the work from yesterday", None),
        ("SV-29: land it (#8)", None),
    ] {
        assert_eq!(subject_branch(subject), branch, "{subject}");
    }
}
