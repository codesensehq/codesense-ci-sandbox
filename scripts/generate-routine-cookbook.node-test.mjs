// SPDX-License-Identifier: MIT
import { test } from "node:test";
import assert from "node:assert/strict";
import { isDeepStrictEqual } from "node:util";
import { readFileSync } from "node:fs";
import { generate } from "./generate-routine-cookbook.mjs";
const content = new URL("../codesense-docs/getting-started/content/", import.meta.url);

test("published MCP calls are the exact shared create dry-run inputs", () => {
  const { examples } = JSON.parse(
    readFileSync(new URL("examples/routines/cookbook.json", content), "utf8"),
  );
  const pages = ["trigger-cookbook", "command-routines"]
    .map((name) => readFileSync(new URL(`arc-g-routines/${name}.html`, content), "utf8"))
    .join("\n");
  const calls = [...pages.matchAll(/<code class="language-json">([\s\S]*?)<\/code>/g)].map(
    (match) =>
      JSON.parse(match[1].replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&")),
  );
  for (const example of examples)
    assert.ok(
      calls.some((call) => isDeepStrictEqual(call, example.mcp)),
      example.kind,
    );
  assert.equal(examples.filter((e) => e.kind !== "command").length, 6);
});

test("the committed cards and command chain have no fixture drift", () => {
  assert.doesNotThrow(() => generate(true));
});
