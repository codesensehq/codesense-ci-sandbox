# factory/overnight.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `overnight.excalidraw` +
`render_canvas` → `overnight.excalidraw.png`. Shipped
`arc-l-factory/running-overnight.html` renders the same shape as an
inline SVG stand-in in the same coordinate space.

Export at **1280×720**.

See `./local-only.excalidraw.sketch.md` §0 for canvas setup, palette
tokens, and font — identical across every factory diagram.

Additional palette entry for this diagram:

| Token     | Hex (dark) | Role             |
| --------- | ---------- | ---------------- |
| `--night` | `#1a2234`  | night frame fill |
| `--moon`  | `#d2d6e0`  | moon glyph       |

---

## 1 · Shape list

### 1.1 Night frame

A single rounded rectangle filling the canvas inside the standard
padding: `(60, 60) → (1220, 660)`, fill `--night`, no stroke. A small
8×8 `--moon` disc sits at `(1180, 100)` with a 4-pixel notch so it reads
as a moon.

### 1.2 Timeline (horizontal axis)

A thin line at `y = 540` from `x = 120` to `x = 1180` in `--muted`,
1px, with vertical tick marks every 100px.

Tick captions (14px, `--muted`, below ticks):
`22:00  00:00  02:00  04:00  06:00  08:00`

### 1.3 Routine triggers (dashed arcs across the night)

Six `inbox_idle` ticks fire across the night at even intervals. Each
one is a dashed arc rising from the timeline to a role column at
`y = 220` to `y = 420`.

| id    | tick-x | target column | role     | time  |
| ----- | ------ | ------------- | -------- | ----- |
| `t-1` | 220    | 300           | builder  | 22:30 |
| `t-2` | 360    | 460           | reviewer | 00:00 |
| `t-3` | 500    | 620           | builder  | 01:30 |
| `t-4` | 640    | 780           | resolver | 03:00 |
| `t-5` | 780    | 940           | builder  | 04:30 |
| `t-6` | 920    | 1100          | reviewer | 06:00 |

Each arc is `--accent`, 1.4px, dash 3-3; a small 14px label `inbox_idle`
sits above each arc at mid-rise.

### 1.4 Role columns

Short vertical labels at the arc endpoints (24px, in role chroma):

| x    | y   | label           | color             |
| ---- | --- | --------------- | ----------------- |
| 300  | 200 | `builder wake`  | `--role-builder`  |
| 460  | 200 | `reviewer wake` | `--role-reviewer` |
| 620  | 200 | `builder wake`  | `--role-builder`  |
| 780  | 200 | `resolver wake` | `--role-resolver` |
| 940  | 200 | `builder wake`  | `--role-builder`  |
| 1100 | 200 | `reviewer wake` | `--role-reviewer` |

### 1.5 Human sleep glyph

A soft dashed horizontal pill at `(120, 440) → (1180, 460)` in
`--muted`, dash 4-4, label `human: asleep` (14px, `--muted`) centered.

### 1.6 Morning summary card

A small rounded rectangle at `(960, 120) → (1180, 200)`, fill
`--surface-2`, stroke `--muted`, labeled `morning summary` with two
sub-lines `+5 PRs opened` / `3 escalations queued` (12px, `--muted`).

### 1.7 Captions

- Title (24px, `--ink`, top-left): "Running overnight · hands-off loop with inbox_idle wake"
- Sub (16px, `--muted`): "no humans in the hot path · morning summary waits"
- Primitives footer (14px, `--muted`, bottom): `routines · inbox_idle · worktrees · agent-dag · morning summary`
