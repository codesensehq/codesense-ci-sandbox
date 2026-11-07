// SPDX-License-Identifier: MIT
import { flushAll } from "./frame-lifecycle";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** Both native quit routes share one deadline, including layout and note writes. */
export async function flushBeforeQuit(): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  await Promise.race([
    Promise.allSettled([
      flushAll(500),
      useViewerStore.getState().flushPersist(),
      useViewerStore.getState().flushNotes(),
    ]),
    new Promise<void>((resolve) => {
      timer = setTimeout(resolve, 500);
    }),
  ]);
  clearTimeout(timer);
}
