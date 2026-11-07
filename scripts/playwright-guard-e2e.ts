// SPDX-License-Identifier: MIT
import { guardConfigServer } from "./playwright-worktree-guard";

export default () => guardConfigServer("http://localhost:1420");
