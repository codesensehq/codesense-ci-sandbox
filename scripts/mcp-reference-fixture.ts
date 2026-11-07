// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { CONTENT, ROOT, readCatalog } from "./generate-mcp-reference.mjs";

type Family = {
  id: string;
  title: string;
  docs: string;
  searchTerms: string[];
  tools: string[];
  calls: Array<{ tool: string; arguments: Record<string, unknown> }>;
};

// Strict fixtures model the last export wave. A story can add catalog tools before
// that wave runs; pending-tool behavior has its own PR-mode tests.
export function exportedCatalog(root = ROOT) {
  const catalog = readCatalog(root) as { schema: number; families: Family[] };
  const exported = new Set<string>(
    JSON.parse(
      readFileSync(join(root, CONTENT, "generated/mcp-tools.dashboard-publish.json"), "utf8"),
    ).tools.map((tool: { name: string }) => tool.name),
  );
  return {
    ...catalog,
    families: catalog.families
      .map((family) => ({
        ...family,
        tools: family.tools.filter((name) => exported.has(name)),
        calls: family.calls.filter((call) => exported.has(call.tool)),
      }))
      .filter((family) => family.tools.length),
  };
}
