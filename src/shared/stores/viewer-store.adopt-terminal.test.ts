// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useOrgStore } from "./org-store";
import { useViewerStore, type Workspace } from "./viewer-store";

installTerminalLayoutHarness();

const ACTIVE = "acme:default";
const OTHER = "acme:other";
const ws = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

describe("adoptTerminalSession into a team that is not active", () => {
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    useViewerStore.setState({
      openedTeams: [ACTIVE, OTHER],
      workspaces: { [ACTIVE]: [ws("a-1")], [OTHER]: [ws("o-1"), ws("o-2")] },
      activeWorkspace: { [ACTIVE]: "a-1", [OTHER]: "o-1" },
      softClosedWorkspaces: {},
    });
    seedTerminalTabs(ACTIVE, "a-1", ["sess-a"]);
    seedTerminalTabs(OTHER, "o-2", ["sess-o"]);
  });

  it("switches no team: the session waits in that team, the active team's focus stands", async () => {
    await useViewerStore.getState().adoptTerminalSession(OTHER, "sess-bg", "o-2");
    expect(useOrgStore.getState().activeTeamName).toBe("default");
    expect(useViewerStore.getState().activeWorkspace[ACTIVE]).toBe("a-1");
  });
});
