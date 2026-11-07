// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { MouseEvent } from "react";

// SW-EW-41: `startDragging` is the title bar's window-drag gate. In console the
// portaled workspace-switcher strip wires it as its own onMouseDown (the
// title-bar row's handler can't fire for foreign-portaled content), so this
// guard is live: it must move the window on empty strip area, but NOT on the
// dnd-kit chips (which drag onto a tile to split) or on interactive controls.
const mockStartDragging = vi.fn();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: mockStartDragging }),
}));

import { startDragging } from "./titlebar";

const evt = (target: HTMLElement, button = 0) => ({ button, target }) as unknown as MouseEvent;

describe("startDragging window-drag guard (SW-EW-41)", () => {
  beforeEach(() => mockStartDragging.mockClear());

  it("moves the window on a bare (empty strip) target", () => {
    startDragging(evt(document.createElement("div")));
    expect(mockStartDragging).toHaveBeenCalledTimes(1);
  });

  it("does NOT move the window when the target is a tray chip (reaches dnd-kit)", () => {
    const chip = document.createElement("div");
    chip.setAttribute("data-tray-chip", "e-1-a");
    startDragging(evt(chip));
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("does NOT move the window when the target is inside a tray chip", () => {
    const chip = document.createElement("div");
    chip.setAttribute("data-tray-chip", "e-1-a");
    const inner = document.createElement("span");
    chip.appendChild(inner);
    startDragging(evt(inner));
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("does NOT move the window on an interactive control (button)", () => {
    startDragging(evt(document.createElement("button")));
    expect(mockStartDragging).not.toHaveBeenCalled();
  });

  it("ignores non-primary mouse buttons", () => {
    startDragging(evt(document.createElement("div"), 2));
    expect(mockStartDragging).not.toHaveBeenCalled();
  });
});
