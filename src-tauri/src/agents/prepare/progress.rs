// SPDX-License-Identifier: MIT
//! SW-XS-7: Step 2's progress events and the startup note (SPEC §8.1, §8.5).
//!
//! A run emits `diff`, then `transfer` and `apply` when it copies anything.
//! The caller then emits exactly one terminal event with [`finish`] — `ready`,
//! `failed` or `skipped` — after its own post-checks, so `ready` never
//! precedes the end of Step 2 (X5). A `warning` (pinned startup files that
//! differ from the agent definition) comes just before it.
use serde::Serialize;

use super::PrepareReport;

/// The Tauri event every Step 2 progress report is emitted on.
pub const PREPARE_PROGRESS_EVENT: &str = "agent-prepare-progress";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreparePhase {
    Diff,
    Transfer,
    Apply,
    /// Something to surface that does not block the launch.
    Warning,
    Ready,
    Failed,
    /// Nothing prepared here (the destination is team content itself).
    Skipped,
    /// SW-XS-8: an explicit operation was cancelled.
    Cancelled,
}

/// What `ready` / `failed` carry for the workspace details and the agent:
/// every list capped at [`SUMMARY_LIST`] paths, with full counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareSummary {
    pub status: super::PrepareStatus,
    pub wanted: usize,
    pub written: usize,
    pub bytes_written: u64,
    pub unchanged: usize,
    pub kept_modified: Vec<String>,
    pub kept_modified_count: usize,
    pub kept_unmanaged: Vec<String>,
    pub kept_unmanaged_count: usize,
    pub skipped_tracked: Vec<String>,
    pub skipped_tracked_count: usize,
    /// `[path, reason]`.
    pub skipped_other: Vec<(String, String)>,
    pub skipped_other_count: usize,
    pub pinned_drift: Vec<String>,
    /// `[path, reason]`.
    pub failed: Vec<(String, String)>,
    pub failed_count: usize,
    pub leftovers: usize,
    pub elapsed_ms: u64,
}

/// How many paths a summary list carries.
pub const SUMMARY_LIST: usize = 50;

impl PrepareSummary {
    pub fn of(report: &PrepareReport) -> Self {
        let cap = |paths: &Vec<String>| paths.iter().take(SUMMARY_LIST).cloned().collect();
        Self {
            status: report.status,
            wanted: report.wanted,
            written: report.written.len(),
            bytes_written: report.bytes_written,
            unchanged: report.unchanged,
            kept_modified: cap(&report.kept_modified),
            kept_modified_count: report.kept_modified.len(),
            kept_unmanaged: cap(&report.kept_unmanaged),
            kept_unmanaged_count: report.kept_unmanaged.len(),
            skipped_tracked: cap(&report.skipped_tracked),
            skipped_tracked_count: report.skipped_tracked.len(),
            skipped_other: report
                .skipped_other
                .iter()
                .take(SUMMARY_LIST)
                .cloned()
                .collect(),
            skipped_other_count: report.skipped_other.len(),
            pinned_drift: report.pinned_drift.clone(),
            failed: report.failed.iter().take(SUMMARY_LIST).cloned().collect(),
            failed_count: report.failed.len(),
            leftovers: report.leftovers,
            elapsed_ms: report.elapsed_ms,
        }
    }
}

/// `{workspace, agent, phase, files, bytes, total}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareProgress {
    pub workspace: String,
    pub agent: String,
    pub phase: PreparePhase,
    /// Files done in this phase.
    pub files: u64,
    /// Bytes done in this phase.
    pub bytes: u64,
    /// What the phase counts towards: wanted files (diff, ready), bytes to
    /// send (transfer), operations (apply).
    pub total: u64,
    /// `ready` / `failed` / `warning` / `skipped`: the startup note, or why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `ready` / `failed`: what was kept, skipped, drifted or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<PrepareSummary>,
    /// SW-XS-8: the explicit Step 3 operation this event belongs to (absent
    /// on a launch's Step 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// SW-XS-8 (review r1): which explicit operation (`hydrate`,
    /// `hydrateStory`, `publish`), so a caption can say what it is doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<String>,
}

/// The terminal events of one run: a `warning` for pinned drift, then one
/// of `ready` / `failed` / `skipped` with the note and summary. Call it after
/// the launch's own post-checks.
pub fn finish(
    progress: &dyn Fn(&PrepareProgress),
    report: &PrepareReport,
    workspace: &str,
    agent: &str,
    from: &str,
) {
    finish_operation(progress, report, workspace, agent, from, None)
}

/// [`finish`] for an explicit Step 3 operation: its events carry its id.
pub fn finish_operation(
    progress: &dyn Fn(&PrepareProgress),
    report: &PrepareReport,
    workspace: &str,
    agent: &str,
    from: &str,
    operation: Option<&str>,
) {
    let event = |phase, note: String, summary| PrepareProgress {
        workspace: workspace.to_string(),
        agent: agent.to_string(),
        phase,
        files: report.written.len() as u64,
        bytes: report.bytes_written,
        total: report.wanted as u64,
        note: Some(note),
        summary,
        operation_id: operation.map(str::to_string),
        operation_kind: None,
    };
    if !report.pinned_drift.is_empty() {
        progress(&event(PreparePhase::Warning, pinned_line(report), None));
    }
    let phase = match report.status {
        super::PrepareStatus::Ready => PreparePhase::Ready,
        super::PrepareStatus::Failed => PreparePhase::Failed,
        super::PrepareStatus::Skipped => PreparePhase::Skipped,
        super::PrepareStatus::Cancelled => PreparePhase::Cancelled,
    };
    progress(&event(
        phase,
        startup_note(report, from),
        Some(PrepareSummary::of(report)),
    ));
}

/// The pinned-drift line (§11.15). Edited or unmanaged pinned files are
/// restorable with a hydrate scoped to exactly those paths; a file the repo
/// tracks (or a symlink, or anything else Step 2 skips) is not, since
/// CodeSense never writes it.
fn pinned_line(report: &PrepareReport) -> String {
    let unfixable = |path: &String| {
        report.skipped_tracked.contains(path) || report.skipped_other.iter().any(|(p, _)| p == path)
    };
    let (blocked, fixable): (Vec<String>, Vec<String>) =
        report.pinned_drift.iter().cloned().partition(unfixable);
    let mut line = format!(
        "{} from the agent definition: {}.",
        if report.pinned_drift.len() == 1 {
            "1 pinned file differs".to_string()
        } else {
            format!("{} pinned files differ", report.pinned_drift.len())
        },
        listed(&report.pinned_drift)
    );
    if !fixable.is_empty() {
        let paths: Vec<String> = fixable.iter().map(|p| format!("{p:?}")).collect();
        line.push_str(&format!(
            " {} kept as the workspace has {}; to restore the definition's version, call hydrate_worktree with paths:[{}] and overwriteModified:true.",
            if fixable.len() == 1 { "It was" } else { "They were" },
            if fixable.len() == 1 { "it" } else { "them" },
            paths.join(",")
        ));
    }
    if !blocked.is_empty() {
        line.push_str(&format!(
            " CodeSense never writes {}, so a hydrate can't restore {}: {}.",
            if blocked.len() == 1 {
                "this one (the repo tracks it, or it isn't a regular file)"
            } else {
                "these (the repo tracks them, or they aren't regular files)"
            },
            if blocked.len() == 1 { "it" } else { "them" },
            listed(&blocked)
        ));
    }
    line
}

/// `1 file`, `2 files`: the count and its noun, agreeing.
fn files(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Emit each report on [`PREPARE_PROGRESS_EVENT`] through `sink`.
pub fn to_sink(sink: &dyn crate::commands::worktree::EventSink) -> impl Fn(&PrepareProgress) + '_ {
    move |progress| {
        if let Ok(payload) = serde_json::to_value(progress) {
            sink.emit(PREPARE_PROGRESS_EVENT, payload);
        }
    }
}

fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes as f64 >= MB / 10.0 {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

fn listed(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
    let more = paths.len().saturating_sub(shown.len());
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// `Prepared 312 files (4.1 MB) from team default in 2.3 s. 2 kept because
/// you modified them: … To get more, call hydrate_worktree.`
pub(super) fn startup_note(report: &PrepareReport, from: &str) -> String {
    if let Some(reason) = &report.skipped_reason {
        return format!("Agent context was not copied: {reason}.");
    }
    let mut note = format!(
        "Prepared {} ({}) from {from} in {:.1} s",
        files(report.wanted, "file"),
        size(report.wanted_bytes),
        report.elapsed_ms as f64 / 1000.0
    );
    if !report.written.is_empty() {
        note.push_str(&format!(
            " ({} copied, {})",
            report.written.len(),
            size(report.bytes_written)
        ));
    }
    note.push('.');
    if !report.kept_modified.is_empty() {
        note.push_str(&format!(
            " {} kept because you modified them: {}.",
            report.kept_modified.len(),
            listed(&report.kept_modified)
        ));
    }
    if !report.kept_unmanaged.is_empty() {
        note.push_str(&format!(
            " {} kept because a file CodeSense didn't write is there: {}.",
            report.kept_unmanaged.len(),
            listed(&report.kept_unmanaged)
        ));
    }
    if !report.pinned_drift.is_empty() {
        note.push(' ');
        note.push_str(&pinned_line(report));
    }
    if !report.skipped_tracked.is_empty() {
        note.push_str(&format!(
            " {} skipped because the repo tracks them: {}.",
            report.skipped_tracked.len(),
            listed(&report.skipped_tracked)
        ));
    }
    if !report.skipped_other.is_empty() {
        let skipped: Vec<String> = report
            .skipped_other
            .iter()
            .map(|(path, why)| format!("{path} ({why})"))
            .collect();
        note.push_str(&format!(
            " {} skipped: {}.",
            skipped.len(),
            listed(&skipped)
        ));
    }
    if !report.failed.is_empty() {
        let failed: Vec<String> = report.failed.iter().map(|(p, _)| p.clone()).collect();
        note.push_str(&format!(
            " {} couldn't be copied: {}.",
            failed.len(),
            listed(&failed)
        ));
    }
    if report.leftovers > 0 {
        note.push_str(&if report.leftovers == 1 {
            " 1 file that automatic sync copied here before is no longer in this agent's context. It stays until you delete it; list_hydrated shows it with origin legacy.".to_string()
        } else {
            format!(
                " {} files that automatic sync copied here before are no longer in this agent's context. They stay until you delete them; list_hydrated shows them with origin legacy.",
                report.leftovers
            )
        });
    }
    note.push_str(" To get more, call hydrate_worktree.");
    note
}

/// The last Step 2 summary of each local launch handle, for the MCP result
/// that reports the launch (`open_workspace`): the launch path stashes it,
/// the result builder takes it once.
fn stash() -> &'static std::sync::Mutex<std::collections::HashMap<String, PrepareSummary>> {
    static STASH: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, PrepareSummary>>,
    > = std::sync::OnceLock::new();
    STASH.get_or_init(Default::default)
}

pub fn stash_summary(handle: &str, summary: PrepareSummary) {
    stash()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle.to_string(), summary);
}

pub fn take_summary(handle: &str) -> Option<PrepareSummary> {
    stash()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(handle)
}
