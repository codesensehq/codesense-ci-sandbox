// SPDX-License-Identifier: MIT
import { basename } from "@/shared/lib/basename";
import {
  createFile,
  deleteContentFile,
  ingestFileIntoContext,
  moveContentFile,
} from "@/tauri/commands";

/**
 * SW-CP-14 / SW-VE-6: orchestration for the nav's structural content gestures
 * (create + delete + rename + move). Pure path logic is split out so it is
 * unit-testable without Tauri; the async wrappers take their command deps so
 * they can be mocked in vitest (mirrors `create-canvas.ts`).
 */

export type StructuralResult = { ok: true } | { ok: false; error: string };

// The basename (last path segment) now lives in one shared util; re-exported
// here under the historical name so existing importers are unaffected.
export { basename as baseName } from "@/shared/lib/basename";

/**
 * A name/target collision from the gated commands — `create_file` errors
 * "File already exists" and `move_content_file` errors "target already exists".
 * Recognising it lives HERE, next to the command contract, so the UI checks a
 * boolean (`result.collision`) instead of re-matching backend error text.
 */
function isCollision(message: string): boolean {
  return /already exists/i.test(message);
}

/**
 * Validate a single path segment (a basename, not a path). Returns the trimmed
 * name, or `null` if it is empty, contains a path separator, or is a `.`/`..`
 * traversal token. Shared by every gesture that composes a target path, so the
 * name rule lives in exactly one place (the backend also validates — this is
 * fast UX feedback).
 */
export function sanitizeSegment(name: string): string | null {
  const trimmed = name.trim();
  if (
    !trimmed ||
    trimmed.includes("/") ||
    trimmed.includes("\\") ||
    trimmed === "." ||
    trimmed === ".."
  ) {
    return null;
  }
  return trimmed;
}

/**
 * Resolve a rename of `oldPath` to the sibling `newName` (a basename, not a
 * path). Returns `null` for an invalid name so the caller can reject before
 * issuing a command.
 */
export function computeRenameTarget(oldPath: string, newName: string): string | null {
  const trimmed = sanitizeSegment(newName);
  if (!trimmed) return null;
  const stripped = oldPath.replace(/\/+$/, "");
  const idx = stripped.lastIndexOf("/");
  const dir = idx >= 0 ? stripped.slice(0, idx) : "";
  return dir ? `${dir}/${trimmed}` : trimmed;
}

/**
 * Resolve a new child path `dirPath/name`, validating the name. Trailing
 * slashes on `dirPath` are tolerated. Returns `null` for an invalid name.
 */
export function computeChildTarget(dirPath: string, name: string): string | null {
  const trimmed = sanitizeSegment(name);
  if (!trimmed) return null;
  const dir = dirPath.replace(/\/+$/, "");
  return dir ? `${dir}/${trimmed}` : trimmed;
}

/** Delete a context-layer content file/dir, propagating across the sync boundary. */
export async function deleteContent(
  path: string,
  deps: { deleteContentFile: typeof deleteContentFile } = { deleteContentFile },
): Promise<StructuralResult> {
  try {
    await deps.deleteContentFile(path);
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * Rename `oldPath` to the sibling `newName`. No-op success when the name is
 * unchanged; rejects an invalid name before issuing the command.
 */
export async function renameContent(
  oldPath: string,
  newName: string,
  deps: { moveContentFile: typeof moveContentFile } = { moveContentFile },
): Promise<StructuralResult & { newPath?: string }> {
  const newPath = computeRenameTarget(oldPath, newName);
  if (!newPath) return { ok: false, error: "Invalid name" };
  if (newPath === oldPath.replace(/\/+$/, "")) return { ok: true, newPath };
  try {
    await deps.moveContentFile(oldPath, newPath);
    return { ok: true, newPath };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * SW-VE-6: create a new empty file `name` inside `dirPath` (a context-layer
 * directory) via the gated `create_file` command. Rejects an invalid name
 * before issuing the command. The backend errors on an existing file, surfaced
 * here as `{ ok: false, path }` so the caller can offer Replace on the resolved
 * path (never a bespoke write). Returns the composed `path` on success so the
 * caller can open the new file read-only.
 */
export async function createContent(
  dirPath: string,
  name: string,
  deps: { createFile: typeof createFile } = { createFile },
): Promise<StructuralResult & { path?: string; collision?: boolean }> {
  const path = computeChildTarget(dirPath, name);
  if (!path) return { ok: false, error: "Invalid name" };
  try {
    await deps.createFile(path, "");
    return { ok: true, path };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    return { ok: false, error, path, collision: isCollision(error) };
  }
}

/**
 * Insert a numeric disambiguator before the extension so a "Keep both" create
 * never collides: `report.md`,2 → `report-2.md`; `notes`,3 → `notes-3`; a
 * dotfile `.env`,2 → `.env-2` (appended — a leading dot is not an extension).
 */
export function suffixName(name: string, n: number): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return `${name}-${n}`;
  return `${name.slice(0, dot)}-${n}${name.slice(dot)}`;
}

/**
 * SW-VE-6: NON-destructive collision resolution ("Keep both"). Create `name`;
 * on a name collision, retry with `name-2`, `name-3`, … until one succeeds.
 * Never deletes or overwrites an existing file — so it needs no structural
 * fan-out reasoning and can never lose data (unlike a destructive Replace).
 * Stops on any non-collision error. Returns the created `path`.
 */
export async function createContentUnique(
  dirPath: string,
  name: string,
  deps: { createFile: typeof createFile } = { createFile },
): Promise<StructuralResult & { path?: string }> {
  for (let n = 1; n <= 99; n++) {
    const candidate = n === 1 ? name : suffixName(name, n);
    const r = await createContent(dirPath, candidate, deps);
    if (r.ok || !r.collision) return r; // created, or a real (non-collision) error
  }
  return { ok: false, error: "Too many files with that name" };
}

/**
 * SW-VE-6: move `fromPath` into `toDirPath` (keeping its basename), across
 * directories or hydration scopes, via the gated `move_content_file` command
 * (which auto-fans-out to every projected worktree). A no-op success when the
 * destination dir already holds it. The backend errors on a target collision,
 * surfaced as `{ ok: false, newPath }` so the caller can offer a resolution.
 */
export async function moveContentTo(
  fromPath: string,
  toDirPath: string,
  deps: { moveContentFile: typeof moveContentFile } = { moveContentFile },
): Promise<StructuralResult & { newPath?: string; collision?: boolean }> {
  const newPath = computeChildTarget(toDirPath, basename(fromPath));
  if (!newPath) return { ok: false, error: "Invalid destination" };
  if (newPath === fromPath.replace(/\/+$/, "")) return { ok: true, newPath };
  try {
    await deps.moveContentFile(fromPath, newPath);
    return { ok: true, newPath };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    return { ok: false, error, newPath, collision: isCollision(error) };
  }
}

/**
 * SW-VE-7: ingest a drag-dropped OS file (`srcPath`, an absolute path OUTSIDE
 * the managed tree) into `dirPath` under `name`, via the gated
 * `ingest_file_into_context` command (server-side byte copy — binary- and
 * text-safe). Mirrors {@link createContent}: rejects an invalid name before the
 * command, and surfaces a name collision as `{ ok: false, path, collision }` so
 * the caller can offer the same non-destructive Keep-both resolution. Returns
 * the composed `path` on success so the caller can open the new file read-only.
 */
export async function ingestFile(
  srcPath: string,
  dirPath: string,
  name: string,
  deps: { ingestFileIntoContext: typeof ingestFileIntoContext } = { ingestFileIntoContext },
): Promise<StructuralResult & { path?: string; collision?: boolean }> {
  const path = computeChildTarget(dirPath, name);
  if (!path) return { ok: false, error: "Invalid name" };
  try {
    await deps.ingestFileIntoContext(srcPath, path);
    return { ok: true, path };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    return { ok: false, error, path, collision: isCollision(error) };
  }
}

/**
 * SW-VE-7: NON-destructive collision resolution for ingest ("Keep both") — the
 * ingest twin of {@link createContentUnique}. Ingests `srcPath` as `name`; on a
 * name collision, retries `name-2`, `name-3`, … until one succeeds. Never
 * overwrites. Stops on any non-collision error. Returns the created `path`.
 */
export async function ingestFileUnique(
  srcPath: string,
  dirPath: string,
  name: string,
  deps: { ingestFileIntoContext: typeof ingestFileIntoContext } = { ingestFileIntoContext },
): Promise<StructuralResult & { path?: string }> {
  for (let n = 1; n <= 99; n++) {
    const candidate = n === 1 ? name : suffixName(name, n);
    const r = await ingestFile(srcPath, dirPath, candidate, deps);
    if (r.ok || !r.collision) return r;
  }
  return { ok: false, error: "Too many files with that name" };
}
