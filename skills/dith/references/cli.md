# Dith CLI reference

Every subcommand accepts `--json`. `dith <command> --help` is always authoritative. `dith capabilities` dumps all commands, parameter ranges, and the default recipe as JSON.

## Contents

- [Install, update, or run from source](#install-update-or-run-from-source)
- [Discovery: list, capabilities, inspect](#discovery)
- [Shared dither flags](#shared-dither-flags) (apply, generate, animate, batch, compare, preview, preset save)
- [apply](#apply)
- [generate](#generate)
- [animate](#animate)
- [compare](#compare)
- [batch](#batch)
- [preview](#preview)
- [preset](#preset)
- [JSON reports](#json-reports)
- [Errors and exit codes](#errors-and-exit-codes)
- [Browser and Node (WebAssembly)](#browser-and-node-webassembly)

## Install, update, or run from source

```sh
# Prebuilt binary + agent skill (macOS Apple Silicon, Linux x86_64, Linux aarch64).
# Asks before copying the skill into each detected agent; add `-s -- --yes` to skip the prompt.
curl -fsSL https://raw.githubusercontent.com/lassejlv/dith/main/install.sh | bash

# Self-update from GitHub Releases (also refreshes the skill in detected agents)
dith update --check --json     # only report whether a newer release exists
dith update                    # install latest; --version v0.2.0 to pin, --force to reinstall, --no-skills to skip skills

# From source (any platform with Rust)
cargo install --git https://github.com/lassejlv/dith

# Inside the dith source checkout
cargo run --release -- --help
```

`install.sh` options:
- `--version <tag>`
- `--dir <path>` (default `~/.local/bin`)
- `--no-skills`
- `--yes`

The same settings work as environment variables: `DITH_VERSION`, `DITH_INSTALL_DIR`, `DITH_NO_SKILLS=1`, `DITH_YES=1`.

## Discovery

```sh
dith capabilities                                     # full manifest (~40 KB JSON)
dith list presets --json                              # built-in + local presets with full recipes
dith list algorithms --category error-diffusion --json   # categories: error-diffusion, ordered, stochastic, baseline
dith list palettes --tag print --json                 # tag substring, case-insensitive
dith list generators --json
dith list formats --json
dith inspect photo.png --colors 6 --json              # --colors 2..=16 dominant colors
```

`inspect` reports:
- `width`, `height`, `aspect_ratio`, `color_type`
- `has_transparency`, `transparent_percent`, `is_grayscale`
- `mean_luminance`, `dynamic_range`, `p5_luminance`, `p50_luminance`, `p95_luminance` (all 0..1)
- `dominant_colors`
- `recommendations[]`, each with `style`, `preset`, `rationale`, and `example_command`

## Shared dither flags

These flags apply to `apply`, `generate`, `animate`, `batch`, `preview`, `preset save`, and to `compare` (where they act as the base for every tile).

The resolution order is the recipe (`--recipe <path|->`), then the preset (`-P/--preset <id>`), then explicit flags. Each later layer overrides the one before it.

**Algorithm and diffusion**

| Flag | Range / values | Default |
| --- | --- | --- |
| `-a, --algorithm` | see [catalog](catalog.md#algorithms) | `floyd-steinberg` |
| `--color-space` | `oklab`, `linear`, `srgb` | `oklab` |
| `--strength` | 0.0..=1.5 (0 posterizes, >1 exaggerates) | 1.0 |
| `--serpentine` / `--no-serpentine` | error diffusion scan direction | serpentine on |
| `--bias` | -0.5..=0.5 (negative darker, positive lighter) | 0.0 |
| `--noise` | 0.0..=0.5 deterministic jitter that breaks banding and grids | 0.0 |
| `--pattern-scale` | 1..=32 cell size for ordered, halftone, line, and blue-noise screens | 1 |
| `--seed` | u64 for blue-noise, random, ign, noise, and auto-palette | 42 |

**Palette**

Choose one source.

| Flag | Notes |
| --- | --- |
| `-p, --palette <id>` | Built-in palette, or `grayN` for N in 2..=64. Default `bw`. |
| `--colors "#a,#b,..."` | Exact hex list. |
| `--palette-file <path>` | `.hex`/`.txt` (one hex per line), `.gpl` (GIMP), or `.json`. |
| `--auto-palette <N>` | 2..=64 colors extracted from the image with Oklab k-means++. |

**Resize** (applied before dithering)

| Flag | Notes |
| --- | --- |
| `--width`, `--height` | Target size. With both set, `--fit contain\|cover\|stretch` applies (default `contain`). |
| `--max-dim <N>` | Caps the longest side, keeping the aspect ratio. |
| `--scale <f>` | Uniform multiplier. |
| `--pixel-size <N>` | 1..=64. Downscale by N, dither, then nearest-upscale. Output dimensions are unchanged. |

**Tone** (applied before dithering)

| Flag | Range |
| --- | --- |
| `--brightness` | -1.0..=1.0 |
| `--contrast` | -1.0..=1.0 |
| `--gamma` | 0.1..=5.0 (<1 brightens midtones) |
| `--saturation` | 0.0..=3.0 (0 is gray, 1 is neutral) |
| `--sharpen` | 0.0..=5.0 unsharp mask |
| `--blur` | 0.0..=20.0 Gaussian sigma |
| `--auto-levels` / `--no-auto-levels` | 1st–99th percentile stretch |
| `--grayscale` / `--no-grayscale` | Perceptual grayscale |
| `--invert` | Invert tones |

**Alpha**

- `--alpha-mode preserve|flatten|threshold|dither` (default `preserve`)
  - `preserve` keeps alpha, and transparent pixels do not diffuse error.
  - `flatten` composites onto `--background "#hex"` (default `#ffffff`).
  - `threshold` cuts alpha at 50%.
  - `dither` stipples alpha with Bayer 4×4.

**Output**

- `-f, --format png|webp|jpeg|gif|bmp|tiff|svg|kitty|braille|ascii`, inferred from the `-o` extension. Stdout (`-o -`) defaults to PNG.
- SVG: `--svg-shape rect|circle|dot`, `--svg-skip-bg`
- `--optimize`: smallest lossless PNG, APNG, spritesheet, and PNG frames (palette order and filter search plus Zopfli). Pixels are identical; encoding takes several times longer. Recorded in recipes as `export.optimize`.
- Text: `--text-width <cols>` for braille and ascii
- Kitty: `--view-cols`, `--view-rows`

## apply

```sh
dith apply <INPUT|-> [-o <PATH|->] [shared flags] [options]
```

| Option | Notes |
| --- | --- |
| `-o, --output` | File path, or `-` for stdout. Optional for braille, ascii, and kitty (printed to stdout). |
| `--no-clobber` | Fail with `OUTPUT_EXISTS` instead of overwriting. Overwriting is the default. |
| `--dry-run` | Validate and plan without writing (`bytes_written: 0`). Includes `recipe`. |
| `--emit-recipe` | Add the fully resolved `recipe` to the report (printed after the summary without `--json`). |
| `--save-preset <name>` | Also save the resolved settings as a preset. `--preset-scope project\|user` (default `project`). The report gains `saved_preset_path`. |
| `--preview` | Add a Braille thumbnail as `preview_braille` in the report. |
| `--view` / `-F` / `--inline` | Kitty-protocol terminal viewer, for interactive TTYs only. |

Inputs can be PNG, JPEG, WebP, GIF (first frame), BMP, or TIFF.

## generate

```sh
dith generate <PATTERN> -o <PATH|-> [shared flags] [options]
```

| Option | Default |
| --- | --- |
| `--canvas-width`, `--canvas-height` | 640, 480 |
| `--angle <deg>` | 135 |
| `--frequency <f>` | 1.0 |
| `--stops "#a,#b,..."` | Pattern default. Source colors interpolated in Oklab. |

It also accepts `--no-clobber`, `--dry-run`, `--emit-recipe`, `--save-preset`/`--preset-scope`, `--preview`, and `--view`/`--inline`. The patterns are listed in the [catalog](catalog.md#generators).

With `--preset`, procedural sources skip the preset's photo preparation (auto-levels, contrast, sharpen) unless you pass those flags explicitly. This also applies to `animate <pattern>`, `compare --pattern`, and `preview --pattern`.

## animate

```sh
dith animate <PATTERN | INPUT.gif|.apng|.webp | FRAME1 FRAME2 ... | STILL.png> -o <out.gif|out.apng|sheet.png> [shared flags] [options]
```

You must pass `-o`, `--frames-dir`, or `--view`. `--pattern <name>` forces procedural mode explicitly.

**Container**

| Option | Notes |
| --- | --- |
| `--anim-format gif\|apng\|spritesheet\|frames` | Inferred from `-o`. A `.png` output needs `spritesheet`. |
| `--sheet-cols <N>` | Spritesheet columns. Use `N = frames` for a single row. The report has `spritesheet_grid: [cols, rows]`. |
| `--frames-dir <DIR>` | Also (or only) write `frame_0001.png`, ... |
| `--loops <N>` | 0 = infinite (default). |
| `--max-size <SIZE>` | File-size budget such as `2MB`, `500KB`, `1.5MiB`, or bytes (KB/MB are 1000-based). Re-renders at smaller frame dimensions, keeping frames and fps, until the output fits. Needs `-o` with gif, apng, or spritesheet output. Fails with `INVALID_ARGUMENT` if it can't fit even at 8 px. The report has `max_size_bytes` and `size_fit_scale`. |

**Timing**

| Option | Notes |
| --- | --- |
| `--frames <N>` | 2..=240, default 24 (procedural and still-photo boil). |
| `--duration <s>` | Mutually exclusive with `--frames`. Frames = s × fps. |
| `--fps <N>` | 1..=60, default 15. Overrides source GIF timing only when passed. GIF delays round to 10 ms. |
| `--max-frames <N>` | Cap decoded frames from input GIFs. |
| `--reverse`, `--ping-pong` | Ping-pong appends the reversed interior frames. |
| `--hold-start <N>`, `--hold-end <N>` | Extra dwell frames, added to `frame_count`. |

**Temporal dithering**

| Option | Notes |
| --- | --- |
| `--temporal locked\|boil\|grain` | Default `locked`, or `boil` for a single still input. `locked` fixes screen and seed; static pixels stay identical only with per-pixel algorithms (`temporally_stable: true` in `dith list algorithms --json`). Error diffusion still shimmers around motion. |
| `--boil-rate <N>` | Update the boil every N frames (2 = on twos). Default 1. |
| `--boil-amount <f>` | 0.0..=0.5 jitter intensity. |
| `--screen-drift <dx,dy>` | Per-frame drift of ordered or halftone screens, in cells. |
| `--per-frame-palette` | Adaptive palette per frame instead of one shared palette. |
| `--cycle-palette` | VGA/Amiga-style palette color cycling. |

**Motion and sweeps**

| Option | Notes |
| --- | --- |
| `--easing linear\|ease-in-out\|ease-in\|ease-out\|smoothstep\|bounce\|steps` | `--easing-steps <N>` (default 6) applies to `steps`. |
| `--cycles <f>` | Motion cycles per loop (default 1.0). |
| `--phase-offset <0..1>` | Start phase, useful for syncing several animations. |
| `--motion-amplitude <f>` | 0 = still, 1 = default, 2 = dramatic. |
| `--sweep-mode linear\|pulse` | `pulse` goes start → end → start, so it loops seamlessly. |
| `--sweep-bias A..B` | e.g. `-0.45..0.0`, an ink-wash dissolve. |
| `--sweep-brightness A..B` | e.g. `-0.7..0.0`, a fade-in. |
| `--sweep-contrast A..B`, `--sweep-gamma A..B` | Tonal sweeps. |
| `--sweep-strength A..B` | e.g. `0.0..1.2`, posterize to full stipple. |
| `--sweep-pixel-size A..B` | e.g. `12..1`, a mosaic focus-in. |

**Procedural source**

- `--canvas-width`/`--canvas-height` (defaults 480×360), `--angle`, `--frequency`, `--stops`
- What each pattern does over time is in `dith list generators --json` (`motion`), and in the [catalog](catalog.md#generators).

**Other**

- `--no-clobber`, `--dry-run`
- `--view` with `--view-mode kitty|braille|ascii` plays live in a TTY; `--inline` prints the first frame instead.

## compare

```sh
dith compare [INPUT | --pattern <name>] -o sheet.png [--out-dir DIR] [shared flags] [options]
```

| Option | Notes |
| --- | --- |
| `--algorithms a,b,...` | One tile per algorithm. |
| `--presets a,b,...` | One tile per preset. |
| `--palettes a,b,...` | One tile per palette. With `--algorithms`, a single palette applies to every algorithm tile. |
| `--columns <N>` | Default: an automatic square-ish grid. |
| `--tile-max-dim <N>` | Default 360. |
| `--light` | Warm paper frame instead of the dark matte. |
| `--out-dir <DIR>` | Write each variant as `<label>.png`. Usable without `-o`. |
| `--dry-run`, `--view`, `--inline` | |

## batch

```sh
dith batch <FILES...> -d|--out-dir <DIR> [shared flags] [--suffix -dithered] [--no-clobber] [--dry-run]
```

- Processes files in parallel. Pass files or a shell glob; directories are not expanded.
- The output name is `<stem><suffix>.<ext>`. The default suffix is `-dithered`; use `--suffix ""` to keep names.
- The extension follows `--format`, or stays `.png` when no format is given.
- When any item fails (including files skipped by `--no-clobber`), `status` is `partial_failure` and the exit code is 1.

## preview

```sh
dith preview [INPUT|- | --pattern <name>] [shared flags] [-m kitty|braille|ascii] [--inline] [--cols N] [--rows N] [--raw]
```

- Fullscreen by default. In agent shells, use `--inline --mode braille|ascii`.
- `--raw` previews the source without dithering, for before/after comparisons.
- With `--pattern`, it also accepts `--canvas-width`, `--canvas-height`, `--angle`, `--frequency`, and `--stops`.

## preset

```sh
dith preset save <name> [shared flags] [--title T] [--description D] [--tags a,b] \
  [--scope project|user | --dir DIR] [--no-clobber] --json
dith preset list [--source all|local|project|user|builtin] [--tag t] --json
dith preset show <name> --json          # local presets first, then built-ins
dith preset delete <name> [--scope project|user | --dir DIR] --json
```

- Names use lowercase letters, digits, and hyphens.
- Storage locations:
  - project: `.dith/presets/<name>.json`
  - user: `~/.config/dith/presets/<name>.json`
- Lookup order: `$DITH_PRESETS_DIR`, then `.dith/presets/`, then `~/.config/dith/presets/`, then built-ins. A local preset with a built-in's name shadows the built-in.
- From a render, `--save-preset <name> --preset-scope project|user` on `apply` or `generate` saves the resolved settings in one step.

## JSON reports

**apply / generate**

- `status`, `dry_run`, `source` (path, or `generate:<pattern>`), `output`, `format`
- `preset`, `algorithm`, `color_space`, `palette_name`, `palette_colors[]`
- `source_dimensions`, `working_dimensions`, `output_dimensions` (each `[w, h]`)
- `bytes_written`, `elapsed_ms`
- `color_usage[]`, each `{hex, pixels, percent}`
- Optional: `recipe`, `preview_braille`, `saved_preset_path`

**animate**

- `status`, `dry_run`, `source`, `output`, `frames_dir`, `anim_format`, `temporal`
- `frame_count`, `fps`, `total_duration_ms`, `loops`
- `preset`, `algorithm`, `color_space`, `palette_name`, `palette_colors[]`
- `source_dimensions`, `frame_dimensions`
- `spritesheet_grid` (spritesheets only)
- `max_size_bytes`, `size_fit_scale` (only with `--max-size`)
- `bytes_written`, `elapsed_ms`, `color_usage[]`

**compare**

- `status`, `dry_run`, `source`, `sheet_output`, `sheet_dimensions`, `out_dir`
- `variants[]`, each `{label, algorithm, palette, preset, file, elapsed_ms}`

**batch**

- `status` (`ok` or `partial_failure`), `dry_run`, `out_dir`, `total`, `succeeded`, `failed`
- `items[]`, each `{input, output, status, output_dimensions, bytes_written, elapsed_ms, error?}`

**update**

- `status`, `current_version`, `latest_version`, `update_available`, `updated`
- `target`, `install_path`, `release_url`
- `skills_updated`, `skills_agents[]`

## Errors and exit codes

With `--json`, errors are printed to stderr:

```json
{
  "status": "error",
  "code": "UNKNOWN_PRESET",
  "message": "unknown preset 'nope'",
  "suggestion": "Run `dith list presets --json` ...",
  "valid_values": ["blueprint", "crt-amber", "..."]
}
```

| Exit | Meaning | Typical codes |
| --- | --- | --- |
| 0 | Success | |
| 2 | Invalid flags or values. Fix and retry. | `CLI_USAGE_ERROR` (clap parse error; message lists possible values), `INVALID_ARGUMENT` (out of range, missing `-o`), `UNKNOWN_PRESET` and other unknown-slug codes (with `valid_values`), `OUTPUT_EXISTS` |
| 1 | Runtime or I/O failure | `RUNTIME_ERROR` (missing or undecodable input, write failure, batch item failures) |

Without `--json`, errors print as `error [CODE]: message` followed by `hint: suggestion`.

## Browser and Node (WebAssembly)

The `dith` npm package wraps the same engine compiled to WebAssembly (ESM, browsers and Node 20+), with a worker pool in browsers. It renders a recipe to the same bytes as the CLI.

```ts
import { render, source, latest, cliCommand, resolveRecipe } from "dith";
const out = await render(file, { preset: "risograph", maxDim: 1200, format: "png" }); // out.bytes, out.mime, out.report
const orb = await render({ pattern: "sphere", width: 800, height: 600 }, { preset: "editorial-print", format: "svg" }); // orb.text
cliCommand({ options: { preset: "risograph", maxDim: 1200 }, input: "photo.jpg" }).command; // dith apply photo.jpg -o out.png --preset risograph --max-dim 1200
```

- Options are every recipe knob named after its flag (`maxDim`, `pixelSize`, `palette: ["#111", "#eee"]` or `auto(8)`, ...), layered like the CLI: `recipe`, then `preset`, then the rest. `resolveRecipe()` gives the `--emit-recipe` JSON; `parseRecipe()`/`stringifyRecipe()` read and write it.
- Generator presets drop photo preparation the way `generate --preset` does.
- `format` picks the result: `png`/`webp`/... bytes, `svg`/`braille`/`ascii` text, `bitmap`, `rgba`, `output: "blob"`, or `target: canvas`. `preview: N` renders a fast reduced approximation; `source(input)` decodes once for many renders, and `latest()` drops superseded renders.
- Errors are `DithError`s with the CLI's `code`s plus `ABORTED`, `INVALID_INPUT`, `UNSUPPORTED`, `WORKER_ERROR`. TIFF input is not decoded in WebAssembly; use `parsePalette()` instead of `--palette-file`. Animations are CLI-only.
- The playground in `web/` (deployed to GitHub Pages) uses the package and copies the exact `dith apply`/`dith generate` command, or a recipe file for `--recipe`.
- Build: `scripts/build-wasm.sh`, then `npm run build` in `npm/` (types generated from the engine, `vp pack`); `cargo build --lib --target wasm32-unknown-unknown --no-default-features --features wasm` builds the library alone.
