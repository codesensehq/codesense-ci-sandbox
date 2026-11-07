// SPDX-License-Identifier: MIT
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::raw_yaml::RawTeamsYml;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileEntry {
    pub provider: String,
    pub repos: Vec<String>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    pub gaps: Vec<ReconcileEntry>,
    pub coming_soon: Vec<ReconcileEntry>,
    pub unsupported: Vec<ReconcileEntry>,
}

impl ReconcileReport {
    pub fn is_clear(&self) -> bool {
        self.gaps.is_empty() && self.coming_soon.is_empty() && self.unsupported.is_empty()
    }
}

pub fn reconcile(
    repos_by_provider: &BTreeMap<String, BTreeSet<String>>,
    connected: &BTreeSet<String>,
    supported: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> ReconcileReport {
    let mut report = ReconcileReport {
        gaps: Vec::new(),
        coming_soon: Vec::new(),
        unsupported: Vec::new(),
    };
    for (provider, repos) in repos_by_provider {
        const MAX_REPOS_PER_PROVIDER: usize = 500;
        let entry = ReconcileEntry {
            provider: provider.clone(),
            repos: repos.iter().take(MAX_REPOS_PER_PROVIDER).cloned().collect(),
            omitted: repos.len().saturating_sub(MAX_REPOS_PER_PROVIDER),
        };
        if supported.contains(provider) {
            if !connected.contains(provider) {
                report.gaps.push(entry);
            }
        } else if known.contains(provider) {
            report.coming_soon.push(entry);
        } else {
            report.unsupported.push(entry);
        }
    }
    report
}

pub fn repos_by_provider_from_raw(raw: &RawTeamsYml) -> BTreeMap<String, BTreeSet<String>> {
    let mut repos = BTreeMap::<String, BTreeSet<String>>::new();
    for source in raw
        .teams
        .iter()
        .filter(|team| !team.archived)
        .flat_map(|team| &team.source_repos)
    {
        repos
            .entry(source.provider_id())
            .or_default()
            .insert(format!("{}/{}", source.owner.trim(), source.repo.trim()));
    }
    repos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_teams_yml;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn attributes_sorted_repos_and_partitions_all_three_states() {
        let raw = parse_teams_yml(b"teams:\n  - name: alpha\n    source_repos:\n      - owner: acme\n        repo: zeta\n        provider: GitLab\n      - owner: acme\n        repo: alpha\n        provider: ' gitlab '\n      - owner: acme\n        repo: core\n      - owner: acme\n        repo: old\n        provider: svn\n").unwrap();
        let repos = repos_by_provider_from_raw(&raw);
        let report = reconcile(
            &repos,
            &BTreeSet::new(),
            &set(&["github"]),
            &set(&["github", "gitlab"]),
        );

        assert_eq!(
            report.gaps[0],
            ReconcileEntry {
                provider: "github".into(),
                repos: vec!["acme/core".into()],
                omitted: 0,
            }
        );
        assert_eq!(
            report.coming_soon[0],
            ReconcileEntry {
                provider: "gitlab".into(),
                repos: vec!["acme/alpha".into(), "acme/zeta".into()],
                omitted: 0,
            }
        );
        assert_eq!(report.unsupported[0].provider, "svn");
    }

    #[test]
    fn connected_provider_is_omitted_and_archived_teams_are_ignored() {
        let raw = parse_teams_yml(b"teams:\n  - name: live\n    source_repos:\n      - owner: acme\n        repo: core\n  - name: retired\n    archived: true\n    source_repos:\n      - owner: acme\n        repo: old\n        provider: svn\n").unwrap();
        let repos = repos_by_provider_from_raw(&raw);
        assert!(reconcile(
            &repos,
            &set(&["github"]),
            &set(&["github"]),
            &set(&["github"])
        )
        .is_clear());
    }

    #[test]
    fn duplicate_repo_rows_are_deduplicated() {
        let raw = parse_teams_yml(b"teams:\n  - name: alpha\n    source_repos:\n      - owner: acme\n        repo: core\n      - owner: acme\n        repo: core\n").unwrap();
        assert_eq!(
            repos_by_provider_from_raw(&raw)["github"],
            set(&["acme/core"])
        );
    }

    #[test]
    fn connected_state_only_satisfies_supported_providers() {
        let raw = parse_teams_yml(b"teams:\n  - name: alpha\n    source_repos:\n      - owner: acme\n        repo: future\n        provider: gitlab\n      - owner: acme\n        repo: legacy\n        provider: svn\n").unwrap();
        let report = reconcile(
            &repos_by_provider_from_raw(&raw),
            &set(&["gitlab", "svn"]),
            &set(&["github"]),
            &set(&["github", "gitlab"]),
        );
        assert_eq!(report.coming_soon[0].provider, "gitlab");
        assert_eq!(report.unsupported[0].provider, "svn");
    }
}
