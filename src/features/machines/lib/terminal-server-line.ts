// SPDX-License-Identifier: MIT
import type { MachineSummary } from "@/tauri/commands";

import type { MachineTone } from "./machine-status";

/** SW-AGR-19: one machine's terminal-server line in Machines — its version next
 *  to the app's, readable without color (`✓`, "older than app"). */
export interface TerminalServerLine {
  text: string;
  tone: MachineTone;
  /** Quiet facts after the line: This Mac's start, a dev build's date and commit. */
  details: string[];
  /** This Mac's server executable. */
  path?: string;
}

const WHEN = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

function when(ms: number): string {
  return WHEN.format(new Date(ms));
}

export function describeTerminalServer(machine: MachineSummary): TerminalServerLine | null {
  const server = machine.terminalServer;
  if (!server) return null;
  const app = server.appVersion;
  const build = server.buildDigest ? ` (${server.buildDigest})` : "";
  const named = server.serverVersion
    ? `Terminal server ${server.serverVersion}`
    : server.relation === "older"
      ? `Terminal server: an older build${build}`
      : `Terminal server: build${build || " unknown"}`;
  const [text, tone]: [string, MachineTone] = (() => {
    switch (server.relation) {
      case "same":
        return [`${named} ✓ up to date`, "ok"];
      case "newer":
        return [`${named} · newer than app ${app}`, "muted"];
      case "older":
        return [`${named} · older than app ${app}`, "warn"];
      case "otherBuild":
        // Not out of date, and nothing to do here: quiet.
        return ["Terminal server from another CodeSense build", "muted"];
      default:
        // A relation this build does not know (a newer backend): just name it.
        return [named, "muted"];
    }
  })();
  const details: string[] = [];
  if (server.devBuild && server.builtAtMs) details.push(`built ${when(server.builtAtMs)}`);
  if (server.devBuild && server.buildCommit) details.push(`commit ${server.buildCommit}`);
  if (machine.kind === "local" && server.startedAtMs)
    details.push(`started ${when(server.startedAtMs)}`);
  return {
    text,
    tone,
    details,
    ...(machine.kind === "local" && server.serverPath ? { path: server.serverPath } : {}),
  };
}
