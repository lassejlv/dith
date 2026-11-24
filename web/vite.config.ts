import { defineConfig } from "vite-plus";

// Relative base so the build works at any path (GitHub Pages serves it under /dith/).
export default defineConfig({
  base: "./",
  build: { target: "es2022", assetsInlineLimit: 0 },
  worker: { format: "es" },
  // dith ships its worker and .wasm as separate files; prebundling would break their URLs.
  optimizeDeps: { exclude: ["dith"] },
  lint: {
    ignorePatterns: ["dist/**"],
    options: { typeAware: true, typeCheck: true },
  },
  fmt: {
    printWidth: 140,
    ignorePatterns: ["dist/**"],
  },
});
