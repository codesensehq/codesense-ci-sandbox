// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::{tests::Fixture, CallerScope, MISSING};

#[test]
fn absent_private_namespace_preserves_unbacked_noop_without_admitting_other_endpoints() {
    let f = Fixture::new();
    let private = crate::context_repo::scratch_space::org_space_at(
        &f.home,
        &crate::domain::Principal::from(1u64),
        "acme/ctx",
    )
    .unwrap();
    assert!(!private.root().exists());
    for org_slug in [None, Some("acme/ctx".into())] {
        let params = RemoveFromContextParams {
            context: ContextEndpoint::Personal { org_slug },
            logical_name: "absent.md".into(),
            user_id: 1,
        };
        let baseline =
            remove_from_context_for(&f.home, Some("acme/ctx"), &params, &CallerScope::Human);
        assert_eq!(baseline, Ok(RemoveFromContextOutcome::RejectedUnbacked));
        assert_eq!(
            remove_from_context_for(&f.home, Some("acme/ctx"), &params, &f.scope),
            baseline
        );
        assert!(
            !private.root().exists(),
            "remove must not create private space"
        );
    }
    for context in [
        ContextEndpoint::Personal { org_slug: None },
        serde_json::from_value(serde_json::json!({"kind":"teamRepo","orgSlug":"acme/ctx","teamName":"parent","codeRepoSlug":{"owner":"acme","repo":"absent","provider":"github"}})).unwrap(),
    ] {
        let params = RemoveFromContextParams { context, logical_name: "absent.md".into(), user_id: 1 };
        assert_eq!(remove_from_context_for(&f.home, None, &params, &f.scope), Err(MISSING.into()));
    }
    let shared = RemoveFromContextParams {
        context: ContextEndpoint::TeamAll {
            org_slug: "acme/ctx".into(),
            team_name: "parent".into(),
        },
        logical_name: "shared.md".into(),
        user_id: 1,
    };
    assert_eq!(
        remove_from_context_for(&f.home, None, &shared, &f.scope),
        Ok(RemoveFromContextOutcome::Removed)
    );
    for logical in ["secret.md", "absent.md"] {
        let params = RemoveFromContextParams {
            logical_name: logical.into(),
            ..shared.clone()
        };
        assert_eq!(
            remove_from_context_for(&f.home, None, &params, &f.scope),
            Err(MISSING.into())
        );
    }
    assert!(!private.root().exists());
}
