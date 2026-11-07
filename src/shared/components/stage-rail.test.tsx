// SPDX-License-Identifier: MIT
import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { StageRail } from "./stage-rail";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import type { StatusState } from "@/tauri/commands";

function seed(states: StatusState[]) {
  useStatusTaxonomyStore.getState().setStates(states);
}

const STATE = (id: string, role: StatusState["role"], label = id): StatusState => ({
  id,
  label,
  color: "#ff0000",
  icon: "Circle",
  order: 0,
  role,
});

function rail(status: string) {
  const { container, unmount } = render(<StageRail status={status} />);
  const el = container.querySelector<HTMLElement>(".cs-rail")!;
  return { el, unmount };
}

describe("StageRail — the stage drawn by taxonomy role", () => {
  beforeEach(() => {
    seed([
      STATE("todo", "not_started", "Todo"),
      STATE("doing", "active", "Doing"),
      STATE("check", "review", "Check"),
      STATE("shipped", "done", "Shipped"),
      STATE("stuck", "blocked", "Stuck"),
    ]);
  });
  afterEach(() => useStatusTaxonomyStore.getState().resetForTest());

  it("keys the shape off the role, never the id: a team's own states draw by role", () => {
    for (const [id, role] of [
      ["todo", "not_started"],
      ["doing", "active"],
      ["check", "review"],
      ["shipped", "done"],
      ["stuck", "blocked"],
    ] as const) {
      const { el, unmount } = rail(id);
      expect(el).toHaveAttribute("data-rail-role", role);
      unmount();
    }
  });

  it("fills in thirds: nothing, one, two, all", () => {
    const width = (id: string) => {
      const { el, unmount } = rail(id);
      const fill = el.querySelector("[data-rail='fill']");
      const w = fill ? Number(fill.getAttribute("width")) : 0;
      unmount();
      return w;
    };
    const [none, one, two, all] = ["todo", "doing", "check", "shipped"].map(width);
    expect(none).toBe(0);
    expect(one).toBeGreaterThan(0);
    expect(two).toBeGreaterThan(one);
    expect(all).toBeGreaterThan(two);
  });

  it("walls Blocked, and dots a status outside the taxonomy", () => {
    const blocked = rail("stuck");
    expect(blocked.el.querySelector("[data-rail='wall']")).not.toBeNull();
    blocked.unmount();
    const off = rail("someday");
    expect(off.el).toHaveAttribute("data-rail-role", "off");
    expect(off.el.querySelector("rect")).toHaveAttribute("stroke-dasharray");
    off.unmount();
  });

  it("is ink only: the taxonomy colour never reaches the rail", () => {
    const { el } = rail("doing");
    expect(el.innerHTML).not.toContain("#ff0000");
    expect(el).toHaveAttribute("aria-label", "Doing");
  });
});
