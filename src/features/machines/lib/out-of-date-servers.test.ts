// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { mockTerminalServer } from "@/app/mock/handlers/machines";
import type { MachineSummary, TerminalServerInfo } from "@/tauri/commands";

import { outOfDateEntry, outOfDateServers, undismissed } from "./out-of-date-servers";

function machine(
  id: string,
  terminalServer: TerminalServerInfo | undefined,
  state: "connected" | "idle" = "connected",
): MachineSummary {
  return {
    machineId: id,
    kind: id === "local" ? "local" : "ssh",
    label: id,
    scope: null,
    declaration: null,
    binding: null,
    canPublish: true,
    status: state === "connected" ? { state, workspaces: 1 } : { state },
    attention: 0,
    capabilities: { agentCallbacks: true },
    terminalServer,
  };
}

const older = (digest: string) =>
  mockTerminalServer(false, {
    serverVersion: undefined,
    upToDate: false,
    relation: "older",
    buildDigest: digest,
  });

describe("out-of-date terminal servers (SW-AGR-19 PR-2)", () => {
  it("counts connected servers older than the app, This Mac included — never other builds, newer ones or offline machines", () => {
    const list = [
      machine("local", mockTerminalServer(true, { upToDate: false, relation: "older" })),
      machine("a", older("aaaaaaaaaaaa")),
      machine("b", mockTerminalServer(false)),
      machine("c", mockTerminalServer(false, { serverVersion: "0.0.30", relation: "newer" })),
      machine("d", older("dddddddddddd"), "idle"),
      machine("e", older("eeeeeeeeeeee")),
      machine("f", mockTerminalServer(false, { upToDate: false, relation: "otherBuild" })),
    ];
    expect(outOfDateServers(list).map((m) => m.machineId)).toEqual(["local", "a", "e"]);
  });

  it("names each stale server by machine and build, falling back past a missing digest", () => {
    expect(outOfDateEntry(machine("a", older("aaaaaaaaaaaa")))).toBe("a@aaaaaaaaaaaa");
    const stamped = (commit: string, builtAtMs: number) =>
      machine(
        "a",
        mockTerminalServer(false, {
          relation: "older",
          serverVersion: "0.0.22",
          buildDigest: undefined,
          buildCommit: commit,
          builtAtMs,
        }),
      );
    expect(outOfDateEntry(stamped("0123456789ab", 5))).toBe("a@0123456789ab:5");
    // One commit rebuilt is another build.
    expect(outOfDateEntry(stamped("0123456789ab", 6))).not.toBe(
      outOfDateEntry(stamped("0123456789ab", 5)),
    );
    const bare = machine(
      "a",
      mockTerminalServer(false, {
        relation: "older",
        serverVersion: undefined,
        buildDigest: undefined,
        announcedVersion: "0.1.0",
      }),
    );
    expect(outOfDateEntry(bare)).toBe("a@0.1.0");
  });

  it("stays dismissed while every stale server was dismissed; a new one or a new build brings it back", () => {
    const a = machine("a", older("aaaaaaaaaaaa"));
    const b = machine("b", older("bbbbbbbbbbbb"));
    const dismissed = [outOfDateEntry(a), outOfDateEntry(b)];
    expect(undismissed([a, b], dismissed)).toBe(false);
    // One updated (or offline): still dismissed.
    expect(undismissed([a], dismissed)).toBe(false);
    // Another stale server, or another stale build on one already dismissed.
    expect(undismissed([a, b, machine("c", older("cccccccccccc"))], dismissed)).toBe(true);
    expect(undismissed([machine("a", older("abababababab")), b], dismissed)).toBe(true);
    expect(undismissed([], [])).toBe(false);
  });
});
