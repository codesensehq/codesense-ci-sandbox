// SPDX-License-Identifier: MIT
//! Frame identities live for one host mount. Origins are public identifiers;
//! capabilities are delivered only to the registered embedded document.
use super::token::ScopedCapabilityStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameTeam {
    pub org: String,
    pub team: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameGrant {
    pub frame_id: String,
    pub principal: String,
    pub dashboard: String,
    pub slot: String,
    pub team: Option<FrameTeam>,
    pub scope: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FramePolicy {
    pub own_folder_only: bool,
    pub block_service_workers: bool,
    pub no_outside_network: bool,
    #[serde(skip)]
    pub inbox_media: bool,
    pub allowed_hosts: Vec<String>,
    pub team_safe: bool,
}

impl FramePolicy {
    /// A service worker can replace navigation responses and strip their CSP.
    /// Network isolation therefore also requires service-worker isolation.
    pub fn blocks_service_workers(&self) -> bool {
        self.block_service_workers || self.no_outside_network
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMessage {
    pub org: String,
    pub team: String,
    pub message_id: i64,
}

/// Caller classification comes from validated mint facts, never capability scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameCallerKind {
    Message,
    Team,
    Private,
    Builtin,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRecord {
    #[serde(skip)]
    pub records_binding: Option<crate::records::binding::Binding>,
    pub parent_frame: Option<String>,
    pub effective_grant: Vec<String>,
    pub kind: FrameCallerKind,
    pub frame_id: String,
    pub origin: String,
    pub token: String,
    pub grant: FrameGrant,
    pub content_root: String,
    pub document_path: String,
    pub asset_folder: String,
    pub policy: FramePolicy,
    pub message: Option<FrameMessage>,
    /// What the page declared at mint, so a rebind recomputes the same grant for a new team.
    #[serde(skip)]
    pub declared: Vec<String>,
}

pub struct FrameStore {
    records: Mutex<HashMap<String, FrameRecord>>,
    tokens: ScopedCapabilityStore<FrameGrant>,
}
impl Default for FrameStore {
    fn default() -> Self {
        Self {
            records: Mutex::new(HashMap::new()),
            // Teardown revokes these. A long-lived visible dashboard must not
            // suddenly lose its grant after the read-token store's default TTL.
            tokens: ScopedCapabilityStore::new(Duration::from_secs(100 * 365 * 24 * 3600)),
        }
    }
}
impl FrameStore {
    pub fn mint(
        &self,
        port: u16,
        grant: FrameGrant,
        content_root: String,
        document_path: String,
        asset_folder: String,
        policy: FramePolicy,
        message: Option<FrameMessage>,
    ) -> FrameRecord {
        let kind = if message.is_some()
            || grant.slot == "message"
            || document_path.starts_with("inbox-message://")
        {
            FrameCallerKind::Message
        } else if document_path.starts_with("builtin:") || document_path.starts_with("virtual:") {
            FrameCallerKind::Builtin
        } else if grant.team.is_some() {
            FrameCallerKind::Team
        } else {
            FrameCallerKind::Private
        };
        self.mint_with_grant(
            port,
            grant,
            content_root,
            document_path,
            asset_folder,
            policy,
            message,
            crate::verbs::capabilities::full(),
            kind,
        )
    }
    pub fn mint_with_grant(
        &self,
        port: u16,
        mut grant: FrameGrant,
        content_root: String,
        document_path: String,
        asset_folder: String,
        policy: FramePolicy,
        message: Option<FrameMessage>,
        effective_grant: Vec<String>,
        kind: FrameCallerKind,
    ) -> FrameRecord {
        let id = uuid::Uuid::new_v4().simple().to_string();
        grant.frame_id = id.clone();
        let token = self.tokens.mint(grant.clone());
        let record = FrameRecord {
            records_binding: None,
            parent_frame: None,
            effective_grant,
            kind,
            frame_id: id.clone(),
            origin: format!("http://f-{id}.localhost:{port}"),
            token,
            grant,
            content_root,
            document_path,
            asset_folder,
            policy,
            message,
            declared: Vec::new(),
        };
        self.records
            .lock()
            .expect("frame store poisoned")
            .insert(id, record.clone());
        record
    }
    pub fn bind_records(
        &self,
        id: &str,
        binding: crate::records::binding::Binding,
    ) -> Result<FrameRecord, String> {
        let mut records = self.records.lock().map_err(|_| "Frame store unavailable")?;
        let frame = records.get_mut(id).ok_or("denied: frame released")?;
        frame.records_binding = Some(binding);
        Ok(frame.clone())
    }
    pub fn declare(&self, id: &str, declared: &[String]) {
        if let Some(record) = self
            .records
            .lock()
            .ok()
            .as_mut()
            .and_then(|r| r.get_mut(id))
        {
            record.declared = declared.to_vec();
        }
    }
    /// Re-binds a live frame to a new team in place: same frame id and origin (so the page
    /// never reloads), a new token carrying the recomputed grant, and the old token revoked at
    /// once. Refused when `token` is no longer the frame's, so a stale rebind cannot win.
    pub fn rebind(
        &self,
        id: &str,
        token: &str,
        team: Option<FrameTeam>,
        scope: String,
        effective_grant: Vec<String>,
        policy: FramePolicy,
    ) -> Result<FrameRecord, String> {
        let mut records = self.records.lock().map_err(|_| "Frame store unavailable")?;
        let record = records.get_mut(id).ok_or("denied: frame identity")?;
        if record.token != token {
            return Err("denied: frame identity".into());
        }
        let grant = FrameGrant {
            team,
            scope,
            ..record.grant.clone()
        };
        let fresh = self.tokens.mint(grant.clone());
        self.tokens.revoke(&record.token);
        record.token = fresh;
        record.grant = grant;
        record.effective_grant = effective_grant;
        record.policy = policy;
        Ok(record.clone())
    }
    pub fn link_child(&self, id: &str, parent: &str) -> Result<FrameRecord, String> {
        let mut records = self.records.lock().map_err(|_| "Frame store unavailable")?;
        let pane = records.get(parent).ok_or("denied: parent frame")?;
        if !crate::verbs::capabilities::SLOTS.contains(&pane.grant.slot.as_str())
            || pane.message.is_some()
        {
            return Err("denied: parent frame".into());
        }
        let child = records.get_mut(id).ok_or("denied: child frame")?;
        if child.message.is_none() {
            return Err("denied: child frame".into());
        }
        child.parent_frame = Some(parent.into());
        Ok(child.clone())
    }
    pub fn release(&self, id: &str) {
        let removed = self
            .records
            .lock()
            .ok()
            .map(|mut records| {
                let ids: Vec<_> = records
                    .values()
                    .filter(|r| r.frame_id == id || r.parent_frame.as_deref() == Some(id))
                    .map(|r| r.frame_id.clone())
                    .collect();
                ids.into_iter()
                    .filter_map(|id| records.remove(&id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for record in removed {
            self.tokens.revoke(&record.token);
        }
    }
    pub fn by_id(&self, id: &str) -> Option<FrameRecord> {
        self.records.lock().ok()?.get(id).cloned()
    }
    pub fn by_host(&self, host: &str, port: u16) -> Option<FrameRecord> {
        let suffix = format!(".localhost:{port}");
        let id = host.strip_prefix("f-")?.strip_suffix(&suffix)?;
        self.records.lock().ok()?.get(id).cloned()
    }
    pub fn by_origin(&self, origin: &str) -> Option<FrameRecord> {
        self.records
            .lock()
            .ok()?
            .values()
            .find(|r| r.origin == origin)
            .cloned()
    }
    pub fn resolve(&self, origin: &str, token: &str) -> Option<FrameGrant> {
        let record = self.by_origin(origin)?;
        let grant = self.tokens.validate(token)?;
        if let Some(parent) = &record.parent_frame {
            let parent = self.by_id(parent)?;
            if !crate::verbs::capabilities::SLOTS.contains(&parent.grant.slot.as_str())
                || parent.message.is_some()
            {
                return None;
            }
        }
        (record.grant == grant).then_some(grant)
    }
}
impl FrameRecord {
    pub fn allows_path(&self, file: &Path) -> bool {
        if !self.policy.own_folder_only && !(self.policy.team_safe && self.message.is_some()) {
            return true;
        }
        if file.components().any(|c| matches!(c, Component::ParentDir)) {
            return false;
        }
        let Ok(real) = file.canonicalize() else {
            return false;
        };
        let document = PathBuf::from(&self.document_path).canonicalize().ok();
        let folder = PathBuf::from(&self.asset_folder).canonicalize().ok();
        (!self.policy.own_folder_only
            || document.as_ref() == Some(&real)
            || folder.is_some_and(|dir| real.starts_with(dir)))
            && (!self.policy.team_safe
                || PathBuf::from(&self.content_root)
                    .canonicalize()
                    .ok()
                    .is_some_and(|dir| real.starts_with(dir))
                || self.message.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mint(store: &FrameStore) -> FrameRecord {
        store.mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "board".into(),
                slot: "centre".into(),
                team: None,
                scope: "open".into(),
            },
            "/ctx".into(),
            "/ctx/board.html".into(),
            "/ctx".into(),
            FramePolicy::default(),
            None,
        )
    }
    #[test]
    fn origin_token_and_teardown_are_bound() {
        let store = FrameStore::default();
        let a = mint(&store);
        let b = mint(&store);
        assert_ne!(a.origin, b.origin);
        assert!(store.resolve(&a.origin, &a.token).is_some());
        assert!(store.resolve(&a.origin, &b.token).is_none());
        assert!(store.by_host("f-unknown.localhost:9321", 9321).is_none());
        assert!(store
            .by_host(a.origin.trim_start_matches("http://"), 9321)
            .is_some());
        store.release(&a.frame_id);
        assert!(store.resolve(&a.origin, &a.token).is_none());
        assert!(store.by_origin(&a.origin).is_none());
        assert_ne!(mint(&store).origin, a.origin);
    }
    #[test]
    fn restricted_files_and_symlinks_stay_in_the_asset_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("own");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(own.join("board.html"), "board").unwrap();
        std::fs::write(own.join("asset.js"), "asset").unwrap();
        std::fs::write(other.join("private.txt"), "private").unwrap();
        let mut record = mint(&FrameStore::default());
        record.document_path = own.join("board.html").to_string_lossy().into_owned();
        record.asset_folder = own.to_string_lossy().into_owned();
        record.policy.own_folder_only = true;
        assert!(record.allows_path(&own.join("board.html")));
        assert!(record.allows_path(&own.join("asset.js")));
        assert!(!record.allows_path(&other.join("private.txt")));
        assert!(!record.allows_path(&own.join("../other/private.txt")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.join("private.txt"), own.join("link.txt")).unwrap();
            assert!(!record.allows_path(&own.join("link.txt")));
        }
        record.policy.own_folder_only = false;
        assert!(record.allows_path(&other.join("private.txt")));
    }
    #[test]
    fn team_safe_message_files_stay_in_the_stored_team_but_open_does_not_narrow_them() {
        let tmp = tempfile::tempdir().unwrap();
        let team = tmp.path().join("a");
        let private = tmp.path().join("private");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(team.join("own.txt"), "a").unwrap();
        std::fs::write(private.join("mail.txt"), "private").unwrap();
        let mut record = mint(&FrameStore::default());
        record.content_root = team.to_string_lossy().into_owned();
        record.asset_folder = record.content_root.clone();
        record.document_path = "inbox-message://acme/a/7".into();
        record.message = Some(FrameMessage {
            org: "acme".into(),
            team: "a".into(),
            message_id: 7,
        });
        assert!(record.allows_path(&private.join("mail.txt")));
        record.policy.team_safe = true;
        assert!(record.allows_path(&team.join("own.txt")));
        assert!(!record.allows_path(&private.join("mail.txt")));
    }
    #[test]
    fn open_keeps_all_switches_off_and_paths_open() {
        let record = mint(&FrameStore::default());
        assert!(
            !record.policy.own_folder_only
                && !record.policy.block_service_workers
                && !record.policy.no_outside_network
        );
        assert!(record.allows_path(Path::new("/another-team/content/file")));
    }
    #[test]
    fn caller_kind_comes_from_mint_facts_even_when_capabilities_are_open() {
        for (slot, document, has_team, expected) in [
            (
                "centre",
                "/ctx/private/board.html",
                false,
                FrameCallerKind::Private,
            ),
            (
                "right-pane",
                "/ctx/team/panes/board.html",
                true,
                FrameCallerKind::Team,
            ),
            (
                "message",
                "/ctx/team/message.html",
                true,
                FrameCallerKind::Message,
            ),
            (
                "centre",
                "inbox-message://acme/team/7",
                true,
                FrameCallerKind::Message,
            ),
            ("centre", "builtin:agents", false, FrameCallerKind::Builtin),
        ] {
            let store = FrameStore::default();
            let record = store.mint(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "human".into(),
                    dashboard: document.into(),
                    slot: slot.into(),
                    team: has_team.then(|| FrameTeam {
                        org: "acme".into(),
                        team: "team".into(),
                    }),
                    scope: "open".into(),
                },
                "/ctx".into(),
                document.into(),
                "/ctx".into(),
                FramePolicy::default(),
                None,
            );
            assert_eq!(record.kind, expected);
            assert_eq!(
                store.resolve(&record.origin, &record.token).unwrap().scope,
                "open"
            );
        }
    }
    #[test]
    fn child_message_identity_requires_a_live_pane_parent_and_revokes_with_it() {
        let frames = FrameStore::default();
        let make = |slot: &str, message| {
            let mut grant = mint(&frames).grant;
            grant.slot = slot.into();
            frames.mint(
                9321,
                grant,
                "/ctx".into(),
                "/ctx/message.html".into(),
                "/ctx".into(),
                FramePolicy::default(),
                message,
            )
        };
        let pane = make("right-pane", None);
        let foreign = make("centre", None);
        let child = make(
            "message",
            Some(FrameMessage {
                org: "acme".into(),
                team: "mail".into(),
                message_id: 7,
            }),
        );
        assert!(frames
            .link_child(&child.frame_id, &foreign.frame_id)
            .is_err());
        assert!(frames
            .link_child(&foreign.frame_id, &pane.frame_id)
            .is_err());
        assert!(frames.link_child(&child.frame_id, "missing").is_err());
        let child = frames.link_child(&child.frame_id, &pane.frame_id).unwrap();
        assert_eq!(child.parent_frame.as_deref(), Some(pane.frame_id.as_str()));
        assert!(frames.resolve(&child.origin, &child.token).is_some());
        assert!(frames.resolve(&child.origin, &pane.token).is_none());
        assert!(frames.resolve(&pane.origin, &child.token).is_none());
        frames.release(&pane.frame_id);
        assert!(frames.by_origin(&child.origin).is_none());
        assert!(frames.resolve(&child.origin, &child.token).is_none());
    }
}
