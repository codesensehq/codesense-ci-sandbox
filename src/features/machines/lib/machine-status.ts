// SPDX-License-Identifier: MIT
import type { MachineSummary } from "@/tauri/commands";

import { machineMemory, type MachineCensus } from "./machine-memory";

/** What this app remembers about a Machine across launches: its last-known
 *  census and when it went offline. */
export interface MachineMemory {
  census?: MachineCensus;
  offlineSince?: number;
}

/**
 * The ONE machine-status grammar (Sally §3): every state maps to a glyph + a word
 * + a tone + its actions, so the picker row, the Connections row and the CLI area
 * render from a single source and can never disagree. The glyph and word carry the
 * meaning on their own; `tone` only adds color.
 */

export type MachineTone = "ok" | "progress" | "warn" | "error" | "muted";

export type MachineActionId =
  | "connect"
  | "retry"
  | "editAddress"
  | "setUp"
  | "reconnect"
  | "reviewKey"
  | "install"
  | "restartServer"
  | "keepReadOnly";

export interface MachineAction {
  id: MachineActionId;
  label: string;
}

export interface MachineStatusView {
  glyph: string;
  word: string;
  /** The qualifier after the word ("2 workspaces", "last reached 3m ago"). */
  detail: string | null;
  tone: MachineTone;
  /** Every non-connected state offers at least one way forward (no dead ends). */
  actions: MachineAction[];
  /** Why "+ New terminal" is unavailable here, or null when it can run. Never a
   *  cue to fall back to This Mac — that path is always an explicit choice. */
  blockedReason: string | null;
}

/** `1 workspace` / `3 workspaces`. */
export function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** Why "+ New terminal" can't run on `machine` right now, or null when it can. */
export function machineBlockedReason(machine: MachineSummary): string | null {
  const name = machine.label;
  switch (machine.status.state) {
    case "connected":
      return null;
    case "idle":
      return `${name} isn't connected`;
    case "connecting":
      return `Still connecting to ${name}`;
    case "handshaking":
      return `Still connecting to ${name}`;
    case "disconnected":
      return `${name} isn't connected`;
    case "connectionFailed":
      return `${name} is unreachable`;
    case "reconnecting":
      return `${name} is reconnecting`;
    case "unreachable":
      return `${name} is unreachable`;
    case "needsCredential":
      return `${name} isn't set up yet`;
    case "authFailed":
      return `Sign-in to ${name} failed`;
    case "hostKeyChanged":
      return `${name}'s host key changed, so it's blocked`;
    case "notInstalled":
      return `${name} has no terminal server yet`;
    case "versionMismatch":
      return `${name}'s terminal server is out of date`;
    case "terminalServerFrozen":
      return `${name}'s terminal server is read-only`;
    case "replacementReserved":
    case "replacing":
    case "readmitting":
      return `${name}'s terminal server is restarting`;
    case "replacementFailed":
      return machine.status.oldStopped
        ? `${name}'s old terminal server stopped, but its replacement is not ready`
        : `${name}'s terminal server remains read-only`;
  }
}

/** `3 tabs in 2 workspaces` — the last-known census, while connecting. */
function lastKnown(census: MachineCensus | undefined): string | null {
  if (!census || census.tabs === 0) return null;
  return `last seen with ${plural(census.tabs, "tab")} in ${plural(census.workspaces.length, "workspace")}`;
}

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** `machine`'s status line. `_nowMs` is the caller's render clock: rows
 *  re-render on it so remembered times stay current. */
export function describeMachineStatus(
  machine: MachineSummary,
  _nowMs: number,
  memory: MachineMemory = machineMemory(machine.machineId),
): MachineStatusView {
  const view = withMemory(describeState(machine), machine, memory);
  // This Mac has no address, key or install to fix. SW-AGR-19: its one
  // action is the update, when its terminal server is not this app's build
  // (older, or another CodeSense build's).
  const relation = machine.terminalServer?.relation;
  const actions =
    machine.kind === "local"
      ? machine.status.state === "connected" && (relation === "older" || relation === "otherBuild")
        ? [terminalServerUpdateAction(machine)]
        : []
      : view.actions;
  return { ...view, actions, blockedReason: machineBlockedReason(machine) };
}

/** Connecting shows what the Machine last held; an unreachable one is
 *  "offline since…", quietly, with its ways forward unchanged. */
function withMemory(
  view: Omit<MachineStatusView, "blockedReason">,
  machine: MachineSummary,
  memory: MachineMemory,
): Omit<MachineStatusView, "blockedReason"> {
  const state = machine.status.state;
  if ((state === "connecting" || state === "handshaking") && lastKnown(memory.census))
    return { ...view, detail: lastKnown(memory.census) };
  if (state === "connectionFailed" && memory.offlineSince !== undefined)
    return {
      ...view,
      word: "Offline",
      detail: `since ${clock(memory.offlineSince)}`,
      tone: "muted",
    };
  return view;
}

/** The one "Update terminal server" action for a machine. Every place that
 *  offers the update runs this, so they cannot drift. */
export function terminalServerUpdateAction(machine: MachineSummary): MachineAction {
  // SW-AGR-19: a machine whose server runs (connected, frozen, or after a
  // failed restart) updates in place — the safe replacement when its server
  // supports it, else the confirmed reinstall (stop by pid, install,
  // reconnect). Only a machine with no server reached goes through set-up.
  const s = machine.status.state;
  return s === "connected" || s === "terminalServerFrozen" || s === "replacementFailed"
    ? { id: "restartServer", label: "Update terminal server" }
    : { id: "install", label: "Update terminal server" };
}

function describeState(machine: MachineSummary): Omit<MachineStatusView, "blockedReason"> {
  const s = machine.status;
  const name = machine.label;
  switch (s.state) {
    case "connected":
      return machine.kind === "local"
        ? {
            glyph: "●",
            word: "Running",
            detail: s.workspaces === null ? null : plural(s.workspaces, "workspace"),
            tone: "ok",
            actions: [],
          }
        : s.terminalServerUpdate
          ? {
              // SW-XS-6: still usable; this app ships a newer terminal server.
              glyph: "●",
              word: "Connected",
              detail: [
                s.workspaces === null ? null : plural(s.workspaces, "workspace"),
                "terminal server update available",
              ]
                .filter(Boolean)
                .join(" · "),
              tone: "warn",
              actions: [terminalServerUpdateAction(machine)],
            }
          : {
              glyph: "●",
              word: "Connected",
              detail: s.workspaces === null ? null : plural(s.workspaces, "workspace"),
              tone: "ok",
              actions:
                machine.declaration?.mode === "private-account"
                  ? [{ id: "restartServer", label: "Restart terminal server" }]
                  : [],
            };
    case "idle":
      return {
        glyph: "○",
        word: "Not connected",
        detail: null,
        tone: "muted",
        actions: [{ id: "connect", label: "Connect" }],
      };
    case "handshaking":
      return {
        glyph: "↻",
        word: "Connecting",
        detail: `verifying ${name}`,
        tone: "progress",
        actions: [{ id: "retry", label: "Retry now" }],
      };
    case "disconnected":
      return {
        glyph: "○",
        word: "Idle",
        detail: null,
        tone: "muted",
        actions: [{ id: "connect", label: "Connect" }],
      };
    case "connectionFailed":
      return {
        glyph: "▲",
        word: "Unreachable",
        detail: s.lastError,
        tone: "error",
        actions: [
          { id: "retry", label: "Retry" },
          { id: "editAddress", label: "Edit address" },
        ],
      };
    case "connecting":
      return {
        glyph: "◐",
        word: "Connecting",
        detail: null,
        tone: "progress",
        actions: [{ id: "retry", label: "Retry now" }],
      };
    case "reconnecting":
      return {
        glyph: "◐",
        word: "Reconnecting",
        detail: `sessions still running on ${name}`,
        tone: "progress",
        actions: [{ id: "retry", label: "Retry now" }],
      };
    case "unreachable":
      return {
        glyph: "▲",
        word: "Offline",
        detail: `since ${clock(s.lastReachedAtMs)}`,
        tone: "muted",
        actions: [
          { id: "retry", label: "Retry" },
          { id: "editAddress", label: "Edit address" },
        ],
      };
    case "needsCredential":
      return {
        glyph: "◌",
        word: "Needs setup",
        detail: null,
        tone: "muted",
        actions: [{ id: "setUp", label: "Set up" }],
      };
    case "authFailed":
      return {
        glyph: "🔑",
        word: "Sign-in failed",
        detail: s.reason,
        tone: "warn",
        actions: [{ id: "reconnect", label: "Reconnect" }],
      };
    case "hostKeyChanged":
      return {
        glyph: "⛔",
        word: "Host key changed",
        detail: "blocked",
        tone: "error",
        actions: [{ id: "reviewKey", label: "Review key…" }],
      };
    case "notInstalled":
      return {
        glyph: "⬇",
        word: "Terminal server not installed",
        detail: null,
        tone: "warn",
        actions: [{ id: "install", label: "Install" }],
      };
    case "versionMismatch":
      return {
        glyph: "◆",
        word: "Update needed",
        detail: `server v${s.serverVersion}, app needs v${s.appNeeds}`,
        tone: "warn",
        // A version mismatch is not an authenticated replacement reservation.
        // Keep this row read-only until admission classifies it as a supported,
        // proof-bound terminalServerFrozen state.
        actions: [{ id: "keepReadOnly", label: "Keep read-only" }],
      };
    case "terminalServerFrozen":
      return {
        glyph: "◆",
        word: "Read-only",
        detail: s.replacementSupported
          ? s.count === null
            ? "session count unavailable"
            : plural(s.count, "session")
          : s.count === null
            ? "update ends its sessions"
            : `${plural(s.count, "session")} · update ends them`,
        tone: "warn",
        // SW-AGR-19: without the safe replacement, the confirmed reinstall.
        actions: s.replacementSupported
          ? [{ id: "restartServer", label: "Restart terminal server…" }]
          : [{ id: "restartServer", label: "Update terminal server…" }],
      };
    case "replacementReserved":
    case "replacing":
    case "readmitting":
      return {
        glyph: "◐",
        word: "Restarting…",
        detail: null,
        tone: "progress",
        actions: [],
      };
    case "replacementFailed":
      return {
        glyph: "▲",
        word: s.oldStopped ? "Restart incomplete" : "Read-only",
        detail: s.repair,
        tone: "error",
        // SW-AGR-19: a failed restart is retried through the same update.
        actions: [{ id: "restartServer", label: "Update terminal server…" }],
      };
  }
}

/** The ⓘ capability note: agents on this machine can't report back (MCP/status
 *  hooks don't reach it). Unsupported → the capability is hidden, never a failure.
 *  A private account reports back through "Allow agents…": the note then names
 *  that action, and disappears once it is on. */
export function agentCallbacksNote(machine: MachineSummary): string | null {
  if (machine.capabilities.agentCallbacks) return null;
  if (machine.kind === "ssh" && machine.declaration?.mode === "private-account") {
    return machine.remoteMcpControl === true ? null : ALLOW_AGENTS_NOTE;
  }
  return "Agents can't report back here";
}

export const ALLOW_AGENTS_NOTE = "Turn on Allow agents… to let agents on this machine report back";

/** Only a private account opens terminal tabs, so a legacy machine says so
 *  before anyone tries to open one. */
export function needsPrivateAccount(machine: MachineSummary): boolean {
  return machine.kind === "ssh" && machine.declaration?.mode === "legacy";
}

export const PRIVATE_ACCOUNT_FOR_TABS = "Terminal tabs need a private account";

export const TONE_CLASS: Record<MachineTone, string> = {
  ok: "text-green-600 dark:text-green-400",
  progress: "text-sky-600 dark:text-sky-400",
  warn: "text-amber-600 dark:text-amber-400",
  error: "text-destructive",
  muted: "text-muted-foreground",
};

/** Stable alphabetical order by label — every machine list sorts this way. */
export const byLabel = (a: MachineSummary, b: MachineSummary): number =>
  a.label.localeCompare(b.label);

/** `devbox-01:/abs/path` — the machine-qualified path copied and shown as tooltip. */
export function machineQualifiedPath(
  machine: Pick<MachineSummary, "label">,
  absPath: string,
): string {
  return `${machine.label}:${absPath}`;
}
