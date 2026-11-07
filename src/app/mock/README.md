# Mock backend (frontend-only dev mode)

`pnpm dev:mock` boots the real React app in a plain browser with **no Rust
backend**. Every `@tauri-apps/*` call is served by an in-memory fake installed
behind a single seam — `window.__TAURI_INTERNALS__` (see `install.ts`) — so the
app becomes a clickable, stateful surface on fixture data without a `tauri dev`
compile-and-spawn cycle.

It exists so an agent or developer can build and validate **UI** stories quickly
and cheaply, proving "this is the UI we want" before committing to backend or
component work.

## How it works

- `install.ts` plants `window.__TAURI_INTERNALS__` first thing in `main.tsx`'s
  `MODE === "mock"` block — before React renders — so the api's synchronous
  metadata reads (`getCurrentWindow()`) and the first `invoke` both find a shim.
- `mock-router.ts` routes every `invoke(cmd, args)`:
  - a **business command** (`load_orgs`, `create_team`, …) dispatches to a
    handler, or throws a loud `not mocked: <command>` if none is registered;
  - a **plugin surface** (`plugin:window|…`, `plugin:event|listen`, …) is an
    inert no-op — those are Tauri runtime boundaries, not backend logic.
- `mock-state.ts` holds one session-scoped in-memory store; `handlers/*` wire
  each collection surface to it. State persists until page reload.

The mock is **demand-driven**: hit the loud stub, add one fixture in the
relevant `handlers/*` file, move on. Seed shapes can be lifted from the existing
`vi.mock("@/tauri/commands")` test fixtures.

## DTO contract — the reimagined sync-viewer publish set

`git-sync-fixtures.ts` (served by `handlers/git-sync.ts`) is **FE-first: the fixture
shape IS the contract the real backend must satisfy**, not the other way around.
The reimagined sync viewer is built and proven against it on `pnpm dev:mock`
before any real backend command exists. Treat a field rename here as a contract
change.

`get_publish_set` returns the **real bucketed `PublishSet` wire shape** —
`{ buckets: [{ workspaceRef: { id, label, kind: "story" | "epic" | "general" },
count, files }], general: { count, files } }` — with real `PublishFile` fields
(`path`, `status` ∈ `added` | `modified` | `deleted`, `badges?`, `renamedFrom?`).
A **rename is not a status**: it is a
`modified`/`added` file with `renamedFrom` set and a `"rename"` badge.

Layered on top, the fixture defines these **net-new facets as the backend
obligation** (they do not exist on the backend yet):

- per file — `scope: "team" | "personal" | "loose"` (repo identity, not a path
  guess) and `contentRole: "agent-context" | "human-doc" | "config" | "binary"` (retires the "Documentation" catch-all);
- top-level — `counts: { team, personal, loose, byRole }`, so the summary
  header reads O(1) without walking rows;
- personal set only — a `personalSurface: "notes" | "dashboards" |
"content-overlay"` facet.

The Scratch space tab's `list_scratch_space` returns the real
`ScratchSpaceListing` shape (one file and two all-orgs items, one of
which no org reads); `create_scratch_file` echoes the new file's path.

The read-only reimagine sources the working-tree set by `repoPath`; the real
command's PR/commit source axes (`workspaceRef` / `prNumber` / `commitSha`) are a
**non-goal** here and are ignored.

Scenarios are keyed by repo path (`FIXTURE_ROOTS`): the two real roots plus the
pathological cases downstream work needs — 3-file baseline, 3,000-file (blank-bug
repro), zero-change, everything-loose, and the team side of empty-team /
full-personal (paired with the personal root).

## DTO contract — Remote Machines

`handlers/machines.ts` answers the machine registry commands, which are real
(`commands.ts`, "Remote Machines": `list_machines`, `list_ssh_sources`,
`capture_machine_passphrase`, `test_machine_connection`, `install_terminal_server`,
`save_machine`, `set_machine_hidden`, `unshare_machine`, `remove_machine`), and the
**draft commands** still in `src/tauri/draft-commands.ts` (no Rust handler yet;
each wrapper names the wiring story — SW-RM-4/5/6 — that implements it). The
shapes are the contract: `MachineSummary` (declaration + personal binding +
`MachineStatus`), `MachineLayoutSnapshot`, the Add-machine `MachineTestReport` /
`InstallTerminalServerResult`, and `machine_fs_list` / `machine_fs_read`. When a
wiring story lands, the wrapper moves into `commands.ts`; the handler here stays
as the `dev:mock` answer.

`?machines=` picks the world: `none` (default — This Mac only, the zero-friction
floor), `shared` (the team shares two machines you haven't set up), `demo` (one
machine per status state, personal + shared). Test hosts in the Add-machine flow
steer the checklist: `timeout`, `root@…`, `mitm`, `…bsd`, `full`, `flaky-vm`, and
`logind…` (an install that warns about logout). A key path containing `cancel`
plays a dismissed passphrase dialog.

## Agents world

`?agents=team` boots a team at work in the bottom CLI workspace (`handlers/agents-team.ts`):
a Manager agent in the team content folder, a story lead with three workers (one parked on a
question, delivered as the real `mcp:ask-user`), a second story's lead with a worker waiting on
approval, a shell-only scratch folder and one tabless background agent. `&manager=none`
leaves the Manager out; `?agents=none` runs no agents at all. A mock walk can
launch another agent or put a question with `window.__mockAgents.addSession(row)` and
`window.__mockAgents.ask(request)`.

`?agents=teams` is that world plus a second open team in a second org (Abacus Labs ›
riverside): its content folder (a working handbook editor, a ready backlog groomer) and an SV-3
story workspace (a working lead, its working mockup worker and a ready reviewer), so the Agents
pane lists two teams as it does for someone in more than one.

## Isolated slot harness

`/?harness=slots` bypasses App initialization. Its mock entry in `main.tsx`
prepares preferences through the canonical reader before rendering `SlotHarness`:

```ts
await readPrefsState();
if (!usePrefsStore.getState().ownedReady)
  throw new Error("Slot harness requires an active local profile");
```

The reader resolves the actual renderer profile and awaits the prepared snapshot,
not live subscription delivery. The existing prefs singleton lives for this
isolated document; its replacement/reset behavior is unchanged, and navigation
tears down the renderer. No second component lifecycle binding is needed.
A missing profile or failed preparation is reported without rendering the harness
or its Ready marker. Tests wait for `slot-harness-ready` before mounting or advancing
the pane clock; fallback view keys still come from the real dashboard policy.
This bootstrap supports mock-browser checks only; it does not prove native or
provider behavior or establish the cause of an earlier failed run.

## What mock mode does NOT prove

Mock mode is a UI dev/validation harness, **not** integration coverage. It does
not exercise, and must not be trusted for:

- **Live terminal / PTY output** — the terminal pane renders inert; there is no
  shell. (A later cosmetic-terminal tier could add canned output.)
- **Real git operations** — no clone, commit, sync, or conflict resolution runs.
- **Real agent execution** — no agent is spawned; runs are static fixtures.
- **Live event motion** — `listen` is stubbed to a no-op, so nothing streams: a
  run does not progress, an agent does not reply, sync waves do not tick. Only
  static seeded states (Tier 1) are represented.
- **Asset URLs** — `convertFileSrc` returns the raw path rather than a real
  `asset://` URL, so any image/asset rendered through it will not load. UI that
  depends on displaying real file assets is out of scope for the mock.

Backend behavior remains owned by the real-app WDIO harness in
`16-app-quality-harness`. If you need to prove the backend does something, use
that — not mock mode.

## Production safety

The installer refuses to run unless Vite's statically replaced mode is `mock`
(or `test` under Vitest). Thus these handlers cannot replace Tauri IPC in a
production or webdriver build even if a future import accidentally calls it.

Mock code is referenced only inside a statically-false `MODE === "mock"` branch,
so Vite/Rollup tree-shakes the entire `src/app/mock/` graph out of the production
and `webdriver` bundles. `scripts/verify-webdriver-absence.ts` (`pnpm
verify:webdriver:absence`) — which builds a fresh production bundle and already
runs in CI — scans `dist/` for mock symbols alongside the webdriver ones and
fails the build if any reach the shipped bundle.
