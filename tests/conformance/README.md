# Dashboard conformance

Run `pnpm test:conformance` for the getting-started built-ins, the host slot cases,
and the suite's good/bad self-tests, all in real WebKit.

To check another pane (relative assets resolve beside the HTML file):

```sh
pnpm test:conformance -- path/to/dashboard.html path/to/another.html
```

Each pane gets a fresh mount for focus-in, Escape, flush, budget denial,
hidden/stale/resync, maximize/restore, reparent, axe plus keyboard escape, and live theme following.
Flush retires a frame, so checks cannot share one mount. Reparent may reload it;
the check compares its view and verifies subscriptions resume. Failures name the
check and its reason. The host fixtures use the real viewer, shim, and slot host,
shared with the pane harness and frame-security tests.

Policy: conformance gates becoming a built-in default. Installing a custom pane
does not require it. Confirmation overrides belong only to the test fixtures;
the app's default permissions remain open.

The theme check applies every bundled theme by id within each light/dark mode, both
while open and while hidden, without reloading. Body colours must change with
the palette; hidden catch-up must match the colours observed while open. It
samples text, control borders and directly painted SVG
shapes against their own surfaces, including alpha compositing. Elements inside
opacity groups are skipped: their ink and background must be composited together,
which this sampler does not model. SVG containers and indirect graphics such as
`use` are also skipped.
Built-ins fail every check. For other panes, a theme failure prints
`doesn't follow theme: <reason>` and the remaining checks still run.
Set `CONFORMANCE_THEME_POLICY=fail` to make external theme failures strict.

`node scripts/check-hardcoded-colors.mjs` also scans all built-in pane HTML and
JavaScript for colour literals. To check another pane with the same scanner:

```sh
node scripts/check-hardcoded-colors.mjs --panes path/to/pane.html
```
