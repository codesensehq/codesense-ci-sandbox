// SPDX-License-Identifier: MIT
import { selectEffectiveManagedOutcome } from "./session-status-signals";
import { toast } from "sonner";

import { formatSessionHandle } from "@/features/viewer/lib/agent-row-presentation";
import { isAgentSession } from "@/features/viewer/lib/hand-agents";
import { activateAndFocus, currentActivationGeneration } from "@/features/viewer/lib/focus-cascade";
import {
  getNeedsYouState,
  isNeedsYouNews,
  type NeedsYouKind,
  type NeedsYouState,
  selectNeedsYouKind,
  selectSessionAiStatus,
  selectSessionsNeedingYou,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { showAgent } from "@/features/viewer/lib/show-agent";
import {
  switchContext,
  canSwitchContext,
  currentContextEntry,
  currentContextNavigationRevision,
  beginContextNavigation,
} from "@/features/viewer/lib/context-switch";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { cliHostRoot } from "./cli-host";
import { focusTileKey, FOCUS_TILE_ATTR } from "./focus-indicator";
import { focusTileFrame, focusTileQuestionBanner } from "@/features/viewer/lib/tile-question-focus";
import {
  labelSessionsNeedingYou,
  workspaceLabel,
} from "@/features/weft/hooks/use-sessions-needing-you";
import { buildTeamKey, getActiveTeamKey } from "@/shared/lib/team-key";
import {
  isSessionSeen,
  type SeenSources,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  selectActiveTerminalTab,
  selectHiddenTabLocation,
  selectSessionHome,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import type { AgentSessionRow, AttentionNeed } from "@/tauri/commands";

import {
  type ActivityAlert,
  type AgentAlert,
  type AlertItem,
  alertLine,
  type FinishedAlert,
  type NeedsYouAlert,
  composeNotification,
  entryKey,
  isBlockingActivityAlert,
  planAlerts,
} from "./attention-alerts";
import { type OsAttention, tauriOsAttention } from "./os-attention";

/**
 * Drives the dock badge, the OS notification and the in-app Jump pill from the
 * live stores (plus the Activity board's arrivals, via
 * {@link reportActivityNeedsYou}), as ONE stream:
 *  - the badge is the count of sessions needing you, updated focused or not and
 *    cleared at zero;
 *  - a new entry while the window is unfocused raises one notification; entries
 *    within `burstMs` collapse into one summary, notifications are at least
 *    `cooldownMs` apart, and one entry does not notify again within
 *    `renotifyFloorMs` unless the user has been back in between;
 *  - a new entry while the window is focused never raises an OS notification:
 *    when the user is looking at that agent's tab nothing happens (they can see
 *    it), otherwise the Jump pill offers it in the app;
 *  - an agent the user started that finishes a turn of `finishMinMs` or more
 *    without the user ever looking at its tab while it ran is news, if that
 *    setting is on: one "finished" notification when the window is unfocused,
 *    else the Jump pill offers it (unless the pill already offers something);
 *    a background agent and a routine's agent are not followed;
 *  - nothing fires with the setting off.
 *
 * Landing. The desktop notification plugin reports no click, but clicking a
 * notification activates the app. So the return lands the user on the
 * notification's tab only when it looks like that click: focus came back within
 * `landWindowMs` of the notification being posted, no pointer went down in the
 * window around the focus (a click into the app is the user choosing where to
 * go), and no dialog, palette, popover or menu is open. Any other return moves
 * nothing: a pill offers "Jump", which does the same landing. A landing never
 * puts keyboard focus in a terminal or on an answer: it selects the tab —
 * switching team or restoring a Hidden tab if needed — and parks focus on its
 * tile frame (a question's banner container, for a question), so keys typed on
 * the way back reach no agent. An agent with no tab is shown the way the Agents
 * pane shows it (adopted into a tab, or its run output), else the Agents pane
 * opens. The pill's target is exposed ({@link getPendingAttentionTarget}) for
 * the "jump to what needs me" cycle; the pill goes away once that tab is
 * selected, stops needing you, or is dismissed.
 *
 * Which sessions need you is the shared needs-you list (seen rule applied: a
 * crash or failure the user has seen drops out), plus every live agent on the
 * roster that needs you but has no tab in the layout (an agent on another
 * machine, a closed workspace), each session once. Both read the agents' own
 * `signal_done` outcomes, as the Agents pane does. Every kind notifies.
 */

export const BURST_MS = 1500;
export const COOLDOWN_MS = 10_000;
export const RENOTIFY_FLOOR_MS = 60_000;
export const LAND_WINDOW_MS = 8_000;
export const POINTER_GRACE_MS = 150;
/** A turn shorter than this finishing while you were away is not worth a notification. */
export const FINISH_MIN_MS = 30_000;
const PILL_ID = "attention-jump";
const PILL_DURATION_MS = 20_000;

/** The in-app Jump pill. `onClose` runs when it closes by itself or the user dismisses it. */
export interface JumpPill {
  show(text: string, onJump: () => void, onClose: () => void): void;
  dismiss(): void;
}

/** Keeps the pill clear of the tile controls along the top edge. */
const sonnerPill: JumpPill = {
  show(text, onJump, onClose) {
    toast(text, {
      id: PILL_ID,
      description: "Jump there (⌘⇧J)",
      duration: PILL_DURATION_MS,
      position: "bottom-center",
      closeButton: true,
      action: { label: "Jump", onClick: onJump },
      onDismiss: onClose,
      onAutoClose: onClose,
    });
  },
  dismiss() {
    toast.dismiss(PILL_ID);
  },
};

export interface AttentionAlertsOptions {
  os?: OsAttention;
  pill?: JumpPill;
  burstMs?: number;
  cooldownMs?: number;
  renotifyFloorMs?: number;
  landWindowMs?: number;
  finishMinMs?: number;
  now?: () => number;
}

function activeTeamKey(): string | null {
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  return activeOrgSlug !== null ? buildTeamKey(activeOrgSlug, activeTeamName) : null;
}

function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

/** A dialog, command palette, popover or menu is open: the user is mid-task. */
export function isOverlayOpen(): boolean {
  if (typeof document === "undefined") return false;
  const open = document.querySelectorAll<HTMLElement>(
    '[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"], [data-radix-popper-content-wrapper], [cmdk-root]',
  );
  for (const el of open) {
    if (typeof el.checkVisibility !== "function" || el.checkVisibility()) return true;
  }
  return false;
}

/** Where `sessionId`'s tab is now (it may have moved since the notification). */
function locate(sessionId: string): {
  teamKey: string;
  workspaceId: string;
  tabId: string;
  hidden: boolean;
} | null {
  const s = useViewerStore.getState();
  const home = selectSessionHome(s, sessionId);
  if (!home) return null;
  return {
    teamKey: home.teamKey,
    workspaceId: home.workspaceId,
    tabId: home.tab.id,
    hidden: selectHiddenTabLocation(s, sessionId) !== undefined,
  };
}

/**
 * Land the user on a tab that needs them: bring the window forward, enter its
 * team, bring back its tile (minimized) or the tab itself (Hidden), and select
 * it (which also pulls it out of tab overflow). Keyboard focus parks on the
 * tile frame — for a question, on its banner container when one is showing —
 * never in the terminal or on an answer, so nothing typed reaches the agent.
 */
export async function routeToAttention(
  target: { sessionId: string; kind: NeedsYouKind | "finished" },
  os: OsAttention = tauriOsAttention,
): Promise<boolean> {
  const entryRevision = beginContextNavigation();
  const entryTeam = getActiveTeamKey();
  const entryActivation = currentActivationGeneration(entryTeam);
  await os.bringWindowForward();
  if (
    currentContextNavigationRevision() !== entryRevision ||
    getActiveTeamKey() !== entryTeam ||
    currentActivationGeneration(entryTeam) !== entryActivation
  )
    return true;
  const where = locate(target.sessionId);
  if (!where) return false;
  const { teamKey, workspaceId, tabId } = where;
  if (!canSwitchContext(teamKey)) return false;
  const origin = currentContextEntry();
  const v = useViewerStore.getState();
  if (where.hidden) {
    v.restoreHiddenTerminalTab(teamKey, workspaceId, tabId);
  }
  const changed = switchContext({ teamKey, workspaceId, tabId }, "agent", origin);
  // A same-place selection is a successful no-op; a refused cross-team switch is not.
  if ((!changed && origin?.teamKey !== teamKey) || getActiveTeamKey() !== teamKey) return false;
  useViewerStore.getState().expandTerminalOnOpen(teamKey);
  // No terminal intent: supersede any pending terminal focus, then park once
  // the selection has rendered.
  activateAndFocus({ teamKey, workspaceId, focus: "preserve-focus" });
  const revision = currentContextNavigationRevision();
  const activation = currentActivationGeneration(teamKey);
  const activeTeam = getActiveTeamKey();
  await nextFrame();
  await nextFrame();
  // The landing succeeded; a newer navigation owns focus now. Returning true
  // also prevents the caller from falling back to Show and undoing that choice.
  if (
    getActiveTeamKey() !== activeTeam ||
    currentContextNavigationRevision() !== revision ||
    currentActivationGeneration(teamKey) !== activation
  )
    return true;
  const frame = cliHostRoot()?.querySelector<HTMLElement>(
    `[${FOCUS_TILE_ATTR}="${CSS.escape(focusTileKey(activeTeam, workspaceId))}"]`,
  );
  if (!frame) return true;
  if (
    target.kind === "question" &&
    selectActiveTerminalTab(useViewerStore.getState(), teamKey, workspaceId) === tabId &&
    focusTileQuestionBanner(workspaceId, frame)
  )
    return true;
  focusTileFrame(workspaceId, frame);
  return true;
}

/** The slice of an Activity row the controller needs: identity, the sentence it
 *  shows, and the declared `need` so the controller can tell a BLOCKING arrival
 *  (must notify) from an AMBIENT one (opt-in). */
export interface ActivityNeedsYouRow {
  id: number;
  summary: string;
  need?: AttentionNeed | null;
}

interface Controller {
  reportActivity(rows: readonly ActivityNeedsYouRow[], open: () => void): void;
  setActivityOwed(ids: Iterable<number>): void;
  pendingSession(): string | null;
}

let active: Controller | null = null;

/**
 * The Activity board's new YOU-OWE arrivals: they join the one notification
 * stream (same focus gate, burst, cooldown and landing). `open` reveals the board.
 */
export function reportActivityNeedsYou(
  rows: readonly ActivityNeedsYouRow[],
  open: () => void,
): void {
  active?.reportActivity(rows, open);
}

/** The Activity board's current YOU-OWE ids: an item that leaves stops being
 *  offered or notified. */
export function setActivityOwed(ids: Iterable<number>): void {
  active?.setActivityOwed(ids);
}

/** The session the Jump pill offers, or null. The "jump to what needs me"
 *  cycle starts with it; selecting that tab dismisses the pill. */
export function getPendingAttentionTarget(): string | null {
  return active?.pendingSession() ?? null;
}

/** The current question/block, crash exit status, or error reason. */
function withDetail<T extends NeedsYouAlert>(row: T): T {
  if (row.kind === "blocked") {
    const progress = useViewerStore.getState().progressBySession[row.sessionId];
    const note = progress?.phase === "blocked" ? progress.note?.trim() : undefined;
    return note ? { ...row, detail: note } : row;
  }
  if (row.kind === "failed") {
    const reason = failureReason(row.sessionId);
    return reason ? { ...row, detail: reason } : row;
  }
  if (row.kind === "attention") {
    const summary = useManagedSessionStore.getState().summaries?.[row.sessionId]?.trim();
    return summary ? { ...row, detail: summary } : row;
  }
  if (row.kind === "question") {
    const asks = Object.values(useManagedSessionStore.getState().pendingAsks)
      .filter((a) => a.sessionId === row.sessionId)
      .sort((a, b) => a.correlationId.localeCompare(b.correlationId, undefined, { numeric: true }));
    return asks[0] ? { ...row, detail: asks[0].question } : row;
  }
  if (row.kind === "crashed") {
    const t2 = useAgentStateStore.getState().bySession[row.sessionId]?.t2?.signal;
    if (t2?.kind === "exited") {
      if (t2.signal) return { ...row, detail: `killed by ${t2.signal}` };
      if (t2.code != null) return { ...row, detail: `exit code ${t2.code}` };
    }
  }
  return row;
}

/** The words for an error kind the provider's failed turn reported. */
const ERROR_REASONS: Record<string, string> = {
  rate_limit: "Rate limited",
  overloaded: "API overloaded",
  authentication_failed: "Sign-in failed",
  oauth_org_not_allowed: "Organization not allowed",
  account_on_hold: "Account on hold",
  billing_error: "Billing problem",
  invalid_request: "Invalid request",
  model_not_found: "Model not available",
  server_error: "API server error",
  max_output_tokens: "Output limit reached",
  cloud_credential_error: "Cloud credentials failed",
};

/** Why an agent's turn failed, when it said: its `signal_done` summary, or an API
 *  error its `StopFailure` hook reported (with its kind, when the terminal host
 *  observed it). */
function failureReason(sessionId: string): string | undefined {
  const managed = useManagedSessionStore.getState();
  const agent = useAgentStateStore.getState();
  if (
    selectEffectiveManagedOutcome({ crashes: agent, outcomes: managed.states }, sessionId) ===
      "failed" &&
    managed.summaries?.[sessionId]
  ) {
    return managed.summaries[sessionId];
  }
  const snapshot = useAgentStateStore.getState().activityBySession[sessionId]?.snapshot;
  if (snapshot?.state === "errored" && snapshot.reason) {
    return ERROR_REASONS[snapshot.reason] ?? "API error";
  }
  const t1 = useAgentStateStore.getState().bySession[sessionId]?.t1?.signal;
  if (t1?.kind === "claude-hook" && t1.event === "StopFailure") return "API error";
  return undefined;
}

/** The agent's name, from its roster row. */
function agentName(row: AgentSessionRow): string {
  return row.displayName?.trim() || formatSessionHandle(row.handle);
}

/**
 * The live agents on the roster that need you but are not in `listed` (the
 * sessions with a tab), each once: an agent on another machine that only the
 * Agents pane lists, a closed workspace's agent, another team's agent. Same
 * definition of needs-you as a tab ({@link selectNeedsYouKind}).
 */
export function selectTablessAgentsNeedingYou(
  state: NeedsYouState & { seen: SeenSources },
  roster: readonly AgentSessionRow[] | null,
  listed: ReadonlySet<string>,
): AgentAlert[] {
  const out: AgentAlert[] = [];
  const seenIds = new Set(listed);
  for (const row of roster ?? []) {
    const sid = row.sessionId;
    if (!row.alive || !sid || seenIds.has(sid)) continue;
    const kind = selectNeedsYouKind(state, sid);
    if (kind === null) continue;
    seenIds.add(sid);
    out.push({
      kind,
      sessionId: sid,
      label: agentName(row),
      seen: isSessionSeen(state.seen, sid),
      agent: row,
    });
  }
  return out;
}

/** The dots of an agent mid-turn (a permission prompt is still inside the turn). */
const BUSY: ReadonlySet<string> = new Set(["running", "thinking", "needs-permission"]);

/** Where the user goes for an agent with no tab Show could open: the Agents pane. */
function openAgentsPane(): void {
  const team = activeTeamKey();
  if (!team) return;
  const v = useViewerStore.getState();
  // The pane lists every agent: this only makes sure the team is shown and the pane is open.
  v.setAgentsPaneTeamHidden(team, false);
  v.setWorkspacePanelCollapsed(false);
}

function rosterRowOf(sessionId: string): AgentSessionRow | undefined {
  return useAgentListProjectionStore.getState().sessions?.find((r) => r.sessionId === sessionId);
}

/** Land on a session's tab ({@link routeToAttention}); one with no tab is shown
 *  the way the Agents pane shows it, else the Agents pane opens. */
async function landOnSession(item: NeedsYouAlert | FinishedAlert, os: OsAttention): Promise<void> {
  if (await routeToAttention(item, os)) return;
  const row = ("agent" in item ? item.agent : undefined) ?? rosterRowOf(item.sessionId);
  if (!row) return;
  const out = await showAgent(row);
  if ("notShown" in out) openAgentsPane();
}

/**
 * The user is looking at this session's tab: it is the selected tab of the
 * active workspace, in the active team, on a tile (not Hidden or minimized).
 * Whether the window is focused is the caller's half of "looking".
 */
function isTabInView(sessionId: string): boolean {
  const v = useViewerStore.getState();
  const home = selectSessionHome(v, sessionId);
  if (!home || home.minimized || !home.workspace) return false;
  if (selectHiddenTabLocation(v, sessionId) !== undefined) return false;
  return (
    home.teamKey === activeTeamKey() &&
    v.activeWorkspace?.[home.teamKey] === home.workspaceId &&
    selectActiveTerminalTab(v, home.teamKey, home.workspaceId) === home.tab.id
  );
}

/** Whether an agent runs in the session (a plain shell finishing a command is not news). */
function runsAnAgent(sessionId: string): boolean {
  const a = useAgentStateStore.getState();
  if (a.bySession[sessionId] !== undefined || a.processAgentBySession?.[sessionId]) return true;
  const row = rosterRowOf(sessionId);
  return row !== undefined && isAgentSession(row);
}

/**
 * Whether a finished turn of this session is the user's news: an agent they
 * started or are working with. An agent running in the background, or one a
 * routine started, works for the user unattended; its turns are not news.
 */
function followsFinishes(sessionId: string): boolean {
  const row = rosterRowOf(sessionId);
  return !row?.backgrounded && !row?.provenance?.routineKey;
}

/** The "finished" entry for a turn, named by its tab or else its roster row. */
function finishedAlert(
  sessionId: string,
  turnStartedAt: number,
  durationMs: number,
): FinishedAlert | null {
  if (!runsAnAgent(sessionId) || !followsFinishes(sessionId)) return null;
  const summary = useManagedSessionStore.getState().summaries?.[sessionId]?.trim();
  const base = {
    kind: "finished" as const,
    sessionId,
    turnStartedAt,
    durationMs,
    ...(summary ? { detail: summary } : {}),
  };
  const home = selectSessionHome(useViewerStore.getState(), sessionId);
  if (home) {
    return {
      ...base,
      label: workspaceLabel(home.workspace, home.workspaceId),
      tabName: home.tab.name || home.tab.id,
    };
  }
  const row = rosterRowOf(sessionId);
  return row ? { ...base, label: agentName(row), agent: row } : null;
}

/** Start driving the badge, notifications and pill. Returns the stop function. */
export function startAttentionAlerts(options: AttentionAlertsOptions = {}): () => void {
  const os = options.os ?? tauriOsAttention;
  const pill = options.pill ?? sonnerPill;
  const burstMs = options.burstMs ?? BURST_MS;
  const cooldownMs = options.cooldownMs ?? COOLDOWN_MS;
  const floorMs = options.renotifyFloorMs ?? RENOTIFY_FLOOR_MS;
  const landWindowMs = options.landWindowMs ?? LAND_WINDOW_MS;
  const finishMinMs = options.finishMinMs ?? FINISH_MIN_MS;
  const now = options.now ?? Date.now;

  let focused = true;
  let known = new Set<string>();
  /** The needs-you list, labelled: what the badge counts. */
  let current: NeedsYouAlert[] = [];
  let badge = -1;
  const batch = new Map<string, AlertItem>();
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  let lastFiredAt = -Infinity;
  /** When each entry last notified or was offered; cleared once the user is back. */
  const notifiedAt = new Map<string, number>();
  /** The last notification's lead entry and when it was posted. */
  let posted: { item: AlertItem; at: number } | null = null;
  let offered: AlertItem | null = null;
  let activityOwed = new Set<number>();
  let landTimer: ReturnType<typeof setTimeout> | null = null;
  let pointerDownAt = -Infinity;
  let stopped = false;
  let queued = false;
  /** What already needed you at start, and on the roster's first load, is not news. */
  let primed = false;
  let rosterLoaded = false;
  /** Each agent's turn in progress: when it began, and whether the user ever
   *  looked at it (window focused, its tab in view) while it ran. */
  const turns = new Map<string, { startedAt: number; attended: boolean }>();

  const settings = () => useViewerStore.getState().settings;
  const enabled = () => settings()?.osNotifications !== false;
  // SLW-TL-4: Inbox (board-arrival) notifications are OPT-IN — off unless the user
  // explicitly turned them on. The pane tab badge and closed right-pane edge show
  // the last published counts. This OS/push escape fires only for an away-from-desk
  // human who asked for it (§2.3).
  const inboxNotifyEnabled = () => settings()?.inboxNotifications === true;
  const finishEnabled = () => settings()?.notifyOnFinish !== false;
  /** The sound setting: "needs-you" sounds for anything blocking a person, "all"
   *  for a finished agent too. */
  const withSound = (items: readonly AlertItem[]) => {
    const mode = settings()?.notificationSound ?? "off";
    if (mode === "all") return true;
    if (mode !== "needs-you") return false;
    return items.some((i) =>
      i.kind === "activity" ? isBlockingActivityAlert(i) : i.kind !== "finished",
    );
  };
  /** Whether an entry may still notify, under its own setting. */
  const allowed = (item: AlertItem) => (item.kind === "finished" ? finishEnabled() : enabled());

  /** Still a valid landing target (a crash the user has seen still is). */
  const isLive = (item: AlertItem) => {
    if (item.kind === "activity") return activityOwed.has(item.id);
    if (item.kind === "finished") return true;
    return selectNeedsYouKind(getNeedsYouState(), item.sessionId) === item.kind;
  };
  /** Still worth offering: still needs you, not yet seen, and its tab not the one
   *  in view. Read from the live row, not the one offered: the tab may have moved
   *  since (a Hidden tab restored onto its tile, a tile brought back from the
   *  tray). A seen crash or error stays on the list but is no longer offered; a
   *  question or a permission is offered until answered. */
  const isOfferable = (item: AlertItem) => {
    if (item.kind === "activity") return activityOwed.has(item.id);
    if (item.kind !== "finished") {
      const row = current.find((i) => entryKey(i) === entryKey(item));
      if (!row) return false;
      if (row.seen !== undefined && !isNeedsYouNews({ kind: row.kind, seen: row.seen })) {
        return false;
      }
    }
    return !isTabInView(item.sessionId);
  };
  const withdraw = () => {
    if (!offered) return;
    offered = null;
    pill.dismiss();
  };

  const recentlyNotified = () => {
    const out = new Set<string>();
    for (const [key, at] of notifiedAt) if (now() - at < floorMs) out.add(key);
    return out;
  };

  const clearBatch = () => {
    batch.clear();
    if (flushTimer !== null) clearTimeout(flushTimer);
    flushTimer = null;
  };

  const cancelLanding = () => {
    if (landTimer !== null) clearTimeout(landTimer);
    landTimer = null;
  };

  const flush = async () => {
    flushTimer = null;
    const items = [...batch.values()].filter((i) => isLive(i) && allowed(i));
    batch.clear();
    if (items.length === 0 || focused) return;
    const at = now();
    lastFiredAt = at;
    for (const item of items) notifiedAt.set(entryKey(item), at);
    // SLW-TL-4: the OS/push notification for AMBIENT Inbox (activity) arrivals is OPT-IN;
    // BLOCKING Inbox arrivals (an agent posting `awaiting-answer` / `needs-approval` /
    // `awaiting-validation`) and terminal-agent alerts are not — the human is waiting
    // for the system to tell them, so these notify at the same floor as a terminal
    // `question` / `permission` / `crashed`. Only ambient `needs-attention` (or an
    // Inbox row with no declared need) respects the opt-in. The in-app reveal/landing
    // (`posted`) still engages below for every activity item, so a default-config
    // ambient arrival is still brought to the Inbox on return.
    const inboxOn = inboxNotifyEnabled();
    const notifyItems = items.filter(
      (i) => i.kind !== "activity" || inboxOn || isBlockingActivityAlert(i),
    );
    let shown = false;
    if (notifyItems.length > 0) {
      const { title, body } = composeNotification(notifyItems);
      shown = await os.notify(title, body, withSound(notifyItems) ? { sound: true } : undefined);
    }
    // Set the landing target so the return reveals the Inbox: a notified item when one
    // was shown, else the arriving activity item (the reveal fires even with the OS
    // push off).
    if (!stopped && !focused) {
      const target = shown ? notifyItems[0] : items.find((i) => i.kind === "activity");
      if (target) posted = { item: target, at };
    }
  };

  const scheduleFlush = () => {
    if (flushTimer !== null) return;
    const delay = Math.max(burstMs, lastFiredAt + cooldownMs - now());
    flushTimer = setTimeout(() => void flush(), delay);
  };

  const land = (item: AlertItem) => {
    withdraw();
    if (item.kind === "activity") {
      item.open();
      return;
    }
    void landOnSession(item, os);
  };

  const offer = (item: AlertItem) => {
    offered = item;
    pill.show(
      alertLine(item),
      () => {
        if (offered === item && isLive(item)) land(item);
      },
      () => {
        if (offered === item) offered = null;
      },
    );
  };

  /** Escape in the toast area (e.g. after Alt+T) dismisses the pill; Escape
   *  anywhere else stays with whoever has focus (a terminal needs it). */
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key !== "Escape" || !offered) return;
    const inToasts =
      document.activeElement instanceof Element &&
      document.activeElement.closest("[data-sonner-toaster]") !== null;
    if (inToasts) withdraw();
  };

  /**
   * Follow each agent's turn. A turn runs while its dot is busy; when it settles
   * on `done` after at least `finishMinMs`, with the user never having looked at
   * it while it ran, it is news: one "finished" entry per turn. Unfocused, it
   * joins the notification batch; focused, the Jump pill offers it when the pill
   * offers nothing else.
   */
  const followTurns = (state: NeedsYouState) => {
    const ids = new Set([
      ...Object.keys(state.agentDots),
      ...Object.keys(state.aiStatusBySession),
      ...turns.keys(),
    ]);
    const at = now();
    for (const sid of ids) {
      const dot = selectSessionAiStatus(state, state.agentDots, sid);
      const turn = turns.get(sid);
      const looking = focused && isTabInView(sid);
      if (dot !== undefined && BUSY.has(dot)) {
        if (!turn) turns.set(sid, { startedAt: at, attended: looking });
        else if (looking) turn.attended = true;
        continue;
      }
      if (!turn) continue;
      turns.delete(sid);
      if (dot !== "done" || turn.attended || !finishEnabled()) continue;
      const durationMs = at - turn.startedAt;
      if (durationMs < finishMinMs) continue;
      const item = finishedAlert(sid, turn.startedAt, durationMs);
      if (!item) continue;
      if (!focused) batch.set(entryKey(item), item);
      else if (!offered && isOfferable(item)) {
        notifiedAt.set(entryKey(item), at);
        offer(item);
      }
    }
  };

  // Every input the evaluation reads: when none moved (the viewer store changes
  // for many unrelated reasons), there is nothing to do.
  let lastInputs: unknown[] = [];
  const inputs = (): unknown[] => {
    const v = useViewerStore.getState();
    const o = useOrgStore.getState();
    return [
      focused,
      v.terminalLayout,
      v.aiStatusBySession,
      v.workspaces,
      v.softClosedWorkspaces,
      v.activeWorkspace,
      v.settings?.osNotifications,
      useAgentStateStore.getState(),
      useManagedSessionStore.getState().pendingAsks,
      useManagedSessionStore.getState().states,
      useAgentListProjectionStore.getState().sessions,
      o.activeOrgSlug,
      o.activeTeamName,
    ];
  };

  const evaluate = () => {
    queued = false;
    if (stopped) return;
    const next = inputs();
    if (next.length === lastInputs.length && next.every((x, i) => Object.is(x, lastInputs[i]))) {
      return;
    }
    lastInputs = next;
    const state = getNeedsYouState();
    const rows = selectSessionsNeedingYou(state, null);
    const roster = useAgentListProjectionStore.getState().sessions;
    const tabless = selectTablessAgentsNeedingYou(
      state,
      roster,
      new Set(rows.map((r) => r.sessionId)),
    );
    current = [...labelSessionsNeedingYou(useViewerStore.getState(), rows), ...tabless].map(
      withDetail,
    );

    if (current.length !== badge) {
      badge = current.length;
      os.setBadgeCount(badge);
    }
    if (offered && !isOfferable(offered)) withdraw();

    if (!primed) {
      primed = true;
      known = new Set(current.map(entryKey));
    }
    if (roster !== null && !rosterLoaded) {
      rosterLoaded = true;
      for (const a of tabless) known.add(entryKey(a));
    }
    const plan = planAlerts(known, current, {
      enabled: enabled(),
      recentlyNotified: recentlyNotified(),
    });
    known = plan.known;
    if (focused) {
      // Focused, the OS notification never fires: an entry on the tab in view
      // needs nothing (the user can see it); one anywhere else is offered in the
      // app, on the Jump pill.
      const elsewhere = plan.fresh.find(
        (i) =>
          (i.seen === undefined || isNeedsYouNews({ kind: i.kind, seen: i.seen })) &&
          !isTabInView(i.sessionId),
      );
      if (elsewhere) {
        notifiedAt.set(entryKey(elsewhere), now());
        offer(elsewhere);
      }
    } else {
      for (const item of plan.fresh) batch.set(entryKey(item), item);
    }
    followTurns(state);
    if (batch.size > 0) scheduleFlush();
  };

  const schedule = () => {
    if (queued || stopped) return;
    queued = true;
    queueMicrotask(() => evaluate());
  };

  const onFocus = (isFocused: boolean) => {
    if (stopped || isFocused === focused) return;
    focused = isFocused;
    if (!isFocused) {
      cancelLanding();
      schedule();
      return;
    }
    clearBatch();
    notifiedAt.clear(); // the user is back: an entry may notify again next time
    const returned = posted;
    posted = null;
    if (returned && isLive(returned.item)) {
      const focusAt = now();
      const quick = focusAt - returned.at <= landWindowMs;
      if (!quick) {
        offer(returned.item);
      } else {
        // Wait out the click that may have brought focus back.
        landTimer = setTimeout(() => {
          landTimer = null;
          if (stopped || !focused || !isLive(returned.item)) return;
          const clickedIn = pointerDownAt >= focusAt - POINTER_GRACE_MS;
          if (clickedIn || isOverlayOpen()) offer(returned.item);
          else land(returned.item);
        }, POINTER_GRACE_MS);
      }
    }
    schedule();
  };

  const onPointerDown = () => {
    pointerDownAt = now();
  };
  if (typeof window !== "undefined") {
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
  }

  const controller: Controller = {
    reportActivity(rows, open) {
      // SLW-TL-4: engage the in-app reveal/landing for EVERY Inbox arrival (the
      // always-on reveal layer) — the opt-in gate lives only on the OS push in
      // `flush`, never here. Still focus-gated (never reveals over a present human)
      // and under the global OS-notifications master (`enabled`).
      if (stopped || focused || !enabled()) return;
      const recent = recentlyNotified();
      for (const row of rows) {
        activityOwed.add(row.id);
        const item: ActivityAlert = {
          kind: "activity",
          id: row.id,
          summary: row.summary,
          need: row.need ?? null,
          open,
        };
        if (!recent.has(entryKey(item))) batch.set(entryKey(item), item);
      }
      if (batch.size > 0) scheduleFlush();
    },
    setActivityOwed(ids) {
      activityOwed = new Set(ids);
      if (offered && !isOfferable(offered)) withdraw();
    },
    pendingSession() {
      return offered && offered.kind !== "activity" ? offered.sessionId : null;
    },
  };
  active = controller;

  const unsubs = [
    useViewerStore.subscribe(schedule),
    useAgentStateStore.subscribe(schedule),
    useManagedSessionStore.subscribe(schedule),
    useAgentListProjectionStore.subscribe(schedule),
    useOrgStore.subscribe(schedule),
    os.subscribeFocus(onFocus),
  ];
  evaluate();

  return () => {
    stopped = true;
    if (active === controller) active = null;
    clearBatch();
    cancelLanding();
    for (const u of unsubs) u();
    if (typeof window !== "undefined") {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
    }
    os.setBadgeCount(0);
  };
}
