// SPDX-License-Identifier: MIT
import { conditionalVerbGrants, describeConditionalGrant } from "./verb-grants.mjs";
import { strArrayConst, strConst, numConst } from "./rust.mjs";
export const SDK_PATH = "src-tauri/src/http/routes/dashboard_kit/codesense.d.ts";
export const REGISTRY_PATH = "src-tauri/src/verbs/registry.generated.json";
/** One source model for installed references, the site, and the primitives explorer. */
export function buildPageSdk(app, ts) {
  const declaration = app.file(SDK_PATH);
  const source = ts.createSourceFile(
    SDK_PATH,
    declaration,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  const interfaces = new Map();
  function visit(node) {
    if (ts.isInterfaceDeclaration(node)) interfaces.set(node.name.text, node);
    ts.forEachChild(node, visit);
  }
  visit(source);
  const members = [];
  function add(nodes, owner, prefix = "") {
    for (const node of nodes) {
      if (!node.name) {
        if (ts.isIndexSignatureDeclaration(node)) {
          members.push({
            name: prefix + "[helper: string]",
            owner,
            signature: node.getText(source),
            description: "Undeclared kit helpers have unknown type.",
            sourceRef: SDK_PATH,
          });
          continue;
        }
        throw new Error(`SDK: unnamed ${owner} member`);
      }
      const name =
        prefix + (ts.isStringLiteral(node.name) ? node.name.text : node.name.getText(source));
      const description = (node.jsDoc ?? [])
        .map((d) => (typeof d.comment === "string" ? d.comment : ""))
        .filter(Boolean)
        .join("\n");
      members.push({
        name,
        owner,
        signature: node.getText(source),
        description,
        sourceRef: SDK_PATH,
      });
      if (node.type && ts.isTypeLiteralNode(node.type)) add(node.type.members, owner, `${name}.`);
    }
  }
  for (const owner of ["Surface", "VerbMethods", "Kit"]) {
    if (!interfaces.has(owner)) throw new Error(`SDK: ${owner} interface missing`);
    add(interfaces.get(owner).members, owner, owner === "Kit" ? "kit." : "");
  }
  const verbMembers = new Map();
  for (const member of members) {
    if (member.owner !== "VerbMethods") continue;
    const overloads = verbMembers.get(member.name) ?? [];
    overloads.push(member);
    verbMembers.set(member.name, overloads);
  }
  const verbDescription = (name) => verbMembers.get(name)?.[0]?.description ?? "";
  const specs = JSON.parse(app.file(REGISTRY_PATH));
  const conditional = conditionalVerbGrants(app, specs);
  const registry = specs.map((verb) => ({
    ...verb,
    ...(conditional[verb.name].length ? { conditionalGrants: conditional[verb.name] } : {}),
  }));
  for (const verb of registry) {
    const methods = verbMembers.get(verb.name);
    if (!methods?.length)
      throw new Error(`SDK: VerbMethods missing ${verb.name}; regenerate pnpm gen:verbs`);
    for (const method of methods)
      Object.assign(method, {
        class: verb.class,
        grant: verb.grant,
        conditionalGrants: verb.conditionalGrants,
        mcpTwin: verb.mcpTwin,
        presentationOnlyReason: verb.presentationOnlyReason,
        argsSchema: verb.argsSchema,
        resultSchema: verb.resultSchema,
        action: verb.action,
        confirmable: verb.confirmable,
      });
  }
  const viewer = app.file("src-tauri/src/http/routes/viewer.rs");
  const features = strArrayConst(viewer, "PAGE_SDK_FEATURES");
  for (const member of members)
    member.feature = features.includes(member.name) ? member.name : null;
  const sdk = members.filter((m) => m.owner === "Surface" || m.owner === "Kit");
  const readVerbs = registry
    .filter((v) => !v.action)
    .map((v) => ({
      verb: v.name,
      tier: v.class,
      description: verbDescription(v.name),
      args: v.argsSchema,
      result: v.resultSchema,
      grant: v.grant,
      conditionalGrants: v.conditionalGrants,
      mcpTwin: v.mcpTwin,
    }));
  const actionVerbs = registry
    .filter((v) => v.action)
    .map((v) => ({
      verb: v.name,
      tier: v.class,
      args: JSON.stringify(v.argsSchema),
      description: verbDescription(v.name),
      denyExample: "denied: grant",
      result: v.resultSchema,
      grant: v.grant,
      conditionalGrants: v.conditionalGrants,
      mcpTwin: v.mcpTwin,
    }));
  const classes = [...new Set(registry.filter((v) => v.action).map((v) => v.class))];
  return {
    schema: 1,
    versions: {
      sdk: strConst(viewer, "PAGE_SDK_VERSION"),
      querySchema: numConst(
        app.file("src-tauri/src/context_query/model.rs"),
        "CONTEXT_QUERY_SCHEMA_VERSION",
      ),
    },
    features,
    counts: {
      bridgeMethods: registry.length,
      actionVerbs: actionVerbs.length,
      readVerbs: readVerbs.length,
      surfaceMembers: sdk.filter((m) => m.owner === "Surface").length,
    },
    sdk,
    members,
    verbs: registry,
    readVerbs,
    actionVerbs,
    tiers: classes.map((tier) => ({
      tier,
      verbs: registry
        .filter((v) => v.action && v.class === tier)
        .map((v) => v.name)
        .join(", "),
      note: `${tier} class; Rust checks the effective grant on every call.`,
    })),
    declaration,
  };
}
/** Match Codesense's Omit<VerbMethods, keyof Surface>: Surface owns overrides. */
export function publicSdkMembers(model) {
  const surfaceNames = new Set(
    model.members.filter((member) => member.owner === "Surface").map((member) => member.name),
  );
  return model.members.filter(
    (member) => member.owner !== "VerbMethods" || !surfaceNames.has(member.name),
  );
}
export function pageSdkMarkdown(model) {
  return (
    (
      "# Page SDK\n\nGenerated from the Rust registry and TypeScript declarations. All " +
      model.counts.bridgeMethods +
      " verbs and every declared Surface member are listed below. Rust checks each call against its frame grant.\n\n" +
      "[Exact TypeScript declarations](codesense.d.ts) · [Machine reference](page-sdk.json) · [Capabilities](capabilities.md) · [LiveHub](live-hub.md)\n\n" +
      `SDK version: ${model.versions.sdk}. Features reported to has(): ${model.features.map((f) => "`" + f + "`").join(", ")}. A null feature means no separate member-named feature flag is declared. Named types in signatures are defined in the linked .d.ts; unknown stays unknown.\n\n` +
      model.members
        .map(
          (m) =>
            `## ${m.owner}: ${m.name}\n\n\`\`\`ts\n${m.signature}\n\`\`\`\n\n${m.description}\n\n` +
            (m.owner === "VerbMethods"
              ? `Class: ${m.class}; default grant: ${m.grant}; MCP twin: ${m.mcpTwin ?? m.presentationOnlyReason ?? "none"}.\n\n` +
                (m.conditionalGrants?.length
                  ? "Conditional grants (override the default when their conditions match):\n\n" +
                    m.conditionalGrants
                      .map((rule) => "- " + describeConditionalGrant(rule))
                      .join("\n") +
                    "\n\n"
                  : "")
              : ""),
        )
        .join("")
    ).trimEnd() + "\n"
  );
}
