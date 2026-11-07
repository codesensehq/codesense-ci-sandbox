// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { buildHeadingPickerItems } from "./heading-picker-items";
import type { Heading } from "./note-headings";
import { fuzzyScore } from "@/shared/lib/fuzzy-match";

function h(level: number, text: string, pos: number, slug: string): Heading {
  return { level, text, pos, slug };
}

describe("buildHeadingPickerItems (SW-SN-6)", () => {
  it("maps headings to picker items in document order", () => {
    const headings = [
      h(1, "Intro", 0, "intro"),
      h(2, "Setup", 10, "setup"),
      h(2, "Usage", 20, "usage"),
    ];
    const items = buildHeadingPickerItems(headings);
    expect(items.map((i) => i.label)).toEqual(["Intro", "Setup", "Usage"]);
    expect(items.map((i) => i.id)).toEqual(["intro", "setup", "usage"]);
  });

  it("puts the heading text in `label` (the searched field) and leaves `sublabel` empty", () => {
    const [item] = buildHeadingPickerItems([
      h(2, "TLS handshake timing", 5, "tls-handshake-timing"),
    ]);
    expect(item.label).toBe("TLS handshake timing");
    // Do NOT duplicate the text into sublabel — the dialog scores max(label,
    // sublabel), so the searched text belongs in label only.
    expect(item.sublabel).toBe("");
  });

  it("uses the heading slug as the id (unique-within-note, safe as key)", () => {
    // enumerateHeadings de-dupes repeated text with an occurrence ordinal, so two
    // "Notes" headings arrive with distinct slugs — the adapter preserves them.
    const items = buildHeadingPickerItems([
      h(2, "Notes", 5, "notes"),
      h(2, "Notes", 30, "notes-2"),
    ]);
    expect(items.map((i) => i.id)).toEqual(["notes", "notes-2"]);
    expect(new Set(items.map((i) => i.id)).size).toBe(2);
  });

  it("produces labels that fuzzy-match a heading-name query", () => {
    const items = buildHeadingPickerItems([h(2, "TLS handshake timing", 5, "tls")]);
    // The dialog searches `label`; a name fragment must score > 0.
    expect(fuzzyScore("tls", items[0].label, { filenameFromPath: true })).toBeGreaterThan(0);
  });

  it("returns an empty list for a note with no headings", () => {
    expect(buildHeadingPickerItems([])).toEqual([]);
  });
});
