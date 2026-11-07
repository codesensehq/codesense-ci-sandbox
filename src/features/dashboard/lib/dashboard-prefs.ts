// SPDX-License-Identifier: MIT
/**
 * Dashboard preferences survive disposable frame origins in the person's app
 * state. Pane keys use DashboardRef identity; a private install reads its
 * source's preferences until the first write forks its own entry.
 */
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { loadAppState, saveAppState } from "@/tauri/commands";
import type { DashboardRef } from "@/tauri/commands";
import { dashboardRefKey } from "./dashboard-ref";

/** The app-state key holding every dashboard's prefs. */
const APP_STATE_KEY = "dashboardPrefs";
/** Largest one dashboard's prefs may grow (serialized). */
export const DASHBOARD_PREFS_MAX_BYTES = 16 * 1024;
const FILE_NAV_PREFS_MAX_BYTES = 256 * 1024;
/** Most dashboards kept; the least recently written go first. */
const MAX_ENTRIES = 500;
/** Largest all dashboards' prefs may grow together (serialized); the least
 *  recently written go first, so app state stays small. */
export const DASHBOARD_PREFS_TOTAL_MAX_BYTES = 512 * 1024;
/** How stale an entry's age may get before a read refreshes it. */
const PREFS_TOUCH_MS = 24 * 60 * 60 * 1000;

type Prefs = Record<string, unknown>;
type Entry = { data: Prefs; at: number };

function prefsEntry(all: Record<string, Entry>, key: string, installedFrom?: DashboardRef) {
  const sourceKey = installedFrom ? dashboardRefKey(installedFrom) : key;
  const storedKey = all[key] ? key : sourceKey;
  return { storedKey, entry: all[storedKey] };
}

/** What the host knows about the doc it renders. */
export type DashboardIdentity = {
  dashboardRef?: DashboardRef;
  docPath?: string;
  pane?: { panesDir: string; fileName: string };
  message?: { org: string; team: string; messageId: number };
};

/** The instance key, or null when the caller is no rendered dashboard. */
export function dashboardPrefsKey(id: DashboardIdentity): string | null {
  if (id.message) return `message:${id.message.org}/${id.message.team}/${id.message.messageId}`;
  if (id.dashboardRef) return dashboardRefKey(id.dashboardRef);
  if (id.pane) return `pane:${id.pane.panesDir.replace(/\/+$/, "")}/${id.pane.fileName}`;
  if (id.docPath) return `doc:${id.docPath}`;
  return null;
}

function requireKey(key: string | null | undefined): string {
  if (!key) throw new Error("unavailable: prefs are kept per dashboard; this caller has none");
  return key;
}

async function loadAll(
  namespace: string,
): Promise<{ state: Record<string, unknown>; all: Record<string, Entry> }> {
  const raw = await loadAppState();
  const state = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
  const stored = state[namespace];
  const all =
    stored && typeof stored === "object" && !Array.isArray(stored)
      ? (stored as Record<string, Entry>)
      : {};
  return { state, all };
}

function isPlainObject(v: unknown): v is Prefs {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

/** This dashboard's prefs (`{}` when it has none). */
export async function readDashboardPrefs(
  key: string | null | undefined,
  namespace = APP_STATE_KEY,
  installedFrom?: DashboardRef,
): Promise<Prefs> {
  const k = requireKey(key);
  const { all } = await loadAll(namespace);
  const { storedKey, entry } = prefsEntry(all, k, installedFrom);
  if (!entry || !isPlainObject(entry.data)) return {};
  // A dashboard that only reads its settings is still in use: refresh its age at
  // most once a day, so set-once settings aren't the first evicted.
  if (Date.now() - (entry.at ?? 0) > PREFS_TOUCH_MS) {
    void serializeAppStateWrite(async () => {
      const fresh = await loadAll(namespace);
      const e = fresh.all[storedKey];
      if (!e || Date.now() - (e.at ?? 0) <= PREFS_TOUCH_MS) return;
      fresh.state[namespace] = { ...fresh.all, [storedKey]: { ...e, at: Date.now() } };
      await saveAppState(JSON.stringify(fresh.state));
    }).catch(() => {});
  }
  return { ...entry.data };
}

/** Merge `patch` into this dashboard's prefs (`null` removes a key); returns the result. */
/** Why `value` is not plain JSON (a Map, Set, BigInt, Date, function, NaN, a
 *  class instance …), or null when it is. A structured-clone message carries
 *  these intact; JSON would silently mangle or throw on them. */
function notJson(value: unknown, path: string, depth = 0): string | null {
  if (depth > 32) return `${path} is nested too deeply`;
  if (value === null || typeof value === "string" || typeof value === "boolean") return null;
  if (typeof value === "number")
    return Number.isFinite(value) ? null : `${path} is not a finite number`;
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) {
      const why = notJson(value[i], `${path}[${i}]`, depth + 1);
      if (why) return why;
    }
    return null;
  }
  if (typeof value === "object") {
    const proto = Object.getPrototypeOf(value);
    if (proto !== Object.prototype && proto !== null) {
      return `${path} is a ${(value as object).constructor?.name ?? "non-plain object"}, not plain JSON`;
    }
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      const why = notJson(v, `${path}.${k}`, depth + 1);
      if (why) return why;
    }
    return null;
  }
  return `${path} is a ${typeof value}, not JSON`;
}

const byteLength = (v: unknown) => new TextEncoder().encode(JSON.stringify(v)).length;

export function writeDashboardPrefs(
  key: string | null | undefined,
  patch: unknown,
  namespace = APP_STATE_KEY,
  installedFrom?: DashboardRef,
): Promise<Prefs> {
  const k = requireKey(key);
  if (!isPlainObject(patch) || Object.getPrototypeOf(patch) !== Object.prototype) {
    return Promise.reject(new Error("invalid: setPrefs takes an object of keys to set"));
  }
  for (const [name, value] of Object.entries(patch)) {
    if (value === null || value === undefined) continue;
    const why = notJson(value, name);
    if (why) return Promise.reject(new Error(`invalid: ${why}`));
  }
  let result: Prefs = {};
  return serializeAppStateWrite(async () => {
    const { state, all } = await loadAll(namespace);
    const { entry } = prefsEntry(all, k, installedFrom);
    const next: Prefs = { ...(entry && isPlainObject(entry.data) ? entry.data : {}) };
    for (const [name, value] of Object.entries(patch)) {
      if (value === null || value === undefined) delete next[name];
      else next[name] = value;
    }
    const size = byteLength(next);
    const maxBytes =
      namespace === "liveHubUi" && k.startsWith('["ui:file-nav",')
        ? FILE_NAV_PREFS_MAX_BYTES
        : DASHBOARD_PREFS_MAX_BYTES;
    if (size > maxBytes) {
      throw new Error(
        `too-large: a dashboard's prefs are capped at ${maxBytes} bytes (this would be ${size})`,
      );
    }
    const updated: Record<string, Entry> = { ...all, [k]: { data: next, at: Date.now() } };
    // Oldest first; this dashboard's entry (just written) is never dropped.
    const oldest = Object.keys(updated)
      .filter((key) => key !== k)
      .sort((a, b) => (updated[a].at ?? 0) - (updated[b].at ?? 0));
    let total = byteLength(updated);
    while (
      oldest.length &&
      (Object.keys(updated).length > MAX_ENTRIES || total > DASHBOARD_PREFS_TOTAL_MAX_BYTES)
    ) {
      const old = oldest.shift()!;
      total -= byteLength({ [old]: updated[old] }) - 2;
      delete updated[old];
    }
    state[namespace] = updated;
    await saveAppState(JSON.stringify(state));
    result = { ...next };
  }).then(() => result);
}
