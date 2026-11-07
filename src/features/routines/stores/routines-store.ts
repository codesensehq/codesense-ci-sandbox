// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { createListenerGroup, type ListenerGroup } from "@/shared/lib/tauri-listeners";
import {
  AGENTS_CHANGED_EVENT,
  ROUTINES_CHANGED_EVENT,
  type RoutinesChangedPayload,
} from "@/tauri/events";

import { reasonCopy } from "../lib/routine-display";
import type { SentenceDraft } from "../lib/routine-sentence";
import {
  agentsList,
  routineAcknowledgeMissed,
  routineArchive,
  routineArmHere,
  routineRefreshOnMachine,
  routineCopyToMine,
  routineGet,
  routineList,
  routineNeedsYouCount,
  routineOpenAuthoringAgent,
  routineRestoreRevision,
  routineRunDetail,
  routineRunNow,
  routineRuns,
  routineRunsSince,
  routineSave,
  routineSchedulerStatus,
  routineSetEnabled,
  routineStopRun,
  type AgentLaunchWarning,
  type AgentRow,
  type RoutineAdmission,
  type RoutineDetail,
  type RoutineForm,
  type RoutineRow,
  type RoutineRun,
  type RoutineRunDetail,
  type RoutineSaveReply,
} from "@/tauri/commands";

/** The selected routine's views. */
export type RoutineView = "overview" | "runs" | "history";

/** Where the user asked to go while the Overview or New routine form held an
 *  unsaved edit. */
export type RoutineLeave =
  | { kind: "select"; key: string | null }
  | { kind: "create" }
  /** Anywhere else (another agent, back to the agent, another view): `run` goes there. */
  | { kind: "then"; run: () => void };

/**
 * The Routines tab's state: the listed routines, the selected routine's runs, the
 * needs-you and missed counts, and which actions are in flight.
 *
 * Every read is async and re-run on `routines:changed` (debounced, so a burst of
 * admissions and endings costs one read); nothing polls. Actions keep a local
 * pending flag so a second click while one is in flight does nothing.
 */
interface RoutinesState {
  /** `null` until the first list read lands. */
  rows: RoutineRow[] | null;
  listError: string | null;
  needsYouCount: number;
  missedCount: number;
  selectedKey: string | null;
  /** The New routine form is open in place of a selected routine. */
  creating: boolean;
  /** What the open New routine form was filled in from ("Keep this…" on a
   *  timer), or null for a blank form. */
  createSeed: RoutineCreateSeed | null;
  /** Bumped by every New routine open, so a refill remounts the form. */
  createSeq: number;
  /** SW-AGR-5: the agent whose routine page is open in the Agents tab (the selected
   *  routine, or New routine with this agent fixed); null with no routine page open. */
  routineOwner: string | null;
  /** The selected routine's Overview, or the New routine form, has an unsaved edit. */
  formDirty: boolean;
  /** Every routine's runs of the last days (the Schedule's Recent), newest first;
   *  null until read. */
  recent: RoutineRun[] | null;
  recentError: string | null;
  /** A switch away from a dirty form, waiting on the user to discard it. */
  pendingLeave: RoutineLeave | null;
  view: RoutineView;
  /** Each read routine with its revisions, by key. Read only for History: the
   *  listed row already carries the definition the Overview edits. */
  details: Record<string, RoutineDetail>;
  detailError: Record<string, string>;
  runs: Record<string, RoutineRun[]>;
  nextBefore: Record<string, string | null>;
  runsError: Record<string, string>;
  /** The run opened from the Runs view. */
  openRun: RoutineRunDetail | null;
  openRunError: string | null;
  /** What the last Run now did when it did not start a run. */
  notice: string | null;
  /** What each routine's last save here was warned about (e.g. a `/command` its agent
   *  does not start with), by key. The save went through. */
  saveWarnings: Record<string, AgentLaunchWarning[]>;
  /** In-flight actions by `<action>:<key or run id>`. */
  pending: Record<string, true>;
  /** The enabled state a toggle in flight asked for, by routine key. */
  pendingEnabled: Record<string, boolean>;
  actionError: string | null;
  /** The list shows only the routines using this agent while set: the Agents tab's
   *  "used by N routines" link sets it, the list's chip clears it. */
  agentFilter: RoutineAgentFilter | null;
  /** Every agent you can see, archived ones included, for the sentence's agent picker;
   *  `null` until read. */
  agents: AgentRow[] | null;
  agentsError: string | null;

  loadList: () => Promise<void>;
  /** Read Recent: every routine's runs of the last {@link RECENT_DAYS} days. */
  loadRecent: () => Promise<void>;
  loadAgents: () => Promise<void>;
  loadCounts: () => Promise<void>;
  loadRuns: (key: string) => Promise<void>;
  loadMoreRuns: (key: string) => Promise<void>;
  select: (key: string | null) => void;
  setView: (view: RoutineView) => void;
  loadDetail: (key: string) => Promise<void>;
  /** Open the New routine form, blank or filled in from `seed`. */
  startCreate: (seed?: RoutineCreateSeed) => void;
  cancelCreate: () => void;
  setFormDirty: (dirty: boolean) => void;
  /** Drop the unsaved edit and go where the user asked. */
  confirmLeave: () => void;
  /** Stay on the form with its edit. */
  cancelLeave: () => void;
  /** Create (no `edit`) or edit a routine. Rejects with the backend's message;
   *  a stale edit resolves as a conflict and writes nothing. */
  saveRoutine: (
    form: RoutineForm,
    edit?: { key: string; baseRevision: number },
    enabled?: boolean,
    team?: { org: string; team: string },
  ) => Promise<RoutineSaveReply>;
  restoreRevision: (key: string, revision: number, baseRevision: number) => Promise<void>;
  /** Open a visible agent primed with `/routine` in the active team workspace. */
  describeToAgent: () => Promise<void>;
  setEnabled: (key: string, enabled: boolean) => Promise<void>;
  runNow: (key: string) => Promise<void>;
  stopRun: (runId: string) => Promise<void>;
  copyToMine: (key: string) => Promise<void>;
  armHere: (key: string) => Promise<void>;
  /** SW-XS-25: prepare a machine-armed routine on its machine from what it is now. */
  refreshOnMachine: (key: string) => Promise<void>;
  acknowledgeMissed: () => Promise<void>;
  openRunDetail: (runId: string) => Promise<void>;
  closeRunDetail: () => void;
  dismissActionError: () => void;
  dismissNotice: () => void;
  setAgentFilter: (filter: RoutineAgentFilter | null) => void;
  /** Open `key`'s routine page under its agent. */
  openRoutine: (agentKey: string | null, key: string) => void;
  /** Open New routine with `agentKey` fixed as its agent, blank or filled in from `seed`. */
  startCreateFor: (agentKey: string, seed?: RoutineCreateSeed) => void;
  /** Close the routine page (back to the agent), asking first over an unsaved edit. */
  closeRoutine: () => void;
  /** Run `then` now, or once the user agrees to drop an unsaved edit. */
  requestLeave: (then: () => void) => void;
  /** Archive a routine (it stops running; History restores it) and close its page. */
  archiveRoutine: (key: string) => Promise<void>;
  /** Move a routine to another agent: it then lives, and shows, under that agent. */
  moveRoutine: (key: string, agentRef: string, baseRevision: number) => Promise<boolean>;
}

/** SW-AGR-6 "Keep this…": a New routine form filled in from a session timer.
 *  `mode` and `machineId` are for the sentence form's mode and machine slots
 *  (SW-AGR-5); the form today reads `draft` and `audience`. */
export interface RoutineCreateSeed {
  draft: Partial<SentenceDraft>;
  /** Who the routine is for: the agent's own space. Omitted, the form's default. */
  audience?: "personal" | "team";
  mode?: "wake" | "fresh";
  /** The machine the timer's tab runs on (`local` for this Mac). */
  machineId?: string;
  /** That machine as people call it ("This Mac", "codesense-test-1"). */
  machineLabel?: string;
  /** The timer it came from: once the routine is saved, the form offers to
   *  cancel it (it keeps running until then). */
  timer?: { sessionId: string; machineId?: string; timerId: string; name: string };
}

/** Told whenever a routine page closes (back, archive, move). */
const closedListeners = new Set<() => void>();

/** Run `listener` whenever a routine page closes; returns its removal. */
export function onRoutineClosed(listener: () => void): () => void {
  closedListeners.add(listener);
  return () => closedListeners.delete(listener);
}

/** One agent's routines, as the Agents tab counted them. */
export interface RoutineAgentFilter {
  agentKey: string;
  name: string;
  /** The routines using the agent, by key. */
  routineKeys: string[];
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const RUNS_PAGE = 50;
/** The most runs one read returns (the backend's page cap). */
const MAX_RUNS_READ = 200;

/** A manual fire that did not start a run, in words; null when it did. */
function admissionNotice(name: string, admission: RoutineAdmission): string | null {
  switch (admission.outcome) {
    case "queued":
    case "duplicate":
      return null;
    case "held":
      return `${name} will run after the run still going.`;
    case "coalesced":
      return `${name} is already running; this run was merged into it.`;
    case "skipped":
      return `${name} didn't run: ${
        reasonCopy({ phase: "skipped", reason: admission.reason }) ?? admission.reason
      }.`;
  }
}

/** Reads of the routines `routines:changed` can make stale; the tab and the nav
 *  entry each hold a watch while they are mounted. */
let countWatchers = 0;
let listWatchers = 0;
/** Holders of the list alone (the sidebar: next fires and queued wakes). */
let rowWatchers = 0;
/** Holders of Recent (the Schedule's Recent list). */
let recentWatchers = 0;
let recentSeq = 0;

/** How far back the Schedule's Recent reaches, and the most runs it reads. */
export const RECENT_DAYS = 7;
/** The most runs Recent shows per routine (merged and skipped fires ride along). */
const RECENT_PER_ROUTINE = 50;
const DAY_MS = 24 * 60 * 60_000;
let listeners: ListenerGroup | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;

/** How long a burst of change events is gathered into one re-read. */
const REFRESH_DEBOUNCE_MS = 150;

/** Request counters: a reply lands only if no newer read (or a close or a
 *  re-select) has been issued since, so an older reply can never overwrite a
 *  newer one or reopen a run the user closed. */
let openRunSeq = 0;
let agentsSeq = 0;
const runsSeq: Record<string, number> = {};
const detailSeq: Record<string, number> = {};

export const useRoutinesStore = create<RoutinesState>((set, get) => {
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
      scheduleRefresh();
    }
  }

  return {
    rows: null,
    listError: null,
    needsYouCount: 0,
    missedCount: 0,
    selectedKey: null,
    creating: false,
    createSeed: null,
    createSeq: 0,
    routineOwner: null,
    formDirty: false,
    recent: null,
    recentError: null,
    pendingLeave: null,
    view: "overview",
    details: {},
    detailError: {},
    runs: {},
    nextBefore: {},
    runsError: {},
    openRun: null,
    openRunError: null,
    pending: {},
    pendingEnabled: {},
    actionError: null,
    notice: null,
    saveWarnings: {},
    agentFilter: null,
    agents: null,
    agentsError: null,

    loadRecent: async () => {
      const seq = ++recentSeq;
      try {
        const runs = await routineRunsSince(Date.now() - RECENT_DAYS * DAY_MS, RECENT_PER_ROUTINE);
        if (seq !== recentSeq) return;
        set({ recent: runs, recentError: null });
      } catch (e) {
        if (seq !== recentSeq) return;
        set({ recentError: errorText(e) });
      }
    },

    loadAgents: async () => {
      const seq = ++agentsSeq;
      try {
        const agents = await agentsList(true);
        if (seq !== agentsSeq) return;
        set({ agents, agentsError: null });
      } catch (e) {
        if (seq !== agentsSeq) return;
        set({ agentsError: errorText(e) });
      }
    },

    loadList: async () => {
      try {
        const rows = await routineList();
        // The list already says which routines need the user; no separate read.
        set({
          rows,
          listError: null,
          needsYouCount: rows.filter((r) => r.needs_you).length,
        });
      } catch (e) {
        set({ listError: errorText(e) });
      }
    },

    loadCounts: async () => {
      // While the tab lists the routines, the list carries the needs-you count.
      const needsRead =
        listWatchers > 0
          ? Promise.reject(new Error("read from the list"))
          : Promise.resolve().then(() => routineNeedsYouCount());
      const [needs, status] = await Promise.allSettled([
        needsRead,
        Promise.resolve().then(() => routineSchedulerStatus()),
      ]);
      set({
        ...(needs.status === "fulfilled" ? { needsYouCount: needs.value } : {}),
        ...(status.status === "fulfilled" ? { missedCount: status.value.missed_count } : {}),
      });
    },

    loadRuns: async (key) => {
      // A re-read keeps however many runs are already shown, so live updates
      // never fold away older pages the user opened.
      const shown = get().runs[key]?.length ?? 0;
      const limit = Math.min(Math.max(RUNS_PAGE, shown), MAX_RUNS_READ);
      const seq = (runsSeq[key] = (runsSeq[key] ?? 0) + 1);
      try {
        const page = await routineRuns(key, limit);
        if (runsSeq[key] !== seq) return;
        // An older page opened while this read was out: re-read at the new size
        // rather than fold it away.
        if ((get().runs[key]?.length ?? 0) > limit && limit < MAX_RUNS_READ) {
          void get().loadRuns(key);
          return;
        }
        set((s) => {
          const runsError = { ...s.runsError };
          delete runsError[key];
          return {
            runs: { ...s.runs, [key]: page.runs },
            nextBefore: { ...s.nextBefore, [key]: page.next_before },
            runsError,
          };
        });
      } catch (e) {
        if (runsSeq[key] !== seq) return;
        set((s) => ({ runsError: { ...s.runsError, [key]: errorText(e) } }));
      }
    },

    loadMoreRuns: async (key) => {
      const before = get().nextBefore[key];
      const id = `more:${key}`;
      if (!before || get().pending[id]) return;
      set((s) => ({ pending: { ...s.pending, [id]: true } }));
      try {
        const page = await routineRuns(key, RUNS_PAGE, before);
        set((s) => {
          // Only extend the page this read continued; a re-read that landed
          // meanwhile already moved the cursor.
          if (s.nextBefore[key] !== before) return {};
          const seen = new Set((s.runs[key] ?? []).map((r) => r.run_id));
          return {
            runs: {
              ...s.runs,
              [key]: [...(s.runs[key] ?? []), ...page.runs.filter((r) => !seen.has(r.run_id))],
            },
            nextBefore: { ...s.nextBefore, [key]: page.next_before },
          };
        });
      } catch (e) {
        set({ actionError: errorText(e) });
      } finally {
        set((s) => {
          const pending = { ...s.pending };
          delete pending[id];
          return { pending };
        });
      }
    },

    select: (key) => {
      const { formDirty, selectedKey, creating } = get();
      if (formDirty && (creating || key !== selectedKey)) {
        set({ pendingLeave: { kind: "select", key } });
        return;
      }
      openRunSeq += 1;
      set({ selectedKey: key, creating: false, openRun: null, openRunError: null });
      if (key) {
        void get().loadRuns(key);
        if (get().view === "history") void get().loadDetail(key);
      }
    },

    setView: (view) => {
      set({ view });
      const key = get().selectedKey;
      if (view === "history" && key) void get().loadDetail(key);
    },

    loadDetail: async (key) => {
      const seq = (detailSeq[key] = (detailSeq[key] ?? 0) + 1);
      try {
        const detail = await routineGet(key);
        if (detailSeq[key] !== seq) return;
        set((s) => {
          const detailError = { ...s.detailError };
          delete detailError[key];
          return { details: { ...s.details, [key]: detail }, detailError };
        });
      } catch (e) {
        if (detailSeq[key] !== seq) return;
        set((s) => ({ detailError: { ...s.detailError, [key]: errorText(e) } }));
      }
    },

    startCreate: (seed) => {
      if (get().creating && !seed) return;
      if (get().formDirty) {
        set({ pendingLeave: { kind: "create" }, createSeed: seed ?? null });
        return;
      }
      openRunSeq += 1;
      set((s) => ({
        creating: true,
        createSeed: seed ?? null,
        createSeq: s.createSeq + 1,
        selectedKey: null,
        openRun: null,
        openRunError: null,
      }));
    },

    cancelCreate: () => set({ creating: false, createSeed: null }),

    setFormDirty: (dirty) => {
      // A clean (or unmounted) form has nothing to discard, so a pending prompt is moot.
      if (!dirty && get().pendingLeave !== null) set({ formDirty: false, pendingLeave: null });
      else if (get().formDirty !== dirty) set({ formDirty: dirty });
    },

    confirmLeave: () => {
      const leave = get().pendingLeave;
      set({ pendingLeave: null, formDirty: false });
      if (leave?.kind === "select") get().select(leave.key);
      else if (leave?.kind === "create") get().startCreate(get().createSeed ?? undefined);
      else if (leave?.kind === "then") leave.run();
    },

    cancelLeave: () => set({ pendingLeave: null }),

    saveRoutine: async (form, edit, enabled, team) => {
      const reply = await routineSave(form, edit, enabled, team);
      if (reply.outcome === "saved") {
        set({ saveWarnings: { ...get().saveWarnings, [reply.key]: reply.warnings } });
        await get().loadList();
        if (!edit) {
          // The new routine is saved: nothing is left to discard.
          set({ formDirty: false, createSeed: null });
          get().select(reply.key);
          set({ view: "overview" });
        }
      }
      return reply;
    },

    restoreRevision: (key, revision, baseRevision) =>
      guarded(`restore:${key}`, async () => {
        const reply = await routineRestoreRevision(key, revision, baseRevision);
        if (reply.outcome === "saved") {
          set({ saveWarnings: { ...get().saveWarnings, [key]: reply.warnings } });
        }
        if (reply.outcome === "conflict") {
          set({ notice: "Changed elsewhere — reloaded. Pick the revision to restore again." });
        }
        await get().loadDetail(key);
        await get().loadList();
      }),

    describeToAgent: () =>
      guarded("describe", async () => {
        set({ notice: null });
        const agent = await routineOpenAuthoringAgent();
        if (!agent.opened) set({ notice: agent.message });
      }),

    setEnabled: async (key, enabled) => {
      if (get().pending[`enable:${key}`]) return;
      set((s) => ({ pendingEnabled: { ...s.pendingEnabled, [key]: enabled } }));
      await guarded(`enable:${key}`, async () => {
        // SW-XS-25: resumed, but not prepared on its machine: say why.
        const warning = await routineSetEnabled(key, enabled);
        if (warning) set({ notice: warning });
        await get().loadList();
      });
      set((s) => {
        const pendingEnabled = { ...s.pendingEnabled };
        delete pendingEnabled[key];
        return { pendingEnabled };
      });
    },

    runNow: (key) =>
      guarded(`run:${key}`, async () => {
        set({ notice: null });
        const admission = await routineRunNow(key);
        const name = get().rows?.find((r) => r.key === key)?.def.name ?? "The routine";
        set({ notice: admissionNotice(name, admission) });
        if (get().selectedKey === key) await get().loadRuns(key);
      }),

    stopRun: (runId) => guarded(`stop:${runId}`, () => routineStopRun(runId)),

    copyToMine: (key) =>
      guarded(`copy:${key}`, async () => {
        await routineCopyToMine(key);
        await get().loadList();
      }),

    refreshOnMachine: (key) =>
      guarded(`refresh:${key}`, async () => {
        await routineRefreshOnMachine(key);
        await get().loadList();
      }),

    armHere: (key) =>
      guarded(`arm:${key}`, async () => {
        await routineArmHere(key);
        await get().loadList();
      }),

    acknowledgeMissed: () =>
      guarded("ack", async () => {
        await routineAcknowledgeMissed();
        await get().loadCounts();
      }),

    openRunDetail: async (runId) => {
      const seq = ++openRunSeq;
      set({ openRunError: null });
      try {
        const run = await routineRunDetail(runId);
        if (seq === openRunSeq) set({ openRun: run });
      } catch (e) {
        if (seq === openRunSeq) set({ openRun: null, openRunError: errorText(e) });
      }
    },

    closeRunDetail: () => {
      openRunSeq += 1;
      set({ openRun: null, openRunError: null });
    },

    dismissActionError: () => set({ actionError: null }),

    dismissNotice: () => set({ notice: null }),

    setAgentFilter: (filter) => set({ agentFilter: filter }),

    openRoutine: (agentKey, key) => {
      const { selectedKey, routineOwner, creating } = get();
      if (selectedKey === key && routineOwner === agentKey && !creating) return;
      get().requestLeave(() => {
        set({ routineOwner: agentKey, creating: false, formDirty: false, view: "overview" });
        get().select(key);
      });
    },

    startCreateFor: (agentKey, seed) => {
      if (get().creating && get().routineOwner === agentKey && !seed) return;
      get().requestLeave(() => {
        set({ routineOwner: agentKey, creating: false, formDirty: false });
        get().startCreate(seed);
      });
    },

    closeRoutine: () =>
      get().requestLeave(() => {
        openRunSeq += 1;
        for (const listener of closedListeners) listener();
        set({
          routineOwner: null,
          selectedKey: null,
          creating: false,
          createSeed: null,
          formDirty: false,
          openRun: null,
          openRunError: null,
        });
      }),

    archiveRoutine: (key) =>
      guarded(`archive:${key}`, async () => {
        await routineArchive(key);
        set({ formDirty: false });
        get().closeRoutine();
        await get().loadList();
      }),

    moveRoutine: async (key, agentRef, baseRevision) => {
      let moved = false;
      await guarded(`move:${key}`, async () => {
        const reply = await routineSave({ agent: agentRef }, { key, baseRevision });
        if (reply.outcome === "conflict") {
          set({ notice: "Changed elsewhere — reloaded. Pick the agent again." });
        } else {
          moved = true;
          set({ formDirty: false });
          get().closeRoutine();
        }
        await get().loadList();
      });
      return moved;
    },

    requestLeave: (then) => {
      if (get().formDirty) {
        set({ pendingLeave: { kind: "then", run: then } });
        return;
      }
      then();
    },
  };
});

/** Re-read the agents too on this burst: only an agent change, or a routine
 *  definition change (from any source) that moves an agent's "used by", makes them
 *  stale, never a run moving. */
let agentsStale = false;

/** Re-read what a change may have made stale, once per burst. */
function scheduleRefresh(agents = false): void {
  if (agents) agentsStale = true;
  if (refreshTimer !== null) return;
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    const store = useRoutinesStore.getState();
    const reloadAgents = agentsStale;
    agentsStale = false;
    if (countWatchers > 0 || listWatchers > 0) void store.loadCounts();
    if (rowWatchers > 0 && listWatchers === 0) void store.loadList();
    if (recentWatchers > 0) void store.loadRecent();
    if (listWatchers > 0) {
      void store.loadList();
      if (reloadAgents) void store.loadAgents();
      const { selectedKey, openRun } = store;
      if (selectedKey) {
        void store.loadRuns(selectedKey);
        if (store.view === "history") void store.loadDetail(selectedKey);
      }
      if (openRun) void store.openRunDetail(openRun.run_id);
    }
  }, REFRESH_DEBOUNCE_MS);
}

function ensureListening(): void {
  if (listeners) return;
  listeners = createListenerGroup();
  listeners.track(
    listen<RoutinesChangedPayload>(ROUTINES_CHANGED_EVENT, (e) =>
      scheduleRefresh(e.payload.run_id === null),
    ),
  );
  // A routine shows its agent's name and archived state, which the Agents tab changes.
  listeners.track(listen(AGENTS_CHANGED_EVENT, () => scheduleRefresh(true)));
}

function releaseListening(): void {
  if (countWatchers > 0 || listWatchers > 0 || rowWatchers > 0 || recentWatchers > 0) return;
  if (!listeners) return;
  listeners.dispose();
  listeners = null;
}

/** Keep the needs-you and missed counts live while the caller is mounted. */
export function watchRoutineCounts(): () => void {
  countWatchers += 1;
  ensureListening();
  void useRoutinesStore.getState().loadCounts();
  return () => {
    countWatchers -= 1;
    releaseListening();
  };
}

/** Keep the list, the counts and the selected routine's runs live while the tab
 *  is mounted. */
export function watchRoutineList(): () => void {
  listWatchers += 1;
  ensureListening();
  const store = useRoutinesStore.getState();
  void store.loadList();
  void store.loadAgents();
  void store.loadCounts();
  if (store.selectedKey) {
    void store.loadRuns(store.selectedKey);
    if (store.view === "history") void store.loadDetail(store.selectedKey);
  }
  return () => {
    listWatchers -= 1;
    releaseListening();
  };
}

/** Keep only the list live (each routine's next fire and queued wake) while the caller is
 *  mounted: the sidebar's Start rows and `due` markers, without runs or agents. */
export function watchRoutineRows(): () => void {
  rowWatchers += 1;
  ensureListening();
  if (useRoutinesStore.getState().rows === null) void useRoutinesStore.getState().loadList();
  return () => {
    rowWatchers -= 1;
    releaseListening();
  };
}

/** Keep Recent live while the Schedule's Recent list is shown: read once, then again on
 *  every routine or run change. */
export function watchRecentRuns(): () => void {
  recentWatchers += 1;
  ensureListening();
  void useRoutinesStore.getState().loadRecent();
  return () => {
    recentWatchers -= 1;
    releaseListening();
  };
}

/** Test seam: forget every watch, timer and read. */
export function resetRoutinesStoreForTests(): void {
  if (refreshTimer !== null) clearTimeout(refreshTimer);
  refreshTimer = null;
  listeners?.dispose();
  listeners = null;
  countWatchers = 0;
  listWatchers = 0;
  rowWatchers = 0;
  recentWatchers = 0;
  recentSeq = 0;
  openRunSeq = 0;
  agentsSeq = 0;
  agentsStale = false;
  for (const key of Object.keys(runsSeq)) delete runsSeq[key];
  for (const key of Object.keys(detailSeq)) delete detailSeq[key];
  useRoutinesStore.setState({
    rows: null,
    listError: null,
    needsYouCount: 0,
    missedCount: 0,
    selectedKey: null,
    creating: false,
    createSeed: null,
    createSeq: 0,
    routineOwner: null,
    formDirty: false,
    recent: null,
    recentError: null,
    pendingLeave: null,
    view: "overview",
    details: {},
    detailError: {},
    runs: {},
    nextBefore: {},
    runsError: {},
    openRun: null,
    openRunError: null,
    pending: {},
    pendingEnabled: {},
    actionError: null,
    notice: null,
    saveWarnings: {},
    agentFilter: null,
    agents: null,
    agentsError: null,
  });
}
