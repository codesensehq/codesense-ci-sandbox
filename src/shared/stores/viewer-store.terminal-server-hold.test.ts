// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import {
  selectHeldTerminalOpens,
  selectTabBySessionId,
  selectWorkspaceTabs,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import { injectWhenReady, sessionExists } from "@/shared/lib/agent-session-injection";
import { resetTerminalServerForTests, restart } from "@/features/shell/lib/terminal-server";
import { installTerminalLayoutHarness, terminalHost } from "@/test/terminal-layout-harness";

const commands = vi.hoisted(() => ({
  terminalServerStatus: vi.fn(),
  restartTerminalServer: vi.fn(),
  writePty: vi.fn(async () => undefined),
  submitPtyLine: vi.fn(async () => undefined),
  sessionIsIdle: vi.fn(async () => true),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...commands,
}));

installTerminalLayoutHarness();

const TEAM = "acme/app:core";
const s = () => useViewerStore.getState();
const tabs = () => selectWorkspaceTabs(s(), TEAM, "w1").map((t) => t.sessionId);
const settle = () => new Promise((r) => setTimeout(r));

beforeEach(() => {
  resetTerminalServerForTests();
  commands.terminalServerStatus.mockReset();
  commands.restartTerminalServer.mockReset();
  commands.writePty.mockClear();
  commands.submitPtyLine.mockClear();
  useViewerStore.setState({
    workspaces: { [TEAM]: [{ id: "w1", kind: "generic", cwd: "/w1", status: "ready" }] },
    activeWorkspace: { [TEAM]: "w1" },
    softClosedWorkspaces: {},
    heldTerminalOpens: {},
  });
});

describe("a new terminal refused by an incompatible terminal server", () => {
  it("is held — no tab, no error — and opens once the server is restarted", async () => {
    commands.terminalServerStatus.mockResolvedValue({
      liveSessions: 2,
      serverProtocol: 5,
      appProtocol: 6,
      reason: "skew",
    });
    commands.restartTerminalServer.mockResolvedValue(2);
    vi.spyOn(terminalHost(), "openTab").mockImplementationOnce(() => {
      throw "restart terminal server: it speaks protocol 5";
    });

    let opened = false;
    const opening = s()
      .addTerminalTab(TEAM, { sessionId: "held" }, "w1")
      .then(() => (opened = true));
    await settle();
    expect(opened).toBe(false);
    expect(tabs()).toEqual([]);

    await restart();
    await opening;
    expect(tabs()).toEqual(["held"]);
  });

  it("while held, the workspace says so; nothing typed can reach the terminal, then or later", async () => {
    commands.terminalServerStatus.mockResolvedValue({
      liveSessions: 1,
      serverProtocol: 5,
      appProtocol: 6,
      reason: "skew",
    });
    commands.restartTerminalServer.mockResolvedValue(1);
    vi.spyOn(terminalHost(), "openTab").mockImplementationOnce(() => {
      throw "restart terminal server: it speaks protocol 5";
    });
    const opening = s().addTerminalTab(TEAM, { sessionId: "held" }, "w1");
    await settle();

    // A clear, visible wait — never a silent no-op.
    expect(selectHeldTerminalOpens(s(), TEAM, "w1")).toBe(1);
    // No tab and no pane: there is nothing to type into, and no writer (the
    // guarded injectors, a send) can reach the held session.
    expect(selectTabBySessionId(s(), "held")).toBeUndefined();
    expect(sessionExists("held")).toBe(false);
    await expect(
      injectWhenReady("held", "rm -rf build", { attempts: 2, sleepMs: 0 }),
    ).rejects.toThrow("not found");

    await restart();
    await opening;
    expect(tabs()).toEqual(["held"]);
    expect(selectHeldTerminalOpens(s(), TEAM, "w1")).toBe(0);
    // It starts clean: nothing typed around the refused open ever runs in it.
    expect(commands.writePty).not.toHaveBeenCalled();
    expect(commands.submitPtyLine).not.toHaveBeenCalled();
  });

  it("a workspace closed while its terminal was held never gets it", async () => {
    commands.terminalServerStatus.mockResolvedValue({
      liveSessions: 0,
      serverProtocol: 5,
      appProtocol: 6,
      reason: "skew",
    });
    commands.restartTerminalServer.mockResolvedValue(0);
    const open = vi.spyOn(terminalHost(), "openTab");
    open.mockImplementationOnce(() => {
      throw "restart terminal server: it speaks protocol 5";
    });
    const opening = s().addTerminalTab(TEAM, { sessionId: "orphan" }, "w1");
    await settle();
    useViewerStore.setState({ workspaces: { [TEAM]: [] } });

    await restart();
    await expect(opening).rejects.toThrow("closed while its terminal was held");
    expect(open).toHaveBeenCalledTimes(1);
    expect(terminalHost().contains("orphan")).toBe(false);
    expect(selectHeldTerminalOpens(s(), TEAM, "w1")).toBe(0);
  });

  it("any other failure is not held: the open rejects", async () => {
    vi.spyOn(terminalHost(), "openTab").mockImplementationOnce(() => {
      throw "spawn failed: no shell";
    });
    await expect(s().addTerminalTab(TEAM, { sessionId: "failed" }, "w1")).rejects.toBe(
      "spawn failed: no shell",
    );
    expect(commands.terminalServerStatus).not.toHaveBeenCalled();
    expect(tabs()).toEqual([]);
  });
});
