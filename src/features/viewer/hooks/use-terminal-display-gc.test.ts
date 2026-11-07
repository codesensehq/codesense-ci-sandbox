// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";
import type { TerminalManager } from "@/shared/terminal/terminal-manager";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useTerminalDisplayGc } from "./use-terminal-display-gc";

installTerminalLayoutHarness();

const TEAM = "acme:default";

function mount() {
  const dispose = vi.fn();
  renderHook(() => useTerminalDisplayGc({ dispose } as unknown as TerminalManager));
  return dispose;
}

describe("a display lives as long as its tab", () => {
  it("closing a tab disposes its display; nothing else does", () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          { id: "ws1", kind: "generic", cwd: "/w/1", status: "ready" },
          { id: "ws2", kind: "generic", cwd: "/w/2", status: "ready" },
        ],
      },
    });
    const ws1 = seedTerminalTabs(TEAM, "ws1", ["a", "b"]);
    seedTerminalTabs(TEAM, "ws2", ["c"]);
    const dispose = mount();

    // Unrelated store churn and a tab moving between workspaces keep displays.
    useViewerStore.setState({ stateLoaded: true });
    terminalHost().placeTab("b", ws1, "tab", 0);
    expect(dispose).not.toHaveBeenCalled();

    terminalHost().closeTab("a", "kill");
    expect(dispose).toHaveBeenCalledExactlyOnceWith("a");
  });
});
