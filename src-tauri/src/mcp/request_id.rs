// SPDX-License-Identifier: MIT
//! Durable completion receipts: retries never run completion side effects twice.
use crate::commands::agent_inbox::{self, AppInboxHost};
use crate::weft::inbox;
use serde_json::Value;
use std::path::PathBuf;

pub(super) enum Claim {
    New(Option<Receipt>),
    Recorded(Value),
}

pub(super) struct Receipt {
    path: PathBuf,
    key: String,
}

pub(super) async fn begin(
    app: &tauri::AppHandle,
    session: &str,
    request_id: Option<&str>,
    payload: Value,
) -> Result<Claim, String> {
    let Some(id) = request_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Claim::New(None));
    };
    let host = AppInboxHost { app: app.clone() };
    let location = agent_inbox::session_store(&host, session).await?;
    let key = format!("signal_done:{session}:{id}");
    let path = location.path;
    begin_at(path, key, payload).await
}

async fn begin_at(path: PathBuf, key: String, payload: Value) -> Result<Claim, String> {
    let lookup = key.clone();
    let hash = inbox::request_payload_hash(&payload);
    let result = agent_inbox::with_store("signal_done.request", path.clone(), move |store| {
        inbox::inbox_claim_completion_request(store, &lookup, &hash)
    })
    .await??;
    match result {
        Some(result) => Ok(Claim::Recorded(
            serde_json::from_str(&result).map_err(|e| e.to_string())?,
        )),
        None => Ok(Claim::New(Some(Receipt { path, key }))),
    }
}

impl Receipt {
    pub(super) async fn finish(self, result: &Value) -> Result<(), String> {
        let result = result.to_string();
        agent_inbox::with_store("signal_done.receipt", self.path, move |store| {
            inbox::inbox_finish_completion_request(store, &self.key, &result)
        })
        .await??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn completion_retry_replays_the_durable_receipt_and_an_interrupted_call_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("inbox.db");
        let payload = serde_json::json!({"status":"needs-attention","summary":"review"});
        let Claim::New(Some(receipt)) = begin_at(path.clone(), "done-1".into(), payload.clone())
            .await
            .unwrap()
        else {
            panic!("new request");
        };
        assert!(begin_at(path.clone(), "done-1".into(), payload.clone())
            .await
            .err()
            .unwrap()
            .contains("uncertain"));
        let result = serde_json::json!({"recorded":true,"activityDelivery":"applied"});
        receipt.finish(&result).await.unwrap();
        let Claim::Recorded(replayed) = begin_at(path.clone(), "done-1".into(), payload)
            .await
            .unwrap()
        else {
            panic!("stored response");
        };
        assert_eq!(replayed, result);
        assert!(begin_at(
            path,
            "done-1".into(),
            serde_json::json!({"status":"failed"})
        )
        .await
        .err()
        .unwrap()
        .contains("different payload"));
    }
}
