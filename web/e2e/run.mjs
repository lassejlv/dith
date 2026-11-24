// End-to-end check of the playground in headless Chromium.
//
// usage: node e2e/run.mjs [baseUrl]   env: DITH_BIN (native CLI for parity), CHROME (browser path),
//        E2E_OUT (artifacts dir), E2E_IMAGE (image to load)
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const base = process.argv[2] ?? "http://localhost:5173/";
const out = resolve(process.env.E2E_OUT ?? "e2e-out");
const image = resolve(process.env.E2E_IMAGE ?? fileURLToPath(new URL("../../assets/intro-poster.jpg", import.meta.url)));
const bin = process.env.DITH_BIN;
mkdirSync(out, { recursive: true });

const browser = await chromium.launch({ executablePath: process.env.CHROME || undefined });
const results = {};

async function settled(page, count) {
  await page.waitForFunction((n) => window.__dith && window.__dith.renders.filter((r) => !r.preview).length >= n, count, {
    timeout: 30_000,
  });
  await page.waitForTimeout(300);
}

async function fullRenders(page) {
  return page.evaluate(() => window.__dith.renders.filter((r) => !r.preview).length);
}

const VIEWS = [
  { name: "desktop-dark", viewport: { width: 1440, height: 900 }, scheme: "dark" },
  { name: "desktop-light", viewport: { width: 1440, height: 900 }, scheme: "light" },
  { name: "phone-dark", viewport: { width: 390, height: 844 }, scheme: "dark" },
  { name: "phone-light", viewport: { width: 390, height: 844 }, scheme: "light" },
];
for (const { name, viewport, scheme } of VIEWS) {
  const context = await browser.newContext({ viewport, colorScheme: scheme, deviceScaleFactor: 2, acceptDownloads: true });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(base);
  await settled(page, 1);
  const perf = await page.evaluate(() => window.__dith);
  results[name] = { workerReadyMs: perf.workerReadyMs, firstRenderMs: perf.firstRenderMs, renders: perf.renders, errors };
  await page.screenshot({ path: join(out, name + ".png"), fullPage: name.startsWith("phone") });
  await context.close();
}

// Full flow on desktop: load an image, change presets and sliders, download, copy command.
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "dark", acceptDownloads: true });
await context.grantPermissions(["clipboard-read", "clipboard-write"]);
const page = await context.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
await page.goto(base);
await settled(page, 1);

let n = await fullRenders(page);
await page.setInputFiles("#file", image);
await settled(page, n + 1);
n = await fullRenders(page);
await page.selectOption("#preset", "risograph");
await settled(page, n + 1);
n = await fullRenders(page);
await page.selectOption("#preset", "gameboy");
await settled(page, n + 1);
n = await fullRenders(page);
await page.selectOption("#size", "1200");
const contrast = page.locator("#sliders input[type=range]").nth(1);
await contrast.focus();
for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowRight");
await settled(page, n + 1);
await page.screenshot({ path: join(out, "desktop-image-gameboy.png") });

await page.click("#show-original");
await page.waitForTimeout(400);
await page.screenshot({ path: join(out, "desktop-original.png") });
await page.click("#show-result");

const downloads = {};
for (const fmt of ["png", "webp", "svg"]) {
  const [download] = await Promise.all([page.waitForEvent("download"), page.click('[data-download="' + fmt + '"]')]);
  const path = join(out, "download." + fmt);
  await download.saveAs(path);
  downloads[fmt] = { path, name: download.suggestedFilename(), bytes: readFileSync(path).length };
}
const [recipeDownload] = await Promise.all([page.waitForEvent("download"), page.click("#recipe")]);
await recipeDownload.saveAs(join(out, "recipe.json"));

await page.click("#copy");
const copied = await page.evaluate(() => navigator.clipboard.readText());
writeFileSync(join(out, "command.txt"), copied + "\n");
results.flow = { downloads, command: copied, errors };

// Per-render latency at 1200 px: drag-like slider input with preview caps, then full renders.
const latency = await page.evaluate(async () => {
  const before = window.__dith.renders.length;
  const slider = document.querySelectorAll("#sliders input[type=range]")[0];
  for (let i = 0; i < 12; i++) {
    slider.value = String(0.6 + (i % 6) * 0.1);
    slider.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((r) => setTimeout(r, 120));
  }
  await new Promise((r) => setTimeout(r, 1500));
  return window.__dith.renders.slice(before);
});
results.sliderRenders = latency;

if (bin) {
  const argv = copied.match(/'[^']*'|\S+/g).map((a) => a.replace(/^'|'$/g, ""));
  argv[2] = image;
  const parity = {};
  for (const fmt of ["png", "webp", "svg"]) {
    const target = join(out, "cli." + fmt);
    const args = argv.slice(1);
    args[args.indexOf("-o") + 1] = target;
    execFileSync(bin, args, { stdio: "inherit" });
    parity[fmt] = readFileSync(target).equals(readFileSync(downloads[fmt].path));
  }
  results.parity = parity;
}

writeFileSync(join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
console.log(JSON.stringify(results, null, 2));
await browser.close();
if (results.parity && Object.values(results.parity).some((ok) => !ok)) process.exit(1);
