// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { StatusState } from "@/tauri/commands";
import { DEFAULT_STATUS_STATES } from "./status-config";

// SW-GT-3 · AC-8b — FE↔Rust default parity.
//
// The board's default taxonomy (DEFAULT_STATUS_STATES) MUST match the Rust
// `default_meta` (`src-tauri/src/commands/status_taxonomy.rs`), ordered by the
// `VALID_STATUSES` id-index. If either side changes a label/color/icon-name/role/
// order, this fails — so the board defaults can never silently drift from
// `validate_story`'s defaults. Update BOTH sides together (and this table).
const RUST_DEFAULT_META: StatusState[] = [
  {
    id: "planning",
    label: "Planning",
    color: "#64748b",
    icon: "CircleDashed",
    order: 0,
    role: "not_started",
  },
  { id: "todo", label: "Todo", color: "#94a3b8", icon: "CircleDot", order: 1, role: "not_started" },
  {
    id: "in-progress",
    label: "In progress",
    color: "#0ea5e9",
    icon: "Circle",
    order: 2,
    role: "active",
  },
  { id: "blocked", label: "Blocked", color: "#ef4444", icon: "Ban", order: 3, role: "blocked" },
  { id: "review", label: "Review", color: "#8b5cf6", icon: "Eye", order: 4, role: "review" },
  { id: "qa", label: "QA", color: "#f59e0b", icon: "ShieldCheck", order: 5, role: "review" },
  { id: "done", label: "Done", color: "#10b981", icon: "CheckCircle2", order: 6, role: "done" },
];

describe("DEFAULT_STATUS_STATES ↔ Rust default_meta parity (AC-8b)", () => {
  it("matches the Rust default taxonomy exactly (labels/colors/icons/roles/order)", () => {
    expect(DEFAULT_STATUS_STATES).toEqual(RUST_DEFAULT_META);
  });

  it("orders the states by `order` = the VALID_STATUSES id-index", () => {
    expect(DEFAULT_STATUS_STATES.map((s) => s.order)).toEqual([0, 1, 2, 3, 4, 5, 6]);
    expect(DEFAULT_STATUS_STATES.map((s) => s.id)).toEqual([
      "planning",
      "todo",
      "in-progress",
      "blocked",
      "review",
      "qa",
      "done",
    ]);
  });
});
