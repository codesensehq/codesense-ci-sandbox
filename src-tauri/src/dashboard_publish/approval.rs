// SPDX-License-Identifier: MIT
//! Which `live` block of a dashboard is on (SW-DP-4b, SV-PBF-286).
//!
//! The manifest's `live` block is a request; a person confirms it. A block
//! turns on only when a person publishes it or confirms it in the app
//! (`confirm`); an agent's publish and the watcher's `follow` apply a block
//! only when it equals or narrows the confirmed one (`gate`), and hold any
//! other as a `LiveRequest` on the record. The saved block is tied to the
//! canonical sha of the manifest's block, the slug and the folder, and must be
//! confirmed, so answers stop at once when they no longer match (fail closed)
//! until the record follows the manifest again. A record saved before
//! confirmation existed is unconfirmed and serves nothing until a person
//! confirms it.

use std::path::Path;

use serde::Serialize;

use super::manifest::Manifest;
use super::state::{self, LiveApproved, LocalState, PublishRecord, Scope};

/// The `live` field of the publish result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveResult {
    /// The block was turned on: it is new, or it changed (queries, fields,
    /// folder or team).
    On,
    /// The block is the one already on.
    Unchanged,
    /// The manifest has no `live` block: live data is off.
    None,
    /// The block is new or wider and waits for a person to confirm it in the
    /// app (SV-PBF-286). Nothing streams until then.
    #[serde(rename = "pending_confirmation")]
    PendingConfirmation,
    /// The person declined the block: it stays off.
    Declined,
}

/// What `decide` settled: the result for the tool and the value for P2
/// `live_approved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub result: LiveResult,
    pub approved: Option<LiveApproved>,
}

/// The saved form of `manifest`'s `live` block for `folder` (already a
/// `state::folder_key`), with the `queryContext` scope `scope`.
fn entry(manifest: &Manifest, folder: String, scope: Option<Scope>) -> Option<LiveApproved> {
    let queries = manifest.live.as_ref()?;
    super::manifest::validate_pin_scope(&manifest.live_args, scope.as_ref()).ok()?;
    Some(LiveApproved {
        sha: manifest.live_sha()?,
        actions: manifest.actions.clone(),
        queries: queries.clone(),
        folder,
        fields: manifest.live_fields.clone(),
        args: manifest.live_args.clone(),
        streams: manifest.live_streams.clone(),
        control: manifest.live_control,
        embeds: manifest.live_embeds.clone(),
        scope,
        approved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        // The block as the manifest wrote it. Whether it is stored as
        // confirmed is `gate`'s call (SV-PBF-286).
        confirmed: true,
    })
}

/// Who asks for a `live` block (SV-PBF-286). Only a person at the native app
/// may turn live data on or widen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    /// A person, through the native app (the UI publish or a confirmation).
    Human,
    /// An MCP caller or the watcher following an edited `dashboard.yml`.
    Agent,
}

impl Initiator {
    pub fn label(self) -> &'static str {
        match self {
            Initiator::Human => "human",
            Initiator::Agent => "agent",
        }
    }
}

/// What a gated block becomes: the block to store and the one held for a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub applied: Option<LiveApproved>,
    pub hold: Option<LiveApproved>,
}

/// Every entry of `new` is in `old` with the same value.
fn map_within<K: Ord, V: PartialEq>(
    new: &std::collections::BTreeMap<K, V>,
    old: &std::collections::BTreeMap<K, V>,
) -> bool {
    new.iter().all(|(k, v)| old.get(k) == Some(v))
}

/// Every pin `old` set stays, with the same or fewer values: a query pinned in
/// `old` must keep an equal or tighter pin. A removed pin, or an empty value
/// list, is wider (it lets the query answer everything).
fn pins_within(new: &super::manifest::LiveArgs, old: &super::manifest::LiveArgs) -> bool {
    old.iter().all(|(query, old_args)| {
        new.get(query).is_some_and(|new_args| {
            old_args.iter().all(|(arg, old_pins)| {
                new_args.get(arg).is_some_and(|new_pins| {
                    !new_pins.is_empty() && new_pins.iter().all(|p| old_pins.contains(p))
                })
            })
        })
    })
}

/// The fields an answer to `query` may carry: its list, or the default
/// frontmatter fields for `queryContext` (`catalog::effective_fields`).
fn effective_fields(fields: &super::manifest::LiveFields, query: &str) -> Vec<String> {
    fields.get(query).cloned().unwrap_or_else(|| {
        if query == "queryContext" {
            super::catalog::DEFAULT_CONTEXT_FIELDS
                .iter()
                .map(|f| f.to_string())
                .collect()
        } else {
            Vec::new()
        }
    })
}

/// The effective field list of every query is within `old`'s.
fn fields_within(new: &super::manifest::LiveFields, old: &super::manifest::LiveFields) -> bool {
    let queries: std::collections::BTreeSet<&str> = new
        .keys()
        .chain(old.keys())
        .map(String::as_str)
        .chain(std::iter::once("queryContext"))
        .collect();
    queries.into_iter().all(|q| {
        let allowed = effective_fields(old, q);
        effective_fields(new, q).iter().all(|f| allowed.contains(f))
    })
}

/// True when `new` gives more than `old` (a new query, stream, control, a
/// bigger pin or field list, another folder or team). Anything that cannot be
/// shown to be within `old` counts as wider (fail closed).
pub fn widens(new: &LiveApproved, old: &LiveApproved) -> bool {
    use super::manifest::LiveQueries;
    let queries_within = match (&new.queries, &old.queries) {
        (_, LiveQueries::All) => true,
        (LiveQueries::All, LiveQueries::List(_)) => false,
        (LiveQueries::List(n), LiveQueries::List(o)) => n.iter().all(|q| o.contains(q)),
    };
    let scope_within = new.scope.is_none() || new.scope == old.scope;
    !(queries_within
        && scope_within
        && new.folder == old.folder
        && new.streams.iter().all(|s| old.streams.contains(s))
        && (!new.control || old.control)
        && fields_within(&new.fields, &old.fields)
        && pins_within(&new.args, &old.args)
        && map_within(&new.actions, &old.actions)
        && new.embeds == old.embeds)
}

/// SV-PBF-286: the one place a requested block becomes the stored one. A
/// person's block is stored confirmed. Anyone else's is stored only when it
/// equals or narrows the confirmed block `current`; otherwise `current` stays
/// as it is and the request is held for a person. `wanted` is `None` when the
/// manifest has no `live` block: that turns live off for anyone.
pub fn gate(by: Initiator, wanted: Option<LiveApproved>, current: Option<&LiveApproved>) -> Gate {
    let Some(mut wanted) = wanted else {
        return Gate {
            applied: None,
            hold: None,
        };
    };
    let allowed = by == Initiator::Human
        || current.is_some_and(|c| c.confirmed && !widens(&wanted, c));
    if allowed {
        wanted.confirmed = true;
        return Gate {
            applied: Some(wanted),
            hold: None,
        };
    }
    wanted.confirmed = false;
    Gate {
        applied: current.cloned(),
        hold: Some(wanted),
    }
}

/// The ask to store after a publish or watcher pass: `hold` is the block held
/// for a person, `visibility` the wider audience an agent asked for. An ask for
/// the same block that a person declined stays declined (the agent sees
/// `declined`); any other block is a new ask. Viewer asks already waiting stay.
pub fn next_request(
    by: Initiator,
    existing: Option<&state::LiveRequest>,
    hold: Option<LiveApproved>,
    visibility: Option<&str>,
) -> Option<state::LiveRequest> {
    let mut next = match hold {
        Some(hold) => match existing.filter(|r| r.block.as_ref().map(|b| &b.sha) == Some(&hold.sha)) {
            Some(same) => same.clone(),
            None => state::LiveRequest {
                block: Some(hold),
                visibility: None,
                share_add: existing.map(|r| r.share_add.clone()).unwrap_or_default(),
                by: by.label().into(),
                requested_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                declined: false,
            },
        },
        None => match existing.cloned() {
            Some(mut kept) => {
                kept.block = None;
                kept.declined = false;
                kept
            }
            // An audience ask with no block and no earlier ask is a new ask.
            None if visibility.is_some() => state::LiveRequest {
                block: None,
                visibility: None,
                share_add: Vec::new(),
                by: by.label().into(),
                requested_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                declined: false,
            },
            None => return None,
        },
    };
    if let Some(v) = visibility {
        next.visibility = Some(v.into());
    }
    (next.block.is_some() || next.visibility.is_some() || !next.share_add.is_empty())
        .then_some(next)
}

/// What the person saw when a confirmation was put in front of them: the block
/// (by sha), the audience it ends with, and the viewers it adds. A confirm
/// applies only a request that still shows exactly this (H1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shown {
    pub sha: Option<String>,
    pub audience: String,
    pub share_add: Vec<String>,
}

impl Shown {
    /// What `request` shows now, on a dashboard whose `dashboard.yml` says
    /// `file_visibility`.
    pub fn of(request: &state::LiveRequest, file_visibility: &str) -> Shown {
        Shown {
            sha: request.block_waiting().map(|b| b.sha.clone()),
            audience: request
                .visibility
                .clone()
                .unwrap_or_else(|| file_visibility.to_string()),
            share_add: request.share_add.clone(),
        }
    }
}

/// What a publish is bound to: the confirmation a person answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    /// A confirm of the waiting request, exactly as shown. The held block is
    /// applied, never a fresh read of `dashboard.yml`.
    Request(Shown),
    /// The UI publish: the person confirmed, or was not asked about, the block
    /// with this sha (`None`: no block). A manifest that differs under the lock
    /// is no longer what they saw, so it is the agent's.
    Block(Option<String>),
    /// The UI publish dialog was answered "Not now" or "Decline".
    Asked,
}

/// A confirm that no longer matches the request (it changed while the dialog
/// was open).
pub const STALE: &str = "What was asked changed while the question was open. CodeSense will ask again about the current version.";

/// What a publish settles about live data and the audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub gated: Gate,
    pub request: Option<state::LiveRequest>,
    pub live_result: LiveResult,
    /// The audience to send and write.
    pub visibility: super::manifest::Visibility,
    /// `team` when an agent asked for it and it waits for a person.
    pub asked_visibility: Option<&'static str>,
    /// The audience a person confirmed, to store on the record.
    pub confirmed_visibility: Option<super::manifest::Visibility>,
    /// A person should be asked (the native dialog).
    pub announce: bool,
}

/// The publish decision without any I/O: gate the block, hold what only a
/// person may grant, and keep a dashboard that streams terminals at the
/// audience a person confirmed. `bound` ties a person's answer to what they saw.
pub fn settle(
    record: &PublishRecord,
    manifest: &Manifest,
    folder: &Path,
    scope: Option<(&str, &str)>,
    visibility: super::manifest::Visibility,
    initiator: Initiator,
    bound: Option<&Bound>,
) -> Result<Settled, String> {
    use super::manifest::Visibility;
    let mut initiator = initiator;
    let mut held = None;
    match bound {
        Some(Bound::Request(shown)) => {
            let request = record
                .live_request
                .as_ref()
                .filter(|r| r.waiting())
                .ok_or(STALE)?;
            if Shown::of(request, manifest.visibility.as_str()) != *shown {
                return Err(STALE.into());
            }
            if let Some(block) = request.block_waiting() {
                if manifest.live_sha().as_deref() != Some(block.sha.as_str()) {
                    return Err(STALE.into());
                }
                held = Some(block.clone());
            }
        }
        Some(Bound::Block(sha)) if manifest.live_sha() != *sha => {
            initiator = Initiator::Agent;
        }
        _ => {}
    }
    let decision = decide(manifest, folder, record.live_approved.as_ref(), scope);
    let gated = match (bound, held) {
        (Some(Bound::Request(_)), Some(mut block)) => {
            if let Some(scope) = decision.approved.as_ref().and_then(|d| d.scope.clone()) {
                block.scope = Some(scope);
            }
            block.folder = state::folder_key(folder);
            block.confirmed = true;
            Gate {
                applied: Some(block),
                hold: None,
            }
        }
        // An audience-only confirm leaves the block as it is.
        (Some(Bound::Request(_)), None) => Gate {
            applied: record.live_approved.clone(),
            hold: None,
        },
        _ => gate(initiator, decision.approved.clone(), record.live_approved.as_ref()),
    };
    let streams = |b: &Option<LiveApproved>| b.as_ref().is_some_and(|b| !b.streams.is_empty());
    let mut visibility = visibility;
    let mut asked_visibility = None;
    if initiator == Initiator::Agent && visibility == Visibility::Team {
        let widens = if streams(&gated.applied) {
            record.live_visibility.as_deref() != Some(Visibility::Team.as_str())
        } else {
            streams(&gated.hold) && manifest.visibility == Visibility::Private
        };
        if widens {
            visibility = Visibility::Private;
            asked_visibility = Some(Visibility::Team.as_str());
        }
    }
    let by = if matches!(bound, Some(Bound::Asked)) {
        Initiator::Human
    } else {
        initiator
    };
    let existing = record.live_request.as_ref();
    let request = match bound {
        // The answered block goes; a block declined or turned off earlier stays declined.
        Some(Bound::Request(shown)) if shown.sha.is_none() => {
            existing.filter(|r| r.declined && r.block.is_some()).map(|r| state::LiveRequest {
                visibility: None,
                share_add: Vec::new(),
                ..r.clone()
            })
        }
        Some(Bound::Request(_)) => None,
        // A person's publish answers the block; viewer and audience asks stay.
        _ if initiator == Initiator::Human && gated.hold.is_none() => {
            next_request(Initiator::Human, existing, None, None)
        }
        _ => next_request(by, existing, gated.hold.clone(), asked_visibility),
    };
    let live_result = match (&gated.hold, &request) {
        (None, _) => decision.result,
        (Some(_), Some(r)) if r.declined => LiveResult::Declined,
        (Some(_), _) => LiveResult::PendingConfirmation,
    };
    let announce = initiator == Initiator::Agent
        && bound.is_none()
        && request.as_ref().is_some_and(|r| r.waiting());
    Ok(Settled {
        gated,
        request,
        live_result,
        visibility,
        asked_visibility,
        confirmed_visibility: (initiator == Initiator::Human).then_some(visibility),
        announce,
    })
}

/// True when `saved` was saved for `folder` (a `state::folder_key`).
fn same_folder(saved: &LiveApproved, folder: &str) -> bool {
    !saved.folder.is_empty() && state::folder_key(Path::new(&saved.folder)) == folder
}

/// The live state for a publish of `manifest` from `folder`. `scope` is the org
/// and team `queryContext` reads (`catalog::team_of`), `None` when unknown:
/// then the team saved for this folder is kept, and with none saved
/// `queryContext` is refused. A block whose sha, folder and scope equal
/// the current one is `Unchanged`; any other block is `On`; no block clears it.
pub fn decide(
    manifest: &Manifest,
    folder: &Path,
    current: Option<&LiveApproved>,
    scope: Option<(&str, &str)>,
) -> Decision {
    let folder = state::folder_key(folder);
    // A team that cannot be worked out now (the context index is not running,
    // say) keeps the one saved for this folder, as `follow` does: the team
    // belongs to the folder, and `catalog::check_scope` still checks it on
    // every query.
    let scope = scope
        .map(|(org, team)| Scope {
            org: org.into(),
            team: team.into(),
        })
        .or_else(|| {
            current
                .filter(|c| same_folder(c, &folder))
                .and_then(|c| c.scope.clone())
        });
    let Some(sha) = manifest.live_sha() else {
        return Decision {
            result: LiveResult::None,
            approved: None,
        };
    };
    if let Some(current) =
        current.filter(|c| c.sha == sha && same_folder(c, &folder) && c.scope == scope)
    {
        return Decision {
            result: LiveResult::Unchanged,
            approved: Some(current.clone()),
        };
    }
    Decision {
        result: LiveResult::On,
        approved: entry(manifest, folder, scope),
    }
}

/// What the watcher stores so the record follows `manifest`'s `live` block:
/// the block to keep (`None`: live data off) and the request to hold for a
/// person (`None`: no ask).
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub struct Follow {
    pub approved: Option<LiveApproved>,
    pub request: Option<state::LiveRequest>,
    /// The record's `visibility_declined` after this pass.
    pub visibility_declined: bool,
}

/// The watcher's half (SW-DP-19, SV-PBF-286): what `record` must become so it
/// follows `manifest`'s current `live` block, or `None` when it already does.
/// The watcher is not a person: a block that is new or wider than the
/// confirmed one is held as a request and the confirmed block stays as it is
/// (and so serves nothing while the manifest differs). A narrower block or a
/// removed one is applied at once. The watcher has no context index, so the
/// scope is the saved one when the folder is unchanged; with none saved,
/// `queryContext` stays refused until `publish_dashboard` works it out.
pub fn follow(record: &PublishRecord, manifest: &Manifest) -> Option<Follow> {
    let base = follow_block(record, manifest);
    let approved = base
        .as_ref()
        .map_or(record.live_approved.as_ref(), |f| f.approved.as_ref());
    let request = base
        .as_ref()
        .map_or(record.live_request.as_ref(), |f| f.request.as_ref());
    // A file-only widening of who sees a dashboard that streams terminals is
    // held for a person; sends keep the confirmed audience (`sent_visibility`).
    // A declined audience stays declined while the file keeps asking for it;
    // once the file says otherwise, asking again later is a new request.
    let declined = record.visibility_declined
        && manifest.visibility == super::manifest::Visibility::Team;
    let widened = manifest.slug == record.slug
        && manifest.visibility == super::manifest::Visibility::Team
        && streams_on(approved)
        && record.live_visibility.as_deref() != Some("team")
        && !declined
        && !request.is_some_and(|r| r.visibility.as_deref() == Some("team"));
    if !widened {
        return match base {
            Some(f) => Some(Follow {
                visibility_declined: declined,
                ..f
            }),
            None if declined != record.visibility_declined => Some(Follow {
                approved: approved.cloned(),
                request: request.cloned(),
                visibility_declined: declined,
            }),
            None => None,
        };
    }
    let mut next = request.cloned().unwrap_or_else(|| state::LiveRequest {
        block: None,
        visibility: None,
        share_add: Vec::new(),
        by: "watcher".into(),
        requested_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        declined: false,
    });
    next.visibility = Some(super::manifest::Visibility::Team.as_str().into());
    Some(Follow {
        approved: approved.cloned(),
        request: Some(next),
        visibility_declined: false,
    })
}

/// True when `approved` is a confirmed block that streams terminals.
fn streams_on(approved: Option<&LiveApproved>) -> bool {
    approved.is_some_and(|a| a.confirmed && !a.streams.is_empty())
}

/// The audience to send to the host (H2): `wanted`, except that a dashboard
/// streaming terminals never goes wider than a person confirmed. The wider
/// value waits as a [`state::LiveRequest`] (`follow`, `management::publish_for`).
pub fn sent_visibility(record: &PublishRecord, wanted: &str) -> String {
    let wider = wanted == super::manifest::Visibility::Team.as_str()
        && record.live_visibility.as_deref() != Some(wanted);
    if wider && streams_on(record.live_approved.as_ref()) {
        super::manifest::Visibility::Private.as_str().into()
    } else {
        wanted.into()
    }
}

fn follow_block(record: &PublishRecord, manifest: &Manifest) -> Option<Follow> {
    if manifest.slug != record.slug {
        return None;
    }
    let folder = state::folder_key(Path::new(record.folder.as_deref()?));
    let current = record.live_approved.as_ref();
    let held = record.live_request.as_ref();
    let Some(sha) = manifest.live_sha() else {
        // A removed block turns live off and drops any ask for it; an
        // audience-only ask is left alone.
        let drop_block = held.is_some_and(|r| r.block.is_some());
        return (current.is_some() || drop_block).then(|| Follow {
            approved: None,
            request: held.filter(|r| r.block.is_none()).cloned(),
            ..Follow::default()
        });
    };
    if current.is_some_and(|c| c.confirmed && c.sha == sha && same_folder(c, &folder)) {
        // The confirmed block is the manifest's: an ask for another block is stale.
        return held.is_some_and(|r| r.block.is_some()).then(|| Follow {
            approved: current.cloned(),
            request: held.filter(|r| r.block.is_none()).cloned(),
            ..Follow::default()
        });
    }
    // Already asked (or declined): nothing changes until the manifest does.
    if held.is_some_and(|r| r.block.as_ref().is_some_and(|b| b.sha == sha)) {
        return None;
    }
    let scope = current
        .filter(|c| same_folder(c, &folder))
        .and_then(|c| c.scope.clone());
    // Invalid selector edits retain the saved scope; the sha mismatch fails closed
    // until a valid edit can follow, just as a malformed manifest does.
    let wanted = entry(manifest, folder, scope)?;
    let gated = gate(Initiator::Agent, Some(wanted), current);
    let request = next_request(Initiator::Agent, held, gated.hold, None).map(|mut r| {
        if r.by == Initiator::Agent.label() {
            r.by = "watcher".into();
        }
        r
    });
    Some(Follow {
        approved: gated.applied,
        request,
        ..Follow::default()
    })
}

/// The held record that stops the watcher asking again after a person turned
/// live data off: the manifest's current block, already declined.
pub fn turned_off(record: &PublishRecord, manifest: &Manifest) -> Option<state::LiveRequest> {
    let folder = state::folder_key(Path::new(record.folder.as_deref()?));
    // Keep the saved team: with none, a manifest with pins has no entry.
    let scope = record
        .live_approved
        .as_ref()
        .filter(|a| same_folder(a, &folder))
        .and_then(|a| a.scope.clone());
    let mut block = entry(manifest, folder, scope)?;
    block.confirmed = false;
    Some(state::LiveRequest {
        block: Some(block),
        visibility: None,
        share_add: Vec::new(),
        by: Initiator::Human.label().into(),
        requested_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        declined: true,
    })
}

/// The queries that are on for `record`, only while the manifest's current
/// `live` block is the saved one, for this slug and this folder. Anything else
/// is `None` (fail closed).
pub fn approved_for<'a>(
    record: &'a PublishRecord,
    manifest: &Manifest,
) -> Option<&'a super::manifest::LiveQueries> {
    approved_block(record, manifest).map(|approved| &approved.queries)
}

/// The whole saved block (queries, fields and streams) under the same rule as
/// [`approved_for`]. Its streams equal the manifest's, because the sha matched.
pub fn approved_block<'a>(
    record: &'a PublishRecord,
    manifest: &Manifest,
) -> Option<&'a LiveApproved> {
    let approved = record.live_approved.as_ref().filter(|a| a.confirmed)?;
    let live_sha = manifest.live_sha()?;
    let folder = state::folder_key(Path::new(record.folder.as_deref()?));
    (approved.sha == live_sha && manifest.slug == record.slug && same_folder(approved, &folder))
        .then_some(approved)
}

/// True when the dashboard counts as live on this Mac (SW-DP-4 AC-9): its
/// record is published and its manifest's `live` block is the saved one.
pub fn is_live(record: &PublishRecord, manifest: &Manifest) -> bool {
    record.state == LocalState::Published && approved_for(record, manifest).is_some()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dashboard_publish::manifest::{LiveQueries, PublishState, Visibility};

    const DIR: &str = "/Users/me/dash";

    pub(crate) fn manifest(live: Option<LiveQueries>) -> Manifest {
        Manifest {
            slug: "ai-usage".into(),
            title: "AI usage".into(),
            state: PublishState::Published,
            visibility: Visibility::Private,
            retain: 5,
            files: vec!["index.html".into()],
            live,
            live_fields: Default::default(),
            live_args: Default::default(),
            live_streams: Vec::new(),
            live_control: false,
            live_embeds: Default::default(),
            actions: Default::default(),
            session_days: 90,
            install: None,
            chrome: Default::default(),
        }
    }

    fn list(names: &[&str]) -> LiveQueries {
        LiveQueries::list(names.iter().map(|n| n.to_string()))
    }

    /// SW-DP-19: `decide` turns a block on (SV-PBF-286: a person confirms it
    /// first, through `settle`); the same block again is unchanged; an edited block, another folder or no block each change it.
    #[test]
    fn a_block_turns_on_and_an_unchanged_one_stays() {
        let m = manifest(Some(list(&["runs"])));
        let d = decide(&m, Path::new(DIR), None, None);
        assert_eq!(d.result, LiveResult::On);
        let on = d.approved.clone().unwrap();
        assert_eq!(on.queries, list(&["runs"]));
        assert_eq!(Some(on.sha.clone()), m.live_sha());

        let again = decide(&m, Path::new(DIR), Some(&on), None);
        assert_eq!(
            again,
            Decision {
                result: LiveResult::Unchanged,
                approved: Some(on.clone())
            }
        );

        let edited = manifest(Some(list(&["runs", "sessionUsage"])));
        let d = decide(&edited, Path::new(DIR), Some(&on), None);
        assert_eq!(d.result, LiveResult::On);
        assert_eq!(d.approved.unwrap().queries, list(&["runs", "sessionUsage"]));

        let none = decide(&manifest(None), Path::new(DIR), Some(&on), None);
        assert_eq!(
            none,
            Decision {
                result: LiveResult::None,
                approved: None
            }
        );

        // SW-DP-4 review S3: the same slug and block from another folder is saved anew.
        let moved = decide(&m, Path::new("/elsewhere"), Some(&on), None);
        assert_eq!(moved.result, LiveResult::On);
        assert_eq!(moved.approved.unwrap().folder, "/elsewhere");
    }

    /// Security review: the queryContext scope is saved with the block. A
    /// scope found later, or another team, saves the block again with it.
    #[test]
    fn the_query_context_scope_is_saved_with_the_block() {
        let m = manifest(Some(LiveQueries::All));
        let blind = decide(&m, Path::new(DIR), None, None).approved.unwrap();
        assert_eq!(blind.scope, None);

        let known = decide(&m, Path::new(DIR), Some(&blind), Some(("acme", "emea")));
        assert_eq!(known.result, LiveResult::On);
        let known = known.approved.unwrap();
        assert_eq!(
            known.scope,
            Some(Scope {
                org: "acme".into(),
                team: "emea".into()
            })
        );

        let same = decide(&m, Path::new(DIR), Some(&known), Some(("acme", "emea")));
        assert_eq!(same.result, LiveResult::Unchanged);
        let other = decide(&m, Path::new(DIR), Some(&known), Some(("acme", "emea-sub")));
        assert_eq!(other.result, LiveResult::On);

        // A re-publish that cannot work out the team keeps the saved one.
        let unknown = decide(&m, Path::new(DIR), Some(&known), None);
        assert_eq!(
            unknown,
            Decision {
                result: LiveResult::Unchanged,
                approved: Some(known.clone())
            }
        );
        let edited = manifest(Some(list(&["queryContext"])));
        let d = decide(&edited, Path::new(DIR), Some(&known), None);
        assert_eq!(d.result, LiveResult::On);
        assert_eq!(d.approved.unwrap().scope, known.scope);
        // With no team saved either, queryContext stays refused.
        let still_blind = decide(&m, Path::new(DIR), Some(&blind), None);
        assert_eq!(still_blind.result, LiveResult::Unchanged);
        assert_eq!(still_blind.approved.unwrap().scope, None);
        // Not for another folder: the team belongs to the folder.
        let moved = decide(&m, Path::new("/elsewhere"), Some(&known), None);
        assert_eq!(moved.result, LiveResult::On);
        assert_eq!(moved.approved.unwrap().scope, None);
    }

    fn published_record(live: Option<LiveApproved>) -> PublishRecord {
        let mut record = PublishRecord::new("ai-usage");
        record.state = LocalState::Published;
        record.folder = Some(DIR.into());
        record.live_approved = live;
        record
    }

    /// What a person's confirmation stores for the ask `follow` holds.
    fn confirmed_ask(record: &PublishRecord) -> LiveApproved {
        let mut block = record
            .live_request
            .as_ref()
            .and_then(|r| r.block.clone())
            .expect("an ask is held");
        block.confirmed = true;
        block
    }

    /// The record after one watcher pass over `manifest`.
    fn after_follow(record: &PublishRecord, manifest: &Manifest) -> Option<PublishRecord> {
        let next = follow(record, manifest)?;
        let mut after = record.clone();
        after.live_approved = next.approved;
        after.live_request = next.request;
        after.visibility_declined = next.visibility_declined;
        Some(after)
    }

    /// SW-DP-19, SV-PBF-286: the watcher's `follow` holds a changed pin as an
    /// ask (the confirmed block stays), keeps the saved team, and a person's
    /// confirmation then stores the new pins with that team.
    #[test]
    fn entry_and_follow_copy_pins_and_keep_scope() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "ok").unwrap();
        let load = |ids: &str| {
            super::super::manifest::parse_with(&format!(
            "slug: ai-usage\ntitle: Pins\npublish: {{state: published, visibility: private, files: [index.html]}}\nlive: {{queries: [storyChanges], args: {{storyChanges: {{storyId: {ids}}}}}}}\n"
        ), dir.path(), super::super::catalog::CATALOG).unwrap()
        };
        let first = load("[SV-CDE-46]");
        let saved = decide(&first, dir.path(), None, Some(("acme", "emea")))
            .approved
            .unwrap();
        assert_eq!(
            serde_json::to_value(&saved).unwrap()["args"]["storyChanges"]["storyId"],
            serde_json::json!(["SV-CDE-46"])
        );
        let mut record = published_record(Some(saved.clone()));
        record.folder = Some(state::folder_key(dir.path()));
        let edited = load("[SV-CDE-45]");
        let held = after_follow(&record, &edited).unwrap();
        assert_eq!(held.live_approved, Some(saved.clone()), "a pin change waits");
        assert!(!held.live_request.as_ref().unwrap().declined);
        let next = confirmed_ask(&held);
        assert_eq!(
            serde_json::to_value(&next).unwrap()["args"]["storyChanges"]["storyId"],
            serde_json::json!(["SV-CDE-45"])
        );
        assert_eq!(next.scope, saved.scope);
        let invalid = load("[{team: family}]");
        assert_eq!(
            follow(&record, &invalid),
            None,
            "an invalid edit must keep the saved scope"
        );
        let corrected = load("[{team: emea}]");
        let corrected = confirmed_ask(&after_follow(&record, &corrected).unwrap());
        assert_eq!(corrected.scope, saved.scope);
        assert_eq!(
            serde_json::to_value(corrected).unwrap()["args"]["storyChanges"]["storyId"],
            serde_json::json!([{"team":"emea"}])
        );
    }

    #[test]
    fn follow_holds_a_wider_block_and_applies_a_removed_one() {
        let block = list(&["queryContext"]);
        let mut saved = decide(
            &manifest(Some(block.clone())),
            Path::new(DIR),
            None,
            Some(("acme", "emea")),
        )
        .approved
        .unwrap();
        let record = published_record(Some(saved.clone()));
        assert_eq!(follow(&record, &manifest(Some(block))), None);

        let mut edited = manifest(Some(list(&["queryContext", "runs"])));
        edited.live_fields = [("queryContext".to_string(), vec!["priority".to_string()])]
            .into_iter()
            .collect();
        let held = after_follow(&record, &edited).expect("an edited block is followed");
        assert_eq!(held.live_approved, Some(saved.clone()));
        let ask = held.live_request.clone().unwrap();
        assert_eq!(ask.by, "watcher");
        let next = confirmed_ask(&held);
        assert_eq!(next.queries, list(&["queryContext", "runs"]));
        assert_eq!(next.fields, edited.live_fields);
        assert_eq!(next.scope, saved.scope, "the team was dropped");
        assert_eq!(Some(next.sha.clone()), edited.live_sha());
        // The same ask is not made again, and a declined one stays declined.
        assert_eq!(follow(&held, &edited), None);
        let mut declined = held.clone();
        declined.live_request.as_mut().unwrap().declined = true;
        assert_eq!(follow(&declined, &edited), None);

        assert_eq!(
            follow(&record, &manifest(None)),
            Some(Follow {
                approved: None,
                request: None,
                visibility_declined: false,
            })
        );
        assert_eq!(follow(&published_record(None), &manifest(None)), None);

        // Saved for another folder: held, with no team carried over.
        saved.folder = "/elsewhere".into();
        let moved = after_follow(&published_record(Some(saved)), &edited).unwrap();
        let next = confirmed_ask(&moved);
        assert_eq!((next.scope, next.folder.as_str()), (None, DIR));

        // A manifest under another slug is never followed.
        let mut renamed = edited.clone();
        renamed.slug = "other".into();
        assert_eq!(follow(&record, &renamed), None);
    }

    fn confirmed_block(block: &LiveQueries) -> LiveApproved {
        LiveApproved {
            scope: None,
            sha: block.sha(),
            queries: block.clone(),
            folder: DIR.into(),
            fields: Default::default(),
            args: Default::default(),
            streams: Vec::new(),
            control: false,
            embeds: Default::default(),
            actions: Default::default(),
            approved_at: "2026-10-17T10:00:00Z".into(),
            confirmed: true,
        }
    }

    /// SW-DP-4 AC-9, SV-PBF-286: editing the block makes the dashboard not
    /// live until the record follows it. A narrower or removed block is
    /// followed at once; a wider one waits for a person.
    #[test]
    fn an_edited_block_is_not_live_until_followed() {
        let block = list(&["runs"]);
        let record = published_record(Some(confirmed_block(&block)));
        assert!(is_live(&record, &manifest(Some(block.clone()))));
        assert_eq!(
            approved_for(&record, &manifest(Some(block.clone()))),
            Some(&block)
        );

        for (edited, live_after_watcher) in [
            (Some(list(&["runs", "sessionUsage"])), false),
            (Some(LiveQueries::All), false),
            (Some(list(&[])), true),
            (None, false),
        ] {
            let m = manifest(edited.clone());
            assert!(!is_live(&record, &m), "{edited:?} stayed live");
            assert_eq!(approved_for(&record, &m), None);
            let followed = after_follow(&record, &m).unwrap();
            assert_eq!(is_live(&followed, &m), live_after_watcher, "{edited:?}");
            // A person's confirmation turns any block on.
            if edited.is_some() && !live_after_watcher {
                let mut confirmed = followed.clone();
                confirmed.live_approved = Some(confirmed_ask(&followed));
                confirmed.live_request = None;
                assert!(is_live(&confirmed, &m), "{edited:?} not live once confirmed");
            }
        }

        // The record now names another folder (the slug reused elsewhere): not live.
        let mut moved = record.clone();
        moved.folder = Some("/elsewhere".into());
        assert!(!is_live(&moved, &manifest(Some(block.clone()))));

        // Not published on this Mac: not live, whatever is saved.
        let mut draft = record.clone();
        draft.state = LocalState::Unpublished;
        assert!(!is_live(&draft, &manifest(Some(block))));
    }

    /// SV-PBF-286 AC3: a record saved before confirmation existed loads as
    /// unconfirmed, serves nothing, and the watcher asks the person.
    #[test]
    fn a_legacy_unconfirmed_block_serves_nothing_until_a_person_confirms() {
        let block = list(&["runs"]);
        let mut legacy = confirmed_block(&block);
        legacy.confirmed = false;
        let m = manifest(Some(block.clone()));
        let record = published_record(Some(legacy.clone()));
        assert!(!is_live(&record, &m));
        assert_eq!(approved_for(&record, &m), None);

        let asked = after_follow(&record, &m).expect("the watcher asks");
        assert!(!is_live(&asked, &m));
        assert_eq!(asked.live_request.as_ref().unwrap().by, "watcher");
        assert_eq!(after_follow(&asked, &m), None, "asked once");

        // A person publishing it confirms it.
        let gated = gate(Initiator::Human, decide(&m, Path::new(DIR), Some(&legacy), None).approved, Some(&legacy));
        let mut on = asked.clone();
        on.live_approved = gated.applied;
        on.live_request = None;
        assert!(is_live(&on, &m));
    }

    /// SV-PBF-286 rows 8, 10, 11: an agent's first `live` block is held; a
    /// widening one is held while the confirmed block stays; a narrowing or
    /// equal one is applied; a person's is always applied confirmed.
    #[test]
    fn only_a_person_turns_live_on_or_widens_it() {
        let m = manifest(Some(list(&["runs"])));
        let wanted = decide(&m, Path::new(DIR), None, None).approved;

        let first = gate(Initiator::Agent, wanted.clone(), None);
        assert_eq!(first.applied, None);
        assert!(!first.hold.as_ref().unwrap().confirmed);

        let person = gate(Initiator::Human, wanted.clone(), None);
        assert!(person.applied.as_ref().unwrap().confirmed);
        assert_eq!(person.hold, None);
        let current = person.applied.unwrap();

        let same = gate(Initiator::Agent, wanted.clone(), Some(&current));
        assert!(same.applied.as_ref().unwrap().confirmed);
        assert_eq!(same.hold, None);

        let mut with_terminal = wanted.clone().unwrap();
        with_terminal.streams = vec!["terminal".into()];
        let widened = gate(Initiator::Agent, Some(with_terminal.clone()), Some(&current));
        assert_eq!(widened.applied, Some(current.clone()));
        assert_eq!(widened.hold.unwrap().streams, ["terminal"]);

        let mut with_control = wanted.clone().unwrap();
        with_control.control = true;
        assert!(gate(Initiator::Agent, Some(with_control.clone()), Some(&current)).hold.is_some());
        assert!(widens(&with_control, &current));

        let mut other_folder = wanted.clone().unwrap();
        other_folder.folder = "/elsewhere".into();
        assert!(widens(&other_folder, &current));

        // Removing terminal or control is applied at once.
        let mut wide = current.clone();
        wide.streams = vec!["terminal".into()];
        wide.control = true;
        let narrowed = gate(Initiator::Agent, wanted.clone(), Some(&wide));
        assert_eq!(narrowed.hold, None);
        assert!(narrowed.applied.unwrap().streams.is_empty());
        assert!(!widens(&current, &wide));

        // A removed block turns live off for anyone.
        assert_eq!(gate(Initiator::Agent, None, Some(&current)), Gate { applied: None, hold: None });
    }

    /// SV-PBF-286 row 14: a declined ask stays declined for the same block and
    /// is a new ask for another.
    #[test]
    fn a_declined_ask_is_not_made_again_for_the_same_block() {
        let m = manifest(Some(list(&["runs"])));
        let hold = gate(Initiator::Agent, decide(&m, Path::new(DIR), None, None).approved, None)
            .hold
            .unwrap();
        let mut ask = next_request(Initiator::Agent, None, Some(hold.clone()), None).unwrap();
        ask.declined = true;
        let again = next_request(Initiator::Agent, Some(&ask), Some(hold.clone()), None).unwrap();
        assert!(again.declined);
        let mut other = hold;
        other.sha = "other".into();
        assert!(!next_request(Initiator::Agent, Some(&ask), Some(other), None).unwrap().declined);
        // No block and no audience ask: nothing is held.
        assert_eq!(next_request(Initiator::Agent, None, None, None), None);
        assert_eq!(
            next_request(Initiator::Agent, None, None, Some("team")).unwrap().visibility.as_deref(),
            Some("team")
        );
    }

    /// A manifest that streams a terminal, with its confirmed block for `record`.
    fn streaming(visibility: Visibility) -> (Manifest, LiveApproved) {
        let mut m = manifest(Some(list(&["runs"])));
        m.live_streams = vec!["terminal".into()];
        m.visibility = visibility;
        let mut block = confirmed_block(&list(&["runs"]));
        block.streams = vec!["terminal".into()];
        block.sha = m.live_sha().unwrap();
        (m, block)
    }

    fn settle_for(
        record: &PublishRecord,
        m: &Manifest,
        visibility: Visibility,
        initiator: Initiator,
        bound: Option<&Bound>,
    ) -> Result<Settled, String> {
        settle(record, m, Path::new(DIR), None, visibility, initiator, bound)
    }

    /// SV-PBF-286 H1, rows 8 and 13: an agent's block is held; a confirm
    /// applies exactly the request the person saw and refuses one that changed.
    #[test]
    fn a_confirm_applies_only_the_request_the_person_saw() {
        let m = manifest(Some(list(&["runs"])));
        let mut record = published_record(None);
        let asked = settle_for(&record, &m, Visibility::Private, Initiator::Agent, None).unwrap();
        assert_eq!(asked.live_result, LiveResult::PendingConfirmation);
        assert!(asked.announce);
        assert_eq!(asked.gated.applied, None);
        record.live_request = asked.request.clone();
        let shown = Shown::of(record.live_request.as_ref().unwrap(), "private");
        let bound = Bound::Request(shown.clone());

        // The agent widens dashboard.yml while the dialog is open.
        let mut wider = m.clone();
        wider.live_streams = vec!["terminal".into()];
        wider.live_control = true;
        let r = settle_for(&record, &wider, Visibility::Private, Initiator::Human, Some(&bound));
        assert_eq!(r, Err(STALE.to_string()));

        // Viewers are added to the same request while it is open.
        let mut more = record.clone();
        more.live_request.as_mut().unwrap().share_add.push("x@y.co".into());
        let r = settle_for(&more, &m, Visibility::Private, Initiator::Human, Some(&bound));
        assert_eq!(r, Err(STALE.to_string()));

        // dashboard.yml's audience changes while it is open.
        let mut team = m.clone();
        team.visibility = Visibility::Team;
        let r = settle_for(&record, &team, Visibility::Private, Initiator::Human, Some(&bound));
        assert_eq!(r, Err(STALE.to_string()));

        // Nothing is waiting any more.
        let r = settle_for(&published_record(None), &m, Visibility::Private, Initiator::Human, Some(&bound));
        assert_eq!(r, Err(STALE.to_string()));

        // The same request: the held block is stored confirmed and the ask clears.
        let ok = settle_for(&record, &m, Visibility::Private, Initiator::Human, Some(&bound)).unwrap();
        let applied = ok.gated.applied.unwrap();
        assert!(applied.confirmed);
        assert_eq!(Some(applied.sha), m.live_sha());
        assert_eq!(ok.request, None);
        assert_eq!(ok.confirmed_visibility, Some(Visibility::Private));
    }

    /// SV-PBF-286 r5 M1: a UI publish that was not asked about a block is bound
    /// to the block it read. A manifest that gained one meanwhile is the agent's.
    #[test]
    fn a_ui_publish_with_no_hold_is_bound_to_the_block_it_read() {
        let none = manifest(None);
        let record = published_record(None);
        let human = Initiator::Human;

        // Nothing changed: still the person's, and no ask.
        let ok = settle_for(&record, &none, Visibility::Private, human, Some(&Bound::Block(None))).unwrap();
        assert!(ok.gated.hold.is_none() && ok.request.is_none());

        // An agent added a streaming block between the read and the lock.
        let mut grown = manifest(Some(list(&["runs"])));
        grown.live_streams = vec!["terminal".into()];
        grown.live_control = true;
        let r = settle_for(&record, &grown, Visibility::Private, human, Some(&Bound::Block(None))).unwrap();
        assert_eq!(r.gated.applied, None, "no dialog, so nothing is turned on");
        let hold = r.gated.hold.expect("the block waits for a person");
        assert!(!hold.confirmed);
        assert_eq!(r.live_result, LiveResult::PendingConfirmation);
        assert!(r.request.is_some_and(|q| q.waiting()));

        // The same block the person read is still theirs.
        let read = Bound::Block(grown.live_sha());
        let ok = settle_for(&record, &grown, Visibility::Private, human, Some(&read)).unwrap();
        assert!(ok.gated.applied.is_some_and(|a| a.confirmed));
    }

    /// SV-PBF-286 r5 L1: confirming only the audience keeps a block hold that a
    /// person declined or turned off.
    #[test]
    fn an_audience_only_confirm_keeps_a_declined_block_hold() {
        let m = manifest(Some(list(&["runs"])));
        let mut record = published_record(None);
        let mut declined = turned_off(&published_record(Some(confirmed_block(&list(&["runs"])))), &m)
            .expect("a hold");
        declined.visibility = Some("team".into());
        declined.share_add = vec!["a@b.co".into()];
        record.live_request = Some(declined);
        let shown = Shown::of(record.live_request.as_ref().unwrap(), "private");
        assert_eq!(shown.sha, None, "the declined block is not part of what was shown");

        let ok = settle_for(&record, &m, Visibility::Team, Initiator::Human, Some(&Bound::Request(shown))).unwrap();
        let kept = ok.request.expect("the declined block stays held");
        assert!(kept.declined && kept.block.is_some());
        assert!(kept.visibility.is_none() && kept.share_add.is_empty());
        assert!(!kept.waiting());
        assert_eq!(ok.gated.applied, None, "the block is not applied");
    }

    /// SV-PBF-286 H1: a confirm of an audience-only ask leaves the block as it
    /// is, even when dashboard.yml has a wider one the watcher has not followed.
    #[test]
    fn an_audience_only_confirm_does_not_apply_an_unfollowed_block() {
        let confirmed = confirmed_block(&list(&["runs"]));
        let mut record = published_record(Some(confirmed.clone()));
        record.live_request = Some(state::LiveRequest {
            block: None,
            visibility: Some("team".into()),
            share_add: Vec::new(),
            by: "agent".into(),
            requested_at: String::new(),
            declined: false,
        });
        let wide = manifest(Some(LiveQueries::All));
        let shown = Shown::of(record.live_request.as_ref().unwrap(), "private");
        let ok = settle_for(&record, &wide, Visibility::Team, Initiator::Human, Some(&Bound::Request(shown))).unwrap();
        assert_eq!(ok.gated.applied, Some(confirmed));
        assert_eq!(ok.confirmed_visibility, Some(Visibility::Team));
    }

    /// SV-PBF-286 H2, B6, row 15: a dashboard that streams terminals is never
    /// sent wider than a person confirmed, however dashboard.yml got edited.
    #[test]
    fn a_file_edit_cannot_widen_a_streaming_dashboard() {
        let (private, block) = streaming(Visibility::Private);
        let mut record = published_record(Some(block));
        record.live_visibility = Some("private".into());
        let mut edited = private.clone();
        edited.visibility = Visibility::Team;

        assert_eq!(sent_visibility(&record, "team"), "private");
        assert_eq!(sent_visibility(&record, "private"), "private");

        // The watcher holds the difference as an ask and keeps the block on.
        let held = after_follow(&record, &edited).expect("the edit is held");
        assert!(held.live_approved.as_ref().unwrap().confirmed);
        assert_eq!(held.live_request.as_ref().unwrap().visibility.as_deref(), Some("team"));
        assert_eq!(sent_visibility(&held, "team"), "private");
        assert_eq!(follow(&held, &edited), None, "asked once");

        // The agent edits the file, then publishes `team`: nothing is widened.
        let s = settle_for(&record, &edited, Visibility::Team, Initiator::Agent, None).unwrap();
        assert_eq!(s.visibility, Visibility::Private);
        assert_eq!(s.asked_visibility, Some("team"));
        assert_eq!(s.request.unwrap().visibility.as_deref(), Some("team"));

        // A person confirming the audience is what makes it team.
        record.live_visibility = Some("team".into());
        assert_eq!(sent_visibility(&record, "team"), "team");
        assert_eq!(follow(&record, &edited), None);

        // With no terminals, an agent may publish team.
        let plain = published_record(Some(confirmed_block(&list(&["runs"]))));
        assert_eq!(sent_visibility(&plain, "team"), "team");
    }

    /// SV-PBF-286 M2: a removed pin, a removed fields list, or an emptied pin
    /// is wider; the same or a tighter one is not.
    #[test]
    fn a_removed_pin_or_field_list_is_wider() {
        let mut old = confirmed_block(&list(&["storyChanges", "queryContext"]));
        old.args = serde_json::from_value(serde_json::json!({"storyChanges": {"storyId": ["SV-CDE-46"]}})).unwrap();
        old.fields = [("queryContext".to_string(), vec!["status".to_string()])]
            .into_iter()
            .collect();
        assert!(!widens(&old, &old));

        let mut no_pin = old.clone();
        no_pin.args.clear();
        assert!(widens(&no_pin, &old));
        let mut empty_pin = old.clone();
        empty_pin.args = serde_json::from_value(serde_json::json!({"storyChanges": {"storyId": []}})).unwrap();
        assert!(widens(&empty_pin, &old));
        let mut other_pin = old.clone();
        other_pin.args = serde_json::from_value(serde_json::json!({"storyChanges": {"storyId": ["SV-CDE-47"]}})).unwrap();
        assert!(widens(&other_pin, &old));

        let mut default_fields = old.clone();
        default_fields.fields.clear();
        assert!(widens(&default_fields, &old), "the default list is wider than [status]");
        let mut more_fields = old.clone();
        more_fields.fields = [("queryContext".to_string(), vec!["status".to_string(), "owner".to_string()])]
            .into_iter()
            .collect();
        assert!(widens(&more_fields, &old));

        // Tighter is not wider: a pin added where there was none, a shorter list.
        let mut loose = old.clone();
        loose.args.clear();
        assert!(!widens(&old, &loose));
        let mut wide_fields = old.clone();
        wide_fields.fields.clear();
        assert!(!widens(&old, &wide_fields));
    }

    /// SV-PBF-286 L3: turning off keeps the saved team, so a block with pins
    /// still has its declined hold and the watcher does not ask again.
    #[test]
    fn turned_off_keeps_the_saved_team_for_a_pinned_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "ok").unwrap();
        let m = super::super::manifest::parse_with(
            "slug: ai-usage\ntitle: Pins\npublish: {state: published, visibility: private, files: [index.html]}\nlive: {queries: [storyChanges], args: {storyChanges: {storyId: [{team: emea}]}}}\n",
            dir.path(),
            super::super::catalog::CATALOG,
        )
        .unwrap();
        let saved = decide(&m, dir.path(), None, Some(("acme", "emea"))).approved.unwrap();
        let mut record = published_record(Some(saved));
        record.folder = Some(state::folder_key(dir.path()));
        let held = turned_off(&record, &m).expect("a pinned block still has its hold");
        assert!(held.declined && held.block.is_some());
        assert_eq!(follow(&{
            let mut r = record.clone();
            r.live_approved = None;
            r.live_request = Some(held);
            r
        }, &m), None);
    }
}
