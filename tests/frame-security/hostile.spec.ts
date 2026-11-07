// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildViewerUrl } from "../../src/shared/lib/viewer-file-type";
import { resolveFrameFixtureBinary } from "./support/fixture-guard.mjs";

let rust: ChildProcess;
let host: Server;
let scratch: string;
let port: number;
let hostUrl: string;
let urls: string[];
let records: Array<{
  origin: string;
  token: string;
  contentRoot: string;
  documentPath: string;
  parentFrame?: string;
}>;
let setupError: Error | undefined;

// A beforeAll throw fails one test and leaves the rest unrun, so setup records its
// error and beforeEach fails every security test with it.
test.beforeAll(async () => {
  try {
    await startFixtures();
  } catch (error) {
    setupError = error instanceof Error ? error : new Error(String(error));
  }
});

async function startFixtures() {
  scratch = await mkdtemp(join(tmpdir(), "codesense-frame-"));
  host = createServer((req, res) => {
    if (req.url?.startsWith("/outside")) {
      res.setHeader("access-control-allow-origin", "*");
      res.end("outside");
      return;
    }
    if (new URL(req.url ?? "/", "http://host").searchParams.has("baseline")) {
      const root = Buffer.from(records[0].contentRoot).toString("base64url");
      res.setHeader("content-type", "text/html");
      res.end(`<script>window.baselineReports=[];addEventListener('message',e=>{
        if(e.data.type==='baseline-rendered')baselineReports.push({origin:e.origin,ready:e.data.ready})
      })</script><iframe src="${records[5].origin}/viewer-baseline/${root}/baseline/baseline.html"></iframe>`);
      return;
    }
    const index = Number(new URL(req.url ?? "/", "http://host").searchParams.get("index") ?? 0);
    const shownUrls = [urls[index], urls[1]];
    const shownRecords = [records[index], records[1]];
    res.setHeader("content-type", "text/html");
    res.end(`<iframe name="team" sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads" src="${shownUrls[0]}"></iframe>
      <iframe name="private" sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads" src="${shownUrls[1]}"></iframe>
      <script>window.records=${JSON.stringify(shownRecords)};window.calls=[];addEventListener('message',e=>{
        var child=${JSON.stringify(records[7])};
        if(e.origin===child.origin&&e.data.token===child.token){
          if(e.source!==frames[0].frames[0])return;
          // Match HtmlViewer's ready handshake: the real live-hub shim defers calls while hidden.
          if(e.data.type==='codesense:ready')e.source.postMessage({type:'codesense:visibility',visible:true,token:child.token},child.origin);
          if(e.data.type==='codesense:invoke'){calls.push(e.data);e.source.postMessage({type:'codesense:invoke-result',id:e.data.id,ok:true,data:{identity:'message'}},child.origin)}
          return;
        }
        if(e.source!==frames[0]&&e.source!==frames[1])return;var i=e.source===frames[1]?1:0;if(e.origin!==records[i].origin||e.data.token!==records[i].token)return;if(e.data.type==='codesense:ready')e.source.postMessage({type:'codesense:visibility',visible:true,token:records[i].token},records[i].origin);if(e.data.type==='codesense:invoke'){calls.push(e.data);e.source.postMessage({type:'codesense:invoke-result',id:e.data.id,ok:true,data:{identity:e.source===frames[1]?'private':'team'}},e.origin)}})</script>`);
  });
  await new Promise<void>((r) => host.listen(0, "127.0.0.1", r));
  hostUrl = `http://127.0.0.1:${(host.address() as { port: number }).port}`;
  const binary = resolveFrameFixtureBinary();
  if (!binary.path) throw new Error(binary.error);
  rust = spawn(binary.path, ["hostile_fixture_server", "--nocapture"], {
    env: {
      ...process.env,
      CODESENSE_FRAME_FIXTURE: join(scratch, "port"),
      CODESENSE_FIXTURE_ALLOWED_ORIGIN: hostUrl,
    },
    stdio: "pipe",
  });
  let output = "";
  let spawnError: Error | undefined;
  rust.once("error", (e) => {
    spawnError = e;
  });
  rust.stderr?.on("data", (b) => {
    output += b;
  });
  for (let i = 0; i < 200; i++) {
    try {
      const info = JSON.parse(await readFile(join(scratch, "port"), "utf8"));
      port = info.port;
      records = info.records;
      break;
    } catch {
      /* The server writes readiness after binding both listeners. */
    }
    if (spawnError) throw new Error(`Fixture server failed to start: ${spawnError.message}`);
    if (rust.exitCode !== null) throw new Error(`Fixture server exited: ${output}`);
    await new Promise((r) => setTimeout(r, 50));
  }
  if (!port) throw new Error("Fixture server did not start");
  urls = records.map((frame) =>
    buildViewerUrl(port, frame.contentRoot, frame.documentPath, { origin: frame.origin }),
  );
}
test.afterAll(async () => {
  rust?.kill("SIGTERM");
  await new Promise<void>((r) => (host ? host.close(() => r()) : r()));
  if (scratch) await rm(scratch, { recursive: true, force: true });
});
test.beforeEach(async ({ page }) => {
  if (setupError) throw setupError;
  await page.goto(hostUrl);
  await expect.poll(() => page.frames().filter((f) => f.url().includes("/viewer/")).length).toBe(2);
  await page.frames()[1].waitForFunction(() => !!(window as any).codesense);
});

test("a: sibling private DOM is isolated", async ({ page }) => {
  const result = await page.frames()[1].evaluate(() => {
    try {
      return parent.frames[1].document.body.dataset.private ?? "denied";
    } catch {
      return "denied";
    }
  });
  expect(result).toBe("denied");
});
test("b: a sibling SDK cannot act as the private frame", async ({ page }) => {
  const result = await page.frames()[1].evaluate(async () => {
    try {
      return await (parent.frames[1] as any).codesense.sessions();
    } catch {
      return "denied: sibling";
    }
  });
  expect(result).toBe("denied: sibling");
});
test("c: captured token cannot be replayed at a different frame", async ({ page, request }) => {
  const token = await page.frames()[1].evaluate(() => (window as any).__codesenseProxy.token);
  const response = await request.get(`http://127.0.0.1:${port}/api/v1/sprints`, {
    headers: {
      host: new URL(records[1].origin).host,
      origin: records[0].origin,
      "x-codesense-proxy-token": token,
    },
  });
  expect(response.status()).toBe(403);
});
test("d: viewer fetches identify their caller; restricted frames refuse foreign/private bytes", async ({
  page,
}) => {
  const targets = ["private/data.txt", "open/data.txt"].map(
    (name) =>
      new URL(
        `/viewer/${Buffer.from(records[0].contentRoot).toString("base64url")}/${name}`,
        records[0].origin,
      ).href,
  );
  const statuses = await page.frames()[1].evaluate(async (targets) => {
    const px = (window as any).__codesenseProxy;
    return Promise.all(
      targets.map(
        async (url) =>
          (
            await fetch(url, {
              headers: { "x-codesense-proxy-token": px.token },
            })
          ).status,
      ),
    );
  }, targets);
  expect(statuses).toEqual([403, 403]);
});
test("e: nested frames cannot load a sibling live document", async ({ page }) => {
  const response = await page.frames()[1].evaluate(async (url) => {
    const child = document.createElement("iframe");
    child.src = url;
    document.body.append(child);
    try {
      return (await fetch(url)).status;
    } catch {
      return 403;
    }
  }, urls[1]);
  expect(response).toBe(403);
});
test("f: a rebinding Host and foreign Origin are refused", async ({ request }) => {
  for (const headers of [
    { host: `attacker.example:${port}` },
    { host: `localhost:${port}`, origin: "https://attacker.example" },
  ]) {
    expect((await request.get(`http://127.0.0.1:${port}/health`, { headers })).status()).toBe(403);
  }
});
test("g: popups open but get no token or bridge authority", async ({ page }) => {
  const popupPromise = page.waitForEvent("popup");
  await page.frames()[1].evaluate(() => window.open("about:blank"));
  const popup = await popupPromise;
  expect(await popup.evaluate(() => (window as any).__codesenseProxy?.token ?? null)).toBeNull();
  expect(await popup.evaluate(() => window.opener)).toBeNull();
  expect(await popup.evaluate(() => (window.opener as any)?.codesense.sessions() ?? "denied")).toBe(
    "denied",
  );
  await popup.close();
});
test("relative fetch and XHR retain their frame credential under cross-site embedding", async ({
  page,
}) => {
  const frame = page.frames()[1];
  const root = Buffer.from(records[0].contentRoot).toString("base64url");
  const result = await frame.evaluate(async (root) => {
    const url = `/viewer/${root}/team/data.txt`;
    const response = await fetch(url);
    const xhrResult = await new Promise<string>((resolve) => {
      const xhr = new XMLHttpRequest();
      xhr.open("GET", url);
      xhr.onload = () => resolve(`${xhr.status}:${xhr.responseText}`);
      xhr.send();
    });
    return [response.status, await response.text(), xhrResult];
  }, root);
  expect(result).toEqual([200, "private-bytes", "200:private-bytes"]);
});
test("neither frame URLs nor served documents contain the process-wide secret", async ({
  request,
}) => {
  for (const [index, url] of urls.entries()) {
    expect(url).not.toContain("process-secret");
    const response = await request.get(url.replace(new URL(url).hostname, "127.0.0.1"), {
      headers: {
        host: new URL(url).host,
        origin: records[index].origin,
        "x-codesense-proxy-token": records[index].token,
      },
    });
    expect(response.status()).toBe(200);
    expect(await response.text()).not.toContain("process-secret");
  }
});

test("open policy keeps cross-scope file reads attributed to the open frame", async ({ page }) => {
  await page.goto(`${hostUrl}/?index=2`);
  const own = page.frames()[1];
  await own.waitForFunction(() => !!(window as any).codesense);
  expect(await own.evaluate(() => (window as any).codesense.sessions())).toEqual({
    identity: "team",
  });
  const root = Buffer.from(records[2].contentRoot).toString("base64url");
  const result = await own.evaluate(async (root) => {
    const px = (window as any).__codesenseProxy;
    const headers = { "x-codesense-proxy-token": px.token };
    const response = await fetch(`/viewer/${root}/private/data.txt`, { headers });
    const identity = await (await fetch("/api/v1/sprints", { headers })).json();
    return { bytes: await response.text(), identity };
  }, root);
  expect(result.bytes).toBe("private-bytes");
  expect(result.identity.identity).toBe("open");
  expect(result.identity.contentRoot).toBe(records[2].contentRoot);
});
test("service-worker blocking is opt-in and blocks registration in the shim", async ({
  page,
  request,
}) => {
  await page.goto(`${hostUrl}/?index=3`);
  const frame = page.frames()[1];
  await frame.waitForFunction(() => !!(window as any).codesense);
  const result = await frame.evaluate(async () => {
    if (!navigator.serviceWorker) return "unavailable";
    try {
      await navigator.serviceWorker.register("/worker.js");
      return "allowed";
    } catch (e) {
      return (e as Error).message;
    }
  });
  // An engine without service workers already refuses them; an engine that
  // exposes registration must hit the policy shim before making a request.
  expect(result === "unavailable" || result.startsWith("denied:")).toBe(true);
  for (const [index, status] of [
    [2, 200],
    [3, 403],
  ]) {
    const frame = records[index];
    const response = await request.get(`http://127.0.0.1:${port}/worker.js`, {
      headers: {
        host: new URL(frame.origin).host,
        origin: frame.origin,
        "service-worker": "script",
        "x-codesense-proxy-token": frame.token,
      },
    });
    expect(response.status()).toBe(status);
  }
});
test("outside network stays open by default; the switch allows declared hosts only", async ({
  page,
}) => {
  const fetchFrom = async (index: number, url: string) => {
    await page.goto(`${hostUrl}/?index=${index}`);
    const frame = page.frames()[1];
    await frame.waitForFunction(() => !!(window as any).codesense);
    return frame.evaluate(async (url) => {
      try {
        return await (await fetch(url)).text();
      } catch {
        return "blocked";
      }
    }, url);
  };
  expect(await fetchFrom(2, `${hostUrl}/outside`)).toBe("outside");
  expect(await fetchFrom(4, `${hostUrl}/outside`)).toBe("outside");
  expect(await fetchFrom(4, `${hostUrl.replace("127.0.0.1", "localhost")}/outside`)).toBe(
    "blocked",
  );
});

test("kit scripts and styles load through the same frame boundary", async ({ page }) => {
  await expect
    .poll(() => page.frames()[1].evaluate(() => !!(window as any).codesense?.kit))
    .toBe(true);
  const styles = await page
    .frames()[1]
    .evaluate(() => [...document.styleSheets].some((s) => s.href?.includes("/kit/")));
  expect(styles).toBe(true);
});

test("open frame HTTP reads honor an explicit managed content root", async ({ page }) => {
  await page.goto(`${hostUrl}/?index=2`);
  const frame = page.frames()[1];
  await frame.waitForFunction(() => !!(window as any).codesense);
  const target = records[2].contentRoot + "/private";
  const roots = await frame.evaluate(
    async (target) =>
      Promise.all(
        ["sprints", "taxonomy"].map(
          async (route) =>
            (
              await (
                await fetch(`/api/v1/${route}?content_root=${encodeURIComponent(target)}`)
              ).json()
            ).contentRoot,
        ),
      ),
    target,
  );
  expect(roots).toEqual([target, target]);
});

test("noOutsideNetwork alone blocks service-worker registration and script requests", async ({
  page,
  request,
}) => {
  for (const [index, expected] of [
    [2, "allowed"],
    [4, "denied:"],
  ] as const) {
    await page.goto(`${hostUrl}/?index=${index}`);
    const frame = page.frames()[1];
    await frame.waitForFunction(() => !!(window as any).codesense);
    const result = await frame.evaluate(async () => {
      if (!navigator.serviceWorker) return "unavailable";
      try {
        await navigator.serviceWorker.register("/network-worker.js");
        return "allowed";
      } catch (error) {
        return (error as Error).message;
      }
    });
    expect(result).toMatch(new RegExp(`^${expected}`));
    const record = records[index];
    const response = await request.get(`http://127.0.0.1:${port}/network-worker.js`, {
      headers: {
        host: new URL(record.origin).host,
        origin: record.origin,
        "service-worker": "script",
        "x-codesense-proxy-token": record.token,
      },
    });
    expect(response.status()).toBe(index === 2 ? 200 : 403);
  }
});

test("file fixture preserves production JSON response types", async ({ page }) => {
  const root = Buffer.from(records[0].contentRoot).toString("base64url");
  const type = await page
    .frames()[1]
    .evaluate(
      async (root) => (await fetch(`/viewer/${root}/team/data.json`)).headers.get("content-type"),
      root,
    );
  expect(type).toBe("application/json");
});

test("a restricted frame cannot relay private blobs through a loopback baseline iframe", async ({
  page,
}) => {
  const root = Buffer.from(records[0].contentRoot).toString("base64url");
  const result = await page.frames()[1].evaluate(async (url) => {
    return new Promise<string>((resolve) => {
      const child = document.createElement("iframe");
      const listener = (e: MessageEvent) => {
        if (e.source === child.contentWindow && e.data.type === "baseline-read") {
          window.removeEventListener("message", listener);
          resolve(e.data.bytes);
        }
      };
      window.addEventListener("message", listener);
      child.src = url;
      document.body.append(child);
      setTimeout(() => {
        window.removeEventListener("message", listener);
        resolve("blocked");
      }, 350);
    });
  }, `http://localhost:${port}/viewer-baseline/${root}/team/baseline.html`);
  expect(result).toBe("blocked");
});

test("host baseline comparison still runs scripts with an opaque origin", async ({
  page,
  request,
}) => {
  const root = Buffer.from(records[0].contentRoot).toString("base64url");
  const response = await request.get(
    `http://127.0.0.1:${port}/viewer-baseline/${root}/team/baseline.html`,
    {
      headers: {
        host: new URL(records[0].origin).host,
        origin: records[0].origin,
        "x-codesense-proxy-token": records[0].token,
      },
    },
  );
  expect(response.status()).toBe(200);
  expect(response.headers()["content-security-policy"]).toContain("sandbox allow-scripts");
  await page.goto(`${hostUrl}/?baseline=1`);
  await expect
    .poll(() => page.evaluate(() => (window as any).baselineReports))
    .toEqual([{ origin: "null", ready: true }]);
});

test("classic scripts cannot obtain outside-folder baseline bytes through loopback", async ({
  page,
}) => {
  const root = Buffer.from(records[0].contentRoot).toString("base64url");
  const result = await page.frames()[1].evaluate(async (src) => {
    const status = await new Promise<string>((resolve) => {
      const script = document.createElement("script");
      script.onload = () => resolve("loaded");
      script.onerror = () => resolve("blocked");
      script.src = src;
      document.body.append(script);
    });
    return [status, (window as any).loopbackLeak ?? null];
  }, `http://localhost:${port}/viewer-baseline/${root}/private/outside.js`);
  expect(result).toEqual(["blocked", null]);
});

test("loopback health, Claude hooks and authenticated API clients still work", async ({
  request,
}) => {
  const url = `http://127.0.0.1:${port}`;
  expect((await request.get(`${url}/health`)).status()).toBe(200);
  expect(
    (await request.post(`${url}/api/v1/terminal/status`, { data: { event: "Stop" } })).status(),
  ).toBe(200);
  expect(
    (
      await request.get(`${url}/api/v1/sprints`, {
        headers: { "x-codesense-proxy-token": "process-secret" },
      })
    ).status(),
  ).toBe(200);
  expect((await request.get(`${url}/api/v1/sprints`)).status()).toBe(401);
});

for (const baseline of [false, true]) {
  for (const [tag, file] of [
    ["audio", "silence.wav"],
    ["video", "black.mp4"],
  ] as const) {
    test(`native ${tag} loads ${baseline ? "baseline" : "default-open live"} media without shim credentials`, async ({
      page,
      context,
    }) => {
      await page.goto(`${hostUrl}/?${baseline ? "baseline=1" : "index=2"}`);
      const frame = page
        .frames()
        .find((f) => f.url().includes(baseline ? "/viewer-baseline/" : "/open.html"))!;
      expect(frame).toBeTruthy();
      await context.clearCookies();
      const responsePromise = page.waitForResponse((r) => r.url().endsWith(`/${file}`));
      const loaded = frame.evaluate(
        ({ tag, file }) =>
          new Promise<{ event: string; duration: number }>((resolve) => {
            const media = document.createElement(tag) as HTMLMediaElement;
            media.preload = "metadata";
            media.muted = true;
            media.onloadedmetadata = () =>
              resolve({ event: "loadedmetadata", duration: media.duration });
            media.onerror = () =>
              resolve({ event: `error ${media.error?.code} ${media.error?.message}`, duration: 0 });
            media.src = `./${file}`;
            document.body.append(media);
            media.load();
          }),
        { tag, file },
      );
      expect((await responsePromise).status()).toBe(200);
      expect(await loaded).toMatchObject({ event: "loadedmetadata", duration: expect.any(Number) });
      expect((await loaded).duration).toBeGreaterThan(0);
    });
  }
  test(`track destinations use the ${baseline ? "baseline" : "live"} frame asset policy`, async ({
    request,
  }) => {
    const record = records[baseline ? 5 : 0];
    const root = Buffer.from(record.contentRoot).toString("base64url");
    const prefix = baseline ? "viewer-baseline" : "viewer";
    const folder = baseline ? "baseline" : "team";
    const get = (path: string) =>
      request.get(`http://127.0.0.1:${port}/${prefix}/${root}/${path}`, {
        headers: {
          host: new URL(record.origin).host,
          "sec-fetch-dest": "track",
          ...(baseline ? {} : { "sec-fetch-site": "same-origin" }),
        },
      });
    expect((await get(`${folder}/captions.vtt`)).status()).toBe(200);
    expect((await get("private/captions.vtt")).status()).toBe(403);
  });
}

test("minted inline children are isolated and call the host directly with their own identity", async ({
  page,
}) => {
  await page.goto(`${hostUrl}/?index=6`);
  const pane = page.frames().find((f) => f.url().includes("/inline/pane.html"))!;
  await expect
    .poll(() => page.frames().some((f) => f.url().includes("/inline/message.html")))
    .toBe(true);
  const child = page.frames().find((f) => f.url().includes("/inline/message.html"))!;
  await child.waitForFunction(() => !!(window as any).codesense);
  expect(
    await child.evaluate(() => {
      try {
        return parent.document.body.textContent;
      } catch {
        return "isolated";
      }
    }),
  ).toBe("isolated");
  expect(
    await child.evaluate(() => {
      try {
        return !!(parent as any).codesense;
      } catch {
        return "isolated";
      }
    }),
  ).toBe("isolated");
  expect(await pane.evaluate(() => getComputedStyle(document.body).color)).toBe("rgb(0, 0, 255)");
  await child.locator("#reply").click();
  await expect(child.locator("body")).toHaveAttribute("data-result", "message");
  expect(await page.evaluate(() => (window as any).calls.at(-1))).toMatchObject({
    command: "replyAsHuman",
    args: { messageId: 7, text: "Reply" },
    token: records[7].token,
  });
  const count = await page.evaluate(() => (window as any).calls.length);
  await pane.evaluate(
    (token) =>
      top!.postMessage(
        {
          token,
          type: "codesense:invoke",
          id: 999,
          command: "replyAsHuman",
          args: { messageId: 7 },
        },
        "*",
      ),
    records[7].token,
  );
  await page.waitForTimeout(50);
  expect(await page.evaluate(() => (window as any).calls.length)).toBe(count);
});
