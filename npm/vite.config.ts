import { defineConfig } from "vite-plus";

export default defineConfig({
  pack: {
    entry: ["src/index.ts", "src/worker.ts"],
    format: ["esm"],
    platform: "neutral",
    target: "es2022",
    dts: true,
    sourcemap: true,
  },
  test: {
    include: ["test/**/*.test.ts"],
    testTimeout: 60_000,
  },
  lint: {
    ignorePatterns: ["dist/**", "bench/dist/**", "wasm/**", "src/generated/**"],
    options: { typeAware: true, typeCheck: true },
  },
  fmt: {
    printWidth: 120,
    ignorePatterns: ["dist/**", "bench/dist/**", "wasm/**", "src/generated/**"],
  },
});
