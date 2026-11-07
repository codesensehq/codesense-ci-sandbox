# inbox-flows/hitl.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `hitl.excalidraw` + `render_canvas` →
`hitl.excalidraw.png`. The shipped `inbox-flows.html` renders the same shape
via an inline SVG stand-in until the MCP tools land. Export at **1280×720**.

See `./dispatch.excalidraw.sketch.md` §0 for the canvas setup, palette tokens,
and font — identical across every inbox-flows diagram.

---

## 1 · Shape list

### 1.1 Nodes

Coordinates are canvas-absolute.

| id         | x    | y   | w   | h   | label                | fill    | stroke (1.5px)    |
| ---------- | ---- | --- | --- | --- | -------------------- | ------- | ----------------- |
| `inbox-1`  | 120  | 240 | 240 | 70  | `incoming`           | surface | `--muted`         |
| `inbox-2`  | 136  | 320 | 240 | 70  | _(no label — stack)_ | surface | `--muted`         |
| `inbox-3`  | 152  | 400 | 240 | 70  | _(no label — stack)_ | surface | `--muted`         |
| `resolver` | 520  | 300 | 400 | 160 | `resolver`           | surface | `--role-resolver` |
| `reply`    | 1040 | 220 | 200 | 70  | `reply`              | surface | `--role-builder`  |
| `handoff`  | 1040 | 420 | 200 | 70  | `handoff`            | surface | `--role-scout`    |

The three `inbox-*` nodes stack with a 16px vertical offset per layer to read
as "a pile of messages." The `resolver` node is a rounded-pill rectangle
(60px corner radius) tagged `(human-held)` in a 14px `--muted` sub-caption.

### 1.2 Edges — routing

| id           | from       | to         | shape    | note                         |
| ------------ | ---------- | ---------- | -------- | ---------------------------- |
| `in→resv`    | `inbox-1`  | `resolver` | straight | message enters               |
| `resv→reply` | `resolver` | `reply`    | straight | human sends reply            |
| `resv→hand`  | `resolver` | `handoff`  | straight | human routes to another role |

### 1.3 Trigger edge

| id          | from      | to         | shape                      | stroke                      |
| ----------- | --------- | ---------- | -------------------------- | --------------------------- |
| `trig-idle` | _(below)_ | `resolver` | long dashed arc from under | `--accent`, dash 3-3, 1.4px |

The arc loops from `(720, 560)` back up to `(720, 460)`; a caption
`inbox_idle · 30m → nudge` sits at `(600, 600)`.

### 1.4 Captions

- Title (24px, `--ink`): "02 · Human-in-the-loop queue"
- Sub (16px, `--muted`): "one pair of hands · routine nudges when stale"
- Primitives footer (14px, `--muted`): `inbox messages · role queues · routines · inbox_idle`
