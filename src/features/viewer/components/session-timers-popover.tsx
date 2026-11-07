// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AlarmClock, ChevronRight, Plus } from "lucide-react";
import { toast } from "sonner";

import { Popover, PopoverAnchor, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { useMachine } from "@/features/machines/stores/machines-store";
import { offerTerminalServerUpdate } from "@/features/machines/lib/machine-actions";
import { SaveSessionAsAgentDialog } from "@/features/agents/components/new-agent-dialog";
import { openRoutineDraft } from "@/features/routines/routines-virtual-tab";
import type { WakeHistoryEntry } from "@/shared/lib/wake-events";
import type { SessionTimerRow } from "@/tauri/commands";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  findTabLocationBySessionId,
  selectMinimizedTerminalTabIndex,
  selectRunningTerminalTabIndex,
  selectWorkspaceCwd,
  useViewerStore,
} from "@/shared/stores/viewer-store";

import { useNowClock } from "../hooks/use-now-clock";
import { useSessionCwd } from "../hooks/use-session-cwd";
import { useTabTimerAgent, type TabTimerAgent } from "../hooks/use-tab-timer-agent";
import {
  LOCAL_TIMER_MACHINE,
  authorWords,
  countdownLabel,
  countdownWords,
  recurrenceWords,
  routineWhenOf,
  shortTime,
  timerTargetKey,
  type TimerTarget,
} from "../lib/session-timers";
import { scrollToWakeMarker, wakeKind, wakeName } from "../lib/wake-marker";
import { useSessionTimersStore, visibleTimers } from "../stores/session-timers-store";
import { RemindForm } from "./remind-popover";

/** A timer due sooner than this is cancelled at once, with no Undo window
 *  (the 5 s toast would outlive it). */
const UNDO_WINDOW_MIN_LEAD_MS = 6_000;

/** Whether `row` fires before an Undo toast could close. */
function dueTooSoonForUndo(row: SessionTimerRow): boolean {
  return Date.parse(row.nextAt) - Date.now() < UNDO_WINDOW_MIN_LEAD_MS;
}

/** The binding sentence for an `unknown` delivery (SPEC §7, design §5.3). */
export const UNKNOWN_DELIVERY_COPY =
  "May have been delivered. Check the terminal before re-sending.";

/** Terminal controls use the app-painted overlay palette. */
export const CHROME_POPOVER =
  "w-80 max-w-[calc(100vw-16px)] gap-0 p-0 text-sm shadow-none ring-1 ring-foreground/10";

/** "This Mac", or the machine's label. */
export function useMachineLabel(machineId: string | undefined): string {
  const machine = useMachine(machineId ?? "");
  if (!machineId || machineId === LOCAL_TIMER_MACHINE) return "This Mac";
  return machine?.label || machineId;
}

/** A home directory as `~`. */
export function homeShort(path: string | null): string | null {
  if (!path) return null;
  return path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, "~");
}

/**
 * Where a tab's session runs, as people read it (`~/repos/codesense`): this
 * Mac's live cwd for a local tab; for a remote tab, its workspace's directory
 * on that machine (the live cwd is only known for local sessions). Tabs are
 * found by machine + session, since ids are only unique per terminal server.
 */
export function useTabCwd(target: TimerTarget, workspaceCwd?: string | null): string | null {
  const local = !target.machineId || target.machineId === LOCAL_TIMER_MACHINE;
  const live = useSessionCwd(local ? target.sessionId : undefined);
  const storedCwd = useViewerStore((s) => {
    for (const index of [selectRunningTerminalTabIndex(s), selectMinimizedTerminalTabIndex(s)]) {
      const found = findTabLocationBySessionId(index, target.sessionId);
      if (
        found &&
        (found.tab.origin?.machineId ?? LOCAL_TIMER_MACHINE) ===
          (target.machineId ?? LOCAL_TIMER_MACHINE)
      )
        return selectWorkspaceCwd(s, found.teamKey, found.workspaceId);
    }
    return null;
  });
  return homeShort((local ? live : null) ?? workspaceCwd ?? storedCwd);
}

export interface SessionTimersPopoverProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  target: TimerTarget;
  /** What to call the agent ("Remind Barb"); null for an unnamed session. */
  agentName: string | null;
  /** The remote tab's machine is unreachable. */
  offline?: boolean;
  /** Which page opens: the timers list (the chip) or the Remind form (the tab menu). */
  startOn?: "timers" | "remind";
  /** The element it hangs from when it has no trigger (the tab pill). */
  anchor?: HTMLElement | null;
  /** The trigger it opens from (the caption chip). */
  trigger?: ReactNode;
  /** The tab's workspace directory (a remote tab's, on its machine). */
  workspaceCwd?: string | null;
}

/**
 * The Timers popover (design §4.12) and, in place, the Remind form (§4.13):
 * one popover so "+ Remind this agent…" swaps the body and a back arrow
 * returns. "Keep this…" may open Save as agent, a modal that outlives it.
 */
export function SessionTimersPopover({
  open,
  onOpenChange,
  target,
  agentName,
  offline = false,
  startOn = "timers",
  anchor,
  trigger,
  workspaceCwd,
}: SessionTimersPopoverProps) {
  const [page, setPage] = useState<"timers" | "remind">(startOn);
  const [saving, setSaving] = useState<SessionTimerRow | null>(null);
  const agent = useTabTimerAgent(target.sessionId);
  const machineLabel = useMachineLabel(target.machineId);
  const cwd = useTabCwd(target, workspaceCwd);
  // Each opening starts on its own page (the chip's trigger opens through here).
  const change = (next: boolean) => {
    if (next) setPage(startOn);
    onOpenChange(next);
  };

  const keep = (row: SessionTimerRow) => {
    if (agent?.saved && agent.agentKey) {
      keepAsRoutine(row, agent.agentKey, target, machineLabel);
      onOpenChange(false);
      return;
    }
    // An unnamed session: save it as an agent first.
    setSaving(row);
    onOpenChange(false);
  };

  return (
    <>
      <Popover open={open} onOpenChange={change}>
        {trigger ? (
          <PopoverTrigger asChild>{trigger}</PopoverTrigger>
        ) : anchor ? (
          <PopoverAnchor virtualRef={{ current: anchor }} />
        ) : null}
        <PopoverContent
          side="bottom"
          align={trigger ? "end" : "start"}
          className={CHROME_POPOVER}
          onOpenAutoFocus={(e) => {
            // Timers focuses the dialog itself, never Cancel (it acts without a
            // confirm); Remind focuses its message (it handles that itself).
            e.preventDefault();
            const el = e.currentTarget as HTMLElement | null;
            if (page === "timers") el?.focus();
          }}
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          {...(page === "timers"
            ? { role: "dialog", "aria-label": "Timers on this tab", tabIndex: -1 }
            : {
                role: "dialog",
                "aria-label": agentName ? `Remind ${agentName}` : "Remind this agent",
                "data-testid": "remind-popover",
              })}
          {...(page === "timers" ? { "data-testid": "session-timers-popover" } : {})}
        >
          {page === "timers" ? (
            <TimersList
              target={target}
              machineLabel={machineLabel}
              offline={offline}
              keepTip={
                agent?.saved
                  ? `Make this timer a routine ${agent.name} keeps.`
                  : "Save this session as an agent, then make this timer its routine."
              }
              onRemind={() => setPage("remind")}
              onKeep={keep}
              onClose={() => onOpenChange(false)}
            />
          ) : (
            <RemindForm
              target={target}
              agentName={agentName}
              offline={offline}
              machineLabel={machineLabel}
              onBack={startOn === "timers" ? () => setPage("timers") : undefined}
              onDone={() => onOpenChange(false)}
            />
          )}
        </PopoverContent>
      </Popover>
      {saving && agent && (
        <SaveSessionAsAgentDialog
          open
          session={{ runtime: agent.runtime, cwd, machineLabel }}
          onClose={() => setSaving(null)}
          onSaved={(key, scope) => {
            const row = saving;
            setSaving(null);
            keepAsRoutine(row, key, target, machineLabel, scope);
          }}
        />
      )}
    </>
  );
}

/** Open the New routine form filled in from `row` for agent `agentKey` (§4.16).
 *  The timer keeps running until the routine is saved. */
function keepAsRoutine(
  row: SessionTimerRow,
  agentKey: string,
  target: TimerTarget,
  machineLabel: string,
  audience?: "personal" | "team",
): void {
  openRoutineDraft({
    draft: {
      agent: agentKey,
      name: row.name,
      task: row.prompt,
      when: routineWhenOf(row),
      mode: "wake",
    },
    ...(audience ? { audience } : {}),
    mode: "wake",
    machineId: target.machineId ?? LOCAL_TIMER_MACHINE,
    machineLabel,
    timer: {
      sessionId: target.sessionId,
      ...(target.machineId ? { machineId: target.machineId } : {}),
      timerId: row.timerId,
      name: row.name,
    },
  });
}

function TimersList({
  target,
  machineLabel,
  offline,
  keepTip,
  onRemind,
  onKeep,
  onClose,
}: {
  target: TimerTarget;
  machineLabel: string;
  offline: boolean;
  keepTip: string;
  onRemind: () => void;
  onKeep: (row: SessionTimerRow) => void;
  onClose: () => void;
}) {
  const key = timerTargetKey(target);
  const entry = useSessionTimersStore((s) => s.bySession[key]);
  const cancelling = useSessionTimersStore((s) => s.cancelling[key]);
  const rows = visibleTimers(entry?.rows, cancelling);
  const coarse = useNowClock(10_000);
  const soon =
    rows?.some((r) => {
      const left = Date.parse(r.nextAt) - coarse;
      return left > 0 && left < 90_000;
    }) ?? false;
  const fine = useNowClock(1_000, soon);
  const now = Math.max(fine, coarse);

  // Opened before the tab had read its timers (the tab menu): read them now.
  const known = entry !== undefined;
  useEffect(() => {
    if (!known && !offline) void useSessionTimersStore.getState().refresh(target);
  }, [known, offline, target]);

  // Cancel hides the row at once; the timer is cancelled for real when the
  // Undo toast closes (5 s), so Undo puts back exactly what was there.
  const cancel = (row: SessionTimerRow) => {
    const store = useSessionTimersStore.getState();
    store.cancel(target, row.timerId);
    const commit = () =>
      void store
        .commitCancel(target, row.timerId)
        .catch((e: unknown) => toast.error(`Couldn't cancel the timer: ${getErrorMessage(e)}`));
    // About to fire: an Undo window would let it fire first. Cancel now.
    if (dueTooSoonForUndo(row)) {
      commit();
      toast(`Timer “${row.name}” cancelled.`);
      return;
    }
    toast(`Timer “${row.name}” cancelled.`, {
      duration: 5_000,
      action: { label: "Undo", onClick: () => store.undoCancel(target, row.timerId) },
      onDismiss: commit,
      onAutoClose: commit,
    });
  };

  let body: ReactNode;
  if (entry?.oldServer) {
    body = (
      <div className="flex flex-col items-start gap-2 px-3 py-2">
        <p className="text-xs">Update the terminal server on {machineLabel} to use timers.</p>
        <Button
          size="xs"
          variant="outline"
          onClick={() => {
            onClose();
            // SW-AGR-19: through the one update router, This Mac included.
            offerTerminalServerUpdate(target.machineId ?? LOCAL_TIMER_MACHINE);
          }}
        >
          Update…
        </Button>
      </div>
    );
  } else if (rows === null) {
    body = (
      <p className="px-3 py-2 text-xs text-muted-foreground">
        {entry?.error ?? (offline ? `${machineLabel} is offline.` : "Loading timers…")}
      </p>
    );
  } else if (rows.length === 0) {
    body = <p className="px-3 py-2 text-xs text-muted-foreground">No timers on this tab.</p>;
  } else {
    body = (
      <ul className="divide-y divide-border">
        {rows.map((row) => (
          <TimerRow
            key={row.timerId}
            row={row}
            now={now}
            readOnly={offline}
            keepTip={keepTip}
            onCancel={() => cancel(row)}
            onKeep={() => onKeep(row)}
          />
        ))}
      </ul>
    );
  }

  return (
    <>
      <header className="flex items-baseline gap-2 border-b border-border px-3 py-2">
        <h2 className="flex-1 text-sm font-semibold">Timers on this tab</h2>
        <span className="text-xs whitespace-nowrap text-muted-foreground">on {machineLabel}</span>
      </header>
      {offline && (
        <p className="border-b border-border px-3 py-2 text-xs">
          {machineLabel} is offline. Its timers keep firing there; you can see and change them once
          it's back.
        </p>
      )}
      <div className={cn(offline && "opacity-60")}>{body}</div>
      {!offline && !entry?.oldServer && (
        <div className="border-t border-border p-1">
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-start"
            onClick={onRemind}
            disabled={(rows?.length ?? 0) >= 10}
          >
            <Plus />
            Remind this agent…
          </Button>
        </div>
      )}
      <WakeHistory target={target} rows={entry?.rows ?? null} onJump={onClose} />
    </>
  );
}

function TimerRow({
  row,
  now,
  readOnly,
  keepTip,
  onCancel,
  onKeep,
}: {
  row: SessionTimerRow;
  now: number;
  readOnly: boolean;
  keepTip: string;
  onCancel: () => void;
  onKeep: () => void;
}) {
  const next = Date.parse(row.nextAt);
  const label = countdownLabel(next, now);
  const due = label === "due";
  return (
    <li data-testid="session-timer-row" className="flex flex-col gap-0.5 px-3 py-2">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate font-medium">{row.name}</span>
        <span
          className={cn(
            "inline-flex shrink-0 items-center gap-1 text-xs tabular-nums",
            due ? "text-foreground" : "text-muted-foreground",
          )}
        >
          {due && <AlarmClock aria-hidden className="size-3 text-[var(--cs-status-busy)]" />}
          {due ? "due" : `in ${label}`}
        </span>
      </div>
      <p className="line-clamp-2 text-xs text-muted-foreground">“{row.prompt}”</p>
      <div className="mt-0.5 flex items-start gap-x-1 text-xs text-muted-foreground">
        {/* Wraps rather than truncates: the whole schedule and author are read. */}
        <span className="min-w-0 flex-1 pt-1">
          {recurrenceWords(row, shortTime)} · {authorWords(row.createdBy)}
        </span>
        {!readOnly && (
          <span className="flex shrink-0 items-center">
            <Button
              size="xs"
              variant="ghost"
              data-testid="timer-keep"
              title={keepTip}
              onClick={onKeep}
            >
              Keep this…
            </Button>
            <Button
              size="xs"
              variant="ghost"
              data-testid="session-timer-cancel"
              aria-label={`Cancel ${row.name}`}
              className="hover:text-destructive"
              onClick={onCancel}
            >
              Cancel
            </Button>
          </span>
        )}
      </div>
    </li>
  );
}

/** A delivery's status: a filled dot when it happened or is happening, a hollow
 *  ring when it did not or nobody knows; the word always beside it. */
const HISTORY_STATUS: Record<
  WakeHistoryEntry["status"],
  { word: string; hue: string; ring: boolean }
> = {
  delivered: { word: "Delivered", hue: "var(--cs-status-done)", ring: false },
  queued: { word: "Queued", hue: "var(--cs-status-busy)", ring: false },
  coalesced: { word: "Merged", hue: "var(--cs-status-idle)", ring: false },
  undelivered: { word: "Undelivered", hue: "var(--cs-status-failed)", ring: true },
  unknown: { word: "Unknown", hue: "var(--cs-status-needs)", ring: true },
};

function historyTime(atMs: number): string {
  const at = new Date(atMs);
  const today = new Date();
  if (at.toDateString() === today.toDateString()) return shortTime(atMs);
  // Within the week: "Tue 9:00"; older: "Oct 14".
  if (today.getTime() - atMs < 6 * 86_400_000)
    return `${new Intl.DateTimeFormat(undefined, { weekday: "short" }).format(at)} ${shortTime(atMs)}`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(at);
}

/** The tab's last 20 deliveries, timers and routine wakes alike (§9.10). */
function WakeHistory({
  target,
  rows,
  onJump,
}: {
  target: TimerTarget;
  /** The tab's timers, to name who set a timer that woke it. */
  rows: SessionTimerRow[] | null;
  onJump: () => void;
}) {
  const key = timerTargetKey(target);
  const open = useSessionTimersStore((s) => s.historyOpen[key] ?? false);
  const history = useSessionTimersStore((s) => s.historyBySession[key]);
  useEffect(() => {
    if (open) void useSessionTimersStore.getState().loadHistory(target);
  }, [open, target]);
  const ref = useRef<HTMLDetailsElement>(null);
  return (
    <details
      ref={ref}
      data-testid="wake-history"
      open={open}
      className="group/wh border-t border-border"
      onToggle={(e) =>
        useSessionTimersStore
          .getState()
          .setHistoryOpen(target, (e.currentTarget as HTMLDetailsElement).open)
      }
    >
      <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-xs font-medium text-muted-foreground select-none hover:text-foreground [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden
          className="size-3 transition-transform duration-[var(--ib-motion-tap)] group-open/wh:rotate-90"
        />
        Wake history
        {history && <span className="tabular-nums">({history.length})</span>}
      </summary>
      {history && history.length === 0 && (
        <p className="px-3 pb-2 text-xs text-muted-foreground">
          Nothing has been delivered to this tab yet.
        </p>
      )}
      {history && history.length > 0 && (
        <ol className="max-h-44 overflow-auto px-1 pb-1.5">
          {history.map((entry) => {
            const status = HISTORY_STATUS[entry.status];
            const origin = entry.origin;
            const timer =
              origin.kind === "timer" && origin.timerId
                ? rows?.find((r) => r.timerId === origin.timerId)
                : undefined;
            const kind = wakeKind(entry, timer ? authorWords(timer.createdBy) : null);
            return (
              <li key={entry.deliveryId || `${entry.marker}-${entry.atMs}`}>
                <button
                  type="button"
                  className="grid w-full grid-cols-[4.5rem_minmax(0,1fr)_auto] items-baseline gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-muted focus-visible:bg-muted focus-visible:outline-none"
                  onClick={() => {
                    scrollToWakeMarker(
                      target.machineId ?? LOCAL_TIMER_MACHINE,
                      target.sessionId,
                      entry.deliveryId,
                    );
                    onJump();
                  }}
                >
                  <span className="text-muted-foreground tabular-nums">
                    {historyTime(entry.atMs)}
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate">
                      {wakeName(entry.marker)}
                      {kind && <span className="text-muted-foreground"> · {kind}</span>}
                    </span>
                    {entry.status === "unknown" && (
                      <span className="block text-foreground">{UNKNOWN_DELIVERY_COPY}</span>
                    )}
                  </span>
                  <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
                    <span
                      aria-hidden
                      className="inline-block size-2 shrink-0 rounded-full"
                      style={
                        status.ring
                          ? { boxShadow: `inset 0 0 0 1.5px ${status.hue}` }
                          : { backgroundColor: status.hue }
                      }
                    />
                    {status.word}
                  </span>
                </button>
              </li>
            );
          })}
        </ol>
      )}
    </details>
  );
}

/** The caption's timer chip (§4.12): the next countdown, `+N` more, opening the
 *  Timers popover. Hidden when the tab has no timers. */
export function SessionTimerChip({
  tabId,
  target,
  rows,
  agent,
  offline,
  workspaceCwd,
}: {
  tabId: string;
  target: TimerTarget;
  rows: SessionTimerRow[];
  agent: TabTimerAgent | null;
  offline: boolean;
  workspaceCwd?: string | null;
}) {
  const [open, setOpen] = useState(false);
  const nextMs = Math.min(...rows.map((r) => Date.parse(r.nextAt)));
  // Seconds matter only in the last minute and a half; otherwise a 10 s tick.
  const coarse = useNowClock(10_000);
  const soon = nextMs - coarse > 0 && nextMs - coarse < 90_000;
  const fine = useNowClock(1_000, soon);
  const now = Math.max(fine, coarse);
  const label = countdownLabel(nextMs, now);
  const more = rows.length - 1;
  const nextWords = label === "due" ? "one is due" : `next ${countdownWords(nextMs, now)}`;
  return (
    <SessionTimersPopover
      open={open}
      onOpenChange={setOpen}
      target={target}
      agentName={agent?.saved ? agent.name : null}
      offline={offline}
      workspaceCwd={workspaceCwd}
      trigger={
        <button
          type="button"
          data-testid={`terminal-grid-caption-timers-${tabId}`}
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`Timers: ${rows.length}, ${nextWords}`}
          className={cn(
            "cs-timer-chip inline-flex h-5 shrink-0 items-center gap-1 rounded-[var(--ib-radius-sm)] px-1.5 text-[11px] leading-none font-medium tabular-nums transition-[background-color,opacity] duration-[var(--ib-motion-tap)]",
            "focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset",
            open
              ? "bg-[color-mix(in_srgb,var(--accent)_85%,transparent)] text-[var(--accent-foreground)] opacity-100"
              : "opacity-80 hover:bg-[color-mix(in_srgb,var(--muted)_25%,transparent)] hover:opacity-100",
          )}
        >
          <AlarmClock aria-hidden size={12} />
          <span>{label}</span>
          {more > 0 && <span className="opacity-60 @max-[11.25rem]/caption:hidden">+{more}</span>}
        </button>
      }
    />
  );
}
