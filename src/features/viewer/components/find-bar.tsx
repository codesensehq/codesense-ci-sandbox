// SPDX-License-Identifier: MIT
import { useState, useEffect, useCallback } from "react";
import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";

interface FindBarProps {
  visible: boolean;
  onClose: () => void;
  containerRef: React.RefObject<HTMLElement | null>;
  /**
   * Identity key for the current search container. When the active
   * viewer changes (keep-alive swaps which wrapper holds `containerRef`)
   * this key changes, and the highlight effect re-runs against the new
   * DOM. Without this, matches from the previous tab stay in a hidden
   * wrapper and the new tab shows stale match counts.
   */
  containerKey?: string;
  /**
   * Monotonic counter that bumps when the current file's rendered
   * content is replaced (e.g. external file update → CodeSense re-
   * parses and commits new DOM). Re-runs the highlight pass against
   * the new DOM so Find highlights survive live reloads instead of
   * being wiped. Parent typically wires this to the same file-refresh-store
   * `fileRefreshKey` used to force MarkdownViewer re-reads.
   */
  contentVersion?: number;
}

export function FindBar({
  visible,
  onClose,
  containerRef,
  containerKey,
  contentVersion,
}: FindBarProps) {
  const [query, setQuery] = useState("");
  const [matchCount, setMatchCount] = useState(0);
  const [currentMatch, setCurrentMatch] = useState(0);

  // DOM-only cleanup — no setState here to avoid triggering React reconciliation
  // while the DOM is in a mutated state. Callers reset state separately.
  const clearHighlights = useCallback(() => {
    if (!containerRef.current) return;
    const marks = containerRef.current.querySelectorAll("mark[data-find]");
    marks.forEach((mark) => {
      const parent = mark.parentNode;
      if (parent) {
        parent.replaceChild(document.createTextNode(mark.textContent ?? ""), mark);
        parent.normalize();
      }
    });
  }, [containerRef]);

  const highlight = useCallback(
    (searchQuery: string) => {
      clearHighlights();
      if (!searchQuery || !containerRef.current) return;
      // Guard against a container that was detached between the time
      // this pass was scheduled (rAF) and when it ran (tab change, file
      // replace). Walking a detached tree would return 0 text nodes and
      // silently leave the match count stale; bail cleanly instead.
      if (!containerRef.current.isConnected) return;

      const walker = document.createTreeWalker(containerRef.current, NodeFilter.SHOW_TEXT);

      const textNodes: Text[] = [];
      while (walker.nextNode()) {
        textNodes.push(walker.currentNode as Text);
      }

      let count = 0;
      const lowerQuery = searchQuery.toLowerCase();

      for (const node of textNodes) {
        const text = node.textContent ?? "";
        const lowerText = text.toLowerCase();
        if (!lowerText.includes(lowerQuery)) continue;

        const frag = document.createDocumentFragment();
        let lastIdx = 0;

        let idx = lowerText.indexOf(lowerQuery, lastIdx);
        while (idx !== -1) {
          if (idx > lastIdx) {
            frag.appendChild(document.createTextNode(text.slice(lastIdx, idx)));
          }
          const mark = document.createElement("mark");
          mark.setAttribute("data-find", String(count));
          mark.className =
            count === 0
              ? "bg-accent/40 text-foreground rounded px-0.5"
              : "bg-yellow-300/40 dark:bg-yellow-500/30 text-foreground rounded px-0.5";
          mark.textContent = text.slice(idx, idx + searchQuery.length);
          frag.appendChild(mark);
          count++;
          lastIdx = idx + searchQuery.length;
          idx = lowerText.indexOf(lowerQuery, lastIdx);
        }

        if (lastIdx < text.length) {
          frag.appendChild(document.createTextNode(text.slice(lastIdx)));
        }

        node.parentNode?.replaceChild(frag, node);
      }

      setMatchCount(count);
      setCurrentMatch(count > 0 ? 1 : 0);

      // Scroll to first match
      if (count > 0) {
        const first = containerRef.current?.querySelector("mark[data-find='0']");
        first?.scrollIntoView({ behavior: "smooth", block: "center" });
      }
    },
    [containerRef, clearHighlights],
  );

  const goToMatch = useCallback(
    (idx: number) => {
      if (!containerRef.current || matchCount === 0) return;
      const wrapped = (((idx - 1) % matchCount) + matchCount) % matchCount;

      // Reset all highlights
      containerRef.current.querySelectorAll("mark[data-find]").forEach((m) => {
        (m as HTMLElement).className =
          "bg-yellow-300/40 dark:bg-yellow-500/30 text-foreground rounded px-0.5";
      });

      // Highlight current
      const current = containerRef.current.querySelector(`mark[data-find='${wrapped}']`);
      if (current) {
        (current as HTMLElement).className = "bg-accent/40 text-foreground rounded px-0.5";
        current.scrollIntoView({ behavior: "smooth", block: "center" });
      }
      setCurrentMatch(wrapped + 1);
    },
    [containerRef, matchCount],
  );

  useEffect(() => {
    if (!visible) {
      clearHighlights();
      // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional state reset when find bar is hidden
      setQuery("");
      setMatchCount(0);
      setCurrentMatch(0);
    }
  }, [visible, clearHighlights]);

  useEffect(() => {
    // Bail on whitespace-only queries — `query.length < 2` alone lets
    // "  " through, which would match every formatting-whitespace text
    // node in the document and balloon the count into the thousands.
    if (query.trim().length < 2) {
      clearHighlights();
      // eslint-disable-next-line react-hooks/set-state-in-effect -- reset state when query is cleared
      setMatchCount(0);

      setCurrentMatch(0);
      return;
    }
    // `requestAnimationFrame` lets a pending content commit (external
    // file update → re-render) settle before walking text nodes. Without
    // this, bumping `contentVersion` in the same tick as the parent's
    // render would see the old DOM (with stale `<mark>` wrappers from
    // the previous highlight pass) or, worse, a half-committed tree.
    const frame = requestAnimationFrame(() => {
      highlight(query);
    });
    return () => cancelAnimationFrame(frame);
    // `containerKey` re-runs on tab switch (container identity change);
    // `contentVersion` re-runs on same-tab content replace (file edit).
  }, [query, containerKey, contentVersion, highlight, clearHighlights]);

  if (!visible) return null;

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 border-b bg-card/80 backdrop-blur-sm">
      <Input
        autoFocus
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            clearHighlights();
            onClose();
          }
          if (e.key === "Enter" && !e.shiftKey) goToMatch(currentMatch + 1);
          if (e.key === "Enter" && e.shiftKey) goToMatch(currentMatch - 1);
        }}
        placeholder="Find in document..."
        className="h-7 w-64 text-xs"
      />
      <span className="text-xs text-muted-foreground min-w-[60px]">
        {matchCount > 0 ? `${currentMatch}/${matchCount}` : query.length >= 2 ? "No matches" : ""}
      </span>
      <Button
        variant="ghost"
        size="sm"
        className="h-6 px-1.5 text-xs"
        onClick={() => goToMatch(currentMatch - 1)}
        disabled={matchCount === 0}
      >
        &uarr;
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="h-6 px-1.5 text-xs"
        onClick={() => goToMatch(currentMatch + 1)}
        disabled={matchCount === 0}
      >
        &darr;
      </Button>
      <Button
        variant="ghost"
        size="sm"
        className="h-6 px-1.5 text-xs"
        onClick={() => {
          clearHighlights();
          onClose();
        }}
      >
        &times;
      </Button>
    </div>
  );
}
