// SPDX-License-Identifier: MIT
//! Postures — widen initiation only (SW-WV-E7).
//!
//! The SPEC: a **posture** (night-mode / away) widens *initiation* by
//! `subclass × time-of-day`, and does **nothing else**. A posture NEVER mints
//! authority: it never elevates a subclass's capability tier (E4), never bypasses
//! the four always-on guarantees (E5), and never opens a graduation gate (E6). It
//! only WIDENS which subclasses may *self-initiate* during a declared time-of-day
//! window. This is how routine work self-starts safely at L3.
//!
//! The **Away** posture is the strict case: deny-by-default — no subclass
//! self-initiates unless a window explicitly widens it — with the four E5 gates
//! still enforced and the E3 tier-2 approval windows **paused** (the owner is
//! away, so lazy-consent windows must not run down). Away-exit resumes them.
//!
//! # What this module is — and is NOT
//!
//! Like SW-WV-E4's [`crate::weft::capability_tier`], E5's
//! [`crate::weft::governance`], and E6's [`crate::weft::graduation`], this is a
//! **pure classifier / guard** for the posture invariant — NOT yet wired into the
//! enforcement path (a follow-up). It has no live caller in the self-initiation
//! path today; it makes "may this subclass self-initiate under this posture, right
//! now?" a *named, machine-checkable fact* a future implementer can consult — a
//! reference to be wired into the seams below later, not the live gate. It
//! introduces **no new store table, MCP verb, or Tauri command** — the enforcement
//! seams it is intended to compose already exist:
//!
//! - the four always-on guarantees are E5's [`crate::weft::governance`];
//! - the tier-2 pause/resume is E3's single-sited
//!   [`crate::weft::sqlite::wv_pause_all_t2_windows`] /
//!   [`crate::weft::sqlite::wv_resume_all_t2_windows`] (ARCHITECTURE §4.1 —
//!   the posture *calls* those; it never writes `paused_at` directly). This module
//!   emits the [`T2Directive`] the away-enter / away-exit caller CALLS through, so
//!   the single-sited rule is preserved.
//!
//! # The two hard invariants (SPEC)
//!
//! 1. **Widen only.** [`Posture::initiation_set`] returns a set that is always a
//!    superset of the baseline for that posture; a posture can add a subclass to
//!    the self-initiation set but never remove one that the baseline admits, and
//!    never touches the capability tier.
//! 2. **Never mint authority.** A posture only widens which subclasses may
//!    self-initiate by subclass × time-of-day; it never grants authority. (SW-AV-27
//!    removed the self-initiation gate itself along with the control plane; the
//!    posture record and its `to_wire`/`from_wire` storage remain.)

use crate::weft::spawn::WorkerSubclass;

/// A control-plane posture (SPEC: night-mode / away). A posture is the outer frame
/// that decides the *baseline* self-initiation policy; concrete widening is layered
/// on top by time-of-day windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Normal (attended) posture — the baseline the rest of the trust model assumes.
    /// Self-initiation follows whatever the graduation gates (E6) and the tiers (E4)
    /// already allow; the posture itself neither widens nor narrows.
    Normal,
    /// Away / night-mode posture — **deny-by-default**. No subclass self-initiates
    /// unless a window explicitly widens it. The four always-on guarantees (E5) stay
    /// enforced and the tier-2 approval windows (E3) are paused while away.
    Away,
}

impl Posture {
    /// The wire / storage string.
    pub fn as_str(self) -> &'static str {
        match self {
            Posture::Normal => "normal",
            Posture::Away => "away",
        }
    }

    /// Parse a posture from its wire string; `None` for an unknown value.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim() {
            "normal" => Some(Posture::Normal),
            "away" => Some(Posture::Away),
            _ => None,
        }
    }

    /// Every posture variant. Kept next to the enum so a new variant forces a
    /// compile error at any exhaustive check.
    pub fn all() -> &'static [Posture] {
        &[Posture::Normal, Posture::Away]
    }

    /// Whether this posture denies self-initiation by default. **Away denies by
    /// default** (only an explicit window widens it); Normal does not (it defers to
    /// the graduation gates + tiers already in force). This is the SPEC's
    /// "away = deny-by-default".
    pub fn denies_by_default(self) -> bool {
        matches!(self, Posture::Away)
    }
}

/// A half-open time-of-day window `[start_min, end_min)` in minutes-past-midnight
/// (`0..=1440`), the "time-of-day" half of a posture's `subclass × time-of-day`
/// rule. A window may WRAP past midnight (`start_min > end_min`), which is the
/// common night-mode case (e.g. 22:00 → 06:00). A zero-width window
/// (`start_min == end_min`) matches no minute — a posture that never widens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeOfDayWindow {
    start_min: u16,
    end_min: u16,
}

/// The number of minutes in a day — the modulus every time-of-day value is reduced
/// against so an out-of-range or wrapping value is always well defined.
pub const MINUTES_PER_DAY: u16 = 24 * 60;

impl TimeOfDayWindow {
    /// Build a half-open `[start, end)` window. Both bounds are reduced mod
    /// [`MINUTES_PER_DAY`] so an out-of-range value can never produce a window that
    /// silently matches everything. A `start == end` window is zero-width (matches
    /// no minute); a `start > end` window WRAPS past midnight.
    pub fn new(start_min: u16, end_min: u16) -> Self {
        TimeOfDayWindow {
            start_min: start_min % MINUTES_PER_DAY,
            end_min: end_min % MINUTES_PER_DAY,
        }
    }

    /// The window's start minute (minutes past midnight).
    pub fn start_min(self) -> u16 {
        self.start_min
    }

    /// The window's end minute (minutes past midnight, exclusive).
    pub fn end_min(self) -> u16 {
        self.end_min
    }

    /// Whether `minute_of_day` (reduced mod [`MINUTES_PER_DAY`]) falls inside this
    /// half-open window. A non-wrapping window (`start < end`) contains `[start,
    /// end)`; a wrapping window (`start > end`) contains `[start, 1440) ∪ [0, end)`;
    /// a zero-width window (`start == end`) contains nothing.
    pub fn contains(self, minute_of_day: u16) -> bool {
        let m = minute_of_day % MINUTES_PER_DAY;
        if self.start_min == self.end_min {
            // Zero-width — never widens. (A whole-day window is expressed as a
            // non-zero-width wrap or two adjacent windows, never as start==end.)
            false
        } else if self.start_min < self.end_min {
            m >= self.start_min && m < self.end_min
        } else {
            // Wraps past midnight.
            m >= self.start_min || m < self.end_min
        }
    }
}

/// One posture window: a `(subclass × time-of-day window)` rule that WIDENS the
/// self-initiation set for exactly `subclass` while `now` is inside `window`
/// (SPEC: a posture = subclass × time-of-day). A rule can only ADD a subclass to
/// the initiation set; there is no rule shape that removes one — widen only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostureWindow {
    /// The worker subclass this window widens self-initiation for.
    pub subclass: WorkerSubclass,
    /// The time-of-day window during which the widening applies.
    pub window: TimeOfDayWindow,
}

impl PostureWindow {
    /// Build a posture window for `subclass` over `window`.
    pub fn new(subclass: WorkerSubclass, window: TimeOfDayWindow) -> Self {
        PostureWindow { subclass, window }
    }

    /// Whether this window widens `subclass` at `minute_of_day`.
    pub fn widens(self, subclass: WorkerSubclass, minute_of_day: u16) -> bool {
        self.subclass == subclass && self.window.contains(minute_of_day)
    }
}

/// A posture configuration: the active [`Posture`] plus the set of
/// `(subclass × time-of-day)` windows that WIDEN self-initiation on top of the
/// posture's baseline (SPEC). The windows are additive — the effective
/// self-initiation set at a given minute is the posture's baseline UNION every
/// subclass a window widens at that minute. Nothing here can narrow the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostureConfig {
    posture: Posture,
    windows: Vec<PostureWindow>,
}

impl PostureConfig {
    /// Build a posture config from a posture and its widening windows.
    pub fn new(posture: Posture, windows: Vec<PostureWindow>) -> Self {
        PostureConfig { posture, windows }
    }

    /// A bare posture with no widening windows. Under [`Posture::Away`] this is the
    /// strictest possible policy: deny-by-default with nothing widened.
    pub fn bare(posture: Posture) -> Self {
        PostureConfig {
            posture,
            windows: Vec::new(),
        }
    }

    /// The active posture.
    pub fn posture(&self) -> Posture {
        self.posture
    }

    /// The widening windows.
    pub fn windows(&self) -> &[PostureWindow] {
        &self.windows
    }

    /// The baseline self-initiation policy for `subclass` under this posture, BEFORE
    /// any time-of-day widening. Under [`Posture::Away`] the baseline is
    /// deny-by-default (`false` for every subclass); under [`Posture::Normal`] the
    /// baseline defers to the rest of the trust model (`true` — the posture itself
    /// neither widens nor narrows, so the graduation gates + tiers govern).
    fn baseline_admits(&self, _subclass: WorkerSubclass) -> bool {
        match self.posture {
            Posture::Normal => true,
            // Deny-by-default: away admits nothing at baseline.
            Posture::Away => false,
        }
    }

    /// Whether any widening window widens `subclass` at `minute_of_day`.
    fn widened(&self, subclass: WorkerSubclass, minute_of_day: u16) -> bool {
        self.windows
            .iter()
            .any(|w| w.widens(subclass, minute_of_day))
    }

    /// Whether the POSTURE (baseline ∪ widening windows) admits `subclass` to
    /// self-initiate at `minute_of_day` (SPEC: widen initiation only). This is the
    /// posture's *initiation* verdict alone — it is NOT authority.
    pub fn admits_initiation(&self, subclass: WorkerSubclass, minute_of_day: u16) -> bool {
        // Widen only: the effective set is baseline UNION widened — a window can add
        // a subclass, never remove one the baseline admits.
        self.baseline_admits(subclass) || self.widened(subclass, minute_of_day)
    }

    /// The full set of subclasses the posture admits to self-initiate at
    /// `minute_of_day` — the baseline UNION every widened subclass (SPEC: the
    /// initiation SET the posture widens). Returned in [`WorkerSubclass::all`] order.
    /// This is the machine-readable "which subclasses may self-start now" fact.
    pub fn initiation_set(&self, minute_of_day: u16) -> Vec<WorkerSubclass> {
        WorkerSubclass::all()
            .iter()
            .copied()
            .filter(|s| self.admits_initiation(*s, minute_of_day))
            .collect()
    }

    /// Serialize this config to its storage / wire form:
    /// `{ "posture": <as_str>, "windows": [{ "subclass", "startMin", "endMin" }] }`.
    /// This is the exact shape [`Self::from_wire`] round-trips and the shape the
    /// `wv_set_posture` verb accepts / `wv_read_posture` returns — the posture SOURCE
    /// the controller persists and reconstructs for the dispatch-seam wrap.
    pub fn to_wire(&self) -> serde_json::Value {
        let windows: Vec<serde_json::Value> = self
            .windows
            .iter()
            .map(|w| {
                serde_json::json!({
                    "subclass": w.subclass.as_str(),
                    "startMin": w.window.start_min(),
                    "endMin": w.window.end_min(),
                })
            })
            .collect();
        serde_json::json!({
            "posture": self.posture.as_str(),
            "windows": windows,
        })
    }

    /// Reconstruct a config from its stored / wire form. An unparseable or absent
    /// posture string falls back to [`Posture::Normal`] (the attended baseline that
    /// defers to the D1/D2 gates) — NEVER silently to [`Posture::Away`], which could
    /// strand earned work. A malformed window entry (unknown subclass or non-numeric
    /// minute) is skipped rather than failing the whole read, so one bad row can't
    /// blank the posture. A missing `windows` array reads as no widening windows.
    pub fn from_wire(value: &serde_json::Value) -> Self {
        let posture = value
            .get("posture")
            .and_then(|p| p.as_str())
            .and_then(Posture::from_wire)
            .unwrap_or(Posture::Normal);
        let windows = value
            .get("windows")
            .and_then(|w| w.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| {
                        let subclass = WorkerSubclass::from_wire(w.get("subclass")?.as_str()?)?;
                        let start = u16::try_from(w.get("startMin")?.as_u64()?).ok()?;
                        let end = u16::try_from(w.get("endMin")?.as_u64()?).ok()?;
                        Some(PostureWindow::new(
                            subclass,
                            TimeOfDayWindow::new(start, end),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        PostureConfig::new(posture, windows)
    }

    /// The tier-2-window directive this posture implies for a posture *transition*
    /// (SW-WV-E3 / ARCHITECTURE §4.1). Entering [`Posture::Away`] pauses the tier-2
    /// approval windows (the owner is away; lazy-consent windows must not run down);
    /// entering [`Posture::Normal`] resumes them. The directive names the E3
    /// single-sited function the transition caller CALLS — this module never writes
    /// `paused_at` itself.
    pub fn t2_directive_on_enter(&self) -> T2Directive {
        match self.posture {
            Posture::Away => T2Directive::PauseAll,
            Posture::Normal => T2Directive::ResumeAll,
        }
    }
}

/// The tier-2 approval-window directive a posture transition implies (SW-WV-E3,
/// ARCHITECTURE §4.1). The posture is pure — it emits the directive; the caller
/// CALLS the single-sited E3 function so `paused_at` / `pause_accumulated_s` stay
/// owned by one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum T2Directive {
    /// Pause every live tier-2 window — call [`crate::weft::wv_pause_all_t2_windows`].
    /// This is what away-enter implies: the owner is away, so lazy-consent must not
    /// run down.
    PauseAll,
    /// Resume every paused tier-2 window — call
    /// [`crate::weft::wv_resume_all_t2_windows`]. This is what away-exit implies: the
    /// owner is back; the windows resume with the full budget of present time.
    ResumeAll,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's own production (non-test) source — everything before the first
    /// `#[test]` (module docs + the pure posture functions D4 wires between
    /// `wv_claim_queue` and `wv_spawn_worker`). Mirrors the grep-target shape the shipped
    /// A1/A2/A6 conformance guards use (`default_agent_packs.rs`,
    /// `weft_controller_run.rs`), so a pattern named inside a test assertion string never
    /// trips the scan — only the pre-test half is read.
    fn postures_module_source() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/weft/postures.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read the postures module source: {e}"));
        match src.find("#[test]") {
            Some(cut) => src[..cut].to_string(),
            None => src,
        }
    }

    // NO-SECOND-ORCHESTRATOR on the posture path: the away-enter pause must stay a
    // synchronous call through the sanctioned `wv_pause_all_t2_windows` verb, never a
    // background timer. Extends the SHIPPED per-module scheduler-scan pattern over the D4
    // posture call site so a posture regression cannot smuggle in a standalone scheduler
    // primitive.
    #[test]
    fn posture_module_adds_no_core_dispatcher_or_second_orchestrator() {
        let src = postures_module_source();
        for forbidden in [
            "tokio::spawn",
            "interval(",
            "cron",
            "std::thread::spawn",
            "loop {",
        ] {
            assert!(
                !src.contains(forbidden),
                "the posture module must not encode a second orchestrator — found {forbidden:?}; \
                 the away-enter pause is a synchronous wv_pause_all_t2_windows call, not a timer"
            );
        }
    }

    // ------------------------------------------------------------------
    // Posture / window basics.
    // ------------------------------------------------------------------

    #[test]
    fn posture_wire_strings_round_trip() {
        for posture in Posture::all() {
            assert_eq!(Posture::from_wire(posture.as_str()), Some(*posture));
        }
        assert_eq!(Posture::from_wire("  away "), Some(Posture::Away));
        assert_eq!(Posture::from_wire("bogus"), None);
    }

    #[test]
    fn away_denies_by_default_normal_does_not() {
        assert!(
            Posture::Away.denies_by_default(),
            "away is deny-by-default (SPEC)"
        );
        assert!(
            !Posture::Normal.denies_by_default(),
            "normal defers to the gates + tiers already in force"
        );
    }

    // ------------------------------------------------------------------
    // AC-7: the posture SOURCE wire form round-trips; a corrupt / absent
    // stored value fails open to Normal (never silently to Away).
    // ------------------------------------------------------------------

    #[test]
    fn posture_config_wire_form_round_trips() {
        let night = TimeOfDayWindow::new(22 * 60, 6 * 60);
        let cfg = PostureConfig::new(
            Posture::Away,
            vec![
                PostureWindow::new(WorkerSubclass::Review, night),
                PostureWindow::new(
                    WorkerSubclass::Research,
                    TimeOfDayWindow::new(9 * 60, 17 * 60),
                ),
            ],
        );
        let wire = cfg.to_wire();
        assert_eq!(wire["posture"], "away");
        assert_eq!(wire["windows"][0]["subclass"], "review");
        assert_eq!(wire["windows"][0]["startMin"], 22 * 60);
        assert_eq!(wire["windows"][0]["endMin"], 6 * 60);
        assert_eq!(
            PostureConfig::from_wire(&wire),
            cfg,
            "wire round-trips exactly"
        );
    }

    #[test]
    fn a_bare_normal_config_round_trips_to_the_default() {
        let cfg = PostureConfig::bare(Posture::Normal);
        let wire = cfg.to_wire();
        assert_eq!(wire["posture"], "normal");
        assert!(wire["windows"].as_array().unwrap().is_empty());
        assert_eq!(PostureConfig::from_wire(&wire), cfg);
    }

    #[test]
    fn a_corrupt_or_absent_posture_reads_as_normal_never_away() {
        // An unknown posture string, a missing posture, and an empty object all
        // resolve to Normal (fail-open to the attended baseline that still enforces
        // D1/D2) — never Away, which could strand earned work.
        for value in [
            serde_json::json!({ "posture": "bogus", "windows": [] }),
            serde_json::json!({ "windows": [] }),
            serde_json::json!({}),
            serde_json::json!({ "posture": "" }),
        ] {
            let cfg = PostureConfig::from_wire(&value);
            assert_eq!(
                cfg.posture(),
                Posture::Normal,
                "a corrupt / absent posture falls back to Normal, never Away: {value}"
            );
            assert!(cfg.windows().is_empty());
        }
    }

    #[test]
    fn a_malformed_window_entry_is_skipped_not_fatal() {
        // One bad window (unknown subclass) is dropped; the valid entry survives, so a
        // single corrupt row can never blank the whole posture.
        let value = serde_json::json!({
            "posture": "away",
            "windows": [
                { "subclass": "not-a-subclass", "startMin": 60, "endMin": 120 },
                { "subclass": "review", "startMin": 22 * 60, "endMin": 6 * 60 },
            ],
        });
        let cfg = PostureConfig::from_wire(&value);
        assert_eq!(cfg.posture(), Posture::Away);
        assert_eq!(cfg.windows().len(), 1, "the malformed entry is skipped");
        assert_eq!(cfg.windows()[0].subclass, WorkerSubclass::Review);
    }

    #[test]
    fn a_non_wrapping_window_is_half_open() {
        let w = TimeOfDayWindow::new(9 * 60, 17 * 60); // 09:00–17:00
        assert!(!w.contains(9 * 60 - 1), "08:59 is before the window");
        assert!(w.contains(9 * 60), "09:00 is the inclusive start");
        assert!(w.contains(12 * 60), "noon is inside");
        assert!(!w.contains(17 * 60), "17:00 is the exclusive end");
        assert!(!w.contains(17 * 60 + 1), "17:01 is after");
    }

    #[test]
    fn a_wrapping_window_spans_midnight() {
        let w = TimeOfDayWindow::new(22 * 60, 6 * 60); // 22:00 → 06:00 (night mode)
        assert!(w.contains(23 * 60), "23:00 is inside the night window");
        assert!(w.contains(0), "midnight is inside");
        assert!(w.contains(5 * 60), "05:00 is inside");
        assert!(!w.contains(6 * 60), "06:00 is the exclusive end");
        assert!(!w.contains(12 * 60), "noon is outside the night window");
    }

    #[test]
    fn a_zero_width_window_matches_nothing() {
        let w = TimeOfDayWindow::new(3 * 60, 3 * 60);
        for m in [0, 3 * 60, 12 * 60, MINUTES_PER_DAY - 1] {
            assert!(!w.contains(m), "a zero-width window never widens ({m})");
        }
    }

    #[test]
    fn out_of_range_minutes_are_reduced_mod_day() {
        let w = TimeOfDayWindow::new(0, 60); // 00:00–01:00
                                             // 1440 == 00:00 of the next day; must reduce into the window.
        assert!(w.contains(MINUTES_PER_DAY));
        assert!(w.contains(MINUTES_PER_DAY + 30));
        // A start beyond the day is reduced too — never a silently all-day window.
        let padded = TimeOfDayWindow::new(MINUTES_PER_DAY + 9 * 60, MINUTES_PER_DAY + 10 * 60);
        assert_eq!(padded.start_min(), 9 * 60);
        assert_eq!(padded.end_min(), 10 * 60);
    }

    // ------------------------------------------------------------------
    // AC-6: a posture is (subclass × time-of-day) — widen only in-window.
    // ------------------------------------------------------------------

    #[test]
    fn a_window_widens_its_subclass_only_inside_its_window() {
        let night = TimeOfDayWindow::new(22 * 60, 6 * 60);
        let cfg = PostureConfig::new(
            Posture::Away,
            vec![PostureWindow::new(WorkerSubclass::Review, night)],
        );
        // Review is widened at 23:00 (inside), refused at noon (outside).
        assert!(cfg.admits_initiation(WorkerSubclass::Review, 23 * 60));
        assert!(!cfg.admits_initiation(WorkerSubclass::Review, 12 * 60));
        // A DIFFERENT subclass is never widened by a review window.
        assert!(!cfg.admits_initiation(WorkerSubclass::Impl, 23 * 60));
    }

    // ------------------------------------------------------------------
    // AC-1: a posture WIDENS the initiation set only — never removes a
    // baseline-admitted subclass, never narrows.
    // ------------------------------------------------------------------

    #[test]
    fn a_posture_only_widens_never_narrows_the_initiation_set() {
        // Under Away (deny-by-default), the baseline set is empty; adding windows can
        // only GROW the set, never shrink it.
        let base = PostureConfig::bare(Posture::Away);
        assert!(
            base.initiation_set(23 * 60).is_empty(),
            "away with no window admits no subclass"
        );

        let night = TimeOfDayWindow::new(22 * 60, 6 * 60);
        let widened = PostureConfig::new(
            Posture::Away,
            vec![
                PostureWindow::new(WorkerSubclass::Review, night),
                PostureWindow::new(WorkerSubclass::Research, night),
            ],
        );
        let set = widened.initiation_set(23 * 60);
        assert!(set.contains(&WorkerSubclass::Review));
        assert!(set.contains(&WorkerSubclass::Research));
        // The widened set is a strict SUPERSET of the bare baseline.
        for s in base.initiation_set(23 * 60) {
            assert!(set.contains(&s), "widening never drops a baseline subclass");
        }
        assert!(set.len() > base.initiation_set(23 * 60).len());
    }

    #[test]
    fn normal_posture_admits_the_full_baseline_and_windows_cannot_narrow_it() {
        // Under Normal the baseline admits every subclass (the posture neither widens
        // nor narrows — the gates + tiers govern). A window is a redundant no-op; it
        // can never REMOVE a subclass.
        let cfg = PostureConfig::new(
            Posture::Normal,
            vec![PostureWindow::new(
                WorkerSubclass::Review,
                TimeOfDayWindow::new(9 * 60, 17 * 60),
            )],
        );
        let set = cfg.initiation_set(3 * 60); // 03:00 — outside the review window
        for subclass in WorkerSubclass::all() {
            assert!(
                set.contains(subclass),
                "normal never narrows the baseline — {subclass:?} still admitted"
            );
        }
    }

    // ------------------------------------------------------------------
    // AC-3: away denies by default; the window is the ONLY way in.
    // ------------------------------------------------------------------

    #[test]
    fn away_denies_every_subclass_with_no_window() {
        let cfg = PostureConfig::bare(Posture::Away);
        for subclass in WorkerSubclass::all() {
            assert!(
                !cfg.admits_initiation(*subclass, 23 * 60),
                "away is deny-by-default — {subclass:?} may not self-initiate"
            );
        }
    }

    #[test]
    fn away_admits_only_the_widened_subclass_in_window() {
        let night = TimeOfDayWindow::new(22 * 60, 6 * 60);
        let cfg = PostureConfig::new(
            Posture::Away,
            vec![PostureWindow::new(WorkerSubclass::Review, night)],
        );
        assert_eq!(
            cfg.initiation_set(23 * 60),
            vec![WorkerSubclass::Review],
            "away admits ONLY the widened subclass, and only in-window"
        );
        // Outside the window even the widened subclass is denied.
        assert!(cfg.initiation_set(12 * 60).is_empty());
    }

    // ------------------------------------------------------------------
    // AC-4: tier-2 windows paused under away; resumed on return.
    // ------------------------------------------------------------------

    #[test]
    fn away_enter_directs_a_tier2_pause_normal_enter_a_resume() {
        assert_eq!(
            PostureConfig::bare(Posture::Away).t2_directive_on_enter(),
            T2Directive::PauseAll,
            "entering away pauses the tier-2 windows (owner is away)"
        );
        assert_eq!(
            PostureConfig::bare(Posture::Normal).t2_directive_on_enter(),
            T2Directive::ResumeAll,
            "returning to normal resumes the tier-2 windows"
        );
    }
}
