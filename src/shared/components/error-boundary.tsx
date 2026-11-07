// SPDX-License-Identifier: MIT
import { Component, type ErrorInfo, type ReactNode } from "react";

import { logger } from "@/shared/lib/logger";

/**
 * A minimal render-error boundary for isolating UNTRUSTED subtrees — e.g. a
 * consumer-supplied render-prop such as the agent module's `answerablePrompts`
 * slot (SW-AW-7), or the app-wide root / per-team backstops (SF-11). A
 * synchronous throw in the wrapped subtree degrades to `fallback` and is logged,
 * instead of unmounting the whole surrounding surface (the right pane, the chat,
 * the terminal affordance — or, at the root, the entire window).
 *
 * Reset per unit of work by keying the ELEMENT — e.g. `key={ask.correlationId}`
 * remounts the boundary (clearing the failed state) when the ask changes. This
 * is the ONLY reset path: there is no `componentDidUpdate` reset, so a boundary
 * whose element key is stable stays failed until it remounts.
 */
export class ErrorBoundary extends Component<
  {
    children: ReactNode;
    fallback: ReactNode;
    /** Log scope + a hook for callers that want to react to the failure. */
    scope?: string;
    onError?: (error: unknown) => void;
  },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }

  componentDidCatch(error: unknown, errorInfo: ErrorInfo): void {
    const scope = this.props.scope ?? "ui";
    // Prefer the Error's own stack; fall back to String(error) for non-Error throws.
    const detail = error instanceof Error && error.stack ? error.stack : String(error);
    // React's component stack names the component that threw — the single most
    // useful artifact for diagnosing which subtree blanked (SF-11). It is
    // `string | null` in React 19.
    const componentStack = errorInfo.componentStack ?? "";
    logger.error(
      scope,
      `render error: ${detail}${componentStack ? `\ncomponent stack:${componentStack}` : ""}`,
    );
    this.props.onError?.(error);
  }

  render(): ReactNode {
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}
