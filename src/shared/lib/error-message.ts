// SPDX-License-Identifier: MIT
/** Extract a human-readable message from an unknown thrown value. */
export function getErrorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** The typed prefixes Rust commands put on an error so code can branch on the kind of failure.
 *  They are for code, never for a person to read. */
const TYPED_ERROR_PREFIX = /^(?:unavailable|denied|invalid|conflict|not_found|busy):\s*/;

/** Rust's "no such story in the index" refusal. */
const UNINDEXED_STORY = /^no managed story with id '([^']+)'$/;

/** SW-CR-9: Rust's "the file exists on disk but can't be read as that story"
 *  refusal — the fallback scan found a candidate, refreshed it, and the retry
 *  still missed (bad `type`, bad `story_id`, missing `source_repo`). */
const STORY_FILE_CANDIDATE = /^story file candidate at (.+) does not carry story_id '([^']+)'$/;

/** SW-FU-B8: the reason shown when the error carries none a person can read. */
export const SAFE_ERROR_REASON = "Something unexpected went wrong. The details are in the log.";

/** The longest reason shown; the log keeps the rest. */
const MAX_REASON = 240;

/** A leading exception name ("Error: ", "TypeError: "). Plain `Error: ` is dropped;
 *  any other name is a bug surfacing, so the whole message is treated as raw. */
const PLAIN_ERROR_PREFIX = /^(?:Error:\s*)+/;

/**
 * SW-FU-B8 (consistency spec, Rule 5): text that came straight from an exception, a
 * Rust error or a serialized payload. Narrow on purpose — plain product copy like
 * "Repository not found" or "Couldn't reach the code host" never matches. Mirrors
 * the SW-FU-H6 `raw-error-text` detector.
 */
const RAW_ERROR_PATTERNS: readonly RegExp[] = [
  /\bpanicked at\b|\bstack backtrace:/, // Rust panic
  /\bat (?:\S+ )?\(?[^\s()]*[./][^\s()]*:\d+:\d+\)?/, // JS stack frame
  /\b[\w./-]+\.(?:rs|tsx?|jsx?|mjs):\d+\b/, // source location
  /\(os error \d+\)|\bNo such file or directory\b/, // io::Error
  /^(?:Uncaught\s+)?[A-Z]\w*(?:Error|Exception): /, // TypeError: …, DOMException: …
  /\b[A-Za-z_]\w*::[A-Za-z_]+/, // Rust path (git2::Error)
  /\b[A-Z]\w*\s?\{\s*[a-z_]+\s*:/, // Rust Debug struct (TargetDirOccupied { path: … })
  /\b[A-Z]\w*\((?:"|[A-Z]\w*\s?[({])/, // Rust Debug tuple (Io(Custom { … }), Internal("…"))
  /^[A-Z][a-z]+(?:[A-Z][a-z]+){2,}$/, // a bare enum variant (TargetDirOccupied)
  /\{\s*"[A-Za-z_]+"\s*:|\[object Object\]/, // JSON / stringified object
  /\binvalid (?:type|value|length):|\bmissing field `|\bunknown variant `/, // serde
  /^(?:undefined|null)$/,
];

/** JS runtime errors: a bug surfacing, never a reason written for a person. */
const RUNTIME_ERROR_NAMES = new Set([
  "TypeError",
  "RangeError",
  "ReferenceError",
  "SyntaxError",
  "EvalError",
  "URIError",
]);

/** The line that carries the reason (git's `fatal:` / `error:` line when there
 *  is one, else the first non-empty line), capped. */
function reasonLine(text: string): string {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  const line = lines.find((l) => /^(?:fatal|error):/i.test(l)) ?? lines[0] ?? "";
  const chars = Array.from(line);
  return chars.length > MAX_REASON ? `${chars.slice(0, MAX_REASON).join("")}…` : line;
}

/**
 * SW-LN-B28: an error message a person can read. Drops the typed `unavailable:` / `denied:` / …
 * prefix and turns the story-index miss into plain copy that says what to check.
 *
 * SW-FU-B8: it never returns raw error text. A panic, stack frame, os error, Rust
 * Debug value, JSON payload or serde message becomes {@link SAFE_ERROR_REASON} (the
 * full text belongs in the log); a readable reason keeps its first line.
 */
export function userFacingErrorMessage(err: unknown): string {
  if (err instanceof Error && RUNTIME_ERROR_NAMES.has(err.name)) return SAFE_ERROR_REASON;
  const raw = err === null || err === undefined ? "" : getErrorMessage(err);
  let body = raw.trim().replace(PLAIN_ERROR_PREFIX, "");
  if (TYPED_ERROR_PREFIX.test(body)) {
    body = body.replace(TYPED_ERROR_PREFIX, "").trim();
    // SW-CR-9 (R2): match on the FIRST LINE only — the Rust refusal keeps the
    // typed form on line 1 and puts the agent-facing hint on line 2+, which is
    // dropped here so the friendly copy stays stable regardless of the hint.
    const first = reasonLine(body);
    const unindexed = UNINDEXED_STORY.exec(first);
    // SW-LN-B50 (AC-3): the index heals a story it read mid-write on its own (trailing
    // watcher event, next scan, gate pre-check), so a miss that lasts is a file that can't
    // be read as a story — say what to check rather than promise a refresh that won't come.
    if (unindexed)
      return `CodeSense can't find a story with id ${unindexed[1]}. Check that its file has \`type: story\`, \`story_id: ${unindexed[1]}\` and a \`source_repo\` in its frontmatter.`;
    // SW-CR-9 (R2): the scan found a candidate on disk but it still won't
    // index — call out the file and what to fix.
    const candidate = STORY_FILE_CANDIDATE.exec(first);
    if (candidate)
      return `CodeSense found a file at ${candidate[1]} but its frontmatter doesn't declare story ${candidate[2]}. Check that it has \`type: story\`, \`story_id: ${candidate[2]}\` and a \`source_repo\`.`;
  }
  body = body.replace(PLAIN_ERROR_PREFIX, "");
  if (body.length === 0 || RAW_ERROR_PATTERNS.some((p) => p.test(body))) return SAFE_ERROR_REASON;
  return reasonLine(body);
}

/**
 * SW-FU-B8: the reason line for a surface that already says what failed (an
 * InlineAlert title, a join stage line): the user-facing reason, or null when the
 * thrown value carries no text at all, so no filler line is added under the title.
 */
export function errorReasonLine(err: unknown): string | null {
  if (err === null || err === undefined) return null;
  const raw = getErrorMessage(err).trim();
  if (raw === "" || raw === "[object Object]") return null;
  return userFacingErrorMessage(err);
}
