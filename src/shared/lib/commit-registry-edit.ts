// SPDX-License-Identifier: MIT
import {
  commitRegistryEditLocal,
  type LocalEditOutcome,
  type RegistryEdit,
} from "@/tauri/commands";

/**
 * The single seam through which every context-repo registry mutation
 * commits.
 *
 * SW-OB-8 (local-only registry edits): all registry edits are a LOCAL
 * git commit, regardless of `org.remote_url`. A user who joined an org
 * by clone has a `remote_url`, but their edits still commit locally —
 * "local-only" is unconditional here, not a function of whether a
 * remote exists.
 *
 * This is deliberately the ONLY place the local-vs-remote routing
 * decision lives. The retired remote/PR path (`commitRegistryEditAsPr`)
 * stays registered but has no callers. When a future epic re-introduces
 * automated remote writes, it re-adds the `remote_url ? PR : local`
 * branch HERE — a one-line seam, not a sweep across six call sites.
 */
export async function commitRegistryEdit(
  githubUserId: number,
  orgSlug: string,
  edit: RegistryEdit,
): Promise<LocalEditOutcome | void> {
  return commitRegistryEditLocal(githubUserId, orgSlug, edit);
}
