// Build and serve the benchmark page, run it in headless Chromium, print the results as JSON.
// usage: node bench/run.mjs   env: DITH_BASELINE (older build to compare), CHROME (browser path)
import { execFileSync, spawn } from "node:child_process";
import { chromium } from "playwright";

const cwd = new URL("..", import.meta.url);
execFileSync("npx", ["vp", "build", "--config", "bench/vite.config.ts"], { cwd, stdio: "inherit" });
const server = spawn(
  "npx",
  ["vp", "preview", "--config", "bench/vite.config.ts", "--port", "4190", "--strictPort", "--host", "127.0.0.1"],
  { cwd, stdio: "ignore" },
);
try {
  await new Promise((r) => setTimeout(r, 3000));
  const browser = await chromium.launch({ executablePath: process.env.CHROME || undefined });
  const modes = ["baseline", "pool", "main"];
  const rounds = Number(process.env.BENCH_ROUNDS ?? 3);
  const all = {};
  for (let round = 0; round < rounds; round++) {
    for (const mode of modes) {
      const page = await browser.newPage();
      page.on("pageerror", (e) => console.error(e));
      await page.goto("http://127.0.0.1:4190/?mode=" + mode);
      const handle = await page.waitForFunction(() => window.__bench, null, { timeout: 300_000 });
      const { result } = await handle.jsonValue();
      await page.close();
      if (!result) continue;
      for (const [key, value] of Object.entries(result)) ((all[mode] ??= {})[key] ??= []).push(value);
    }
  }
  // Median across rounds of each per-page median.
  const med = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
  const summary = Object.fromEntries(
    Object.entries(all).map(([mode, metrics]) => [
      mode,
      Object.fromEntries(Object.entries(metrics).map(([k, xs]) => [k, med(xs)])),
    ]),
  );
  console.log(JSON.stringify(summary, null, 2));
  await browser.close();
} finally {
  server.kill();
}
