// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import {
  clearPendingJoinUrl,
  readPendingJoinUrl,
  setPendingJoinUrl,
  stripUrlCredentials,
} from "./pending-join";

beforeEach(() => {
  globalThis.localStorage?.clear();
});

describe("pending join URL (SW-FU-B6)", () => {
  it("is absent until a URL is typed", () => {
    expect(readPendingJoinUrl()).toBeNull();
  });

  it("keeps the typed URL in storage, so a reload reads it back", () => {
    setPendingJoinUrl("https://github.com/acme/team");
    expect(readPendingJoinUrl()).toBe("https://github.com/acme/team");
  });

  it("an emptied field drops it", () => {
    setPendingJoinUrl("https://github.com/acme/team");
    setPendingJoinUrl("   ");
    expect(readPendingJoinUrl()).toBeNull();
  });

  it("clears", () => {
    setPendingJoinUrl("https://github.com/acme/team");
    clearPendingJoinUrl();
    expect(readPendingJoinUrl()).toBeNull();
  });

  it("never stores credentials typed into the URL", () => {
    setPendingJoinUrl("https://me:ghp_secret@github.com/acme/team");
    expect(readPendingJoinUrl()).toBe("https://github.com/acme/team");
    expect(stripUrlCredentials("git@github.com:acme/team.git")).toBe(
      "git@github.com:acme/team.git",
    );
  });

  it("ignores garbage in storage", () => {
    for (const raw of ["{not json", "[]", "{}", "null", '{"url":3}']) {
      globalThis.localStorage?.setItem("codesense.pending-join", raw);
      expect(readPendingJoinUrl()).toBeNull();
    }
  });
});
