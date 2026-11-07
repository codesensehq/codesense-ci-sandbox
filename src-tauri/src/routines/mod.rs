// SPDX-License-Identifier: MIT
//! Routines: standing instructions ("every weekday at 9:00 have Triage sweep
//! the channel and post what it found").
//!
//! A routine is split across two homes on purpose:
//!
//! - **Definition** (`model`, `defs`, `layout`): a small YAML file under the
//!   agent that runs it, `config/agents/<agent>/routines/<id>/` in the team's
//!   (or personal) space, git-synced with the context repo so every teammate
//!   can see it. Each save also writes an immutable revision snapshot next to
//!   it. `migrate` brings the pre-SW-AGR-1 `config/routines/<id>/` forward and
//!   `lifecycle` archives and restores an agent's routines with it.
//! - **Runtime state** (`state`): enabled flag, trigger cursors and the run log
//!   live in the machine-local coordinator SQLite. Pausing is a local fact, never
//!   a git edit. That database is shared by every account on the Mac, so it is
//!   keyed by the rendered [`RoutineKey`]: `<org>/<team>/<id>` for a team
//!   routine, and `_personal/<account id>/<org>/_scratch/<id>` for a personal one
//!   (filed in that org's scratch space).
//!
//! Run results stay with the routine (its run records and last result text).
//! Routines never deliver results anywhere: the agent running one messages or
//! files whatever it needs itself, so a `deliver` key is refused at save.
//!
//! Because the definition syncs to everyone, firing must not by default: a
//! shared job (`runs_on: one_mac`) is armed only on the Mac its `armed_host`
//! names, and one naming no Mac runs nowhere (`host`, `is_armed_here`). A team
//! routine may instead be a recipe each Mac runs (`runs_on: each_mac`): every
//! Mac whose user turned it on runs its own instance, with that Mac's own
//! cursors, runs and results, since all of those live in its coordinator. The
//! per-Mac switch is local state (`state::enabled_here`), never a git edit.
//! Anyone may move a routine, switch its mode, and change it.
//! `schedule` evaluates cron in a time zone and `tick` is the scheduler pass
//! that turns due occurrences into run rows; `scheduler` is the routines' own
//! thread that runs it. `event` turns a finished agent
//! into run rows for routines with an `agent_done` trigger, and `inbox_idle`
//! fires routines with an `inbox_idle` trigger for agents idle with unread mail.
//!
//! A fire becomes a run in three steps, each leaving the run row saying what
//! happened: `admit` applies the routine's policy (paused, overlap, target) and
//! queues admitted runs for the shared claim loop; `dispatch` launches the
//! agent; `finish` is the one writer of a run's ending, whichever way it ends.
//! A routine names an agent (`model::AgentRef`); `agents` reports which
//! routines use an agent and how its agent reads in the tab.
//! `wake` delivers a `mode: wake` fire into its agent's live session instead
//! of starting another (SW-AGR-2).
//! `discover` scans every locally-synced team plus each synced org's scratch
//! space, and
//! `service` is the single API the MCP tools and the UI wrap. Every service fn is
//! blocking and `AppHandle`-free; async callers run it inside `await_blocking`.

pub mod admit;
pub mod agents;
pub mod chain;
pub mod command;
pub mod defs;
pub mod discover;
pub mod dispatch;
pub mod event;
pub mod events;
pub mod finish;
pub mod host;
pub mod inbox_idle;
pub mod layout;
pub mod lifecycle;
pub(crate) mod machine_arm;
pub(crate) mod machine_arm_app;
pub mod metadata;
#[cfg(test)]
mod metadata_seams;
pub mod migrate;
pub mod model;
pub mod results;
pub mod role_pending;
pub mod runs;
pub mod schedule;
pub mod scheduler;
pub mod service;
pub mod state;
pub mod target;
pub mod template;
pub mod tick;
pub mod wake;
pub mod wake_app;
mod workspace;

pub use discover::{
    discover_routines, discover_routines_in_view, migrate_all, DiscoveredRoutine, RoutineScope,
};
pub use host::{is_armed_here, local_host_id};
pub(crate) use model::parse_max_runtime;
pub use model::{
    AgentRef, Author, AuthorKind, EventSpec, MissedPolicy, OverlapPolicy, Policy, RoutineDef,
    RoutineKey, RoutineMode, RoutineTarget, RoutineTrigger, ScheduleSpec, ValidationError,
    Variable, VariableType, Where,
};
pub use service::{PersonalOwner, Placement, RoutineError, SaveTarget};

#[cfg(test)]
mod agent_home_seams;
#[cfg(test)]
mod agent_seams;
#[cfg(test)]
mod boundary_guard;
#[cfg(test)]
mod each_mac_seams;
#[cfg(test)]
mod event_seams;
#[cfg(test)]
mod inbox_idle_seams;
#[cfg(test)]
mod machine_arm_seams;
#[cfg(test)]
mod read_seams;
#[cfg(test)]
mod run_seams;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wake_seams;

#[cfg(test)]
mod command_seams;
