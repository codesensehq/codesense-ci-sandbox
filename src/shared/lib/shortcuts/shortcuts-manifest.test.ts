// SPDX-License-Identifier: MIT
// The docs' shortcuts reference (arc-n-reference/shortcuts.manifest.json) must match
// the registry: the same bound ids, labels and default chords on both platforms, and
// a menu-bar path only where the native menu bar really builds one.
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { NAV_MENU_ITEMS, SHORTCUT_ACTIONS } from "./registry";

type Entry = { id: string; label: string; mac: string; winLinux: string; menu: string };
type Groups = { groups: { shortcuts: Entry[] }[] };

const ROOT = join(__dirname, "../../../..");
const CONTENT = join(ROOT, "codesense-docs/getting-started/content");
const readJson = <T>(path: string) => JSON.parse(readFileSync(path, "utf8")) as T;

const manifest = readJson<Groups>(join(CONTENT, "arc-n-reference/shortcuts.manifest.json"));
const entries = manifest.groups.flatMap((g) => g.shortcuts);

// The hardcoded native items, read from src-tauri/src/menu.rs: each MENU_ITEMS label in
// the submenu whose builder adds it. Settings… is built just before its CodeSense menu.
function nativeMenu(): Record<string, string> {
  const rust = readFileSync(join(ROOT, "src-tauri/src/menu.rs"), "utf8");
  const table = rust.slice(rust.indexOf("pub const MENU_ITEMS"));
  const labels = new Map(
    [...table.slice(0, table.indexOf("];")).matchAll(/\(\s*"([a-z0-9._-]+)",\s*"([^"]+)",/g)].map(
      (m) => [m[1], m[2]],
    ),
  );
  const body = rust.slice(rust.indexOf("fn build_menu_internal"));
  const menus = [...body.matchAll(/SubmenuBuilder::new\(app, "([^"]+)"\)/g)];
  const paths: Record<string, string> = {};
  for (const item of body.matchAll(/build_item\(app, "([a-z0-9._-]+)"/g)) {
    const menu =
      menus.filter((m) => m.index! < item.index!).at(-1) ??
      menus.find((m) => m.index! > item.index!);
    paths[item[1]] = `${menu![1]} → ${labels.get(item[1])}`;
  }
  expect(Object.keys(paths).sort()).toEqual([...labels.keys()].sort());
  return paths;
}

// View → Panels lists the right-pane panels the frontend registers (menu.rs builds it
// from that list), here by the shortcut that reveals each built-in panel.
const PANELS: Record<string, string> = { "viewer.toggle-inbox-rollup": "View → Panels → Inbox" };

// The manifest writes modifiers in any order, so compare as a sorted set of glyphs.
const sortedGlyphs = (chord: string) => [...chord].sort().join("");
const otherChord = (keystroke: string) =>
  keystroke.replace("CmdOrCtrl", "Ctrl").replace(/Arrow(Left|Right|Up|Down)/g, "$1");

// Platform defaults are read when the registry module loads, so load it fresh per platform.
// Each default comes back raw and as the app displays it on that platform.
async function defaultsOn(platform: string): Promise<Map<string, { raw: string; shown: string }>> {
  vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
  vi.resetModules();
  const fresh = await import("./registry");
  const { formatKeystrokeDisplay } = await import("./keystroke");
  return new Map(
    fresh.SHORTCUT_ACTIONS.map((a) => [
      a.id,
      { raw: a.defaultKeystroke, shown: formatKeystrokeDisplay(a.defaultKeystroke) },
    ]),
  );
}

describe("shortcuts manifest", () => {
  afterEach(() => vi.restoreAllMocks());

  it("lists every bound registry action once, with its label", () => {
    const bound = SHORTCUT_ACTIONS.filter((a) => a.defaultKeystroke);
    expect(entries.map((e) => e.id).sort()).toEqual(bound.map((a) => a.id).sort());
    for (const e of entries) {
      expect(e.label, e.id).toBe(SHORTCUT_ACTIONS.find((a) => a.id === e.id)?.label);
    }
  });

  it("gives each action its registry default on macOS and on Windows/Linux", async () => {
    const mac = await defaultsOn("MacIntel");
    const other = await defaultsOn("Win32");
    for (const e of entries) {
      expect(sortedGlyphs(e.mac), e.id).toBe(sortedGlyphs(mac.get(e.id)?.shown ?? ""));
      expect(e.winLinux, e.id).toBe(otherChord(other.get(e.id)?.raw ?? ""));
    }
  });

  it("names a menu-bar path only where the native menu builds one", () => {
    const native = nativeMenu();
    expect(native["settings.open"]).toBe("CodeSense → Settings…");
    expect(native["viewer.toggle-console"]).toBe("View → Toggle Console");
    const goMenu = new Map(NAV_MENU_ITEMS.map((n) => [n.id, `Go → ${n.label}`]));
    for (const e of entries) {
      const path = native[e.id] ?? PANELS[e.id] ?? goMenu.get(e.id);
      if (path) expect(e.menu, e.id).toBe(path);
      else expect(e.menu, e.id).not.toMatch(/^(CodeSense|File|Edit|View|Go) → /);
    }
  });

  it("is what the generated shortcuts.json that the reference page renders holds", () => {
    const generated = readJson<Groups & { totalCount: number }>(
      join(CONTENT, "generated/shortcuts.json"),
    );
    expect(generated.groups).toEqual(manifest.groups);
    expect(generated.totalCount).toBe(entries.length);
  });

  it("matches the static counts the reference pages show before they load", () => {
    const schemas = readJson<{ schemas: unknown[] }>(join(CONTENT, "generated/schemas.json"));
    expect(readFileSync(join(CONTENT, "arc-n-reference/index.html"), "utf8")).toContain(
      `<data-count id="schemas">${schemas.schemas.length}</data-count>`,
    );
    for (const page of ["arc-n-reference/index.html", "arc-n-reference/keyboard-shortcuts.html"]) {
      const html = readFileSync(join(CONTENT, page), "utf8");
      expect(html, page).toContain(`<data-count id="shortcuts">${entries.length}</data-count>`);
    }
  });
});
