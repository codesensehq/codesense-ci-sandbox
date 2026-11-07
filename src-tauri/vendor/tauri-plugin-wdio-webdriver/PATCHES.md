# Local patches

This is `tauri-plugin-wdio-webdriver` 1.3.0 as published on crates.io (MIT, see `LICENSE`;
upstream: https://github.com/webdriverio/desktop-mobile, `packages/tauri-plugin-webdriver`),
vendored so the app's end-to-end WebDriver builds can carry the fix below. The root `Cargo.toml`
patches it in for the crates.io package (`[patch.crates-io]`); the app depends on it only through
its `e2e-webdriver` feature, and `scripts/verify-wdio-plugin-release-absence.sh` proves a release
build's dependency graph never holds it. Dropped from the published package: `Cargo.lock`,
`.cargo-ok` and `docs/release-notes/v1.0.0.md` (an upstream release note with no build role; the
repository's public-export rules deny `docs/` paths). `Cargo.toml.orig` is kept as published and does not carry the patch's feature change.

Drop this copy once upstream runs a frame context's scripts inside the frame on macOS.

## macOS: scripts run inside the frame a session switched to

Upstream, `switchToFrame` on macOS only checks that the frame exists; every later script runs from
the top as `frame.contentWindow.eval(...)`, which WebKit refuses for a frame of another origin.

Patched:

- `src/platform/frame_registry.rs` (new): which native frame (`WKFrameInfo`) each entered frame
  context is, keyed by window label and frame path; the switch handshake's scripts.
- `src/platform/macos.rs`:
  - every webview gets a script in all frames, in a private `WKContentWorld`, and a message handler
    only that world sees. `switch_to_frame` checks the frame as before, then posts a one-time nonce
    into the frame's window; the frame's script hands it back through the handler, which files
    `message.frameInfo` under the new context. A frame that never answers fails the switch with
    `no such frame`.
  - in a frame context, `evaluate_js` (so every element command and `execute/sync`) and
    `execute_async_script` run through `callAsyncJavaScript…inFrame:` with the filed frame, in the
    page world. A context with no frame filed fails with `no such frame`; nothing falls back to
    running from the top. At the top, nothing changes.
  - the DirectEval loop is shared (`run_reported`) with the in-frame path; a result the message
    handler cannot take is now reported as an error instead of timing out.
- `src/platform/mod.rs`, `src/lib.rs`: the frame registry is managed app state.
- `src/platform/executor.rs`: `wrap_script_for_frame_context` is unused on macOS.
- `Cargo.toml`: `objc2-web-kit` feature `WKUserScript`, and an empty `[workspace]` so the crate
  can be tested on its own (`cargo test --manifest-path src-tauri/vendor/tauri-plugin-wdio-webdriver/Cargo.toml --lib`,
  with the root `Cargo.lock` copied in for the same versions).
