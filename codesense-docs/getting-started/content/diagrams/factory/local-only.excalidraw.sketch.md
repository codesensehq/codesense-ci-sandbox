# factory/local-only.excalidraw — canvas sketch

Authoring spec for the follow-up PR that calls `apply_canvas` to
produce `local-only.excalidraw` and `render_canvas` to produce
the sibling `local-only.excalidraw.png`. Until those MCP tools land, the
shipped `arc-l-factory/local-only.html` renders the same shape via an
inline SVG stand-in using the same coordinate space, so swapping to the
PNG is a no-layout change.

Export at **1280×720** so the HTML's 16:9 frame math stays trivial.

---

## 0 · Canvas setup

| Prop                | Value                                                       |
| ------------------- | ----------------------------------------------------------- |
| viewBackgroundColor | `#0b0e14` (dark; a light re-export carries the same shapes) |
| gridSize            | 10                                                          |
| exportBackground    | true                                                        |
| exportPadding       | 24                                                          |
| viewportSize        | 1280 × 720                                                  |

Palette tokens (match the arc-l-factory HTML role chroma):

| Token             | Hex (dark) | Role                 |
| ----------------- | ---------- | -------------------- |
| `--role-triager`  | `#6ea8f0`  | triager              |
| `--role-builder`  | `#4ec28a`  | builder              |
| `--role-reviewer` | `#a898ff`  | reviewer             |
| `--role-scout`    | `#e0ac55`  | scout                |
| `--role-resolver` | `#f0a93b`  | resolver             |
| `--mac-local`     | `#4ec28a`  | local-mac dot        |
| `--ink`           | `#e6ebf3`  | labels, node strokes |
| `--muted`         | `#8a94a6`  | edges, badges        |
| `--accent`        | `#6ea8f0`  | trigger edges        |
| `--surface`       | `#111621`  | node fills           |
| `--surface-2`     | `#171d2a`  | frame fills          |

Font: Excalidraw's `Nunito` at 24px for role labels, 16px for sub-captions.

---

## 1 · Shape list

### 1.1 Mac frame (single)

| id    | x   | y   | w    | h   | label                      | fill      | stroke (2px) |
| ----- | --- | --- | ---- | --- | -------------------------- | --------- | ------------ |
| `mac` | 80  | 80  | 1120 | 560 | `MacBook — dev@local` | surface-2 | `--muted`    |

Top-right corner carries a 10×10 solid dot in `--mac-local`.

### 1.2 Agent chips (inside the Mac)

Coordinates are canvas-absolute. Every chip has a 10% role-tinted fill
(`color-mix(role 14%, surface)`) and the role name in `--ink`.

| id          | x   | y   | w   | h   | label      | stroke            |
| ----------- | --- | --- | --- | --- | ---------- | ----------------- |
| `triager`   | 160 | 180 | 220 | 80  | `triager`  | `--role-triager`  |
| `builder-1` | 480 | 150 | 220 | 70  | `builder`  | `--role-builder`  |
| `builder-2` | 480 | 240 | 220 | 70  | `builder`  | `--role-builder`  |
| `builder-3` | 480 | 330 | 220 | 70  | `builder`  | `--role-builder`  |
| `reviewer`  | 800 | 200 | 220 | 70  | `reviewer` | `--role-reviewer` |
| `resolver`  | 800 | 300 | 220 | 70  | `resolver` | `--role-resolver` |
| `scout`     | 800 | 400 | 220 | 70  | `scout`    | `--role-scout`    |

Each chip carries a 6×6 `--mac-local` dot in its top-left — "runs on this Mac."

### 1.3 Worktrees row

A row of three thin worktree glyphs under the builder chips at
`(480, 440)` through `(700, 500)`, each labeled `wt-DEMO-N` in 14px
`--muted`. These exist to show "N worktrees" visually; no edges attach.

### 1.4 Edges — routing

Arrow heads: `--muted`, 1.4px, `marker-end: triangle` (filled).

| id      | from         | to          | shape         | note            |
| ------- | ------------ | ----------- | ------------- | --------------- |
| `t→b1`  | `triager`    | `builder-1` | gentle curve  | build route     |
| `t→b2`  | `triager`    | `builder-2` | straight      | build route     |
| `t→b3`  | `triager`    | `builder-3` | gentle curve  | build route     |
| `t→rev` | `triager`    | `reviewer`  | gentle curve  | review route    |
| `t→res` | `triager`    | `resolver`  | gentle curve  | decisions route |
| `t→sct` | `triager`    | `scout`     | gentle curve  | scout route     |
| `loop`  | `(1060,470)` | `triager`   | long arc back | reply queue     |

### 1.5 Trigger edges

| id         | from         | to          | stroke                      |
| ---------- | ------------ | ----------- | --------------------------- |
| `trig-b`   | _(off-left)_ | `builder-1` | `--accent`, dash 3-3, 1.4px |
| `trig-rev` | _(off-left)_ | `reviewer`  | `--accent`, dash 3-3, 1.4px |

Captions `inbox_idle · 2m` sit above each dashed arc in `--muted` 16px.

### 1.6 Captions

- Title (24px, `--ink`, top-left of mac frame): "Local-only · all on one Mac"
- Sub (16px, `--muted`): "N worktrees + N agents, no network"
- Legend (bottom-right, 12px): one `--mac-local` dot = "this Mac"
- Primitives footer (14px, `--muted`, below mac): `worktrees · inbox roles · routines · inbox_idle`
