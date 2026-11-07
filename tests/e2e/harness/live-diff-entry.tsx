// SPDX-License-Identifier: MIT
import { createRoot } from "react-dom/client";
import { useSyncExternalStore } from "react";

import { CodeDiffReview } from "@/shared/components/code-editor";

/**
 * SLW-DV-5 real-layout harness entry. Renders the REAL `<CodeDiffReview>` in a
 * fixed-size scroll region and exposes `window.__diffHarness.set(partialProps)`
 * so a Playwright spec can drive live before/after updates and observe the
 * flash / in-place-patch / follow-scroll behavior under a real browser layout.
 *
 * Props flow through a tiny external store (`useSyncExternalStore`) so the setter
 * lives outside React — driveable from `page.evaluate` — with no render-phase
 * global writes and no ready/commit race. NOT wrapped in StrictMode: its
 * double-invoke would construct the CodeMirror view twice, adding nondeterminism.
 */
interface HarnessProps {
  before: string;
  after: string;
  path: string;
  mode: "split" | "inline";
  autoFollow: boolean;
}

let state: HarnessProps = {
  before: "",
  after: "",
  path: "harness.ts",
  mode: "split",
  autoFollow: true,
};
const subscribers = new Set<() => void>();

function setProps(partial: Partial<HarnessProps>): void {
  state = { ...state, ...partial };
  subscribers.forEach((notify) => notify());
}

function subscribe(notify: () => void): () => void {
  subscribers.add(notify);
  return () => subscribers.delete(notify);
}

function Harness(): React.ReactElement {
  const props = useSyncExternalStore(subscribe, () => state);
  return (
    // A viewport shorter than the content so the merge-view frame owns a real,
    // scrollable overflow — the WD-7 outer scroller the follow/anchor drive.
    <div data-testid="harness-viewport" style={{ height: "360px", width: "820px" }}>
      <CodeDiffReview
        before={props.before}
        after={props.after}
        path={props.path}
        mode={props.mode}
        theme="light"
        fontSize={13}
        autoFollow={props.autoFollow}
      />
    </div>
  );
}

createRoot(document.getElementById("harness-root") as HTMLElement).render(<Harness />);

// Exposed for the Playwright spec to drive live prop updates.
(window as unknown as { __diffHarness: unknown }).__diffHarness = { set: setProps, ready: true };
