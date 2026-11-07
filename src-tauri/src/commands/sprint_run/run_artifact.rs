// SPDX-License-Identifier: MIT
//! SW-OR-12 — per-run output artifacts (a headless run's captured transcript).
//!
//! SW-AU-52's `RunOutcomeEvent` is a compact roll-up (`status` / `summary` /
//! `artifact_ref`); the run's actual *bytes* live here, one file per run at
//! `{codesense_home}/run-artifacts/{run_id}.log`, read only on demand when the
//! `AgentResultPanel` opens. Keeping the transcript OUT of `run-store.jsonl` keeps that
//! append-only log's read fold restart-cheap — a large transcript would bloat every read.
//! `artifact_ref` is a *pointer* to this file (`run-artifacts/{run_id}.log`), never the bytes.
//!
//! Anti-forgery (SE-5, AC-8): both the writer and the reader accept only the opaque minted
//! `run_id`, and reject any value that could escape the managed dir (a path separator / `..`).
//! Every fs touch runs off the async runtime via [`await_blocking`] (AC-2) — no work on a hot
//! async worker or the UI thread.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use crate::commands::await_blocking;
use crate::commands::paths::codesense_home_dir;
use crate::shared::proc_capture::CaptureOutput;

/// The persisted transcript's byte ceiling (~1 MiB, dev-tunable). A headless run has a
/// 30-min ceiling; a runaway could emit a lot, so we cap the file and keep the **tail** (the
/// end holds the outcome/error) so neither the disk nor the on-demand viewer read is unbounded.
const ARTIFACT_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunArtifactKind {
    Plain,
    Turns,
    RawTerminal,
}

impl RunArtifactKind {
    /// The kind's file extension — the ONE place each kind's suffix is spelled, so the
    /// latest-attempt name and the archived-attempt name (SW-AU-82A) can never disagree.
    fn ext(self) -> &'static str {
        match self {
            Self::Plain => "log",
            Self::Turns => "turns.json",
            Self::RawTerminal => "typescript",
        }
    }

    pub(crate) fn filename(self, run_id: &str) -> Option<String> {
        valid_run_id(run_id).then(|| format!("{run_id}.{}", self.ext()))
    }

    /// The archive name for a superseded attempt `n` (1-based): `{run_id}.a{n}.<ext>`.
    /// A retry reuses the `run_id` (gate.rs, GT-24), so a prior attempt's evidence is
    /// rotated here before the latest overwrites the legacy `{run_id}.<ext>` name.
    fn archive_filename(self, run_id: &str, n: u32) -> Option<String> {
        valid_run_id(run_id).then(|| format!("{run_id}.a{n}.{}", self.ext()))
    }
}

/// SW-AU-82A (AC-3b): before writing the latest evidence for `run_id`/`kind`, preserve any
/// existing file by moving it to the next free `{run_id}.a{n}.<ext>` archive slot — so a
/// GT-24 retry (which deliberately reuses the `run_id`) never overwrites a prior attempt's
/// evidence (ARCH-INV-9 append-only, applied to the evidence blobs). The legacy
/// `{run_id}.<ext>` name always holds the LATEST attempt, so existing readers are
/// byte-identical. Best-effort: a rename failure logs and leaves the current file to be
/// overwritten (the pre-AU-82A behavior), never failing the write. `pub(crate)` so the
/// `.turns.json` writer in `run_transcript` shares the ONE rotation rule.
///
/// **Numbering is chronological:** the legacy `{run_id}.<ext>` name is the *newest* attempt;
/// `a1` is the *oldest* superseded attempt, `a2` the next, and so on. This on-disk axis is not
/// the `run_identity::Attempt` ordinal — today the projection surfaces only the newest attempt
/// as the run's evidence keyed by `run_id`; full `a1..aN` → ordered-`Attempt` enumeration (and
/// routing archived reads back through the redacting reader) lands with the aggregate reader
/// (Slice 5 / AU-82B).
///
/// **Single-writer by construction:** a `run_id` is claimed once by the durable claim loop (each
/// run executes once) and its session writes its artifacts only as it terminates, so no two
/// writers rotate the same `run_id` concurrently. The `exists()`→`rename` probe therefore needs no cross-process
/// lock on this store; if that invariant ever changes, this must move to an exclusive-create
/// reservation.
pub(crate) fn archive_prior_attempt(dir: &Path, run_id: &str, kind: RunArtifactKind) {
    let Some(current) = kind.filename(run_id) else {
        return;
    };
    let current_path = dir.join(&current);
    if !current_path.exists() {
        return;
    }
    let mut n = 1u32;
    let archive = loop {
        let Some(name) = kind.archive_filename(run_id, n) else {
            return;
        };
        let candidate = dir.join(&name);
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };
    if let Err(e) = std::fs::rename(&current_path, &archive) {
        log::warn!(target: "run_artifact", "attempt archive failed for {run_id} ({e}); overwriting");
    }
}

/// `{codesense_home}/run-artifacts`, or `None` when the home dir is unresolvable (headless CI
/// with no `$HOME`) — the write then no-ops, same benign degrade as a failed write (AC-2).
/// `pub(crate)` so every run-artifact kind (the `.log` here, SW-OR-16's `.turns.json`) resolves the
/// ONE managed dir, never a second copy.
pub(crate) fn run_artifacts_dir() -> Option<PathBuf> {
    codesense_home_dir().map(|h| h.join("run-artifacts"))
}

/// AC-8: is `run_id` a well-formed opaque minted key (`run-headless-{ms}-{n}`)? The ONE anti-forgery
/// allowlist — ASCII alphanumerics plus `-`/`_`/`.`, no `..` — which rejects every path separator,
/// traversal, whitespace/control char, and non-minted value DETERMINISTICALLY (never left to degrade
/// via an OS error). `pub(crate)` so every artifact kind inherits the SAME guard (SW-OR-16 reuses it
/// for `{run_id}.turns.json`).
pub(crate) fn valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && !run_id.contains("..")
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The per-run filename `{run_id}.log`, or `None` if `run_id` is not a well-formed minted key.
fn artifact_filename(run_id: &str) -> Option<String> {
    RunArtifactKind::Plain.filename(run_id)
}

/// Keep only the last `budget` bytes of `bytes`, sliced on a UTF-8 boundary, prefixed with a
/// one-line `…[N bytes truncated]` marker when clipped. The END of a captured run holds its
/// outcome/error, so the tail is the signal. The ONE keep-the-tail truncator in the tree, so
/// the UTF-8 boundary rule lives in exactly one place.
pub fn tail_bounded(bytes: &[u8], budget: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= budget {
        return text.into_owned();
    }
    let largest_marker = format!("…[{} bytes truncated]\n", text.len());
    if largest_marker.len() > budget {
        if budget == 0 {
            return String::new();
        }
        let marker = if budget >= '…'.len_utf8() {
            "…"
        } else {
            "~"
        };
        let tail_budget = budget - marker.len();
        let mut start = text.len().saturating_sub(tail_budget);
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        return format!("{marker}{}", &text[start..]);
    }
    let mut start = text.len() - (budget - largest_marker.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let marker = format!("…[{start} bytes truncated]\n");
    let mut out = String::with_capacity(marker.len() + text.len() - start);
    out.push_str(&marker);
    out.push_str(&text[start..]);
    debug_assert!(out.len() <= budget);
    out
}

/// Compose the persisted transcript from a captured run: stdout, then a `--- stderr ---`
/// section when stderr is non-empty (v1 plain-text framing — no schema). Bounded to
/// [`ARTIFACT_MAX_BYTES`] (keep-the-tail), so the file the viewer reads is always bounded.
fn render_transcript(stdout: &[u8], stderr: &[u8]) -> String {
    if stderr.is_empty() {
        return tail_bounded(stdout, ARTIFACT_MAX_BYTES);
    }
    let mut buf = Vec::with_capacity(stdout.len() + stderr.len() + 16);
    buf.extend_from_slice(stdout);
    buf.extend_from_slice(b"\n--- stderr ---\n");
    buf.extend_from_slice(stderr);
    tail_bounded(&buf, ARTIFACT_MAX_BYTES)
}

/// Write the bounded transcript into `dir`, returning the stable relative `artifact_ref`
/// (`run-artifacts/{run_id}.log`). Pure over `dir` so it is unit-testable against a tempdir,
/// mirroring `run_store`'s `&Path`-injected core. `None` (no ref) when: the `run_id` is
/// rejected (AC-8), the run produced NO output at all (a side-effect-only success → the panel's
/// "No output captured" state — we don't litter empty files), or the write failed (best-effort).
/// `pub(crate)` so the workflow-executor `agent` arm persists its captured transcript through the
/// ONE writer (same rotation + `{run_id}.log` key the headless run path uses), rather than a second.
pub(crate) fn write_artifact_to(
    dir: &Path,
    run_id: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> Option<String> {
    let filename = artifact_filename(run_id)?;
    if stdout.is_empty() && stderr.is_empty() {
        return None;
    }
    let body = render_transcript(stdout, stderr);
    archive_prior_attempt(dir, run_id, RunArtifactKind::Plain);
    if let Err(e) =
        crate::shared::fs_atomic::write_bytes_atomic(&dir.join(&filename), body.as_bytes())
    {
        log::warn!(target: "run_artifact", "artifact write failed for {run_id}: {e}");
        return None;
    }
    Some(format!("run-artifacts/{run_id}.log"))
}

fn write_raw_terminal_to(dir: &Path, run_id: &str, snapshot: &str) -> bool {
    let Some(filename) = RunArtifactKind::RawTerminal.filename(run_id) else {
        return false;
    };
    if snapshot.is_empty() {
        return false;
    }
    let redacted = crate::shared::secret_scan::redact_secrets(snapshot);
    let bounded = tail_bounded(redacted.as_bytes(), ARTIFACT_MAX_BYTES);
    archive_prior_attempt(dir, run_id, RunArtifactKind::RawTerminal);
    crate::shared::fs_atomic::write_bytes_atomic(&dir.join(filename), bounded.as_bytes()).is_ok()
}

pub async fn write_raw_terminal_snapshot(run_id: &str, snapshot: String) {
    let Some(dir) = run_artifacts_dir() else {
        log::warn!(target: "run_artifact", "raw terminal snapshot directory is unavailable for {run_id}");
        return;
    };
    write_raw_terminal_snapshot_to(dir, run_id, snapshot).await;
}

pub(crate) async fn write_raw_terminal_snapshot_to(dir: PathBuf, run_id: &str, snapshot: String) {
    let run_id = run_id.to_string();
    if let Err(error) = await_blocking("run_artifact.raw.write", move || {
        if !write_raw_terminal_to(&dir, &run_id, &snapshot) {
            log::warn!(target: "run_artifact", "raw terminal snapshot not written for {run_id}");
        }
        Ok::<(), String>(())
    })
    .await
    {
        log::warn!(target: "run_artifact", "blocking raw terminal snapshot write failed: {error}");
    }
}

fn open_raw_terminal_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    {
        // WinBase.h: open the reparse point itself instead of following a
        // symlink/junction to content outside the confined artifact directory.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(std::io::Error::other(
                "raw terminal snapshot is not a regular file",
            ));
        }
        Err(error) => return Err(error),
    }
    options.open(path)
}

fn read_raw_terminal_from(dir: &Path, run_id: &str) -> Result<Option<String>, String> {
    let Some(filename) = RunArtifactKind::RawTerminal.filename(run_id) else {
        return Ok(None);
    };
    let path = dir.join(filename);
    let file = match open_raw_terminal_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("raw terminal snapshot open failed: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("raw terminal snapshot metadata failed: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("raw terminal snapshot is not a regular file".into());
    }
    if metadata.len() > ARTIFACT_MAX_BYTES as u64 {
        return Err("raw terminal snapshot exceeds the artifact byte limit".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(ARTIFACT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("raw terminal snapshot read failed: {error}"))?;
    if bytes.len() > ARTIFACT_MAX_BYTES {
        return Err("raw terminal snapshot exceeds the artifact byte limit".into());
    }
    String::from_utf8(bytes)
        // SW-AU-82A (AC-6): render-boundary scrub over the ONE routine (idempotent over the
        // persist-time scrub) — redaction holds on every read of the raw terminal snapshot.
        .map(|s| Some(crate::shared::secret_scan::redact_secrets(&s)))
        .map_err(|error| format!("raw terminal snapshot is not valid UTF-8: {error}"))
}

#[tauri::command]
pub async fn read_raw_terminal_snapshot(run_id: String) -> Result<Option<String>, String> {
    await_blocking("run_artifact.raw.read", move || match run_artifacts_dir() {
        Some(dir) => read_raw_terminal_from(&dir, &run_id),
        None => Ok(None),
    })
    .await
}

#[tauri::command]
pub async fn has_raw_terminal_snapshot(run_id: String) -> Result<bool, String> {
    await_blocking("run_artifact.raw.presence", move || {
        let Some(dir) = run_artifacts_dir() else {
            return Ok(false);
        };
        has_raw_terminal_from(&dir, &run_id)
    })
    .await
}

fn has_raw_terminal_from(dir: &Path, run_id: &str) -> Result<bool, String> {
    let Some(filename) = RunArtifactKind::RawTerminal.filename(run_id) else {
        return Ok(false);
    };
    match std::fs::symlink_metadata(dir.join(filename)) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("raw terminal snapshot metadata failed: {error}")),
    }
}

/// SW-OR-16: persist an already-composed, **already-scrubbed** text floor as the run's `.log`,
/// reusing the plain path's tail bound + atomic write + traversal guard. The structured path
/// (`run_transcript`) calls this with REDACTED stdout/stderr so a declaring run's floor (which is the
/// unredacted `stream-json`) never reaches disk raw; `write_run_artifact` stays byte-identical for a
/// non-declaring pack.
pub(crate) fn write_artifact_text(
    dir: &Path,
    run_id: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> Option<String> {
    write_artifact_to(dir, run_id, stdout, stderr)
}

/// Read a run's persisted transcript from `dir`. Pure over `dir` (tempdir-testable). `None`
/// when the `run_id` is rejected (AC-8), the run left no artifact (side-effect-only success, or
/// a run predating this feature), or the read failed → the panel renders "No output captured".
/// `pub(crate)` so the workflow-executor `agent` arm's tests read agent evidence back through the
/// same scrubbing reader the panel uses (proving the evidence path is wired to the read-time scrub).
pub(crate) fn read_artifact_from(dir: &Path, run_id: &str) -> Option<String> {
    let filename = artifact_filename(run_id)?;
    match std::fs::read_to_string(dir.join(&filename)) {
        // SW-AU-82A (AC-6): redaction is a data property — scrub at render, over the ONE routine.
        // The Plain `.log` is written RAW (unlike the raw-terminal artifact, which scrubs at persist),
        // so this read-time scrub is the authoritative redaction for it and also covers any export
        // that reads this path.
        Ok(body) => Some(crate::shared::secret_scan::redact_secrets(&body)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!(target: "run_artifact", "artifact read failed for {run_id}: {e}");
            None
        }
    }
}

/// SW-OR-12 (AC-1/AC-2/AC-4): persist a completed headless run's captured transcript, keyed by
/// the trusted-side-resolved `run_id`, off the async runtime. Returns the `artifact_ref` to
/// thread onto SW-AU-52's outcome, or `None` on any failure / empty run — best-effort, never
/// fatal: a failed write is a `warn!` and the outcome simply carries `artifact_ref: None`.
pub async fn write_run_artifact(run_id: &str, out: &CaptureOutput) -> Option<String> {
    let Some(dir) = run_artifacts_dir() else {
        log::warn!(target: "run_artifact", "no codesense home — skipping artifact write for {run_id}");
        return None;
    };
    let run_id = run_id.to_string();
    let stdout = out.stdout.clone();
    let stderr = out.stderr.clone();
    await_blocking("run_artifact.write", move || {
        Ok::<_, String>(write_artifact_to(&dir, &run_id, &stdout, &stderr))
    })
    .await
    .unwrap_or(None)
}

/// SW-OR-12 (AC-5/AC-8): read a completed run's persisted transcript by its opaque `run_id` —
/// the `AgentResultPanel`'s body source (completed case). Off the async runtime (AC-2). `None`
/// → the panel renders its "No output captured" state. A `run_id` with a path separator / `..`
/// is rejected (AC-8); only `{codesense_home}/run-artifacts/` is ever read.
#[tauri::command]
pub async fn read_run_artifact(run_id: String) -> Result<Option<String>, String> {
    await_blocking("run_artifact.read", move || {
        Ok(run_artifacts_dir().and_then(|dir| read_artifact_from(&dir, &run_id)))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_bounded_returns_whole_input_under_budget() {
        assert_eq!(tail_bounded(b"hello", 1024), "hello");
    }

    #[test]
    fn tail_bounded_keeps_tail_and_marks_drop_over_budget() {
        let out = tail_bounded(b"0123456789012345678901234567890123456789", 32);
        assert!(out.ends_with("56789"), "should keep the tail: {out:?}");
        assert!(
            out.contains("bytes truncated"),
            "should mark the drop: {out:?}"
        );
        assert!(out.len() <= 32);
    }

    #[test]
    fn tail_bounded_slices_on_utf8_boundary() {
        // "é" is 2 bytes; a budget that would split it must step forward to a boundary,
        // never panic or emit invalid UTF-8.
        let input = "aéb".as_bytes(); // 'a'(1) + 'é'(2) + 'b'(1) = 4 bytes
        let out = tail_bounded(input, 2);
        assert!(out.is_char_boundary(0));
        assert!(out.ends_with('b'));
    }

    // ── SW-AU-82A (AC-3b): retries never overwrite a prior attempt's evidence ────────────────

    #[test]
    fn attempt_and_archive_filenames_are_stable_and_reject_bad_ids() {
        assert_eq!(
            RunArtifactKind::Plain.filename("run-1").as_deref(),
            Some("run-1.log"),
            "legacy name is byte-identical (back-compat)"
        );
        assert_eq!(
            RunArtifactKind::Turns
                .archive_filename("run-1", 2)
                .as_deref(),
            Some("run-1.a2.turns.json")
        );
        assert!(RunArtifactKind::Plain.filename("../escape").is_none());
        assert!(RunArtifactKind::Plain
            .archive_filename("../escape", 1)
            .is_none());
    }

    #[test]
    fn archive_prior_attempt_is_a_noop_when_nothing_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        archive_prior_attempt(dir.path(), "run-1", RunArtifactKind::Plain);
        assert!(!dir.path().join("run-1.log").exists());
        assert!(!dir.path().join("run-1.a1.log").exists());
    }

    #[test]
    fn a_retry_rotates_the_prior_log_and_never_clobbers_it() {
        let dir = tempfile::tempdir().unwrap();
        // Attempt 1 lands on the legacy name (existing readers unchanged).
        write_artifact_to(dir.path(), "run-1", b"first attempt", b"").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.log")).unwrap(),
            "first attempt"
        );
        // Attempt 2 (a GT-24 retry reuses the run_id) rotates attempt 1 to a1 and writes latest.
        write_artifact_to(dir.path(), "run-1", b"second attempt", b"").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.log")).unwrap(),
            "second attempt",
            "legacy name holds the LATEST attempt"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.a1.log")).unwrap(),
            "first attempt",
            "prior attempt preserved, never clobbered"
        );
        // Attempt 3 rotates again — every attempt's evidence survives.
        write_artifact_to(dir.path(), "run-1", b"third attempt", b"").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.log")).unwrap(),
            "third attempt"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.a2.log")).unwrap(),
            "second attempt"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-1.a1.log")).unwrap(),
            "first attempt"
        );
    }

    // ── SW-AU-82A (AC-6): redaction holds at the render boundary, not only at persist ────────

    #[test]
    fn read_artifact_scrubs_a_secret_at_render_even_if_the_file_was_written_raw() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        // Simulate evidence written before the persist-time scrub (or an export path).
        std::fs::write(dir.path().join("run-1.log"), format!("token {secret}\n")).unwrap();
        let body = read_artifact_from(dir.path(), "run-1").expect("reads the file");
        assert!(
            !body.contains(secret),
            "secret must not survive the render read"
        );
        assert!(
            body.contains("[redacted]"),
            "render scrub leaves a legible marker"
        );
    }

    #[test]
    fn read_raw_terminal_scrubs_a_secret_at_render_even_if_written_raw() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        // A `.typescript` snapshot placed on disk raw (older evidence / export) — no persist scrub.
        std::fs::write(
            dir.path().join("run-1.typescript"),
            format!("boot {secret}\n"),
        )
        .unwrap();
        let got = read_raw_terminal_from(dir.path(), "run-1")
            .expect("read ok")
            .expect("reads the snapshot");
        assert!(
            !got.contains(secret),
            "secret must not survive the raw-terminal render read"
        );
        assert!(got.contains("[redacted]"), "render scrub leaves a marker");
    }

    #[test]
    fn a_retry_rotates_the_prior_raw_terminal_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_raw_terminal_to(dir.path(), "run-2", "first snapshot"));
        assert!(write_raw_terminal_to(
            dir.path(),
            "run-2",
            "second snapshot"
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-2.typescript")).unwrap(),
            "second snapshot",
            "legacy name holds the latest attempt"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-2.a1.typescript")).unwrap(),
            "first snapshot",
            "prior attempt rotated, never clobbered"
        );
    }

    #[test]
    fn tail_bounded_tiny_budgets_still_mark_truncation() {
        assert_eq!(tail_bounded(b"abcdef", 0), "");
        assert_eq!(tail_bounded(b"abcdef", 1), "~");
        assert_eq!(tail_bounded(b"abcdef", 2), "~f");
        assert_eq!(tail_bounded(b"abcdef", 3), "…");
    }

    #[test]
    fn tail_bounded_keeps_full_marker_when_it_exactly_fills_budget() {
        let input = vec![b'x'; 100];
        let marker = "…[100 bytes truncated]\n";
        assert_eq!(tail_bounded(&input, marker.len()), marker);
    }

    #[test]
    fn render_transcript_stdout_only_has_no_stderr_section() {
        let out = render_transcript(b"just stdout", b"");
        assert_eq!(out, "just stdout");
    }

    #[test]
    fn render_transcript_appends_stderr_section() {
        let out = render_transcript(b"out", b"err");
        assert_eq!(out, "out\n--- stderr ---\nerr");
    }

    #[test]
    fn write_then_read_round_trips_and_returns_relative_ref() {
        let dir = tempfile::tempdir().unwrap();
        let reff = write_artifact_to(dir.path(), "run-abc", b"the log", b"").unwrap();
        assert_eq!(reff, "run-artifacts/run-abc.log");
        assert_eq!(
            read_artifact_from(dir.path(), "run-abc").as_deref(),
            Some("the log")
        );
    }

    #[test]
    fn write_bounds_an_over_budget_transcript_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![b'x'; ARTIFACT_MAX_BYTES + 4096];
        write_artifact_to(dir.path(), "run-big", &big, b"").unwrap();
        let body = read_artifact_from(dir.path(), "run-big").unwrap();
        assert!(
            body.len() <= ARTIFACT_MAX_BYTES + 64,
            "bounded to ~1 MiB: {}",
            body.len()
        );
        assert!(body.starts_with("…["), "keeps the tail with a marker");
    }

    #[test]
    fn empty_run_writes_no_file_and_returns_no_ref() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(write_artifact_to(dir.path(), "run-empty", b"", b""), None);
        // a side-effect-only run leaves NO artifact → the reader reports "no output".
        assert_eq!(read_artifact_from(dir.path(), "run-empty"), None);
    }

    #[test]
    fn read_of_absent_run_is_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_artifact_from(dir.path(), "never-ran"), None);
    }

    #[test]
    fn malformed_run_ids_are_rejected_on_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        // Traversal / separators AND non-minted shapes (whitespace, control, unicode) are all
        // rejected deterministically by the allowlist — never written, never read.
        for bad in [
            "../escape",
            "a/b",
            "a\\b",
            "..",
            "",
            "a b",
            "a\tb",
            "rün",
            "a\0b",
        ] {
            assert_eq!(artifact_filename(bad), None, "must reject {bad:?}");
            assert_eq!(write_artifact_to(dir.path(), bad, b"x", b""), None);
            assert_eq!(read_artifact_from(dir.path(), bad), None);
        }
        // The minted shape (alphanumerics + dashes) is accepted.
        assert_eq!(
            artifact_filename("run-headless-1724680000000-3").as_deref(),
            Some("run-headless-1724680000000-3.log"),
        );
    }

    #[test]
    fn raw_terminal_is_redacted_bounded_and_uses_closed_filename() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "token: hunter2secret\n";
        let snapshot = format!("{secret}{}tail", "x".repeat(ARTIFACT_MAX_BYTES + 32));
        assert!(write_raw_terminal_to(dir.path(), "run-raw-1", &snapshot));
        let body = read_raw_terminal_from(dir.path(), "run-raw-1")
            .unwrap()
            .unwrap();
        assert!(!body.contains("hunter2secret"));
        assert!(body.ends_with("tail"));
        assert!(body.len() <= ARTIFACT_MAX_BYTES);
        assert!(dir.path().join("run-raw-1.typescript").is_file());
    }

    #[test]
    fn raw_terminal_rejects_empty_and_forged_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!write_raw_terminal_to(dir.path(), "run-empty", ""));
        assert!(!write_raw_terminal_to(dir.path(), "../escape", "data"));
        assert_eq!(read_raw_terminal_from(dir.path(), "../escape"), Ok(None));
    }

    #[test]
    fn raw_terminal_read_distinguishes_missing_from_real_io_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_raw_terminal_from(dir.path(), "run-missing"), Ok(None));
        std::fs::create_dir(dir.path().join("run-unreadable.typescript")).unwrap();
        assert!(read_raw_terminal_from(dir.path(), "run-unreadable").is_err());
    }

    #[test]
    fn raw_terminal_read_rejects_oversize_and_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("run-oversize.typescript"),
            vec![b'x'; ARTIFACT_MAX_BYTES + 1],
        )
        .unwrap();
        assert!(read_raw_terminal_from(dir.path(), "run-oversize").is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("run-symlink.typescript"))
                .unwrap();
            assert_eq!(has_raw_terminal_from(dir.path(), "run-symlink"), Ok(false));
            assert!(read_raw_terminal_from(dir.path(), "run-symlink").is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn raw_terminal_read_rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-fifo.typescript");
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is a valid, NUL-terminated path owned for the duration
        // of the call; mkfifo does not retain its pointer.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

        let error = read_raw_terminal_from(dir.path(), "run-fifo").unwrap_err();
        assert!(error.contains("not a regular file"));
    }

    #[test]
    fn raw_terminal_read_rejects_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run-invalid.typescript"), [0xff, 0xfe]).unwrap();
        let error = read_raw_terminal_from(dir.path(), "run-invalid").unwrap_err();
        assert!(error.contains("not valid UTF-8"));
    }

    #[test]
    fn raw_terminal_presence_distinguishes_non_file_and_metadata_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(has_raw_terminal_from(dir.path(), "run-missing"), Ok(false));
        std::fs::create_dir(dir.path().join("run-directory.typescript")).unwrap();
        assert_eq!(
            has_raw_terminal_from(dir.path(), "run-directory"),
            Ok(false)
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                "run-loop.typescript",
                dir.path().join("run-loop.typescript"),
            )
            .unwrap();
            assert_eq!(has_raw_terminal_from(dir.path(), "run-loop"), Ok(false));
        }
    }

    #[tokio::test]
    async fn blocking_raw_write_failure_is_nonfatal() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_directory = dir.path().join("file");
        std::fs::write(&not_a_directory, b"occupied").unwrap();
        write_raw_terminal_snapshot_to(not_a_directory, "run-write-fails", "snapshot".into()).await;
    }
}
