import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildHash, withBuild } from "./src/swBuild.ts";

/** Rewrites the BUILD placeholder line in dist/sw.js (the public dir is copied first, in
 *  renderStart) with this build's hash and its emitted /assets list, so the service worker precaches
 *  the build at install and can keep serving a superseded build's chunks to tabs that have not
 *  reloaded yet. A missing or doubled placeholder throws, so it cannot silently ship as "dev". */
function swBuild(): Plugin {
  // closeBundle runs after the bundle is written and takes no bundle argument, so the /assets list
  // is captured at writeBundle and held for the rewrite.
  let assets: string[] = [];
  return {
    name: "colonizer-sw-build",
    apply: "build",
    writeBundle(_options, bundle) {
      assets = Object.keys(bundle)
        .filter((name) => name.startsWith("assets/"))
        .sort()
        .map((name) => `/${name}`);
    },
    async closeBundle() {
      // dist/sw.js has been on disk since renderStart (the public dir is copied there, see
      // vite:prepare-out-dir), so by closeBundle there is nothing to wait for.
      const out = path.join(path.dirname(fileURLToPath(import.meta.url)), "dist", "sw.js");
      const source = await fs.promises.readFile(out, "utf8");
      await fs.promises.writeFile(out, withBuild(source, buildHash(assets), assets));
    },
  };
}

// The demo build (`vite build --mode demo`, `npm run build:demo`) is served from /demo on
// colonizer.dev by the website repo, so its assets are rooted at /demo/ (issue #682).
export default defineConfig(({ mode }) => ({
  base: mode === "demo" ? "/demo/" : "/",
  plugins: [react(), tailwindcss(), ...(mode === "demo" ? [demoDropManifest()] : [swBuild()])],
  // The main chunk is React + assistant-ui + markdown (~700 kB); xterm loads separately.
  build: { outDir: mode === "demo" ? "dist-demo" : "dist", emptyOutDir: true, chunkSizeWarningLimit: 900 },
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:7878", ws: true, changeOrigin: false },
    },
  },
}));

// The demo is not installable: public/manifest.webmanifest claims the real cockpit's `/` scope, so
// installing the demo would put a mock-data app in the dock pointing at the site root. Dropping the
// link (not editing the manifest, which production serves) keeps the demo a plain page.
function demoDropManifest(): Plugin {
  return {
    name: "demo-drop-manifest",
    apply: "build",
    transformIndexHtml(html) {
      return html.replace(/[ \t]*<link rel="manifest"[^>]*>\n?/, "");
    },
  };
}
