// SPDX-License-Identifier: MIT
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
process.argv.push("--test-import");
const { renderRegistry, staleOutputs, schemaType } = await import("./generate-verb-registry.mjs");
const root = new URL("../", import.meta.url).pathname;
test("a registered fixture emits TS names and contract rows; stale output is rejected", () => {
  const outputs = renderRegistry([
    {
      name: "records.fixture",
      class: "read",
      confirmable: false,
      action: false,
      mcpTwin: "read_fixture",
      grant: "context:read",
      inboxTarget: true,
    },
  ]);
  assert.match(outputs["src/features/dashboard/lib/verb-registry.generated.ts"], /records.fixture/);
  assert.match(outputs["src/features/dashboard/lib/verb-names.generated.d.ts"], /records.fixture/);
  assert.match(
    outputs["src/features/dashboard/lib/verb-registry.generated.ts"],
    /"inboxTarget": true/,
  );
  assert.match(
    outputs["codesense-docs/getting-started/content/generated/dashboard-verbs.generated.md"],
    /\| records.fixture \| read \| context:read \| no \| read_fixture \|/,
  );
  const dts = outputs["src-tauri/src/http/routes/dashboard_kit/codesense.d.ts"];
  assert.match(dts, /"records\.fixture"\(args\?: unknown\): Promise<unknown>;/);
  assert.match(
    outputs["src-tauri/src/http/routes/verb_methods.generated.js"],
    /\["records.fixture"\]/,
  );
  assert.equal(staleOutputs(outputs, () => "old").length, 5);
  assert.deepEqual(
    staleOutputs(outputs, (path) => outputs[path]),
    [],
  );
});

test("schemas become types; generic ones stay Record<string, unknown> and unknown", () => {
  assert.equal(
    schemaType({ type: "object", properties: {}, additionalProperties: true }),
    "Record<string, unknown>",
  );
  assert.equal(schemaType({ description: "host result" }), "unknown");
  assert.equal(schemaType({ type: ["string", "null"] }), "string | null");
  assert.equal(schemaType({ enum: ["a", "b"] }), '"a" | "b"');
  assert.equal(
    schemaType({ type: "array", items: { type: "integer" }, minItems: 2, maxItems: 2 }),
    "[number, number]",
  );
  assert.equal(
    schemaType({
      type: "object",
      properties: { a: { type: "string" }, b: { const: true } },
      required: ["a"],
    }),
    "{\n  a: string;\n  b?: true;\n  [key: string]: unknown;\n}",
  );
});

test("the committed generated files match the registry", () => {
  const specs = JSON.parse(
    readFileSync(resolve(root, "src-tauri/src/verbs/registry.generated.json"), "utf8"),
  );
  assert.deepEqual(
    staleOutputs(renderRegistry(specs), (path) => readFileSync(resolve(root, path), "utf8")),
    [],
    "run pnpm gen:verbs",
  );
});
