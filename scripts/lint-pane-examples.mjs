// SPDX-License-Identifier: MIT
// Build-time checks for the copyable examples. Runtime liveness is conformance's job.
import ts from "typescript";
import { Script } from "node:vm";
import { spawnSync } from "node:child_process";

const PREFIX = "getting-started/content/";
const decode = (value) =>
  value
    .replace(/&quot;/g, '"')
    .replace(/&#39;|&apos;/g, "'")
    .replace(/&amp;/g, "&");
function meta(html, name) {
  for (const match of html.matchAll(/<meta\b[^>]*>/gi)) {
    const attrs = Object.fromEntries(
      [...match[0].matchAll(/([\w:-]+)\s*=\s*(["'])(.*?)\2/gs)].map((m) => [
        m[1].toLowerCase(),
        decode(m[3]),
      ]),
    );
    if (attrs.name === name) return attrs.content ?? "";
  }
}
// Shipped example panes: examples/panes/** and examples/**/panes/**. The docs team's own panes
// (getting-started/panes/*) sit outside the shipped content tree and call ready() through the
// CodeSenseDocsPane wrapper, so this lint does not see them.
const isPaneExample = (path) =>
  path.startsWith(`${PREFIX}examples/panes/`) || /^[^]*\/examples\/(?:[^]+\/)?panes\//.test(path);
const json = (files, name) => JSON.parse(files.get(`${PREFIX}generated/${name}`).toString());

export function lintPaneExamples(files) {
  const slots = new Set(json(files, "slots.json").slots.map((slot) => slot.id));
  const caps = new Set(json(files, "capabilities.json").capabilities.map((cap) => cap.name));
  const commandSchema = json(files, "pane-declaration.schema.json").properties.panes.items
    .properties.commands.items;
  const errors = [];
  for (const [path, data] of files) {
    if (!path.endsWith(".html")) continue;
    if (!path.startsWith(`${PREFIX}examples/dashboards/`) && !isPaneExample(path)) continue;
    const html = data.toString();
    let ready = false;
    for (const match of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script\s*>/gi)) {
      if (/\bsrc\s*=|\btype\s*=\s*["']application\/json/i.test(match[1])) continue;
      try {
        if (/(?:^|\s)type\s*=\s*(?:"module"|'module'|module(?=\s|$))/i.test(match[1])) {
          const result = spawnSync(process.execPath, ["--input-type=module", "--check"], {
            input: match[2],
            encoding: "utf8",
          });
          if (result.error) throw result.error;
          if (result.status !== 0)
            throw new Error(result.stderr.trim() || "module syntax check failed");
        } else new Script(match[2], { filename: path });
      } catch (error) {
        errors.push(`${path}: script syntax: ${error.message}`);
      }
      const source = ts.createSourceFile(
        path,
        match[2],
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.JS,
      );
      const aliases = new Set(["codesense"]);
      const sdk = (node) =>
        (ts.isIdentifier(node) && aliases.has(node.text)) ||
        (ts.isPropertyAccessExpression(node) && node.getText(source) === "window.codesense");
      const visit = (node) => {
        if (
          ts.isVariableDeclaration(node) &&
          ts.isIdentifier(node.name) &&
          node.initializer &&
          sdk(node.initializer)
        )
          aliases.add(node.name.text);
        if (
          ts.isCallExpression(node) &&
          ts.isPropertyAccessExpression(node.expression) &&
          node.expression.name.text === "ready" &&
          sdk(node.expression.expression)
        )
          ready = true;
        ts.forEachChild(node, visit);
      };
      visit(source);
    }
    if (!ready)
      errors.push(
        `${path}: missing explicit executable codesense.ready() (required by slot liveness)`,
      );
    for (const [name, allowed] of [
      ["slots", slots],
      ["capabilities", caps],
    ]) {
      for (const value of (meta(html, `codesense:${name}`) ?? "").split(/[,\s]+/).filter(Boolean)) {
        if (!allowed.has(value)) errors.push(`${path}: unknown ${name}: ${value}`);
      }
    }
    const raw = meta(html, "codesense:commands");
    if (raw === undefined || !raw.trim()) continue;
    try {
      const commands = JSON.parse(raw);
      if (!Array.isArray(commands)) throw new Error("expected an array");
      const ids = new Set();
      for (const command of commands) {
        if (!command || typeof command !== "object") throw new Error("expected a command object");
        for (const key of commandSchema.required)
          if (!(key in command)) throw new Error(`command missing ${key}`);
        for (const [key, schema] of Object.entries(commandSchema.properties)) {
          const value = command[key];
          if (value === undefined) continue;
          const types = schema.anyOf ?? [schema];
          if (
            !types.some((type) =>
              type.type === "null" ? value === null : typeof value === type.type,
            )
          )
            throw new Error(`command ${key} has wrong type`);
          if (schema.enum && !schema.enum.includes(value))
            throw new Error(`unknown command ${key}: ${value}`);
        }
        if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(command.id) || ids.has(command.id))
          throw new Error("invalid or duplicate command id");
        if (!command.label.trim()) throw new Error("command label is empty");
        ids.add(command.id);
        if (command.defaultChord != null) {
          const parts = command.defaultChord.split("+");
          const keys = parts.filter((p) => !["CmdOrCtrl", "Alt", "Shift"].includes(p));
          if (
            parts.includes("") ||
            keys.length !== 1 ||
            !(
              parts.includes("CmdOrCtrl") ||
              parts.includes("Alt") ||
              /^(F([1-9]|1\d|2[0-4])|Escape)$/.test(keys[0])
            )
          )
            throw new Error("invalid command defaultChord");
        }
      }
    } catch (error) {
      errors.push(`${path}: codesense:commands: ${error.message}`);
    }
  }
  if (errors.length)
    throw new Error(`build-codesense-docs: example lint failed:\n  - ${errors.join("\n  - ")}`);
}
