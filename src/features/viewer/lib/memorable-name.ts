// SPDX-License-Identifier: MIT
const ADJECTIVES = [
  "Amber",
  "Azure",
  "Blue",
  "Bold",
  "Brave",
  "Breezy",
  "Bright",
  "Brisk",
  "Cheery",
  "Clever",
  "Cosmic",
  "Crimson",
  "Crystal",
  "Dapper",
  "Deep",
  "Dusty",
  "Eager",
  "Earthy",
  "Ember",
  "Emerald",
  "Fancy",
  "Fiery",
  "Fluffy",
  "Fond",
  "Frosty",
  "Gentle",
  "Giddy",
  "Gilded",
  "Golden",
  "Grand",
  "Happy",
  "Hazel",
  "Humble",
  "Icy",
  "Indigo",
  "Iron",
  "Ivory",
  "Jade",
  "Jolly",
  "Keen",
  "Kind",
  "Lemon",
  "Lively",
  "Lucky",
  "Lunar",
  "Merry",
  "Minty",
  "Misty",
  "Mossy",
  "Noble",
  "Opal",
  "Peach",
  "Pearl",
  "Plucky",
  "Polar",
  "Quick",
  "Quiet",
  "Radiant",
  "Rapid",
  "Rose",
  "Royal",
  "Ruby",
  "Rustic",
  "Shiny",
  "Silent",
  "Silky",
  "Silver",
  "Snowy",
  "Solar",
  "Steady",
  "Stormy",
  "Sunny",
  "Swift",
  "Tangy",
  "Tidal",
  "Topaz",
  "Velvet",
  "Vivid",
  "Warm",
  "Witty",
] as const;

const NOUNS = [
  "Acorn",
  "Anchor",
  "Arrow",
  "Badger",
  "Beacon",
  "Beetle",
  "Berry",
  "Blossom",
  "Breeze",
  "Brook",
  "Canyon",
  "Cedar",
  "Cliff",
  "Clover",
  "Comet",
  "Coral",
  "Cosmos",
  "Delta",
  "Dolphin",
  "Dragon",
  "Dune",
  "Eagle",
  "Echo",
  "Falcon",
  "Feather",
  "Fern",
  "Finch",
  "Fjord",
  "Forest",
  "Fox",
  "Frost",
  "Garden",
  "Glade",
  "Grove",
  "Harbor",
  "Hawk",
  "Heron",
  "Island",
  "Lagoon",
  "Lantern",
  "Lark",
  "Leaf",
  "Lion",
  "Lotus",
  "Lynx",
  "Maple",
  "Meadow",
  "Meteor",
  "Nebula",
  "Oak",
  "Ocean",
  "Octopus",
  "Orchid",
  "Otter",
  "Owl",
  "Panda",
  "Pearl",
  "Pebble",
  "Pine",
  "Planet",
  "Quasar",
  "Raven",
  "Reef",
  "Ridge",
  "River",
  "Rocket",
  "Sage",
  "Shore",
  "Sparrow",
  "Spire",
  "Star",
  "Stone",
  "Summit",
  "Thunder",
  "Tiger",
  "Tulip",
  "Valley",
  "Wave",
  "Whale",
  "Willow",
] as const;

function mix32(h: number): number {
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b) >>> 0;
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35) >>> 0;
  h ^= h >>> 16;
  return h >>> 0;
}

function hash32(input: string, seed: number): number {
  let h = seed >>> 0;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i) & 0xff;
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return mix32(h);
}

export function memorableNameFromString(input: string): string {
  const seeded = `codesense:memorable:v1\u0000${input}`;
  const adjIndex = hash32(seeded, 0x811c9dc5) % ADJECTIVES.length;
  const nounIndex = hash32(seeded, 0x9e3779b1) % NOUNS.length;
  return `${ADJECTIVES[adjIndex]} ${NOUNS[nounIndex]}`;
}

const HANDLE_PREFIX_PATTERNS = [/^term-tab-/, /^pty-session-/, /^remote-tab-/, /^remote-shell-/];

export function looksLikeOpaqueSessionHandle(handle: string): boolean {
  const trimmed = handle.trim();
  if (!trimmed) return true;
  return HANDLE_PREFIX_PATTERNS.some((p) => p.test(trimmed));
}
