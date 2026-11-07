// SPDX-License-Identifier: MIT
import {
  getViewerFileType,
  isDerivedRenderArtifact,
  stripViewerExtension,
} from "@/shared/lib/viewer-file-type";
import {
  relativeSupportingDocPath,
  upsertSupportingDoc,
} from "@/features/dashboard/lib/attach-supporting-doc";

/**
 * SW-SC-9 — create-canvas affordance.
 *
 * Pure composition over already-shipped rails: SC-1's blank-open, the
 * `create_file` command (errors-on-exist → FE collision-suffix), and SC-2's
 * `supporting_docs:` attach lib. No new backend (AC-7). The React surface
 * (`pipeline-status-nav.tsx`) injects the Tauri commands + the open handler so
 * this orchestration stays pure and unit-testable.
 */

/** Doc extensions that can host a sibling canvas — the AC-2 scope gate.
 *  An explicit allow-list (not `getViewerFileType`, whose unknown-ext fallback
 *  is `"markdown"` — unsafe for a membership test on folders/non-docs). */
const CANVAS_HOST_EXTENSIONS = new Set(["md", "html", "htm"]);

function basenameOf(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash >= 0 ? path.slice(slash + 1) : path;
}

function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/**
 * AC-1/AC-2: "Create canvas" is offered only on markdown/HTML doc nodes — both
 * story docs and plain context docs (architecture, ux, …). Gated purely on the
 * path's extension, since the context-menu state carries no node-type field
 * (Finding D). `.excalidraw`, images, the derived `*.excalidraw.png` render,
 * folders, and generic non-viewable files are all excluded by the positive
 * md/html gate; `isDerivedRenderArtifact` is belt-and-suspenders.
 */
export function canCreateCanvas(path: string): boolean {
  const name = basenameOf(path);
  return CANVAS_HOST_EXTENSIONS.has(extensionOf(name)) && !isDerivedRenderArtifact(name);
}

/**
 * SW-SC-13 (Finding F / AC-9): the single canvas-node gate shared by the
 * "Export as PNG/SVG" items and SC-12's "Attach to story…" guard. A node is a
 * canvas iff the central viewer-file-type map resolves it to "excalidraw" — the
 * derived `*.excalidraw.png` render resolves to "image" and is excluded, with
 * `isDerivedRenderArtifact` as belt-and-suspenders. Routing through
 * `getViewerFileType` (not raw extension surgery) keeps one definition of
 * "is this a canvas" across every context-menu affordance.
 */
export function canExportCanvas(path: string): boolean {
  return getViewerFileType(path) === "excalidraw" && !isDerivedRenderArtifact(basenameOf(path));
}

/**
 * AC-3: the Nth blank-canvas candidate path for a doc — always a flat sibling
 * in the doc's own directory, named after the doc. Attempt 0 →
 * `<base>.excalidraw`; attempt N≥1 → `<base>-(N+1).excalidraw`
 * (`architecture.md` → `architecture.excalidraw`, then `architecture-2`, …).
 * The flat-sibling placement is load-bearing for the `All`-projection path
 * (Finding A) — do not relocate into a subdir.
 */
export function canvasCandidatePath(docPath: string, attempt: number): string {
  const slash = docPath.lastIndexOf("/");
  const dir = slash >= 0 ? docPath.slice(0, slash) : "";
  const base = stripViewerExtension(basenameOf(docPath));
  const suffix = attempt <= 0 ? "" : `-${attempt + 1}`;
  const name = `${base}${suffix}.excalidraw`;
  return dir ? `${dir}/${name}` : name;
}

/**
 * Finding B: `create_file` rejects with `File already exists: <path>` on a name
 * collision (and never truncates) — so collision-suffixing is driven FE-side by
 * catch-and-retry. The create attempt itself is the existence check (no TOCTOU,
 * no pre-stat).
 */
export function isFileExistsError(err: unknown): boolean {
  // Match `create_file`'s exact contract string, not a bare "already exists"
  // substring — so an unrelated lower-layer error ("directory already exists",
  // a wrapped symlink error) is NOT misread as a collision and silently retried.
  return String(err).includes("File already exists");
}

/** Number of distinct candidate names tried before giving up: the bare
 *  `<base>.excalidraw` plus suffixes `-2 … -99` (Finding B collision retry). */
const MAX_CANVAS_ATTEMPTS = 99;

export interface CreateCanvasDeps {
  /** `create_file` — creates a new file, errors on an existing path. */
  createFile: (path: string, content: string) => Promise<void>;
  readFile: (path: string) => Promise<string>;
  /** `write_content_file` — atomic, watcher-suppressed text write. */
  writeContentFile: (path: string, content: string) => Promise<void>;
  /** Open the new canvas in a viewer pane (`onFileSelect`). */
  onOpen: (canvasPath: string) => void;
}

export type CreateCanvasResult =
  | { ok: true; canvasPath: string; attach: "written" | "already" | "failed" }
  | { ok: false; reason: "create-failed" | "name-exhausted" };

/**
 * AC-3/AC-4/AC-5: create a blank sibling `.excalidraw` for `docPath`, open it,
 * then attach it to the doc's `supporting_docs:` frontmatter — one gesture.
 *
 * Order is **create → open → attach** (non-blocking): the blank canvas paints
 * before the frontmatter-write IPC, since correctness is order-independent (the
 * canvas is blank either way and the attach has no bearing on the open). Attach
 * is non-fatal — a failed attach still leaves the canvas created and opened.
 */
export async function createCanvasForDoc(
  docPath: string,
  deps: CreateCanvasDeps,
): Promise<CreateCanvasResult> {
  let canvasPath: string | null = null;
  for (let attempt = 0; attempt < MAX_CANVAS_ATTEMPTS; attempt++) {
    const candidate = canvasCandidatePath(docPath, attempt);
    try {
      await deps.createFile(candidate, "");
      canvasPath = candidate;
      break;
    } catch (err) {
      if (isFileExistsError(err)) continue;
      return { ok: false, reason: "create-failed" };
    }
  }
  if (!canvasPath) return { ok: false, reason: "name-exhausted" };

  // Open immediately — don't block the blank-canvas paint on the attach write.
  // Guarded: a throwing open handler must not become an unhandled rejection
  // (the caller fires this fire-and-forget) nor skip the attach below.
  try {
    deps.onOpen(canvasPath);
  } catch {
    /* open is best-effort — the canvas exists on disk and the attach still runs */
  }

  // Attach (non-fatal): idempotent `supporting_docs:` upsert on the doc.
  try {
    const rel = relativeSupportingDocPath(docPath, canvasPath);
    const text = await deps.readFile(docPath);
    const { text: next, changed } = upsertSupportingDoc(text, rel);
    if (changed) await deps.writeContentFile(docPath, next);
    return { ok: true, canvasPath, attach: changed ? "written" : "already" };
  } catch {
    return { ok: true, canvasPath, attach: "failed" };
  }
}
