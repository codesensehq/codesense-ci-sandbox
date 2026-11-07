// SPDX-License-Identifier: MIT
import { createInvokeRouter, type HandlerRegistry } from "@/app/mock/mock-router";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { machineHandlers, type MachineFixtureKey } from "@/app/mock/handlers/machines";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "@/app/mock/fixtures";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";

import { __resetMachineLoadsForTest, useMachinesStore } from "../stores/machines-store";

export const ZERO_LATENCY = { connectMs: 0, listMs: 0, slowListMs: 0, verySlowListMs: 0 };

/** The team the `shared`/`demo` fixtures share machines with. */
export const TEAM_KEY = buildTeamKey(MOCK_ORG_SLUG, MOCK_TEAM_NAME);

/**
 * Serve the Machines draft commands from the REAL mock handlers (the contract),
 * behind the same `__TAURI_INTERNALS__` seam `pnpm dev:mock` uses — so component
 * tests exercise the DTOs end to end. Returns the recorded invoke calls.
 */
export function installMachinesBackend(key: MachineFixtureKey, overrides: HandlerRegistry = {}) {
  (
    window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }
  ).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  const calls: { cmd: string; args: Record<string, unknown> }[] = [];
  const router = createInvokeRouter({ ...machineHandlers(key, ZERO_LATENCY), ...overrides });
  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = buildTauriInternals(
    (cmd, args = {}) => {
      calls.push({ cmd, args });
      return router(cmd, args);
    },
  );
  return { calls };
}

/** Reset every store the Machines UI reads, and put the user on {@link TEAM_KEY}. */
export function resetMachinesUi(): void {
  __resetMachineLoadsForTest();
  useMachinesStore.setState({ byTeam: {}, keptReadOnly: [], dismissedEpochs: [] });
  useUiStore.setState({ machinesPanel: null });
  useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: MOCK_TEAM_NAME });
}
