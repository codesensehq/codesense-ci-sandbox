// SPDX-License-Identifier: MIT
import ts from "typescript";
import { conditionalVerbGrants, describeConditionalGrant } from "./verb-grants.mjs";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  parseItems as parseRustItems,
  numConst,
  strArrayConst,
  stringLiterals,
  fnBody,
  matchBracket,
  splitTopLevel,
  productionPart,
} from "./rust.mjs";
import { buildPageSdk, pageSdkMarkdown } from "./page-sdk.mjs";
export const GENERATED = "codesense-docs/getting-started/content/generated/";
export const json = (value) => JSON.stringify(value, null, 2) + "\n";
export function sourceTree(root) {
  return { file: (path) => readFileSync(resolve(root, path), "utf8") };
}
function tracked(app) {
  const sources = {};
  return {
    sources,
    file(path) {
      const text = app.file(path);
      sources[path] = createHash("sha256").update(text).digest("hex");
      return text;
    },
  };
}
const table = (headers, rows) =>
  `| ${headers.join(" | ")} |\n| ${headers.map(() => "---").join(" | ")} |\n` +
  rows
    .map(
      (row) =>
        "| " +
        row
          .map((c) =>
            String(c ?? "")
              .replaceAll("|", "\\|")
              .replaceAll("\n", " "),
          )
          .join(" | ") +
        " |",
    )
    .join("\n") +
  "\n";
const ref = (name, model, md) => ({
  [GENERATED + name + ".json"]: json(model),
  [GENERATED + name + ".md"]: md,
});
export function pageSdkOutputs(app) {
  const input = tracked(app),
    { declaration, ...model } = buildPageSdk(input, ts);
  return {
    ...ref(
      "page-sdk",
      { ...model, sources: input.sources },
      pageSdkMarkdown({ ...model, declaration }),
    ),
    [GENERATED + "codesense.d.ts"]: declaration,
  };
}
function parseItems(source) {
  const items = parseRustItems(source);
  const production = productionPart(source);
  for (const item of items.values()) {
    item.customSerialize = new RegExp(
      `impl(?:<[^>]*>)?\\s+(?:[\\w:]+::)?Serialize\\s+for\\s+${item.name}\\b`,
    ).test(production);
  }
  return items;
}
function rustSchema(items, name, seen = new Set()) {
  if (seen.has(name)) return { $ref: `#/$defs/${name}` };
  const item = items.get(name);
  if (!item) return { description: `untyped (Value): ${name}`, rustType: name };
  if (item.customSerialize)
    return {
      description: `untyped (Value): ${name} has a custom Serialize implementation; variant names do not define its wire values.`,
      rustType: name,
    };
  if (item.kind === "enum") return { type: "string", enum: item.members.map((m) => m.name) };
  const next = new Set([...seen, name]),
    properties = {},
    required = [];
  for (const field of item.members) {
    if (field.serde.skip) continue;
    if (field.serde.flatten) {
      const flattened = rustSchema(items, field.type.replace(/^super::/, ""), next);
      Object.assign(properties, flattened.properties);
      required.push(...(flattened.required ?? []));
      continue;
    }
    const schema = rustType(items, field.type, next);
    if (field.docs.length)
      schema.description = [schema.description, field.docs.join(" ")].filter(Boolean).join(" ");
    if (field.serde.default !== undefined || item.serde.default !== undefined) {
      if (field.type.startsWith("Vec<")) schema.default = [];
      else if (field.type === "bool") schema.default = false;
      else if (field.type.startsWith("Option<")) schema.default = null;
    }
    properties[field.name] = schema;
    if (!field.optional) required.push(field.name);
  }
  return { type: "object", properties, required };
}
function rustType(items, type, seen = new Set()) {
  const option = /^Option<(.+)>$/.exec(type);
  if (option) return { anyOf: [rustType(items, option[1], seen), { type: "null" }] };
  const vector = /^Vec<(.+)>$/.exec(type);
  if (vector) return { type: "array", items: rustType(items, vector[1], seen) };
  if (type === "String" || type === "str") return { type: "string" };
  if (type === "bool") return { type: "boolean" };
  if (/^[ui](\d+|size)$/.test(type))
    return { type: "integer", ...(type.startsWith("u") ? { minimum: 0 } : {}) };
  if (/^f\d+$/.test(type)) return { type: "number" };
  return rustSchema(items, type, seen);
}
export function declarationOutputs(app) {
  const input = tracked(app),
    items = new Map([
      ...parseItems(input.file("src-tauri/src/panes/mod.rs")),
      ...parseItems(input.file("src-tauri/src/panes/manifest.rs")),
    ]);
  const defs = Object.fromEntries(
    [
      "DashboardDeclaration",
      "DashboardCommand",
      "CommandScope",
      "PaneManifestEntry",
      "PaneManifest",
    ].map((name) => [name, rustSchema(items, name)]),
  );
  defs.PaneManifest.properties.version.const = numConst(
    input.file("src-tauri/src/panes/manifest.rs"),
    "MANIFEST_VERSION",
  );
  const model = {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    title: "Pane manifest",
    ...defs.PaneManifest,
    $defs: defs,
    sources: input.sources,
  };
  const declarations = items.get("DashboardDeclaration").members.filter((f) => !f.serde.skip);
  const htmlTags = new Set(
    [
      ...fnBody(input.file("src-tauri/src/panes/mod.rs"), "from_html").matchAll(
        /"(codesense:[^"]+)"/g,
      ),
    ].map((m) => m[1]),
  );
  const md =
    "# Pane declarations\n\nGenerated from panes/mod.rs and manifest.rs. Manifest declarations are flattened into each panes entry. Missing declaration fields use their serde defaults. Commands require id, label and scope. HTML and YAML declarations must agree when both are present.\n\n[JSON Schema](pane-declaration.schema.json) · [SDK typings](codesense.d.ts) · [Capabilities](capabilities.md) · [Slots](slots.md)\n\n" +
    table(
      ["Field", "HTML meta tag (content)", "YAML entry", "Default"],
      declarations.map((f) => [
        f.name,
        htmlTags.has(`codesense:${f.name}`)
          ? `codesense:${f.name} (${f.name === "commands" ? "JSON array" : f.type.startsWith("Vec<") ? "comma or whitespace separated" : "string"})`
          : "YAML only",
        f.name,
        JSON.stringify(defs.DashboardDeclaration.properties[f.name].default ?? null),
      ]),
    ) +
    "\n## YAML manifest entry\n\n" +
    table(
      ["Field", "Type", "Required", "Default"],
      Object.entries(defs.PaneManifestEntry.properties).map(([key, val]) => [
        key,
        val.type ?? JSON.stringify(val.anyOf),
        defs.PaneManifestEntry.required.includes(key),
        JSON.stringify(val.default ?? "—"),
      ]),
    ) +
    "\n## Commands (JSON in meta; objects in YAML)\n\n" +
    table(
      ["Field", "Type", "Required"],
      Object.entries(defs.DashboardCommand.properties).map(([key, val]) => [
        key,
        val.enum?.join(", ") ?? val.type ?? JSON.stringify(val.anyOf),
        defs.DashboardCommand.required.includes(key),
      ]),
    ) +
    '\n```html\n<meta name="codesense:slots" content="right-pane">\n<meta name="codesense:capabilities" content="sessions:read">\n<meta name="codesense:surface" content="notes">\n<meta name="codesense:commands" content=\'[ {"id":"refresh","label":"Refresh","scope":"slot-visible"} ]\'>\n```\n\n```yaml\nversion: 1\npanes:\n  - id: example\n    html: example.html\n    slots: [right-pane]\n    capabilities: [sessions:read]\n    surface: notes\n    commands:\n      - id: refresh\n        label: Refresh\n        scope: slot-visible\n```\n';
  return {
    [GENERATED + "pane-declaration.schema.json"]: json(model),
    [GENERATED + "pane-declarations.md"]: md,
  };
}
export function capabilityModel(app) {
  const src = app.file("src-tauri/src/verbs/capabilities.rs"),
    verbs = JSON.parse(app.file("src-tauri/src/verbs/registry.generated.json"));
  const conditional = conditionalVerbGrants(app, verbs);
  const verbsFor = (name) =>
    verbs
      .filter(
        (verb) =>
          verb.grant === name || conditional[verb.name].some((rule) => rule.capability === name),
      )
      .map((verb) => verb.name);
  const all = strArrayConst(src, "ALL"),
    team = strArrayConst(src, "TEAM_EXCLUDED"),
    message = strArrayConst(src, "MESSAGE_EXCLUDED");
  const records = fnBody(src, "records_allows");
  const strings = (expression) => [...expression.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  const restricted = strings(records.match(/matches!\(capability,([\s\S]*?)\)/)[1]);
  const deniedSlots = strings(records.match(/matches!\(slot,([\s\S]*?)\)/)[1]);
  const deniedScopes = strings(records.match(/matches!\(scope,([\s\S]*?)\)/)[1]);
  const slotCeilings = Object.fromEntries(
    strArrayConst(src, "SLOTS").map((slot) => [
      slot,
      all.filter(
        (cap) =>
          !(slot === "dock" && team.includes(cap)) &&
          !(restricted.includes(cap) && deniedSlots.includes(slot)),
      ),
    ]),
  );
  return {
    count: all.length,
    capabilities: all.map((name) => ({
      name,
      teamSafe: !team.includes(name),
      messageSafe: !team.includes(name) && !message.includes(name),
      verbs: verbsFor(name),
      conditionalVerbs: verbs.flatMap((verb) =>
        conditional[verb.name]
          .filter((rule) => rule.capability === name)
          .map((rule) => ({ verb: verb.name, ...rule })),
      ),
    })),
    teamExcluded: team,
    messageExcluded: message,
    slotCeilings,
    privateRecords: {
      capabilities: restricted,
      excludedSlots: deniedSlots,
      excludedScopes: deniedScopes,
    },
    policySource: {
      scopeAllows: fnBody(src, "scope_allows"),
      slotAllows: fnBody(src, "slot_allows"),
      recordsAllows: records,
    },
  };
}
export function capabilityOutputs(app) {
  const input = tracked(app),
    model = capabilityModel(input);
  return ref(
    "capabilities",
    { ...model, sources: input.sources },
    "# Capabilities\n\n" +
      `${model.count} grantable capabilities. Under team-safe, declarations are intersected with the slot and scope ceilings. The default open policy grants full reach. Registry grants are defaults. Conditional grants override them when the topic, caller scope or bound collection scope matches; all admitted verbs below include those conditional paths. Frame ceilings can further restrict reach.\n\n` +
      table(
        [
          "Capability",
          "Team-safe team",
          "Team-safe message",
          "Admitted verbs",
          "Conditional admission",
        ],
        model.capabilities.map((c) => [
          c.name,
          c.teamSafe,
          c.messageSafe,
          c.verbs.join(", ") || "none in page registry",
          c.conditionalVerbs
            .map((rule) => `${rule.verb}: ${describeConditionalGrant(rule)}`)
            .join("; "),
        ]),
      ) +
      "\n## Slot ceilings\n\n" +
      table(
        ["Slot", "Capabilities"],
        Object.entries(model.slotCeilings).map(([slot, caps]) => [slot, caps.join(", ")]),
      ) +
      `\nPrivate-record capabilities ${model.privateRecords.capabilities.join(", ")} are excluded from slots ${model.privateRecords.excludedSlots.join(", ")} and scopes ${model.privateRecords.excludedScopes.join(", ")} under team-safe. Reads and subscriptions also resolve own/team reach from the bound collection.\n`,
  );
}
/** Evaluate only literal TypeScript data and references to it, never source code. */
export function tsConstants(source, external = {}) {
  const file = ts.createSourceFile("source.ts", source, ts.ScriptTarget.Latest, true),
    defs = new Map();
  function visit(node) {
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer)
      defs.set(node.name.text, node.initializer);
    ts.forEachChild(node, visit);
  }
  visit(file);
  function evaluate(node) {
    if (
      ts.isAsExpression(node) ||
      ts.isSatisfiesExpression(node) ||
      ts.isParenthesizedExpression(node)
    )
      return evaluate(node.expression);
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
    if (ts.isNumericLiteral(node)) return Number(node.text);
    if (node.kind === ts.SyntaxKind.TrueKeyword) return true;
    if (node.kind === ts.SyntaxKind.FalseKeyword) return false;
    if (ts.isArrayLiteralExpression(node)) return node.elements.map(evaluate);
    if (ts.isObjectLiteralExpression(node))
      return Object.fromEntries(
        node.properties.map((p) => {
          if (!ts.isPropertyAssignment(p))
            throw new Error("Unsupported TS property " + p.getText(file));
          return [p.name.text, evaluate(p.initializer)];
        }),
      );
    if (ts.isIdentifier(node)) {
      if (node.text in external) return external[node.text];
      if (defs.has(node.text)) return evaluate(defs.get(node.text));
    }
    if (ts.isElementAccessExpression(node))
      return evaluate(node.expression)[evaluate(node.argumentExpression)];
    if (ts.isPropertyAccessExpression(node)) return evaluate(node.expression)[node.name.text];
    throw new Error("Unsupported TS data expression: " + node.getText(file));
  }
  return {
    get: (name) => {
      if (!(name in external) && !defs.has(name)) throw new Error("Missing TS constant " + name);
      return name in external ? external[name] : evaluate(defs.get(name));
    },
    defs,
    evaluate,
  };
}
export function slotModel(app) {
  const ids = tsConstants(app.file("src/features/shell/right-pane/right-pane-layout.ts"));
  const caps = capabilityModel(app);
  const constants = tsConstants(app.file("src/features/shell/slots/slot-registry.ts"), {
    SCRATCH_PANEL_ID: ids.get("SCRATCH_PANEL_ID"),
    SLOT_GRANT_CEILINGS: caps.slotCeilings,
  });
  const slots = Object.values(constants.get("SLOT_REGISTRY"));
  return { count: slots.length, slots };
}
export function slotOutputs(app) {
  const input = tracked(app),
    model = slotModel(input);
  return ref(
    "slots",
    { ...model, sources: input.sources },
    "# Slots\n\n" +
      `${model.count} host slots. Overlay is a presentation, not another slot. The host retains ownership of chrome and placement.\n\n` +
      table(
        [
          "Id",
          "Presentations",
          "Multiplicity",
          "Native defaults",
          "Narrow below (px)",
          "Team binding",
        ],
        model.slots.map((s) => [
          s.id,
          s.presentations.join(", "),
          s.multiplicity,
          `\`${JSON.stringify(s.nativeDefault)}\``,
          s.narrowBelowPx,
          s.teamBinding,
        ]),
      ),
  );
}
function schemaLabel(value) {
  if (value.const !== undefined) return JSON.stringify(value.const);
  if (value.enum) return value.enum.map((item) => JSON.stringify(item)).join(" | ");
  if (value.anyOf) return value.anyOf.map(schemaLabel).join(" | ");
  if (value.type === "array") return `Array<${schemaLabel(value.items)}>`;
  if (value.description?.includes("untyped (Value)")) return value.description;
  return value.type ?? value.description ?? JSON.stringify(value);
}
function jsonObjectShapes(source) {
  const rows = [];
  for (const match of productionPart(source).matchAll(/json!\(\s*\{/g)) {
    const open = match.index + match[0].lastIndexOf("{");
    const body = source.slice(open + 1, matchBracket(source, open) - 1);
    const properties = {};
    for (const member of splitTopLevel(body)) {
      const pair = /^"([^"]+)"\s*:\s*([\s\S]*)$/.exec(member);
      if (!pair) continue;
      const expression = pair[2];
      let schema = { description: "untyped (Value)", sourceExpression: expression };
      try {
        const literal = JSON.parse(expression);
        if (literal === null) schema = { type: "null" };
        else if (["string", "number", "boolean"].includes(typeof literal))
          schema = { type: typeof literal, const: literal };
      } catch {}
      properties[pair[1]] = schema;
    }
    if (Object.keys(properties).length) rows.push({ type: "object", properties });
  }
  return rows;
}
export function liveHubOutputs(app) {
  const input = tracked(app),
    projection = input.file("src-tauri/src/live_hub/projection.rs"),
    items = parseItems(projection);
  const builtins = stringLiterals(projection.match(/topics:\s*\[([^\]]+)\]/)[1]);
  const viewSource = input.file("src-tauri/src/live_hub/view_key.rs");
  const hub = input.file("src-tauri/src/live_hub/mod.rs");
  const sessionsSource = input.file("src-tauri/src/live_hub/agent_list/sessions.rs");
  const sessionItems = parseItems(input.file("src-tauri/src/mcp/session_list.rs"));
  const sessionSchema = rustSchema(sessionItems, "SessionListRow");
  const agentFields = input.file("src-tauri/src/agents/session_fields.rs");
  const listFields = input.file("src-tauri/src/live_hub/agent_list/fields.rs");
  const contributed = [
    ...strArrayConst(agentFields, "AGENT_FIELDS"),
    ...strArrayConst(listFields, "FIELDS"),
  ];
  const core = hub.match(/FieldProviders::new\(\[([\s\S]*?)\]\)/);
  if (!core) throw new Error("LiveHub core fields missing");
  const coreFields = [...core[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  const sessions = {
    type: "object",
    description:
      "untyped (Value): runtime and roster objects are merged; these are the source-declared core fields, not a closed row type.",
    properties: Object.fromEntries(
      [...new Set([...coreFields, ...contributed])].map((name) => [
        name,
        sessionSchema.properties[name] ?? { description: "untyped (Value)" },
      ]),
    ),
  };
  const status = input.file("src-tauri/src/live_hub/effective_status.rs");
  const providerFields = [...productionPart(status).matchAll(/row\.insert\(\s*"([^"]+)"/g)].map(
    (m) => m[1],
  );
  for (const name of providerFields) sessions.properties[name] = { description: "untyped (Value)" };
  // rowNames is assigned by the actual projection producer, after providers run.
  for (const m of productionPart(sessionsSource).matchAll(/projected\["([^"]+)"\]\s*=/g))
    sessions.properties[m[1]] ??= { description: "untyped (Value)" };
  for (const field of productionPart(hub).matchAll(/row\["([^"]+)"\]\s*=/g)) {
    sessions.properties[field[1]] ??= { description: "untyped (Value)" };
  }
  const inboxSource = input.file("src-tauri/src/live_hub/inbox.rs");
  const inbox = jsonObjectShapes(inboxSource).filter((row) => row.properties.rowType);
  const agentItems = parseItems(input.file("src-tauri/src/live_hub/agent_list/rows.rs"));
  const agent = rustSchema(agentItems, "AgentListRow");
  const views = input.file("src-tauri/src/live_hub/agent_list/views.rs");
  const meta = jsonObjectShapes(hub).find(
    (row) => row.properties.ready && row.properties.teamOrder,
  );
  for (const m of productionPart(views).matchAll(/meta\["([^"]+)"\]\s*=\s*([^;]+);/g))
    meta.properties[m[1]] = { description: "untyped (Value)", sourceExpression: m[2] };
  const recordSource = input.file("src-tauri/src/records/projection.rs");
  const record = jsonObjectShapes(recordSource).find(
    (row) => row.properties.fields && row.properties.preview,
  );
  for (const m of productionPart(recordSource).matchAll(/row\["([^"]+)"\]\s*=\s*([^;]+);/g))
    record.properties[m[1]] = { description: "untyped (Value)", sourceExpression: m[2] };
  const ui = input.file("src/features/dashboard/lib/live-hub.ts");
  const binding = input.file("src-tauri/src/records/binding.rs");
  const recordViewKeySource = fnBody(binding, "view_key");
  const globalUiTopics = JSON.parse(ui.match(/GLOBAL_UI_TOPICS[^=]*= new Set\((\[[^\]]*\])/)[1]);
  if (!ui.includes("upserts: [{ ...state, id: topic }]"))
    throw new Error("LiveHub UI producer changed; update source reader");
  const families = projection.includes('topic.starts_with("ui:")')
    ? [
        {
          name: "ui:<slotId>",
          row: {
            type: "object",
            description:
              "untyped (Value): persisted shared slot state spread into a row with id set to the topic.",
            properties: { id: { type: "string" } },
          },
          producer: "src/features/dashboard/lib/live-hub.ts",
        },
      ]
    : [];
  const shapes = { sessions: [sessions], inbox, agentList: [agent, meta], records: [record] };
  const topics = builtins.map((name) => ({
    name,
    rows: shapes[name] ?? [{ description: "untyped (Value)" }],
  }));
  const model = {
    counts: { builtins: topics.length, families: families.length },
    topics,
    families,
    envelopes: Object.fromEntries(
      ["Snapshot", "Delta", "SubscriptionReply"].map((name) => [name, rustSchema(items, name)]),
    ),
    viewKeys: {
      records: {
        pattern: recordViewKeySource
          .match(/format!\("([^"]+)", store.collection.name/)[1]
          .replace("{}", "{collection}"),
        source: recordViewKeySource,
      },
      globalUiTopics,
      team: viewSource.match(/format!\("([^"]+)"\)/)?.[1],
      all: viewSource.match(/Some\("([^"]+)"\.into\(\)\)/)?.[1],
      orderingSuffix: viewSource.match(/strip_suffix\("([^"]+)"\)/)?.[1],
      resolveSource: fnBody(viewSource, "resolve"),
      admitsSource: fnBody(viewSource, "admits"),
    },
    sources: input.sources,
  };
  const untyped = topics
    .filter((t) => JSON.stringify(t.rows).includes("untyped (Value)"))
    .map((t) => t.name)
    .concat(families.map((f) => f.name));
  model.untypedTopics = untyped;
  const md =
    "# LiveHub topics\n\n" +
    `${topics.length} built-in topics and ${families.length} dynamic topic family. Row shapes are read from actual producers; Value expressions and dynamic roster/collection state remain explicitly untyped. Topics with untyped fields: ${untyped.join(", ")}.\n\n` +
    `View keys: \`${model.viewKeys.all}\` for non-team grants; \`${model.viewKeys.team}\` for a pinned team; \`${model.viewKeys.orderingSuffix}\` is the agentList status ordering suffix. For sessions, inbox and agentList, team grants cannot read other teams. Records instead use \`${model.viewKeys.records.pattern}\`: private owner is the org; team owner is org/team. Global UI topics ${globalUiTopics.map((t) => "\`" + t + "\`").join(", ")} always use \`all\`, including in a team-pinned frame. Other UI topics follow the frame view.\n\n` +
    "[SDK subscribe/page signatures and envelopes](codesense.d.ts) · [Machine reference](live-hub.json)\n\n" +
    [...topics, ...families.map((f) => ({ name: f.name, rows: [f.row] }))]
      .map(
        (t) =>
          "## " +
          t.name +
          "\n\n" +
          t.rows
            .map(
              (row, i) =>
                `Row shape ${i + 1}${row.description ? ": " + row.description : ""}\n\n` +
                table(
                  ["Field", "Type / source expression"],
                  Object.entries(row.properties ?? {}).map(([name, val]) => [
                    name,
                    val.const !== undefined ? JSON.stringify(val.const) : schemaLabel(val),
                  ]),
                ),
            )
            .join("\n"),
      )
      .join("\n") +
    "\n## Envelopes\n\n" +
    Object.entries(model.envelopes)
      .map(
        ([name, schema]) =>
          "### " +
          name +
          "\n\n" +
          table(
            ["Field", "Type"],
            Object.entries(schema.properties).map(([key, val]) => [key, schemaLabel(val)]),
          ),
      )
      .join("\n");
  return ref("live-hub", model, md);
}
function cssDeclarations(body) {
  return Object.fromEntries(
    [...body.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map((m) => [
      m[1],
      m[2].trim(),
    ]),
  );
}
function cssBlock(source, selector) {
  const cleaned = source.replace(/\/\*[\s\S]*?\*\//g, "");
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|})\\s*${escaped}\\s*\\{([^}]*)\\}`, "m").exec(cleaned);
  if (!match) throw new Error("CSS block missing: " + selector);
  return cssDeclarations(match[1]);
}
function resolvePaletteAliases(value, palette, visiting = new Set()) {
  return value.replace(/var\((--[\w-]+)\s*\)/g, (reference, name) => {
    if (!(name in palette)) return reference;
    if (visiting.has(name)) throw new Error(`Cyclic palette alias ${name}`);
    return resolvePaletteAliases(palette[name], palette, new Set([...visiting, name]));
  });
}
export function tokenModel(app) {
  const kit = app.file("src-tauri/src/http/routes/dashboard_kit/tokens.css");
  const defaults = cssBlock(kit, ":root");
  const theme = app.file("src/features/dashboard/components/doc-theme.ts");
  const chains = tsConstants(theme).get("TOKEN_SOURCES");
  const css = app.file("src/index.css"),
    light = cssBlock(css, ":root"),
    dark = { ...light, ...cssBlock(css, ".dark") };
  // Generated kit values already use the shared surface roles. Host-only tokens
  // still resolve through their documented app fallback chains.
  const font = cssBlock(css, "@theme inline")["--font-sans"];
  const palette = { light, dark };
  const tokens = new Map(
    Object.entries(defaults).map(([name, value]) => [
      name,
      { name, default: value, light: value, dark: value, sources: ["tokens.css"] },
    ]),
  );
  for (const [name, sources] of chains) {
    const row = tokens.get(name) ?? { name };
    row.sources = sources;
    if (row.default !== undefined) {
      tokens.set(name, row);
      continue;
    }
    for (const scheme of ["light", "dark"]) {
      const values = palette[scheme];
      row[scheme] =
        sources
          .map((prop) =>
            prop === "color"
              ? values["--foreground"]
              : prop === "font-family"
                ? font
                : values[prop],
          )
          .find(Boolean) ??
        row[scheme] ??
        "CanvasText";
    }
    tokens.set(name, row);
  }
  const colors = app.file("src/shared/lib/pipeline-graph/epic-colors.ts");
  const palettes = tsConstants(colors);
  for (const scheme of ["light", "dark"]) {
    const selector = new RegExp(
      `${scheme}:\\s*(\\w+)\\.filter\\(\\(color\\) => \\[([^\\]]+)\\]\\.includes`,
    ).exec(colors);
    if (!selector) throw new Error(`Managed tree ${scheme} accent selector missing`);
    const hues = selector[2].split(",").map(Number);
    const selected = palettes
      .get(selector[1])
      .filter((color) => hues.includes(Number(color.match(/hsl\((\d+)/)?.[1])));
    selected.forEach((value, i) => {
      const name = `--cs-tree-accent-${i + 1}`,
        row = tokens.get(name) ?? { name, sources: ["MANAGED_TREE_ACCENTS"] };
      row[scheme] = value;
      tokens.set(name, row);
    });
  }
  tokens.set("--cs-color-scheme", {
    name: "--cs-color-scheme",
    light: "light",
    dark: "dark",
    sources: ["computed from surface background luminance"],
  });
  for (const row of tokens.values())
    for (const scheme of ["light", "dark"]) {
      row[scheme] = row[scheme].replace(/light-dark\(([^,()]+),\s*([^()]+)\)/g, (_, l, d) =>
        scheme === "light" ? l.trim() : d.trim(),
      );
      row[scheme] = resolvePaletteAliases(row[scheme], palette[scheme]);
    }
  return {
    count: tokens.size,
    palette:
      "Bundled CodeSense light/dark; selected app themes and host overrides take precedence.",
    tokens: [...tokens.values()],
  };
}
export function tokenOutputs(app) {
  const input = tracked(app),
    model = tokenModel(input),
    kit = input.file("src-tauri/src/http/routes/dashboard_kit/kit.css");
  const declarations = (scheme) => model.tokens.map((t) => `  ${t.name}: ${t[scheme]};`).join("\n");
  const componentsAt = kit.indexOf(".cs-app {");
  if (componentsAt < 0) throw new Error("Kit .cs-app reset missing");
  // Keep standalone controls and layout on the same CSS as injected pages; the
  // /kit font URL is host-only, so the fallback uses the token font stack.
  const components = kit.slice(componentsAt);
  const fallback =
    "/* Generated by pnpm gen:docs from the shared token model. Do not edit. */\n:root {\n  color-scheme: light dark;\n" +
    model.tokens.map((t) => `  ${t.name}: ${t.default ?? t.light};`).join("\n") +
    "\n}\n@media (prefers-color-scheme: dark) {\n  :root {\n" +
    declarations("dark") +
    '\n  }\n}\n:root[data-theme="light"] {\n' +
    declarations("light") +
    '\n}\n:root[data-theme="dark"] {\n' +
    declarations("dark") +
    "\n}\n" +
    components;
  return {
    ...ref(
      "tokens",
      { ...model, sources: input.sources },
      "# Dashboard tokens\n\n" +
        `${model.count} kit and host tokens. ${model.palette} Expressions remain CSS expressions where the runtime resolves them. The fallback stylesheet uses this same model. Live pages should use the host theme, not prefers-color-scheme.\n\n` +
        table(
          ["Token", "Light", "Dark", "Host source / default"],
          model.tokens.map((t) => [t.name, t.light, t.dark, t.sources.join(" → ")]),
        ),
    ),
    "codesense-docs/getting-started/content/_shared/kit-fallback.css": fallback,
  };
}
export const builders = {
  "page-sdk": pageSdkOutputs,
  "pane-declarations": declarationOutputs,
  capabilities: capabilityOutputs,
  slots: slotOutputs,
  "live-hub": liveHubOutputs,
  tokens: tokenOutputs,
};
