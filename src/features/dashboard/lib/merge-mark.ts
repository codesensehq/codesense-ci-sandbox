// SPDX-License-Identifier: MIT
import { extensionOf } from "@/shared/lib/viewer-file-type";

/**
 * SW-CP-27: the merge-mark format gate. The right-click "Mark as merge file"
 * toggle is enabled ONLY for the structured config types CP-24's forward-merge
 * engine can deterministically deep-merge.
 *
 * This mirrors the Rust source of truth `ConfigFormat::detect`
 * (`src-tauri/src/shared/config_format.rs`) — extension-only, lowercased. Keep
 * this set in lockstep with that match arm: a drift where the UI offers a mark
 * CP-24 would reject at set-time is the exact bug this gate exists to prevent.
 * (The backend still re-rejects a non-structured path — this is the UX gate, not
 * the security boundary.)
 */
const MERGE_MARKABLE_EXTENSIONS = new Set(["json", "jsonc", "json5", "yaml", "yml", "toml"]);

/**
 * Is `path` a structured config file the CP-24 merge engine can deep-merge —
 * i.e. may the "Mark as merge file" toggle be offered for it? Extension-only
 * (via the shared `extensionOf` rule), to match `ConfigFormat::detect` exactly.
 */
export function canMergeMark(path: string): boolean {
  return MERGE_MARKABLE_EXTENSIONS.has(extensionOf(path) ?? "");
}
