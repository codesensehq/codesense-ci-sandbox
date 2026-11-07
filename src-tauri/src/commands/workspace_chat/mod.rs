// SPDX-License-Identifier: MIT
//! One canonical log per workspace, with scoped persisted read/told positions.
//! Inbox notices refer to these posts without copying them into session mail.

pub mod commands;
pub mod index;
pub mod model;
pub mod resolve;
pub mod state;
pub mod store;

#[cfg(test)]
mod tests;

// Re-export EVERYTHING from the commands submodule so Tauri's generated
// `__cmd__*` wrappers (invisible to a named import) are reachable from
// `invoke_handler![crate::commands::workspace_chat::workspace_chat_send]`.
pub use commands::*;
#[allow(unused_imports)]
pub use commands::{emit_system_message, ensure_chat_started, ChatUpdatedPayload, UPDATED_EVENT};
#[allow(unused_imports)]
pub use index::{workspace_slug, ChatIndexEntry};
#[allow(unused_imports)]
pub use model::{
    MessageContentType, MessageKind, MessageOnClick, WorkspaceChatMessage, HUMAN, MAX_TEXT_BYTES,
};
pub use resolve::session_workspace_id;
#[allow(unused_imports)]
pub use state::{ChatPosition, ChatUnread};
#[allow(unused_imports)]
pub use store::{ChatAppend, ChatReadResult, ChatStoreError, WorkspaceChatStore};
