// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useSyncExternalStore } from "react";
import {
  projectedSession,
  useAgentListProjectionStore,
} from "@/features/viewer/lib/agent-list-projection";
import { activityQualifier, withActivityQualifiers } from "@/shared/lib/agent-activity-words";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { offlineMachineName } from "@/features/machines/lib/machine-offline";
import {
  selectNeedsYouKind,
  selectSessionAiStatus,
  type DotSources,
  type NeedsYouState,
  type PendingAsks,
} from "@/features/viewer/lib/session-status-signals";
export {
  selectNeedsYouKind,
  selectSessionAiStatus,
  type NeedsYouState,
  type PendingAsks,
} from "@/features/viewer/lib/session-status-signals";
import {
  useViewerStore,
  selectActiveTerminalTab,
  selectHiddenTerminalTabIndex,
  selectHiddenTerminalTabs,
  selectMinimizedTerminalTabIndex,
  selectTabBySessionId,
  selectTerminalTabIndex,
  selectWorkspaceTabs,
} from "@/shared/stores/viewer-store";
import {
  type AgentDots,
  isSessionSeen,
  type SeenSources,
  selectAgentCrashStands,
  selectAgentDots,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { activeWorkspaceIdForTeam } from "@/shared/lib/team-key";
import type { ManagedSessionState } from "@/shared/lib/managed-session";
import type { NeedsYouKind } from "@/shared/lib/terminal-ai-status";
import { exitWords, formatMarkLabel } from "@/shared/lib/terminal-dot-words";
import { sessionEndInfo } from "@/shared/lib/session-end";
import {
  countMarks,
  type MarkCounts,
  MARK_WORDS,
  markOfAgentStatus,
  rollupWords,
  type StatusMarkKind,
  topMark,
} from "@/shared/lib/status-mark";
import { subscribeToAge } from "@/shared/lib/age-clock";
import type {
  SessionProgress,
  TerminalAiStatus,
  TerminalTab,
  TerminalTabIndex,
  ViewerState,
} from "@/shared/stores/viewer-store";

function activeWorkspaceOf(s: ViewerState, teamKey: string): string {
  return activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
}

// ---------------------------------------------------------------------------
// Imperative getters — for non-React contexts (event handlers, store actions,
// callbacks that read one-shot state).
// ---------------------------------------------------------------------------

export function getActiveWorkspaceTabs(teamKey: string): TerminalTab[] {
  const s = useViewerStore.getState();
  return selectWorkspaceTabs(s, teamKey, activeWorkspaceOf(s, teamKey));
}

export function getActiveWorkspaceActiveTab(teamKey: string): string | undefined {
  const s = useViewerStore.getState();
  return selectActiveTerminalTab(s, teamKey, activeWorkspaceOf(s, teamKey));
}

// ---------------------------------------------------------------------------
// Reactive hooks — for React component subscriptions. Each re-renders only
// when the selected slice changes (Zustand Object.is equality).
// ---------------------------------------------------------------------------

export function useActiveWorkspaceTabs(teamKey: string): TerminalTab[] {
  return useViewerStore((s) => selectWorkspaceTabs(s, teamKey, activeWorkspaceOf(s, teamKey)));
}

export function useActiveWorkspaceActiveTab(teamKey: string): string | undefined {
  return useViewerStore((s) => selectActiveTerminalTab(s, teamKey, activeWorkspaceOf(s, teamKey)));
}

/** One session a mark stands for, with its tab's own exit when the scope holds its tab. */
export interface ScopeMember {
  sessionId: string;
  exit?: TerminalTab["exit"];
  machineId?: string;
}

/**
 * Every session of one workspace, open or minimized: the tabs shown on its tile
 * plus the ones Hidden from it (still running, listed under its tray chip).
 */
function workspaceMembers(s: ViewerState, teamKey: string, wsId: string): ScopeMember[] {
  return [
    ...selectWorkspaceTabs(s, teamKey, wsId),
    ...selectHiddenTerminalTabs(s, teamKey, wsId),
  ].map(memberOf);
}

function memberOf(t: TerminalTab): ScopeMember {
  return { sessionId: t.sessionId, exit: t.exit, machineId: t.origin?.machineId };
}

/** The sessions behind one mark, named the way its surface names them. */
export type DotScope =
  | { sessionIds: readonly string[] }
  | { members: readonly ScopeMember[] }
  | { teamKey: string; wsIds: readonly string[] }
  | { teamKey: string; hiddenOf: string }
  | { teamKey: string; story: { filePath: string; storyId: string | undefined } };

function scopeMembers(s: ViewerState, scope: DotScope): readonly ScopeMember[] {
  if ("members" in scope)
    return scope.members.map((member) => ({
      ...member,
      machineId: member.machineId ?? selectTabBySessionId(s, member.sessionId)?.origin?.machineId,
    }));
  if ("sessionIds" in scope) {
    return scope.sessionIds.map((sessionId) => ({
      sessionId,
      exit: selectTabBySessionId(s, sessionId)?.exit,
      machineId: selectTabBySessionId(s, sessionId)?.origin?.machineId,
    }));
  }
  if ("story" in scope) {
    const ws = findStoryWorkspace(s, scope.teamKey, scope.story.filePath, scope.story.storyId);
    return ws ? workspaceMembers(s, scope.teamKey, ws.id) : [];
  }
  if ("wsIds" in scope) return scope.wsIds.flatMap((w) => workspaceMembers(s, scope.teamKey, w));
  return selectHiddenTerminalTabs(s, scope.teamKey, scope.hiddenOf).map(memberOf);
}

type AgentState = ReturnType<typeof useAgentStateStore.getState>;

/** What a session's mark reads from the agent-state store. */
type MarkAgentSources = Pick<AgentState, "bySession" | "exitBySession" | "stateSince"> &
  Partial<
    Pick<
      AgentState,
      | "activityBySession"
      | "outputAtBySession"
      | "completionSeqBySession"
      | "processAgentBySession"
      | "launchBySession"
    >
  >;

/** One session's mark and what its words need: the live status behind it, since
 *  when, and how its process ended. */
export interface SessionMarkFacts {
  mark: StatusMarkKind;
  status?: TerminalAiStatus;
  /** It waits on a question (`ask_user`) rather than an approval. */
  asking?: boolean;
  words?: string;
  qualifier?: string;
  since: number | null;
  exit?: string | null;
  /** Its agent's subagents running now. */
  subagents?: number;
}

/**
 * The ONE read of a session's mark. Its own process's end wins (a crash, or a
 * clean exit CodeSense did not ask for); then a question it waits on; otherwise
 * its live status ({@link selectSessionAiStatus}), where a finished turn reads
 * `idle`. Null when the session has no status at all (a plain shell).
 */
export function selectSessionMark(
  s: DotSources,
  dots: AgentDots,
  agent: MarkAgentSources,
  member: ScopeMember,
  pendingAsks: PendingAsks = {},
  outcomes: Readonly<Record<string, ManagedSessionState>> = {},
  now = Date.now(),
): SessionMarkFacts | null {
  const { sessionId } = member;
  const end = sessionEndInfo(
    agent.exitBySession[sessionId],
    member.exit,
    selectAgentCrashStands(sessionId)(agent),
  );
  if (end) {
    // The tab's own end, in the ended bar's words, said once: no age.
    const exit = agent.exitBySession[sessionId] ?? member.exit;
    return { mark: end.end, since: null, exit: exit ? exitWords(exit) : null };
  }
  if (offlineMachineName(member.machineId)) {
    // The agent can end while its containing shell remains alive. Derive that
    // known end before applying availability, just as the agents list does.
    const known = selectSessionMark(
      s,
      dots,
      agent,
      { ...member, machineId: undefined },
      pendingAsks,
      outcomes,
      now,
    );
    return known?.mark === "ended" || known?.mark === "crashed"
      ? known
      : { mark: "offline", since: null };
  }
  const status = selectSessionAiStatus(s, dots, sessionId);
  const activity = agent.activityBySession?.[sessionId];
  const qualifier =
    activity?.snapshot ||
    agent.bySession[sessionId] ||
    outcomes[sessionId] ||
    agent.processAgentBySession?.[sessionId]
      ? activityQualifier(activity)
      : undefined;
  if (Object.values(pendingAsks).some((a) => a.sessionId === sessionId)) {
    return {
      mark: "question",
      asking: true,
      since: agent.stateSince[sessionId] ?? null,
      qualifier,
    };
  }
  const projected = projectedSession(sessionId);
  if (projected?.hasAgent === false) return null;
  if (!projected && status === undefined && outcomes[sessionId] === undefined && !qualifier)
    return null;
  const derived = projected?.effectiveStatus ?? "unknown";
  const facts: SessionMarkFacts = {
    mark: markOfAgentStatus(derived),
    qualifier: activityQualifier(agent.activityBySession?.[sessionId], markOfAgentStatus(derived)),
    status,
    since: projected?.statusSinceMs ?? agent.stateSince[sessionId] ?? null,
  };
  if (derived === "dropped") facts.words = "Needs you";
  // A turn that ended while its subagents run reads as their work, never Ready.
  if (projected?.subagents) facts.subagents = projected.subagents;
  if (status === "crashed") facts.exit = exitOf(agent, sessionId, member.exit);
  return facts;
}

/** A crashed agent's exit in words: its own exit as its record holds it, else its
 *  process's (`pty-exit`), else its tab's layout entry. */
function exitOf(
  agent: Pick<AgentState, "bySession" | "exitBySession">,
  sessionId: string,
  layoutExit: TerminalTab["exit"] | undefined,
): string | null {
  const t2 = agent.bySession[sessionId]?.t2?.signal;
  const own = t2?.kind === "exited" ? exitWords(t2) : null;
  const pty = agent.exitBySession[sessionId] ?? layoutExit;
  return own ?? (pty ? exitWords(pty) : null);
}

/** What a mark over several sessions shows: the most urgent member's mark
 *  ({@link topMark}), how many members show each mark, and — when only one
 *  member has a mark — that member's own facts, for its words and age. */
export interface ScopeMarkFacts {
  qualifiers?: string[];
  mark?: StatusMarkKind;
  counts: MarkCounts;
  single?: SessionMarkFacts;
}

/** {@link ScopeMarkFacts} for a scope's members, as a string key: a subscriber
 *  re-renders only when the mark, a count or the single member's facts change. */
export function selectScopeMarkKey(
  s: DotSources,
  dots: AgentDots,
  agent: MarkAgentSources,
  members: readonly ScopeMember[],
  pendingAsks: PendingAsks = {},
  outcomes: Readonly<Record<string, ManagedSessionState>> = {},
  now = Date.now(),
): string {
  return JSON.stringify(selectScopeMarkFacts(s, dots, agent, members, pendingAsks, outcomes, now));
}

/** {@link ScopeMarkFacts} for a scope's members: the one roll-up every mark draws. */
function selectScopeMarkFacts(
  s: DotSources,
  dots: AgentDots,
  agent: MarkAgentSources,
  members: readonly ScopeMember[],
  pendingAsks: PendingAsks,
  outcomes: Readonly<Record<string, ManagedSessionState>> = {},
  now = Date.now(),
): ScopeMarkFacts {
  const marks = members.flatMap((m) => {
    const f = selectSessionMark(s, dots, agent, m, pendingAsks, outcomes, now);
    return f ? [f] : [];
  });
  const facts: ScopeMarkFacts = {
    mark: topMark(marks.map((f) => f.mark)),
    counts: countMarks(marks.map((f) => f.mark)),
  };
  const qualifiers = [...new Set(marks.flatMap((f) => (f.qualifier ? [f.qualifier] : [])))];
  if (qualifiers.length) facts.qualifiers = qualifiers;
  if (marks.length === 1) facts.single = marks[0];
  return facts;
}

/** A story as a story scope names it. */
export type ScopeStory = { filePath: string; storyId: string | undefined };

/**
 * Each story's mark, by file path, for the stories that have one: the same roll-up its
 * workspace pill draws ({@link useScopeMark} over `{ teamKey, story }`). A story with no
 * workspace, or whose workspace runs no agent, is absent.
 */
export function selectStoryMarks(
  s: ViewerState,
  dots: AgentDots,
  agent: MarkAgentSources,
  teamKey: string,
  stories: readonly ScopeStory[],
  pendingAsks: PendingAsks = {},
  outcomes: Readonly<Record<string, ManagedSessionState>> = {},
  now = Date.now(),
): Record<string, StatusMarkKind> {
  const out: Record<string, StatusMarkKind> = {};
  if (!s.workspaces?.[teamKey]?.length) return out;
  for (const story of stories) {
    // Machine availability also marks ordinary remote shells in terminal chrome.
    // Story agent lenses must only roll up sessions that have a status of their
    // own; disconnecting a bare shell cannot create an agent on that story.
    const members = scopeMembers(s, { teamKey, story }).flatMap((member) => {
      if (!offlineMachineName(member.machineId)) return [member];
      const id = member.sessionId;
      const hasAgent =
        agent.bySession[id] !== undefined ||
        dots[id] !== undefined ||
        s.aiStatusBySession[id] !== undefined ||
        outcomes[id] !== undefined ||
        agent.activityBySession?.[id]?.snapshot?.observed === true ||
        Object.values(pendingAsks).some((ask) => ask.sessionId === id);
      if (hasAgent) return [member];
      // A bare shell's known exit still has its Ended/Crash mark, independent
      // of connectivity. A live bare shell has no story agent mark at all.
      const shell = { ...member, machineId: undefined };
      return selectSessionMark(s, dots, agent, shell, pendingAsks, outcomes, now) ? [shell] : [];
    });
    if (members.length === 0) continue;
    const { mark } = selectScopeMarkFacts(s, dots, agent, members, pendingAsks, outcomes, now);
    if (mark) out[story.filePath] = mark;
  }
  return out;
}

/** Reactive {@link selectStoryMarks}: re-renders only when some story's mark changes. */
export function useStoryMarks(
  teamKey: string,
  stories: readonly ScopeStory[],
): Readonly<Record<string, StatusMarkKind>> {
  const key = useManagedDotSelector((s, dots, agent, { pendingAsks, states: outcomes }) =>
    s
      ? JSON.stringify(selectStoryMarks(s, dots, agent, teamKey, stories, pendingAsks, outcomes))
      : "{}",
  );
  return useMemo(() => JSON.parse(key) as Record<string, StatusMarkKind>, [key]);
}

/** The workspaces of `stories` (each once, in story order): what an epic's mark stands for. */
export function selectStoryWorkspaceIds(
  s: { workspaces?: Record<string, StoryWorkspace[]> },
  teamKey: string,
  stories: readonly ScopeStory[],
): string[] {
  const ids = new Set<string>();
  if (!s.workspaces?.[teamKey]?.length) return [];
  for (const story of stories) {
    const ws = findStoryWorkspace(s, teamKey, story.filePath, story.storyId);
    if (ws) ids.add(ws.id);
  }
  return [...ids];
}

/** Reactive {@link selectStoryWorkspaceIds}, as a stable array. */
export function useStoryWorkspaceIds(teamKey: string, stories: readonly ScopeStory[]): string[] {
  const key = useViewerStore((s) => selectStoryWorkspaceIds(s, teamKey, stories).join("\n"));
  return useMemo(() => (key ? key.split("\n") : []), [key]);
}

/** One tab a story's mark stands for, named as its tab is. */
export interface StoryTab extends ScopeMember {
  name: string;
}

/** The tabs of a story's workspace (shown and Hidden), as {@link scopeMembers} reads them. */
export function useStoryTabs(teamKey: string, story: ScopeStory): StoryTab[] {
  const key = useViewerStore((s) => {
    const ws = findStoryWorkspace(s, teamKey, story.filePath, story.storyId);
    if (!ws) return "[]";
    const tabs = [
      ...selectWorkspaceTabs(s, teamKey, ws.id),
      ...selectHiddenTerminalTabs(s, teamKey, ws.id),
    ];
    return JSON.stringify(
      tabs.map((t) => ({ sessionId: t.sessionId, exit: t.exit, name: t.name })),
    );
  });
  return useMemo(() => JSON.parse(key) as StoryTab[], [key]);
}

/** A mark over one scope, with its words: the tooltip's (with the state's age, for
 *  a single session) and the accessible name's (no age, joined by commas). */
export interface ScopeMark {
  mark: StatusMarkKind | undefined;
  label: string;
  words: string;
}

function scopeMarkWords(
  facts: ScopeMarkFacts,
  ageMs: number | undefined,
  separator: string,
): string {
  if (facts.single) return formatMarkLabel({ ...facts.single, ageMs }, separator);
  if (facts.mark === undefined) return MARK_WORDS.ready;
  return withActivityQualifiers(
    rollupWords(facts.counts, separator),
    facts.qualifiers ?? [],
    separator,
  );
}

/**
 * The mark for one surface over the sessions it stands for — a tab, a tile, a
 * workspace pill, an epic, a tray "+N", a tile's Hidden tabs. Its words name
 * what it stands for: one session's state and age ("Needs approval · 2m"), or
 * a roll-up's members by state ("2 need you · 1 working"). The age is kept
 * current by the shared age clock, so the host re-renders exactly when the
 * phrase changes.
 */
export function useScopeMark(scope: DotScope): ScopeMark {
  const key = useManagedDotSelector((s, dots, agent, { pendingAsks, states: outcomes }) =>
    s ? selectScopeMarkKey(s, dots, agent, scopeMembers(s, scope), pendingAsks, outcomes) : "",
  );
  const facts = useMemo<ScopeMarkFacts>(
    () => (key ? (JSON.parse(key) as ScopeMarkFacts) : { counts: {} }),
    [key],
  );
  const since = facts.single?.since ?? null;
  const subscribe = useCallback(
    (onChange: () => void) => (since === null ? () => {} : subscribeToAge(since, onChange)),
    [since],
  );
  const phrase = useCallback(
    () => scopeMarkWords(facts, since === null ? undefined : Date.now() - since, " · "),
    [facts, since],
  );
  const label = useSyncExternalStore(subscribe, phrase);
  return { mark: facts.mark, label, words: scopeMarkWords(facts, undefined, ", ") };
}

/** Managed outcomes and asks obey the same scalar selection as the other
 * sources: an unrelated session must not rerender this scope. */
function useManagedDotSelector<T>(
  select: (
    s: ViewerState,
    dots: AgentDots,
    agent: AgentState,
    managed: ReturnType<typeof useManagedSessionStore.getState>,
  ) => T,
): T {
  useManagedSessionStore((managed) => {
    const s = useViewerStore.getState?.();
    const agent = useAgentStateStore.getState();
    return s ? select(s, selectAgentDots(agent), agent, managed) : undefined;
  });
  return useDotSelector((s, dots, agent) =>
    select(s, dots, agent, useManagedSessionStore.getState()),
  );
}

/**
 * Subscribe to a value derived from the viewer store (tabs, stored status) and
 * the agent-state store (records' projected dots, seen). `select` must return a
 * scalar (or a string key): it runs as a selector on each source store, so the host
 * re-renders only when the value itself changes, whichever store moved — a
 * hook event in one tab re-renders no unrelated dot.
 */
export function useDotSelector<T>(
  select: (s: ViewerState, dots: AgentDots, agent: AgentState) => T,
): T {
  useAgentListProjectionStore(() => {
    const s = useViewerStore.getState?.();
    const agent = useAgentStateStore.getState();
    return s ? select(s, selectAgentDots(agent), agent) : undefined;
  });
  useMachinesStore(() => {
    const s = useViewerStore.getState?.();
    const agent = useAgentStateStore.getState();
    return s ? select(s, selectAgentDots(agent), agent) : undefined;
  });
  useAgentStateStore((agent) => {
    const s = useViewerStore.getState?.();
    return s ? select(s, selectAgentDots(agent), agent) : undefined;
  });
  return useViewerStore((s) => {
    const agent = useAgentStateStore.getState();
    return select(s, selectAgentDots(agent), agent);
  });
}

/**
 * Keep `stateSince` — when each session's fine mark last changed — in step with the
 * projected mark ({@link selectSessionMark}), whichever store moved it: a
 * record transition or a stored-only write (the idle-output needs-you, a plain
 * shell's status). A needs-you that appears over a seen state is unseen again.
 * Shared by every caller (reference-counted). Returns the stop.
 */
export function watchDotSince(): () => void {
  watchers++;
  if (watchers === 1) {
    syncDotSince();
    stopWatching = [
      useViewerStore.subscribe(syncDotSince),
      useAgentStateStore.subscribe(syncDotSince),
      useManagedSessionStore.subscribe(syncDotSince),
    ];
    stopWatching.push(useAgentListProjectionStore.subscribe(syncDotSince));
  }
  return () => {
    watchers--;
    if (watchers === 0) for (const stop of stopWatching.splice(0)) stop();
  };
}

let watchers = 0;
let stopWatching: (() => void)[] = [];
/** The dots last stamped — kept across watcher restarts, so a restart re-stamps
 *  only dots that actually changed and never un-sees a seen one. */
let lastDots: Record<string, StatusMarkKind> = {};
let lastSources: readonly unknown[] | null = null;

function syncDotSince(): void {
  const s = useViewerStore.getState();
  const agent = useAgentStateStore.getState();
  const dots = selectAgentDots(agent);
  const { states: outcomes, pendingAsks } = useManagedSessionStore.getState();
  const now = Date.now();
  const sources = [
    s.aiStatusBySession,
    dots,
    agent.bySession,
    agent.exitBySession,
    agent.activityBySession,
    agent.outputAtBySession,
    agent.completionSeqBySession,
    s.progressTickBySession,
    outcomes,
    pendingAsks,
    useAgentListProjectionStore.getState().sessions,
  ];
  if (lastSources?.every((source, index) => source === sources[index])) return;
  lastSources = sources;
  const next: Record<string, StatusMarkKind> = {};
  const since: Record<string, number | null> = {};
  const ids = new Set([
    ...Object.keys(s.aiStatusBySession),
    ...Object.keys(dots),
    ...Object.keys(agent.bySession),
    ...Object.keys(outcomes),
    ...Object.values(pendingAsks).map((ask) => ask.sessionId),
  ]);
  for (const id of ids) {
    const facts = selectSessionMark(s, dots, agent, { sessionId: id }, pendingAsks, outcomes, now);
    if (facts) {
      next[id] = facts.mark;
      since[id] = facts.mark === "stalled" ? facts.since : now;
    }
  }
  const changes: Record<string, number | null> = {};
  let changed = false;
  for (const id of new Set([...Object.keys(lastDots), ...Object.keys(next)])) {
    if (lastDots[id] === next[id]) continue;
    changes[id] = next[id] === undefined ? null : since[id];
    changed = true;
  }
  lastDots = next;
  if (changed) agent.stampDotSince(changes);
}

/** Test-only: forget what the dot-start watcher last saw. */
export function resetDotSinceForTests(): void {
  lastDots = {};
  lastSources = null;
}

/** The tabs Hidden from one workspace's tile (listed under its tray chip). */
export function useHiddenTerminalTabs(teamKey: string, wsId: string): TerminalTab[] {
  return useViewerStore((s) => selectHiddenTerminalTabs(s, teamKey, wsId));
}

/**
 * The ONE read of a session's dot status. The per-tab dot, the tile's "+N"
 * badge, its roll-up and the tray chips all go through this, so they can never
 * disagree. It projects the session's agent-state record when it has one
 * (`thinking` apart from `running`, `crashed` apart from `error`), and falls
 * back to the stored status for a session without one (a plain shell). A
 * stored `needs-permission` outranks the record's `running` / `thinking`.
 */

// ---------------------------------------------------------------------------
// Needs you — the ONE definition, anchored to the dot
// ---------------------------------------------------------------------------

/** Pending blocking questions (`ask_user`), keyed by correlation id. */

/** The dot states that mean "a human must act": blocked, dead, or failed. */
export const NEEDS_YOU_STATUSES: ReadonlySet<TerminalAiStatus> = new Set<TerminalAiStatus>([
  "needs-permission",
  "crashed",
  "error",
]);

export type { NeedsYouKind };

/** What "needs you" reads: the dot's sources, the pending questions, and the
 *  agents' own `signal_done` outcomes. */

/**
 * Why `sessionId` needs you, or `null` when it does not. A pending `ask_user`
 * is a `question`; a crash its dot shows is `crashed`; then what the agent
 * said of itself through `signal_done`: `failed`, or `needs-attention`
 * (→ `attention`); then the dot: `error` (→ `failed`) or `needs-permission`
 * (→ `permission`). Otherwise a state the dot does not show (a `done`, a plain
 * shell's exit the dot does not read as crashed) is not needs-you. The agents
 * list reads the same outcomes, so a session counts the same everywhere.
 *
 * Seen never removes a session from needs-you: a crash or a failure stays
 * until the agent moves on (a new turn changes the dot), its session ends
 * (the tab closes or respawns), or the crash is dismissed from its pane.
 * Seen only stops the dot pulsing and the alert re-notifying.
 */

/**
 * The dot-anchored predicate: the session's dot is `needs-permission`,
 * `crashed` or `error`, or a question from it is pending — seen or not
 * (see {@link selectNeedsYouKind} for how long each lasts).
 */
export function selectSessionNeedsYou(state: NeedsYouState, sessionId: string): boolean {
  return selectNeedsYouKind(state, sessionId) !== null;
}

/** One session that needs you, as the needs-you list reports it. */
export interface SessionNeedsYou {
  sessionId: string;
  teamKey: string;
  workspaceId: string;
  tabId: string;
  /** Not on screen: a tab Hidden from its tile. The jump reveals it. */
  hidden: boolean;
  kind: NeedsYouKind;
  /** The user has seen its current state: it no longer pulses or notifies. */
  seen: boolean;
}

/**
 * Whether a needs-you row is still news: a question or a permission prompt
 * always is (it blocks); a crash or an error only until the user has seen it.
 * A seen crash or error stays on the list (its agent is still stuck), but the
 * jump and the Jump pill move past it.
 */
export function isNeedsYouNews(row: Pick<SessionNeedsYou, "kind" | "seen">): boolean {
  return (
    !row.seen || row.kind === "question" || row.kind === "permission" || row.kind === "blocked"
  );
}

/** What the needs-you list walks: the terminal layout's tab indexes, the
 *  workspace records, and seen. */
export interface NeedsYouListState extends NeedsYouState {
  /** The tabs shown on every open workspace (`selectTerminalTabIndex`). */
  openTabs: TerminalTabIndex;
  /** The tabs Hidden from a tile, open or minimized (`selectHiddenTerminalTabIndex`). */
  hiddenTabs: TerminalTabIndex;
  /** Every tab of a minimized workspace, Hidden ones included
   *  (`selectMinimizedTerminalTabIndex`). */
  minimizedTabs: TerminalTabIndex;
  /** The open workspace records. */
  workspaces: Readonly<Record<string, readonly { id: string }[]>>;
  /** When each dot began (`stateSince`, orders the list) and was seen. */
  seen: SeenSources;
}

/**
 * Every session that needs you ({@link selectNeedsYouKind}), seen or not,
 * in `teamKey`, or in every team when absent/null: open tabs, Hidden tabs
 * (`hidden: true`) and the tabs of tray-minimized workspaces, each session
 * once. A Hidden tab whose workspace has neither a record nor shown tabs is
 * skipped (a jump there would go nowhere). Oldest first, by when its dot
 * began; a session with no tracked start sorts last, in walk order.
 */
export function selectSessionsNeedingYou(
  state: NeedsYouListState,
  teamKey?: string | null,
): SessionNeedsYou[] {
  const teams = teamKey
    ? [teamKey]
    : [
        ...new Set([
          ...Object.keys(state.openTabs),
          ...Object.keys(state.hiddenTabs),
          ...Object.keys(state.minimizedTabs),
        ]),
      ];
  const out: SessionNeedsYou[] = [];
  const listed = new Set<string>();
  const visit = (
    team: string,
    workspaceId: string,
    tabs: readonly TerminalTab[],
    hidden: (tab: TerminalTab) => boolean,
  ) => {
    for (const tab of tabs) {
      if (listed.has(tab.sessionId)) continue;
      const kind = selectNeedsYouKind(state, tab.sessionId);
      if (kind === null) continue;
      const seen = isSessionSeen(state.seen, tab.sessionId);
      listed.add(tab.sessionId);
      out.push({
        sessionId: tab.sessionId,
        teamKey: team,
        workspaceId,
        tabId: tab.id,
        hidden: hidden(tab),
        kind,
        seen,
      });
    }
  };
  for (const team of teams) {
    const records = new Set((state.workspaces[team] ?? []).map((w) => w.id));
    const hiddenOf = (wsId: string) =>
      new Set((state.hiddenTabs[team]?.[wsId] ?? []).map((t) => t.sessionId));
    for (const [wsId, tabs] of Object.entries(state.openTabs[team] ?? {})) {
      if (!records.has(wsId)) continue;
      visit(team, wsId, tabs, () => false);
    }
    for (const [wsId, tabs] of Object.entries(state.minimizedTabs[team] ?? {})) {
      const hidden = hiddenOf(wsId);
      visit(team, wsId, tabs, (t) => hidden.has(t.sessionId));
    }
    for (const [wsId, tabs] of Object.entries(state.hiddenTabs[team] ?? {})) {
      const resolvable =
        records.has(wsId) ||
        state.minimizedTabs[team]?.[wsId] !== undefined ||
        (state.openTabs[team]?.[wsId]?.length ?? 0) > 0;
      if (resolvable) visit(team, wsId, tabs, () => true);
    }
  }
  const since = (id: string) => state.seen.stateSince[id] ?? Number.POSITIVE_INFINITY;
  return out
    .map((row, i) => ({ row, i }))
    .sort((a, b) => since(a.row.sessionId) - since(b.row.sessionId) || a.i - b.i)
    .map(({ row }) => row);
}

function needsYouListState(
  s: ViewerState,
  agent: AgentState,
  pendingAsks: PendingAsks,
  outcomes: Readonly<Record<string, ManagedSessionState>>,
): NeedsYouListState {
  return {
    aiStatusBySession: s.aiStatusBySession ?? {},
    openTabs: selectTerminalTabIndex(s),
    hiddenTabs: selectHiddenTerminalTabIndex(s),
    minimizedTabs: selectMinimizedTerminalTabIndex(s),
    workspaces: s.workspaces ?? {},
    agentDots: selectAgentDots(agent),
    pendingAsks,
    outcomes,
    crashes: agent,
    seen: agent,
  };
}

/** The live {@link NeedsYouListState}, read imperatively from the three stores. */
export function getNeedsYouState(): NeedsYouListState {
  const managed = useManagedSessionStore.getState();
  return needsYouListState(
    useViewerStore.getState(),
    useAgentStateStore.getState(),
    managed.pendingAsks,
    managed.states,
  );
}

/** Imperative {@link selectSessionNeedsYou} (next needs-you). */
export function getSessionNeedsYou(sessionId: string): boolean {
  return selectSessionNeedsYou(getNeedsYouState(), sessionId);
}

/** Reactive {@link selectSessionNeedsYou} for one session. */
export function useSessionNeedsYou(sessionId: string): boolean {
  const pendingAsks = useManagedSessionStore((m) => m.pendingAsks);
  const outcomes = useManagedSessionStore((m) => m.states);
  return useDotSelector((s, _dots, agent) =>
    s
      ? selectSessionNeedsYou(needsYouListState(s, agent, pendingAsks, outcomes), sessionId)
      : false,
  );
}

/**
 * Reactive {@link selectSessionsNeedingYou}. Computed inside the store
 * selectors to a string key, so the host re-renders only when the list itself
 * changes — not on every dot-start stamp or unrelated tick.
 */
export function useNeedsYouList(teamKey?: string | null): SessionNeedsYou[] {
  const pendingAsks = useManagedSessionStore((m) => m.pendingAsks);
  const outcomes = useManagedSessionStore((m) => m.states);
  const key = useDotSelector((s, _dots, agent) =>
    s
      ? JSON.stringify(
          selectSessionsNeedingYou(needsYouListState(s, agent, pendingAsks, outcomes), teamKey),
        )
      : "[]",
  );
  return useMemo(() => (key ? (JSON.parse(key) as SessionNeedsYou[]) : []), [key]);
}

/**
 * SW-FX-52: live agent status for ONE session. Scalar selector — a status tick
 * for any OTHER session leaves this value unchanged (Object.is), so only the dot
 * bound to `sessionId` re-renders. Used by the per-tab dots.
 */
export function useSessionAiStatus(sessionId: string): TerminalAiStatus | undefined {
  return useDotSelector((s, dots) => selectSessionAiStatus(s, dots, sessionId));
}

/**
 * The session the user is looking at while `teamKey` is the active team, or `undefined`
 * when none: the active tab of the team's active workspace, while the CLI pane is shown
 * and that workspace is not minimized. (Window focus is the caller's to add.)
 */
export function selectWatchedSessionId(s: ViewerState, teamKey: string): string | undefined {
  if (!s.terminalVisible) return undefined;
  const wsId = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
  if (s.softClosedWorkspaces[teamKey]?.[wsId]) return undefined;
  const tabId = selectActiveTerminalTab(s, teamKey, wsId);
  return selectWorkspaceTabs(s, teamKey, wsId).find((t) => t.id === tabId)?.sessionId;
}

/** SW-FX-52: imperative read of one session's status (non-React callers). */
export function getSessionAiStatus(sessionId: string): TerminalAiStatus | undefined {
  return selectSessionAiStatus(
    useViewerStore.getState(),
    selectAgentDots(useAgentStateStore.getState()),
    sessionId,
  );
}

/**
 * SW-AM-4: retained live progress for ONE session. Scalar Zustand selector — a
 * progress tick for any OTHER session leaves this value unchanged (Object.is), so
 * only the Agents-panel row bound to `sessionId` re-renders. Mirrors
 * {@link useSessionAiStatus}. Returns `undefined` for a session that has emitted
 * no progress, so a no-progress row renders exactly as before.
 */
export function useSessionProgress(sessionId: string): SessionProgress | undefined {
  return useViewerStore((s) => s.progressBySession[sessionId]);
}

type StoryWorkspace = { id: string; storyFilePath?: string; storyId?: string };

/** A story's workspace, matched by file path, else by story id. */
function findStoryWorkspace(
  s: { workspaces?: Record<string, StoryWorkspace[]> },
  teamKey: string,
  filePath: string,
  storyId: string | undefined,
): StoryWorkspace | undefined {
  return s.workspaces?.[teamKey]?.find(
    (w) => w.storyFilePath === filePath || (!!storyId && w.storyId === storyId),
  );
}

/** How many times each tab started a new process in place: a tab's pane is
 *  keyed by it, so a respawn remounts the pane on the new process. */
export function useTerminalLives(): Record<string, number> {
  return useViewerStore((s) => s.terminalLives);
}
