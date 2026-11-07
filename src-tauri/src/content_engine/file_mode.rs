// SPDX-License-Identifier: MIT
//! Path-static destination mode bits for materialized content.
//!
//! Executable information can't always ride along with content bytes, so
//! the destination mode is reconstituted from the logical name at write
//! time. The rule is a single path-prefix check: anything under
//! `.claude/hooks/codesense-` materializes as `0o755`; everything else as
//! `0o644`.
//!
//! Layer-agnostic — the SINGLE source of truth for the "hook ⇒ `0o755`"
//! policy, applied to every materialized file regardless of which content
//! layer won the resolve. Its callers:
//! the preparation engine's writes (`agents::prepare`) and the structural
//! source ops; SW-XS-8 deleted the projection and install write paths.
//!
//! (Previously lived in the now-deleted `core_source` module as
//! `read_core_file_mode`; it was always called cross-layer by the sync
//! materialize engine — see `sync::materialize`.)

/// Default (non-executable) destination mode. A CoW clone / copy already
/// lands a materialized dest at this mode, so [`reconcile_executable_mode`]
/// treats it as the "nothing to do" case and skips the syscall.
const DEFAULT_MODE: u32 = 0o644;
/// Executable destination mode for Codesense-managed hook scripts.
const EXECUTABLE_MODE: u32 = 0o755;

/// Logical-name prefix that identifies a Codesense-managed hook script.
/// Hooks under this prefix materialize executable (`0o755`); everything
/// else gets `0o644`. See [`executable_mode_for`].
const HOOK_PREFIX: &str = ".claude/hooks/codesense-";

/// Return the destination mode bits to use when materializing `logical_name`.
///
/// Pure function — no filesystem access. The sync materialize engine calls
/// this after the atomic rename, for every layer.
pub fn executable_mode_for(logical_name: &str) -> u32 {
    if logical_name.starts_with(HOOK_PREFIX) {
        EXECUTABLE_MODE
    } else {
        DEFAULT_MODE
    }
}

#[cfg(not(unix))]
pub(crate) fn apply_mode_bits(_dest: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// The mode to FORCE on a freshly-materialized `dest`, or `None` to leave the
/// clone's inherited source mode untouched.
///
/// The first-write analog of [`reconcile_executable_mode`]'s gating: only hook
/// scripts get a forced mode ([`EXECUTABLE_MODE`]); every other path returns
/// `None`. This is the surgical fix for the projection/content path — which was
/// `mode: None` (inherit everything) before SW-FX-60 and only needed hooks
/// forced executable. Forcing non-hook files to [`DEFAULT_MODE`] would strip a
/// source's exec bit (the content path is two-way; codesense is not the
/// authority on non-hook modes there) and would make first-write inconsistent
/// with the self-heal. Both now touch ONLY hook paths.
///
/// (The install/resolver path is one-way and codesense-authoritative, so it
/// deliberately passes `Some(executable_mode_for(..))` to normalize every dest —
/// a different tier with different authority.)
pub(crate) fn forced_mode_for(logical_name: &str) -> Option<u32> {
    match executable_mode_for(logical_name) {
        EXECUTABLE_MODE => Some(EXECUTABLE_MODE),
        _ => None,
    }
}

/// Mode required at a selected destination, while its manifest retains the
/// authored source mode for provenance and admission validation.
pub(crate) fn selected_destination_mode(logical_name: &str, authored_mode: u32) -> u32 {
    forced_mode_for(logical_name).unwrap_or(authored_mode & 0o777)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_mode_only_forces_codesense_hooks() {
        assert_eq!(
            selected_destination_mode(".claude/hooks/codesense-notify.sh", 0o644),
            0o755
        );
        assert_eq!(
            selected_destination_mode(".claude/hooks/user.sh", 0o644),
            0o644
        );
        assert_eq!(selected_destination_mode("scripts/tool.sh", 0o755), 0o755);
    }

    #[test]
    fn mode_is_executable_for_hook_path() {
        assert_eq!(
            executable_mode_for(".claude/hooks/codesense-notify.sh"),
            0o755
        );
    }

    #[test]
    fn mode_is_readable_for_skill_path() {
        assert_eq!(
            executable_mode_for(".claude/skills/sense-worktree-discovery/SKILL.md"),
            0o644
        );
    }

    #[test]
    fn mode_is_readable_for_unrelated_path() {
        assert_eq!(executable_mode_for("README.md"), 0o644);
    }

    #[test]
    fn forced_mode_is_some_only_for_hooks() {
        // Hooks are forced executable; everything else returns None (inherit the
        // clone's source mode — the projection path does not force 0o644).
        assert_eq!(
            forced_mode_for(".claude/hooks/codesense-notify.sh"),
            Some(0o755)
        );
        assert_eq!(forced_mode_for(".claude/skills/foo/SKILL.md"), None);
        assert_eq!(forced_mode_for(".claude/scripts/tool.sh"), None);
        assert_eq!(forced_mode_for("README.md"), None);
    }
}
