// SPDX-License-Identifier: MIT
//! Cent-stable spend values share the existing agentList projection.
use crate::session_usage::spend_caps::{Budget, BudgetUsd, Evaluation};
use serde_json::{json, Value};

pub(crate) fn rows(value: &Evaluation) -> Vec<Value> {
    [("agent", &value.agents), ("routine", &value.routines)].into_iter().flat_map(|(kind, scopes)| {
        scopes.iter().map(move |(key, status)| {
            let team = crate::context_repo::scoped_key::ScopedKey::parse(key).map(|k| format!("{}:{}", k.org, k.team)).unwrap_or_default();
            json!({"id":format!("spend:{kind}:{key}"), "rowType":"spend", "scopeKind":kind, "key":key,
                "teamKey":team, "spend": {
                    "today": (status.spend.today * 100.0).round() / 100.0,
                    "week": (status.spend.week * 100.0).round() / 100.0,
                    "cap": (status.effective != Budget::default()).then(|| BudgetUsd::from(status.effective)),
                    "state":status.spend.state, "pending":status.budget_pending,
                    "requestedCap":status.budget_usd, "pendingAt":status.pending_at, "unpricedModels":status.unpriced_models, "pendingRates":status.pending_rates
                }})
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_hub::projection::TopicRegistry;
    use crate::session_usage::{
        event::Tokens,
        tests::{event, store},
    };
    #[test]
    fn agent_and_routine_spend_share_the_existing_team_scoped_snapshot() {
        let (_dir, mut usage) = store();
        let day = chrono::Local::now().date_naive();
        let mut value = Evaluation::default();
        let cap = Budget {
            per_day: Some(50.0),
            per_week: None,
        };
        for team in ["backend", "other"] {
            let key = format!("acme/api/{team}/worker");
            value.agents.insert(
                key.clone(),
                usage.cap_status("agent", &key, cap, day, false).unwrap(),
            );
            let key = format!("acme/api/{team}/daily");
            value.routines.insert(
                key.clone(),
                usage.cap_status("routine", &key, cap, day, false).unwrap(),
            );
        }
        let mut registry = TopicRegistry::default();
        super::super::views::replace(
            &mut registry,
            "team:acme/api/backend",
            &[],
            &json!({"spendRows":rows(&value)}),
        )
        .unwrap();
        let snapshot = registry
            .snapshot("agentList", "team:acme/api/backend")
            .unwrap()
            .snapshot;
        let spend: Vec<_> = snapshot
            .iter()
            .filter(|r| r["rowType"] == "spend")
            .collect();
        assert_eq!(spend.len(), 2);
        assert!(spend.iter().all(|r| r["teamKey"] == "acme/api:backend"));
        assert_eq!(spend[0]["spend"]["cap"]["perDay"], 50.0);
    }
    #[test]
    fn ingest_crossing_eighty_emits_once_and_unchanged_shown_values_emit_nothing() {
        let (_dir, mut usage) = store();
        let key = "org/team/worker";
        usage
            .attribute("s", "org", Some("team"), key, "worker", None)
            .unwrap();
        let mut registry = TopicRegistry::default();
        let mut value = Evaluation::default();
        let mut project = |usage: &mut crate::session_usage::UsageStore| {
            value.agents.insert(
                key.into(),
                usage
                    .cap_status(
                        "agent",
                        key,
                        Budget {
                            per_day: Some(100.0),
                            per_week: None,
                        },
                        chrono::Local::now().date_naive(),
                        false,
                    )
                    .unwrap(),
            );
            super::super::views::replace(
                &mut registry,
                "all",
                &[],
                &json!({"spendRows": rows(&value)}),
            )
            .unwrap()
        };
        let mut row = event(
            "one",
            "s",
            "claude-sonnet-4-6",
            chrono::Utc::now().timestamp_millis(),
            Tokens::default(),
        );
        row.reported_cost_usd = Some(79.99);
        usage.ingest(&[row.clone()]).unwrap();
        project(&mut usage);
        row.event_id = "two".into();
        row.reported_cost_usd = Some(0.01);
        usage.ingest(&[row.clone()]).unwrap();
        let deltas = project(&mut usage);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].upserts.len(), 1);
        assert_eq!(deltas[0].upserts[0]["spend"]["state"], "warn");
        row.event_id = "three".into();
        row.reported_cost_usd = Some(0.001);
        usage.ingest(&[row]).unwrap();
        assert!(project(&mut usage).is_empty());
    }
    #[test]
    fn pending_routine_rates_reach_the_shared_spend_projection() {
        let (_dir, mut usage) = store();
        let mut status = usage
            .cap_status(
                "routine",
                "org/team/routine",
                Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                chrono::Local::now().date_naive(),
                false,
            )
            .unwrap();
        status
            .pending_rates
            .push(crate::session_usage::SetModelRateParams {
                model_prefix: "unknown-model".into(),
                effective_from: "2026-10-22".into(),
                input_per_mtok: 1.25,
                output_per_mtok: 7.5,
                cache_read_per_mtok: Some(0.125),
                long: None,
            });
        let value = Evaluation {
            routines: [("org/team/routine".into(), status)].into(),
            ..Default::default()
        };
        let row = &rows(&value)[0];
        assert_eq!(row["scopeKind"], "routine");
        assert_eq!(row["spend"]["pendingRates"][0]["inputPerMtok"], 1.25);
    }
}
