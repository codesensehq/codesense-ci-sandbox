// SPDX-License-Identifier: MIT
import { guardConfigServer } from "./playwright-worktree-guard";

export default () => guardConfigServer(`http://localhost:${Number(process.env.PORT ?? 5198)}`);
