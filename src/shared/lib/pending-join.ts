// SPDX-License-Identifier: MIT
/**
 * SW-FU-B6: the URL typed into an open join form, kept in localStorage so it
 * survives a window reload (a guest who connects a code host mid-join is
 * reloaded into the signed-in identity, `App.tsx`).
 *
 * Whether the join form must reopen after a sign-in or reload is the first-run
 * store's `joinPending`; this only holds the URL for the form's prefill. The form
 * drops it when it closes (unless a join is pending), and a finished join drops it.
 */

const STORAGE_KEY = "codesense.pending-join";

/** Never keep credentials typed into a URL (`https://user:token@host/…`). */ // trufflehog:ignore
export function stripUrlCredentials(url: string): string {
  return url.replace(/^(\s*[a-z][a-z0-9+.-]*:\/\/)[^@/\s]*@/i, "$1");
}

export function readPendingJoinUrl(): string | null {
  try {
    const raw = globalThis.localStorage?.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as { url?: unknown } | null;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return typeof parsed.url === "string" ? parsed.url : null;
  } catch {
    return null;
  }
}

export function setPendingJoinUrl(url: string): void {
  try {
    if (url.trim().length === 0) {
      globalThis.localStorage?.removeItem(STORAGE_KEY);
      return;
    }
    globalThis.localStorage?.setItem(
      STORAGE_KEY,
      JSON.stringify({ url: stripUrlCredentials(url) }),
    );
  } catch {
    // Best-effort: a storage failure only loses the URL across a reload.
  }
}

export function clearPendingJoinUrl(): void {
  try {
    globalThis.localStorage?.removeItem(STORAGE_KEY);
  } catch {
    // Best-effort.
  }
}
