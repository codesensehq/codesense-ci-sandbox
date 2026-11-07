// SPDX-License-Identifier: MIT
/**
 * Simple fuzzy match scoring used by the quick-open, quick-repo, and
 * workspace switcher dialogs. Returns 0 for no match; higher is better.
 *
 * Precedence: prefix match > substring match > fuzzy walk. Consecutive
 * matches and word-boundary matches score higher inside the fuzzy walk.
 *
 * `opts.filenameFromPath`: when true, the prefix check runs against the
 * last `/`-segment of `candidate` (useful for file paths where the user
 * expects "foo" to prefix-match `src/lib/foo.ts`). Default false — the
 * prefix check runs against the whole candidate.
 */
export function fuzzyScore(
  query: string,
  candidate: string,
  opts?: { filenameFromPath?: boolean },
): number {
  const q = query.toLowerCase();
  const c = candidate.toLowerCase();

  if (!q) return 1;

  const prefixTarget = opts?.filenameFromPath ? (c.split("/").pop() ?? c) : c;
  if (prefixTarget.startsWith(q)) return 100 + 1 / candidate.length;
  if (c.includes(q)) return 50 + 1 / candidate.length;

  let score = 0;
  let qi = 0;
  let prevMatch = false;

  for (let ci = 0; ci < c.length && qi < q.length; ci++) {
    if (c[ci] === q[qi]) {
      score += prevMatch ? 3 : 1;
      if (ci === 0 || c[ci - 1] === "/" || c[ci - 1] === "-" || c[ci - 1] === "_") {
        score += 2;
      }
      prevMatch = true;
      qi++;
    } else {
      prevMatch = false;
    }
  }

  return qi === q.length ? score : 0;
}

/** Rank an id/title choice: exact id, id prefix, title, other id, then combined matches.
 * Each tier uses fuzzyScore with a bounded tie-break so long fuzzy walks
 * cannot cross into a higher-priority field. */
export function fuzzyIdTitleScore(query: string, id: string, title: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 1;
  if (id.toLowerCase() === q) return 4;
  const idScore = fuzzyScore(q, id);
  if (id.toLowerCase().startsWith(q)) return 3 + idScore / (idScore + 1);
  const titleScore = fuzzyScore(q, title);
  if (titleScore) return 2 + titleScore / (titleScore + 1);
  if (idScore) return 1 + idScore / (idScore + 1);
  const combinedScore = fuzzyScore(q, `${id} ${title}`);
  return combinedScore ? combinedScore / (combinedScore + 1) : 0;
}
