// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { describeDiffError, needsServerUpdate } from "./source-diff-errors";

describe("describeDiffError", () => {
  it("passes local string and Error rejections through", () => {
    expect(describeDiffError("fatal: bad object")).toBe("fatal: bad object");
    expect(describeDiffError(new Error("boom"))).toBe("boom");
  });

  it("never shows a bare undefined/null for an empty rejection", () => {
    for (const e of [undefined, null, "", new Error("")]) {
      expect(describeDiffError(e)).toBe("The diff couldn’t be loaded.");
    }
  });

  it("names the problem for each typed remote error instead of [object Object]", () => {
    expect(describeDiffError({ kind: "disconnected" })).toMatch(/reach the machine/);
    expect(describeDiffError({ kind: "boundsExceeded" })).toMatch(/too large/);
    expect(
      describeDiffError({ kind: "transportFailure", message: "Update the terminal server" }),
    ).toBe("Update the terminal server");
    expect(describeDiffError({ kind: "unauthorized" })).not.toContain("[object");
  });

  it("words notFound for what was being read", () => {
    expect(describeDiffError({ kind: "notFound" })).toMatch(/file is no longer/);
    expect(describeDiffError({ kind: "notFound" }, "changes")).toMatch(/workspace is no longer/);
    for (const kind of ["boundsExceeded", "conflict"]) {
      expect(describeDiffError({ kind })).toMatch(/file/i);
      expect(describeDiffError({ kind }, "changes")).not.toMatch(/file/i);
    }
  });

  it("never prints [object Object] for an unlisted kind or a bare object", () => {
    expect(describeDiffError({ kind: "somethingNew", message: "machine said no" })).toBe(
      "machine said no",
    );
    expect(describeDiffError({ kind: "somethingNew" })).not.toContain("[object");
    expect(describeDiffError({})).not.toContain("[object");
  });
});

describe("needsServerUpdate", () => {
  it("recognizes the backend's out-of-date terminal server message, and nothing else", () => {
    expect(
      needsServerUpdate(
        "This machine's terminal server is too old to diff its workspaces. Update the terminal server from Machines, then retry.",
      ),
    ).toBe(true);
    expect(needsServerUpdate("Can’t reach the machine right now.")).toBe(false);
  });
});
