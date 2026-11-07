// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { classifyDocType } from "./classify-doc-type";
import { DOC_TYPE_ICON } from "../pipeline-graph/doc-type-icon";

describe("classifyDocType", () => {
  it("prefers a known documentType", () => {
    expect(classifyDocType("a.md", "story")).toBe("story");
    expect(classifyDocType("a.md", "bug")).toBe("bug");
    expect(classifyDocType("a.md", "spike")).toBe("spike");
    expect(classifyDocType("a.md", "retro")).toBe("retro");
  });

  it("falls back to extension for non-story files", () => {
    expect(classifyDocType("architecture.md")).toBe("doc");
    expect(classifyDocType("notes.html")).toBe("doc");
    expect(classifyDocType("spec.pdf")).toBe("doc");
    expect(classifyDocType("mockup.png")).toBe("image");
    expect(classifyDocType("diagram.excalidraw")).toBe("canvas");
  });

  it("ignores an unknown documentType and uses the extension", () => {
    expect(classifyDocType("thing.png", "totally-unknown")).toBe("image");
  });

  it("defaults to doc for an unknown/absent extension", () => {
    expect(classifyDocType("Makefile")).toBe("doc");
    expect(classifyDocType("data.bin")).toBe("doc");
  });

  it("only ever returns a key that exists in DOC_TYPE_ICON", () => {
    const samples = ["a.md", "b.png", "c.excalidraw", "d.pdf", "Makefile", "e.bin"];
    for (const s of samples) {
      expect(DOC_TYPE_ICON).toHaveProperty(classifyDocType(s));
    }
    for (const t of ["story", "bug", "spike", "doc", "retro", "canvas", "image"]) {
      expect(DOC_TYPE_ICON).toHaveProperty(classifyDocType("x.md", t));
    }
  });
});
