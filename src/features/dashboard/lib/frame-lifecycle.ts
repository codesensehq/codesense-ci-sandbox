// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { warmScopes } from "@/shared/lib/warm-scopes";

/** The host owns retirement; a page can finish its writes only until its deadline. */
export interface LiveFrame {
  frameId: string;
  parentFrame?: string;
  dashboard: string;
  documentPath?: string;
  panelId?: string;
  slotId?: string;
  /** The team whose view mounts the frame (not its grant's source team); gates commands and focus. */
  teamKey?: string;
  element?: HTMLIFrameElement;
  post: (message: Record<string, unknown>) => void;
  hide: () => void;
  remint?: () => void;
  beforeReload?: () => Promise<unknown>;
  scrollY?: () => number;
  freeze?: (frozen: boolean) => void;
}
interface Entry {
  frame: LiveFrame;
  pending?: {
    id: number;
    retire: boolean;
    deadline: number;
    timer: ReturnType<typeof setTimeout>;
    promise: Promise<void>;
    finish: (timedOut?: boolean) => void;
  };
  tornDown: boolean;
  ready: boolean;
  /** The page called `codesense.ready()` (or subscribed), not just the shim's handshake. */
  live: boolean;
}
const frames = new Map<string, Entry>();
/** A warm team's hidden frames take no commands or focus meant for the team on screen. */
const onScreenFrame = (frame: LiveFrame) =>
  !frame.teamKey || warmScopes.tierOf(frame.teamKey) !== "warm";
let nextId = 0;
const focusRequests = new Set<string>();
export function requestFrameFocus(panelId: string): void {
  focusRequests.add(panelId);
  for (const entry of frames.values()) {
    if (
      entry.frame.panelId === panelId &&
      entry.ready &&
      !entry.tornDown &&
      onScreenFrame(entry.frame)
    ) {
      focusRequests.delete(panelId);
      entry.frame.element?.focus();
    }
  }
}
export function markFrameReady(frameId: string): void {
  const entry = frames.get(frameId);
  if (!entry) return;
  entry.ready = true;
  if (entry.frame.panelId && focusRequests.has(entry.frame.panelId))
    requestFrameFocus(entry.frame.panelId);
}
export function registerFrame(frame: LiveFrame): () => void {
  const entry: Entry = { frame, tornDown: false, ready: false, live: false };
  frames.set(frame.frameId, entry);
  // A mounted slot owns its startup deadline and drops commands on a real fallback.
  for (const queued of commandQueue) {
    if (!commandMatchesFrame(queued, frame)) continue;
    clearTimeout(queued.timer);
    queued.timer = undefined;
  }
  return () => {
    if (!entry.tornDown && !entry.pending) {
      // Inline message rows cannot retain their host when the conversation leaves.
      entry.frame.post({ type: "codesense:flush", id: ++nextId });
      entry.frame.post({ type: "codesense:teardown" });
    }
    if (entry.pending) {
      entry.pending.retire = true;
      entry.pending.finish();
    }
    if (frames.get(frame.frameId) === entry) frames.delete(frame.frameId);
    // If the host leaves before readiness, restore the never-mounted backstop.
    for (const queued of commandQueue) {
      if (!commandMatchesFrame(queued, frame) || queued.timer) continue;
      if (![...frames.values()].some((e) => !e.tornDown && commandMatchesFrame(queued, e.frame)))
        queued.timer = setTimeout(
          () => dropQueuedCommands(queued.dashboard, queued.delivery.panelId),
          COMMAND_QUEUE_BACKSTOP_MS,
        );
    }
  };
}
/** A slot dashboard's command, as posted to its page. */
export interface DashboardCommandDelivery {
  /** A host-owned instance when the dashboard is also open as an ordinary pane. */
  panelId?: string;
  /** The id the dashboard declared. */
  id: string;
  args?: unknown;
  /** For the dropped-command toast. */
  label: string;
  dashboardTitle: string;
}
/** A dashboard command waiting for its dashboard's page to become live. */
interface QueuedCommand {
  dashboard: string;
  slotId: string;
  message: Record<string, unknown>;
  delivery: DashboardCommandDelivery;
  timer?: ReturnType<typeof setTimeout>;
}
const commandQueue: QueuedCommand[] = [];
function commandMatchesFrame(
  queued: Pick<QueuedCommand, "dashboard" | "slotId" | "delivery">,
  frame: LiveFrame,
) {
  return (
    queued.dashboard === frame.dashboard &&
    queued.slotId === frame.slotId &&
    (!queued.delivery.panelId || queued.delivery.panelId === frame.panelId)
  );
}
/** How long a queued command waits when its slot never mounts a frame at all; a frame
 *  that mounts and fails drops it sooner, through the slot's own fallback. */
export const PANE_STARTUP_TIMEOUT_MS = 30_000;
export const COMMAND_QUEUE_BACKSTOP_MS = PANE_STARTUP_TIMEOUT_MS;

function liveFrameFor(dashboard: string, slotId: string, panelId?: string): Entry | undefined {
  return [...frames.values()].find(
    (e) =>
      e.live &&
      !e.tornDown &&
      !e.pending &&
      onScreenFrame(e.frame) &&
      e.frame.dashboard === dashboard &&
      e.frame.slotId === slotId &&
      (!panelId || e.frame.panelId === panelId),
  );
}

/** Remove and return the queued commands `select` picks, with their timers stopped. */
function takeQueued(select: (queued: QueuedCommand) => boolean): QueuedCommand[] {
  const taken = commandQueue.filter(select);
  for (const queued of taken) {
    clearTimeout(queued.timer);
    commandQueue.splice(commandQueue.indexOf(queued), 1);
  }
  return taken;
}

/** The page in this frame said it is ready; deliver what was queued for it, once. */
export function markFrameLive(frameId: string): void {
  const entry = frames.get(frameId);
  if (!entry || entry.tornDown) return;
  entry.live = true;
  // A hidden team's page going live takes nothing meant for the team on screen.
  if (!onScreenFrame(entry.frame)) return;
  const { dashboard, slotId } = entry.frame;
  for (const queued of takeQueued(
    (q) =>
      q.dashboard === dashboard &&
      q.slotId === slotId &&
      (!q.delivery.panelId || q.delivery.panelId === entry.frame.panelId),
  ))
    entry.frame.post(queued.message);
}

/**
 * Post `codesense:command` to `dashboard`'s live page in `slotId`, or hold it until
 * that page calls `ready()`. Each dispatch is posted at most once; one still held
 * when the slot fails is dropped with a toast.
 */
export function postDashboardCommand(
  dashboard: string,
  slotId: string,
  delivery: DashboardCommandDelivery,
): void {
  const message = {
    type: "codesense:command",
    id: delivery.id,
    args: delivery.args,
    dispatchId: crypto.randomUUID(),
  };
  const live = liveFrameFor(dashboard, slotId, delivery.panelId);
  if (live) {
    live.frame.post(message);
    return;
  }
  commandQueue.push({
    dashboard,
    slotId,
    message,
    delivery,
    timer: [...frames.values()].some(
      (e) => !e.tornDown && commandMatchesFrame({ dashboard, slotId, delivery }, e.frame),
    )
      ? undefined
      : setTimeout(
          () => dropQueuedCommands(dashboard, delivery.panelId),
          COMMAND_QUEUE_BACKSTOP_MS,
        ),
  });
}

/** The dashboard's page didn't load: drop its queued commands, with one toast. */
export function dropQueuedCommands(dashboard: string, panelId?: string): void {
  const dropped = takeQueued(
    (q) =>
      q.dashboard === dashboard &&
      (!panelId || !q.delivery.panelId || q.delivery.panelId === panelId),
  );
  if (!dropped.length) return;
  const [first] = dropped;
  const what =
    dropped.length === 1
      ? `${first.delivery.label} didn't run`
      : `${dropped.length} commands didn't run`;
  toast.warning(`${first.delivery.dashboardTitle} pane didn't load — ${what}.`);
}

/** Whether keyboard focus is in one of `dashboard`'s frames. */
export function isDashboardFrameFocused(dashboard: string): boolean {
  const active = typeof document === "undefined" ? null : document.activeElement;
  return !!active && hasLiveFrames((f) => f.dashboard === dashboard && f.element === active);
}

export function isFrameFlushing(frameId: string): boolean {
  const entry = frames.get(frameId);
  return !!entry?.pending && !entry.tornDown;
}
export function acknowledgeFlush(frameId: string, id: unknown): void {
  const pending = frames.get(frameId)?.pending;
  if (pending && pending.id === id) pending.finish();
}
function liveEntryForElement(element: HTMLIFrameElement | null): Entry | undefined {
  return element
    ? [...frames.values()].find((e) => !e.tornDown && e.frame.element === element)
    : undefined;
}
function frameFamily(entry: Entry | undefined): Entry[] {
  const group = entry && !entry.tornDown ? [entry] : [];
  for (const parent of group) {
    for (const child of frames.values()) {
      if (!child.tornDown && child.frame.parentFrame === parent.frame.frameId) group.push(child);
    }
  }
  return group;
}
/** Keep the same owned documents frozen until this preparation ends. */
export function setFrameFrozen(element: HTMLIFrameElement | null, frozen: boolean): () => void {
  const group = frameFamily(liveEntryForElement(element));
  for (const entry of group) entry.frame.freeze?.(frozen);
  return () => {
    for (const entry of group) if (!entry.tornDown) entry.frame.freeze?.(false);
  };
}
export function restoreFrameFocus(element: HTMLIFrameElement | null): void {
  liveEntryForElement(element)?.frame.post({ type: "codesense:restore-focus" });
}
export function frameScrollY(element: HTMLIFrameElement | null): number {
  return liveEntryForElement(element)?.frame.scrollY?.() ?? 0;
}
export function captureReloadState(element: HTMLIFrameElement): Promise<unknown> {
  return liveEntryForElement(element)?.frame.beforeReload?.() ?? Promise.resolve(null);
}

/** Flush autosaves without hiding, tearing down or revoking the shown document. */
export function flushFrameWrites(element: HTMLIFrameElement | string, ms = 300): Promise<void> {
  const entry = typeof element === "string" ? frames.get(element) : liveEntryForElement(element);
  const deadline = Date.now() + ms;
  return Promise.all(
    frameFamily(entry).map((owned) =>
      flush(owned.frame.frameId, Math.max(0, deadline - Date.now()), false),
    ),
  ).then(() => {});
}
export function flushFrame(frame: string | { frameId: string }, ms = 300): Promise<void> {
  return flush(typeof frame === "string" ? frame : frame.frameId, ms, true);
}
function flush(frame: string, ms: number, retire: boolean): Promise<void> {
  const entry = frames.get(frame);
  if (!entry || entry.tornDown) return Promise.resolve();
  const deadline = Date.now() + ms;
  if (entry.pending) {
    entry.pending.retire ||= retire;
    if (deadline < entry.pending.deadline) {
      clearTimeout(entry.pending.timer);
      entry.pending.deadline = deadline;
      entry.pending.timer = setTimeout(() => entry.pending?.finish(true), ms);
    }
    return entry.pending.promise;
  }
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  const finish = (timedOut = false) => {
    if (entry.tornDown) return;
    const retiring = entry.pending?.retire;
    clearTimeout(entry.pending?.timer);
    entry.pending = undefined;
    if (retiring) entry.tornDown = true;
    if (timedOut) console.warn(`frame flush timed out: ${entry.frame.dashboard}`);
    if (retiring) entry.frame.post({ type: "codesense:teardown" });
    resolve();
  };
  entry.pending = {
    id: ++nextId,
    retire,
    deadline,
    promise,
    finish,
    timer: setTimeout(() => finish(true), ms),
  };
  entry.frame.post({ type: "codesense:flush", id: entry.pending.id });
  return promise;
}
export async function flushAll(ms = 300): Promise<void> {
  await Promise.all([...frames.keys()].map((id) => flushFrame(id, ms)));
}
export function remintFrames(select: (frame: LiveFrame) => boolean): void {
  for (const entry of frames.values()) if (select(entry.frame)) entry.frame.remint?.();
}
export function hasLiveFrames(select: (frame: LiveFrame) => boolean): boolean {
  return [...frames.values()].some((e) => !e.tornDown && select(e.frame));
}
/** Keep the caller's host mounted, hidden and inert until all its frames settle. */
export async function retireAfterFlush(
  select: (frame: LiveFrame) => boolean,
  remove: () => void,
  ms = 300,
): Promise<void> {
  const selected = [...frames.values()].filter((e) => !e.tornDown && select(e.frame));
  selected.forEach((e) => e.frame.hide());
  await Promise.all(selected.map((e) => flushFrame(e.frame.frameId, ms)));
  remove();
}
