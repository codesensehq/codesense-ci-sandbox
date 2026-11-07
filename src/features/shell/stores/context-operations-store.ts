// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { AgentPrepareState } from "@/features/agents/lib/agent-prepare-progress";
import { formatBytes } from "@/features/viewer/lib/workspace-setup-state";

/**
 * SW-XS-10 (SPEC §8.2): the explicit context operations (Step 3) the workspace
 * pill's chip shows — `hydrate_worktree`, `hydrate_story`, `reverse_sync_worktree`
 * — folded from `agent-prepare-progress` events that carry an `operationId`.
 * Nothing else lights the chip: nothing syncs in the background any more.
 *
 * Ephemeral, in-window state (never persisted). A finished operation fades from
 * the chip after {@link OPERATION_FADE_MS}; a failure — or a partial result, such
 * as a publish that left conflicts unwritten — sticks until it is dismissed.
 */

/** How long a finished operation stays on the chip. */
export const OPERATION_FADE_MS = 4000;

/** Operations kept per window (the oldest settled ones go first). */
const KEEP = 40;

export interface ContextOperation {
  id: string;
  /** The workspace the operation names: a local root, or a machine handle. */
  workspace: string;
  /** Who called it: a tab's session id (local), or `explicit` (a machine). */
  caller: string;
  state: AgentPrepareState;
  startedAt: number;
  settledAt?: number;
  /** A sticky failure the person closed. */
  dismissed?: boolean;
  /** Stop was asked for and has not settled yet. */
  stopping?: boolean;
  /** The most files this run reported moving: the chip's count never runs
   *  backwards when apply restarts its counters. */
  peakFiles?: number;
  /** Its kept-modified files were all discarded: it no longer holds the chip. */
  keptResolved?: boolean;
}

interface ContextOperationsStore {
  byId: Record<string, ContextOperation>;
  /** Fold one operation state (with an `operationId`) in. */
  apply: (state: AgentPrepareState, now?: number) => void;
  dismiss: (id: string) => void;
  markStopping: (id: string) => void;
  /** Drop an operation that will send no more events (Stop found it ended). */
  remove: (id: string) => void;
  /** Its "N kept" were discarded: the result no longer sticks on the chip. */
  resolveKept: (id: string) => void;
  reset: () => void;
}

export const useContextOperationsStore = create<ContextOperationsStore>((set) => ({
  byId: {},
  apply: (state, now = Date.now()) => {
    const id = state.operationId;
    if (!id) return;
    set((s) => {
      const prev = s.byId[id];
      const prevSettled = prev?.settledAt !== undefined;
      // A repeated terminal event (a retry's replay) updates what it says but
      // keeps when it settled and whether it was dismissed: the fade doesn't
      // restart and a dismissed failure stays closed.
      if (prevSettled && state.settled) {
        return { byId: { ...s.byId, [id]: { ...prev, state } } };
      }
      // A late tick after the settle (a throttled count) is stale; only a new
      // run, which starts with its diff, reopens a kept id.
      if (prevSettled && !state.settled && state.phase !== "diff") return s;
      const restarted = prevSettled && !state.settled;
      const continuing = prev && !restarted;
      const next: ContextOperation = {
        id,
        workspace: state.workspace,
        caller: state.agent,
        state,
        startedAt: continuing ? prev.startedAt : now,
        ...(state.settled ? { settledAt: now } : {}),
        ...(continuing && prev.stopping && !state.settled ? { stopping: true } : {}),
        peakFiles: Math.max(continuing ? (prev.peakFiles ?? 0) : 0, state.files),
      };
      const byId = { ...s.byId, [id]: next };
      const ids = Object.keys(byId);
      if (ids.length > KEEP) {
        const settled = ids
          .filter((k) => byId[k].settledAt !== undefined)
          .sort((a, b) => byId[a].settledAt! - byId[b].settledAt!);
        for (const k of settled.slice(0, ids.length - KEEP)) delete byId[k];
      }
      return { byId };
    });
  },
  dismiss: (id) =>
    set((s) =>
      s.byId[id] ? { byId: { ...s.byId, [id]: { ...s.byId[id], dismissed: true } } } : s,
    ),
  resolveKept: (id) =>
    set((s) =>
      s.byId[id] ? { byId: { ...s.byId, [id]: { ...s.byId[id], keptResolved: true } } } : s,
    ),
  remove: (id) =>
    set((s) => {
      // Only a running one: a settled operation keeps its line (its events
      // already said how it ended).
      if (!s.byId[id] || s.byId[id].settledAt !== undefined) return s;
      const byId = { ...s.byId };
      delete byId[id];
      return { byId };
    }),
  markStopping: (id) =>
    set((s) =>
      s.byId[id] && s.byId[id].settledAt === undefined
        ? { byId: { ...s.byId, [id]: { ...s.byId[id], stopping: true } } }
        : s,
    ),
  reset: () => set({ byId: {} }),
}));

/** One path the way both sides spell it: no trailing slash, no `/private`
 *  prefix macOS adds to canonical temp and var paths. */
export function workspaceIdentityKey(path: string): string {
  return path.replace(/\/+$/, "").replace(/^\/private(?=\/)/, "");
}

/** How an operation reads on the chip. */
export interface OperationView {
  tone: "busy" | "done" | "partial" | "failed" | "stopped";
  /** Which way the content moved: into the workspace, or out to team content. */
  direction: "down" | "up";
  text: string;
  /** Stays until dismissed (a failure, or a partial result). */
  sticky: boolean;
}

const nf = new Intl.NumberFormat("en-US");
const count = (n: number, one: string, many = `${one}s`) =>
  `${nf.format(n)} ${n === 1 ? one : many}`;

/** `0.6 s`, `12 s`. */
export function formatDuration(ms: number): string {
  const s = ms / 1000;
  return s >= 10 ? `${Math.round(s)} s` : `${s.toFixed(1)} s`;
}

/** How long the operation took: the engine's own clock when it reported one. */
export function operationElapsedMs(op: ContextOperation): number | undefined {
  if (op.settledAt === undefined) return undefined;
  return op.state.summary?.elapsedMs || op.settledAt - op.startedAt;
}

/** A publish's conflicts: changes it found but did not write. */
export function publishConflicts(op: ContextOperation): number {
  return op.state.operationKind === "publish" && op.state.phase === "ready"
    ? Math.max(0, op.state.total - op.state.files)
    : 0;
}

/** The chip's line for one operation. */
export function describeOperation(op: ContextOperation): OperationView {
  const { state } = op;
  const publish = state.operationKind === "publish";
  const direction = publish ? "up" : "down";
  const elapsed = operationElapsedMs(op);
  const took = elapsed !== undefined ? ` · ${formatDuration(elapsed)}` : "";
  if (!state.settled) {
    const moving = state.phase === "transfer" || state.phase === "apply";
    const verb = op.stopping ? "Stopping" : publish ? "Publishing" : "Hydrating";
    const files = Math.max(state.files, op.peakFiles ?? 0);
    const what = publish ? count(files, "change") : count(files, "file");
    return {
      tone: "busy",
      direction,
      text: moving && files > 0 && !op.stopping ? `${verb} ${what}` : verb,
      sticky: false,
    };
  }
  switch (state.phase) {
    case "ready":
    case "skipped": {
      if (publish) {
        const conflicts = publishConflicts(op);
        if (state.files === 0 && conflicts === 0)
          return { tone: "done", direction, text: `Nothing to publish${took}`, sticky: false };
        if (conflicts > 0)
          return {
            tone: "partial",
            direction,
            text: `Published ${count(state.files, "change")} · ${count(conflicts, "conflict")}`,
            sticky: true,
          };
        return {
          tone: "done",
          direction,
          text: `Published ${count(state.files, "change")}${took}`,
          sticky: false,
        };
      }
      const written = state.summary?.written ?? state.files;
      // Files it kept because they were edited stay on the chip, so their
      // "Discard my edits" is there to reach — it never fades out from under you.
      const kept = op.keptResolved ? 0 : (state.summary?.keptModifiedCount ?? 0);
      return {
        tone: "done",
        direction,
        text:
          (written > 0 ? `Hydrated ${count(written, "file")}` : "Already up to date") +
          (kept > 0 ? ` · ${kept} kept` : took),
        sticky: kept > 0,
      };
    }
    case "cancelled":
      return {
        tone: "stopped",
        direction,
        text: publish
          ? "Publish stopped — team content unchanged"
          : "Hydrate stopped — nothing partial left",
        sticky: false,
      };
    default:
      return {
        tone: "failed",
        direction,
        text: publish ? "Publish failed" : "Hydrate failed",
        sticky: true,
      };
  }
}

/** The facts the details popover lists. */
export function operationFacts(op: ContextOperation): Array<[string, string]> {
  const { state } = op;
  const s = state.summary;
  const facts: Array<[string, string]> = [];
  const kind =
    state.operationKind === "publish"
      ? "Publish to team content"
      : state.operationKind === "hydrateStory"
        ? "Hydrate story context"
        : "Hydrate workspace";
  facts.push(["Operation", kind]);
  if (s) {
    facts.push(["Files", `${nf.format(s.written)} written · ${nf.format(s.unchanged)} unchanged`]);
    facts.push(["Bytes", formatBytes(s.bytesWritten)]);
  } else if (state.settled || state.files > 0) {
    facts.push([
      "Files",
      state.operationKind === "publish"
        ? `${nf.format(state.files)} published`
        : `${nf.format(state.files)} copied`,
    ]);
    facts.push(["Bytes", formatBytes(state.bytes)]);
  }
  const elapsed = operationElapsedMs(op);
  if (elapsed !== undefined) facts.push(["Duration", formatDuration(elapsed)]);
  return facts;
}

/** The operation the chip shows for a workspace at `now`: a running one first,
 *  then a sticky one nobody dismissed, then one that finished within the fade. */
export function selectChipOperation(
  byId: Record<string, ContextOperation>,
  identities: readonly string[],
  now: number,
): ContextOperation | null {
  const keys = new Set(identities.filter(Boolean).map(workspaceIdentityKey));
  if (keys.size === 0) return null;
  let running: ContextOperation | null = null;
  let sticky: ContextOperation | null = null;
  let fresh: ContextOperation | null = null;
  for (const op of Object.values(byId)) {
    if (!keys.has(workspaceIdentityKey(op.workspace))) continue;
    if (op.settledAt === undefined) {
      if (!running || op.startedAt > running.startedAt) running = op;
      continue;
    }
    if (describeOperation(op).sticky) {
      if (!op.dismissed && (!sticky || op.settledAt > sticky.settledAt!)) sticky = op;
      continue;
    }
    if (now - op.settledAt < OPERATION_FADE_MS && (!fresh || op.settledAt > fresh.settledAt!))
      fresh = op;
  }
  return running ?? sticky ?? fresh;
}
