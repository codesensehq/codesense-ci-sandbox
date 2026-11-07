// SPDX-License-Identifier: MIT
//! Match provider receipts to host reservations in either arrival order.
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
struct Session {
    waiting: VecDeque<(String, String)>,
    seen: VecDeque<String>,
}

#[derive(Default)]
pub(super) struct Receipts(HashMap<String, Session>);

impl Receipts {
    /// A submit learned its provider receipt. Return the reservation if its hook won.
    pub(super) fn register(&mut self, session: &str, receipt: &str, token: &str) -> Option<String> {
        let state = self.0.entry(session.into()).or_default();
        if let Some(at) = state.seen.iter().position(|id| id == receipt) {
            state.seen.remove(at);
            return Some(token.into());
        }
        state.waiting.retain(|(_, pending)| pending != token);
        state.waiting.push_back((receipt.into(), token.into()));
        if state.waiting.len() > super::KEPT {
            state.waiting.pop_front();
        }
        None
    }

    /// Only called after normal hook admission. Unknown IDs are retained during an
    /// in-flight submit, since its hook can arrive before the RPC receipt does.
    pub(super) fn confirm(
        &mut self,
        session: &str,
        receipts: &[String],
        submitting: bool,
    ) -> Vec<String> {
        let state = self.0.entry(session.into()).or_default();
        let mut settled = Vec::new();
        for receipt in receipts {
            if let Some(at) = state.waiting.iter().position(|(id, _)| id == receipt) {
                settled.push(state.waiting.remove(at).unwrap().1);
            } else if submitting && !state.seen.contains(receipt) {
                state.seen.push_back(receipt.clone());
                if state.seen.len() > super::KEPT {
                    state.seen.pop_front();
                }
            }
        }
        settled
    }

    pub(super) fn forget(&mut self, session: &str) {
        self.0.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_receipt_and_its_hook_can_arrive_in_either_order_without_crossing_sessions() {
        for hook_first in [false, true] {
            let mut receipts = Receipts::default();
            if hook_first {
                assert!(receipts
                    .confirm("s", &["admission".into()], true)
                    .is_empty());
                assert_eq!(
                    receipts.register("s", "admission", "reservation"),
                    Some("reservation".into())
                );
            } else {
                assert_eq!(receipts.register("s", "admission", "reservation"), None);
                assert!(receipts
                    .confirm("other", &["admission".into()], false)
                    .is_empty());
                assert_eq!(
                    receipts.confirm("s", &["admission".into()], false),
                    ["reservation"]
                );
            }
            assert!(receipts
                .confirm("s", &["admission".into()], false)
                .is_empty());
            receipts.forget("s");
            assert!(receipts
                .confirm("s", &["admission".into()], false)
                .is_empty());
        }
    }
}
