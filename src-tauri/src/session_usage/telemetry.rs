// SPDX-License-Identifier: MIT
//! Maps OpenTelemetry log records, as the agent CLIs export them, into [`UsageEvent`]s. Each CLI's
//! mapper picks out its own per-request record and reads only the fields it needs. A record that
//! is not a usage record is skipped without error.

use serde_json::Value;

use codesense_pty_core::agent_cli::AgentCli;

use super::event::UsageEvent;

/// Point a launch at its engine-owned usage endpoint after all saved configuration.
/// CLI settings precede a prompt introduced by `--`.
pub(crate) fn configure_export(
    export: Option<&codesense_pty_core::agent_cli::TelemetryExport>,
    api_port: Option<u16>,
    session: &str,
    env: &mut Vec<(String, String)>,
    args: &mut Vec<String>,
) {
    let (Some(export), Some(port)) = (export, api_port) else {
        return;
    };
    let endpoint = crate::http::routes::telemetry::endpoint_for(port, session);
    let (export_env, export_args) = export.resolved(&endpoint);
    env.retain(|(key, _)| {
        !codesense_pty_core::spawn::telemetry_owned_env(key)
            && !export_env.iter().any(|(owned, _)| owned == key)
    });
    env.extend(export_env);
    for (key, value) in [
        ("OTEL_EXPORTER_OTLP_ENDPOINT".into(), endpoint.clone()),
        (
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT".into(),
            format!("{endpoint}/v1/logs"),
        ),
        ("OTEL_EXPORTER_OTLP_PROTOCOL".into(), "http/json".into()),
        (
            "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL".into(),
            "http/json".into(),
        ),
        ("OTEL_LOGS_EXPORTER".into(), "otlp".into()),
        ("OTEL_SDK_DISABLED".into(), "false".into()),
        ("DISABLE_TELEMETRY".into(), "0".into()),
        ("DO_NOT_TRACK".into(), "0".into()),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "0".into(),
        ),
    ] {
        env.retain(|(existing, _)| existing != &key);
        env.push((key, value));
    }
    let mut export_args = export_args;
    for (key, tokens) in &export.default_overrides {
        if !sets_config(args, key) {
            export_args.extend(tokens.iter().cloned());
        }
    }
    let end = codesense_pty_core::agent_cli::operands_at(args);
    args.splice(end..end, export_args);
}

/// Whether `args` already carry a `-c key=value` (or `--config key=value`) override for `key`.
fn sets_config(args: &[String], key: &str) -> bool {
    let prefix = format!("{key}=");
    args.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-c" | "--config") && pair[1].trim_start().starts_with(&prefix)
    }) || args.iter().any(|arg| {
        arg.strip_prefix("--config=")
            .is_some_and(|value| value.trim_start().starts_with(&prefix))
    })
}

/// The attributes of one OTLP log record, flattened to strings. OTLP writes every value under a
/// typed key (`stringValue`, `intValue`, ...), and some CLIs send numbers as strings.
pub(crate) struct Attrs(Vec<(String, String)>);

impl Attrs {
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// An id, where an empty value is no id: two empty ids would share one event key and overwrite
    /// each other.
    pub(crate) fn id(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|v| !v.is_empty())
    }

    pub(crate) fn count(&self, key: &str) -> u64 {
        self.get(key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .map_or(0, |n| (n.max(0.0) as u64).min(i64::MAX as u64))
    }

    pub(crate) fn real(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|v| v.parse::<f64>().ok())
    }
}

fn typed_value(v: &Value) -> Option<String> {
    v.get("stringValue")
        .or_else(|| v.get("intValue"))
        .or_else(|| v.get("doubleValue"))
        .or_else(|| v.get("boolValue"))
        .map(|x| match x {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
}

fn attrs_of(record: &Value) -> Attrs {
    let pairs = record
        .get("attributes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let key = a.get("key")?.as_str()?.to_string();
            Some((key, typed_value(a.get("value")?)?))
        })
        .collect();
    Attrs(pairs)
}

/// When the record was made, in epoch milliseconds.
pub(crate) fn at_ms(attrs: &Attrs) -> Option<i64> {
    let text = attrs.get("event.timestamp")?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Every log record in one OTLP/JSON logs export.
fn log_records(export: &Value) -> impl Iterator<Item = &Value> {
    export
        .get("resourceLogs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|rl| rl.get("scopeLogs").and_then(Value::as_array).into_iter().flatten())
        .flat_map(|sl| sl.get("logRecords").and_then(Value::as_array).into_iter().flatten())
}

/// Every usage record in one OTLP/JSON logs export (`POST /v1/logs`).
pub fn events_from_logs(export: &Value) -> Vec<UsageEvent> {
    log_records(export)
        .filter_map(|record| map_record(&attrs_of(record)))
        .collect()
}

/// The timing signals in an export: Codex's finished tool calls, each with the time it reports
/// (its turn boundaries come from the host's activity snapshots). The launch's token is the
/// CodeSense session, so the caller files them under it.
pub(crate) fn codex_timing_signals(export: &Value) -> Vec<(i64, super::timing::Signal)> {
    log_records(export)
        .filter_map(|record| {
            let attrs = attrs_of(record);
            (attrs.get("event.name")? == "codex.tool_result").then_some(())?;
            Some((at_ms(&attrs)?, super::timing::Signal::CodexTool))
        })
        .collect()
}

/// The usage event in one record: the first CLI's whose own usage record it is.
fn map_record(a: &Attrs) -> Option<UsageEvent> {
    AgentCli::ALL
        .into_iter()
        .find_map(|cli| (crate::agents::adapter::desktop(cli).usage_record)(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::event::Tokens;
    use serde_json::json;

    fn export(records: Vec<Vec<Value>>) -> Value {
        let log_records: Vec<Value> = records
            .into_iter()
            .map(|attributes| json!({ "attributes": attributes }))
            .collect();
        json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": log_records }] }] })
    }

    fn s(key: &str, v: &str) -> Value {
        json!({ "key": key, "value": { "stringValue": v } })
    }

    fn i(key: &str, v: i64) -> Value {
        json!({ "key": key, "value": { "intValue": v } })
    }

    fn d(key: &str, v: f64) -> Value {
        json!({ "key": key, "value": { "doubleValue": v } })
    }

    #[test]
    fn launch_exports_preserve_explicit_env_and_precede_prompt_operands() {
        for cli in [AgentCli::Claude, AgentCli::Codex, AgentCli::OpenCode] {
            let export = codesense_pty_core::agent_cli::descriptor(cli)
                .telemetry
                .as_ref()
                .unwrap();
            let mut env = vec![
                (
                    "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                    "explicit".to_string(),
                ),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT".to_string(),
                    "explicit-logs".to_string(),
                ),
                ("OTEL_LOGS_EXPORTER".to_string(), "console".to_string()),
                ("OTEL_SDK_DISABLED".to_string(), "true".to_string()),
                ("SAVED_CUSTOM_SETTING".to_string(), "kept".to_string()),
                ("CODESENSE_USAGE_ENDPOINT".to_string(), "stale".to_string()),
            ];
            let mut args = vec!["--".to_string(), "prompt".to_string()];
            configure_export(Some(export), Some(4567), "token", &mut env, &mut args);
            let unique: std::collections::HashSet<_> = env.iter().map(|(key, _)| key).collect();
            assert_eq!(
                unique.len(),
                env.len(),
                "every exported env key is unique for {cli}"
            );
            assert_eq!(
                env.iter()
                    .find(|(key, _)| key == "OTEL_EXPORTER_OTLP_ENDPOINT")
                    .unwrap()
                    .1,
                crate::http::routes::telemetry::endpoint_for(4567, "token")
            );
            for pair in [
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT".to_string(),
                    format!(
                        "{}/v1/logs",
                        crate::http::routes::telemetry::endpoint_for(4567, "token")
                    ),
                ),
                ("OTEL_LOGS_EXPORTER".to_string(), "otlp".to_string()),
                ("OTEL_SDK_DISABLED".to_string(), "false".to_string()),
                ("SAVED_CUSTOM_SETTING".to_string(), "kept".to_string()),
            ] {
                assert!(env.contains(&pair), "missing {pair:?} for {cli}");
            }
            assert_eq!(&args[args.len() - 2..], ["--", "prompt"]);
            let before = (env.clone(), args.clone());
            configure_export(Some(export), None, "token", &mut env, &mut args);
            assert_eq!((env, args), before);
        }
    }

    #[test]
    fn a_claude_api_request_maps_to_one_event_keyed_by_request_id() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_1"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
            i("input_tokens", 2),
            i("output_tokens", 4),
            i("cache_read_tokens", 10977),
            i("cache_creation_tokens", 15737),
            d("cost_usd", 0.00325937),
        ]]));
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.cli, AgentCli::Claude);
        assert_eq!(e.event_id, "req_1");
        assert_eq!(e.session_id, "sess-a");
        assert_eq!(
            e.tokens,
            Tokens {
                input: 2,
                output: 4,
                cache_read: 10977,
                cache_write: 15737,
                ..Default::default()
            }
        );
        assert_eq!(e.reported_cost_usd, Some(0.00325937));
    }

    #[test]
    fn a_claude_api_request_keeps_the_one_hour_part_of_its_cache_writes() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_1"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-opus-5-5"),
            i("cache_creation_tokens", 100),
            i("cache_creation_1h_tokens", 60),
            i("cache_creation_5m_tokens", 40),
        ]]));
        assert_eq!(events[0].tokens.cache_write, 100);
        assert_eq!(events[0].tokens.cache_write_1h, 60);
    }

    #[test]
    fn a_codex_completed_response_splits_cached_input_out_of_the_input_count() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "codex.sse_event"),
            s("event.kind", "response.completed"),
            s("conversation.id", "conv-1"),
            s("event.timestamp", "2026-10-16T18:53:59.775Z"),
            s("model", "gpt-6-astra"),
            s("input_token_count", "14264"),
            s("cached_token_count", "12288"),
            s("output_token_count", "5"),
            s("cache_write_token_count", "0"),
        ]]));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].tokens,
            Tokens {
                input: 1976,
                output: 5,
                cache_read: 12288,
                cache_write: 0,
                ..Default::default()
            }
        );
        assert_eq!(events[0].reported_cost_usd, None);
    }

    #[test]
    fn codex_streaming_events_that_are_not_completed_are_skipped() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "codex.sse_event"),
            s("event.kind", "response.output_text.delta"),
            s("conversation.id", "conv-1"),
            s("event.timestamp", "2026-10-16T18:53:59.775Z"),
            s("model", "gpt-6-astra"),
            s("input_token_count", "14264"),
        ]]));
        assert!(events.is_empty());
    }

    #[test]
    fn a_claude_cost_of_zero_with_tokens_is_unknown_not_free() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_free"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
            i("input_tokens", 5),
            d("cost_usd", 0.0),
        ]]));
        assert_eq!(events[0].reported_cost_usd, None);
    }

    #[test]
    fn a_claude_cost_above_the_bound_is_not_stored_as_reported() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_huge"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
            s("input_tokens", "5"),
            d("cost_usd", 1e9),
        ]]));
        assert_eq!(events[0].reported_cost_usd, None);
    }

    #[test]
    fn a_claude_cost_of_zero_with_no_tokens_stays_zero() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_cached"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
            d("cost_usd", 0.0),
        ]]));
        assert_eq!(events[0].reported_cost_usd, Some(0.0));
    }

    #[test]
    fn a_negative_claude_cost_is_refused() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("request_id", "req_neg"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
            i("input_tokens", 5),
            d("cost_usd", -1.0),
        ]]));
        assert_eq!(events[0].reported_cost_usd, None);
    }

    fn opencode_usage(message: &str) -> Vec<Value> {
        vec![
            s("event.name", "opencode.usage"),
            s("message_id", message),
            s("session.id", "ses_1"),
            s("event.timestamp", "2026-10-24T18:00:00.000Z"),
            s("model", "deepseek-v4-1-flash"),
            s("provider", "opencode-go"),
            i("input", 1200),
            i("output", 300),
            i("reasoning", 50),
            i("cache_read", 9000),
            i("cache_write", 700),
            d("cost", 0.0123),
        ]
    }

    #[test]
    fn an_opencode_usage_record_maps_to_one_event_keyed_by_message_id() {
        let events = events_from_logs(&export(vec![opencode_usage("msg_1")]));
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.cli, AgentCli::OpenCode);
        assert_eq!(e.event_id, "msg_1");
        assert_eq!(e.session_id, "ses_1");
        assert_eq!(e.model, "deepseek-v4-1-flash");
        assert_eq!(
            e.tokens,
            Tokens {
                input: 1200,
                output: 350,
                cache_read: 9000,
                cache_write: 700,
                ..Default::default()
            }
        );
        assert_eq!(e.reported_cost_usd, Some(0.0123));
    }

    #[test]
    fn an_opencode_cost_of_zero_with_tokens_is_unknown_not_free() {
        let mut record = opencode_usage("msg_free");
        record.retain(|a| a["key"] != "cost");
        record.push(d("cost", 0.0));
        let events = events_from_logs(&export(vec![record]));
        assert_eq!(events[0].reported_cost_usd, None);
    }

    #[test]
    fn a_free_opencode_variants_cost_of_zero_is_a_real_zero() {
        let mut record = opencode_usage("msg_free");
        record.retain(|a| a["key"] != "cost" && a["key"] != "model");
        record.push(s("model", "deepseek-v4.1-flash-free"));
        record.push(d("cost", 0.0));
        let events = events_from_logs(&export(vec![record]));
        assert_eq!(events[0].reported_cost_usd, Some(0.0));
    }

    #[test]
    fn an_opencode_record_without_a_message_id_is_not_guessed_at() {
        let mut record = opencode_usage("");
        record.retain(|a| a["key"] != "message_id");
        assert!(events_from_logs(&export(vec![record])).is_empty());
    }

    #[test]
    fn records_that_are_not_usage_are_skipped() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "user_prompt"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
        ]]));
        assert!(events.is_empty());
    }

    #[test]
    fn an_api_request_without_a_request_id_is_not_guessed_at() {
        let events = events_from_logs(&export(vec![vec![
            s("event.name", "api_request"),
            s("session.id", "sess-a"),
            s("event.timestamp", "2026-10-16T18:52:24.455Z"),
            s("model", "claude-haiku-5-5"),
        ]]));
        assert!(events.is_empty());
    }

    #[test]
    fn codex_tool_results_become_timing_signals() {
        use crate::session_usage::timing::Signal;
        let got = codex_timing_signals(&export(vec![
            vec![s("event.name", "codex.user_prompt"), s("event.timestamp", "2026-10-16T18:52:24.000Z")],
            vec![s("event.name", "codex.tool_result"), s("event.timestamp", "2026-10-16T18:52:26.000Z")],
            vec![s("event.name", "codex.sse_event"), s("event.timestamp", "2026-10-16T18:52:27.000Z")],
        ]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, Signal::CodexTool);
    }

    fn codex_args(saved: &[&str]) -> Vec<String> {
        let export = codesense_pty_core::agent_cli::descriptor(AgentCli::Codex).telemetry.as_ref().unwrap();
        let mut env = Vec::new();
        let mut args: Vec<String> = saved.iter().map(|a| a.to_string()).collect();
        args.extend(["--".to_string(), "prompt".to_string()]);
        configure_export(Some(export), Some(4567), "token", &mut env, &mut args);
        args
    }

    fn overrides(args: &[String], key: &str) -> Vec<String> {
        args.windows(2)
            .filter(|w| w[0] == "-c" && w[1].starts_with(&format!("{key}=")))
            .map(|w| w[1].clone())
            .collect()
    }

    #[test]
    fn a_managed_codex_launch_turns_its_default_metrics_and_analytics_export_off() {
        let args = codex_args(&[]);
        assert_eq!(overrides(&args, "otel.metrics_exporter"), ["otel.metrics_exporter=\"none\""]);
        assert_eq!(overrides(&args, "analytics.enabled"), ["analytics.enabled=false"]);
        // The usage export to CodeSense stays, and the prompt stays after the options.
        assert_eq!(overrides(&args, "otel.exporter").len(), 1);
        assert_eq!(&args[args.len() - 2..], ["--", "prompt"]);
    }

    #[test]
    fn an_agents_own_override_is_the_opt_in_and_is_not_overridden() {
        let args = codex_args(&["-c", "otel.metrics_exporter=\"statsig\"", "--config", "analytics.enabled=true"]);
        assert_eq!(overrides(&args, "otel.metrics_exporter"), ["otel.metrics_exporter=\"statsig\""]);
        assert_eq!(args.iter().filter(|a| a.starts_with("analytics.enabled=")).count(), 1);
        assert!(args.contains(&"analytics.enabled=true".to_string()));
    }

    #[test]
    fn claude_and_opencode_launches_add_no_codex_overrides() {
        for cli in [AgentCli::Claude, AgentCli::OpenCode] {
            let export = codesense_pty_core::agent_cli::descriptor(cli).telemetry.as_ref().unwrap();
            let mut args = vec!["--".to_string(), "prompt".to_string()];
            configure_export(Some(export), Some(4567), "token", &mut Vec::new(), &mut args);
            assert!(!args.iter().any(|a| a.contains("analytics.enabled") || a.contains("metrics_exporter")));
        }
    }
}
