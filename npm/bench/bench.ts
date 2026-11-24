// Browser benchmark: engine init, first render, and per-render latency at 640, 1200 and 2400 px,
// for this package (worker pool and main-thread mode) and an optional baseline build.
import { createDith, type DitherOptions } from "../src/index.ts";
import wasmUrl from "../wasm/dith_bg.wasm?url";
import DithWorker from "../src/worker.ts?worker";
import posterUrl from "../../assets/intro-poster.jpg?url";

interface BaselineModule {
  init(input: unknown): Promise<void>;
  presetRecipe(name: string): Record<string, unknown> & { preprocess: Record<string, unknown> };
  Source: {
    fromBytes(bytes: Uint8Array): {
      render(recipe: unknown, options: { format: string }): { bytes: Uint8Array };
      free(): void;
    };
  };
}

const RUNS = 9;
const SIZES: [number, DitherOptions][] = [
  [640, { maxDim: 640 }],
  [1200, { maxDim: 1200 }],
  [2400, { scale: 1.875 }],
];

const median = (xs: number[]): number => {
  const s = [...xs].sort((a, b) => a - b);
  return Math.round((s[Math.floor(s.length / 2)] ?? 0) * 10) / 10;
};

async function timeRuns(fn: () => unknown): Promise<number> {
  await fn();
  const times: number[] = [];
  for (let i = 0; i < RUNS; i++) {
    const t = performance.now();
    await fn();
    times.push(performance.now() - t);
  }
  return median(times);
}

async function poster(): Promise<Uint8Array> {
  return new Uint8Array(await (await fetch(posterUrl)).arrayBuffer());
}

/** One fresh page per mode, so module compilation and JIT warm-up are not shared between modes. */
async function benchPackage(workers: "auto" | false): Promise<Record<string, number>> {
  const bytes = await poster();
  const t0 = performance.now();
  const dith = createDith({ wasm: wasmUrl, workers, worker: () => new DithWorker() });
  await dith.ready();
  const init = performance.now() - t0;
  const src = await dith.source(bytes);
  await src.render({ preset: "mac-classic", maxDim: 640, format: "rgba" });
  const firstMs = performance.now() - t0;
  const out: Record<string, number> = {
    init_ms: Math.round(init * 10) / 10,
    first_render_ms: Math.round(firstMs * 10) / 10,
  };
  for (const [px, size] of SIZES) {
    out["render_" + px + "_ms"] = await timeRuns(() => src.render({ preset: "mac-classic", ...size, format: "rgba" }));
    out["preview_" + px + "_ms"] = await timeRuns(() =>
      src.render({ preset: "mac-classic", ...size, format: "rgba", preview: 800 }),
    );
  }
  src.dispose();
  dith.terminate();
  return out;
}

async function benchBaseline(): Promise<Record<string, number> | null> {
  const mod = (await import("dith-baseline")) as unknown as BaselineModule & { missing?: boolean };
  if (mod.missing) return null;
  const wasm = (await import("dith-baseline-wasm?url")) as { default: string };
  const bytes = await poster();
  const t0 = performance.now();
  await mod.init(wasm.default);
  const init = performance.now() - t0;
  const src = mod.Source.fromBytes(bytes);
  const r = mod.presetRecipe("mac-classic");
  src.render({ ...r, preprocess: { ...r.preprocess, max_dim: 640 } }, { format: "rgba" });
  const firstMs = performance.now() - t0;
  const out: Record<string, number> = {
    init_ms: Math.round(init * 10) / 10,
    first_render_ms: Math.round(firstMs * 10) / 10,
  };
  for (const [px, size] of SIZES) {
    const pre = { ...r.preprocess, max_dim: size.maxDim ?? null, scale: size.scale ?? null };
    out["render_" + px + "_ms"] = await timeRuns(() => src.render({ ...r, preprocess: pre }, { format: "rgba" }));
  }
  src.free();
  return out;
}

const mode = new URLSearchParams(location.search).get("mode") ?? "pool";
const result = mode === "baseline" ? await benchBaseline() : await benchPackage(mode === "pool" ? "auto" : false);
(window as unknown as { __bench: unknown }).__bench = { mode, result };
document.getElementById("out")!.textContent = JSON.stringify({ mode, result }, null, 2);
