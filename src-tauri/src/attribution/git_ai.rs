// SPDX-License-Identifier: MIT
//! [`AttributionSource`] over git-ai's notes (`refs/notes/sense`, then `refs/notes/ai`;
//! `authorship/3.x`).
//!
//! It maps each line to the commit that introduced it with `git blame --porcelain`, then
//! reads that commit's note from CodeSense's own ref, falling back to the `ai` ref a user's
//! own or an older git-ai wrote. It never reads git-ai's private
//! `.git/ai` files and does not depend on `git-ai blame --json`, whose schema is
//! undocumented.

use super::authorship::{Authorship, Origin};
use super::ranges::{AttributionError, AttributionSource, Basis, LineRange, HUMAN_TOOL};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How to make git-ai finish the work it queued before a read: its daemon records commits
/// asynchronously, so a note written a moment ago may not exist yet.
#[derive(Debug, Clone)]
pub(crate) struct Drain {
    pub binary: PathBuf,
    /// The managed git-ai environment (its home and daemon sockets).
    pub env: Vec<(String, String)>,
}

/// The notes refs read, first match wins: CodeSense's local-only ref, then git-ai's default.
pub(crate) const NOTES_REFS: [&str; 2] = [super::managed::NOTES_REF, "ai"];

/// How long a drain may wait, in seconds.
const DRAIN_SECONDS: &str = "5";

#[derive(Debug, Clone)]
pub(crate) struct GitAiSource {
    git: String,
    drain: Option<Drain>,
}

/// Ambient variables that would point git at another repository, index or object store.
pub(crate) const REPO_OVERRIDES: [&str; 7] = [
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

fn local_read_env(command: &mut Command) {
    // Only the repository at `repo` decides anything.
    for var in REPO_OVERRIDES {
        command.env_remove(var);
    }
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        // Object-read errors below have stable Git diagnostics in this locale.
        .env("LC_ALL", "C");
}

impl GitAiSource {
    pub(crate) fn new(git: impl Into<String>) -> Self {
        Self {
            git: git.into(),
            drain: None,
        }
    }

    pub(crate) fn for_home(home: &Path) -> Self {
        let source = Self::default();
        let binary = home.join("bin/git-ai");
        if binary.is_file() {
            source.draining(Drain {
                binary,
                env: super::managed::Managed::for_codesense_home(home).daemon_env(),
            })
        } else {
            source
        }
    }

    /// Run `git-ai await` before each read.
    pub(crate) fn draining(mut self, drain: Drain) -> Self {
        self.drain = Some(drain);
        self
    }

    /// This source's git, run against `repo`, with nothing else set.
    pub(crate) fn command(&self, repo: &Path) -> Command {
        let mut command = Command::new(&self.git);
        command.arg("-C").arg(repo);
        command
    }

    pub(crate) fn git(
        &self,
        repo: &Path,
        args: &[&str],
    ) -> Result<std::process::Output, AttributionError> {
        let mut command = self.command(repo);
        command.args(args);
        local_read_env(&mut command);
        #[cfg(test)]
        crate::test_support::strip_attribution_env(&mut command);
        let output = command
            .stdin(Stdio::null())
            .output()
            .map_err(|error| AttributionError::Git(error.to_string()))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            // These are object-read failures, not the ordinary "no note found" case.
            if [
                "bad object",
                "unable to read",
                "cannot read",
                "could not read",
                "failed to read",
                "could not fetch",
                "missing blob",
                "no such path",
            ]
            .iter()
            .any(|message| error.contains(message))
            {
                return Err(AttributionError::ObjectsNotLocal);
            }
        }
        Ok(output)
    }

    pub(crate) fn drain(&self, repo: &Path) {
        let Some(drain) = &self.drain else { return };
        let mut command = super::detect::git_ai_command(&drain.binary);
        command
            .args(["await", "--timeout", DRAIN_SECONDS])
            .current_dir(repo)
            .envs(drain.env.iter().map(|(k, v)| (k, v)));
        local_read_env(&mut command);
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// The raw note on `commit` from the first of [`NOTES_REFS`] that has one: the one test of
    /// "this commit has a git-ai note here" every reader shares.
    pub(crate) fn raw_note(
        &self,
        repo: &Path,
        commit: &str,
    ) -> Result<Option<std::process::Output>, AttributionError> {
        for notes_ref in NOTES_REFS {
            let output = self.git(
                repo,
                &["notes", &format!("--ref={notes_ref}"), "show", commit],
            )?;
            if output.status.success() {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }

    /// The parsed note on `commit` ([`Self::raw_note`]); `None` when it has none or it cannot
    /// be read as an authorship note, so its lines read as unknown rather than failing the
    /// whole file.
    pub(crate) fn note(
        &self,
        repo: &Path,
        commit: &str,
    ) -> Result<Option<Authorship>, AttributionError> {
        let Some(output) = self.raw_note(repo, commit)? else {
            return Ok(None);
        };
        match Authorship::parse(&String::from_utf8_lossy(&output.stdout)) {
            Ok(note) => Ok(Some(note)),
            Err(error) => {
                log::warn!(target: "attribution", "note on {commit} unreadable: {error}");
                Ok(None)
            }
        }
    }
}

impl Default for GitAiSource {
    fn default() -> Self {
        Self::new(crate::commands::worktree::git_bin())
    }
}

/// One line of a file at the revision, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlamedLine {
    /// 1-based line in the file at the revision.
    pub line: u32,
    /// The commit that introduced it.
    pub commit: String,
    /// Its 1-based line in that commit.
    pub origin_line: u32,
    /// Its path in that commit.
    pub file: String,
}

/// `git blame --porcelain` output as one entry per final line.
pub(crate) fn parse_blame(porcelain: &str) -> Vec<BlamedLine> {
    let mut files: HashMap<&str, String> = HashMap::new();
    let mut lines = Vec::new();
    let mut current: Option<(&str, u32, u32)> = None;
    for text in porcelain.lines() {
        if text.starts_with('\t') {
            if let Some((commit, origin_line, line)) = current.take() {
                lines.push((commit, origin_line, line));
            }
        } else if let Some(header) = header(text) {
            current = Some(header);
        } else if let (Some((commit, _, _)), Some(file)) = (current, text.strip_prefix("filename "))
        {
            files.insert(commit, unquote_c(file));
        }
    }
    lines
        .into_iter()
        .map(|(commit, origin_line, line)| BlamedLine {
            line,
            commit: commit.to_owned(),
            origin_line,
            file: files.get(commit).cloned().unwrap_or_default(),
        })
        .collect()
}

/// A path as git prints it: plain, or in double quotes with C escapes (`\303\251`, `\"`,
/// `\\`, `\t`, `\n`) when it holds a non-ASCII byte, a control character, `"` or `\`.
pub(crate) fn unquote_c(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'0'..=b'3'
                if i + 2 < bytes.len()
                    && bytes[i + 1..=i + 2]
                        .iter()
                        .all(|b| (b'0'..=b'7').contains(b)) =>
            {
                out.push(
                    (bytes[i] - b'0') << 6 | (bytes[i + 1] - b'0') << 3 | (bytes[i + 2] - b'0'),
                );
                i += 3;
                continue;
            }
            b'a' => out.push(7),
            b'b' => out.push(8),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(11),
            b'f' => out.push(12),
            b'r' => out.push(b'\r'),
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `<40 hex> <origin line> <final line>[ <group size>]`.
fn header(text: &str) -> Option<(&str, u32, u32)> {
    let mut words = text.split(' ');
    let commit = words
        .next()
        .filter(|w| w.len() == 40 && w.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let origin_line = words.next()?.parse().ok()?;
    let line = words.next()?.parse().ok()?;
    Some((commit, origin_line, line))
}

fn range(line: u32, commit: String, origin: Option<Origin>) -> LineRange {
    let (tool, native_session_id, model, basis, trace_id) = match origin {
        Some(Origin::Agent(agent, trace_id)) => (
            Some(agent.tool),
            Some(agent.id),
            Some(agent.model).filter(|m| !m.is_empty() && m != "unknown"),
            Basis::Exact,
            trace_id,
        ),
        Some(Origin::Human) => (Some(HUMAN_TOOL.to_owned()), None, None, Basis::Exact, None),
        None => (None, None, None, Basis::Unknown, None),
    };
    LineRange {
        start: line,
        end: line,
        commit: Some(commit),
        trace_id,
        tool,
        native_session_id,
        model,
        basis,
    }
}

impl AttributionSource for GitAiSource {
    fn ranges(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
    ) -> Result<Vec<LineRange>, AttributionError> {
        let mut notes: HashMap<String, Option<Authorship>> = HashMap::new();
        self.blamed_ranges(repo, rev, path, |blamed| {
            if !notes.contains_key(&blamed.commit) {
                notes.insert(blamed.commit.clone(), self.note(repo, &blamed.commit)?);
            }
            Ok(notes[&blamed.commit]
                .as_ref()
                .and_then(|note| note.origin_of_line(&blamed.file, blamed.origin_line)))
        })
    }
}

impl GitAiSource {
    /// [`AttributionSource::ranges`] for a reader that only wants the lines of `rev` itself: the
    /// ranges of every other commit come back unknown without reading their notes, and `own` (the
    /// note on `rev`, read by the caller once) answers for `rev`. The ranges of `rev` are the
    /// same ones `ranges` returns; one `git notes show` per commit instead of one per blamed commit.
    pub(crate) fn own_ranges(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
        own: &Option<Authorship>,
    ) -> Result<Vec<LineRange>, AttributionError> {
        self.blamed_ranges(repo, rev, path, |blamed| {
            Ok(own
                .as_ref()
                .filter(|_| blamed.commit == rev)
                .and_then(|note| note.origin_of_line(&blamed.file, blamed.origin_line)))
        })
    }

    fn blamed_ranges(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
        mut origin_of: impl FnMut(&BlamedLine) -> Result<Option<Origin>, AttributionError>,
    ) -> Result<Vec<LineRange>, AttributionError> {
        if rev.starts_with('-') || path.starts_with('-') {
            return Err(AttributionError::Git(
                "revision or path is an option".into(),
            ));
        }
        self.drain(repo);
        let blame = self.git(
            repo,
            &[
                "blame",
                "--porcelain",
                "--ignore-revs-file=",
                rev,
                "--",
                path,
            ],
        )?;
        if !blame.status.success() {
            return Err(AttributionError::Git(
                String::from_utf8_lossy(&blame.stderr).trim().to_owned(),
            ));
        }
        let mut ranges: Vec<LineRange> = Vec::new();
        for blamed in parse_blame(&String::from_utf8_lossy(&blame.stdout)) {
            let origin = origin_of(&blamed)?;
            let next = range(blamed.line, blamed.commit, origin);
            match ranges.last_mut() {
                Some(last) if last.end + 1 == next.start && same_origin(last, &next) => {
                    last.end = next.end
                }
                _ => ranges.push(next),
            }
        }
        Ok(ranges)
    }
}

fn same_origin(a: &LineRange, b: &LineRange) -> bool {
    (
        &a.trace_id,
        &a.commit,
        &a.tool,
        &a.native_session_id,
        &a.model,
        a.basis,
    ) == (
        &b.trace_id,
        &b.commit,
        &b.tool,
        &b.native_session_id,
        &b.model,
        b.basis,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn a_renamed_drain_binary_runs_as_git_ai() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("git-ai-macos-arm64");
        super::super::testing::argv0_binary(&binary);
        GitAiSource::default()
            .draining(Drain {
                binary,
                env: vec![("HOME".into(), dir.path().to_string_lossy().into_owned())],
            })
            .drain(dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("argv0-calls"))
                .unwrap()
                .trim(),
            "await --timeout 5"
        );
    }

    use super::*;
    use std::process::Command;

    /// The notes and files `git-ai` 1.7.5 itself produced (`testdata/git_ai_1_7_5`; see its
    /// README for how): commit 0 has no note, commit 1 holds a Claude and a Codex range,
    /// commit 2 a human line.
    fn testdata() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/attribution/testdata/git_ai_1_7_5")
    }

    pub(crate) fn git(repo: &Path, args: &[&str]) -> String {
        let output = crate::test_support::strip_attribution_env(&mut Command::new("git"))
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn fixture_git_does_not_trace_agent_environment() {
        isolated_trace_probe("writer");
    }

    #[test]
    fn fixture_reader_does_not_trace_agent_environment() {
        isolated_trace_probe("reader");
    }

    fn isolated_trace_probe(seam: &str) {
        const CHILD: &str = "CODESENSE_FIXTURE_TRACE_PROBE";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            if seam == "writer" {
                let repo = rebuild();
                assert!(!git(repo.path(), &["rev-parse", "HEAD"]).trim().is_empty());
            } else {
                let output = GitAiSource::new("git")
                    .git(&root.join("repo"), &["rev-parse", "HEAD"])
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        if seam == "reader" {
            let repo = rebuild();
            std::fs::rename(repo.path(), root.path().join("repo")).unwrap();
        }
        let test_name = if seam == "writer" {
            "attribution::git_ai::tests::fixture_git_does_not_trace_agent_environment"
        } else {
            "attribution::git_ai::tests::fixture_reader_does_not_trace_agent_environment"
        };
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(CHILD, root.path())
            .env("GIT_AI", "fixture-owned")
            .env("GIT_AI_FUTURE", "fixture-owned")
            .env("GIT_TRACE2_EVENT_NESTING", "0")
            .env("GIT_TRACE2_FUTURE", "fixture-owned")
            .args(["--exact", test_name, "--nocapture"]);
        let targets = ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"]
            .map(|key| (key, root.path().join(format!("{key}.trace"))));
        for (key, target) in &targets {
            child.env(key, target);
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child did not run its exact regression: {output:?}"
        );
        for (key, target) in targets {
            assert!(
                !target.exists(),
                "{seam} fixture leaked {key} into Git: {}",
                target.display()
            );
        }
    }

    /// Rebuild the fixture repository: each commit's files, then its real note attached with
    /// `git notes --ref=ai add -F`.
    pub(crate) fn rebuild() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        for n in 0..3 {
            let tree = testdata().join(format!("c{n}"));
            for entry in std::fs::read_dir(&tree).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), repo.path().join(entry.file_name())).unwrap();
            }
            git(repo.path(), &["add", "."]);
            let message = std::fs::read_to_string(testdata().join(format!("c{n}.msg"))).unwrap();
            git(repo.path(), &["commit", "-q", "-m", message.trim()]);
            let note = testdata().join(format!("c{n}.note"));
            if note.exists() {
                git(
                    repo.path(),
                    &["notes", "--ref=ai", "add", "-F", note.to_str().unwrap()],
                );
            }
        }
        repo
    }

    fn read(repo: &Path, path: &str) -> Vec<LineRange> {
        // These older assertions focus on note fields; provenance is asserted independently below.
        GitAiSource::new("git")
            .ranges(repo, "HEAD", path)
            .unwrap()
            .into_iter()
            .map(|mut range| {
                assert!(range.commit.is_some());
                range.commit = None;
                range.trace_id = None;
                range
            })
            .collect()
    }

    pub(crate) const CLAUDE: &str = "11111111-2222-4333-8444-555555555555";
    pub(crate) const CODEX: &str = "019a1111-aaaa-7bbb-8ccc-ddddeeeeffff";

    fn agent(start: u32, end: u32, tool: &str, id: &str, model: Option<&str>) -> LineRange {
        LineRange {
            start,
            end,
            commit: None,
            trace_id: None,
            tool: Some(tool.into()),
            native_session_id: Some(id.into()),
            model: model.map(Into::into),
            basis: Basis::Exact,
        }
    }

    fn unknown(start: u32, end: u32) -> LineRange {
        LineRange {
            start,
            end,
            commit: None,
            trace_id: None,
            tool: None,
            native_session_id: None,
            model: None,
            basis: Basis::Unknown,
        }
    }

    #[test]
    fn fixture_traces_split_adjacent_lines_of_the_same_session() {
        let repo = rebuild();
        let ranges = GitAiSource::default()
            .ranges(repo.path(), "HEAD", "a.txt")
            .unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].trace_id.as_deref(), Some("t_754527e57d0ef8"));
        assert_eq!(ranges[1].trace_id.as_deref(), Some("t_48a574dff00e10"));
        assert_eq!(ranges[0].native_session_id, ranges[1].native_session_id);
        let human = GitAiSource::default()
            .ranges(repo.path(), "HEAD", "d.txt")
            .unwrap();
        assert!(human.iter().all(|r| r.trace_id.is_none()));
    }

    #[test]
    fn introducing_commits_survive_later_moves_and_split_ranges() {
        let repo = rebuild();
        let commits = git(repo.path(), &["rev-list", "--reverse", "HEAD"]);
        let commits: Vec<_> = commits.lines().collect();
        let source = GitAiSource::default();
        let ranges = source.ranges(repo.path(), "HEAD", "d.txt").unwrap();
        assert_eq!(ranges[0].commit.as_deref(), Some(commits[0]));
        assert_eq!(ranges[1].commit.as_deref(), Some(commits[2]));
        std::fs::rename(repo.path().join("a.txt"), repo.path().join("moved.txt")).unwrap();
        git(repo.path(), &["add", "a.txt", "moved.txt"]);
        git(repo.path(), &["commit", "-qm", "Move file"]);
        let moved = source.ranges(repo.path(), "HEAD", "moved.txt").unwrap();
        assert_eq!(moved[0].commit.as_deref(), Some(commits[1]));
        std::fs::write(repo.path().join("base.txt"), "base 1\nbase 2\nnew line\n").unwrap();
        git(repo.path(), &["add", "base.txt"]);
        git(repo.path(), &["commit", "-qm", "Append untracked line"]);
        let unknown = source.ranges(repo.path(), "HEAD", "base.txt").unwrap();
        assert!(
            unknown.len() >= 2,
            "adjacent unknown commits must stay separate"
        );
        assert_ne!(unknown[0].commit, unknown.last().unwrap().commit);
    }

    #[test]
    fn real_claude_and_codex_ranges_read_back_with_tool_session_and_model() {
        let repo = rebuild();
        // The note's model for Claude is "unknown" (git-ai had no transcript): none.
        assert_eq!(
            read(repo.path(), "a.txt"),
            [
                agent(1, 2, "claude", CLAUDE, None),
                agent(3, 3, "claude", CLAUDE, None)
            ]
        );
        assert_eq!(
            read(repo.path(), "b.txt"),
            [agent(1, 3, "codex", CODEX, Some("gpt-6.1-sol"))]
        );
    }

    #[test]
    fn a_human_line_is_exact_human_and_lines_in_a_commit_with_no_note_are_unknown() {
        let repo = rebuild();
        let human = LineRange {
            start: 3,
            end: 3,
            commit: None,
            trace_id: None,
            tool: Some(HUMAN_TOOL.into()),
            native_session_id: None,
            model: None,
            basis: Basis::Exact,
        };
        assert_eq!(read(repo.path(), "d.txt"), [unknown(1, 2), human]);
        assert_eq!(read(repo.path(), "base.txt"), [unknown(1, 2)]);
    }

    #[test]
    fn a_line_missing_from_the_attestation_is_unknown_not_human() {
        let repo = rebuild();
        // Replace c1's note with one that covers only a.txt line 1.
        let note = repo.path().join("note.txt");
        std::fs::write(
            &note,
            "a.txt\n  s_014f8695c01cc2::t_754527e57d0ef8 1\n---\n{\"schema_version\":\"authorship/3.0.0\",\"sessions\":{\"s_014f8695c01cc2\":{\"agent_id\":{\"tool\":\"claude\",\"id\":\"S\",\"model\":\"m\"}}}}",
        )
        .unwrap();
        let c1 = git(repo.path(), &["rev-list", "--reverse", "HEAD"])
            .lines()
            .nth(1)
            .unwrap()
            .to_owned();
        git(
            repo.path(),
            &[
                "notes",
                "--ref=ai",
                "add",
                "-f",
                "-F",
                note.to_str().unwrap(),
                &c1,
            ],
        );
        assert_eq!(
            read(repo.path(), "a.txt"),
            [agent(1, 1, "claude", "S", Some("m")), unknown(2, 3)]
        );
    }

    #[test]
    fn a_malformed_note_reads_as_unknown_rather_than_failing_the_file() {
        let repo = rebuild();
        let note = repo.path().join("note.txt");
        std::fs::write(&note, "garbage").unwrap();
        let c1 = git(repo.path(), &["rev-list", "--reverse", "HEAD"])
            .lines()
            .nth(1)
            .unwrap()
            .to_owned();
        git(
            repo.path(),
            &[
                "notes",
                "--ref=ai",
                "add",
                "-f",
                "-F",
                note.to_str().unwrap(),
                &c1,
            ],
        );
        assert_eq!(read(repo.path(), "b.txt"), [unknown(1, 3)]);
    }

    #[test]
    fn an_unreadable_path_or_option_like_input_is_an_error() {
        let repo = rebuild();
        let source = GitAiSource::new("git");
        assert!(source.ranges(repo.path(), "HEAD", "missing.txt").is_err());
        assert!(source.ranges(repo.path(), "--all", "a.txt").is_err());
        assert!(source.ranges(repo.path(), "HEAD", "--exec=x").is_err());
    }

    #[test]
    fn a_real_note_carries_no_prompt_text() {
        // The scripted Claude session below had a transcript with these words in it.
        let transcript =
            std::fs::read_to_string(testdata().join("prompt/transcript.jsonl")).unwrap();
        let note = std::fs::read_to_string(testdata().join("prompt.note")).unwrap();
        for secret in ["PROMPTSECRET", "ASSISTANTSECRET"] {
            assert!(
                transcript.contains(secret),
                "the fixture must hold {secret}"
            );
            assert!(!note.contains(secret), "the note leaked {secret}");
        }
        // And the note still parses and names the session, with the model the transcript gave.
        let parsed = Authorship::parse(&note).unwrap();
        let Some(Origin::Agent(agent, _)) = parsed.origin_of_line("y.txt", 1) else {
            panic!("y.txt line 1 is not attributed");
        };
        assert_eq!(agent.model, "claude-haiku-5-5");
    }

    #[test]
    fn blame_porcelain_maps_final_lines_to_origin_commit_line_and_path() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let text = format!(
            "{a} 1 1 2\nauthor X\nfilename old.txt\n\tone\n{a} 2 2\n\ttwo\n{b} 7 3 1\nauthor Y\nfilename new.txt\n\tthree\n"
        );
        let blamed = parse_blame(&text);
        let at = |line, commit: &str, origin, file: &str| BlamedLine {
            line,
            commit: commit.into(),
            origin_line: origin,
            file: file.into(),
        };
        assert_eq!(
            blamed,
            [
                at(1, &a, 1, "old.txt"),
                at(2, &a, 2, "old.txt"),
                at(3, &b, 7, "new.txt")
            ]
        );
    }

    #[test]
    fn gits_quoted_filenames_decode_to_the_path_a_note_names() {
        for (quoted, plain) in [
            ("plain.txt", "plain.txt"),
            ("\"caf\\303\\251.txt\"", "caf\u{e9}.txt"),
            ("\"we\\\"ird.txt\"", "we\"ird.txt"),
            ("\"back\\\\slash\"", "back\\slash"),
            ("\"tab\\there\\nline\"", "tab\there\nline"),
            ("\"\\346\\227\\245.txt\"", "\u{65e5}.txt"),
        ] {
            assert_eq!(unquote_c(quoted), plain, "{quoted}");
        }
    }

    /// Real git quotes `filename` in blame's porcelain output for these names; the note
    /// names the decoded path.
    #[test]
    fn lines_of_files_with_non_ascii_or_quoted_names_keep_their_attribution() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        let names = ["caf\u{e9}.txt", "we\"ird.txt"];
        for name in names {
            std::fs::write(repo.path().join(name), "one\ntwo\n").unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "names"]);
        let mut note = String::new();
        for name in names {
            note.push_str(&format!("{}\n  s_1::t_1 1-2\n", name));
        }
        note.push_str("---\n{\"schema_version\":\"authorship/3.0.0\",\"sessions\":{\"s_1\":{\"agent_id\":{\"tool\":\"claude\",\"id\":\"S\",\"model\":\"m\"}}}}");
        let note_file = repo.path().join(".git/note.txt");
        std::fs::write(&note_file, note).unwrap();
        git(
            repo.path(),
            &[
                "notes",
                "--ref=ai",
                "add",
                "-F",
                note_file.to_str().unwrap(),
            ],
        );
        for name in names {
            assert_eq!(
                read(repo.path(), name),
                [agent(1, 2, "claude", "S", Some("m"))],
                "{name}"
            );
        }
    }
}
