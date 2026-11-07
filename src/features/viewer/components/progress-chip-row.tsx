// SPDX-License-Identifier: MIT
interface ProgressChipRowProps {
  total: number;
  done: number;
}

/**
 * Inline story progress: a micro bar plus the done/total fraction.
 *
 * The bar always shows, so the fraction beside a filter field never reads as a match count.
 * Screen readers and the tooltip get the spelled-out sentence.
 */
export function ProgressChipRow({ total, done }: ProgressChipRowProps) {
  if (total === 0) return null;

  const pct = Math.min(100, Math.round((done / total) * 100));
  const sentence = `${done} of ${total} stories done`;

  return (
    <div
      className="flex flex-shrink-0 items-center gap-1.5 text-[10px] text-muted-foreground tabular-nums"
      title={sentence}
      data-testid="nav-story-progress"
    >
      <div
        className="h-1 w-8 flex-shrink-0 overflow-hidden rounded-full"
        style={{ backgroundColor: "var(--border)" }}
        aria-hidden="true"
        data-testid="nav-story-progress-bar"
      >
        <div
          className="h-full rounded-full"
          style={{ width: `${pct}%`, backgroundColor: "var(--green, #10b981)" }}
        />
      </div>
      <span aria-hidden="true">
        {done}/{total}
      </span>
      <span className="sr-only">{sentence}</span>
    </div>
  );
}
