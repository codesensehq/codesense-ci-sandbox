// SPDX-License-Identifier: MIT
use crate::domain::CredentialRef;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AttachmentWire", into = "AttachmentWire")]
pub struct Attachment {
    pub repo_path: String,
    pub remote_url: Option<String>,
    pub attached_at: Option<DateTime<Utc>>,
    pub tracking_prompt_dismissed_at: Option<DateTime<Utc>>,
    /// SW-FG-3: the connection this source repo is bound to, as a
    /// provider-neutral `credential_ref (provider, account)`. `None` = unbound
    /// (legacy record or never set) → callers fall back to the active
    /// connection. Resolves against FG-2's `list_accounts(provider)` with no
    /// cast at the resolver boundary. All serde lives on `AttachmentWire`.
    pub credential_ref: Option<CredentialRef>,
    /// SW-MC-1: ownership marker. Absent/`false` ⇒ user-attached (hands-off,
    /// today's behavior — codesense reads but never pulls/removes the dir).
    /// `true` ⇒ codesense cloned this dir and may pull/GC it (SW-MC-3 scopes
    /// every lifecycle action by it).
    pub managed: bool,
}

/// Serde bridge for [`Attachment`] on BOTH directions (`from` + `into`), so the
/// wire shape is owned in one place.
///
/// **Load (`from`):** a record written before SW-FG-3 carries
/// `github_account_id: <u64>` (SW-OB-11's stable GitHub key) instead of
/// `credential_ref`; when `credential_ref` is absent, the legacy `u64` lifts to
/// `CredentialRef::github(u64)` — GitHub is the only provider that ever wrote it.
/// This load-migration STAYS (reaped by SW-FG-12), hence the field is still
/// deserialized.
///
/// **Save (`into`):** SW-FG-8 re-keyed the FE off `github_account_id` onto
/// `credential_ref`, so FG-3's FE back-compat mirror is dead and REMOVED — the
/// wire now emits `credential_ref` as the single source (no stale mirrored id
/// beside it). The `github_account_id` field is therefore `skip_serializing`:
/// read on load (legacy migration), never written.
#[derive(Serialize, Deserialize)]
struct AttachmentWire {
    repo_path: String,
    #[serde(default)]
    remote_url: Option<String>,
    #[serde(default)]
    attached_at: Option<DateTime<Utc>>,
    #[serde(default)]
    tracking_prompt_dismissed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_ref: Option<CredentialRef>,
    /// SW-FG-3 load-migration ONLY (reaped by SW-FG-12): a legacy record's
    /// `github_account_id` lifts to `credential_ref` on load. Never serialized —
    /// SW-FG-8 removed the FE back-compat mirror.
    #[serde(default, skip_serializing)]
    github_account_id: Option<u64>,
    #[serde(default)]
    managed: bool,
}

impl From<AttachmentWire> for Attachment {
    fn from(w: AttachmentWire) -> Self {
        // `user_id == 0` is rejected at account add-time (SW-OB-10 R1), so a
        // legacy `github_account_id: 0` could only come from a corrupt/hand-edited
        // file; migrating it would persist a ref that can never resolve. Drop it
        // to unbound instead (safe fallback), matching the store's invariant.
        let credential_ref = w.credential_ref.or_else(|| {
            w.github_account_id
                .filter(|id| *id != 0)
                .map(CredentialRef::github)
        });
        Attachment {
            repo_path: w.repo_path,
            remote_url: w.remote_url,
            attached_at: w.attached_at,
            tracking_prompt_dismissed_at: w.tracking_prompt_dismissed_at,
            credential_ref,
            managed: w.managed,
        }
    }
}

impl From<Attachment> for AttachmentWire {
    fn from(a: Attachment) -> Self {
        AttachmentWire {
            repo_path: a.repo_path,
            remote_url: a.remote_url,
            attached_at: a.attached_at,
            tracking_prompt_dismissed_at: a.tracking_prompt_dismissed_at,
            credential_ref: a.credential_ref,
            // SW-FG-8: the FE back-compat mirror is removed — the wire is
            // `credential_ref`-only. `skip_serializing` keeps this unwritten.
            github_account_id: None,
            managed: a.managed,
        }
    }
}

/// Strict top-level wrapper, loose per-record (allows future fields).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachments {
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_all_fields_set() {
        let original = Attachments {
            attachments: vec![Attachment {
                repo_path: "/Users/dev/my-repo".into(),
                remote_url: Some("https://github.com/org/repo.git".into()),
                attached_at: Some("2025-01-15T10:30:00Z".parse().unwrap()),
                tracking_prompt_dismissed_at: Some("2025-01-16T08:00:00Z".parse().unwrap()),
                credential_ref: Some(CredentialRef::github(12_345)),
                managed: false,
            }],
        };

        let yaml = serde_yaml::to_string(&original).unwrap();
        let roundtripped: Attachments = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(roundtripped.attachments.len(), 1);
        assert_eq!(roundtripped.attachments[0], original.attachments[0]);
    }

    #[test]
    fn serde_roundtrip_all_optional_fields_none() {
        let original = Attachments {
            attachments: vec![Attachment {
                repo_path: "/Users/dev/bare".into(),
                remote_url: None,
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: false,
            }],
        };

        let yaml = serde_yaml::to_string(&original).unwrap();
        let roundtripped: Attachments = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(roundtripped.attachments[0].remote_url, None);
        assert_eq!(roundtripped.attachments[0].attached_at, None);
        assert_eq!(
            roundtripped.attachments[0].tracking_prompt_dismissed_at,
            None
        );
        assert_eq!(roundtripped.attachments[0].credential_ref, None);
    }

    // SW-FG-3 AC-1: a record with no binding loads as `None` (serde default) —
    // an unbound repo falls back to the active connection, as today.
    #[test]
    fn record_without_binding_loads_as_none() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/legacy
    remote_url: https://github.com/org/legacy.git
    attached_at: 2025-01-15T10:30:00Z
    tracking_prompt_dismissed_at: null
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].credential_ref, None);
    }

    // SW-FG-3 AC-1: a bound record round-trips the credential_ref.
    #[test]
    fn serde_roundtrip_preserves_credential_ref() {
        let original = Attachments {
            attachments: vec![Attachment {
                repo_path: "/Users/dev/bound".into(),
                remote_url: Some("https://github.com/org/bound.git".into()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: Some(CredentialRef::github(987_654_321)),
                managed: false,
            }],
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let roundtripped: Attachments = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            roundtripped.attachments[0].credential_ref,
            Some(CredentialRef::github(987_654_321))
        );
    }

    // SW-FG-3 AC-3: migrate-on-load — a legacy record written with
    // `github_account_id: <u64>` and no `credential_ref` lifts to
    // `Some(CredentialRef::github(<u64>))` (provider inferred GitHub).
    #[test]
    fn legacy_github_account_id_migrates_to_credential_ref() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/legacy-bound
    remote_url: https://github.com/org/legacy-bound.git
    github_account_id: 42
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(
            parsed.attachments[0].credential_ref,
            Some(CredentialRef::github(42))
        );
    }

    // SW-FG-8 (AC-8): the serialized wire is `credential_ref`-ONLY — FG-3's FE
    // back-compat `github_account_id` mirror is removed now the FE reads
    // `credential_ref`. Two sources on the wire would let a stale mirrored id
    // drift from the FE's binding.
    #[test]
    fn serialize_emits_only_credential_ref_no_mirror() {
        let att = Attachment {
            repo_path: "/Users/dev/bound".into(),
            remote_url: Some("https://github.com/org/bound.git".into()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: Some(CredentialRef::github(12_345)),
            managed: true,
        };
        let yaml = serde_yaml::to_string(&att).unwrap();
        assert!(
            yaml.contains("credential_ref:"),
            "credential_ref missing; yaml was:\n{yaml}"
        );
        assert!(
            !yaml.contains("github_account_id"),
            "the FE back-compat mirror must be gone; yaml was:\n{yaml}"
        );
    }

    // SW-FG-8 (AC-8): an unbound record serializes NEITHER binding field — no
    // `credential_ref` (None + skip-if-none) and no `github_account_id` mirror.
    #[test]
    fn serialize_unbound_emits_no_binding_fields() {
        let att = Attachment {
            repo_path: "/Users/dev/unbound".into(),
            remote_url: None,
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        };
        let yaml = serde_yaml::to_string(&att).unwrap();
        assert!(!yaml.contains("github_account_id"), "yaml was:\n{yaml}");
        assert!(!yaml.contains("credential_ref"), "yaml was:\n{yaml}");
    }

    // SW-FG-3: the FE read-modify-write round-trip preserves the binding. The FE
    // reads a serialized record (which exposes `github_account_id`), rebuilds a
    // record carrying ONLY `github_account_id` (it doesn't know `credential_ref`),
    // and re-saves — the binding must survive.
    #[test]
    fn fe_resave_via_github_account_id_only_preserves_binding() {
        // What the FE would send back: no credential_ref, just the mirror it read.
        let fe_payload = r#"
repo_path: /Users/dev/bound
remote_url: https://github.com/org/bound.git
github_account_id: 777
managed: true
"#;
        let parsed: Attachment = serde_yaml::from_str(fe_payload).unwrap();
        assert_eq!(
            parsed.credential_ref,
            Some(CredentialRef::github(777)),
            "a re-save carrying only github_account_id must keep the binding"
        );
    }

    // SW-FG-3: a legacy `github_account_id: 0` (only reachable via a corrupt file
    // — the store rejects id 0 at add-time) migrates to unbound, not a dead
    // `github(0)` ref that can never resolve.
    #[test]
    fn legacy_github_account_id_zero_migrates_to_unbound() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/ghost
    github_account_id: 0
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.attachments[0].credential_ref, None);
    }

    // SW-FG-3 AC-3: a legacy record with neither field loads unbound.
    #[test]
    fn legacy_record_without_binding_fields_loads_unbound() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/legacy-unbound
    remote_url: https://github.com/org/legacy-unbound.git
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.attachments[0].credential_ref, None);
    }

    // SW-FG-3 AC-3: an explicit `credential_ref` wins over any legacy
    // `github_account_id` present in the same record.
    #[test]
    fn credential_ref_wins_over_legacy_github_account_id() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/both
    credential_ref:
      provider: github
      account: "100"
    github_account_id: 42
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            parsed.attachments[0].credential_ref,
            Some(CredentialRef::github(100))
        );
    }

    // SW-MC-1 AC-2: a legacy record written before `managed` existed loads
    // as `false` (serde default) → user-attached, today's hands-off behavior.
    #[test]
    fn legacy_record_without_managed_loads_as_false() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/legacy
    remote_url: https://github.com/org/legacy.git
    attached_at: 2025-01-15T10:30:00Z
"#;
        let parsed: Attachments = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert!(
            !parsed.attachments[0].managed,
            "absent `managed` must default to false (user-attached)"
        );
        // NOTE: kept alongside the FG-3 migration tests above — this fixture has
        // neither binding field, so it also asserts unbound-by-default.
    }

    // SW-MC-1 AC-2: a codesense-owned clone round-trips `managed: true`.
    #[test]
    fn serde_roundtrip_preserves_managed_true() {
        let original = Attachments {
            attachments: vec![Attachment {
                repo_path: "/Users/uid/sources/org/repo".into(),
                remote_url: Some("https://github.com/org/repo.git".into()),
                attached_at: None,
                tracking_prompt_dismissed_at: None,
                credential_ref: None,
                managed: true,
            }],
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let roundtripped: Attachments = serde_yaml::from_str(&yaml).unwrap();
        assert!(roundtripped.attachments[0].managed);
    }

    #[test]
    fn deny_unknown_fields_rejects_unknown_top_level_key() {
        let yaml = "attachments: []\nextra_key: bad\n";
        let result: Result<Attachments, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "should reject unknown top-level fields");
    }

    #[test]
    fn allows_unknown_per_record_fields() {
        let yaml = r#"
attachments:
  - repo_path: /Users/dev/repo
    remote_url: null
    future_field: some_value
"#;
        let result: Result<Attachments, _> = serde_yaml::from_str(yaml);
        assert!(result.is_ok(), "should allow unknown fields on records");
        assert_eq!(result.unwrap().attachments[0].repo_path, "/Users/dev/repo");
    }
}
