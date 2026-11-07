// SPDX-License-Identifier: MIT
/**
 * Debug-only instrumentation for the real-app profile runs (SV-PLG-46).
 *
 * One gate: the `webdriver` build, the e2e-webdriver app the sandbox serves through vite.
 * `import.meta.env.MODE` is replaced at build time, so in a release build every guarded body is
 * dead code: no marks, no listeners, no buffer, no `window.__csDebug`, no output.
 *
 * Every entry goes to `console.debug` and into the ring buffer `window.__csDebug` (the harness
 * cannot capture the WKWebView console, but WebDriver can read the buffer back).
 */

export interface CsDebugEntry {
  /** `performance.now()` when the entry was recorded. */
  t: number;
  kind: string;
  [field: string]: unknown;
}

declare global {
  interface Window {
    __csDebug?: CsDebugEntry[];
  }
}

const BUFFER_CAP = 500;

/** The gate. Read at call time so a test can stub the mode; off in release and in unit tests. */
export function csDebugEnabled(): boolean {
  return import.meta.env.MODE === "webdriver";
}

/** Record one entry: a ring buffer on `window.__csDebug` plus `console.debug`. No-op when off. */
export function csDebug(kind: string, data: Record<string, unknown> = {}): void {
  if (!csDebugEnabled() || typeof window === "undefined") return;
  const entry: CsDebugEntry = { ...data, t: performance.now(), kind };
  const buffer = (window.__csDebug ??= []);
  buffer.push(entry);
  if (buffer.length > BUFFER_CAP) buffer.splice(0, buffer.length - BUFFER_CAP);
  console.debug(`[cs-debug] ${kind}`, data);
}

/** A User Timing mark (`cs:<name>`) plus a buffer entry. No-op when off. */
export function csMark(name: string, data: Record<string, unknown> = {}): void {
  if (!csDebugEnabled()) return;
  performance.mark(`cs:${name}`);
  csDebug("mark", { ...data, name });
}

/** A User Timing measure (`cs:<name>`) between two timestamps. No-op when off. */
export function csMeasure(name: string, start: number, end: number): void {
  if (!csDebugEnabled()) return;
  performance.measure(`cs:${name}`, { start, end });
}
