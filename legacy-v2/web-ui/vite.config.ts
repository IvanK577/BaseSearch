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
        // The server rejects any request whose Origin does not match its own
        // authority (src/server/security.rs), which is what stops a hostile
        // page from driving a local workspace. `changeOrigin` only rewrites
        // Host, so without this every proxied API call answered 403 and the
        // dev server could not load data at all.
        headers: { Origin: "http://127.0.0.1:7833" },
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 1200,
  },
});
