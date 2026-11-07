// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { logger } from "@/shared/lib/logger";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { AGENT_INBOX_CHANGED_EVENT, AGENT_STATE_CHANGED_EVENT } from "@/tauri/events";

import type { InboxSource } from "../api";

export interface InboxRead<T> {
  data: T | null;
  source: InboxSource | null;
  error: string | null;
  reload: () => Promise<void>;
}

const NO_EVENTS: readonly string[] = [];

/**
 * Read `load` now and again on every inbox or agent-state change, and on each of `also` (a
 * stable list of further events). `key` names what is read; a result for an older key is
 * never shown under a newer one.
 */
export function useInboxRead<T>(
  key: string | null,
  /** A stable loader (a module-level function): what it reads is named by `key`. */
  load: (key: string) => Promise<{ dto: T; source: InboxSource }>,
  also: readonly string[] = NO_EVENTS,
): InboxRead<T> {
  const [state, setState] = useState<{ key: string; data: T; source: InboxSource } | null>(null);
  // The error is kept with its key, like the data: an older key's failure never shows under
  // a newer one.
  const [failure, setFailure] = useState<{ key: string; message: string } | null>(null);
  const seq = useRef(0);

  const reload = useCallback(async () => {
    if (key === null) return;
    const mine = ++seq.current;
    try {
      const { dto, source } = await load(key);
      if (mine !== seq.current) return;
      setState({ key, data: dto, source });
      setFailure(null);
    } catch (err) {
      if (mine !== seq.current) return;
      const message = err instanceof Error ? err.message : String(err);
      logger.info("inbox", `Inbox read failed: ${message}`);
      setFailure({ key, message });
    }
  }, [key, load]);

  useEffect(() => {
    if (key === null) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the read lands asynchronously
    void reload();
    const subs = [AGENT_INBOX_CHANGED_EVENT, AGENT_STATE_CHANGED_EVENT, ...also].map((event) =>
      // No event bus (a plain browser, a test): the read still happens once.
      listen(event, () => void reload()).catch(() => () => {}),
    );
    return () => {
      for (const sub of subs) void sub.then(safeUnlisten).catch(() => {});
    };
  }, [key, reload, also]);

  const current = state && state.key === key ? state : null;
  const error = failure && failure.key === key ? failure.message : null;
  return { data: current?.data ?? null, source: current?.source ?? null, error, reload };
}
