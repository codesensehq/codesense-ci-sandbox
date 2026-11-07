// SPDX-License-Identifier: MIT
import { isRunningAgent } from "@/features/viewer/lib/hand-agents";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { create } from "zustand";

/** How a tab's inbox is drawn: conversations, a dense log, or a sequence diagram. */
export type InboxLens = "threads" | "spool" | "wire";
export const DEFAULT_INBOX_LENS: InboxLens = "threads";

/** One session's rail state. Present → the rail is open on that session. */
export interface AgentInboxRailTarget {
  /** Bumped each time someone asks to write: the rail's composer takes focus. */
  composeRequest: number;
  groupRequest?: number;
  groupHandled?: number;
  /**
   * The last request the rail acted on. Kept here, not in the rail, so a remount (the tab
   * moved to another pane) never acts on an old request again.
   */
  composeHandled?: number;
}

interface AgentInboxRailState {
  /** Keyed by terminal `sessionId`. Kept for the app's lifetime only. */
  bySession: Record<string, AgentInboxRailTarget | undefined>;
  /**
   * SW-IB1-B2: per-tab remembered push-pane width in CSS px, keyed by the
   * terminal tab's stable `tab.id` (NOT `sessionId`). This way a resize
   * survives the tab's session changing mid-tab (e.g. a re-attach that mints
   * a new PTY session id), but is still bounded to this app session —
   * nothing writes it to disk, so cold-boot falls back to the default
   * (`INBOX_RAIL_WIDTH_DEFAULT`).
   */
  widthByTab: Record<string, number | undefined>;
  /** The lens each tab's inbox shows, keyed like `widthByTab` so it survives a re-attach. */
  lensByTab: Record<string, InboxLens | undefined>;
  open: (sessionId: string, opts?: { compose?: boolean }) => void;
  close: (sessionId: string) => void;
  openGroup: (sessionId: string) => void;
  markGroupHandled: (sessionId: string, n: number) => void;
  /** Record that the rail on `sessionId` acted on compose request `n`. */
  markComposeHandled: (sessionId: string, n: number) => void;
  /** SW-IB1-B2: commit a remembered pane width for a tab. */
  setWidth: (tabId: string, px: number) => void;
  setLens: (tabId: string, lens: InboxLens) => void;
  /** SW-IB1-B2: drop a tab's remembered width and lens (called when the tab closes). */
  cleanupTab: (tabId: string) => void;
}

let watchingEligibility = false;

/** Rails follow lifecycle even when their tab is hidden or its workspace is minimized. */
function watchEligibility(): void {
  if (watchingEligibility) return;
  watchingEligibility = true;
  useAgentStateStore.subscribe((agent) => {
    const { bySession } = useAgentInboxRailStore.getState();
    const ended = Object.keys(bySession).filter((id) => {
      if (!bySession[id] || isRunningAgent(agent, id)) return false;
      // A layout refresh or reconnect temporarily hides live controls. Remember
      // the open rail until current lifecycle evidence confirms the agent ended.
      const activity = agent.activityBySession[id];
      return !activity || activity.freshness === "current" || !!agent.exitBySession[id];
    });
    if (ended.length === 0) return;
    const next = { ...bySession };
    for (const id of ended) next[id] = undefined;
    useAgentInboxRailStore.setState({ bySession: next });
  });
}

/**
 * The per-terminal-tab agent inbox rail's state: which tabs are showing it, and
 * whether the composer was asked for. Not persisted — every tile boots with the
 * rail closed, same as the HUD and source-diff overlays.
 */
export const useAgentInboxRailStore = create<AgentInboxRailState>((set) => ({
  bySession: {},
  widthByTab: {},
  lensByTab: {},
  open: (sessionId, opts) =>
    set((s) => {
      if (!isRunningAgent(useAgentStateStore.getState(), sessionId)) return s;
      watchEligibility();
      const compose = opts?.compose === true;
      const cur = s.bySession[sessionId];
      const composeRequest = compose ? (cur?.composeRequest ?? 0) + 1 : (cur?.composeRequest ?? 0);
      if (cur && cur.composeRequest === composeRequest) return s;
      return { bySession: { ...s.bySession, [sessionId]: { ...cur, composeRequest } } };
    }),
  openGroup: (sessionId) =>
    set((s) => {
      if (!isRunningAgent(useAgentStateStore.getState(), sessionId)) return s;
      watchEligibility();
      const cur = s.bySession[sessionId];
      return {
        bySession: {
          ...s.bySession,
          [sessionId]: { composeRequest: 0, ...cur, groupRequest: (cur?.groupRequest ?? 0) + 1 },
        },
      };
    }),
  markGroupHandled: (sessionId, n) =>
    set((s) => {
      const cur = s.bySession[sessionId];
      return cur ? { bySession: { ...s.bySession, [sessionId]: { ...cur, groupHandled: n } } } : s;
    }),
  markComposeHandled: (sessionId, n) =>
    set((s) => {
      const cur = s.bySession[sessionId];
      if (!cur || (cur.composeHandled ?? 0) >= n) return s;
      return { bySession: { ...s.bySession, [sessionId]: { ...cur, composeHandled: n } } };
    }),
  close: (sessionId) =>
    set((s) =>
      s.bySession[sessionId] ? { bySession: { ...s.bySession, [sessionId]: undefined } } : s,
    ),
  setWidth: (tabId, px) =>
    set((s) => {
      // Preserve the store identity when the value is already current — the
      // live-drag path writes on every pointermove, so a no-op write mustn't
      // churn subscribers.
      if (s.widthByTab[tabId] === px) return s;
      return { widthByTab: { ...s.widthByTab, [tabId]: px } };
    }),
  setLens: (tabId, lens) =>
    set((s) =>
      s.lensByTab[tabId] === lens ? s : { lensByTab: { ...s.lensByTab, [tabId]: lens } },
    ),
  cleanupTab: (tabId) =>
    set((s) => {
      if (!(tabId in s.widthByTab) && !(tabId in s.lensByTab)) return s;
      // Delete to keep the maps bounded; closing + reopening the tab is a
      // fresh contract that falls back to the defaults.
      const widthByTab = { ...s.widthByTab };
      const lensByTab = { ...s.lensByTab };
      delete widthByTab[tabId];
      delete lensByTab[tabId];
      return { widthByTab, lensByTab };
    }),
}));

/** Open the per-tab rail on `sessionId`, optionally with the composer focused. */
export function openAgentInboxRail(sessionId: string, opts?: { compose?: boolean }) {
  useAgentInboxRailStore.getState().open(sessionId, opts);
}

/** The last compose request the rail on `sessionId` acted on (0 when none). */
export function useAgentInboxRailComposeHandled(sessionId: string): number {
  return useAgentInboxRailStore((s) => s.bySession[sessionId]?.composeHandled ?? 0);
}

export function markAgentInboxRailComposeHandled(sessionId: string, n: number): void {
  useAgentInboxRailStore.getState().markComposeHandled(sessionId, n);
}

/** Close the per-tab rail on `sessionId`. */
export function closeAgentInboxRail(sessionId: string) {
  useAgentInboxRailStore.getState().close(sessionId);
}

/**
 * SW-IB1-B2: subscribe to a tab's remembered push-pane width (in px) or
 * `undefined` when it has none (consumer falls back to the default). Keyed by the
 * stable `tab.id`, so a session change within the same tab preserves width.
 */
export function useAgentInboxRailWidth(tabId: string | undefined | null): number | undefined {
  return useAgentInboxRailStore((s) => (tabId ? s.widthByTab[tabId] : undefined));
}

/** SW-IB1-B2: commit a tab's remembered push-pane width in CSS px. */
export function setAgentInboxRailWidth(tabId: string, px: number): void {
  useAgentInboxRailStore.getState().setWidth(tabId, px);
}

/** SW-IB1-B2: drop the stored width and lens for a tab (call on tab close). */
export function cleanupAgentInboxRailTab(tabId: string): void {
  useAgentInboxRailStore.getState().cleanupTab(tabId);
}

/** A tab's inbox lens, defaulting to Threads. */
export function useAgentInboxRailLens(tabId: string | undefined | null): InboxLens {
  return useAgentInboxRailStore(
    (s) => (tabId ? s.lensByTab[tabId] : undefined) ?? DEFAULT_INBOX_LENS,
  );
}

export function setAgentInboxRailLens(tabId: string, lens: InboxLens): void {
  useAgentInboxRailStore.getState().setLens(tabId, lens);
}
