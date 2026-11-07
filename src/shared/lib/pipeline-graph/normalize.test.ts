// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { normalizeScopePath, PipelinePathError } from "./normalize";

// Shared test fixture — duplicated on the Rust side to enforce rule parity
// across languages. See src-tauri/src/commands/pipeline_scanner.rs.
const VALID_CASES: Array<[string, string]> = [
  ["", ""],
  ["implementation-artifacts", "implementation-artifacts"],
  ["  implementation-artifacts  ", "implementation-artifacts"],
  ["/implementation-artifacts", "implementation-artifacts"],
  ["implementation-artifacts/", "implementation-artifacts"],
  ["/implementation-artifacts/", "implementation-artifacts"],
  ["//implementation-artifacts//", "implementation-artifacts"],
  ["\\implementation-artifacts\\", "implementation-artifacts"],
  ["in-progress/30-pipeline-viewer-ui", "in-progress/30-pipeline-viewer-ui"],
  ["/in-progress/30-pipeline-viewer-ui/", "in-progress/30-pipeline-viewer-ui"],
  ["   ", ""],
  ["/", ""],
];

const INVALID_CASES: string[] = ["..", "../foo", "foo/..", "foo/../bar", "./../baz"];

// SW-FX-56: `normalizePipelineRoot` deleted (its only caller, `setPipelineRoot`,
// was removed). `normalizeScopePath` exercises the shared `normalize` rules.
describe("normalizeScopePath", () => {
  it.each(VALID_CASES)("normalizes %j to %j", (input, expected) => {
    expect(normalizeScopePath(input)).toBe(expected);
  });

  it.each(INVALID_CASES)("rejects %j with PipelinePathError", (input) => {
    expect(() => normalizeScopePath(input)).toThrow(PipelinePathError);
  });

  it("error message mentions scopePath field", () => {
    expect(() => normalizeScopePath("foo/..")).toThrow(/scopePath/);
  });
});
