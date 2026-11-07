// SPDX-License-Identifier: MIT
import { openPath } from "@tauri-apps/plugin-opener";

import type { LoadedOrg } from "@/shared/types/registry";
import { loadOrgCmd } from "@/tauri/commands";

/**
 * Loads an org's registry files (org.yml / people.yml / teams.yml) from
 * the local working tree (SW-ID-9), so unpublished local edits are
 * reflected without waiting on publish. `remoteUrl` is carried through
 * for the org's remote identity only; it does not select the read source.
 * Returns a LoadedOrg: `org` is populated when every file is clean,
 * otherwise `file_errors` lists per-file failures and `partial` holds
 * whichever raw shapes did parse.
 */
export async function loadOrg(
  slug: string,
  orgRoot: string,
  remoteUrl: string | null,
  principal: string | null = null,
): Promise<LoadedOrg> {
  return loadOrgCmd(slug, orgRoot, remoteUrl, principal);
}

/**
 * Opens the given file in the OS default editor via tauri-plugin-opener.
 * Used by registry-error UI surfaces (e.g. C.6a Dashboard) for "Open to
 * repair" buttons. Centralized so every caller reuses the same path.
 */
export async function openRegistryFileForRepair(filePath: string): Promise<void> {
  await openPath(filePath);
}
