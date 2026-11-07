// SPDX-License-Identifier: MIT
// Builds the picker's local emoji file from emojibase-data (MIT, build-time only).
// Output: src-tauri/src/http/routes/dashboard_kit/emoji.json, served at /kit/v1/emoji.json.
//   { v, groups: [{ key, label }], e: [[groupIndex, emoji, label, searchTags, skinnable?]] }
// `skinnable` is 1 for a base emoji whose five single-tone variants (light … dark) are the
// base with its first code point's variation selector dropped and the tone inserted after
// it (the picker derives them; the generator only sets 1 when that matches emojibase), or
// the five variants themselves when they cannot be derived (people holding hands, couples).
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const OUTPUT = "src-tauri/src/http/routes/dashboard_kit/emoji.json";
const require = createRequire(import.meta.url);

const TONES = ["1F3FB", "1F3FC", "1F3FD", "1F3FE", "1F3FF"];
const MAX_TAGS = 2;

// The tone of a skin variant when every modifier in it is the same one, else null.
function singleTone(hexcode) {
  const found = hexcode.split("-").filter((part) => TONES.includes(part));
  const first = found[0];
  return first && found.every((part) => part === first) ? TONES.indexOf(first) : null;
}

export function withTone(emoji, tone) {
  const [first, ...rest] = Array.from(emoji);
  const tail = rest[0] === "\uFE0F" ? rest.slice(1) : rest;
  return first + String.fromCodePoint(parseInt(TONES[tone], 16)) + tail.join("");
}

export function build() {
  const compact = require("emojibase-data/en/compact.json");
  const messages = require("emojibase-data/en/messages.json");
  const groups = messages.groups
    .filter((g) => g.key !== "component")
    .sort((a, b) => a.order - b.order);
  const index = new Map(
    groups.map((g, i) => [messages.groups.find((m) => m.key === g.key).order, i]),
  );
  const rows = compact
    .filter((e) => index.has(e.group))
    .sort((a, b) => a.order - b.order)
    .map((e) => {
      const row = [
        index.get(e.group),
        e.unicode,
        e.label,
        (e.tags ?? []).slice(0, MAX_TAGS).join(" "),
      ];
      const skins = [];
      for (const skin of e.skins ?? []) {
        const tone = singleTone(skin.hexcode);
        if (tone !== null) skins[tone] = skin.unicode;
      }
      const derived = TONES.every((_, tone) => skins[tone] === withTone(e.unicode, tone));
      if (derived) row.push(1);
      else if (TONES.every((_, tone) => skins[tone])) row.push(skins);
      return row;
    });
  const data = { v: 1, groups: groups.map((g) => ({ key: g.key, label: g.message })), e: rows };
  return Buffer.from(JSON.stringify(data) + "\n");
}

export function generate(root = ROOT, { check = false } = {}) {
  const bytes = build();
  const path = resolve(root, OUTPUT);
  if (check) {
    if (!existsSync(path) || !readFileSync(path).equals(bytes))
      throw new Error(`Stale generated file: ${OUTPUT}; run node scripts/generate-emoji-data.mjs`);
  } else {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, bytes);
  }
  return bytes;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const check = process.argv.includes("--check");
  const bytes = generate(ROOT, { check });
  console.log(`emoji data: ${check ? "ok" : "generated"} (${bytes.length} bytes)`);
}
