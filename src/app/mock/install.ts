// SPDX-License-Identifier: MIT
import { buildTauriInternals } from "./tauri-internals";
import { createMockBackend } from "./mock-backend";
import { resolveScenario } from "./scenario";
import { MockEventBus } from "./mock-events";

/**
 * Install the mock backend by planting `window.__TAURI_INTERNALS__`. Call this
 * FIRST in the `MODE === "mock"` boot block — before React renders — so the
 * synchronous `getCurrentWindow()` metadata reads and the first `invoke` both
 * find a shim instead of throwing `window.__TAURI_INTERNALS__ is undefined`.
 *
 * The `?scenario=` URL param selects which first-run world boots, so every
 * empty-state rung and a set-up team is reachable by link; other params select
 * per-surface worlds (`?terminalServer=incompatible`, `?machines=demo`).
 */
export function installMockBackend(): void {
  if (import.meta.env.MODE !== "mock" && import.meta.env.MODE !== "test") {
    throw new Error("The mock backend is unavailable outside mock/test builds");
  }
  if (typeof window === "undefined") return;
  // Default the mock session to dark mode (next-themes reads localStorage["theme"]
  // before first paint). Only when unset, so a manual theme toggle still persists.
  if (window.localStorage.getItem("theme") === null) {
    window.localStorage.setItem("theme", "dark");
  }
  const scenario = resolveScenario(window.location.search);
  const events = new MockEventBus();
  const router = createMockBackend(scenario, window.location.search, events);
  const internals = buildTauriInternals(router);
  events.bind(internals.runCallback);
  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = internals;
  // `listen`'s unlisten reaches for the event plugin's registry; the mock bus
  // keeps its own, so there is nothing to unregister there.
  (
    window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }
  ).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
}
