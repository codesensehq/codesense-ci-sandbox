// SPDX-License-Identifier: MIT
/**
 * SLW-FX-21: the ONE shared read → write → toast orchestration behind the three
 * "relate docs" gestures — chip "Relate a file…" (capability 1), nav drag-to-link
 * (capability 2), and chip per-row unlink (capability 3). All four toast strings
 * live here, so drag and dropdown stay byte-identical (AC-6).
 *
 * Lives in `dashboard/lib` (with `attach-supporting-doc.ts` / `use-attach-doc-to-node.ts`)
 * because it touches `@/tauri/commands`. Disk I/O — one `readFile` + at most one
 * atomic `writeContentFile` — happens ONLY on an explicit relate/unlink call,
 * never on hover, drag-over, or popover-open, so no gesture blocks the UI. The
 * write is watcher-suppressed but self-nudges `context-file-change`, so the
 * pipeline re-scans and the chip / nav re-derive with no manual refresh.
 */

import { toast } from "sonner";

import { readFile, writeContentFile } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

import { attachSupportingDoc, detachSupportingDoc } from "./attach-supporting-doc";

/** The four user-facing strings, centralized so every surface is identical. */
export const RELATE_TOAST = {
  added: "Added as a related doc",
  already: "Already related",
  unlinked: "Unlinked",
  addError: "Couldn't relate that file",
  unlinkError: "Couldn't unlink that file",
} as const;

// The command handles are read lazily at call time (not captured at module load)
// so importing this module never touches `@/tauri/commands` — a consumer that
// partial-mocks that module in tests (e.g. App.coverage) doesn't have to enumerate
// readFile/writeContentFile just to import a component that transitively pulls this.
const io = () => ({ readFile, writeContentFile });

// Per-target-path write serializer. Each gesture is a read → surgery → write;
// two gestures onto the SAME target (e.g. relating several files onto one open
// doc, or two rapid drops onto one node) would otherwise both read the original
// frontmatter and the second write would clobber the first (silent lost relation).
// Chaining by target path makes each read-modify-write see the prior result.
// Mirrors the SW-FX-30 per-file status-write serializer. Different targets stay
// concurrent (distinct keys). The entry is pruned once its chain drains, so the
// map can't grow unbounded across a long session.
const writeChains = new Map<string, Promise<unknown>>();

function serializePerTarget<T>(targetPath: string, op: () => Promise<T>): Promise<T> {
  const prev = writeChains.get(targetPath) ?? Promise.resolve();
  // Run `op` after `prev` settles whether it resolved or REJECTED (both handlers
  // are `op`), so one failed write never wedges the queue for that target.
  const result = prev.then(op, op);
  const settled = result.then(
    () => undefined,
    () => undefined,
  );
  writeChains.set(targetPath, settled);
  void settled.then(() => {
    // Drop the entry only if we're still the tail (no newer op enqueued).
    if (writeChains.get(targetPath) === settled) writeChains.delete(targetPath);
  });
  return result;
}

/**
 * Record `docPath` into `targetMdPath`'s `supporting_docs` (idempotent). Never
 * throws — an IO failure is logged and surfaced as an error toast, so a caller
 * in an event handler needn't guard. `targetMdPath` is the node whose frontmatter
 * grows the relation (the OPEN doc for the chip, the DROP TARGET for the nav);
 * `docPath` is the file being related. Serialized per target so concurrent
 * relates/unlinks onto one node can't lose a write.
 */
export function relateDoc(targetMdPath: string, docPath: string): Promise<void> {
  return serializePerTarget(targetMdPath, async () => {
    try {
      const { changed } = await attachSupportingDoc(targetMdPath, docPath, io());
      if (changed) toast.success(RELATE_TOAST.added);
      else toast(RELATE_TOAST.already);
    } catch (err) {
      logger.error("relate-doc", `relate ${docPath} → ${targetMdPath} failed: ${err}`);
      toast.error(RELATE_TOAST.addError);
    }
  });
}

/**
 * Remove `docPath` from `targetMdPath`'s `supporting_docs` (idempotent). Silent
 * when the doc wasn't related (`changed:false`) per AC-3 — only a successful
 * unlink toasts. Never throws (IO failure → logged + error toast). Shares the
 * per-target serializer with {@link relateDoc}.
 */
export function unrelateDoc(targetMdPath: string, docPath: string): Promise<void> {
  return serializePerTarget(targetMdPath, async () => {
    try {
      const { changed } = await detachSupportingDoc(targetMdPath, docPath, io());
      if (changed) toast.success(RELATE_TOAST.unlinked);
    } catch (err) {
      logger.error("relate-doc", `unrelate ${docPath} from ${targetMdPath} failed: ${err}`);
      toast.error(RELATE_TOAST.unlinkError);
    }
  });
}
