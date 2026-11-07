// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { MachineStatus, MachineSummary } from "@/tauri/commands";

import {
  ALLOW_AGENTS_NOTE,
  agentCallbacksNote,
  describeMachineStatus,
  machineBlockedReason,
  machineQualifiedPath,
} from "./machine-status";

const NOW = 1_000_000_000;

function machine(status: MachineStatus, over: Partial<MachineSummary> = {}): MachineSummary {
  return {
    machineId: "devbox-01",
    kind: "ssh",
    label: "devbox-01",
    scope: { kind: "personal" },
    declaration: null,
    binding: null,
    canPublish: true,
    status,
    attention: 0,
    capabilities: { agentCallbacks: true },
    ...over,
  };
}

// Sally §3 — one row per state: glyph + word, and the primary action.
const TABLE: [MachineStatus, string, string, string | null][] = [
  [{ state: "connected", workspaces: 2 }, "●", "Connected", null],
  [{ state: "idle" }, "○", "Not connected", "Connect"],
  [{ state: "connecting" }, "◐", "Connecting", "Retry now"],
  [{ state: "handshaking" }, "↻", "Connecting", "Retry now"],
  [{ state: "disconnected" }, "○", "Idle", "Connect"],
  [{ state: "connectionFailed", lastError: "ssh ended" }, "▲", "Unreachable", "Retry"],
  [{ state: "reconnecting", attempt: 2 }, "◐", "Reconnecting", "Retry now"],
  [
    { state: "unreachable", lastReachedAtMs: NOW - 180_000, attempt: 3, retryInSecs: 15 },
    "▲",
    "Offline",
    "Retry",
  ],
  [{ state: "needsCredential" }, "◌", "Needs setup", "Set up"],
  [{ state: "authFailed", reason: "key rejected" }, "🔑", "Sign-in failed", "Reconnect"],
  [
    { state: "hostKeyChanged", pinnedFp: "a", presentedFp: "b" },
    "⛔",
    "Host key changed",
    "Review key…",
  ],
  [{ state: "notInstalled" }, "⬇", "Terminal server not installed", "Install"],
  [
    { state: "versionMismatch", serverVersion: "0.8.2", appNeeds: "1.0", sessions: 5 },
    "◆",
    "Update needed",
    "Keep read-only",
  ],
];

describe("describeMachineStatus — Sally §3 status table", () => {
  it.each(TABLE)("%o → %s %s", (status, glyph, word, primary) => {
    const view = describeMachineStatus(machine(status), NOW);
    expect(view.glyph).toBe(glyph);
    expect(view.word).toBe(word);
    expect(view.actions[0]?.label ?? null).toBe(primary);
  });

  it("no state is a dead end: every state that blocks terminals offers a way forward", () => {
    for (const [status] of TABLE) {
      const view = describeMachineStatus(machine(status), NOW);
      if (view.blockedReason) {
        expect(view.actions.length, status.state).toBeGreaterThan(0);
      }
    }
  });

  it("names the machine and last contact in the qualifiers", () => {
    expect(describeMachineStatus(machine(TABLE[6][0]), NOW).detail).toBe(
      "sessions still running on devbox-01",
    );
    expect(describeMachineStatus(machine(TABLE[7][0]), NOW).detail).toBe(
      `since ${new Date(NOW - 180_000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`,
    );
  });

  it("unreachable offers Retry · Edit address", () => {
    const view = describeMachineStatus(machine(TABLE[7][0]), NOW);
    expect(view.actions.map((a) => a.id)).toEqual(["retry", "editAddress"]);
  });

  it("connection failure preserves Retry · Edit address", () => {
    const view = describeMachineStatus(
      machine({ state: "connectionFailed", lastError: "ssh ended" }),
      NOW,
    );
    expect(view.detail).toBe("ssh ended");
    expect(view.actions.map((a) => a.id)).toEqual(["retry", "editAddress"]);
  });

  it("version mismatch remains read-only until safe replacement is authenticated", () => {
    const view = describeMachineStatus(machine(TABLE[12][0]), NOW);
    expect(view.actions.map((a) => a.id)).toEqual(["keepReadOnly"]);
    expect(view.detail).toBe("server v0.8.2, app needs v1.0");
  });

  it("offers safe replacement only for a capability-aware frozen server", () => {
    const exact = describeMachineStatus(
      machine({ state: "terminalServerFrozen", count: 3, replacementSupported: true }),
      NOW,
    );
    expect(exact.detail).toBe("3 sessions");
    expect(exact.actions.map((a) => a.id)).toEqual(["restartServer"]);

    const unknown = describeMachineStatus(
      machine({ state: "terminalServerFrozen", count: null, replacementSupported: true }),
      NOW,
    );
    expect(unknown.detail).toBe("session count unavailable");

    const unsupported = describeMachineStatus(
      machine({ state: "terminalServerFrozen", count: null, replacementSupported: false }),
      NOW,
    );
    // SW-AGR-19: no safe replacement is no dead end — the confirmed reinstall.
    expect(unsupported.detail).toBe("update ends its sessions");
    expect(
      describeMachineStatus(
        machine({ state: "terminalServerFrozen", count: 3, replacementSupported: false }),
        NOW,
      ).detail,
    ).toBe("3 sessions · update ends them");
    expect(unsupported.actions).toEqual([
      { id: "restartServer", label: "Update terminal server…" },
    ]);
  });

  it("renders replacement phases, and a failed one offers the confirmed update again", () => {
    for (const state of ["replacementReserved", "replacing", "readmitting"] as const) {
      const view = describeMachineStatus(machine({ state, count: null }), NOW);
      expect(view.word).toBe("Restarting…");
      expect(view.actions).toEqual([]);
    }
    const before = describeMachineStatus(
      machine({
        state: "replacementFailed",
        oldStopped: false,
        count: 2,
        repair: "authority changed",
      }),
      NOW,
    );
    const after = describeMachineStatus(
      machine({
        state: "replacementFailed",
        oldStopped: true,
        count: 2,
        repair: "candidate failed",
      }),
      NOW,
    );
    expect(before.word).toBe("Read-only");
    expect(after.word).toBe("Restart incomplete");
    // SW-AGR-19: a failed restart is retried through the same confirmed update.
    const retry = [{ id: "restartServer", label: "Update terminal server…" }];
    expect(before.actions).toEqual(retry);
    expect(after.actions).toEqual(retry);
  });

  it("offers an explicit restart for a connected private machine without blocking terminals", () => {
    const privateMachine = machine(
      { state: "connected", workspaces: 2 },
      {
        declaration: {
          mode: "private-account",
          host: "box",
          port: 22,
          jumpHost: null,
          defaultBaseDir: null,
          pinnedHostKeyFp: "pin",
          pinnedServerId: "server",
        },
      },
    );
    const view = describeMachineStatus(privateMachine, NOW);
    expect(view.actions).toEqual([{ id: "restartServer", label: "Restart terminal server" }]);
    expect(view.blockedReason).toBeNull();
  });

  it("names a stale terminal server build on a connected machine and offers the update", () => {
    const declaration = {
      mode: "private-account" as const,
      host: "box",
      port: 22,
      jumpHost: null,
      defaultBaseDir: null,
      pinnedHostKeyFp: "pin",
      pinnedServerId: "server",
    };
    const stale = describeMachineStatus(
      machine({ state: "connected", workspaces: 2, terminalServerUpdate: true }, { declaration }),
      NOW,
    );
    expect(stale.word).toBe("Connected");
    expect(stale.detail).toBe("2 workspaces · terminal server update available");
    expect(stale.tone).toBe("warn");
    expect(stale.actions).toEqual([{ id: "restartServer", label: "Update terminal server" }]);
    expect(stale.blockedReason).toBeNull();
    const legacy = describeMachineStatus(
      machine({ state: "connected", workspaces: null, terminalServerUpdate: true }),
      NOW,
    );
    expect(legacy.detail).toBe("terminal server update available");
    // SW-AGR-19 (AC5): a machine that is not private-account updates in place
    // through the confirmed reinstall — never only set-up.
    expect(legacy.actions).toEqual([{ id: "restartServer", label: "Update terminal server" }]);
  });

  it("This Mac reads Running · N workspaces, with nothing to fix", () => {
    const view = describeMachineStatus(
      machine({ state: "connected", workspaces: 4 }, { kind: "local", machineId: "local" }),
      NOW,
    );
    expect([view.word, view.detail, view.actions]).toEqual(["Running", "4 workspaces", []]);
  });

  it("a connected remote without a census shows no count, never an apology", () => {
    const view = describeMachineStatus(machine({ state: "connected", workspaces: null }), NOW);
    expect([view.word, view.detail]).toEqual(["Connected", null]);
  });

  it("SW-AGR-19 PR-5: This Mac offers its update for an older or another build's server", () => {
    for (const relation of ["older", "otherBuild"] as const) {
      const view = describeMachineStatus(
        machine(
          { state: "connected", workspaces: 1 },
          {
            kind: "local",
            machineId: "local",
            terminalServer: {
              appVersion: "0.0.23",
              upToDate: false,
              relation,
              announcedVersion: "0.1.0",
              devBuild: false,
            },
          },
        ),
        NOW,
      );
      expect(view.actions).toEqual([{ id: "restartServer", label: "Update terminal server" }]);
    }
  });

  it("This Mac never offers SSH verbs (Edit address, Install, Set up…) in any state", () => {
    for (const [status] of TABLE) {
      const view = describeMachineStatus(
        machine(status, { kind: "local", machineId: "local" }),
        NOW,
      );
      expect(view.actions).toEqual([]);
    }
  });
});

describe("machineBlockedReason", () => {
  it("is null only when connected — every other state names why", () => {
    for (const [status] of TABLE) {
      const reason = machineBlockedReason(machine(status));
      if (status.state === "connected") expect(reason).toBeNull();
      else expect(reason).toMatch(/devbox-01/);
    }
  });
});

describe("helpers", () => {
  it("agent callbacks unavailable → an ⓘ note; available → none", () => {
    expect(agentCallbacksNote(machine({ state: "idle" }))).toBeNull();
    expect(
      agentCallbacksNote(machine({ state: "idle" }, { capabilities: { agentCallbacks: false } })),
    ).toBe("Agents can't report back here");
  });

  it("a private account's note names Allow agents…, and goes once it is on", () => {
    const privateAccount = (remoteMcpControl: boolean) =>
      machine(
        { state: "connected", workspaces: null },
        {
          capabilities: { agentCallbacks: false },
          remoteMcpControl,
          declaration: {
            mode: "private-account",
            host: "box",
            port: 22,
            jumpHost: null,
            defaultBaseDir: null,
            pinnedHostKeyFp: "pin",
            pinnedServerId: "server",
          },
        },
      );
    expect(agentCallbacksNote(privateAccount(false))).toBe(ALLOW_AGENTS_NOTE);
    expect(agentCallbacksNote(privateAccount(true))).toBeNull();
  });

  it("qualifies a path with its machine", () => {
    expect(machineQualifiedPath({ label: "devbox-01" }, "/home/dev/x.rs")).toBe(
      "devbox-01:/home/dev/x.rs",
    );
  });
});
