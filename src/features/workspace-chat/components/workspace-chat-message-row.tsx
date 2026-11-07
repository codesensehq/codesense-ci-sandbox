// SPDX-License-Identifier: MIT
import { InboxMessageContent } from "@/features/weft/components/inbox-message-content";
import type { WorkspaceChatMessage } from "@/tauri/commands";

import type { ChatIdentity } from "./workspace-chat-body";
import { formatMessageTime } from "../lib/format-time";

export interface WorkspaceChatMessageRowProps {
  identity?: ChatIdentity;
  message: WorkspaceChatMessage;
  isLead: boolean;
  isHuman: boolean;
  avatarGlyph: string;
  roleColour: string;
}

/**
 * SW-WC-3 — one chat row.
 *
 * Owns the WRAPPER (avatar + handle + timestamp); delegates the body to the
 * shipped `<InboxMessageContent>` per engine design §3.5 MANDATORY reuse
 * directive. The density-specific grammar is CSS-driven under the pane's
 * `data-density` attribute.
 *
 * `live` is intentionally not threaded for v1 (`InboxMessageContent`'s
 * `live` prop takes an `InboxTeam`, which workspace-chat doesn't carry) —
 * HTML bodies render through the sanitized path, matching agent-to-agent
 * behaviour in the inbox. A v2 story may widen the live-dashboard contract
 * so workspace-chat HTML to the human renders as a live dashboard too.
 */
export function WorkspaceChatMessageRow({
  identity,
  message,
  isLead,
  isHuman,
  avatarGlyph,
  roleColour,
}: WorkspaceChatMessageRowProps): React.JSX.Element {
  // Set --wc-role (used by avatar + role pill), --wc-handle (used by handle
  // text), and --wc-mood (used by the mood dot in B). Mood is informational —
  // we don't know live session liveness here, so leave it at the muted
  // fallback the CSS carries.
  const style: React.CSSProperties = {
    ["--wc-role" as string]: identity?.color ?? roleColour,
    ["--wc-handle" as string]: identity?.color ?? roleColour,
  };

  const classes = ["wc-msg"];
  if (isLead) classes.push("wc-msg--lead");
  else classes.push("wc-msg--follow");

  const dataAttrs: Record<string, string> = {};
  if (isHuman) dataAttrs["data-you"] = "true";

  return (
    <div
      className={classes.join(" ")}
      style={style}
      data-testid="workspace-chat-message"
      data-message-id={message.messageId}
      {...dataAttrs}
    >
      <span className="wc-msg__ava" aria-hidden="true">
        {identity?.initials ?? avatarGlyph}
      </span>
      <div className="wc-msg__body">
        <div className="wc-msg__hdr">
          <span className="wc-msg__handle">
            {identity?.name ??
              (isHuman ? message.fromName || "you" : message.fromName || message.from)}
          </span>
          {identity ? (
            <>
              {identity.tag && <span className="wc-msg__tag">{identity.tag}</span>}
              {identity.machine &&
                identity.machine !== "This Mac" &&
                identity.machine !== "local" && (
                  <span className="ibx-mtag">{identity.machine}</span>
                )}
            </>
          ) : (
            <span className="wc-msg__role">{isHuman ? "you" : "crew"}</span>
          )}
          <time
            className="wc-msg__time"
            dateTime={new Date(message.createdAtMs).toISOString()}
            title={new Date(message.createdAtMs).toLocaleString()}
          >
            {formatMessageTime(message.createdAtMs)}
          </time>
        </div>
        <div className="wc-msg__content">
          <InboxMessageContent text={message.text} contentType={message.contentType} />
        </div>
      </div>
    </div>
  );
}
