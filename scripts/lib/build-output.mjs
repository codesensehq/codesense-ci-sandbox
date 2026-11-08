// SPDX-License-Identifier: MIT
import { readdirSync, renameSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** The newest modification time (ms) under `paths`, walking directories as Cargo does. */
export function newestMtime(paths) {
  let newest = 0;
  const visit = (path) => {
    let stat;
    try {
      stat = statSync(path);
    } catch {
      return;
    }
    newest = Math.max(newest, stat.mtimeMs);
    if (stat.isDirectory()) for (const name of readdirSync(path)) visit(join(path, name));
  };
  for (const path of paths) visit(path);
  return newest;
}

/**
 * Write a generated archive through a temp file and rename, so concurrent builds never read
 * a torn one, dated to its newest input. Cargo reruns a build script when a watched file is
 * newer than the script's last start, and Tauri watches every bundle resource: an archive
 * dated "now" would rebuild the app once more on the next cargo command.
 */
export function writeBuildOutput(output, bytes, inputs) {
  const temp = `${output}.${process.pid}.${Date.now()}.tmp`;
  try {
    writeFileSync(temp, bytes);
    const newest = newestMtime(inputs);
    if (newest > 0) utimesSync(temp, new Date(), new Date(newest));
    renameSync(temp, output);
  } catch (error) {
    rmSync(temp, { force: true });
    throw error;
  }
}
