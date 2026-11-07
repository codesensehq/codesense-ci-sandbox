// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { getErrorMessage, SAFE_ERROR_REASON, userFacingErrorMessage } from "./error-message";

describe("userFacingErrorMessage (SW-LN-B28)", () => {
  it("strips every typed backend prefix", () => {
    for (const prefix of ["unavailable", "denied", "invalid", "conflict", "not_found", "busy"]) {
      expect(userFacingErrorMessage(`${prefix}: something went wrong`)).toBe(
        "something went wrong",
      );
    }
  });

  it("names a story the index can't find and what to check (SW-LN-B50 AC-3)", () => {
    const msg = userFacingErrorMessage("unavailable: no managed story with id 'TR-2'");
    expect(msg).toBe(
      "CodeSense can't find a story with id TR-2. Check that its file has `type: story`, `story_id: TR-2` and a `source_repo` in its frontmatter.",
    );
    expect(msg).not.toMatch(/indexed|try again/i);
  });

  it("still renders friendly copy when SW-CR-9 appends a hint on line 2", () => {
    // SW-CR-9 (R2): the Rust refusal keeps the typed form on line 1 so the
    // regex matches off `reasonLine`; the agent-facing hint on line 2 is
    // dropped from the user-facing copy.
    const msg = userFacingErrorMessage(
      "unavailable: no managed story with id 'TR-2'\n" +
        "Frontmatter cache + fallback scan both missed; verify the file is in a content layer with type: story and story_id: TR-2. Call refresh_frontmatter_cache(path) if the file was just written outside CodeSense.",
    );
    expect(msg).toBe(
      "CodeSense can't find a story with id TR-2. Check that its file has `type: story`, `story_id: TR-2` and a `source_repo` in its frontmatter.",
    );
    expect(msg).not.toContain("refresh_frontmatter_cache");
  });

  it("names the candidate file + fix when SW-CR-9's fallback finds an orphan", () => {
    // SW-CR-9 (R2): the fallback scan found a file whose frontmatter won't
    // index — the friendly copy calls out the file and what to fix.
    const msg = userFacingErrorMessage(
      "unavailable: story file candidate at /ctx/_bmad-output/x.md does not carry story_id 'SW-ORPHAN'\n" +
        "Verify its frontmatter has type: story, story_id: SW-ORPHAN, and source_repo. Call refresh_frontmatter_cache(path) after fixing it.",
    );
    expect(msg).toBe(
      "CodeSense found a file at /ctx/_bmad-output/x.md but its frontmatter doesn't declare story SW-ORPHAN. Check that it has `type: story`, `story_id: SW-ORPHAN` and a `source_repo`.",
    );
  });

  it("leaves an untyped message alone and reads Error objects", () => {
    expect(userFacingErrorMessage(new Error("disk full"))).toBe("disk full");
    expect(userFacingErrorMessage("path: /x is odd")).toBe("path: /x is odd");
    expect(getErrorMessage(new Error("x"))).toBe("x");
  });
});

describe("userFacingErrorMessage never shows raw error text (SW-FU-B8)", () => {
  const raw = [
    "thread 'main' panicked at src/commands/org.rs:12:5",
    "failed to read registry: No such file or directory (os error 2)",
    'TargetDirOccupied { path: "/x" }',
    "git2::Error: remote rejected",
    '{"kind":"failure","message":"x"}',
    "[object Object]",
    "TypeError: Cannot read properties of undefined (reading 'x')",
    "at doThing (app.js:10:4)",
    'invalid type: string "x", expected u64',
    "undefined",
  ];
  it.each(raw)("replaces %j with the safe reason", (text) => {
    expect(userFacingErrorMessage(text)).toBe(SAFE_ERROR_REASON);
    expect(userFacingErrorMessage(new Error(text))).toBe(SAFE_ERROR_REASON);
  });

  it("drops a leading `Error: ` and keeps a readable reason", () => {
    expect(userFacingErrorMessage("Error: disk full")).toBe("disk full");
  });

  it("keeps only the first line and caps a long reason", () => {
    expect(userFacingErrorMessage("Repository not found\nhint: more git noise")).toBe(
      "Repository not found",
    );
    const long = userFacingErrorMessage("a".repeat(400));
    expect(long.length).toBeLessThanOrEqual(241);
    expect(long.endsWith("…")).toBe(true);
  });

  it("gives the safe reason for an empty value", () => {
    expect(userFacingErrorMessage("")).toBe(SAFE_ERROR_REASON);
    expect(userFacingErrorMessage(null)).toBe(SAFE_ERROR_REASON);
  });
});

describe("userFacingErrorMessage keeps readable copy (SW-FU-B8 review)", () => {
  it.each([
    "Couldn't reach GitHub (check your network)",
    "connector needs a newer CodeSense (def schema v2; this app supports v1) — update to use it",
    "delete it on GitHub (or reopen its PR) first",
  ])("keeps %j", (text) => {
    expect(userFacingErrorMessage(text)).toBe(text);
  });

  it("hides a JS runtime error by its type, not only by its text", () => {
    expect(userFacingErrorMessage(new TypeError("x is not a function"))).toBe(SAFE_ERROR_REASON);
    expect(userFacingErrorMessage("Uncaught TypeError: x is not a function")).toBe(
      SAFE_ERROR_REASON,
    );
    expect(userFacingErrorMessage("DOMException: The operation was aborted")).toBe(
      SAFE_ERROR_REASON,
    );
  });

  it("hides Rust Debug values without a camel-case hump", () => {
    expect(userFacingErrorMessage('Io(Custom { kind: Other, error: "x" })')).toBe(
      SAFE_ERROR_REASON,
    );
  });

  it("prefers git's fatal line over progress chatter", () => {
    expect(
      userFacingErrorMessage("Cloning into 'x'...\nfatal: repository 'https://h/x' not found"),
    ).toBe("fatal: repository 'https://h/x' not found");
  });
});
