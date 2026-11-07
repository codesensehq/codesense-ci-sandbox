// SPDX-License-Identifier: MIT
import { readdirSync } from "node:fs";
import { resolve } from "node:path";

export const AGENT_LIST_PANE = "src/features/shell/slots/__fixtures__/agent-list-pane.html";
export const BUILTIN_PANE_EXAMPLES = [
  { id: "inbox", source: "src/features/shell/right-pane/__fixtures__/inbox-pane.html" },
  { id: "notes", source: "src/features/shell/right-pane/__fixtures__/notes-pane.html" },
  { id: "agent-list", source: AGENT_LIST_PANE },
].map((pane) => ({
  ...pane,
  example: `codesense-docs/getting-started/content/examples/panes/${pane.id}.html`,
}));

export function builtInPaneFiles(root = process.cwd()) {
  const docs = "codesense-docs/getting-started/panes";
  return [
    ...readdirSync(resolve(root, docs))
      .filter((name) => /\.(html|js)$/.test(name))
      .map((name) => `${docs}/${name}`),
    ...BUILTIN_PANE_EXAMPLES.map(({ source }) => source),
    "src/features/shell/slots/builtin/files-tree.html",
  ];
}
