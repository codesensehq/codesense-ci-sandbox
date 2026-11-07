// SPDX-License-Identifier: MIT
import type {
  ConversationMessage,
  InboxConversationSummary,
  InboxMessageRow,
  InboxTeam,
  RollupDto,
} from "../../tauri/commands";

// The message envelope emitted by live_hub/inbox.rs. There is no
// named TypeScript envelope type; its two payloads use the real command DTOs.
export type InboxFixtureRow = {
  id: string;
  rowType: "message";
  team: InboxTeam;
  messageId: number;
  createdAt: number;
  message: InboxMessageRow;
  conversation: ConversationMessage & { conversation: string };
};

export function inboxFixtureRow(
  messageId: number,
  text: string,
  overrides: Partial<InboxMessageRow> = {},
  team: InboxTeam = { org: "fixture", team: "dev" },
): InboxFixtureRow {
  const message: InboxMessageRow = {
    messageId,
    text,
    from: "fixture-agent",
    fromName: "Fixture sender",
    to: "human",
    toName: "You",
    toHuman: true,
    contentType: "plain",
    createdAtMs: Date.now(),
    inReplyTo: null,
    inReplyToTeam: null,
    unread: true,
    kind: "update",
    ...overrides,
  };
  message.conversation ??= message.from === "human" ? message.to : message.from;
  const states = {
    queued: "pending",
    reading: "claimed",
    accepted: "accepted",
    done: "acknowledged",
    withdrawn: "withdrawn",
  } as const;
  const conversation: InboxFixtureRow["conversation"] = {
    id: message.messageId,
    team,
    conversation: message.conversation,
    from: message.from,
    fromName: message.fromName,
    to: message.toRole ? null : message.to,
    toName: message.toRole ? null : message.toName,
    toRole: message.toRole ?? null,
    text: message.text,
    attachments: message.attachments,
    contentType: message.contentType ?? null,
    createdAtMs: message.createdAtMs,
    archivedAtMs: message.archivedAtMs ?? null,
    inReplyTo: message.inReplyTo,
    via: null,
    replyTo: message.replyTo ?? null,
    summary: message.summary ?? null,
    unread: message.unread,
    delivery: message.delivery
      ? {
          state: states[message.delivery.state],
          claimedBy: message.delivery.instance,
        }
      : null,
    kind: message.kind,
    ...(message.reactions ? { reactions: message.reactions } : {}),
  };
  return {
    id: `message:${team.org}/${team.team}/${message.messageId}`,
    rowType: "message",
    team,
    messageId: message.messageId,
    createdAt: message.createdAtMs,
    message,
    conversation,
  };
}

/** The rollup DTO is distinct from the live-hub envelope, as on the real backend. */
export function inboxFixtureRollup(
  rows: readonly InboxFixtureRow[],
): RollupDto & { conversations: InboxConversationSummary[] } {
  const messages = rows
    .map((row) => row.conversation)
    .sort((a, b) => a.createdAtMs - b.createdAtMs || a.id - b.id);
  const summaries = new Map<string, InboxConversationSummary>();
  for (const message of messages) {
    if (message.archivedAtMs !== null || (message.from !== "human" && message.to !== "human"))
      continue;
    const withHandle = message.conversation;
    const key = JSON.stringify([message.team.org, message.team.team, withHandle]);
    const summary = summaries.get(key) ?? {
      with: withHandle,
      team: message.team,
      unread: 0,
      messageCount: 0,
      lastAt: 0,
      lastMessageId: 0,
      lastPreview: "",
    };
    summary.messageCount++;
    if (
      message.to === "human" &&
      message.from !== "human" &&
      message.unread &&
      message.delivery?.state !== "withdrawn"
    )
      summary.unread++;
    summary.lastAt = message.createdAtMs;
    summary.lastMessageId = message.id;
    summary.lastPreview = (
      message.summary?.trim()
        ? message.summary
        : message.contentType === "html"
          ? "HTML message"
          : message.text
    ).slice(0, 120);
    summaries.set(key, summary);
  }
  const conversations = [...summaries.values()];
  return {
    messages,
    conversations,
    roles: [],
    humanUnread: conversations.reduce((sum, c) => sum + c.unread, 0),
  };
}
