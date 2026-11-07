// SPDX-License-Identifier: MIT
//! B-layer library module for install infrastructure.
//!
//! Contains the frontmatter cache (in-memory index of story files across
//! all teams) and the story linker (projects per-file symlinks into source
//! repos). No Tauri commands — C-layer wrappers live in `commands::install`.

pub mod agent_account_store;
pub mod attachment_resolver;
pub mod capture;
pub mod frontmatter_cache;
pub mod frontmatter_perf;
pub mod hydration_closure;
pub mod merge_marks;
pub mod org_story_resolver;
pub mod projection_excludes;
pub mod projection_manifest;
pub mod story_linker;
pub mod story_tips;
pub mod team_pins;
pub mod worktree_index;
