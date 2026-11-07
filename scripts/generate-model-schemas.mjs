#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The Rust exporter is the only writer of the fixture. Never rehash a stale fixture here.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = "codesense-docs/getting-started/content";
export function verifySources(fixture, readSource = (path) => readFileSync(join(ROOT, path))) {
  for (const [path, expected] of Object.entries(fixture.sources)) {
    const actual = createHash("sha256").update(readSource(path)).digest("hex");
    if (actual !== expected)
      throw new Error(
        `Model schema fixture is stale: ${path}. Export with UPDATE_DOCS_SCHEMAS=1 cargo test -p codesense --lib docs_schemas::export_model_schemas, then pnpm gen:docs. Do not rehash the fixture.`,
      );
  }
}
function deref(schema, root) {
  return schema.$ref ? root.$defs[schema.$ref.split("/").at(-1)] : schema;
}
export function schemaFields(root) {
  const rows = [];
  function visit(raw, name, required, seen = new Set()) {
    const schema = deref(raw, root);
    const choices = schema.anyOf ?? schema.oneOf;
    const defaultValue = Object.hasOwn(raw, "default") ? raw.default : schema.default;
    const types =
      schema.type ??
      (schema.enum
        ? "enum"
        : choices
          ? choices.flatMap((s) => deref(s, root).type ?? "object")
          : "any");
    rows.push({
      name,
      type: Array.isArray(types) ? [...new Set(types)].join(" | ") : types,
      required,
      default: defaultValue === undefined ? null : JSON.stringify(defaultValue),
      allowed:
        schema.enum ??
        (choices?.every((part) => Object.hasOwn(deref(part, root), "const"))
          ? [...new Set(choices.map((part) => deref(part, root).const))]
          : null),
      pattern: schema.pattern ?? null,
      description: raw.description ?? schema.description ?? "",
      sourceRef: null,
    });
    if (seen.has(schema)) return;
    const nextSeen = new Set([...seen, schema]);
    for (const part of choices ?? [schema]) {
      const object = deref(part, root);
      for (const [key, value] of Object.entries(object.properties ?? {}))
        visit(value, `${name}.${key}`, required && (object.required ?? []).includes(key), nextSeen);
      if (object.items) visit(object.items, `${name}[]`, false, nextSeen);
      if (
        object.additionalProperties &&
        typeof object.additionalProperties === "object" &&
        Object.keys(object.additionalProperties).length
      )
        visit(object.additionalProperties, `${name}.*`, false, nextSeen);
    }
  }
  for (const [key, value] of Object.entries(root.properties))
    visit(value, key, (root.required ?? []).includes(key));
  return rows.filter((r, i) => rows.findIndex((other) => other.name === r.name) === i);
}
export function modelSchemas(root = ROOT) {
  const fixture = JSON.parse(
    readFileSync(join(root, "src-tauri/src/docs_schemas.generated.json"), "utf8"),
  );
  verifySources(fixture, (path) => readFileSync(join(root, path)));
  return Object.fromEntries(
    Object.entries(fixture.schemas).map(([slug, schema]) => [
      slug,
      { schema, fields: schemaFields(schema) },
    ]),
  );
}
function output(path, content, check) {
  if (check) {
    if (readFileSync(path, "utf8") !== content)
      throw new Error(`${path} is stale; run pnpm gen:docs`);
  } else {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, content);
  }
}
export function generate(check = false) {
  const models = modelSchemas();
  for (const [slug, { schema }] of Object.entries(models))
    output(
      join(ROOT, CONTENT, "generated/schemas", `${slug}.schema.json`),
      JSON.stringify(schema, null, 2) + "\n",
      check,
    );
  const page = join(ROOT, CONTENT, "arc-g-routines/routine-yml.html");
  const current = readFileSync(page, "utf8");
  const esc = (s) =>
    String(s)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;");
  const rows = models["routine-yml"].fields
    .map(
      (f) =>
        `<tr><td><code>${esc(f.name)}</code></td><td>${esc(f.type)}</td><td>${f.required ? "required" : "optional"}</td><td>${esc(f.description)}</td></tr>`,
    )
    .join("\n");
  const updated = current.replace(
    /<!-- model-fields:start -->[\s\S]*?<!-- model-fields:end -->/,
    `<!-- model-fields:start -->\n${rows}\n<!-- model-fields:end -->`,
  );
  if (!current.includes("<!-- model-fields:start -->"))
    throw new Error("routine.yml page is missing model field markers");
  output(page, updated, check);
  console.log(`generate-model-schemas: ${check ? "ok" : "generated serde schemas and field rows"}`);
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  generate(process.argv.includes("--check"));
