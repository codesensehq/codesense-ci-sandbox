// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { MachineSummary, TerminalServerInfo } from "@/tauri/commands";

import { describeTerminalServer } from "./terminal-server-line";

function server(over: Partial<TerminalServerInfo> = {}): TerminalServerInfo {
  return {
    serverVersion: "0.0.22",
    appVersion: "0.0.22",
    upToDate: true,
    relation: "same",
    announcedVersion: "0.1.0",
    devBuild: false,
    ...over,
  };
}

function machine(kind: "local" | "ssh", terminalServer?: TerminalServerInfo): MachineSummary {
  return {
    machineId: kind === "local" ? "local" : "box",
    kind,
    label: kind === "local" ? "This Mac" : "box",
    scope: null,
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: true },
    terminalServer,
  };
}

// 2026-10-03 14:05 local time: formatted by the viewer's locale, so assert pieces.
const BUILT = new Date(2026, 9, 3, 14, 5).getTime();

describe("describeTerminalServer (SW-AGR-19)", () => {
  it("renders nothing for a machine without a connected server", () => {
    expect(describeTerminalServer(machine("ssh"))).toBeNull();
  });

  it("says up to date for the app's own build", () => {
    const line = describeTerminalServer(machine("ssh", server()))!;
    expect(line.text).toBe("Terminal server 0.0.22 ✓ up to date");
    expect(line.tone).toBe("ok");
    expect(line.details).toEqual([]);
  });

  it("names an older release against the app's", () => {
    const line = describeTerminalServer(
      machine(
        "ssh",
        server({
          serverVersion: "0.0.21",
          appVersion: "0.0.22",
          upToDate: false,
          relation: "older",
        }),
      ),
    )!;
    expect(line.text).toBe("Terminal server 0.0.21 · older than app 0.0.22");
    expect(line.tone).toBe("warn");
  });

  it("names an older build that predates build stamps by its digest", () => {
    const line = describeTerminalServer(
      machine(
        "ssh",
        server({
          serverVersion: undefined,
          appVersion: "0.0.23",
          upToDate: false,
          relation: "older",
          buildDigest: "a1b2c3d4e5f6",
        }),
      ),
    )!;
    expect(line.text).toBe(
      "Terminal server: an older build (a1b2c3d4e5f6) · older than app 0.0.23",
    );
  });

  it("leaves a newer server alone", () => {
    const line = describeTerminalServer(
      machine("ssh", server({ serverVersion: "0.0.24", relation: "newer" })),
    )!;
    expect(line.text).toBe("Terminal server 0.0.24 · newer than app 0.0.22");
    expect(line.tone).toBe("muted");
  });

  it("shows This Mac's server from another build with its path, never as out of date", () => {
    const line = describeTerminalServer(
      machine(
        "local",
        server({
          upToDate: false,
          relation: "otherBuild",
          serverPath: "/Users/me/codesense/target/release/codesense-session-server",
          appServerPath: "/Applications/CodeSense.app/Contents/MacOS/codesense-session-server",
          startedAtMs: BUILT,
          pid: 49806,
        }),
      ),
    )!;
    expect(line.text).toBe("Terminal server from another CodeSense build");
    expect(line.tone).toBe("muted");
    expect(line.path).toBe("/Users/me/codesense/target/release/codesense-session-server");
    expect(line.details.join(" ")).toMatch(/started/);
  });

  it("shows This Mac's start time and executable", () => {
    const local = server({
      serverPath: "/Applications/CodeSense.app/Contents/MacOS/codesense-session-server",
      startedAtMs: BUILT,
      pid: 49806,
    });
    const line = describeTerminalServer(machine("local", local))!;
    expect(line.details.some((d) => d.startsWith("started "))).toBe(true);
    expect(line.path).toBe(local.serverPath);
    // A remote machine's executable is not ours to show.
    expect(describeTerminalServer(machine("ssh", local))!.path).toBeUndefined();
  });

  it("never calls an unstamped newer or unknown server older", () => {
    const newer = describeTerminalServer(
      machine(
        "ssh",
        server({ serverVersion: undefined, relation: "newer", buildDigest: "a1b2c3d4e5f6" }),
      ),
    )!;
    expect(newer.text).toBe("Terminal server: build (a1b2c3d4e5f6) · newer than app 0.0.22");
    const unknown = describeTerminalServer(
      machine("ssh", server({ relation: "sideways" as never })),
    )!;
    expect(unknown.text).toBe("Terminal server 0.0.22");
    expect(unknown.tone).toBe("muted");
  });

  it("adds build date and commit in a dev build", () => {
    const line = describeTerminalServer(
      machine("ssh", server({ devBuild: true, buildCommit: "0123456789ab", builtAtMs: BUILT })),
    )!;
    expect(line.details).toContain("commit 0123456789ab");
    expect(line.details.some((d) => d.startsWith("built "))).toBe(true);
    const release = describeTerminalServer(
      machine("ssh", server({ buildCommit: "0123456789ab", builtAtMs: BUILT })),
    )!;
    expect(release.details).toEqual([]);
  });
});
