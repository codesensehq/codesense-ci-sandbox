// SPDX-License-Identifier: MIT
import { loadInboxRollup, type RollupDto } from "../api";

import { useInboxRead, type InboxRead } from "./use-inbox-read";

const loadRollup = () => loadInboxRollup();

/** Every synced team's mail and role queues, while enabled. */
export function useInboxRollup(enabled: boolean): InboxRead<RollupDto> {
  return useInboxRead(enabled ? "rollup" : null, loadRollup);
}
