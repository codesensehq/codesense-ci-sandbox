// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { applyPillSelection } from "./agents-pane-pill";

describe("applyPillSelection", () => {
  it("asks for one explicit reveal, leaving visibility, folds and filters to their existing owners", () => {
    const store = { revealAgentsWorkspace: vi.fn() };
    applyPillSelection(store, "acme:default", "ws-a");
    expect(store.revealAgentsWorkspace).toHaveBeenCalledExactlyOnceWith("acme:default", "ws-a");
  });
});
