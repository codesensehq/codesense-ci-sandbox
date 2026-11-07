// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";
import {
  cancelWorktreeRun,
  evaluateSetupProvenance,
  fetchAndCreateWorktree,
  getWorktreeConfig,
  listWorktrees,
  removeWorktree,
  runWorktreeSetup,
} from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { safeUnlisten } from "../tauri-listeners";
import { branchRefMatches } from "./resolve";
import type {
  WorktreePhase,
  WorktreeSetupError,
  WorktreeSetupFailureDiagnostics,
  WorktreeSetupOptions,
  WorktreeSetupResult,
} from "./types";
import type { CommandResult } from "@/tauri/commands";

export const MAX_SETUP_DIAGNOSTIC_STREAM_CHARS = 64 * 1024;
const SETUP_DIAGNOSTIC_TRUNCATION_MARKER = "\n… output truncated …\n";

function safeSliceStart(value: string, start: number): string {
  let adjusted = start;
  if (adjusted > 0 && adjusted < value.length && /[\uDC00-\uDFFF]/.test(value.charAt(adjusted))) {
    adjusted += 1;
  }
  return value.slice(adjusted);
}

function safeSliceEnd(value: string, end: number): string {
  let adjusted = end;
  if (adjusted > 0 && /[\uD800-\uDBFF]/.test(value.charAt(adjusted - 1))) adjusted -= 1;
  return value.slice(0, adjusted);
}

function boundDiagnosticStream(value: string): {
  text: string;
  originalLength: number;
  truncated: boolean;
} {
  if (value.length <= MAX_SETUP_DIAGNOSTIC_STREAM_CHARS) {
    return { text: value, originalLength: value.length, truncated: false };
  }
  const contentBudget =
    MAX_SETUP_DIAGNOSTIC_STREAM_CHARS - SETUP_DIAGNOSTIC_TRUNCATION_MARKER.length;
  const headLength = Math.floor(contentBudget / 4);
  const tailLength = contentBudget - headLength;
  return {
    text:
      safeSliceEnd(value, headLength) +
      SETUP_DIAGNOSTIC_TRUNCATION_MARKER +
      safeSliceStart(value, value.length - tailLength),
    originalLength: value.length,
    truncated: true,
  };
}

export function buildSetupFailureDiagnostics(
  results: CommandResult[],
  overallExitCode: number,
): WorktreeSetupFailureDiagnostics | undefined {
  const failedResult = results.find((result) => !result.passed);
  if (!failedResult) return undefined;
  const stdout = boundDiagnosticStream(failedResult.stdout);
  const stderr = boundDiagnosticStream(failedResult.stderr);
  return {
    overallExitCode,
    failedResult: { ...failedResult, stdout: stdout.text, stderr: stderr.text },
    streams: {
      stdout: { originalLength: stdout.originalLength, truncated: stdout.truncated },
      stderr: { originalLength: stderr.originalLength, truncated: stderr.truncated },
    },
  };
}

interface RawEventPayload {
  stream: "stdout" | "stderr";
  line: string;
  phase: WorktreePhase;
  step_index: number;
  ts_ms: number;
}

/** SW-WT-12 (S2): step-lifecycle payload on the `worktree-run-{runId}` channel,
 *  discriminated from a line event by the `event` field (line events omit it). */
interface RawStepPayload {
  event: "step-started" | "step-finished";
  phase: WorktreePhase;
  step_index: number;
  command?: string;
  passed?: boolean;
  exit_code?: number | null;
}

type RawWorktreeRunPayload = RawEventPayload | RawStepPayload;

function isStepPayload(p: RawWorktreeRunPayload): p is RawStepPayload {
  return "event" in p && (p.event === "step-started" || p.event === "step-finished");
}

/** RF-perf-14 (decouple) — Phase A: `git worktree add` only (the fast phase the
 *  open path can complete on). Everything the interactive open needs to become
 *  "ready" — the on-disk worktree dir — exists once this resolves. */
export interface CreateWorktreeOptions {
  repoPath: string;
  branchName: string;
  worktreePath: string;
  /** An explicit base for a new branch, used verbatim. `null` = resolved by the backend
   *  for a new branch: `defaultStartPoint`, else the repo's default branch. */
  startPoint: string | null;
  /** The worktree.yaml `default_start_point`, resolved by the backend the way the MCP open
   *  resolves it (a branch name prefers its cached `origin/` ref). Used only when
   *  `startPoint` is `null`. */
  defaultStartPoint?: string | null;
  signal?: AbortSignal;
}

export interface CreateWorktreeResult {
  worktreePath: string;
  /** True when git already had a matching worktree at the path (rehydrate) — the
   *  caller should treat the worktree as ready and SKIP the setup phase. */
  rehydrated: boolean;
}

/**
 * Phase A — create the worktree (or detect an existing one). Uses prepared local refs for
 * `git worktree add`, no user `setup:` commands. Throws the same `create` /
 * `orphan` / `cancelled` errors `setupWorktree` always has; returns
 * `rehydrated: true` when a matching worktree already exists (setup replay is
 * skipped). Story registration is awaited separately before setup or launch.
 *
 * Split out of `setupWorktree` (RF-perf-14 decouple) so the open path can flip
 * the workspace to "ready" + mount the terminal the instant the dir exists,
 * then run the (potentially multi-second) `setup:` commands in the background
 * ({@link runWorktreeSetupPhase}). `fetch_and_create_worktree` emits no
 * `worktree-run-{runId}` progress events (only the setup runner does — see
 * `WorktreePhase`), so no progress listener is needed here.
 */
export async function createWorktree(
  options: CreateWorktreeOptions,
): Promise<CreateWorktreeResult> {
  const { repoPath, branchName, worktreePath, startPoint, defaultStartPoint, signal } = options;

  if (signal?.aborted) throw makeError("cancelled", "aborted before start", true);

  // ── pre-flight: rehydrate or orphan-detect ─────────────────────────────
  const existing = await listWorktrees(repoPath).catch(() => []);
  const matching = existing.find((w) => w.path === worktreePath);
  if (matching) {
    const matchesBranch = branchRefMatches(matching.branch, branchName);
    if (matchesBranch) {
      // Rehydrate path: the worktree already exists, so skip setup replay.
      return { worktreePath, rehydrated: true };
    }
    throw makeError(
      "orphan",
      `Worktree at ${worktreePath} tracks branch '${matching.branch ?? "(detached)"}', expected '${branchName}'. Remove the directory or pick a different branch and retry.`,
      true,
    );
  }

  // ── local git worktree add ──────────────────────────────────────
  try {
    await fetchAndCreateWorktree(
      repoPath,
      branchName,
      worktreePath,
      startPoint,
      defaultStartPoint ?? null,
    );
  } catch (e) {
    throw makeError("create", errorMessage(e), false);
  }
  // A cancel that landed while `git worktree add` ran: the run hasn't started
  // (no `cancelWorktreeRun` target exists yet), so just undo the fresh dir.
  if (signal?.aborted) {
    // Cleanup never implies permission to discard dirty or ignored files.
    await removeWorktree(repoPath, worktreePath, false).catch(() => {});
    throw makeError("cancelled", "aborted after create_worktree", true);
  }

  return { worktreePath, rehydrated: false };
}

export interface RunWorktreeSetupPhaseOptions extends WorktreeSetupOptions {
  /**
   * RF-perf-14 (decouple) — when true (interactive/background mode), a setup
   * FAILURE or mid-setup CANCEL does NOT remove the worktree: it is already
   * "ready" and the user has a usable shell, so the failure surfaces as a
   * retryable badge instead of destroying the dir under them (OQ-2 default:
   * keep + retry). Default `false` = the legacy blocking-open behavior — roll
   * the worktree back on setup failure/cancel (a failed open leaves nothing).
   */
  keepWorktreeOnFailure?: boolean;
}

/**
 * Phase B — run the worktree's `setup:` commands (the potentially multi-second
 * `npm ci`-class work). Assumes the dir already exists ({@link createWorktree}).
 * Owns the `worktree-run-{runId}` progress listener, the SW-WT-9 provenance
 * gate, the cancel wiring, and the failure classification. With
 * `keepWorktreeOnFailure` it becomes non-destructive so a backgrounded setup
 * failure leaves the mounted terminal alone.
 */
export async function runWorktreeSetupPhase(
  options: RunWorktreeSetupPhaseOptions,
): Promise<WorktreeSetupResult> {
  const { repoPath, orgRoot, worktreePath, runId, signal, onProgress, onStep, setupApproval } =
    options;
  const keepWorktreeOnFailure = options.keepWorktreeOnFailure ?? false;

  // ── stream listener wiring (per-line output + SW-WT-12 step boundaries) ──
  let unlisten: (() => void) | null = null;
  if (onProgress || onStep) {
    unlisten = await listen<RawWorktreeRunPayload>(`worktree-run-${runId}`, (event) => {
      const p = event.payload;
      if (isStepPayload(p)) {
        onStep?.({
          kind: p.event === "step-started" ? "started" : "finished",
          phase: p.phase,
          stepIndex: p.step_index,
          command: p.command,
          passed: p.passed,
          // Normalize absent → null so a signal-killed child (Rust `None`) and a
          // missing field compare equal downstream (`stepsEqual`).
          exitCode: p.exit_code ?? null,
        });
        return;
      }
      onProgress?.({
        phase: p.phase,
        stepIndex: p.step_index,
        stream: p.stream,
        line: p.line,
        tsMs: p.ts_ms,
      });
    });
  }

  // ── cancel wiring ──────────────────────────────────────────────────────
  let cancelled = false;
  const onAbort = () => {
    cancelled = true;
    cancelWorktreeRun(runId).catch(() => {});
  };
  signal?.addEventListener("abort", onAbort);

  // Roll the worktree back on failure ONLY in the legacy blocking mode; in
  // background mode the worktree is already "ready" and must survive a bad setup.
  const rollbackUnlessKept = async () => {
    // Preserve edits when the existing removal primitive refuses safe cleanup.
    if (!keepWorktreeOnFailure) await removeWorktree(repoPath, worktreePath, false).catch(() => {});
  };

  try {
    // Already aborted before we started (e.g. a background setup whose workspace
    // was closed WHILE it waited behind the concurrency gate): the `onAbort`
    // listener above never fires for an already-aborted signal, so bail here so
    // the setup subprocess never spawns. keepWorktreeOnFailure applies (no
    // rollback of an already-usable worktree).
    if (signal?.aborted) throw makeError("cancelled", "aborted before setup", true);

    // ── load config ──────────────────────────────────────────────────────
    // SW-WT-1: a missing worktree.yaml is a clean, non-blocking state — NOT an
    // error. Keep the worktree and skip setup as a no-op. Do not remove
    // the worktree or throw (AC-6). Native completion still records this successful no-op.
    const savedConfig = await getWorktreeConfig(repoPath, orgRoot);
    // Even no-step setup completes durable registration through the native owner.
    const config = savedConfig ?? { version: 1 as const, setup: [] };

    // ── SW-WT-9: team-sync provenance gate ─────────────────────────────────
    // Before running setup commands, check whether this repo's worktree.yaml
    // came from a teammate's commit. If so and it isn't already approved for
    // its current content, ask for one-time consent. Declining leaves the
    // worktree intact and setup retryable; agents wait for approved setup.
    let approvedConfigHash: string | undefined;
    if (setupApproval && config.setup.length > 0) {
      const report = await evaluateSetupProvenance(repoPath, orgRoot).catch((e) => {
        // A provenance read failure must never block setup; fall back to
        // "no gate" (the backend guard still refuses unapproved team-synced).
        logger.warn("worktree-setup", `provenance check failed, proceeding ungated: ${e}`);
        return null;
      });
      if (report?.provenance === "team_synced" && report.config_hash) {
        const hash = report.config_hash;
        if (setupApproval.isApproved(repoPath, hash)) {
          approvedConfigHash = hash;
        } else {
          const approved = await setupApproval.prompt({ steps: config.setup, configHash: hash });
          if (!approved) {
            // Decline: keep the worktree, skip setup. Do NOT remove it.
            throw makeError(
              "setup",
              "Setup approval is required. Retry setup to review and approve the commands.",
              true,
            );
          }
          // Persisting the approval is best-effort: a disk-write failure must
          // not veto the consent the user just gave. Proceed with this run; if
          // the write failed, the next run simply re-prompts (not remembered).
          await setupApproval.record(repoPath, hash).catch((e) => {
            logger.warn("worktree-setup", `failed to persist setup approval: ${e}`);
          });
          approvedConfigHash = hash;
        }
      }
    }

    const result = await runWorktreeSetup(
      worktreePath,
      repoPath,
      config,
      runId,
      orgRoot,
      approvedConfigHash,
    ).catch(async (e) => {
      // SW-WT-9 backstop: the backend refuses team-synced setup whose live
      // content isn't approved (e.g. the config changed between our gate and
      // execution) with `NEEDS_APPROVAL:<live-hash>`. Honor the documented
      // contract — re-prompt once with the live hash and retry — instead of
      // surfacing it as a generic setup failure. Without approval hooks, the
      // refusal stands (rethrown as the real error).
      const liveHash = setupApproval ? parseNeedsApproval(errorMessage(e)) : null;
      if (liveHash === null) throw e;
      // The backstop fires precisely because the on-disk config changed since
      // we gated. Re-fetch it so the preview shows the CURRENT commands that
      // will run — the user must approve exactly what executes (AC-2), not the
      // stale list captured at the start of this call.
      const liveConfig = (await getWorktreeConfig(repoPath, orgRoot).catch(() => null)) ?? config;
      const approved = await setupApproval!.prompt({
        steps: liveConfig.setup,
        configHash: liveHash,
      });
      if (!approved) return NEEDS_APPROVAL_DECLINED;
      await setupApproval!
        .record(repoPath, liveHash)
        .catch((err) => logger.warn("worktree-setup", `failed to persist setup approval: ${err}`));
      return runWorktreeSetup(worktreePath, repoPath, liveConfig, runId, orgRoot, liveHash);
    });

    // Decline keeps durable setup incomplete and exposes the existing retry path.
    if (result === NEEDS_APPROVAL_DECLINED) {
      throw makeError(
        "setup",
        "Setup approval is required. Retry setup to review and approve the commands.",
        true,
      );
    }

    if (cancelled || result.exit_code === 130 /* sigint convention for cancel */) {
      await rollbackUnlessKept();
      throw makeError("cancelled", "setup cancelled by user", true);
    }
    if (result.exit_code !== 0) {
      await rollbackUnlessKept();
      const firstFailure = result.results.find((r) => !r.passed);
      // Log full result.results so the actual stderr per step is visible
      // in DevTools + Tauri log; the toast only gets a one-line summary.
      logger.error(
        "worktree-setup",
        `setup failed (exit=${result.exit_code}): ${JSON.stringify(result.results, null, 2)}`,
      );
      let detail: string;
      if (firstFailure) {
        const header = `${firstFailure.phase}[${firstFailure.step_index}] '${firstFailure.command}' failed (exit ${firstFailure.exit_code ?? "?"})`;
        const stderrSnippet = firstFailure.stderr?.trim().slice(0, 400);
        detail = stderrSnippet ? `${header}\n${stderrSnippet}` : header;
      } else {
        detail = `setup exited ${result.exit_code}`;
      }
      throw makeError(
        "setup",
        detail,
        true,
        buildSetupFailureDiagnostics(result.results, result.exit_code),
      );
    }

    return {
      worktreePath,
      results: result.results,
      ...(savedConfig ? {} : { setupSkipped: true }),
    };
  } finally {
    signal?.removeEventListener("abort", onAbort);
    if (unlisten) safeUnlisten(unlisten);
  }
}

/**
 * Orchestrate worktree setup for a story workspace — the BLOCKING form: create
 * the worktree, then run its `setup:` commands, resolving only when both are
 * done. Composes {@link createWorktree} (Phase A) + {@link runWorktreeSetupPhase}
 * (Phase B); a setup failure rolls the worktree back (legacy behavior). Used by
 * callers that need a fully-prepared worktree on resolve (e.g. an auto-launched
 * dev agent). The interactive open path instead drives the two phases directly
 * (create → flip ready + mount → background setup) so the terminal is usable the
 * instant `git worktree add` completes (RF-perf-14 decouple).
 *
 * Failure modes: `config`/`orphan`/`create`/`setup`/`cancelled` (unchanged).
 */
export async function setupWorktree(options: WorktreeSetupOptions): Promise<WorktreeSetupResult> {
  const created = await createWorktree(options);
  // Establish durable ownership before repository setup or agent launch.
  const setupComplete = await options.onCreated?.();
  if (created.rehydrated && setupComplete !== false) {
    // Rehydrate: worktree already existed → skip setup replay (SW-WT-6).
    return { worktreePath: options.worktreePath, results: [] };
  }
  return runWorktreeSetupPhase(options);
}

function makeError(
  failedStep: WorktreeSetupError["failedStep"],
  message: string,
  recoverable: boolean,
  diagnostics?: WorktreeSetupFailureDiagnostics,
): WorktreeSetupError {
  const err = new Error(message) as WorktreeSetupError;
  err.failedStep = failedStep;
  err.recoverable = recoverable;
  err.diagnostics = diagnostics;
  return err;
}

function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  return JSON.stringify(e);
}

/** SW-WT-9: sentinel meaning the user declined the backend's NEEDS_APPROVAL
 *  re-prompt — the worktree is kept and setup is skipped. */
const NEEDS_APPROVAL_DECLINED = Symbol("needs-approval-declined");

/** Extract the live hash from a backend `NEEDS_APPROVAL:<hash>` error message,
 *  or null when the message isn't that sentinel OR carries no hash. An empty
 *  hash must not be treated as a re-prompt (it can never match anything to
 *  approve) — fall through to rethrow the error instead. */
function parseNeedsApproval(msg: string): string | null {
  const marker = "NEEDS_APPROVAL:";
  const idx = msg.indexOf(marker);
  if (idx === -1) return null;
  const hash = msg.slice(idx + marker.length).trim();
  return hash === "" ? null : hash;
}
