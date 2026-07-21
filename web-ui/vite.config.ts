import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Rust server serves the built assets from `/` and the API from `/api`.
// In dev, Vite proxies `/api` to the local Base Search server.
export default defineConfig({
  plugins: [react()],
  base: "/",
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:7833",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 1200,
  },
});
