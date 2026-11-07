// SPDX-License-Identifier: MIT
// K11 harness states (SV-CDE-86): the six states every dashboard is reviewed in, as fixtures a Playwright test (or a page opened
// with ?state=...) applies to a fake host. A dashboard ships a screenshot of each, so states are reviewed before code.
//
//   cold     nothing cached; every read takes `latencyMs`
//   warm     the last answers are in storage (`seed`); reads still take `latencyMs`, so the page must paint from cache first
//   section  warm, and section reads take `sectionLatencyMs`, long enough to show "Still reading"
//   switch   warm, and a second selection (`switchTo`) whose reads are slow, so the first scope's late answers must be dropped
//   offline  nothing cached and every read fails: the page shows its offline state, never an error wall
//   return   warm, then the page is hidden for `hiddenMs` (10 minutes) and shown again: zero reads, the DOM identical
//
// Plain data and strings only: no dependencies, usable from a test or from a page's own ?state= handler.
export const STATES = ["cold", "warm", "section", "switch", "offline", "return"];

export function stateFromSearch(search) {
  const state = new URLSearchParams(search).get("state");
  return STATES.includes(state) ? state : null;
}

// `seed` is {storageKey: entry}: what cachedQuery wrote on the previous visit ({at, value}).
export function stateFixture(
  state,
  { seed = {}, latencyMs = 400, sectionLatencyMs = 20_000, hiddenMs = 600_000 } = {},
) {
  if (!STATES.includes(state)) throw new Error(`unknown dashboard state: ${state}`);
  const warm = state === "warm" || state === "section" || state === "switch" || state === "return";
  return {
    state,
    storage: warm ? seed : {},
    latencyMs,
    sectionLatencyMs: state === "section" || state === "switch" ? sectionLatencyMs : latencyMs,
    offline: state === "offline",
    switchTo: state === "switch",
    hiddenMs: state === "return" ? hiddenMs : 0,
  };
}

// The tab switch of the `return` state, run inside the dashboard's own frame: hidden (document, window), then shown again. Between the
// two the test advances its virtual clock by the fixture's hiddenMs.
export const hideScript = () => `(() => {
  Object.defineProperty(document, "hidden", { value: true, configurable: true });
  Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
  window.dispatchEvent(new Event("blur"));
})()`;
export const showScript = () => `(() => {
  Object.defineProperty(document, "hidden", { value: false, configurable: true });
  Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
  window.dispatchEvent(new Event("focus"));
  window.dispatchEvent(new Event("pageshow"));
  window.dispatchEvent(new Event("online"));
})()`;

// The script a fake host runs (in the shell frame that stands for the app or the share viewer). It answers reads from `answers`
// ({name: value | (args) => value}) after the fixture's latency, logs every read in window.__reads, and tracks the most reads
// ever in flight in window.__maxInFlight (the saturation proof). Timers are the page's own, so page.clock drives them.
export function bridgeScript(fixture) {
  return `(() => {
    const fx = ${JSON.stringify(fixture)};
    window.__reads = [];
    window.__inFlight = 0;
    window.__maxInFlight = 0;
    window.__answers = window.__answers || {};
    window.__bridge = (name, args) => new Promise((resolve, reject) => {
      window.__reads.push({ name, args });
      window.__inFlight++;
      window.__maxInFlight = Math.max(window.__maxInFlight, window.__inFlight);
      const slow = /^(sessionRead|storyChanges)$/.test(name);
      setTimeout(() => {
        window.__inFlight--;
        if (fx.offline) return reject(new Error("no_answer"));
        const answer = window.__answers[name];
        resolve(typeof answer === "function" ? answer(args) : answer);
      }, slow ? fx.sectionLatencyMs : fx.latencyMs);
    });
  })();`;
}

// An init script that puts the fixture's storage in place before the page's own scripts run.
export function storageScript(fixture) {
  // A shared page's frame is sandboxed (an opaque origin): it has no storage, and seeding it must not break the page.
  return `(() => { try { const entries = ${JSON.stringify(fixture.storage)}; for (const [k, v] of Object.entries(entries)) localStorage.setItem(k, JSON.stringify(v)); } catch (e) { /* no storage here */ } })();`;
}

// One screenshot per state: `step(state)` puts the page in that state; the file is dir/<name>-<state>.png.
export async function shootStates(page, dir, name, step, states = STATES) {
  const files = [];
  for (const state of states) {
    await step(state);
    const path = `${dir}/${name}-${state}.png`;
    await page.screenshot({ path, fullPage: true });
    files.push(path);
  }
  return files;
}
