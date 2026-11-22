// Serves the app exactly as `npm run dev` does, with one extra module (entry.ts) loaded before
// src/main.tsx. That module replaces the Tauri IPC with the demo stand-in. Port 1431, so it never
// collides with the real dev server on 1420. Used by capture.mjs; see the comment there.
import { defineConfig, mergeConfig, type Plugin } from "vite";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import base from "../../vite.config";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = resolve(here, "../..");

function injectStandIn(): Plugin {
  return {
    name: "vibelens-screenshot-stand-in",
    transformIndexHtml(html) {
      const tag = '<script type="module" src="/src/main.tsx"></script>';
      if (!html.includes(tag)) throw new Error("index.html no longer loads /src/main.tsx; update scripts/screenshots/vite.config.ts");
      return html.replace(tag, `<script type="module" src="/scripts/screenshots/entry.ts"></script>\n    ${tag}`);
    },
  };
}

const baseConfig = typeof base === "function" ? base({ command: "serve", mode: "development" }) : base;

export default defineConfig(
  mergeConfig(baseConfig, {
    root: appRoot,
    plugins: [injectStandIn()],
    server: { port: 1431, strictPort: true, host: "127.0.0.1", hmr: false },
  }),
);
