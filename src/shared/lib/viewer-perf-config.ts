// SPDX-License-Identifier: MIT
/**
 * Single source of truth for performance tunables in the viewer layer.
 *
 * Every magic number read by `file-content-cache.ts` and the (stretch)
 * MarkdownViewer keep-alive logic lives in this file. Changing one line
 * here propagates everywhere; no grep-and-replace across call sites.
 *
 * These are internal tunables, not user settings — users don't benefit
 * from poking at cache sizes.
 */
export const VIEWER_PERF = {
  /** Shared cap for bridged frames in slots and centre viewers; pinned frames may exceed it. */
  maxBridgedFrames: 16,
  /** Max entries in the file-content LRU cache. Evict least-recently-touched on overflow. */
  fileCacheMaxEntries: 25,
  /**
   * Max MarkdownViewer instances kept mounted (active + hidden) for fast
   * tab-switch. Bumped from an initial 5 to 10 after live-use feedback —
   * at 10, every recently-viewed file re-opens with zero re-parse /
   * re-highlight cost, and per-viewer memory (~1–2 MB of JS + DOM for
   * typical docs) stays well inside desktop-app budgets.
   */
  keepAliveMaxViewers: 10,
  /**
   * Route markdown parsing (`remark-parse` / `remark-gfm` /
   * `remark-frontmatter` / `remark-rehype` / `rehype-highlight`) through
   * a Web Worker (`src/shared/lib/markdown-worker/`) so the synchronous
   * tokenization pass in `rehype-highlight` can't block the main thread.
   * Main-thread stays responsive to keyboard + scroll + clicks while a
   * cold file is parsing; only the lightweight hast → React elements
   * step happens on main via `hast-util-to-jsx-runtime`. Flip to `false`
   * to fall back to the synchronous `<ReactMarkdown>` path if a
   * regression surfaces.
   */
  parseInWorker: true,
  /**
   * Viewer render strategy.
   *
   * - `"blocks"` (default) — main-thread skips `hast-util-to-jsx-runtime`
   *   entirely. The worker emits a pre-split `Block[]` (islands for code
   *   fences and mermaid, `dangerouslySetInnerHTML`-ready HTML for
   *   everything else), and a post-commit `useEffect` mounts React
   *   islands via `createRoot` on `data-island-id` placeholders. Eliminates
   *   the 35-46ms main-thread JSX walk story 32.5 measured on ~62 KB files,
   *   and pairs with `parseResultCache` for a sub-millisecond repeat-open
   *   path.
   * - `"jsx"` — rollback to the 32.4 path (hast → `toJsxRuntime` on main).
   *   Retained so flipping one line restores identical 32.4 behavior if a
   *   regression surfaces; there is no other functional reason to prefer
   *   it. Requires `parseInWorker: true` to make sense; if both flags are
   *   off, the viewer falls back to the synchronous `<ReactMarkdown>` path
   *   (test default via `src/test-setup.ts`).
   */
  renderMode: "blocks" as "jsx" | "blocks",
  /**
   * SW-FX-48: run the commit graph's layout in a Web Worker instead of
   * synchronously on the main thread (`use-async-graph-layout`), so a large
   * graph never freezes the WebKit renderer. Flip to `false` to fall back to the
   * synchronous main-thread layout if a regression surfaces (also the automatic
   * path where `Worker` is unavailable, e.g. tests via `src/test-setup.ts`).
   */
  layoutInWorker: true,
  /**
   * SR-0 (Q3): trace the `sync-wave-completed` FE reaction. OFF by default —
   * a pure measurement flag for the SR-0 scale spike, never on in normal use.
   * When on, {@link traceWaveReaction} wraps each wave reaction in
   * `performance.mark`/`measure` and emits a `wave_id`-keyed instance-count
   * delta (`[sr0-wave-reaction] …`) to the console so the soak harness (driving
   * the dev build via Playwright) can correlate a wave to native-backing
   * allocation. Adds zero synchronous work to any listener when off; when on,
   * only a cheap snapshot + a deferred second snapshot. The aggregate RAM
   * ratchet is read separately from the Rust `memory` line's `fe_*` fields
   * (no FE code needed) — this flag is for the per-wave mechanism only.
   */
  traceWaveReaction: false,
} as const;
