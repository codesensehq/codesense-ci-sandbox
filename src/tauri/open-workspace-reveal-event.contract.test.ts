// SPDX-License-Identifier: MIT
// SW-WA-F5: cross-check the FE reveal-event constant against the Rust literal the
// trusted-side `open_workspace` MCP tool emits, so the emitter (Rust) and the
// renderer's listener contract can never silently drift to different event names.

import { readFileSync } from "fs";
import { resolve } from "path";
import { describe, expect, it } from "vitest";

import { OPEN_WORKSPACE_REVEAL_EVENT } from "./events";

const handlerSrc = readFileSync(resolve(__dirname, "../../src-tauri/src/mcp/handler.rs"), "utf-8");
// The verb's body (legacy and targeted) and the one emit helper live here.
const emitSrc = readFileSync(
  resolve(__dirname, "../../src-tauri/src/commands/workspace_open.rs"),
  "utf-8",
);

describe("SW-WA-F5 open_workspace reveal-event contract", () => {
  it("the TS event const equals the Rust OPEN_WORKSPACE_REVEAL_EVENT literal", () => {
    // The trusted side declares `pub const OPEN_WORKSPACE_REVEAL_EVENT: &str = "…";`
    // — the FE listener subscribes to the SAME name, so a rename on one side must
    // fail here rather than deliver reveals to a dead channel.
    const match = handlerSrc.match(/OPEN_WORKSPACE_REVEAL_EVENT:\s*&str\s*=\s*"([^"]+)"/);
    expect(match, "Rust OPEN_WORKSPACE_REVEAL_EVENT const not found").not.toBeNull();
    expect(OPEN_WORKSPACE_REVEAL_EVENT).toBe(match![1]);
  });

  it("open_workspace emits the reveal event ONLY for visible modes", () => {
    // The emit is guarded by `mode.is_visible()` so a headless (opt-in) open never
    // fires a reveal — it stays in the Background agents panel.
    expect(emitSrc).toMatch(/if mode\.is_visible\(\) \{\s*port\.reveal\(/);
    expect(handlerSrc).toContain("OPEN_WORKSPACE_REVEAL_EVENT");
  });

  it("the reveal payload carries the handle + a focus intent", () => {
    // The renderer reveals by opaque handle and steals focus only when `focus` is
    // true (headed) — visible-unfocused surfaces a tab without grabbing the viewport.
    expect(emitSrc).toContain("OPEN_WORKSPACE_REVEAL_EVENT");
    expect(emitSrc).toMatch(/"handle":\s*handle,\s*"focus":\s*focus/);
    expect(emitSrc).toMatch(/port\.reveal\(&outcome\.handle,\s*mode\.is_focused\(\)\)/);
  });
});
