// SPDX-License-Identifier: MIT
// The registry-changed and MCP team-switch events are emitted by Rust and
// applied by `useOrgRegistrySync`; pin the TS names to the Rust literals so a
// rename on one side fails here instead of delivering to a dead channel.

import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, expect, it } from "vitest";

import {
  ACTIVE_TEAM_SWITCH_EVENT,
  ORG_REGISTRY_CHANGED_EVENT,
  SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
} from "./events";

function rustConst(file: string, name: string): string | undefined {
  const src = readFileSync(resolve(__dirname, "../../src-tauri/src", file), "utf-8");
  return src.match(new RegExp(`${name}:\\s*&str\\s*=\\s*"([^"]+)"`))?.[1];
}

describe("org registry event contract", () => {
  it("org-registry-changed matches the Rust emitter", () => {
    expect(rustConst("commands/registry_local.rs", "ORG_REGISTRY_CHANGED_EVENT")).toBe(
      ORG_REGISTRY_CHANGED_EVENT,
    );
  });

  it("the MCP team-switch event matches the Rust emitter", () => {
    expect(rustConst("mcp/setup_tools.rs", "ACTIVE_TEAM_SWITCH_EVENT")).toBe(
      ACTIVE_TEAM_SWITCH_EVENT,
    );
  });

  it("the source-repo attachments event matches the Rust emitter", () => {
    expect(rustConst("commands/attachments.rs", "SOURCE_ATTACHMENTS_CHANGED_EVENT")).toBe(
      SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
    );
  });
});
