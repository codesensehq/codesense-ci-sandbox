// SPDX-License-Identifier: MIT
//! The sessions waiting for mail: one place per session, held by the inbox waiter of
//! the agent process its host named, its committed invocation (the host admits a waiter
//! only from that process, see `ActivityOwner::inbox_admission`), so nothing here ranks
//! processes: a newer
//! invocation's waiter takes the place, and the invocation's own waiters take it as
//! their arm allows. A host wakes its waiters through its own [`WaiterSignal`], so this
//! core is synchronous.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::activity::peer;

/// How long one inbox wait holds before answering that nothing came; the waiter asks
/// again. Below the waiter hook's own timeout.
pub const INBOX_WAIT_HOLD: Duration = Duration::from_secs(20 * 60);

/// How long after its waiter woke it an agent still reads as one new mail wakes: it runs
/// the turn the notice started and re-arms at its next tool call or turn end.
pub const ARMED_GRACE: Duration = Duration::from_secs(25 * 60);

/// How long after a waiter armed or ended a quiet wait its agent still reads as one new
/// mail wakes: a live waiter asks again within its pause, so past this it has none (its
/// agent exited, or the waiter ended). A waiter that asks before its host committed the
/// agent's invocation waits this long for the commit.
pub const REASK_GRACE: Duration = Duration::from_secs(2 * 60);

/// How a waiter takes its session's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitArm {
    /// Armed at a boot or a turn end: the session's waiter from now on, replacing any
    /// before it.
    Replace,
    /// Armed after a tool call (to re-arm a session whose waiter woke it mid-turn), or a
    /// waiter asking again: one already waiting keeps its place, and this one ends.
    IfAbsent,
}

impl WaitArm {
    /// How a waiter armed at its CLI's hook `event` takes its place: one of
    /// `replace_events` (a boot or a turn end) replaces; anything else (a tool call, or a
    /// waiter's own re-ask) defers to a waiter already waiting.
    pub fn from_hook(event: &str, replace_events: &[&str]) -> Self {
        if replace_events.contains(&event) {
            Self::Replace
        } else {
            Self::IfAbsent
        }
    }
}

/// How a host wakes one of its waiters. The registry calls it with its lock held, so it
/// must not block or call back into the registry.
pub trait WaiterSignal: Send + Sync {
    /// Mail was stored for its session. The signal must keep a permit, so mail stored
    /// between the waiter's look and its wait is not missed.
    fn mail(&self);
    /// A newer waiter took its place, or its session was torn down: it ends without
    /// waking the agent.
    fn replaced(&self);
}

/// A running process, by pid and start time: a reused pid never matches it.
pub type Owner = peer::PeerProcess;

/// The sessions waiting for mail: one place per session.
#[derive(Debug)]
pub struct InboxWaiters<S> {
    places: Mutex<HashMap<String, Place<S>>>,
    next: AtomicU64,
}

impl<S> Default for InboxWaiters<S> {
    fn default() -> Self {
        Self {
            places: Mutex::default(),
            next: AtomicU64::default(),
        }
    }
}

/// A session's place for its inbox waiter.
#[derive(Debug)]
struct Place<S> {
    /// The waiter waiting now.
    waiting: Option<WaiterSlot<S>>,
    /// The agent process (the session's committed invocation) whose waiters hold it.
    owner: Owner,
    /// Until when the agent reads as one new mail wakes after its waiter was last seen,
    /// armed or ending a wait (see [`ARMED_GRACE`], [`REASK_GRACE`]).
    seen_until: Instant,
}

impl<S> Place<S> {
    /// Whether new mail wakes its agent: a waiter waits, or one re-arms within its grace,
    /// and the agent still runs.
    fn armed(&self, now: Instant) -> bool {
        (self.waiting.is_some() || self.seen_until > now) && peer::runs(self.owner)
    }
}

#[derive(Debug)]
struct WaiterSlot<S> {
    id: u64,
    signal: Arc<S>,
}

/// `mutex`'s guard, poisoned or not: a registry left by a panic still serves every
/// waiter, rather than ending each one for good.
pub(crate) fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl<S: WaiterSignal> InboxWaiters<S> {
    /// A place for a waiter of `session` run by `owner`, its host's committed agent
    /// process, woken through `signal`; `None` when it defers to a waiter of the same
    /// process already waiting, or `owner` has exited. A waiter of another process (a
    /// newer invocation) takes the place however it arms.
    pub fn register(
        &self,
        session: &str,
        arm: WaitArm,
        owner: Owner,
        signal: Arc<S>,
    ) -> Option<Registration<'_, S>> {
        if !peer::runs(owner) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut places = locked(&self.places);
        let defers = places
            .get(session)
            .is_some_and(|place| place.waiting.is_some() && place.owner == owner);
        if arm == WaitArm::IfAbsent && defers {
            return None;
        }
        let now = Instant::now();
        // A place whose agent exited no longer reads armed: it goes once nothing waits
        // in it.
        places.retain(|_, place| place.waiting.is_some() || place.armed(now));
        let slot = WaiterSlot { id, signal };
        let old = match places.get_mut(session) {
            Some(place) => {
                place.owner = owner;
                place.seen_until = now + REASK_GRACE;
                place.waiting.replace(slot)
            }
            None => {
                places.insert(
                    session.to_string(),
                    Place {
                        waiting: Some(slot),
                        owner,
                        seen_until: now + REASK_GRACE,
                    },
                );
                None
            }
        };
        if let Some(old) = old {
            old.signal.replaced();
        }
        Some(Registration {
            waiters: self,
            session: session.to_string(),
            id,
            woke: false,
        })
    }

    /// Mail was held for `session`: wake its waiter, if one is waiting.
    pub fn mail_stored(&self, session: &str) {
        if let Some(slot) = locked(&self.places)
            .get(session)
            .and_then(|place| place.waiting.as_ref())
        {
            slot.signal.mail();
        }
    }

    /// `session` was torn down: its waiter ends without waking it.
    pub fn forget(&self, session: &str) {
        if let Some(old) = locked(&self.places)
            .remove(session)
            .and_then(|place| place.waiting)
        {
            old.signal.replaced();
        }
    }
}

impl<S> InboxWaiters<S> {
    /// Whether `session`'s agent has an inbox waiter: new mail wakes it, whether a waiter
    /// is waiting this instant or re-arms at its next step. An agent that exited has none.
    pub fn armed(&self, session: &str) -> bool {
        locked(&self.places)
            .get(session)
            .is_some_and(|place| place.armed(Instant::now()))
    }

    /// Every session with a place: one waiting, or seen within its grace.
    pub fn sessions(&self) -> Vec<String> {
        locked(&self.places).keys().cloned().collect()
    }

    /// Whether a waiter is waiting in `session`'s place this instant.
    pub fn is_waiting(&self, session: &str) -> bool {
        locked(&self.places)
            .get(session)
            .is_some_and(|place| place.waiting.is_some())
    }

    /// Until when `session`'s agent reads as one new mail wakes, once no waiter waits.
    pub fn seen_until(&self, session: &str) -> Option<Instant> {
        locked(&self.places)
            .get(session)
            .map(|place| place.seen_until)
    }

    /// The waiter `id` of `session` ends, `grace` before its agent stops reading as one
    /// new mail wakes. Only one still holding its place was seen waiting; a replaced
    /// one's successor already was, and a released session's must not read as one new
    /// mail wakes.
    fn release(&self, session: &str, id: u64, grace: Duration) {
        if let Some(place) = locked(&self.places).get_mut(session) {
            if place.waiting.as_ref().is_some_and(|slot| slot.id == id) {
                place.waiting = None;
                place.seen_until = Instant::now() + grace;
            }
        }
    }
}

/// A waiter's place in [`InboxWaiters`], given up however its wait ends (a caller that
/// went away drops it mid-wait).
pub struct Registration<'a, S> {
    waiters: &'a InboxWaiters<S>,
    session: String,
    id: u64,
    /// Its wait woke the agent, which re-arms only once that turn reaches a tool call or
    /// its end.
    pub woke: bool,
}

impl<S> Drop for Registration<'_, S> {
    fn drop(&mut self) {
        let grace = if self.woke { ARMED_GRACE } else { REASK_GRACE };
        self.waiters.release(&self.session, self.id, grace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Count {
        mail: AtomicUsize,
        replaced: AtomicUsize,
    }

    impl WaiterSignal for Count {
        fn mail(&self) {
            self.mail.fetch_add(1, Ordering::Relaxed);
        }

        fn replaced(&self) {
            self.replaced.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Count {
        fn mails(&self) -> usize {
            self.mail.load(Ordering::Relaxed)
        }

        fn replacements(&self) -> usize {
            self.replaced.load(Ordering::Relaxed)
        }
    }

    fn me() -> Owner {
        peer::inspect(std::process::id()).expect("this process can be inspected")
    }

    /// A live child process, and its owner identity.
    fn child() -> (std::process::Child, Owner) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let owner = peer::inspect(child.id()).expect("the child can be inspected");
        (child, owner)
    }

    #[test]
    fn a_boot_or_turn_end_replaces_and_every_other_event_defers() {
        let replace = ["SessionStart", "Stop"];
        assert_eq!(WaitArm::from_hook("Stop", &replace), WaitArm::Replace);
        assert_eq!(
            WaitArm::from_hook("PostToolUse", &replace),
            WaitArm::IfAbsent
        );
        assert_eq!(WaitArm::from_hook("", &replace), WaitArm::IfAbsent);
    }

    #[test]
    fn mail_wakes_the_waiter_holding_the_place_and_a_replacing_arm_ends_it() {
        let waiters = InboxWaiters::default();
        let first = Arc::new(Count::default());
        let held = waiters
            .register("S", WaitArm::Replace, me(), first.clone())
            .unwrap();
        assert!(waiters.is_waiting("S") && waiters.armed("S"));
        waiters.mail_stored("S");
        waiters.mail_stored("other");
        assert_eq!(first.mails(), 1);

        let second = Arc::new(Count::default());
        let newer = waiters
            .register("S", WaitArm::Replace, me(), second.clone())
            .unwrap();
        assert_eq!(first.replacements(), 1);
        // The replaced waiter's end leaves the newer one in place.
        drop(held);
        assert!(waiters.is_waiting("S"));
        waiters.mail_stored("S");
        assert_eq!((first.mails(), second.mails()), (1, 1));
        drop(newer);
        assert!(!waiters.is_waiting("S") && waiters.armed("S"));
    }

    #[test]
    fn a_re_arm_defers_to_a_waiter_of_its_own_process_already_waiting() {
        let waiters = InboxWaiters::default();
        let _held = waiters
            .register("S", WaitArm::Replace, me(), Arc::new(Count::default()))
            .unwrap();
        assert!(waiters
            .register("S", WaitArm::IfAbsent, me(), Arc::new(Count::default()))
            .is_none());
    }

    #[test]
    fn a_newer_invocations_waiter_takes_the_place_however_it_arms() {
        let waiters = InboxWaiters::default();
        let old = Arc::new(Count::default());
        let _held = waiters
            .register("S", WaitArm::Replace, me(), old.clone())
            .unwrap();
        let (mut process, restarted) = child();
        let _newer = waiters
            .register(
                "S",
                WaitArm::IfAbsent,
                restarted,
                Arc::new(Count::default()),
            )
            .unwrap();
        assert_eq!(old.replacements(), 1);
        assert!(waiters.is_waiting("S"));
        let _ = process.kill();
        let _ = process.wait();
    }

    #[test]
    fn an_exited_agent_takes_no_place_and_its_place_reads_unarmed() {
        let waiters = InboxWaiters::default();
        let (mut process, agent) = child();
        let held = waiters
            .register("S", WaitArm::Replace, agent, Arc::new(Count::default()))
            .unwrap();
        drop(held);
        assert!(waiters.armed("S"), "within its grace");
        let _ = process.kill();
        let _ = process.wait();
        assert!(!waiters.armed("S"), "its agent exited");
        assert!(waiters
            .register("S", WaitArm::Replace, agent, Arc::new(Count::default()))
            .is_none());
    }

    #[test]
    fn only_a_wake_keeps_the_agent_armed_through_its_turn() {
        let waiters = InboxWaiters::default();
        let register = || {
            waiters
                .register("S", WaitArm::Replace, me(), Arc::new(Count::default()))
                .unwrap()
        };
        drop(register());
        let left = waiters.seen_until("S").unwrap() - Instant::now();
        assert!(left <= REASK_GRACE);
        let mut woke = register();
        woke.woke = true;
        drop(woke);
        assert!(waiters.seen_until("S").unwrap() - Instant::now() > REASK_GRACE);
    }

    #[test]
    fn a_torn_down_session_ends_its_waiter_and_reads_unarmed() {
        let waiters = InboxWaiters::default();
        let signal = Arc::new(Count::default());
        let held = waiters
            .register("S", WaitArm::Replace, me(), signal.clone())
            .unwrap();
        waiters.forget("S");
        assert_eq!(signal.replacements(), 1);
        drop(held);
        assert!(!waiters.armed("S"));
        assert!(waiters.sessions().is_empty());
    }
}
