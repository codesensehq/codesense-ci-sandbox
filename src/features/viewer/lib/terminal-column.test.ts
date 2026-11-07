// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { isTerminalColumnShown, isTerminalSplitShown } from "./terminal-column";

describe("isTerminalColumnShown", () => {
  it("full-screen CLI is always shown", () => {
    expect(isTerminalColumnShown({ layoutMode: "console", terminalVisible: true })).toBe(true);
    expect(isTerminalColumnShown({ layoutMode: "console", terminalVisible: false })).toBe(true);
  });

  it("the collapsed (viewer) layout is closed, whatever the visibility flag says", () => {
    expect(isTerminalColumnShown({ layoutMode: "viewer", terminalVisible: true })).toBe(false);
  });

  it("otherwise follows terminalVisible, closed when never settled", () => {
    expect(isTerminalColumnShown({ layoutMode: "shared", terminalVisible: true })).toBe(true);
    expect(isTerminalColumnShown({ layoutMode: "shared", terminalVisible: false })).toBe(false);
    expect(isTerminalColumnShown({ layoutMode: null, terminalVisible: false })).toBe(false);
  });
});

describe("isTerminalSplitShown", () => {
  it("shows the split only when the column is shown outside full-screen CLI", () => {
    expect(isTerminalSplitShown({ layoutMode: null, terminalVisible: true })).toBe(true);
    expect(isTerminalSplitShown({ layoutMode: "shared", terminalVisible: true })).toBe(true);
    expect(isTerminalSplitShown({ layoutMode: "viewer", terminalVisible: true })).toBe(false);
    expect(isTerminalSplitShown({ layoutMode: "shared", terminalVisible: false })).toBe(false);
    expect(isTerminalSplitShown({ layoutMode: null, terminalVisible: false })).toBe(false);
  });

  it("is never on in full-screen CLI, which has no split", () => {
    expect(isTerminalSplitShown({ layoutMode: "console", terminalVisible: true })).toBe(false);
  });
});

it("without a shown team saved console or shared chrome shows no column or split", () => {
  for (const layoutMode of ["console", "shared"]) {
    expect(isTerminalColumnShown({ layoutMode, terminalVisible: true }, false)).toBe(false);
    expect(isTerminalSplitShown({ layoutMode, terminalVisible: true }, false)).toBe(false);
  }
});
