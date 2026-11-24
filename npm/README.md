# dith

Deterministic dithering, procedural dither art and vector halftones for the browser (and Node 20+). It is the [dith](https://github.com/lassejlv/dith) Rust engine compiled to WebAssembly, so a recipe rendered here gives the same bytes as the `dith` CLI.

- **Fast.** The module is compiled once with `compileStreaming` and shared with a worker pool. Inputs and outputs move between threads without copies, and stale renders are cancelled. A preview mode downscales before dithering.
- **Typed.** Presets, palettes, algorithms, generators, formats and every recipe field are literal types, generated from the engine. Ranges are in the TSDoc, and the result type follows the format you ask for.
- **No runtime dependencies.** Nothing loads until the first render.

```sh
npm install dith
```

## Quick start

```ts
import { render } from "dith";

const out = await render(file, { preset: "risograph", maxDim: 1200, format: "webp" });
out.bytes; // Uint8Array
out.mime; // "image/webp"
out.report; // the same fields as `dith apply --json`
```

With Vite, hand the worker and the .wasm to the engine, because bundlers rewrite their URLs:

```ts
import { configure } from "dith";
import wasm from "dith/wasm/dith_bg.wasm?url";
import DithWorker from "dith/worker?worker";

configure({ wasm, worker: () => new DithWorker() });
```

Also add `optimizeDeps: { exclude: ["dith"] }` to your Vite config.

## API

**`render(input, options)`** renders one input.

- Inputs: `File`, `Blob`, URL, `ArrayBuffer`, `Uint8Array`, `HTMLImageElement`, `HTMLCanvasElement`, `OffscreenCanvas`, `ImageBitmap`, `ImageData`, `{ data, width, height }` raw RGBA, or a generator spec such as `{ pattern: "sphere", width: 800, height: 600 }`.
- Encoded bytes are decoded by the engine exactly like the CLI. Elements, bitmaps and ImageData use the browser's decoded pixels.

| `format`                                                          | Result                              |
| ----------------------------------------------------------------- | ----------------------------------- |
| `"png"` (default), `"webp"`, `"gif"`, `"bmp"`, `"tiff"`, `"jpeg"` | `{ bytes, mime }`                   |
| `"svg"`, `"braille"`, `"ascii"`                                   | `{ text, mime }`                    |
| `"bitmap"`                                                        | `{ bitmap: ImageBitmap }`           |
| `"rgba"`                                                          | `{ pixels: Uint8ClampedArray }`     |
| `output: "blob"`                                                  | `{ blob }` instead of bytes or text |
| `target: canvas`                                                  | draws into the canvas               |

Every result carries `width`, `height`, `preview` and `report`.

**Options** are every recipe knob, named after its CLI flag:

- From a preset or recipe: `recipe`, `preset`.
- Palette: `palette` (an id, a list of hex colours, or `auto(n)`).
- Dithering: `algorithm`, `colorSpace`, `strength`, `serpentine`, `bias`, `noise`, `patternScale`, `seed`.
- Size: `pixelSize`, `width`, `height`, `maxDim`, `scale`, `fit`.
- Tone: `brightness`, `contrast`, `gamma`, `saturation`, `sharpen`, `blur`, `autoLevels`, `grayscale`, `invert`.
- Alpha: `alphaMode`, `background`.
- SVG and size of the file: `svgShape`, `svgSkipBg`, `optimize`.
- Render control: `preview`, `signal`.

They layer like the CLI: recipe, then preset, then the rest.

**`source(input)`** decodes once for many renders, which is what live UIs want:

```ts
import { latest, source } from "dith";

const photo = await source(file);
const live = latest<BitmapResult>();
slider.oninput = async () => {
  const out = await live((signal) =>
    photo.render({ preset: "gameboy", contrast: +slider.value, format: "bitmap", preview: 900, signal }),
  );
  if (out) canvas.getContext("bitmaprenderer")!.transferFromImageBitmap(out.bitmap);
};
```

`photo.original(options)` returns the undithered source at the same size, and `photo.dispose()` frees it.

**Recipes and the CLI**

- `resolveRecipe(options)` returns the full recipe.
- `parseRecipe(json)` and `stringifyRecipe(recipe)` read and write the JSON of `dith apply --emit-recipe` / `--recipe`.
- `cliCommand({ recipe, input })` returns the exact `dith apply` or `dith generate` command.

**Palettes and presets**

- `parsePalette(text, fileName)` and `readPalette(file)` parse .hex, .gpl and .json palettes with the CLI's rules.
- `definePreset({ name, extends: "film-stipple", options })` creates a custom preset, and `registerPreset` makes it usable by name.

**Catalogs** are available without loading the engine: `PRESETS`, `PALETTES`, `ALGORITHMS`, `GENERATORS`, `PRESET_RECIPES`, `DEFAULT_RECIPE`.

**Errors** are `DithError`s. Narrow `AnyDithError` on `code`, which uses the same codes as the CLI's `--json` errors (`UNKNOWN_PRESET` carries `validValues`), plus `ABORTED`, `INVALID_INPUT`, `UNSUPPORTED` and `WORKER_ERROR`.

**Engines:** `createDith({ workers, worker, wasm, mainThreadMaxPixels })` creates an engine with its own pool. `workers` defaults to `hardwareConcurrency - 1`, capped at 4. Sources up to 65,536 pixels render on the calling thread, and Node always does.

## Performance tips

- Keep one `source()` per image and render it many times. Decoding happens once.
- Use `preview` (about the displayed size) while sliders move, then render without it on release or download.
- Use `latest()` for live input, so superseded renders are dropped before they start.
- Ask for `format: "bitmap"` to draw on screen; encoding PNG or WebP costs more than the dithering at large sizes.
- Pass `{ transfer: true }` to hand over ArrayBuffers and ImageBitmaps without a copy. The caller's object becomes unusable.
- Call `preload()` when the page is idle to compile the module before the first render.

Animations are not exposed yet; use the CLI's `dith animate`.

## Development

```sh
../scripts/build-wasm.sh                     # build wasm/ from the Rust crate
npm ci && npm run build                      # generate types from the engine, then vp pack
npx vp check && npx vp test                  # lint, format, types; tests
DITH_BIN=../target/release/dith npx vp test  # adds byte parity with the native CLI
DITH_BASELINE=/path/to/old/build npm run bench
```
