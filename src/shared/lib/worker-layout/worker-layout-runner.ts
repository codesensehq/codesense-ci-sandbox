// SPDX-License-Identifier: MIT
/**
 * SW-FX-48 → SLW-WD-9 (RD-8): the generic off-main-thread layout runner.
 *
 * The commit-graph (lane sweep) and other off-main-thread layouts need the SAME
 * subtle worker lifecycle — a lazily-spawned singleton worker reused across
 * requests, an in-flight `pending` map keyed by request id (so stale/cancelled
 * results are dropped), a watchdog timeout (a silently-dead worker must reject,
 * not hang), an `error`/`messageerror` fail-all-and-reset (a wedged worker is
 * terminated and re-spawned on the next call), and a synchronous fallback when
 * `Worker` is unavailable / can't be constructed / `postMessage` throws.
 *
 * Rather than fork ~110 lines of concurrency-critical code, that lifecycle lives
 * here ONCE, parameterized by the worker factory, the request/response message
 * adapters, and the sync fallback. Each `createWorkerLayoutRunner` call owns its
 * own singleton + pending map + id counter (one runner per worker kind).
 */

export interface WorkerLayoutHandle<R> {
  /** Resolves with the layout result. Rejects on worker failure / timeout. */
  promise: Promise<R>;
  /** Drop this request so a stale result is ignored when it arrives. */
  cancel: () => void;
}

export interface WorkerLayoutRunner<I, R> {
  /** Run one layout; off the main thread when possible, sync fallback otherwise. */
  run: (input: I) => WorkerLayoutHandle<R>;
}

/** Routing + payload parsed out of a raw worker response message. */
export type ParsedResponse<R> =
  | { requestId: number; ok: true; result: R }
  | { requestId: number; ok: false; error: string };

export interface WorkerLayoutRunnerConfig<I, R, Msg, Res> {
  /**
   * Lazily construct the worker (a Vite `?worker` default import, invoked as
   * `() => new Worker()`). Called on first use and after a fail-reset; may throw
   * (→ sync fallback).
   */
  createWorker: () => Worker;
  /** Build the message posted to the worker, embedding the runner's request id. */
  toMessage: (requestId: number, input: I) => Msg;
  /** Parse a raw worker response into routing + payload / error. */
  fromMessage: (data: Res) => ParsedResponse<R>;
  /**
   * Synchronous main-thread fallback (Worker undefined / construct fails /
   * `postMessage` throws). May throw — the runner wraps it so a throw becomes a
   * promise rejection, never a synchronous throw out of `run`.
   */
  runSync: (input: I) => R;
  /** Watchdog: reject a request the worker never answers. Default 30_000 ms. */
  timeoutMs?: number;
  /** Failure-message labels (each worker kind keeps its own wording). */
  labels?: { timeout?: string; workerError?: string; cloneError?: string };
}

interface PendingEntry<R> {
  resolve: (r: R) => void;
  reject: (e: Error) => void;
}

const DEFAULT_TIMEOUT_MS = 30_000;

export function createWorkerLayoutRunner<I, R, Msg, Res>(
  config: WorkerLayoutRunnerConfig<I, R, Msg, Res>,
): WorkerLayoutRunner<I, R> {
  const timeoutMs = config.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const timeoutLabel = config.labels?.timeout ?? "layout worker timed out";
  const workerErrorLabel = config.labels?.workerError ?? "layout worker error";
  const cloneErrorLabel =
    config.labels?.cloneError ?? "layout worker: response could not be cloned";

  // Per-runner singleton + one in-flight request map keyed by id; stale/
  // cancelled ids are simply absent when their response lands.
  let worker: Worker | null = null;
  let nextId = 1;
  const pending = new Map<number, PendingEntry<R>>();

  function ensureWorker(): Worker {
    if (worker) return worker;
    const w = config.createWorker();
    worker = w;
    w.addEventListener("message", (ev: MessageEvent<Res>) => {
      const parsed = config.fromMessage(ev.data);
      const entry = pending.get(parsed.requestId);
      if (!entry) return; // stale / cancelled / timed out
      pending.delete(parsed.requestId);
      if (parsed.ok) entry.resolve(parsed.result);
      else entry.reject(new Error(parsed.error));
    });
    // `error` = uncaught exception in the worker; `messageerror` = a response
    // that couldn't be structured-cloned. Either leaves the worker in an
    // unknowable state, so reject every pending request, terminate it, and null
    // the singleton — the next call spawns a fresh worker instead of hanging on
    // a wedged one (and the dead worker thread isn't leaked).
    const failAllAndReset = (reason: string) => {
      for (const [, entry] of pending) entry.reject(new Error(reason));
      pending.clear();
      if (worker) {
        worker.terminate();
        worker = null;
      }
    };
    w.addEventListener("error", (ev) => failAllAndReset(ev.message || workerErrorLabel));
    w.addEventListener("messageerror", () => failAllAndReset(cloneErrorLabel));
    return w;
  }

  /**
   * Lay out synchronously on the current thread. Runs inside the promise
   * executor so a throw becomes a rejection (never a synchronous throw out of
   * `run`, which would escape the caller's `.catch` into a React effect).
   */
  function runSyncHandle(input: I): WorkerLayoutHandle<R> {
    return {
      promise: new Promise<R>((resolve) => resolve(config.runSync(input))),
      cancel: () => {},
    };
  }

  function run(input: I): WorkerLayoutHandle<R> {
    if (typeof Worker === "undefined") return runSyncHandle(input);

    let w: Worker;
    try {
      w = ensureWorker();
    } catch {
      return runSyncHandle(input);
    }

    const requestId = nextId++;
    // Post before wiring up `pending`: JS is single-threaded, so the worker's
    // response can't be processed until this synchronous call returns. If
    // `postMessage` throws (a non-cloneable payload), fall back to a sync layout
    // rather than leaving an unsettled promise.
    try {
      w.postMessage(config.toMessage(requestId, input));
    } catch {
      return runSyncHandle(input);
    }

    let timer: ReturnType<typeof setTimeout> | undefined;
    const promise = new Promise<R>((resolve, reject) => {
      timer = setTimeout(() => {
        if (pending.delete(requestId)) reject(new Error(timeoutLabel));
      }, timeoutMs);
      pending.set(requestId, {
        resolve: (r) => {
          clearTimeout(timer);
          resolve(r);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
    });
    return {
      promise,
      cancel: () => {
        clearTimeout(timer);
        pending.delete(requestId);
      },
    };
  }

  return { run };
}
