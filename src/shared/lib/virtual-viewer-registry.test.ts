// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { createElement, type ReactElement } from "react";

import {
  VIRTUAL_TAB_SCHEME,
  isVirtualTabPath,
  registerVirtualTab,
  resolveVirtualTab,
  resolveVirtualTabTitle,
  __resetVirtualTabsForTest,
} from "@/shared/lib/virtual-viewer-registry";

function dummy(label: string) {
  return (): ReactElement => createElement("div", { "data-label": label });
}

describe("virtual-viewer-registry @P1", () => {
  beforeEach(() => __resetVirtualTabsForTest());

  it("isVirtualTabPath detects the scheme", () => {
    expect(isVirtualTabPath(`${VIRTUAL_TAB_SCHEME}notes`)).toBe(true);
    expect(isVirtualTabPath("/real/file.md")).toBe(false);
    expect(isVirtualTabPath(null)).toBe(false);
    expect(isVirtualTabPath(undefined)).toBe(false);
  });

  it("resolves a registered component + title for its prefix", () => {
    const comp = dummy("notes");
    registerVirtualTab("virtual://notes", "Notes", comp);
    expect(resolveVirtualTab("virtual://notes")).toBe(comp);
    expect(resolveVirtualTab("virtual://notes/anything")).toBe(comp);
    expect(resolveVirtualTabTitle("virtual://notes")).toBe("Notes");
  });

  it("returns null for a non-virtual or unregistered path", () => {
    registerVirtualTab("virtual://notes", "Notes", dummy("notes"));
    expect(resolveVirtualTab("/real/file.md")).toBeNull();
    expect(resolveVirtualTab("virtual://settings")).toBeNull();
    expect(resolveVirtualTabTitle("/real/file.md")).toBeNull();
  });

  it("a sibling whose path shares a string-prefix is NOT matched (segment-aware)", () => {
    const comp = dummy("notes");
    registerVirtualTab("virtual://notes", "Notes", comp);
    // String-prefix but not a `/`-delimited descendant → must NOT resolve.
    expect(resolveVirtualTab("virtual://noteser")).toBeNull();
    expect(resolveVirtualTab("virtual://notes-other")).toBeNull();
    // Exact + real descendant still match.
    expect(resolveVirtualTab("virtual://notes")).toBe(comp);
    expect(resolveVirtualTab("virtual://notes/x")).toBe(comp);
  });

  it("longest-prefix wins (a second surface registers cleanly — AC-14)", () => {
    const broad = dummy("notes");
    const specific = dummy("notes-conflict");
    const settings = dummy("settings");
    registerVirtualTab("virtual://notes", "Notes", broad);
    registerVirtualTab("virtual://notes/conflict", "Conflict", specific);
    registerVirtualTab("virtual://settings", "Settings", settings);

    expect(resolveVirtualTab("virtual://notes")).toBe(broad);
    expect(resolveVirtualTab("virtual://notes/conflict/x")).toBe(specific);
    expect(resolveVirtualTabTitle("virtual://notes/conflict/x")).toBe("Conflict");
    // A second, unrelated surface coexists with no notes coupling.
    expect(resolveVirtualTab("virtual://settings")).toBe(settings);
  });
});
