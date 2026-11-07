// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::*;

const BRIDGE: &str = "/opt/codesense/bin/codesense-bridge";

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = crate::test_support::strip_attribution_env(&mut Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A CodeSense home, a user repo with one commit and a linked worktree beside it, as a
/// story worktree is; `record` registers it in the worktree index, as establish does.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
}

fn fixture(record: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let main = root.join("repo");
    std::fs::create_dir_all(&main).unwrap();
    run_git(&main, &["init", "-q"]);
    run_git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = root.join("repo-wt").join("sw-1");
    run_git(
        &main,
        &["worktree", "add", "-q", "-b", "sw-1", wt.to_str().unwrap()],
    );
    if record {
        record_worktree(&home, &wt);
    }
    Fixture {
        _tmp: tmp,
        home,
        main,
        wt,
    }
}

fn record_worktree(home: &Path, dir: &Path) {
    crate::install::worktree_index::write_record(
        home,
        &crate::domain::Principal::from(42u64),
        &crate::install::worktree_index::WorktreeStoryRecord::new(
            dir.to_path_buf(),
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "repo").unwrap(),
            "SW-1",
            home.join("users/42/orgs/acme/ctx/team/content/SW-1.md"),
        ),
    )
    .unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn notify_hooks(settings: &Value, event: &str) -> usize {
    settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("codesense-notify"))
        })
        .count()
}

/// The CodeSense files written into `dir`.
fn written(dir: &Path) -> Vec<&'static str> {
    [
        ".mcp.json",
        ".codex/config.toml",
        ".claude/settings.local.json",
    ]
    .into_iter()
    .filter(|rel| dir.join(rel).exists())
    .collect()
}

#[test]
fn a_managed_worktree_gets_the_config_ignored_by_that_worktree_alone() {
    let f = fixture(true);
    // The user's own untracked root file in the main checkout.
    std::fs::write(f.main.join(".mcp.json"), "{}").unwrap();

    write_in(&f.wt, &f.home, Some(Path::new(BRIDGE)));

    assert_eq!(
        read_json(&f.wt.join(".mcp.json"))["mcpServers"]["codesense"]["command"],
        BRIDGE
    );
    let codex: toml::Value =
        toml::from_str(&std::fs::read_to_string(f.wt.join(".codex/config.toml")).unwrap()).unwrap();
    assert_eq!(
        codex["mcp_servers"]["codesense"]["command"].as_str(),
        Some(BRIDGE)
    );
    let local = read_json(&f.wt.join(".claude/settings.local.json"));
    for ev in crate::agent_state::status_hooks().map(|hook| hook.event) {
        assert_eq!(notify_hooks(&local, ev), 1, "{ev}");
    }

    assert_eq!(
        run_git(&f.wt, &["status", "--porcelain"]),
        "",
        "nothing shows"
    );
    run_git(&f.wt, &["add", "-A"]);
    assert_eq!(
        run_git(&f.wt, &["diff", "--cached", "--name-only"]),
        "",
        "git add -A stages none of it"
    );
    // The main checkout still sees its own untracked `.mcp.json`; the shared
    // info/exclude is untouched.
    assert_eq!(
        run_git(&f.main, &["status", "--porcelain"]),
        "?? .mcp.json\n"
    );
    let shared = std::fs::read_to_string(f.main.join(".git/info/exclude")).unwrap_or_default();
    assert!(!shared.contains("mcp.json"), "{shared}");

    // Idempotent: a second open changes nothing and keeps one include.
    write_in(&f.wt, &f.home, Some(Path::new(BRIDGE)));
    let config = std::fs::read_to_string(f.main.join(".git/config")).unwrap();
    assert_eq!(config.matches("includeIf").count(), 1, "{config}");
    assert_eq!(run_git(&f.wt, &["status", "--porcelain"]), "");
}

#[test]
fn the_worktree_keeps_the_users_own_excludes() {
    let f = fixture(true);
    let ignore = f.main.join("../user-ignore");
    std::fs::write(&ignore, "*.log\n").unwrap();
    run_git(
        &f.main,
        &["config", "core.excludesFile", ignore.to_str().unwrap()],
    );
    std::fs::write(f.wt.join("build.log"), "x").unwrap();
    assert_eq!(run_git(&f.wt, &["status", "--porcelain"]), "");

    write_in(&f.wt, &f.home, Some(Path::new(BRIDGE)));

    assert_eq!(
        run_git(&f.wt, &["status", "--porcelain"]),
        "",
        "still ignores *.log and now the CodeSense files"
    );
}

#[test]
fn a_users_checkout_or_an_unrecorded_worktree_is_never_written() {
    let f = fixture(false);
    // The main checkout, even recorded (a story pointed at it), is the user's.
    record_worktree(&f.home, &f.main);
    write_in(&f.main, &f.home, Some(Path::new(BRIDGE)));
    assert!(written(&f.main).is_empty(), "{:?}", written(&f.main));
    // A linked worktree CodeSense never recorded is the user's too.
    write_in(&f.wt, &f.home, Some(Path::new(BRIDGE)));
    assert!(written(&f.wt).is_empty(), "{:?}", written(&f.wt));
    assert_eq!(run_git(&f.main, &["status", "--porcelain"]), "");
    let config = std::fs::read_to_string(f.main.join(".git/config")).unwrap();
    assert!(!config.contains("includeIf"), "{config}");
}

#[test]
fn a_context_repo_is_never_written_but_an_epic_dir_is() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let content = home.join("users/42/orgs/acme/ctx/team/content");
    let epic = home.join("users/42/_epic/acme/E-1");
    for dir in [&content, &epic] {
        std::fs::create_dir_all(dir).unwrap();
        write_in(dir, home, Some(Path::new(BRIDGE)));
    }
    assert!(written(&content).is_empty());
    assert_eq!(
        written(&epic),
        vec![
            ".mcp.json",
            ".codex/config.toml",
            ".claude/settings.local.json"
        ]
    );
}

#[test]
fn rewriting_keeps_the_teams_servers_and_replaces_a_stale_bridge_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join("users/42/_worker/acme/team/sprint-1");
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(
        dir.join(".mcp.json"),
        r#"{"mcpServers":{"codesense":{"command":"/old/bridge"},"slack":{"command":"slack-mcp"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join(".claude/settings.json"),
        serde_json::json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
            "command": codesense_pty_core::notify_hook::status_command()}]}]}})
        .to_string(),
    )
    .unwrap();

    write_in(&dir, home, Some(Path::new(BRIDGE)));
    write_in(&dir, home, Some(Path::new(BRIDGE)));

    let mcp = read_json(&dir.join(".mcp.json"));
    assert_eq!(mcp["mcpServers"]["codesense"]["command"], BRIDGE);
    assert_eq!(mcp["mcpServers"]["slack"]["command"], "slack-mcp");
    let local = read_json(&dir.join(".claude/settings.local.json"));
    assert_eq!(
        notify_hooks(&local, "Stop"),
        0,
        "the committed settings run it"
    );
    assert_eq!(notify_hooks(&local, "PreToolUse"), 1, "once, however often");
    // A launch through CodeSense there adds no second status hook on any event: only its
    // inbox hooks, which no folder carries.
    let mut launch = serde_json::json!({});
    crate::commands::content_engine::merge_launch_notify_hooks(
        &mut launch,
        &dir,
        codesense_pty_core::agent_cli::AgentCli::Claude,
    );
    assert!(!launch.to_string().contains("codesense-notify"), "{launch}");
    assert!(launch.to_string().contains("--inbox-wait"), "{launch}");
}

#[test]
fn a_tracked_file_missing_from_the_worktree_is_not_recreated_nor_a_bad_one_touched() {
    let f = fixture(true);
    std::fs::write(f.wt.join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    run_git(&f.wt, &["add", "-f", ".mcp.json"]);
    run_git(&f.wt, &["commit", "-q", "-m", "team mcp"]);
    std::fs::remove_file(f.wt.join(".mcp.json")).unwrap();
    std::fs::create_dir_all(f.wt.join(".claude")).unwrap();
    std::fs::write(f.wt.join(".claude/settings.local.json"), "not json").unwrap();

    write_in(&f.wt, &f.home, Some(Path::new(BRIDGE)));

    assert!(
        !f.wt.join(".mcp.json").exists(),
        "the team's file stays deleted"
    );
    assert_eq!(
        std::fs::read_to_string(f.wt.join(".claude/settings.local.json")).unwrap(),
        "not json"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_config_dir_is_never_written_through() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join("users/42/_epic/acme/E-2");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(".codex")).unwrap();

    write_in(&dir, home, Some(Path::new(BRIDGE)));

    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(
        dir.join(".mcp.json").is_file(),
        "the root file is still written"
    );
}

#[test]
fn without_a_bridge_only_the_hooks_are_written() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("users/42/_epic/acme/E-3");
    std::fs::create_dir_all(&dir).unwrap();
    write_in(&dir, tmp.path(), None);
    assert_eq!(written(&dir), vec![".claude/settings.local.json"]);
    // A hand-typed `claude` there runs the script its tab's host installed, on every
    // status event.
    let local = read_json(&dir.join(".claude/settings.local.json"));
    for ev in crate::agent_state::status_hooks().map(|hook| hook.event) {
        assert_eq!(
            local["hooks"][ev][0]["hooks"][0]["command"],
            codesense_pty_core::notify_hook::status_command(),
            "{ev}"
        );
    }
}

#[test]
fn nested_cwd_uses_registered_root_protects_tracked_files_and_excludes_generated_files() {
    let f = fixture(true);
    let nested = f.wt.join("team/content");
    std::fs::create_dir_all(&nested).unwrap();
    let original = br#"{"mcpServers":{"team":{"command":"team-mcp"}}}"#;
    std::fs::write(nested.join(".mcp.json"), original).unwrap();
    run_git(&f.wt, &["add", "team/content/.mcp.json"]);
    run_git(&f.wt, &["commit", "-qm", "team config"]);
    write_in(&nested, &f.home, Some(Path::new("/bridge")));
    assert_eq!(std::fs::read(nested.join(".mcp.json")).unwrap(), original);
    assert!(nested.join(".claude/settings.local.json").is_file());
    assert_eq!(run_git(&f.wt, &["status", "--porcelain"]).trim(), "");
    let git = crate::content_engine::git_worktree::GitWorktree::discover(&nested)
        .unwrap()
        .unwrap();
    assert!(git.exclude().is_file());
    assert!(std::fs::read_to_string(git.exclude())
        .unwrap()
        .contains("team/content/.claude/settings.local.json"));
}
