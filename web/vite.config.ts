import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  base: "/",
  plugins: [react(), tailwindcss()],
  // The main chunk is React + assistant-ui + markdown (~700 kB); xterm loads separately.
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 900 },
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:7878", ws: true, changeOrigin: false },
    },
  },
});
