// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useState, useEffect, useLayoutEffect, useRef, useMemo, memo, useCallback } from "react";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { createRoot, type Root } from "react-dom/client";
import { Copy, Check } from "lucide-react";
import { InfoTip } from "@/components/ui/tooltip";
import { FileDeletedPlaceholder } from "@/shared/components/file-deleted-placeholder";
import { resolveViewerLinkPath, type CommonViewerProps } from "@/shared/lib/viewer-file-type";
import { useEffectiveRefreshKey } from "@/shared/hooks/use-effective-refresh-key";
import ReactMarkdown from "react-markdown";
import { VIEWER_REMARK_PLUGINS, VIEWER_REHYPE_PLUGINS } from "@/shared/lib/markdown-render-plugins";
import { toJsxRuntime, type Components } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { Root as HastRoot } from "hast";
import mermaid from "mermaid";
import { useTheme } from "next-themes";
import { logger } from "@/shared/lib/logger";
import { readFile } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { closeActiveFileTab } from "@/shared/lib/close-active-file-tab";
import { isFileMissingError } from "@/shared/lib/file-missing-error";
import { SCROLL_CONTAINER_STYLE, useViewerChrome, useViewerScrollKeys } from "./viewer-chrome";
import {
  useDiagramContainer,
  useSelfClearingRef,
  removeMermaidOrphan,
} from "./use-diagram-container";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { useScrollSnapshot } from "@/shared/hooks/use-scroll-snapshot";
import { parseMarkdown, parseToBlocks } from "@/shared/lib/markdown-worker/client";
import type { Block } from "@/shared/lib/markdown-worker/types";
import { parseResultCache } from "@/shared/lib/parse-result-cache";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { CsQueryIsland } from "./csquery/cs-query-view";
import morphdom from "morphdom";

// F.34: morphdom options — hoisted to module scope to avoid per-block
// closure allocation. `onBeforeElUpdated` preserves diagram island
// placeholders (managed by createRoot) and skips structurally-equal
// subtrees (cheap pre-checks before the deep `isEqualNode` comparison).
// Threshold above which `isEqualNode` (deep O(n) comparison) is skipped
// in favour of letting morphdom's own diff handle it. Avoids double-walking
// large subtrees (e.g. 200-row tables) that would exceed the 16ms frame budget.
const MORPHDOM_ISEQUAL_THRESHOLD = 50;

const MORPHDOM_OPTS: Parameters<typeof morphdom>[2] = {
  childrenOnly: true,
  onBeforeElUpdated(fromEl, toEl) {
    if (fromEl.hasAttribute("data-island-id")) return false;
    if (fromEl.attributes.length !== toEl.attributes.length) return true;
    if (fromEl.childNodes.length !== toEl.childNodes.length) return true;
    // Skip the expensive deep comparison on large subtrees — morphdom's
    // own diff is cheaper than isEqualNode + morphdom double-walk.
    if (fromEl.childNodes.length > MORPHDOM_ISEQUAL_THRESHOLD) return true;
    return !fromEl.isEqualNode(toEl);
  },
};

// Mermaid static config — set once at module import. Seeds the theme
// from the DOM's dark-class so the first diagram render doesn't flash
// with the wrong theme before the theme-subscribing effect runs.
// `RepoViewerView` owns the theme-reapply effect so all mounted viewers
// share one subscription instead of fanning out N identical calls.
mermaid.initialize({
  startOnLoad: false,
  securityLevel: "strict",
  // @ts-expect-error suppressErrors exists at runtime but is missing from type defs
  suppressErrors: true,
  theme:
    typeof document !== "undefined" && document.documentElement.classList.contains("dark")
      ? "dark"
      : "default",
});

/**
 * Wraps `<pre>` with a copy button. Used only by the `"jsx"` and sync
 * render paths (the `pre` override in the components map). The
 * `"blocks"` path leaves code fences as inert HTML and handles copy
 * via delegated `onClick` on the scroll container — no React island,
 * no `createRoot`, no per-fence mount cost.
 */
function CopyableCodeBlock({ children, ...props }: React.ComponentPropsWithRef<"pre">) {
  const preRef = useRef<HTMLPreElement>(null);
  const { copiedValue, handleCopy } = useCopyToClipboard();
  const copied = copiedValue !== null;
  const handleClick = () => {
    const codeEl = preRef.current?.querySelector("code");
    const lines = codeEl?.querySelectorAll(".code-line");
    let text: string;
    if (lines && lines.length > 0) {
      text = Array.from(lines)
        .map((l) => l.textContent ?? "")
        .join("\n");
    } else {
      text = codeEl?.textContent ?? preRef.current?.textContent ?? "";
    }
    handleCopy(text.trimEnd());
  };

  return (
    <div className="relative group/code">
      <pre ref={preRef} {...props}>
        {children}
      </pre>
      <InfoTip label="Copy code">
        <button
          className="absolute top-2 right-2 p-1.5 rounded-md bg-card/90 border border-border/50 opacity-0 group-hover/code:opacity-100 transition-opacity shadow-sm"
          onClick={handleClick}
          aria-label="Copy code"
        >
          {copied ? (
            <Check size={13} className="text-green-500" />
          ) : (
            <Copy size={13} className="text-muted-foreground" />
          )}
        </button>
      </InfoTip>
    </div>
  );
}

/**
 * Markdown-specific viewer extras (post-SF-1). Composed with
 * `CommonViewerProps` via intersection to keep the shared base contract
 * (`filePath`, `refreshKey`, `teamContentPath`, `onNavigate`) DRY across
 * every viewer. Future image / PDF / code viewers should mirror this
 * pattern with their own `*Extras` shape rather than redefining the
 * base.
 */
export type MarkdownExtras = {
  /**
   * Set of file paths that changed in the latest watcher debounce burst,
   * or `null` for broad refreshes (viewer must re-read regardless). When
   * `filePath` is not a member of this Set, the viewer takes the Layer 2
   * fast-path: it skips `readFile` and just updates the cache token.
   *
   * SF-3 review: changed from `string` (last-changed path) to `Set<string>`
   * (all paths from this burst). The string shape was incompatible with
   * React 18 / Zustand batching — N sync `set` calls collapsed into one
   * render with only the last path visible, leaving viewers for the other
   * N-1 changed files stale. See file-refresh-store `lastChangedContextFile` docs.
   */
  changedPaths?: Set<string> | null;
  /**
   * When set, the viewer scrolls to this anchor id (e.g. `#section`) on
   * the first frame after the file's content renders. Consumed once,
   * then the parent is expected to clear it.
   */
  pendingHash?: string | null;
  onPendingHashConsumed?: () => void;
  /**
   * Fires after the viewer commits rendered content to the DOM — i.e.
   * after `blocks` / `hast` / `content` updates propagate through
   * React's commit phase. Distinct from `refreshKey` (a parent-held
   * pre-parse intent signal) because the actual DOM swap happens on
   * an async worker round-trip, well after `refreshKey` bumps. Post-
   * commit DOM work (e.g. Find's re-highlight) must listen here, not
   * on `refreshKey`, to operate against the new DOM.
   */
  onContentCommitted?: () => void;
  /**
   * RF-perf-12: composed session key (`fileViewKey(teamKey, filePath)`) under
   * which this file's scroll position is snapshotted across an unmount→remount
   * (RF-perf-10 team-unmount / eviction). When set, the viewer captures scroll
   * on unmount and seeds its restore target from the slice on a fresh mount.
   * Absent (non-team call sites) → the component-local scroll map only.
   */
  viewStateKey?: string;
  /**
   * SW-GS-3: when set (sync view), the markdown file renders as a redline
   * (working-tree vs the publish baseline) instead of plain content. Read by
   * `renderViewer` to route to the redline viewer; the normal `MarkdownViewer`
   * ignores it.
   */
  redline?: {
    orgRoot: string;
    teamKey: string;
    navRepoPath: string;
    /** SW-GS-14: pre-rename org-root-relative path of the active changed file
     *  (only set when it's a rename), so the redline diffs by the old baseline
     *  path instead of reading a moved doc as all-added. */
    oldPath?: string;
    /** SW-GS-19: when set, the redline diffs the file across THIS PR (merge-base
     *  blob vs PR-head blob) instead of baseline-vs-working-tree. */
    prNumber?: number;
    /** SW-GS-13: when set (and no `prNumber`), the redline diffs the file across
     *  THIS landed commit (first-parent blob vs commit blob). */
    commitSha?: string;
    /** Whether this tab is the one on screen: a hidden tab's diff re-reads when shown. */
    active?: boolean;
  } | null;
  /**
   * SW-DX-5: the scanned pipeline tree, threaded from the host (which already
   * scanned it once for the nav — no second scan). An inline
   * `csquery` island uses it to resolve a `scope:` sub-epic name (its rows come
   * from the context engine); it is a `createRoot` subtree DETACHED from React
   * context, so the tree must arrive as a prop, not via `useRightPaneHost()`.
   */
  tree?: DerivedPipelineTree | null;
};

type MarkdownViewerProps = CommonViewerProps & MarkdownExtras;

/**
 * Resolves an in-document hash (e.g. `#what-makes-this-special`) to the
 * matching `[id=…]` element inside `container` and scrolls it to the
 * top of the viewport. `rehype-slug` emits github-style kebab-case ids
 * on headings in the worker pipeline; this is the one call site that
 * turns those ids into navigation. Returns `true` if a target was found
 * and scrolled, `false` otherwise (empty hash, missing container, no
 * matching id, or invalid selector).
 *
 * `behavior: "instant"` is deliberate — the viewer uses
 * `content-visibility: auto` on top-level markdown blocks (index.css
 * `@supports` block), which means off-screen blocks are laid out at an
 * estimated 200px until scrolled into view. A smooth scroll animates
 * through intermediate layouts, letting measured heights replace
 * estimates mid-animation, and the target's `offsetTop` becomes a
 * moving goalpost. Instant scroll computes position once and lands
 * deterministically.
 */
export function scrollToAnchor(hash: string, container: HTMLElement | null): boolean {
  if (!container) return false;
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw) return false;
  // rehype-slug emits decoded ids on headings, but anchor hrefs may
  // arrive percent-encoded (e.g. `#caf%C3%A9` for a "café" heading).
  // `decodeURIComponent` can throw on malformed input — catch and fall
  // back to the raw value rather than failing the whole scroll.
  let id: string;
  try {
    id = decodeURIComponent(raw);
  } catch {
    id = raw;
  }
  let target: Element | null = null;
  try {
    target = container.querySelector(`[id="${CSS.escape(id)}"]`);
  } catch {
    return false;
  }
  if (!target) return false;
  target.scrollIntoView({ behavior: "instant", block: "start" });
  return true;
}

/**
 * Reads the resolved internal path + original `#fragment` off an anchor
 * element. `rewriteInternalLinks` in the worker writes the absolute
 * path to `data-internal-href` while preserving the original `href`
 * (including any fragment) so right-click / copy-link stay sensible.
 * Returns `null` if the target isn't an anchor with a resolved internal
 * href. Used by `onClick`, `onAuxClick`, and context-menu targeting.
 */
export function resolveInternalHref(target: HTMLElement): { path: string; hash?: string } | null {
  const anchor = target.closest<HTMLAnchorElement>("a[data-internal-href]");
  if (!anchor) return null;
  const path = anchor.dataset.internalHref;
  if (!path) return null;
  const href = anchor.getAttribute("href") ?? "";
  const fragment = href.split("#")[1];
  return fragment ? { path, hash: `#${fragment}` } : { path };
}

// Dynamic highlight.js theme loader
export function useHighlightTheme() {
  useEffect(() => {
    document.getElementById("hljs-theme")?.remove();
  }, []);
}

function MermaidDiagram({ children }: { children: string }) {
  // RF-perf-15 (F-2): the shared hook clears innerHTML on unmount; this effect
  // owns removing mermaid's detached measuring node for the id IT created.
  const ref = useDiagramContainer();

  useEffect(() => {
    if (!ref.current) return;
    const id = `mermaid-${Math.random().toString(36).slice(2)}`;
    let cancelled = false;
    mermaid
      .render(id, children)
      .then(({ svg }) => {
        if (!cancelled && ref.current) ref.current.innerHTML = svg;
      })
      .catch(() => {
        if (!cancelled && ref.current) ref.current.textContent = children;
      })
      .finally(() => {
        // If teardown raced the async render, the orphan `d<id>` node is appended
        // only now — the cleanup below already ran and found nothing, so remove it
        // here. Gated on `cancelled`, so it never double-fires with that cleanup.
        if (cancelled) removeMermaidOrphan(id);
      });
    // Remove THIS render's orphan on re-run (children change) AND unmount, so a
    // changing diagram source can't accrue orphan `d<id>` nodes. `ref` is a stable
    // hook return — included to satisfy exhaustive-deps.
    return () => {
      cancelled = true;
      removeMermaidOrphan(id);
    };
  }, [children, ref]);

  return <div ref={ref} className="my-4 flex justify-center" />;
}

// Cached D2 instance across every diagram mount — the WASM module ships
// at ~7.8 MB, so we dynamic-import once (lazy, only if the user has a D2
// fence) and hold the instance singleton-style. On load failure the
// promise is cleared so the next call can retry.
type D2Api = {
  compile: (input: string) => Promise<{ diagram: unknown; renderOptions: unknown }>;
  render: (diagram: unknown, opts: unknown) => Promise<string>;
};
let d2InstancePromise: Promise<D2Api> | null = null;
function getD2(): Promise<D2Api> {
  if (!d2InstancePromise) {
    const p = import("@terrastruct/d2")
      .then((mod) => {
        const instance = new mod.D2();
        return {
          compile: (input: string) =>
            instance.compile(input) as unknown as Promise<{
              diagram: unknown;
              renderOptions: unknown;
            }>,
          render: (diagram: unknown, opts: unknown) =>
            instance.render(diagram as never, opts as never),
        };
      })
      .catch((e: unknown) => {
        // Don't let a rejected promise wedge the singleton — next
        // call can retry (transient chunk-load failures, offline blips).
        if (d2InstancePromise === p) d2InstancePromise = null;
        throw e;
      });
    d2InstancePromise = p;
  }
  return d2InstancePromise;
}

function D2Diagram({ children, salt }: { children: string; salt: string }) {
  // RF-perf-15 (F-3): shared teardown clears the last-rendered SVG on unmount.
  const ref = useDiagramContainer();
  const { resolvedTheme } = useTheme();
  // Compile is expensive and depends only on `children`; re-render on
  // theme change must NOT recompile. Split the effect so the compile
  // result is cached by the React state and reused across theme toggles.
  const [compiled, setCompiled] = useState<{
    diagram: unknown;
    renderOptions: unknown;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    getD2()
      .then((d2) => d2.compile(children))
      .then((result) => {
        if (!cancelled) setCompiled(result);
      })
      .catch((e: unknown) => {
        if (!cancelled && ref.current) {
          ref.current.textContent = e instanceof Error ? e.message : String(e);
        }
      });
    return () => {
      cancelled = true;
    };
    // `ref` is a stable hook return (useRef) — included to satisfy exhaustive-deps.
  }, [children, ref]);

  useEffect(() => {
    if (!compiled || !ref.current) return;
    let cancelled = false;
    getD2()
      .then((d2) =>
        d2.render(compiled.diagram, {
          ...(compiled.renderOptions as Record<string, unknown>),
          themeID: resolvedTheme === "dark" ? 200 : 0,
          pad: 20,
          // Per-diagram salt prevents SVG id collisions when multiple D2
          // fences share a document. Library docs explicitly require it.
          salt,
        }),
      )
      .then((svg) => {
        if (cancelled || !ref.current) return;
        // Library's return type is Promise<string>, but the wrapper uses
        // `as never` casts — guard at runtime so a future API shape
        // change doesn't silently stringify to "[object Object]".
        if (typeof svg !== "string") return;
        ref.current.innerHTML = svg;
      })
      .catch((e: unknown) => {
        if (!cancelled && ref.current) {
          ref.current.textContent = e instanceof Error ? e.message : String(e);
        }
      });
    return () => {
      cancelled = true;
    };
    // `ref` is a stable hook return (useRef) — included to satisfy exhaustive-deps.
  }, [compiled, resolvedTheme, salt, ref]);

  return <div ref={ref} className="my-4 flex justify-center" />;
}

export const MarkdownViewer = memo(function MarkdownViewer({
  filePath,
  refreshKey,
  changedPaths,
  teamContentPath,
  hostTeamKey,
  onNavigate,
  pendingHash,
  onPendingHashConsumed,
  onContentCommitted,
  tree,
  viewStateKey,
}: MarkdownViewerProps) {
  const [content, setContent] = useState<string>("");
  const lastCommittedContent = useRef<string | null>(null);
  const [hast, setHast] = useState<HastRoot | null>(null);
  const [blocks, setBlocks] = useState<Block[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const proseRef = useRef<HTMLDivElement>(null);
  const lastLoadedPath = useRef<string | null>(null);
  const prevFilePath = useRef<string | null>(null);
  const scrollPositions = useRef<Map<string, number>>(new Map());
  // RF-perf-12: persist this file's scroll across an unmount→remount (team
  // switch / eviction). Capture-only here — restore stays woven into the
  // existing async-render gate (`scrollTargetRef` → the `[blocks]` layout
  // effect below), which already avoids a scroll-top flash.
  useScrollSnapshot({
    viewKey: viewStateKey ?? null,
    getEl: () => scrollRef.current,
    ready: true,
    restore: false,
  });
  // Monotonic token so out-of-order worker responses for a previous
  // content value don't overwrite hast/blocks from the current content.
  const parseTokenRef = useRef(0);
  // F.34: scroll target captured in Effect 1 (before async work), restored
  // in the useLayoutEffect after blocks render. Bridges the async gap
  // between file-read and blocks-arrival so the browser never paints a
  // shifted scroll position.
  const scrollTargetRef = useRef<number | null>(null);
  // F.34: track pendingHash via ref so Effect 1 can read it without
  // adding it to the dependency array (which would cause unnecessary
  // file re-reads on pendingHash transitions). Synced in useLayoutEffect
  // so the ref is current before any regular useEffect reads it.
  const pendingHashRef = useRef(pendingHash);
  // Effect 1 is gated on `effectiveRefreshKey` (below) so it doesn't re-run on
  // unrelated sibling-file ticks. But its body still needs the GLOBAL
  // `refreshKey` (the cache version, kept coherent with the prefetch path) and
  // `changedPaths` (fresh-mount reconciliation). Reading them via refs keeps
  // them out of the dep array — listing them would re-introduce the redundant
  // re-runs the gate removes — while staying current (synced pre-effect here,
  // same as `pendingHashRef`).
  const refreshKeyRef = useRef(refreshKey);
  const changedPathsRef = useRef(changedPaths);
  // SW-DX-5: the scanned tree, read by the csquery island at (deferred) mount
  // time inside the island effect below — so a below-fold block that mounts on
  // scroll picks up a tree that arrived after first render, without re-running
  // the island effect (which would remount every diagram). Live updates flow via
  // `onStatus`, not via this ref changing. Synced here (not during render) to
  // keep it current before any effect reads it.
  const treeRef = useRef(tree);
  // SW-DX-5: the csquery island is a `createRoot` subtree mounted once (frozen
  // props), so a later `onNavigate` identity change would strand a stale click
  // handler. Route row clicks through a STABLE callback that reads the latest
  // handler off a ref — always current, never re-mounts the island.
  const onNavigateRef = useRef(onNavigate);
  useLayoutEffect(() => {
    pendingHashRef.current = pendingHash;
    refreshKeyRef.current = refreshKey;
    changedPathsRef.current = changedPaths;
    treeRef.current = tree;
    onNavigateRef.current = onNavigate;
  });
  const stableNavigate = useCallback((filePath: string) => onNavigateRef.current?.(filePath), []);
  const { widthClass, hasTheme, fontSize } = useViewerChrome();
  useHighlightTheme();

  const useBlocksMode = VIEWER_PERF.parseInWorker && VIEWER_PERF.renderMode === "blocks";
  const useJsxMode = VIEWER_PERF.parseInWorker && VIEWER_PERF.renderMode === "jsx";

  // Save scroll position when switching files
  useEffect(() => {
    if (prevFilePath.current && prevFilePath.current !== filePath && scrollRef.current) {
      scrollPositions.current.set(prevFilePath.current, scrollRef.current.scrollTop);
    }
    prevFilePath.current = filePath;
  }, [filePath]);

  // Scroll to `pendingHash` once the current file's content has
  // committed. Used for file+hash navigation — the parent passes the
  // fragment after a Cmd+click / link-with-hash promotion, and this
  // effect consumes it on the first animation frame after `blocks` /
  // `hast` / `content` flips to non-empty for the new `filePath`.
  // `onPendingHashConsumed` clears the parent's stored hash so the
  // effect doesn't fire again on a subsequent unrelated re-render.
  useEffect(() => {
    if (!pendingHash) return;
    // If this file failed to load, the content branches below would
    // never satisfy and the parent's `pendingHash` would linger
    // indefinitely — leaking into a future successful load. Consume it
    // now so the parent clears its state.
    if (error) {
      onPendingHashConsumed?.();
      return;
    }
    if (useBlocksMode && !blocks) return;
    if (useJsxMode && !hast) return;
    if (!useBlocksMode && !useJsxMode && !content) return;
    const frame = requestAnimationFrame(() => {
      scrollToAnchor(pendingHash, scrollRef.current);
      // F.34: clear scrollTargetRef so the useLayoutEffect on [blocks]
      // doesn't overwrite the anchor scroll with savedScroll on the
      // next blocks commit.
      scrollTargetRef.current = null;
      onPendingHashConsumed?.();
    });
    return () => cancelAnimationFrame(frame);
  }, [pendingHash, blocks, hast, content, error, useBlocksMode, useJsxMode, onPendingHashConsumed]);

  // Post-commit signal — fires after the viewer commits rendered
  // content to the DOM. Consumers (e.g. FindBar) use this to schedule
  // work that must run against the fresh DOM, not the pre-parse intent
  // signal (`refreshKey`). Refresh-key bumps arrive immediately on
  // file-change events, but the DOM swap happens after an async worker
  // round-trip — an rAF on refreshKey alone targets stale DOM. Fires
  // when the relevant state for the active render mode becomes non-
  // empty: `blocks` (blocks-mode), `hast` (jsx-mode), `content` (sync).
  useEffect(() => {
    if (useBlocksMode) {
      if (!blocks) return;
    } else if (useJsxMode) {
      if (!hast) return;
    } else if (!content) {
      return;
    }
    onContentCommitted?.();
  }, [blocks, hast, content, useBlocksMode, useJsxMode, onContentCommitted]);

  // Viewer-scoped keyboard scroll shortcuts (shared with the redline viewer).
  useViewerScrollKeys(scrollRef, proseRef);

  // Gate the per-repo `refreshKey` down to a per-file key — the same shared
  // hook html/image/excalidraw viewers use. It advances only when THIS file is
  // in `changedPaths` (or on a broad/null refresh), so Effect 1 no longer
  // re-runs on every unrelated sibling-file tick across the repo; only a
  // genuine change to this file (or a file switch) re-fires it. The fresh-mount
  // cache reconciliation below still keys on the global `refreshKey` for cache
  // coherence with the prefetch path (`team-workspace-view`).
  const effectiveRefreshKey = useEffectiveRefreshKey(refreshKey, changedPaths, filePath);

  // Effect 1: filePath → content (readFile + cache)
  useEffect(() => {
    if (!filePath) {
      lastCommittedContent.current = null;
      setContent("");
      setError(null);
      setLoading(false);
      // SF-2: also reset the "last committed filePath" ref. The Layer 2
      // fresh-mount gate (lines 619-630) reads this ref to distinguish
      // first-mount-of-this-file from a warm rerender; leaving it pointing
      // at the previous filePath would close the gate on the next mount
      // for the same path even though content state was just cleared.
      lastLoadedPath.current = null;
      return;
    }

    const isSameFile = lastLoadedPath.current === filePath;
    if (!isSameFile) lastCommittedContent.current = null;
    // RF-perf-12: on a FRESH mount (post team-unmount / eviction) the local
    // scroll map is empty — fall back to the session slice so the restore
    // target survives the remount. O(1) store read, no disk.
    const savedScroll = isSameFile
      ? (scrollRef.current?.scrollTop ?? 0)
      : (scrollPositions.current.get(filePath) ??
        (viewStateKey
          ? useViewerStore.getState().readViewState(viewStateKey)?.scrollTop
          : undefined) ??
        0);
    // F.34: persist scroll target across the async gap between file-read
    // and blocks-arrival. The useLayoutEffect on [blocks] restores from
    // this ref after morphdom commits. Skip when pendingHash is set — the
    // anchor scroll (via rAF in the pendingHash effect) is the intended
    // target, and writing savedScroll here would cause the useLayoutEffect
    // to reset scrollTop before the rAF fires, producing a flash.
    if (!pendingHashRef.current) {
      scrollTargetRef.current = savedScroll;
    }
    const currentRefreshKey = refreshKeyRef.current ?? 0;

    const cached = fileContentCache.get(filePath);
    if (cached && cached.refreshKey === currentRefreshKey) {
      lastLoadedPath.current = filePath;
      lastCommittedContent.current = cached.content;
      setContent(cached.content);
      setError(null);
      // Worker paths still need the hast/blocks handoff from effect 2;
      // sync path is ready as soon as content lands.
      setLoading(VIEWER_PERF.parseInWorker);
      // F.34: in blocks mode, the useLayoutEffect on [blocks] is the
      // authoritative scroll restore — the rAF fires too early (before
      // blocks arrive from the async worker).
      if (!useBlocksMode) {
        requestAnimationFrame(() => {
          if (scrollRef.current) scrollRef.current.scrollTop = savedScroll;
        });
      }
      return;
    }

    // Layer 2 (fresh-mount reconciliation): skip the readFile IPC when the
    // changed-paths Set doesn't include this viewer's filePath. Forward the new
    // refreshKey to the cache so future checks are hits. `null` (broad refresh)
    // falls through to the readFile path.
    //
    // Since Effect 1 is now gated on `effectiveRefreshKey`, a sibling-file tick
    // on an ALREADY-MOUNTED viewer no longer re-runs this effect at all (the
    // hook holds the key). This branch therefore now matters only for the
    // FRESH-MOUNT race below — a mount that coincides with a sibling-file
    // refresh, where the effect runs (filePath dep) but this file didn't change.
    //
    // SF-2: on a fresh mount this viewer's `content` state is the initial
    // empty string and won't match `cached.content`. Without reconciling
    // state from the cache, the render path paints a blank prose container
    // until the next refresh-key bump or remount. The fresh-mount gate
    // below (`lastLoadedPath.current !== filePath`) restricts the state
    // sets to first-mount-of-this-file on this instance. Warm rerenders
    // (sibling-file bumps arriving at an already-rendered viewer) skip
    // straight to the cache-token forward and return — flipping
    // `loading=true` there would hang the UI on "Loading…" in worker mode
    // because Effect 2's deps are unchanged and it won't re-fire to clear.
    //
    // SF-3: condition changed from `changedFilePath !== filePath` (string)
    // to `!changedPaths.has(filePath)` (Set membership). The Set shape
    // sidesteps React 18 / Zustand batching — see refresh-debounce.ts and
    // file-refresh-store `lastChangedContextFile` docs for why a per-path loop
    // collapsed into one render with only the last path visible.
    const changedPaths = changedPathsRef.current;
    if (!fileChangeAffects(changedPaths, filePath) && cached) {
      fileContentCache.set(filePath, cached.content, currentRefreshKey);
      if (lastLoadedPath.current !== filePath) {
        lastLoadedPath.current = filePath;
        lastCommittedContent.current = cached.content;
        setContent(cached.content);
        setError(null);
        setLoading(VIEWER_PERF.parseInWorker);
      }
      return;
    }

    const showLoading = lastCommittedContent.current === null;
    lastLoadedPath.current = filePath;

    if (showLoading) {
      setLoading(true);
    }
    setError(null);

    logger.debug("viewer", `file load: ${filePath}`);
    // SW-AU-85A-iii: a read still in flight when this effect re-runs (file switch / refresh)
    // or unmounts is SUPERSEDED — its late resolution must not write content, error, OR
    // lifecycle, or an out-of-order settle (a slow deleted-catch landing after a fast
    // restore-then) would clobber the current file's state. The cleanup flips this flag.
    let cancelled = false;
    readFile(filePath)
      .then((text) => {
        if (cancelled) return;
        // Clear the lifecycle even when the text already matches this viewer.
        useViewerStore.getState().setOpenFileLifecycle(filePath, null);
        fileContentCache.set(filePath, text, currentRefreshKey);
        // A cache hit says nothing about what this mounted instance has displayed.
        if (lastCommittedContent.current === text) {
          setLoading(false);
          return;
        }
        lastCommittedContent.current = text;
        setContent(text);
        if (!VIEWER_PERF.parseInWorker || text === "") {
          setLoading(false);
        }
        // F.34: blocks mode defers scroll restore to the useLayoutEffect
        // on [blocks] which fires after morphdom commits.
        if (!useBlocksMode) {
          requestAnimationFrame(() => {
            if (scrollRef.current) scrollRef.current.scrollTop = savedScroll;
          });
        }
      })
      .catch((e) => {
        if (cancelled) return;
        logger.error("viewer", `file read failed: ${filePath} — ${e}`);
        // SW-AU-85A-iii: publish this read's failure so the editor tab's badge shows the
        // honest state — a missing file is "deleted" (badge renders nothing), any other
        // read failure is "temporarily_unavailable" (muted marker). Rides this existing
        // read; adds no per-tab IPC.
        useViewerStore
          .getState()
          .setOpenFileLifecycle(
            filePath,
            isFileMissingError(e) ? "deleted" : "temporarily_unavailable",
          );
        setError(String(e));
        lastCommittedContent.current = null;
        setContent("");
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // Gated on `effectiveRefreshKey` (per-file), not the raw `refreshKey`, so a
    // sibling-file tick on a mounted viewer no longer re-runs this effect. The
    // global `refreshKey`/`changedPaths` it still needs are read via refs above.
    // RF-perf-12: `viewStateKey` is a pure function of (teamKey, filePath) and
    // only changes when `filePath` does, so it adds no extra re-runs.
  }, [filePath, effectiveRefreshKey, useBlocksMode, viewStateKey]);

  // SW-AU-85A-iii: the viewer OWNS its file's presence lifecycle — it sets it above from the
  // read outcome and drops it when it stops viewing the file. Clearing on unmount/path-change
  // (not from the tab badge, a pure reader) keeps the runtime-only slice bounded to
  // actively-viewed files and means a tab-badge remount can never clobber a still-valid entry.
  useEffect(() => {
    if (!filePath) return;
    return () => useViewerStore.getState().setOpenFileLifecycle(filePath, null);
  }, [filePath]);

  // Effect 2: content → hast | blocks via worker.
  //
  // Branches:
  //   - renderMode="blocks" path: try parse-result-cache first (0.2ms hit);
  //     on miss, parseToBlocks → cache set → setBlocks.
  //   - renderMode="jsx" path: parseMarkdown → setHast (32.4 rollback).
  //   - parseInWorker=false: no-op (sync `<ReactMarkdown>` render uses
  //     `content` directly below).
  useEffect(() => {
    if (!VIEWER_PERF.parseInWorker) return;
    if (!content || !filePath) {
      // Initial empty state is not a completed zero-byte read. The file
      // loader owns loading until it commits content (including "").
      setHast(null);
      setBlocks(null);
      if (!filePath || lastCommittedContent.current !== null) setLoading(false);
      return;
    }

    const myToken = ++parseTokenRef.current;

    if (useBlocksMode) {
      const cached = parseResultCache.get(filePath, content);
      if (cached) {
        setBlocks(cached);
        setLoading(false);
        return;
      }
      const handle = parseToBlocks(content, filePath, teamContentPath);
      handle.promise
        .then((r) => {
          if (myToken !== parseTokenRef.current) return;
          parseResultCache.set(filePath, content, r.blocks);
          setBlocks(r.blocks);
          setLoading(false);
        })
        .catch((e) => {
          if (myToken !== parseTokenRef.current) return;
          logger.error("viewer", `worker parse (blocks) failed: ${e}`);
          setError(e instanceof Error ? e.message : String(e));
          setLoading(false);
        });
      return () => handle.cancel();
    }

    // useJsxMode — rollback path, identical to 32.4.
    const handle = parseMarkdown(content);
    handle.promise
      .then((h) => {
        if (myToken !== parseTokenRef.current) return;
        setHast(h);
        setLoading(false);
      })
      .catch((e) => {
        if (myToken !== parseTokenRef.current) return;
        logger.error("viewer", `worker parse (hast) failed: ${e}`);
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      });
    return () => handle.cancel();
  }, [content, filePath, useBlocksMode, teamContentPath]);

  // F.34: Restore scroll position after blocks render. Fires synchronously
  // after React's commit (including BlockSection children's useLayoutEffect
  // morphdom calls) but before paint. React runs useLayoutEffect
  // child-before-parent, so BlockSection morphdom patches are already
  // committed when this parent effect runs — the browser never paints the
  // shifted position.
  useLayoutEffect(() => {
    if (!useBlocksMode || blocks === null) return;
    if (scrollTargetRef.current === null) return;
    const target = scrollTargetRef.current;
    scrollTargetRef.current = null;
    if (scrollRef.current) {
      scrollRef.current.scrollTop = target;
    }
  }, [blocks, useBlocksMode]);

  // SW-DX-5: has the scanned tree EVER been available? Folded into the csquery
  // fingerprint below so a cold-start `null → present` transition remounts ONLY
  // the csquery island (which needs the tree for first paint) — not every
  // diagram. It is a MONOTONIC latch, not `tree != null`: (a) a re-scan (new
  // tree object, still present) does NOT re-key/remount the block, and (b) a
  // transient `present → null` (a drill/scan-error tick where `usePipelineTree`
  // briefly returns null) does NOT tear a populated block back to "Loading…".
  const [treeReady, setTreeReady] = useState(tree != null);
  useEffect(() => {
    if (tree != null && !treeReady) setTreeReady(true);
  }, [tree, treeReady]);

  // F.34: stable island fingerprint — only island blocks (mermaid/d2/csquery)
  // contribute. When non-island blocks change (html/table edits), the
  // fingerprint stays identical and the island effect is NOT re-run, preserving
  // existing React roots and IntersectionObserver entries.
  const diagramFingerprint = useMemo(() => {
    if (!blocks) return "";
    let fp = "";
    for (const b of blocks) {
      if (b.kind === "mermaid" || b.kind === "d2") {
        if (fp) fp += "\n";
        fp += `${b.id}:${b.kind}:${b.code}`;
      } else if (b.kind === "csquery") {
        if (fp) fp += "\n";
        fp += `${b.id}:csquery:${hostTeamKey ?? teamContentPath ?? ""}:${treeReady ? "T" : "N"}:${b.code}`;
      }
    }
    return fp;
  }, [blocks, treeReady, teamContentPath, hostTeamKey]);

  // Persistent diagram state — survives across renders so that unchanged
  // diagrams keep their createRoot + observer entries intact.
  const diagramRootsRef = useRef(new Map<HTMLElement, Root>());
  const diagramObserverRef = useRef<IntersectionObserver | null>(null);
  const prevDiagramMapRef = useRef(new Map<number, string>());

  // Mount diagram React islands at [data-island-id] placeholders. Only
  // diagram fences (mermaid, d2) become islands; plain code fences stay
  // inert in the HTML and the copy button is handled by event delegation
  // below (no createRoot, no mount cost per code fence).
  //
  // Mounts are IntersectionObserver-gated: a diagram only renders when its
  // placeholder scrolls within 500px of the viewport. Above-fold diagrams
  // commit immediately (observer fires synchronously for already-
  // intersecting entries); below-fold ones defer their async render cost
  // (mermaid: ~50-200ms; d2: ~100-500ms + one-time ~7.8MB WASM load) until
  // the user actually scrolls there.
  //
  // F.34: Decoupled from `blocks` reference. Depends on `diagramFingerprint`
  // so only diagram content changes trigger teardown/rebuild. Non-diagram
  // edits leave existing React roots and observer entries intact.
  useEffect(() => {
    if (!useBlocksMode) return;
    if (!blocks || !proseRef.current) return;
    const container = proseRef.current;
    const scrollRoot = scrollRef.current;
    const rootsByEl = diagramRootsRef.current;
    const prevMap = prevDiagramMapRef.current;

    // Build current diagram map: id → code (no React nodes yet — defer
    // element construction until after the early returns below).
    const currentMap = new Map<number, string>();
    for (const b of blocks) {
      if (b.kind === "mermaid" || b.kind === "d2") {
        currentMap.set(b.id, b.code);
      } else if (b.kind === "csquery") {
        // Mirror the fingerprint: a tree-readiness flip re-keys the block so it
        // remounts (and re-reads `treeRef.current`) once the scan lands.
        currentMap.set(
          b.id,
          `${hostTeamKey ?? teamContentPath ?? ""}:${treeReady ? "T" : "N"}:${b.code}`,
        );
      }
    }

    // Teardown removed or changed diagrams
    for (const [id, oldCode] of prevMap) {
      const newCode = currentMap.get(id);
      if (newCode === oldCode) continue;
      for (const [el, root] of rootsByEl) {
        if (Number(el.dataset.islandId) === id) {
          root.unmount();
          rootsByEl.delete(el);
          diagramObserverRef.current?.unobserve(el);
          break;
        }
      }
    }
    prevDiagramMapRef.current = currentMap;

    if (currentMap.size === 0) return;

    const placeholders = Array.from(container.querySelectorAll<HTMLElement>("[data-island-id]"));
    if (placeholders.length === 0) return;

    // Lazily create React nodes inside mountAt — defers element
    // construction until the placeholder actually intersects, so
    // below-fold diagrams never allocate their React element.
    const mountAt = (el: HTMLElement) => {
      if (rootsByEl.has(el)) return;
      const id = Number(el.dataset.islandId);
      const b = blocks.find(
        (b): b is Extract<Block, { kind: "mermaid" | "d2" | "csquery" }> =>
          b.id === id && (b.kind === "mermaid" || b.kind === "d2" || b.kind === "csquery"),
      );
      if (!b) return;
      const node =
        b.kind === "mermaid" ? (
          <MermaidDiagram>{b.code}</MermaidDiagram>
        ) : b.kind === "d2" ? (
          <D2Diagram salt={String(b.id)}>{b.code}</D2Diagram>
        ) : (
          // SW-DX-5: live inline query block. `tree` via ref (detached root);
          // live via onStatus. basePath anchors relative related:/backlink: refs.
          <CsQueryIsland
            code={b.code}
            tree={treeRef.current ?? null}
            basePath={filePath ?? undefined}
            callerPage={hostTeamKey ? undefined : teamContentPath || undefined}
            callerTeam={hostTeamKey}
            onNavigate={stableNavigate}
          />
        );
      const root = createRoot(el);
      root.render(node);
      rootsByEl.set(el, root);
    };

    // Recreate the observer on each effect re-run so the callback
    // captures the current `mountAt` closure. A persistent observer
    // would hold a stale closure and fail to mount newly-added diagrams.
    if (diagramObserverRef.current) {
      diagramObserverRef.current.disconnect();
      diagramObserverRef.current = null;
    }
    if (typeof IntersectionObserver !== "undefined") {
      diagramObserverRef.current = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (!entry.isIntersecting) continue;
            const el = entry.target as HTMLElement;
            mountAt(el);
            diagramObserverRef.current?.unobserve(el);
          }
        },
        {
          root: scrollRoot,
          rootMargin: "500px 0px",
          threshold: 0,
        },
      );
    }

    // Mount or observe new/changed diagrams
    for (const el of placeholders) {
      const id = Number(el.dataset.islandId);
      if (!currentMap.has(id)) continue;
      if (rootsByEl.has(el)) continue; // already mounted

      if (typeof IntersectionObserver === "undefined") {
        mountAt(el);
      } else {
        diagramObserverRef.current?.observe(el);
      }
    }
  }, [diagramFingerprint, useBlocksMode, teamContentPath, hostTeamKey]); // eslint-disable-line react-hooks/exhaustive-deps -- blocks read inside but fingerprint is the stable trigger

  // Unmount all diagram roots on component teardown.
  useEffect(() => {
    const roots = diagramRootsRef.current;
    const prevMap = prevDiagramMapRef.current;
    return () => {
      diagramObserverRef.current?.disconnect();
      diagramObserverRef.current = null;
      for (const r of roots.values()) r.unmount();
      roots.clear();
      prevMap.clear();
    };
  }, []);

  // Triple-click: select the nearest block element's contents instead of
  // the whole prose container. Intercepted on `mousedown` (before the
  // browser's paragraph-boundary walk commits) so there's no visible
  // flash of the wrong selection. Browser's default would walk up past
  // each block's `display: contents` <section> to .md-viewer-prose and
  // select everything inside it.
  const handleContainerMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!useBlocksMode) return;
      if (e.detail !== 3) return;
      const target = e.target as HTMLElement;
      // `code` comes first so triple-click on a single-backtick inline
      // `<code>` selects just that span. `pre` for fenced code blocks.
      // Everything else is a standard block boundary; fallback to the
      // nearest `.md-viewer-prose > section` wrapper so markup that
      // isn't in the list (admonitions, definition lists, details,
      // captions) still selects one block instead of the whole doc.
      const block =
        target.closest<HTMLElement>(
          "code, pre, p, li, blockquote, dt, dd, figcaption, summary, caption, h1, h2, h3, h4, h5, h6, td, th, div.markdown-alert",
        ) ?? target.closest<HTMLElement>(".md-viewer-prose > section");
      if (!block) return;
      e.preventDefault();
      const range = document.createRange();
      range.selectNodeContents(block);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    },
    [useBlocksMode],
  );

  // Delegated click handler for every interactive bit inside the block-
  // mode DOM. Three dispatch targets — all work at any depth inside
  // dangerouslySetInnerHTML content:
  //   1. `<button.copy-btn>` → copies the adjacent `<pre><code>` text.
  //   2. `<a href="#…">` → in-document anchor, scrolls to matching id
  //      (rehype-slug emits github-style ids on headings).
  //   3. `<a data-internal-href>` → fires `onNavigate`. Cmd/Ctrl+click
  //      routes through `{ openInNewTab: true }`; fragment on the
  //      original href passes through as `options.hash`.
  const handleContainerClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!useBlocksMode) return;
      // A triple-click (detail === 3) fires this synthetic click right
      // after our mousedown handler selected the block. Skip the copy /
      // anchor branches so triple-clicking a paragraph with a link
      // doesn't navigate away after selecting.
      if (e.detail >= 3) return;
      const target = e.target as HTMLElement;

      const copyBtn = target.closest<HTMLButtonElement>("button.copy-btn");
      if (copyBtn) {
        e.preventDefault();
        const wrap = copyBtn.closest(".code-wrap");
        const codeEl = wrap?.querySelector<HTMLElement>("pre code");
        const text = codeEl?.textContent ?? "";
        // Guard against environments where `navigator.clipboard` is
        // undefined (old WebView, denied permission, insecure context)
        // so a sync throw doesn't crash the handler. `.catch` swallows
        // a rejected writeText (e.g. when the document lost focus).
        navigator.clipboard
          ?.writeText(text.trimEnd())
          .then(() => {
            copyBtn.classList.add("copied");
            window.setTimeout(() => copyBtn.classList.remove("copied"), 1500);
          })
          .catch(() => {
            /* clipboard unavailable or rejected — UI stays in idle state */
          });
        return;
      }

      // Shift+click is the OS convention for "extend selection" — let it
      // pass through so the browser can extend the user's selection
      // instead of being treated as a link click. The copy-button branch
      // above is intentionally exempt — it's an explicit button click.
      if (e.shiftKey) return;

      // In-document hash anchor (e.g. PRD ToC → section heading). Must
      // run before the internal-href branch so `[data-internal-href]`
      // anchors (which ALSO preserve a hash on `href`) keep their
      // navigation semantics; the internal-href branch handles its own
      // fragment via `options.hash`.
      const hashAnchor = target.closest<HTMLAnchorElement>(
        "a[href^='#']:not([data-internal-href])",
      );
      if (hashAnchor) {
        e.preventDefault();
        scrollToAnchor(hashAnchor.getAttribute("href") ?? "", scrollRef.current);
        return;
      }

      const resolved = resolveInternalHref(target);
      if (!resolved) return;
      // Always `preventDefault` on an internal-.md anchor — the worker
      // leaves the original `href` on the element so right-click / copy-
      // link stays sensible, but browser-default navigation on an in-app
      // `.md` link would take the entire webview off the viewer (data
      // loss in a Tauri shell). Prevent first, then dispatch if we have
      // a handler.
      e.preventDefault();
      if (!onNavigate) return;
      const openInNewTab = e.metaKey || e.ctrlKey;
      if (openInNewTab || resolved.hash) {
        onNavigate(resolved.path, {
          ...(openInNewTab ? { openInNewTab: true } : {}),
          ...(resolved.hash ? { hash: resolved.hash } : {}),
        });
      } else {
        onNavigate(resolved.path);
      }
    },
    [useBlocksMode, onNavigate],
  );

  // Auxiliary-click (middle-click / button 1). Fires on the same
  // elements as `onClick` but only for non-primary mouse buttons. Used
  // here to open internal `.md` links in a new pinned tab, mirroring
  // Cmd+click behaviour without stealing primary-click semantics.
  const handleContainerAuxClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!useBlocksMode) return;
      if (e.button !== 1) return; // middle-click only
      const resolved = resolveInternalHref(e.target as HTMLElement);
      if (!resolved) return;
      e.preventDefault();
      if (!onNavigate) return;
      onNavigate(resolved.path, {
        openInNewTab: true,
        ...(resolved.hash ? { hash: resolved.hash } : {}),
      });
    },
    [useBlocksMode, onNavigate],
  );

  // Components map — used by `jsx` and sync render paths. `blocks` mode
  // bypasses this map entirely; link resolution runs in the worker and
  // `pre`/`code` are direct islands in the block list.
  //
  // PTS-2: `teamContentPath` participates in `resolveViewerLinkPath`'s
  // anchor fallback (post-F4). It's in the deps array below so a workspace
  // switch invalidates the memo and the link resolver picks up the new
  // per-team content dir.
  const components = useMemo<Components>(
    () => ({
      pre: CopyableCodeBlock,
      a({ href, children, ...props }) {
        if (!href) return <a {...props}>{children}</a>;

        if (href.startsWith("http://") || href.startsWith("https://")) {
          return (
            <a href={href} target="_blank" rel="noopener noreferrer" {...props}>
              {children}
            </a>
          );
        }

        if (href.startsWith("#")) {
          return (
            <a
              href={href}
              onClick={(e) => {
                e.preventDefault();
                scrollToAnchor(href, scrollRef.current);
              }}
              {...props}
            >
              {children}
            </a>
          );
        }

        if (onNavigate && filePath) {
          const resolved = resolveViewerLinkPath(href, filePath, teamContentPath);
          if (resolved) {
            const fragment = href.split("#")[1];
            const hash = fragment ? `#${fragment}` : undefined;

            const navigate = (openInNewTab: boolean) => {
              if (openInNewTab || hash) {
                onNavigate(resolved, {
                  ...(openInNewTab ? { openInNewTab: true } : {}),
                  ...(hash ? { hash } : {}),
                });
              } else {
                onNavigate(resolved);
              }
            };

            return (
              <a
                href="#"
                onClick={(e) => {
                  e.preventDefault();
                  navigate(e.metaKey || e.ctrlKey);
                }}
                onAuxClick={(e) => {
                  if (e.button !== 1) return;
                  e.preventDefault();
                  navigate(true);
                }}
                className="cursor-pointer"
                {...props}
              >
                {children}
              </a>
            );
          }
        }

        return (
          <a href={href} {...props}>
            {children}
          </a>
        );
      },
      code({ className, children, ...props }) {
        if (typeof className === "string" && className.includes("language-mermaid")) {
          return <MermaidDiagram>{String(children)}</MermaidDiagram>;
        }
        // SW-DX-5: jsx/sync-mode parity — the worker island path only runs in
        // `blocks` mode, so mirror mermaid and render the live query block here
        // too. Non-empty fences only (mirrors the worker's empty-fence guard).
        if (typeof className === "string" && className.includes("language-csquery")) {
          const code = String(children);
          if (code.trim() !== "") {
            return (
              <CsQueryIsland
                code={code}
                // `tree` is deliberately NOT a dependency of this memo (below):
                // depending on its identity would rebuild the whole `components`
                // map and re-render the entire doc on every pipeline re-scan.
                // The memo rebuilds per-doc (on `filePath`), and the island stays
                // live via `onStatus` regardless, so this render-time snapshot is
                // all it needs for first paint.
                tree={tree ?? null}
                basePath={filePath ?? undefined}
                callerPage={hostTeamKey ? undefined : teamContentPath || undefined}
                callerTeam={hostTeamKey}
                onNavigate={stableNavigate}
              />
            );
          }
        }
        return (
          <code className={className} {...props}>
            {children}
          </code>
        );
      },
    }),
    // `tree` intentionally excluded: it changes identity on every pipeline
    // re-scan and would churn a full-doc re-render in jsx/sync mode; the csquery
    // island stays live via onStatus (see the code() override above).
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [filePath, onNavigate, teamContentPath, stableNavigate],
  );

  if (!filePath) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Select a file to view
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Loading...
      </div>
    );
  }

  if (error) {
    const isFileDeleted = isFileMissingError(error);
    if (isFileDeleted) {
      logger.info("viewer", `file deleted from disk: ${filePath}`);
      const fileName = filePath?.split("/").pop() ?? "File";
      return (
        <FileDeletedPlaceholder
          fileName={fileName}
          // SW-FX-38: route through the shared `closeActiveFileTab` primitive —
          // a slug-less workspace (repoPath null) must STILL close its tab.
          onClose={() => closeActiveFileTab(filePath)}
        />
      );
    }
    return (
      <div className="flex items-center justify-center h-full text-red-500 text-sm">
        Failed to load file: {error}
      </div>
    );
  }

  return (
    <div
      ref={scrollRef}
      className={`h-full overflow-y-auto ${hasTheme ? "rounded-none" : ""}`}
      style={SCROLL_CONTAINER_STYLE}
    >
      <div className={`mx-auto px-8 py-6 ${widthClass}`}>
        <div
          ref={proseRef}
          className="md-viewer-prose prose prose-sm max-w-none"
          style={{ fontSize }}
          onMouseDown={handleContainerMouseDown}
          onClick={handleContainerClick}
          onAuxClick={handleContainerAuxClick}
        >
          {useBlocksMode ? (
            blocks?.map((b) => <BlockSection key={b.id} block={b} />)
          ) : useJsxMode ? (
            hast &&
            toJsxRuntime(hast, {
              Fragment,
              jsx,
              jsxs,
              components,
              passNode: true,
            })
          ) : (
            <ReactMarkdown
              remarkPlugins={VIEWER_REMARK_PLUGINS}
              rehypePlugins={VIEWER_REHYPE_PLUGINS}
              components={components}
            >
              {content}
            </ReactMarkdown>
          )}
        </div>
      </div>
    </div>
  );
});

/**
 * Renders a single block's HTML into a `<section>` element, using `morphdom`
 * to patch the DOM in-place when the block's content changes. Unchanged
 * subtrees within the section are preserved (no innerHTML teardown), which
 * eliminates the flash/flicker visible on live file updates (F.34 RC1).
 *
 * Island blocks (mermaid, d2, csquery) render `null` — their placeholder
 * `<span data-island-id>` lives inside a sibling html block, and the
 * post-commit IntersectionObserver-gated mount loop handles actual rendering.
 */
const BlockSection = memo(
  function BlockSection({ block }: { block: Block }) {
    // SR-8: route the imperatively-patched <section> through the shared
    // self-clearing ref (RF-perf-15 seam). On unmount it blanks innerHTML,
    // releasing the native backing of the morphdom-built subtree — the
    // reclamation gap RF-perf-15 left uncovered. Empty-dep teardown fires ONLY
    // on unmount, never on the in-place patch below, so F.34's flash-free
    // morphdom path is untouched. `prevHtmlRef` holds only a string and is
    // released with the fiber on unmount — no separate teardown needed.
    const ref = useSelfClearingRef<HTMLElement>();
    const prevHtmlRef = useRef<string | null>(null);

    useLayoutEffect(() => {
      const el = ref.current;
      if (!el) return;

      const { html } = block as Extract<Block, { kind: "html" | "table" }>;
      // SR-8 fix: `prevHtmlRef` (a ref) survives StrictMode's simulated
      // remount, but `useSelfClearingRef`'s unmount cleanup blanks `el.innerHTML`
      // between the two mounts — so the ref can say "already painted `html`"
      // while the DOM is actually empty. Trusting the ref alone then early-returns
      // and the section stays blank forever (the "flash then disappears" bug).
      // Gate the skip on the DOM matching too, and treat an emptied container as a
      // fresh paint. `firstChild == null` is an O(1) proxy for "was cleared".
      const isEmpty = el.firstChild === null;
      if (prevHtmlRef.current === html && !isEmpty) return;

      if (prevHtmlRef.current === null || isEmpty) {
        // Fresh mount OR the container was cleared (StrictMode remount / teardown):
        // full paint is the reliable path — morphdom against an emptied node is not.
        el.innerHTML = html;
      } else {
        // Patch in-place via a temporary element — avoids the template
        // string concat + wrapper parse that the string overload requires.
        const tpl = document.createElement("section");
        tpl.innerHTML = html;
        morphdom(el, tpl, MORPHDOM_OPTS);
      }
      prevHtmlRef.current = html;
      // `ref` is a stable object from useSelfClearingRef (useRef under the
      // hood); listed to satisfy exhaustive-deps now that it's hook-provided.
    }, [block, ref]);

    if (block.kind === "mermaid" || block.kind === "d2" || block.kind === "csquery") return null;
    return <section ref={ref} />;
  },
  (prev, next) => {
    if (prev.block.id !== next.block.id || prev.block.kind !== next.block.kind) return false;
    switch (prev.block.kind) {
      case "html":
      case "table":
        return prev.block.html === (next.block as typeof prev.block).html;
      case "mermaid":
      case "d2":
      case "csquery":
        return prev.block.code === (next.block as typeof prev.block).code;
    }
  },
);
