// SPDX-License-Identifier: MIT
//! Warm-pool member handoff. A pool member is an ordinary registry session
//! flagged `is_pool_member`; handing it off re-keys it to the frontend's session
//! id, resizes it, stamps its metadata, arms the output gate, and `cd`s it to the
//! target — so the viewport sees a clean prompt with no startup flash. The pool's
//! membership / readiness policy is the host's; this is the session side.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::warn;

use crate::domain::{TerminalDomainEvent, TerminalEventSink};
use crate::state::{deliver_output, PtyState, SessionMeta, TerminalSession};
use crate::term_state::{SessionRuntime, SharedTermState};

/// How long the warm-handoff output gate waits for the post-cd OSC 133
/// prompt-start before falling back to a silent flush (Surface 5). Covers the
/// rare case where shell integration is disabled/overridden so the marker never
/// fires — the user sees the cd flash but is never stuck on a blank viewport.
const WARM_SUPPRESS_TIMEOUT: Duration = Duration::from_millis(1000);

/// Single-quote a value for a PTY-injected shell line (the warm-pool
/// `cd <path> && clear`, an agent launch line), so a value containing
/// spaces or shell metacharacters is passed literally. Every PTY-line producer
/// shares ONE quoter instead of re-spelling it.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The single PTY-writer line applied on warm-pool handoff: re-export the
/// rebound session id (so the agent status hook reports the real id, not the
/// `__warm_pool_N` placeholder it booted with), `cd` to the target, and `clear`
/// the echo. Folded into one line so the trailing `clear` wipes all three under
/// the suppress-until-prompt gate.
///
/// The line begins with a LEADING SPACE so the managed `.zshrc`'s
/// `setopt HIST_IGNORE_SPACE` keeps this codesense-plumbing line out of the
/// user's ↑ history (`clear` only wipes the visible echo, not the history
/// entry). The `cd`-safety validation still holds — a leading space can't break
/// the single-line `&&` chain or the OSC gate.
fn warm_handoff_command(session_id: &str, target_cwd: &str) -> String {
    format!(
        " export CODESENSE_TAB_SESSION_ID={} && cd {} && clear\n",
        shell_quote(session_id),
        shell_quote(target_cwd),
    )
}

/// Whether `target_cwd` is safe to hand to a warm-pool member, which `cd`s into
/// it via a single shell line echoed under the suppress-until-prompt gate
/// ([`warm_handoff_command`]). The warm path is only sound when the path can't
/// break that one line and the `cd` will succeed:
/// - control chars defeat the gate — a newline splits the handoff into multiple
///   shell commands (extra prompt-start → the gate opens early and leaks the
///   echo), and an ESC could forge an OSC 133 marker;
/// - an empty or non-existent directory makes `cd` fail, which short-circuits
///   the trailing `clear` (`&&`) and stalls first paint until the 1s timeout.
///
/// The path must also be absolute: the gate stats it via [`std::path::Path`]
/// (relative to the process cwd), but the warm member booted at `$HOME` and its
/// `cd <rel>` would resolve there — so a relative path could pass the gate yet
/// fail the `cd` (the same stall). An absolute `cd` is base-independent, so
/// requiring absolute makes the gate verdict match the member's outcome.
///
/// When unsafe, the caller skips the warm path and cold-spawns instead, which
/// applies the cwd via `exec` (no shell parse, so control chars are literal) and
/// fails loudly at spawn for a missing dir rather than stalling.
pub fn is_warm_safe_cwd(target_cwd: &str) -> bool {
    let path = Path::new(target_cwd);
    // Cheap checks first; the `is_dir` stat last. (`is_absolute` also rejects "".)
    path.is_absolute() && !target_cwd.contains(char::is_control) && path.is_dir()
}

impl PtyState {
    /// Re-key a popped warm member to the frontend session id and apply the
    /// handoff (resize, metadata stamp, pool-flag clear, gate arm, terminal restart,
    /// `cd` write) under the sessions lock, then arm the timeout guard. On any
    /// failure the member is retired (killed and reaped under its own id), so
    /// the caller only has to cold-spawn instead.
    pub fn handoff_warm_member(
        &self,
        member_id: &str,
        session_id: &str,
        target_cwd: &str,
        cols: u16,
        rows: u16,
        meta: SessionMeta,
    ) -> Result<(), String> {
        let (shared, runtime, displaced) = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            let mut session = sessions
                .remove(member_id)
                .ok_or_else(|| format!("warm member {} vanished before handoff", member_id))?;
            match apply_handoff(&mut session, session_id, target_cwd, cols, rows, meta) {
                Ok(()) => {
                    if let Some(incarnation) = self.activity.register(session_id, session.pty.pid())
                    {
                        let _ = session.runtime.activity.set((
                            self.activity.clone(),
                            session_id.to_owned(),
                            incarnation,
                        ));
                    }
                    let shared = session.shared.clone();
                    let runtime = session.runtime.clone();
                    let displaced = sessions.insert(session_id.to_string(), session);
                    (shared, runtime, displaced)
                }
                Err(error) => {
                    drop(sessions);
                    // Report the member's eventual exit under its own id, never
                    // under the frontend id the caller is about to cold-spawn.
                    *session
                        .runtime
                        .session_id
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = member_id.to_string();
                    self.retire(session);
                    return Err(error);
                }
            }
        };
        if let Some(old) = displaced {
            self.retire(old);
        }

        spawn_suppress_timeout_guard(&self.sink, &shared, &runtime);
        // RF-perf-17: the handed-off session is now live for `target_cwd`.
        // Announced after the `sessions` map lock is released above.
        let _ = self.sink.emit(TerminalDomainEvent::WorktreeLive {
            cwd: PathBuf::from(target_cwd),
        });
        Ok(())
    }
}

/// Apply the handoff to a popped member (the caller holds the sessions lock):
/// resize, metadata stamp, rename + pool-flag clear + gate arm, terminal
/// restart, and the `cd` write.
fn apply_handoff(
    session: &mut TerminalSession,
    session_id: &str,
    target_cwd: &str,
    cols: u16,
    rows: u16,
    meta: SessionMeta,
) -> Result<(), String> {
    // Geometry: the member booted at the pool default size.
    session
        .pty
        .resize(cols, rows)
        .map_err(|e| format!("Resize error: {}", e))?;

    // Stamp metadata so the handed-off terminal is a normal user terminal
    // everywhere. Rename + clear pool flag + arm the gate via the
    // shared runtime — the already-running batcher sees these live.
    session.story_id = meta.story_id;
    session.workspace_id = meta.workspace_id;
    session.epic_id = meta.epic_id;
    // RF-perf-17: re-anchor the reverse-sync liveness home to the workspace the
    // member is being handed off into. A pool member SPAWNS in a neutral warm dir
    // and `cd`s to `target_cwd` on handoff, so its spawn cwd is NOT its worktree —
    // the stable anchor must follow the handoff. `target_cwd` is
    // `is_warm_safe_cwd`-validated by the caller.
    session.home_cwd = Some(PathBuf::from(target_cwd));
    *session
        .runtime
        .session_id
        .lock()
        .map_err(|e| format!("Lock error: {}", e))? = session_id.to_string();
    session
        .runtime
        .is_pool_member
        .store(false, Ordering::Relaxed);
    session
        .runtime
        .suppress_until_prompt
        .store(true, Ordering::Relaxed);

    // Drop the $HOME-boot scrollback so reattach/persist start at the target
    // prompt: a fresh emulator terminal at the handoff size.
    {
        let mut g = session
            .shared
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        g.held.clear();
        #[cfg(debug_assertions)]
        g.record_resize(cols, rows);
        let seq = g.seq;
        if let Some(emulator) = g.emulator.as_mut() {
            emulator.restart(seq, (cols, rows));
        }
    }

    // Write the handoff command LAST (bounded) — the only step that touches the
    // PTY writer. Re-exports the rebound session id so the in-shell agent status
    // hook stops reporting the `__warm_pool_N` placeholder.
    // LOCK-ORDER INVARIANT: the caller holds `sessions` (map) and this takes the
    // `writer` lock — never the reverse. The hot write path takes the map lock,
    // clones the writer `Arc`, then RELEASES the map before locking the writer, so
    // no path ever holds a `writer` lock while acquiring `sessions`.
    let cd = warm_handoff_command(session_id, target_cwd);
    let writer = session.pty.writer();
    let mut writer = writer.lock().map_err(|e| format!("Lock error: {}", e))?;
    writer
        .write_all(cd.as_bytes())
        .map_err(|e| format!("Write error: {}", e))?;
    writer.flush().map_err(|e| format!("Flush error: {}", e))?;
    Ok(())
}

/// Surface 5 (silent): if the post-cd OSC 133 prompt-start never arrives within
/// [`WARM_SUPPRESS_TIMEOUT`], drop the gate and flush whatever the shell
/// produced since handoff so the viewport isn't stuck blank. No toast / no UI.
fn spawn_suppress_timeout_guard(
    sink: &Arc<dyn TerminalEventSink>,
    shared: &Arc<Mutex<SharedTermState>>,
    runtime: &Arc<SessionRuntime>,
) {
    let sink = sink.clone();
    let shared = shared.clone();
    let runtime = runtime.clone();
    // Fallback handle: if the guard thread can't be spawned (resource
    // exhaustion), clear the gate so a subscriber's bounded wait isn't paid in
    // full and first paint proceeds — the batcher still opens the gate normally
    // on the real prompt-start; this only matters if OSC 133 also never fires.
    let runtime_fallback = runtime.clone();
    let spawned = std::thread::Builder::new()
        .name("warm-suppress-timeout".into())
        .spawn(move || {
            std::thread::sleep(WARM_SUPPRESS_TIMEOUT);
            // Open the gate + flush the buffered post-handoff output atomically
            // under the per-session lock, so the batcher (which also opens the
            // gate + delivers under this lock) can never deliver bytes this flush
            // already covered — no double-delivery. If the batcher opened the
            // gate first (prompt-start fired), `suppress` is already false and we
            // do nothing.
            let Ok(mut g) = shared.lock() else {
                return;
            };
            if !runtime.suppress_until_prompt.load(Ordering::Relaxed) {
                return;
            }
            let snapshot = std::mem::take(&mut g.held);
            // The held output ends at the stream's current end.
            let seq = g.seq;
            runtime
                .suppress_until_prompt
                .store(false, Ordering::Relaxed);
            let sid = runtime.current_id();
            warn!(target: "warm_pool", "warm handoff prompt-start timeout for {sid} — flushing buffered output");
            if !snapshot.is_empty() {
                #[cfg(debug_assertions)]
                g.record_output(seq, &snapshot);
                deliver_output(&*sink, &mut g.subscribers, &sid, &snapshot, seq);
                if let Some(emulator) = g.emulator.as_mut() {
                    emulator.feed(seq, &snapshot);
                }
            }
        });
    if spawned.is_err() {
        warn!(target: "warm_pool", "could not spawn warm-handoff timeout guard — clearing gate");
        runtime_fallback
            .suppress_until_prompt
            .store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::gated_slice;

    /// Regression: the warm-handoff writer line MUST re-export the
    /// rebound session id ahead of the `cd`, or the in-shell agent status hook
    /// keeps reporting the `__warm_pool_N` placeholder and the per-tab dot never
    /// lights. The bug was a missing `export`; this pins it.
    #[test]
    fn warm_handoff_command_reexports_rebound_session_id() {
        let cmd = warm_handoff_command("shell-abc", "/tmp/x");
        // Leading space: HIST_IGNORE_SPACE keeps this plumbing line out of ↑ history.
        assert_eq!(
            cmd,
            " export CODESENSE_TAB_SESSION_ID='shell-abc' && cd '/tmp/x' && clear\n"
        );
        assert!(
            cmd.starts_with(' '),
            "leading space hides it from shell history"
        );
        // The export must precede the cd (env is set before the shell settles at
        // the target so any agent launched there inherits the real id).
        let export_at = cmd.find("export CODESENSE_TAB_SESSION_ID").unwrap();
        let cd_at = cmd.find("cd ").unwrap();
        assert!(export_at < cd_at, "export must come before cd");
    }

    /// The warm path is gated on a warm-safe cwd. A real
    /// directory is safe; empty, control-char (gate-defeating), and
    /// non-existent paths are not — those route to a cold spawn.
    #[test]
    fn is_warm_safe_cwd_accepts_real_dir_rejects_unsafe() {
        // An existing directory (the crate dir at compile time) is safe.
        assert!(is_warm_safe_cwd(env!("CARGO_MANIFEST_DIR")));
        // Empty → `cd ''` would fail → stall.
        assert!(!is_warm_safe_cwd(""));
        // Relative → gate stats vs process cwd but the member `cd`s from $HOME,
        // so a relative path is false-safe; require absolute.
        assert!(!is_warm_safe_cwd("relative/dir"));
        assert!(!is_warm_safe_cwd("."));
        // Newline → splits the single handoff line into multiple shell commands.
        assert!(!is_warm_safe_cwd("/tmp\nrm -rf /"));
        // ESC → could forge an OSC 133 prompt-start and defeat the gate.
        assert!(!is_warm_safe_cwd("/tmp/\x1b]133;A"));
        // Non-existent path → `cd` fails → stall (cold spawn fails loudly instead).
        assert!(!is_warm_safe_cwd(
            "/nonexistent/cs-warm-safe-cwd/should/not/exist"
        ));
    }

    /// While gated, the delivered slice begins
    /// exactly at the post-cd prompt-start marker — the handoff-command echo
    /// (`export … && cd … && clear`) is never delivered, so the re-exported
    /// session id never flashes into the viewport.
    #[test]
    fn gated_slice_cuts_to_prompt_start_byte_exact() {
        // A realistic post-handoff batch: echo + 133;B + clear + 133;D + OSC7 + 133;A + prompt.
        let prompt = "\x1b]133;A\x1b\\user@host /target $ ";
        let echo = warm_handoff_command("shell-abc", "/target");
        let batch = format!(
            "{echo}\x1b]133;B\x1b\\\x1b[H\x1b[2J\x1b]133;D;0\x1b\\\x1b]7;file:///target\x1b\\{prompt}"
        );
        // The parser reports the prompt-start ESC offset; gate slices from there.
        let off = batch.find("\x1b]133;A").unwrap();
        let delivered = gated_slice(Some(off), &batch).unwrap();
        assert_eq!(delivered, prompt);
        assert!(!delivered.contains("cd "), "cd echo must not be delivered");
        assert!(
            !delivered.contains("export "),
            "export (session-id) echo must not be delivered"
        );
        assert!(!delivered.contains("shell-abc"), "session id must not leak");
        assert!(
            !delivered.contains("\x1b[2J"),
            "clear must not be delivered"
        );
    }

    /// No prompt-start in the batch → deliver nothing (stay gated).
    #[test]
    fn gated_slice_is_none_without_prompt_start() {
        assert_eq!(
            gated_slice(None, &warm_handoff_command("shell-x", "/x")),
            None
        );
    }

    /// Cwd quoting passes spaces / quotes literally to the injected cd.
    #[test]
    fn shell_quote_handles_spaces_and_quotes() {
        assert_eq!(shell_quote("/Users/me/my dir"), "'/Users/me/my dir'");
        assert_eq!(shell_quote("/a/o'brien"), "'/a/o'\\''brien'");
    }
}
