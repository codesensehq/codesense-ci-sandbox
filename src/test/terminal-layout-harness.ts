// SPDX-License-Identifier: MIT
/**
 * Unit-test harness for terminal tabs: routes the test terminal host's
 * announcements into the viewer store (its one writer, `applyLayout`) and
 * seeds tabs the way a host that already runs them would restore them.
 * Call {@link installTerminalLayoutHarness} at a test file's top level.
 */

import { beforeEach } from "vitest";

import { selectWorkspaceKey, useViewerStore } from "@/shared/stores/viewer-store";
import { EMPTY_TERMINAL_LAYOUT } from "@/shared/lib/terminal-layout-projection";
import type { SeededEntry } from "@/app/mock/terminal-layout-host";
import {
  resetTestTerminalHost,
  setTestRemoteHost,
  testRemoteHost,
  testTerminalHost,
  TestRemoteHost,
} from "./terminal-layout-source-mock";

/** Before each test: a fresh host lifetime wired into the store, whose layout
 *  (empty) the store has hydrated from. */
export function installTerminalLayoutHarness(): void {
  beforeEach(() => {
    const host = resetTestTerminalHost((change) =>
      useViewerStore.getState().applyLayout({ kind: "change", target: host.target(), change }),
    );
    setTestRemoteHost(null);
    useViewerStore.setState({ terminalLayout: EMPTY_TERMINAL_LAYOUT, tabGridLayout: {} });
    useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [host.reply()] });
  });
}

/** Admit a remote Machine's terminal server beside This Mac's host: its
 *  announcements reach the store under its private authority, and the store
 *  re-reads every member's layout. */
export function connectTestRemoteHost(machineId = "box", privateAuthorityEpoch = "7") {
  const remote: TestRemoteHost = new TestRemoteHost(
    (change) => {
      if (!remote.online) return;
      useViewerStore
        .getState()
        .applyLayout({ kind: "change", target: remote.target(), change, privateAuthorityEpoch });
    },
    machineId,
    privateAuthorityEpoch,
  );
  setTestRemoteHost(remote);
  refetchTestLayout();
  return remote;
}

/** Every member's layout, read again (a snapshot), as a resync does. */
export function refetchTestLayout(): void {
  useViewerStore.getState().applyLayout({
    kind: "snapshot",
    replies: [testTerminalHost.reply(), ...(testRemoteHost ? [testRemoteHost.reply()] : [])],
  });
}

/** The test host (for exits, busy state, and what was opened). */
export function terminalHost() {
  return testTerminalHost;
}

/** A tab to seed: a session id plus, optionally, its name, role, exit. */
export type SeedTab = string | SeededEntry;

/**
 * Seed `(teamKey, workspaceId)`'s tabs in the host, in strip order, as if
 * their sessions were already running — then its focus and grid. The workspace
 * key is the store's, so seed after the workspace record exists when it is a
 * story (its key uses the story id).
 */
export function seedTerminalTabs(
  teamKey: string,
  workspaceId: string,
  tabs: SeedTab[],
  opts: {
    active?: string;
    paned?: string[];
    collapsed?: boolean;
    preset?: "columns" | "rows" | "grid";
  } = {},
): string {
  const key = selectWorkspaceKey(useViewerStore.getState(), teamKey, workspaceId);
  for (const tab of tabs) {
    const entry = typeof tab === "string" ? { session: tab } : tab;
    testTerminalHost.seed(key, { displayName: workspaceId, kind: "test" }, entry);
  }
  if (opts.active) testTerminalHost.setActive(key, opts.active);
  if (opts.paned || opts.collapsed !== undefined || opts.preset) {
    testTerminalHost.setGrid(key, {
      paned: opts.paned ?? [],
      preset: opts.preset ?? "columns",
      collapsed: opts.collapsed ?? false,
    });
  }
  return key;
}
