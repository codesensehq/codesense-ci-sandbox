// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { HTML_BRIDGE_ALLOWLIST } from "./html-bridge";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();
const team = "org:default";
const workspace = "close-result";

beforeEach(() => {
  useViewerStore.setState({
    workspaces: { [team]: [{ id: workspace, kind: "generic", cwd: "/w", status: "ready" }] },
    activeWorkspace: {},
    softClosedWorkspaces: {},
  });
});

describe("page closeTerminalTab host result", () => {
  it("reports success after the host closes the shown tab", async () => {
    seedTerminalTabs(team, workspace, [{ session: "shown", role: "agent" }]);
    await expect(HTML_BRIDGE_ALLOWLIST.closeTerminalTab({ handle: "shown" })).resolves.toEqual({
      closed: true,
    });
    expect(terminalHost().contains("shown")).toBe(false);
  });

  it.each([true, false])("rejects a host failure for a shown=%s tab", async (shown) => {
    if (shown) seedTerminalTabs(team, workspace, [{ session: "refused", role: "agent" }]);
    const close = vi.spyOn(terminalHost(), "closeTab").mockImplementation(() => {
      throw new Error("host unavailable");
    });
    try {
      await expect(
        HTML_BRIDGE_ALLOWLIST.closeTerminalTab({ handle: "refused" }),
      ).rejects.toBeDefined();
      if (shown) expect(terminalHost().contains("refused")).toBe(true);
    } finally {
      close.mockRestore();
    }
  });
});

it("an unknown handle is unavailable and never reaches the terminal host", async () => {
  const close = vi.spyOn(terminalHost(), "closeTab");
  await expect(HTML_BRIDGE_ALLOWLIST.closeTerminalTab({ handle: "missing" })).rejects.toThrow(
    "unavailable: no such tab",
  );
  expect(close).not.toHaveBeenCalled();
  close.mockRestore();
});

it("resolves a tab id before a different tab's matching session string", async () => {
  const viewerModule = await import("@/shared/stores/viewer-store");
  const first = { id: "chosen", sessionId: "session-a", name: "Chosen" };
  const collision = { id: "other", sessionId: "chosen", name: "Other" };
  const index = vi
    .spyOn(viewerModule, "selectRunningTerminalTabIndex")
    .mockReturnValue({ [team]: { [workspace]: [first, collision] } } as never);
  const close = vi
    .spyOn(useViewerStore.getState(), "removeTerminalTab")
    .mockResolvedValue(undefined);
  const bySession = vi.spyOn(useViewerStore.getState(), "removeTerminalTabBySession");
  try {
    await HTML_BRIDGE_ALLOWLIST.closeTerminalTab({ handle: "chosen" });
    expect(close).toHaveBeenCalledWith(
      team,
      "chosen",
      workspace,
      expect.objectContaining({ kill: true }),
    );
    expect(bySession).not.toHaveBeenCalled();
  } finally {
    index.mockRestore();
    close.mockRestore();
    bySession.mockRestore();
  }
});
