// SPDX-License-Identifier: MIT
//! The `role_pending` event trigger: a routine that fires when a message is queued for
//! a role inbox (`{kind: role_pending}`).
//!
//! Nothing claims a role message on its own: it waits until an instance of the role
//! claims it. A controller routine watches the queues through this trigger and decides
//! whether to start a holder. Each message fires once per routine (the occurrence key
//! is the message), so a replayed offer admits nothing new.
//!
//! A team routine reaches the queues of its own team; a personal routine those of every
//! team in its org.
//!
//! The run's `{{trigger.role}}`, `{{trigger.message_id}}`, `{{trigger.org}}` and
//! `{{trigger.team}}` name the queue and the message.

use tauri::{AppHandle, Manager, Runtime};

use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
use crate::commands::sprint_run::causation::{CausalEdge, Causation};
use crate::coordinator::StoreResult;
use crate::weft::role_inbox::role_name;

use super::admit::{self, Admission, EventCause, Fire, Outcome};
use super::discover::DiscoveredRoutine;
use super::model::{RoutineDef, RoutineKey, RoutineTrigger, EVENT_ROLE_PENDING};
use super::state::RoutineStateStore;
use super::tick::{Account, RoutineTicker, RunSource};

/// A message queued for `role` in `org`/`team`'s inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePendingEvent {
    pub org: String,
    pub team: String,
    pub role: String,
    pub message_id: i64,
    /// The session that queued the message, when it was a running session (never the
    /// human or the dashboard), with the routine whose run it is, if any. A routine
    /// never fires on a message its own run chain queued (the self-fire guard).
    pub sender_session: Option<String>,
    pub sender_routine: Option<String>,
}

impl RolePendingEvent {
    fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "role": role_name(&self.role),
            "message_id": self.message_id,
            "org": self.org,
            "team": self.team,
        })
    }
}

/// Whether `def` has a `role_pending` trigger.
pub fn matches(def: &RoutineDef) -> bool {
    def.triggers
        .iter()
        .any(|t| matches!(t, RoutineTrigger::Event(spec) if spec.kind.trim() == EVENT_ROLE_PENDING))
}

/// Whether routine `key` watches the queue `event` landed in.
pub fn in_reach(key: &RoutineKey, event: &RolePendingEvent) -> bool {
    key.org == event.org && (key.account.is_some() || key.team == event.team)
}

/// The routine runs behind the session that queued `event`'s message: the chain its
/// run row carries, else its own routine's edge. Empty for a message the human, the
/// dashboard or a session outside any routine queued.
fn sender_causation(store: &RoutineStateStore, event: &RolePendingEvent) -> Causation {
    let run = match event.sender_session.as_deref() {
        Some(session) => store.run_by_session(session).unwrap_or_else(|e| {
            log::warn!(target: "routines", "session {session}: run unreadable: {e}");
            None
        }),
        None => None,
    };
    if let Some(chain) = run.as_ref().and_then(|r| r.causation.as_deref()) {
        if let Ok(chain) = serde_json::from_str::<Causation>(chain) {
            return chain;
        }
    }
    match run
        .map(|r| r.routine_key)
        .or_else(|| event.sender_routine.clone())
    {
        Some(key) => Causation::default().descend(CausalEdge::new(key.clone(), key)),
        None => Causation::default(),
    }
}

/// Admit `event` for every routine in `routines` this app fires (armed here, enabled)
/// with a `role_pending` trigger in reach, each run dispatched from this Mac's
/// `account`. Each admission is its own transaction.
pub fn offer(
    account: &Account,
    routines: &[DiscoveredRoutine],
    store: &RoutineStateStore,
    event: &RolePendingEvent,
    now: i64,
) -> StoreResult<Vec<(String, Admission)>> {
    let host =
        super::host::local_host_id(&account.0).map_err(crate::coordinator::StoreError::Io)?;
    let payload = event.payload();
    let role = role_name(&event.role);
    // The sender's chain rides on every fire, so the run it starts carries it on, and
    // the loop guard skips (with a `loop_guard` run row) a routine already on it: a
    // message this routine's own run chain queued, or a cycle through other
    // `role_pending` routines (A's run queues for B's role, B's run for A's). Not
    // rate-limited: every message is distinct work.
    let causation = sender_causation(store, event);
    let origin = match event.sender_session.as_deref() {
        Some(session) => format!("role message {} from session {session}", event.message_id),
        None => format!("role message {}", event.message_id),
    };
    let mut admissions = Vec::new();
    for routine in routines {
        if !routine.armed_here
            || !in_reach(&routine.key, event)
            || !routine.def.triggers.iter().any(|trigger| {
                matches!(trigger, RoutineTrigger::Event(spec)
                    if spec.kind.trim() == EVENT_ROLE_PENDING
                        && spec.role.as_deref().is_none_or(|filter| role_name(filter) == role))
            })
        {
            continue;
        }
        match store.is_enabled(&routine.key, &routine.def, &host) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                log::warn!(target: "routines", "routine {}: state unreadable: {e}", routine.key);
                continue;
            }
        }
        let key = routine.key.to_string();
        let occurrence_key = format!(
            "{key}@role_pending:{}/{}/{}:{}",
            event.org, event.team, role, event.message_id
        );
        let fire = Fire {
            key: &routine.key,
            principal: &account.1,
            def: &routine.def,
            source: RunSource::Event,
            occurrence_key: Some(&occurrence_key),
            scheduled_at: None,
            now,
            variables: Default::default(),
            trigger_payload: Some(payload.clone()),
            cause: Some(EventCause {
                origin: &origin,
                causation: &causation,
                rate_limited: false,
            }),
        };
        match store.transaction(|tx| admit::admit(tx, &account.0, &fire)) {
            Ok(admission) => {
                if matches!(&admission.outcome, Outcome::Skipped { reason } if reason == super::runs::reason::LOOP_GUARD)
                {
                    log::warn!(
                        target: "routines",
                        "routine {key}: role_pending fire for message {} skipped: its run chain queued it",
                        event.message_id
                    );
                }
                admissions.push((key, admission))
            }
            Err(e) => log::warn!(
                target: "routines",
                "routine {key}: role_pending fire for message {} failed: {e}",
                event.message_id
            ),
        }
    }
    Ok(admissions)
}

/// The session behind `sender`: a handle's bound session, else `sender` itself (a
/// local agent may name its tab's session id, and an unnamed MCP sender in a tab with
/// no handle is that tab's session). A sender that is no routine run's session finds
/// no run chain, so passing it through is harmless. `None` for the human.
fn sender_session(launch: Option<&DashboardLaunchState>, sender: &str) -> Option<String> {
    let sender = sender.trim();
    if sender.is_empty() || sender == crate::weft::inbox::HUMAN {
        return None;
    }
    launch
        .and_then(|l| l.resolve(sender))
        .or_else(|| Some(sender.to_string()))
}

/// Offer a message queued for `role` in `team` to the app's routines without blocking
/// the caller: discovery and admission run on the blocking pool.
pub fn offer_role_pending<R: Runtime>(
    app: &AppHandle<R>,
    team: &LaunchTeam,
    role: &str,
    message_id: i64,
    sender: &str,
) {
    if app.try_state::<RoutineTicker>().is_none() {
        return;
    }
    let launch = app.try_state::<DashboardLaunchState>();
    let sender_session = sender_session(launch.as_deref(), sender);
    let sender_routine = sender_session
        .as_deref()
        .and_then(|s| launch.as_deref().and_then(|l| l.routine_key_for_session(s)));
    let event = RolePendingEvent {
        org: team.org.clone(),
        team: team.team.clone(),
        role: role_name(role).to_string(),
        message_id,
        sender_session,
        sender_routine,
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let offered = crate::commands::await_blocking("routine_offer_role_pending", move || {
            if let Some(ticker) = app.try_state::<RoutineTicker>() {
                ticker.offer_role_pending(&event);
            }
            Ok(())
        })
        .await;
        if let Err(e) = offered {
            log::warn!(target: "routines", "routine role_pending offer failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::discover::discover_routines;
    use crate::routines::model::EventSpec;
    use crate::routines::run_seams::{key, routine, save, store};
    use crate::routines::test_support::{alice, TempHome, ORG, TEAM};

    fn on_role_pending(id: &str) -> RoutineDef {
        let mut def = routine(id, "0 9 * * *");
        def.task = "Queue {{trigger.role}} has message {{trigger.message_id}}".into();
        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: EVENT_ROLE_PENDING.into(),
            from_agent: None,
            role: None,
            metadata: None,
        })];
        def
    }

    fn queued(team: &str, message_id: i64) -> RolePendingEvent {
        RolePendingEvent {
            org: ORG.into(),
            team: team.into(),
            role: "reviewer".into(),
            message_id,
            sender_session: None,
            sender_routine: None,
        }
    }

    #[test]
    fn a_message_queued_by_the_routines_own_run_does_not_fire_it_again() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("controller"));
        save(&home, on_role_pending("other"));
        let routines = discover_routines(&home, &alice()).unwrap();
        let store = store(&home);
        let account = (home.clone(), alice());
        let now = crate::routines::finish::now_ms();

        let mut event = queued(TEAM, 9);
        event.sender_session = Some("sess-controller-run".into());
        event.sender_routine = Some(key("controller").to_string());
        let fired = offer(&account, &routines, &store, &event, now).unwrap();
        let started: Vec<_> = fired
            .iter()
            .filter(|(_, a)| a.outcome == Outcome::Queued)
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(
            started,
            vec![key("other").to_string()],
            "only the other routine fires"
        );
        // The skip is on the record, not only in the log.
        let runs = store.runs(&key("controller")).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].phase, "skipped");
        assert_eq!(runs[0].reason.as_deref(), Some("loop_guard"));
    }

    /// Record `run_id`'s launched session, as dispatch does.
    fn launched(store: &RoutineStateStore, run_id: &str, session: &str) {
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs SET session_id = ?2 WHERE run_id = ?1",
                    rusqlite::params![run_id, session],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn two_role_pending_routines_queuing_for_each_other_stop_at_the_cycle() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("a"));
        save(&home, on_role_pending("b"));
        let routines = discover_routines(&home, &alice()).unwrap();
        let a: Vec<_> = routines
            .iter()
            .filter(|r| r.key == key("a"))
            .cloned()
            .collect();
        let b: Vec<_> = routines
            .iter()
            .filter(|r| r.key == key("b"))
            .cloned()
            .collect();
        let store = store(&home);
        let account = (home.clone(), alice());
        let now = crate::routines::finish::now_ms();

        // The human queues a message A watches: A's run starts and carries its chain.
        let fired = offer(&account, &a, &store, &queued(TEAM, 1), now).unwrap();
        assert_eq!(fired[0].1.outcome, Outcome::Queued);
        launched(&store, &fired[0].1.run_id, "sess-a");

        // A's run queues for B's role: B starts (no rate limit across messages), and
        // its run carries A's chain on.
        let mut from_a = queued(TEAM, 2);
        from_a.sender_session = Some("sess-a".into());
        let fired = offer(&account, &b, &store, &from_a, now + 1).unwrap();
        assert_eq!(fired[0].1.outcome, Outcome::Queued);
        launched(&store, &fired[0].1.run_id, "sess-b");

        // B's run queues for A's role: A is already on the chain, so it is skipped.
        let mut from_b = queued(TEAM, 3);
        from_b.sender_session = Some("sess-b".into());
        let fired = offer(&account, &a, &store, &from_b, now + 2).unwrap();
        assert_eq!(
            fired[0].1.outcome,
            Outcome::Skipped {
                reason: "loop_guard".into()
            }
        );
        let detail = store.run(&fired[0].1.run_id).unwrap().unwrap().result_text;
        assert_eq!(
            detail.as_deref(),
            Some("cycle (role message 3 from session sess-b)")
        );

        // A message the human queues later still fires A: every message is new work.
        let fired = offer(&account, &a, &store, &queued(TEAM, 4), now + 3).unwrap();
        assert_ne!(
            fired[0].1.outcome,
            Outcome::Skipped {
                reason: "loop_guard".into()
            }
        );
    }

    #[test]
    fn a_queued_role_message_fires_each_watching_routine_once() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("controller"));
        save(&home, routine("nightly", "0 9 * * *"));
        let routines = discover_routines(&home, &alice()).unwrap();
        let store = store(&home);
        let account = (home.clone(), alice());
        let now = crate::routines::finish::now_ms();

        let fired = offer(&account, &routines, &store, &queued(TEAM, 7), now).unwrap();
        assert_eq!(fired.len(), 1, "only the role_pending routine fires");
        assert_eq!(fired[0].0, key("controller").to_string());
        assert_eq!(fired[0].1.outcome, Outcome::Queued);

        // The same message again is a duplicate, never a second run.
        let again = offer(&account, &routines, &store, &queued(TEAM, 7), now).unwrap();
        assert_eq!(again[0].1.outcome, Outcome::Duplicate);
        assert_eq!(store.runs(&key("controller")).unwrap().len(), 1);
    }

    #[test]
    fn prefixed_filter_and_queued_role_name_one_canonical_occurrence() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("controller"));
        let mut routines = discover_routines(&home, &alice()).unwrap();
        // A hand-authored definition can be discovered before an editor save.
        let RoutineTrigger::Event(spec) = &mut routines[0].def.triggers[0] else {
            panic!("event trigger expected");
        };
        spec.role = Some(" role: reviewer ".into());
        let store = store(&home);
        let account = (home.clone(), alice());
        let now = crate::routines::finish::now_ms();
        let mut event = queued(TEAM, 7);
        event.role = " role: reviewer ".into();
        let fired = offer(&account, &routines, &store, &event, now).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].1.outcome, Outcome::Queued);
        let runs = store.runs(&key("controller")).unwrap();
        assert_eq!(
            runs[0].trigger_payload.as_ref().unwrap()["role"],
            "reviewer"
        );
        assert!(runs[0]
            .occurrence_key
            .as_ref()
            .unwrap()
            .contains("/reviewer:7"));
        event.role = "reviewer".into();
        let again = offer(&account, &routines, &store, &event, now).unwrap();
        assert_eq!(again[0].1.outcome, Outcome::Duplicate);
    }

    #[test]
    fn a_team_routine_ignores_another_teams_queue() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("controller"));
        let routines = discover_routines(&home, &alice()).unwrap();
        let fired = offer(
            &(home.clone(), alice()),
            &routines,
            &store(&home),
            &queued("elsewhere", 7),
            crate::routines::finish::now_ms(),
        )
        .unwrap();
        assert!(fired.is_empty());
    }

    #[test]
    fn a_sender_named_by_session_id_is_that_session() {
        let launch = DashboardLaunchState::default();
        assert_eq!(sender_session(Some(&launch), " human "), None);
        assert_eq!(sender_session(Some(&launch), ""), None);
        assert_eq!(
            sender_session(Some(&launch), "sess-tab-1").as_deref(),
            Some("sess-tab-1"),
            "an unbound id is taken as the session itself"
        );
        assert_eq!(
            sender_session(None, "sess-tab-1").as_deref(),
            Some("sess-tab-1")
        );
    }

    #[test]
    fn a_message_queued_by_the_routines_run_session_id_does_not_fire_it_again() {
        let temp = TempHome::new();
        let home = temp.codesense_home();
        save(&home, on_role_pending("controller"));
        let routines = discover_routines(&home, &alice()).unwrap();
        let store = store(&home);
        let account = (home.clone(), alice());
        let now = crate::routines::finish::now_ms();
        let first = offer(&account, &routines, &store, &queued(TEAM, 1), now).unwrap();
        launched(&store, &first[0].1.run_id, "sess-run");

        // The run's agent names its tab's session id as the sender: no handle to
        // resolve, yet its run chain is found and the routine does not re-fire.
        let mut event = queued(TEAM, 2);
        event.sender_session = sender_session(None, "sess-run");
        let fired = offer(&account, &routines, &store, &event, now + 1).unwrap();
        assert_eq!(
            fired[0].1.outcome,
            Outcome::Skipped {
                reason: "loop_guard".into()
            }
        );
    }

    #[test]
    fn reach_follows_the_routine_scope() {
        let team = key("controller");
        assert!(in_reach(&team, &queued(TEAM, 1)));
        assert!(!in_reach(&team, &queued("other", 1)));
        let mut personal = key("mine");
        personal.account = Some("1".into());
        assert!(in_reach(&personal, &queued("other", 1)));
        let mut foreign = queued(TEAM, 1);
        foreign.org = "someone-else".into();
        assert!(!in_reach(&team, &foreign));
    }
}
