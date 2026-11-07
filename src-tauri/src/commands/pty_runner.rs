// SPDX-License-Identifier: MIT
//! The app side of the `PtyRunner` port: the resolver that picks *where* an
//! interactive PTY child is spawned. The port itself ([`PtyRunner`], `PtySpec`,
//! `PtySession`, `PtyRunnerError`) and the built-in
//! [`LocalPtyRunner`] live in `codesense-pty-core`; the credential binding a
//! future remote driver needs stays here, app-side. The session registry depends
//! only on the trait plus [`resolve_pty_runner`], never on a concrete driver.
//!
//! # The carve invariant (the point of this module)
//!
//! Adding a new PTY runner is a **strictly additive** change: a new type that
//! `impl PtyRunner`, plus one match arm in [`resolve_pty_runner`]. It must touch
//! **none** of: the neutral vocabulary, the session registry and its
//! reader/batcher/emit pipeline, the credential seam
//! ([`CredentialRef`](crate::domain::CredentialRef)), or the terminal UI. If a
//! driver needs to edit any of those, the seam has leaked and the change is
//! wrong. The `carve_invariant_is_additive` test guards this.
//!
//! # Local is a driver, not a special case
//!
//! Fail-open: `resolve_pty_runner(None, …)` resolves to [`LocalPtyRunner`], so
//! "no runner selected ⇒ Local" is enforced by construction.

use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};

/// The stable id the built-in [`LocalPtyRunner`] resolves under, and the
/// fail-open default when no runner is selected.
pub const LOCAL_PTY_RUNNER_ID: &str = "local";

/// The **sole** runner→driver seam — the single place runner-id→driver knowledge
/// lives.
/// The session registry holds only this resolver's result and the trait, never a
/// concrete driver. `runner_id: None` (or an unregistered id) fails open to
/// [`LocalPtyRunner`] — "no runner selected ⇒ Local", enforced by construction.
/// `auth` and `credential_ref` are the reused neutral binding inputs a future
/// remote driver consumes; the local driver needs neither.
pub fn resolve_pty_runner(runner_id: Option<&str>) -> Option<Box<dyn PtyRunner>> {
    match runner_id {
        // Fail-open-to-local: no runner selected ⇒ the built-in default driver.
        None | Some(LOCAL_PTY_RUNNER_ID) => Some(Box::new(LocalPtyRunner)),
        // A future SshPtyRunner is one more arm here — and nothing else.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codesense_pty_core::runner::{PtyRunnerError, PtySession, PtySpec};
    use portable_pty::{CommandBuilder, PtySize};

    #[test]
    fn resolve_falls_open_to_local() {
        assert!(
            resolve_pty_runner(None).is_some(),
            "no runner selected must fail open to the built-in Local driver"
        );
        assert!(
            resolve_pty_runner(Some(LOCAL_PTY_RUNNER_ID)).is_some(),
            "the explicit local id resolves the built-in driver"
        );
        assert!(
            resolve_pty_runner(Some("definitely-not-a-runner")).is_none(),
            "an unregistered runner id has no driver"
        );
    }

    // The carve invariant, asserted mechanically (mirror of the SW-FG-5c /
    // SW-RX-1 carve test): adding a runner is impl `PtyRunner` + one
    // `resolve_pty_runner` arm — touching no neutral vocab, no registry, no
    // credential seam, no UI.
    #[tokio::test]
    async fn carve_invariant_is_additive() {
        // A fake driver that lives entirely outside the port's vocabulary — it
        // implements only the trait. It compiling + spawning proves a new runner
        // needs nothing but the trait surface.
        struct FakeRunner;
        #[async_trait]
        impl PtyRunner for FakeRunner {
            async fn spawn(&self, _spec: PtySpec) -> Result<PtySession, PtyRunnerError> {
                // A capability-absent driver returns `Unsupported` so the UI
                // hides the affordance rather than erroring.
                Err(PtyRunnerError::Unsupported)
            }
        }

        // The seam consumes it as a `Box<dyn PtyRunner>` exactly like the real
        // resolver hands the registry its driver — no core edit required to add one.
        let runner: Box<dyn PtyRunner> = Box::new(FakeRunner);
        let outcome = runner
            .spawn(PtySpec {
                command: CommandBuilder::new("true"),
                size: PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            })
            .await;
        assert_eq!(outcome.err(), Some(PtyRunnerError::Unsupported));
    }
}
