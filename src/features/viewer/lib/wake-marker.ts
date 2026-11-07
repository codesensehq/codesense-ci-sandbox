// SPDX-License-Identifier: MIT
/**
 * SW-AGR-6 (R4, SPEC §9.10, design §4.15): the scrollback annotation a
 * delivered wake leaves in its tab — a thin rule and a small label pill on the
 * row above the pasted text, drawn as an xterm decoration. Nothing is written
 * to the PTY stream, so a TUI's redraw is never corrupted.
 *
 * **Where.** At the delivery point, not wherever the cursor is when the event
 * arrives: the delivered text carries its own plain-ASCII run header
 * (`[timer: ‹name› | fire N]`, `[routine: ‹name› | run ‹id›]`), so the marker
 * goes on the row above the newest line holding that header. Output can trail
 * the event, so the header is looked for again a few times; never found (the
 * line scrolled away, a TUI redrew it), there is no marker and Wake history
 * keeps the record. A header the agent's TUI wrapped in a narrow pane is
 * found across the rows it spans. While a long paste sits collapsed in the
 * agent's prompt (Claude Code's `[Pasted text #N +M lines]`) the header is not
 * on screen; the delivery event comes after the submit, when the transcript
 * shows it whole again.
 *
 * **Which terminal.** The tab's display in the terminal manager, shown or
 * parked: a delivery into a background tab is annotated too. An evicted
 * display has nothing to annotate.
 *
 * The marker is disposed with its line when that line leaves the buffer.
 */

import type { IDecoration, IMarker, Terminal } from "@xterm/xterm";

import type { WakeDeliveredEvent } from "@/shared/lib/wake-events";

/** What a marker needs of xterm. */
export type MarkerTerminal = Pick<
  Terminal,
  "registerMarker" | "registerDecoration" | "scrollToLine"
> & {
  cols?: number;
  options?: { theme?: { foreground?: string; background?: string } };
  buffer: {
    active: {
      length: number;
      baseY: number;
      cursorY: number;
      getLine(y: number): { translateToString(trim?: boolean): string } | undefined;
    };
  };
};

/** A delivery's markers by `machine|session|deliveryId`, while their lines live. */
const markers = new Map<
  string,
  {
    term: MarkerTerminal;
    marker: IMarker;
    /** Rows from the marker down to its header. */ toHeader: number;
  }
>();
/** Deliveries whose header is still being looked for. */
const pending = new Map<string, ReturnType<typeof setTimeout>>();

const keyOf = (machineId: string, sessionId: string, deliveryId: string) =>
  `${machineId}|${sessionId}|${deliveryId}`;

/** How far back from the newest line a header is looked for. */
const SEARCH_ROWS = 2_000;
/** When to look again for a header the terminal has not shown yet. */
export const MARKER_RETRY_MS = [250, 1_000, 3_000] as const;

const SET_BY = /\s\(set by ([^)]*)\)$/;

/** The delivery's name without the server's " (set by …)" suffix. */
export function wakeName(marker: string): string {
  return marker.replace(SET_BY, "");
}

/**
 * What kind of delivery it was, naming who set a timer: "routine",
 * "timer set by Barb", "timer". `timerAuthor` is the timer's `createdBy`
 * words when the tab still lists it ("set by Barb"); otherwise the marker's
 * own suffix says.
 */
export function wakeKind(
  event: Pick<WakeDeliveredEvent, "origin" | "marker">,
  timerAuthor?: string | null,
): string | null {
  if (event.origin.kind === "routine") return "routine";
  if (event.origin.kind !== "timer") return null;
  if (timerAuthor) return `timer ${timerAuthor}`;
  const by = SET_BY.exec(event.marker)?.[1];
  return by ? `timer set by ${by}` : "timer";
}

/** The pill's words: "Morning triage · routine · 9:00", "Check the build · timer set by Barb · 9:02". */
export function wakeMarkerLabel(
  event: WakeDeliveredEvent,
  time: (ms: number) => string,
  timerAuthor?: string | null,
): string {
  const kind = wakeKind(event, timerAuthor);
  return [wakeName(event.marker), kind, time(event.deliveredAtMs)].filter(Boolean).join(" · ");
}

function hoverTitle(event: WakeDeliveredEvent): string {
  const at = new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  }).format(event.deliveredAtMs);
  const run = event.origin.kind === "routine" ? ` · run ${event.origin.runId}` : "";
  return `Delivered by ${wakeName(event.marker)} at ${at}${run}`;
}

/** What narrows the search for one delivery's header. */
export interface DeliveryHint {
  /** A timer's possible fire numbers, likeliest first (from its listed
   *  `fires`), so a recurring timer's marker never lands on an earlier fire's
   *  header. Unknown (the tab no longer lists the timer): any fire below the
   *  session's last marker. */
  fires?: number[];
  /** Accept only lines at or after this one (absolute buffer line). */
  from?: number;
}

/** The header the delivery core wrote before the text, as exactly as it is known. */
function headerOf(event: WakeDeliveredEvent, fire?: number): string | null {
  // The server writes the name in ASCII with `?` for anything else (and `]`).
  const ascii = wakeName(event.marker).replace(/[^\x20-\x7e]|\]/g, "?");
  if (event.origin.kind === "routine") return `[routine: ${ascii} | run ${event.origin.runId}]`;
  if (event.origin.kind === "timer")
    return fire ? `[timer: ${ascii} | fire ${fire}]` : `[timer: ${ascii} | fire `;
  return null;
}

/** How many rows a header may span: an agent's TUI word-wraps it (and may
 *  hard-break a long run id) in a narrow pane, indenting the continuation —
 *  Claude Code does in a 60-column pane (SW-AGR-7). */
const HEADER_ROWS = 3;

/** `text` without whitespace: a wrap only ever adds or moves whitespace. */
const squash = (text: string) => text.replace(/\s+/g, "");

/** A header, prepared once per search: whole, and squashed for wrapped rows. */
interface HeaderNeedle {
  text: string;
  /** `[routine:` / `[timer:` without whitespace: it must start on the row. */
  opening: string;
  squashed: string;
}

function needle(header: string): HeaderNeedle {
  return {
    text: header,
    opening: squash(header.slice(0, header.indexOf(":") + 1)),
    squashed: squash(header),
  };
}

/** Where on row `y` the header starts, whole or wrapped onto the rows below
 *  it; -1 when it does not start there. */
function headerAt(buf: MarkerTerminal["buffer"]["active"], y: number, h: HeaderNeedle): number {
  const row = buf.getLine(y)?.translateToString(true);
  if (row === undefined) return -1;
  const whole = row.indexOf(h.text);
  if (whole >= 0) return whole;
  const start = row.indexOf(h.text.slice(0, h.text.indexOf(":") + 1));
  if (start < 0) return -1;
  const first = squash(row);
  if (!first.includes(h.opening)) return -1;
  let joined = first;
  for (let k = 1; k < HEADER_ROWS && y + k < buf.length; k++) {
    joined += squash(buf.getLine(y + k)?.translateToString(true) ?? "");
    const at = joined.indexOf(h.squashed);
    if (at >= 0) return at < first.length ? start : -1;
  }
  return -1;
}

/** An agent's prompt glyph right before the header: where a delivery is typed
 *  (an agent quoting the header in its reply has no glyph before it). */
const PROMPT_GLYPH = /[❯>›]\s*$/;

function afterPrompt(buf: MarkerTerminal["buffer"]["active"], y: number, at: number): boolean {
  const row = buf.getLine(y)?.translateToString(true) ?? "";
  return PROMPT_GLYPH.test(row.slice(0, at));
}

/** The absolute buffer line holding the delivery's header, newest first, or null.
 *  A header row right after a prompt glyph wins over a quoted one. */
export function findDeliveryLine(
  term: MarkerTerminal,
  event: WakeDeliveredEvent,
  hint: DeliveryHint = {},
): number | null {
  const headers = (hint.fires?.length ? hint.fires : [undefined])
    .map((fire) => headerOf(event, fire))
    .filter((h): h is string => h !== null)
    .map(needle);
  const buf = term.buffer.active;
  const stop = Math.max(0, buf.length - SEARCH_ROWS, hint.from ?? 0);
  // Exact fire numbers first, in order: the likeliest wins, never an older one.
  for (const header of headers) {
    let quoted: number | null = null;
    for (let y = buf.length - 1; y >= stop; y--) {
      const at = headerAt(buf, y, header);
      if (at < 0) continue;
      if (afterPrompt(buf, y, at)) return y;
      quoted ??= y;
    }
    if (quoted !== null) return quoted;
  }
  return null;
}

/** The header line of a session's newest marker: a later delivery's header is below it. */
function lastMarkedLine(machineId: string, sessionId: string): number {
  let last = -1;
  const prefix = `${machineId}|${sessionId}|`;
  for (const [key, { marker, toHeader }] of markers)
    if (key.startsWith(prefix) && !marker.isDisposed) last = Math.max(last, marker.line + toHeader);
  return last;
}

/** How far above the buffer's end at the event's arrival its paste may already
 *  sit (the paste can reach the terminal before the event reaches the app). */
const ARRIVAL_SLACK_ROWS = 200;

const ALARM_SVG =
  '<svg aria-hidden="true" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="13" r="8"/><path d="M12 9v4l2 2"/><path d="M5 3 2 6"/><path d="m22 6-3-3"/><path d="M6.38 18.7 4 21"/><path d="M17.64 18.67 20 21"/></svg>';

/** Paint the decoration's element: the rule and the pill (styled by `.cs-wake-marker`). */
function paint(
  el: HTMLElement,
  label: string,
  title: string,
  theme: { foreground?: string; background?: string } | undefined,
): void {
  if (el.dataset.testid === "wake-marker") return;
  // Tinted from the terminal's own colours, so it reads on any terminal theme.
  if (theme?.foreground) el.style.setProperty("--cs-wake-fg", theme.foreground);
  if (theme?.background) el.style.setProperty("--cs-wake-bg", theme.background);
  el.dataset.testid = "wake-marker";
  el.setAttribute("aria-hidden", "true");
  el.classList.add("cs-wake-marker");
  el.title = title;
  const pill = document.createElement("span");
  pill.className = "cs-wake-marker-pill";
  pill.innerHTML = ALARM_SVG;
  pill.append(document.createTextNode(label));
  el.append(pill);
}

/**
 * Annotate `term` for a delivered wake, at its delivery point. True when the
 * marker is (or already was) placed; false when the header is not in the
 * buffer (yet). A re-read delivery (`replayed`) is never annotated.
 */
export function annotateWake(
  term: MarkerTerminal,
  event: WakeDeliveredEvent,
  label: string,
  hint: DeliveryHint = {},
): boolean {
  if (event.replayed) return true;
  const key = keyOf(event.machineId, event.sessionId, event.deliveryId);
  if (markers.has(key)) return true;
  // Never an older delivery's header: only below this session's last marker.
  const from = Math.max(hint.from ?? 0, lastMarkedLine(event.machineId, event.sessionId) + 1);
  const line = findDeliveryLine(term, event, { ...hint, from });
  if (line === null) return false;
  const buf = term.buffer.active;
  // Offsets count from the cursor's row; the marker sits on the row above the
  // header. A header on the buffer's first row keeps its marker (Wake history
  // jumps to it) but gets no decoration: it would cover the header itself.
  const above = line > 0;
  const marker = term.registerMarker((above ? line - 1 : line) - (buf.baseY + buf.cursorY));
  if (!marker) return false;
  markers.set(key, { term, marker, toHeader: above ? 1 : 0 });
  marker.onDispose(() => markers.delete(key));
  if (!above) return true;
  // Full width, one row high, on the row above: it never covers the pasted text.
  const decoration: IDecoration | undefined = term.registerDecoration({
    marker,
    x: 0,
    width: term.cols ?? 1,
    height: 1,
    layer: "top",
  });
  const title = hoverTitle(event);
  const theme = term.options?.theme;
  decoration?.onRender((el) => paint(el, label, title, theme));
  return true;
}

/**
 * Annotate the delivery's tab, whichever display shows it (`terminalOf`
 * finds it, parked or shown), looking again while its output catches up.
 */
export function annotateDelivery(
  event: WakeDeliveredEvent,
  label: string,
  terminalOf: (machineId: string, sessionId: string) => MarkerTerminal | null,
  hint: Omit<DeliveryHint, "from"> = {},
): void {
  if (event.replayed) return;
  const key = keyOf(event.machineId, event.sessionId, event.deliveryId);
  if (markers.has(key) || pending.has(key)) return;
  // The buffer's end when the event arrived: the paste is near it or after it.
  const arrived = terminalOf(event.machineId, event.sessionId)?.buffer.active.length;
  const from = arrived === undefined ? 0 : Math.max(0, arrived - ARRIVAL_SLACK_ROWS);
  const attempt = (n: number) => {
    pending.delete(key);
    const term = terminalOf(event.machineId, event.sessionId);
    if (term && annotateWake(term, event, label, { ...hint, from })) return;
    if (n < MARKER_RETRY_MS.length)
      pending.set(
        key,
        setTimeout(() => attempt(n + 1), MARKER_RETRY_MS[n]),
      );
  };
  attempt(0);
}

/** Scroll a tab's terminal to a delivery's marker; false once its line is gone. */
export function scrollToWakeMarker(
  machineId: string,
  sessionId: string,
  deliveryId: string,
): boolean {
  const found = markers.get(keyOf(machineId, sessionId, deliveryId));
  if (!found || found.marker.isDisposed || found.marker.line < 0) return false;
  found.term.scrollToLine(found.marker.line);
  return true;
}

export function resetWakeMarkersForTests(): void {
  for (const t of pending.values()) clearTimeout(t);
  pending.clear();
  markers.clear();
}
