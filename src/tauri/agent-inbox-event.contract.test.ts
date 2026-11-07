// SPDX-License-Identifier: MIT
// The Messages list refetches on the event the Rust inbox emits; a rename on one side
// must fail here instead of leaving the list stale.

import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, expect, it } from "vitest";

import { AGENT_INBOX_CHANGED_EVENT } from "./events";

const inboxSrc = readFileSync(
  resolve(__dirname, "../../src-tauri/src/commands/agent_inbox.rs"),
  "utf-8",
);

describe("agent inbox changed-event contract", () => {
  it("the TS event const equals the Rust AGENT_INBOX_CHANGED_EVENT literal", () => {
    const match = inboxSrc.match(/AGENT_INBOX_CHANGED_EVENT:\s*&str\s*=\s*"([^"]+)"/);
    expect(match, "Rust AGENT_INBOX_CHANGED_EVENT const not found").not.toBeNull();
    expect(AGENT_INBOX_CHANGED_EVENT).toBe(match![1]);
  });
});
