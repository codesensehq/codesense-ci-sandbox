// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { TagSchema, TagType } from "@/tauri/commands";
import { resolveLucideIcon, FALLBACK_LUCIDE_ICON } from "@/shared/lib/icons/lucide-icon-map";

import { tagDisplay, rowDisplay } from "./tag-display";

function schema(tags: TagSchema["tags"]): TagSchema {
  return { tags };
}

describe("rowDisplay per-type default glyphs (SW-AU-77I / AC-8)", () => {
  // The whole point of AC-8 is a visible org-tag glyph; if a per-type default icon
  // name isn't in the curated Lucide allowlist it silently renders the neutral
  // fallback. Pin every type's default to a resolvable icon.
  const ALL_TYPES: TagType[] = [
    "text",
    "select",
    "multi_select",
    "ordered",
    "status",
    "date",
    "relation",
    "unknown",
  ];
  it.each(ALL_TYPES)("resolves a real (non-fallback) glyph for a %s tag", (type) => {
    const { icon } = rowDisplay({ name: "x", type });
    expect(resolveLucideIcon(icon)).not.toBe(FALLBACK_LUCIDE_ICON);
  });
});

describe("tagDisplay (SW-DX-1 — the one tag render-config source)", () => {
  it("uses a declared color and icon verbatim", () => {
    const s = schema([
      {
        name: "priority",
        type: "select",
        allowedValues: ["low"],
        color: "--cs-status-modified",
        icon: "Flag",
      },
    ]);
    const d = tagDisplay(s, "priority");
    expect(d.color).toBe("var(--cs-status-modified)");
    expect(d.icon).toBe("Flag");
    expect(d.label).toBe("Priority");
  });

  it("passes a non-token declared color through unchanged", () => {
    const s = schema([{ name: "p", type: "select", allowedValues: [], color: "hsl(200 50% 50%)" }]);
    expect(tagDisplay(s, "p").color).toBe("hsl(200 50% 50%)");
  });

  it("falls back to a per-type --cs-* token + default icon when undeclared", () => {
    const s = schema([
      { name: "context_level", type: "ordered", allowedValues: ["machine", "team"] },
      { name: "due", type: "date", allowedValues: [] },
    ]);
    const ordered = tagDisplay(s, "context_level");
    expect(ordered.color).toBe("var(--cs-tag-ordered)");
    expect(ordered.icon).toBe("ArrowUpNarrowWide");
    expect(ordered.label).toBe("Context level");

    const date = tagDisplay(s, "due");
    expect(date.color).toBe("var(--cs-tag-date)");
    expect(date.icon).toBe("Calendar");
  });

  it("matches a tag by canonical key (case- and hyphen-insensitive, mirrors Rust)", () => {
    const s = schema([{ name: "context_level", type: "ordered", allowedValues: ["machine"] }]);
    // A doc key spelled with a hyphen or different case resolves the same def —
    // so the FE render config agrees with the Rust validity verdict.
    expect(tagDisplay(s, "context-level").icon).toBe("ArrowUpNarrowWide");
    expect(tagDisplay(s, "Context_Level").color).toBe("var(--cs-tag-ordered)");
    expect(tagDisplay(s, " context-level ").icon).toBe("ArrowUpNarrowWide");
  });

  it("handles an unknown-type tag with neutral defaults (mirrors Rust TagType::Unknown)", () => {
    const s = schema([{ name: "board", type: "unknown", allowedValues: [] }]);
    const d = tagDisplay(s, "board");
    expect(d.color).toBe("var(--cs-tag-text)");
    expect(d.icon).toBe("Tag");
  });

  it("uses neutral text-tag fallback for an unknown tag (no TagDef)", () => {
    const s = schema([{ name: "known", type: "select", allowedValues: ["a"] }]);
    const d = tagDisplay(s, "mystery");
    expect(d.color).toBe("var(--cs-tag-text)");
    expect(d.icon).toBe("Tag");
    expect(d.label).toBe("Mystery");
  });

  it("uses only a --cs-* token color (no hex literal) for every default", () => {
    const types = [
      "text",
      "select",
      "multi_select",
      "ordered",
      "status",
      "date",
      "relation",
    ] as const;
    for (const type of types) {
      const d = tagDisplay(schema([{ name: "t", type, allowedValues: [] }]), "t");
      expect(d.color, `${type} default color must be a --cs-* token`).toMatch(/^var\(--cs-tag-/);
      expect(d.icon.length, `${type} must have a default icon`).toBeGreaterThan(0);
    }
  });
});
