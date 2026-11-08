// SPDX-License-Identifier: MIT
import {
  loadEntries,
  validEntries,
  quarantineReason as formatReason,
} from "../../scripts/lib/quarantine.mjs";

const entries = loadEntries();

export function quarantineReason(id: string): string | undefined {
  const entry = validEntries(entries).find((entry) => entry.id === id);
  return entry ? formatReason(entry) : undefined;
}
