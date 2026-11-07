// SPDX-License-Identifier: MIT
//! SLW-PT-17: the agent-facing content-mobility spine shared by the MCP tools.
//!
//! Three concerns, all PURE / off-agent so `keep agents out of core` holds — the
//! MCP layer (`mcp/handler.rs`) is a thin typed adapter over these:
//!
//!   1. [`parse_context_id`] — decode the SLW-PT-11 stable logical id string an
//!      agent passes (the SAME address the UI enumeration returns) into the
//!      [`ContextEndpoint`] the merged move/copy/remove spine already resolves.
//!      Mirrors the FE `parseContextId` encoding EXACTLY so an id round-trips:
//!      enumerate -> id -> move/copy/remove.
//!
//!   2. [`first_unmet_gate`] — the §P typed gate ladder, ONE gate at a time in the
//!      fixed precedence `cross_org -> privacy -> sole_copy -> collision`. Returns
//!      the FIRST unmet gate (or `None` when the op may proceed) so an agent builds
//!      a deterministic retry ladder. This is the Rust mirror of the FE
//!      `planMoveGates` (`move-gate-ladder.ts`); UI/MCP parity is unit-tested so
//!      the precedence can never silently drift between the two surfaces.
//!
//!   3. [`list_context_items`] — enumerate a context's content items, each with its
//!      stable logical id + human path, so the id it returns feeds straight back
//!      into move/copy/remove.
//!
//! No banned internal word (provider / overlay / layer / scratch / mint / unmint)
//! appears in any type, field, or reason string here — only Personal / Shared and
//! the stable id.

use std::path::Path;

use serde::Serialize;

use super::move_between_contexts::ContextEndpoint;
use crate::content_engine::is_safe_logical_name;
use crate::domain::SourceRepoSlug;

// ---------------------------------------------------------------------------
// 1. Stable logical id -> ContextEndpoint (the SLW-PT-11 address space).
//
// Encodings (mirrors `context-model.ts`, ":" separates fields; the repo slug
// keeps its own "/"):
//   personal:<orgSlug>               (that org's scratch space; the org is required)
//   all-orgs                         (the scratch folder of inert items)
//   team:<orgSlug>:<teamName>
//   team-repo:<orgSlug>:<teamName>:<owner>/<repo>
//   repo:<owner>/<repo>              (view-only; NOT a writable move endpoint)
// ---------------------------------------------------------------------------

/// Decode a stable logical id into the writable [`ContextEndpoint`] the merged
/// spine resolves. A view-only `repo:` id (never a move/copy/remove endpoint) and
/// any malformed id return `None`, so the tool surfaces a typed rejection rather
/// than resolving against the wrong place.
pub fn parse_context_id(id: &str) -> Option<ContextEndpoint> {
    if let Some(org) = id.strip_prefix("personal:") {
        if crate::context_repo::org_slug::validate_org_slug(org).is_some() {
            return None;
        }
        return Some(ContextEndpoint::Personal {
            org_slug: Some(org.to_string()),
        });
    }
    if id == "all-orgs" {
        return Some(ContextEndpoint::ScratchAllOrgs);
    }
    if let Some(rest) = id.strip_prefix("team-repo:") {
        let (org_slug, team_name, slug) = split_org_team_slug(rest)?;
        return Some(ContextEndpoint::TeamRepo {
            org_slug,
            team_name,
            code_repo_slug: slug,
        });
    }
    if let Some(rest) = id.strip_prefix("team:") {
        let idx = rest.find(':')?;
        let org_slug = rest[..idx].to_string();
        let team_name = rest[idx + 1..].to_string();
        if org_slug.is_empty() || team_name.is_empty() {
            return None;
        }
        return Some(ContextEndpoint::TeamAll {
            org_slug,
            team_name,
        });
    }
    // `repo:` (view-only) is intentionally NOT a writable endpoint; everything
    // else is malformed.
    None
}

/// The org a context id belongs to, for the cross-org gate. A personal id names
/// its org's scratch space, so it carries that org; all-orgs and a view-only
/// `repo:` id are org-agnostic.
pub fn context_org(id: &str) -> Option<String> {
    if let Some(org) = id.strip_prefix("personal:") {
        return (!org.is_empty()).then(|| org.to_string());
    }
    if let Some(rest) = id.strip_prefix("team-repo:") {
        let (org_slug, _, _) = split_org_team_slug(rest)?;
        return Some(org_slug);
    }
    if let Some(rest) = id.strip_prefix("team:") {
        let idx = rest.find(':')?;
        let org = &rest[..idx];
        return (!org.is_empty()).then(|| org.to_string());
    }
    None
}

/// Whether a context id denotes Personal.
pub fn is_personal_context(id: &str) -> bool {
    id.starts_with("personal:") || id == "all-orgs"
}

/// Split `<orgSlug>:<teamName>:<owner>/<repo>` — orgSlug is the first field, the
/// slug is the final "/"-joined pair, teamName is everything between (so a team
/// name may itself contain colons). Mirrors the FE `splitOrgTeamSlug`.
fn split_org_team_slug(rest: &str) -> Option<(String, String, SourceRepoSlug)> {
    let first_colon = rest.find(':')?;
    let org_slug = rest[..first_colon].to_string();
    let after_org = &rest[first_colon + 1..];
    let last_colon = after_org.rfind(':')?;
    let team_name = after_org[..last_colon].to_string();
    let slug = parse_slug(&after_org[last_colon + 1..])?;
    if org_slug.is_empty() || team_name.is_empty() {
        return None;
    }
    Some((org_slug, team_name, slug))
}

fn parse_slug(raw: &str) -> Option<SourceRepoSlug> {
    let (owner, repo) = raw.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    SourceRepoSlug::new(owner, repo).ok()
}

// ---------------------------------------------------------------------------
// 2. The §P typed gate ladder — one gate at a time, fixed precedence.
// ---------------------------------------------------------------------------

/// The operation a mobility gate ladder is planning. A COPY retains the source,
/// so it SKIPS the sole-copy gate; every other gate + its precedence is shared
/// with MOVE (mirrors the FE `ContextMobilityOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobilityOp {
    Move,
    Copy,
    Remove,
}

/// The typed FIRST-unmet gate an agent must satisfy before the op proceeds — in
/// §P precedence order. Each names its resolution so the retry ladder is
/// deterministic. Serialized as a tagged `reason` with a snake_case discriminant;
/// no banned internal word appears in any variant or field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum GateReason {
    /// §P gate 1 — the two contexts live in different orgs. Resolve by switching
    /// into a team of that org (`set_active_team`) or passing an explicit `org` scope, then
    /// retry. Names the required org so the agent knows which to switch into.
    CrossOrgRejected {
        /// The org the target lives in — switch into this to proceed.
        required_org: String,
        /// The resolution tool to call first. Never a silent no-op.
        resolve_with: &'static str,
    },
    /// §P gate 2 — moving a Personal item into a Shared (team) context makes it
    /// visible to everyone on that team. Resolve by re-calling with
    /// `confirm_make_shared: true`.
    PrivacyEscalationRequired {
        /// The confirmation flag to set true to proceed.
        confirm_with: &'static str,
    },
    /// §P gate 3 — moving/removing a scratch item: the scratch space is the
    /// user's own local folder and CodeSense cannot know whether they keep a
    /// copy elsewhere, so the only copy may go. Resolve by re-calling with
    /// `allow_sole_copy_move: true` (an explicit, acknowledged risk).
    SoleCopy {
        /// The override flag to set true to proceed.
        allow_with: &'static str,
    },
    /// §P gate 4 (LAST) — the target already holds an item at this logical id.
    /// Resolve by choosing a collision `policy` (`keep_both` | `replace` |
    /// `keep_mine`).
    CollisionPolicyRequired {
        /// The parameter to set with the chosen policy.
        choose_with: &'static str,
    },
}

/// The facts the ladder needs beyond the two context ids. `to` is `None` for a
/// REMOVE (there is no target — only gates 1/2/4 that involve a target are
/// skipped, and the sole-copy gate still applies to a Personal source).
#[derive(Debug, Clone)]
pub struct GateFacts<'a> {
    pub op: MobilityOp,
    /// The source context's stable logical id.
    pub from: &'a str,
    /// The target context's stable logical id — `None` for a REMOVE.
    pub to: Option<&'a str>,
    /// Whether an item already exists at the target under this logical id.
    pub target_collision: bool,
    /// The caller has confirmed making a Personal item Shared (gate 2 satisfied).
    pub confirm_make_shared: bool,
    /// The caller has acknowledged the sole-copy discard risk (gate 3 satisfied).
    pub allow_sole_copy: bool,
    /// A collision policy was supplied (gate 4 satisfied).
    pub policy_supplied: bool,
}

/// Decide the FIRST unmet gate in the §P precedence order
/// `cross_org -> privacy -> sole_copy -> collision`, or `None` when the op may
/// proceed. Deterministic and pure: exactly one gate is returned at a time, never
/// two, so an agent resolves them one call at a time. Mirrors the FE
/// `planMoveGates` ordering (UI/MCP parity).
pub fn first_unmet_gate(facts: &GateFacts<'_>) -> Option<GateReason> {
    let source_personal = is_personal_context(facts.from);

    // 1. cross_org — evaluated first; a target in a different org stops the ladder.
    //    The one exception is "Copy to another org": a COPY from one org's scratch
    //    space into another's names both orgs explicitly and leaves the original
    //    where it was, so no org switch is needed.
    if let Some(to) = facts.to {
        let scratch_to_scratch_copy = facts.op == MobilityOp::Copy
            && facts.from.starts_with("personal:")
            && to.starts_with("personal:");
        if let (Some(a), Some(b), false) = (
            context_org(facts.from),
            context_org(to),
            scratch_to_scratch_copy,
        ) {
            if a != b {
                return Some(GateReason::CrossOrgRejected {
                    required_org: b,
                    resolve_with: "set_active_team",
                });
            }
        }
    }

    // 2. privacy — Personal -> Shared (a target present + shared) only, unless
    //    already confirmed. Shared -> Personal (MORE private) needs no gate.
    if let Some(to) = facts.to {
        let target_shared = !is_personal_context(to);
        if source_personal && target_shared && !facts.confirm_make_shared {
            return Some(GateReason::PrivacyEscalationRequired {
                confirm_with: "confirm_make_shared",
            });
        }
    }

    // 3. sole_copy — a REMOVE of a scratch item, which may delete its only copy
    //    (§J). A MOVE or COPY lands the item somewhere (a team, all-orgs, another
    //    org's scratch space), so nothing is lost and it never gates here; an
    //    explicit `allow_sole_copy` satisfies it.
    if facts.op == MobilityOp::Remove && source_personal && !facts.allow_sole_copy {
        return Some(GateReason::SoleCopy {
            allow_with: "allow_sole_copy_move",
        });
    }

    // 4. collision — LAST (§P step 4). A REMOVE has no target, so it never
    //    collides.
    if facts.to.is_some() && facts.target_collision && !facts.policy_supplied {
        return Some(GateReason::CollisionPolicyRequired {
            choose_with: "policy",
        });
    }

    None
}

/// The collision-resolution policy an agent supplies to satisfy gate 4. Mirrors
/// the UI's Keep-both / Replace / Cancel chooser (§P step 4) as a typed param, so
/// the MCP and UI resolve a collision the same way. Deserialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// Land the moved/copied item under a fresh `-N` name; both survive.
    KeepBoth,
    /// Overwrite the colliding target item.
    Replace,
    /// Keep the existing target item; do not move/copy over it (a no-op reject the
    /// caller reads as "left as-is").
    KeepMine,
}

/// Compute a Keep-both target logical name by inserting a `-N` suffix before the
/// extension, incrementing until it doesn't collide. `exists` reports whether a
/// candidate already lives at the target. Mirrors the FE `keepBothName` so the
/// chosen name matches the UI's never-silent-"notes-2" rule and can be surfaced.
pub fn keep_both_name(logical_name: &str, exists: impl Fn(&str) -> bool) -> String {
    let (dir, base) = match logical_name.rfind('/') {
        Some(slash) => (&logical_name[..=slash], &logical_name[slash + 1..]),
        None => ("", logical_name),
    };
    // A leading dot (dotfile) is not an extension separator — match the FE's
    // `dot <= 0` guard.
    let (stem, ext) = match base.rfind('.') {
        Some(dot) if dot > 0 => (&base[..dot], &base[dot..]),
        _ => (base, ""),
    };
    let mut n = 2;
    loop {
        let candidate = format!("{dir}{stem}-{n}{ext}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// How a resolved collision policy maps onto the merged move/copy spine's knobs.
/// Pure so the mapping (and its keep-both rename) is unit-tested through a real
/// seam, off the `AppHandle`-bound tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollisionKnobs {
    /// Proceed against a fresh, non-colliding target name (Keep-both). The name
    /// is surfaced to the caller as `renamedTo`.
    RenameTo(String),
    /// Overwrite the colliding target (Replace).
    Force,
    /// Leave the target as-is — nothing moves/copies (Keep-mine). The caller
    /// short-circuits to a "left as-is" reject without touching disk.
    LeaveTargetAsIs,
    /// No collision (or the collision gate was not reached) — a plain proceed.
    Proceed,
}

/// Resolve the spine knobs for a move/copy given whether the target collides and
/// the caller's chosen policy. Only meaningful once the collision gate is
/// satisfied (a policy was supplied); with no collision it is always `Proceed`.
/// `exists` reports target occupancy for the Keep-both rename search.
pub fn resolve_collision_knobs(
    logical_name: &str,
    target_collision: bool,
    policy: Option<CollisionPolicy>,
    exists: impl Fn(&str) -> bool,
) -> CollisionKnobs {
    if !target_collision {
        return CollisionKnobs::Proceed;
    }
    match policy {
        Some(CollisionPolicy::KeepBoth) => {
            CollisionKnobs::RenameTo(keep_both_name(logical_name, exists))
        }
        Some(CollisionPolicy::Replace) => CollisionKnobs::Force,
        Some(CollisionPolicy::KeepMine) => CollisionKnobs::LeaveTargetAsIs,
        // The gate ladder guarantees a policy is present past the collision gate;
        // defensively proceed as a plain (reject-on-collision) move otherwise.
        None => CollisionKnobs::Proceed,
    }
}

// ---------------------------------------------------------------------------
// 3. Enumerate a context's content items with their stable logical id + path.
// ---------------------------------------------------------------------------

/// One enumerated content item — its stable logical id (the SAME address the
/// move/copy/remove tools accept) and its human, content-root-relative path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItem {
    /// The content-root-relative logical name — the id that round-trips into
    /// move/copy/remove as their `logical_name`.
    pub logical_name: String,
    /// The human, `/`-separated path of the item within the context.
    pub path: String,
}

/// Enumerate every content item under a resolved context root, sorted by path so
/// the listing is stable across calls (the id round-trips into move/copy/remove).
/// A missing/unbacked root yields an empty list rather than an error — an empty
/// context is a valid, non-error answer. Skips dotfiles the sync engine hides
/// (`.git`, lockfiles) the same way the resolver enumeration does.
pub fn list_context_items(content_root: &Path) -> std::io::Result<Vec<ContextItem>> {
    list_context_items_for(content_root, &Default::default())
}

pub(crate) fn list_context_items_for(
    content_root: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> std::io::Result<Vec<ContextItem>> {
    let mut items = Vec::new();
    if !scope.visible(content_root) || !content_root.exists() {
        return Ok(items);
    }
    collect_items(content_root, content_root, &mut items, scope)?;
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

fn collect_items(
    root: &Path,
    dir: &Path,
    out: &mut Vec<ContextItem>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip VCS/internal dot-entries so the listing shows only content items.
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !scope.visible(&path) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_items(root, &path, out, scope)?;
        } else if file_type.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                let logical = rel.to_string_lossy().replace('\\', "/");
                // Defensive: only surface items addressable by the move/copy/remove
                // spine's own safe-name guard, so every returned id round-trips.
                if is_safe_logical_name(&logical) {
                    out.push(ContextItem {
                        logical_name: logical.clone(),
                        path: logical,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ---- 1. ContextId parse: round-trips with the FE encoding ----

    #[test]
    fn parses_personal_id_only_with_its_org() {
        assert!(matches!(
            parse_context_id("personal:acme/ctx"),
            Some(ContextEndpoint::Personal { org_slug: Some(org) }) if org == "acme/ctx"
        ));
        assert!(
            parse_context_id("personal").is_none(),
            "a personal target never silently uses the active org"
        );
        assert!(parse_context_id("personal:../x").is_none());
        assert!(matches!(
            parse_context_id("all-orgs"),
            Some(ContextEndpoint::ScratchAllOrgs)
        ));
    }

    #[test]
    fn a_personal_id_into_another_orgs_team_is_cross_org() {
        let facts = base_facts(
            MobilityOp::Copy,
            "personal:acme/a",
            Some("team:acme/b:default"),
        );
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::CrossOrgRejected { required_org, .. }) if required_org == "acme/b"
        ));
    }

    #[test]
    fn parses_team_all_id() {
        let ep = parse_context_id("team:acme:default").unwrap();
        match ep {
            ContextEndpoint::TeamAll {
                org_slug,
                team_name,
            } => {
                assert_eq!(org_slug, "acme");
                assert_eq!(team_name, "default");
            }
            _ => panic!("expected TeamAll"),
        }
    }

    #[test]
    fn parses_team_repo_id_slug_keeps_its_slash() {
        let ep = parse_context_id("team-repo:acme:default:owner/repo").unwrap();
        match ep {
            ContextEndpoint::TeamRepo {
                org_slug,
                team_name,
                code_repo_slug,
            } => {
                assert_eq!(org_slug, "acme");
                assert_eq!(team_name, "default");
                assert_eq!(code_repo_slug.owner, "owner");
                assert_eq!(code_repo_slug.repo, "repo");
            }
            _ => panic!("expected TeamRepo"),
        }
    }

    #[test]
    fn team_name_may_contain_colons() {
        // orgSlug is the FIRST field, the slug the LAST "/"-pair; the team name is
        // everything between and may itself hold colons.
        let ep = parse_context_id("team-repo:acme:a:b:c:owner/repo").unwrap();
        match ep {
            ContextEndpoint::TeamRepo {
                org_slug,
                team_name,
                ..
            } => {
                assert_eq!(org_slug, "acme");
                assert_eq!(team_name, "a:b:c");
            }
            _ => panic!("expected TeamRepo"),
        }
    }

    #[test]
    fn view_only_and_malformed_ids_are_not_writable_endpoints() {
        assert!(parse_context_id("repo:owner/repo").is_none());
        assert!(parse_context_id("team:acme").is_none());
        assert!(parse_context_id("team:").is_none());
        assert!(parse_context_id("garbage").is_none());
    }

    #[test]
    fn context_org_is_none_for_all_orgs_and_view_only() {
        assert_eq!(context_org("all-orgs"), None);
        assert_eq!(
            context_org("personal:acme/ctx"),
            Some("acme/ctx".to_string())
        );
        assert_eq!(context_org("repo:owner/repo"), None);
        assert_eq!(context_org("team:acme:default"), Some("acme".to_string()));
        assert_eq!(
            context_org("team-repo:acme:default:owner/repo"),
            Some("acme".to_string())
        );
    }

    // ---- 2. gate ladder: first-unmet-gate, fixed §P order, one at a time ----

    fn base_facts<'a>(op: MobilityOp, from: &'a str, to: Option<&'a str>) -> GateFacts<'a> {
        GateFacts {
            op,
            from,
            to,
            target_collision: false,
            confirm_make_shared: false,
            allow_sole_copy: false,
            policy_supplied: false,
        }
    }

    #[test]
    fn cross_org_is_the_first_gate_and_names_the_org_and_resolver() {
        // A Personal->team pair is never cross-org (Personal is org-agnostic), so
        // use two concrete team orgs.
        let facts = base_facts(
            MobilityOp::Move,
            "team:acme:default",
            Some("team:other:default"),
        );
        match first_unmet_gate(&facts) {
            Some(GateReason::CrossOrgRejected {
                required_org,
                resolve_with,
            }) => {
                assert_eq!(required_org, "other");
                assert_eq!(resolve_with, "set_active_team");
            }
            other => panic!("expected cross_org first, got {other:?}"),
        }
    }

    #[test]
    fn cross_org_precedes_every_other_gate() {
        // Even with a collision, cross-org wins.
        let mut facts = base_facts(
            MobilityOp::Move,
            "team:acme:default",
            Some("team:other:default"),
        );
        facts.target_collision = true;
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::CrossOrgRejected { .. })
        ));
    }

    #[test]
    fn privacy_is_gate_two_for_personal_into_shared() {
        let facts = base_facts(MobilityOp::Copy, "personal:acme", Some("team:acme:default"));
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::PrivacyEscalationRequired { .. })
        ));
    }

    #[test]
    fn shared_into_personal_has_no_privacy_gate() {
        // MORE private needs no gate; a clean/shared source has no sole-copy risk.
        let facts = base_facts(MobilityOp::Move, "team:acme:default", Some("personal:acme"));
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn sharing_with_a_team_never_gates_on_sole_copy() {
        // Scratch->team MOVE ("Share with ‹Team›"): privacy, then straight through —
        // the item lands in the team, so nothing is lost.
        let mut facts = base_facts(MobilityOp::Move, "personal:acme", Some("team:acme:default"));
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::PrivacyEscalationRequired { .. })
        ));
        facts.confirm_make_shared = true;
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn copy_skips_the_sole_copy_gate() {
        // A COPY retains the source, so it NEVER gates on sole-copy — even from an
        // scratch source (privacy already confirmed).
        let mut facts = base_facts(MobilityOp::Copy, "personal:acme", Some("team:acme:default"));
        facts.confirm_make_shared = true;
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn collision_is_the_last_gate() {
        // Clean shared->shared move with a collision: only the collision gate
        // remains.
        let mut facts = base_facts(MobilityOp::Move, "team:acme:alpha", Some("team:acme:beta"));
        facts.target_collision = true;
        match first_unmet_gate(&facts) {
            Some(GateReason::CollisionPolicyRequired { choose_with }) => {
                assert_eq!(choose_with, "policy");
            }
            other => panic!("expected collision last, got {other:?}"),
        }
        // Supplying a policy clears the ladder to success.
        facts.policy_supplied = true;
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn full_move_ladder_advances_one_gate_per_resolution_to_success() {
        // Scratch->shared MOVE, colliding: the agent walks the ladder one
        // resolution at a time in §P order and reaches success.
        let mut facts = base_facts(MobilityOp::Move, "personal:acme", Some("team:acme:default"));
        facts.target_collision = true;

        // gate 2: privacy.
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::PrivacyEscalationRequired { .. })
        ));
        facts.confirm_make_shared = true;
        // gate 4: collision (a move has no sole-copy gate).
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::CollisionPolicyRequired { .. })
        ));
        facts.policy_supplied = true;
        // success.
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn remove_from_personal_gates_on_sole_copy_only() {
        // A REMOVE has no target: no cross-org / privacy / collision — but a
        // scratch source still gates on sole-copy (§F/§J).
        let mut facts = base_facts(MobilityOp::Remove, "personal:acme", None);
        assert!(matches!(
            first_unmet_gate(&facts),
            Some(GateReason::SoleCopy { .. })
        ));
        // An acknowledged removal proceeds.
        facts.allow_sole_copy = true;
        assert_eq!(first_unmet_gate(&facts), None);
    }

    /// Walk the ladder the way an agent does — one gate, resolve it, call again —
    /// and name each gate the way the UI's planner does.
    fn walk_ladder(op: MobilityOp, from: &str, to: Option<&str>, collision: bool) -> Vec<String> {
        let mut facts = base_facts(op, from, to);
        facts.target_collision = collision;
        let mut steps = Vec::new();
        while let Some(gate) = first_unmet_gate(&facts) {
            match gate {
                GateReason::CrossOrgRejected { .. } => {
                    steps.push("crossOrg".to_string());
                    break;
                }
                GateReason::PrivacyEscalationRequired { .. } => {
                    steps.push("privacy".to_string());
                    facts.confirm_make_shared = true;
                }
                GateReason::SoleCopy { .. } => {
                    steps.push("soleCopy".to_string());
                    facts.allow_sole_copy = true;
                }
                GateReason::CollisionPolicyRequired { .. } => {
                    steps.push("collision".to_string());
                    facts.policy_supplied = true;
                }
            }
        }
        steps
    }

    #[test]
    fn the_ladder_matches_the_ui_case_for_case() {
        // The same table `move-gate-ladder.test.ts` checks the UI planner against.
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../../src/shared/lib/mobility-gate-cases.json"
        ))
        .unwrap();
        let cases = table["cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let op = match case["op"].as_str().unwrap() {
                "move" => MobilityOp::Move,
                "copy" => MobilityOp::Copy,
                "remove" => MobilityOp::Remove,
                other => panic!("unknown op {other}"),
            };
            let expected: Vec<String> = case["steps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect();
            let got = walk_ladder(
                op,
                case["from"].as_str().unwrap(),
                case["to"].as_str(),
                case["collision"].as_bool().unwrap(),
            );
            assert_eq!(got, expected, "case: {}", case["name"]);
        }
    }

    #[test]
    fn remove_from_shared_has_no_gate() {
        let facts = base_facts(MobilityOp::Remove, "team:acme:default", None);
        assert_eq!(first_unmet_gate(&facts), None);
    }

    #[test]
    fn no_banned_internal_word_in_any_gate_reason() {
        // Serialize every gate variant and assert none leaks an internal word.
        let banned = ["overlay", "provider", "layer", "scratch", "mint", "unmint"];
        let reasons = [
            GateReason::CrossOrgRejected {
                required_org: "acme".into(),
                resolve_with: "set_active_team",
            },
            GateReason::PrivacyEscalationRequired {
                confirm_with: "confirm_make_shared",
            },
            GateReason::SoleCopy {
                allow_with: "allow_sole_copy_move",
            },
            GateReason::CollisionPolicyRequired {
                choose_with: "policy",
            },
        ];
        for r in reasons {
            let s = serde_json::to_string(&r).unwrap().to_lowercase();
            for w in banned {
                assert!(!s.contains(w), "gate reason {s} leaks banned word {w}");
            }
        }
    }

    // ---- 3. list_context_items: stable ids that round-trip ----

    #[test]
    fn lists_items_sorted_with_round_trippable_ids() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/c.md"), "c").unwrap();
        // A hidden VCS dir must not surface as content.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref").unwrap();

        let items = list_context_items(root).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.logical_name.as_str()).collect();
        assert_eq!(names, vec!["a.md", "b.md", "sub/c.md"]);
        // Every id is a safe logical name (round-trips into move/copy/remove).
        for item in &items {
            assert!(is_safe_logical_name(&item.logical_name));
            assert_eq!(item.logical_name, item.path);
        }
    }

    #[test]
    fn keep_both_name_inserts_incrementing_suffix() {
        assert_eq!(
            keep_both_name("notes.md", |c| c == "notes.md"),
            "notes-2.md"
        );
        let taken = ["notes-2.md", "notes-3.md"];
        assert_eq!(
            keep_both_name("notes.md", |c| taken.contains(&c)),
            "notes-4.md"
        );
        assert_eq!(
            keep_both_name("a/b/skill", |c| c == "a/b/skill-2"),
            "a/b/skill-3"
        );
        assert_eq!(keep_both_name("a/b/skill", |_| false), "a/b/skill-2");
        // A leading dot is not an extension separator.
        assert_eq!(keep_both_name(".gitignore", |_| false), ".gitignore-2");
    }

    #[test]
    fn collision_knobs_map_each_policy() {
        // No collision ⇒ plain proceed regardless of policy.
        assert_eq!(
            resolve_collision_knobs("n.md", false, Some(CollisionPolicy::Replace), |_| true),
            CollisionKnobs::Proceed
        );
        // Replace ⇒ force overwrite.
        assert_eq!(
            resolve_collision_knobs("n.md", true, Some(CollisionPolicy::Replace), |_| true),
            CollisionKnobs::Force
        );
        // Keep-mine ⇒ leave target as-is.
        assert_eq!(
            resolve_collision_knobs("n.md", true, Some(CollisionPolicy::KeepMine), |_| true),
            CollisionKnobs::LeaveTargetAsIs
        );
        // Keep-both ⇒ a fresh non-colliding name from the live target listing.
        let taken = ["n.md", "n-2.md"];
        assert_eq!(
            resolve_collision_knobs("n.md", true, Some(CollisionPolicy::KeepBoth), |c| taken
                .contains(&c)),
            CollisionKnobs::RenameTo("n-3.md".to_string())
        );
    }

    #[test]
    fn absent_context_root_lists_empty_not_error() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("never-created");
        assert_eq!(list_context_items(&missing).unwrap(), Vec::new());
    }

    // ---- Round-trip through the REAL move spine: list → parse id → move.
    //      Proves the id an agent gets back from enumeration feeds straight into
    //      the merged move/copy/remove commands the MCP tools wrap. ----

    #[test]
    fn listed_id_round_trips_into_the_real_move_spine() {
        use crate::commands::move_between_contexts::{
            move_between_contexts_impl, resolve_endpoint_root, MoveBetweenContextsParams,
        };

        let tmp = tempdir().unwrap();
        let home = tmp.path();
        const UID: u64 = 42;
        const ORG: &str = "acme/repo";
        crate::context_repo::scratch_space::ensure_org_space(
            home,
            &crate::domain::Principal::from(UID),
            ORG,
        )
        .unwrap();

        // Seed a Personal item, then ENUMERATE it exactly as list_context_content does.
        let personal_ep = parse_context_id(&format!("personal:{ORG}")).unwrap();
        let personal_root = resolve_endpoint_root(home, UID, Some(ORG), &personal_ep).unwrap();
        std::fs::write(personal_root.join("note.md"), "hi").unwrap();
        let items = list_context_items(&personal_root).unwrap();
        let id = &items
            .iter()
            .find(|i| i.path == "note.md")
            .unwrap()
            .logical_name;

        // Feed the enumerated id straight into the move spine (Personal → team).
        let team_id = "team:acme/repo:default";
        let out = move_between_contexts_impl(
            home,
            Some(ORG),
            &MoveBetweenContextsParams {
                from: personal_ep,
                to: parse_context_id(team_id).unwrap(),
                logical_name: id.clone(),
                to_logical_name: None,
                user_id: UID,
                force: false,
                retain_source: false,
            },
        )
        .unwrap();
        assert_eq!(
            out,
            crate::commands::move_between_contexts::MoveBetweenContextsOutcome::Moved
        );

        // The item now enumerates in the TARGET context under the same id, and is
        // gone from the source — the full list→move→list round-trip closes.
        let team_root =
            resolve_endpoint_root(home, UID, Some(ORG), &parse_context_id(team_id).unwrap())
                .unwrap();
        let moved = list_context_items(&team_root).unwrap();
        assert!(moved.iter().any(|i| &i.logical_name == id));
        assert!(list_context_items(&personal_root).unwrap().is_empty());
    }
}
