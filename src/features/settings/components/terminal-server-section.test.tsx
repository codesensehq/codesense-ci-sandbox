// SPDX-License-Identifier: MIT
/**
 * SV-7: the "Terminal server" section of Settings → General, served by the real
 * mock backend (`src/app/mock/handlers/terminal-server.ts`) behind the same
 * `__TAURI_INTERNALS__` seam `pnpm dev:mock` uses.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { createInvokeRouter } from "@/app/mock/mock-router";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { terminalServerHandlers } from "@/app/mock/handlers/terminal-server";
import type { HandlerRegistry } from "@/app/mock/mock-router";
import { resetTerminalServerForTests } from "@/features/shell/lib/terminal-server";

import { TerminalServerSection, uptime } from "./terminal-server-section";

/** Serve the terminal server commands for the mock world `search` names;
 *  returns the recorded invoke calls. */
function installTerminalServerBackend(search: string, overrides: HandlerRegistry = {}) {
  (
    window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }
  ).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  const calls: { cmd: string; args: Record<string, unknown> }[] = [];
  const router = createInvokeRouter({
    ...terminalServerHandlers(search, () => ({ session: "s1" })),
    list_stray_terminal_servers: () => [],
    ...overrides,
  });
  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = buildTauriInternals(
    (cmd, args = {}) => {
      calls.push({ cmd, args });
      return router(cmd, args);
    },
  );
  return { calls };
}

describe("Settings → Terminal server (SV-7)", () => {
  beforeEach(() => resetTerminalServerForTests());

  it("shows a running server: what it holds and the instances connected", async () => {
    installTerminalServerBackend("?otherInstances=1");
    render(<TerminalServerSection />);
    expect(await screen.findByTestId("terminal-server-state")).toHaveTextContent("Running");
    expect(screen.getByTestId("terminal-server-identity")).toHaveTextContent(
      /pid 4321 · 0\.0\.25 · up 3h 0m/,
    );
    expect(screen.getByTestId("terminal-server-sessions")).toHaveTextContent(
      "4 sessions · 2 running agents · 1 exited, kept",
    );
    expect(screen.getByTestId("terminal-server-instances")).toHaveTextContent(
      "2 CodeSense instances connected (1 besides this one)",
    );
    expect(screen.queryByTestId("terminal-server-start")).toBeNull();
  });

  it("stops after a confirmation naming what ends, warning of the other instance", async () => {
    const user = userEvent.setup();
    const { calls } = installTerminalServerBackend("?otherInstances=1");
    render(<TerminalServerSection />);
    await user.click(await screen.findByTestId("terminal-server-stop"));
    const confirm = screen.getByTestId("terminal-server-confirm");
    expect(confirm).toHaveTextContent(
      "Stop the terminal server? It ends 3 running sessions, 2 of them running agents. Its exited session goes too.",
    );
    expect(within(confirm).getByTestId("terminal-server-confirm-others")).toHaveTextContent(
      "1 other CodeSense instance is connected to it: its tabs end too.",
    );
    expect(screen.getByTestId("terminal-server-confirm-cancel")).toHaveFocus();
    await user.click(screen.getByTestId("terminal-server-stop-yes"));
    await waitFor(() =>
      expect(screen.getByTestId("terminal-server-state")).toHaveTextContent("Not running"),
    );
    const stop = calls.find((call) => call.cmd === "stop_terminal_server");
    expect(stop?.args).toMatchObject({
      expected: { serverId: "mock-server", epoch: "mock-epoch-1", pid: 4321 },
    });
  });

  it("starts a server that is not running", async () => {
    const user = userEvent.setup();
    installTerminalServerBackend("?terminalServer=stopped");
    render(<TerminalServerSection />);
    expect(await screen.findByTestId("terminal-server-state")).toHaveTextContent("Not running");
    expect(screen.queryByTestId("terminal-server-stop")).toBeNull();
    expect(screen.queryByTestId("terminal-server-restart")).toBeNull();
    await user.click(screen.getByTestId("terminal-server-start"));
    await waitFor(() =>
      expect(screen.getByTestId("terminal-server-state")).toHaveTextContent("Running"),
    );
    expect(screen.getByTestId("terminal-server-sessions")).toHaveTextContent("0 sessions");
  });

  it("restarts a running server bound to what it read", async () => {
    const user = userEvent.setup();
    const { calls } = installTerminalServerBackend("");
    render(<TerminalServerSection />);
    await user.click(await screen.findByTestId("terminal-server-restart"));
    expect(screen.getByTestId("terminal-server-confirm")).toHaveTextContent(
      "Restart the terminal server? It ends 3 running sessions, 2 of them running agents.",
    );
    expect(screen.queryByTestId("terminal-server-confirm-others")).toBeNull();
    await user.click(screen.getByTestId("terminal-server-restart-yes"));
    await waitFor(
      () => expect(screen.getByTestId("terminal-server-sessions")).toHaveTextContent("0 sessions"),
      { timeout: 3000 },
    );
    expect(calls.some((call) => call.cmd === "replace_terminal_server")).toBe(true);
  });

  it("lists the sessions no app holds, each with End", async () => {
    const user = userEvent.setup();
    installTerminalServerBackend("?unowned=2");
    render(<TerminalServerSection />);
    const first = await screen.findByTestId("terminal-server-unowned-left-1");
    expect(first).toHaveTextContent("claude");
    expect(first).toHaveTextContent("running an agent");
    expect(screen.getByTestId("terminal-server-unowned-left-2")).toHaveTextContent("shell");
    await user.click(screen.getByTestId("terminal-server-unowned-end-left-1"));
    expect(screen.getByTestId("terminal-server-unowned-confirm-left-1")).toHaveTextContent(
      "End claude? The agent it runs stops.",
    );
    await user.click(screen.getByTestId("terminal-server-unowned-end-yes-left-1"));
    await waitFor(() => expect(screen.queryByTestId("terminal-server-unowned-left-1")).toBeNull());
    expect(screen.getByTestId("terminal-server-unowned-left-2")).toBeInTheDocument();
  });

  it("asks for a fresh review when the server changed since the confirmation opened", async () => {
    const user = userEvent.setup();
    let reads = 0;
    const handlers = terminalServerHandlers("", () => ({ session: "s1" }));
    installTerminalServerBackend("", {
      terminal_server_overview: (args) => {
        reads += 1;
        const read = handlers.terminal_server_overview(args) as Record<string, unknown>;
        // Every read after the first shows one more running session.
        if (reads === 1) return read;
        const expected = read.expected as { sessionIds: string[] };
        return {
          ...read,
          ends: 4,
          expected: { ...expected, sessionIds: [...expected.sessionIds, "new"] },
        };
      },
    });
    render(<TerminalServerSection />);
    await user.click(await screen.findByTestId("terminal-server-stop"));
    expect(screen.getByTestId("terminal-server-confirm")).toHaveTextContent(
      "It ends 3 running sessions",
    );
    await waitFor(
      () => expect(screen.getByTestId("terminal-server-confirm-changed")).toBeInTheDocument(),
      { timeout: 7000 },
    );
    expect(screen.queryByTestId("terminal-server-stop-yes")).toBeNull();
    await user.click(screen.getByTestId("terminal-server-review-again"));
    await user.click(await screen.findByTestId("terminal-server-stop"));
    expect(screen.getByTestId("terminal-server-confirm")).toHaveTextContent(
      "It ends 4 running sessions",
    );
  }, 15000);

  it("restarts an incompatible server through the restart prompt's path, with its count", async () => {
    const user = userEvent.setup();
    installTerminalServerBackend("?terminalServer=incompatible&liveSessions=2");
    render(<TerminalServerSection />);
    expect(await screen.findByTestId("terminal-server-identity")).toHaveTextContent("pid 3210");
    expect(screen.queryByTestId("terminal-server-stop")).toBeNull();
    await user.click(screen.getByTestId("terminal-server-restart"));
    expect(screen.getByTestId("terminal-server-confirm")).toHaveTextContent(
      "It ends 2 running sessions.",
    );
    await user.click(screen.getByTestId("terminal-server-restart-yes"));
    await waitFor(
      () => expect(screen.getByTestId("terminal-server-state")).toHaveTextContent(/^Running$/),
      { timeout: 3000 },
    );
  });

  it("offers nothing to manage when the app hosts its terminals itself", async () => {
    installTerminalServerBackend("?terminalServer=inProcess");
    render(<TerminalServerSection />);
    expect(await screen.findByTestId("terminal-server-state")).toHaveTextContent(
      "This app hosts its terminals itself",
    );
    expect(screen.queryByTestId("terminal-server-start")).toBeNull();
    expect(screen.queryByTestId("terminal-server-restart")).toBeNull();
  });

  it("says when a read failed, keeping what it said last", async () => {
    let reads = 0;
    const handlers = terminalServerHandlers("", () => ({ session: "s1" }));
    installTerminalServerBackend("", {
      terminal_server_overview: (args) => {
        reads += 1;
        if (reads > 1) throw new Error("no answer");
        return handlers.terminal_server_overview(args);
      },
    });
    render(<TerminalServerSection />);
    expect(await screen.findByTestId("terminal-server-state")).toHaveTextContent("Running");
    await waitFor(
      () => expect(screen.getByTestId("terminal-server-read-failed")).toBeInTheDocument(),
      { timeout: 7000 },
    );
    expect(screen.getByTestId("terminal-server-state")).toHaveTextContent("Running");
  }, 15000);

  it("says how long it has run", () => {
    expect(uptime(0, 59_000)).toBe("0m");
    expect(uptime(0, 3 * 3_600_000 + 5 * 60_000)).toBe("3h 5m");
    expect(uptime(0, 50 * 3_600_000)).toBe("2d 2h");
  });
});
