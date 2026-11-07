// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import type { SessionTimerAsk, SessionTimerRow } from "@/tauri/commands";
import type { WakeDeliveredEvent, WakeHistoryEntry } from "@/shared/lib/wake-events";

import { isAgentTeamWorld } from "./agents-team";

/**
 * SW-AGR-6: session timers in the mock — `session_timers`, `remind_session`,
 * `cancel_session_timer` and `wake_history`, as the terminal server that runs a
 * tab would answer them. The `?agents=team` world seeds timers on two tabs: the
 * saved Notifications triage agent (one set by the agent, one recurring set by
 * you) and the unnamed login lead (a starter). `globalThis.__mockSessionTimers
 * .deliver(sessionId, marker)` plays a delivery (the `wake-delivered` event),
 * so "Woken by" and the scrollback marker can be seen.
 */

const MIN = 60_000;
const LOCAL = "local";

function iso(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
}

function parseDuration(raw: string): number {
  let total = 0;
  for (const m of raw.matchAll(/(\d+)([dhms])/g)) {
    total += Number(m[1]) * { d: 86_400_000, h: 3_600_000, m: MIN, s: 1_000 }[m[2] as "d"];
  }
  return total;
}

function seed(now: number): Record<string, SessionTimerRow[]> {
  const row = (sessionId: string, patch: Partial<SessionTimerRow>): SessionTimerRow => ({
    timerId: `t-${Math.random().toString(36).slice(2, 8)}`,
    name: "",
    prompt: "",
    nextAt: iso(now + 18 * MIN),
    every: null,
    until: null,
    count: null,
    remaining: null,
    fires: 0,
    createdBy: "you",
    machineId: LOCAL,
    sessionId,
    ...patch,
  });
  const noon = new Date(now);
  noon.setHours(noon.getHours() + 3, 0, 0, 0);
  return {
    "sess-notify-lead": [
      row("sess-notify-lead", {
        name: "Check the build",
        prompt: "Check whether CI on #1973 finished and fix it if red",
        nextAt: iso(now + 18 * MIN + 20_000),
        createdBy: "agent:Notifications triage",
      }),
      row("sess-notify-lead", {
        name: "Inbox nudge",
        prompt: "Read your inbox and answer anything addressed to you",
        nextAt: iso(now + 42 * MIN),
        every: "15m",
        until: iso(noon.getTime()),
      }),
    ],
    "sess-login-lead": [
      row("sess-login-lead", {
        name: "Re-run the repro",
        prompt: "Re-run the throttled-backend repro and post the timings to the inbox",
        nextAt: iso(now + 1 * 3_600_000 + 40 * MIN),
        every: "30m",
        count: 8,
        remaining: 5,
        fires: 3,
        createdBy: "agent:Login timeout lead",
      }),
    ],
  };
}

function history(now: number): Record<string, WakeHistoryEntry[]> {
  return {
    "sess-notify-lead": [
      {
        status: "delivered",
        deliveredAtMs: now - 95 * MIN,
        deliveryId: "d-mock-1",
        marker: "Morning triage",
        origin: { kind: "routine", routineKey: "acme/backend/triage/morning", runId: "run_1a" },
        atMs: now - 95 * MIN,
      },
      {
        status: "delivered",
        deliveredAtMs: now - 45 * MIN,
        deliveryId: "d-mock-2",
        marker: "Check CI (set by Notifications triage)",
        origin: { kind: "timer", timerId: "t-old" },
        atMs: now - 45 * MIN,
      },
      {
        status: "unknown",
        detail: "The machine didn't confirm the paste.",
        deliveryId: "d-mock-3",
        marker: "Inbox sweep",
        origin: { kind: "routine", routineKey: "acme/backend/triage/sweep", runId: "run_8f2c" },
        atMs: now - 30 * MIN,
      },
    ],
  };
}

export function sessionTimerHandlers(events: MockEventBus, search: string): HandlerRegistry {
  const now = Date.now();
  const timers: Record<string, SessionTimerRow[]> = isAgentTeamWorld(search) ? seed(now) : {};
  const deliveries: Record<string, WakeHistoryEntry[]> = isAgentTeamWorld(search)
    ? history(now)
    : {};
  let n = 0;

  const deliver = (sessionId: string, marker: string) => {
    const at = Date.now();
    const event: WakeDeliveredEvent = {
      machineId: LOCAL,
      sessionId,
      deliveryId: `d-mock-live-${++n}`,
      marker,
      caption: `Woken by ${marker}`,
      origin: { kind: "timer", timerId: timers[sessionId]?.[0]?.timerId ?? null },
      deliveredAtMs: at,
      source: "push",
      replayed: false,
    };
    (deliveries[sessionId] ??= []).push({
      status: "delivered",
      deliveredAtMs: at,
      deliveryId: event.deliveryId,
      marker,
      origin: event.origin,
      atMs: at,
    });
    // The paste reaches the terminal first, with its run header, as the
    // server writes it; then the event.
    const write = (globalThis as { __mockTerminalWrite?: (s: string, t: string) => void })
      .__mockTerminalWrite;
    const name = marker.replace(/\s\(set by [^)]*\)$/, "");
    write?.(
      sessionId,
      `\r\n> [timer: ${name} | fire ${n}]\r\n  Check whether the build finished\r\n> `,
    );
    events.emit("wake-delivered", event);
  };
  (globalThis as { __mockSessionTimers?: unknown }).__mockSessionTimers = { deliver };

  return {
    session_timers: (args) => {
      const sessionId = String(args.sessionId);
      return [...(timers[sessionId] ?? [])].sort((a, b) => a.nextAt.localeCompare(b.nextAt));
    },
    remind_session: (args) => {
      const sessionId = String(args.sessionId);
      const ask = args.timer as SessionTimerAsk;
      const rows = (timers[sessionId] ??= []);
      if (rows.length >= 10) throw "This tab already has 10 timers, the most it can have";
      const first = ask.at ? Date.parse(ask.at) : Date.now() + parseDuration(ask.in ?? "20m");
      const name = ask.name ?? ask.prompt.split(/\s+/).slice(0, 4).join(" ");
      const row: SessionTimerRow = {
        timerId: `t-${Math.random().toString(36).slice(2, 8)}`,
        name,
        prompt: ask.prompt,
        nextAt: iso(first),
        every: ask.every ?? null,
        until: ask.until ? iso(Date.parse(ask.until)) : null,
        count: ask.count ?? null,
        remaining: ask.count ?? null,
        fires: 0,
        createdBy: "you",
        machineId: (args.machineId as string | null) ?? LOCAL,
        sessionId,
      };
      rows.push(row);
      events.emit("session-timers-changed", { machineId: row.machineId, sessionId });
      return {
        timerId: row.timerId,
        nextAt: row.nextAt,
        name,
        machineId: row.machineId,
        sessionId,
      };
    },
    cancel_session_timer: (args) => {
      const sessionId = String(args.sessionId);
      const rows = timers[sessionId] ?? [];
      const i = rows.findIndex((r) => r.timerId === args.timerId);
      if (i < 0) return false;
      rows.splice(i, 1);
      return true;
    },
    wake_history: (args) => [...(deliveries[String(args.sessionId)] ?? [])],
  };
}
