# inbox-flows/sdlc.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `sdlc.excalidraw` + `render_canvas` →
`sdlc.excalidraw.png`. The shipped `inbox-flows.html` renders the same shape
via an inline SVG stand-in until the MCP tools land. Export at **1280×720**.

See `./dispatch.excalidraw.sketch.md` §0 for the canvas setup, palette tokens,
and font — identical across every inbox-flows diagram.

---

## 1 · Shape list

### 1.1 Nodes

Coordinates are canvas-absolute.

| id         | x   | y   | w   | h   | label      | fill    | stroke (1.5px)    |
| ---------- | --- | --- | --- | --- | ---------- | ------- | ----------------- |
| `scout`    | 60  | 300 | 260 | 90  | `scout`    | surface | `--role-scout`    |
| `builder`  | 440 | 300 | 260 | 90  | `builder`  | surface | `--role-builder`  |
| `reviewer` | 820 | 300 | 260 | 90  | `reviewer` | surface | `--role-reviewer` |
| `resolver` | 560 | 520 | 260 | 90  | `resolver` | surface | `--role-resolver` |

### 1.2 Edges — routing

| id         | from       | to         | shape            | note                  |
| ---------- | ---------- | ---------- | ---------------- | --------------------- |
| `scout→b`  | `scout`    | `builder`  | straight         | forward               |
| `b→rev`    | `builder`  | `reviewer` | straight         | forward               |
| `b→resv`   | `builder`  | `resolver` | down-right curve | escalate on ambiguity |
| `rev→resv` | `reviewer` | `resolver` | down-left curve  | escalate on ambiguity |

Caption "escalate on ambiguity" (14px, `--muted`) sits at `(620, 460)` above
the resolver, between the two escalation hops.

### 1.3 Trigger edges

| id         | from         | to                       | shape                      | stroke                      |
| ---------- | ------------ | ------------------------ | -------------------------- | --------------------------- |
| `trig-adv` | _(off-left)_ | `builder` AND `reviewer` | dashed arc from `(0, 345)` | `--accent`, dash 3-3, 1.4px |

Caption `inbox_idle · 1m` at `(40, 220)`.

### 1.4 Captions

- Title (24px, `--ink`): "03 · SDLC pipeline"
- Sub (16px, `--muted`): "linear with an escalation hop"
- Primitives footer (14px, `--muted`): `inbox messages · role queues · routines · inbox_idle`
