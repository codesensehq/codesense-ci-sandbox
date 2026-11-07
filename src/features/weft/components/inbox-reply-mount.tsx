// SPDX-License-Identifier: MIT
import {
  findTabLocationBySessionId,
  selectRunningTerminalTabIndex,
  useViewerStore,
} from "@/shared/stores/viewer-store";

import { useSessionLiveness } from "../hooks/use-session-liveness";
import type { InboxItem } from "../lib/inbox-model";
import { InboxAgentReplyComposer } from "./inbox-agent-reply-composer";

/**
 * SLW-TL-7 — resolves an Inbox row's liveness (the SAME roster + tab signals the CLI door
 * uses) and renders the durable {@link InboxAgentReplyComposer} in the matching mode. Live +
 * tab-reachable → the reply types straight into the waiting agent; otherwise (exited /
 * headless / gone) → the SAME "Reply" re-dispatches a continuation. The row keys this off
 * `need`, so a question row always gets a real, non-dead-ending Reply under one label.
 */
export function InboxReplyMount({
  item,
  onRedispatch,
  onClose,
  onSent,
}: {
  item: InboxItem;
  onRedispatch: (item: InboxItem, reply: string) => Promise<void>;
  onClose: () => void;
  onSent?: () => void;
}): React.JSX.Element {
  const liveId = item.row.liveSessionId ?? null;
  const liveness = useSessionLiveness(liveId);
  // Reachable-live = a live handle that resolves to a running terminal tab here (the inline
  // send target). A live-but-tabless or exited run falls to the re-dispatch path.
  const reachable =
    !!liveId &&
    !!findTabLocationBySessionId(selectRunningTerminalTabIndex(useViewerStore.getState()), liveId);
  const live = reachable && liveness.alive;
  return (
    <InboxAgentReplyComposer
      item={item}
      live={live}
      liveSessionId={live ? liveId : null}
      onRedispatch={onRedispatch}
      onClose={onClose}
      onSent={onSent}
    />
  );
}
