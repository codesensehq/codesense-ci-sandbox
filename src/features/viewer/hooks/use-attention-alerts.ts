// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { startAttentionAlerts } from "@/features/viewer/lib/attention-alerts-controller";

/**
 * The dock badge and the OS notifications for terminal agents that need you
 * (a permission prompt, a question, a crash). Mounted once at the app root.
 */
export function useAttentionAlerts(): void {
  useEffect(() => startAttentionAlerts(), []);
}
