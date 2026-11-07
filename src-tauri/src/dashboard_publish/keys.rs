// SPDX-License-Identifier: MIT
use super::client::{CallError, Client, DASH_HOST, TOKEN_ACCOUNT, TOKEN_SERVICE};
use crate::commands::credential_store::{default_store, CredentialStore, Secret};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Missing,
    Configured,
    Refused,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub struct KeyStatus {
    pub status: KeyState,
    pub hint: Option<String>,
}

pub fn store() -> Arc<dyn CredentialStore> {
    Arc::from(default_store(TOKEN_SERVICE))
}

pub async fn token(store: Arc<dyn CredentialStore>) -> Result<Option<Secret>, String> {
    tokio::task::spawn_blocking(move || store.get(TOKEN_ACCOUNT))
        .await
        .map_err(|_| "Couldn't read your Keychain".to_string())?
        .map_err(|e| crate::feedback::redact(&e))
}

pub async fn status_with(store: Arc<dyn CredentialStore>, host: Option<&str>) -> KeyStatus {
    let token = match token(store).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            return KeyStatus {
                status: KeyState::Missing,
                hint: None,
            }
        }
        Err(_) => {
            return KeyStatus {
                status: KeyState::Unavailable,
                hint: None,
            }
        }
    };
    let tail: String = token
        .expose()
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let hint = Some(format!("csp_…{tail}"));
    let status = match host.and_then(|h| Client::new(h).ok()) {
        None => KeyState::Unavailable,
        Some(client) => match client.shares(&token, "publisher-key-check").await {
            Ok(_) | Err(CallError::Status { status: 404, .. }) => KeyState::Configured,
            Err(CallError::Status { status: 401, .. }) => KeyState::Refused,
            Err(_) => KeyState::Unavailable,
        },
    };
    KeyStatus { status, hint }
}

pub async fn save_with(store: Arc<dyn CredentialStore>, key: String) -> Result<(), String> {
    let key = key.trim();
    if !key.starts_with("csp_")
        || key.len() < 12
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("This doesn't look like a publishing key. They start with csp_.".into());
    }
    let secret = Secret::new(key);
    tokio::task::spawn_blocking(move || store.set(TOKEN_ACCOUNT, &secret))
        .await
        .map_err(|_| "Couldn't save to your Keychain. Nothing changed.".to_string())?
        .map_err(|e| {
            format!(
                "Couldn't save to your Keychain: {}. Nothing changed.",
                crate::feedback::redact(&e)
            )
        })
}

pub async fn remove_with(store: Arc<dyn CredentialStore>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || store.delete(TOKEN_ACCOUNT))
        .await
        .map_err(|_| "Couldn't remove the publishing key".to_string())?
        .map_err(|e| crate::feedback::redact(&e))
}

#[tauri::command]
pub async fn dashboard_publisher_key_status() -> KeyStatus {
    status_with(store(), DASH_HOST).await
}
#[tauri::command]
pub async fn dashboard_publisher_key_save(key: String) -> Result<KeyStatus, String> {
    save_with(store(), key).await?;
    Ok(dashboard_publisher_key_status().await)
}
#[tauri::command]
pub async fn dashboard_publisher_key_remove() -> Result<KeyStatus, String> {
    remove_with(store()).await?;
    Ok(dashboard_publisher_key_status().await)
}

#[cfg(test)]
mod tests {
    use super::super::client::fake::{Fake, Reply};
    use super::*;
    use crate::commands::credential_store::FakeStore;

    #[tokio::test]
    async fn key_lifecycle_never_returns_the_secret() {
        let store: Arc<dyn CredentialStore> = Arc::new(FakeStore::default());
        let fake = Fake::start(|_| Reply::Json(404, r#"{"error":"not_found"}"#.into())).await;
        assert_eq!(
            status_with(store.clone(), Some(&fake.base)).await.status,
            KeyState::Missing
        );
        save_with(store.clone(), "csp_private_test_a91f".into())
            .await
            .unwrap();
        let result = status_with(store.clone(), Some(&fake.base)).await;
        assert_eq!(result.status, KeyState::Configured);
        let wire = serde_json::to_string(&result).unwrap();
        assert!(wire.contains("csp_…a91f"));
        assert!(!wire.contains("private_test"));
        assert!(save_with(store.clone(), "wrong".into()).await.is_err());
        assert_eq!(
            status_with(store.clone(), Some(&fake.base)).await.hint,
            result.hint
        );
        remove_with(store.clone()).await.unwrap();
        assert_eq!(
            status_with(store, Some(&fake.base)).await.status,
            KeyState::Missing
        );
    }

    #[tokio::test]
    async fn refused_and_offline_checks_keep_the_saved_key() {
        let store: Arc<dyn CredentialStore> = Arc::new(FakeStore::default());
        save_with(store.clone(), "csp_private_test_a91f".into())
            .await
            .unwrap();
        let refused = Fake::start(|_| Reply::Json(401, "{}".into())).await;
        assert_eq!(
            status_with(store.clone(), Some(&refused.base)).await.status,
            KeyState::Refused
        );
        let offline = Fake::start(|_| Reply::Drop).await;
        assert_eq!(
            status_with(store.clone(), Some(&offline.base)).await.status,
            KeyState::Unavailable
        );
        assert!(store.get(TOKEN_ACCOUNT).unwrap().is_some());
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    struct Locked;
    impl CredentialStore for Locked {
        fn get(&self, _: &str) -> Result<Option<Secret>, String> {
            Err("Keychain is locked".into())
        }
        fn set(&self, _: &str, _: &Secret) -> Result<(), String> {
            Err("could not write csp_private_test_a91f".into())
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            Err("Keychain is locked".into())
        }
    }
    #[tokio::test]
    async fn keychain_failures_are_unavailable_and_scrubbed() {
        let store: Arc<dyn CredentialStore> = Arc::new(Locked);
        assert_eq!(
            status_with(store.clone(), None).await.status,
            KeyState::Unavailable
        );
        let error = save_with(store.clone(), "csp_private_test_a91f".into())
            .await
            .unwrap_err();
        assert!(!error.contains("private_test"));
        assert!(error.contains("Nothing changed"));
        assert!(remove_with(store).await.is_err());
    }
}
