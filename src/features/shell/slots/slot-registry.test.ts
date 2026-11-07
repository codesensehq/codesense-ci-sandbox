// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { expect, it } from "vitest";
import { SLOT_REGISTRY } from "./slot-registry";

it("keeps TypeScript and Rust slot lists identical", () => {
  const rust = readFileSync("src-tauri/src/verbs/capabilities.rs", "utf8");
  const slots = [
    ...rust.match(/pub const SLOTS:[\s\S]*?= &\[([\s\S]*?)\];/)![1].matchAll(/"([^"]+)"/g),
  ].map((m) => m[1]);
  expect(Object.keys(SLOT_REGISTRY).sort()).toEqual(slots.sort());
  expect(SLOT_REGISTRY["file-nav"]).toMatchObject({
    multiplicity: "one",
    presentations: ["docked"],
    teamBinding: "follows-context",
  });
});
