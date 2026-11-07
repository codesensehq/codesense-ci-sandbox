// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import {
  formatStoryDisplayTitle,
  humanizeStoryStem,
  resolveDeclaredStoryTitle,
  storyMetadataTitle,
} from "./story-display-title";

describe("humanizeStoryStem", () => {
  it("drops a derived id prefix and turns dashes into words", () => {
    expect(humanizeStoryStem("ll-214-refund-endpoint-idempotency-keys")).toBe(
      "Refund endpoint idempotency keys",
    );
  });

  it("drops a multi-segment id prefix", () => {
    expect(humanizeStoryStem("SW-PL-22-story-titles-on-cards")).toBe("Story titles on cards");
  });

  it("uses the explicit story id to strip the prefix", () => {
    expect(humanizeStoryStem("30-5-pipeline-interactivity", "30-5")).toBe("Pipeline interactivity");
  });

  it("humanizes a stem without an id", () => {
    expect(humanizeStoryStem("release_notes-draft")).toBe("Release notes draft");
  });

  it("returns empty when the stem is only an id", () => {
    expect(humanizeStoryStem("LL-214")).toBe("");
  });
});

describe("formatStoryDisplayTitle", () => {
  // SW-LN-34: the metadata title wins over the body heading and the stem.
  it("prefers the metadata title over the body heading", () => {
    expect(
      formatStoryDisplayTitle({
        storyId: "SW-LN-34",
        titleMeta: "Story details read frontmatter title",
        heading: "Some H1",
        fileName: "SW-LN-34-rust-reads-frontmatter-title.md",
      }),
    ).toBe("SW-LN-34 — Story details read frontmatter title");
  });

  it("drops a leading repeat of the id from the metadata title", () => {
    expect(
      formatStoryDisplayTitle({
        storyId: "SW-LN-34",
        titleMeta: "SW-LN-34: Frontmatter titles",
        fileName: "SW-LN-34-x.md",
      }),
    ).toBe("SW-LN-34 — Frontmatter titles");
  });

  it("falls back to the heading when the metadata title is only the id", () => {
    expect(
      formatStoryDisplayTitle({
        storyId: "SW-LN-34",
        titleMeta: "SW-LN-34",
        heading: "Frontmatter titles",
        fileName: "SW-LN-34-x.md",
      }),
    ).toBe("SW-LN-34 — Frontmatter titles");
  });

  it("falls back to the heading when the metadata title is blank", () => {
    expect(
      formatStoryDisplayTitle({ titleMeta: "  ", heading: "Heading", fileName: "ll-9-x.md" }),
    ).toBe("LL-9 — Heading");
  });

  it("falls back to id + humanized stem when there is no heading", () => {
    expect(
      formatStoryDisplayTitle({ fileName: "ll-214-refund-endpoint-idempotency-keys.md" }),
    ).toBe("LL-214 — Refund endpoint idempotency keys");
  });

  it("prefers the frontmatter story id over the stem-derived one", () => {
    expect(formatStoryDisplayTitle({ storyId: "W-5", fileName: "w-5-source-repo.md" })).toBe(
      "W-5 — Source repo",
    );
  });

  it("prefers the heading over the humanized stem", () => {
    expect(
      formatStoryDisplayTitle({
        storyId: "LL-214",
        heading: "Idempotent refunds",
        fileName: "ll-214-refund-endpoint-idempotency-keys.md",
      }),
    ).toBe("LL-214 — Idempotent refunds");
  });

  it.each([
    "LL-214: Idempotent refunds",
    "LL-214 — Idempotent refunds",
    "Story LL-214: Idempotent refunds",
    "ll-214 - Idempotent refunds",
  ])("does not double the id when the heading already carries it (%s)", (heading) => {
    expect(formatStoryDisplayTitle({ storyId: "LL-214", heading, fileName: "ll-214-x.md" })).toBe(
      "LL-214 — Idempotent refunds",
    );
  });

  it("shows the title alone when there is no id", () => {
    expect(formatStoryDisplayTitle({ heading: "Onboarding notes", fileName: "notes.html" })).toBe(
      "Onboarding notes",
    );
    expect(formatStoryDisplayTitle({ fileName: "onboarding-notes.html" })).toBe("Onboarding notes");
  });

  it("shows the id alone when nothing else is left", () => {
    expect(formatStoryDisplayTitle({ fileName: "LL-214.md" })).toBe("LL-214");
  });

  it("ignores a blank heading", () => {
    expect(formatStoryDisplayTitle({ heading: "   ", fileName: "ll-9-fix-it.md" })).toBe(
      "LL-9 — Fix it",
    );
  });
});

describe("storyMetadataTitle / resolveDeclaredStoryTitle (SW-LN-B42)", () => {
  it("reads the scan's metadata title from openTags", () => {
    expect(storyMetadataTitle({ openTags: { title: ["  API health  "] } })).toBe("API health");
    expect(storyMetadataTitle({ openTags: { title: ["  "] } })).toBeUndefined();
    expect(storyMetadataTitle({ openTags: null })).toBeUndefined();
  });

  it("prefers the metadata title over the heading and drops a repeated id", () => {
    expect(
      resolveDeclaredStoryTitle({ storyId: "TR-1", titleMeta: "TR-1: API health", heading: "H1" }),
    ).toBe("API health");
    expect(resolveDeclaredStoryTitle({ storyId: "TR-1", heading: "H1" })).toBe("H1");
    expect(resolveDeclaredStoryTitle({ storyId: "TR-1" })).toBeUndefined();
  });
});
