#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Generates the built-in crest avatar pack: one heraldic shield per tincture × emblem,
// written as SVG files plus the pack's manifest into src/assets/avatar-packs/builtin-crest/.
// Run: node scripts/gen-avatar-crests.mjs
import { mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(
  dirname(fileURLToPath(import.meta.url)),
  "../src/assets/avatar-packs/builtin-crest",
);

// field: the shield; ground: the tile behind it; metal: border and emblem.
const GOLD = "#f4cc5a";
const TINCTURES = [
  { id: "red", label: "Red", field: "#c8323c", ground: "#8a2a33", metal: GOLD },
  { id: "blue", label: "Blue", field: "#2f63c9", ground: "#2a4785", metal: GOLD },
  { id: "green", label: "Green", field: "#2d8f57", ground: "#265f40", metal: GOLD },
  { id: "purple", label: "Purple", field: "#7a42b3", ground: "#563580", metal: GOLD },
  { id: "orange", label: "Orange", field: "#de7a26", ground: "#93501d", metal: GOLD },
  { id: "black", label: "Black", field: "#2b2b36", ground: "#5b5e70", metal: GOLD },
];

// Emblems drawn in the shield's centre, in the metal colour (`M`) and the field colour (`F`).
const EMBLEMS = [
  {
    id: "bolt",
    label: "bolt",
    draw: (M) => `<path d="M35 15 22 33h8l-3 14 13-19h-8z" fill="${M}"/>`,
  },
  {
    id: "star",
    label: "star",
    draw: (M) =>
      `<path d="m32 15 4.1 9.4 10.2.9-7.7 6.8 2.3 10-8.9-5.3-8.9 5.3 2.3-10-7.7-6.8 10.2-.9z" fill="${M}"/>`,
  },
  {
    id: "key",
    label: "key",
    draw: (M, F) =>
      `<circle cx="32" cy="21" r="6.5" fill="${M}"/><circle cx="32" cy="21" r="2.6" fill="${F}"/>` +
      `<path d="M30 26h4v19h-4z" fill="${M}"/><path d="M34 37h5v3h-5zM34 42h4v3h-4z" fill="${M}"/>`,
  },
  {
    id: "moon",
    label: "moon",
    draw: (M, F) =>
      `<circle cx="31" cy="30" r="12" fill="${M}"/><circle cx="36.5" cy="26.5" r="10" fill="${F}"/>`,
  },
  {
    id: "gear",
    label: "gear",
    draw: (M, F) =>
      `<g fill="${M}">${[0, 45, 90, 135]
        .map(
          (a) =>
            `<rect x="29.5" y="15" width="5" height="28" rx="1" transform="rotate(${a} 32 29)"/>`,
        )
        .join(
          "",
        )}<circle cx="32" cy="29" r="10"/></g><circle cx="32" cy="29" r="4.2" fill="${F}"/>`,
  },
  {
    id: "flame",
    label: "flame",
    draw: (M, F) =>
      `<path d="M32 13c3 7 11 11 11 21a11 11 0 0 1-22 0c0-6 3-9 5-12 0 4 2 6 4 6-2-6 0-11 2-15z" fill="${M}"/>` +
      `<path d="M32 31c2 3 5 5 5 8a5 5 0 0 1-10 0c0-3 3-5 5-8z" fill="${F}"/>`,
  },
];

const SHIELD = "M13 9h38v20c0 14-8 23-19 28C21 52 13 43 13 29z";
const INNER = "M17 13h30v16c0 11-6 18-15 22-9-4-15-11-15-22z";

function crest(t, e) {
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">` +
    `<rect width="64" height="64" fill="${t.ground}"/>` +
    `<path d="${SHIELD}" fill="${t.metal}"/>` +
    `<path d="${INNER}" fill="${t.field}"/>` +
    e.draw(t.metal, t.field) +
    `</svg>\n`
  );
}

mkdirSync(OUT, { recursive: true });
for (const file of readdirSync(OUT)) if (file.endsWith(".svg")) rmSync(join(OUT, file));
const faces = [];
for (const e of EMBLEMS) {
  for (const t of TINCTURES) {
    const id = `${t.id}-${e.id}`;
    writeFileSync(join(OUT, `${id}.svg`), crest(t, e));
    faces.push({ id, file: `${id}.svg`, label: `${t.label} ${e.label}` });
  }
}
writeFileSync(join(OUT, "pack.json"), JSON.stringify({ name: "Crests", faces }, null, 2) + "\n");
console.log(`${faces.length} crests`);
