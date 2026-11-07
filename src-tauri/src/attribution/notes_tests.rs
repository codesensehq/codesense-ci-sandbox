// SPDX-License-Identifier: MIT
//! SV-CDE-40 PR B: local-only notes by default, as one table over the story's cases 9–19.
//!
//! Every row runs in its own temp CodeSense home with a fake git-ai (the SV-CDE-59 argv0
//! fixtures where argv[0] matters); nothing touches the real `~/.codesense` or `~/.git-ai`.

use super::launch::{prepare, reapply_config, Plan, Reapplied, Request};
use super::managed::Managed;
use super::prefs::{self, AttributionPrefs, NotesMode};
use super::testing::{version_script, Launch};
use super::{AttributionStatus, OffReason};
use crate::agents::resolve::tests::alice;
use codesense_pty_core::agent_cli::AgentCli;
use std::path::Path;

const HTTPS: &str = "https://github.com/acme/api.git";
const SSH: &str = "git@github.com:acme/api.git";

fn launch(fixture: &Launch, runtime: AgentCli) -> Plan {
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

fn set_notes(fixture: &Launch, checkout: &str, notes: NotesMode) -> Result<(), String> {
    prefs::set_for_checkout(fixture.home(), &alice(), checkout, None, Some(notes))
}

fn cwd(fixture: &Launch) -> String {
    fixture.cwd.to_string_lossy().into_owned()
}

/// The managed git-ai's calls, without the `HOME=` suffix every logged call carries.
fn calls(fixture: &Launch) -> Vec<String> {
    fixture
        .calls()
        .into_iter()
        .map(|call| call.split(" HOME=").next().unwrap().to_owned())
        .collect()
}

/// `notes_push_repositories` as the fake git-ai's config file would hold it: what the last
/// `unset` left, plus every `--add` after it.
fn push_list(fixture: &Launch) -> Option<Vec<String>> {
    let calls = calls(fixture);
    let last = calls
        .iter()
        .rposition(|c| c == "config unset notes_push_repositories")?;
    Some(
        calls[last + 1..]
            .iter()
            .filter_map(|c| {
                c.strip_prefix("config set notes_push_repositories ")?
                    .strip_suffix(" --add")
                    .map(str::to_owned)
            })
            .collect(),
    )
}

fn local_notes_config_applied(fixture: &Launch) -> bool {
    let calls = calls(fixture);
    ["config set notes_ref sense", "config set notes_push never"]
        .iter()
        .all(|expected| calls.iter().any(|c| c == expected))
}

fn git(repo: &Path, args: &[&str]) -> String {
    crate::attribution::git_ai::tests::git(repo, args)
}

/// Case 9: a repo with no setting captures, and the managed config keeps its notes local.
fn case_9_default_repo_captures_with_local_notes() {
    let fixture = Launch::new(AgentCli::Claude);
    fixture.set_prefs(AttributionPrefs::default());
    let _daemon = fixture.daemon_running();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    assert!(
        local_notes_config_applied(&fixture),
        "{:?}",
        calls(&fixture)
    );
    assert_eq!(push_list(&fixture), Some(Vec::new()));
    let doctor = super::launch::doctor(fixture.home(), None);
    assert_eq!(doctor.notes_ref, "sense");
    // Honesty guard: a git-ai that refuses the local-notes keys would push, so a local repo
    // is off and a pushing repo still captures.
    fixture.install(&format!(
        "{}\nif [ \"$1 $2 $3\" = 'config set notes_ref' ]; then echo unknown key >&2; exit 1; fi",
        version_script(super::install::PIN.expected_version)
    ));
    // A changed binary re-applies the config (its version is in the sentinel).
    std::fs::remove_file(fixture.home().join("git-ai/config-applied")).unwrap();
    let refused = launch(&fixture, AgentCli::Claude);
    assert_eq!(
        refused.status,
        AttributionStatus::Off {
            reason: OffReason::NotesWouldPush
        }
    );
    assert!(refused.hook_command.is_none() && refused.env.is_empty());
    set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
}

/// Case 10: a repo set `push` names its origin, in https and ssh forms, and nothing else.
fn case_10_push_repo_lists_both_origin_forms_only() {
    let fixture = Launch::new(AgentCli::Codex);
    let _daemon = fixture.daemon_running();
    fixture.attach_checkout("other", "https://github.com/acme/other.git");
    set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Codex).status,
        AttributionStatus::Active
    );
    assert_eq!(
        push_list(&fixture),
        Some(vec![SSH.to_owned(), HTTPS.to_owned()])
    );
    // The list is credential-free and keeps the origin's case, for git-ai's path match.
    let fork = fixture.attach_checkout("fork", "https://token@github.com/Acme/Fork.git");
    set_notes(&fixture, &fork, NotesMode::Push).unwrap();
    launch(&fixture, AgentCli::Codex);
    let list = push_list(&fixture).unwrap();
    assert!(
        list.contains(&"https://github.com/Acme/Fork.git".to_owned()),
        "{list:?}"
    );
    assert!(
        list.contains(&"git@github.com:Acme/Fork.git".to_owned()),
        "{list:?}"
    );
    assert!(
        !list
            .iter()
            .any(|d| d.contains("token") || d.contains("other")),
        "{list:?}"
    );
}

/// Case 11: CodeSense's notes have a ref of their own, set only in the managed git-ai home,
/// so a user's own git-ai (its `~/.git-ai`, default ref `ai`) never writes or pushes them. The
/// fork binary's real push behaviour is the bare-remote proof recorded in the PR.
fn case_11_the_sense_ref_is_configured_only_in_the_managed_home() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    launch(&fixture, AgentCli::Claude);
    let managed_home = Managed::for_codesense_home(fixture.home()).git_ai_home();
    let refs: Vec<_> = fixture
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("config set notes_ref "))
        .collect();
    assert_eq!(
        refs,
        [format!(
            "config set notes_ref sense HOME={}",
            managed_home.display()
        )]
    );
    assert_eq!(super::git_ai::NOTES_REFS, ["sense", "ai"]);
}

/// Case 12: in a CodeSense session a user's own git-ai hook (from its `install-hooks`)
/// cannot reach a daemon, while CodeSense's hook reaches the managed one: one checkpoint.
/// The user fake follows upstream's path: no daemon home it can create means exit 0.
fn case_12_user_hook_never_checkpoints_in_a_codesense_session() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    fixture.install(&format!(
        "{}\necho \"daemon=$GIT_AI_DAEMON_HOME\" >> '{}'",
        version_script(super::install::PIN.expected_version),
        fixture.log().display()
    ));
    let plan = launch(&fixture, AgentCli::Claude);
    let ours = plan.hook_command.clone().unwrap();
    let user_home = fixture.home().join("user-home");
    let counted = fixture.home().join("user-checkpoints");
    let user_binary = user_home.join(".local/bin/git-ai");
    super::testing::script(
        &user_binary,
        &format!(
            "mkdir -p \"${{GIT_AI_DAEMON_HOME:-$HOME/.git-ai/internal/daemon}}\" 2>/dev/null || exit 0\necho \"$*\" >> '{}'",
            counted.display()
        ),
    );
    let theirs = format!(
        "{} checkpoint claude --hook-input stdin",
        user_binary.display()
    );
    let user_hook = serde_json::json!({"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": theirs}]}]}}).to_string();
    let config_dir = fixture.home().join("claude-config");
    let projects = [fixture.cwd.clone()];
    let present = |config_dir: Option<&Path>| {
        super::launch::user_hooks_present(&super::launch::HookSettings {
            user_home: &user_home,
            claude_config_dir: config_dir,
            codex_home: None,
            projects: &projects,
        })
    };
    assert!(!present(None));
    // Every place a launch's Claude settings merge from: user, local, CLAUDE_CONFIG_DIR, project.
    for file in [
        user_home.join(".claude/settings.json"),
        user_home.join(".claude/settings.local.json"),
        config_dir.join("settings.json"),
        fixture.cwd.join(".claude/settings.local.json"),
    ] {
        super::testing::script(&file, "");
        std::fs::write(&file, &user_hook).unwrap();
        let dir = (file.starts_with(&config_dir)).then_some(config_dir.as_path());
        assert!(present(dir), "{}", file.display());
        std::fs::remove_file(&file).unwrap();
    }
    // CodeSense's own hook is not the user's.
    std::fs::write(
        user_home.join(".claude/settings.json"),
        serde_json::json!({"hooks": {"PreToolUse": [{"hooks": [{"command": ours}]}]}}).to_string(),
    )
    .unwrap();
    assert!(!present(None));
    std::fs::write(user_home.join(".claude/settings.json"), &user_hook).unwrap();
    assert!(present(None));
    let fire = |env: &[(String, String)]| {
        for hook in [&theirs, &ours] {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", hook])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &user_home)
                .envs(env.iter().map(|(k, v)| (k, v)))
                .stdin(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{hook}");
        }
    };
    // Control: outside a CodeSense session the user's hook does checkpoint.
    fire(&[]);
    assert_eq!(super::testing::logged_calls(&counted).len(), 1);
    std::fs::remove_file(&counted).unwrap();
    std::fs::remove_dir_all(user_home.join(".git-ai")).unwrap();
    let before = calls(&fixture)
        .iter()
        .filter(|c| c.starts_with("checkpoint claude"))
        .count();
    fire(&plan.env);
    assert!(
        !counted.exists(),
        "the user's hook checkpointed inside the session"
    );
    let checkpoints: Vec<_> = calls(&fixture)
        .into_iter()
        .filter(|c| c.starts_with("checkpoint claude"))
        .collect();
    assert_eq!(checkpoints.len(), before + 1, "{checkpoints:?}");
    let managed = Managed::for_codesense_home(fixture.home());
    let daemon = format!("daemon={}", managed.root().join("daemon").display());
    assert!(
        super::testing::logged_calls(&fixture.log())
            .iter()
            .any(|c| c == &daemon),
        "CodeSense's hook keeps its managed daemon"
    );
    assert!(
        !user_home.join(".git-ai").exists(),
        "nothing written to the user's git-ai home"
    );
}

/// Case 12, Codex: the launch's own composition. Codex runs hooks with its environment, the
/// launch env carries the neutralised daemon paths, and CodeSense's `-c hooks.*` command sets
/// the managed ones; so the same user hook stays silent and CodeSense's checkpoints.
fn case_12_codex_hooks_run_in_the_launch_environment() {
    let fixture = Launch::new(AgentCli::Codex);
    let _daemon = fixture.daemon_running();
    let (prepared, _, id) =
        crate::commands::agents_launch::tests::attribution_entry::launched(&fixture);
    crate::commands::mcp_scope::remove_scope_config(&id);
    let env = &prepared.scope.extra_env;
    for (key, value) in super::launch::USER_DAEMON_NEUTRALISED {
        assert!(
            env.iter().any(|(k, v)| k == key && v == value),
            "{key} not in the Codex launch env: {env:?}"
        );
    }
    let flag = prepared
        .launch
        .args
        .iter()
        .find(|a| {
            a.starts_with("hooks.PostToolUse=")
                && a.contains(codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER)
        })
        .expect("CodeSense's Codex hook flag");
    let parsed: toml::Value =
        toml::from_str(&format!("v = {}", flag.split_once('=').unwrap().1)).unwrap();
    let ours = parsed["v"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let managed = Managed::for_codesense_home(fixture.home());
    assert!(
        ours.contains(&format!(
            "GIT_AI_DAEMON_HOME={}",
            codesense_pty_core::shell_quote(&managed.root().join("daemon").to_string_lossy())
        )),
        "{ours}"
    );
    let counted = fixture.home().join("user-checkpoints");
    let theirs = fixture.home().join("user-home/.local/bin/git-ai");
    super::testing::script(
        &theirs,
        &format!(
            "mkdir -p \"${{GIT_AI_DAEMON_HOME:-$HOME/.git-ai/internal/daemon}}\" 2>/dev/null || exit 0\necho \"$*\" >> '{}'",
            counted.display()
        ),
    );
    let before = calls(&fixture)
        .iter()
        .filter(|c| c.starts_with("checkpoint codex"))
        .count();
    for hook in [
        format!("{} checkpoint codex --hook-input stdin", theirs.display()),
        ours,
    ] {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &hook])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", fixture.home().join("user-home"))
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{hook}");
    }
    assert!(
        !counted.exists(),
        "the user's Codex hook checkpointed inside the session"
    );
    let after = calls(&fixture)
        .iter()
        .filter(|c| c.starts_with("checkpoint codex"))
        .count();
    assert_eq!(after, before + 1);
}

/// Case 13: another machine's local-only notes never arrive, so a read says why.
fn case_13_other_machine_reads_unknown_with_local_only_reason() {
    let home = tempfile::tempdir().unwrap();
    let repo = crate::attribution::git_ai::tests::rebuild();
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["update-ref", "-d", "refs/notes/ai"]);
    let result = crate::attribution::why_line::tests::answer_default(home.path());
    assert_eq!(result["ranges"][0]["basis"], "unknown");
    let reason = result["ranges"][0]["reason"].as_str().unwrap();
    assert!(
        reason.ends_with("stay on the machine that wrote them (local-only)"),
        "{reason}"
    );
    prefs::update(home.path(), &alice(), "github.com/acme/api", |p| {
        p.notes = Some(NotesMode::Push)
    })
    .unwrap();
    let result = crate::attribution::why_line::tests::answer_default(home.path());
    assert_eq!(
        result["ranges"][0]["reason"],
        "no git-ai notes in local copy"
    );
    // L4: the tail is only for a commit with no note here, never for a note without the line.
    prefs::update(home.path(), &alice(), "github.com/acme/api", |p| {
        p.notes = None
    })
    .unwrap();
    let uncovered = "other.txt\n  h_x 1\n---\n{\"schema_version\":\"authorship/3.0.0\",\"humans\":{\"h_x\":{\"author\":\"X <x@example.invalid>\"}}}";
    let line_commit = crate::attribution::why_line::tests::answer_default(home.path())["ranges"][0]
        ["commit"]
        .as_str()
        .unwrap()
        .to_owned();
    git(
        repo.path(),
        &[
            "notes",
            "--ref=sense",
            "add",
            "-f",
            "-m",
            uncovered,
            &line_commit,
        ],
    );
    assert_eq!(
        crate::attribution::why_line::tests::answer_default(home.path())["ranges"][0]["reason"],
        "no attribution in note"
    );
    // Notes exist in this copy, but not on this line's commit: the local-only tail applies.
    let root = git(repo.path(), &["rev-list", "--max-parents=0", "HEAD"])
        .trim()
        .to_owned();
    git(
        repo.path(),
        &["notes", "--ref=sense", "remove", &line_commit],
    );
    if root != line_commit {
        git(
            repo.path(),
            &["notes", "--ref=sense", "add", "-f", "-m", uncovered, &root],
        );
        let reason = crate::attribution::why_line::tests::answer_default(home.path())["ranges"][0]
            ["reason"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            reason,
            "no git-ai note on introducing commit; notes for this repo stay on the machine that wrote them (local-only)"
        );
    }
}

/// Case 14: today's prefs file loads as local with capture on; `private` changes nothing.
fn case_14_old_prefs_file_loads_local_and_private_is_ignored() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let file = crate::context_repo::path_to_user_home(fixture.home(), &alice())
        .join(["attribution-prefs", ".json"].concat());
    std::fs::write(
        &file,
        r#"{"github.com/acme/api":{"mode":"on","privacy":{"value":"private","source":"user","checkedAt":"2026-10-20T00:00:00Z"}}}"#,
    )
    .unwrap();
    let prefs = fixture.prefs();
    assert_eq!(prefs.notes, None);
    assert_eq!(AttributionPrefs::notes_mode(&prefs), NotesMode::Local);
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    // `auto` used to wait for a privacy answer; it captures now.
    prefs::set_for_checkout(
        fixture.home(),
        &alice(),
        &cwd(&fixture),
        Some(prefs::AttributionChoice::Auto),
        None,
    )
    .unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    let params: crate::mcp::local_setup::AttachSourceCheckoutParams =
        serde_json::from_value(serde_json::json!({"repoPath": "/x", "private": true})).unwrap();
    assert_eq!(params.private, Some(true));
}

/// Case 15: an opt-in made while a launch applies the config, and one made before it, both
/// end in the opted-in config; a restart then finds no stale `installing`.
fn case_15_opt_in_during_a_launch_and_before_it_both_win() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let started = fixture.home().join("started.fifo");
    let go = fixture.home().join("go.fifo");
    for fifo in [&started, &go] {
        assert!(std::process::Command::new("mkfifo")
            .arg(fifo)
            .status()
            .unwrap()
            .success());
    }
    // The first config write (`git_path`, after the push list is read under the config
    // lock) rendezvouses with the test, then waits for it.
    let gate = fixture.home().join("gated");
    fixture.install(&format!(
        "{}\nif [ \"$1 $2 $3\" = 'config set git_path' ] && [ ! -e '{gate}' ]; then touch '{gate}'; echo > '{started}'; cat '{go}' > /dev/null; fi",
        version_script(super::install::PIN.expected_version),
        gate = gate.display(),
        started = started.display(),
        go = go.display(),
    ));
    // Releases the waiting git-ai however this case ends, so no thread is left blocked.
    struct Release<'a>(&'a Path);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0, b"\n");
        }
    }
    let (signal, signalled) = std::sync::mpsc::channel();
    let rendezvous = signal.clone();
    let started_fifo = started.clone();
    // Detached: if the launch never writes the config, this reader is the only thing left
    // waiting, and the case fails instead of hanging.
    std::thread::spawn(move || {
        if std::fs::read(&started_fifo).is_ok() {
            let _ = rendezvous.send("started");
        }
    });
    let launched = &fixture;
    std::thread::scope(|scope| {
        let launching = scope.spawn(move || {
            let plan = launch(launched, AgentCli::Claude);
            let _ = signal.send("done");
            plan
        });
        assert_eq!(
            signalled.recv().unwrap(),
            "started",
            "the launch finished without writing the managed config"
        );
        let release = Release(&go);
        set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap();
        let reapplying = scope.spawn(|| reapply_config(fixture.home()));
        drop(release);
        assert_eq!(launching.join().unwrap().status, AttributionStatus::Active);
        assert_eq!(reapplying.join().unwrap(), Reapplied::Now);
    });
    assert_eq!(
        push_list(&fixture),
        Some(vec![SSH.to_owned(), HTTPS.to_owned()])
    );
    // The other order, back to local: the pref first, then the launch.
    set_notes(&fixture, &cwd(&fixture), NotesMode::Local).unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Codex).status,
        AttributionStatus::Active
    );
    assert_eq!(push_list(&fixture), Some(Vec::new()));
    // An app restart re-reads only disk: the record says active, never installing.
    assert_eq!(fixture.status(), Some(AttributionStatus::Active));
    assert_eq!(super::install::status(fixture.home()), None);
}

/// Case 16: a daemon started before the config revision bump is restarted after the new
/// config (`notes_ref=sense`) is applied, and capture is off until it is.
fn case_16_daemon_from_an_older_config_revision_restarts_after_the_config() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let stamp = fixture.home().join("git-ai/daemon-binary-version");
    std::fs::create_dir_all(stamp.parent().unwrap()).unwrap();
    // PR A's stamp: the bare version, from revision 1.
    std::fs::write(&stamp, super::install::PIN.expected_version).unwrap();
    let managed = Managed::for_codesense_home(fixture.home());
    assert!(managed.restart_pending(Some(super::install::PIN.expected_version)));
    let fail = fixture.home().join("restart-fails");
    fixture.install(&format!(
        "{}\nif [ \"$1 $2\" = 'bg restart' ] && [ -e '{}' ]; then exit 9; fi",
        version_script(super::install::PIN.expected_version),
        fail.display(),
    ));
    std::fs::write(&fail, "").unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Off {
            reason: OffReason::NotesWouldPush
        }
    );
    std::fs::remove_file(&fail).unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    let calls = calls(&fixture);
    let config = calls
        .iter()
        .position(|c| c == "config set notes_ref sense")
        .unwrap();
    let restart = calls.iter().rposition(|c| c == "bg restart").unwrap();
    assert!(config < restart, "{calls:?}");
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap(),
        Managed::daemon_stamp(super::install::PIN.expected_version)
    );
    assert!(!managed.restart_pending(Some(super::install::PIN.expected_version)));
}

/// Case 17: the MCP save applies the push list at once, and describe_setup reports it.
fn case_17_mcp_opt_in_reapplies_and_describe_setup_reports_it() {
    let fixture = Launch::new(AgentCli::Claude);
    let app = crate::mcp::local_setup::tests::app(fixture.home());
    let result = tauri::async_runtime::block_on(crate::mcp::local_setup::dispatch(
        app.handle(),
        fixture.home(),
        "save_source_repo_attachment",
        serde_json::json!({"repoPath": fixture.cwd, "notes": "push"}),
    ))
    .unwrap();
    assert_eq!(
        result,
        serde_json::json!({"notes": {"applied": "now", "mode": "push"}})
    );
    assert_eq!(
        push_list(&fixture),
        Some(vec![SSH.to_owned(), HTTPS.to_owned()])
    );
    let setup = tauri::async_runtime::block_on(crate::mcp::setup_tools::describe_setup_at(
        app.handle(),
        fixture.home().to_path_buf(),
        1,
        None,
    ))
    .unwrap();
    let setup = crate::mcp::setup_tools::ToolEnvelope::Ok(setup);
    let setup = crate::mcp::handler::envelope_json(&setup).unwrap();
    let setup: serde_json::Value =
        serde_json::from_str(&setup.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(
        setup["data"]["attribution"]["notesPushRepos"],
        serde_json::json!(["github.com/acme/api"])
    );
    assert_eq!(setup["data"]["attribution"]["notesRef"], "sense");
}

/// Case 18: with no origin there is nowhere to push; the repo stays local.
fn case_18_push_without_origin_is_refused() {
    let fixture = Launch::new(AgentCli::Claude);
    git2::Repository::open(&fixture.cwd)
        .unwrap()
        .remote_delete("origin")
        .unwrap();
    let error = set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap_err();
    assert!(error.starts_with("no_remote"), "{error}");
    assert_eq!(fixture.prefs().notes, None);
    // Through MCP the refusal comes first: the request's other choice is not half-saved.
    let before = fixture.prefs();
    let app = crate::mcp::local_setup::tests::app(fixture.home());
    let refused = tauri::async_runtime::block_on(crate::mcp::local_setup::dispatch(
        app.handle(),
        fixture.home(),
        "save_source_repo_attachment",
        serde_json::json!({"repoPath": fixture.cwd, "attribution": "off", "notes": "push"}),
    ))
    .unwrap_err();
    assert!(refused.starts_with("no_remote"), "{refused}");
    assert_eq!(fixture.prefs(), before);
}

/// L2: the managed config is one per CodeSense home, so its push list is every local
/// profile's choices together; one profile's launch never drops another's.
fn push_list_is_the_union_of_every_profile() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    let other = crate::domain::Principal::from(2);
    let checkout = fixture.attach_checkout("bob-api", "https://github.com/bob/api.git");
    crate::commands::attachments::upsert_attachment_record(
        fixture.home(),
        2,
        crate::commands::attachments::existing_checkout_attachment(Path::new(&checkout)).unwrap(),
        None,
        false,
    )
    .unwrap();
    prefs::set_for_checkout(
        fixture.home(),
        &other,
        &checkout,
        None,
        Some(NotesMode::Push),
    )
    .unwrap();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    let list = push_list(&fixture).unwrap();
    assert!(
        list.contains(&"https://github.com/bob/api.git".to_owned()),
        "{list:?}"
    );
    assert_eq!(prefs::push_repos(fixture.home()), ["github.com/bob/api"]);
    assert_eq!(
        super::launch::doctor(fixture.home(), None).notes_push_repos,
        ["github.com/bob/api"]
    );
}

/// L3: a notes change says what happened to it: applied now, pending until git-ai is
/// installed (still saved), or refused by a git-ai that cannot keep notes local.
fn notes_changes_report_whether_they_applied() {
    let fixture = Launch::new(AgentCli::Claude);
    set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap();
    assert_eq!(reapply_config(fixture.home()), Reapplied::Now);
    assert!(push_list(&fixture).unwrap().contains(&HTTPS.to_owned()));
    fixture.uninstall();
    set_notes(&fixture, &cwd(&fixture), NotesMode::Local).unwrap();
    assert_eq!(reapply_config(fixture.home()), Reapplied::Pending);
    assert_eq!(
        AttributionPrefs::notes_mode(&fixture.prefs()),
        NotesMode::Local
    );
    fixture.install(&format!(
        "{}\nif [ \"$1 $2 $3\" = 'config set notes_push' ]; then echo unknown key >&2; exit 1; fi",
        version_script(super::install::PIN.expected_version)
    ));
    assert!(matches!(
        reapply_config(fixture.home()),
        Reapplied::Unsupported { message } if message.contains("unknown key")
    ));
}

/// L1: the origin as configured is listed too, without credentials, so `.git`-less and
/// `ssh://` remotes match; and it is the checkout's current origin, never a cached one.
fn push_destinations_keep_the_raw_origin_without_credentials() {
    let fixture = Launch::new(AgentCli::Claude);
    let lab = fixture.attach_checkout("lab", "https://user:secret@gitlab.example.com/team/lab");
    git2::Repository::open(&lab)
        .unwrap()
        .remote_set_url("origin", "ssh://git@gitlab.example.com/team/lab")
        .unwrap();
    let raw = fixture.attach_checkout("raw", "https://tok@github.com/acme/raw");
    for checkout in [&lab, &raw] {
        prefs::set_for_checkout(
            fixture.home(),
            &alice(),
            checkout,
            None,
            Some(NotesMode::Push),
        )
        .unwrap();
    }
    let list = prefs::push_destinations(fixture.home());
    for expected in [
        "ssh://git@gitlab.example.com/team/lab",
        "https://gitlab.example.com/team/lab.git",
        "git@gitlab.example.com:team/lab.git",
        "https://github.com/acme/raw",
    ] {
        assert!(list.contains(&expected.to_owned()), "{expected}: {list:?}");
    }
    assert!(
        !list
            .iter()
            .any(|d| d.contains("secret") || d.contains("tok@")),
        "{list:?}"
    );
}

/// Round 2 L-c: the local-only tail follows the per-Mac push list, so another profile's
/// opt-in counts for every profile's reads.
fn local_only_reason_follows_the_per_mac_push_list() {
    let home = tempfile::tempdir().unwrap();
    let repo = crate::attribution::git_ai::tests::rebuild();
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["update-ref", "-d", "refs/notes/ai"]);
    let tail = |result: &serde_json::Value| {
        result["ranges"][0]["reason"]
            .as_str()
            .unwrap()
            .ends_with("(local-only)")
    };
    assert!(tail(&crate::attribution::why_line::tests::answer_default(
        home.path()
    )));
    let other = crate::domain::Principal::from(2);
    prefs::update(home.path(), &other, "github.com/acme/api", |p| {
        p.notes = Some(NotesMode::Push)
    })
    .unwrap();
    let read = crate::attribution::why_line::tests::answer_default(home.path());
    assert!(
        !tail(&read),
        "profile 2 pushes, so profile 1's read has no tail: {read}"
    );
}

/// Round 2 L-d: with nothing set to push, applying the config opens no checkout.
fn no_push_choice_opens_no_checkout() {
    let fixture = Launch::new(AgentCli::Claude);
    let _daemon = fixture.daemon_running();
    fixture.attach_checkout("other", "https://github.com/acme/other.git");
    prefs::opened::take();
    assert_eq!(
        launch(&fixture, AgentCli::Claude).status,
        AttributionStatus::Active
    );
    assert!(prefs::push_destinations(fixture.home()).is_empty());
    assert_eq!(prefs::opened::take(), 0);
    set_notes(&fixture, &cwd(&fixture), NotesMode::Push).unwrap();
    assert!(!prefs::push_destinations(fixture.home()).is_empty());
    assert!(prefs::opened::take() > 0);
}

/// Case 20: a commit with a `sense` note that does not cover a line is not a story association,
/// even with an `SV-…:` subject: the line stays `unknown`.
fn case_20_a_sense_note_never_becomes_a_story_association() {
    let home = tempfile::tempdir().unwrap();
    let repo = crate::attribution::git_ai::tests::rebuild();
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    git(repo.path(), &["update-ref", "-d", "refs/notes/ai"]);
    std::fs::write(
        repo.path().join("a.txt"),
        "one\ntwo by a story commit\nthree\n",
    )
    .unwrap();
    git(repo.path(), &["add", "a.txt"]);
    git(
        repo.path(),
        &[
            "-c",
            "user.name=F",
            "-c",
            "user.email=f@example.invalid",
            "commit",
            "-q",
            "-m",
            "SV-1: story change",
        ],
    );
    let uncovered = "other.txt\n  h_x 1\n---\n{\"schema_version\":\"authorship/3.0.0\",\"humans\":{\"h_x\":{\"author\":\"X <x@example.invalid>\"}}}";
    git(
        repo.path(),
        &["notes", "--ref=sense", "add", "-m", uncovered, "HEAD"],
    );
    let result = crate::attribution::why_line::tests::answer_default(home.path());
    assert_eq!(result["ranges"][0]["basis"], "unknown", "{result}");
    assert_eq!(
        result["ranges"][0]["reason"], "no attribution in note",
        "{result}"
    );
}

/// Case 19: a commit with both a `sense` and an `ai` note reads the `sense` one.
fn case_19_sense_note_wins_over_ai() {
    let home = tempfile::tempdir().unwrap();
    let repo = crate::attribution::git_ai::tests::rebuild();
    crate::attribution::why_line::tests::attach(home.path(), repo.path());
    for line in git(repo.path(), &["notes", "--ref=ai", "list"]).lines() {
        let commit = line.split_whitespace().nth(1).unwrap();
        let note = git(repo.path(), &["notes", "--ref=ai", "show", commit]);
        git(
            repo.path(),
            &["notes", "--ref=sense", "add", "-m", &note, commit],
        );
        git(
            repo.path(),
            &[
                "notes",
                "--ref=ai",
                "add",
                "-f",
                "-m",
                "malformed note",
                commit,
            ],
        );
    }
    let result = crate::attribution::why_line::tests::answer_default(home.path());
    assert_eq!(result["ranges"][0]["basis"], "exact", "{result}");
    let notes = result["notesRef"].as_str().unwrap();
    assert!(
        notes.starts_with("sense=") && notes.contains(" ai="),
        "{notes}"
    );
}

#[test]
fn local_notes_case_table() {
    let cases: [(u8, fn()); 18] = [
        (9, case_9_default_repo_captures_with_local_notes),
        (10, case_10_push_repo_lists_both_origin_forms_only),
        (
            11,
            case_11_the_sense_ref_is_configured_only_in_the_managed_home,
        ),
        (
            12,
            case_12_user_hook_never_checkpoints_in_a_codesense_session,
        ),
        (12, case_12_codex_hooks_run_in_the_launch_environment),
        (
            13,
            case_13_other_machine_reads_unknown_with_local_only_reason,
        ),
        (
            14,
            case_14_old_prefs_file_loads_local_and_private_is_ignored,
        ),
        (15, case_15_opt_in_during_a_launch_and_before_it_both_win),
        (
            16,
            case_16_daemon_from_an_older_config_revision_restarts_after_the_config,
        ),
        (
            17,
            case_17_mcp_opt_in_reapplies_and_describe_setup_reports_it,
        ),
        (18, case_18_push_without_origin_is_refused),
        (19, case_19_sense_note_wins_over_ai),
        (20, case_20_a_sense_note_never_becomes_a_story_association),
        // Review round 1 rows.
        (102, push_list_is_the_union_of_every_profile),
        (103, notes_changes_report_whether_they_applied),
        (
            104,
            push_destinations_keep_the_raw_origin_without_credentials,
        ),
        (105, local_only_reason_follows_the_per_mac_push_list),
        (106, no_push_choice_opens_no_checkout),
    ];
    let failed: Vec<String> = cases
        .into_iter()
        .filter_map(|(case, run)| {
            std::panic::catch_unwind(run).err().map(|panic| {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                format!("case {case}: {message}")
            })
        })
        .collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
