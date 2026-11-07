# inbox-flows/dispatch.excalidraw — canvas sketch

Authoring spec for the follow-up PR that calls `apply_canvas` to
produce `dispatch.excalidraw` and `render_canvas` to produce the
sibling `dispatch.excalidraw.png`. Until those MCP tools land, the shipped
`inbox-flows.html` renders the same shape via an inline SVG stand-in (same
coordinate space, so swapping to the PNG is a no-layout change).

Shape ids match `data-shape` attributes in `inbox-flows.html` where applicable.
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

Palette tokens (match the HTML's role chroma — the mock mirrors these):

| Token             | Hex (dark) | Role                 |
| ----------------- | ---------- | -------------------- |
| `--role-triager`  | `#6ea8f0`  | triager              |
| `--role-builder`  | `#4ec28a`  | builder              |
| `--role-reviewer` | `#a898ff`  | reviewer             |
| `--role-scout`    | `#e0ac55`  | scout                |
| `--ink`           | `#e6ebf3`  | labels, node strokes |
| `--muted`         | `#8a94a6`  | edges, badges        |
| `--accent`        | `#6ea8f0`  | trigger edges        |
| `--surface`       | `#111621`  | node fills           |
| `--surface-2`     | `#171d2a`  | frame fills          |

Font: Excalidraw's `Nunito` at 24px for role labels, 16px for sub-captions.

---

## 1 · Shape list

### 1.1 Nodes (role chips)

Coordinates are canvas-absolute.

| id          | x   | y   | w   | h   | label      | fill    | stroke (1.5px)    |
| ----------- | --- | --- | --- | --- | ---------- | ------- | ----------------- |
| `triager`   | 240 | 300 | 240 | 90  | `triager`  | surface | `--role-triager`  |
| `builder-1` | 760 | 120 | 240 | 70  | `builder`  | surface | `--role-builder`  |
| `builder-2` | 760 | 230 | 240 | 70  | `builder`  | surface | `--role-builder`  |
| `builder-3` | 760 | 340 | 240 | 70  | `builder`  | surface | `--role-builder`  |
| `reviewer`  | 760 | 460 | 240 | 70  | `reviewer` | surface | `--role-reviewer` |
| `scout`     | 760 | 570 | 240 | 70  | `scout`    | surface | `--role-scout`    |

Each node carries a 10% tinted fill (`color-mix(role 14%, surface)`) and the
role name in `--ink`. A `machine` badge sits in the top-left corner as a 6×6
dot (`--muted` by default; the HTML's Local⇄Remote toggle recolors it live).

### 1.2 Edges — routing

Arrow heads: `--muted`, 1.4px, `marker-end: triangle` (filled, no outline).

| id          | from      | to          | shape                     | note                      |
| ----------- | --------- | ----------- | ------------------------- | ------------------------- |
| `t→b1`      | `triager` | `builder-1` | gentle curve, bezier      | "build" route             |
| `t→b2`      | `triager` | `builder-2` | straight                  | "build" route             |
| `t→b3`      | `triager` | `builder-3` | gentle curve, bezier      | "build" route             |
| `t→rev`     | `triager` | `reviewer`  | gentle curve, bezier      | "review" route            |
| `t→sct`     | `triager` | `scout`     | gentle curve, bezier      | "scout" route             |
| `loop-back` | _(side)_  | `triager`   | long arc under the column | reply queue — closed loop |

The `loop-back` edge wraps underneath the right column from `(1000, 580)` to
`(240, 345)` and demonstrates the closed loop: replies from workers return to
the triager, never to a human.

### 1.3 Trigger edge

| id          | from         | to        | shape                        | stroke                      |
| ----------- | ------------ | --------- | ---------------------------- | --------------------------- |
| `trig-idle` | _(off-left)_ | `triager` | dashed arc from `(140, 345)` | `--accent`, dash 3-3, 1.4px |

Caption `inbox_idle · 2m` sits above the arc at `(140, 300)` in `--muted` 16px.

### 1.4 Captions

- Title (24px, `--ink`, top-left): "01 · Dispatch factory"
- Sub (16px, `--muted`): "closed loop · no humans in hot path"
- Primitives footer (14px, `--muted`, bottom): `inbox messages · role queues · routines · inbox_idle`
