// SPDX-License-Identifier: MIT
//! SW-WC-2 — the on-wire `WorkspaceChatMessage`.
//!
//! The schema is a **strict superset** of `InboxMessageRow`
//! (`src/tauri/commands.ts:770` ↔ `src-tauri/src/weft/inbox.rs::InboxMessage`):
//! every field the inbox carries is kept under the same name and type; the chat
//! adds only `workspace_id` and `kind`. The inbox-shared `MessageContentType`
//! and `MessageOnClick` enums are **re-exported, not re-declared** — one source
//! of truth, zero drift risk (per `_bmad-output/21-workspace-chat/design/engine-architecture.md`
//! §3.1 and §3.5).

use serde::{Deserialize, Serialize};

// Re-exported from the inbox so the chat schema never redeclares them.
pub use crate::weft::inbox::{MessageContentType, MessageOnClick, HUMAN};

/// One line in a workspace chat log. On-wire shape: a `WorkspaceChatMessage`
/// is a strict superset of an `InboxMessageRow` (`src/tauri/commands.ts:770`).
///
/// Fields that mirror `InboxMessageRow` carry the SAME names and types so a FE
/// consumer written for the inbox can render a chat message with no transform
/// beyond ignoring `workspaceId` and `kind`. The chat-specific additions are
/// the final two fields (`workspace_id`, `kind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChatMessage {
    // ─── fields that mirror InboxMessageRow (same name, same type) ──────────
    /// Monotonic per-workspace id. 1-based, dense. The primary ordering key.
    /// Same role as `InboxMessageRow.messageId`.
    pub message_id: i64,
    /// Author identity frozen at write time. Session handle for an agent;
    /// the inbox-wide `HUMAN` sentinel (`"human"`) for the human in the chair;
    /// `"system"` for a lifecycle event (see `MessageKind::System`).
    pub from: String,
    /// Display name resolved at write time (launch display name for an agent;
    /// the account's display or OS username for the human; "System" for a
    /// lifecycle event).
    pub from_name: String,
    /// Wall-clock epoch-ms (`chrono::Utc::now().timestamp_millis()`).
    pub created_at_ms: i64,
    /// v1: always `None`. Reply chains across workspace chat are explicitly
    /// deferred to v2 (OQ-11.b). The field is on-wire so a v2 upgrade is
    /// schema-stable — no store migration required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<i64>,
    /// Message body. UTF-8, size cap enforced at the write boundary (32 KiB).
    pub text: String,
    /// Attachment hint — same semantics as `InboxMessageRow.primaryFile`
    /// (SW-BSI-2). A managed logical path the chat pane renders as an
    /// attachment card. `None` for plain text-only posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_file: Option<String>,
    /// Click action — same `GotoAgent | OpenFile | ShowMessage` enum the
    /// inbox uses. `None` → the row opens the chat thread (default affordance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click: Option<MessageOnClick>,
    /// Body rendering mode. `None` → the renderer applies its default
    /// (`markdown`), matching inbox behaviour exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<MessageContentType>,

    // ─── chat-specific additions (the ONLY two) ─────────────────────────────
    /// The workspace this message belongs to. Carried **inside** each line so a
    /// corrupt filename never silently reassigns messages (design §2.2).
    pub workspace_id: String,
    /// `Chat` (ordinary user/agent post) or `System` (lifecycle event — the
    /// chat has lifecycle rows the inbox does not).
    pub kind: MessageKind,
}

/// What a chat row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageKind {
    /// A chat post from a human or an agent.
    Chat,
    /// A lifecycle event stamped by the engine (chat started, session joined,
    /// workspace archived). Rendered with a muted treatment; `onClick` and
    /// `primaryFile` are always `None` for a system row.
    System,
}

impl MessageKind {
    /// The sentinel the engine stamps as `from` for a system row. Picked
    /// `"system"` because the inbox has no system row (so no `HUMAN`-style
    /// shared constant to reuse); kept distinct from `HUMAN` so `from ==
    /// HUMAN` stays the one branch for "the human posted this".
    pub const SYSTEM_FROM: &'static str = "system";
    /// Display name for a system row.
    pub const SYSTEM_FROM_NAME: &'static str = "System";
}

/// Hard cap on a message `text` in bytes. Matches the role-inbox cap pattern:
/// 32 KiB UTF-8 is plenty for the human → markdown post and small enough that
/// a run-away agent cannot blow the log file in one write.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_json_roundtrip_keeps_inbox_field_names() {
        let msg = WorkspaceChatMessage {
            message_id: 7,
            from: HUMAN.to_string(),
            from_name: "Sam".to_string(),
            created_at_ms: 1_700_000_000_000,
            in_reply_to: None,
            text: "hello workspace".to_string(),
            primary_file: None,
            on_click: None,
            content_type: Some(MessageContentType::Markdown),
            workspace_id: "story:ex-1".to_string(),
            kind: MessageKind::Chat,
        };
        let encoded = serde_json::to_value(&msg).unwrap();
        // Camel-case on the wire, matching InboxMessageRow:
        assert_eq!(encoded["messageId"], 7);
        assert_eq!(encoded["from"], "human");
        assert_eq!(encoded["fromName"], "Sam");
        assert_eq!(encoded["createdAtMs"], 1_700_000_000_000_i64);
        assert_eq!(encoded["text"], "hello workspace");
        assert_eq!(encoded["contentType"], "markdown");
        assert_eq!(encoded["workspaceId"], "story:ex-1");
        assert_eq!(encoded["kind"], "chat");
        // `inReplyTo`/`primaryFile`/`onClick` dropped when None (same shape
        // the inbox uses so FE consumers can treat them uniformly).
        assert!(encoded.get("inReplyTo").is_none());
        assert!(encoded.get("primaryFile").is_none());
        assert!(encoded.get("onClick").is_none());

        let decoded: WorkspaceChatMessage = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn system_row_uses_the_shared_sentinels() {
        assert_eq!(MessageKind::SYSTEM_FROM, "system");
        assert_ne!(MessageKind::SYSTEM_FROM, HUMAN);
    }
}
