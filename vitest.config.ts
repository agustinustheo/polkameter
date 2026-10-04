import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Cargo's target directory can contain archived upstream integration fixtures.
    include: ["src/**/*.test.ts"]
  }
});
