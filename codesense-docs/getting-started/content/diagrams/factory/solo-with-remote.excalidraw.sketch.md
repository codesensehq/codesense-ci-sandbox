# factory/solo-with-remote.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `solo-with-remote.excalidraw` +
`render_canvas` → `solo-with-remote.excalidraw.png`. Until those MCP
tools land, `arc-l-factory/solo-with-remote.html` renders the same
shape via an inline SVG stand-in with the same coordinate space.

Export at **1280×720**.

See `./local-only.excalidraw.sketch.md` §0 for canvas setup, palette
tokens, and font — identical across every factory diagram.

Additional palette entry for this diagram:

| Token          | Hex (dark) | Role                     |
| -------------- | ---------- | ------------------------ |
| `--mac-remote` | `#a898ff`  | remote-mac dot and frame |

---

## 1 · Shape list

### 1.1 Mac frames (two)

| id      | x   | y   | w   | h   | label                              | fill      | stroke (2px)   |
| ------- | --- | --- | --- | --- | ---------------------------------- | --------- | -------------- |
| `mac-l` | 60  | 80  | 540 | 560 | `MacBook · local — dev@local` | surface-2 | `--mac-local`  |
| `mac-r` | 680 | 80  | 540 | 560 | `MacBook · remote — dev@mini` | surface-2 | `--mac-remote` |

Each frame carries a 10×10 solid dot in its stroke color in the
top-right corner. A thin dashed arc between them (320, 320) → (680, 320)
labeled `private-account bridge` sits above both frames, 1.4px, dash 3-3,
in `--muted`.

### 1.2 Agent chips — local Mac

Coordinates are canvas-absolute.

| id          | x   | y   | w   | h   | label      | stroke            |
| ----------- | --- | --- | --- | --- | ---------- | ----------------- |
| `triager`   | 100 | 180 | 220 | 80  | `triager`  | `--role-triager`  |
| `builder-1` | 100 | 290 | 220 | 70  | `builder`  | `--role-builder`  |
| `resolver`  | 100 | 400 | 220 | 70  | `resolver` | `--role-resolver` |
| `analyst`   | 360 | 290 | 220 | 70  | `analyst`  | `--role-triager`  |

Every chip carries a 6×6 `--mac-local` dot in its top-left corner.

### 1.3 Agent chips — remote Mac

| id          | x   | y   | w   | h   | label      | stroke            |
| ----------- | --- | --- | --- | --- | ---------- | ----------------- |
| `builder-2` | 720 | 180 | 220 | 70  | `builder`  | `--role-builder`  |
| `builder-3` | 720 | 270 | 220 | 70  | `builder`  | `--role-builder`  |
| `reviewer`  | 720 | 390 | 220 | 70  | `reviewer` | `--role-reviewer` |
| `scout`     | 980 | 290 | 220 | 70  | `scout`    | `--role-scout`    |

Every chip carries a 6×6 `--mac-remote` dot in its top-left corner.

### 1.4 Edges — routing

Arrow heads: `--muted`, 1.4px.

| id      | from         | to          | shape                       | note            |
| ------- | ------------ | ----------- | --------------------------- | --------------- |
| `t→b1`  | `triager`    | `builder-1` | straight                    | local build     |
| `t→b2`  | `triager`    | `builder-2` | arc across (crosses bridge) | remote build    |
| `t→b3`  | `triager`    | `builder-3` | arc across                  | remote build    |
| `t→rev` | `triager`    | `reviewer`  | arc across                  | remote review   |
| `t→res` | `triager`    | `resolver`  | straight                    | local decisions |
| `t→sct` | `triager`    | `scout`     | arc across                  | remote scout    |
| `t→an`  | `triager`    | `analyst`   | straight                    | local synthesis |
| `loop`  | `(1200,580)` | `triager`   | long arc under both frames  | reply queue     |

Cross-bridge edges thicken to 1.6px in `--muted` and carry a 10×6 tag
`private-account` centered on the arc.

### 1.5 Trigger edges

| id         | from         | to          | stroke                      |
| ---------- | ------------ | ----------- | --------------------------- |
| `trig-b`   | _(off-left)_ | `builder-1` | `--accent`, dash 3-3, 1.4px |
| `trig-b2`  | _(off-top)_  | `builder-2` | `--accent`, dash 3-3, 1.4px |
| `trig-rev` | _(off-top)_  | `reviewer`  | `--accent`, dash 3-3, 1.4px |

Captions `inbox_idle · 2m` sit above each dashed arc in `--muted` 16px.

### 1.6 Captions

- Title (24px, `--ink`, top-left): "Solo + remote · 1 Mac + 1-2 remote macs"
- Sub (16px, `--muted`): "home office + the Mac mini in the closet"
- Legend (bottom-right, 12px): one `--mac-local` dot = "home mac"; one `--mac-remote` dot = "mini"
- Primitives footer (14px, `--muted`): `worktrees · inbox roles · routines · private-account · inbox_idle`
