// SPDX-License-Identifier: MIT
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { logger } from "@/shared/lib/logger";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";

/**
 * The OS side of "an agent needs you": the notification, the dock badge, and the
 * window's focus. Every call is best-effort — outside Tauri (tests, web) or with
 * notifications denied, each is a quiet no-op and the rest keeps working.
 */
export interface OsAttention {
  /** Show one notification. Resolves false when it could not be shown (denied,
   *  no OS support). Permission is asked for on the first call, never earlier.
   *  `sound` plays the system's notification sound with it, which the OS still
   *  silences under Focus / Do Not Disturb or a muted alert volume. */
  notify(title: string, body: string, opts?: { sound?: boolean }): Promise<boolean>;
  /** The dock badge; 0 clears it. */
  setBadgeCount(count: number): void;
  /** Call `cb` with the window's focus now and on every change. Returns the
   *  unsubscribe. */
  subscribeFocus(cb: (focused: boolean) => void): () => void;
  /** Unminimize, show and focus the app window. */
  bringWindowForward(): Promise<void>;
}

/** Ask the OS once per app run: a "no" is not asked again, but a grant made
 *  later in System Settings is still picked up (the check is cheap). */
let askedPermission = false;

async function ensurePermission(): Promise<boolean> {
  try {
    if (await isPermissionGranted()) return true;
    if (askedPermission) return false;
    askedPermission = true;
    return (await requestPermission()) === "granted";
  } catch {
    return false;
  }
}

/** The platform's own notification sound, by the name its notification centre
 *  knows. Windows plays its default toast sound on its own, so it gets none. */
export function systemNotificationSound(
  userAgent: string = typeof navigator === "undefined" ? "" : navigator.userAgent,
): string | undefined {
  if (/Mac|darwin/i.test(userAgent)) return "Glass";
  if (/Linux/i.test(userAgent)) return "message-new-instant";
  return undefined;
}

/** Test-only: forget that permission was asked. */
export function resetOsAttentionForTests(): void {
  askedPermission = false;
}

export const tauriOsAttention: OsAttention = {
  async notify(title, body, opts) {
    if (!(await ensurePermission())) return false;
    try {
      const sound = opts?.sound ? systemNotificationSound() : undefined;
      sendNotification(sound ? { title, body, sound } : { title, body });
      return true;
    } catch (e) {
      logger.debug("attention", `notification failed: ${String(e)}`);
      return false;
    }
  },

  setBadgeCount(count) {
    try {
      void getCurrentWindow()
        .setBadgeCount(count > 0 ? count : undefined)
        .catch((e: unknown) => logger.debug("attention", `badge failed: ${String(e)}`));
    } catch {
      // No Tauri window (tests / web).
    }
  },

  subscribeFocus(cb) {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    try {
      const win = getCurrentWindow();
      win
        .isFocused()
        .then((f) => {
          if (!cancelled) cb(f);
        })
        .catch(() => {});
      win
        .onFocusChanged(({ payload }) => cb(payload))
        .then((fn) => {
          if (cancelled) safeUnlisten(fn);
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // No Tauri window: the DOM's focus is the window's.
      if (typeof window === "undefined") return () => {};
      const onFocus = () => cb(true);
      const onBlur = () => cb(false);
      window.addEventListener("focus", onFocus);
      window.addEventListener("blur", onBlur);
      cb(typeof document !== "undefined" && document.hasFocus());
      return () => {
        window.removeEventListener("focus", onFocus);
        window.removeEventListener("blur", onBlur);
      };
    }
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  },

  async bringWindowForward() {
    try {
      const win = getCurrentWindow();
      await win.unminimize().catch(() => {});
      await win.show().catch(() => {});
      await win.setFocus().catch(() => {});
    } catch {
      // No Tauri window.
    }
  },
};
