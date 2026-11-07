// SPDX-License-Identifier: MIT
//! Architecture: one health guard covers binding, authority checks, and tool execution.
use rmcp::model::{CallToolRequestParams, ErrorData};
use std::{future::Future, time::Duration};

// Policy (open): every call has a 15-minute absolute backstop; positive seconds are configurable.
const ABSOLUTE_DEFAULT_SECONDS: u64 = 15 * 60;
// Architecture: this poll reads the OS observer's decision; it does not decide staleness.
const HEALTH_POLL: Duration = Duration::from_secs(1);

fn absolute_seconds(value: Option<&str>) -> Duration {
    Duration::from_secs(
        value
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(ABSOLUTE_DEFAULT_SECONDS),
    )
}

pub(super) async fn call<T, F, Fut>(
    request: CallToolRequestParams,
    handler: F,
) -> Result<T, ErrorData>
where
    T: Send + 'static,
    F: FnOnce(CallToolRequestParams) -> Fut,
    Fut: Future<Output = Result<T, ErrorData>> + Send + 'static,
{
    guard(
        request,
        absolute_seconds(
            std::env::var("CODESENSE_MCP_TOOL_TIMEOUT_SECS")
                .ok()
                .as_deref(),
        ),
        crate::runtime_diagnostics::runtime_stalled,
        handler,
    )
    .await
}

fn stalled_error() -> ErrorData {
    ErrorData::internal_error("CodeSense UI/runtime unresponsive (runtime heartbeat stalled); wait, then retry; restart the app if it persists. Outcome may be unknown; inspect state before repeating mutations", None)
}

async fn guard<T, F, Fut>(
    request: CallToolRequestParams,
    backstop: Duration,
    unhealthy: impl Fn() -> bool,
    handler: F,
) -> Result<T, ErrorData>
where
    T: Send + 'static,
    F: FnOnce(CallToolRequestParams) -> Fut,
    Fut: Future<Output = Result<T, ErrorData>> + Send + 'static,
{
    if unhealthy() {
        return Err(stalled_error());
    }
    // Architecture: accepted work survives caller failure, preserving outbox execution and cleanup.
    let mut task = tokio::spawn(handler(request));
    let stalled = async {
        loop {
            if unhealthy() {
                return;
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    };
    // Architecture: complete Tokio starvation prevents ANY future from delivering this error
    // or the backstop. The independent OS observer's self-capture sample supplies evidence then.
    tokio::select! {
        biased;
        result = &mut task => result.map_err(|e| ErrorData::internal_error(format!("MCP handler task failed: {e}"), None))?,
        _ = stalled => Err(stalled_error()),
        _ = tokio::time::sleep(backstop) => Err(ErrorData::internal_error(format!("MCP tool call exceeded the absolute backstop of {} s. Outcome may be unknown; inspect state before retrying", backstop.as_secs()), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn slow() -> Result<u32, ErrorData> {
        tokio::time::sleep(Duration::from_secs(120)).await;
        Ok(7)
    }

    fn routes() -> [CallToolRequestParams; 2] {
        [
            CallToolRequestParams::new("test_machine_connection"),
            CallToolRequestParams::new("codesense_call_tool").with_arguments(
                serde_json::json!({"name": "save_machine", "arguments": {"connect": true}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_long_calls_are_not_cut_off_on_either_route() {
        for request in routes() {
            assert_eq!(
                guard(request, absolute_seconds(None), || false, |_| slow())
                    .await
                    .unwrap(),
                7
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_flag_fails_inflight_calls_on_both_routes_without_cancelling_cleanup() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        for request in routes() {
            let flag = Arc::new(AtomicBool::new(false));
            let read_flag = flag.clone();
            let (finished, cleanup) = tokio::sync::oneshot::channel();
            let started = tokio::time::Instant::now();
            let call = guard(
                request,
                absolute_seconds(None),
                move || read_flag.load(Ordering::Acquire),
                |_| async move {
                    let result = slow().await;
                    finished.send(()).unwrap();
                    result
                },
            );
            let stall = async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                // Architecture: simulate the atomic decision published by the OS observer.
                flag.store(true, Ordering::Release);
            };
            let (result, ()) = tokio::join!(call, stall);
            let error = result.unwrap_err();
            assert!(error.message.contains("CodeSense UI/runtime unresponsive"));
            assert!(error
                .message
                .contains("wait, then retry; restart the app if it persists"));
            assert!(started.elapsed() <= Duration::from_secs(3));
            cleanup.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_calls_still_have_a_fifteen_minute_absolute_backstop() {
        for request in routes() {
            let started = tokio::time::Instant::now();
            let error = guard(
                request,
                absolute_seconds(None),
                || false,
                |_| async {
                    tokio::time::sleep(Duration::from_secs(901)).await;
                    Ok(7)
                },
            )
            .await
            .unwrap_err();
            assert_eq!(started.elapsed(), Duration::from_secs(900));
            assert!(error.message.contains("absolute backstop of 900 s"));
            assert!(!error.message.contains("UI/runtime unresponsive"));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn known_stalled_runtime_rejects_before_starting_work() {
        let error = guard::<u32, _, _>(
            routes()[0].clone(),
            absolute_seconds(None),
            || true,
            |_| async { panic!("known-stalled calls must not start mutations") },
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("runtime heartbeat stalled"));
    }

    #[test]
    fn absolute_backstop_configuration_is_positive_and_open() {
        assert_eq!(absolute_seconds(None), Duration::from_secs(900));
        assert_eq!(absolute_seconds(Some("0")), Duration::from_secs(900));
        assert_eq!(absolute_seconds(Some("invalid")), Duration::from_secs(900));
        assert_eq!(absolute_seconds(Some("1800")), Duration::from_secs(1800));
    }
}
