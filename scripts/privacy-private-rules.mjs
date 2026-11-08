// SPDX-License-Identifier: MIT
// Public-safe loader: an explicit local inventory supplies every private value.
import fs from "node:fs";
import { GENERIC_RULES } from "./privacy-rules.mjs";

// Frozen slots from the original scanner. No pattern, name or value is embedded here.
const PRIVATE_LAYOUT = new Map([
  [5, ["error", "g"]],
  [6, ["error", "gi"]],
  [7, ["error", "gi"]],
  [8, ["error", "gi"]],
  [9, ["error", "g"]],
  [24, ["text", "g"]],
]);
const FIELDS = new Set(["id", "sev", "kind", "source", "flags", "order", "classification"]);

// Safe to print: it names no term.
export const INVENTORY_ERROR = "Invalid or unavailable private rule inventory";

export function loadPrivateRules(file) {
  try {
    const inventory = JSON.parse(fs.readFileSync(file, "utf8"));
    if (
      inventory.version !== 1 ||
      !Array.isArray(inventory.rules) ||
      inventory.rules.length !== PRIVATE_LAYOUT.size
    )
      throw new Error();
    const seen = new Set();
    return inventory.rules.map((rule) => {
      if (
        !rule ||
        typeof rule !== "object" ||
        Object.keys(rule).length !== FIELDS.size ||
        Object.keys(rule).some((key) => !FIELDS.has(key))
      )
        throw new Error();
      const layout = PRIVATE_LAYOUT.get(rule.order);
      if (
        !Number.isInteger(rule.order) ||
        !layout ||
        seen.has(rule.order) ||
        rule.sev !== layout[0] ||
        rule.flags !== layout[1] ||
        rule.kind !== "show" ||
        rule.classification !== "private-only" ||
        typeof rule.id !== "string" ||
        !/^[a-z][a-z0-9-]*$/.test(rule.id) ||
        typeof rule.source !== "string" ||
        rule.source.length === 0
      )
        throw new Error();
      seen.add(rule.order);
      const { source, flags, ...descriptor } = rule;
      return { ...descriptor, re: new RegExp(source, flags) };
    });
  } catch {
    throw new Error(INVENTORY_ERROR);
  }
}

export function combinedRules(file) {
  return file
    ? [...GENERIC_RULES, ...loadPrivateRules(file)].sort((a, b) => a.order - b.order)
    : GENERIC_RULES;
}

export const ruleIds = (rules) =>
  new Set([...rules.map((rule) => rule.id), "key-file", "image-metadata"]);
