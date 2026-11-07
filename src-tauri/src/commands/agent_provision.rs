// SPDX-License-Identifier: MIT
//! SW-SN-7 — agent-agnostic provisioning helpers shared by codesense-managed agent
//! surfaces (config-additions, …). A born-minimized / headless
//! agent has no visible TUI to approve tool prompts at, so each surface must
//! pre-allow exactly the tools it uses in `.claude/settings.local.json`. This
//! module owns the generic allowlist writer; each surface supplies its own
//! allow/deny sets, reusing the same merge-never-clobber logic instead of
//! duplicating it.

use std::path::Path;

use serde_json::{Map, Value};

/// Union `additions` into an existing JSON string array, preserving prior entries
/// and order (existing first, then any new) — so a user who hand-adds allows keeps
/// them. Pure.
///
/// SW-CP-24 DRY: the permissions `allow`/`deny` arrays are a **scalar identity
/// set** — the exact "arrays with a key → order-independent set-union, dedup"
/// policy the merge engine applies (here the element *is* its own key). Delegates
/// to the shared [`crate::shared::json_io::union_array`] (key = `None`) so there
/// is ONE dedup implementation across this writer and the
/// merge-marked-content projection — the cross-merger fixed point CP-24 rests on.
pub(crate) fn merge_str_set(existing: Option<&Value>, additions: &[&str]) -> Vec<String> {
    let mut base: Vec<Value> = existing
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|v| v.is_string()).cloned().collect())
        .unwrap_or_default();
    let patch: Vec<Value> = additions
        .iter()
        .map(|s| Value::String((*s).to_string()))
        .collect();
    crate::shared::json_io::union_array(
        &mut base,
        &patch,
        None,
        &crate::shared::json_io::ReplaceAll,
        "",
    );
    base.into_iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Merge an agent permission block (`allow`/`deny`, and optionally a
/// `defaultMode`) into an existing `settings.local.json` body (preserving every
/// other user key) and return the canonical pretty JSON (trailing newline).
/// Merge-never-clobber + idempotent (feeding its own output back yields
/// byte-identical output).
///
/// SW-GS-29 (AC-1): `default_mode` is `Some("dontAsk")` for a HEADLESS surface
/// (git-sync) whose born-minimized session has no TUI to answer an approval
/// prompt at — `dontAsk` makes Claude Code AUTO-DENY any tool call outside
/// `allow` instead of parking on an unanswerable interactive prompt. It's set
/// authoritatively (this settings file is codesense-owned + git-excluded), but
/// only for surfaces that opt in; `None` leaves any user/other `defaultMode`
/// untouched (the notes surface, which is not deny-Bash-isolated, passes `None`).
pub(crate) fn merge_agent_permissions(
    existing: Option<&str>,
    allow: &[&str],
    deny: &[&str],
    default_mode: Option<&str>,
) -> String {
    let mut root: Map<String, Value> = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();

    let mut perms = root
        .get("permissions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    perms.insert(
        "allow".into(),
        serde_json::json!(merge_str_set(perms.get("allow"), allow)),
    );
    perms.insert(
        "deny".into(),
        serde_json::json!(merge_str_set(perms.get("deny"), deny)),
    );
    if let Some(mode) = default_mode {
        perms.insert("defaultMode".into(), Value::String(mode.to_string()));
    }
    root.insert("permissions".into(), Value::Object(perms));

    let mut out = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    out.push('\n');
    out
}

/// Install (or merge) an agent permission allowlist into
/// `repo_dir/.claude/settings.local.json`. Best-effort, merge-never-clobber,
/// idempotent (no write when already satisfied). Bails untouched on a symlink /
/// dir / non-object existing file. `git_exclude` adds the file to
/// `.git/info/exclude`; pass `false` for a synthetic dir OUTSIDE any clone (the
/// notes / epic dirs) — `git rev-parse --git-path` walks UP and would otherwise
/// pollute an unrelated ancestor repo's exclude file (SW-SN-7 review).
pub(crate) fn install_agent_permissions(
    repo_dir: &Path,
    allow: &[&str],
    deny: &[&str],
    git_exclude: bool,
    default_mode: Option<&str>,
) {
    if !repo_dir.is_dir() {
        return;
    }
    let rel = ".claude/settings.local.json";
    let path = repo_dir.join(rel);
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
        || path.is_dir()
    {
        return;
    }
    let raw = std::fs::read_to_string(&path).ok();
    // An empty / whitespace-only file is "touched but never written" → fresh write.
    // A NON-empty file that is not a JSON OBJECT is real content we must not destroy.
    let existing: Option<&str> = match raw.as_deref() {
        Some(body) if body.trim().is_empty() => None,
        Some(body) => {
            if !matches!(serde_json::from_str::<Value>(body), Ok(Value::Object(_))) {
                log::warn!(
                    target: "agent_provision",
                    "install_agent_permissions: existing {} is not a JSON object — leaving it untouched",
                    path.display()
                );
                return;
            }
            Some(body)
        }
        None => None,
    };
    let desired = merge_agent_permissions(existing, allow, deny, default_mode);
    let already = raw.as_deref() == Some(desired.as_str());
    if !already && crate::content_engine::write_tmp_fsync_rename(&path, desired.as_bytes()).is_err()
    {
        return; // best-effort: degrade silently
    }
    if git_exclude {
        let _ = crate::content_engine::git_exclude::add_entries(repo_dir, &[rel]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const ALLOW: &[&str] = &["mcp__codesense__post_agent_message", "Read", "Edit"];
    const DENY: &[&str] = &["Bash"];

    #[test]
    fn merges_allow_and_deny_and_is_idempotent() {
        let body = merge_agent_permissions(None, ALLOW, DENY, None);
        let v: Value = serde_json::from_str(&body).unwrap();
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|x| x == "Read"));
        assert!(allow
            .iter()
            .any(|x| x == "mcp__codesense__post_agent_message"));
        assert!(v["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "Bash"));
        // No default_mode passed → the key is absent (unrelated surfaces stay untouched).
        assert!(v["permissions"].get("defaultMode").is_none());
        // Idempotent: feeding output back is byte-identical.
        assert_eq!(
            merge_agent_permissions(Some(&body), ALLOW, DENY, None),
            body
        );
    }

    /// SW-GS-29 (AC-1): a headless surface opts into `defaultMode:"dontAsk"`; it's
    /// written, survives a re-merge byte-identically, and coexists with allow/deny.
    #[test]
    fn default_mode_dont_ask_is_written_and_idempotent() {
        let body = merge_agent_permissions(None, ALLOW, DENY, Some("dontAsk"));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["permissions"]["defaultMode"], "dontAsk");
        // Idempotent with the mode present.
        assert_eq!(
            merge_agent_permissions(Some(&body), ALLOW, DENY, Some("dontAsk")),
            body
        );
    }

    #[test]
    fn preserves_unrelated_user_keys_and_prior_allows() {
        let user = r#"{"model":"opus","permissions":{"allow":["WebSearch"]}}"#;
        let merged = merge_agent_permissions(Some(user), ALLOW, DENY, None);
        let v: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["model"], "opus");
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|x| x == "WebSearch"));
        assert!(allow.iter().any(|x| x == "Read"));
    }

    #[test]
    fn install_writes_creates_parent_and_is_idempotent() {
        let dir = tempdir().unwrap();
        install_agent_permissions(dir.path(), ALLOW, DENY, false, None);
        let p = dir.path().join(".claude/settings.local.json");
        assert!(p.exists());
        let first = std::fs::read_to_string(&p).unwrap();
        install_agent_permissions(dir.path(), ALLOW, DENY, false, None);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), first);
    }

    #[test]
    fn install_never_clobbers_a_non_object_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join(".claude/settings.local.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "[1,2,3]").unwrap();
        install_agent_permissions(dir.path(), ALLOW, DENY, false, None);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[1,2,3]");
    }
}
