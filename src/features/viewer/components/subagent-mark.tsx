// SPDX-License-Identifier: MIT
import { subagentCountWords } from "@/shared/lib/terminal-dot-words";

/** The subagent glyph: a node with two children branching off its stem. */
function SubagentGlyph(): React.JSX.Element {
  return (
    <svg className="ag-subs-glyph" viewBox="0 0 12 12" fill="none" aria-hidden>
      <circle cx="3.25" cy="2.5" r="1.5" fill="currentColor" />
      <path
        d="M3.25 4v4.25A1.25 1.25 0 0 0 4.5 9.5h2.5M3.25 6h3.75"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
      />
      <circle cx="9.25" cy="6" r="1.5" fill="currentColor" />
      <circle cx="9.25" cy="9.5" r="1.5" fill="currentColor" />
    </svg>
  );
}

/**
 * How many subagents an agent runs now, as a small mark beside its status word: the branch glyph
 * and the count. It takes the ink of the words it follows. Its words ("3 subagents running") are
 * its tooltip and its accessible name, so the count never rests on the glyph alone.
 */
export function SubagentMark({
  count,
  testId = "agent-subagents",
}: {
  count: number;
  testId?: string;
}): React.JSX.Element | null {
  if (count <= 0) return null;
  const words = `${subagentCountWords(count)} running`;
  return (
    <span
      role="img"
      className="ag-subs"
      aria-label={words}
      title={words}
      data-testid={testId}
      data-count={count}
    >
      <SubagentGlyph />
      <span aria-hidden>{count}</span>
    </span>
  );
}
