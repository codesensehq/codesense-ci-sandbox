// SPDX-License-Identifier: MIT
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { build } from "vite";
import { editorDigest } from "./kit-editor-digest.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const entry = resolve(root, "src/features/notes/editor/kit-entry.ts");
const out = resolve(root, "src-tauri/src/http/routes/dashboard_kit");
const sourceCss = readFileSync(resolve(root, "src/styles/scratch-note.css"), "utf8");
const { digest, tiptap } = editorDigest(root, entry);
if (!tiptap) throw new Error("Cannot identify TipTap version in lockfile");
const stamp = `/* CodeSense kit editor | TipTap ${tiptap} | inputs ${digest} */\n`;

function section(start, end) {
  return sourceCss.slice(sourceCss.indexOf(start), sourceCss.indexOf(end));
}
let css = section(".nt-pane,", "/* Light viewer themes").replace(
  /\.nt-pane,\s*\.nt-pop/,
  ".scratch-note-editor",
);
css = css
  .replace("var(--cs-viewer-bg, var(--background))", "var(--cs-bg, Canvas)")
  .replace("var(--tw-prose-body, var(--foreground))", "var(--cs-fg, CanvasText)")
  .replace("var(--muted-foreground)", "var(--cs-muted-fg, GrayText)");
css += "\n.scratch-note-editor { color-scheme: var(--cs-color-scheme, normal); }\n";
css += ".scratch-note-editor, .scratch-note-editor * { box-sizing: border-box; }\n";
css +=
  ".scratch-note-editor :where(button) { appearance: none; background: transparent; border: 0; cursor: pointer; }\n";
css += section(".nt-kbd {", ".nt-btn {").replaceAll(".nt-kbd", ".scratch-note-editor .nt-kbd");
css += section(".scratch-note-editor {", "/* the hover send button");
css += section(".scratch-note-editor .nt-chg {", ".nt-removed {");
css += section(".nt-removed {", ".nt-banner {").replaceAll(
  ".nt-removed",
  ".scratch-note-editor .nt-removed",
);
css += section(".nt-pane ::selection {", ".nt-body {").replace(
  ".nt-pane ::selection",
  ".scratch-note-editor ::selection",
);
css += readFileSync(resolve(root, "src/styles/status-mark.css"), "utf8").split(
  "/* The story-stage rail",
)[0];
const replacements = [
  ["var(--accent)", "var(--cs-highlight, var(--cs-link, LinkText))"],
  ["var(--ring)", "var(--cs-highlight, Highlight)"],
  ["var(--font-sans)", "var(--cs-font-sans, system-ui, sans-serif)"],
  ["var(--popover-foreground, var(--foreground))", "var(--nt-ink)"],
  ["var(--popover)", "var(--nt-float)"],
  ["var(--foreground)", "var(--cs-fg, CanvasText)"],
  ["var(--tw-prose-pre-code, var(--nt-ink))", "var(--nt-ink)"],
  [
    "var(--cs-code-comment, var(--nt-quiet))",
    "var(--cs-code-comment, light-dark(#6e7781, #8b949e))",
  ],
  ["var(--cs-code-keyword, var(--nt-ink))", "var(--cs-code-keyword, light-dark(#cf222e, #ff7b72))"],
  ["var(--cs-code-string, var(--nt-ink))", "var(--cs-code-string, light-dark(#0a3069, #a5d6ff))"],
  ["var(--cs-status-busy)", "var(--cs-state-running)"],
  ["var(--cs-status-needs)", "var(--cs-state-needs-you)"],
  ["var(--cs-status-failed)", "var(--cs-state-failed)"],
  ["var(--cs-status-idle)", "var(--cs-state-idle)"],
  ["var(--cs-status-added, #3fb950)", "var(--cs-success, light-dark(#1a7f37, #3fb950))"],
  ["var(--cs-status-deleted, #f85149)", "var(--cs-deleted, var(--cs-danger))"],
];
for (const [from, to] of replacements) css = css.replaceAll(from, to);
css = stamp + css;

if (process.argv.includes("--check")) {
  for (const name of ["editor.js", "editor.css"]) {
    const path = resolve(out, name);
    if (
      !existsSync(path) ||
      !readFileSync(path, "utf8").startsWith(stamp) ||
      (name === "editor.css" && readFileSync(path, "utf8") !== css)
    ) {
      throw new Error(`${name} is stale; run pnpm build:kit-editor`);
    }
  }
  console.log(`Kit editor stamp current: ${digest}`);
} else {
  const result = await build({
    configFile: false,
    logLevel: "warn",
    resolve: { alias: { "@": resolve(root, "src") } },
    define: { __NOTE_EDITOR_VERSION__: JSON.stringify(digest) },
    build: {
      write: false,
      target: "es2022",
      minify: "esbuild",
      lib: { entry, formats: ["es"], fileName: () => "editor.js" },
      rollupOptions: { output: { inlineDynamicImports: true } },
    },
  });
  const output = (Array.isArray(result) ? result[0] : result).output;
  const chunks = output.filter((item) => item.type === "chunk");
  if (chunks.length !== 1) throw new Error("Kit editor must be one standalone module");
  const code = stamp + chunks[0].code;
  if (/\beval\s*\(|\bnew\s+Function\s*\(/.test(code))
    throw new Error("Kit editor violates the frame CSP");
  if (chunks[0].imports.length) throw new Error("Kit editor has external imports");
  writeFileSync(resolve(out, "editor.js"), code);
  writeFileSync(resolve(out, "editor.css"), css);
  console.log(
    `Kit editor ${digest}: ${gzipSync(code).length + gzipSync(css).length} bytes gzipped (JS + CSS)`,
  );
}
