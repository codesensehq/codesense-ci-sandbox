// SPDX-License-Identifier: MIT
/**
 * SW-AV (agent-viz reset) — the WebDriver-only Agents-surface control. It exposes
 * the minimum the `weft-e2e` real-app suite needs to observe the Agents surfaces:
 * open the bottom agents list, the Inbox dashboard and the Agents tab
 * (`virtual://agents`), report whether app hydration has settled, and enumerate the
 * registered panel titles (so the suite can prove retired right-pane panels stay gone).
 *
 * Every ledger/roster transition the suite drives still goes through the real Tauri
 * commands (`core.invoke`) — this control never seeds or mutates weft/roster state;
 * it only mounts the read surfaces so the suite can assert the live nodes. The pre-reset controller/lease/gate/tier model this file
 * used to help prove is DELETED (SW-AV-27); the Inbox dashboard
 * is served by the pane host.
 *
 * Behind the `webdriver` build mode (imported only from App.tsx's
 * `MODE === "webdriver"` block), so it is tree-shaken from — and proven absent
 * (`scripts/verify-webdriver-absence.ts`) in — the shipped app.
 */
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { listPanels } from "@/features/shell/right-pane/panel-registry";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { fixtureHydrationSettled } from "./e2e-cli-workspace-control";

/** The WebDriver-only Agents-surface control. Exported as the single shared shape so
 *  the e2e spec references it via `import type`. */
export interface WeftE2eControl {
  /** True once the app's persisted-state hydration has settled (reused from the
   *  shared fixture-control seam) — the suite waits on it before seeding. */
  hydrationSettled: () => boolean;
  /** Reveal the profile's Inbox surface, opening its dashboard. */
  openBoard: () => boolean;
  /** Show the active team's bottom agents list (terminal column + expanded list). */
  openAgentsList: () => void;
  /** Open the Agents tab (`virtual://agents`), where agents are listed and launched. */
  openAgents: () => void;
  /** The titles of every registered right-pane panel — the suite asserts the retired
   *  panels are ABSENT. */
  registeredPanelTitles: () => string[];
}

function openAgentsList(): void {
  const store = useViewerStore.getState();
  store.setWorkspacePanelCollapsed(false);
  store.revealTerminalInPlace();
}

export function registerWeftE2eControl(): void {
  window.__CODESENSE_WEFT_E2E__ = {
    hydrationSettled: fixtureHydrationSettled,
    openBoard: () => {
      revealSlot("right-pane", { surface: "inbox", intent: { focusComposer: false } });
      return true;
    },
    openAgentsList,
    openAgents: () => openAgents(),
    registeredPanelTitles: () => listPanels().map((panel) => panel.title),
  };
}

declare global {
  interface Window {
    __CODESENSE_WEFT_E2E__?: WeftE2eControl;
  }
}
