// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { looksLikeOpaqueSessionHandle, memorableNameFromString } from "./memorable-name";

describe("memorableNameFromString", () => {
  it("produces two title-case words separated by a space", () => {
    const name = memorableNameFromString("term-tab-1791084011927390000-0");
    const words = name.split(" ");
    expect(words).toHaveLength(2);
    for (const word of words) {
      expect(word).toMatch(/^[A-Z][a-z]+$/);
    }
  });

  it("is deterministic per seed", () => {
    expect(memorableNameFromString("session-a")).toBe(memorableNameFromString("session-a"));
    expect(memorableNameFromString("session-b")).not.toBe(memorableNameFromString("session-c"));
  });

  it("rarely collides in a small sample", () => {
    const names = new Set<string>();
    for (let i = 0; i < 200; i++) {
      names.add(memorableNameFromString(`term-tab-${i}`));
    }
    expect(names.size).toBeGreaterThan(180);
  });
});

describe("looksLikeOpaqueSessionHandle", () => {
  it("recognises raw session-like handles", () => {
    expect(looksLikeOpaqueSessionHandle("term-tab-1791084011927390000-0")).toBe(true);
    expect(looksLikeOpaqueSessionHandle("pty-session-abc")).toBe(true);
    expect(looksLikeOpaqueSessionHandle("remote-tab-xyz")).toBe(true);
    expect(looksLikeOpaqueSessionHandle("remote-shell-abc")).toBe(true);
    expect(looksLikeOpaqueSessionHandle("")).toBe(true);
  });

  it("leaves a story / epic / repo handle alone", () => {
    expect(looksLikeOpaqueSessionHandle("SW-1")).toBe(false);
    expect(looksLikeOpaqueSessionHandle("SW-1#demo")).toBe(false);
    expect(looksLikeOpaqueSessionHandle("acme::team")).toBe(false);
    expect(looksLikeOpaqueSessionHandle("/path/to/story.md")).toBe(false);
  });
});
