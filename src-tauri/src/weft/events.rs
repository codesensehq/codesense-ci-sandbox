// SPDX-License-Identifier: MIT
//! SW-WV-F1 — the Weft FE push channel constant.
//!
//! `wv:attention-changed` is the fire-and-forget Tauri event that nudges a mounted
//! Weft Board to re-fetch (same pattern as `automation:changed`). It is EMITTED by
//! SW-WV-B3's `wv_upsert_attention` write handler (a write lands → the board
//! re-reads), NOT by F1 — F1 only SUBSCRIBES (`use-attention-board.ts`). Kept as a
//! single string here and mirrored in `src/tauri/events.ts`, maintained in sync by
//! the FE type-contract test (F1 Task 4d) — no codegen for a one-string constant.

/// The Tauri event a `wv_attention` write emits so a mounted Weft Board re-fetches
/// live without polling. MUST match `WEFT_ATTENTION_CHANGED_EVENT` in
/// `src/tauri/events.ts`.
pub const WEFT_ATTENTION_CHANGED_EVENT: &str = "wv:attention-changed";
