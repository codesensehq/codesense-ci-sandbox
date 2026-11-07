// SPDX-License-Identifier: MIT
import type { AgentRow } from "@/tauri/commands";

import { agentIdOfKey } from "./launch-sheet";

/**
 * The listed agent a session's launch named by `key`. When the list knows it under another
 * key with the same id (the inbox is addressed by that id), the team's live one is preferred,
 * then any live one.
 */
export function matchAgentRow(rows: readonly AgentRow[], key: string): AgentRow | undefined {
  const id = agentIdOfKey(key);
  return (
    rows.find((r) => r.key === key) ??
    rows.find((r) => !r.archived && r.scope === "team" && agentIdOfKey(r.key) === id) ??
    rows.find((r) => !r.archived && agentIdOfKey(r.key) === id)
  );
}

/** A name for an agent the list does not know, from its id: `bug-triage` → `Bug triage`. */
export function nameFromAgentKey(key: string): string {
  const id = agentIdOfKey(key).replace(/[-_]+/g, " ").trim();
  return id ? id[0].toUpperCase() + id.slice(1) : key;
}
