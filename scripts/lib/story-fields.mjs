// SPDX-License-Identifier: MIT
// Reads the story-field registry (`STORY_FIELDS`) out of the app source. The one
// reader behind the shipped story-fields page, the site's story-fields reference
// and the site's live-data-contract section, so no copy of a field's description
// lives outside the registry.
import { productionPart, stringLiterals, strArrayConst, structTableConst } from "./rust.mjs";

export const STORY_FIELDS_SOURCE = "src-tauri/src/commands/pipeline_format/fields.rs";
export const PIPELINE_FORMAT_SOURCE = "src-tauri/src/commands/pipeline_format/mod.rs";

const ROLES = new Set(["Typed", "Open", "Retired", "Tolerated"]);
const ROW_KEYS = ["yaml", "cs_meta", "kind", "description", "enum_values", "required", "role"];
const KINDS = { Text: "text", List: "list", Number: "number" };

function variant(raw, prefix, allowed) {
  const m = new RegExp(`^${prefix}::(\\w+)$`).exec(raw ?? "");
  if (!m || !allowed(m[1])) throw new Error(`story-fields: unexpected ${prefix} value \`${raw}\``);
  return m[1];
}

function enumValues(raw, app) {
  const text = raw.trim();
  if (text.startsWith("&[")) return stringLiterals(text);
  const name = /^(?:super::)?([A-Z_]+)$/.exec(text);
  if (!name) throw new Error(`story-fields: can't read enum_values \`${text}\``);
  return strArrayConst(productionPart(app.file(PIPELINE_FORMAT_SOURCE)), name[1]);
}

/** Every registry row, in registry order. `app.file(path)` returns a source file's text. */
export function readStoryFields(app) {
  const rows = structTableConst(productionPart(app.file(STORY_FIELDS_SOURCE)), "STORY_FIELDS");
  if (!rows.length) throw new Error("story-fields: STORY_FIELDS is empty");
  return rows.map((row) => {
    for (const key of ROW_KEYS) {
      if (row[key] === undefined) throw new Error(`story-fields: row ${row.yaml} has no ${key}`);
    }
    const extra = Object.keys(row).filter((key) => !ROW_KEYS.includes(key));
    if (extra.length)
      throw new Error(`story-fields: row ${row.yaml} has unknown ${extra.join(", ")}`);
    if (row.cs_meta !== `cs:${row.yaml.replaceAll("_", "-")}`) {
      throw new Error(`story-fields: row ${row.yaml} has cs_meta \`${row.cs_meta}\``);
    }
    if (row.required !== "true" && row.required !== "false") {
      throw new Error(`story-fields: row ${row.yaml} has required \`${row.required}\``);
    }
    return {
      yaml: row.yaml,
      csMeta: row.cs_meta,
      kind: KINDS[variant(row.kind, "FieldKind", (k) => k in KINDS)],
      description: row.description,
      enumValues: enumValues(row.enum_values, app),
      required: row.required === "true",
      role: variant(row.role, "FieldRole", (r) => ROLES.has(r)),
    };
  });
}

/** The rows `get_story_format` publishes (METADATA_CONTRACT_FIELDS), in order. */
export function contractFields(rows) {
  return rows.filter((row) => row.role === "Typed" || row.role === "Open");
}

/** A row as the app serves it (`StoryField::contract_entry`). */
export function contractEntry(row) {
  return {
    name: row.yaml,
    cs_meta: row.csMeta,
    kind: row.kind,
    required: row.required,
    description: row.description,
    ...(row.enumValues.length ? { values: row.enumValues } : {}),
  };
}
