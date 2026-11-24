import { readFileSync } from "node:fs";
import { describe, expect, it } from "vite-plus/test";

import {
  DEFAULT_RECIPE,
  PRESETS,
  auto,
  cliCommand,
  createDith,
  definePreset,
  isDithError,
  latest,
  parsePalette,
  parseRecipe,
  presetRecipe,
  registerPreset,
  render,
  resolveRecipe,
  source,
  stringifyRecipe,
} from "../src/index.js";

const poster = readFileSync(new URL("../../assets/intro-poster.jpg", import.meta.url));

describe("render", () => {
  it("renders encoded bytes to PNG with a typed result and CLI report", async () => {
    const out = await render(poster, { preset: "risograph", maxDim: 320, name: "intro-poster.jpg" });
    expect(out.format).toBe("png");
    expect(out.mime).toBe("image/png");
    expect([out.width, out.height]).toEqual([320, 180]);
    expect(String.fromCharCode(...out.bytes.subarray(1, 4))).toBe("PNG");
    expect(out.report.source).toBe("intro-poster.jpg");
    expect(out.report.bytes_written).toBe(out.bytes.length);
    expect(out.report.recipe).toEqual(resolveRecipe({ preset: "risograph", maxDim: 320 }));
  });

  it("returns text for SVG, a Blob on request, and raw pixels for rgba", async () => {
    const svg = await render(poster, { preset: "gameboy", maxDim: 96, format: "svg" });
    expect(svg.text.startsWith("<svg")).toBe(true);
    expect(svg.mime).toBe("image/svg+xml");
    const blob = await render(poster, { maxDim: 96, format: "webp", output: "blob" });
    expect(blob.blob.type).toBe("image/webp");
    expect(blob.blob.size).toBe(blob.report.bytes_written);
    const rgba = await render(poster, { maxDim: 100, format: "rgba" });
    expect(rgba.pixels.length).toBe(100 * 56 * 4);
  });

  it("renders generators like dith generate and raw RGBA input", async () => {
    const orb = await render({ pattern: "sphere", width: 200, height: 120 }, { preset: "editorial-print" });
    expect([orb.width, orb.height]).toEqual([200, 120]);
    expect(orb.report.source).toBe("generate:sphere");
    expect(orb.report.recipe).toEqual(presetRecipe("editorial-print", { procedural: true }));
    const raw = await render({ data: new Uint8Array(4 * 4 * 4).fill(128), width: 4, height: 4 }, { format: "rgba" });
    expect(raw.pixels.length).toBe(64);
  });

  it("previews downscale before dithering", async () => {
    const src = await source(poster);
    const full = await src.render({ maxDim: 1000, format: "rgba", preview: 2000 });
    expect(full.preview).toBe(false);
    const preview = await src.render({ maxDim: 1000, format: "rgba", preview: 400 });
    expect(preview.preview).toBe(true);
    expect([preview.width, preview.height]).toEqual([400, 225]);
    const original = await src.original({ maxDim: 300, format: "rgba" });
    expect([original.width, original.height]).toEqual([300, 169]);
    src.dispose();
  });

  it("aborts and keeps only the latest render", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(render(poster, { signal: controller.signal })).rejects.toMatchObject({ code: "ABORTED" });
    const live = latest<number>();
    const first = live(async () => 1);
    const second = live(async () => 2);
    expect(await first).toBeUndefined();
    expect(await second).toBe(2);
  });

  it("throws DithErrors with the engine's codes", async () => {
    await expect(render(poster, { strength: 9 })).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    await expect(render(new Uint8Array([1, 2, 3]))).rejects.toMatchObject({ code: "DECODE_ERROR" });
    try {
      resolveRecipe({ preset: "nope" as "risograph" });
      expect.unreachable();
    } catch (error) {
      expect(isDithError(error)).toBe(true);
      if (isDithError(error) && error.code === "UNKNOWN_PRESET") expect(error.validValues).toContain("risograph");
    }
  });

  it("runs separate instances without workers in Node", async () => {
    const dith = createDith({ workers: 4 });
    const src = await dith.source({ pattern: "rings", width: 64, height: 64 });
    expect(src.onMainThread).toBe(true);
    expect((await src.render({ algorithm: "bayer4" })).report.algorithm).toBe("bayer4");
  });
});

describe("recipes", () => {
  it("layers recipe, preset and knobs like the CLI", () => {
    const recipe = resolveRecipe({
      preset: "risograph",
      palette: ["#111", "#eeeeee"],
      contrast: 0.25,
      serpentine: false,
      background: "#fff",
    });
    expect(recipe.preset).toBe("risograph");
    expect(recipe.palette).toEqual({
      mode: "colors",
      value: [
        { r: 17, g: 17, b: 17 },
        { r: 238, g: 238, b: 238 },
      ],
    });
    expect(recipe.preprocess.contrast).toBe(0.25);
    expect(recipe.dither.serpentine).toBe(false);
    expect(resolveRecipe({ palette: auto(6) }).palette).toEqual({ mode: "adaptive", value: 6 });
    const replaced = resolveRecipe({
      recipe: { ...DEFAULT_RECIPE, preset: null, dither: { ...DEFAULT_RECIPE.dither, strength: 0.5 } },
      preset: "gameboy",
    });
    expect(replaced).toEqual(presetRecipe("gameboy"));
  });

  it("round-trips recipes through JSON", () => {
    const recipe = resolveRecipe({ preset: "halftone-zine", maxDim: 900, invert: true });
    expect(parseRecipe(stringifyRecipe(recipe))).toEqual(recipe);
    expect(parseRecipe('{"dither":{"strength":0.5}}').dither.strength).toBe(0.5);
    expect(() => parseRecipe('{"preprocess":{"fit":"zoom"}}')).toThrow(/fit/);
  });

  it("supports custom presets that extend built-ins", () => {
    const brand = definePreset({
      name: "brand",
      extends: "film-stipple",
      options: { palette: ["#0f172a", "#f5efe6"], contrast: 0.1 },
    });
    const recipe = resolveRecipe({ preset: brand, maxDim: 500 });
    expect(recipe.preset).toBe("film-stipple");
    expect(recipe.dither.algorithm).toBe("blue-noise");
    expect(recipe.preprocess.contrast).toBe(0.1);
    expect(recipe.preprocess.max_dim).toBe(500);
    registerPreset({ name: "brand-hero", extends: brand, options: { sharpen: 0.4 } });
    const named = resolveRecipe({ preset: "brand-hero" as never });
    expect(named.preprocess.sharpen).toBe(0.4);
    expect(named.preprocess.contrast).toBe(0.1);
    expect(cliCommand({ recipe: named, input: "a.png" }).command).toBe(
      "dith apply a.png -o out.png --preset film-stipple --colors '#0f172a,#f5efe6' --contrast 0.1 --sharpen 0.4",
    );
  });

  it("builds the CLI command", () => {
    const cmd = cliCommand({
      options: { preset: "risograph", palette: ["#111111", "#eeeeee"], maxDim: 960, contrast: 0.25 },
      input: "photo.jpg",
    });
    expect(cmd.command).toBe(
      "dith apply photo.jpg -o out.png --preset risograph --colors '#111111,#eeeeee' --max-dim 960 --contrast 0.25",
    );
    const gen = cliCommand({
      options: { preset: "gameboy" },
      generator: { pattern: "sphere", width: 800 },
      format: "webp",
    });
    expect(gen.command).toBe("dith generate sphere -o out.webp --canvas-width 800 --preset gameboy");
    const fallback = cliCommand({
      recipe: { ...DEFAULT_RECIPE, dither: { ...DEFAULT_RECIPE.dither, pattern_offset: [1, 2] } },
    });
    expect(fallback.flagsOnly).toBe(false);
    expect(fallback.argv.slice(-2)).toEqual(["--recipe", "recipe.json"]);
  });

  it("exposes the catalog without loading the engine", () => {
    expect(PRESETS.map((p) => p.id)).toContain("mac-classic");
  });
});

describe("palettes", () => {
  it("parses hex, GIMP and JSON palette files with the CLI's rules", () => {
    expect(parsePalette("#111 // ink\n; comment\n#eee, #ABCDEF")).toEqual(["#111111", "#eeeeee", "#abcdef"]);
    expect(parsePalette("GIMP Palette\nName: x\n# c\n 0 0 0 black\n255 255 255\n", "p.gpl")).toEqual([
      "#000000",
      "#ffffff",
    ]);
    expect(parsePalette('{"colors":["#000","#fff"]}', "p.json")).toEqual(["#000000", "#ffffff"]);
    expect(() => parsePalette("#123")).toThrow(/at least 2/);
    expect(() => parsePalette("#12345")).toThrow(/invalid hex/);
  });
});
