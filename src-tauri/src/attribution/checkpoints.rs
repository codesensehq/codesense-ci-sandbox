// SPDX-License-Identifier: MIT
//! Durable import of checkpoint identities; the engine owns the source logs.
use super::git_ai::GitAiSource;
use crate::session_usage::{trace_link::CheckpointRow, UsageStore};
use crate::shared::hash::sha256_hex;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[derive(Debug)]
pub(crate) struct Log {
    pub path: PathBuf,
    pub base: Option<String>,
    /// False for a seen live or archive directory that holds no `checkpoints.jsonl`: it is
    /// stored as a seen base with no records, so a miss under it is not "log not available".
    pub present: bool,
}
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub records: usize,
    pub malformed: usize,
    pub partial: usize,
    pub bytes_read: usize,
    pub logs_seen: usize,
    pub logs_new: usize,
    pub rows_stored: usize,
    pub session_mapping_rows_stored: usize,
}
/// Per-file commits survive a later failure; keep their counts with the error.
#[derive(Debug)]
pub(crate) struct HarvestError {
    pub stats: Stats,
    pub error: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<Option<String>, String> {
    #[cfg(test)]
    tests::record_git(repo, args);
    #[cfg(test)]
    if args.first() == Some(&"rev-list") {
        tests::PARENT_READS.with(|count| count.set(count.get() + 1));
    }
    let out = GitAiSource::default()
        .git(repo, args)
        .map_err(|e| e.to_string())?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
}
pub(crate) fn discover(repo: &Path, bases: &BTreeSet<String>) -> Result<Vec<Log>, String> {
    if bases.is_empty() {
        return Ok(Vec::new());
    }
    let mut roots = vec![repo.to_owned()];
    if let Some(list) = git(repo, &["worktree", "list", "--porcelain", "-z"])? {
        roots.extend(
            list.split('\0')
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(PathBuf::from),
        );
    }
    let mut directories = BTreeSet::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for flag in ["--absolute-git-dir", "--git-common-dir"] {
            if let Some(dir) = git(&root, &["rev-parse", flag])? {
                let dir = PathBuf::from(dir.trim_end_matches(&['\r', '\n'][..]));
                let dir = if dir.is_absolute() {
                    dir
                } else {
                    root.join(dir)
                };
                if let Ok(dir) = dir.canonicalize() {
                    directories.insert(dir);
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut logs = Vec::new();
    for dir in directories {
        for root in admin_log_roots(&dir, &mut subdirectories) {
            for base in bases {
                for label in [base.clone(), format!("old-{base}")] {
                    if let Some(log) = seen_log(&root.join(&label), base) {
                        if seen.insert(log.path.clone()) {
                            logs.push(log);
                        }
                    }
                }
            }
        }
    }
    Ok(logs)
}
/// All consumers share the attachment identity; move earlier path-keyed evidence before reading.
pub(crate) fn repository_key(
    repo: &Path,
    key: Option<&str>,
    store: &mut UsageStore,
) -> Result<Option<String>, String> {
    let key = match key {
        Some(key) => key.to_owned(),
        // Some local/partial clones have an origin that has no canonical source identity.
        // They still contribute Git facts; only their checkpoint import is unavailable.
        None => {
            let Ok(attachment) = crate::commands::attachments::existing_checkout_attachment(repo)
            else {
                return Ok(None);
            };
            let Some(key) = crate::commands::attachments::attachment_key(&attachment) else {
                return Ok(None);
            };
            key
        }
    };
    let mut previous = BTreeSet::from([repo.to_string_lossy().into_owned()]);
    if let Ok(path) = repo.canonicalize() {
        previous.insert(path.to_string_lossy().into_owned());
    }
    if let Some(dirs) = crate::commands::context_git_watch::GitDirs::resolve(repo) {
        let mut checkouts = Vec::new();
        if let Ok(main) = git2::Repository::open(&dirs.common_dir) {
            if let Some(root) = main.workdir() {
                checkouts.push(root.to_owned());
            }
        }
        if let Ok(entries) = std::fs::read_dir(dirs.common_dir.join("worktrees")) {
            for entry in entries.flatten() {
                if let Ok(gitdir) =
                    std::fs::read_to_string(std::fs::DirEntry::path(&entry).join("gitdir"))
                {
                    if let Some(root) = Path::new(gitdir.trim()).parent() {
                        checkouts.push(root.to_owned());
                    }
                }
            }
        }
        for root in checkouts {
            // libgit2 workdir() (the earlier story key) retains a trailing separator.
            previous.insert(root.join("").to_string_lossy().into_owned());
            previous.insert(root.to_string_lossy().into_owned());
            if let Ok(root) = root.canonicalize() {
                previous.insert(root.join("").to_string_lossy().into_owned());
                previous.insert(root.to_string_lossy().into_owned());
            }
        }
    }
    store.migrate_checkpoint_keys(&previous, &key)?;
    Ok(Some(key))
}

/// git-ai's log layout, shared by import discovery and the checkpoint watcher: each Git admin
/// dir (the common dir and every `worktrees/*`) holds `ai/working_logs` and
/// `ai/worktrees/<namespace>/working_logs`. Namespace names are discovered, never derived
/// from admin names. `children` lists a container (`<common>/worktrees`, `<admin>/ai/worktrees`)
/// and runs before any root under it is returned, so a caller can watch a container first.
pub(crate) fn working_log_roots(
    common: &Path,
    children: &mut dyn FnMut(&Path) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut admins = vec![common.to_owned()];
    admins.extend(children(&common.join("worktrees")));
    let mut roots = Vec::new();
    for admin in admins {
        roots.extend(admin_log_roots(&admin, children));
    }
    roots
}
fn admin_log_roots(admin: &Path, children: &mut dyn FnMut(&Path) -> Vec<PathBuf>) -> Vec<PathBuf> {
    let mut roots = vec![admin.join("ai/working_logs")];
    for namespace in children(&admin.join("ai/worktrees")) {
        roots.push(namespace.join("working_logs"));
    }
    roots
}
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| std::fs::DirEntry::path(&entry))
        .collect()
}
/// The log of a live or archive directory for `base`: its `checkpoints.jsonl` when there is one,
/// else the directory's would-be log path marked absent. `None` when the directory is not there.
fn seen_log(dir: &Path, base: &str) -> Option<Log> {
    let dir = Result::ok(dir.canonicalize()).filter(|dir| dir.is_dir())?;
    let path = dir.join("checkpoints.jsonl");
    let present = path.is_file();
    Some(Log {
        path,
        base: Some(base.to_owned()),
        present,
    })
}
/// Discover retained archives and live logs without a Git process for each worktree or label.
fn discover_all(repo: &Path) -> Vec<Log> {
    let Some(dirs) = crate::commands::context_git_watch::GitDirs::resolve(repo) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut logs = Vec::new();
    for root in working_log_roots(&dirs.common_dir, &mut subdirectories) {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let label = entry.file_name();
            let Some(label) = label.to_str() else {
                continue;
            };
            let base = label.strip_prefix("old-").unwrap_or(label);
            if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let dir = std::fs::DirEntry::path(&entry);
            if label.starts_with("old-") && !dir.join(".archived_at").is_file() {
                continue;
            }
            if let Some(log) = seen_log(&dir, base) {
                if seen.insert(log.path.clone()) {
                    logs.push(log);
                }
            }
        }
    }
    logs
}

pub(crate) fn harvest_all(
    repo: &Path,
    key: &str,
    store: &mut UsageStore,
) -> Result<Stats, HarvestError> {
    let mut stats = Stats::default();
    let result = (|| {
        let Some(key) = repository_key(repo, Some(key), store)? else {
            return Ok(());
        };
        harvest_logs(discover_all(repo), &key, store, &mut stats)
    })();
    match result {
        Ok(()) => Ok(stats),
        Err(error) => Err(HarvestError { stats, error }),
    }
}

#[derive(Deserialize)]
struct Agent {
    tool: String,
    id: String,
}
#[derive(Deserialize)]
struct Metadata {
    tool_use_id: Option<String>,
}
#[derive(Deserialize)]
struct LineAttribution {
    author_id: String,
}
#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    line_attributions: Vec<LineAttribution>,
}
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    entries: Vec<Entry>,
    kind: String,
    trace_id: Option<String>,
    agent_id: Option<Agent>,
    agent_metadata: Option<Metadata>,
}
fn bases(repo: &Path, commits: &BTreeSet<String>) -> BTreeSet<String> {
    let mut bases = commits.clone();
    if !commits.is_empty() {
        let mut args = vec!["rev-list", "--parents", "--no-walk"];
        args.extend(commits.iter().map(String::as_str));
        if let Ok(Some(lines)) = git(repo, &args) {
            bases.extend(
                lines
                    .lines()
                    .flat_map(|line| line.split_whitespace().skip(1))
                    .map(str::to_owned),
            );
        }
    }
    bases
}
const WINDOW_BYTES: usize = 4096;
/// Verify at most the last complete 4KiB, never a partial trailing record.
fn preceding_window(file: &mut std::fs::File, offset: u64) -> std::io::Result<Vec<u8>> {
    let length = offset.min(WINDOW_BYTES as u64) as usize;
    file.seek(SeekFrom::Start(offset - length as u64))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn retain_window(window: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= WINDOW_BYTES {
        window.clear();
        window.extend_from_slice(&bytes[bytes.len() - WINDOW_BYTES..]);
    } else {
        let overflow = (window.len() + bytes.len()).saturating_sub(WINDOW_BYTES);
        drop(window.drain(..overflow));
        window.extend_from_slice(bytes);
    }
}
pub(crate) fn harvest(
    repo: &Path,
    key: &str,
    store: &mut UsageStore,
    commits: &BTreeSet<String>,
) -> Result<Stats, String> {
    let Some(key) = repository_key(repo, Some(key), store)? else {
        return Ok(Stats::default());
    };
    let mut stats = Stats::default();
    harvest_logs(
        discover(repo, &bases(repo, commits))?,
        &key,
        store,
        &mut stats,
    )?;
    Ok(stats)
}

fn harvest_logs(
    logs: Vec<Log>,
    key: &str,
    store: &mut UsageStore,
    stats: &mut Stats,
) -> Result<(), String> {
    for log in logs {
        stats.logs_seen += 1;
        if !log.present {
            if let Some(base) = log.base.as_deref() {
                if store.checkpoint_base_seen(key, base)? {
                    continue;
                }
            }
            let cursor = crate::session_usage::trace_link::CheckpointCursor {
                length: 0,
                modified: Default::default(),
                offset: 0,
                identity: Default::default(),
                window_hash: sha256_hex(&[]),
            };
            store.store_checkpoint_file(
                key,
                log.base.as_deref(),
                &[],
                &log.path.to_string_lossy(),
                &cursor,
            )?;
            continue;
        }
        let Ok(mut file) = std::fs::File::open(&log.path) else {
            continue;
        };
        let Ok(meta) = file.metadata() else {
            continue;
        };
        let identity = format!("{}:{}", meta.dev(), meta.ino());
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|t| t.as_nanos().to_string())
            .unwrap_or_default();
        let path = log.path.to_string_lossy();
        let old = store.checkpoint_cursor(key, &path)?;
        if old.as_ref().is_some_and(|c| {
            c.identity == identity
                && c.length == meta.len()
                && !modified.is_empty()
                && c.modified == modified
        }) {
            continue;
        }
        let (start, mut window) = old
            .as_ref()
            .filter(|c| c.identity == identity && meta.len() >= c.offset)
            .and_then(|c| {
                preceding_window(&mut file, c.offset)
                    .ok()
                    .filter(|bytes| sha256_hex(bytes) == c.window_hash)
                    .map(|bytes| (c.offset, bytes))
            })
            .unwrap_or_default();
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut offset = start;
        let mut partial = false;
        let mut bytes = Vec::new();
        let mut rows = Vec::new();
        let mut readable = true;
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    stats.bytes_read += n;
                }
                Err(_) => {
                    readable = false;
                    break;
                }
            }
            if !bytes.ends_with(b"\n") {
                stats.partial += 1;
                partial = true;
                break;
            }
            offset += bytes.len() as u64;
            retain_window(&mut window, &bytes);
            let record = match serde_json::from_slice::<Record>(&bytes) {
                Ok(r) => r,
                Err(_) => {
                    stats.malformed += 1;
                    continue;
                }
            };
            if record.kind != "AiAgent" {
                continue;
            }
            match (
                record.agent_id,
                record.trace_id,
                record.agent_metadata.and_then(|m| m.tool_use_id),
            ) {
                (Some(a), Some(trace), Some(tool))
                    if !a.tool.is_empty()
                        && !a.id.is_empty()
                        && !trace.is_empty()
                        && !tool.is_empty() =>
                {
                    rows.push(CheckpointRow {
                        git_ai_session_id: record
                            .entries
                            .iter()
                            .flat_map(|e| &e.line_attributions)
                            .filter_map(|line| {
                                match super::authorship::Author::parse(&line.author_id) {
                                    Some(super::authorship::Author::Session(session, t))
                                        if t == trace =>
                                    {
                                        Some(session)
                                    }
                                    _ => None,
                                }
                            })
                            .next()
                            .unwrap_or_default(),
                        cli: a.tool,
                        native_session_id: a.id,
                        trace_id: trace,
                        tool_use_id: tool,
                    });
                    stats.records += 1;
                }
                _ => stats.malformed += 1,
            }
        }
        if readable {
            let cursor = crate::session_usage::trace_link::CheckpointCursor {
                length: meta.len(),
                modified,
                offset,
                identity,
                window_hash: sha256_hex(&window),
            };
            store.store_checkpoint_file(
                key,
                if partial { None } else { log.base.as_deref() },
                &rows,
                &path,
                &cursor,
            )?;
            stats.logs_new += usize::from(old.is_none());
            stats.rows_stored += rows.len();
            stats.session_mapping_rows_stored += rows
                .iter()
                .filter(|row| !std::string::String::is_empty(&row.git_ai_session_id))
                .count();
        }
    }
    Ok(())
}
/// Directory label = base; direct/parent association is inferred and unverified without a binary.
/// Persist only observed bases. Unrelated live logs are never evidence for this commit.
pub(crate) fn covered(
    repo: &Path,
    key: &str,
    commit: &str,
    store: &UsageStore,
) -> Result<bool, String> {
    for base in bases(repo, &BTreeSet::from([commit.to_owned()])) {
        if store.checkpoint_base_seen(key, &base)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::{git, rebuild};
    use crate::session_usage::record::SessionRecord;
    type Calls = std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>;
    fn probes() -> &'static std::sync::Mutex<std::collections::BTreeMap<PathBuf, Calls>> {
        static PROBES: std::sync::OnceLock<
            std::sync::Mutex<std::collections::BTreeMap<PathBuf, Calls>>,
        > = std::sync::OnceLock::new();
        PROBES.get_or_init(Default::default)
    }
    pub(crate) struct Probe {
        repo: PathBuf,
        calls: Calls,
    }
    impl Probe {
        pub(crate) fn new(repo: &Path) -> Self {
            let repo = repo.canonicalize().unwrap();
            let calls = Calls::default();
            probes().lock().unwrap().insert(repo.clone(), calls.clone());
            Self { repo, calls }
        }
        pub(crate) fn take(&self) -> Vec<Vec<String>> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            probes().lock().unwrap().remove(&self.repo);
        }
    }
    pub(super) fn record_git(repo: &Path, args: &[&str]) {
        let Ok(repo) = repo.canonicalize() else {
            return;
        };
        if let Some(calls) = probes().lock().unwrap().get(&repo) {
            calls
                .lock()
                .unwrap()
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        }
    }
    #[tokio::test]
    async fn exact_agent_requests_harvest_once_on_cold_and_warm_raw_cache_reads() {
        use crate::attribution::turns::tests::{checkpoint_log, query, record};
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        record(home.path(), repo.path(), "a.txt", "request-session");
        checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        let probe = Probe::new(repo.path());
        for _ in 0..2 {
            let answer = query(home.path(), "a.txt").await;
            assert_eq!(answer["ranges"][0]["sessionId"], "request-session");
            assert_eq!(answer["ranges"][0]["turn"]["toolUseId"], "toolu_01CCC");
            let calls = probe.take();
            assert_eq!(
                calls.iter().filter(|args| args[0] == "rev-list").count(),
                1,
                "{calls:?}"
            );
            assert_eq!(
                calls.iter().filter(|args| args[0] == "worktree").count(),
                1,
                "{calls:?}"
            );
        }
    }
    #[tokio::test]
    async fn requests_without_exact_agent_ranges_do_no_checkpoint_discovery_or_store_work() {
        use crate::attribution::{turns::tests::params, why_line};
        for kind in ["human", "unknown", "missing revision", "missing path"] {
            let repo = rebuild();
            let home = tempfile::tempdir().unwrap();
            why_line::tests::attach(home.path(), repo.path());
            let mut params = params("base.txt");
            match kind {
                "human" => {
                    params.path = "d.txt".into();
                    params.start = 3;
                    params.end = 3;
                }
                "missing revision" => params.rev = Some("missing-revision".into()),
                "missing path" => params.path = "missing.txt".into(),
                _ => {}
            }
            let probe = Probe::new(repo.path());
            let answer = why_line::resolve_at(
                home.path(),
                &crate::domain::Principal::from(1),
                &home.path().join("usage.sqlite"),
                &home.path().join("session-conversations"),
                &params,
                &crate::context_repo::caller_scope::CallerScope::Human,
                Default::default(),
            )
            .await
            .unwrap();
            match kind {
                "human" => assert_eq!(
                    answer["ranges"][0]["tool"],
                    super::super::ranges::HUMAN_TOOL
                ),
                "unknown" => assert_eq!(answer["ranges"][0]["basis"], "unknown"),
                _ => assert_eq!(answer["status"], "unavailable"),
            }
            assert!(probe.take().is_empty(), "{kind}");
            assert!(!home.path().join("usage.sqlite").exists(), "{kind}");
        }
    }
    #[test]
    fn empty_checkpoint_discovery_runs_no_git_processes() {
        let repo = rebuild();
        let probe = Probe::new(repo.path());
        assert!(super::discover(repo.path(), &BTreeSet::new())
            .unwrap()
            .is_empty());
        assert!(probe.take().is_empty());
    }
    thread_local! {
        pub(super) static PARENT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    #[test]
    fn many_commit_checkpoint_harvests_use_one_parent_process_even_when_warm() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let tree = git(repo.path(), &["rev-parse", "HEAD^{tree}"]);
        let mut parent = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let base = parent.clone();
        let mut commits = BTreeSet::new();
        for i in 0..20 {
            parent = git(
                repo.path(),
                &[
                    "commit-tree",
                    tree.trim(),
                    "-p",
                    &parent,
                    "-m",
                    &format!("checkpoint history {i}"),
                ],
            )
            .trim()
            .to_owned();
            commits.insert(parent.clone());
        }
        put_log(&repo.path().join(".git"), &format!("old-{base}"), CLAUDE);
        let older = git(repo.path(), &["rev-parse", "HEAD^"]);
        put_log(
            &repo.path().join(".git"),
            &format!("old-{}", older.trim()),
            CODEX,
        );
        for warm in [false, true] {
            PARENT_READS.with(|count| count.set(0));
            let stats = super::harvest(repo.path(), KEY, &mut store, &commits).unwrap();
            assert_eq!(PARENT_READS.with(std::cell::Cell::get), 1);
            assert_eq!(stats.bytes_read, if warm { 0 } else { CLAUDE.len() });
            assert!(get(&store, &row(CLAUDE)).is_some());
            assert!(get(&store, &row(CODEX)).is_none());
        }
    }
    const CLAUDE: &str = include_str!("testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl");
    const CODEX: &str = include_str!("testdata/git_ai_1_7_5_checkpoints/archived-codex.jsonl");
    const LINKED_CODEX: &str =
        include_str!("testdata/git_ai_1_7_5_worktree_layout/linked-archive-codex.jsonl");
    const KEY: &str = "github.com/acme/api";
    fn harvest(repo: &Path, key: &str, store: &mut UsageStore) -> Result<Stats, String> {
        let head = git(repo, &["rev-parse", "HEAD"]).trim().to_owned();
        super::harvest(repo, key, store, &[head].into())
    }
    fn discover(repo: &Path) -> Result<Vec<Log>, String> {
        let head = git(repo, &["rev-parse", "HEAD"]).trim().to_owned();
        super::discover(repo, &super::bases(repo, &[head].into()))
    }

    fn row(text: &str) -> CheckpointRow {
        let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        CheckpointRow {
            git_ai_session_id: String::new(),
            cli: v["agent_id"]["tool"].as_str().unwrap().into(),
            native_session_id: v["agent_id"]["id"].as_str().unwrap().into(),
            trace_id: v["trace_id"].as_str().unwrap().into(),
            tool_use_id: v["agent_metadata"]["tool_use_id"].as_str().unwrap().into(),
        }
    }
    fn put_log(git_dir: &Path, label: &str, text: &str) -> PathBuf {
        put_log_at(&git_dir.join("ai/working_logs"), label, text)
    }
    fn put_log_at(root: &Path, label: &str, text: &str) -> PathBuf {
        let p = root.join(label).join("checkpoints.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p.canonicalize().unwrap()
    }
    fn get(
        store: &UsageStore,
        r: &CheckpointRow,
    ) -> Option<crate::session_usage::trace_link::CheckpointLink> {
        store
            .checkpoint_link(&r.cli, &r.native_session_id, &r.trace_id)
            .unwrap()
    }
    #[test]
    fn linked_worktree_archive_is_discovered_and_harvested() {
        let repo = rebuild();
        let temp = tempfile::tempdir().unwrap();
        let member = temp.path().join("member");
        git(
            repo.path(),
            &["worktree", "add", "-b", "capture", member.to_str().unwrap()],
        );
        assert!(member.join(".git").is_file());
        let parent = git(&member, &["rev-parse", "HEAD^"]);
        let path = put_log_at(
            &repo
                .path()
                .join(".git/ai/worktrees/renamed-member/working_logs"),
            &format!("old-{}", parent.trim()),
            LINKED_CODEX,
        );
        let r = row(LINKED_CODEX);
        assert_eq!(r.cli, "codex");
        assert_eq!(r.trace_id, "t_1f478a747e06d0");
        assert_eq!(r.tool_use_id, "exec-e0ec0000-0000-4000-9000-000000000001");
        for root in [repo.path(), member.as_path()] {
            let logs = discover(root).unwrap();
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].path, path);
            assert_eq!(logs[0].base.as_deref(), Some(parent.trim()));
            let mut store = UsageStore::open(&temp.path().join(if root == repo.path() {
                "main.sqlite"
            } else {
                "member.sqlite"
            }))
            .unwrap();
            assert_eq!(harvest(root, KEY, &mut store).unwrap().records, 1);
            assert_eq!(get(&store, &r).unwrap().tool_use_id, r.tool_use_id);
        }
    }
    #[test]
    fn main_worktree_name_accepts_direct_and_archived_commit_and_parent_labels() {
        for rev in ["HEAD", "HEAD^"] {
            for prefix in ["", "old-"] {
                let repo = rebuild();
                let home = tempfile::tempdir().unwrap();
                let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
                let base = git(repo.path(), &["rev-parse", rev]);
                let path = put_log_at(
                    &repo
                        .path()
                        .join(".git/ai/worktrees")
                        .join(repo.path().file_name().unwrap())
                        .join("working_logs"),
                    &format!("{prefix}{}", base.trim()),
                    LINKED_CODEX,
                );
                let logs = discover(repo.path()).unwrap();
                assert_eq!(logs.len(), 1, "{rev} {prefix}");
                assert_eq!(logs[0].path, path);
                assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().records, 1);
                let head = git(repo.path(), &["rev-parse", "HEAD"]);
                assert!(covered(repo.path(), KEY, head.trim(), &store).unwrap());
            }
        }
    }
    #[test]
    fn marker_only_archive_covers_commit_but_unrelated_worktree_archive_does_not() {
        // A pruned archive (marker, no log) of a base of the commit was seen, so a miss under it
        // is "edit not recorded"; an archive of a base the commit does not have covers nothing.
        for pruned in [true, false] {
            let repo = rebuild();
            let home = tempfile::tempdir().unwrap();
            let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
            let base = git(
                repo.path(),
                &["rev-parse", if pruned { "HEAD^" } else { "HEAD~2" }],
            );
            let path = put_log_at(
                &repo.path().join(".git/ai/worktrees/member/working_logs"),
                &format!("old-{}", base.trim()),
                LINKED_CODEX,
            );
            if pruned {
                std::fs::remove_file(&path).unwrap();
                std::fs::write(path.parent().unwrap().join(".archived_at"), "1793224217\n")
                    .unwrap();
            }
            let seen = discover(repo.path()).unwrap();
            assert_eq!(seen.len(), usize::from(pruned));
            assert!(seen.iter().all(|log| !log.present));
            assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().records, 0);
            let head = git(repo.path(), &["rev-parse", "HEAD"]);
            assert_eq!(
                covered(repo.path(), KEY, head.trim(), &store).unwrap(),
                pruned
            );
            assert!(get(&store, &row(LINKED_CODEX)).is_none());
        }
    }
    #[test]
    fn discovery_uses_common_and_linked_git_dirs_and_deduplicates_them() {
        let repo = rebuild();
        let temp = tempfile::tempdir().unwrap();
        let member = temp.path().join("member");
        git(
            repo.path(),
            &["worktree", "add", "-b", "capture", member.to_str().unwrap()],
        );
        assert!(member.join(".git").is_file());
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let parent = git(repo.path(), &["rev-parse", "HEAD^"]);
        let a = put_log(
            &repo.path().join(".git"),
            &format!("old-{}", parent.trim()),
            CODEX,
        );
        let member_git = PathBuf::from(git(&member, &["rev-parse", "--absolute-git-dir"]).trim());
        let b = put_log(&member_git, head.trim(), CLAUDE);
        let c = put_log_at(
            &member_git.join("ai/worktrees/member/working_logs"),
            head.trim(),
            LINKED_CODEX,
        );
        for root in [repo.path(), member.as_path()] {
            let paths = discover(root).unwrap();
            assert_eq!(paths.len(), 3);
            assert_eq!(
                paths
                    .into_iter()
                    .map(|p| p.path)
                    .collect::<std::collections::BTreeSet<_>>(),
                [a.clone(), b.clone(), c.clone()].into()
            );
        }
    }
    #[test]
    fn real_records_harvest_in_both_record_orders_reopen_and_outlive_archives() {
        for record_first in [false, true] {
            let repo = rebuild();
            let home = tempfile::tempdir().unwrap();
            let db = home.path().join("usage.sqlite");
            let mut store = UsageStore::open(&db).unwrap();
            let c = row(CLAUDE);
            let x = row(CODEX);
            let mut record = SessionRecord::new("checkpoint-session");
            record.native_session_id = Some(c.native_session_id.clone());
            record.cli = Some(c.cli.clone());
            if record_first {
                store.write_session_record(&record).unwrap();
            }
            let head = git(repo.path(), &["rev-parse", "HEAD"]);
            let path = put_log(
                &repo.path().join(".git"),
                &format!("old-{}", head.trim()),
                &format!("{CODEX}{CLAUDE}"),
            );
            assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().records, 3);
            if !record_first {
                assert!(store
                    .read_session_record(&c.native_session_id)
                    .unwrap()
                    .is_none());
                store.write_session_record(&record).unwrap();
            }
            for text in CLAUDE.lines().chain(CODEX.lines()) {
                let r = row(text);
                let got = get(&store, &r).unwrap();
                assert_eq!(got.tool_use_id, r.tool_use_id);
                assert!(!got.ambiguous);
            }
            harvest(repo.path(), KEY, &mut store).unwrap();
            drop(store);
            let mut store = UsageStore::open(&db).unwrap();
            std::fs::remove_file(path).unwrap();
            harvest(repo.path(), KEY, &mut store).unwrap();
            assert_eq!(get(&store, &c).unwrap().tool_use_id, c.tool_use_id);
            assert_eq!(get(&store, &x).unwrap().tool_use_id, x.tool_use_id);
            assert!(covered(repo.path(), KEY, head.trim(), &store).unwrap());
            let count: i64 = rusqlite::Connection::open(&db)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 3);
            let fresh = UsageStore::open(&home.path().join("fresh.sqlite")).unwrap();
            assert!(!covered(repo.path(), KEY, head.trim(), &fresh).unwrap());
            assert!(get(&fresh, &c).is_none());
        }
    }
    #[test]
    fn only_direct_or_parent_base_labels_are_positive_coverage() {
        for (rev, expected) in [("HEAD", true), ("HEAD^", true), ("HEAD~2", false)] {
            let repo = rebuild();
            let home = tempfile::tempdir().unwrap();
            let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
            let label = git(repo.path(), &["rev-parse", rev]);
            put_log(&repo.path().join(".git"), label.trim(), "");
            harvest(repo.path(), KEY, &mut store).unwrap();
            let head = git(repo.path(), &["rev-parse", "HEAD"]);
            assert_eq!(
                covered(repo.path(), KEY, head.trim(), &store).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn malformed_complete_lines_do_not_hide_valid_lines_and_partial_tail_is_retried() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let first = CLAUDE.lines().next().unwrap();
        let p = put_log(
            &repo.path().join(".git"),
            head.trim(),
            &format!("{{broken}}\n{CODEX}{first}"),
        );
        let stats = harvest(repo.path(), KEY, &mut store).unwrap();
        assert_eq!((stats.malformed, stats.partial), (1, 1));
        assert!(get(&store, &row(CODEX)).is_some());
        assert!(get(&store, &row(first)).is_none());
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(p)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().partial, 0);
        assert!(get(&store, &row(first)).is_some());
    }
    #[test]
    fn conflicting_links_stay_ambiguous_and_never_overwrite_the_original_id() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let r = row(CODEX);
        let p = put_log(&repo.path().join(".git"), head.trim(), CODEX);
        harvest(repo.path(), KEY, &mut store).unwrap();
        let mut conflicting: serde_json::Value = serde_json::from_str(CODEX).unwrap();
        conflicting["agent_metadata"]["tool_use_id"] = serde_json::json!("call_conflicting");
        std::fs::write(&p, format!("{CODEX}{conflicting}\n")).unwrap();
        harvest(repo.path(), KEY, &mut store).unwrap();
        let link = get(&store, &r).unwrap();
        assert!(link.ambiguous);
        assert_eq!(link.tool_use_id, r.tool_use_id);
        std::fs::write(p, CODEX).unwrap();
        harvest(repo.path(), KEY, &mut store).unwrap();
        assert!(get(&store, &r).unwrap().ambiguous);
        let other = CheckpointRow {
            cli: "claude".into(),
            ..r.clone()
        };
        store
            .store_checkpoint_rows(KEY, None, &[other.clone()])
            .unwrap();
        assert!(!get(&store, &other).unwrap().ambiguous);
    }
    fn cursor_fixture() -> (tempfile::TempDir, tempfile::TempDir, UsageStore, PathBuf) {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let path = put_log(&repo.path().join(".git"), head.trim(), &CLAUDE.repeat(4));
        harvest(repo.path(), KEY, &mut store).unwrap();
        (repo, home, store, path)
    }
    fn replacement_records() -> (String, CheckpointRow, CheckpointRow) {
        let mut new: serde_json::Value =
            serde_json::from_str(CLAUDE.lines().next().unwrap()).unwrap();
        new["trace_id"] = serde_json::json!("t_new_before_old_offset");
        new["agent_metadata"]["tool_use_id"] = serde_json::json!("toolu_new");
        let new = new.to_string();
        let mut conflict: serde_json::Value =
            serde_json::from_str(CLAUDE.lines().next().unwrap()).unwrap();
        conflict["agent_metadata"]["tool_use_id"] = serde_json::json!("toolu_conflicting");
        let conflict = conflict.to_string();
        (
            format!("{new}\n{conflict}\n{}", CLAUDE.repeat(8)),
            row(&new),
            row(&conflict),
        )
    }
    #[test]
    fn larger_replacement_reads_prefix_mapping_and_conflict_before_old_offset() {
        let (repo, _home, mut store, path) = cursor_fixture();
        let old = store
            .checkpoint_cursor(KEY, path.to_str().unwrap())
            .unwrap()
            .unwrap();
        let (text, new, conflict) = replacement_records();
        assert!(text.len() as u64 > old.offset);
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, text).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        harvest(repo.path(), KEY, &mut store).unwrap();
        assert!(
            get(&store, &new).is_some(),
            "replacement prefix mapping skipped"
        );
        assert!(
            get(&store, &conflict).unwrap().ambiguous,
            "replacement prefix conflict skipped"
        );
    }
    #[test]
    fn same_inode_truncate_and_regrow_reads_prefix_before_old_offset() {
        use std::os::unix::fs::MetadataExt;
        let (repo, _home, mut store, path) = cursor_fixture();
        let inode = path.metadata().unwrap().ino();
        let (text, new, conflict) = replacement_records();
        std::fs::write(&path, text).unwrap();
        assert_eq!(path.metadata().unwrap().ino(), inode);
        harvest(repo.path(), KEY, &mut store).unwrap();
        assert!(
            get(&store, &new).is_some(),
            "regrown prefix mapping skipped"
        );
        assert!(
            get(&store, &conflict).unwrap().ambiguous,
            "regrown prefix conflict skipped"
        );

        // A shorter complete rewrite must replace the saved cursor, not replay forever.
        std::fs::write(&path, CODEX).unwrap();
        assert_eq!(path.metadata().unwrap().ino(), inode);
        assert_eq!(
            harvest(repo.path(), KEY, &mut store).unwrap().bytes_read,
            CODEX.len()
        );
        let shorter = store
            .checkpoint_cursor(KEY, path.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(shorter.offset, CODEX.len() as u64);
        assert_eq!(harvest(repo.path(), KEY, &mut store).unwrap().bytes_read, 0);
    }
    #[test]
    fn normal_append_parses_only_new_record_bytes() {
        use std::io::Write;
        let (repo, _home, mut store, path) = cursor_fixture();
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(CODEX.as_bytes())
            .unwrap();
        let stats = harvest(repo.path(), KEY, &mut store).unwrap();
        assert_eq!(stats.bytes_read, CODEX.len());
        assert_eq!(stats.malformed, 0);
        assert!(get(&store, &row(CODEX)).is_some());
    }
    #[test]
    fn replacement_cut_midline_restarts_without_counting_valid_record_as_malformed() {
        let (repo, _home, mut store, path) = cursor_fixture();
        let offset = store
            .checkpoint_cursor(KEY, path.to_str().unwrap())
            .unwrap()
            .unwrap()
            .offset as usize;
        let mut record: serde_json::Value = serde_json::from_str(CODEX.trim()).unwrap();
        record["replacement_padding"] = serde_json::json!("x".repeat(offset + 1));
        let text = format!("{}\n", record);
        assert!(text.len() > offset);
        assert_ne!(text.as_bytes()[offset - 1], b'\n');
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, text).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let stats = harvest(repo.path(), KEY, &mut store).unwrap();
        assert_eq!(
            stats.malformed, 0,
            "resume began inside a valid replacement record"
        );
        assert!(get(&store, &row(CODEX)).is_some());
    }
    #[test]
    fn stale_cursor_replay_keeps_all_trace_rows_unchanged() {
        use std::io::Write;
        let (repo, home, mut store, path) = cursor_fixture();
        let path = path.to_str().unwrap();
        let older = store.checkpoint_cursor(KEY, path).unwrap().unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(CODEX.as_bytes())
            .unwrap();
        harvest(repo.path(), KEY, &mut store).unwrap();
        let conn = rusqlite::Connection::open(home.path().join("usage.sqlite")).unwrap();
        let snapshot = || {
            let mut statement = conn.prepare("SELECT cli,native_session_id,trace_id,tool_use_id,ambiguous FROM checkpoint_trace ORDER BY cli,native_session_id,trace_id").unwrap();
            let rows = statement
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, bool>(4)?,
                    ))
                })
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        };
        let before = snapshot();
        store
            .store_checkpoint_file(KEY, None, &[], path, &older)
            .unwrap();
        let replay = harvest(repo.path(), KEY, &mut store).unwrap();
        assert_eq!(
            replay.bytes_read,
            CODEX.len(),
            "stale cursor must replay the append"
        );
        assert_eq!(
            snapshot(),
            before,
            "replay must not duplicate or change trace rows"
        );
    }
    #[test]
    fn sweep_archive_marker_live_append_replacement_and_reopen() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("usage.sqlite");
        let mut store = UsageStore::open(&db).unwrap();
        let base = git(repo.path(), &["rev-parse", "HEAD~2"]).trim().to_owned();
        let log = put_log(&repo.path().join(".git"), &format!("old-{base}"), CLAUDE);
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            0
        );
        std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            2
        );
        std::fs::remove_file(&log).unwrap();
        drop(store);
        let mut store = UsageStore::open(&db).unwrap();
        assert!(store.checkpoint_base_seen(KEY, &base).unwrap());
        assert!(get(&store, &row(CLAUDE)).is_some());
        let head = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let live = put_log(&repo.path().join(".git"), &head, CODEX);
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            1
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&live)
            .unwrap()
            .write_all(CLAUDE.as_bytes())
            .unwrap();
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .bytes_read,
            CLAUDE.len()
        );
        std::fs::remove_file(&live).unwrap();
        std::fs::write(&live, LINKED_CODEX).unwrap();
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            1
        );
        let probe = Probe::new(repo.path());
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .bytes_read,
            0
        );
        let calls = probe.take();
        assert!(calls.len() <= 1, "{calls:?}");
        assert!(calls
            .iter()
            .all(|a| a == &["rev-parse", "--git-common-dir"]));
    }
    #[test]
    fn sweep_discovers_isolated_and_linked_admin_logs_and_ambiguity() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        for root in [
            ".git/ai/worktrees/new/working_logs",
            ".git/worktrees/new/ai/working_logs",
            ".git/worktrees/new/ai/worktrees/isolated/working_logs",
        ] {
            let log = put_log_at(&repo.path().join(root), &format!("old-{head}"), CLAUDE);
            std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
        }
        let worktree_home = tempfile::tempdir().unwrap();
        let member = worktree_home.path().join("member");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "sweep-linked",
                member.to_str().unwrap(),
                "HEAD",
            ],
        );
        assert_eq!(
            super::harvest_all(&member, KEY, &mut store)
                .unwrap()
                .records,
            6
        );
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .bytes_read,
            0
        );
        let other = rebuild();
        put_log(
            &other.path().join(".git"),
            &head,
            &CLAUDE.replace(&row(CLAUDE).tool_use_id, "conflicting-tool"),
        );
        super::harvest_all(other.path(), KEY, &mut store).unwrap();
        assert!(get(&store, &row(CLAUDE)).unwrap().ambiguous);
    }
    #[tokio::test]
    async fn sweep_pruned_marker_is_seen_and_reads_as_edit_not_recorded() {
        // The startup-sweep entry path for case row 2: a marker-only archive of a base of the
        // commit is stored as seen, so the miss reads "edit not recorded", not "log unavailable".
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        crate::attribution::turns::tests::record(
            home.path(),
            repo.path(),
            "a.txt",
            "pruned-session",
        );
        let before = crate::attribution::turns::tests::query(home.path(), "a.txt").await;
        // The archive must belong to a base of the range's own commit (that commit or a parent),
        // which need not be HEAD.
        let commit = before["ranges"][0]["commit"].as_str().unwrap().to_owned();
        let log = put_log(&repo.path().join(".git"), &format!("old-{commit}"), CLAUDE);
        std::fs::remove_file(&log).unwrap();
        std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            0
        );
        assert!(covered(repo.path(), KEY, &commit, &store).unwrap());
        let after = crate::attribution::turns::tests::query(home.path(), "a.txt").await;
        assert_eq!(
            before["ranges"][0]["unavailable"]["turn"],
            crate::attribution::turns::LOG_UNAVAILABLE
        );
        assert_eq!(
            after["ranges"][0]["unavailable"]["turn"],
            crate::attribution::turns::TRACE_MISSING
        );
    }

    #[test]
    fn sweep_root_key_migration_preserves_pruned_coverage_and_clone_paths() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("usage.sqlite");
        let mut store = UsageStore::open(&db).unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let log = put_log(&repo.path().join(".git"), &head, CLAUDE);
        let legacy = repo.path().to_string_lossy();
        super::harvest(repo.path(), &legacy, &mut store, &[head.clone()].into()).unwrap();
        std::fs::remove_file(&log).unwrap();
        let linked_home = tempfile::tempdir().unwrap();
        let linked = linked_home.path().join("linked");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "legacy-linked",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        let parent = git(repo.path(), &["rev-parse", "HEAD^"]).trim().to_owned();
        let admin = PathBuf::from(git(&linked, &["rev-parse", "--absolute-git-dir"]).trim());
        let linked_log = put_log(&admin, &parent, CLAUDE);
        let linked_legacy = git2::Repository::open(&linked)
            .unwrap()
            .workdir()
            .unwrap()
            .to_owned();
        super::harvest(
            &linked,
            &linked_legacy.to_string_lossy(),
            &mut store,
            &[parent.clone()].into(),
        )
        .unwrap();
        std::fs::remove_file(&linked_log).unwrap();
        super::harvest_all(repo.path(), KEY, &mut store).unwrap();
        assert!(store.checkpoint_base_seen(KEY, &head).unwrap());
        assert!(store.checkpoint_base_seen(KEY, &parent).unwrap());
        assert!(store
            .checkpoint_cursor(KEY, &linked_log.to_string_lossy())
            .unwrap()
            .is_some());
        assert!(store
            .checkpoint_cursor(KEY, &log.to_string_lossy())
            .unwrap()
            .is_some());
        let other = rebuild();
        let other_log = put_log(&other.path().join(".git"), &head, CODEX);
        assert!(store
            .checkpoint_cursor(KEY, &other_log.to_string_lossy())
            .unwrap()
            .is_none());
        assert_eq!(
            super::harvest_all(other.path(), KEY, &mut store)
                .unwrap()
                .records,
            1
        );
        drop(store);
        let mut store = UsageStore::open(&db).unwrap();
        super::harvest_all(repo.path(), KEY, &mut store).unwrap();
        assert!(store.checkpoint_base_seen(KEY, &head).unwrap());
    }
    #[tokio::test]
    async fn sweep_completed_archive_resolves_why_line_after_pruning() {
        use crate::attribution::turns::tests::{query, replay_log, replay_repo};
        let repo = replay_repo(true);
        let home = tempfile::tempdir().unwrap();
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let mut record = SessionRecord::new("replay-session");
        record.cli = Some("claude".into());
        record.native_session_id = Some("c1a0de00-0000-4000-a000-000000000101".into());
        store.write_session_record(&record).unwrap();
        let log = replay_log(repo.path());
        std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
        assert_eq!(
            super::harvest_all(repo.path(), KEY, &mut store)
                .unwrap()
                .records,
            2
        );
        std::fs::remove_file(log).unwrap();
        drop(store);
        for (path, tool) in [
            ("edited.txt", "toolu_01CDE55Edit"),
            ("written.txt", "toolu_01CDE55Write"),
        ] {
            let answer = query(home.path(), path).await;
            assert_eq!(answer["ranges"][0]["sessionId"], "replay-session");
            assert_eq!(
                answer["ranges"][0]["nativeSessionId"],
                "c1a0de00-0000-4000-a000-000000000101"
            );
            assert_eq!(answer["ranges"][0]["turn"]["toolUseId"], tool);
        }
    }

    #[test]
    fn sweep_root_key_migration_is_transactional() {
        let repo = rebuild();
        let home = tempfile::tempdir().unwrap();
        let mut store = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let legacy = repo.path().to_string_lossy();
        store
            .store_checkpoint_rows(&legacy, Some("pruned-base"), &[])
            .unwrap();
        rusqlite::Connection::open(home.path().join("usage.sqlite")).unwrap().execute_batch("CREATE TRIGGER interrupt_migration BEFORE DELETE ON checkpoint_log_base BEGIN SELECT RAISE(ABORT, 'interrupted'); END;").unwrap();
        assert!(super::harvest_all(repo.path(), KEY, &mut store).is_err());
        assert!(store.checkpoint_base_seen(&legacy, "pruned-base").unwrap());
        assert!(!store.checkpoint_base_seen(KEY, "pruned-base").unwrap());
        rusqlite::Connection::open(home.path().join("usage.sqlite"))
            .unwrap()
            .execute_batch("DROP TRIGGER interrupt_migration")
            .unwrap();
        super::harvest_all(repo.path(), KEY, &mut store).unwrap();
        assert!(store.checkpoint_base_seen(KEY, "pruned-base").unwrap());
    }
}
