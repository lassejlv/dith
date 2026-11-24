// Byte parity between this package and the native CLI. Set DITH_BIN to a dith binary (for
// example ../target/release/dith) to run it; it is skipped otherwise.
//
// Known gap: procedural generators (sin, cos, exp, powf) and the saturation, grayscale and gamma
// steps (powf) call the platform libm natively (glibc, macOS) but Rust's own libm inside
// WebAssembly, and the two can differ in the last bit. Rarely that moves a pixel by one 8-bit
// level and changes the dithered bytes: about 3 in 90 generator renders in a sweep, never in the
// photo cases. The generator case that hits it on glibc is marked todo rather than hidden.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vite-plus/test";

import {
  cliCommand,
  parsePalette,
  render,
  resolveRecipe,
  stringifyRecipe,
  type DitherOptions,
  type GeneratorSpec,
} from "../src/index.js";

const bin = process.env.DITH_BIN;
const posterPath = fileURLToPath(new URL("../../assets/intro-poster.jpg", import.meta.url));
const poster = readFileSync(posterPath);
const dir = mkdtempSync(join(tmpdir(), "dith-parity-"));
const cli = (argv: string[]): string => execFileSync(bin as string, argv.slice(1), { cwd: dir, encoding: "utf8" });

const cases: [string, DitherOptions][] = [
  ["mac-classic", { preset: "mac-classic", maxDim: 640 }],
  ["risograph", { preset: "risograph" }],
  ["editorial", { preset: "editorial-print", strength: 0.8, bias: -0.1, maxDim: 900, contrast: 0.2, gamma: 1.2 }],
  ["bayer", { preset: "gameboy", algorithm: "bayer8", patternScale: 2, pixelSize: 3, maxDim: 800 }],
  [
    "blue-noise-colors",
    {
      preset: "vapor-mesh",
      algorithm: "blue-noise",
      seed: 7,
      maxDim: 700,
      brightness: 0.05,
      palette: ["#0f172a", "#2563eb", "#f5efe6"],
    },
  ],
  ["adaptive", { preset: "lo-fi-photo", maxDim: 600, blur: 0.8, palette: { mode: "adaptive", value: 8 } }],
  ["halftone", { preset: "halftone-zine", maxDim: 960, invert: true, sharpen: 1.1 }],
  ["riemersma", { preset: "film-stipple", algorithm: "riemersma", maxDim: 500 }],
  ["ign", { preset: "crt-amber", algorithm: "ign", noise: 0.1, maxDim: 500, svgShape: "circle" }],
];
const LIBM_SENSITIVE = new Set(["blue-noise-colors"]);

describe.skipIf(!bin)("parity with the native CLI", () => {
  for (const [name, options] of cases) {
    for (const format of ["png", "webp", "svg"] as const) {
      it(name + " " + format + " image matches dith apply", async () => {
        const out = await render(poster, { ...options, format, name: posterPath, output: "blob" });
        const wasm = Buffer.from(await out.blob.arrayBuffer());
        writeFileSync(join(dir, "recipe.json"), stringifyRecipe(resolveRecipe(options)));
        cli([
          "dith",
          "apply",
          posterPath,
          "-o",
          name + "-recipe." + format,
          "--recipe",
          "recipe.json",
          "--format",
          format,
        ]);
        expect(wasm.equals(readFileSync(join(dir, name + "-recipe." + format)))).toBe(true);
        const cmd = cliCommand({ options, input: posterPath, output: name + "-flags." + format, format });
        expect(cmd.flagsOnly).toBe(true);
        cli(cmd.argv);
        expect(wasm.equals(readFileSync(join(dir, name + "-flags." + format))), cmd.command).toBe(true);
      });
    }

    it(name + " options resolve to the CLI's recipe", () => {
      const cmd = cliCommand({ options, input: posterPath, output: "x.png" });
      const report = JSON.parse(cli([...cmd.argv, "--dry-run", "--emit-recipe", "--json"])) as { recipe: unknown };
      expect(report.recipe).toEqual(resolveRecipe(options));
    });

    for (const pattern of ["sphere", "mesh", "plasma"] as const) {
      const todo = LIBM_SENSITIVE.has(name);
      (todo ? it.todo : it)(name + " " + pattern + " generator matches dith generate", async () => {
        const generator: GeneratorSpec = {
          pattern,
          width: 480,
          height: 320,
          angle: 120,
          stops: ["#0a0a0d", "#e85d3f", "#f7f7f9"],
        };
        for (const format of ["png", "webp"] as const) {
          const out = await render(generator, { ...options, format });
          const file = name + "-" + pattern + "." + format;
          const cmd = cliCommand({ options, generator, output: file, format });
          expect(cmd.flagsOnly).toBe(true);
          cli(cmd.argv);
          expect(Buffer.from(out.bytes).equals(readFileSync(join(dir, file))), cmd.command).toBe(true);
        }
      });
    }
  }

  it("palette files parse like --palette-file", async () => {
    const files: [string, string][] = [
      ["brand.hex", "#0f172a // ink\n; note\n#2563eb, #f5efe6\n"],
      ["brand.gpl", "GIMP Palette\nName: brand\n#\n 15  23  42 ink\n245 239 230 paper\n"],
      ["brand.json", '{"colors":["#111","#e85d3f","#fafafa"]}'],
    ];
    for (const [file, text] of files) {
      writeFileSync(join(dir, file), text);
      const out = await render(poster, { preset: "film-stipple", maxDim: 400, palette: parsePalette(text, file) });
      cli([
        "dith",
        "apply",
        posterPath,
        "-o",
        file + ".png",
        "--preset",
        "film-stipple",
        "--max-dim",
        "400",
        "--palette-file",
        file,
      ]);
      expect(Buffer.from(out.bytes).equals(readFileSync(join(dir, file + ".png"))), file).toBe(true);
    }
  });
});
