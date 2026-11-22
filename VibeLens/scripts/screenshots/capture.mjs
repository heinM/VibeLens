#!/usr/bin/env node
// README screenshots: renders the real UI in headless Google Chrome with the Tauri backend
// replaced by a demo stand-in, and writes one PNG per scene to docs/images/.
//
// How it works
//   * vite.config.ts (this folder) serves the app as `npm run dev` does, on port 1431, with
//     entry.ts loaded before src/main.tsx. entry.ts installs mock-ipc.ts (demo data in the exact
//     return types of src/ipc/types.ts) through @tauri-apps/api's own mockIPC.
//   * scenes.ts drives the app into each state; scene-list.ts names the scenes and their files.
//   * Result rows come from data/*.json, copied from the seeded dev databases by refresh-data.mjs.
//   * This script starts that dev server and Chrome (Chrome DevTools Protocol over Node's built-in
//     WebSocket, no Puppeteer), captures 1440 x 900 CSS pixels at device scale factor 2, and stops
//     both again.
//
// Rerun (from VibeLens/, after `npm install`):
//   node scripts/screenshots/capture.mjs              # every scene
//   node scripts/screenshots/capture.mjs mysql sso    # some scenes
//   node scripts/screenshots/capture.mjs --out /tmp/shots
//   CHROME=/path/to/chrome node scripts/screenshots/capture.mjs
// Then look at every PNG before committing it.
//
// Nothing here reads the machine's AWS config, keychain or app profiles: the app talks only to
// the stand-in, and Chrome runs with a fresh temporary profile.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { SCENES } from "./scene-list.ts";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = resolve(here, "../..");
const PORT = 1431;
const WIDTH = 1440;
const HEIGHT = 900;
const SCALE = 2;

const argv = process.argv.slice(2);
const outIdx = argv.indexOf("--out");
const outDir = outIdx >= 0 ? resolve(argv[outIdx + 1]) : resolve(appRoot, "../docs/images");
const wanted = argv.filter((a, i) => !a.startsWith("--") && !(outIdx >= 0 && i === outIdx + 1));
const names = wanted.length > 0 ? wanted : Object.keys(SCENES);
for (const n of names) if (!SCENES[n]) throw new Error(`unknown scene ${n}; known: ${Object.keys(SCENES).join(", ")}`);

const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitHttp(url, ms) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {
      /* not up yet */
    }
    if (Date.now() > end) throw new Error(`${url} did not come up`);
    await sleep(250);
  }
}

function startVite() {
  const vite = spawn(process.execPath, [join(appRoot, "node_modules/vite/bin/vite.js"), "--config", join(here, "vite.config.ts")], {
    cwd: appRoot,
    stdio: ["ignore", "pipe", "pipe"],
  });
  vite.stderr.on("data", (d) => process.stderr.write(`[vite] ${d}`));
  return vite;
}

async function startChrome() {
  const profile = mkdtempSync(join(tmpdir(), "vibelens-shots-"));
  const chrome = spawn(
    CHROME,
    [
      "--headless=new",
      "--remote-debugging-port=0",
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-extensions",
      "--disable-sync",
      "--hide-scrollbars",
      "--force-color-profile=srgb",
      "--font-render-hinting=none",
      `--window-size=${WIDTH},${HEIGHT}`,
      "about:blank",
    ],
    { stdio: "ignore" },
  );
  const portFile = join(profile, "DevToolsActivePort");
  const end = Date.now() + 20000;
  while (!existsSync(portFile) || readFileSync(portFile, "utf8").split("\n").length < 2) {
    if (Date.now() > end) throw new Error("Chrome did not start");
    await sleep(100);
  }
  const [port, path] = readFileSync(portFile, "utf8").split("\n");
  return { chrome, profile, ws: `ws://127.0.0.1:${port}${path}` };
}

/** A minimal CDP client over the browser WebSocket, with flattened per-target sessions. */
function cdp(url) {
  const ws = new WebSocket(url);
  let seq = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(`${msg.error.message} ${msg.error.data ?? ""}`));
      else resolve(msg.result);
    }
  };
  const ready = new Promise((r, j) => {
    ws.onopen = r;
    ws.onerror = j;
  });
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const id = ++seq;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return { ready, send, close: () => ws.close() };
}

async function capture(client, name) {
  const { file } = SCENES[name];
  const { browserContextId } = await client.send("Target.createBrowserContext");
  const { targetId } = await client.send("Target.createTarget", { url: "about:blank", browserContextId });
  const { sessionId } = await client.send("Target.attachToTarget", { targetId, flatten: true });
  const s = (m, p) => client.send(m, p, sessionId);
  try {
    await s("Page.enable");
    await s("Runtime.enable");
    await s("Emulation.setDeviceMetricsOverride", { width: WIDTH, height: HEIGHT, deviceScaleFactor: SCALE, mobile: false });
    await s("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: "dark" }] });
    await s("Page.navigate", { url: `http://127.0.0.1:${PORT}/?scene=${name}` });
    const end = Date.now() + 60000;
    let state;
    for (;;) {
      const r = await s("Runtime.evaluate", {
        expression: "JSON.stringify({ done: !!window.__sceneDone, error: window.__sceneError ?? null, log: window.__mockLog ?? [] })",
        returnByValue: true,
      });
      state = JSON.parse(r.result.value ?? "{}");
      if (state.done || state.error) break;
      if (Date.now() > end) {
        state.error = "scene did not finish within 60 s";
        break;
      }
      await sleep(250);
    }
    for (const line of state.log ?? []) console.warn(`  [${name}] stand-in: ${line}`);
    const shot = await s("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
    if (state.error) {
      // Keep what the page showed, for debugging, under a name the README never uses.
      writeFileSync(join(tmpdir(), `vibelens-shot-${name}-FAILED.png`), Buffer.from(shot.data, "base64"));
      throw new Error(`${name}: ${state.error} (page saved to ${join(tmpdir(), `vibelens-shot-${name}-FAILED.png`)})`);
    }
    const target = join(outDir, file);
    writeFileSync(target, Buffer.from(shot.data, "base64"));
    console.log(`  ${name} -> ${target}`);
  } finally {
    await client.send("Target.closeTarget", { targetId }).catch(() => {});
    await client.send("Target.disposeBrowserContext", { browserContextId }).catch(() => {});
  }
}

const vite = startVite();
let chrome;
let failed = 0;
try {
  mkdirSync(outDir, { recursive: true });
  await waitHttp(`http://127.0.0.1:${PORT}/`, 30000);
  chrome = await startChrome();
  const client = cdp(chrome.ws);
  await client.ready;
  for (const n of names) {
    try {
      await capture(client, n);
    } catch (e) {
      failed++;
      console.error(`  FAILED ${e.message}`);
    }
  }
  client.close();
} finally {
  if (chrome) {
    chrome.chrome.kill();
    await sleep(300);
    rmSync(chrome.profile, { recursive: true, force: true });
  }
  vite.kill();
}
process.exit(failed ? 1 : 0);
