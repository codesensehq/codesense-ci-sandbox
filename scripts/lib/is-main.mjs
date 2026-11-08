// SPDX-License-Identifier: MIT
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** Whether the module at `url` is the script node was run with. By real path: run through a
 *  symlink, argv[1] names the link and the module's URL its target. A path that cannot be resolved
 *  (no argv[1], `node -e`, a deleted file) is not the main module. */
export function isMain(url) {
  try {
    return (
      Boolean(process.argv[1]) && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(url))
    );
  } catch {
    return false;
  }
}
