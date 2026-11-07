// SPDX-License-Identifier: MIT
//! Story-wide line shares: who wrote the lines the story's commits added.
//! Reads raw git-ai attribution only (cache first); no session, turn or story join.
use super::*;
use crate::attribution::ranges::{
    Basis, HISTORY_PAST_SHALLOW, HISTORY_PAST_SHALLOW_PARTIAL, HUMAN_TOOL, NO_LOCAL_COPY, OBJECTS_NOT_LOCAL, PATH_NOT_IN_REVISION, REPO_NOT_FOUND,
    REVISION_NOT_LOCAL,
};
use crate::attribution::why_line;
use crate::domain::Principal;
use codesense_pty_core::agent_cli::AgentCli;
use crate::session_usage::contribution;
use serde::Serialize;
use std::path::PathBuf;

/// AI lines per agent CLI, one bucket per listed CLI (keyed by its registry id) plus `other` for
/// any tool not listed (unknown names and unlisted CLIs). Serialises as `{<cli id>: n, ..., other: n}`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ByRuntime {
    listed: [u64; AgentCli::ALL.len()],
    other: u64,
}

impl ByRuntime {
    fn add(&mut self, tool: &str, n: u64) {
        match AgentCli::ALL.iter().position(|cli| cli.as_str() == tool) {
            Some(at) => self.listed[at] += n,
            None => self.other += n,
        }
    }
}

impl Serialize for ByRuntime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let ids = AgentCli::ALL.map(AgentCli::as_str);
        serializer.collect_map(ids.into_iter().zip(self.listed).chain([("other", self.other)]))
    }
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Ai {
    pub total: u64,
    pub by_runtime: ByRuntime,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Lines {
    pub ai: Ai,
    pub human: u64,
    pub unattributed: u64,
    pub total: u64,
}

impl Lines {
    pub(super) const ZERO: Lines = Lines {
        ai: Ai { total: 0, by_runtime: ByRuntime { listed: [0; AgentCli::ALL.len()], other: 0 } },
        human: 0,
        unattributed: 0,
        total: 0,
    };

    /// Count `n` lines written by `tool` (a runtime, or [`HUMAN_TOOL`]).
    fn tally(&mut self, tool: &str, n: u64) {
        if tool == HUMAN_TOOL {
            return self.human += n;
        }
        self.ai.by_runtime.add(tool, n);
        self.ai.total += n;
    }
}

/// The names a commit's lines are stored under in a contribution row: the four counts, the AI total
/// and one per runtime bucket (`ai.<cli id>`, `ai.other`).
const NUM_TOTAL: &str = "total";
const NUM_HUMAN: &str = "human";
const NUM_UNATTRIBUTED: &str = "unattributed";
const NUM_AI: &str = "ai";

fn runtime_num(bucket: &str) -> String {
    format!("{NUM_AI}.{bucket}")
}

impl Lines {
    /// Add another count (one commit's) to this one.
    pub(super) fn add(&mut self, other: &Lines) {
        self.ai.total += other.ai.total;
        for (mine, theirs) in self.ai.by_runtime.listed.iter_mut().zip(other.ai.by_runtime.listed) {
            *mine += theirs;
        }
        self.ai.by_runtime.other += other.ai.by_runtime.other;
        self.human += other.human;
        self.unattributed += other.unattributed;
        self.total += other.total;
    }

    /// Store as a row's numbers: the counts, or the reason they could not be counted.
    pub(super) fn store(outcome: &Outcome, row: &mut contribution::Contribution) {
        match outcome {
            Err(reason) => row.num("lines", None, reason),
            Ok(lines) => {
                for (name, n) in [
                    (NUM_TOTAL.to_owned(), lines.total),
                    (NUM_HUMAN.to_owned(), lines.human),
                    (NUM_UNATTRIBUTED.to_owned(), lines.unattributed),
                    (NUM_AI.to_owned(), lines.ai.total),
                    (runtime_num("other"), lines.ai.by_runtime.other),
                ]
                .into_iter()
                .chain(AgentCli::ALL.iter().zip(lines.ai.by_runtime.listed).map(|(cli, n)| (runtime_num(cli.as_str()), n)))
                {
                    row.num(&name, Some(n as f64), "");
                }
            }
        }
    }

    /// A row's counts, or the reason it has none.
    pub(super) fn load(row: &contribution::Contribution) -> Result<Lines, String> {
        let num = |name: &str| match row.nums.get(name) {
            Some(Some(v)) => Ok(*v as u64),
            _ => Err(Option::unwrap_or_else(row.reasons.get("lines").cloned(), || UNREADABLE.to_owned())),
        };
        let mut lines = Lines::ZERO;
        lines.total = num(NUM_TOTAL)?;
        lines.human = num(NUM_HUMAN)?;
        lines.unattributed = num(NUM_UNATTRIBUTED)?;
        lines.ai.total = num(NUM_AI)?;
        lines.ai.by_runtime.other = num(&runtime_num("other"))?;
        for (at, cli) in AgentCli::ALL.iter().enumerate() {
            lines.ai.by_runtime.listed[at] = num(&runtime_num(cli.as_str()))?;
        }
        Ok(lines)
    }
}

#[cfg(test)]
impl Lines {
    /// Lines per tool name (a listed CLI id; any other name, an unlisted CLI included, counts in `other`), plus the human and unattributed counts.
    pub(super) fn of(by_tool: &[(&str, u64)], human: u64, unattributed: u64) -> Self {
        let mut lines = Lines::default();
        for (tool, n) in by_tool {
            lines.tally(tool, *n);
        }
        lines.human = human;
        lines.unattributed = unattributed;
        lines.total = lines.ai.total + human + unattributed;
        lines
    }
}

/// Either every counted commit was attributed, or one static reason says why not.
pub(super) type Outcome = Result<Lines, &'static str>;

/// Totals over every commit of the story that landed (never the page), or every commit when none has. Merge
/// commits, binary files and submodule pointers add no lines; any commit whose attribution cannot
/// be read makes the whole answer unavailable, as does incomplete history (`unavailable.commits`).
///
/// Cost: per repository one org lookup, one cache open and one notes snapshot; per (commit, file)
/// one cache lookup (and its LRU touch) plus a clip of the cached ranges. A cold miss pays the one
/// blame the ranges path pays. Bounded by `git::SCAN_LIMIT` commits.
///
/// `budget` bounds the counting: past it the lines are unavailable (`OVER_BUDGET`), never 0.
pub(super) async fn totals(home: &Path, facts: &git::GitFacts, budget: Option<std::time::Duration>) -> Outcome {
    totals_with(home, facts, budget, None).await
}

/// [`totals`] that also stops, as over budget, once `cancel` is set (a hosted read past its deadline).
pub(super) async fn totals_with(
    home: &Path,
    facts: &git::GitFacts,
    budget: Option<std::time::Duration>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Outcome {
    super::count_git_read(home);
    if let Some(reason) = facts.unavailable.get("commits") {
        return Err(reason);
    }
    // With merged PRs, a commit read only through a branch that never merged did not land.
    let landed = Iterator::filter(<[git::Commit]>::iter(&facts.commits), |c| !(facts.landed_lines && c.unlanded));
    let commits = Iterator::collect::<Vec<_>>(Iterator::cloned(landed));
    let (home, candidates) = (home.to_owned(), facts.repo_paths.clone());
    let deadline = Option::map(budget, |budget| std::time::Instant::now() + budget);
    let stop = Stop { deadline, cancel };
    crate::commands::await_blocking("story_changes.lines", move || {
        Ok(count(&home, &commits, &candidates, &stop))
    })
    .await
    .unwrap_or(Err(UNREADABLE))
}

pub(super) const UNREADABLE: &str = "attribution unavailable for one or more commits";

/// The reason when the background budget ran out before every commit was counted.
pub(crate) const OVER_BUDGET: &str = "too large to compute in the background";

/// Whether a reason says the read failed (it may work later) rather than what the commit's
/// attribution is: a failure is retried with a wait, a reading is the commit's answer.
pub(super) fn is_transient(reason: &str) -> bool {
    matches!(reason, UNREADABLE | OVER_BUDGET | why_line::NOTES_CHANGED)
}

/// Whether a reason came from where the commit was looked for (a path that is gone, a clone that
/// is shallow or lacks the objects) rather than from the commit: another walk may find it readable.
pub(super) fn is_environmental(reason: &str) -> bool {
    matches!(
        reason,
        OBJECTS_NOT_LOCAL | REPO_NOT_FOUND | REVISION_NOT_LOCAL | HISTORY_PAST_SHALLOW | HISTORY_PAST_SHALLOW_PARTIAL | NO_LOCAL_COPY
    )
}

/// Reasons are shared with the answer's `unavailable` map, so they are a closed set.
fn reason(error: &str) -> &'static str {
    match error {
        why_line::NOTES_CHANGED => why_line::NOTES_CHANGED,
        OBJECTS_NOT_LOCAL => OBJECTS_NOT_LOCAL,
        REPO_NOT_FOUND => REPO_NOT_FOUND,
        REVISION_NOT_LOCAL => REVISION_NOT_LOCAL,
        PATH_NOT_IN_REVISION => PATH_NOT_IN_REVISION,
        HISTORY_PAST_SHALLOW => HISTORY_PAST_SHALLOW,
        HISTORY_PAST_SHALLOW_PARTIAL => HISTORY_PAST_SHALLOW_PARTIAL,
        NO_LOCAL_COPY => NO_LOCAL_COPY,
        _ => UNREADABLE,
    }
}

/// One reader per (principal, repository), opened on first use and kept for the whole call.
#[derive(Default)]
pub(super) struct Readers(Vec<((Principal, String), Result<why_line::RawReader, &'static str>)>);

impl Readers {
    fn reader_for(&mut self, home: &Path, principal: &Principal, remote: &str) -> Result<&mut why_line::RawReader, &'static str> {
        let at = match Iterator::position(&mut self.0.iter(), |((p, r), _)| p == principal && r == remote) {
            Some(at) => at,
            None => {
                let opened = why_line::RawReader::start_reads(home, principal, remote).map_err(|e| reason(&e));
                self.0.push(((principal.clone(), remote.to_owned()), opened));
                self.0.len() - 1
            }
        };
        self.0[at].1.as_mut().map_err(|reason| *reason)
    }
}

fn count(
    home: &Path,
    commits: &[git::Commit],
    candidates: &BTreeMap<String, Vec<PathBuf>>,
    stop: &Stop,
) -> Outcome {
    let (mut lines, mut counter) = (Lines::ZERO, Counter::new(home));
    for commit in commits {
        if over_budget(stop) {
            return Err(OVER_BUDGET);
        }
        lines.add(&counter.commit_lines_of(home, commit, candidates, stop)?);
    }
    Ok(lines)
}

/// What counting commits shares across them: the readers (one per repository) and the checkouts.
pub(super) struct Counter {
    readers: Readers,
    preparer: ranges::Preparer,
}

impl Counter {
    pub(super) fn new(home: &Path) -> Self {
        Self { readers: Readers(vec![]), preparer: ranges::Preparer::new(home) }
    }

    /// The lines one commit added, by who wrote them: the one count both the whole-story total and
    /// the per-commit rows are made of. A merge commit adds none; a commit whose attribution cannot
    /// be read is the reason, never a partial count.
    pub(super) fn commit_lines_of(
        &mut self,
        home: &Path,
        commit: &git::Commit,
        candidates: &BTreeMap<String, Vec<PathBuf>>,
        stop: &Stop,
    ) -> Outcome {
        super::count_commit_line_read(home);
        let mut lines = Lines::ZERO;
        match self.preparer.one(home, commit, candidates, true) {
            Ok(bindings) => commit_lines(home, &mut self.readers, &commit.sha, &bindings, &mut lines, stop)?,
            Err(error) if error == ranges::MERGE_COMMIT => {}
            Err(error) => return Err(reason(&error)),
        }
        Ok(lines)
    }
}

/// When counting stops: past a deadline, or once the read's cancel flag is set.
pub(super) struct Stop {
    pub(super) deadline: Option<std::time::Instant>,
    pub(super) cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

fn over_budget(stop: &Stop) -> bool {
    Option::is_some_and(stop.deadline, |deadline| {
        std::time::Instant::now() >= deadline
    }) || super::git::cancelled(&stop.cancel)
}

fn commit_lines(
    home: &Path,
    readers: &mut Readers,
    sha: &str,
    bindings: &[ranges::Binding],
    lines: &mut Lines,
    stop: &Stop,
) -> Result<(), &'static str> {
    // Every binding of one commit sees the same added windows.
    for (path, windows) in &bindings[0].2 {
        if over_budget(stop) {
            return Err(OVER_BUDGET);
        }
        let raw = first_readable(bindings, |(principal, remote, _)| {
            readers.reader_for(home, principal, remote)?.own_ranges_of(sha, path).map_err(|e| reason(&e))
        })?;
        let added: u64 = windows.iter().map(|&(from, to)| u64::from(to - from) + 1).sum();
        let classified_before = lines.ai.total + lines.human;
        for range in raw.iter().filter(|r| r.commit.as_deref() == Some(sha)) {
            let (Basis::Exact, Some(tool)) = (range.basis, range.tool.as_deref()) else {
                continue;
            };
            let overlap: u64 = windows
                .iter()
                .map(|&(from, to)| {
                    let (from, to) = (from.max(range.start), to.min(range.end));
                    if from <= to { u64::from(to - from) + 1 } else { 0 }
                })
                .sum();
            lines.tally(tool, overlap);
        }
        lines.unattributed += added - (lines.ai.total + lines.human - classified_before);
        lines.total += added;
    }
    Ok(())
}

/// The first binding that reads, else the most actionable error seen: a specific reason is never
/// replaced by a vague one from a later binding.
fn first_readable<T>(
    bindings: &[ranges::Binding],
    mut read: impl FnMut(&ranges::Binding) -> Result<T, &'static str>,
) -> Result<T, &'static str> {
    let mut best = UNREADABLE;
    for binding in bindings {
        match read(binding) {
            Ok(value) => return Ok(value),
            Err(error) if best == UNREADABLE || (best == NO_LOCAL_COPY && error != UNREADABLE) => {
                best = error
            }
            Err(_) => {}
        }
    }
    Err(best)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::git;
    use crate::attribution::testing::{logged_calls, logging_binary};

    fn note(path: &str, annotated: usize, tool: &str) -> String {
        let text = if tool == "human" {
            format!("{path}\n  h_x 1-{annotated}\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"humans\":{{\"h_x\":{{\"author\":\"Fixture <fixture@example.com>\"}}}}}}")
        } else {
            format!("{path}\n  s_x::t_x 1-{annotated}\n---\n{{\"schema_version\":\"authorship/3.0.0\",\"git_ai_version\":\"1.7.5\",\"base_commit_sha\":\"0000000000000000000000000000000000000000\",\"prompts\":{{}},\"sessions\":{{\"s_x\":{{\"agent_id\":{{\"tool\":\"{tool}\",\"id\":\"sess-{tool}\",\"model\":\"unknown\"}},\"human_author\":\"Fixture <fixture@example.com>\"}}}}}}")
        };
        assert!(crate::attribution::authorship::Authorship::parse(&text).is_ok());
        text
    }

    pub(in super::super) fn annotate(repo: &Path, path: &str, annotated: usize, tool: &str) {
        git(repo, &["notes", "--ref=ai", "add", "-m", &note(path, annotated, tool), "HEAD"]);
    }

    pub(in super::super) fn head(repo: &Path) -> String {
        git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Commit `lines` text lines in `path`; `tool` annotates the first `annotated` lines.
    pub(in super::super) fn add(repo: &Path, path: &str, lines: usize, subject: &str, tool: Option<(&str, usize)>) -> String {
        let text: String = (1..=lines).map(|n| format!("{path} {n}\n")).collect();
        std::fs::write(repo.join(path), text).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-qm", subject]);
        if let Some((tool, annotated)) = tool {
            annotate(repo, path, annotated, tool);
        }
        head(repo)
    }

    pub(in super::super) fn facts(repo: &Path, shas: &[String]) -> git::GitFacts {
        git::GitFacts {
            commits: shas
                .iter()
                .map(|sha| git::Commit { sha: sha.clone(), basis: "exact", ..Default::default() })
                .collect(),
            repo_paths: shas.iter().map(|s| (s.clone(), vec![repo.to_owned()])).collect(),
            ..Default::default()
        }
    }

    pub(in super::super) fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        crate::attribution::why_line::tests::attach(home.path(), repo.path());
        (home, repo)
    }

    /// One row per listed CLI, from the registry: a new CLI is covered without editing this test.
    #[test]
    fn every_listed_cli_has_its_own_bucket_and_unknown_names_land_in_other() {
        let mut lines = Lines::default();
        for (i, cli) in AgentCli::ALL.iter().enumerate() {
            lines.tally(cli.as_str(), i as u64 + 1);
        }
        lines.tally("not-a-registered-cli", 100);
        let json = serde_json::to_value(&lines.ai.by_runtime).unwrap();
        for (i, cli) in AgentCli::ALL.iter().enumerate() {
            assert_eq!(json[cli.as_str()], i as u64 + 1, "{cli} bucket");
        }
        assert_eq!(json["other"], 100);
        assert_eq!(json.as_object().unwrap().len(), AgentCli::ALL.len() + 1);
        assert_eq!(lines.ai.total, (1..=AgentCli::ALL.len() as u64).sum::<u64>() + 100);
    }

    pub(in super::super) type Build = fn(&Path) -> Vec<String>;

    /// One commit story per way a commit's lines can land, with the lines it must give.
    pub(in super::super) fn cases() -> Vec<(&'static str, Build, Lines)> {
        vec![
            ("claude exact", |r| vec![add(r, "a.txt", 3, "c", Some(("claude", 3)))], Lines::of(&[("claude", 3)], 0, 0)),
            ("codex exact, no session record", |r| vec![add(r, "a.txt", 2, "c", Some(("codex", 2)))], Lines::of(&[("codex", 2)], 0, 0)),
            ("opencode", |r| vec![add(r, "a.txt", 2, "c", Some(("opencode", 2)))], Lines::of(&[("opencode", 2)], 0, 0)),
            ("unknown agent tool", |r| vec![add(r, "a.txt", 1, "c", Some(("cursor", 1)))], Lines::of(&[("unknown-tool", 1)], 0, 0)),
            ("human note", |r| vec![add(r, "a.txt", 4, "c", Some(("human", 4)))], Lines::of(&[], 4, 0)),
            ("no note", |r| vec![add(r, "a.txt", 3, "c", None)], Lines::of(&[], 0, 3)),
            ("partly covered note", |r| vec![add(r, "a.txt", 3, "c", Some(("claude", 2)))], Lines::of(&[("claude", 2)], 0, 1)),
            ("associated lines stay unattributed and an edit counts only its own lines", |r| {
                let story = add(r, "a.txt", 3, "SV-29: story work, no note", None);
                std::fs::write(r.join("a.txt"), "a.txt 1\na.txt 2\na.txt 3\nnew 4\nnew 5\n").unwrap();
                git(r, &["commit", "-qam", "edit"]);
                annotate(r, "a.txt", 5, "claude");
                vec![story, head(r)]
            }, Lines::of(&[("claude", 2)], 0, 3)),
            ("binary file is not counted", |r| {
                std::fs::write(r.join("blob.bin"), [0u8, 1, 2, 0, 3]).unwrap();
                vec![add(r, "a.txt", 2, "c", Some(("claude", 2)))]
            }, Lines::of(&[("claude", 2)], 0, 0)),
            ("a submodule bump is not a line", |r| {
                std::fs::write(r.join("a.txt"), "a.txt 1\na.txt 2\n").unwrap();
                git(r, &["add", "a.txt"]);
                git(r, &["update-index", "--add", "--cacheinfo", "160000,1111111111111111111111111111111111111111,sub"]);
                git(r, &["commit", "-qm", "bump"]);
                annotate(r, "a.txt", 2, "claude");
                vec![head(r)]
            }, Lines::of(&[("claude", 2)], 0, 0)),
            ("a renamed file counts only the edited lines", |r| {
                add(r, "big.txt", 500, "big", Some(("claude", 500)));
                git(r, &["mv", "big.txt", "moved.txt"]);
                let text: String = (1..=500)
                    .map(|n| if n <= 10 { format!("edited {n}\n") } else { format!("big.txt {n}\n") })
                    .collect();
                std::fs::write(r.join("moved.txt"), text).unwrap();
                git(r, &["add", "."]);
                git(r, &["commit", "-qm", "move and edit"]);
                annotate(r, "moved.txt", 10, "claude");
                vec![head(r)]
            }, Lines::of(&[("claude", 10)], 0, 0)),
            ("merge commit is not counted", |r| {
                add(r, "base.txt", 1, "base", None);
                git(r, &["checkout", "-qb", "side"]);
                add(r, "side.txt", 1, "side", None);
                git(r, &["checkout", "-q", "main"]);
                add(r, "main.txt", 1, "main", None);
                git(r, &["merge", "--no-ff", "-qm", "Merge pull request #1 from acme/side", "side"]);
                vec![head(r)]
            }, Lines::of(&[], 0, 0)),
        ]
    }

    #[tokio::test]
    async fn every_case_lands_in_its_bucket_and_the_invariant_holds() {
        let rows = cases();
        for (name, build, expected) in rows {
            let (home, repo) = fixture();
            let shas = build(repo.path());
            let got = totals(home.path(), &facts(repo.path(), &shas), None).await;
            let got = got.unwrap_or_else(|reason| panic!("{name}: unavailable: {reason}"));
            assert_eq!(got, expected, "{name}");
            assert_eq!(got.ai.total + got.human + got.unattributed, got.total, "{name}");
        }
    }

    /// A spent budget makes the lines unavailable with its own reason, never a partial count or 0.
    #[tokio::test]
    async fn a_spent_budget_makes_lines_unavailable_not_zero() {
        let (home, repo) = fixture();
        let shas = vec![add(repo.path(), "a.txt", 3, "c", Some(("claude", 3)))];
        let facts = facts(repo.path(), &shas);
        let spent = totals(home.path(), &facts, Some(std::time::Duration::ZERO)).await;
        assert_eq!(spent.unwrap_err(), OVER_BUDGET);
        let roomy = totals(home.path(), &facts, Some(std::time::Duration::from_secs(600))).await;
        assert_eq!(roomy.unwrap(), Lines::of(&[("claude", 3)], 0, 0));
    }

    /// SV-ENG-121: a set cancel flag stops the count at the next commit, as over budget.
    #[tokio::test]
    async fn a_cancelled_count_is_over_budget_not_zero() {
        let (home, repo) = fixture();
        let shas = vec![add(repo.path(), "a.txt", 3, "c", Some(("claude", 3)))];
        let facts = facts(repo.path(), &shas);
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let stopped = totals_with(home.path(), &facts, None, Some(flag)).await;
        assert_eq!(stopped.unwrap_err(), OVER_BUDGET);
        let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counted = totals_with(home.path(), &facts, None, Some(open)).await;
        assert_eq!(counted.unwrap(), Lines::of(&[("claude", 3)], 0, 0));
    }

    #[tokio::test]
    async fn unreadable_attribution_is_unavailable_never_a_partial_count() {
        let (home, repo) = fixture();
        let sha = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
        let good = facts(repo.path(), &[sha.clone()]);
        assert!(totals(home.path(), &good, None).await.is_ok());
        let mut no_checkout = facts(repo.path(), &[sha.clone()]);
        no_checkout.repo_paths.clear();
        assert_eq!(totals(home.path(), &no_checkout, None).await.unwrap_err(), REPO_NOT_FOUND);
        let mut missing = facts(repo.path(), &[sha.clone(), "1".repeat(40)]);
        missing.repo_paths.insert("1".repeat(40), vec![repo.path().to_owned()]);
        assert_eq!(totals(home.path(), &missing, None).await.unwrap_err(), REVISION_NOT_LOCAL);
        let mut incomplete = facts(repo.path(), &[sha]);
        incomplete.unavailable.insert("commits", "history objects not in local copy");
        assert_eq!(totals(home.path(), &incomplete, None).await.unwrap_err(), "history objects not in local copy");
    }

    /// SV-PBF-296: with merged PRs, a commit read only through a never-merged branch is left out
    /// (even one whose attribution cannot be read); a landed commit that cannot be read still
    /// makes the whole answer unavailable; with nothing landed, every commit counts.
    #[tokio::test]
    async fn unmerged_side_commits_do_not_null_the_landed_lines() {
        let (home, repo) = fixture();
        let landed = add(repo.path(), "a.txt", 2, "c", Some(("claude", 2)));
        let side = "1".repeat(40);
        let mut story = facts(repo.path(), &[landed.clone(), side.clone()]);
        story.repo_paths.insert(side.clone(), vec![repo.path().to_owned()]);
        story.landed_lines = true;
        story.commits[1].unlanded = true;
        story.unavailable.insert("branches", "one or more story branches have unavailable commits");
        assert_eq!(totals(home.path(), &story, None).await.unwrap(), Lines::of(&[("claude", 2)], 0, 0));

        story.commits[1].unlanded = false;
        assert_eq!(totals(home.path(), &story, None).await.unwrap_err(), REVISION_NOT_LOCAL);

        story.commits[1].unlanded = true;
        story.landed_lines = false;
        assert_eq!(totals(home.path(), &story, None).await.unwrap_err(), REVISION_NOT_LOCAL);
    }

    #[test]
    fn reasons_are_a_closed_set_of_the_shared_error_strings() {
        for known in [why_line::NOTES_CHANGED, OBJECTS_NOT_LOCAL, REPO_NOT_FOUND, REVISION_NOT_LOCAL, PATH_NOT_IN_REVISION, HISTORY_PAST_SHALLOW, HISTORY_PAST_SHALLOW_PARTIAL, NO_LOCAL_COPY] {
            assert_eq!(reason(known), known);
        }
        assert_eq!(reason("git: something odd"), UNREADABLE);
    }

    #[test]
    fn every_binding_is_tried_and_the_most_actionable_error_wins() {
        let binding = |n: u64| (Principal::from(n), format!("host/o/r{n}"), Default::default());
        let bindings = [binding(1), binding(2), binding(3)];
        let mut seen = Vec::new();
        let got = first_readable(&bindings, |b| {
            seen.push(b.1.clone());
            if b.1.ends_with('3') { Ok(7) } else { Err(NO_LOCAL_COPY) }
        });
        assert_eq!(got, Ok(7));
        assert_eq!(seen.len(), 3, "failed bindings fall through to the next");
        let got: Result<u8, _> = first_readable(&bindings, |b| {
            Err(if b.1.ends_with('2') { why_line::NOTES_CHANGED } else { NO_LOCAL_COPY })
        });
        assert_eq!(got, Err(why_line::NOTES_CHANGED), "a vague later error never hides a specific one");
        let got: Result<u8, _> = first_readable(&bindings, |b| {
            Err(if b.1.ends_with('1') { NO_LOCAL_COPY } else { UNREADABLE })
        });
        assert_eq!(got, Err(NO_LOCAL_COPY), "a vague later error never replaces a known one");
    }

    #[tokio::test]
    async fn totals_cover_all_commits_and_a_warm_call_does_not_blame_again() {
        let (home, repo) = fixture();
        let one = add(repo.path(), "a.txt", 2, "one", Some(("claude", 2)));
        let two = add(repo.path(), "b.txt", 3, "two", Some(("codex", 3)));
        let three = add(repo.path(), "c.txt", 1, "three", None);
        let log = home.path().join("adapter-await.log");
        logging_binary(&home.path().join("bin/git-ai"), &log, "exit 0");
        let all = facts(repo.path(), &[one, two, three]);
        let cold = totals(home.path(), &all, None).await.unwrap();
        assert_eq!(cold, Lines::of(&[("claude", 2), ("codex", 3)], 0, 1));
        let reads = logged_calls(&log).len();
        assert_eq!(reads, 3, "cold: one blame per (commit, file)");
        assert_eq!(totals(home.path(), &all, None).await.unwrap(), cold);
        assert_eq!(logged_calls(&log).len(), reads, "warm: served from the cache");
    }

    #[tokio::test]
    async fn a_notes_write_during_the_scan_is_absorbed_and_endless_writes_null_lines() {
        let (home, repo) = fixture();
        let root = add(repo.path(), "root.txt", 1, "root", None);
        let one = add(repo.path(), "a.txt", 2, "one", Some(("claude", 2)));
        let log = home.path().join("adapter-await.log");
        let flag = home.path().join("notes-moved");
        let once = format!(
            "if [ ! -f \"{flag}\" ]; then touch \"{flag}\"; git -c user.name=f -c user.email=f@example.com notes --ref=ai add -f -m moved {root}; fi",
            flag = flag.display()
        );
        logging_binary(&home.path().join("bin/git-ai"), &log, &once);
        let all = facts(repo.path(), &[one]);
        assert_eq!(totals(home.path(), &all, None).await.unwrap(), Lines::of(&[("claude", 2)], 0, 0));
        assert_eq!(logged_calls(&log).len(), 2, "that file was read again, once");
        // A repository whose notes move on every read never settles.
        std::fs::remove_file(home.path().join("adapter-await.log")).unwrap();
        let always = format!(
            "git -c user.name=f -c user.email=f@example.com notes --ref=ai add -f -m \"m$RANDOM$RANDOM\" {root}"
        );
        logging_binary(&home.path().join("bin/git-ai"), &log, &always);
        let other = add(repo.path(), "b.txt", 1, "two", Some(("codex", 1)));
        assert_eq!(totals(home.path(), &facts(repo.path(), &[other]), None).await.unwrap_err(), why_line::NOTES_CHANGED);
        assert_eq!(logged_calls(&log).len(), 3, "three reads, then the reason");
    }

    #[tokio::test]
    async fn a_commit_that_moves_many_files_counts_only_their_edited_lines() {
        let (home, repo) = fixture();
        // A repository's own `diff.renameLimit` must not shrink what totals can follow: 210 moved
        // files against 20 would be skipped by libgit2 unless the limit is set explicitly.
        git(repo.path(), &["config", "diff.renameLimit", "20"]);
        std::fs::create_dir(repo.path().join("old")).unwrap();
        for n in 0..210 {
            let text: String = (1..=10).map(|l| format!("file{n} line{l}\n")).collect();
            std::fs::write(repo.path().join(format!("old/f{n}.txt")), text).unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "many files"]);
        git(repo.path(), &["mv", "old", "new"]);
        let mut attestation = String::new();
        for n in 0..210 {
            let text: String = (1..=10)
                .map(|l| if l == 1 { format!("edited {n}\n") } else { format!("file{n} line{l}\n") })
                .collect();
            std::fs::write(repo.path().join(format!("new/f{n}.txt")), text).unwrap();
            attestation.push_str(&format!("new/f{n}.txt\n  s_x::t_x 1\n"));
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "move all, edit one line each"]);
        let json = "{\"schema_version\":\"authorship/3.0.0\",\"git_ai_version\":\"1.7.5\",\"base_commit_sha\":\"0000000000000000000000000000000000000000\",\"prompts\":{},\"sessions\":{\"s_x\":{\"agent_id\":{\"tool\":\"claude\",\"id\":\"sess-claude\",\"model\":\"unknown\"},\"human_author\":\"Fixture <fixture@example.com>\"}}}";
        git(repo.path(), &["notes", "--ref=ai", "add", "-m", &format!("{attestation}---\n{json}"), "HEAD"]);
        let got = totals(home.path(), &facts(repo.path(), &[head(repo.path())]), None).await.unwrap();
        assert_eq!(got, Lines::of(&[("claude", 210)], 0, 0), "a configured diff.renameLimit does not stop moves from being followed");
    }
}
