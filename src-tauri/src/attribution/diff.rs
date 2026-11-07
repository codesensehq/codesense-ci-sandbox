// SPDX-License-Identifier: MIT
//! Read a configured source revision again on every diff-tab read. No fetching.
use crate::context_repo::caller_scope::CallerScope;
use crate::domain::Principal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Params {
    pub repo: String,
    pub rev: String,
    pub path: String,
    pub story_id: Option<String>,
    pub range: Option<AfterRange>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AfterRange {
    pub start: usize,
    pub end: usize,
    pub side: Option<String>,
}

fn state(status: &str, reason: impl ToString) -> Value {
    json!({"status":status,"reason":reason.to_string()})
}

pub(crate) fn read(home: &Path, principal: &Principal, scope: &CallerScope, params: &Params) -> Value {
    match read_inner(home, principal, scope, params) {
        Ok(value) => value,
        Err(reason) => state("unavailable", reason),
    }
}

/// Whether a live worktree path, and every symlink on the way, stays under the canonical root.
fn stays_inside(root: &Path, relative: &str) -> bool {
    let Ok(root) = root.canonicalize() else { return false };
    let full = root.join(relative);
    let mut probe = full.as_path();
    loop {
        match probe.canonicalize() {
            Ok(resolved) => return resolved.starts_with(&root),
            Err(_) if probe.symlink_metadata().is_ok() => return false,
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent,
                None => return false,
            },
        }
    }
}

fn read_inner(home: &Path, principal: &Principal, scope: &CallerScope, params: &Params) -> Result<Value, String> {
    if std::string::String::is_empty(&params.path)
        || params.path.contains('\0')
        || Path::new(&params.path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Ok(state("refused", "Use a repository-relative file path"));
    }
    if let Some(range) = &params.range {
        if range.start == 0
            || range.end < range.start
            || range.side.as_deref().is_some_and(|side| side != "after")
        {
            return Ok(state("refused", "Use a 1-based inclusive after-side range"));
        }
    }
    let worktree = params.rev == "WORKTREE";
    if worktree
        && params
            .story_id
            .as_deref()
            .is_none_or(|id| std::primitive::str::is_empty(id.trim()))
    {
        return Ok(state("refused", "WORKTREE requires storyId"));
    }
    if !worktree
        && (!(4..=40).contains(&params.rev.len())
            || !std::iter::Iterator::all(&mut std::primitive::str::bytes(&params.rev), |c| c.is_ascii_hexdigit()))
    {
        return Ok(state(
            "refused",
            "Use a committed full or short SHA, or WORKTREE with storyId",
        ));
    }
    let Some(configured) = super::why_line::configured_repo(home, principal, &params.repo)? else {
        return Ok(state("unavailable", "Source repository is not configured"));
    };
    if configured.attachment != crate::mcp::setup_tools::AttachmentStatus::Attached {
        return Ok(state(
            "unavailable",
            "Source repository is not attached on this Mac",
        ));
    }
    let root = configured
        .local_path
        .ok_or("Source repository is not attached")?;
    let local = git2::Repository::open(&root).map_err(|e| e.to_string())?;
    let (path, rev) = if worktree {
        let records = crate::install::worktree_index::find_by_story_id(
            home,
            principal,
            params.story_id.as_deref().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let (visible, hidden): (Vec<_>, Vec<_>) = records
            .into_iter()
            .partition(|record| scope.allows(&record.story_source_path));
        if std::vec::Vec::is_empty(&visible) && !std::vec::Vec::is_empty(&hidden) {
            return Ok(state("refused", "This story's worktree is outside this view"));
        }
        let matches: Vec<_> = visible
            .into_iter()
            .filter(|record| {
                record
                    .source_repo
                    .remote_key()
                    .eq_ignore_ascii_case(&configured.remote_key)
                    && record.code_repo_path.is_dir()
            })
            .collect();
        if std::vec::Vec::is_empty(&matches) {
            return Ok(state("worktreeRemoved", "The worktree was removed"));
        }
        if matches.len() != 1 {
            return Ok(state(
                "refused",
                "More than one local worktree matches this story",
            ));
        }
        (matches[0].code_repo_path.clone(), params.rev.clone())
    } else {
        // A hex revision is a commit id; refs never stand in for it.
        let commit = match local.find_commit_by_prefix(&params.rev) {
            Ok(commit) => git2::Commit::id(&commit).to_string(),
            Err(error) if error.code() == git2::ErrorCode::Ambiguous => {
                return Ok(state("refused", "This short SHA matches more than one object; use the full SHA"))
            }
            Err(_) if params.rev.len() == 40 => {
                return Ok(
                    json!({"status":"commitNotLocal","reason":"This commit isn't on this Mac yet","fetchCommand":format!("git fetch origin {}",params.rev)}),
                )
            }
            Err(_) => return Ok(state("refused", "This short SHA isn't on this Mac; use the full SHA")),
        };
        (Path::new(&root).to_path_buf(), commit)
    };
    read_revision(&path, &rev, params)
}

fn read_revision(root: &Path, rev: &str, params: &Params) -> Result<Value, String> {
    use crate::commands::git_sync;
    let live = rev == "WORKTREE";
    if live && !stays_inside(root, &params.path) {
        return Ok(state("refused", "The path leaves the story worktree"));
    }
    let files = if live {
        git_sync::source_worktree_changeset_impl(root, None)
    } else {
        git_sync::source_commit_changeset_impl(root, rev)
    }?;
    let file = std::iter::Iterator::find(&mut files.iter(), |file| file.path == params.path);
    if file.is_none() {
        let present = if live {
            root.join(&params.path).is_file()
        } else {
            git2::Repository::open(root).is_ok_and(|repo| {
                repo.revparse_single(rev)
                    .and_then(|object| object.peel_to_tree())
                    .is_ok_and(|tree| tree.get_path(Path::new(&params.path)).is_ok())
            })
        };
        if !present {
            return Ok(state(
                "unavailable",
                "The file is not present at this revision",
            ));
        }
    }
    let old_path = file.and_then(|file| file.rename_old.as_deref());
    let redline = if live {
        git_sync::source_file_redline_impl(root, &params.path, old_path, None)
    } else {
        git_sync::commit_file_redline_impl(root, rev, &params.path, old_path)
    }?;
    if let Some(range) = &params.range {
        if redline.binary
            || std::string::String::is_empty(&redline.after)
            || range.end > redline.after.lines().count()
        {
            return Ok(state(
                "refused",
                "The after-side range is not present in this file",
            ));
        }
    }
    Ok(json!({"status":"opened","resolvedRev":rev,"redline":redline,"files":files}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::{git, rebuild};

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Params) {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        super::super::why_line::tests::attach(home.path(), repo.path());
        let rev = git(repo.path(), &["rev-parse", "HEAD"]);
        (
            home,
            repo,
            Params {
                repo: "acme/api".into(),
                rev: rev.trim().into(),
                path: "a.txt".into(),
                story_id: None,
                range: Some(AfterRange {
                    start: 1,
                    end: 1,
                    side: Some("after".into()),
                }),
            },
        )
    }
    #[test]
    fn committed_and_short_sha_use_local_first_parent_content() {
        let (home, repo, mut params) = fixture();
        std::fs::write(repo.path().join("a.txt"), "after one\nafter two\n").unwrap();
        git(repo.path(), &["add", "a.txt"]);
        git(repo.path(), &["commit", "-qm", "edit"]);
        params.rev = git(repo.path(), &["rev-parse", "--short", "HEAD"])
            .trim()
            .into();
        let result = read(home.path(), &Principal::from(1), &CallerScope::Human, &params);
        assert_eq!(result["status"], "opened");
        assert_eq!(result["redline"]["after"], "after one\nafter two\n");
        assert_ne!(result["redline"]["before"], result["redline"]["after"]);
        std::fs::write(repo.path().join("a.txt"), "uncommitted").unwrap();
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["redline"]["after"],
            "after one\nafter two\n"
        );
    }
    #[test]
    fn missing_commit_has_typed_fetch_advice_and_attachment_is_revalidated() {
        let (home, repo, mut params) = fixture();
        params.rev = "a".repeat(40);
        let result = read(home.path(), &Principal::from(1), &CallerScope::Human, &params);
        assert_eq!(result["status"], "commitNotLocal");
        assert_eq!(
            result["fetchCommand"],
            format!("git fetch origin {}", params.rev)
        );
        drop(repo);
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "unavailable"
        );
    }
    #[test]
    fn refuses_old_or_missing_after_ranges_and_unbound_live_work() {
        let (home, _repo, mut params) = fixture();
        params.range.as_mut().unwrap().side = Some("before".into());
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "refused"
        );
        params.range = None;
        let valid = params.clone();
        for (field, value, status) in [("path", "/tmp/outside.rs", "refused"), ("repo", "unknown/repo", "unavailable"), ("rev", "HEAD", "refused")] {
            let mut rejected = valid.clone();
            match field { "path" => rejected.path = value.into(), "repo" => rejected.repo = value.into(), _ => rejected.rev = value.into() }
            assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &rejected)["status"], status);
        }
        params.rev = "WORKTREE".into();
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "refused"
        );
        params.story_id = Some("SV-X".into());
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "worktreeRemoved"
        );
    }
    #[test]
    fn rename_uses_the_recorded_old_path() {
        let (home, repo, mut params) = fixture();
        git(repo.path(), &["mv", "a.txt", "renamed.txt"]);
        git(repo.path(), &["commit", "-qm", "rename"]);
        params.path = "renamed.txt".into();
        params.rev = git(repo.path(), &["rev-parse", "HEAD"]).trim().into();
        let result = read(home.path(), &Principal::from(1), &CallerScope::Human, &params);
        assert_eq!(result["status"], "opened");
        assert_eq!(result["redline"]["before"], result["redline"]["after"]);
    }
    #[test]
    fn live_work_is_bound_to_story_and_removed_work_never_falls_back() {
        let (home, repo, mut params) = fixture();
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        let live = home.path().join("live");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-qb",
                "feat/SV-X",
                live.to_str().unwrap(),
            ],
        );
        record_story_worktree(home.path(), &live, home.path().join("story.md"));
        std::fs::write(live.join("a.txt"), "live change\n").unwrap();
        params.rev = "WORKTREE".into();
        params.story_id = Some("SV-X".into());
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["redline"]["after"],
            "live change\n"
        );
        std::fs::remove_dir_all(&live).unwrap();
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "worktreeRemoved"
        );
        params.rev = git(repo.path(), &["rev-parse", "HEAD"]).trim().into();
        assert_eq!(
            read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"],
            "opened"
        );
    }
    fn record_story_worktree(home: &Path, live: &Path, story: std::path::PathBuf) {
        let record = crate::install::worktree_index::WorktreeStoryRecord::new(
            live,
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "api").unwrap(),
            "SV-X",
            story,
        );
        crate::install::worktree_index::write_record(home, &Principal::from(1), &record).unwrap();
    }
    fn live_fixture() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf, Params) {
        let (home, repo, mut params) = fixture();
        git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let live = home.path().join("live");
        git(repo.path(), &["worktree", "add", "-qb", "feat/SV-X", live.to_str().unwrap()]);
        record_story_worktree(home.path(), &live, home.path().join("story.md"));
        params.rev = "WORKTREE".into();
        params.story_id = Some("SV-X".into());
        params.range = None;
        (home, repo, live, params)
    }
    fn commit_file(repo: &Path, path: &str, bytes: &[u8]) -> String {
        std::fs::write(repo.join(path), bytes).unwrap();
        git(repo, &["add", path]);
        git(repo, &["commit", "-qm", path]);
        git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
    }
    #[test]
    fn worktree_reads_stay_inside_the_canonical_worktree() {
        let (home, _repo, live, valid) = live_fixture();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside secret\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), live.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path(), live.join("out")).unwrap();
        std::os::unix::fs::symlink(live.join("a.txt"), live.join("inside.txt")).unwrap();
        let secret = outside.path().join("secret.txt");
        for path in ["link.txt", "out/secret.txt", "../outside/secret.txt", secret.to_str().unwrap()] {
            let mut params = valid.clone();
            params.path = path.into();
            let result = read(home.path(), &Principal::from(1), &CallerScope::Human, &params);
            assert_eq!(result["status"], "refused", "{path}: {result}");
            assert!(!result.to_string().contains("outside secret"), "{path}");
        }
        let mut inside = valid.clone();
        inside.path = "inside.txt".into();
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &inside)["status"], "opened");
    }
    #[test]
    fn a_story_worktree_outside_the_callers_view_is_refused() {
        let scoped = crate::context_repo::caller_scope::tests::Fixture::new();
        let repo = rebuild();
        super::super::why_line::tests::attach(&scoped.home, repo.path());
        git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let live = scoped.home.join("live");
        git(repo.path(), &["worktree", "add", "-qb", "feat/SV-X", live.to_str().unwrap()]);
        let story = scoped.write("parent", "content/secret-story.md", "# SV-X\n");
        let team = |team: &str| CallerScope::team(&scoped.home, &Principal::from(1), &crate::commands::dashboard_launch::LaunchTeam { org: "acme/ctx".into(), team: team.into() });
        let (hidden, visible) = (team("child"), team("parent"));
        assert!(!hidden.allows(&story) && visible.allows(&story), "fixture must exercise team admission");
        record_story_worktree(&scoped.home, &live, story);
        let params = Params { repo: "acme/api".into(), rev: "WORKTREE".into(), path: "a.txt".into(), story_id: Some("SV-X".into()), range: None };
        assert_eq!(read(&scoped.home, &Principal::from(1), &hidden, &params)["status"], "refused");
        assert_eq!(read(&scoped.home, &Principal::from(1), &visible, &params)["status"], "opened");
    }
    #[tokio::test]
    async fn open_diff_read_applies_the_calling_frames_view() {
        let f = crate::context_repo::caller_scope::tests::AppFixture::new();
        let home = f.fixture.home.clone();
        let repo = rebuild();
        super::super::why_line::tests::attach(&home, repo.path());
        git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let live = home.join("live");
        git(repo.path(), &["worktree", "add", "-qb", "feat/SV-X", live.to_str().unwrap()]);
        record_story_worktree(&home, &live, f.fixture.write("parent", "content/secret-story.md", "# SV-X\n"));
        let page = f.fixture.write("child", "content/trail.html", "<title>Trail</title>");
        let frame = f.frame(&page, "workspace");
        let params = Params { repo: "acme/api".into(), rev: "WORKTREE".into(), path: "a.txt".into(), story_id: Some("SV-X".into()), range: None };
        let call = |frame_id: Option<String>, page: Option<String>| crate::commands::data_read_model::open_diff_read(f.app.handle().clone(), params.clone(), frame_id, page);
        let framed = call(Some(frame.frame_id.clone()), Some(page.display().to_string())).await.unwrap();
        assert_eq!(framed["status"], "refused", "{framed}");
        assert_eq!(call(None, None).await.unwrap()["status"], "opened");
    }
    #[test]
    fn after_ranges_end_at_the_last_line() {
        let (home, repo, mut params) = fixture();
        params.rev = commit_file(repo.path(), "a.txt", b"one\ntwo\n");
        params.range = Some(AfterRange { start: 1, end: 2, side: Some("after".into()) });
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "opened");
        params.range = Some(AfterRange { start: 2, end: 3, side: Some("after".into()) });
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "refused");
    }
    #[test]
    fn hex_revisions_resolve_as_commit_ids_never_refs() {
        let (home, repo, mut params) = fixture();
        let first = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        commit_file(repo.path(), "a.txt", b"later\n");
        let local = git2::Repository::open(repo.path()).unwrap();
        let odb = local.odb().unwrap();
        let name = (0u32..)
            .map(|n| format!("{:08x}", 0xdead_0000u32.wrapping_add(n)))
            .find(|name| odb.exists_prefix(git2::Oid::from_str(name).unwrap(), name.len()).is_err())
            .unwrap();
        git(repo.path(), &["branch", &name, &first]);
        params.rev = name;
        params.range = None;
        let result = read(home.path(), &Principal::from(1), &CallerScope::Human, &params);
        assert_eq!(result["status"], "refused", "{result}");
        assert!(result["fetchCommand"].is_null());
    }
    #[test]
    fn binary_files_refuse_an_after_range() {
        let (home, repo, mut params) = fixture();
        params.rev = commit_file(repo.path(), "image.bin", b"\0binary\0");
        params.path = "image.bin".into();
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "refused");
        params.range = None;
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "opened");
    }
    #[test]
    fn two_matching_story_worktrees_refuse() {
        let (home, repo, _live, params) = live_fixture();
        let second = home.path().join("second");
        git(repo.path(), &["worktree", "add", "-qb", "feat/SV-X-2", second.to_str().unwrap()]);
        record_story_worktree(home.path(), &second, home.path().join("story.md"));
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "refused");
    }
    #[test]
    fn a_path_missing_at_the_revision_is_unavailable() {
        let (home, _repo, mut params) = fixture();
        params.path = "missing.txt".into();
        params.range = None;
        assert_eq!(read(home.path(), &Principal::from(1), &CallerScope::Human, &params)["status"], "unavailable");
    }
    #[test]
    fn pr3_case_table_diff_facts() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("../../assets/dashboards/story-trail/pr3-case-table.json")).unwrap();
        for row in cases.iter().filter(|row| row["native"] == "diff") {
            match row["id"].as_u64().unwrap() {
                12 => committed_and_short_sha_use_local_first_parent_content(),
                13 => { missing_commit_has_typed_fetch_advice_and_attachment_is_revalidated(); hex_revisions_resolve_as_commit_ids_never_refs(); },
                14 | 15 => live_work_is_bound_to_story_and_removed_work_never_falls_back(),
                16 => { refuses_old_or_missing_after_ranges_and_unbound_live_work(); after_ranges_end_at_the_last_line(); },
                20 => worktree_reads_stay_inside_the_canonical_worktree(),
                21 => binary_files_refuse_an_after_range(),
                22 => { two_matching_story_worktrees_refuse(); a_story_worktree_outside_the_callers_view_is_refused(); },
                23 => a_path_missing_at_the_revision_is_unavailable(),
                id => panic!("Missing native diff case {id}"),
            }
        }
    }

}
