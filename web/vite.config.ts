import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 8372,
    strictPort: true,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8371",
        changeOrigin: true,
        ws: true,
      },
    },
  },
  build: {
    target: "es2024",
  },
  test: {
    environment: "jsdom",
  },
});
