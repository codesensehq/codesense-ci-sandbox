# CodeSense Test Framework

## Overview

CodeSense uses a multi-level testing strategy:

| Level       | Framework           | Location              | Purpose                                     |
| ----------- | ------------------- | --------------------- | ------------------------------------------- |
| Unit        | Vitest + happy-dom  | `src/**/*.test.ts(x)` | Store logic, utilities, component rendering |
| Integration | Vitest + mock Tauri | `src/**/*.test.ts(x)` | Store↔command interactions                  |
| E2E         | Playwright          | `tests/e2e/*.spec.ts` | Full app flows in browser                   |
| Rust Unit   | cargo test          | `src-tauri/src/**`    | Backend operations                          |

## Setup

```bash
# Install Playwright browsers (first time only)
pnpm exec playwright install chromium

# Install project dependencies (if not done)
pnpm install --frozen-lockfile
```

## Running Tests

```bash
# Unit + component tests (Vitest)
pnpm test

# E2E tests (Playwright)
pnpm run test:e2e                 # Headless
pnpm run test:e2e:headed          # With browser
pnpm run test:e2e:ui              # Playwright UI mode
pnpm run test:e2e:mock            # Layout specs on the mock backend (:5198, or PORT=<free port>)

# Rust tests
cd src-tauri && cargo test
```

Playwright reuses a dev server already on its port, but only one started from **this** worktree
(dev-only `/__cs-worktree` marker). A server from another checkout fails the run with
its root and pid: stop it, use another `PORT=` (mock config), or start your own server and set
`BASE_URL`, which skips the check. A server whose marker cannot be read within 10s also fails
the run rather than being reused unverified.

## Architecture

```
tests/
├── e2e/                          # Playwright E2E specs
│   ├── app-launch.spec.ts        # App startup tests
│   └── …                         # Keyboard, theme, accessibility and other flows
├── support/
│   ├── fixtures/
│   │   └── index.ts              # Extended Playwright fixtures (appReady)
│   ├── helpers/
│   │   └── tauri-mock.ts         # Vitest Tauri invoke mock utilities
│   └── factories/
│       └── repo-factory.ts       # RepoState factory
└── README.md                     # This file

src/**/*.test.ts(x)               # Co-located Vitest unit/component tests
```

## Test Data Factories

Factories create unique test data with sensible defaults and overrides:

```typescript
import { createRepoState } from "../support/factories/repo-factory";

// Defaults
const repo = createRepoState();

// With overrides
const customRepo = createRepoState({ repoName: "my-project", hasRemote: true });
```

## Tauri Mock Helper (Vitest)

For integration tests that need to mock Tauri commands:

```typescript
import { createTauriMock, mockCommand } from "../support/helpers/tauri-mock";

beforeEach(() => {
  createTauriMock();
  mockCommand("check_path_exists", true);
  mockCommand("load_app_state", '{"repos":[]}');
});
```

## Best Practices

1. **No hard waits** — use `waitForSelector`, `expect().toBeVisible()`, or Playwright auto-waiting
2. **Tests < 300 lines** — break large tests into focused scenarios
3. **Tests < 1.5 min** — if slower, split or optimize
4. **Self-cleaning** — factories create unique data, no shared state between tests
5. **Parallel-safe** — no global state mutations, no shared temp directories
6. **Explicit assertions** — every test body must have at least one `expect()`
7. **Given/When/Then** — structure tests as setup → action → assertion

## Test results

Playwright writes its HTML report to `playwright-report/`. On CI it also writes JUnit XML to
`test-results/e2e-results.xml`.
