// SPDX-License-Identifier: MIT
/**
 * SW-DX-5 — the inline error surface for a `csquery` block (AC-6). A friendly,
 * bordered notice — never a stack trace, never a white-screen. Used for both a
 * parse error (bad DSL, with the offending line echoed) and, via the island's
 * `ErrorBoundary` fallback, a runtime throw inside the view.
 */

type CsQueryErrorCardProps = {
  /** Short, human message (e.g. "Couldn't parse this csquery block"). */
  message: string;
  /** The parser's `error` string or a caught runtime message. */
  detail?: string;
  /** 1-based source line the error points at, if known. */
  line?: number;
};

export function CsQueryErrorCard({ message, detail, line }: CsQueryErrorCardProps) {
  return (
    <div
      role="note"
      className="my-4 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm"
    >
      <div className="flex items-center gap-2 font-medium text-destructive">
        <span aria-hidden>⚠</span>
        <span>{message}</span>
      </div>
      {detail && (
        <p className="mt-1 whitespace-pre-wrap font-mono text-xs text-muted-foreground">{detail}</p>
      )}
      {line != null && (
        <p className="mt-1 text-xs text-muted-foreground">
          at line <span className="font-mono">{line}</span>
        </p>
      )}
    </div>
  );
}
