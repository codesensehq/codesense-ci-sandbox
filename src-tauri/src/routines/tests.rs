// SPDX-License-Identifier: MIT
//! Seam tests for the routine service: real temp homes, real org layouts, the
//! real coordinator file and the real projection path.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use crate::context_repo::{path_to_org_root, path_to_team_config_dir, path_to_team_content_dir};
use crate::domain::Principal;

use super::defs;
use super::discover::{discover_routines, RoutineScope};
use super::model::{
    AgentRef, Author, AuthorKind, Policy, RoutineDef, RoutineKey, RoutineTrigger, ScheduleSpec,
    ValidationError, Where,
};
use super::service::{
    self, PersonalOwner, Placement, RoutineError, SaveTarget, MAX_ACTIVE_ROUTINES,
};
use super::state::RoutineStateStore;
use super::test_support::copy_dir;

const ORG: &str = "acme/api";
const TEAM: &str = "backend";

const PEOPLE_YML: &str = r#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice A
  - github_user_id: 2
    github_username: bob
"#;
const TEAMS_YML: &str = r#"
teams:
  - name: backend
  - name: frontend
"#;

fn alice() -> Principal {
    Principal::from(1u64)
}

fn bob() -> Principal {
    Principal::from(2u64)
}

fn user() -> Author {
    Author::user("alice")
}

/// A locally-cloned org for `user_id`, with `backend` synced.
fn write_org(home: &Path, user_id: u64, slug: &str) -> PathBuf {
    let org_root = path_to_org_root(home, &Principal::from(user_id), slug).unwrap();
    if slug == ORG {
        crate::context_repo::scratch_space::set_active_org(home, Some(ORG));
    }
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {slug}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(org_root.join("people.yml"), PEOPLE_YML).unwrap();
    std::fs::write(org_root.join("teams.yml"), TEAMS_YML).unwrap();
    std::fs::create_dir_all(path_to_team_content_dir(&org_root, TEAM)).unwrap();
    // The team agent the test routines run (they live under it, SW-AGR-1).
    if slug == ORG {
        let triage = path_to_team_config_dir(&org_root, TEAM).join("agents/triage/agent.yaml");
        if !triage.exists() {
            super::test_support::save_team_agent(
                home,
                &Principal::from(user_id),
                super::test_support::agent(
                    "triage",
                    codesense_pty_core::agent_cli::AgentCli::Claude,
                ),
            );
        }
    }
    org_root
}

#[test]
fn installed_reviewer_example_is_discovered_with_its_agent_and_valid_schedule() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path(), 1, ORG);
    let example = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../codesense-docs/getting-started/content/examples/agents/reviewer");
    let dir = path_to_team_config_dir(&org_root, TEAM).join("agents/reviewer");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(example.join("agent.yaml"), dir.join("agent.yaml")).unwrap();
    copy_dir(&example.join("routines"), &dir.join("routines"));
    crate::agents::defs::read_def(&dir)
        .unwrap()
        .unwrap()
        .validate()
        .unwrap();
    let found = discover_routines(home.path(), &alice()).unwrap();
    let review = found
        .iter()
        .find(|r| r.def.id == "while-you-were-away")
        .unwrap();
    assert!(!review.unassigned);
    assert_eq!(review.def.agent.as_str(), "reviewer");
    review.def.validate().unwrap();
    assert_eq!(
        review.def.triggers,
        vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 7 * * 1-5".into(),
            tz: "America/Los_Angeles".into(),
        })]
    );
}

fn team() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

fn personal(user_id: u64) -> Placement {
    Placement::Personal(PersonalOwner {
        github_user_id: user_id,
        username: "alice".into(),
        display_name: "Alice A".into(),
        avatar_url: None,
        org: "acme/api".into(),
    })
}

fn routine(id: &str) -> RoutineDef {
    RoutineDef {
        action: Default::default(),
        command: Vec::new(),
        after: None,
        id: id.into(),
        name: format!("Routine {id}"),
        owner: String::new(),
        agent: AgentRef::from("triage"),
        mode: String::new(),
        task: "Sweep since {{last_run_at}}".into(),
        target: Where::team_workspace(),
        triggers: vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * 1-5".into(),
            tz: "America/Denver".into(),
        })],
        policy: Policy {
            max_runtime: Some("30m".into()),
            ..Policy::default()
        },
        variables: Default::default(),
        revision: 0,
        armed_host: String::new(),
        result_schema: None,
        runs_on: String::new(),
        archived_at: None,
        armed_machine: None,
        arm_epoch: 0,
        handover_from: None,
        archived_with_agent: false,
        migrated: None,
        extra: Default::default(),
    }
}

fn key(id: &str) -> RoutineKey {
    RoutineKey::new(ORG, TEAM, id)
}

fn create(home: &Path, id: &str) -> RoutineDef {
    service::save(
        home,
        &alice(),
        SaveTarget::Create(team()),
        routine(id),
        &user(),
    )
    .unwrap()
    .def
}

fn routine_dir(home: &Path, principal: &Principal, id: &str) -> PathBuf {
    let org_root = path_to_org_root(home, principal, ORG).unwrap();
    defs::routine_dir(&path_to_team_config_dir(&org_root, TEAM), id)
}

#[test]
fn create_writes_revision_one_owned_and_armed_here() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let def = create(home.path(), "triage");

    assert_eq!(def.revision, 1);
    assert_eq!(def.owner, "1");
    assert_eq!(def.armed_host, super::local_host_id(home.path()).unwrap());
    let dir = routine_dir(home.path(), &alice(), "triage");
    // Definitions live in the team's git-tracked config dir, not under content/.
    assert!(dir.ends_with("backend/config/agents/triage/routines/triage"));
    assert_eq!(defs::read_def(&dir).unwrap().unwrap(), def);
    let rev = defs::read_revision(&dir, 1).unwrap().unwrap();
    assert_eq!(rev.routine, def);
    assert_eq!(rev.author.kind, AuthorKind::User);
    assert_eq!(rev.author.name, "alice");

    // The same id again is refused.
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            routine("triage"),
            &user()
        )
        .unwrap_err(),
        RoutineError::AlreadyExists(key("triage"))
    );
}

// SW-CR-1: `builtin` is reserved for starter definitions. The slug validator
// rejects the bare `builtin`; `check_org` catches a team-shaped `builtin/<x>`
// org that would otherwise sail through the relaxed gate (mirrors the agents
// service's parallel guard).
// SW-CR-5: the reservation is case-insensitive (ASCII) so a mixed-case
// `Builtin` / `Builtin/foo` / `_Personal/foo` can't alias onto the reserved
// namespace on a case-folding filesystem.
#[test]
fn create_refuses_a_routine_whose_org_is_the_reserved_builtin_prefix() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    for org in [
        "builtin",
        "builtin/foo",
        "Builtin",
        "BUILTIN/foo",
        "_Personal/foo",
    ] {
        let err = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: org.into(),
                team: TEAM.into(),
            }),
            routine("triage"),
            &user(),
        )
        .unwrap_err();
        assert!(
            matches!(err, RoutineError::InvalidLocation(_)),
            "{org}: {err:?}"
        );
    }
}

#[test]
fn a_profile_style_agent_ref_is_refused_as_an_ordinary_invalid_agent() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let mut profile = routine("triage");
    profile.agent = AgentRef::from("codex:default");
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            profile,
            &user()
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::InvalidAgent("codex:default".into()))
    );
    // A hand-written `{pack, id}` map is not an agent reference at all: the
    // definition does not read, so nothing can save or run it.
    let def = create(home.path(), "mapped");
    let dir = routine_dir(home.path(), &alice(), "mapped");
    let yaml = serde_yaml::to_string(&def)
        .unwrap()
        .replace("agent: triage", "agent: {pack: codex, id: default}");
    std::fs::write(dir.join("routine.yml"), yaml).unwrap();
    assert!(defs::read_def(&dir).is_err());
    assert!(service::get(home.path(), &alice(), &key("mapped")).is_err());
}

#[test]
fn create_rejects_invalid_definitions_and_credits_its_creator() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let mut bad = routine("triage");
    bad.task = "{{nope}}".into();
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            bad,
            &user()
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::UnknownToken("nope".into()))
    );
    // A routine never delivers; a `deliver` key is refused, not carried.
    let mut delivering = routine("triage");
    delivering
        .extra
        .insert("deliver".into(), serde_yaml::Value::from("inbox"));
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            delivering,
            &user()
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::DeliverUnsupported)
    );
    // `owner` records who created it, so a supplied one is replaced, not refused.
    let mut named = routine("named");
    named.owner = "2".into();
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(team()),
        named,
        &user(),
    )
    .unwrap();
    assert_eq!(saved.def.owner, "1");
    // An unsynced team is not a placement.
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: "frontend".into()
            }),
            routine("triage"),
            &user()
        ),
        Err(RoutineError::InvalidLocation(_))
    ));
    // A team dir that is not a registered, synced team is not a placement
    // either: discovery would never scan it.
    let org_root = path_to_org_root(home.path(), &alice(), ORG).unwrap();
    std::fs::create_dir_all(path_to_team_content_dir(&org_root, "rogue")).unwrap();
    assert!(matches!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: "rogue".into()
            }),
            routine("triage"),
            &user()
        ),
        Err(RoutineError::InvalidLocation(_))
    ));
    // An invalid first personal save leaves no personal space behind.
    let mut invalid = routine("journal");
    invalid.policy.max_runtime = None;
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(personal(1)),
            invalid,
            &user()
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::MaxRuntimeRequired)
    );
    assert!(
        crate::context_repo::scratch_space::org_space(home.path(), &alice(), ORG).is_none(),
        "a refused save leaves no scratch space behind"
    );
    // A traversal key never resolves.
    assert!(matches!(
        service::get(
            home.path(),
            &alice(),
            &RoutineKey::new("../x", TEAM, "triage")
        ),
        Err(RoutineError::InvalidLocation(_))
    ));
    assert!(!routine_dir(home.path(), &alice(), "triage").exists());
}

#[test]
fn update_bumps_revision_and_stale_base_conflicts_without_writing() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let v1 = create(home.path(), "triage");

    let mut edit = v1.clone();
    edit.name = "Renamed".into();
    // A caller cannot smuggle an owner or host change through a plain save.
    edit.owner = "2".into();
    edit.armed_host = "elsewhere".into();
    let v2 = service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: 1,
        },
        edit,
        &Author::agent("triage-agent"),
    )
    .unwrap()
    .def;
    assert_eq!(v2.revision, 2);
    assert_eq!(v2.name, "Renamed");
    assert_eq!(v2.owner, "1");
    assert_eq!(v2.armed_host, v1.armed_host);

    let dir = routine_dir(home.path(), &alice(), "triage");
    let before = std::fs::read(dir.join("routine.yml")).unwrap();
    let mut stale = v1.clone();
    stale.name = "Lost edit".into();
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Update {
                key: key("triage"),
                base_revision: 1
            },
            stale,
            &user()
        )
        .unwrap_err(),
        RoutineError::Conflict {
            current_revision: 2
        }
    );
    assert_eq!(std::fs::read(dir.join("routine.yml")).unwrap(), before);
    assert!(defs::read_revision(&dir, 3).unwrap().is_none());

    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    let authors: Vec<_> = detail
        .revisions
        .iter()
        .map(|r| (r.revision, r.author.kind))
        .collect();
    assert_eq!(authors, vec![(1, AuthorKind::User), (2, AuthorKind::Agent)]);

    let current = service::get_current(home.path(), &alice(), &key("triage")).unwrap();
    assert_eq!(current.def, detail.def);
    assert_eq!(current.enabled, detail.enabled);
    assert!(current.revisions.is_empty(), "the history is not read");
}

#[test]
fn concurrent_saves_with_one_base_yield_exactly_one_conflict() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let v1 = create(home.path(), "triage");
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<Result<_, _>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let mut edit = v1.clone();
                edit.name = format!("edit {i}");
                let home = home.path();
                s.spawn(move || {
                    barrier.wait();
                    service::save(
                        home,
                        &alice(),
                        SaveTarget::Update {
                            key: key("triage"),
                            base_revision: 1,
                        },
                        edit,
                        &user(),
                    )
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(RoutineError::Conflict {
                    current_revision: 2
                })
            )
        })
        .count();
    assert_eq!((ok, conflicts), (1, 1), "{results:?}");
}

#[test]
fn restore_writes_a_new_revision_with_the_old_body() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let v1 = create(home.path(), "triage");
    let mut edit = v1.clone();
    edit.task = "Something else".into();
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: 1,
        },
        edit,
        &user(),
    )
    .unwrap();

    let v3 = service::restore(home.path(), &alice(), &key("triage"), 1, 2, &user())
        .unwrap()
        .def;
    assert_eq!(v3.revision, 3);
    assert_eq!(RoutineDef { revision: 1, ..v3 }, v1);

    assert_eq!(
        service::restore(home.path(), &alice(), &key("triage"), 1, 2, &user()).unwrap_err(),
        RoutineError::Conflict {
            current_revision: 3
        }
    );
    assert_eq!(
        service::restore(home.path(), &alice(), &key("triage"), 9, 3, &user()).unwrap_err(),
        RoutineError::RevisionNotFound {
            key: key("triage"),
            revision: 9
        }
    );
}

#[test]
fn archive_keeps_runs_and_reads_but_leaves_discovery_and_arming() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    create(home.path(), "triage");
    let store = RoutineStateStore::open(home.path()).unwrap();
    store
        .set_enabled(&key("triage"), super::model::RunsOn::OneMac, true)
        .unwrap();
    let coordinator = crate::coordinator::default_store_path(home.path());
    rusqlite::Connection::open(&coordinator)
        .unwrap()
        .execute(
            "INSERT INTO routine_runs (run_id, routine_key, revision, source, phase, created_at)
             VALUES ('run-1', ?1, 1, 'manual', 'succeeded', 0)",
            [key("triage").to_string()],
        )
        .unwrap();

    let archived = service::archive(home.path(), &alice(), &key("triage"), &user())
        .unwrap()
        .def;
    assert_eq!(archived.revision, 2);
    assert!(archived.archived_at.is_some());
    assert!(!super::is_armed_here(
        &archived,
        &super::local_host_id(home.path()).unwrap()
    ));

    assert!(discover_routines(home.path(), &alice()).unwrap().is_empty());
    assert!(service::list_summaries(home.path(), &alice())
        .unwrap()
        .is_empty());
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert!(detail.def.is_archived());
    assert!(!detail.armed_here);
    assert_eq!(
        service::arm_here(home.path(), &alice(), &key("triage"), &user()).unwrap_err(),
        RoutineError::Archived(key("triage"))
    );
    let runs: i64 = rusqlite::Connection::open(&coordinator)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM routine_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 1, "archiving never deletes run history");
    // Archiving again is a no-op.
    assert_eq!(
        service::archive(home.path(), &alice(), &key("triage"), &user())
            .unwrap()
            .def
            .revision,
        2
    );
}

#[test]
fn a_body_this_build_rejects_can_still_be_archived() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    create(home.path(), "triage");
    // A newer build (or a hand edit) wrote a value this build does not accept.
    let dir = routine_dir(home.path(), &alice(), "triage");
    let mut def = defs::read_def(&dir).unwrap().unwrap();
    def.target.kind = "machine:studio".into();
    defs::write_revision(&dir, &RoutineDef { revision: 2, ..def }, &user(), "t").unwrap();

    let archived = service::archive(home.path(), &alice(), &key("triage"), &user())
        .unwrap()
        .def;
    assert_eq!(archived.revision, 3);
    assert!(archived.is_archived());
}

#[test]
fn pause_is_local_state_only() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    create(home.path(), "triage");
    let dir = routine_dir(home.path(), &alice(), "triage");
    let before = std::fs::read(dir.join("routine.yml")).unwrap();

    service::set_enabled(home.path(), &alice(), &key("triage"), false).unwrap();
    assert_eq!(std::fs::read(dir.join("routine.yml")).unwrap(), before);
    assert!(defs::read_revision(&dir, 2).unwrap().is_none());
    let listed = service::list_summaries(home.path(), &alice()).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].enabled);
    assert_eq!(listed[0].created_by.as_ref().unwrap().name, "alice");

    service::set_enabled(home.path(), &alice(), &key("triage"), true).unwrap();
    assert!(
        service::get(home.path(), &alice(), &key("triage"))
            .unwrap()
            .enabled
    );
}

/// A teammate changes a team routine like any shared content: every write goes
/// through, credited to them in history, and the creator stays its `owner`.
#[test]
fn seam_a_teammate_can_change_every_part_of_a_team_routine() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    write_org(home.path(), 2, ORG);
    let v1 = create(home.path(), "triage");
    // Simulate the git pull that brings Alice's routine to Bob.
    copy_dir(
        &routine_dir(home.path(), &alice(), "triage"),
        &defs::sync_target(
            &routine_dir(home.path(), &alice(), "triage"),
            &routine_dir(home.path(), &bob(), "triage"),
        ),
    );
    let by_bob = Author::user("bob");

    let mut edit = v1.clone();
    edit.name = "Bob's take".into();
    let saved = service::save(
        home.path(),
        &bob(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: 1,
        },
        edit,
        &by_bob,
    )
    .unwrap();
    assert_eq!(
        (
            saved.def.revision,
            saved.def.name.as_str(),
            saved.def.owner.as_str()
        ),
        (2, "Bob's take", "1")
    );
    // The local stale-edit guard still holds for him.
    assert_eq!(
        service::save(
            home.path(),
            &bob(),
            SaveTarget::Update {
                key: key("triage"),
                base_revision: 1,
            },
            v1.clone(),
            &by_bob,
        )
        .unwrap_err(),
        RoutineError::Conflict {
            current_revision: 2
        }
    );
    let restored = service::restore(home.path(), &bob(), &key("triage"), 1, 2, &by_bob).unwrap();
    assert_eq!(
        (restored.def.revision, restored.def.name),
        (3, v1.name.clone())
    );

    service::set_enabled(home.path(), &bob(), &key("triage"), false).unwrap();
    assert!(
        !service::get(home.path(), &bob(), &key("triage"))
            .unwrap()
            .enabled
    );
    service::set_enabled(home.path(), &bob(), &key("triage"), true).unwrap();

    let other_mac = tempfile::tempdir().unwrap();
    write_org(other_mac.path(), 2, ORG);
    copy_dir(
        &routine_dir(home.path(), &bob(), "triage"),
        &defs::sync_target(
            &routine_dir(home.path(), &bob(), "triage"),
            &routine_dir(other_mac.path(), &bob(), "triage"),
        ),
    );
    let moved = service::arm_here(other_mac.path(), &bob(), &key("triage"), &by_bob)
        .unwrap()
        .def;
    assert_eq!(
        moved.armed_host,
        super::local_host_id(other_mac.path()).unwrap()
    );
    assert_eq!(moved.owner, "1", "moving it changes where, not who");

    let archived = service::archive(home.path(), &bob(), &key("triage"), &by_bob).unwrap();
    assert!(archived.def.is_archived());

    // History credits each change to whoever made it.
    let authors: Vec<String> = defs::read_revisions(&routine_dir(home.path(), &bob(), "triage"))
        .unwrap()
        .into_iter()
        .map(|r| r.author.name)
        .collect();
    assert_eq!(authors, ["alice", "bob", "bob", "bob"]);
}

/// Copying stays as a convenience: a fresh routine the copier created.
#[test]
fn a_teammate_can_still_copy_a_routine_to_their_own() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    write_org(home.path(), 2, ORG);
    let v1 = create(home.path(), "triage");
    copy_dir(
        &routine_dir(home.path(), &alice(), "triage"),
        &defs::sync_target(
            &routine_dir(home.path(), &alice(), "triage"),
            &routine_dir(home.path(), &bob(), "triage"),
        ),
    );

    let copy = service::copy_to_mine(
        home.path(),
        &bob(),
        &key("triage"),
        Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        &Author::user("bob"),
    )
    .unwrap();
    assert_eq!(copy.key, key("triage-copy"));
    assert_eq!(copy.def.owner, "2");
    assert_eq!(copy.def.revision, 1);
    assert_eq!(copy.def.task, v1.task);
    let bobs = discover_routines(home.path(), &bob()).unwrap();
    let armed: Vec<_> = bobs
        .iter()
        .map(|r| (r.key.id.as_str(), r.armed_here))
        .collect();
    // One Mac: both are armed on it, whoever created them.
    assert_eq!(armed, vec![("triage", true), ("triage-copy", true)]);
    let rev1 = defs::read_revision(&routine_dir(home.path(), &bob(), "triage-copy"), 1)
        .unwrap()
        .unwrap();
    assert_eq!(rev1.author.name, "bob");

    // A second copy takes the next free id.
    let again = service::copy_to_mine(
        home.path(),
        &bob(),
        &key("triage"),
        team(),
        &Author::user("bob"),
    )
    .unwrap();
    assert_eq!(again.key.id, "triage-copy-2");
}

/// Two personal spaces, one synced definition: the Mac its `armed_host` names
/// arms it, and anyone may move it.
#[test]
fn seam_two_principals_one_synced_definition() {
    let alices_mac = tempfile::tempdir().unwrap();
    write_org(alices_mac.path(), 1, ORG);
    create(alices_mac.path(), "triage");
    let bobs_mac = tempfile::tempdir().unwrap();
    write_org(bobs_mac.path(), 2, ORG);
    let sync = |from: &Path, who_from: &Principal, to: &Path, who_to: &Principal| {
        let dest = defs::sync_target(
            &routine_dir(from, who_from, "triage"),
            &routine_dir(to, who_to, "triage"),
        );
        let _ = std::fs::remove_dir_all(&dest);
        copy_dir(&routine_dir(from, who_from, "triage"), &dest);
    };
    sync(alices_mac.path(), &alice(), bobs_mac.path(), &bob());

    let for_alice = discover_routines(alices_mac.path(), &alice()).unwrap();
    let for_bob = discover_routines(bobs_mac.path(), &bob()).unwrap();
    assert_eq!(for_alice.len(), 1);
    assert_eq!(for_bob.len(), 1);
    assert!(for_alice[0].armed_here);
    assert!(!for_bob[0].armed_here, "armed on Alice's Mac, not Bob's");
    assert_eq!(for_bob[0].owner_display, "Alice A");
    assert_eq!(for_bob[0].scope, RoutineScope::Team);
    assert_eq!(for_bob[0].key, key("triage"));

    // On the armed Mac it is armed from any personal space, creator or not.
    write_org(alices_mac.path(), 2, ORG);
    sync(alices_mac.path(), &alice(), alices_mac.path(), &bob());
    assert!(discover_routines(alices_mac.path(), &bob()).unwrap()[0].armed_here);

    // Bob moves it to his Mac: a revision credited to him, the creator kept.
    let moved = service::arm_here(
        bobs_mac.path(),
        &bob(),
        &key("triage"),
        &Author::user("bob"),
    )
    .unwrap()
    .def;
    assert_eq!(moved.revision, 2);
    assert_eq!(moved.owner, "1");
    assert_eq!(
        moved.armed_host,
        super::local_host_id(bobs_mac.path()).unwrap()
    );
    assert!(discover_routines(bobs_mac.path(), &bob()).unwrap()[0].armed_here);
    // Once his edit reaches Alice's Mac, it no longer fires there.
    sync(bobs_mac.path(), &bob(), alices_mac.path(), &alice());
    assert!(!discover_routines(alices_mac.path(), &alice()).unwrap()[0].armed_here);
}

/// A definition written by hand may name no Mac. It runs on none, so no two
/// Macs can both fire it, until a save or "Run on this Mac" places it.
#[test]
fn a_routine_with_no_armed_host_runs_nowhere_until_it_is_placed() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    write_org(home.path(), 2, ORG);
    let mut def = create(home.path(), "triage");
    def.armed_host = String::new();
    def.revision = 2;
    for who in [alice(), bob()] {
        defs::write_revision(
            &routine_dir(home.path(), &who, "triage"),
            &def,
            &Author::user("hand edit"),
            "2026-10-01T00:00:00Z",
        )
        .unwrap();
    }
    // Not even its creator's Mac arms it, and the tab shows no next run.
    for who in [alice(), bob()] {
        assert!(!discover_routines(home.path(), &who).unwrap()[0].armed_here);
        let summary = &service::list_summaries(home.path(), &who).unwrap()[0];
        assert!(!summary.armed_here);
        assert_eq!(summary.next_run_at, None);
    }

    // A save from a Mac places it there: every routine CodeSense writes names
    // exactly one Mac.
    let here = super::local_host_id(home.path()).unwrap();
    let mut edit = def.clone();
    edit.name = "Renamed".into();
    let saved = service::save(
        home.path(),
        &bob(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: 2,
        },
        edit,
        &Author::user("bob"),
    )
    .unwrap();
    assert_eq!(saved.def.armed_host, here);
    assert!(discover_routines(home.path(), &bob()).unwrap()[0].armed_here);
    assert!(service::list_summaries(home.path(), &bob()).unwrap()[0]
        .next_run_at
        .is_some());

    // A restore does too, and so does "Run on this Mac".
    for (who, place) in [("restore", 0), ("arm", 1)] {
        let mut unplaced = service::get(home.path(), &alice(), &key("triage"))
            .unwrap()
            .def;
        unplaced.armed_host = String::new();
        unplaced.revision += 1;
        defs::write_revision(
            &routine_dir(home.path(), &alice(), "triage"),
            &unplaced,
            &Author::user("hand edit"),
            "2026-10-01T00:00:00Z",
        )
        .unwrap();
        let placed = if place == 0 {
            service::restore(
                home.path(),
                &alice(),
                &key("triage"),
                1,
                unplaced.revision,
                &Author::user("alice"),
            )
        } else {
            service::arm_here(
                home.path(),
                &alice(),
                &key("triage"),
                &Author::user("alice"),
            )
        }
        .unwrap()
        .def;
        assert_eq!(placed.armed_host, here, "{who}");
    }

    // A routine already placed elsewhere is never moved by an edit.
    let mut elsewhere = service::get(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def;
    elsewhere.armed_host = "another-mac".into();
    elsewhere.revision += 1;
    defs::write_revision(
        &routine_dir(home.path(), &alice(), "triage"),
        &elsewhere,
        &Author::user("bob"),
        "2026-10-01T00:00:00Z",
    )
    .unwrap();
    let mut edit = elsewhere.clone();
    edit.name = "Again".into();
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: elsewhere.revision,
        },
        edit,
        &Author::user("alice"),
    )
    .unwrap();
    assert_eq!(saved.def.armed_host, "another-mac");
}

#[test]
fn discovery_spans_orgs_and_the_personal_team_without_an_active_team() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let other_org = "other/org";
    write_org(home.path(), 1, other_org);
    create(home.path(), "triage");
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: other_org.into(),
            team: TEAM.into(),
        }),
        routine("nightly"),
        &user(),
    )
    .unwrap();
    let mine = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(personal(1)),
        routine("journal"),
        &user(),
    )
    .unwrap();
    assert!(mine.key.is_personal());
    assert_eq!(mine.key.org, "acme/api");

    // A malformed definition is skipped, not fatal.
    let broken = routine_dir(home.path(), &alice(), "broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("routine.yml"), "id: [not a routine").unwrap();

    let found = discover_routines(home.path(), &alice()).unwrap();
    let mut tags: Vec<_> = found
        .iter()
        .map(|r| (r.key.to_string(), r.scope, r.armed_here))
        .collect();
    tags.sort_by(|a, b| a.0.cmp(&b.0));
    let personal_key = "_personal/1/acme/api/_scratch/journal".to_string();
    let mut expected = vec![
        (personal_key, RoutineScope::Personal, true),
        (
            "acme/api/backend/triage".to_string(),
            RoutineScope::Team,
            true,
        ),
        (
            "other/org/backend/nightly".to_string(),
            RoutineScope::Team,
            true,
        ),
    ];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        tags, expected,
        "each routine exactly once, deduped by content dir"
    );
}

#[test]
fn the_active_routine_cap_counts_only_enabled_armed_routines() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    for i in 0..MAX_ACTIVE_ROUTINES {
        create(home.path(), &format!("r{i}"));
    }
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            routine("one-more"),
            &user()
        )
        .unwrap_err(),
        RoutineError::LimitReached {
            limit: MAX_ACTIVE_ROUTINES
        }
    );
    assert!(!routine_dir(home.path(), &alice(), "one-more").exists());

    // Editing an existing routine at the cap is fine.
    let r0 = service::get(home.path(), &alice(), &key("r0")).unwrap().def;
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("r0"),
            base_revision: 1,
        },
        RoutineDef {
            name: "edited".into(),
            ..r0
        },
        &user(),
    )
    .unwrap();

    // A paused routine frees a slot; resuming it past the cap is refused.
    service::set_enabled(home.path(), &alice(), &key("r0"), false).unwrap();
    create(home.path(), "one-more");
    assert_eq!(
        service::set_enabled(home.path(), &alice(), &key("r0"), true).unwrap_err(),
        RoutineError::LimitReached {
            limit: MAX_ACTIVE_ROUTINES
        }
    );
}

// ---------------------------------------------------------------------------
// Scheduler pass: real saved routines, the real coordinator file, an injected
// clock.
// ---------------------------------------------------------------------------

mod tick_seams {
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use chrono::DateTime;
    use rusqlite::Transaction;
    use tauri::Manager;

    use super::*;
    use crate::coordinator::StoreResult;
    use crate::routines::admit::Admission;
    use crate::routines::discover::DiscoveredRoutine;
    use crate::routines::model::MissedPolicy;
    use crate::routines::schedule;
    use crate::routines::tick::{
        run_routine_tick, run_routine_tick_with, Account, Admit, Clock, Occurrence,
        RoutineAdmitter, RoutineTicker, RunSource, DISCOVERY_TTL_MS,
    };

    /// Mondays 2026-10-05 08:00 MDT, then the 09:00 MDT fire of `0 9 * * 1-5`.
    const MON_0800: &str = "2026-10-05T14:00:00Z";
    const MON_0900: &str = "2026-10-05T15:00:00Z";

    fn ms(rfc3339: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .timestamp_millis()
    }

    fn min(n: i64) -> i64 {
        n * 60_000
    }

    /// Admission that writes the queued row and counts its calls.
    #[derive(Default)]
    struct RecordingAdmitter {
        calls: AtomicUsize,
        seen: Mutex<Vec<Occurrence>>,
    }

    impl Admit for RecordingAdmitter {
        fn admit(
            &self,
            tx: &Transaction<'_>,
            account: &Account,
            routine: &DiscoveredRoutine,
            occurrence: &Occurrence,
        ) -> StoreResult<Admission> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(occurrence.clone());
            RoutineAdmitter.admit(tx, account, routine, occurrence)
        }
    }

    fn acct(home: &Path) -> Account {
        (home.to_path_buf(), alice())
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Row {
        occurrence_key: String,
        source: String,
        phase: String,
        reason: Option<String>,
    }

    fn runs(home: &Path) -> Vec<Row> {
        let conn =
            rusqlite::Connection::open(crate::coordinator::default_store_path(home)).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT occurrence_key, source, phase, reason FROM routine_runs
                 ORDER BY occurrence_key",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(Row {
                    occurrence_key: r.get(0)?,
                    source: r.get(1)?,
                    phase: r.get(2)?,
                    reason: r.get(3)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    fn row(id: &str, at: &str, source: &str, phase: &str, reason: Option<&str>) -> Row {
        let occurrence_key = format!("{ORG}/{TEAM}/{id}@{at}");
        let missed = reason.is_some_and(|r| r.starts_with("missed:"));
        Row {
            occurrence_key: if missed {
                crate::routines::tick::missed_key(&occurrence_key)
            } else {
                occurrence_key
            },
            source: source.into(),
            phase: phase.into(),
            reason: reason.map(Into::into),
        }
    }

    fn cursor(store: &RoutineStateStore, id: &str) -> Option<Option<i64>> {
        let trigger_id = format!("{}#0", key(id));
        store
            .triggers()
            .unwrap()
            .into_iter()
            .find(|t| t.trigger_id == trigger_id)
            .map(|t| t.next_run_at)
    }

    fn discovered(home: &Path) -> Vec<DiscoveredRoutine> {
        discover_routines(home, &alice()).unwrap()
    }

    fn create_with(home: &Path, id: &str, missed: MissedPolicy) {
        let mut def = routine(id);
        def.policy.missed = missed;
        service::save(home, &alice(), SaveTarget::Create(team()), def, &user()).unwrap();
    }

    /// A temp home with Alice's org synced and one weekday-09:00 Denver routine
    /// whose cursor is seeded at Monday 08:00.
    fn seeded(missed: MissedPolicy) -> (tempfile::TempDir, RoutineStateStore) {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        create_with(home.path(), "triage", missed);
        let store = RoutineStateStore::open(home.path()).unwrap();
        let report = run_routine_tick_with(
            ms(MON_0800),
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(report.seeded, 1);
        assert_eq!(cursor(&store, "triage"), Some(Some(ms(MON_0900))));
        (home, store)
    }

    fn fixed_ticker(home: &Path, clock: &Arc<AtomicI64>) -> RoutineTicker {
        let c = Arc::clone(clock);
        let clock: Clock = Arc::new(move || c.load(Ordering::SeqCst));
        let home = home.to_path_buf();
        RoutineTicker::new(
            clock,
            Arc::new(RoutineAdmitter),
            Arc::new(move || Some((home.clone(), alice()))),
            Arc::new(crate::routines::test_support::FakeSessions::default()),
        )
    }

    #[test]
    fn the_registered_wake_admits_one_queued_run_per_occurrence() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        create(home.path(), "triage");
        let clock = Arc::new(AtomicI64::new(ms(MON_0800)));
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(fixed_ticker(home.path(), &clock));

        run_routine_tick(app.handle());
        assert!(runs(home.path()).is_empty(), "the first wake only seeds");

        clock.store(ms(MON_0900) + min(2), Ordering::SeqCst);
        run_routine_tick(app.handle());
        run_routine_tick(app.handle());
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "queued",
                None
            )]
        );
        let store = RoutineStateStore::open(home.path()).unwrap();
        assert_eq!(
            cursor(&store, "triage"),
            Some(Some(ms("2026-10-06T15:00:00Z"))),
            "advanced to Tuesday 09:00 MDT"
        );
    }

    #[test]
    fn racing_wakes_admit_each_occurrence_once() {
        let (home, store) = seeded(MissedPolicy::Skip);
        let routines = discovered(home.path());
        let admitter = RecordingAdmitter::default();
        let now = ms(MON_0900) + min(1);
        let barrier = Barrier::new(2);
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    barrier.wait();
                    run_routine_tick_with(
                        now,
                        ms(MON_0800),
                        &acct(home.path()),
                        &routines,
                        &store,
                        &admitter,
                    )
                    .unwrap();
                });
            }
        });
        assert_eq!(admitter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "queued",
                None
            )]
        );
        let seen = admitter.seen.lock().unwrap();
        assert_eq!(seen[0].source, RunSource::Schedule);
        assert_eq!(seen[0].revision, 1);
        assert_eq!(seen[0].scheduled_at, ms(MON_0900));
    }

    #[test]
    fn a_late_wake_while_running_is_not_missed() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // App Nap held the tick for ten minutes; still one occurrence, due
        // after this process started.
        let report = run_routine_tick_with(
            ms(MON_0900) + min(10),
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!((report.admitted, report.skipped), (1, 0));
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "queued",
                None
            )]
        );
        assert_eq!(store.state(&key("triage")).unwrap(), None, "nothing missed");
    }

    #[test]
    fn one_occurrence_while_the_app_was_closed_is_missed() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // This process started after the 09:00 fire.
        let now = ms(MON_0900) + min(30);
        run_routine_tick_with(
            now,
            now,
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "skipped",
                Some("missed:1")
            )]
        );
        assert_eq!(
            store.state(&key("triage")).unwrap().unwrap().missed_count,
            1
        );
    }

    /// Wednesday 09:30 MDT: Monday, Tuesday and Wednesday 09:00 have elapsed
    /// since the cursor, all before this process started.
    const WED_0930: &str = "2026-10-07T15:30:00Z";

    #[test]
    fn downtime_with_skip_records_one_skipped_row() {
        let (home, store) = seeded(MissedPolicy::Skip);
        let now = ms(WED_0930);
        let report = run_routine_tick_with(
            now,
            now,
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!((report.admitted, report.skipped), (0, 3));
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-07T15:00:00Z",
                "schedule",
                "skipped",
                Some("missed:3")
            )]
        );
        let next = cursor(&store, "triage").unwrap().unwrap();
        assert!(next > now);
        assert_eq!(next, ms("2026-10-08T15:00:00Z"));
        assert_eq!(
            store.state(&key("triage")).unwrap().unwrap().missed_count,
            3
        );
    }

    #[test]
    fn downtime_with_run_once_admits_one_catch_up() {
        let (home, store) = seeded(MissedPolicy::RunOnce);
        let now = ms(WED_0930);
        let admitter = RecordingAdmitter::default();
        let report = run_routine_tick_with(
            now,
            now,
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &admitter,
        )
        .unwrap();
        assert_eq!((report.admitted, report.skipped), (1, 2));
        assert_eq!(admitter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            runs(home.path()),
            vec![
                row(
                    "triage",
                    "2026-10-06T15:00:00Z",
                    "schedule",
                    "skipped",
                    Some("missed:2")
                ),
                row("triage", "2026-10-07T15:00:00Z", "catch_up", "queued", None),
            ]
        );
        assert_eq!(
            cursor(&store, "triage"),
            Some(Some(ms("2026-10-08T15:00:00Z")))
        );
        assert_eq!(
            store.state(&key("triage")).unwrap().unwrap().missed_count,
            2
        );
        // The next wake does not replay anything.
        run_routine_tick_with(
            now + min(1),
            now,
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &admitter,
        )
        .unwrap();
        assert_eq!(runs(home.path()).len(), 2);
    }

    #[test]
    fn a_paused_routine_is_not_scanned_and_resumes_from_now() {
        let (home, store) = seeded(MissedPolicy::Skip);
        service::set_enabled(home.path(), &alice(), &key("triage"), false).unwrap();
        let routines = discovered(home.path());
        let later = ms(MON_0900) + min(5);
        run_routine_tick_with(
            later,
            ms(MON_0800),
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert!(runs(home.path()).is_empty());
        assert_eq!(
            cursor(&store, "triage"),
            None,
            "no cursor kept while paused"
        );

        // A week later the owner resumes; the gap is neither fired nor missed.
        service::set_enabled(home.path(), &alice(), &key("triage"), true).unwrap();
        let week_later = ms("2026-10-12T16:30:00Z");
        let report = run_routine_tick_with(
            week_later,
            ms(MON_0800),
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(
            report,
            crate::routines::tick::TickReport {
                seeded: 1,
                ..Default::default()
            }
        );
        assert!(runs(home.path()).is_empty());
        assert_eq!(
            cursor(&store, "triage"),
            Some(Some(ms("2026-10-13T15:00:00Z")))
        );
        assert_eq!(
            store.state(&key("triage")).unwrap().unwrap().missed_count,
            0
        );
    }

    #[test]
    fn archived_and_teammate_routines_produce_nothing() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // Armed on another Mac: discovered, but not armed here.
        let mut routines = discovered(home.path());
        routines[0].armed_here = false;
        let now = ms(MON_0900) + min(1);
        run_routine_tick_with(
            now,
            ms(MON_0800),
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert!(runs(home.path()).is_empty());
        assert_eq!(cursor(&store, "triage"), None);

        service::archive(home.path(), &alice(), &key("triage"), &user()).unwrap();
        assert!(discovered(home.path()).is_empty());
        let report = run_routine_tick_with(
            now,
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(report, Default::default());
        assert!(runs(home.path()).is_empty());
    }

    #[test]
    fn an_edited_schedule_reseeds_from_now_without_back_firing() {
        let (home, store) = seeded(MissedPolicy::RunOnce);
        let mut def = routine("triage");
        def.policy.missed = MissedPolicy::RunOnce;
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 10 * * 1-5".into(),
            tz: "America/Denver".into(),
        })];
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Update {
                key: key("triage"),
                base_revision: 1,
            },
            def,
            &user(),
        )
        .unwrap();
        // The old cursor (Monday 09:00) is two days overdue, but the edit wins.
        let now = ms(WED_0930);
        let report = run_routine_tick_with(
            now,
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(report.seeded, 1);
        assert!(runs(home.path()).is_empty());
        assert_eq!(
            cursor(&store, "triage"),
            Some(Some(ms("2026-10-07T16:00:00Z")))
        );
    }

    #[test]
    fn local_saves_drop_the_discovery_cache_and_synced_edits_land_within_the_ttl() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let clock = Arc::new(AtomicI64::new(ms(MON_0800)));
        let ticker = fixed_ticker(home.path(), &clock);
        assert_eq!(ticker.tick(), Some(Default::default()));

        // A local save is seen by the very next wake.
        create(home.path(), "triage");
        assert_eq!(ticker.tick().unwrap().seeded, 1);

        // A definition that arrives by sync (written straight to disk, no
        // service call) waits for the cache to expire.
        let mut synced = defs::read_def(&routine_dir(home.path(), &alice(), "triage"))
            .unwrap()
            .unwrap();
        synced.id = "synced".into();
        defs::write_revision(
            &routine_dir(home.path(), &alice(), "synced"),
            &synced,
            &user(),
            "2026-10-05T14:00:00Z",
        )
        .unwrap();
        clock.store(ms(MON_0800) + DISCOVERY_TTL_MS / 2, Ordering::SeqCst);
        assert_eq!(ticker.tick().unwrap().seeded, 0);
        clock.store(ms(MON_0800) + DISCOVERY_TTL_MS + 1, Ordering::SeqCst);
        assert_eq!(ticker.tick().unwrap().seeded, 1);
        let store = RoutineStateStore::open(home.path()).unwrap();
        assert!(cursor(&store, "synced").is_some());
    }

    #[test]
    fn a_missing_zone_is_pinned_to_the_system_zone_at_save() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let mut def = routine("triage");
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * 1-5".into(),
            tz: String::new(),
        })];
        let saved = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            def,
            &user(),
        )
        .unwrap();
        let stored = defs::read_def(&routine_dir(home.path(), &alice(), "triage"))
            .unwrap()
            .unwrap();
        assert_eq!(stored, saved.def);
        let RoutineTrigger::Schedule(spec) = &stored.triggers[0] else {
            panic!("schedule trigger expected");
        };
        assert_eq!(spec.tz, schedule::system_zone());
        assert!(!spec.tz.is_empty());
    }

    #[test]
    fn saving_yaml_canonicalizes_the_role_queue_filter() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let mut document = serde_yaml::to_value(routine("role-mail")).unwrap();
        document["triggers"] =
            serde_yaml::from_str("- event: {kind: role_pending, role: ' role: reviewer '}")
                .unwrap();
        let def: RoutineDef = serde_yaml::from_value(document).unwrap();
        let saved = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            def,
            &user(),
        )
        .unwrap();
        let dir = routine_dir(home.path(), &alice(), "role-mail");
        let stored = defs::read_def(&dir).unwrap().unwrap();
        assert_eq!(stored, saved.def);
        let RoutineTrigger::Event(spec) = &stored.triggers[0] else {
            panic!("event trigger expected");
        };
        assert_eq!(spec.role.as_deref(), Some("reviewer"));
        let yaml = std::fs::read_to_string(dir.join("routine.yml")).unwrap();
        assert!(yaml.contains("role: reviewer"), "{yaml}");
        assert!(!yaml.contains("role: reviewer "), "{yaml}");
    }

    #[test]
    fn a_too_frequent_schedule_is_refused_at_save() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let mut def = routine("triage");
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "*/2 * * * *".into(),
            tz: "UTC".into(),
        })];
        let err = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            def,
            &user(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            RoutineError::Validation(ValidationError::InvalidSchedule(
                schedule::ScheduleError::IntervalTooShort {
                    min: schedule::MIN_INTERVAL
                }
            ))
        );
    }

    #[test]
    fn denver_dst_through_the_tick() {
        // A 01:30 daily routine across the 2026-11-01 fall-back fires once.
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let mut def = routine("night");
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "30 1 * * *".into(),
            tz: "America/Denver".into(),
        })];
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            def,
            &user(),
        )
        .unwrap();
        let store = RoutineStateStore::open(home.path()).unwrap();
        let routines = discovered(home.path());
        let start = ms("2026-11-01T06:00:00Z"); // 00:00 MDT
        run_routine_tick_with(
            start,
            start,
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        // Wake every 15s through the repeated hour and past it.
        let mut now = start;
        while now < ms("2026-11-01T10:00:00Z") {
            now += 15_000;
            run_routine_tick_with(
                now,
                start,
                &acct(home.path()),
                &routines,
                &store,
                &RoutineAdmitter,
            )
            .unwrap();
        }
        let rows = runs(home.path());
        assert_eq!(
            rows,
            vec![row(
                "night",
                "2026-11-01T07:30:00Z",
                "schedule",
                "queued",
                None
            )]
        );
        assert_eq!(
            cursor(&store, "night"),
            Some(Some(ms("2026-11-02T08:30:00Z")))
        );
    }

    #[test]
    fn a_hand_edited_schedule_the_save_would_refuse_never_fires() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // A synced edit writes an every-minute cron straight to disk.
        let dir = routine_dir(home.path(), &alice(), "triage");
        let mut def = defs::read_def(&dir).unwrap().unwrap();
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "* * * * *".into(),
            tz: "America/Denver".into(),
        })];
        def.revision += 1;
        defs::write_revision(&dir, &def, &user(), "2026-10-05T14:00:00Z").unwrap();
        let routines = discovered(home.path());
        let mut now = ms(MON_0800);
        for _ in 0..20 {
            now += min(1);
            run_routine_tick_with(
                now,
                ms(MON_0800),
                &acct(home.path()),
                &routines,
                &store,
                &RoutineAdmitter,
            )
            .unwrap();
        }
        assert!(runs(home.path()).is_empty());
        assert_eq!(
            cursor(&store, "triage"),
            Some(None),
            "parked without a cursor"
        );
    }

    #[test]
    fn a_routine_briefly_missing_from_discovery_keeps_its_cursor() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // Mid-sync the definition is unreadable, so discovery returns nothing.
        run_routine_tick_with(
            ms(MON_0800) + min(1),
            ms(MON_0800),
            &acct(home.path()),
            &[],
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(cursor(&store, "triage"), Some(Some(ms(MON_0900))));
        // It reappears after the fire: the occurrence is still admitted.
        run_routine_tick_with(
            ms(MON_0900) + min(1),
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "queued",
                None
            )]
        );
    }

    #[test]
    fn two_triggers_on_one_missed_instant_count_once() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let mut def = routine("triage");
        def.triggers.push(RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * *".into(),
            tz: "America/Denver".into(),
        }));
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(team()),
            def,
            &user(),
        )
        .unwrap();
        let store = RoutineStateStore::open(home.path()).unwrap();
        let routines = discovered(home.path());
        run_routine_tick_with(
            ms(MON_0800),
            ms(MON_0800),
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        // The app was closed over Monday 09:00.
        let now = ms(MON_0900) + min(30);
        run_routine_tick_with(
            now,
            now,
            &acct(home.path()),
            &routines,
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert_eq!(
            runs(home.path()),
            vec![row(
                "triage",
                "2026-10-05T15:00:00Z",
                "schedule",
                "skipped",
                Some("missed:1")
            )]
        );
        assert_eq!(
            store.state(&key("triage")).unwrap().unwrap().missed_count,
            1
        );
    }

    #[test]
    fn a_fire_between_launch_and_the_first_wake_is_on_time() {
        let (home, _store) = seeded(MissedPolicy::Skip);
        // Launched ten seconds before 09:00; the first wake lands after it.
        let clock = Arc::new(AtomicI64::new(ms(MON_0900) - 10_000));
        let ticker = fixed_ticker(home.path(), &clock);
        clock.store(ms(MON_0900) + 5_000, Ordering::SeqCst);
        assert_eq!(ticker.tick().unwrap().admitted, 1);
        assert_eq!(runs(home.path())[0].phase, "queued");
    }

    /// A `*/5` Denver routine saved for Alice with the given missed policy.
    fn create_every_five(home: &Path, missed: MissedPolicy) {
        let mut def = routine("poll");
        def.policy.missed = missed;
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "*/5 * * * *".into(),
            tz: "America/Denver".into(),
        })];
        service::save(home, &alice(), SaveTarget::Create(team()), def, &user()).unwrap();
    }

    fn missed_count(home: &Path, id: &str) -> u32 {
        RoutineStateStore::open(home)
            .unwrap()
            .state(&key(id))
            .unwrap()
            .map_or(0, |s| s.missed_count)
    }

    #[test]
    fn a_twelve_minute_nap_runs_the_latest_occurrence_whatever_the_policy() {
        for missed in [MissedPolicy::Skip, MissedPolicy::RunOnce] {
            let home = tempfile::tempdir().unwrap();
            write_org(home.path(), 1, ORG);
            create_every_five(home.path(), missed);
            // Launched at 08:00:30; the first wakes seed 08:05 and find nothing due.
            let clock = Arc::new(AtomicI64::new(ms(MON_0800) + 30_000));
            let ticker = fixed_ticker(home.path(), &clock);
            assert_eq!(ticker.tick().unwrap().seeded, 1);
            clock.store(ms(MON_0800) + min(3) + 30_000, Ordering::SeqCst);
            assert_eq!(ticker.tick(), Some(Default::default()));

            // App Nap holds the next wake twelve minutes: 08:05, 08:10 and
            // 08:15 all came due while the app was running.
            clock.store(ms(MON_0800) + min(15) + 30_000, Ordering::SeqCst);
            let report = ticker.tick().unwrap();
            assert_eq!((report.admitted, report.skipped), (1, 2), "{missed:?}");
            assert_eq!(
                runs(home.path()),
                vec![
                    row(
                        "poll",
                        "2026-10-05T14:10:00Z",
                        "schedule",
                        "skipped",
                        Some("missed:2")
                    ),
                    row("poll", "2026-10-05T14:15:00Z", "schedule", "queued", None),
                ],
                "{missed:?}"
            );
            let store = RoutineStateStore::open(home.path()).unwrap();
            assert_eq!(
                cursor(&store, "poll"),
                Some(Some(ms("2026-10-05T14:20:00Z")))
            );
            assert_eq!(missed_count(home.path(), "poll"), 2);

            // The next wake is back on cadence and replays nothing.
            clock.store(ms(MON_0800) + min(15) + 45_000, Ordering::SeqCst);
            assert_eq!(ticker.tick(), Some(Default::default()));
            assert_eq!(runs(home.path()).len(), 2);
        }
    }

    #[test]
    fn occurrences_while_the_app_was_closed_follow_the_missed_policy() {
        let cases = [
            (
                MissedPolicy::Skip,
                vec![row(
                    "poll",
                    "2026-10-05T14:15:00Z",
                    "schedule",
                    "skipped",
                    Some("missed:3"),
                )],
                3,
            ),
            (
                MissedPolicy::RunOnce,
                vec![
                    row(
                        "poll",
                        "2026-10-05T14:10:00Z",
                        "schedule",
                        "skipped",
                        Some("missed:2"),
                    ),
                    row("poll", "2026-10-05T14:15:00Z", "catch_up", "queued", None),
                ],
                2,
            ),
        ];
        for (missed, expected, missed_total) in cases {
            let home = tempfile::tempdir().unwrap();
            write_org(home.path(), 1, ORG);
            create_every_five(home.path(), missed);
            // One app session seeds 08:05, then the app quits.
            let clock = Arc::new(AtomicI64::new(ms(MON_0800) + 30_000));
            assert_eq!(fixed_ticker(home.path(), &clock).tick().unwrap().seeded, 1);
            // Relaunched at 08:17: 08:05, 08:10 and 08:15 fell during the downtime.
            clock.store(ms(MON_0800) + min(17), Ordering::SeqCst);
            let ticker = fixed_ticker(home.path(), &clock);
            clock.store(ms(MON_0800) + min(17) + 5_000, Ordering::SeqCst);
            ticker.tick().unwrap();
            assert_eq!(runs(home.path()), expected, "{missed:?}");
            assert_eq!(missed_count(home.path(), "poll"), missed_total);
            let store = RoutineStateStore::open(home.path()).unwrap();
            assert_eq!(
                cursor(&store, "poll"),
                Some(Some(ms("2026-10-05T14:20:00Z")))
            );
        }
    }

    #[test]
    fn a_relaunch_just_before_a_fire_runs_it_and_skips_the_downtime() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        create_every_five(home.path(), MissedPolicy::Skip);
        let clock = Arc::new(AtomicI64::new(ms(MON_0800) + 30_000));
        fixed_ticker(home.path(), &clock).tick().unwrap();
        // Relaunched at 08:14:50; 08:05 and 08:10 were missed while closed, and
        // 08:15 comes due before the first wake.
        clock.store(ms(MON_0800) + min(15) - 10_000, Ordering::SeqCst);
        let ticker = fixed_ticker(home.path(), &clock);
        clock.store(ms(MON_0800) + min(15) + 5_000, Ordering::SeqCst);
        let report = ticker.tick().unwrap();
        assert_eq!((report.admitted, report.skipped), (1, 2));
        assert_eq!(
            runs(home.path()),
            vec![
                row(
                    "poll",
                    "2026-10-05T14:10:00Z",
                    "schedule",
                    "skipped",
                    Some("missed:2")
                ),
                row("poll", "2026-10-05T14:15:00Z", "schedule", "queued", None),
            ]
        );
    }

    #[test]
    fn a_pause_takes_effect_on_the_next_wake_despite_the_discovery_cache() {
        let (home, _store) = seeded(MissedPolicy::Skip);
        let clock = Arc::new(AtomicI64::new(ms(MON_0900) - min(2)));
        let ticker = fixed_ticker(home.path(), &clock);
        assert_eq!(ticker.tick(), Some(Default::default()));
        // Paused within the discovery TTL; the cached scan still lists it.
        service::set_enabled(home.path(), &alice(), &key("triage"), false).unwrap();
        clock.store(ms(MON_0900) + 5_000, Ordering::SeqCst);
        assert_eq!(ticker.tick(), Some(Default::default()));
        assert!(runs(home.path()).is_empty());
        // Resumed: reseeded from now, the 09:00 gap is neither fired nor missed.
        service::set_enabled(home.path(), &alice(), &key("triage"), true).unwrap();
        clock.store(ms(MON_0900) + 20_000, Ordering::SeqCst);
        assert_eq!(ticker.tick().unwrap().seeded, 1);
        assert!(runs(home.path()).is_empty());
        assert_eq!(missed_count(home.path(), "triage"), 0);
    }

    #[test]
    fn archiving_drops_the_routine_on_the_next_wake_despite_the_discovery_cache() {
        let (home, _store) = seeded(MissedPolicy::Skip);
        let clock = Arc::new(AtomicI64::new(ms(MON_0900) - min(2)));
        let ticker = fixed_ticker(home.path(), &clock);
        assert_eq!(ticker.tick(), Some(Default::default()));
        service::archive(home.path(), &alice(), &key("triage"), &user()).unwrap();
        clock.store(ms(MON_0900) + 5_000, Ordering::SeqCst);
        assert_eq!(ticker.tick(), Some(Default::default()));
        assert!(runs(home.path()).is_empty());
    }

    #[test]
    fn the_account_is_resolved_at_most_once_per_ttl() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        let clock = Arc::new(AtomicI64::new(ms(MON_0800)));
        let c = Arc::clone(&clock);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let dir = home.path().to_path_buf();
        let ticker = RoutineTicker::new(
            Arc::new(move || c.load(Ordering::SeqCst)),
            Arc::new(RoutineAdmitter),
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Some((dir.clone(), alice()))
            }),
            Arc::new(crate::routines::test_support::FakeSessions::default()),
        );
        for wake in 0..4 {
            clock.store(ms(MON_0800) + wake * 15_000, Ordering::SeqCst);
            ticker.tick().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        clock.store(ms(MON_0800) + DISCOVERY_TTL_MS, Ordering::SeqCst);
        ticker.tick().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_due_cursor_whose_zone_no_longer_parses_is_parked() {
        let (home, store) = seeded(MissedPolicy::Skip);
        // The stored zone stopped parsing after the cursor was seeded.
        let dir = routine_dir(home.path(), &alice(), "triage");
        let mut def = defs::read_def(&dir).unwrap().unwrap();
        def.triggers = vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "0 9 * * 1-5".into(),
            tz: "Mars/Olympus".into(),
        })];
        defs::write_revision(&dir, &def, &user(), "2026-10-05T14:00:00Z").unwrap();
        store
            .seed_trigger(
                &format!("{}#0", key("triage")),
                &key("triage"),
                Some(ms(MON_0900)),
                &crate::routines::tick::spec_hash("0 9 * * 1-5", "Mars/Olympus"),
                &alice(),
            )
            .unwrap();
        run_routine_tick_with(
            ms(MON_0900) + min(1),
            ms(MON_0800),
            &acct(home.path()),
            &discovered(home.path()),
            &store,
            &RoutineAdmitter,
        )
        .unwrap();
        assert!(runs(home.path()).is_empty());
        assert_eq!(cursor(&store, "triage"), Some(None));
    }

    #[test]
    fn restoring_a_revision_without_a_zone_pins_one() {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1, ORG);
        create(home.path(), "triage");
        // Revision 1 as an older build wrote it, with no zone.
        let dir = routine_dir(home.path(), &alice(), "triage");
        let mut rec = defs::read_revision(&dir, 1).unwrap().unwrap();
        if let RoutineTrigger::Schedule(spec) = &mut rec.routine.triggers[0] {
            spec.tz = String::new();
        }
        let path = dir.join("revisions/1.yml");
        assert!(path.exists(), "{}", path.display());
        std::fs::write(&path, serde_yaml::to_string(&rec).unwrap()).unwrap();
        let restored =
            service::restore(home.path(), &alice(), &key("triage"), 1, 1, &user()).unwrap();
        let RoutineTrigger::Schedule(spec) = &restored.def.triggers[0] else {
            panic!("schedule trigger expected");
        };
        assert_eq!(spec.tz, schedule::system_zone());
    }
}

fn update(home: &Path, who: &Principal, def: RoutineDef) -> Result<RoutineDef, RoutineError> {
    let base_revision = service::get_current(home, who, &key(&def.id))
        .map(|d| d.def.revision)
        .unwrap_or(1);
    service::save(
        home,
        who,
        SaveTarget::Update {
            key: key(&def.id),
            base_revision,
        },
        def,
        &user(),
    )
    .map(|saved| saved.def)
}

/// A recipe each Mac runs names no Mac when created, is switched on per Mac
/// without an edit, and gets its Mac from a change of mode; a personal routine
/// cannot be one.
#[test]
fn a_recipe_is_switched_per_mac_and_placed_only_by_a_change_of_mode() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1, ORG);
    let here = super::local_host_id(home.path()).unwrap();
    let mut def = routine("digest");
    def.runs_on = "each_mac".into();
    let created = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(team()),
        def,
        &user(),
    )
    .unwrap()
    .def;
    assert_eq!((created.armed_host.as_str(), created.revision), ("", 1));
    let detail = || service::get(home.path(), &alice(), &key("digest")).unwrap();
    assert!(detail().armed_here, "a recipe can run on every Mac");
    assert!(!detail().enabled, "off until this Mac turns it on");
    let file = routine_dir(home.path(), &alice(), "digest").join("routine.yml");
    let written = std::fs::read(&file).unwrap();

    // "Run on this Mac" on a recipe turns it on here; nothing is written.
    let armed = service::arm_here(home.path(), &alice(), &key("digest"), &user()).unwrap();
    assert_eq!((armed.def.revision, armed.def.armed_host.as_str()), (1, ""));
    assert!(detail().enabled);
    service::set_enabled(home.path(), &alice(), &key("digest"), false).unwrap();
    assert!(!detail().enabled, "turned off here");
    assert_eq!(std::fs::read(&file).unwrap(), written, "never a git change");
    assert_eq!(detail().revisions.len(), 1);

    // An edit that keeps it a recipe does not place it.
    let mut edit = detail().def;
    edit.task = "Digest v2".into();
    assert_eq!(update(home.path(), &alice(), edit).unwrap().armed_host, "");
    // Becoming a shared job places it on the Mac that changed it.
    let mut edit = detail().def;
    edit.runs_on = "one_mac".into();
    assert_eq!(
        update(home.path(), &alice(), edit).unwrap().armed_host,
        here
    );
    assert!(detail().enabled, "its old recipe switch does not pause it");
    // Becoming a recipe again keeps that Mac, which then starts on.
    let mut edit = detail().def;
    edit.runs_on = "each_mac".into();
    assert_eq!(
        update(home.path(), &alice(), edit).unwrap().armed_host,
        here
    );
    assert!(
        detail().enabled,
        "the switch turned off before it was a shared job was forgotten"
    );
    // Restoring the recipe as first written keeps where it is placed now.
    let restored = service::restore(home.path(), &alice(), &key("digest"), 1, 4, &user()).unwrap();
    assert_eq!(restored.def.armed_host, here);

    // An unknown mode is refused.
    let mut edit = detail().def;
    edit.runs_on = "every_mac".into();
    assert_eq!(
        update(home.path(), &alice(), edit).unwrap_err(),
        RoutineError::Validation(ValidationError::UnknownRunsOn("every_mac".into()))
    );

    // A personal routine has no mode: never a recipe, and a copy is not one.
    let mut journal = routine("journal");
    journal.runs_on = "each_mac".into();
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(personal(1)),
            journal,
            &user()
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::EachMacNeedsTeam)
    );
    let copy =
        service::copy_to_mine(home.path(), &alice(), &key("digest"), personal(1), &user()).unwrap();
    assert_eq!(
        (copy.def.runs_on.as_str(), copy.def.armed_host.as_str()),
        ("", here.as_str())
    );
    let mut edit = copy.def.clone();
    edit.runs_on = "each_mac".into();
    assert_eq!(
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Update {
                key: copy.key.clone(),
                base_revision: 1,
            },
            edit,
            &user(),
        )
        .unwrap_err(),
        RoutineError::Validation(ValidationError::EachMacNeedsTeam)
    );
    // Naming the default mode on a personal routine writes no mode either.
    let mut edit = copy.def.clone();
    edit.runs_on = "one_mac".into();
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: copy.key.clone(),
            base_revision: 1,
        },
        edit,
        &user(),
    )
    .unwrap();
    assert_eq!(saved.def.runs_on, "");
}

#[test]
fn definitions_in_the_codesense_docs_are_read_only_but_the_local_switch_is_not_a_write() {
    let home = tempfile::tempdir().unwrap();
    let docs = crate::context_repo::codesense_docs::bundle_info();
    crate::context_repo::codesense_docs::ensure_placed(
        home.path(),
        &alice(),
        include_bytes!("../../resources/codesense-docs.tar.gz"),
    )
    .unwrap();
    let key = RoutineKey::scoped(&alice(), &docs.slug, &docs.team, "update-codesense-docs");
    let refused = |result: Result<_, RoutineError>| {
        let error = result
            .expect_err("a docs routine write must be refused")
            .to_string();
        assert!(
            error.contains(crate::context_repo::codesense_docs::READ_ONLY_MESSAGE),
            "{error}"
        );
    };
    refused(service::archive(home.path(), &alice(), &key, &user()));
    // An each-Mac recipe needs no arming: a no-op that writes nothing.
    let _ = service::arm_here(home.path(), &alice(), &key, &user());
    let mut def = service::get(home.path(), &alice(), &key).unwrap().def;
    def.name = "Renamed".into();
    refused(service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key.clone(),
            base_revision: def.revision,
        },
        def,
        &user(),
    ));
    // This Mac's on/off switch is local state, outside the docs.
    service::set_enabled(home.path(), &alice(), &key, true).unwrap();
    let root = crate::context_repo::codesense_docs::docs_root(home.path(), &alice());
    let repo = git2::Repository::open(root).unwrap();
    assert!(
        repo.statuses(None).unwrap().is_empty(),
        "the docs stay clean"
    );
}

/// A personal routine never reaches a workspace: its `config/` is no content
/// tier (ported to the preparation engine, SW-XS-8b review r1).
#[test]
fn seam_personal_routine_is_never_hydrated() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let me = Principal::from(42u64);
    write_org(home, 42, ORG);
    service::save(
        home,
        &me,
        SaveTarget::Create(personal(42)),
        RoutineDef {
            owner: String::new(),
            ..routine("private-journal")
        },
        &user(),
    )
    .unwrap();
    let space = crate::context_repo::scratch_space::org_space(home, &me, ORG).unwrap();
    assert!(defs::routine_dir(&space.config_dir(), "private-journal")
        .join("routine.yml")
        .exists());
    let personal_content = space.content_dir();
    std::fs::create_dir_all(personal_content.join("skills")).unwrap();
    std::fs::write(personal_content.join("skills/s.md"), "personal skill").unwrap();

    let target = crate::agents::ManifestTarget::team(ORG, TEAM);
    let (workspace, paths) =
        crate::agents::prepare::step3::tests::hydrate_defaults(home, &me, &target);
    assert!(workspace.path().join("skills/s.md").exists());
    let leaked: Vec<_> = paths
        .iter()
        .filter(|p| {
            p.components().any(|c| c.as_os_str() == "routines")
                || p.file_name().is_some_and(|n| n == "routine.yml")
        })
        .collect();
    assert!(leaked.is_empty(), "personal routine leaked: {leaked:?}");
}
