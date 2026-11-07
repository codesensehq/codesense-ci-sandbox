// SPDX-License-Identifier: MIT
/**
 * SW-OC-26 — live context queries. `subscribeContext(query, cb, page)` runs the
 * query through the Rust engine now and again after index changes that touch
 * its scope (`context-index-changed`), delivering the full result each time. No
 * client filtering, sorting or delta folding: the engine's answer is the view.
 *
 * Scope filtering: a result reports the orgs it was narrowed to (`scopeOrgs`);
 * a change re-runs the query only when it touches one of those orgs, touches
 * all-orgs content, or replaces the index (`truncated`). An unscoped query
 * (every org) re-runs on every change.
 *
 * One Tauri listener is shared by every subscription. A slower, older query can
 * never overwrite a newer one (per-subscription generation counter).
 */

import { listen } from "@tauri-apps/api/event";

import { queryContext } from "@/tauri/commands";
import { CONTEXT_INDEX_CHANGED_EVENT, type ContextIndexChangedPayload } from "@/tauri/events";
import { createListenerGroup, type ListenerGroup } from "@/shared/lib/tauri-listeners";

import type { ContextQuery, ContextQueryResult } from "./types";

export type ContextUpdate = { ok: true; result: ContextQueryResult } | { ok: false; error: string };

type OnChange = (change: ContextIndexChangedPayload | null) => void;

const subscribers = new Set<OnChange>();
let group: ListenerGroup | null = null;

function attach(): void {
  if (group) return;
  group = createListenerGroup();
  group.track(
    listen<ContextIndexChangedPayload>(CONTEXT_INDEX_CHANGED_EVENT, (event) => {
      for (const s of [...subscribers]) s(event.payload ?? null);
    }),
  );
}

function detachIfIdle(): void {
  if (subscribers.size === 0 && group) {
    group.dispose();
    group = null;
  }
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Does `change` concern a query last scoped to `scopeOrgs` (absent = every org)? */
export function changeTouchesScope(
  change: ContextIndexChangedPayload | null,
  scopeOrgs: string[] | undefined,
): boolean {
  if (!change || !scopeOrgs || change.truncated || change.allOrgs) return true;
  return change.orgs.some((org) => scopeOrgs.includes(org));
}

/** Run `query` now and on every index change in its scope. Returns an idempotent unsubscribe. */
export function subscribeContext(
  query: ContextQuery,
  cb: (update: ContextUpdate) => void,
  page: string,
  callerPage?: string,
  callerTeam?: string,
): () => void {
  let disposed = false;
  let generation = 0;
  let scopeOrgs: string[] | undefined;
  const rerun = () => {
    const mine = ++generation;
    const request = callerTeam
      ? queryContext(query, page, undefined, callerPage, callerTeam)
      : callerPage
        ? queryContext(query, page, undefined, callerPage)
        : queryContext(query, page);
    request
      .then((result) => {
        if (disposed || mine !== generation) return;
        scopeOrgs = result.scopeOrgs;
        cb({ ok: true, result });
      })
      .catch((e: unknown) => {
        if (!disposed && mine === generation) cb({ ok: false, error: errorText(e) });
      });
  };
  const onChange: OnChange = (change) => {
    if (changeTouchesScope(change, scopeOrgs)) rerun();
  };
  subscribers.add(onChange);
  attach();
  rerun();
  return () => {
    if (disposed) return;
    disposed = true;
    subscribers.delete(onChange);
    detachIfIdle();
  };
}
