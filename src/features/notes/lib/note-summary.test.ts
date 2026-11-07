// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";

import { summarize } from "./note-summary";

// The same cases as the backend's `cp2_previews_match_the_native_notes_summary`: the
// open note's preview (built here) and every other note's (built by the records
// projection) must read the same.
it("matches the records projection's preview", () => {
  expect(
    summarize(
      "# Plan\n- [ ] Call the bank [@Jimmy](cs:agent:jimmy%23one)\n```js\nconst x = 1\n```\n> quoted **bold** [link](http://x)\n1. fifth line\n",
    ),
  ).toEqual({
    snippet: "Plan · Call the bank · js · const x = 1",
    refs: [{ handle: "jimmy#one", name: "Jimmy" }],
  });
  expect(
    summarize("---\n* [x] done _it_\n\n[@](cs:agent:a) [@B](cs:agent:a) [@C](cs:agent:%E0%A4%A)\n"),
  ).toEqual({ snippet: "done it", refs: [{ handle: "a", name: "a" }] });
  expect(summarize("a".repeat(300)).snippet).toHaveLength(240);
  expect(summarize("")).toEqual({ snippet: "", refs: [] });
});
