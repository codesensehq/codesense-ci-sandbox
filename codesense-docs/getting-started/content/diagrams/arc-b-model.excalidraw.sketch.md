# arc-b-model.excalidraw — canvas sketch

Authoring spec for the follow-up PR that calls `apply_canvas` to
produce `arc-b-model.excalidraw` and `render_canvas` to produce
the sibling `arc-b-model.excalidraw.png`. Until those MCP tools land, the
shipped `arc-b-model/index.html` references the PNG and the diagrams-lint
is held off by the matching entry in `./.pending.yml`.

Export at **1600×1000** so the HTML hero's 16:10 frame math stays trivial.

---

## 0 · Canvas setup

| Prop                | Value                                                       |
| ------------------- | ----------------------------------------------------------- |
| viewBackgroundColor | `#0b0e14` (dark; a light re-export carries the same shapes) |
| gridSize            | 10                                                          |
| exportBackground    | true                                                        |
| exportPadding       | 32                                                          |
| viewportSize        | 1600 × 1000                                                 |

Palette tokens (match the arc chroma registered on `cs-docs-chip[data-arc="B"]`):

| Token             | Hex (dark) | Role                                 |
| ----------------- | ---------- | ------------------------------------ |
| `--arc-b`         | `#b48bff`  | arc-B accent — layer frames, chain   |
| `--layer-team`    | `#89a9ff`  | repo and team-chain bands (1, 3–5)   |
| `--layer-private` | `#f6b454`  | private band (layer 2)               |
| `--ink`           | `#e6ebf3`  | labels, strokes                      |
| `--muted`         | `#8a94a6`  | captions, path strings               |
| `--surface`       | `#111621`  | band fills                           |
| `--surface-2`     | `#171d2a`  | frame fills                          |

Font: Excalidraw's `Nunito` at 22px for layer labels, 14px for path captions.

---

## 1 · Shape list

### 1.1 Five horizontal bands (closest first; top is highest priority)

The bands are the places a file can come from when an agent launches in a
repo, in the order the resolver checks them, per file. The first band that
holds the file supplies it; folders are merged across bands.

Each band is 1200 wide, 120 tall, left-anchored at x=80. Vertical pitch is 150
(30px of gutter between bands). Captions sit inside each band at
`(x+24, y+28)` for the label and `(x+24, y+68)` for the on-disk path in muted
mono.

| id         | y   | stroke            | label                              | path                                                                  |
| ---------- | --- | ----------------- | ---------------------------------- | --------------------------------------------------------------------- |
| `layer-1`  | 80  | `--layer-team`    | `Repo content · only in that repo` | `<org>/<team>/repos/<host>/<owner>/<repo>/content/`                   |
| `layer-2`  | 230 | `--layer-private` | `Private · yours alone`            | `~/.codesense/users/<you>/scratch/orgs/<org>/  (+ all-orgs, inert only)` |
| `layer-3`  | 380 | `--layer-team`    | `Team content · default`           | `<org>/default/content/`                                              |
| `layer-4`  | 530 | `--layer-team`    | `Parent team · platform`           | `<org>/platform/content/`                                             |
| `layer-5`  | 680 | `--layer-team`    | `Its parent · root`                | `<org>/root/content/`                                                 |

Band fill: `surface` with a 10% stroke-colour tint (`color-mix(stroke 10%, surface)`).
Stroke width: 1.6px.

### 1.2 Priority arrow (left margin)

A single tapered arrow runs from top of `layer-1` to bottom of `layer-5` at
`x=56`. Head at the top, tail at the bottom (closest first). Stroke 2px in
`--arc-b`; label `"closest wins, per file"` at the top in 12px `--muted`, rotated −90°.

### 1.3 Team-chain ribbon (right margin)

A vertical ribbon with three stacked chips at `x=1320` showing the sample
team chain. Chips are 220 wide, 60 tall, pitch 80 (gutter 20).

| id            | y    | label             | caption                       |
| ------------- | ---- | ----------------- | ----------------------------- |
| `chain-root`    | 80   | `root`            | `top of the chain`            |
| `chain-platform`| 240  | `platform`        | `parent_team: root`           |
| `chain-default` | 400  | `default · active`| `parent_team: platform`       |

Dashed arrow (stroke-dasharray 4 3, `--arc-b`) walks downward through the
chips: `chain-default` → `chain-platform` → `chain-root` (nearest-first on
the chain, mirroring the band arrow on the left).

### 1.4 Edges tying the chain into the bands

Thin 1.2px `--muted` arcs connect each chip to the band it supplies:

- `chain-default` → right edge of `layer-1` (its repo content) and `layer-3` (its team content)
- `chain-platform` → right edge of `layer-4`
- `chain-root` → right edge of `layer-5`

Visual beat: a team's context is its own content plus its parents'. The repo
band belongs to the active team only, and the private band belongs to you
alone (no chain edge touches `layer-2`).

### 1.5 Footer caption

Bottom-centre caption at `(800, 940)` in 14px `--muted`:
`Resolved per file · closest wins · an agent's context picks what a launch copies`

---

## 2 · Export + sibling PNG

Export at exactly 1600×1000 with `exportBackground: true` and
`exportPadding: 32`. The HTML uses `object-fit: contain` inside a 16:10
`.cs-docs-card.diag`, so the PNG lands dead-centred without further math.
The light re-export carries the same shapes and the same ids; palette
tokens flip in the Excalidraw theme toggle.

---

## 3 · When to drop the pending entry

As soon as `arc-b-model.excalidraw` + `arc-b-model.excalidraw.png` land in
this folder via `apply_canvas` / `render_canvas`, delete the
`- arc-b-model` line from `./.pending.yml`. The diagrams lint falls back
to the strict pair-check on the next build.
