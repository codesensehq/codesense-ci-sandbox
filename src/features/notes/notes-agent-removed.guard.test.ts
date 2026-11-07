// SPDX-License-Identifier: MIT
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * The special notes agent is gone: notes are handed to the person's own agents
 * through the normal launch, a running session, or a routine. Nothing may reach for
 * the retired notes-agent module, its engagement store, its session adapter, its
 * headless workspace or its reveal door again.
 */

const SRC = resolve(__dirname, "../..");
const SELF = relative(SRC, __filename);

const RETIRED = [
  "NotesAgentPanelMount",
  "notes-agent-module",
  "use-notes-store",
  "useNotesAgentStore",
  "connect-notes-agent",
  "connectNotesAgent",
  "notes-session",
  "connectNotesSession",
  "open-notes-workspace",
  "openNotesWorkspace",
  "openOrFocusNotesWorkspace",
  "ensureNotesWorkspaceMinimized",
  "NOTES_WORKSPACE_ID",
  "NOTES_AGENT_PANEL_ID",
  "rehomeAgentWorkspaceToActiveTeam",
  "Tidy notes with an agent",
];

function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules") continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) sourceFiles(path, out);
    else if (/\.(tsx?|css)$/.test(entry.name)) out.push(path);
  }
  return out;
}

describe("the retired notes agent", () => {
  it("has no references left in the app source", () => {
    const hits: string[] = [];
    for (const file of sourceFiles(SRC)) {
      const rel = relative(SRC, file);
      if (rel === SELF) continue;
      const text = readFileSync(file, "utf8");
      for (const name of RETIRED) if (text.includes(name)) hits.push(`${rel}: ${name}`);
    }
    expect(hits).toEqual([]);
  });

  it("is no workspace kind any more", () => {
    const caps = readFileSync(join(SRC, "shared/lib/workspace-kind-capabilities.ts"), "utf8");
    expect(caps).not.toMatch(/WorkspaceKind = [^;]*"notes"/);
  });
});
