// SPDX-License-Identifier: MIT
//! The trigger-source labels a run's provenance carries (`RunProvenance.trigger_source`), so a run
//! view can say what started a worker. Routines stamp their own label
//! ([`crate::routines::dispatch::TRIGGER_SOURCE`]).

/// A fire-once headless run.
pub const TRIGGER_SOURCE_HEADLESS: &str = "headless";
