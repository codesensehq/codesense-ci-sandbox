# inbox-flows/one-shot.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `one-shot.excalidraw` + `render_canvas` →
`one-shot.excalidraw.png`. The shipped `inbox-flows.html` renders the same
shape via an inline SVG stand-in until the MCP tools land. Export at
**1280×720**.

See `./dispatch.excalidraw.sketch.md` §0 for the canvas setup, palette tokens,
and font — identical across every inbox-flows diagram.

---

## 1 · Shape list

### 1.1 Nodes

Coordinates are canvas-absolute.

| id        | x   | y   | w   | h   | label                     | fill    | stroke (1.5px)   | shape                   |
| --------- | --- | --- | --- | --- | ------------------------- | ------- | ---------------- | ----------------------- |
| `ci`      | 120 | 280 | 160 | 160 | `CI`                      | surface | `--muted`        | circle                  |
| `routine` | 420 | 280 | 280 | 160 | `routine`<br/>`cron · 1m` | surface | `--role-builder` | rect, dashed (dash 4-3) |
| `me`      | 840 | 280 | 320 | 160 | `me`<br/>`(my inbox)`     | surface | `--ink`          | rect                    |

### 1.2 Edges — routing

| id     | from      | to        | shape    | caption (above, 14px, `--muted`) |
| ------ | --------- | --------- | -------- | -------------------------------- |
| `ci→r` | `ci`      | `routine` | straight | `poll`                           |
| `r→me` | `routine` | `me`      | straight | `ping (once)`                    |

### 1.3 No trigger edge (the routine IS the trigger)

The routine's dashed border carries the trigger semantics — the explicit
dashed arc used in the other three diagrams is intentionally absent here to
reinforce that this shape has no inbox-driven queue.

### 1.4 Captions

- Title (24px, `--ink`): "04 · One-shot notify"
- Sub (16px, `--muted`): "smallest possible shape — no queue, no triage"
- Primitives footer (14px, `--muted`): `inbox messages · role queues · routines · inbox_idle`
