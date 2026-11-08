// SPDX-License-Identifier: MIT
import { readdirSync } from "node:fs";
import { resolve } from "node:path";

/**
 * One row per built-in pane, each on one line starting `{ id: "`, in the order of `BUILTINS` in
 * `src-tauri/src/panes/builtin.rs`. A Rust test reads these lines and keeps each row's id and
 * source equal to the Rust entry's. `example` ships a read-only copy in the docs;
 * `opener.label` names the pane's open item in the View menu and command palette.
 * @type {{ id: string, source: string, example?: true, opener?: { label: string } }[]}
 */
// prettier-ignore
export const BUILTIN_PANES = [
  { id: "inbox", source: "src/features/shell/right-pane/__fixtures__/inbox-pane.html", example: true },
  { id: "story-trail", source: "src-tauri/assets/dashboards/story-trail/index.html" },
  { id: "files-tree", source: "src/features/shell/slots/builtin/files-tree.html" },
  { id: "agent-list", source: "src/features/shell/slots/__fixtures__/agent-list-pane.html", example: true },
  { id: "notes", source: "src/features/shell/right-pane/__fixtures__/notes-pane.html", example: true },
];

export const AGENT_LIST_PANE = BUILTIN_PANES.find(({ id }) => id === "agent-list").source;
export const BUILTIN_PANE_EXAMPLES = BUILTIN_PANES.filter((pane) => pane.example).map(
  ({ id, source }) => ({
    id,
    source,
    example: `codesense-docs/getting-started/content/examples/panes/${id}.html`,
  }),
);

/** The pane files the pane checks scan. Bundled dashboards are scanned with their own assets. */
export function builtInPaneFiles(root = process.cwd()) {
  const docs = "codesense-docs/getting-started/panes";
  return [
    ...readdirSync(resolve(root, docs))
      .filter((name) => /\.(html|js)$/.test(name))
      .map((name) => `${docs}/${name}`),
    ...BUILTIN_PANES.map(({ source }) => source).filter(
      (source) => !source.startsWith("src-tauri/assets/dashboards/"),
    ),
  ];
}
