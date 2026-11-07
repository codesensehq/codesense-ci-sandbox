// SPDX-License-Identifier: MIT
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const RETAINED_DELTAS: usize = 256;
pub const PAGE_SIZE: usize = 200;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub topic: String,
    pub view_key: String,
    pub rev: u64,
    pub snapshot: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub topic: String,
    pub view_key: String,
    pub rev: u64,
    pub upserts: Vec<Value>,
    pub removes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip)]
    pub base_rev: Option<u64>,
    #[serde(skip)]
    arrivals: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionReply {
    pub topic: String,
    pub view_key: String,
    pub rev: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deltas: Option<Vec<Delta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}
impl From<Snapshot> for SubscriptionReply {
    fn from(s: Snapshot) -> Self {
        Self {
            topic: s.topic,
            view_key: s.view_key,
            rev: s.rev,
            snapshot: Some(s.snapshot),
            deltas: None,
            total: s.total,
            cursor: s.cursor,
        }
    }
}
#[derive(Default)]
struct View {
    rev: u64,
    rows: BTreeMap<String, Value>,
    log: VecDeque<Delta>,
}
impl View {
    fn commit(
        &mut self,
        topic: &str,
        key: &str,
        upserts: Vec<Value>,
        removes: Vec<String>,
        request_id: Option<String>,
        arrivals: BTreeSet<String>,
    ) -> Option<Delta> {
        if upserts.is_empty() && removes.is_empty() {
            return None;
        }
        self.rev += 1;
        let delta = Delta {
            actor: None,
            cursor: None,
            kind: None,
            topic: topic.into(),
            view_key: key.into(),
            rev: self.rev,
            upserts,
            removes,
            request_id,
            total: None,
            base_rev: None,
            arrivals,
        };
        self.log.push_back(delta.clone());
        if self.log.len() > RETAINED_DELTAS {
            self.log.pop_front();
        }
        Some(delta)
    }
}
pub struct CachedProjection {
    topic: String,
    rows: BTreeMap<String, (Option<String>, Value)>,
    views: BTreeMap<String, View>,
}
impl Default for CachedProjection {
    fn default() -> Self {
        Self::new("sessions")
    }
}
pub type SessionsProjection = CachedProjection;
pub trait Projection {
    fn snapshot(&mut self, view_key: &str) -> Snapshot;
    fn replace(
        &mut self,
        rows: Vec<(Option<String>, Value)>,
        request_id: Option<String>,
    ) -> Vec<Delta>;
}
impl CachedProjection {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.into(),
            rows: BTreeMap::new(),
            views: BTreeMap::new(),
        }
    }
    fn filtered(&self, key: &str) -> BTreeMap<String, Value> {
        self.rows
            .iter()
            .filter(|(_, (scope, _))| super::view_key::admits(key, scope.as_deref()))
            .map(|(id, (_, row))| (id.clone(), row.clone()))
            .collect()
    }
    fn ordered(&self, key: &str) -> Vec<Value> {
        let mut rows: Vec<_> = self.views[key].rows.values().cloned().collect();
        if self.topic == "inbox" {
            rows.sort_by(|a, b| {
                let time = |r: &Value| {
                    r["createdAt"]
                        .as_i64()
                        .or_else(|| r["messageId"].as_i64())
                        .unwrap_or(i64::MAX)
                };
                time(b)
                    .cmp(&time(a))
                    .then_with(|| b["messageId"].as_i64().cmp(&a["messageId"].as_i64()))
                    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
        }
        if self.topic == "files" {
            rows.sort_by(|a, b| {
                (b["kind"] == "dir")
                    .cmp(&(a["kind"] == "dir"))
                    .then_with(|| {
                        codesense_fs_core::natural_sort::natural_cmp(
                            a["name"].as_str().unwrap_or(""),
                            b["name"].as_str().unwrap_or(""),
                        )
                    })
            });
        }
        if matches!(self.topic.as_str(), "agentList" | "records") {
            rows.sort_by(|a, b| {
                a["order"]
                    .as_f64()
                    .partial_cmp(&b["order"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
        }
        rows
    }
    fn ensure_view(&mut self, key: &str) {
        if !self.views.contains_key(key) {
            let rows = self.filtered(key);
            self.views.insert(
                key.into(),
                View {
                    rows,
                    ..Default::default()
                },
            );
        }
    }
    fn replace_view(&mut self, key: &str, rows: Vec<(Option<String>, Value)>) -> Option<Delta> {
        self.ensure_view(key);
        let next: BTreeMap<String, Value> = rows
            .into_iter()
            .filter(|(scope, _)| super::view_key::admits(key, scope.as_deref()))
            .map(|(_, row)| (row["id"].as_str().unwrap().to_owned(), row))
            .collect();
        let view = self.views.get_mut(key).unwrap();
        let arrivals = next
            .keys()
            .filter(|id| !view.rows.contains_key(*id))
            .cloned()
            .collect();
        let upserts = next
            .iter()
            .filter(|(id, row)| view.rows.get(*id) != Some(*row))
            .map(|(_, row)| row.clone())
            .collect();
        let removes = view
            .rows
            .keys()
            .filter(|id| !next.contains_key(*id))
            .cloned()
            .collect();
        view.rows = next;
        view.commit(&self.topic, key, upserts, removes, None, arrivals)
    }
    fn replay(&mut self, key: &str, since: Option<u64>) -> Option<Vec<Delta>> {
        self.ensure_view(key);
        let view = &self.views[key];
        let since = since?;
        if since > view.rev
            || (since < view.rev && view.log.front().is_none_or(|d| since + 1 < d.rev))
        {
            return None;
        }
        Some(view.log.iter().filter(|d| d.rev > since).cloned().collect())
    }
    fn patch(
        &mut self,
        upserts: Vec<(Option<String>, Value)>,
        removes: Vec<String>,
        request_id: Option<String>,
    ) -> Vec<Delta> {
        let mut affected: BTreeSet<String> = removes.iter().cloned().collect();
        let arrivals: BTreeSet<String> = upserts
            .iter()
            .filter_map(|(_, row)| {
                let id = row["id"].as_str().expect("projected row id");
                (!self.rows.contains_key(id)).then(|| id.to_owned())
            })
            .collect();
        for id in removes {
            self.rows.remove(&id);
        }
        for (scope, row) in upserts {
            let id = row["id"].as_str().expect("projected row id").to_owned();
            affected.insert(id.clone());
            self.rows.insert(id, (scope, row));
        }
        let mut deltas = Vec::new();
        for (key, view) in &mut self.views {
            let mut upserts = Vec::new();
            let mut removes = Vec::new();
            for id in &affected {
                let next = self
                    .rows
                    .get(id)
                    .filter(|(scope, _)| super::view_key::admits(key, scope.as_deref()))
                    .map(|(_, row)| row);
                match next {
                    Some(row) if view.rows.get(id) != Some(row) => {
                        view.rows.insert(id.clone(), row.clone());
                        upserts.push(row.clone());
                    }
                    None if view.rows.remove(id).is_some() => removes.push(id.clone()),
                    _ => {}
                }
            }
            if let Some(delta) = view.commit(
                &self.topic,
                key,
                upserts,
                removes,
                request_id.clone(),
                arrivals.clone(),
            ) {
                deltas.push(delta);
            }
        }
        deltas
    }
}
impl Projection for CachedProjection {
    fn snapshot(&mut self, key: &str) -> Snapshot {
        self.ensure_view(key);
        Snapshot {
            topic: self.topic.clone(),
            view_key: key.into(),
            rev: self.views[key].rev,
            snapshot: self.ordered(key),
            total: None,
            cursor: None,
        }
    }
    fn replace(
        &mut self,
        rows: Vec<(Option<String>, Value)>,
        request_id: Option<String>,
    ) -> Vec<Delta> {
        let next: BTreeSet<_> = rows
            .iter()
            .map(|(_, row)| row["id"].as_str().unwrap().to_owned())
            .collect();
        let removes = self
            .rows
            .keys()
            .filter(|id| !next.contains(*id))
            .cloned()
            .collect();
        self.patch(rows, removes, request_id)
    }
}
struct Subscription {
    topic: String,
    key: String,
    owner: Option<String>,
    window: Option<[usize; 2]>,
    loaded: BTreeMap<String, Value>,
    total: usize,
    rev: u64,
    log: VecDeque<Delta>,
}
pub struct TopicRegistry {
    topics: BTreeMap<String, CachedProjection>,
    subscriptions: BTreeMap<String, Subscription>,
}
impl Default for TopicRegistry {
    fn default() -> Self {
        Self {
            topics: [
                "sessions",
                "inbox",
                "agentList",
                "records",
                "paneSources",
                "machineStats",
                "runEndorsements",
                "runEvidence",
                "files",
                "stories",
                "agentDefinitions",
                "profile",
                "workspaceConfig",
                "prefs",
                "storyTotals",
            ]
            .into_iter()
            .map(|t| (t.into(), CachedProjection::new(t)))
            .collect(),
            subscriptions: BTreeMap::new(),
        }
    }
}
impl TopicRegistry {
    fn topic(&mut self, topic: &str) -> Result<&mut CachedProjection, String> {
        if topic.starts_with("ui:") && topic.len() > 3 {
            self.topics
                .entry(topic.into())
                .or_insert_with(|| CachedProjection::new(topic));
        }
        self.topics
            .get_mut(topic)
            .ok_or_else(|| format!("unknown topic: {topic}"))
    }
    pub fn replace_view(
        &mut self,
        topic: &str,
        key: &str,
        rows: Vec<(Option<String>, Value)>,
    ) -> Result<Vec<Delta>, String> {
        Ok(self
            .topic(topic)?
            .replace_view(key, rows)
            .into_iter()
            .collect())
    }
    pub fn replace_records(
        &mut self,
        key: &str,
        rows: Vec<Value>,
        actor: Option<Value>,
        kind: Option<String>,
        request_id: Option<String>,
    ) -> Result<Vec<Delta>, String> {
        let scoped = rows
            .into_iter()
            .map(|row| (Some(key.to_owned()), row))
            .collect();
        let mut deltas = self.replace_view("records", key, scoped)?;
        for delta in &mut deltas {
            delta.actor = actor.clone();
            delta.kind = kind.clone();
            delta.request_id = request_id.clone();
            if let Some(saved) = self
                .topic("records")?
                .views
                .get_mut(key)
                .and_then(|v| v.log.back_mut())
            {
                *saved = delta.clone();
            }
        }
        Ok(deltas)
    }
    pub fn snapshot(&mut self, topic: &str, key: &str) -> Result<Snapshot, String> {
        Ok(self.topic(topic)?.snapshot(key))
    }
    pub fn subscribe(
        &mut self,
        topic: &str,
        key: &str,
        since: Option<u64>,
    ) -> Result<SubscriptionReply, String> {
        let p = self.topic(topic)?;
        if let Some(deltas) = p.replay(key, since) {
            return Ok(SubscriptionReply {
                topic: topic.into(),
                view_key: key.into(),
                rev: p.views[key].rev,
                snapshot: None,
                deltas: Some(deltas),
                total: None,
                cursor: None,
            });
        }
        let mut snapshot = p.snapshot(key);
        if matches!(topic, "inbox" | "records" | "files" | "stories") {
            let total = snapshot.snapshot.len();
            snapshot.snapshot.truncate(PAGE_SIZE);
            snapshot.cursor = cursor_for(&snapshot.snapshot, &p.ordered(key), topic, None);
            snapshot.total = Some(total);
        }
        Ok(snapshot.into())
    }
    pub fn replace(
        &mut self,
        topic: &str,
        rows: Vec<(Option<String>, Value)>,
        request_id: Option<String>,
    ) -> Result<Vec<Delta>, String> {
        Ok(self.topic(topic)?.replace(rows, request_id))
    }
    pub(super) fn has_subscription(&self, topic: &str, key: &str) -> bool {
        self.subscriptions
            .values()
            .any(|s| s.topic == topic && s.key == key)
    }
    pub fn subscribe_window(
        &mut self,
        id: &str,
        topic: &str,
        key: &str,
        since: Option<u64>,
        window: Option<[usize; 2]>,
    ) -> Result<SubscriptionReply, String> {
        let window = if matches!(topic, "records" | "files" | "stories") {
            Some(window.unwrap_or([0, PAGE_SIZE]))
        } else {
            window
        };
        let p = self.topic(topic)?;
        let full = p.snapshot(key);
        let retained = p.replay(key, since).is_some();
        if let (Some(s), Some(since)) = (self.subscriptions.get(id), since) {
            let covered = since == s.rev
                || s.log
                    .front()
                    .is_some_and(|d| d.base_rev.is_some_and(|base| since >= base));
            if retained
                && covered
                && since <= s.rev
                && s.topic == topic
                && s.key == key
                && s.window == window
            {
                return Ok(SubscriptionReply {
                    topic: topic.into(),
                    view_key: key.into(),
                    rev: s.rev,
                    snapshot: None,
                    deltas: Some(s.log.iter().filter(|d| d.rev > since).cloned().collect()),
                    total: Some(s.total),
                    cursor: None,
                });
            }
        }
        let total = full.snapshot.len();
        let selected = select(
            &full.snapshot,
            window,
            matches!(topic, "inbox" | "records" | "files" | "stories"),
        );
        let cursor = cursor_for(&selected, &full.snapshot, topic, window);
        let loaded = selected
            .iter()
            .map(|r| (r["id"].as_str().unwrap().into(), r.clone()))
            .collect();
        self.subscriptions.insert(
            id.into(),
            Subscription {
                topic: topic.into(),
                key: key.into(),
                owner: None,
                window,
                loaded,
                total,
                rev: full.rev,
                log: VecDeque::new(),
            },
        );
        Ok(SubscriptionReply {
            topic: topic.into(),
            view_key: key.into(),
            rev: full.rev,
            snapshot: Some(selected),
            deltas: None,
            total: (window.is_some() || matches!(topic, "inbox" | "records" | "files" | "stories"))
                .then_some(total),
            cursor,
        })
    }
    pub fn page(&mut self, id: &str, cursor: &str) -> Result<Snapshot, String> {
        let s = self
            .subscriptions
            .get(id)
            .ok_or("subscription unavailable")?;
        let (topic, key) = (s.topic.clone(), s.key.clone());
        if matches!(topic.as_str(), "records" | "files" | "stories") {
            if cursor != format!("{topic}:next") {
                return Err("invalid records cursor".into());
            }
            let full = self.snapshot(&topic, &key)?;
            let loaded = &self.subscriptions[id].loaded;
            let end = self.subscriptions[id]
                .window
                .map(|w| w[1])
                .unwrap_or(loaded.len());
            let remaining = full
                .snapshot
                .into_iter()
                .skip(end)
                .filter(|r| !loaded.contains_key(r["id"].as_str().unwrap()))
                .collect::<Vec<_>>();
            let selected = remaining
                .iter()
                .take(PAGE_SIZE)
                .cloned()
                .collect::<Vec<_>>();
            let cursor = (remaining.len() > selected.len()).then(|| format!("{topic}:next"));
            let subscription = self.subscriptions.get_mut(id).unwrap();
            subscription.loaded.extend(
                selected
                    .iter()
                    .map(|r| (r["id"].as_str().unwrap().to_owned(), r.clone())),
            );
            if let Some([start, end]) = subscription.window {
                subscription.window = Some([start, end + selected.len()]);
            }
            return Ok(Snapshot {
                topic,
                view_key: key,
                rev: full.rev,
                snapshot: selected,
                total: Some(subscription.total),
                cursor,
            });
        }
        let mut before: BTreeMap<String, i64> =
            serde_json::from_str(cursor).map_err(|_| "invalid cursor")?;
        let full = self.snapshot(&topic, &key)?;
        let loaded = &self.subscriptions[id].loaded;
        let remaining: Vec<_> = full
            .snapshot
            .into_iter()
            .filter(|r| {
                !loaded.contains_key(r["id"].as_str().unwrap())
                    && r["messageId"]
                        .as_i64()
                        .is_some_and(|n| before.get(&cursor_team(r)).is_some_and(|id| n < *id))
            })
            .collect();
        let selected: Vec<_> = remaining.iter().take(PAGE_SIZE).cloned().collect();
        advance_cursor(&mut before, &selected);
        let cursor =
            (remaining.len() > selected.len()).then(|| serde_json::to_string(&before).unwrap());
        let s = self.subscriptions.get_mut(id).unwrap();
        s.loaded.extend(
            selected
                .iter()
                .map(|r| (r["id"].as_str().unwrap().into(), r.clone())),
        );
        Ok(Snapshot {
            topic,
            view_key: key,
            rev: s.rev,
            snapshot: selected,
            total: Some(s.total),
            cursor,
        })
    }
    pub fn move_window(&mut self, id: &str, window: [usize; 2]) -> Result<Delta, String> {
        let s = self
            .subscriptions
            .get(id)
            .ok_or("subscription unavailable")?;
        let (topic, key) = (s.topic.clone(), s.key.clone());
        let full = self.snapshot(&topic, &key)?;
        let s = self.subscriptions.get_mut(id).unwrap();
        s.window = Some(window);
        let delta = update_subscription(s, &full, &BTreeSet::new());
        s.rev = full.rev;
        s.log.clear();
        Ok(delta)
    }
    pub fn remove_view(&mut self, topic: &str, key: &str) {
        if let Some(projection) = self.topics.get_mut(topic) {
            projection.views.remove(key);
        }
    }
    pub fn has_subscribers(&self, topic: &str) -> bool {
        self.subscriptions.values().any(|s| s.topic == topic)
    }
    pub fn set_owner(&mut self, id: &str, owner: &str) {
        if let Some(subscription) = self.subscriptions.get_mut(id) {
            subscription.owner = Some(owner.into());
        }
    }
    pub fn drop_owner(&mut self, owner: &str) {
        self.subscriptions
            .retain(|_, s| s.owner.as_deref() != Some(owner));
    }
    pub fn has_view_subscribers(&self, topic: &str, key: &str) -> bool {
        self.subscriptions
            .values()
            .any(|s| s.topic == topic && s.key == key)
    }
    pub fn unsubscribe(&mut self, id: &str) {
        self.subscriptions.remove(id);
    }
    pub fn reset_subscriptions(
        &mut self,
        topic: &str,
        deltas: &[Delta],
    ) -> Result<Vec<(String, Delta)>, String> {
        let keys: BTreeSet<_> = deltas.iter().map(|delta| delta.view_key.as_str()).collect();
        for (key, view) in &mut self.topic(topic)?.views {
            if keys.contains(key.as_str()) {
                view.log.clear();
            }
        }
        let ids: Vec<_> = self
            .subscriptions
            .iter()
            .filter(|(_, subscription)| {
                subscription.topic == topic && keys.contains(subscription.key.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut resets = Vec::new();
        for id in ids {
            let subscription = self.subscriptions.remove(&id).unwrap();
            let view = &self.topics[topic].views[&subscription.key];
            resets.push((
                id,
                Delta {
                    actor: None,
                    cursor: None,
                    kind: None,
                    topic: topic.into(),
                    view_key: subscription.key,
                    rev: view.rev,
                    upserts: Vec::new(),
                    removes: Vec::new(),
                    request_id: None,
                    total: Some(view.rows.len()),
                    base_rev: None,
                    arrivals: BTreeSet::new(),
                },
            ));
        }
        Ok(resets)
    }
    pub fn patch_view(
        &mut self,
        topic: &str,
        rows: Vec<(Option<String>, Value)>,
        removes: Vec<String>,
        request_id: Option<String>,
    ) -> Result<Vec<Delta>, String> {
        Ok(self.topic(topic)?.patch(rows, removes, request_id))
    }
    pub fn patch(
        &mut self,
        topic: &str,
        rows: Vec<(Option<String>, Value)>,
        removes: Vec<String>,
        request_id: Option<String>,
    ) -> Result<Vec<(String, Delta)>, String> {
        let deltas = self.topic(topic)?.patch(rows, removes, request_id);
        Ok(self.deliver(&deltas))
    }
    pub fn deliver(&mut self, deltas: &[Delta]) -> Vec<(String, Delta)> {
        let mut out = Vec::new();
        for d in deltas {
            let full = self.snapshot(&d.topic, &d.view_key).unwrap();
            for (id, s) in &mut self.subscriptions {
                if s.topic != d.topic || s.key != d.view_key {
                    continue;
                }
                let old_total = s.total;
                let mut delta = update_subscription(s, &full, &d.arrivals);
                delta.request_id = d.request_id.clone();
                delta.actor = d.actor.clone();
                delta.kind = d.kind.clone();
                if !delta.upserts.is_empty()
                    || !delta.removes.is_empty()
                    || delta.total != Some(old_total)
                {
                    s.rev = full.rev;
                    s.log.push_back(delta.clone());
                    if s.log.len() > RETAINED_DELTAS {
                        s.log.pop_front();
                    }
                    out.push((id.clone(), delta));
                }
            }
        }
        out
    }
}
fn select(rows: &[Value], window: Option<[usize; 2]>, paged: bool) -> Vec<Value> {
    let [from, to] = window.unwrap_or([0, if paged { PAGE_SIZE } else { rows.len() }]);
    rows.iter()
        .skip(from)
        .take(to.saturating_sub(from))
        .cloned()
        .collect()
}
fn cursor_team(row: &Value) -> String {
    serde_json::to_string(&row["team"]).unwrap()
}
fn advance_cursor(before: &mut BTreeMap<String, i64>, rows: &[Value]) {
    for row in rows {
        if let Some(id) = row["messageId"].as_i64() {
            before
                .entry(cursor_team(row))
                .and_modify(|n| *n = (*n).min(id))
                .or_insert(id);
        }
    }
}
fn cursor_for(
    rows: &[Value],
    full: &[Value],
    topic: &str,
    window: Option<[usize; 2]>,
) -> Option<String> {
    if matches!(topic, "records" | "files" | "stories") {
        return (full.len() > window.map(|w| w[1]).unwrap_or(rows.len()))
            .then(|| format!("{topic}:next"));
    }
    if topic != "inbox" || window.is_some() || full.len() <= rows.len() {
        return None;
    }
    let mut before = BTreeMap::new();
    for row in full {
        if row["messageId"].is_i64() {
            before.insert(cursor_team(row), i64::MAX);
        }
    }
    advance_cursor(&mut before, rows);
    Some(serde_json::to_string(&before).unwrap())
}
fn update_subscription(
    s: &mut Subscription,
    full: &Snapshot,
    arrivals: &BTreeSet<String>,
) -> Delta {
    let next: BTreeMap<String, Value> = if s.window.is_some() {
        select(&full.snapshot, s.window, false)
            .into_iter()
            .map(|r| (r["id"].as_str().unwrap().into(), r))
            .collect()
    } else {
        full.snapshot
            .iter()
            .filter(|r| {
                s.loaded.contains_key(r["id"].as_str().unwrap())
                    || arrivals.contains(r["id"].as_str().unwrap())
            })
            .map(|r| (r["id"].as_str().unwrap().into(), r.clone()))
            .collect()
    };
    let upserts = next
        .iter()
        .filter(|(id, row)| s.loaded.get(*id) != Some(*row))
        .map(|(_, row)| row.clone())
        .collect();
    let removes = s
        .loaded
        .keys()
        .filter(|id| !next.contains_key(*id))
        .cloned()
        .collect();
    s.loaded = next;
    s.total = full.snapshot.len();
    Delta {
        actor: None,
        cursor: (matches!(s.topic.as_str(), "records" | "files" | "stories")).then(|| {
            cursor_for(
                &s.loaded.values().cloned().collect::<Vec<_>>(),
                &full.snapshot,
                &s.topic,
                s.window,
            )
        }),
        kind: None,
        topic: s.topic.clone(),
        view_key: s.key.clone(),
        rev: full.rev,
        upserts,
        removes,
        request_id: None,
        total: Some(s.total),
        base_rev: Some(s.rev),
        arrivals: BTreeSet::new(),
    }
}

pub trait FieldProvider: Send + Sync {
    fn fields(&self) -> &'static [&'static str];
    fn contribute(&self, source: &Map<String, Value>, row: &mut Map<String, Value>);
}
pub struct FieldProviders {
    owners: BTreeSet<&'static str>,
    providers: Vec<Box<dyn FieldProvider>>,
}
impl FieldProviders {
    pub fn new(process_fields: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            owners: process_fields.into_iter().collect(),
            providers: Vec::new(),
        }
    }
    pub fn reserve(&mut self, fields: impl IntoIterator<Item = &'static str>) {
        self.owners.extend(fields);
    }
    pub fn register(&mut self, provider: Box<dyn FieldProvider>) -> Result<(), String> {
        for field in provider.fields() {
            if self.owners.contains(field) {
                return Err(format!("field already owned: {field}"));
            }
        }
        self.owners.extend(provider.fields().iter().copied());
        self.providers.push(provider);
        Ok(())
    }
    pub fn project(&self, source: &Map<String, Value>, mut process: Map<String, Value>) -> Value {
        for provider in &self.providers {
            let mut contribution = Map::new();
            provider.contribute(source, &mut contribution);
            for field in provider.fields() {
                if let Some(value) = contribution.remove(*field) {
                    process.insert((*field).into(), value);
                }
            }
        }
        Value::Object(process)
    }
}
