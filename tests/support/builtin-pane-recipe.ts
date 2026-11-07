// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const recipe = readFileSync(
  resolve(
    "codesense-docs/getting-started/content/arc-i-dashboard-primitives/customize-built-in-panes.html",
  ),
  "utf8",
);
const decode = (text: string) =>
  text.replace(
    /&(amp|lt|gt|quot|#x27|#39);/g,
    (_, key: string) => ({ amp: "&", lt: "<", gt: ">", quot: '"', "#x27": "'", "#39": "'" })[key]!,
  );

/** Apply the actual copyable recipe text to the actual shipped example. */
export function recipePane(id: "inbox" | "notes" | "agent-list") {
  let html = readFileSync(
    resolve(`codesense-docs/getting-started/content/examples/panes/${id}.html`),
    "utf8",
  );
  const snippet = recipe.match(
    new RegExp(`<code[^>]*data-recipe="${id}">([\\s\\S]*?)<\\/code>`),
  )?.[1];
  if (!snippet) throw new Error(`Missing ${id} recipe`);
  const code = decode(snippet);
  for (const hunk of code.split("@@\n").filter(Boolean)) {
    const lines = hunk.trimEnd().split("\n");
    const before = lines
      .filter((line) => line.startsWith("-"))
      .map((line) => line.slice(1))
      .join("\n");
    const after = lines
      .filter((line) => line.startsWith("+"))
      .map((line) => line.slice(1))
      .join("\n");
    if (!before || html.split(before).length !== 2)
      throw new Error(`${id} recipe replacement anchor moved: ${before}`);
    html = html.replace(before, after);
  }
  return html;
}
