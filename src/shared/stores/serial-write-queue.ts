// SPDX-License-Identifier: MIT
/** Return a queue that runs each async write in dispatch order. */
export function createSerialWriteQueue() {
  let tail: Promise<void> = Promise.resolve();
  return function enqueue<T>(write: () => Promise<T>): Promise<T> {
    const result = tail.then(write);
    tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  };
}
