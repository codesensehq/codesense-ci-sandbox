// SPDX-License-Identifier: MIT
import type { WorktreeConfig } from "@/shared/types/worktree-config";
import type { CommandResult } from "@/tauri/commands";

export type { WorktreeConfig };

/**
 * The phases the Rust setup runner streams events for, mirroring the `phase`
 * field on `src-tauri/src/commands/worktree.rs::emit_line`. Single source of
 * truth so the public event type and the internal raw-payload type can't drift
 * (SW-WT-6 removed the old `"copy"` prepare phase).
 */
export type WorktreePhase = "setup" | "validate" | "cleanup";

/**
 * Streaming progress event emitted while a worktree is being set up.
 * Faithfully mirrors the Rust event payload from
 * `src-tauri/src/commands/worktree.rs::emit_line` (channel
 * `worktree-run-{runId}`).
 *
 * Per-line output rides this event; explicit per-command lifecycle boundaries
 * ride {@link WorktreeStepEvent} (SW-WT-12) on the same channel. Final results
 * still land via the `setupWorktree` promise resolution.
 */
export interface WorktreeProgressEvent {
  phase: WorktreePhase;
  stepIndex: number;
  stream: "stdout" | "stderr";
  line: string;
  tsMs: number;
}

/**
 * SW-WT-12 (S2): a per-command lifecycle boundary from
 * `run_command_sequence` (`emit_step_started`/`emit_step_finished`), on the same
 * `worktree-run-{runId}` channel as {@link WorktreeProgressEvent}. Drives the
 * create UI's per-step checklist: `started` flips a row to running, `finished`
 * flips it to ✓ (`passed`) or ✗ with its `exitCode`.
 */
export interface WorktreeStepEvent {
  kind: "started" | "finished";
  phase: WorktreePhase;
  stepIndex: number;
  /** Present on `started` — the verbatim command, for the checklist row label. */
  command?: string;
  /** Present on `finished`. */
  passed?: boolean;
  exitCode?: number | null;
}

export interface WorktreeSetupOptions {
  repoPath: string;
  /** SS.8: the workspace's `contextRepoPath` — backend resolves the yaml
   *  from `{orgRoot}/_codesense/worktree/{owner}/{repo}.yaml` before
   *  falling back to the legacy `<source>/.codesense/worktree.yaml`. */
  orgRoot: string;
  branchName: string;
  worktreePath: string;
  /** Git start point for a new branch, used verbatim: a dependency branch name or an
   *  override. `null` leaves it to the backend: `defaultStartPoint`, else the repo's
   *  default branch. */
  startPoint: string | null;
  /** The worktree.yaml `default_start_point`, resolved by the backend only when
   *  `startPoint` is `null` (a branch name prefers its cached `origin/` ref). */
  defaultStartPoint?: string | null;
  /** Opaque per-setup id used as the cancel handle and the Tauri event channel suffix. */
  runId: string;
  /** Aborting the signal cancels in-flight subprocesses and triggers rollback. */
  signal?: AbortSignal;
  /** Streamed progress events. Optional — callers that only need the final result can omit. */
  onProgress?: (event: WorktreeProgressEvent) => void;
  /** SW-WT-12 (S2): per-command lifecycle boundaries for the create checklist. */
  onStep?: (event: WorktreeStepEvent) => void;
  /** Awaited after the directory exists and before setup, including reopen. */
  onCreated?: () => void | boolean | Promise<void | boolean>;
  /**
   * SW-WT-9: hooks for the team-sync provenance gate. When present and the
   * repo's worktree.yaml setup is of `team_synced` provenance, `setupWorktree`
   * shows a one-time preview-and-approve gate before running setup. Omit for
   * flows that don't gate (the backend still refuses unapproved team-synced
   * setup as a backstop).
   */
  setupApproval?: WorktreeSetupApprovalHooks;
}

/** SW-WT-9: the data shown in the preview-and-approve gate. */
export interface SetupApprovalRequest {
  /** Ordered setup command-lines that will run, verbatim (AC-2). */
  steps: string[];
  /** SHA-256 of the on-disk config that will execute (AC-3 key / AC-6 check). */
  configHash: string;
}

/** SW-WT-9: gate collaborators supplied by the caller (config-store + UI). */
export interface WorktreeSetupApprovalHooks {
  /** Already-approved for this repo+hash? Skips the prompt when true. */
  isApproved: (repoPath: string, configHash: string) => boolean;
  /** Persist an approval after the user consents. */
  record: (repoPath: string, configHash: string) => Promise<void>;
  /** Show the gate; resolve `true` to run setup, `false` to skip it. */
  prompt: (request: SetupApprovalRequest) => Promise<boolean>;
}

export interface WorktreeSetupResult {
  worktreePath: string;
  /** Per-step results from the Rust setup runner. Empty when no setup ran (rehydrate path). */
  results: CommandResult[];
  /** SW-WT-9: true when the user declined the provenance gate — the worktree
   *  exists and is usable, but teammate-authored setup was not run. */
  setupSkipped?: boolean;
}

export interface WorktreeSetupError extends Error {
  /** Coarse classifier for callers that branch on failure mode. */
  failedStep: "create" | "setup" | "cancelled" | "config" | "orphan";
  /** True when the user can productively retry (transient failure / abort). */
  recoverable: boolean;
  /** Bounded, structured data for a failed command. Absent for legacy/non-command failures. */
  diagnostics?: WorktreeSetupFailureDiagnostics;
}

export interface WorktreeSetupFailureDiagnostics {
  readonly overallExitCode: number;
  readonly failedResult: Readonly<CommandResult>;
  readonly streams: Readonly<{
    stdout: SetupDiagnosticStreamMetadata;
    stderr: SetupDiagnosticStreamMetadata;
  }>;
}

export interface SetupDiagnosticStreamMetadata {
  readonly originalLength: number;
  readonly truncated: boolean;
}
