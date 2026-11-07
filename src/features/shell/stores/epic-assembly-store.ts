// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { DevEnvRun } from "@/tauri/commands";

/** Product readiness follows source repository assembly and bounded development setup.
 * Context changes only through an agent startup manifest or a selected operation. */
export type EpicInstallState = "running" | "done" | "failed";

export interface EpicRepoFailure {
  owner: string;
  repo: string;
  /** Create-time reason (no local clone / worktree add) — absent for an install
   *  failure (a non-zero `npm ci` / provenance refusal), which we only know
   *  failed, not why (the reason streamed on the log channel). */
  error?: string;
}

interface EpicInstall {
  owner: string;
  repo: string;
  state: EpicInstallState;
}

interface EpicAssemblyEntry {
  /** `run_id` → install, seeded from `AssembleEpicResult.devEnvRuns` (all
   *  `running`), advanced to `done`/`failed` by the terminal events. */
  installs: Record<string, EpicInstall>;
  /** Repos that never assembled (`AssembleEpicResult.failures`) — they get no
   *  install run, so they live here rather than in `installs`. */
  createFailures: EpicRepoFailure[];
  /** The whole `assemble_epic_worktrees` command threw (no result) — a TOTAL
   *  failure (the epic couldn't set up at all → in-pane error, not a flag). */
  assemblyError?: string;
  /** Has `AssembleEpicResult` arrived? Before it, the batch can't be `settled`
   *  (the run_id set — hence `M` — is unknown), and the overlay stays up. */
  seeded: boolean;
}

interface EpicAssemblyStore {
  /** epic `workspace_dir` (= workspace `cwd`) → batch entry. */
  byCwd: Record<string, EpicAssemblyEntry>;
  /** `run_id` → owning epic `cwd`, so the terminal-event listener (which only
   *  carries the run_id) can route a completion to its epic. */
  runIdToCwd: Record<string, string>;
  /** `run_id` → ok, for completions that arrived BEFORE the batch was seeded
   *  (the event raced the assemble reply). Drained by `seedEpicAssembly`. Every
   *  `worktree-run-completed` is an epic install (only the epic fan-out emits
   *  it), so an unrouted completion is always a not-yet-seeded epic run.
   *  Accepted LOW residual (review): a prior attempt's install that completes
   *  LATE (after a close+reopen pruned its route) buffers an inert orphan here —
   *  its unique seq'd id never matches a future seed, so it can't mis-attribute;
   *  it's just a bounded-per-session boolean. Not GC'd because a blanket clear
   *  would wipe a concurrent DIFFERENT epic's legit pre-seed buffer. */
  pendingByRunId: Record<string, boolean>;
  /** Begin (or RESET) a fresh assembly batch for an epic — called once per real
   *  open, before the result arrives, so the overlay shows immediately. Resets
   *  (not idempotent-skips) so a close+reopen starts clean; this reset — NOT a
   *  close-time clear — is the batch's cleanup point (entries are ephemeral,
   *  bounded by the number of distinct epics opened this session). */
  beginEpicAssembly: (cwd: string) => void;
  /** Seed the expected install set + create failures from `AssembleEpicResult`. */
  seedEpicAssembly: (
    cwd: string,
    devEnvRuns: DevEnvRun[],
    createFailures: EpicRepoFailure[],
  ) => void;
  /** The assemble command threw entirely — a total failure. */
  markEpicAssemblyError: (cwd: string, error: string) => void;
  /** From the `worktree-run-completed` listener. Returns the owning epic `cwd`
   *  (or `null` if the run_id isn't a tracked epic install), so the listener can
   *  bridge the settled failure set to the chip echo. */
  markEpicInstall: (runId: string, ok: boolean) => string | null;
}

export const useEpicAssemblyStore = create<EpicAssemblyStore>((set, get) => ({
  byCwd: {},
  runIdToCwd: {},
  pendingByRunId: {},
  beginEpicAssembly: (cwd) =>
    set((s) => {
      // Drop any stale run_ids from a PRIOR batch on this cwd — both the routes
      // and any completions buffered while the epic was closed. Run_ids are
      // globally unique per attempt (backend seq), so a prior batch's ids never
      // collide with this one's — pruning them is precise.
      const prior = s.byCwd[cwd];
      const pendingByRunId = { ...s.pendingByRunId };
      if (prior) for (const id of Object.keys(prior.installs)) delete pendingByRunId[id];
      return {
        byCwd: {
          ...s.byCwd,
          [cwd]: { installs: {}, createFailures: [], seeded: false },
        },
        runIdToCwd: Object.fromEntries(Object.entries(s.runIdToCwd).filter(([, c]) => c !== cwd)),
        pendingByRunId,
      };
    }),
  seedEpicAssembly: (cwd, devEnvRuns, createFailures) =>
    set((s) => {
      const installs: Record<string, EpicInstall> = {};
      const runIdToCwd = { ...s.runIdToCwd };
      const pendingByRunId = { ...s.pendingByRunId };
      for (const r of devEnvRuns) {
        // Apply any completion that raced ahead of this seed (pre-seed buffer),
        // else the install would stay "running" forever and the overlay hangs.
        const buffered = pendingByRunId[r.runId];
        const state: EpicInstallState =
          buffered === undefined ? "running" : buffered ? "done" : "failed";
        installs[r.runId] = { owner: r.owner, repo: r.repo, state };
        if (buffered !== undefined) delete pendingByRunId[r.runId];
        runIdToCwd[r.runId] = cwd;
      }
      const prev = s.byCwd[cwd];
      // A real report supersedes a provisional "never reported" failure from the
      // overlay watchdog, so a slow-but-successful assembly is not left failed.
      return {
        byCwd: {
          ...s.byCwd,
          [cwd]: { ...prev, installs, createFailures, seeded: true, assemblyError: undefined },
        },
        runIdToCwd,
        pendingByRunId,
      };
    }),
  markEpicAssemblyError: (cwd, error) =>
    set((s) => {
      const prev = s.byCwd[cwd] ?? { installs: {}, createFailures: [], seeded: false };
      return { byCwd: { ...s.byCwd, [cwd]: { ...prev, assemblyError: error, seeded: true } } };
    }),
  markEpicInstall: (runId, ok) => {
    const cwd = get().runIdToCwd[runId];
    if (!cwd) {
      // Pre-seed race: the completion beat the assemble reply that seeds this
      // run_id. Buffer it — `seedEpicAssembly` drains it (review: else hang).
      // Guard on the `epic-` prefix so a future non-epic emitter on this shared
      // channel can't grow the buffer unbounded (only the epic fan-out emits
      // these today — an unrouted `epic-` id is always a not-yet-seeded epic run).
      if (runId.startsWith("epic-")) {
        set((s) => ({ pendingByRunId: { ...s.pendingByRunId, [runId]: ok } }));
      }
      return null;
    }
    set((s) => {
      const entry = s.byCwd[cwd];
      const inst = entry?.installs[runId];
      // Idempotent: a duplicate/late completion for an already-terminal run is a
      // no-op (referentially unchanged → no re-render).
      if (!entry || !inst || inst.state !== "running") return s;
      return {
        byCwd: {
          ...s.byCwd,
          [cwd]: {
            ...entry,
            installs: {
              ...entry.installs,
              [runId]: { ...inst, state: ok ? "done" : "failed" },
            },
          },
        },
      };
    });
    return cwd;
  },
}));

/** The overlay-facing derived view of an epic's assembly batch. */
export interface EpicBatchStatus {
  seeded: boolean;
  /** M — expected dev-env installs. */
  total: number;
  /** Installs finished OK. */
  ready: number;
  /** `"owner/repo"` of installs still running (for the "installing {repo}…" tail). */
  installingRepos: string[];
  /** Create and install failures. */
  failures: EpicRepoFailure[];
  /** Present iff the assemble command threw (total failure). */
  assemblyError?: string;
  /** Every expected install is terminal (done/failed). */
  allInstallsTerminal: boolean;
  /** Assembly returned and every setup run settled. */
  settled: boolean;
  /** The epic couldn't set up at all → in-pane error (close + reopen). */
  totalFailure: boolean;
  /** Some repos ready, some failed → USABLE epic + in-pane/chip flag (AC-4). */
  partialFailure: boolean;
}

/** Pure assembly/setup readiness; background context waves are unrelated. */
export function deriveEpicBatchStatus(entry: EpicAssemblyEntry): EpicBatchStatus {
  const installs = Object.values(entry.installs);
  const total = installs.length;
  const ready = installs.filter((i) => i.state === "done").length;
  const installingRepos = installs
    .filter((i) => i.state === "running")
    .map((i) => `${i.owner}/${i.repo}`);
  const installFailures: EpicRepoFailure[] = installs
    .filter((i) => i.state === "failed")
    .map((i) => ({ owner: i.owner, repo: i.repo }));
  const failures: EpicRepoFailure[] = [...entry.createFailures, ...installFailures];
  const allInstallsTerminal = installs.every((i) => i.state !== "running");
  const totalFailure = entry.assemblyError !== undefined;
  const settled = totalFailure || (entry.seeded && allInstallsTerminal);
  const partialFailure = settled && !totalFailure && failures.length > 0;
  return {
    seeded: entry.seeded,
    total,
    ready,
    installingRepos,
    failures,
    assemblyError: entry.assemblyError,
    allInstallsTerminal,
    settled,
    totalFailure,
    partialFailure,
  };
}

/** The current product assembly batch, independently of context operations. */
export function useEpicBatchStatus(cwd: string | undefined): EpicBatchStatus | undefined {
  const entry = useEpicAssemblyStore((s) => (cwd ? s.byCwd[cwd] : undefined));
  return entry === undefined ? undefined : deriveEpicBatchStatus(entry);
}

/** The overlay's epic assembly sub-label (AC-3) — repo-count based, since the
 *  per-repo install stream is a lifecycle (not a `done/total`). */
export function formatEpicAssemblyProgress(b: EpicBatchStatus): string {
  if (b.total === 0) return "Setting up epic…";
  const head = `Assembling ${b.total} repo${b.total === 1 ? "" : "s"} — ${b.ready} of ${b.total} ready`;
  const current = b.installingRepos[0];
  return current ? `${head} · installing ${current}…` : head;
}

/** The comma-joined `"owner/repo"` labels of a failure set (a create failure has
 *  no owner-less form; the inheritance pseudo-failure carries only a repo). The
 *  single label builder shared by every epic-failure surface. */
export function epicFailureLabels(failures: EpicRepoFailure[]): string {
  return failures.map((f) => (f.owner ? `${f.owner}/${f.repo}` : f.repo)).join(", ");
}

/** The chip echo / eager summary for an epic's failed repos (AC-4). One verb
 *  ("couldn't set up") across every surface — the eager create-failure toast at
 *  open AND the combined settle echo — so they never disagree. */
export function formatEpicFailureSummary(failures: EpicRepoFailure[]): string {
  const n = failures.length;
  return `${n} repo${n === 1 ? "" : "s"} couldn't set up: ${epicFailureLabels(failures)}`;
}

/** AC-4: the in-pane partial-failure flag on a settled-but-usable epic. */
export function formatEpicPartialFailure(b: EpicBatchStatus): string {
  const readyPart = `${b.ready} repo${b.ready === 1 ? "" : "s"} ready`;
  return `${readyPart} · ${epicFailureLabels(b.failures)} couldn't set up — reopen to retry`;
}
