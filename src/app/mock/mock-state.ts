// SPDX-License-Identifier: MIT
import { createCollection } from "./mock-store";
import type { Collection } from "./mock-store";

/** One content-repo file addressed by its path; `id` mirrors `path` so it slots
 *  into the id-keyed collection primitive. */
export interface StoredContentFile {
  id: string;
  path: string;
  content: string;
}

/** The whole mock session's in-memory state: one collection per CRUD surface,
 *  shared by every handler group so writes on one surface are visible to reads
 *  on another. Lives for the browser session; a reload resets it. */
export interface MockState {
  contentFiles: Collection<StoredContentFile>;
}

export function createMockState(): MockState {
  return {
    contentFiles: createCollection<StoredContentFile>(),
  };
}
