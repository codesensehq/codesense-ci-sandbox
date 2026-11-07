// SPDX-License-Identifier: MIT
//! SW-AGR-1 seam tests: routines live under their agent. Real temp homes, the
//! real coordinator SQLite, real agent and routine stores; a "pre-upgrade"
//! home is seeded the way a v0.0.20 client leaves it (legacy
//! `config/routines/<id>/` files plus run-store rows) and then migrated.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use crate::agents::model::LaunchMode;
use crate::agents::AgentKey;
use crate::context_repo::scratch_space;
use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
use codesense_pty_core::agent_cli::AgentCli;

use super::admit::{self, Fire};
use super::defs;
use super::discover::{discover_routines, migrate_all};
use super::layout;
use super::lifecycle;
use super::migrate::{self, MOVED_TO};
use super::model::{AgentRef, Author, RoutineDef, RoutineKey, RoutineMode, RunsOn};
use super::service::{self, Placement, RoutineError, SaveTarget};
use super::state::RoutineStateStore;
use super::test_support::{agent, alice, copy_dir, save_team_agent, write_org, ORG, TEAM};
use super::tick::RunSource;

/// A home with Alice's org synced, its `default` agent, and Barb.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path(), 1);
    save_team_agent(home.path(), &alice(), agent("barb", AgentCli::Claude));
    home
}

fn team_config(home: &Path) -> PathBuf {
    path_to_team_config_dir(&path_to_org_root(home, &alice(), ORG).unwrap(), TEAM)
}

fn scratch_config(home: &Path) -> PathBuf {
    scratch_space::ensure_org_space(home, &alice(), ORG)
        .unwrap()
        .0
        .config_dir()
}

fn key(id: &str) -> RoutineKey {
    RoutineKey::new(ORG, TEAM, id)
}

fn barb() -> AgentKey {
    AgentKey::new(ORG, TEAM, "barb")
}

fn user() -> Author {
    Author::user("alice")
}

/// A routine as v0.0.20 wrote it: `agent:` named, no `mode`.
fn legacy_yaml(id: &str, agent: &str, revision: u32, task: &str) -> String {
    format!(
        "id: {id}\nname: Routine {id}\nowner: '1'\nagent: {agent}\ntask: {task}\nwhere:\n  kind: team_workspace\ntriggers:\n- schedule:\n    cron: 0 9 * * 1-5\n    tz: America/Denver\npolicy:\n  overlap: coalesce\n  missed: skip\n  max_runtime: 30m\n  visible: false\nrevision: {revision}\narmed_host: host-legacy\n"
    )
}

/// Write a legacy routine with revisions `1..=revisions` into `config_dir`.
fn write_legacy(config_dir: &Path, id: &str, agent: &str, revisions: u32) -> PathBuf {
    let dir = layout::legacy_routine_dir(config_dir, id);
    std::fs::create_dir_all(dir.join("revisions")).unwrap();
    for n in 1..=revisions {
        let body = legacy_yaml(id, agent, n, &format!("Sweep revision {n}"));
        let indented: String = body.lines().map(|l| format!("  {l}\n")).collect();
        std::fs::write(
            dir.join(format!("revisions/{n}.yml")),
            format!("revision: {n}\nauthor:\n  kind: user\n  name: alice\nat: 2026-09-0{n}T09:00:00Z\nroutine:\n{indented}"),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("routine.yml"),
        legacy_yaml(id, agent, revisions, &format!("Sweep revision {revisions}")),
    )
    .unwrap();
    dir
}

/// Every `routine_*` table, row for row, as text: what a migration must not
/// touch.
fn run_store_snapshot(home: &Path) -> BTreeMap<String, Vec<String>> {
    let conn = rusqlite::Connection::open(crate::coordinator::default_store_path(home)).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'routine_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = BTreeMap::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = stmt.column_count();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..columns)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        out.insert(table, rows);
    }
    out
}

/// Every file under `root`, relative path to bytes.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path().strip_prefix(root).unwrap().display().to_string(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// Seed the run store the way a v0.0.20 client leaves it: two runs of
/// `triage`, a cursor for its schedule, and `nightly` paused here.
fn seed_run_store(home: &Path) {
    let store = RoutineStateStore::open(home).unwrap();
    let triage = defs::read_raw(&layout::legacy_routine_dir(&team_config(home), "triage"))
        .unwrap()
        .unwrap();
    for now in [1_000, 2_000] {
        store
            .transaction(|tx| {
                admit::admit(
                    tx,
                    home,
                    &Fire {
                        key: &key("triage"),
                        principal: &alice(),
                        def: &triage,
                        source: RunSource::Manual,
                        occurrence_key: None,
                        scheduled_at: None,
                        now,
                        variables: BTreeMap::new(),
                        trigger_payload: None,
                        cause: None,
                    },
                )
            })
            .unwrap();
    }
    store
        .seed_trigger(
            &format!("{}#0", key("triage")),
            &key("triage"),
            Some(9_999_999),
            "hash",
            &alice(),
        )
        .unwrap();
    store
        .set_enabled(&key("nightly"), RunsOn::OneMac, false)
        .unwrap();
}

fn pre_upgrade_home() -> tempfile::TempDir {
    let home = home();
    let config = team_config(home.path());
    write_legacy(&config, "triage", "barb", 2);
    write_legacy(&config, "nightly", "builtin/claude", 1);
    write_legacy(
        &scratch_config(home.path()),
        "journal",
        "acme/api/backend/barb",
        1,
    );
    seed_run_store(home.path());
    home
}

// ── AC1: legacy routines move under their agents, runs and switches intact ──

#[test]
fn legacy_routines_move_under_their_agents_with_run_history_and_switches_intact() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    let before = run_store_snapshot(home.path());
    assert_eq!(before["routine_runs"].len(), 2, "seeded runs: {before:?}");

    migrate_all(home.path(), &alice()).unwrap();

    // Barb's routine, with its own revisions, under Barb.
    let triage = config.join("agents/barb/routines/triage");
    for file in ["routine.yml", "revisions/1.yml", "revisions/2.yml"] {
        assert!(triage.join(file).is_file(), "{file} missing");
    }
    let yaml = std::fs::read_to_string(triage.join("routine.yml")).unwrap();
    assert!(yaml.contains("mode: fresh"), "{yaml}");
    assert!(!yaml.contains("agent:"), "the path names the agent: {yaml}");
    // The starter's under `_builtin/<starter-id>`.
    assert!(config
        .join("agents/_builtin/claude/routines/nightly/routine.yml")
        .is_file());
    // A personal routine on a team agent: the full-key dir.
    assert!(scratch_config(home.path())
        .join("agents/acme~api~backend~barb/routines/journal/routine.yml")
        .is_file());

    // The run store is untouched: same tables, rows and keys.
    assert_eq!(run_store_snapshot(home.path()), before);
    let store = RoutineStateStore::open(home.path()).unwrap();
    let host = super::local_host_id(home.path()).unwrap();
    let nightly = service::get(home.path(), &alice(), &key("nightly")).unwrap();
    assert!(!store
        .is_enabled(&key("nightly"), &nightly.def, &host)
        .unwrap());
    assert_eq!(store.runs(&key("triage")).unwrap().len(), 2);

    // Reads see the agent the path names, and the full history.
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert_eq!(detail.def.agent, AgentRef::from("barb"));
    assert_eq!(detail.agent_key, Some(barb()));
    assert!(!detail.unassigned);
    assert_eq!(detail.def.mode().unwrap(), RoutineMode::Fresh);
    assert_eq!(detail.revisions.len(), 2);
    assert_eq!(detail.revisions[0].routine.task, "Sweep revision 1");
    assert_eq!(detail.def.revision, 2);

    // Listed once each, where they moved, with their last run.
    let listed = service::list_summaries(home.path(), &alice()).unwrap();
    let ids: Vec<&str> = listed.iter().map(|s| s.key.id.as_str()).collect();
    assert_eq!(
        ids.iter().filter(|id| **id == "triage").count(),
        1,
        "{ids:?}"
    );
    let history = service::runs(home.path(), &alice(), &key("triage"), Default::default()).unwrap();
    assert_eq!(
        history.runs.len(),
        2,
        "its run history reads where it moved"
    );
    let journal = listed.iter().find(|s| s.key.id == "journal").unwrap();
    assert_eq!(
        journal.agent_key,
        Some(barb()),
        "a personal routine on a team agent belongs to it"
    );
}

#[test]
fn migration_is_idempotent_keyed_by_content_and_writes_nothing_on_a_rerun() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    let first = migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
    assert_eq!(first.moved.len(), 2, "{first:?}");
    let after_first = tree(&config);
    for _ in 0..2 {
        let again = migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
        assert!(again.is_empty(), "a rerun writes nothing: {again:?}");
    }
    assert_eq!(tree(&config), after_first);
}

// ── AC5: an older client still reads the legacy location for one release ──

/// The routine definition exactly as v0.0.20 parses it: `agent` required,
/// unknown keys kept in a catch-all. (Frozen copy of that release's shape.)
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V0020RoutineDef {
    id: String,
    name: String,
    #[serde(default)]
    owner: String,
    agent: String,
    task: String,
    #[serde(default)]
    result_schema: Option<serde_json::Value>,
    #[serde(rename = "where")]
    target: V0020Where,
    #[serde(default)]
    triggers: Vec<serde_yaml::Value>,
    #[serde(default)]
    policy: serde_yaml::Value,
    #[serde(default)]
    variables: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    armed_host: String,
    #[serde(default)]
    runs_on: String,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct V0020Where {
    kind: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

#[test]
fn the_legacy_tombstone_still_reads_as_the_full_routine_for_an_older_client() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    migrate_all(home.path(), &alice()).unwrap();

    let raw =
        std::fs::read_to_string(layout::legacy_routine_dir(&config, "triage").join("routine.yml"))
            .unwrap();
    let old: V0020RoutineDef = serde_yaml::from_str(&raw).expect("v0.0.20 parses the tombstone");
    assert_eq!(old.agent, "barb");
    assert_eq!(old.revision, 2);
    assert_eq!(
        old.armed_host, "host-legacy",
        "it still fires only where it did"
    );
    assert_eq!(
        old.extra.get(MOVED_TO),
        Some(&serde_yaml::Value::String(
            "agents/barb/routines/triage".into()
        ))
    );
    assert!(
        !old.extra.contains_key("mode"),
        "nothing new is written there"
    );
    // And it passes the checks a save of that release runs.
    let def: RoutineDef = serde_yaml::from_str(&raw).unwrap();
    def.validate().unwrap();
    // Its revisions are where that client reads them.
    assert!(layout::legacy_routine_dir(&config, "triage")
        .join("revisions/2.yml")
        .is_file());
}

#[test]
fn an_older_clients_edit_is_merged_three_way_keeping_newer_edits_and_the_archive() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    migrate_all(home.path(), &alice()).unwrap();
    let legacy_dir = layout::legacy_routine_dir(&config, "triage");
    // The older client's copy of its file, before the newer client acts.
    let stale = std::fs::read_to_string(legacy_dir.join("routine.yml")).unwrap();

    // A newer client renames it, sets what an older one does not know, then
    // archives it.
    let current = service::get_current(home.path(), &alice(), &key("triage")).unwrap();
    let mut edited = current.def.clone();
    edited.name = "Triage (renamed by a newer client)".into();
    edited.target.machine = Some("m-7f3a".into());
    // A machine other than this Mac needs wake (SW-AGR-2: fresh stays local).
    edited.mode = "wake".into();
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: current.def.revision,
        },
        edited,
        &user(),
    )
    .unwrap();
    service::archive(home.path(), &alice(), &key("triage"), &user()).unwrap();
    let archived_at = service::get_current(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def
        .archived_at
        .unwrap();
    // The archive reached the tombstone: an older Mac stops firing it.
    let tombstone = defs::read_raw(&legacy_dir).unwrap().unwrap();
    assert_eq!(tombstone.archived_at.as_deref(), Some(archived_at.as_str()));
    assert_eq!(
        tombstone.name, "Routine triage",
        "only what it fires by is mirrored"
    );

    // The older client, still on its stale copy, edits the task and points it
    // at another agent, as revision 3.
    std::fs::write(legacy_dir.join("routine.yml"), &stale).unwrap();
    let mut old = defs::read_raw(&legacy_dir).unwrap().unwrap();
    old.task = "Sweep, edited on the old client".into();
    old.agent = AgentRef::from("default");
    old.revision = 3;
    defs::write_revision(
        &legacy_dir,
        &old,
        &Author::user("bob"),
        "2026-10-01T00:00:00Z",
    )
    .unwrap();

    migrate_all(home.path(), &alice()).unwrap();
    assert!(
        config
            .join("agents/default/routines/triage/routine.yml")
            .is_file(),
        "it follows the agent the old client picked"
    );
    assert!(!config.join("agents/barb/routines/triage").exists());
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    // Only what the old client changed is applied...
    assert_eq!(detail.def.task, "Sweep, edited on the old client");
    // ...the newer client's edits, the fields only it knows and its archive stay.
    assert_eq!(detail.def.name, "Triage (renamed by a newer client)");
    assert_eq!(detail.def.target.machine.as_deref(), Some("m-7f3a"));
    assert_eq!(
        detail.def.archived_at.as_deref(),
        Some(archived_at.as_str())
    );
    assert_eq!(detail.def.mode, "wake", "the newer client's mode stays");
    let last = detail.revisions.last().unwrap();
    assert_eq!(last.author, Author::user("bob"));
    assert_eq!(last.at, "2026-10-01T00:00:00Z");
    // The run store did not move.
    assert_eq!(
        RoutineStateStore::open(home.path())
            .unwrap()
            .runs(&key("triage"))
            .unwrap()
            .len(),
        2
    );
    // Applied once.
    let tree_after = tree(&config);
    migrate_all(home.path(), &alice()).unwrap();
    assert_eq!(tree(&config), tree_after);
}

#[test]
fn a_newer_clients_save_mirrors_what_an_older_one_fires_by_and_reads_as_no_edit() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    migrate_all(home.path(), &alice()).unwrap();
    let legacy_dir = layout::legacy_routine_dir(&config, "triage");
    // This Mac takes the routine over: the older Mac it was armed on must stop.
    service::arm_here(home.path(), &alice(), &key("triage"), &user()).unwrap();
    let host = super::local_host_id(home.path()).unwrap();
    let tombstone = defs::read_raw(&legacy_dir).unwrap().unwrap();
    assert_eq!(tombstone.armed_host, host);
    assert_eq!(
        tombstone.extra.get(MOVED_TO),
        Some(&serde_yaml::Value::String(
            "agents/barb/routines/triage".into()
        ))
    );
    // Moved to another agent: the tombstone follows.
    let current = service::get_current(home.path(), &alice(), &key("triage")).unwrap();
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: current.def.revision,
        },
        RoutineDef {
            agent: AgentRef::from("default"),
            ..current.def
        },
        &user(),
    )
    .unwrap();
    let tombstone = defs::read_raw(&legacy_dir).unwrap().unwrap();
    assert_eq!(tombstone.agent, AgentRef::from("default"));
    assert_eq!(
        tombstone.extra.get(MOVED_TO),
        Some(&serde_yaml::Value::String(
            "agents/default/routines/triage".into()
        ))
    );
    // A save whose mirror never ran (a crash between the two writes): the next
    // migration pass settles the tombstone.
    let mut moved_on = service::get_current(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def;
    moved_on.armed_host = "another-mac".into();
    moved_on.revision += 1;
    let new_dir = config.join("agents/default/routines/triage");
    defs::write_revision(&new_dir, &moved_on, &user(), "2026-10-05T10:00:00Z").unwrap();
    assert_eq!(
        defs::read_raw(&legacy_dir).unwrap().unwrap().armed_host,
        host
    );
    migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
    assert_eq!(
        defs::read_raw(&legacy_dir).unwrap().unwrap().armed_host,
        "another-mac"
    );
    // The mirror is no older client's edit: nothing is re-applied.
    let revision = service::get_current(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def
        .revision;
    let report = migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(
        service::get_current(home.path(), &alice(), &key("triage"))
            .unwrap()
            .def
            .revision,
        revision
    );
}

#[test]
fn the_migration_hash_is_keyed_on_the_file_not_on_this_builds_model() {
    // A hand-written legacy file that leaves out every defaulted field.
    let raw: serde_yaml::Value = serde_yaml::from_str(
        "id: tiny\nname: Tiny\nowner: '1'\nagent: barb\ntask: Sweep\nwhere:\n  kind: team_workspace\npolicy:\n  max_runtime: 30m\nrevision: 1\narmed_host: h\n",
    )
    .unwrap();
    // Fixed by the file's content alone: adding (and defaulting) a field in
    // `RoutineDef` cannot move it, and neither can `moved_to` or key order.
    let golden = migrate::content_hash(&raw);
    assert_eq!(
        golden,
        migrate::content_hash(
            &serde_yaml::from_str::<serde_yaml::Value>(
                "moved_to: agents/barb/routines/tiny\narmed_host: h\nrevision: 1\npolicy:\n  max_runtime: 30m\nwhere:\n  kind: team_workspace\ntask: Sweep\nagent: barb\nowner: '1'\nname: Tiny\nid: tiny\n",
            )
            .unwrap()
        )
    );
    // Re-serializing through this build's model adds its defaults: a
    // different content, which the migrator never compares against.
    let through_model: RoutineDef = serde_yaml::from_value(raw.clone()).unwrap();
    assert_ne!(
        golden,
        migrate::content_hash(&serde_yaml::to_value(&through_model).unwrap())
    );
    // A minimal legacy file migrates once and then reads as unchanged.
    let home = home();
    let config = team_config(home.path());
    let dir = layout::legacy_routine_dir(&config, "tiny");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("routine.yml"),
        serde_yaml::to_string(&raw).unwrap(),
    )
    .unwrap();
    assert_eq!(
        migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config)
            .moved
            .len(),
        1
    );
    let after = tree(&config);
    assert!(migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config).is_empty());
    assert_eq!(tree(&config), after);
}

#[test]
fn two_clients_migrating_the_same_tree_write_identical_bytes_so_their_syncs_merge() {
    // Mac A and Mac B each have the same synced legacy tree.
    let a = pre_upgrade_home();
    let b = home();
    let (config_a, config_b) = (team_config(a.path()), team_config(b.path()));
    std::fs::remove_dir_all(layout::agents_dir(&config_b).join("barb")).ok();
    copy_dir(&config_a, &config_b);

    migrate_all(a.path(), &alice()).unwrap();
    migrate_all(b.path(), &alice()).unwrap();
    assert_eq!(
        tree(&config_a),
        tree(&config_b),
        "byte-identical: a clean merge"
    );

    // The merged tree (B's pushed onto A's) migrates to nothing new.
    copy_dir(&config_b, &config_a);
    let merged = tree(&config_a);
    migrate_all(a.path(), &alice()).unwrap();
    assert_eq!(tree(&config_a), merged);
    assert!(!config_a.join("agents/barb/routines/triage-2").exists());
}

#[test]
fn concurrent_migrations_on_one_mac_copy_each_routine_once() {
    let home = pre_upgrade_home();
    let config = team_config(home.path());
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let (home, config, barrier) =
                (home.path().to_path_buf(), config.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                migrate::migrate_space(&home, &alice(), ORG, TEAM, &config)
            })
        })
        .collect();
    let reports: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let moved: usize = reports.iter().map(|r| r.moved.len()).sum();
    assert_eq!(
        moved, 2,
        "each legacy routine copied exactly once: {reports:?}"
    );
    assert!(reports.iter().all(|r| r.renamed.is_empty()));
    let mut ids: Vec<String> = discover_routines(home.path(), &alice())
        .unwrap()
        .into_iter()
        .filter(|r| !r.key.is_personal())
        .map(|r| r.key.id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["nightly", "triage"]);
}

#[test]
fn a_legacy_id_already_taken_under_an_agent_is_renamed_and_the_holders_runs_stay() {
    let home = home();
    let config = team_config(home.path());
    // A newer client created `standup` under Barb, and it has a live run here...
    create(home.path(), "standup", "barb");
    let live = service::run_now(home.path(), &alice(), &key("standup"), BTreeMap::new())
        .unwrap()
        .run_id;
    // ...while an older client's own `standup` (on `default`) syncs into the legacy dir.
    write_legacy(&config, "standup", "default", 1);

    let report = migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
    assert_eq!(
        report.renamed,
        vec![("standup".to_string(), "standup-2".to_string())]
    );
    let renamed = service::get_current(home.path(), &alice(), &key("standup-2")).unwrap();
    assert_eq!(renamed.def.agent, AgentRef::from("default"));
    let note = renamed
        .def
        .migrated
        .as_ref()
        .and_then(|m| m.note.clone())
        .unwrap();
    assert!(note.contains("Renamed from standup"), "{note}");
    // Barb's routine keeps its id, and its runs stay with it, untouched.
    let kept = service::get_current(home.path(), &alice(), &key("standup")).unwrap();
    assert_eq!(kept.def.agent, AgentRef::from("barb"));
    let store = RoutineStateStore::open(home.path()).unwrap();
    let runs = store.runs(&key("standup")).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].run_id.as_str(), runs[0].phase.as_str()),
        (live.as_str(), "queued")
    );
    assert!(store.runs(&key("standup-2")).unwrap().is_empty());
    // And the rename is stable on a rerun.
    assert!(migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config).is_empty());
}

#[test]
fn a_read_only_docs_context_is_never_migrated() {
    let home = home();
    let docs = crate::context_repo::codesense_docs::bundle_info();
    let org_root = path_to_org_root(home.path(), &alice(), &docs.slug).unwrap();
    let config = path_to_team_config_dir(&org_root, &docs.team);
    write_legacy(&config, "update-codesense-docs", "builtin/claude", 1);
    let before = tree(&config);
    let report = migrate::migrate_space(home.path(), &alice(), &docs.slug, &docs.team, &config);
    assert!(report.is_empty());
    assert_eq!(tree(&config), before);
}

// ── Storage: under the agent, no `agent:` key, ids unique across the space ──

fn routine_body(id: &str, agent: &str) -> RoutineDef {
    let mut def: RoutineDef = serde_yaml::from_str(&legacy_yaml(id, agent, 1, "Sweep")).unwrap();
    def.armed_host = String::new();
    def
}

fn create(home: &Path, id: &str, agent: &str) -> RoutineDef {
    service::save(
        home,
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        routine_body(id, agent),
        &user(),
    )
    .unwrap()
    .def
}

#[test]
fn a_new_routine_is_written_under_its_agent_and_never_stored_as_wake() {
    let home = home();
    let config = team_config(home.path());
    let def = create(home.path(), "triage", "barb");
    let dir = config.join("agents/barb/routines/triage");
    let yaml = std::fs::read_to_string(dir.join("routine.yml")).unwrap();
    assert!(!yaml.contains("agent:"), "{yaml}");
    let rev1 = std::fs::read_to_string(dir.join("revisions/1.yml")).unwrap();
    assert!(!rev1.contains("agent:"), "{rev1}");
    assert_eq!(def.agent, AgentRef::from("barb"));
    assert!(
        !layout::legacy_routine_dir(&config, "triage").exists(),
        "nothing new in the legacy dir"
    );
    // No mode picked on a headless routine: its agent's default, `fresh`,
    // written explicitly.
    assert_eq!(def.mode, "fresh");
    assert!(yaml.contains("mode: fresh"), "{yaml}");
    // A routine stored with no mode (written before modes) gets it on its next save.
    let mut bare = defs::read_raw(&dir).unwrap().unwrap();
    bare.mode = String::new();
    bare.agent = AgentRef::default();
    defs::write_current(&dir, &bare).unwrap();
    let current = service::get_current(home.path(), &alice(), &key("triage")).unwrap();
    assert!(current.def.mode.is_empty());
    let updated = service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: current.def.revision,
        },
        RoutineDef {
            name: "Renamed".into(),
            ..current.def
        },
        &user(),
    )
    .unwrap();
    assert_eq!(updated.def.mode, "fresh");
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert_eq!(detail.effective_mode, RoutineMode::Fresh);
    // `wake` is accepted on a saved agent; a starter's routines stay fresh.
    for (agent, accepted) in [("barb", true), ("builtin/claude", false)] {
        let saved = service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            RoutineDef {
                mode: "wake".into(),
                ..routine_body(&format!("woken-{}", agent.replace('/', "-")), agent)
            },
            &user(),
        );
        if accepted {
            assert_eq!(saved.unwrap().def.mode, "wake");
        } else {
            assert!(
                matches!(
                    saved,
                    Err(RoutineError::Validation(
                        super::ValidationError::WakeNeedsSavedAgent(_)
                    ))
                ),
                "{saved:?}"
            );
        }
    }
    // The default a new routine gets respects visibility: only a
    // visible routine on a saved foreground agent wakes.
    let mut visible = routine_body("v", "barb");
    visible.policy.visible = true;
    assert_eq!(
        service::default_mode(home.path(), &alice(), &key("v"), &visible),
        RoutineMode::Wake
    );
    assert_eq!(
        service::default_mode(home.path(), &alice(), &key("v"), &routine_body("v", "barb")),
        RoutineMode::Fresh,
        "headless is fresh"
    );
    save_team_agent(home.path(), &alice(), agent("opencode", AgentCli::OpenCode));
    let mut open_code = routine_body("opencode-visible", "opencode");
    open_code.policy.visible = true;
    assert_eq!(
        service::default_mode(home.path(), &alice(), &key("opencode-visible"), &open_code),
        RoutineMode::Wake,
        "a foreground CLI with a wake delivery path defaults to wake"
    );
    let explicit_wake = RoutineDef {
        mode: "wake".into(),
        ..routine_body("opencode-explicit-wake", "opencode")
    };
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        explicit_wake.clone(),
        &user(),
    );
    assert_eq!(saved.unwrap().def.mode, "wake");
    assert_eq!(
        service::effective_mode(
            home.path(),
            &alice(),
            &key("opencode-explicit-wake"),
            &explicit_wake,
        ),
        RoutineMode::Wake,
        "a wake the CLI can deliver is delivered"
    );
    let mut starter = routine_body("s", "builtin/claude");
    starter.policy.visible = true;
    assert_eq!(
        service::default_mode(home.path(), &alice(), &key("s"), &starter),
        RoutineMode::Fresh
    );
    let mut headless = agent("batch", AgentCli::Codex);
    headless.launch = LaunchMode::Headless;
    save_team_agent(home.path(), &alice(), headless);
    let mut batch = routine_body("b", "batch");
    batch.policy.visible = true;
    assert_eq!(
        service::default_mode(home.path(), &alice(), &key("b"), &batch),
        RoutineMode::Fresh
    );
}

#[test]
fn two_routines_of_one_id_list_one_as_a_duplicate_that_never_fires() {
    let home = home();
    let config = team_config(home.path());
    create(home.path(), "triage", "barb");
    // A sync brings a second `triage`, under `default`.
    let barbs = config.join("agents/barb/routines/triage");
    let defaults = config.join("agents/default/routines/triage");
    copy_dir(&barbs, &defaults);
    let found: Vec<_> = discover_routines(home.path(), &alice())
        .unwrap()
        .into_iter()
        .filter(|r| r.key == key("triage"))
        .collect();
    assert_eq!(found.len(), 2);
    // The first in agent-dir order owns the key, as every keyed read resolves it.
    let winner = found.iter().find(|r| !r.duplicate).unwrap();
    let loser = found.iter().find(|r| r.duplicate).unwrap();
    assert_eq!(winner.dir, barbs);
    assert_eq!(loser.dir, defaults);
    assert!(loser.held() && !winner.held());
    assert_eq!(
        service::routine_dir_of(home.path(), &alice(), &key("triage")).unwrap(),
        barbs
    );
    let listed = service::list_summaries(home.path(), &alice()).unwrap();
    let dup = listed.iter().find(|s| s.duplicate).unwrap();
    assert_eq!(dup.next_run_at, None);
    // Agent lifecycle never acts on the duplicate under a shared key.
    assert!(
        lifecycle::active_owned(home.path(), &alice(), &AgentKey::new(ORG, TEAM, "default"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_agent_that_is_there_but_does_not_read_keeps_its_routine_firing() {
    let home = home();
    let config = team_config(home.path());
    create(home.path(), "triage", "barb");
    // Barb's file is there but malformed (a hand edit, a newer build's shape).
    std::fs::write(
        config.join("agents/barb/agent.yaml"),
        "runtime: [not, a, runtime\n",
    )
    .unwrap();
    let listed = service::list_summaries(home.path(), &alice()).unwrap();
    assert!(!listed[0].unassigned, "only a missing agent is Unassigned");
    // It fires, and its run fails visibly at dispatch (`agent_unavailable`).
    let admission =
        service::run_now(home.path(), &alice(), &key("triage"), BTreeMap::new()).unwrap();
    assert!(admission.is_queued());
}

#[test]
fn a_routine_id_is_unique_across_every_agent_of_the_space() {
    let home = home();
    create(home.path(), "triage", "barb");
    let clash = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        routine_body("triage", "default"),
        &user(),
    );
    assert!(
        matches!(clash, Err(RoutineError::AlreadyExists(_))),
        "{clash:?}"
    );
    assert_eq!(
        service::first_free_id(&team_config(home.path()), "triage"),
        "triage-2"
    );
}

#[test]
fn picking_another_agent_moves_the_routine_with_its_key_history_and_runs() {
    let home = home();
    let config = team_config(home.path());
    create(home.path(), "triage", "barb");
    service::run_now(home.path(), &alice(), &key("triage"), BTreeMap::new()).unwrap();
    let current = service::get_current(home.path(), &alice(), &key("triage")).unwrap();
    service::save(
        home.path(),
        &alice(),
        SaveTarget::Update {
            key: key("triage"),
            base_revision: current.def.revision,
        },
        RoutineDef {
            agent: AgentRef::from("default"),
            ..current.def
        },
        &user(),
    )
    .unwrap();
    assert!(!config.join("agents/barb/routines/triage").exists());
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert_eq!(detail.agent_key, Some(AgentKey::new(ORG, TEAM, "default")));
    assert_eq!(detail.revisions.len(), 2, "its own history came along");
    assert_eq!(
        RoutineStateStore::open(home.path())
            .unwrap()
            .runs(&key("triage"))
            .unwrap()
            .len(),
        1
    );
}

// ── AC2: a routine whose agent is gone is Unassigned and never fires ──

#[test]
fn a_routine_whose_agent_was_deleted_is_unassigned_and_never_fires() {
    let home = home();
    let config = team_config(home.path());
    // Two routines due every five minutes; Barb then disappears (a teammate's
    // delete arriving by sync).
    for (id, agent) in [("orphan", "barb"), ("kept", "default")] {
        let mut body = routine_body(id, agent);
        body.triggers = vec![super::RoutineTrigger::Schedule(super::ScheduleSpec {
            cron: "*/5 * * * *".into(),
            tz: "UTC".into(),
        })];
        service::save(
            home.path(),
            &alice(),
            SaveTarget::Create(Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            }),
            body,
            &user(),
        )
        .unwrap();
    }
    let barb_dir = layout::agents_dir(&config).join("barb");
    std::fs::remove_file(barb_dir.join("agent.yaml"))
        .or_else(|_| std::fs::remove_file(barb_dir.join("agent.yml")))
        .unwrap();
    std::fs::remove_dir_all(barb_dir.join("revisions")).ok();

    let listed = service::list_summaries(home.path(), &alice()).unwrap();
    let orphan = listed.iter().find(|s| s.key == key("orphan")).unwrap();
    assert!(orphan.unassigned);
    assert_eq!(orphan.next_run_at, None);
    assert!(
        !listed
            .iter()
            .find(|s| s.key == key("kept"))
            .unwrap()
            .unassigned
    );
    // Barb's dir holds only routines: it is no agent.
    let agents = crate::agents::discover::discover_agents(home.path(), &alice(), true).unwrap();
    assert!(agents.iter().all(|a| a.key.id != "barb"));

    // Not by hand...
    assert!(matches!(
        service::run_now(home.path(), &alice(), &key("orphan"), BTreeMap::new()),
        Err(RoutineError::Unassigned(_))
    ));
    // ...and not by the scheduler: only `kept` is seeded, and at its due
    // time only it is admitted.
    let store = RoutineStateStore::open(home.path()).unwrap();
    let routines = discover_routines(home.path(), &alice()).unwrap();
    let account = (home.path().to_path_buf(), alice());
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-05T09:01:00Z")
        .unwrap()
        .timestamp_millis();
    let tick = |at: i64| {
        super::tick::run_routine_tick_with(
            at,
            now,
            &account,
            &routines,
            &store,
            &super::tick::RoutineAdmitter,
        )
        .unwrap()
    };
    assert_eq!(tick(now).seeded, 1);
    let due = tick(now + 10 * 60 * 1000);
    assert_eq!(due.admitted, 1);
    assert!(
        due.changed
            .iter()
            .all(|(k, _)| *k == key("kept").to_string()),
        "{:?}",
        due.changed
    );
    assert!(store.runs(&key("orphan")).unwrap().is_empty());
}

// ── AC3: archive takes the routines, unarchive offers them back ──

fn two_barb_routines(home: &Path) {
    create(home, "triage", "barb");
    create(home, "digest", "barb");
    create(home, "other", "default");
}

#[test]
fn archiving_an_agent_archives_its_routines_and_unarchive_offers_them_back() {
    use crate::mcp::agent_tools::{
        self, AgentCaller, AgentDeleteParams, AgentReply, AgentUnarchiveParams,
    };
    let home = home();
    two_barb_routines(home.path());
    let caller = AgentCaller::new(&alice(), None);
    let delete = |archive_routines| AgentDeleteParams {
        id: barb().to_string(),
        hard: false,
        archive_routines,
        session_id: None,
    };

    // The confirm lists both, and nothing changes until it is given.
    let unconfirmed = agent_tools::delete(
        home.path(),
        &alice(),
        &caller,
        &super::agents::RoutineReferrers,
        delete(false),
    )
    .unwrap();
    let AgentReply::Refused(refusal) = unconfirmed else {
        panic!("needs a confirm: {unconfirmed:?}");
    };
    let listed = serde_json::to_value(&refusal).unwrap();
    assert_eq!(listed["code"], "needs_confirm");
    assert_eq!(
        listed["referrers"],
        serde_json::json!([key("digest").to_string(), key("triage").to_string()])
    );
    assert!(
        refusal.message.contains("Routine digest, Routine triage"),
        "{}",
        refusal.message
    );
    assert!(!crate::agents::service::get(home.path(), &alice(), &barb())
        .unwrap()
        .agent
        .is_archived());

    let confirmed = agent_tools::delete(
        home.path(),
        &alice(),
        &caller,
        &super::agents::RoutineReferrers,
        delete(true),
    )
    .unwrap();
    let json = serde_json::to_value(&confirmed).unwrap();
    assert_eq!(
        json["data"]["archived_routines"].as_array().unwrap().len(),
        2,
        "{json}"
    );
    for id in ["triage", "digest"] {
        let def = service::get_current(home.path(), &alice(), &key(id))
            .unwrap()
            .def;
        assert!(def.is_archived() && def.archived_with_agent, "{id}");
    }
    assert!(!service::get_current(home.path(), &alice(), &key("other"))
        .unwrap()
        .def
        .is_archived());

    // `agent_restore` (an old revision of the agent) is not the hook...
    let revision = crate::agents::service::get(home.path(), &alice(), &barb())
        .unwrap()
        .agent
        .revision;
    crate::agents::service::restore(home.path(), &alice(), &barb(), 1, revision, &user()).unwrap();
    assert!(service::get_current(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def
        .is_archived());

    // ...`agent_unarchive` is: it offers both, then restores them on request.
    let revision = crate::agents::service::get(home.path(), &alice(), &barb())
        .unwrap()
        .agent
        .revision;
    let offered = agent_tools::unarchive(
        home.path(),
        &alice(),
        &caller,
        AgentUnarchiveParams {
            id: barb().to_string(),
            base_revision: revision,
            restore_routines: false,
            session_id: None,
        },
    )
    .unwrap();
    let json = serde_json::to_value(&offered).unwrap();
    assert_eq!(
        json["data"]["archived_routines"].as_array().unwrap().len(),
        2,
        "{json}"
    );
    assert!(service::get_current(home.path(), &alice(), &key("triage"))
        .unwrap()
        .def
        .is_archived());
    let restored = lifecycle::restore_owned(home.path(), &alice(), &barb(), &user()).unwrap();
    assert_eq!(restored.len(), 2);
    for id in ["triage", "digest"] {
        let def = service::get_current(home.path(), &alice(), &key(id))
            .unwrap()
            .def;
        assert!(!def.is_archived() && !def.archived_with_agent, "{id}");
    }
}

#[test]
fn deleting_an_agent_is_refused_while_its_routines_are_active_and_keeps_archived_ones() {
    let home = home();
    create(home.path(), "triage", "barb");
    let refused = crate::agents::service::delete(
        home.path(),
        &alice(),
        &barb(),
        None,
        &super::agents::RoutineReferrers,
    );
    let Err(e) = refused else {
        panic!("refused while a routine is active");
    };
    assert_eq!(
        e.to_string(),
        "agent cannot be deleted: it still has active routines: acme/api/backend/triage; archive them first"
    );

    service::archive(home.path(), &alice(), &key("triage"), &user()).unwrap();
    crate::agents::service::delete(
        home.path(),
        &alice(),
        &barb(),
        None,
        &super::agents::RoutineReferrers,
    )
    .unwrap();
    // The archived routine keeps its definition and history.
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert!(detail.def.is_archived());
    assert_eq!(detail.revisions.len(), 2);
    assert!(crate::agents::service::get(home.path(), &alice(), &barb()).is_err());
    // A new Barb takes the id (its dir held only routines) and her old routine with it.
    save_team_agent(home.path(), &alice(), agent("barb", AgentCli::Claude));
    let detail = service::get(home.path(), &alice(), &key("triage")).unwrap();
    assert!(!detail.unassigned);
    assert_eq!(detail.revisions.len(), 2);
}

#[test]
fn agent_discovery_silently_skips_underscore_dirs_and_routine_only_dirs() {
    let home = home();
    let agents_dir = layout::agents_dir(&team_config(home.path()));
    create(home.path(), "nightly", "builtin/claude");
    // Even an agent file inside a `_` dir is not an agent.
    std::fs::create_dir_all(agents_dir.join("_unassigned")).unwrap();
    std::fs::copy(
        agents_dir.join("barb").join("agent.yaml"),
        agents_dir.join("_unassigned").join("agent.yaml"),
    )
    .or_else(|_| {
        std::fs::copy(
            agents_dir.join("barb").join("agent.yml"),
            agents_dir.join("_unassigned").join("agent.yml"),
        )
    })
    .unwrap();
    let scanned: Vec<String> = crate::agents::defs::scan(&agents_dir)
        .into_iter()
        .filter_map(|(dir, _)| dir.file_name()?.to_str().map(str::to_string))
        .collect();
    assert_eq!(scanned, ["barb", "default"]);
}

// ── AC4: agent_get lists the agent's routines with their next run ──

#[test]
fn agent_get_lists_its_routines_with_mode_next_run_and_switch() {
    use crate::mcp::agent_tools::{self, AgentGetParams, AgentReply};
    let home = home();
    two_barb_routines(home.path());
    service::set_enabled(home.path(), &alice(), &key("digest"), false).unwrap();
    let reply = agent_tools::get(
        home.path(),
        &alice(),
        AgentGetParams {
            id: barb().to_string(),
        },
    )
    .unwrap();
    let AgentReply::Ok(got) = &reply else {
        panic!("{reply:?}");
    };
    let json = serde_json::to_value(&got.routines).unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{json}");
    let triage = rows
        .iter()
        .find(|r| r["id"] == key("triage").to_string())
        .unwrap();
    assert_eq!(triage["name"], "Routine triage");
    assert_eq!(triage["mode"], "fresh", "what a fire does in this build");
    assert_eq!(triage["enabled"], true);
    assert!(
        triage["nextRunAt"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z')),
        "{triage}"
    );
    let digest = rows
        .iter()
        .find(|r| r["id"] == key("digest").to_string())
        .unwrap();
    assert_eq!(digest["enabled"], false);
    assert!(digest["nextRunAt"].is_null(), "paused here: {digest}");
}

// ── MCP: `agent` required (or a saved session agent), agent filters ──

#[test]
fn routine_create_needs_an_agent_unless_the_session_is_a_saved_one_and_lists_filter_by_agent() {
    use crate::mcp::routine_tools::{self, CallerSession, STARTER_AGENT_DEPRECATED};
    let home = home();
    let now = chrono::Utc::now();
    let make = |name: &str, session: CallerSession, extra: serde_json::Value| {
        let mut args = serde_json::json!({ "name": name, "task": "Sweep", "when": "daily 09:00" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let reply = routine_tools::create(
            home.path(),
            &alice(),
            Placement::Team {
                org: ORG.into(),
                team: TEAM.into(),
            },
            &session,
            &user(),
            serde_json::from_value(args).unwrap(),
            now,
        )
        .unwrap();
        serde_json::to_value(reply).unwrap()
    };
    let session_as = |agent: AgentKey| CallerSession {
        agent: Some(agent),
        ..CallerSession::default()
    };

    // No agent and no session: refused.
    let none = make("none", CallerSession::default(), serde_json::json!({}));
    assert_eq!(none["status"], "validation", "{none}");
    // A session launched as a saved agent: that agent, no deprecation.
    let saved = make("by barb", session_as(barb()), serde_json::json!({}));
    assert_eq!(saved["status"], "ok", "{saved}");
    assert_eq!(saved["data"]["agent_key"], barb().to_string());
    assert_eq!(
        saved["data"]["mode"], "fresh",
        "none picked: its agent's default for a headless routine, written explicitly"
    );
    assert_eq!(saved["data"]["effective_mode"], "fresh");
    assert!(saved["data"].get("deprecations").is_none(), "{saved}");
    // A starter (ad-hoc) session: still accepted for one release, noted.
    let starter = make(
        "by starter",
        session_as(AgentKey::builtin("claude")),
        serde_json::json!({}),
    );
    assert_eq!(starter["status"], "ok", "{starter}");
    assert_eq!(
        starter["data"]["deprecations"],
        serde_json::json!([STARTER_AGENT_DEPRECATED])
    );
    assert_eq!(starter["data"]["effective_mode"], "fresh");
    // An explicit agent and mode.
    let explicit = make(
        "explicit",
        CallerSession::default(),
        serde_json::json!({ "agent": "default", "mode": "fresh" }),
    );
    assert_eq!(explicit["data"]["mode"], "fresh", "{explicit}");
    // `wake` on a saved agent, aimed at a machine.
    let wake = make(
        "woken",
        CallerSession::default(),
        serde_json::json!({ "agent": "barb", "mode": "wake", "machine": "codesense-test-1" }),
    );
    assert_eq!(wake["status"], "ok", "{wake}");
    assert_eq!(wake["data"]["mode"], "wake");
    assert_eq!(wake["data"]["effective_mode"], "wake");

    // routine_list / routine_get filter by agent (its id or key).
    let list = |agent: &str| {
        let params = serde_json::from_value(serde_json::json!({ "agent": agent })).unwrap();
        let reply = routine_tools::list(home.path(), &alice(), params, now).unwrap();
        let json = serde_json::to_value(reply).unwrap();
        json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(list("barb"), ["by barb", "woken"]);
    assert_eq!(list(&barb().to_string()), ["by barb", "woken"]);
    assert_eq!(list("builtin/claude"), ["by starter"]);
    let get = |agent: &str| {
        let params = serde_json::from_value(serde_json::json!({
            "id": saved["data"]["id"], "agent": agent
        }))
        .unwrap();
        serde_json::to_value(routine_tools::get(home.path(), &alice(), params, now).unwrap())
            .unwrap()
    };
    assert_eq!(get("barb")["status"], "ok");
    assert_eq!(get("barb")["data"]["agent_key"], barb().to_string());
    assert_eq!(get("default")["status"], "validation");
}

/// SW-AGR-5: the move records the revision it wrote, so the editor shows why a
/// migrated routine starts a new session only until it is saved again.
#[test]
fn the_move_records_the_revision_it_wrote() {
    let home = home();
    let config = team_config(home.path());
    write_legacy(&config, "nightly", "default", 2);
    migrate::migrate_space(home.path(), &alice(), ORG, TEAM, &config);
    let moved = service::get_current(home.path(), &alice(), &key("nightly")).unwrap();
    let migrated = moved.def.migrated.expect("moved under its agent");
    assert_eq!(migrated.revision, Some(moved.def.revision));
}
