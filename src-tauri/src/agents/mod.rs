// SPDX-License-Identifier: MIT
//! Agents: named, reusable workers a whole team can see and use.
//!
//! An agent is one small YAML definition under a team's (or the personal
//! space's) `config/agents/<id>/`, git-synced with the context repo and
//! revisioned on every save. It names its runtime (`claude` or `codex`, the
//! built-in CLI adapters), extra args, the MCP servers it may
//! reach, the content paths it starts with, and the runtime's own native
//! permission config. Which provider login runs it is each user's machine-local
//! choice ([`binding`]), never part of the shared definition.
//!
//! - `model`, `paths`, `context`: the definition, its startup paths and its
//!   context sets (with per-repo overrides), with validation.
//! - `keys`: the stable `<org>/<team>/<id>` identity.
//! - `defs`: on-disk layout and revisions.
//! - `roots`: the one seam that finds team and personal dirs on disk.
//! - `discover`, `service`: the API every surface wraps (no `AppHandle`).
//! - `adapter`: how the app drives each runtime's CLI (`claude`, `codex`): app code,
//!   never team-editable.
//! - `builtin`: the read-only starter agents the app ships.
//! - `manifest`: the frozen startup-manifest types the context lifecycle
//!   consumes.
//! - `manifest_resolve`, `content_reader`: the producer of those manifests,
//!   resolving startup paths across content layers into exact files.
//! - `resolve`: an agent turned into exactly what a launch runs (runtime,
//!   account, manifest), with `permissions` (the generated per-launch
//!   settings), `provides` (the `/name`s it starts with) and `launch_check`
//!   (the warnings shown, never enforced).
//! - `launch`: the one prepare step every agent launch calls, and the one
//!   writer of its generated permission files.
//! - `session_events`: agent-done intake, feeding a finished session to
//!   routines and then to every registered consumer.
//!
//! CodeSense shows; it never controls. There is no write authorization, no
//! approval state, and nothing is committed, pushed or announced on a change.

pub(crate) mod adapter;
pub mod binding;
pub mod builtin;
pub mod content_reader;
pub mod context;
pub mod context_edit;
pub mod context_suggestion;
pub mod defs;
pub mod discover;
pub mod events;
pub mod keys;
pub mod launch;
pub(crate) mod launch_budget;
pub mod launch_check;
pub mod manifest;
pub mod manifest_resolve;
pub mod model;
pub mod paths;
pub mod permissions;
pub mod prepare;
pub(crate) mod product_bootstrap;
pub mod provides;
pub(crate) mod remote_launch;
pub mod resolve;
pub mod roots;
pub mod service;
pub mod session_events;
pub(crate) mod workspace_trust;

pub use discover::{AgentScope, DiscoveredAgent};
pub use keys::AgentKey;
pub use manifest::{AgentRevisionRef, StartupManifest};
pub use manifest_resolve::{resolve_manifest, ManifestTarget};
pub use model::{Agent, ValidationError};
pub use paths::StartupPath;
pub use resolve::{preview_agent, resolve_agent, ResolveError, ResolveOptions, ResolvedAgent};
#[cfg(test)]
pub use service::NoReferrers;
pub use service::{AgentError, AgentReferrers, Placement, SaveTarget};

#[cfg(test)]
mod retired_names_guard;
#[cfg(test)]
mod tests;

pub(crate) mod session_fields;
