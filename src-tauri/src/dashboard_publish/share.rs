// SPDX-License-Identifier: MIT
//! SW-PB-22f-5: who can view a published dashboard. `add` and `remove` are
//! applied to the server's current list (R8), and the result is sent as the
//! whole list (R9), so an address the caller did not name is never dropped by
//! accident. Removing an address ends that viewer's sessions on the server.

use std::path::Path;
use std::sync::Arc;

use super::client::{CallError, Client, Share, DASH_HOST, TOKEN_SERVICE};
use super::manifest;
use super::push::{fingerprint, DriveError};
use super::state::{self, LocalState};
use crate::commands::credential_store::{default_store, CredentialStore};

/// The outcome of a share call: the viewers after it, as the server lists them,
/// and any address asked to be removed that was not on the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    pub viewers: Vec<String>,
    pub shares: Vec<Share>,
    pub not_on_list: Vec<String>,
}

/// Change or list a published dashboard's viewers.
pub async fn share_hosted(
    data_dir: &Path,
    folder: &Path,
    add: &[String],
    remove: &[String],
) -> Result<Shared, DriveError> {
    let Some(host) = DASH_HOST else {
        return Err(DriveError::NoHost);
    };
    let client = Client::new(host).map_err(DriveError::Storage)?;
    let store: Arc<dyn CredentialStore> = Arc::from(default_store(TOKEN_SERVICE));
    share_with(&client, store, data_dir, folder, add, remove).await
}

/// `share_hosted` with its client and Keychain given, for tests.
pub(crate) async fn share_with(
    client: &Client,
    store: Arc<dyn CredentialStore>,
    data_dir: &Path,
    folder: &Path,
    add: &[String],
    remove: &[String],
) -> Result<Shared, DriveError> {
    let slug = manifest::load_slug(folder).map_err(|e| DriveError::Storage(e.to_string()))?;
    let record = state::load(data_dir, &slug).map_err(DriveError::Storage)?;
    if record.state != LocalState::Published {
        return Err(DriveError::NotPublished);
    }
    share_slug_with(client, store, data_dir, &slug, add, remove).await
}

pub async fn share_slug_hosted(
    data_dir: &Path,
    slug: &str,
    add: &[String],
    remove: &[String],
) -> Result<Shared, DriveError> {
    let client = Client::new(DASH_HOST.ok_or(DriveError::NoHost)?).map_err(DriveError::Storage)?;
    share_slug_with(
        &client,
        Arc::from(default_store(TOKEN_SERVICE)),
        data_dir,
        slug,
        add,
        remove,
    )
    .await
}

pub(crate) async fn share_slug_with(
    client: &Client,
    store: Arc<dyn CredentialStore>,
    data_dir: &Path,
    slug: &str,
    add: &[String],
    remove: &[String],
) -> Result<Shared, DriveError> {
    let key = |e: &str| e.trim().to_ascii_lowercase();
    if let Some(both) = add.iter().find(|a| remove.iter().any(|r| key(r) == key(a))) {
        return Err(DriveError::BadRequest(format!(
            "{} is in both add and remove",
            both.trim()
        )));
    }
    let mut record = state::load(data_dir, slug).map_err(DriveError::Storage)?;
    if !add.is_empty() {
        if let Some(block) = &record.live_approved {
            if let Some((query, arg)) = manifest::missing_pin(&block.queries, &block.args) {
                return Err(DriveError::BadRequest(format!(
                    "live.args: {query}.{arg} needs a pin before adding viewers"
                )));
            }
        }
    }
    let token = super::keys::token(store).await.map_err(DriveError::Keychain)?
        .ok_or_else(|| DriveError::Keychain("no publisher token".into()))?;
    // A refused token is not sent again (AC-6).
    if record.refused_token.as_deref() == Some(fingerprint(Some(&token)).as_str()) {
        return Err(DriveError::Refused);
    }

    let refused = |e: CallError, record: &mut state::PublishRecord| match e {
        CallError::Status { status: 401, .. } => {
            record.refused_token = Some(fingerprint(Some(&token)));
            let _ = state::save(data_dir, record);
            DriveError::Share(e)
        }
        // No dashboard under this slug for this token's publisher.
        CallError::Status {
            status: 404,
            ref code,
        } if code == "not_found" => DriveError::NotOwner,
        e => DriveError::Share(e),
    };
    let current = match client.shares(&token, &slug).await {
        Ok(shares) => shares,
        Err(e) => return Err(refused(e, &mut record)),
    };
    let emails: Vec<String> = current.iter().map(|s| s.email.clone()).collect();
    let not_on_list: Vec<String> = remove
        .iter()
        .filter(|r| !emails.iter().any(|e| key(e) == key(r)))
        .map(|r| r.trim().to_string())
        .collect();
    let next = merged(&emails, add, remove);
    if next == emails {
        return Ok(Shared {
            viewers: emails,
            shares: current,
            not_on_list,
        });
    }
    let shares = match client.put_shares(&token, &slug, &next).await {
        Ok(shares) => shares,
        Err(e) => return Err(refused(e, &mut record)),
    };
    Ok(Shared {
        viewers: shares.iter().map(|s| s.email.clone()).collect(),
        shares,
        not_on_list,
    })
}

/// `current` plus `add`, minus `remove`, compared case-insensitively and
/// trimmed. Order is kept: current addresses first, then new ones.
fn merged(current: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let key = |e: &str| e.trim().to_ascii_lowercase();
    let removed: Vec<String> = remove.iter().map(|e| key(e)).collect();
    let mut out: Vec<String> = Vec::new();
    for email in current.iter().chain(add.iter()) {
        let k = key(email);
        if k.is_empty() || removed.contains(&k) || out.iter().any(|e| key(e) == k) {
            continue;
        }
        out.push(email.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::client::fake::{Fake, Reply};
    use super::super::push::tests::{dashboard, token_store, TOKEN};
    use super::super::state::{Event, PublishRecord};
    use super::*;

    fn published(data: &Path) {
        let mut r = PublishRecord::new("ai-usage");
        r.apply(Event::Publish).unwrap();
        r.apply(Event::FirstCommitOk).unwrap();
        state::save(data, &r).unwrap();
    }

    #[tokio::test]
    async fn sharing_requires_saved_pins_but_removal_never_does() {
        use super::super::{approval, catalog::CATALOG};
        // SV-PBF-287: `all` now also covers storyTotals, which needs its own storyIds pin.
        for (queries, pinned) in [
            ("[storyChanges, whyLine]", "args: {storyChanges: {storyId: [SV-CDE-46]}, whyLine: {repo: [alderco/codesense]}}"),
            ("all", "args: {storyChanges: {storyId: [SV-CDE-46]}, storyTotals: {storyIds: [SV-CDE-46]}, whyLine: {repo: [alderco/codesense]}}"),
        ] {
            for pins in ["", pinned] {
                let folder = dashboard();
                let path = folder.path().join(manifest::MANIFEST_FILE);
                let head = std::fs::read_to_string(&path).unwrap();
                std::fs::write(&path, format!("{head}\nlive: {{queries: {queries}{} }}\n", if pins.is_empty() { String::new() } else { format!(", {pins}") })).unwrap();
                let data = tempfile::tempdir().unwrap();
                published(data.path());
                let m = manifest::load_with(folder.path(), CATALOG).unwrap();
                let mut r = state::load(data.path(), "ai-usage").unwrap();
                r.live_approved = approval::decide(&m, folder.path(), None, None).approved;
                state::save(data.path(), &r).unwrap();
                let fake = Fake::start(|_| Reply::Json(200, r#"{"shares":[]}"#.into())).await;
                let client = Client::new(&fake.base).unwrap();
                let added = share_with(&client, token_store(TOKEN), data.path(), folder.path(), &["ada@example.com".into()], &[]).await;
                if pins.is_empty() {
                    assert!(matches!(added, Err(DriveError::BadRequest(ref e)) if e.contains("storyChanges.storyId")));
                    assert!(fake.calls().is_empty());
                } else { assert!(added.is_ok(), "{added:?}"); }
                assert!(share_slug_with(&client, token_store(TOKEN), data.path(), "ai-usage", &[], &["ada@example.com".into()]).await.is_ok());
            }
        }
    }

    #[test]
    fn merge_keeps_unnamed_addresses_and_ignores_case_and_repeats() {
        let current = vec!["ada@example.com".to_string(), "bob@example.com".to_string()];
        let add = vec![" Cy@Example.com ".to_string(), "ADA@example.com".to_string()];
        let remove = vec!["BOB@example.com".to_string()];
        assert_eq!(
            merged(&current, &add, &remove),
            vec!["ada@example.com".to_string(), "Cy@Example.com".to_string()]
        );
    }

    #[tokio::test]
    async fn adding_a_viewer_keeps_the_others_and_sends_the_whole_list() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        published(data.path());
        let fake = Fake::start(|seen| {
            if seen.method == "GET" {
                Reply::Json(200, r#"{"slug":"ai-usage","shares":[{"id":"s1","email":"ada@example.com","added_at":1}]}"#.into())
            } else {
                let emails = seen.json()["emails"].clone();
                assert_eq!(emails, serde_json::json!(["ada@example.com", "bob@example.com"]));
                Reply::Json(200, r#"{"shares":[{"id":"s1","email":"ada@example.com","added_at":1},{"id":"s2","email":"bob@example.com","added_at":2}]}"#.into())
            }
        })
        .await;
        let client = Client::new(&fake.base).unwrap();
        let viewers = share_with(
            &client,
            token_store(TOKEN),
            data.path(),
            folder.path(),
            &["bob@example.com".to_string()],
            &[],
        )
        .await
        .unwrap()
        .viewers;
        assert_eq!(viewers, vec!["ada@example.com".to_string(), "bob@example.com".to_string()]);
        assert_eq!(
            fake.routes(),
            vec!["GET /api/dash/v1/d/ai-usage", "PUT /api/dash/v1/d/ai-usage/shares"]
        );
    }

    #[tokio::test]
    async fn listing_viewers_sends_no_change() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        published(data.path());
        let fake = Fake::start(|_| {
            Reply::Json(200, r#"{"shares":[{"id":"s1","email":"ada@example.com","added_at":1}]}"#.into())
        })
        .await;
        let client = Client::new(&fake.base).unwrap();
        let shared = share_with(
            &client,
            token_store(TOKEN),
            data.path(),
            folder.path(),
            &[],
            &["zed@example.com".to_string()],
        )
        .await
        .unwrap();
        assert_eq!(shared.viewers, vec!["ada@example.com".to_string()]);
        // Removing someone who was not a viewer changes nothing, and says so.
        assert_eq!(shared.not_on_list, vec!["zed@example.com".to_string()]);
        assert_eq!(fake.routes(), vec!["GET /api/dash/v1/d/ai-usage"]);
    }

    #[tokio::test]
    async fn an_address_in_both_add_and_remove_is_refused_before_any_call() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        published(data.path());
        let fake = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        let client = Client::new(&fake.base).unwrap();
        let both = ["bob@example.com".to_string()];
        let err = share_with(&client, token_store(TOKEN), data.path(), folder.path(), &both, &both)
            .await
            .unwrap_err();
        assert_eq!(err.reason(), "bob@example.com is in both add and remove");
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_dashboard_owned_by_another_token_is_named() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        published(data.path());
        let fake = Fake::start(|_| Reply::Json(404, r#"{"error":"not_found"}"#.into())).await;
        let client = Client::new(&fake.base).unwrap();
        let err = share_with(&client, token_store(TOKEN), data.path(), folder.path(), &[], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, DriveError::NotOwner));
    }

    #[tokio::test]
    async fn a_dashboard_that_is_not_published_is_not_shared() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|_| Reply::Json(200, "{}".into())).await;
        let client = Client::new(&fake.base).unwrap();
        let err = share_with(
            &client,
            token_store(TOKEN),
            data.path(),
            folder.path(),
            &["bob@example.com".to_string()],
            &[],
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DriveError::NotPublished));
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_401_saves_the_refusal_and_the_token_is_not_sent_again() {
        let folder = dashboard();
        let data = tempfile::tempdir().unwrap();
        published(data.path());
        let fake = Fake::start(|_| Reply::Json(401, r#"{"error":"unauthenticated"}"#.into())).await;
        let client = Client::new(&fake.base).unwrap();
        let add = ["bob@example.com".to_string()];
        assert!(share_with(&client, token_store(TOKEN), data.path(), folder.path(), &add, &[])
            .await
            .is_err());
        assert!(state::load(data.path(), "ai-usage").unwrap().refused_token.is_some());
        let sent = fake.calls().len();
        let err = share_with(&client, token_store(TOKEN), data.path(), folder.path(), &add, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, DriveError::Refused));
        assert_eq!(fake.calls().len(), sent);
    }
}

#[cfg(test)]
mod hosted_tests {
    use super::super::client::fake::{Fake, Reply};
    use super::super::push::tests::{token_store, TOKEN};
    use super::*;
    #[tokio::test]
    async fn hosted_only_viewers_use_server_list_and_wait_for_confirmation() {
        let data = tempfile::tempdir().unwrap();
        let fake = Fake::start(|seen| {
            if seen.method == "PUT" {
                Reply::Json(
                    200,
                    r#"{"shares":[{"email":"viewer@example.com","invite":"drafted"}]}"#.into(),
                )
            } else {
                Reply::Json(200, r#"{"shares":[]}"#.into())
            }
        })
        .await;
        let result = share_slug_with(
            &Client::new(&fake.base).unwrap(),
            token_store(TOKEN),
            data.path(),
            "hosted-only",
            &["viewer@example.com".into()],
            &[],
        )
        .await
        .unwrap();
        assert_eq!(result.shares[0].invite.as_deref(), Some("drafted"));
        assert_eq!(
            fake.routes(),
            vec![
                "GET /api/dash/v1/d/hosted-only",
                "PUT /api/dash/v1/d/hosted-only/shares"
            ]
        );
        let refused = Fake::start(|seen| {
            if seen.method == "PUT" {
                Reply::Json(503, "{}".into())
            } else {
                Reply::Json(200, r#"{"shares":[]}"#.into())
            }
        })
        .await;
        assert!(share_slug_with(
            &Client::new(&refused.base).unwrap(),
            token_store(TOKEN),
            data.path(),
            "hosted-only",
            &["viewer@example.com".into()],
            &[]
        )
        .await
        .is_err());
    }
}
