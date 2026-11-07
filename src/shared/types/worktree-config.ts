// SPDX-License-Identifier: MIT
// Worktree config artifact schema — mirror of the Rust struct in
// src-tauri/src/commands/worktree.rs. Story 29.2 Task 1.2.
//
// Every field here corresponds 1:1 to the Rust struct. Serde's
// `#[serde(default)]` attributes on the Rust side make optional fields
// omissible at the YAML level; this type marks those fields optional
// with `?` so TypeScript callers don't have to supply them when
// building a config in-memory.

export const WORKTREE_CONFIG_VERSION = 1 as const;

export interface WorktreeConfig {
  /** Literal 1. The Rust reader hard-rejects any other value. */
  version: typeof WORKTREE_CONFIG_VERSION;

  /** Shell commands run after worktree creation. Sequential, abort on first failure. */
  setup: string[];

  /**
   * Optional commands run before worktree removal. Reserved — parsed but not
   * yet executed (no executor); a future worktree-teardown story wires it up.
   * Defaults to `[]`. (SW-WT-11 removed the sibling inert `validate` field.)
   */
  cleanup?: string[];

  /**
   * Optional environment variable overrides applied to every command
   * executed by setup (and future cleanup).
   */
  env?: Record<string, string>;

  /** Default branch prefix for worktree branches. Defaults to `feat/`. */
  branch_prefix?: string;

  /**
   * Default start point for worktree creation when no dependency branch
   * exists. Falls back to `"main"` when absent. F-29.
   */
  default_start_point?: string;

  /**
   * Directory pattern for worktree locations. `{repo}` and `{slug}` are
   * the only supported tokens. Defaults to `../{repo}-wt/{slug}`.
   */
  worktree_dir?: string;

  /**
   * Per-command soft timeout in seconds. Commands that exceed this are
   * killed with SIGKILL and recorded with `exit_code: -2`. Defaults to 600.
   */
  timeout_seconds?: number;
}
