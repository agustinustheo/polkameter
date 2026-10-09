/// <reference types="vitest/config" />
import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    host: host ?? "127.0.0.1",
    port: 1420,
    strictPort: true
  },
  test: {
    // Cargo's target directory can contain archived upstream integration fixtures.
    include: ["src/**/*.test.ts"]
  }
});
