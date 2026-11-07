// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { isPipelineFileName, PIPELINE_EXTENSIONS } from "./pipeline-file-filter";
import { stripPipelineExtension } from "./pipeline-utils";

describe("isPipelineFileName", () => {
  it("accepts .md files", () => {
    expect(isPipelineFileName("30-7-directory-derived.md")).toBe(true);
    expect(isPipelineFileName("story.md")).toBe(true);
  });

  it("accepts .html and .htm files", () => {
    expect(isPipelineFileName("1-alpha.html")).toBe(true);
    expect(isPipelineFileName("2-beta.htm")).toBe(true);
  });

  it("accepts case-variant extensions", () => {
    expect(isPipelineFileName("story.MD")).toBe(true);
    expect(isPipelineFileName("story.Md")).toBe(true);
    expect(isPipelineFileName("story.HTML")).toBe(true);
    expect(isPipelineFileName("story.HTM")).toBe(true);
  });

  it("rejects README and index (case-insensitive, any pipeline extension)", () => {
    expect(isPipelineFileName("README.md")).toBe(false);
    expect(isPipelineFileName("readme.md")).toBe(false);
    expect(isPipelineFileName("README.MD")).toBe(false);
    expect(isPipelineFileName("readme.html")).toBe(false);
    expect(isPipelineFileName("README.htm")).toBe(false);
    expect(isPipelineFileName("index.md")).toBe(false);
    expect(isPipelineFileName("INDEX.md")).toBe(false);
    expect(isPipelineFileName("index.html")).toBe(false);
    expect(isPipelineFileName("INDEX.HTM")).toBe(false);
  });

  it("rejects non-pipeline extensions", () => {
    expect(isPipelineFileName("story.txt")).toBe(false);
    expect(isPipelineFileName("story")).toBe(false);
    expect(isPipelineFileName("pipeline.json")).toBe(false);
    expect(isPipelineFileName("image.png")).toBe(false);
  });
});

describe("PIPELINE_EXTENSIONS", () => {
  it("contains expected extensions", () => {
    expect(PIPELINE_EXTENSIONS.has("md")).toBe(true);
    expect(PIPELINE_EXTENSIONS.has("html")).toBe(true);
    expect(PIPELINE_EXTENSIONS.has("htm")).toBe(true);
  });

  it("does not contain viewer-only extensions", () => {
    expect(PIPELINE_EXTENSIONS.has("txt")).toBe(false);
    expect(PIPELINE_EXTENSIONS.has("json")).toBe(false);
  });
});

describe("stripPipelineExtension", () => {
  it("strips .md", () => {
    expect(stripPipelineExtension("story.md")).toBe("story");
  });

  it("strips .html", () => {
    expect(stripPipelineExtension("story.html")).toBe("story");
  });

  it("strips .htm", () => {
    expect(stripPipelineExtension("story.htm")).toBe("story");
  });

  it("is case-insensitive", () => {
    expect(stripPipelineExtension("story.MD")).toBe("story");
    expect(stripPipelineExtension("story.HTML")).toBe("story");
  });

  it("leaves non-pipeline extensions alone", () => {
    expect(stripPipelineExtension("story.txt")).toBe("story.txt");
    expect(stripPipelineExtension("data.json")).toBe("data.json");
  });

  it("handles no extension", () => {
    expect(stripPipelineExtension("story")).toBe("story");
  });

  it("handles empty string", () => {
    expect(stripPipelineExtension("")).toBe("");
  });

  it("strips only the last extension", () => {
    expect(stripPipelineExtension("my.md.file.md")).toBe("my.md.file");
  });
});
