// SPDX-License-Identifier: MIT
//! The hosted `media` query: the bytes of one image cached on this Mac, for a
//! message in the dashboard team's inbox, in chunks that fit one live answer.
//! It reads the content-addressed cache only and never fetches anything.

use std::sync::Arc;

use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::agents::{self, AgentHost, Team};
use super::super::catalog::{self, Call, LiveCtx, QueryError};
use super::faces;
use crate::commands::agent_inbox::{AppInboxHost, InboxHost};
use crate::commands::dashboard_launch::LaunchTeam;

/// The largest image a hosted page can load (raw bytes). Larger: `too_large`. The only place
/// the cap is defined: the Mac answers `too_large` and the toolkit never checks a size itself.
pub const MAX_MEDIA_BYTES: usize = 5 * 1024 * 1024;
/// Raw bytes per answer. Base64 makes 245,760 characters, inside the 256 KB answer cap.
pub const CHUNK_BYTES: usize = 180 * 1024;
const _: () = assert!(CHUNK_BYTES.div_ceil(3) * 4 + 1024 <= super::super::live::MAX_ANSWER_BYTES);

/// An attachment as the hosted projection shows it with `media` declared. Never the
/// source URL or host, or the failure reason.
pub(crate) fn projected(a: &crate::media::Attachment) -> Value {
    let mut out = json!({"kind": a.kind, "alt": a.alt, "state": a.state});
    for (key, value) in [
        ("width", a.width.map(u64::from)),
        ("height", a.height.map(u64::from)),
        ("bytes", a.bytes),
    ] {
        if let Some(value) = value {
            out[key] = json!(value);
        }
    }
    for (key, value) in [("mime", &a.mime), ("sha256", &a.sha256)] {
        if let Some(value) = value {
            out[key] = json!(value);
        }
    }
    out
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Args {
    message_id: i64,
    sha256: String,
    #[serde(default)]
    offset: usize,
}

fn refused(reason: &str) -> Value {
    json!({"refused": reason})
}

pub(crate) fn query(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        // `fields` holds the dashboard's `live.fields.inbox`: attachments need `media`
        // in it, and a sender's face needs `faces`.
        let declared = |name: &str| call.fields.iter().any(|f| f == name);
        let (media, faces) = (declared("media"), declared("faces"));
        if !media && !faces {
            return Err(QueryError::NotApproved(
                "inbox media is not declared".into(),
            ));
        }
        let args: Args = catalog::strict(call.args, catalog::query_args("media"))?;
        if args.message_id <= 0
            || args.sha256.len() != 64
            || !args
                .sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(QueryError::InvalidArgs("messageId or sha256".into()));
        }
        let agents = agents::host_of(&ctx)?;
        let team = agents::scoped_team(&agents, &call.folder, call.scope.as_ref()).await?;
        let agent_team = team.clone();
        let team = LaunchTeam {
            org: team.org,
            team: team.team,
        };
        let host: Arc<dyn InboxHost> = match &ctx.inbox {
            Some(host) => host.clone(),
            None => Arc::new(AppInboxHost {
                app: ctx.app.clone().ok_or_else(super::failed)?,
            }),
        };
        let faces = if faces {
            let sessions = agents.sessions().await.map_err(|_| super::failed())?;
            let known = host.sessions().await.map_err(|_| super::failed())?;
            Some(FaceProof {
                agents,
                team: agent_team,
                sessions,
                known,
            })
        } else {
            None
        };
        // Only this team's store is opened, so a message or hash of another team is
        // simply not there.
        let path = host.team_store(&team).await.map_err(|_| super::failed())?;
        catalog::blocking(call.hold, "live.media", move || {
            Ok(chunk(&*host, &team, path, &args, media, faces.as_ref()))
        })
        .await
        .map_err(|_| super::failed())?
    })
}

/// What proves a face hash: the sender's face is drawn again from the same sources the
/// projection used, for the sender of the message asked about.
struct FaceProof {
    agents: Arc<dyn AgentHost>,
    team: Team,
    sessions: Vec<crate::mcp::session_list::SessionListRow>,
    /// The sessions the inbox names senders by: the projection's `fromName` comes from these.
    known: crate::commands::agent_inbox::KnownSessions,
}

fn chunk(
    host: &dyn InboxHost,
    team: &LaunchTeam,
    path: std::path::PathBuf,
    args: &Args,
    attachments_declared: bool,
    faces: Option<&FaceProof>,
) -> Result<Value, QueryError> {
    // RI-2-ALLOW: SQLite work runs on the blocking pool.
    let store = crate::weft::WeftStore::open(path).map_err(|_| super::failed())?;
    let Some(message) =
        crate::weft::inbox::inbox_get(&store, args.message_id).map_err(|_| super::failed())?
    else {
        return Ok(refused("not_found"));
    };
    if attachments_declared {
        let mut attachments = message.attachments;
        host.present_media(team, &mut attachments);
        if let Some(attachment) = attachments
            .iter()
            .find(|a| a.kind == "image" && a.sha256.as_deref() == Some(args.sha256.as_str()))
        {
            return attachment_chunk(host, attachment, args);
        }
    }
    // Not an attachment of this message (or attachments are not declared): the face of its
    // sender, if that is what the hash names. Anything else is the same `not_found`.
    if let Some(proof) = faces {
        if let Some(faces::Face::Image(image)) =
            faces::Faces::new(&*proof.agents, &proof.team, &proof.sessions).resolve(
                &message.from,
                &crate::commands::agent_inbox::address_name(host, &proof.known, &message.from),
            )
        {
            if image.sha256 == args.sha256 {
                return serve(&image.png, args);
            }
        }
    }
    Ok(refused("not_found"))
}

fn attachment_chunk(
    host: &dyn InboxHost,
    attachment: &crate::media::Attachment,
    args: &Args,
) -> Result<Value, QueryError> {
    if attachment.state != "cached" {
        return Ok(refused("not_cached"));
    }
    if attachment.bytes.is_some_and(|n| n > MAX_MEDIA_BYTES as u64) {
        return Ok(json!({"refused": "too_large", "bytes": attachment.bytes}));
    }
    let Ok(bytes) = host.cached_media(&args.sha256) else {
        return Ok(refused("not_cached"));
    };
    serve(&bytes, args)
}

/// One chunk of `bytes`, which must be an image of at most [`MAX_MEDIA_BYTES`] that still
/// hashes to the requested name.
fn serve(bytes: &[u8], args: &Args) -> Result<Value, QueryError> {
    if bytes.len() > MAX_MEDIA_BYTES {
        return Ok(json!({"refused": "too_large", "bytes": bytes.len()}));
    }
    // The cache names files by hash; a file that no longer matches is not served.
    if format!("{:x}", Sha256::digest(bytes)) != args.sha256 {
        return Ok(refused("not_cached"));
    }
    let Ok((mime, width, height)) = crate::media::inspect(bytes) else {
        return Ok(refused("not_cached"));
    };
    if args.offset >= bytes.len() {
        return Err(QueryError::InvalidArgs("offset".into()));
    }
    let end = (args.offset + CHUNK_BYTES).min(bytes.len());
    Ok(json!({
        "mime": mime, "width": width, "height": height,
        "total": bytes.len(), "offset": args.offset, "length": end - args.offset,
        "done": end == bytes.len(),
        "data": base64::engine::general_purpose::STANDARD.encode(&bytes[args.offset..end]),
    }))
}

/// `catalog::redact` for a `media` answer, except the `data` string: image bytes that
/// passed the magic-byte check and the hash match, whose base64 the secret scan could
/// corrupt. `data` must be strict base64, else it is dropped. Every other string is redacted.
pub(crate) fn redact(mut answer: Value) -> Value {
    let data = answer.as_object_mut().and_then(|m| m.remove("data"));
    let mut answer = catalog::redact(answer);
    if let Some(Value::String(data)) = data {
        let strict = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .is_ok();
        if strict {
            answer["data"] = Value::String(data);
        }
    }
    answer
}
