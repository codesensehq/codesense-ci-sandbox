// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { configurationPrepare } from "@/tauri/commands";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
import { liveHub, type LiveHub, type HubSnapshot } from "./live-hub";

export type PrefsState = Record<string, unknown> & { viewer?: Record<string, unknown> };
export const usePrefsStore = create<{
  principal: string | null;
  global: PrefsState;
  owned: PrefsState;
  globalReady: boolean;
  ownedReady: boolean;
}>(() => ({ principal: null, global: {}, owned: {}, globalReady: false, ownedReady: false }));

export function prefsFromView(view: HubSnapshot): PrefsState {
  const result: PrefsState = {};
  for (const row of view.snapshot) {
    const path = row.path as string[];
    let object: Record<string, unknown> = result;
    for (const key of path.slice(0, -1)) {
      object = (object[key] ??= {}) as Record<string, unknown>;
    }
    object[path.at(-1)!] = row.value;
  }
  return result;
}

export function connectPrefs(principal: string, hub: LiveHub = liveHub) {
  const owner = crypto.randomUUID();
  let disposed = false;
  let shown = true;
  const frames: ReturnType<LiveHub["attach"]>[] = [];
  usePrefsStore.setState({ principal, owned: {}, ownedReady: false });
  const live: Promise<unknown>[] = [];
  const warn = (scope: string) => (error: unknown) =>
    console.warn(`prefs live channel failed (${scope})`, error);
  const ready = Promise.all(
    (["global", "principal"] as const).map(async (scope) => {
      const view = await configurationPrepare("prefs", principal, undefined, scope);
      if (disposed) return;
      const commit = (snapshot: HubSnapshot) => {
        if (disposed) return;
        const value = prefsFromView(snapshot);
        usePrefsStore.setState(
          scope === "global"
            ? { global: value, globalReady: true }
            : { principal, owned: value, ownedReady: true },
        );
      };
      // Visibility throttles live updates only; the prepared snapshot lands at once.
      const apply = (snapshot: HubSnapshot) => {
        if (!shown) return;
        commit(snapshot);
      };
      hub.seed(view);
      commit(view);
      const frame = hub.attach(
        `native:prefs:${scope}:${owner}`,
        view.viewKey,
        (message) => {
          if (disposed || !shown || message.topic !== "prefs") return;
          void frame
            .subscribe("prefs", undefined, { viewKey: view.viewKey })
            .then(apply)
            .catch(warn(scope));
        },
        shown,
        true,
      );
      frames.push(frame);
      live.push(
        frame
          .subscribe("prefs", view.rev, { viewKey: view.viewKey })
          .then(apply)
          .catch(warn(scope)),
      );
    }),
  );
  // Callers observe `ready`; this derived promise must not reject unobserved when preparation fails.
  const delivered = ready.then(() => Promise.all(live));
  delivered.catch(() => {});
  return {
    ready,
    /** Settles once both live subscriptions have delivered; parked while the window is hidden. */
    live: delivered,
    visible(value: boolean) {
      shown = value;
      for (const frame of frames) frame.visible(value);
    },
    dispose() {
      disposed = true;
      for (const frame of frames) frame.dispose();
    },
  };
}

let connection: { principal: string; value: ReturnType<typeof connectPrefs> } | undefined;
export function ensurePrefsConnection(principal: string) {
  if (connection?.principal !== principal) {
    connection?.value.dispose();
    connection = { principal, value: connectPrefs(principal) };
  }
  return connection.value;
}
export function resetPrefsConnection() {
  connection?.value.dispose();
  connection = undefined;
  usePrefsStore.setState({ principal: null, owned: {}, ownedReady: false });
}
export async function readPrefsState(): Promise<PrefsState> {
  const principal = await rendererPrincipal();
  if (!principal) return {};
  await ensurePrefsConnection(principal).ready;
  const { global, owned, principal: owner } = usePrefsStore.getState();
  if (owner !== principal) throw new Error("unavailable: profile changed");
  return structuredClone({ ...global, ...owned, viewer: { ...global.viewer, ...owned.viewer } });
}
