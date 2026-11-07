// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";
import { getLogDebugEnabled } from "@/tauri/commands";

// SV-ENG-120 phase 1: timing evidence for held or unanswered dashboard bridge
// reads. Every line carries the `[bridge-timing]` prefix. Times are
// `performance.now()` ms on the host renderer's clock.
//
// Off unless the app's log level keeps debug (CODESENSE_LOG=debug, or a dev
// build): the level is read once from the backend, and while it is off no string
// is built, no log IPC is sent and no observer or listener is installed.
const TAG = "bridge-timing";

let enabled = false;
let probesWanted = false;

/** Takes a thunk so the message is only built when debug logging is on. */
export function bridgeTiming(msg: () => string): void {
  if (enabled) logger.debug(TAG, msg());
}

if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
  void getLogDebugEnabled()
    .then((on) => {
      enabled = on === true;
      if (!enabled) return;
      bridgeTiming(
        () =>
          `on longtask=${typeof PerformanceObserver !== "undefined" && PerformanceObserver.supportedEntryTypes?.includes("longtask") === true} timeOrigin=${Math.round(performance.timeOrigin)}`,
      );
      if (probesWanted) attachProbes();
    })
    .catch(() => {});
}

export function nowMs(): number {
  return Math.round(performance.now());
}

/**
 * `lag=` field: host receipt minus the page's `sentAt` (epoch ms stamped by the
 * frame shim). A large lag with a fast native settle means the host main thread
 * held the message; no longtask support or timer needed.
 */
export function lagField(sentAt: unknown): string {
  return typeof sentAt === "number"
    ? ` lag=${Math.round(performance.timeOrigin + performance.now() - sentAt)}`
    : "";
}

/** A page-controlled command name, clamped so it cannot write arbitrary text into the log. */
export function safeCmd(command: unknown): string {
  return typeof command === "string" && /^[A-Za-z0-9_.:-]{1,64}$/.test(command) ? command : "?";
}

let lastHostMessage: { type: string; at: number } | null = null;

/** Records the latest bridge message the host handled, for long-task attribution. */
export function noteHostMessage(type: unknown): void {
  if (typeof type === "string") lastHostMessage = { type, at: nowMs() };
}

let installed = false;

/** Asks for the probes; they attach once debug logging is known to be on. */
export function installBridgeTimingProbes(): void {
  probesWanted = true;
  if (enabled) attachProbes();
}

/**
 * Once per renderer: host main-thread long tasks (>100 ms) and the page
 * lifecycle signals that show the renderer being hidden, frozen or resumed.
 * Event-driven; no timers or polling.
 */
function attachProbes(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  try {
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.duration < 100) continue;
        const last = lastHostMessage
          ? ` lastBridgeMsg=${lastHostMessage.type}@${lastHostMessage.at}`
          : "";
        bridgeTiming(
          () =>
            `longtask start=${Math.round(entry.startTime)} dur=${Math.round(entry.duration)}${last}`,
        );
      }
    });
    observer.observe({ entryTypes: ["longtask"] });
  } catch {
    // longtask is not exposed by every webview.
  }
  const lifecycle = (name: string) => () =>
    bridgeTiming(() => `page ${name} t=${nowMs()} visibility=${document.visibilityState}`);
  document.addEventListener("visibilitychange", lifecycle("visibilitychange"));
  document.addEventListener("freeze", lifecycle("freeze"));
  document.addEventListener("resume", lifecycle("resume"));
  window.addEventListener("pagehide", lifecycle("pagehide"));
  window.addEventListener("pageshow", lifecycle("pageshow"));
}
