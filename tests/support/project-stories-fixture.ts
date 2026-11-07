// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { buildStoriesRows } from "../../src/features/dashboard/lib/stories-projection";
import { useStatusTaxonomyStore } from "../../src/shared/stores/status-taxonomy-store";

// The Rust streaming seam executes the same adapter the native scan owner publishes.
const input = JSON.parse(readFileSync(0, "utf8"));
useStatusTaxonomyStore.getState().setStates(input.states);
process.stdout.write(JSON.stringify(buildStoriesRows(input.tree, {})));
