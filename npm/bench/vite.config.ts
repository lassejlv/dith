import { fileURLToPath } from "node:url";

import { defineConfig } from "vite-plus";

// Set DITH_BASELINE to another build of this package (a folder with dist/ and wasm/) to compare.
const baseline = process.env.DITH_BASELINE;
const stub = fileURLToPath(new URL("./no-baseline.ts", import.meta.url));

export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  base: "./",
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
  worker: { format: "es" },
  resolve: {
    alias: [
      { find: /^dith-baseline-wasm/, replacement: baseline ? baseline + "/wasm/dith_bg.wasm" : stub },
      { find: /^dith-baseline$/, replacement: baseline ? baseline + "/dist/index.js" : stub },
    ],
  },
});
