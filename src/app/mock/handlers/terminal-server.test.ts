// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import type { TerminalServerOverview } from "@/tauri/commands";
import { incompatibleTerminalServer, terminalServerHandlers } from "./terminal-server";

/** The host's open, as the mock backend hands it in: answers with the entry. */
const open = vi.fn(() => ({ session: "s1" }));

describe("terminalServerHandlers", () => {
  afterEach(() => vi.useRealTimers());

  it("reports a compatible terminal server by default", () => {
    expect(terminalServerHandlers("", open).terminal_server_status({})).toBeNull();
    expect(
      terminalServerHandlers("?scenario=onboarded", open).terminal_server_status({}),
    ).toBeNull();
  });

  it("boots an incompatible server by link, with its count, cleared by a restart", async () => {
    vi.useFakeTimers();
    const h = terminalServerHandlers("?terminalServer=incompatible&liveSessions=1", open);
    expect(h.terminal_server_status({})).toEqual(incompatibleTerminalServer(1));
    const restarted = h.restart_terminal_server({}) as Promise<number | null>;
    await vi.runAllTimersAsync();
    expect(await restarted).toBe(1);
    expect(h.terminal_server_status({})).toBeNull();
    expect(
      terminalServerHandlers(
        "?terminalServer=incompatible&liveSessions=unknown",
        open,
      ).terminal_server_status({}),
    ).toEqual(incompatibleTerminalServer(null));
  });

  it("refuses new terminals while incompatible, and opens them once restarted", async () => {
    vi.useFakeTimers();
    open.mockClear();
    const h = terminalServerHandlers("?terminalServer=incompatible&liveSessions=2", open);
    const reason = incompatibleTerminalServer(2).reason;
    await expect(h.terminal_open_tab({ request: {} })).rejects.toBe(reason);
    expect(open).not.toHaveBeenCalled();
    const restarted = h.restart_terminal_server({}) as Promise<number | null>;
    await vi.runAllTimersAsync();
    await restarted;
    expect(await h.terminal_open_tab({ request: {} })).toEqual({ session: "s1" });
    expect(open).toHaveBeenCalledOnce();
    expect(await terminalServerHandlers("", open).terminal_open_tab({})).toEqual({ session: "s1" });
  });

  it("fails the first restart as asked, then succeeds", async () => {
    vi.useFakeTimers();
    for (const [kind, oldStopped] of [
      ["partial", true],
      ["error", false],
    ] as const) {
      const h = terminalServerHandlers(`?terminalServer=incompatible&restart=${kind}`, open);
      const first = h.restart_terminal_server({}) as Promise<number | null>;
      const failed = expect(first).rejects.toMatchObject({ oldStopped });
      await vi.runAllTimersAsync();
      await failed;
      const second = h.restart_terminal_server({}) as Promise<number | null>;
      await vi.runAllTimersAsync();
      await expect(second).resolves.not.toBeUndefined();
    }
  });

  it("SV-7: stops a running server bound to its review, then starts one", async () => {
    const h = terminalServerHandlers("?otherInstances=1", open);
    const running = h.terminal_server_overview({}) as TerminalServerOverview;
    expect(running.state).toBe("running");
    expect(running.otherInstances).toBe(1);
    const stale = { ...running.expected!, sessionIds: [] };
    await expect(h.stop_terminal_server({ expected: stale })).rejects.toMatchObject({
      oldStopped: false,
    });
    await expect(h.stop_terminal_server({ expected: running.expected })).resolves.toEqual({
      ended: 3,
    });
    expect((h.terminal_server_overview({}) as TerminalServerOverview).state).toBe("stopped");
    const started = h.start_terminal_server({}) as TerminalServerOverview;
    expect(started.state).toBe("running");
    expect(started.sessions).toBe(0);
  });

  it("SV-7: restarts a running server and ends a session no app holds", async () => {
    vi.useFakeTimers();
    const h = terminalServerHandlers("?unowned=2", open);
    const running = h.terminal_server_overview({}) as TerminalServerOverview;
    expect(running.unowned.map((u) => u.session)).toEqual(["left-1", "left-2"]);
    await h.end_unowned_terminal_session({ session: "left-1" });
    const after = h.terminal_server_overview({}) as TerminalServerOverview;
    expect(after.unowned.map((u) => u.session)).toEqual(["left-2"]);
    await expect(h.end_unowned_terminal_session({ session: "tab-1" })).rejects.toMatch(
      "not a session no app holds",
    );
    const restarted = h.replace_terminal_server({ expected: after.expected });
    await vi.runAllTimersAsync();
    await restarted;
    const fresh = h.terminal_server_overview({}) as TerminalServerOverview;
    expect(fresh.sessions).toBe(0);
    expect(fresh.expected!.epoch).not.toBe(after.expected!.epoch);
  });

  it("SV-7: a stopped world starts with none running", () => {
    const h = terminalServerHandlers("?terminalServer=stopped", open);
    expect((h.terminal_server_overview({}) as TerminalServerOverview).state).toBe("stopped");
  });
});
