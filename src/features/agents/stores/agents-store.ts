// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { getCurrentUser } from "@/shared/lib/get-current-user";
import {
  readAgentDefinitions,
  showAgentDefinitions,
  resetAgentDefinitions,
} from "../lib/agent-definitions";
import { requestScope } from "../lib/request-scope";
import type { ScheduleFilter } from "../lib/schedule-model";
import {
  agentArchive,
  agentBindingClear,
  agentBindingSet,
  agentCopyToMine,
  agentDelete,
  agentOpenAuthoringAgent,
  agentRestoreRevision,
  agentRestoreRoutines,
  agentSave,
  agentUnarchive,
  listAgentAccounts,
  type AgentAccountView,
  type AgentDetailView,
  type AgentForm,
  type AgentLaunchWarning,
  type AgentRow,
  type AgentSaveReply,
} from "@/tauri/commands";

/** The selected agent's views. */
export type AgentView = "overview" | "running" | "edit" | "history";

/** The Agents tab's two views: who (Agents) and when (Schedule), SW-AGR-5. */
export type AgentsTabView = "agents" | "schedule" | "roles";

/** The Schedule list: what will run, or what ran. */
export type ScheduleList = "upcoming" | "recent";

/**
 * A one-shot request to the Agents tab, addressed to one team's tab (`scope`, the team on screen
 * when it was made) and one agent: open the agent's account picker, or jump its editor to a
 * section. Only that team's shown tab acts on it (`useAgentRequest`); leaving the guarded
 * navigation it waited behind drops it.
 */
export type AgentRequest =
  | { kind: "account-picker"; agentKey: string; scope: string | null }
  | { kind: "edit-section"; agentKey: string; scope: string | null; section: string };

/** Where the user asked to go while the editor or the New agent form held an unsaved edit. */
export type AgentLeave = { kind: "select"; key: string | null } | { kind: "create" };

/**
 * The Agents tab's state: the listed agents, the selected agent's detail, your accounts
 * (for the "your account" control) and which actions are in flight.
 *
 * The app-root LiveHub bridge materializes keyed rows. Reads join that shared projection;
 * selection, drafts and action state remain local to this UI.
 */
interface AgentsState {
  /** `null` until the first list read lands. */
  rows: AgentRow[] | null;
  listError: string | null;
  includeArchived: boolean;
  capFocusKey: string | null;
  selectedKey: string | null;
  /** The New agent form is open in place of a selected agent. */
  creating: boolean;
  view: AgentView;
  /** The editor, or the New agent form, holds an unsaved edit. */
  formDirty: boolean;
  /** A switch away from a dirty form, waiting on the user to discard it. */
  pendingLeave: AgentLeave | null;
  /** What each agent's last save here was warned about, by key. The save went through. */
  saveWarnings: Record<string, { revision: number; warnings: AgentLaunchWarning[] }>;
  details: Record<string, AgentDetailView>;
  detailError: Record<string, string>;
  /** Your provider accounts; `null` until read. */
  accounts: AgentAccountView[] | null;
  accountsError: string | null;
  /** The routines a refused delete named, by agent key, shown inline on the row. */
  inUse: Record<string, string[]>;
  /** The pending one-shot request (open an account picker, jump to an editor section). */
  request: AgentRequest | null;
  notice: string | null;
  /** SW-AGR-5: Agents (the list and detail) or Schedule (every agent's routines). */
  tabView: AgentsTabView;
  /** What the Schedule list shows: one agent's routines, what needs attention, or all. */
  scheduleFilter: ScheduleFilter | null;
  scheduleList: ScheduleList;
  /** The routine open on the Schedule's right, kept across a look at the Agents view. */
  scheduleRoutine: { agentKey: string | null; key: string } | null;
  /** Undelivered or unknown runs the user has opened: they no longer count as needing
   *  attention. */
  seenRuns: ReadonlySet<string>;
  /** In-flight actions by `<action>:<key>`. */
  pending: Record<string, true>;
  actionError: string | null;

  loadList: () => Promise<void>;
  loadDetail: (key: string) => Promise<void>;
  loadAccounts: () => Promise<void>;
  setIncludeArchived: (include: boolean) => void;
  /** Select an agent; with an unsaved edit open, ask first. */
  select: (key: string | null) => void;
  /** Switch the selected agent's view. The editor stays mounted, so an edit survives it. */
  setView: (view: AgentView) => void;
  startCreate: () => void;
  cancelCreate: () => void;
  setFormDirty: (dirty: boolean) => void;
  /** Drop the unsaved edit and go where the user asked. */
  confirmLeave: () => void;
  /** Stay on the form with its edit. */
  cancelLeave: () => void;
  /** Create (no `baseRevision`) or replace an agent. A stale edit resolves as `conflict` and a
   *  refused input as `invalid`, writing nothing; anything else rejects with its message. */
  saveAgent: (form: AgentForm, baseRevision?: number) => Promise<AgentSaveReply>;
  /** Write revision `revision`'s definition as a new revision, made against `baseRevision`. */
  restoreRevision: (key: string, revision: number, baseRevision: number) => Promise<void>;
  /** Open a visible agent primed with `/agent` in the active team workspace. */
  /** Open an authoring agent for the user to describe an agent to. `opened: false` says why not. */
  describeToAgent: () => Promise<{ opened: boolean; message?: string }>;
  /** Archive an agent; one with routines needs `archiveRoutines` (the user confirmed the
   *  list), and they are archived with it. Resolves `"needs_confirm"` when the backend
   *  found routines the listed row did not show: the list is re-read so the confirm can
   *  name them. */
  archive: (key: string, archiveRoutines?: boolean) => Promise<"done" | "needs_confirm">;
  /** Unarchive an agent; the routines archived with it stay offered back on its row
   *  (`AgentRow.archived_routines`) until restored. */
  unarchive: (key: string) => Promise<void>;
  /** Unarchive the routines the agent's archive archived. */
  restoreRoutines: (key: string) => Promise<void>;
  /** Delete against `baseRevision`, the revision the user confirmed. */
  remove: (key: string, baseRevision: number) => Promise<void>;
  /** `team` absent clones to your scratch space (the default); passing a team clones
   *  the template straight to that team's shared agents dir. The New agent dialog
   *  calls this with no team for personal, with the picked team for team-scope. */
  copyToMine: (key: string, team?: { org: string; team: string }) => Promise<void>;
  /** Bind the agent to `accountId` for you, or (`null`) go back to your default. */
  setAccount: (key: string, accountId: string | null) => Promise<void>;
  /** Give or withdraw your consent to a raw terminal snapshot of the agent's interactive
   *  launches. */
  setRawTerminalCapture: (key: string, capture: boolean) => Promise<void>;
  /** Show the selected agent's Running tab. */
  showRunning: () => void;
  /** Open `agentKey`'s Edit at `section`. */
  openEditSection: (agentKey: string, section: string) => void;
  /** Ask the shown Agents tab to open `agentKey`'s account picker. */
  requestAccountPicker: (agentKey: string) => void;
  /** The request's actor took it: clear it, if it is still the pending one. */
  consumeRequest: (request: AgentRequest) => void;
  dismissActionError: () => void;
  dismissNotice: () => void;
  setTabView: (view: AgentsTabView) => void;
  setScheduleFilter: (filter: ScheduleFilter | null) => void;
  setScheduleList: (list: ScheduleList) => void;
  setScheduleRoutine: (routine: { agentKey: string | null; key: string } | null) => void;
  markRunsSeen: (runIds: string[]) => void;
}

/** Where the Agents tab's view state is kept across launches (this Mac only): the view,
 *  the Schedule list, and the undelivered or unknown runs already opened. */
const VIEW_STATE_KEY = "codesense.agents.view.v1";
/** The most opened runs remembered (they age out with their runs). */
const SEEN_CAP = 300;

interface PersistedView {
  tabView?: AgentsTabView;
  scheduleList?: ScheduleList;
  seenRuns?: string[];
}

function readView(): PersistedView {
  try {
    const raw = globalThis.localStorage?.getItem(VIEW_STATE_KEY);
    return raw ? (JSON.parse(raw) as PersistedView) : {};
  } catch {
    return {};
  }
}

function writeView(view: PersistedView): void {
  try {
    globalThis.localStorage?.setItem(VIEW_STATE_KEY, JSON.stringify(view));
  } catch {
    // Storage unavailable: the view resets next launch, nothing else.
  }
}

const persisted = readView();

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Keep the view state for the next launch. */
function persistView(s: {
  tabView: AgentsTabView;
  scheduleList: ScheduleList;
  seenRuns: ReadonlySet<string>;
}): void {
  writeView({ tabView: s.tabView, scheduleList: s.scheduleList, seenRuns: [...s.seenRuns] });
}

let listWatchers = 0;
let detailWatchers = 0;

/** Request counters: a reply lands only if no newer read has been issued since. */
let listSeq = 0;
let accountsSeq = 0;
const detailSeq: Record<string, number> = {};

const RELOADED = "Changed elsewhere — reloaded. Try again.";

export const useAgentsStore = create<AgentsState>((set, get) => {
  /** Run `action` once per `id` at a time; a repeat while it runs is dropped. */
  async function guarded(id: string, action: () => Promise<unknown>): Promise<void> {
    if (get().pending[id]) return;
    set((s) => ({ pending: { ...s.pending, [id]: true }, actionError: null }));
    try {
      await action();
    } catch (e) {
      set({ actionError: errorText(e) });
    } finally {
      set((s) => {
        const pending = { ...s.pending };
        delete pending[id];
        return { pending };
      });
    }
  }

  // Mutations carry the revision the person saw when they chose the action.
  const revisionOf = (key: string) =>
    get().rows?.find((r) => r.key === key)?.revision ?? get().details[key]?.definition.revision;

  return {
    rows: null,
    listError: null,
    includeArchived: false,
    capFocusKey: null,
    selectedKey: null,
    creating: false,
    view: "overview",
    formDirty: false,
    pendingLeave: null,
    saveWarnings: {},
    details: {},
    detailError: {},
    accounts: null,
    accountsError: null,
    inUse: {},

    request: null,
    notice: null,
    tabView:
      persisted.tabView === "schedule" || persisted.tabView === "roles"
        ? persisted.tabView
        : "agents",
    scheduleFilter: null,
    scheduleList: persisted.scheduleList === "recent" ? "recent" : "upcoming",
    scheduleRoutine: null,
    seenRuns: new Set<string>(persisted.seenRuns ?? []),
    pending: {},
    actionError: null,

    loadList: async () => {
      const seq = ++listSeq;
      try {
        await readAgentDefinitions(get().selectedKey ?? undefined);
      } catch (e) {
        if (seq === listSeq) set({ listError: errorText(e) });
      }
    },

    loadDetail: async (key) => {
      const seq = (detailSeq[key] = (detailSeq[key] ?? 0) + 1);
      try {
        await readAgentDefinitions(key);
      } catch (e) {
        if (detailSeq[key] === seq)
          set((s) => ({ detailError: { ...s.detailError, [key]: errorText(e) } }));
      }
    },

    loadAccounts: async () => {
      const seq = ++accountsSeq;
      const user = getCurrentUser();
      if (!user) {
        set({ accounts: [], accountsError: null });
        return;
      }
      try {
        const accounts = await listAgentAccounts(user.github_user_id);
        if (seq !== accountsSeq) return;
        set({ accounts, accountsError: null });
      } catch (e) {
        if (seq !== accountsSeq) return;
        set({ accountsError: errorText(e) });
      }
    },

    setIncludeArchived: (include) => {
      if (get().includeArchived === include) return;
      set({ includeArchived: include });
      void get().loadList();
    },

    select: (key) => {
      const { formDirty, selectedKey, creating } = get();
      if (formDirty && (creating || key !== selectedKey || get().capFocusKey === key)) {
        set({ pendingLeave: { kind: "select", key } });
        return;
      }
      set({
        selectedKey: key,
        creating: false,
        ...(get().capFocusKey === key ? { view: "overview" as const } : {}),
      });
      if (key) {
        void get().loadDetail(key);
        void get().loadAccounts();
      }
    },

    setView: (view) => {
      set({ view });
      const key = get().selectedKey;
      if (key) void get().loadDetail(key);
    },

    startCreate: () => {
      if (get().creating) return;
      if (get().formDirty) {
        set({ pendingLeave: { kind: "create" } });
        return;
      }
      set({ creating: true, selectedKey: null });
    },

    cancelCreate: () => set({ creating: false }),

    setFormDirty: (dirty) => {
      // A clean (or unmounted) form has nothing to discard, so a pending prompt is moot.
      if (!dirty && get().pendingLeave !== null) set({ formDirty: false, pendingLeave: null });
      else if (get().formDirty !== dirty) set({ formDirty: dirty });
    },

    confirmLeave: () => {
      const leave = get().pendingLeave;
      set({ pendingLeave: null, formDirty: false });
      if (leave?.kind === "select") get().select(leave.key);
      else if (leave?.kind === "create") get().startCreate();
    },

    cancelLeave: () => set({ pendingLeave: null, capFocusKey: null, request: null }),

    saveAgent: async (form, baseRevision) => {
      const reply = await agentSave(form, baseRevision);
      if (reply.outcome === "saved") {
        set((s) => ({
          saveWarnings: {
            ...s.saveWarnings,
            [reply.key]: { revision: reply.revision, warnings: reply.warnings },
          },
        }));
        await get().loadList();
        if (baseRevision === undefined) {
          // The new agent is saved: nothing is left to discard.
          set({ formDirty: false, view: "overview" });
          get().select(reply.key);
        } else {
          await get().loadDetail(reply.key);
        }
      }
      return reply;
    },

    restoreRevision: (key, revision, baseRevision) =>
      guarded(`restore:${key}`, async () => {
        const reply = await agentRestoreRevision(key, revision, baseRevision);
        if (reply.outcome === "conflict") {
          set({ notice: "Changed elsewhere — reloaded. Pick the revision to restore again." });
        } else if (reply.outcome === "invalid") {
          set({ actionError: reply.message });
        }
        await get().loadDetail(key);
        await get().loadList();
      }),

    describeToAgent: async () => {
      let result: { opened: boolean; message?: string } = { opened: false };
      await guarded("describe", async () => {
        set({ notice: null });
        const opened = await agentOpenAuthoringAgent();
        if (!opened.opened) set({ notice: opened.message });
        result = opened.opened ? { opened: true } : { opened: false, message: opened.message };
      });
      return result;
    },

    archive: async (key, archiveRoutines = false) => {
      let outcome: "done" | "needs_confirm" = "done";
      await guarded(`archive:${key}`, async () => {
        try {
          await agentArchive(key, archiveRoutines);
        } catch (e) {
          // Routines appeared since the list was read: re-read it so the confirm
          // lists them, rather than showing the refusal.
          if (!archiveRoutines && /confirm first/.test(errorText(e))) {
            outcome = "needs_confirm";
            await get().loadList();
            return;
          }
          throw e;
        }
      });
      return outcome;
    },

    unarchive: (key) =>
      guarded(`unarchive:${key}`, async () => {
        const base = revisionOf(key);
        if (base === undefined) return;
        const reply = await agentUnarchive(key, base);
        if (reply.outcome === "conflict") set({ notice: RELOADED });
      }),

    restoreRoutines: (key) =>
      guarded(`restore-routines:${key}`, async () => {
        await agentRestoreRoutines(key);
        await get().loadList();
      }),

    remove: (key, baseRevision) =>
      guarded(`delete:${key}`, async () => {
        const reply = await agentDelete(key, baseRevision);
        if (reply.outcome === "in_use") {
          set((s) => ({ inUse: { ...s.inUse, [key]: reply.referrers } }));
        } else if (reply.outcome === "conflict") {
          set({ notice: RELOADED });
        } else {
          set((s) => {
            const inUse = { ...s.inUse };
            delete inUse[key];
            return { inUse, selectedKey: s.selectedKey === key ? null : s.selectedKey };
          });
        }
      }),

    copyToMine: (key, team) =>
      guarded(`copy:${key}`, async () => {
        const reply = await agentCopyToMine(key, team);
        if (reply.outcome === "saved") {
          await get().loadList();
          get().select(reply.key);
        }
      }),

    setAccount: (key, accountId) =>
      guarded(`account:${key}`, async () => {
        if (accountId === null) await agentBindingClear(key);
        else await agentBindingSet(key, { accountId });
        await get().loadDetail(key);
      }),

    setRawTerminalCapture: (key, capture) =>
      guarded(`raw:${key}`, async () => {
        await agentBindingSet(key, { captureRawTerminal: capture });
        await get().loadDetail(key);
      }),

    showRunning: () => get().setView("running"),
    openEditSection: (agentKey, section) => {
      set({ request: { kind: "edit-section", agentKey, section, scope: requestScope() } });
      get().setView("edit");
    },
    requestAccountPicker: (agentKey) =>
      set({ request: { kind: "account-picker", agentKey, scope: requestScope() } }),
    consumeRequest: (request) => set((s) => (s.request === request ? { request: null } : {})),
    dismissActionError: () => set({ actionError: null }),
    dismissNotice: () => set({ notice: null }),
    setTabView: (tabView) => {
      set({ tabView });
      persistView(get());
    },
    setScheduleFilter: (scheduleFilter) => set({ scheduleFilter }),
    setScheduleList: (scheduleList) => {
      set({ scheduleList });
      persistView(get());
    },
    setScheduleRoutine: (scheduleRoutine) => set({ scheduleRoutine }),
    markRunsSeen: (runIds) => {
      if (runIds.every((id) => get().seenRuns.has(id))) return;
      set((s) => ({ seenRuns: new Set([...s.seenRuns, ...runIds].slice(-SEEN_CAP)) }));
      persistView(get());
    },
  };
});

/** Register visible demand; all tabs share the app-root projection bridge. */
export function watchAgentList(details = true): () => void {
  const firstList = ++listWatchers === 1;
  const firstDetail = details && ++detailWatchers === 1;
  if (firstList || firstDetail) showAgentDefinitions(true);
  return () => {
    const lastDetail = details && --detailWatchers === 0;
    if (--listWatchers === 0) showAgentDefinitions(false);
    else if (lastDetail) showAgentDefinitions(true);
  };
}

export function agentListIsVisible(): boolean {
  return listWatchers > 0;
}

export function agentDetailIsVisible(): boolean {
  return detailWatchers > 0;
}

/** Test seam: release the shared view and clear local UI state. */
export function resetAgentsStoreForTests(): void {
  try {
    globalThis.localStorage?.removeItem(VIEW_STATE_KEY);
  } catch {
    // no storage in this environment
  }
  resetAgentDefinitions(true);
  listWatchers = 0;
  detailWatchers = 0;
  listSeq = 0;
  accountsSeq = 0;
  for (const key of Object.keys(detailSeq)) delete detailSeq[key];
  useAgentsStore.setState({
    rows: null,
    listError: null,
    includeArchived: false,
    capFocusKey: null,
    selectedKey: null,
    creating: false,
    view: "overview",
    formDirty: false,
    pendingLeave: null,
    saveWarnings: {},
    details: {},
    detailError: {},
    accounts: null,
    accountsError: null,
    inUse: {},

    request: null,
    notice: null,
    tabView: "agents",
    scheduleFilter: null,
    scheduleList: "upcoming",
    scheduleRoutine: null,
    seenRuns: new Set<string>(),
    pending: {},
    actionError: null,
  });
}
