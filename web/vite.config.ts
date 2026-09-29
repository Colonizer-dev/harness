import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The demo build (`vite build --mode demo`, `npm run build:demo`) is served from /demo on
// colonizer.dev by the website repo, so its assets are rooted at /demo/ (issue #682).
export default defineConfig(({ mode }) => ({
  base: mode === "demo" ? "/demo/" : "/",
  plugins: [react(), tailwindcss(), ...(mode === "demo" ? [demoDropManifest()] : [])],
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
