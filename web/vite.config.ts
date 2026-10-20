import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the Rust server runs on :8080 and Vite proxies the API, so the
// browser always talks to one origin (no CORS needed).
const api = process.env.XENIUM_API ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    proxy: { "/api": api, "/healthz": api },
  },
  build: { target: "es2022", sourcemap: true, chunkSizeWarningLimit: 2000 },
});
