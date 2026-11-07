# factory/multi-mac.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `multi-mac.excalidraw` +
`render_canvas` → `multi-mac.excalidraw.png`. Shipped
`arc-l-factory/multi-mac-distributed.html` renders the same shape as an
inline SVG stand-in in the same coordinate space.

Export at **1280×720**.

See `./local-only.excalidraw.sketch.md` §0 for canvas setup, palette
tokens, and font — identical across every factory diagram.

Additional palette entries for the per-developer tint:

| Token         | Hex (dark) | Role         |
| ------------- | ---------- | ------------ |
| `--dev-alice` | `#4ec28a`  | Alice's Macs |
| `--dev-bob`   | `#a898ff`  | Bob's Macs   |
| `--dev-carol` | `#e0ac55`  | Carol's Macs |

---

## 1 · Shape list

### 1.1 Mac frames (three, side-by-side)

| id      | x   | y   | w   | h   | label                    | stroke (2px)  |
| ------- | --- | --- | --- | --- | ------------------------ | ------------- |
| `mac-a` | 60  | 120 | 370 | 500 | `MacBook — alice@mobile` | `--dev-alice` |
| `mac-b` | 455 | 120 | 370 | 500 | `MacBook — bob@desk`     | `--dev-bob`   |
| `mac-c` | 850 | 120 | 370 | 500 | `MacBook — carol@laptop` | `--dev-carol` |

Each frame carries a 10×10 solid dot in its stroke color in the top-right.

### 1.2 Agent chips — per Mac

Every chip carries a 6×6 dot in its owning Mac's tint in its top-left.

| id            | x   | y   | w   | h   | label      | stroke (role chroma) |
| ------------- | --- | --- | --- | --- | ---------- | -------------------- |
| `triager`     | 80  | 200 | 330 | 70  | `triager`  | `--role-triager`     |
| `a-builder-1` | 80  | 300 | 330 | 60  | `builder`  | `--role-builder`     |
| `a-resolver`  | 80  | 400 | 330 | 60  | `resolver` | `--role-resolver`    |
| `b-builder-1` | 475 | 200 | 330 | 60  | `builder`  | `--role-builder`     |
| `b-builder-2` | 475 | 290 | 330 | 60  | `builder`  | `--role-builder`     |
| `b-reviewer`  | 475 | 400 | 330 | 60  | `reviewer` | `--role-reviewer`    |
| `c-scout`     | 870 | 200 | 330 | 60  | `scout`    | `--role-scout`       |
| `c-builder-1` | 870 | 290 | 330 | 60  | `builder`  | `--role-builder`     |
| `c-reviewer`  | 870 | 400 | 330 | 60  | `reviewer` | `--role-reviewer`    |

### 1.3 Shared team repo pill

A pill at `(540, 580)` size `200×36`, label `team repo (git)`, fill
`--surface`, stroke `--muted`. Three thin edges (1.0px, `--muted`,
dash 4-4) run from each Mac frame's bottom edge to the pill, labeled
`pull/push` in 12px.

### 1.4 Edges — routing between agents

Arrow heads: `--muted`, 1.4px.

| id        | from      | to            | shape               | note            |
| --------- | --------- | ------------- | ------------------- | --------------- |
| `t→a-b`   | `triager` | `a-builder-1` | straight            | local to Alice  |
| `t→b-b1`  | `triager` | `b-builder-1` | arc across (bridge) | to Bob          |
| `t→b-b2`  | `triager` | `b-builder-2` | arc across          | to Bob          |
| `t→b-rev` | `triager` | `b-reviewer`  | arc across          | to Bob          |
| `t→c-s`   | `triager` | `c-scout`     | arc across          | to Carol        |
| `t→c-b`   | `triager` | `c-builder-1` | arc across          | to Carol        |
| `t→c-rev` | `triager` | `c-reviewer`  | arc across          | to Carol        |
| `t→a-r`   | `triager` | `a-resolver`  | straight            | local decisions |

Every cross-Mac arc crosses a vertical dotted guide at `x=440` and
`x=835` (the inter-frame gaps) tagged with a tiny label `private-account`
in 11px `--muted`.

### 1.5 Captions

- Title (24px, `--ink`, top): "Multi-mac distributed · several developers' Macs cooperating"
- Sub (16px, `--muted`): "one team context repo, N Macs, N developers"
- Legend (bottom-right, 12px): three colored dots = Alice, Bob, Carol
- Primitives footer (14px, `--muted`): `teams · worktrees · inbox roles · private-account · team repo`
