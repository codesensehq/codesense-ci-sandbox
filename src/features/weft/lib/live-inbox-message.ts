// SPDX-License-Identifier: MIT
import type { MessageContentType } from "@/tauri/commands";

/** Only HTML mail from an agent to the human runs as a live dashboard. */
export function isLiveInboxMessage(m: {
  to: string | null;
  from: string;
  contentType?: MessageContentType | null;
}): boolean {
  return m.to === "human" && m.from !== "human" && m.contentType === "html";
}
