# Vendored dependency patches

Patches here are applied automatically by pnpm on every `pnpm install`
(`pnpm.patchedDependencies` in the root `package.json`). They are committed to
git, so fresh clones, CI, and every developer get them with no manual step.

## `@xterm__addon-webgl@0.19.0.patch` — un-share the WebGL glyph atlas; size the render model per row

**What it changes:** `acquireTextureAtlas` in BOTH published builds —
`lib/addon-webgl.mjs` (the package `module` entry, which Vite bundles) and
`lib/addon-webgl.js` (the CJS `main` entry) — loses the cross-terminal
"join an existing same-config atlas" loop:

```js
// removed:
for (let t = 0; t < cache.length; t++) {
  const entry = cache[t];
  if (configEquals(entry.config, config)) return (entry.ownedBy.push(terminal), entry.atlas);
}
```

so acquisition always falls through to `new TextureAtlas(...)`. Result: **each
terminal gets its own glyph atlas**. The per-terminal reuse loop (the same
`Terminal` re-acquiring its own atlas) and `removeTerminalFromCache` disposal are
untouched, so lifecycle and cleanup stay correct.

**Both files must be patched.** Which one ships depends on the resolver: Vite
picks `module` (`.mjs`), Node/CJS tooling picks `main` (`.js`). The patch once
edited only the `.js`, so the shipped bundle kept sharing while a file-level
guard passed.

**Why:** xterm shares one `TextureAtlas` across every same-config terminal, but
each terminal's WebGL renderer caches glyph texture coordinates for cells that
haven't changed. When any terminal clears or repopulates the shared atlas, its
siblings keep drawing from the stale coordinates — blank cells or the wrong
glyph ("DDDD" drawn as "6666") that `term.refresh()` cannot heal.

**Cost:** one glyph atlas per terminal (a few MB of texture each) instead of one
shared — negligible.

**Guards (they fail if the un-share regresses):**

- `src/shared/lib/webgl-atlas-unshared.test.ts` — drives the real addon code
  (both entries) with two real xterm terminals and asserts distinct atlases.
- `scripts/vite-plugin-webgl-atlas-guard.ts` — fails every Vite build whose
  bundle still contains the sharing loop (keyed on the `ownedBy` property names,
  which survive minification; fails closed if the atlas cache can't be found).

### Second change — `RenderModel.resize` reallocates `lineLengths` on a row change

**What it changes:** upstream `RenderModel.resize(cols, rows)` reallocates both
`cells` and `lineLengths` only when the cell COUNT (`cols*rows`) changes. The patch
splits that: `cells` still reallocates on a count change, and `lineLengths` now
reallocates whenever its length differs from `rows`.

**Why:** the glyph renderer copies vertices for `lineLengths.length` rows only. A
transposed resize with the same cell count (e.g. 36x17 → 17x36, a wide pane turned
into a tall column by a mosaic preset switch) kept the old 17-entry array, so rows
17+ drew blank forever while the buffer, and the cursor (drawn by the rectangle
renderer), were intact. No refresh heals it; only a later resize to a different cell
count does.

**Guard:** `src/shared/lib/webgl-render-model-resize.test.ts` (both entries) resizes
a real terminal 80x36 → 36x17 → 17x36 and asserts one line length per row and that
the last row gets a line length on render.

### Re-creating this patch after an `@xterm/addon-webgl` upgrade

pnpm will FAIL the install if the pinned version (`0.19.0`) no longer matches —
that is intentional, so the fix can't silently disappear. To re-apply:

```sh
pnpm patch @xterm/addon-webgl@<new-version>
# In the printed temp dir, delete the `acquireTextureAtlas` cross-terminal join
# loop (the one that returns a cached atlas via `ownedBy.push`), and split
# RenderModel.resize so `lineLengths` reallocates whenever its length !== rows,
# in BOTH lib/addon-webgl.mjs and lib/addon-webgl.js.
pnpm patch-commit <temp-dir>
```

Then run the guards above (`vitest run src/shared/lib/webgl-atlas-unshared.test.ts
src/shared/lib/webgl-render-model-resize.test.ts` and `pnpm run build`).

**Upstream watch:** if xterm ever exposes a per-terminal-atlas option (or an
`acquireTextureAtlas` opt-out), prefer that over this patch and delete it If an
upstream release reallocates `lineLengths` on a row change, drop that half.

## `@wdio__tauri-service@1.3.0.patch` — teardown log-stream write-after-end flake

**What it changes:** wraps the file-log write in `forwardLog` (both `dist/esm/index.js`
and `dist/cjs/index.js`) in a new `safeWriterWrite(writer, msg)` helper instead of the
bare `writer.write(msg)`. The helper:

- skips the write when the underlying `LogWriter.logStream` is already
  `writableEnded`/`writableFinished`/`destroyed`/not-writable (the teardown case);
- attaches a one-time `'error'` listener that swallows ONLY `ERR_STREAM_WRITE_AFTER_END`
  (the WriteStream reports write-after-end via an async `'error'` event, not a throw);
- try/catches the write and re-throws anything that is not `ERR_STREAM_WRITE_AFTER_END`.

**Why:** on shutdown the service calls `closeLogWriter()` → `LogWriter.close()` →
`this.logStream.end()`, but the singleton still reports `isLogWriterInitialized()` true
until the async `end()` callback nulls the stream. If the built app emits a final stdout
log line during that window, `createLogCapture`'s `onLine` → `forwardLog` →
`writer.write()` → `stream.write()` runs AFTER end, the WriteStream emits an unhandled
`'error'` (`ERR_STREAM_WRITE_AFTER_END`), and the driver process crashes with exit code 2
— even though the spec already fully passed (`12 passing`). `scripts/quality-real.ts`
derives the run verdict from the process exit (2 → HARNESS_FAILURE), so a fully-clean run
flipped to HARNESS_FAILURE (cli-lifecycle run 7 PASS vs run 8 HARNESS_FAILURE, no code
change). This protects ALL real-app suites (smoke, terminal-webgl, soak, cli-lifecycle).

**Why it can't mask a real failure:** it only neutralizes writes to a stream that is
already ending/ended, and only swallows the single `ERR_STREAM_WRITE_AFTER_END` code. Any
other write error still throws; all in-test logging (stream open) is unchanged; and it does
not touch the exit code of a genuine crash (app-launch failure, mid-test driver crash, real
product exit), which crash before/independently of the log stream being `.end()`ed.

### Re-creating this patch after a `@wdio/tauri-service` upgrade

pnpm will FAIL the install if the pinned version (`1.3.0`) no longer matches. To re-apply:

```sh
pnpm patch @wdio/tauri-service@<new-version>
# In the printed temp dir, in BOTH dist/esm/index.js and dist/cjs/index.js:
#   - add the `safeWriterWrite` helper just above `function forwardLog(`
#   - replace the `writer.write(formattedMessage)` call inside forwardLog's
#     isInitialized branch with `safeWriterWrite(writer, formattedMessage)`
pnpm patch-commit <temp-dir>
```

**Upstream watch:** if `@wdio/tauri-service`/`@wdio/native-core` ever guards the
teardown write itself (e.g. `LogWriter.write` no-ops after `close()`), prefer that and
delete this patch.
