// SPDX-License-Identifier: MIT
let tauriLog: typeof import("@tauri-apps/plugin-log") | null = null;
// Memoize the plugin import and the attachConsole init separately so repeat
// calls are idempotent: `initLogging()` is kicked from `main.tsx` (earliest
// boot) AND awaited in App's boot effect — both must share ONE plugin load and
// ONE `attachConsole`. The lazy `logger.*` fast-path reuses the same load.
let loadPromise: Promise<void> | null = null;
let attachPromise: Promise<void> | null = null;

function loadPlugin(): Promise<void> {
  if (!loadPromise) {
    loadPromise = (async () => {
      try {
        tauriLog = await import("@tauri-apps/plugin-log");
      } catch (e) {
        tauriLog = null;
        // Clear the memo so a later call can retry a TRANSIENT import failure
        // (the success path stays memoized). Concurrent awaiters of this
        // in-flight promise still resolve via the console fallback.
        loadPromise = null;
        console.warn("[logger] tauri-plugin-log unavailable, using console fallback:", e);
      }
    })();
  }
  return loadPromise;
}

function ensurePlugin(): typeof import("@tauri-apps/plugin-log") | null {
  if (tauriLog) return tauriLog;
  void loadPlugin();
  return null;
}

function format(tag: string, msg: string): string {
  return `[${tag}] ${msg}`;
}

export const logger = {
  error(tag: string, msg: string) {
    const formatted = format(tag, msg);
    const plugin = ensurePlugin();
    if (plugin) {
      plugin.error(formatted).catch(() => {});
    } else {
      console.error(formatted);
    }
  },
  warn(tag: string, msg: string) {
    const formatted = format(tag, msg);
    const plugin = ensurePlugin();
    if (plugin) {
      plugin.warn(formatted).catch(() => {});
    } else {
      console.warn(formatted);
    }
  },
  info(tag: string, msg: string) {
    const formatted = format(tag, msg);
    const plugin = ensurePlugin();
    if (plugin) {
      plugin.info(formatted).catch(() => {});
    } else {
      console.info(formatted);
    }
  },
  debug(tag: string, msg: string) {
    const formatted = format(tag, msg);
    const plugin = ensurePlugin();
    if (plugin) {
      plugin.debug(formatted).catch(() => {});
    } else {
      console.debug(formatted);
    }
  },
  trace(tag: string, msg: string) {
    const formatted = format(tag, msg);
    const plugin = ensurePlugin();
    if (plugin) {
      plugin.trace(formatted).catch(() => {});
    } else {
      console.debug(formatted);
    }
  },
};

/**
 * Load the plugin and pipe console.* → the rotated log file via `attachConsole`.
 * Idempotent: safe to call from both `main.tsx` (earliest, before first render)
 * and App's boot effect — the shared `attachPromise` guarantees exactly one
 * `attachConsole`. Awaiting it always resolves after init completes.
 */
export function initLogging(): Promise<void> {
  if (!attachPromise) {
    attachPromise = (async () => {
      await loadPlugin();
      if (tauriLog) {
        try {
          await tauriLog.attachConsole();
        } catch (e) {
          // Intentionally terminal (NOT cleared like the load path below):
          // attachConsole fails when there's no Tauri runtime — a STABLE
          // condition, so retrying every call would be futile churn. `logger.*`
          // still routes to the loaded plugin regardless of the console attach.
          console.warn("[logger] attachConsole failed (no Tauri runtime?):", e);
        }
      } else {
        // Load failed — clear the memo so a later call retries (mirrors
        // loadPlugin's retry). A successful init stays memoized (one attach).
        attachPromise = null;
      }
    })();
  }
  return attachPromise;
}
