// SPDX-License-Identifier: MIT
//! Seam tests for a team routine's two modes across two Macs: a shared job
//! that runs once for the team (`runs_on: one_mac`) and a recipe each Mac runs
//! (`runs_on: each_mac`). Each Mac is its own `$HOME`, so it has its own host
//! id and its own coordinator; a definition reaches the other Mac by copying
//! its directory, as a git sync would. Fires go through the real tick,
//! admission, claim loop and outcome funnel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
use crate::domain::Principal;

use super::model::{Author, RoutineDef};
use super::run_seams::{key, ms, routine, run, store, ticker_as};
use super::service::{self, Placement, SaveTarget};
use super::test_support::{
    alice, copy_dir, drain_claim_loop, stub_agent, StubLauncher, TempHome, ORG, TEAM,
};
use super::tick::{RoutineTicker, TickReport};

/// One Mac: its `$HOME`, who is signed in there, its scheduler and its agents.
struct Mac {
    root: PathBuf,
    home: PathBuf,
    who: Principal,
    ticker: RoutineTicker,
    launcher: Arc<StubLauncher>,
}

impl Mac {
    fn new(root: PathBuf, who: Principal, clock: &Arc<AtomicI64>) -> Self {
        let home = root.join(".codesense");
        let launcher = Arc::new(StubLauncher::new(stub_agent(&root)));
        let ticker = ticker_as(&home, clock, launcher.clone(), who.clone());
        Self {
            root,
            home,
            who,
            ticker,
            launcher,
        }
    }

    fn tick(&self) -> TickReport {
        self.ticker.tick().expect("the wake evaluates")
    }

    fn dir(&self, id: &str) -> PathBuf {
        let org_root = path_to_org_root(&self.home, &self.who, ORG).unwrap();
        super::defs::routine_dir(&path_to_team_config_dir(&org_root, TEAM), id)
    }

    fn def(&self, id: &str) -> RoutineDef {
        service::get_current(&self.home, &self.who, &key(id))
            .unwrap()
            .def
    }

    fn author(&self) -> Author {
        Author::user(if self.who == alice() { "alice" } else { "bob" })
    }

    /// Save `def` as an edit of the routine as this Mac has it.
    fn edit(&self, def: RoutineDef) -> RoutineDef {
        let base_revision = self.def(&def.id).revision;
        service::save(
            &self.home,
            &self.who,
            SaveTarget::Update {
                key: key(&def.id),
                base_revision,
            },
            def,
            &self.author(),
        )
        .unwrap()
        .def
    }

    fn switch(&self, id: &str, on: bool) {
        service::set_enabled(&self.home, &self.who, &key(id), on).unwrap();
    }

    /// Whether this Mac's list shows the routine switched on here.
    fn on_here(&self, id: &str) -> bool {
        service::list_summaries(&self.home, &self.who)
            .unwrap()
            .into_iter()
            .find(|r| r.key == key(id))
            .expect("listed")
            .enabled
    }

    /// Launch every queued run, then have each new agent report `summary`
    /// and exit, through this Mac's outcome funnel.
    async fn run_queued(&self, temp: &TempHome, summary: &str) -> Vec<String> {
        let before = self.launcher.calls();
        let launched = drain_claim_loop(&self.home, self.launcher.clone()).await;
        assert_eq!(self.launcher.calls(), before + launched);
        temp.act_as(&self.root);
        let mut prompts = Vec::new();
        for l in self.launcher.launched().into_iter().skip(before) {
            prompts.push(String::from_utf8(self.launcher.received_prompt(&l.session_id)).unwrap());
            self.launcher
                .signal_done(&l.session_id, "success", summary)
                .await;
            self.launcher.exit(&l.session_id).await;
        }
        temp.act_as(temp.root());
        prompts
    }

    /// Every run row this Mac has of the routine, by occurrence key.
    fn occurrences(&self, id: &str) -> Vec<String> {
        store(&self.home)
            .runs(&key(id))
            .unwrap()
            .into_iter()
            .filter_map(|r| r.occurrence_key)
            .collect()
    }
}

/// Copy the routine as `from` has it into `to`'s clone, as a git sync would.
fn sync(from: &Mac, to: &Mac, id: &str) {
    let dest = super::defs::sync_target(&from.dir(id), &to.dir(id));
    let _ = std::fs::remove_dir_all(&dest);
    copy_dir(&from.dir(id), &dest);
}

fn create_team(mac: &Mac, def: RoutineDef) -> RoutineDef {
    service::save(
        &mac.home,
        &mac.who,
        SaveTarget::Create(Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        def,
        &mac.author(),
    )
    .unwrap()
    .def
}

fn recipe(id: &str, cron: &str) -> RoutineDef {
    RoutineDef {
        runs_on: "each_mac".into(),
        ..routine(id, cron)
    }
}

fn occurrence(id: &str, at: &str) -> String {
    format!("{}@{at}", key(id))
}

/// Alice's Mac and Bob's Mac, on one clock that starts before any fire.
fn two_macs(temp: &TempHome, clock: &Arc<AtomicI64>) -> (Mac, Mac) {
    let alices = Mac::new(temp.root().to_path_buf(), alice(), clock);
    let bobs = Mac::new(temp.other_mac("bobs-mac", 2), Principal::from(2u64), clock);
    (alices, bobs)
}

/// A recipe each Mac runs fires only on the Macs that turned it on, each with
/// its own schedule, runs and `{{last_run_summary}}`; turning it on is local
/// state and never an edit; and an edit to the shared definition reaches
/// every Mac's next fire.
#[tokio::test]
async fn a_recipe_fires_only_where_it_is_on_each_mac_with_its_own_runs() {
    let temp = TempHome::new();
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let (alices, bobs) = two_macs(&temp, &clock);
    let created = create_team(&alices, recipe("digest", "*/5 * * * *"));
    assert_eq!(created.armed_host, "", "a recipe names no Mac");
    sync(&alices, &bobs, "digest");
    let file = std::fs::read(alices.dir("digest").join("routine.yml")).unwrap();

    // Off on every Mac, the one that created it included.
    assert!(!alices.on_here("digest") && !bobs.on_here("digest"));
    assert_eq!(alices.tick().seeded, 0);
    assert_eq!(bobs.tick().seeded, 0);
    // A run by hand on a Mac where it is off is recorded as off here.
    let manual =
        service::run_now(&alices.home, &alice(), &key("digest"), Default::default()).unwrap();
    assert_eq!(
        run(&alices.home, &manual.run_id).reason.as_deref(),
        Some("off_here")
    );

    // Bob turns it on for his Mac. Nothing in the shared definition changes.
    bobs.switch("digest", true);
    assert!(bobs.on_here("digest") && !alices.on_here("digest"));
    assert_eq!(bobs.def("digest").revision, 1);
    clock.store(ms("2026-10-05T09:02:00Z"), Ordering::SeqCst);
    assert_eq!(bobs.tick().seeded, 1);
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0, "off on Alice's Mac");
    assert_eq!(bobs.tick().admitted, 1);
    let prompts = bobs.run_queued(&temp, "bob: 3 new issues").await;
    assert_eq!(prompts, ["Summarize what changed. Last time: "]);

    // Alice turns it on for hers: from then on both fire, each its own run.
    alices.switch("digest", true);
    clock.store(ms("2026-10-05T09:06:00Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().seeded, 1);
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 1);
    // Each Mac's task sees its own last result.
    let prompts = alices.run_queued(&temp, "alice: quiet").await;
    assert_eq!(prompts, ["Summarize what changed. Last time: "]);
    let prompts = bobs.run_queued(&temp, "bob: 1 new issue").await;
    assert_eq!(
        prompts,
        ["Summarize what changed. Last time: bob: 3 new issues"]
    );
    // Each Mac's history holds its own runs only.
    let ten = occurrence("digest", "2026-10-05T09:10:00Z");
    let alice_fires: Vec<String> = alices
        .occurrences("digest")
        .into_iter()
        .filter(|o| !o.contains("@manual"))
        .collect();
    assert_eq!(alice_fires, std::slice::from_ref(&ten));
    assert_eq!(
        bobs.occurrences("digest"),
        [occurrence("digest", "2026-10-05T09:05:00Z"), ten]
    );
    let last = |mac: &Mac| {
        service::list_summaries(&mac.home, &mac.who)
            .unwrap()
            .into_iter()
            .find(|r| r.key == key("digest"))
            .and_then(|r| r.last_run)
            .and_then(|r| r.result_text)
    };
    assert_eq!(last(&alices).as_deref(), Some("alice: quiet"));
    assert_eq!(last(&bobs).as_deref(), Some("bob: 1 new issue"));
    // Switching it on and off never touched the shared file.
    assert_eq!(
        std::fs::read(alices.dir("digest").join("routine.yml")).unwrap(),
        file
    );

    // Alice edits the task; once her edit syncs, both Macs' next fires run it.
    let mut edited = alices.def("digest");
    edited.task = "Digest v2. Last time: {{last_run_summary}}".into();
    let edited = alices.edit(edited);
    assert_eq!(edited.armed_host, "", "an edit does not place a recipe");
    sync(&alices, &bobs, "digest");
    clock.store(ms("2026-10-05T09:15:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 1);
    assert_eq!(
        alices.run_queued(&temp, "a").await,
        ["Digest v2. Last time: alice: quiet"]
    );
    assert_eq!(
        bobs.run_queued(&temp, "b").await,
        ["Digest v2. Last time: bob: 1 new issue"]
    );

    // Bob turns it off for his Mac: only Alice's keeps firing.
    bobs.switch("digest", false);
    clock.store(ms("2026-10-05T09:20:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 0);
    alices.run_queued(&temp, "a").await;
}

/// A shared job that becomes a recipe keeps running on the Mac it ran on,
/// from the same cursor, and starts on nowhere else; a recipe that becomes a
/// shared job runs on the Mac that changed it and stops everywhere else. No
/// occurrence fires twice in either direction.
#[tokio::test]
async fn switching_modes_never_fires_an_occurrence_twice() {
    let temp = TempHome::new();
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let (alices, bobs) = two_macs(&temp, &clock);
    let job = create_team(&alices, routine("triage", "*/5 * * * *"));
    let alices_host = super::local_host_id(&alices.home).unwrap();
    assert_eq!(job.armed_host, alices_host);
    sync(&alices, &bobs, "triage");
    assert_eq!(alices.tick().seeded, 1);
    assert_eq!(bobs.tick().seeded, 0, "a shared job armed on Alice's Mac");

    // Bob makes it a recipe. It still names Alice's Mac, so it stays on there.
    let mut each = bobs.def("triage");
    each.runs_on = "each_mac".into();
    let each = bobs.edit(each);
    assert_eq!(each.armed_host, alices_host);
    assert!(!bobs.on_here("triage"));
    sync(&bobs, &alices, "triage");
    assert!(alices.on_here("triage"));
    // Alice's 09:05 fire comes from the cursor seeded before the switch: once.
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 0, "off on Bob's Mac");
    clock.store(ms("2026-10-05T09:05:20Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0);
    alices.run_queued(&temp, "a").await;
    assert_eq!(
        alices.occurrences("triage"),
        [occurrence("triage", "2026-10-05T09:05:00Z")]
    );
    assert!(bobs.occurrences("triage").is_empty());

    // Bob turns it on for his Mac, and Alice turns hers on too (it already
    // was): both run their own instance.
    bobs.switch("triage", true);
    alices.switch("triage", true);
    clock.store(ms("2026-10-05T09:06:00Z"), Ordering::SeqCst);
    assert_eq!(bobs.tick().seeded, 1);
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 1);
    alices.run_queued(&temp, "a").await;
    bobs.run_queued(&temp, "b").await;

    // Bob makes it a shared job again: it is placed on his Mac, the one that
    // changed it, and Alice's stops once the change reaches her.
    let mut one = bobs.def("triage");
    one.runs_on = "one_mac".into();
    let one = bobs.edit(one);
    assert_eq!(one.armed_host, super::local_host_id(&bobs.home).unwrap());
    sync(&bobs, &alices, "triage");
    clock.store(ms("2026-10-05T09:15:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0, "no longer runs on Alice's Mac");
    assert_eq!(bobs.tick().admitted, 1, "Bob's cursor carries on");
    bobs.run_queued(&temp, "b").await;
    let fifteen = occurrence("triage", "2026-10-05T09:15:00Z");
    let fires = |id: &str, mac: &Mac| -> usize {
        mac.occurrences(id)
            .iter()
            .filter(|o| **o == fifteen)
            .count()
    };
    assert_eq!(fires("triage", &alices) + fires("triage", &bobs), 1);
    assert!(!super::is_armed_here(&alices.def("triage"), &alices_host));

    // Back to a recipe: Bob's Mac (named now) starts on; Alice's earlier
    // "on" was settled when her Mac saw the shared job, so she starts off.
    let mut each = bobs.def("triage");
    each.runs_on = "each_mac".into();
    bobs.edit(each);
    sync(&bobs, &alices, "triage");
    assert!(bobs.on_here("triage"));
    assert!(!alices.on_here("triage"));
    clock.store(ms("2026-10-05T09:20:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0);
    assert_eq!(bobs.tick().admitted, 1);
    bobs.run_queued(&temp, "b").await;
}

/// A shared job paused on its Mac stays paused there when a teammate makes it
/// a recipe: a mode change never starts a routine someone stopped.
#[tokio::test]
async fn a_paused_shared_job_stays_off_on_its_mac_when_it_becomes_a_recipe() {
    let temp = TempHome::new();
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let (alices, bobs) = two_macs(&temp, &clock);
    create_team(&alices, routine("sweep", "*/5 * * * *"));
    alices.switch("sweep", false);
    sync(&alices, &bobs, "sweep");

    let mut each = bobs.def("sweep");
    each.runs_on = "each_mac".into();
    bobs.edit(each);
    sync(&bobs, &alices, "sweep");
    assert!(!alices.on_here("sweep"), "still paused on Alice's Mac");
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0);
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0, "the settled switch keeps it off");
    assert!(!alices.on_here("sweep"));

    // Alice turns it on for her Mac when she wants it.
    alices.switch("sweep", true);
    clock.store(ms("2026-10-05T09:11:00Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().seeded, 1);
    clock.store(ms("2026-10-05T09:15:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    alices.run_queued(&temp, "a").await;
}

/// Sync lag: a mode change reaches another Mac only with its next pull. A
/// shared job that becomes a recipe opens no double-fire window, since every
/// other Mac starts off and the Mac it ran on keeps firing it from one cursor.
/// One that comes back as a shared job on another Mac before this Mac pulls
/// fires on both until this Mac pulls, and this Mac stops on its first wake
/// after the pull. That window is the sync interval, the same one moving a
/// shared job with "Run on this Mac" has.
#[tokio::test]
async fn a_mac_that_has_not_pulled_a_mode_change_stops_on_its_first_wake_after_the_pull() {
    let temp = TempHome::new();
    let clock = Arc::new(AtomicI64::new(ms("2026-10-05T09:01:00Z")));
    let (alices, bobs) = two_macs(&temp, &clock);
    create_team(&alices, routine("triage", "*/5 * * * *"));
    sync(&alices, &bobs, "triage");
    assert_eq!(alices.tick().seeded, 1);

    // Bob makes it a recipe; Alice has not pulled and still holds the shared
    // job. Only Alice fires: Bob's Mac starts off.
    let mut each = bobs.def("triage");
    each.runs_on = "each_mac".into();
    bobs.edit(each);
    assert!(!bobs.on_here("triage"));
    clock.store(ms("2026-10-05T09:05:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1);
    assert_eq!(bobs.tick().admitted, 0, "off on Bob's Mac");
    alices.run_queued(&temp, "a").await;

    // Bob makes it a shared job again, now on his Mac, still before Alice
    // pulls: until she does, her stale copy fires too.
    let mut one = bobs.def("triage");
    one.runs_on = "one_mac".into();
    bobs.edit(one);
    clock.store(ms("2026-10-05T09:06:00Z"), Ordering::SeqCst);
    assert_eq!(bobs.tick().seeded, 1);
    clock.store(ms("2026-10-05T09:10:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 1, "not pulled yet");
    assert_eq!(bobs.tick().admitted, 1);
    alices.run_queued(&temp, "a").await;
    bobs.run_queued(&temp, "b").await;

    // Alice pulls: her first wake after it fires nothing, and it stays so.
    sync(&bobs, &alices, "triage");
    clock.store(ms("2026-10-05T09:15:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0);
    assert_eq!(bobs.tick().admitted, 1);
    bobs.run_queued(&temp, "b").await;
    clock.store(ms("2026-10-05T09:20:10Z"), Ordering::SeqCst);
    assert_eq!(alices.tick().admitted, 0);
    assert_eq!(bobs.tick().admitted, 1);
    bobs.run_queued(&temp, "b").await;
    let twenty = occurrence("triage", "2026-10-05T09:20:00Z");
    assert!(!alices.occurrences("triage").contains(&twenty));
    assert!(bobs.occurrences("triage").contains(&twenty));
}

/// A tick's discovery can finish before an edit and resume after the user
/// pauses the new mode. Its stale recipe must not rewrite that pause's mode.
#[test]
fn stale_tick_after_mode_edit_preserves_current_mode_pause() {
    let temp = TempHome::new();
    let now = ms("2026-10-05T09:01:00Z");
    let clock = Arc::new(AtomicI64::new(now));
    let mac = Mac::new(temp.root().to_path_buf(), alice(), &clock);
    create_team(&mac, recipe("digest", "*/5 * * * *"));
    let stale = super::discover::discover_routines(&mac.home, &mac.who).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].def.runs_on, "each_mac");

    let mut edited = mac.def("digest");
    edited.runs_on = "one_mac".into();
    mac.edit(edited);
    mac.switch("digest", false);
    assert!(!mac.on_here("digest"));

    let state = store(&mac.home);
    super::tick::run_routine_tick_with(
        now,
        now,
        &(mac.home.clone(), mac.who.clone()),
        &stale,
        &state,
        &super::tick::RoutineAdmitter,
    )
    .unwrap();
    let switches = state.switches().unwrap();
    let switch = switches.get(&key("digest").to_string()).unwrap();
    assert_eq!(switch.mode, super::model::RunsOn::OneMac);
    assert!(!switch.enabled);
    assert!(!mac.on_here("digest"));
    assert_eq!(mac.tick().seeded, 0, "the current job remains paused");
}
