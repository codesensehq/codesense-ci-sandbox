// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";

import { scanPipelineTree, listContextFiles, type ContextFile } from "@/tauri/commands";
import { CONTEXT_INDEX_CHANGED_EVENT, type ContextIndexChangedPayload } from "@/tauri/events";

export const CONTENT_SCAN_TIMEOUT_MS = 10_000;

type Read = () => Promise<unknown>;
interface Request {
  key: string;
  read: Read;
  promise: Promise<unknown>;
  resolve: (tree: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  expired: boolean;
}
interface Slot {
  whenIdle: Promise<void>;
  markIdle: () => void;
  running?: Request;
  next?: Request;
}

export class ContentScanSuperseded extends Error {
  constructor(readonly whenIdle: Promise<void>) {
    super("Content scan superseded by a newer request.");
  }
}
function newSlot(): Slot {
  let markIdle!: () => void;
  const whenIdle = new Promise<void>((resolve) => {
    markIdle = resolve;
  });
  return { whenIdle, markIdle };
}

/** Started native reads cannot be aborted: keep their slot even after UI timeout. */
export function createContentScanQueue() {
  const owners = new Map<string, Slot>();
  const start = (owner: string, slot: Slot, request: Request) => {
    slot.running = request;
    void Promise.resolve()
      .then(request.read)
      .then(
        (tree) => {
          if (!request.expired) request.resolve(tree);
        },
        (error: unknown) => {
          if (!request.expired)
            request.reject(error instanceof Error ? error : new Error(String(error)));
        },
      )
      .finally(() => {
        clearTimeout(request.timer);
        slot.running = undefined;
        const next = slot.next;
        slot.next = undefined;
        if (next && !next.expired) start(owner, slot, next);
        else {
          owners.delete(owner);
          slot.markIdle();
        }
      });
  };
  return {
    request<T>(
      owner: string,
      key: string,
      read: () => Promise<T>,
      timeoutMs = CONTENT_SCAN_TIMEOUT_MS,
    ): Promise<T> {
      const slot = owners.get(owner) ?? newSlot();
      if (slot.running?.expired)
        return Promise.reject(
          new Error("The previous content scan is still finishing. Retry after it completes."),
        );
      if (slot.next?.key === key) return slot.next.promise as Promise<T>;
      if (!slot.next && slot.running?.key === key) return slot.running.promise as Promise<T>;
      let resolve!: Request["resolve"];
      let reject!: Request["reject"];
      const promise = new Promise<unknown>((yes, no) => {
        resolve = yes;
        reject = no;
      });
      const request: Request = {
        key,
        read,
        promise,
        resolve,
        reject,
        expired: false,
        timer: setTimeout(
          () => {
            request.expired = true;
            request.reject(
              new Error("Reading team context timed out. Retry when the scan finishes."),
            );
            if (slot.next === request) slot.next = undefined;
            if (slot.running === request && slot.next) {
              clearTimeout(slot.next.timer);
              slot.next.reject(
                new Error("Reading team context timed out. Retry when the scan finishes."),
              );
              slot.next = undefined;
            }
          },
          Math.max(0, timeoutMs),
        ),
      };
      owners.set(owner, slot);
      if (slot.running) {
        if (slot.next) {
          clearTimeout(slot.next.timer);
          slot.next.reject(new ContentScanSuperseded(slot.whenIdle));
        }
        slot.next = request;
      } else start(owner, slot, request);
      return promise as Promise<T>;
    },
  };
}

const scans = createContentScanQueue();

export function requestContentScan(
  args: Parameters<typeof scanPipelineTree>,
  revision: string | number = 0,
  timeoutMs = CONTENT_SCAN_TIMEOUT_MS,
) {
  // Only small scan inputs are serialized; the returned tree stays off this key path.
  return scans.request(
    args[0],
    JSON.stringify(["tree", args, revision]),
    async () => {
      const tree = await scanPipelineTree(...args);
      const pending = [tree];
      while (pending.length) {
        const node = pending.pop()!;
        if (node.orderError?.startsWith("Failed to read directory:"))
          throw new Error(node.orderError);
        pending.push(...node.subEpics);
      }
      return tree;
    },
    timeoutMs,
  );
}

// SV-PBF-267: the ONE place the frontend lists a folder's files. The backend now
// answers from the context index (no walk), so listings are cached per root and
// valid until the index publishes a newer generation. Every consumer (⌘P, the
// command palette, the move/copy picker, scratch occupancy) shares this cache and
// joins in-flight requests, instead of each listing the same root itself.

/** Prefix of the backend error for a root the context index does not hold. */
const NOT_INDEXED_PREFIX = "not indexed:";

/** True when `error` is the backend refusing a root that is not in the index. */
export function isNotIndexedError(error: unknown): boolean {
  const text = error instanceof Error ? error.message : String(error);
  return text.startsWith(NOT_INDEXED_PREFIX);
}

/** A message for a failed file listing: a plain explanation when the folder is not indexed. */
export function fileListErrorMessage(error: unknown): string {
  if (isNotIndexedError(error))
    return "This folder isn't part of your synced context yet, so its files can't be listed. Sync the team and try again.";
  return error instanceof Error ? error.message : String(error);
}

let indexGeneration = 0;
const fileIndexCache = new Map<string, ContextFile[]>();
const fileIndexInFlight = new Map<string, Promise<ContextFile[]>>();
const generationListeners = new Set<(generation: number) => void>();
let indexListener: Promise<() => void> | null = null;
// Lists are cached only while the generation listener is live: without it nothing
// would ever invalidate them.
let indexListening = false;

function attachIndexListener() {
  if (indexListener) return;
  indexListener = listen<ContextIndexChangedPayload>(CONTEXT_INDEX_CHANGED_EVENT, (event) => {
    const generation = event.payload?.generation;
    // Same or older generation: nothing changed that a cached list could miss.
    if (typeof generation === "number" && generation <= indexGeneration) return;
    indexGeneration = typeof generation === "number" ? generation : indexGeneration + 1;
    fileIndexCache.clear();
    for (const listener of [...generationListeners]) listener(indexGeneration);
  }).then(
    (unlisten) => {
      indexListening = true;
      return unlisten;
    },
    () => () => {},
  );
}

/** Run `listener` whenever the index publishes a newer generation (cached lists are then dropped). */
export function onContextIndexGeneration(listener: (generation: number) => void): () => void {
  attachIndexListener();
  generationListeners.add(listener);
  return () => {
    generationListeners.delete(listener);
  };
}

/** The files under `root`: cached per (root, index generation), in-flight requests joined. */
export function requestContentFileIndex(root: string): Promise<ContextFile[]> {
  attachIndexListener();
  const cached = fileIndexCache.get(root);
  if (cached) return Promise.resolve(cached);
  const pending = fileIndexInFlight.get(root);
  if (pending) return pending;
  const startedAt = indexGeneration;
  const request = listContextFiles(root)
    .then((files) => {
      // A newer generation published while this ran: serve it, but do not cache it.
      if (indexListening && startedAt === indexGeneration) fileIndexCache.set(root, files);
      return files;
    })
    .finally(() => {
      if (fileIndexInFlight.get(root) === request) fileIndexInFlight.delete(root);
    });
  fileIndexInFlight.set(root, request);
  return request;
}

/** Test seam: forget every cached list and the known generation. */
export function resetContentFileIndexForTests() {
  fileIndexCache.clear();
  fileIndexInFlight.clear();
  generationListeners.clear();
  indexGeneration = 0;
  indexListening = false;
  void indexListener?.then((unlisten) => unlisten());
  indexListener = null;
}
