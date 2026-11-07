// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

vi.mock("@/tauri/commands", () => ({
  readFile: vi.fn(),
  WORKTREE_WRITE_CONFLICT_PREFIX: "conflict:",
}));

import { withLineEnding } from "./worktree-file-pane";

describe("withLineEnding (the merge's disk side)", () => {
  it("shows LF disk in a CRLF buffer's endings, and the reverse", () => {
    expect(withLineEnding("a\nb\n", "\r\n")).toBe("a\r\nb\r\n");
    expect(withLineEnding("a\r\nb\r\n", "\n")).toBe("a\nb\n");
  });
  it("leaves text already in the buffer's ending untouched (lone CRs included)", () => {
    expect(withLineEnding("a\rb\r\nc\r\n", "\r\n")).toBe("a\rb\r\nc\r\n");
  });
});
