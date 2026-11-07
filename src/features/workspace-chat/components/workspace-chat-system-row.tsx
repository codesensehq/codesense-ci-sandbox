// SPDX-License-Identifier: MIT
import type { ChatDensity } from "../lib/chat-density";
import { formatMessageTime } from "../lib/format-time";

export interface WorkspaceChatSystemRowProps {
  text: string;
  createdAtMs: number;
  density: ChatDensity;
}

/** A centred pill for chat lifecycle events (created, joined, archived, …). */
export function WorkspaceChatSystemRow({
  text,
  createdAtMs,
}: WorkspaceChatSystemRowProps): React.JSX.Element {
  return (
    <div className="wc-sys" data-testid="workspace-chat-system">
      <span className="wc-sys__pill">
        <span className="wc-sys__text">{text}</span>
        <time
          dateTime={new Date(createdAtMs).toISOString()}
          title={new Date(createdAtMs).toLocaleString()}
        >
          {formatMessageTime(createdAtMs)}
        </time>
      </span>
    </div>
  );
}
