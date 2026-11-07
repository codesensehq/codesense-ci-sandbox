// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::tests::store;

fn skills(names: &[&str]) -> Vec<SkillVersion> {
    names
        .iter()
        .map(|n| SkillVersion {
            name: n.to_string(),
            hash: Some(format!("hash-{n}")),
            reason: None,
        })
        .collect()
}

fn record(session: &str) -> SessionRecord {
    let mut r = SessionRecord::new(session);
    r.org = Some("acme".into());
    r.team = Some("riverside".into());
    r.handle = Some("SV-1".into());
    r.story_id = Some("SV-1".into());
    r
}

fn capture(
    store: &UsageStore,
    record: &SessionRecord,
    inputs: &TaskInputs,
    launcher: Option<&str>,
) -> TaskRecord {
    capture_with(store, record, inputs, None, launcher, None)
}

fn capture_with(
    store: &UsageStore,
    record: &SessionRecord,
    inputs: &TaskInputs,
    routine: Option<&RoutineBrief>,
    launcher: Option<&str>,
    story_file: Option<&Path>,
) -> TaskRecord {
    capture_launch_task(
        store,
        &Capture {
            session_id: &record.session_id,
            record,
            inputs,
            routine,
            launcher_task: launcher,
            story_file,
            now_ms: 1_000,
        },
    )
    .unwrap()
}

fn brief(text: &str) -> TaskInputs {
    TaskInputs {
        brief: Some(text.into()),
        ..Default::default()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

const STORY: &str = "---\ntype: bug\nstatus: ready\ntitle: \"Fix the flicker\"\n---\n\n# SV-9: Fix the flicker\n\n## Purpose\n\nStop the pane flickering when a session ends. It happens on every close.\nSecond line of the paragraph.\n\n## Acceptance criteria\n\n1. nothing.\n";

#[test]
fn a_plain_brief_gives_its_first_lines_as_the_goal_and_skips_directives() {
    let text = "Task-Type: feature\n\n  \nYou are the developer for SV-30: task records.\nBuild the store first.\nThen capture.\nAnd a fourth line that is not part of the goal.\n";
    assert_eq!(
        goal_from_text(text).as_deref(),
        Some("You are the developer for SV-30: task records. Build the store first. Then capture.")
    );
    let long = "x".repeat(600);
    let goal = goal_from_text(&long).unwrap();
    assert_eq!(goal.chars().count(), GOAL_CAP);
    assert!(goal.ends_with('…'));
    assert_eq!(goal_from_text("Task-Type: feature\n\n"), None);
}

#[test]
fn a_brief_that_only_points_at_a_file_takes_its_goal_from_that_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dev-brief.md");
    std::fs::write(
        &file,
        "Task-Type: feature\nSubject: code\nTargets: wall_minutes=60\nYou are the developer for **SV-30**.\nRun $bmad-dev-story.\n",
    )
    .unwrap();
    let pointer = format!("Read {} and follow it.", file.display());
    assert_eq!(
        goal_for_brief(&pointer, None).as_deref(),
        Some("You are the developer for **SV-30**. Run $bmad-dev-story.")
    );
    // A relative path resolves against the launch folder.
    assert_eq!(
        goal_for_brief("Read `dev-brief.md` and follow it.", Some(dir.path())).as_deref(),
        Some("You are the developer for **SV-30**. Run $bmad-dev-story.")
    );
    // The directives and the skill come from the file too.
    let (text, from_file) = brief_text(&pointer, None);
    assert!(from_file);
    let d = directives(&text);
    assert_eq!(d.task_type.as_deref(), Some("feature"));
    assert_eq!(d.subject.as_deref(), Some("code"));
    assert_eq!(d.targets["wall_minutes"], 60.0);
    assert_eq!(
        main_skill(&text, &skills(&["bmad-dev-story"])).as_deref(),
        Some("bmad-dev-story")
    );
    // Prose that merely mentions a file is a brief of its own, as is a missing file.
    let prose = format!("Please review the change described in {} and report every finding you can see, with evidence, to the lead.", file.display());
    assert_eq!(goal_for_brief(&prose, None), goal_from_text(&prose));
    assert_eq!(
        goal_for_brief("Read /no/such/brief.md and follow it.", None).as_deref(),
        Some("Read /no/such/brief.md and follow it.")
    );
}

#[test]
fn a_routines_goal_is_its_name_then_the_first_line_of_its_task() {
    let routine = RoutineBrief {
        name: "Nightly digest".into(),
        task: "\n  Summarise the day's merged PRs.\nPost to the inbox.".into(),
    };
    assert_eq!(
        goal_for_routine(&routine),
        "Nightly digest: Summarise the day's merged PRs."
    );
    let bare = RoutineBrief {
        name: "Heartbeat".into(),
        task: String::new(),
    };
    assert_eq!(goal_for_routine(&bare), "Heartbeat");
}

#[test]
fn a_storys_goal_is_its_title_and_the_first_sentence_of_its_purpose() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("SV-9-fix.md");
    std::fs::write(&file, STORY).unwrap();
    let d = crate::session_usage::story::story_detail(&file);
    assert_eq!(d.kind.as_deref(), Some("bug"));
    assert_eq!(
        goal_for_story("SV-9", d.title.as_deref(), d.purpose.as_deref()),
        "Fix the flicker: Stop the pane flickering when a session ends"
    );
    assert_eq!(goal_for_story("SV-9", None, None), "SV-9");
}

#[test]
fn a_credential_in_a_brief_never_reaches_the_task() {
    let (_d, store) = store();
    let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let t = capture(
        &store,
        &record("tab-1"),
        &brief(&format!("Deploy with token {secret} today")),
        None,
    );
    assert!(!t.goal.contains(&secret), "{}", t.goal);
    assert!(t.goal.contains("Deploy with token"));
}

#[test]
fn each_type_rule_decides_and_names_itself() {
    let none = Directives::default();
    let said = |t: &str| Directives {
        task_type: canonical_type(t),
        ..Default::default()
    };
    let policy: TaskPolicy =
        serde_yaml::from_str("skill_types:\n  bmad-code-review: {type: review, subject: code}\n")
            .unwrap();
    // 1. The brief's own line wins over everything, and common spellings fold.
    let c = resolve_type(
        &said("Bug Fix"),
        true,
        Some("bmad-code-review"),
        Some(&policy),
        Some("spike"),
    );
    assert_eq!((c.task_type.as_str(), c.source), ("bugfix", "brief"));
    assert_eq!(canonical_type("spike").as_deref(), Some("exploratory"));
    assert_eq!(canonical_type("Not A Tag!"), None);
    // 2. A routine.
    let c = resolve_type(
        &none,
        true,
        Some("bmad-code-review"),
        Some(&policy),
        Some("bug"),
    );
    assert_eq!((c.task_type.as_str(), c.source), ("routine", "routine"));
    // 3. The policy's skill map, with its subject.
    let c = resolve_type(
        &none,
        false,
        Some("bmad-code-review"),
        Some(&policy),
        Some("bug"),
    );
    assert_eq!(
        (c.task_type.as_str(), c.subject.as_deref(), c.source),
        ("review", Some("code"), "skill")
    );
    // A skill the map does not list falls through.
    let c = resolve_type(&none, false, Some("other"), Some(&policy), Some("bug"));
    assert_eq!(c.source, "story");
    // 4. The story's type, one case per kind.
    for (kind, want) in [
        ("bug", "bugfix"),
        ("spike", "exploratory"),
        ("story", "feature"),
    ] {
        let c = resolve_type(&none, false, None, None, Some(kind));
        assert_eq!((c.task_type.as_str(), c.source), (want, "story"), "{kind}");
    }
    // 5. Nothing matched: unclassified, with the reason.
    let c = resolve_type(&none, false, None, None, Some("epic"));
    assert_eq!(
        (c.task_type.as_str(), c.source),
        ("unclassified", "unclassified")
    );
    assert!(c.reason.unwrap().contains("no rule matched"));
}

#[test]
fn the_main_skill_is_the_first_provided_skill_the_brief_names() {
    let provided = skills(&["bmad-build", "bmad-code-review"]);
    assert_eq!(
        main_skill("Run $bmad-code-review now", &provided).as_deref(),
        Some("bmad-code-review")
    );
    assert_eq!(
        main_skill("then /bmad-build.", &provided).as_deref(),
        Some("bmad-build")
    );
    assert_eq!(
        main_skill("use bmad-build for this", &provided).as_deref(),
        Some("bmad-build")
    );
    assert_eq!(
        main_skill("Run $bmad-code-review then $bmad-build", &provided).as_deref(),
        Some("bmad-code-review")
    );
    // A skill the launch does not provide, or a path that merely contains the name, is not one.
    assert_eq!(
        main_skill("Run $impeccable and read /x/bmad-build/notes", &provided),
        None
    );
    assert_eq!(main_skill("Task-Type: bmad-build", &provided), None);
}

#[test]
fn the_policy_gives_default_targets_and_the_brief_overrides_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
    std::fs::write(
        dir.path().join(POLICY_FILE),
        "types:\n  feature:\n    targets: {wall_minutes: 240, cost_usd: 25, review_rounds: 3}\n  routine:\n    targets: {wall_minutes: 5}\ntamper_globs: ['**/tests/**', '.github/**']\n",
    )
    .unwrap();
    let (_d, store) = store();
    let inputs = |b: &str| TaskInputs {
        brief: Some(b.into()),
        policy_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let t = capture(
        &store,
        &record("a"),
        &inputs("Task-Type: feature\nBuild it."),
        None,
    );
    let targets = t.targets.unwrap();
    assert_eq!(targets.source, "default");
    assert_eq!(targets.values["wall_minutes"], 240);
    assert_eq!(t.tamper_globs, ["**/tests/**", ".github/**"]);

    let t = capture(
        &store,
        &record("b"),
        &inputs("Task-Type: feature\nTargets: wall_minutes=600 cost_usd=80\nBuild it."),
        None,
    );
    let targets = t.targets.unwrap();
    assert_eq!(targets.source, "brief");
    assert_eq!(
        (
            &targets.values["wall_minutes"],
            &targets.values["review_rounds"]
        ),
        (&serde_json::json!(600.0), &serde_json::json!(3))
    );

    // A type the policy sets nothing for says so.
    let t = capture(
        &store,
        &record("c"),
        &inputs("Task-Type: ops\nRotate keys."),
        None,
    );
    assert!(t.targets.is_none());
    assert!(t.unavailable["targets"].contains("sets none for type 'ops'"));
}

#[test]
fn with_no_policy_targets_say_why() {
    let (_d, store) = store();
    let t = capture(
        &store,
        &record("a"),
        &brief("Task-Type: feature\nBuild it."),
        None,
    );
    assert!(t.targets.is_none());
    assert!(t.unavailable["targets"].contains("no team content folder"));
    let dir = tempfile::tempdir().unwrap();
    let inputs = TaskInputs {
        policy_dir: Some(dir.path().to_path_buf()),
        ..brief("Build it.")
    };
    let t = capture(&store, &record("b"), &inputs, None);
    assert!(t.unavailable["targets"].contains("has no tasks/task-types.yml"));
}

#[test]
fn a_story_file_supplies_the_goal_type_and_root_when_the_launch_has_no_brief() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("SV-9-fix.md");
    std::fs::write(&file, STORY).unwrap();
    let (_d, store) = store();
    let mut r = record("tab-9");
    r.story_id = Some("SV-9".into());
    let t = capture_with(&store, &r, &TaskInputs::default(), None, None, Some(&file));
    assert_eq!(
        t.goal,
        "Fix the flicker: Stop the pane flickering when a session ends"
    );
    assert!(t.unavailable["goal"].contains("no brief"));
    assert_eq!(
        (t.task_type.as_str(), t.type_source.as_deref()),
        ("bugfix", Some("story"))
    );
    assert_eq!(
        (t.parent_id.as_deref(), t.parent_source.as_deref()),
        (Some("story:SV-9"), Some("story"))
    );
    let root = store.read_task("story:SV-9").unwrap().unwrap();
    assert_eq!(root.goal, t.goal);
    assert_eq!(root.task_type, "bugfix");
}

#[test]
fn the_launcher_is_the_parent_whichever_of_the_two_tasks_is_written_first() {
    for launcher_task_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let open = || UsageStore::open(&path).unwrap();
        // The launcher is named by the launch itself: no row of its own is looked up, so the
        // child written first (before the launcher has a session record or a task) is parented.
        let write_launcher = || {
            capture(
                &open(),
                &record("lead-tab"),
                &brief("Task-Type: feature\nLead SV-1."),
                None,
            );
        };
        if launcher_task_first {
            write_launcher();
        }
        let child = capture(
            &open(),
            &record("worker-tab"),
            &brief("Task-Type: review\nReview it."),
            Some("lead-tab"),
        );
        if !launcher_task_first {
            write_launcher();
        }
        assert_eq!(
            child.parent_id.as_deref(),
            Some("lead-tab"),
            "{launcher_task_first}"
        );
        assert_eq!(child.parent_source.as_deref(), Some("launcher"));
        assert!(!child.unavailable.contains_key("parent"));
        // A restart keeps the tree.
        let ids: Vec<String> = open()
            .task_subtree("lead-tab")
            .unwrap()
            .into_iter()
            .map(|t| t.task_id)
            .collect();
        assert_eq!(ids, ["lead-tab", "worker-tab"], "{launcher_task_first}");
    }
}

#[test]
fn a_launch_with_no_resolved_launcher_goes_under_its_story_root_and_says_why() {
    let (_d, store) = store();
    // A person, the app, or a launcher that has exited resolves to nothing at launch.
    let t = capture(&store, &record("human"), &brief("Do it."), None);
    assert_eq!(
        (t.parent_id.as_deref(), t.parent_source.as_deref()),
        (Some("story:SV-1"), Some("story"))
    );
    assert!(t.unavailable["parent"].contains("launched by a person"));
    // A launcher that is the session itself is no parent.
    let t = capture(&store, &record("selfie"), &brief("Do it."), Some("selfie"));
    assert_eq!(t.parent_source.as_deref(), Some("story"));
    assert!(t.unavailable["parent"].contains("the session itself"));
    // A launch with no story has no parent at all.
    let mut r = record("loose");
    r.story_id = None;
    let t = capture(&store, &r, &brief("Do it."), None);
    assert_eq!(
        (t.parent_id, t.parent_source.as_deref()),
        (None, Some("none"))
    );
}

#[test]
fn the_parent_is_the_session_id_resolved_at_launch_and_a_reused_handle_does_not_reparent() {
    let (_d, store) = store();
    store.write_session_record(&record("lead-1")).unwrap();
    store.write_session_record(&record("lead-2")).unwrap();
    // The first worker was launched by the session that held the handle then.
    let first = capture(&store, &record("worker-1"), &brief("One."), Some("lead-1"));
    // The handle is reused by a second session, which launches another worker.
    let second = capture(&store, &record("worker-2"), &brief("Two."), Some("lead-2"));
    assert_eq!(first.parent_id.as_deref(), Some("lead-1"));
    assert_eq!(second.parent_id.as_deref(), Some("lead-2"));
    // Nothing read later re-resolves a handle: the first is still under lead-1.
    assert_eq!(
        store
            .read_task("worker-1")
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some("lead-1")
    );
}

#[test]
fn a_routine_runs_task_is_its_run_id_with_no_parent_and_the_routine_type() {
    let (_d, store) = store();
    let mut r = record("routine-tab");
    r.story_id = Some("routine:acme/riverside/digest@r1".into());
    r.run_id = Some("run-77".into());
    let routine = RoutineBrief {
        name: "Nightly digest".into(),
        task: "Summarise merged PRs.".into(),
    };
    let t = capture_with(
        &store,
        &r,
        &brief("the fired prompt"),
        Some(&routine),
        None,
        None,
    );
    assert_eq!(t.task_id, "run-77");
    assert_eq!(t.goal, "Nightly digest: Summarise merged PRs.");
    assert_eq!(
        (t.task_type.as_str(), t.type_source.as_deref()),
        ("routine", Some("routine"))
    );
    assert_eq!((t.parent_id, t.story_id), (None, None));
    assert_eq!(store.task_links("run-77").unwrap()[0].target, "routine-tab");
}

#[test]
fn the_layer_versions_are_copied_from_the_launch_and_a_missing_one_says_why() {
    let work = repo();
    let context = repo();
    let head = |d: &Path| crate::session_usage::record::current_head(d).unwrap();
    let (_d, store) = store();
    let inputs = |b: &str| TaskInputs {
        brief: Some(b.into()),
        skills: skills(&["bmad-build", "bmad-code-review"]),
        policy_dir: Some(context.path().to_path_buf()),
        ..Default::default()
    };
    let mut lead = record("lead-tab");
    lead.worktree = Some(work.path().to_string_lossy().into_owned());
    let t = capture(&store, &lead, &inputs("Run $bmad-build for SV-1."), None);
    assert_eq!(t.main_skill.as_deref(), Some("bmad-build"));
    assert_eq!(t.layers.source.as_deref(), Some(head(work.path()).as_str()));
    assert_eq!(
        t.layers.context.as_deref(),
        Some(head(context.path()).as_str())
    );
    assert_eq!(t.layers.skills.len(), 2);

    // A folder outside git, and a launch with no content folder, say why.
    let plain = tempfile::tempdir().unwrap();
    let mut loose = record("loose");
    loose.worktree = Some(plain.path().to_string_lossy().into_owned());
    let t = capture(&store, &loose, &brief("Do it."), None);
    assert_eq!((t.layers.source, t.layers.context), (None, None));
    assert!(t.unavailable["source"].contains("not in a git repository"));
    assert!(t.unavailable["context"].contains("not in a git repository"));
}

#[test]
fn a_second_capture_changes_nothing_and_a_session_link_is_kept() {
    let (_d, store) = store();
    let r = record("tab-1");
    let first = capture(&store, &r, &brief("First goal."), None);
    store
        .close_task("tab-1", "done", Some("success"), 5)
        .unwrap();
    let again = capture(&store, &r, &brief("Second goal."), None);
    assert_eq!(first.goal, "First goal.");
    assert_eq!(
        (again.goal.as_str(), again.status.as_str()),
        ("First goal.", "done")
    );
    assert_eq!(store.task_links("tab-1").unwrap().len(), 1);
}

#[test]
fn the_inputs_of_a_prepared_launch_are_its_brief_its_skills_and_its_team_content_folder() {
    let (home, _root, launch, _) = crate::agents::product_bootstrap::tests::fixture();
    let inputs = TaskInputs::from_launch(&launch, Some("  Do the thing.\n"));
    assert_eq!(inputs.brief.as_deref(), Some("Do the thing."));
    let provided = crate::agents::provides::derive(launch.runtime, &launch.manifest).skills;
    let named: Vec<&str> = inputs.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        named,
        provided.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(inputs
        .skills
        .iter()
        .all(|s| s.hash.is_some() == s.reason.is_none()));
    let dir = inputs.policy_dir.expect("the team's content folder exists");
    assert!(dir.starts_with(home.path()), "{dir:?}");
    assert_eq!(TaskInputs::from_launch(&launch, Some("  \n")).brief, None);
    assert_eq!(TaskInputs::from_launch(&launch, None).brief, None);
}

#[test]
fn the_effective_prompt_is_the_launchs_own_or_the_agents_default() {
    let (_home, _root, mut launch, _) = crate::agents::product_bootstrap::tests::fixture();
    launch.default_prompt =
        Some("Task-Type: ops\nRun $bmad-build for the agent's default goal.".into());
    // No prompt of its own: the agent starts with its default, and the task is made from it.
    let inputs = TaskInputs::from_launch(&launch, None);
    assert_eq!(inputs.brief.as_deref(), launch.default_prompt.as_deref());
    let (_d, store) = store();
    let t = capture(&store, &record("tab-d"), &inputs, None);
    assert_eq!(t.goal, "Run $bmad-build for the agent's default goal.");
    assert_eq!(
        (t.task_type.as_str(), t.type_source.as_deref()),
        ("ops", Some("brief"))
    );
    // A prompt of its own wins, and a blank one does not.
    assert_eq!(
        TaskInputs::from_launch(&launch, Some("Mine."))
            .brief
            .as_deref(),
        Some("Mine.")
    );
    assert_eq!(
        TaskInputs::from_launch(&launch, Some("  ")).brief,
        launch.default_prompt
    );
}

#[test]
fn one_goal_rule_caps_and_redacts_whoever_wrote_the_goal() {
    let secret = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let long = format!("{} {}", "word".repeat(100), secret);
    let goal = normalize_goal(&format!("  Deploy {secret} now  "));
    assert!(!goal.contains(&secret), "{goal}");
    let goal = normalize_goal(&long);
    assert_eq!(goal.chars().count(), GOAL_CAP);
    assert!(!goal.contains(&secret));
    assert_eq!(normalize_goal("short"), "short");
}

#[test]
fn a_subject_is_a_free_tag_and_only_a_type_takes_aliases() {
    assert_eq!(subject_tag(" Research ").as_deref(), Some("research"));
    assert_eq!(subject_tag("bug").as_deref(), Some("bug"));
    assert_eq!(subject_tag("A$B"), None);
    assert_eq!(canonical_type("bug").as_deref(), Some("bugfix"));
}

#[test]
fn a_routine_start_makes_the_bare_task_and_the_launch_completes_it_once() {
    let (_d, store) = store();
    let routine = RoutineBrief {
        name: "Nightly digest".into(),
        task: "Summarise merged PRs with $bmad-build.".into(),
    };
    let start = RoutineStart {
        org: "acme",
        team: "riverside",
        run_id: "run-5",
        routine: &routine,
        policy_dir: None,
        cwd: None,
        session_id: None,
        now_ms: 7,
    };
    capture_routine_start(&store, &start).unwrap();
    capture_routine_start(&store, &start).unwrap();
    let bare = store.read_task("run-5").unwrap().unwrap();
    assert_eq!(
        (bare.task_type.as_str(), bare.created_by.as_deref()),
        ("routine", Some(ROUTINE_START))
    );
    assert_eq!(
        bare.goal,
        "Nightly digest: Summarise merged PRs with $bmad-build."
    );
    assert!(bare.layers.skills.is_empty());
    // A fresh launch of the same run adds its facts to the bare task, once.
    let mut r = record("routine-tab");
    r.run_id = Some("run-5".into());
    r.story_id = None;
    let inputs = TaskInputs {
        skills: skills(&["bmad-build"]),
        ..brief("Run $bmad-build.")
    };
    let done = capture_with(&store, &r, &inputs, Some(&routine), None, None);
    assert_eq!(done.task_id, "run-5");
    assert_eq!(done.main_skill.as_deref(), Some("bmad-build"));
    assert_eq!(done.layers.skills.len(), 1);
    assert_ne!(done.created_by.as_deref(), Some(ROUTINE_START));
    // The task keeps the wake's start time and is not completed twice.
    assert_eq!(done.created_at_ms, 7);
    let mut again = inputs.clone();
    again.skills = skills(&["bmad-build", "other"]);
    let twice = capture_with(&store, &r, &again, Some(&routine), None, None);
    assert_eq!(twice.layers.skills.len(), 1);
}

#[tokio::test]
async fn an_early_close_is_visible_as_pending_and_late_capture_keeps_the_first_verdict_after_reopen() {
    use crate::session_usage::task::{ABANDONED, Visibility, TaskFilter};
    use crate::session_usage::task_outcome::{close_for_session, close_for_session_async};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    let r = record("early-session");
    open().write_session_record(&r).unwrap();
    close_for_session_async(Ok(path.clone()), &r.session_id, "failed", Some(r.session_id.clone())).await;
    let pending = open().read_task(&r.session_id).unwrap().unwrap();
    assert_eq!(pending.goal, "Task capture pending");
    assert_eq!(pending.task_type, "unclassified");
    for key in ["goal", "type", "tamper"] {
        assert!(pending.unavailable[key].contains("pending"));
    }
    let pending_view = open().task_view(&r.session_id).unwrap().unwrap();
    assert_eq!(pending_view.task.goal, "Task capture pending");
    assert!(pending_view.task.unavailable["type"].contains("pending"));
    let listed = open().list_tasks(&TaskFilter {
        visible_to: Some(Visibility { org: r.org.clone(), team: r.team.clone() }),
        ..Default::default()
    }).unwrap();
    assert!(listed.iter().any(|t| t.task_id == r.session_id));
    assert!(close_for_session(&open(), &r.session_id, "success", 3_000).unwrap().is_empty());
    let captured = capture(&open(), &r, &brief("Task-Type: feature\nImplement the task records."), None);
    assert_eq!(captured.goal, "Implement the task records.");
    assert_eq!(captured.task_type, "feature");
    assert_eq!(captured.status, ABANDONED);
    assert_eq!(captured.raw_status.as_deref(), Some("failed"));
    assert_eq!(captured.closed_at_ms, pending.closed_at_ms);
    assert!(!captured.unavailable["tamper"].contains("pending"));
    let duplicate = capture(&open(), &r, &brief("Task-Type: ops\nA later brief"), None);
    assert_eq!(duplicate, captured);
    assert_eq!(open().task_view(&r.session_id).unwrap().unwrap().task, captured);
}

#[test]
fn a_task_update_between_routine_start_and_launch_enrichment_remains_authoritative_after_reopen() {
    use crate::mcp::task_tools;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    let routine = RoutineBrief { name: "Review".into(), task: "Task-Type: ops\nSubject: research\nTargets: wall_minutes=60\nInvestigate.".into() };
    capture_routine_start(&open(), &RoutineStart {
        org: "acme", team: "riverside", run_id: "refined-run", routine: &routine,
        policy_dir: None, cwd: None, session_id: None, now_ms: 7,
    }).unwrap();
    let mut r = record("refining-session");
    r.run_id = Some("refined-run".into());
    open().write_session_record(&r).unwrap();
    task_tools::update(&open(), &r.session_id, serde_json::from_value(serde_json::json!({
        "sessionId": r.session_id, "taskId": "refined-run", "type": "review", "subject": "security",
        "targets": {"wall_minutes": 90}, "status": "done"
    })).unwrap(), 8).unwrap();
    let enriched = capture_with(&open(), &r, &brief("Original launch"), Some(&routine), None, None);
    assert_eq!((enriched.task_type.as_str(), enriched.subject.as_deref(), enriched.type_source.as_deref()),
        ("review", Some("security"), Some("task_update")));
    let targets = enriched.targets.as_ref().unwrap();
    assert_eq!(targets.source, "task_update");
    assert_eq!(targets.values["wall_minutes"], 90);
    assert_eq!((enriched.status.as_str(), enriched.closed_at_ms), ("done", Some(8)));
    assert_eq!(open().read_task("refined-run").unwrap().unwrap(), enriched);
}

fn capture_relative_pointer_case(pointer: &str, parent: bool) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let file = if parent { dir.path().join("brief.md") } else { cwd.join("brief.md") };
    std::fs::write(file, "Task-Type: ops\nSubject: research\nTargets: wall_minutes=60 cost_usd=5\nCapture the relative brief.\nRun $bmad-build.\n").unwrap();
    let (_db, store) = store();
    let mut record = record("relative-pointer-session");
    record.worktree = Some(cwd.to_string_lossy().into_owned());
    let inputs = TaskInputs { skills: skills(&["bmad-build"]), ..brief(pointer) };
    let task = capture(&store, &record, &inputs, None);
    assert_eq!(task.goal, "Capture the relative brief. Run $bmad-build.", "{pointer}");
    assert_eq!(task.task_type, "ops", "{pointer}");
    assert_eq!(task.subject.as_deref(), Some("research"), "{pointer}");
    assert_eq!(task.main_skill.as_deref(), Some("bmad-build"), "{pointer}");
    let targets = task.targets.unwrap();
    assert_eq!(targets.values["wall_minutes"], 60.0, "{pointer}");
    assert_eq!(targets.values["cost_usd"], 5.0, "{pointer}");
}

#[test]
fn dot_relative_brief_file_supplies_goal_and_directives() {
    capture_relative_pointer_case("Read ./brief.md and follow it.", false);
}

#[test]
fn parent_relative_brief_file_supplies_goal_and_directives() {
    capture_relative_pointer_case("Read ../brief.md and follow it.", true);
}

#[test]
fn quoted_relative_brief_files_supply_goal_and_directives() {
    capture_relative_pointer_case("Read 'brief.md' and follow it.", false);
    capture_relative_pointer_case("Read \"./brief.md\" and follow it.", false);
    capture_relative_pointer_case("Read '../brief.md' and follow it.", true);
}

#[test]
fn backtick_relative_brief_file_supplies_goal_and_directives() {
    capture_relative_pointer_case("Read `../brief.md` and follow it.", true);
}

#[test]
fn sentence_punctuation_after_relative_brief_keeps_goal_and_directives() {
    capture_relative_pointer_case("Read ./brief.md.", false);
    capture_relative_pointer_case("Read \"../brief.md\".", true);
}

fn links(store: &UsageStore, task: &str, kind: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .task_links(task)
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == kind)
        .map(|l| l.target)
        .collect();
    out.sort();
    out
}

/// SV-CDE-83 I1, case 1: a restore relaunch (`launched_by: "CodeSense restore"`, no launcher)
/// keeps its story, team and task parent, and its task continues the predecessor's.
#[test]
fn cde83_a_restore_relaunch_continues_its_predecessor() {
    let (_d, store) = store();
    let first = capture(
        &store,
        &record("old-tab"),
        &brief("Build it."),
        Some("lead-task"),
    );
    assert_eq!(first.parent_id.as_deref(), Some("lead-task"));
    let mut restored = record("new-tab");
    restored.launched_by = Some("CodeSense restore".into());
    let inputs = TaskInputs {
        continues: Some("old-tab".into()),
        ..brief("Resume.")
    };
    let t = capture(&store, &restored, &inputs, None);
    assert_eq!(t.story_id.as_deref(), Some("SV-1"));
    assert_eq!(t.field_sources["story"], "launch");
    assert_eq!(
        (t.org.as_deref(), t.team.as_deref()),
        (Some("acme"), Some("riverside"))
    );
    assert_eq!(t.parent_id.as_deref(), Some("lead-task"));
    assert_eq!(t.parent_source.as_deref(), Some("continues"));
    assert!(!t.unavailable.contains_key("parent"));
    assert_eq!(t.created_by.as_deref(), Some("CodeSense restore"));
    assert_eq!(
        links(&store, "new-tab", "relates_to:continues"),
        vec!["old-tab"]
    );
    // story_changes reads tasks by story: both sessions' tasks are the story's.
    let ids: Vec<String> = store
        .tasks_for_story("SV-1")
        .unwrap()
        .into_iter()
        .map(|t| t.task_id)
        .collect();
    assert!(ids.contains(&"old-tab".to_string()) && ids.contains(&"new-tab".to_string()));

    // A relaunch whose launch lost its story takes the predecessor's.
    let mut bare = record("bare-tab");
    bare.story_id = None;
    let inputs = TaskInputs {
        continues: Some("new-tab".into()),
        ..brief("Resume.")
    };
    let t = capture(&store, &bare, &inputs, None);
    assert_eq!(t.story_id.as_deref(), Some("SV-1"));
    assert_eq!(t.field_sources["story"], "continues");
    assert_eq!(
        links(&store, "bare-tab", "relates_to:continues"),
        vec!["new-tab"]
    );

    // A predecessor the store does not know is no link and no guess.
    let mut stray = record("stray-tab");
    stray.story_id = None;
    let inputs = TaskInputs {
        continues: Some("never-seen".into()),
        ..brief("Resume.")
    };
    let t = capture(&store, &stray, &inputs, None);
    assert_eq!(t.story_id, None);
    assert!(links(&store, "stray-tab", "relates_to:continues").is_empty());
}

/// Case 2: a hand-relaunch in a story workspace with no predecessor takes the workspace's
/// story. Case 3: a non-story workspace gets none.
#[test]
fn cde83_a_relaunch_takes_its_workspace_story_and_nothing_else() {
    let (_d, store) = store();
    let mut hand = record("hand-tab");
    hand.story_id = None;
    let inputs = TaskInputs {
        workspace_story: Some("SV-7".into()),
        ..brief("Carry on.")
    };
    let t = capture(&store, &hand, &inputs, None);
    assert_eq!(t.story_id.as_deref(), Some("SV-7"));
    assert_eq!(t.field_sources["story"], "workspace");
    assert_eq!(t.parent_source.as_deref(), Some("story"));

    let mut plain = record("plain-tab");
    plain.story_id = None;
    let t = capture(&store, &plain, &brief("Carry on."), None);
    assert_eq!(t.story_id, None);
    assert_eq!(t.field_sources["story"], "none");

    // The launch's own story wins over its folder's.
    let inputs = TaskInputs {
        workspace_story: Some("SV-7".into()),
        ..brief("Carry on.")
    };
    let t = capture(&store, &record("own-tab"), &inputs, None);
    assert_eq!(
        (t.story_id.as_deref(), t.field_sources["story"].as_str()),
        (Some("SV-1"), "launch")
    );
}

/// I3, case 4: a story launch with role `reviewer` naming the dev task is a review task, typed
/// by its role, linked `relates_to:reviews` to that task exactly.
#[test]
fn cde83_a_reviewer_launch_naming_its_task_is_an_exact_review() {
    let (_d, store) = store();
    capture(&store, &record("dev-a"), &brief("Build it."), None);
    capture(&store, &record("dev-b"), &brief("Build more."), None);
    let inputs = TaskInputs {
        role: Some("reviewer".into()),
        reviews: Some("dev-a".into()),
        ..brief("Review PR 12.")
    };
    let t = capture(&store, &record("review-tab"), &inputs, None);
    assert_eq!(t.task_type, "review");
    assert_eq!(t.type_source.as_deref(), Some("role"));
    assert!(!t.unavailable.contains_key("type"));
    assert_eq!(
        links(&store, "review-tab", "relates_to:reviews"),
        vec!["dev-a"]
    );
    assert_eq!(t.field_sources["reviews"], "exact");
    assert_eq!(store.review_tasks_of("dev-a").unwrap(), vec!["review-tab"]);
}

/// Case 5: with no `reviews`, the story's latest open non-review task is linked, `associated`;
/// a named task this Mac does not know falls back the same way and says why.
#[test]
fn cde83_a_reviewer_launch_without_a_task_links_the_latest_open_one() {
    let (_d, store) = store();
    let early = record("dev-early");
    capture_launch_task(
        &store,
        &Capture {
            session_id: "dev-early",
            record: &early,
            inputs: &brief("Build it."),
            routine: None,
            launcher_task: None,
            story_file: None,
            now_ms: 500,
        },
    )
    .unwrap();
    capture(&store, &record("dev-late"), &brief("Build more."), None);
    capture(&store, &record("dev-done"), &brief("Done already."), None);
    store
        .close_task("dev-done", "done", Some("success"), 2_000)
        .unwrap();
    let reviewer = |reviews: Option<&str>| TaskInputs {
        role: Some("Code-Reviewer".into()),
        reviews: reviews.map(str::to_owned),
        ..brief("Review it.")
    };
    let t = capture(&store, &record("review-1"), &reviewer(None), None);
    assert_eq!(
        links(&store, "review-1", "relates_to:reviews"),
        vec!["dev-late"]
    );
    assert_eq!(t.field_sources["reviews"], "associated");
    assert!(!t.unavailable.contains_key("reviews"));

    let t = capture(&store, &record("review-2"), &reviewer(Some("gone")), None);
    assert_eq!(
        links(&store, "review-2", "relates_to:reviews"),
        vec!["dev-late"]
    );
    assert_eq!(t.field_sources["reviews"], "associated");
    assert!(t.unavailable["reviews"].contains("gone"));
}

#[test]
fn cde83_a_reviewer_with_nothing_to_review_says_why() {
    let (_d, store) = store();
    let inputs = TaskInputs {
        role: Some("reviewer".into()),
        ..brief("Review it.")
    };
    let t = capture(&store, &record("lonely-review"), &inputs, None);
    assert_eq!(t.task_type, "review");
    assert!(links(&store, "lonely-review", "relates_to:reviews").is_empty());
    assert!(t.unavailable["reviews"].contains("no open task"));
}

/// Who is a reviewer: the role, else the saved agent's queue role or name; a word that starts
/// with `review`, never one that only contains it. A brief's `Task-Type` still wins, and a
/// launch with no story is not typed by its role.
#[test]
fn cde83_reviewer_typing_reads_role_then_agent_and_yields_to_the_brief() {
    let with = |role: Option<&str>, queue: Option<&str>| TaskInputs {
        role: role.map(str::to_owned),
        queue_role: queue.map(str::to_owned),
        ..Default::default()
    };
    assert_eq!(
        reviewer_source(&with(Some("reviewer"), None), None),
        Some("role")
    );
    assert_eq!(
        reviewer_source(&with(Some("worker"), Some("reviewers")), None),
        Some("agent")
    );
    assert_eq!(
        reviewer_source(&with(None, None), Some("acme/ctx/riverside/dev-reviewer@3")),
        Some("agent")
    );
    assert_eq!(
        reviewer_source(
            &with(Some("preview"), None),
            Some("acme/ctx/riverside/dev@1")
        ),
        None
    );
    assert_eq!(
        reviewer_source(&with(None, None), Some("acme/reviewers-team/dev@1")),
        None
    );

    let (_d, store) = store();
    let briefed = TaskInputs {
        role: Some("reviewer".into()),
        ..brief("Task-Type: bugfix\nFix it.")
    };
    let t = capture(&store, &record("briefed"), &briefed, None);
    assert_eq!(
        (t.task_type.as_str(), t.type_source.as_deref()),
        ("bugfix", Some("brief"))
    );
    let mut storyless = record("storyless");
    storyless.story_id = None;
    let t = capture(
        &store,
        &storyless,
        &TaskInputs {
            role: Some("reviewer".into()),
            ..brief("Look.")
        },
        None,
    );
    assert_ne!(t.task_type, "review");
}

/// r1 L2: an exact `reviews` link is only to a task of the same story and team; a named task of
/// another story or team falls back to `associated` and says why.
#[test]
fn cde83_a_named_task_of_another_story_or_team_is_never_an_exact_review() {
    let (_d, store) = store();
    capture(&store, &record("dev-a"), &brief("Build it."), None);
    let mut other_story = record("dev-other-story");
    other_story.story_id = Some("SV-2".into());
    capture(&store, &other_story, &brief("Build SV-2."), None);
    let mut other_team = record("dev-other-team");
    other_team.team = Some("elsewhere".into());
    capture(&store, &other_team, &brief("Build it elsewhere."), None);
    for (named, why) in [("dev-other-story", "another story"), ("dev-other-team", "another team")] {
        let inputs = TaskInputs {
            role: Some("reviewer".into()),
            reviews: Some(named.into()),
            ..brief("Review it.")
        };
        let id = format!("review-{named}");
        let t = capture(&store, &record(&id), &inputs, None);
        assert_eq!(links(&store, &id, "relates_to:reviews"), vec!["dev-a"], "{named}");
        assert_eq!(t.field_sources["reviews"], "associated", "{named}");
        assert!(t.unavailable["reviews"].contains(why), "{named}: {}", t.unavailable["reviews"]);
    }
}

/// r1 L3: a restored reviewer keeps the exact target of the review it continues, rather than
/// the story's latest open task.
#[test]
fn cde83_a_restored_reviewer_keeps_its_exact_review_target() {
    let (_d, store) = store();
    capture(&store, &record("dev-a"), &brief("Build it."), None);
    let reviewer = TaskInputs {
        role: Some("reviewer".into()),
        reviews: Some("dev-a".into()),
        ..brief("Review PR 12.")
    };
    let first = capture(&store, &record("review-old"), &reviewer, None);
    assert_eq!(first.field_sources["reviews"], "exact");
    let mut later = record("dev-b");
    later.recorded_at_ms = 5_000;
    capture_launch_task(&store, &Capture {
        session_id: "dev-b", record: &later, inputs: &brief("Build more."), routine: None,
        launcher_task: None, story_file: None, now_ms: 5_000,
    })
    .unwrap();

    let mut restored = record("review-new");
    restored.launched_by = Some("CodeSense restore".into());
    let inputs = TaskInputs {
        role: Some("reviewer".into()),
        continues: Some("review-old".into()),
        ..brief("Resume the review.")
    };
    let t = capture(&store, &restored, &inputs, None);
    assert_eq!(t.task_type, "review");
    assert_eq!(links(&store, "review-new", "relates_to:reviews"), vec!["dev-a"], "not the later dev-b");
    assert_eq!(t.field_sources["reviews"], "exact");
    assert!(!t.unavailable.contains_key("reviews"));
    assert_eq!(links(&store, "review-new", "relates_to:continues"), vec!["review-old"]);
}

/// r1 L4: `reviews` on a launch whose task is not a review links nothing and says so in
/// `unavailable.reviews`, never silently.
#[test]
fn cde83_reviews_on_a_launch_that_is_not_a_review_is_recorded_not_dropped() {
    let (_d, store) = store();
    capture(&store, &record("dev-a"), &brief("Build it."), None);
    let inputs = TaskInputs { reviews: Some("dev-a".into()), ..brief("Build more.") };
    let t = capture(&store, &record("not-a-reviewer"), &inputs, None);
    assert_ne!(t.task_type, "review");
    assert!(links(&store, "not-a-reviewer", "relates_to:reviews").is_empty());
    assert!(!t.field_sources.contains_key("reviews"));
    let why = &t.unavailable["reviews"];
    assert!(why.contains("dev-a") && why.contains("not a review"), "{why}");
}
