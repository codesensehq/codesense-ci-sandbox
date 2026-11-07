// SPDX-License-Identifier: MIT
//! The face a message's sender wears, as a hosted page is given it (`faces` declared).
//!
//! One resolver serves both sides: the projection names a sender's face on each row, and
//! the `media` query proves a requested hash is that face by running the same resolver
//! again for the sender of the message it is asked about. Nothing is stored, so a hash
//! that is not the face of a sender in the dashboard team's inbox is simply not found.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::super::agents::{AgentHost, Team};
use crate::avatars::hosted::{self, FaceImage, FaceRef};
use crate::avatars::pool;
use crate::avatars::FaceFormat;
use crate::commands::local_profile::MAX_IDENTITY_AVATAR_BYTES;
use crate::mcp::session_list::{self, SessionListRow};

/// A sender's face.
#[derive(Debug, Clone)]
pub(crate) enum Face {
    /// A picture, drawn to a small PNG.
    Image(Arc<FaceImage>),
    /// A glyph such as an emoji.
    Emoji(String),
    /// A `#rrggbb` colour; the page draws the sender's initials on it.
    Colour(String),
}

impl Face {
    /// The face as a row's `sender.face`.
    pub(crate) fn projected(&self) -> Value {
        match self {
            Face::Image(image) => json!({
                "sha256": image.sha256, "mime": "image/png",
                "width": image.width, "height": image.height,
            }),
            Face::Emoji(emoji) => json!({"emoji": emoji}),
            Face::Colour(colour) => json!({"colour": colour}),
        }
    }
}

/// Resolves the faces of senders in one team, reading the shared sources once.
pub(crate) struct Faces<'a> {
    host: &'a dyn AgentHost,
    team: &'a Team,
    sessions: &'a [SessionListRow],
    /// The refs agents with no face of their own are dealt from, read on first need.
    pool: OnceCell<Vec<String>>,
}

impl<'a> Faces<'a> {
    pub(crate) fn new(
        host: &'a dyn AgentHost,
        team: &'a Team,
        sessions: &'a [SessionListRow],
    ) -> Self {
        Self {
            host,
            team,
            sessions,
            pool: OnceCell::new(),
        }
    }

    /// The face refs `from` may wear, most preferred first: a session's own avatar, then its
    /// saved Agent's default. The human has only the one from their profile.
    fn refs(&self, from: &str) -> Vec<String> {
        if from == crate::weft::inbox::HUMAN {
            return self.host.human_avatar().into_iter().collect();
        }
        let Some(session) = session_list::row_for_handle(self.sessions, from) else {
            return Vec::new();
        };
        let mut refs: Vec<String> = session.avatar.iter().cloned().collect();
        if let Some(avatar) = session
            .agent
            .as_deref()
            .and_then(|key| self.host.agent_avatar(key))
        {
            refs.push(avatar);
        }
        refs
    }

    /// The face dealt to an agent with none, as the app deals it. A session is dealt by its
    /// saved Agent's key, else its name, else its handle (the agents list). A sender with no
    /// session is dealt as the in-app Inbox deals it: by its display name `from_name`, else
    /// its handle.
    fn dealt(&self, from: &str, from_name: &str) -> Option<String> {
        let key = match session_list::row_for_handle(self.sessions, from) {
            Some(session) => session.agent.as_deref().unwrap_or(&session.display_name),
            None => pool::inbox_key(from, from_name),
        };
        let pool = self.pool.get_or_init(|| {
            self.host
                .default_pool(self.team)
                .unwrap_or_else(|| pool::builtin_refs(pool::FALLBACK_AVATAR_SET))
        });
        pool::deal(pool, key).map(str::to_string)
    }

    /// What `value` names. `Err(())` means it names a picture the phone is never given, and
    /// the sender then has no face at all (not the next ref, not a dealt one): the shipped
    /// packs' SVG is drawn, a team's or user's SVG (a markup file that anyone with write
    /// access to the pack can fill) is not.
    fn resolve_ref(&self, value: &str) -> Result<Option<Face>, ()> {
        let Some(named) = hosted::classify(value) else {
            return Ok(None);
        };
        let source = match named {
            FaceRef::Colour(colour) => return Ok(Some(Face::Colour(colour))),
            FaceRef::Glyph(glyph) => return Ok(Some(Face::Emoji(glyph.to_string()))),
            FaceRef::Pack(pack, id) if pack.starts_with(crate::avatars::BUILTIN_PREFIX) => {
                let Some(bytes) = crate::http::routes::dashboard_kit::builtin_face(pack, id) else {
                    return Ok(None);
                };
                (FaceFormat::Svg, bytes.to_vec())
            }
            FaceRef::Pack(pack, id) => match self.host.avatar_face(self.team, pack, id) {
                Some((FaceFormat::Svg, _)) => return Err(()),
                Some(found) => found,
                None => return Ok(None),
            },
            FaceRef::Data(url) => match hosted::from_data_url(url, MAX_IDENTITY_AVATAR_BYTES) {
                Ok((FaceFormat::Svg, _)) => return Err(()),
                Ok(found) => found,
                Err(_) => return Ok(None),
            },
        };
        Ok(hosted::normalize(source.0, &source.1).ok().map(Face::Image))
    }

    /// The face `from` wears: the first of its refs that resolves (one that does not, such
    /// as a pack this Mac lacks, falls through to the next, as the app's own list does),
    /// else, for an agent, the face it is dealt. The human has none beyond their own. A ref
    /// that names a team or user SVG ends the search with no face.
    pub(crate) fn resolve(&self, from: &str, from_name: &str) -> Option<Face> {
        for value in self.refs(from) {
            match self.resolve_ref(&value) {
                Ok(Some(face)) => return Some(face),
                Ok(None) => {}
                Err(()) => return None,
            }
        }
        if from == crate::weft::inbox::HUMAN {
            return None;
        }
        self.resolve_ref(&self.dealt(from, from_name)?)
            .ok()
            .flatten()
    }

    /// The face of each distinct sender (`from`, display name) in `senders`, resolved once.
    pub(crate) fn faces_for_rows<'f>(
        &self,
        senders: impl IntoIterator<Item = (&'f str, &'f str)>,
    ) -> HashMap<String, Option<Face>> {
        let mut out = HashMap::new();
        for (from, from_name) in senders {
            if !out.contains_key(from) {
                out.insert(from.to_string(), self.resolve(from, from_name));
            }
        }
        out
    }
}
