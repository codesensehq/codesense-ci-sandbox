// SPDX-License-Identifier: MIT
import { SESSION_STATE_CHANGED_EVENT } from "@/features/viewer/lib/session-events";

import { loadSessionInbox, type SessionInboxDto } from "../api";

import { useInboxRead, type InboxRead } from "./use-inbox-read";

const IDENTITY_EVENTS = [SESSION_STATE_CHANGED_EVENT] as const;

/** Everything `handle` sent, received, or holds via its roles — live, across its own and every synced team. */
export function useSessionInbox(handle: string | null): InboxRead<SessionInboxDto> {
  return useInboxRead(handle, loadSessionInbox, IDENTITY_EVENTS);
}
