// SPDX-License-Identifier: MIT
import { projectedSession } from "@/features/viewer/lib/agent-list-projection";
import { create } from "zustand";
import {
  admitsActivityCompletion,
  equalActivityCompletion,
  followsActivityCompletion,
  sameActivityIdentity,
} from "@/shared/lib/activity-completion";
export { sameActivityIdentity } from "@/shared/lib/activity-completion";

import {
  advanceSessionAgent,
  agentCrashSeq,
  agentRecordDot,
  commandEndIsAgentExit,
  endedByExit,
  sessionExitOutcome,
  type ProcessSignal,
  ptyExitIsAgentExit,
  type SessionAgentInput,
  type SessionAgentRecord,
} from "@/shared/lib/agent-state";
import type { TerminalAiStatus } from "@/shared/lib/terminal-ai-status";
import type { AgentLaunchSpec } from "@/shared/lib/agent-session-launcher";
import type { ActivityCompletion, ActivitySnapshot } from "@/tauri/commands";

export interface SessionActivity {
  snapshot: ActivitySnapshot | null;
  freshness: "current" | "recovering" | "unavailable" | "upgrade-required";
  /** Completion boundary for which this newly accepted revision is an anchor.
   * Recovery and equal-revision replay cannot prove a later turn. */
  completionAnchorSeq?: number;
}

/**
 * How a session's own process ended (`pty-exit`), as its host reaped it — the
 * in-process runtime or the session-server sidecar that owns the child. For a
 * tab this is its shell's exit. The ended/crashed bar in the pane reads it.
 */
export interface SessionExit {
  /** The exit code, or null when the process died by a signal or the status is unknown. */
  code: number | null;
  /** The signal that killed it, or null. */
  signal: string | null;
  /** CodeSense ended it (a tab close, a stop), so a signal death is not a crash. */
  requested: boolean;
  /** `sessionExitOutcome`: `crashed` for a signal death nobody asked for, or
   *  the exit that crashed the session's agent; else `done`. */
  outcome: "done" | "crashed";
  /** When the exit was recorded (ms): the age its mark's words give. */
  at?: number;
}

/**
 * Canonical agent state per PTY session (`agent-state.ts`): each tier's latest
 * input plus the resolved state, which keeps what the dot flattens (a model turn
 * in flight reads `thinking`, a running tool `working`, a dead agent `crashed`).
 * Alongside it, how each session's own process ended (`exitBySession`), for
 * every session whose PTY exited — agent or plain shell.
 *
 * Its own store, apart from the viewer store: hooks fire on every tool call, and
 * a write here must not notify the viewer store's subscribers. Ephemeral
 * (never persisted). The viewer store prunes it wherever it prunes
 * `aiStatusBySession`; a respawn into the same session id resets it.
 */
/** A restart or relaunch of a session: in flight, or failed and why. */
export interface RelaunchState {
  busy: boolean;
  failure: { action: "Restart" | "Relaunch"; reason: string } | null;
}

/** How CodeSense launched a session's agent, and the exact line it typed. */
export interface AgentLaunch {
  line: string;
  /** The agent's name (the executable's basename), for labels. */
  agent: string;
  /** Everything needed to launch it again the same way. */
  spec: AgentLaunchSpec;
}

interface AgentStateStore {
  /** Output can precede the first provider observation, especially for headless runs. */
  outputAtBySession: Record<string, number>;
  /** Display-only ordering against a later turn; managed task outcomes stay intact. */
  completionSeqBySession: Record<string, number>;
  /** Optional host ordering proof; absent for older producers and local safety nets. */
  completionActivityBySession: Record<string, ActivityCompletion>;
  /** Admission and live/history projection are separate decisions. */
  recordTaskCompletion: (
    sessionId: string,
    activityCompletion?: ActivityCompletion,
  ) => { accepted: boolean; historical: boolean; repeated: boolean };
  activityBySession: Record<string, SessionActivity>;
  recordActivity: (snapshot: ActivitySnapshot, sentAt: number, receivedAt?: number) => boolean;
  /** Fresh output from an admitted current stream; never snapshot replay. */
  recordOutput: (sessionId: string, at?: number) => void;
  activityUnavailable: (
    sessions: Iterable<string>,
    freshness?: "recovering" | "unavailable" | "upgrade-required",
  ) => void;
  bySession: Record<string, SessionAgentRecord>;
  /** Consumed by the pane's ended/crashed bar. */
  exitBySession: Record<string, SessionExit>;
  /** When (ms) each session's fine mark last changed, independently of activity.
   *  Stamped by `watchDotSince` (terminal-tab-selectors), which projects the mark. */
  stateSince: Record<string, number>;
  /** When (ms) each session was last seen: its tab the active, visible tab
   *  with the window focused. A state is seen once `seenAt >= stateSince`. */
  seenAt: Record<string, number>;
  /** The user is looking at this session's tab now. */
  markSeen: (sessionId: string, at?: number) => void;
  /** Record when sessions' dots changed (`null` = the dot cleared). */
  stampDotSince: (changes: Readonly<Record<string, number | null>>) => void;
  /** How each session's agent was launched, when CodeSense launched it. Kept
   *  across a respawn (Relaunch replays it); dropped with the tab. */
  launchBySession: Record<string, AgentLaunch>;
  /** A restart or relaunch in flight or failed, per session. It outlives the
   *  pane (a respawn remounts it) and a respawn's reset; dropped with the tab. */
  relaunchBySession: Record<string, RelaunchState>;
  /** The known agent CLI (`claude`, `codex`, …) the tab-process probe last found
   *  running under each session's shell. Read from the live process tree, so it
   *  holds for an agent no hook announced: one started by hand, one started
   *  before this app launched (a reattached session), one with no hooks at all. */
  processAgentBySession: Record<string, string>;
  /** Set (or with `null`, clear) the agent CLI the probe sees in a session. */
  setProcessAgent: (sessionId: string, cli: string | null, sentAt?: number) => void;
  recordLaunch: (sessionId: string, launch: AgentLaunch) => void;
  /** Set (or with `null`, clear) a session's restart/relaunch state. */
  setRelaunch: (sessionId: string, state: RelaunchState | null) => void;
  /** Fold a Tier-1 observation in, creating the session's record. */
  recordSignal: (sessionId: string, input: Extract<SessionAgentInput, { tier: 1 }>) => void;
  /** Fold a liveness observation in. Only a session that already has a record
   *  (an agent declared itself) takes one, so a tick never recreates a record
   *  dropped at a prune. `sentAt` is `agentInputSeq()` when the probe was
   *  sent: a result whose record took a newer Tier-1 input meanwhile is stale
   *  (a "gone" for the previous process must not end a turn that just opened)
   *  and is dropped. A session whose PTY exited takes none: nothing runs there. */
  recordProcess: (sessionId: string, signal: ProcessSignal, sentAt: number) => void;
  /** A shell's command-end status in the session: folded as the agent's exit
   *  when the record says the agent was what ran (`commandEndIsAgentExit`).
   *  Returns the record when it folded. */
  recordCommandEnd: (sessionId: string, code: number) => SessionAgentRecord | undefined;
  /** The session's own process exited: record how, and fold it into the
   *  record as the agent's exit when it is one (`ptyExitIsAgentExit`). Returns
   *  the session's record (or `null` without one), or `undefined` when the
   *  session was pruned (its tab closed) and the exit is ignored. */
  recordPtyExit: (
    sessionId: string,
    exit: { code: number | null; signal: string | null; requested: boolean },
  ) => SessionAgentRecord | null | undefined;
  /** Drop everything kept for sessions that are going away; a later
   *  `pty-exit` for one of them is ignored and drops any record recreated
   *  meanwhile. Only a `reset` (a respawn) reuses a pruned id. */
  remove: (sessionIds: Iterable<string>) => void;
  /** Drop a session's record, exit, dot times and dismissal because its id is about to
   *  host a new process (a respawn), whose exit must still be recorded. Its
   *  launch and its restart/relaunch state stay. */
  reset: (sessionId: string) => void;
}

/** Ingestion order: one monotonic seq across the tiers, which all arrive here. */
let seq = 0;

/** Sessions pruned whose `pty-exit` may still arrive (a closed tab's PTY is
 *  killed, or reaped later). Kept until reset; a bounded legacy defense. */
const pruned = new Set<string>();
const PRUNED_CAP = 1024;
const resetAt = new Map<string, number>();
let processAgentSeq = 0;
const processAgentVersion = new Map<string, number>();
const retiredActivity = new Map<string, ActivitySnapshot["identity"]>();
function retireActivity(id: string, snapshot: ActivitySnapshot | null | undefined): void {
  if (!snapshot) return;
  retiredActivity.delete(id);
  retiredActivity.set(id, snapshot.identity);
  if (retiredActivity.size > PRUNED_CAP)
    retiredActivity.delete(retiredActivity.keys().next().value!);
}

/**
 * Whether the session's host activity speaks for it: a snapshot that has observed its
 * agent (a hook reached the host) and whose agent has not exited. Only such a snapshot
 * shuts out the fallbacks (legacy hooks, MCP signals, the liveness floor); one that has
 * observed nothing, or whose process ended, must not block the signals that do speak.
 */
function activityIsAuthoritative(snapshot: ActivitySnapshot | null | undefined): boolean {
  return snapshot != null && snapshot.observed && snapshot.exit == null;
}

/** How many of `sessionId`'s agent's subagents run now, by its host (which counts them
 *  only for an observed, live invocation). */
export function selectSubagentsOpen(
  s: { activityBySession?: Readonly<Record<string, { snapshot: ActivitySnapshot | null }>> },
  sessionId: string,
): number {
  return s.activityBySession?.[sessionId]?.snapshot?.subagentsOpen ?? 0;
}

/** Reactive {@link selectSubagentsOpen}: 0 for no session. */
export function useSubagentsOpen(sessionId: string | null | undefined): number {
  return useAgentStateStore((s) => (sessionId ? selectSubagentsOpen(s, sessionId) : 0));
}

/** Shared admission gate for both records and dots, including in-flight probes. */
export function admitsAgentUpdate(sessionId: string, sentAt = Infinity): boolean {
  const current = useAgentStateStore.getState().bySession[sessionId];
  return (
    !activityIsAuthoritative(
      useAgentStateStore.getState().activityBySession[sessionId]?.snapshot,
    ) &&
    !pruned.has(sessionId) &&
    (resetAt.get(sessionId) ?? 0) <= sentAt &&
    (current?.t1?.seq ?? 0) <= sentAt
  );
}

function tombstone(sessionId: string): void {
  pruned.delete(sessionId);
  pruned.add(sessionId);
  if (pruned.size > PRUNED_CAP) {
    const oldest = pruned.values().next().value;
    if (oldest !== undefined) pruned.delete(oldest);
  }
}

function validActivityAge(age: unknown): age is number {
  return typeof age === "number" && Number.isFinite(age) && age >= 0;
}

/** Receipt is a clock anchor, never activity evidence by itself. */
function snapshotActivityAt(snapshot: ActivitySnapshot, receivedAt: number): number | undefined {
  return snapshot.observed &&
    validActivityAge(snapshot.activityAgeMs) &&
    Number.isFinite(receivedAt) &&
    receivedAt >= 0 &&
    receivedAt <= Number.MAX_SAFE_INTEGER
    ? Math.max(0, receivedAt - snapshot.activityAgeMs)
    : undefined;
}

function freshestActivity(
  known: number | undefined,
  observed: number | undefined,
): number | undefined {
  return known === undefined
    ? observed
    : observed === undefined
      ? known
      : Math.max(known, observed);
}

function isObservedHostTurn(snapshot: ActivitySnapshot): boolean {
  return (
    snapshot.observed &&
    snapshot.exit == null &&
    ["thinking", "working", "needs-permission"].includes(snapshot.state)
  );
}

export const useAgentStateStore = create<AgentStateStore>((set, get) => {
  const fold = (sessionId: string, input: SessionAgentInput, create: boolean, sentAt = Infinity) =>
    set((s) => {
      if (!admitsAgentUpdate(sessionId, sentAt)) return s;
      const current = s.bySession[sessionId];
      if (current === undefined && !create) return s;
      if (current?.t1 != null && current.t1.seq > sentAt) return s;
      const next = advanceSessionAgent(current, input, ++seq);
      if (next === current) return s;
      if (input.tier === 1) next.activityAt = Date.now();
      if (
        input.tier === 1 &&
        input.signal.kind === "claude-hook" &&
        (input.signal.event === "UserPromptSubmit" ||
          (input.signal.event === "PreToolUse" &&
            current !== undefined &&
            (endedByExit(current) ||
              (current.t1?.signal.kind === "claude-hook" &&
                ["Stop", "StopFailure", "SessionEnd"].includes(current.t1.signal.event)))))
      ) {
        // A tool hook needs an observed end before it can establish a new turn;
        // trailing cleanup and the first hook after reset lack that ordering.
        next.turnStartedSeq = seq;
      }
      const ended =
        endedByExit(next) ||
        (input.tier === 1 &&
          input.signal.kind === "claude-hook" &&
          input.signal.event === "SessionEnd");
      const processes = { ...s.processAgentBySession };
      if (ended) delete processes[sessionId];
      return {
        bySession: { ...s.bySession, [sessionId]: next },
        ...(ended ? { processAgentBySession: processes } : {}),
      };
    });

  type Keyed =
    | "outputAtBySession"
    | "completionSeqBySession"
    | "completionActivityBySession"
    | "activityBySession"
    | "bySession"
    | "exitBySession"
    | "stateSince"
    | "seenAt"
    | "launchBySession"
    | "relaunchBySession"
    | "processAgentBySession";
  const ALL: readonly Keyed[] = [
    "outputAtBySession",
    "completionSeqBySession",
    "completionActivityBySession",
    "activityBySession",
    "bySession",
    "exitBySession",
    "stateSince",
    "seenAt",
    "launchBySession",
    "relaunchBySession",
    "processAgentBySession",
  ];
  const drop = (sessionIds: Iterable<string>, keys: readonly Keyed[] = ALL) => {
    const ids = [...sessionIds];
    set((s) => {
      const next: Partial<Pick<AgentStateStore, Keyed>> = {};
      for (const key of keys) {
        const map = s[key] as Record<string, unknown>;
        if (!ids.some((id) => id in map)) continue;
        const copy = { ...map };
        for (const id of ids) delete copy[id];
        (next as Record<Keyed, unknown>)[key] = copy;
      }
      return Object.keys(next).length === 0 ? s : next;
    });
  };

  return {
    outputAtBySession: {},
    completionSeqBySession: {},
    completionActivityBySession: {},
    recordTaskCompletion: (sessionId, activityCompletion) => {
      let disposition = { accepted: false, historical: false, repeated: false };
      set((s) => {
        if (
          pruned.has(sessionId) ||
          !admitsActivityCompletion(activityCompletion, s.completionActivityBySession[sessionId])
        )
          return s;
        const historical = followsActivityCompletion(
          s.bySession[sessionId]?.hostTurn,
          activityCompletion,
        );
        const repeated = equalActivityCompletion(
          activityCompletion,
          s.completionActivityBySession[sessionId],
        );
        disposition = { accepted: true, historical, repeated };
        if (repeated) return s;
        const completionSeq = ++seq;
        const completionActivityBySession = { ...s.completionActivityBySession };
        if (activityCompletion) completionActivityBySession[sessionId] = activityCompletion;
        else delete completionActivityBySession[sessionId];
        const record = s.bySession[sessionId];
        // The new-turn snapshot may arrive before the completion event. Keep
        // its display without changing the retained task result or activity age.
        return {
          completionSeqBySession: { ...s.completionSeqBySession, [sessionId]: completionSeq },
          completionActivityBySession,
          ...(historical && record
            ? { bySession: { ...s.bySession, [sessionId]: { ...record, turnStartedSeq: ++seq } } }
            : {}),
        };
      });
      return disposition;
    },
    activityBySession: {},
    recordActivity: (snapshot, sentAt, receivedAt = Date.now()) => {
      const id = snapshot.session;
      if (pruned.has(id) || (resetAt.get(id) ?? 0) > sentAt) return false;
      const retired = retiredActivity.get(id);
      if (retired && sameActivityIdentity(retired, snapshot.identity)) return false;
      const activity = get().activityBySession[id];
      const previous = activity?.snapshot;
      const completionSeq = get().completionSeqBySession[id];
      const completion = get().completionActivityBySession[id];
      if (
        previous &&
        sameActivityIdentity(previous.identity, snapshot.identity) &&
        previous.revision > snapshot.revision
      )
        return false;
      if (
        previous &&
        sameActivityIdentity(previous.identity, snapshot.identity) &&
        previous.revision === snapshot.revision
      ) {
        const firstAge =
          !validActivityAge(previous.activityAgeMs) && validActivityAge(snapshot.activityAgeMs);
        const firstTool =
          typeof previous.toolOpen !== "boolean" && typeof snapshot.toolOpen === "boolean";
        const enriched =
          firstAge || firstTool
            ? {
                ...previous,
                ...(firstAge ? { activityAgeMs: snapshot.activityAgeMs } : {}),
                ...(firstTool ? { toolOpen: snapshot.toolOpen } : {}),
              }
            : previous;
        set((s) => {
          const record = s.bySession[id];
          const activityAt = freshestActivity(
            record?.activityAt,
            firstAge ? snapshotActivityAt(enriched, receivedAt) : undefined,
          );
          const clearTools =
            firstTool && snapshot.toolOpen === false && record?.openTools !== undefined;
          return {
            activityBySession: {
              ...s.activityBySession,
              [id]: { ...activity, snapshot: enriched, freshness: "current" },
            },
            ...(record && (activityAt !== record.activityAt || clearTools)
              ? {
                  bySession: {
                    ...s.bySession,
                    [id]: {
                      ...record,
                      activityAt,
                      openTools: clearTools ? undefined : record.openTools,
                    },
                  },
                }
              : {}),
          };
        });
        return true;
      }
      const ingestion = ++seq;
      const existing = get().bySession[id];
      const sameLife = !previous || sameActivityIdentity(previous.identity, snapshot.identity);
      const anchored =
        completionSeq === undefined || activity?.completionAnchorSeq === completionSeq;
      const hasMatchingCompletion =
        completion && sameActivityIdentity(completion.identity, snapshot.identity);
      const newHostTurn = hasMatchingCompletion
        ? isObservedHostTurn(snapshot) && followsActivityCompletion(snapshot, completion)
        : previous != null &&
          sameLife &&
          activity?.freshness === "current" &&
          anchored &&
          snapshot.turnGeneration > previous.turnGeneration &&
          isObservedHostTurn(snapshot);
      // Without matching owner proof (older/unavailable producers), the first
      // post-completion revision or recovery only anchors the conservative fallback.
      const nextActivity: SessionActivity = {
        snapshot,
        freshness: "current",
        ...(completionSeq === undefined ? {} : { completionAnchorSeq: completionSeq }),
      };
      if (sameLife && !snapshot.observed && snapshot.exit == null && existing?.t1 != null) {
        // The host has observed nothing yet, while a fallback has: keep what the fallback said.
        set((s) => ({
          activityBySession: { ...s.activityBySession, [id]: nextActivity },
        }));
        return true;
      }
      const record: SessionAgentRecord = {
        hostTurn: isObservedHostTurn(snapshot)
          ? {
              identity: snapshot.identity,
              turnGeneration: snapshot.turnGeneration,
              revision: snapshot.revision,
            }
          : sameLife
            ? existing?.hostTurn
            : undefined,
        turnStartedSeq: newHostTurn ? ingestion : existing?.turnStartedSeq,
        state:
          snapshot.state === "needs-permission" && snapshot.reason === "blocked"
            ? "blocked"
            : snapshot.state,
        activityAt: freshestActivity(
          sameLife ? existing?.activityAt : undefined,
          snapshotActivityAt(snapshot, receivedAt),
        ),
        openTools:
          sameLife && snapshot.state === "working" && snapshot.toolOpen !== false
            ? (existing?.openTools ??
              (existing?.t1?.signal.kind === "claude-hook" &&
              existing.t1.signal.event === "PreToolUse"
                ? 1
                : undefined))
            : undefined,
        agent: snapshot.exit ? null : snapshot.provider,
        t1: null,
        t2: snapshot.exit ? { seq: ingestion, signal: { kind: "exited", ...snapshot.exit } } : null,
      };
      set((s) => ({
        activityBySession: { ...s.activityBySession, [id]: nextActivity },
        bySession: { ...s.bySession, [id]: record },
        ...(snapshot.exit
          ? {
              processAgentBySession: Object.fromEntries(
                Object.entries(s.processAgentBySession).filter(([sessionId]) => sessionId !== id),
              ),
            }
          : {}),
        ...(!sameLife
          ? {
              outputAtBySession: Object.fromEntries(
                Object.entries(s.outputAtBySession).filter(([sessionId]) => sessionId !== id),
              ),
            }
          : {}),
      }));
      return true;
    },
    recordOutput: (sessionId, at = Date.now()) =>
      set((s) => {
        const record = s.bySession[sessionId];
        if (!Number.isFinite(at) || at < 0 || pruned.has(sessionId) || sessionId in s.exitBySession)
          return s;
        // Output can be high volume: bound updates to once a second. This is far
        // finer than the one-minute stall poll without rendering per byte chunk.
        const known = freshestActivity(record?.activityAt, s.outputAtBySession[sessionId]);
        if (known !== undefined && at - known < 1000) return s;
        return {
          outputAtBySession: { ...s.outputAtBySession, [sessionId]: at },
          ...(record
            ? { bySession: { ...s.bySession, [sessionId]: { ...record, activityAt: at } } }
            : {}),
        };
      }),
    activityUnavailable: (sessions, freshness = "unavailable") =>
      set((s) => {
        const next = { ...s.activityBySession };
        let changed = false;
        for (const id of sessions) {
          if (pruned.has(id) || next[id]?.freshness === freshness) continue;
          next[id] = { snapshot: next[id]?.snapshot ?? null, freshness };
          changed = true;
        }
        return changed ? { activityBySession: next } : s;
      }),
    bySession: {},
    exitBySession: {},
    stateSince: {},
    seenAt: {},
    stampDotSince: (changes) =>
      set((s) => {
        const stateSince = { ...s.stateSince };
        for (const [id, at] of Object.entries(changes)) {
          if (at === null) delete stateSince[id];
          else stateSince[id] = at;
        }
        return { stateSince };
      }),
    markSeen: (sessionId, at = Date.now()) =>
      set((s) => (s.seenAt[sessionId] === at ? s : { seenAt: { ...s.seenAt, [sessionId]: at } })),
    launchBySession: {},
    relaunchBySession: {},
    recordLaunch: (sessionId, launch) =>
      set((s) => ({ launchBySession: { ...s.launchBySession, [sessionId]: launch } })),
    setRelaunch: (sessionId, state) => {
      if (state === null) drop([sessionId], ["relaunchBySession"]);
      else set((s) => ({ relaunchBySession: { ...s.relaunchBySession, [sessionId]: state } }));
    },
    processAgentBySession: {},
    setProcessAgent: (sessionId, cli, sentAt) => {
      if ((cli !== null || sentAt !== undefined) && !admitsAgentUpdate(sessionId, sentAt)) return;
      if (cli === null) processAgentVersion.delete(sessionId);
      else processAgentVersion.set(sessionId, ++processAgentSeq);
      if (cli === null) drop([sessionId], ["processAgentBySession"]);
      else
        set((s) =>
          s.processAgentBySession[sessionId] === cli
            ? s
            : { processAgentBySession: { ...s.processAgentBySession, [sessionId]: cli } },
        );
    },
    recordSignal: (sessionId, input) => fold(sessionId, input, true),
    recordProcess: (sessionId, signal, sentAt) => {
      if (sessionId in get().exitBySession) return;
      fold(sessionId, { tier: 2, signal, source: "probe" }, false, sentAt);
    },
    recordCommandEnd: (sessionId, code) => {
      if (!admitsAgentUpdate(sessionId)) return undefined;
      const current = get().bySession[sessionId];
      if (current === undefined || !commandEndIsAgentExit(current, code)) return undefined;
      fold(sessionId, { tier: 2, signal: { kind: "exited", code }, source: "command-end" }, false);
      return get().bySession[sessionId];
    },
    recordPtyExit: (sessionId, { code, signal, requested }) => {
      if (activityIsAuthoritative(get().activityBySession[sessionId]?.snapshot)) return undefined;
      if (pruned.has(sessionId)) {
        // A closed tab's process ended: drop whatever its agent recreated since.
        drop([sessionId]);
        return undefined;
      }
      const exited = { kind: "exited", code, signal, requested } as const;
      const current = get().bySession[sessionId];
      const agentExit = current !== undefined && ptyExitIsAgentExit(current);
      if (agentExit) fold(sessionId, { tier: 2, signal: exited, source: "pty" }, false);
      const agentCrashed = agentExit && get().bySession[sessionId]?.state === "crashed";
      set((s) => ({
        exitBySession: {
          ...s.exitBySession,
          [sessionId]: {
            code,
            signal,
            requested,
            outcome: sessionExitOutcome({ code, signal, requested }, agentCrashed),
            at: Date.now(),
          },
        },
      }));
      return get().bySession[sessionId] ?? null;
    },
    remove: (sessionIds) => {
      const ids = [...sessionIds];
      for (const id of ids) {
        retireActivity(id, get().activityBySession[id]?.snapshot);
        tombstone(id);
        resetAt.delete(id);
        processAgentVersion.delete(id);
      }
      drop(ids);
    },
    reset: (sessionId) => {
      retireActivity(sessionId, get().activityBySession[sessionId]?.snapshot);
      pruned.delete(sessionId);
      resetAt.set(sessionId, ++seq);
      processAgentVersion.delete(sessionId);
      drop(
        [sessionId],
        [
          "outputAtBySession",
          "activityBySession",
          "bySession",
          "exitBySession",
          "stateSince",
          "seenAt",
          "processAgentBySession",
        ],
      );
    },
  };
});

/** The latest seq handed out; stamp a probe with it when it is sent. */
export function agentInputSeq(): number {
  return seq;
}

/** Prune entry point for the viewer store's tab/workspace/team teardown paths. */
export function pruneAgentSessions(sessionIds: Iterable<string>): void {
  useAgentStateStore.getState().remove(sessionIds);
}

/** What a respawn retires for one session once its new process started. */
export interface AgentSessionSnapshot {
  outputAt?: number;
  activity?: SessionActivity;
  processAgent?: string;
  processAgentVersion?: number;
  record: SessionAgentRecord | undefined;
  exit: SessionExit | undefined;
  stateSince: number | undefined;
  seenAt: number | undefined;
}

/** Keep aside what a respawn's reset would drop for `sessionId`. */
export function snapshotAgentSession(sessionId: string): AgentSessionSnapshot {
  const s = useAgentStateStore.getState();
  return {
    outputAt: s.outputAtBySession[sessionId],
    activity: s.activityBySession[sessionId],
    processAgent: s.processAgentBySession[sessionId],
    processAgentVersion: processAgentVersion.get(sessionId),
    record: s.bySession[sessionId],
    exit: s.exitBySession[sessionId],
    stateSince: s.stateSince[sessionId],
    seenAt: s.seenAt[sessionId],
  };
}

/**
 * A respawn's new process took the id: drop what the ended one left — each
 * part only while it is still what `snap` saw, so anything the new process
 * already reported (a quick exit, its agent's first status) stays.
 */
export function retireAgentSession(sessionId: string, snap: AgentSessionSnapshot): void {
  retireActivity(sessionId, snap.activity?.snapshot);
  pruned.delete(sessionId);
  resetAt.set(sessionId, ++seq);
  useAgentStateStore.setState((s) => {
    const next: Partial<AgentStateStore> = {};
    const retire = <
      K extends
        | "outputAtBySession"
        | "activityBySession"
        | "processAgentBySession"
        | "bySession"
        | "exitBySession"
        | "stateSince"
        | "seenAt",
    >(
      key: K,
      was: unknown,
    ) => {
      const map = s[key] as Record<string, unknown>;
      if (!(sessionId in map) || map[sessionId] !== was) return;
      const copy = { ...map };
      delete copy[sessionId];
      (next as Record<string, unknown>)[key] = copy;
    };
    retire("bySession", snap.record);
    retire("outputAtBySession", snap.outputAt);
    retire("activityBySession", snap.activity);
    if (processAgentVersion.get(sessionId) === snap.processAgentVersion) {
      retire("processAgentBySession", snap.processAgent);
      processAgentVersion.delete(sessionId);
    }
    retire("exitBySession", snap.exit);
    retire("stateSince", snap.stateSince);
    retire("seenAt", snap.seenAt);
    return next;
  });
}

/**
 * The dot each session's agent record dictates, for the sessions whose record
 * has a say (`agentRecordDot`); any other session's dot is its stored status.
 */
export type AgentDots = Readonly<Record<string, TerminalAiStatus>>;

/** What the projection reads: the records, and which sessions' own process exited. */
export type AgentDotSources = Pick<AgentStateStore, "bySession" | "exitBySession">;

const NO_AGENT_DOTS: AgentDots = Object.freeze({});
let dotsFor: AgentDotSources["bySession"] | null = null;
let exitsFor: AgentDotSources["exitBySession"] | null = null;
let dots: AgentDots = NO_AGENT_DOTS;

/**
 * Whether the record still speaks for its session. Once the session's own
 * process has exited, only a record that ended on that exit does: any other
 * (an MCP-only agent, a settled Claude whose shell then died) describes a life
 * that is over, and the stored status the exit left stands instead.
 */
function recordSpeaks(sessionId: string, rec: SessionAgentRecord, s: AgentDotSources): boolean {
  return !(sessionId in s.exitBySession) || endedByExit(rec);
}

/**
 * Project every record to its dot. The result keeps its identity until some
 * session's projected dot changes, so a subscriber to it re-renders only then
 * (a hook event that moves a record without moving its dot changes nothing).
 */
export function selectAgentDots(s: AgentDotSources): AgentDots {
  if (s.bySession === dotsFor && s.exitBySession === exitsFor) return dots;
  const next: Record<string, TerminalAiStatus> = {};
  let changed = false;
  let count = 0;
  for (const [sessionId, rec] of Object.entries(s.bySession)) {
    const dot = recordSpeaks(sessionId, rec, s) ? agentRecordDot(rec) : null;
    if (dot === null) continue;
    next[sessionId] = dot;
    count++;
    if (dots[sessionId] !== dot) changed = true;
  }
  dotsFor = s.bySession;
  exitsFor = s.exitBySession;
  if (changed || count !== Object.keys(dots).length) {
    dots = count === 0 ? NO_AGENT_DOTS : next;
  }
  return dots;
}

/**
 * Whether the user has seen `sessionId`'s current state: its tab was the
 * active tab of a shown terminal pane, with the window focused, at or after
 * its dot last changed. A session with no dot has no tracked state.
 */
export function isSessionSeen(s: SeenSources, sessionId: string): boolean {
  const since = s.stateSince[sessionId];
  const seen = s.seenAt[sessionId];
  return since !== undefined && seen !== undefined && seen >= since;
}

/** Mark a session seen now (its tab is the active, visible tab, window focused). */
export function markSessionSeen(sessionId: string): void {
  useAgentStateStore.getState().markSeen(sessionId);
}

/** What "seen" reads. */
export type SeenSources = Pick<AgentStateStore, "stateSince" | "seenAt">;

/** How `sessionId`'s own process ended, once it has — for the pane's ended/crashed bar. */
export function selectSessionExit(sessionId: string) {
  return (s: Pick<AgentStateStore, "exitBySession">): SessionExit | undefined =>
    s.exitBySession[sessionId];
}

/**
 * The agent in this session crashed and that crash still stands: it is the
 * record's latest word and the user has not dismissed it.
 */
export function selectAgentCrashStands(sessionId: string) {
  return (s: Pick<AgentStateStore, "bySession">): boolean => {
    const crash = agentCrashSeq(s.bySession[sessionId]);
    const projected = projectedSession(sessionId);
    return (
      crash !== null &&
      (!projected?.crashId ||
        projected.effectiveStatus === "crashed" ||
        projected.effectiveStatus === "failed")
    );
  };
}

/** Test-only: forget every pruned session. */
export function resetPrunedAgentSessionsForTests(): void {
  pruned.clear();
  resetAt.clear();
  processAgentVersion.clear();
  retiredActivity.clear();
}
