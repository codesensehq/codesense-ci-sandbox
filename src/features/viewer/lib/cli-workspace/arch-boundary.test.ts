// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync } from "fs";
import { fileURLToPath } from "url";

/**
 * OC-4 AC7 — architecture enforcement, in the repo's proven source-scan style
 * (mirrors `src/features/git-sync/lib/no-direct-invoke.test.ts`). It encodes the
 * dependency direction
 *
 *   CodeSense adapter → CLI workspace public API → framework/transport adapters
 *   → Rust terminal domain
 *
 * in two guards:
 *
 *   A. Contract purity — the new `cli-workspace/**` contract modules import NONE
 *      of Tauri, the global viewer store, React, Zustand, xterm, the DOM, or the
 *      dashboard/pipeline/surface-sync features. This is green now and stays
 *      green: it is the FR13/NFR6 "public API leaks no implementation types"
 *      guarantee.
 *
 *   B. Seam direction with a burn-down allowlist — the existing CLI seam modules
 *      must not reach `@/tauri/commands`/`invoke(` or the global viewer store
 *      except for today's known violators, each tagged with the extraction story
 *      (OC-9/OC-10) that removes it. The gate is green now and only shrinks; a
 *      NEW forbidden import fails immediately. The allowlist is also asserted to
 *      be free of stale entries, so when OC-9 removes an import the entry must be
 *      removed too — the list cannot calcify.
 *
 * Comments are stripped before scanning: these files discuss the forbidden
 * imports in prose, and only real code imports are violations.
 */

function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
}

const FORBIDDEN = {
  tauri: /from\s+["']@\/tauri\/commands["']|\binvoke\s*[(<]/,
  viewerStore: /viewer-store|useViewerStore/,
  // Requires the `@/features/<feature>/` path so a `@/features/shell/...`
  // surface-sync *retry* hook is not mistaken for surface-sync *presentation*.
  upward: /@\/features\/(dashboard|pipeline|surface-sync)\//,
  react: /from\s+["']react["']/,
  zustand: /from\s+["']zustand["']/,
  xterm: /@xterm\//,
  dom: /\bdocument\.|\bwindow\./,
} as const;

function hits(src: string, re: RegExp): boolean {
  return re.test(stripComments(src));
}

// ---- Guard A: contract purity -------------------------------------------------

const contractSources = import.meta.glob("./*.ts", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

describe("OC-4 arch guard A — contract module purity (AC4/AC7/NFR6)", () => {
  const modules = Object.entries(contractSources).filter(([path]) => !path.includes(".test."));

  it("globbed the contract modules (guards against a silent no-match)", () => {
    // identities, lifecycle, activation, diagnostics, ports.
    expect(modules.length).toBeGreaterThanOrEqual(5);
  });

  it("no contract module imports Tauri, the viewer store, React, Zustand, xterm, the DOM, or dashboard/pipeline/surface-sync", () => {
    const offenders = modules
      .filter(([, src]) => Object.values(FORBIDDEN).some((re) => hits(src, re)))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});

// ---- Guard B: CLI seam direction, computed burn-down baseline -----------------

/** A file is CLI-workspace seam if its name marks it terminal / PTY / CLI /
 *  agent-session surface. Coverage is defined by this pattern over the seam
 *  directories — not a hand-curated file list — so the seam cannot be silently
 *  under-scanned. */
const SEAM_NAME =
  /(terminal|pty|shell-terminal|workspace-terminal|foreground-tile|focus-cascade|directional-focus|open-epic-terminal|agent-session|send-to-terminal|cli-bottom|workspace-cycle|team-workspace-view)/i;

/** The seam directories, relative to this test. */
const SEAM_DIRS = [
  "../",
  "../../components/",
  "../../views/",
  "../../hooks/",
  "../../../../shared/lib/",
  "../../../../shared/hooks/",
];

/** Every seam-named, non-test source file across the seam dirs, with its source. */
function collectSeamFiles(): Array<[string, string]> {
  const files: Array<[string, string]> = [];
  for (const dir of SEAM_DIRS) {
    const abs = fileURLToPath(new URL(dir, import.meta.url));
    for (const name of readdirSync(abs)) {
      if (!/\.(ts|tsx)$/.test(name) || /\.test\.|\.stories\./.test(name)) continue;
      if (!SEAM_NAME.test(name)) continue;
      files.push([name, readFileSync(abs + name, "utf-8")]);
    }
  }
  return files;
}

const seamFiles = collectSeamFiles();

function violationFlags(src: string): string {
  return `${hits(src, FORBIDDEN.tauri) ? "T" : "-"}${hits(src, FORBIDDEN.viewerStore) ? "V" : "-"}`;
}

/** The burn-down baseline: every CLI-seam file that still imports terminal Tauri
 *  commands ("T") and/or the global viewer store ("V"). This set is *computed*
 *  from the seam and pinned here, so it is honest and self-maintaining — a new
 *  seam file or a new forbidden import changes the computed set and fails the test
 *  until this map is updated, and it can only *shrink* as files route the seam
 *  through the cli-workspace transport seam + store door. A clean ("--") file
 *  never appears.
 *
 *  OC-9 removed the 16 behaviour-safe entries (type-only re-routes, the transport
 *  `*-source.ts` seam, and imperative `getState()` access). OC-9b removed the six
 *  reactive-selector surfaces — five to `--`, and the app-shell host to the
 *  HOST_BASELINE exemption below. The burn-down for seam files is now COMPLETE:
 *  this set is empty and must stay so. */
const VIOLATION_BASELINE: Record<string, string> = {};

/** The app-shell composition root (`team-workspace-view.tsx`) is the CodeSense
 *  *consumer* side of the AC7 direction: it assembles dashboard/pipeline/
 *  surface-sync chrome around the CLI and legitimately reads non-CLI viewer state
 *  (sidebar, nav, right-pane, HTML-trust, file tabs, …) and calls non-CLI host
 *  Tauri commands — none of which belong inside the CLI capability or the terminal
 *  transport seam. Its genuinely-CLI reads DO route through the capability (see the
 *  `store/terminal-selectors` hooks it consumes); this exemption covers only its
 *  host-composition access. It is a permanent, documented exemption — NOT a pending
 *  burn-down target (OC-9b is the last owner of the mount path). */
const HOST_BASELINE: Record<string, string> = {
  "team-workspace-view.tsx": "TV",
};

/** The only seam file allowed to import dashboard/pipeline/surface-sync: the same
 *  app-shell host/composition view. Exempt from the upward rule only. */
const UPWARD_BASELINE = ["team-workspace-view.tsx"];

describe("OC-4 arch guard B — CLI seam direction, computed burn-down baseline (AC7)", () => {
  it("scanned a non-trivial CLI seam (guards against a silent no-match)", () => {
    expect(seamFiles.length).toBeGreaterThanOrEqual(30);
  });

  it("the computed seam violator set matches the pinned baseline exactly (burn-down empty; only the documented host exemption remains)", () => {
    const actual: Record<string, string> = {};
    for (const [name, src] of seamFiles) {
      const flags = violationFlags(src);
      if (flags !== "--") actual[name] = flags;
    }
    expect(actual).toEqual({ ...VIOLATION_BASELINE, ...HOST_BASELINE });
  });

  it("only the app-shell host view imports dashboard/pipeline/surface-sync presentation", () => {
    const importers = seamFiles
      .filter(([, src]) => hits(src, FORBIDDEN.upward))
      .map(([name]) => name)
      .sort();
    expect(importers).toEqual([...UPWARD_BASELINE].sort());
  });
});

// ---- Guard C: the module impl layer is the single Tauri/store door -------------

/** The cli-workspace module's impl layer lives in subdirectories the contract-
 *  purity glob (Guard A, `./*.ts`) and the seam scan (Guard B, sibling dirs) do
 *  not reach. This guard closes that blind spot with the positive analogue of
 *  git-sync's `no-direct-invoke` exempt-list: within the module the ONLY files
 *  allowed to import `@/tauri/commands` are the transport `*-source.ts` seam
 *  files, and the ONLY files allowed to import the viewer store are those under
 *  `store/` — the imperative accessor plus the reactive read hooks that wrap the
 *  store's pure slot selectors — so a stray Tauri/store import anywhere else in
 *  the module fails here rather than hiding in an unscanned subdir. */
function collectModuleImplFiles(): Array<[string, string]> {
  const files: Array<[string, string]> = [];
  const walk = (relDir: string) => {
    const abs = fileURLToPath(new URL(relDir, import.meta.url));
    for (const entry of readdirSync(abs, { withFileTypes: true })) {
      const rel = `${relDir}${entry.name}`;
      if (entry.isDirectory()) walk(`${rel}/`);
      else if (/\.(ts|tsx)$/.test(entry.name) && !/\.test\./.test(entry.name))
        files.push([rel, readFileSync(abs + entry.name, "utf-8")]);
    }
  };
  for (const dir of ["./adapters/", "./store/", "./transport/"]) {
    try {
      walk(dir);
    } catch {
      // A subdir may not exist yet (e.g. adapters/ is assembled by a later consumer).
    }
  }
  return files;
}

describe("OC-9 arch guard C — the module impl layer is the single Tauri/store door (AC4)", () => {
  const implFiles = collectModuleImplFiles();

  it("scanned a non-trivial impl layer (guards against a silent no-match if a dir is renamed/moved)", () => {
    // 3 transport `*-source.ts` + the store door files (viewer-access +
    // the reactive selector hooks). Mirrors Guard A/B's count floor: without
    // it, renaming `transport/` or `store/` would make every check below pass
    // vacuously (zero files scanned).
    expect(implFiles.length).toBeGreaterThanOrEqual(4);
  });

  it("only transport `*-source.ts` files import Tauri commands", () => {
    const offenders = implFiles
      .filter(([, src]) => hits(src, FORBIDDEN.tauri))
      .map(([path]) => path)
      .filter((path) => !/\/transport\/[\w.-]+-source\.ts$/.test(path));
    expect(offenders).toEqual([]);
  });

  it("only store/ files import the viewer store", () => {
    const offenders = implFiles
      .filter(([, src]) => hits(src, FORBIDDEN.viewerStore))
      .map(([path]) => path)
      .filter((path) => !/\/store\/[\w.-]+\.tsx?$/.test(path));
    expect(offenders).toEqual([]);
  });

  it("the impl layer never depends upward on dashboard/pipeline/surface-sync presentation", () => {
    const offenders = implFiles
      .filter(([, src]) => hits(src, FORBIDDEN.upward))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});

// ---- The rule fires (a guard that never fails proves nothing) ------------------

describe("OC-4 arch guard — the forbidden-import predicate actually fires (AC7)", () => {
  it("detects a synthetic Tauri import", () => {
    expect(hits(`import { invoke } from "@/tauri/commands";`, FORBIDDEN.tauri)).toBe(true);
    expect(hits(`invoke("spawn_pty");`, FORBIDDEN.tauri)).toBe(true);
  });

  it("detects a synthetic viewer-store import", () => {
    expect(
      hits(`import { useViewerStore } from "@/shared/stores/viewer-store";`, FORBIDDEN.viewerStore),
    ).toBe(true);
  });

  it("ignores the same tokens inside comments", () => {
    expect(hits(`// we must never invoke("x") here`, FORBIDDEN.tauri)).toBe(false);
  });
});
