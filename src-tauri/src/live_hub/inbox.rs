// SPDX-License-Identifier: MIT
use crate::commands::{
    agent_inbox::{Announced, SessionUnread, UnreadByHandle},
    dashboard_launch::LaunchTeam,
};
use crate::weft::inbox::{InboxMessage, MessageKind, HUMAN};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const INBOX_FIELDS: &[&str] = &[
    "unread",
    "oldestUnreadAt",
    "groupUnread",
    "groupReadId",
    "mailNotice",
    "notifiedAt",
];
#[cfg(test)]
thread_local! {
    /// Runs where the view's rows are materialised, so a test can park that work.
    pub(super) static VIEW_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}
pub fn team_key(team: &LaunchTeam) -> String {
    super::view_key::team_key(&team.org, &team.team)
}
pub fn message_key(team: &LaunchTeam, id: i64) -> String {
    format!("message:{}/{}/{id}", team.org, team.team)
}
#[derive(Default, Clone)]
struct TeamInbox {
    messages: BTreeMap<i64, (InboxMessage, MessageKind)>,
    unread: BTreeMap<String, BTreeSet<(i64, i64)>>,
    needs: BTreeMap<String, BTreeSet<i64>>,
    announced: BTreeMap<String, Announced>,
    holders: crate::weft::inbox::RoleHolders,
    conversations: Arc<Vec<crate::commands::agent_read_model::ConversationSummary>>,
}
impl TeamInbox {
    fn session_unread(&self, team: &LaunchTeam, recipient: &str) -> Option<SessionUnread> {
        let entries = self
            .unread
            .get(recipient)
            .filter(|entries| !entries.is_empty())?;
        Some(SessionUnread {
            count: entries.len() as u32,
            oldest_at_ms: entries.first().map(|(at, _)| *at),
            newest_id: entries.iter().map(|(_, id)| *id).max(),
            team: team.clone(),
            announced: self.announced.get(recipient).cloned(),
        })
    }
    fn index(&mut self, m: &InboxMessage, kind: MessageKind, add: bool) {
        let pending = m
            .delivery
            .as_ref()
            .map_or(m.read_at_ms.is_none() && m.withdrawn_at_ms.is_none(), |d| {
                d.state == crate::weft::inbox::InboxDeliveryState::Queued
            });
        if !pending {
            return;
        }
        let recipient = m
            .role
            .as_ref()
            .map(|role| format!("#{role}"))
            .unwrap_or_else(|| m.to.clone());
        if recipient != HUMAN || (m.from != HUMAN && m.archived_at_ms.is_none()) {
            let entries = self.unread.entry(recipient).or_default();
            if add {
                entries.insert((m.created_at_ms, m.id));
            } else {
                entries.remove(&(m.created_at_ms, m.id));
            }
        }
        if m.to == HUMAN
            && m.from != HUMAN
            && kind == MessageKind::NeedsYou
            && m.archived_at_ms.is_none()
        {
            let entries = self.needs.entry(m.from.clone()).or_default();
            if add {
                entries.insert(m.id);
            } else {
                entries.remove(&m.id);
            }
        }
    }
}
#[derive(Default, Clone)]
pub(crate) struct InboxProjection {
    /// Team history is shared copy-on-write: ordinary no-mail refreshes never deep-copy messages.
    teams: BTreeMap<String, (LaunchTeam, Arc<TeamInbox>)>,
    pub session_fields: BTreeMap<String, Value>,
    known_sessions: Vec<crate::commands::agent_inbox::KnownSession>,
    session_unread: UnreadByHandle,
    pub bridge_rows: BTreeMap<String, TeamRows>,
}
/// A team's message rows. Rows are shared, so a reader takes handles under the hub lock and
/// builds its view after releasing it.
pub(crate) type TeamRows = Arc<BTreeMap<i64, Arc<crate::commands::agent_inbox::InboxRow>>>;
/// What a view needs from the projection: shared handles, cheap to take under the hub lock.
pub(crate) struct ViewParts {
    rows: Option<TeamRows>,
    human_unread: u32,
    conversations: Arc<Vec<crate::commands::agent_read_model::ConversationSummary>>,
}
impl ViewParts {
    /// Clones every row; call it with the hub lock released.
    pub(crate) fn build(self) -> crate::commands::agent_inbox::InboxView {
        #[cfg(test)]
        VIEW_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
        crate::commands::agent_inbox::InboxView {
            messages: self
                .rows
                .iter()
                .flat_map(|rows| rows.values().rev().map(|row| (**row).clone()))
                .collect(),
            human_unread: self.human_unread,
            conversations: self.conversations.as_ref().clone(),
            has_more: false,
            matched: 0,
        }
    }
}
impl InboxProjection {
    pub(crate) fn view_parts(&self, team: &LaunchTeam) -> ViewParts {
        let key = team_key(team);
        let state = self.teams.get(&key).map(|(_, s)| s);
        let human_unread = state
            .and_then(|s| s.unread.get(HUMAN))
            .map(|set| set.len() as u32)
            .unwrap_or(0);
        ViewParts {
            rows: self.bridge_rows.get(&key).cloned(),
            human_unread,
            conversations: state.map(|s| s.conversations.clone()).unwrap_or_default(),
        }
    }
    fn index_conversations(&mut self, team: &LaunchTeam) {
        let key = team_key(team);
        let (messages, conversations) = {
            let (team, state) = self.teams.get(&key).expect("team exists");
            let messages: BTreeMap<i64, InboxMessage> = state
                .messages
                .iter()
                .map(|(id, (m, _))| (*id, m.clone()))
                .collect();
            let conversations = Arc::new(crate::commands::agent_read_model::conversation_summaries(
                team, &messages,
            ));
            (messages, conversations)
        };
        let state = Arc::make_mut(&mut self.teams.get_mut(&key).unwrap().1);
        state.conversations = conversations;
        let Some(rows) = self.bridge_rows.get_mut(&key) else {
            return;
        };
        // Off the hub model lock we can COW-update the shared row map in place,
        // cloning only rows whose conversation key actually changed.
        let rows = Arc::make_mut(rows);
        for (id, row) in rows.iter_mut() {
            let new_key =
                crate::commands::agent_read_model::conversation_key(&messages[id], &messages);
            if row.conversation != new_key {
                let mut cloned = (**row).clone();
                cloned.conversation = new_key;
                *row = Arc::new(cloned);
            }
        }
    }
    pub fn contains_team(&self, team: &LaunchTeam) -> bool {
        self.teams.contains_key(&team_key(team))
    }
    pub fn human_unread_total(&self) -> u32 {
        self.teams
            .values()
            .filter_map(|(_, state)| state.unread.get(HUMAN))
            .map(|entries| entries.len() as u32)
            .sum()
    }
    pub fn retain_teams(&mut self, allowed: &BTreeSet<String>) -> Vec<String> {
        let retired: Vec<_> = self
            .teams
            .keys()
            .filter(|key| !allowed.contains(*key))
            .cloned()
            .collect();
        let mut removes = Vec::new();
        for key in retired {
            let (team, state) = self.teams.remove(&key).unwrap();
            removes.extend(state.messages.keys().map(|id| message_key(&team, *id)));
            removes.push(format!("counts:{key}"));
            self.bridge_rows.remove(&key);
            self.session_fields
                .retain(|_, fields| fields["team"] != json!(team));
        }
        self.session_unread
            .found
            .retain(|_, unread| allowed.contains(&team_key(&unread.team)));
        removes
    }
    fn ensure_team(&mut self, team: &LaunchTeam) -> &mut Arc<TeamInbox> {
        let key = team_key(team);
        if !self.teams.contains_key(&key) {
            self.teams
                .insert(key.clone(), (team.clone(), Arc::new(TeamInbox::default())));
        }
        &mut self.teams.get_mut(&key).unwrap().1
    }
    pub fn put(&mut self, team: &LaunchTeam, message: InboxMessage, kind: MessageKind) {
        let state = self.ensure_team(team);
        if state.messages.get(&message.id) == Some(&(message.clone(), kind)) {
            return;
        }
        let state = Arc::make_mut(state);
        if let Some((old, kind)) = state.messages.remove(&message.id) {
            state.index(&old, kind, false);
        }
        state.index(&message, kind, true);
        state.messages.insert(message.id, (message, kind));
    }
    pub fn remove(&mut self, team: &LaunchTeam, id: i64) {
        if let Some(rows) = self.bridge_rows.get_mut(&team_key(team)) {
            Arc::make_mut(rows).remove(&id);
        }
        if let Some((_, state)) = self.teams.get_mut(&team_key(team)) {
            let state = Arc::make_mut(state);
            if let Some((old, kind)) = state.messages.remove(&id) {
                state.index(&old, kind, false);
            }
        }
    }
    pub fn set_announced(&mut self, team: &LaunchTeam, announced: BTreeMap<String, Announced>) {
        let state = Arc::make_mut(self.ensure_team(team));
        state.announced = announced;
    }
    pub fn counts(&self, team: &LaunchTeam) -> Value {
        let key = team_key(team);
        let state = self.teams.get(&key).map(|(_, s)| s);
        let recipients: BTreeMap<_, _> = state
            .into_iter()
            .flat_map(|s| s.unread.iter())
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();
        let needs: BTreeMap<_, _> = state
            .into_iter()
            .flat_map(|s| s.needs.iter())
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();
        let fields: BTreeMap<_, _> = self
            .session_fields
            .iter()
            .filter(|(_, v)| v["team"] == json!(team))
            .map(|(h, v)| (h.clone(), v.clone()))
            .collect();
        json!({"id":format!("counts:{key}"),"rowType":"counts","team":team,"humanUnread":recipients.get(HUMAN).copied().unwrap_or(0),"recipients":recipients,"needsYouBySender":needs,"sessions":fields})
    }
    pub fn count_rows(&self) -> Vec<(Option<String>, Value)> {
        self.teams
            .values()
            .map(|(team, _)| (Some(team_key(team)), self.counts(team)))
            .collect()
    }
    pub fn unread(&self) -> UnreadByHandle {
        self.session_unread.clone()
    }
    fn all_unread(&self) -> UnreadByHandle {
        let mut out = UnreadByHandle::default();
        for (team, state) in self.teams.values() {
            for recipient in state
                .unread
                .keys()
                .filter(|r| r.as_str() != HUMAN && !r.starts_with('#'))
            {
                if let Some(summary) = state.session_unread(team, recipient) {
                    out.found.insert(recipient.clone(), summary);
                }
            }
        }
        out
    }
    pub fn fill_session_fields(&mut self, rows: &[crate::mcp::session_list::SessionListRow]) {
        let mut joined = rows.to_vec();
        let mut unread = self.all_unread();
        // Direct session mail belongs to its launch team's inbox; cross-team rollups use recipients.
        for row in &joined {
            if let Some(team) = &row.team {
                if let Some((_, state)) = self.teams.get(&team_key(team)) {
                    if let Some(summary) = state.session_unread(team, &row.handle) {
                        unread.found.insert(row.handle.clone(), summary);
                    } else {
                        unread.found.remove(&row.handle);
                    }
                } else {
                    unread.failed.insert(row.handle.clone());
                }
            }
        }
        crate::mcp::session_list::with_unread(&mut joined, &unread);
        self.session_unread = unread;
        self.session_fields = joined
            .iter()
            .map(|row| {
                let source = serde_json::to_value(row).unwrap();
                let mut fields = serde_json::Map::new();
                fields.insert("team".into(), source["team"].clone());
                for field in INBOX_FIELDS {
                    fields.insert((*field).into(), source[*field].clone());
                }
                (row.handle.clone(), Value::Object(fields))
            })
            .collect();
    }
    pub fn apply_batch(
        &mut self,
        host: &dyn crate::commands::agent_inbox::InboxHost,
        sessions: &crate::commands::agent_inbox::KnownSessions,
        team: &LaunchTeam,
        batch: InboxBatch,
        asked: &[i64],
    ) -> (Vec<(Option<String>, Value)>, Vec<String>) {
        let found: BTreeSet<_> = batch.messages.iter().map(|m| m.id).collect();
        let removes = asked
            .iter()
            .filter(|id| !found.contains(id))
            .map(|id| {
                self.remove(team, *id);
                message_key(team, *id)
            })
            .collect();
        self.set_announced(team, batch.announced);
        let state = Arc::make_mut(&mut self.teams.get_mut(&team_key(team)).unwrap().1);
        let mut messages: BTreeMap<_, _> = batch.messages.into_iter().map(|m| (m.id, m)).collect();
        if state.holders != batch.holders {
            // Only messages whose kind is inferred can change when a sender joins a role.
            for (message, _) in state.messages.values().filter(|(m, _)| m.kind.is_none()) {
                messages
                    .entry(message.id)
                    .or_insert_with(|| message.clone());
            }
        }
        state.holders = batch.holders.clone();
        let reactions = batch.reactions;
        let mut updates: Vec<(Option<String>, Value)> = messages
            .into_values()
            .map(|message| {
                // Only a row the batch read carries fresh reactions; a row re-projected for
                // its kind or its session keeps the ones it had.
                let on_message: Option<Vec<_>> = found.contains(&message.id).then(|| {
                    reactions
                        .iter()
                        .filter(|r| r.message_id == message.id)
                        .collect()
                });
                self.project_message(
                    host,
                    sessions,
                    team,
                    &batch.holders,
                    message,
                    on_message.as_deref(),
                )
            })
            .collect();
        self.index_conversations(team);
        for (_, row) in &mut updates {
            if let Some(bridge) = row["messageId"].as_i64().and_then(|id| {
                self.bridge_rows
                    .get(&team_key(team))
                    .and_then(|rows| rows.get(&id))
            }) {
                row["message"]["conversation"] = json!(bridge.conversation);
                row["conversation"]["conversation"] = json!(bridge.conversation);
            }
        }
        (updates, removes)
    }
    fn project_message(
        &mut self,
        host: &dyn crate::commands::agent_inbox::InboxHost,
        sessions: &crate::commands::agent_inbox::KnownSessions,
        team: &LaunchTeam,
        holders: &crate::weft::inbox::RoleHolders,
        message: InboxMessage,
        reactions: Option<&[&crate::weft::reactions::Reaction]>,
    ) -> (Option<String>, Value) {
        let kind = crate::commands::agent_inbox::message_kind(&message, holders, sessions);
        let mut bridge =
            crate::commands::agent_inbox::inbox_row(host, sessions, holders, message.clone());
        host.present_media(team, &mut bridge.attachments);
        let mut conversation = crate::commands::agent_read_model::conversation_message(
            host,
            sessions,
            holders,
            team,
            message.clone(),
        );
        // Presentation refreshes reuse the full-history key; a write batch reindexes it below.
        if let Some(existing) = self
            .bridge_rows
            .get(&team_key(team))
            .and_then(|rows| rows.get(&message.id))
        {
            bridge.conversation = existing.conversation.clone();
            conversation.conversation = existing.conversation.clone();
            bridge.reactions = existing.reactions.clone();
        }
        if let Some(reactions) = reactions {
            bridge.reactions = crate::weft::reactions::group_reactions(reactions);
        }
        conversation.reactions = bridge.reactions.clone();
        let (id, created_at) = (message.id, message.created_at_ms);
        self.put(team, message, kind);
        let row = json!({"id":message_key(team,id),"rowType":"message","team":team,"messageId":id,"createdAt":created_at,"message":bridge,"conversation":conversation});
        Arc::make_mut(self.bridge_rows.entry(team_key(team)).or_default())
            .insert(id, Arc::new(bridge));
        (Some(team_key(team)), row)
    }
    pub fn refresh_sessions(
        &mut self,
        host: &dyn crate::commands::agent_inbox::InboxHost,
        rows: &[crate::mcp::session_list::SessionListRow],
    ) -> Vec<(Option<String>, Value)> {
        use crate::commands::agent_inbox::{KnownSession, KnownSessions};
        let next: Vec<_> = rows.iter().map(KnownSession::from).collect();
        if next == self.known_sessions {
            return Vec::new();
        }
        // Presentation can name either a handle or its tab session. Reproject affected
        // cached messages without rereading SQLite or recounting unchanged mail.
        let changed: BTreeSet<_> = self
            .known_sessions
            .iter()
            .chain(&next)
            .filter(|s| {
                self.known_sessions
                    .iter()
                    .find(|old| old.handle == s.handle)
                    != next.iter().find(|new| new.handle == s.handle)
            })
            .flat_map(|s| std::iter::once(s.handle.clone()).chain(s.session.clone()))
            .collect();
        self.known_sessions = next.clone();
        let sessions = KnownSessions::new(next);
        let affected: Vec<_> = self
            .teams
            .values()
            .flat_map(|(team, state)| {
                state
                    .messages
                    .values()
                    .filter(|(m, _)| {
                        changed.contains(&m.from)
                            || changed.contains(&m.to)
                            || m.reply_to.as_ref().is_some_and(|to| changed.contains(to))
                    })
                    .map(|(m, _)| (team.clone(), state.holders.clone(), m.clone()))
            })
            .collect();
        affected
            .into_iter()
            .map(|(team, holders, m)| {
                self.project_message(host, &sessions, &team, &holders, m, None)
            })
            .collect()
    }
    pub fn join(&self, rows: &mut [crate::mcp::session_list::SessionListRow]) {
        for row in rows {
            if let Some(fields) = self.session_fields.get(&row.handle) {
                row.unread = fields["unread"].as_u64().map(|n| n as u32);
                row.oldest_unread_at = fields["oldestUnreadAt"].as_str().map(str::to_string);
                row.mail_notice = match fields["mailNotice"].as_str() {
                    Some("pending") => Some(crate::mcp::session_list::MailNotice::Pending),
                    Some("told") => Some(crate::mcp::session_list::MailNotice::Told),
                    _ => None,
                };
                row.notified_at = fields["notifiedAt"].as_str().map(str::to_string);
                row.group_unread = fields["groupUnread"].as_u64().unwrap_or(0) as u32;
                row.group_read_id = fields["groupReadId"].as_i64().unwrap_or(0);
            }
        }
    }
}

/// The same bounded SQL read and application path serves production and write-seam tests.
pub(crate) struct InboxBatch {
    messages: Vec<InboxMessage>,
    reactions: Vec<crate::weft::reactions::Reaction>,
    holders: crate::weft::inbox::RoleHolders,
    announced: BTreeMap<String, Announced>,
}
pub(crate) async fn read_batch(
    path: std::path::PathBuf,
    initial: bool,
    ids: Vec<i64>,
) -> Result<InboxBatch, String> {
    crate::commands::agent_inbox::with_store("live_hub.inbox", path, move |store| {
        let messages = if initial {
            let mut out = Vec::new();
            let mut before = None;
            loop {
                let page = crate::weft::inbox::inbox_page(store, before, 200)?;
                let len = page.len();
                before = page.last().map(|m| m.id);
                out.extend(page);
                if len < 200 {
                    break;
                }
            }
            out
        } else {
            crate::weft::inbox::inbox_rows_by_id(store, &ids)?
        };
        let reactions = if initial {
            crate::weft::reactions::reactions_all(store)?
        } else {
            crate::weft::reactions::reactions_for(store, &ids)?
        };
        Ok(InboxBatch {
            messages,
            reactions,
            holders: crate::weft::inbox::role_holders(store)?,
            announced: crate::weft::inbox::inbox_announced(store)?,
        })
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_unread_total_includes_every_cached_team_and_excludes_self_mail() {
        let dir = tempfile::tempdir().unwrap();
        let mut projection = InboxProjection::default();
        for name in ["default", "design"] {
            let team = LaunchTeam {
                org: "acme".into(),
                team: name.into(),
            };
            let store =
                crate::weft::WeftStore::open(dir.path().join(format!("{name}.sqlite3"))).unwrap();
            for from in ["worker", HUMAN] {
                let id = crate::weft::inbox::inbox_insert(
                    &store, from, HUMAN, "mail", None, None, None, None, None,
                )
                .unwrap();
                let message = crate::weft::inbox::inbox_get(&store, id).unwrap().unwrap();
                let kind = message.effective_kind();
                projection.put(&team, message, kind);
            }
            assert_eq!(projection.counts(&team)["humanUnread"], 1);
        }
        assert_eq!(projection.human_unread_total(), 2);
    }
}
