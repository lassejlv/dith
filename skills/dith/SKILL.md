---
name: dith
description: >-
  Craft deterministic dithered images, vector SVGs, procedural art, and looping animations with the dith CLI.
  Use when an agent needs to dither photos or assets, generate procedural dither graphics, animate GIFs/APNGs/spritesheets or film-boil loops, build algorithm/preset comparison contact sheets, save reusable local presets, render terminal previews, or batch-dither directories.
---

# Dith

Use `dith` to produce deterministic, palette-exact dithered artwork, vector halftones, and animations with structured JSON reporting.

## Run the workflow

1. Confirm `dith` is available (`dith --version`). Inside the `dith` source checkout without a global install, invoke via `mbx run -- ...` (or `cargo run --release -- ...`). If `dith` is missing, tell the user and offer the build/install command from [CLI reference](references/cli.md); wait for authorization before installing software.
2. Query live capabilities instead of memorizing catalogs: run `dith capabilities` once for the full JSON manifest, or targeted `dith list presets|algorithms|palettes|generators --json` (filterable via `--tag` / `--category`).
3. For existing photos or when the visual direction is open, run `dith inspect <input> --json` to read dimensions, luminance, dominant Oklab colors, and recommended commands. Render a labeled contact sheet with `dith compare <input> -o sheet.png --presets ... --json` when choosing between looks.
4. Execute the render (`apply`, `generate`, `animate`, or `batch`) with `--json`. Run `--dry-run --json` first when validating complex flag sets or batch targets, and pass `--no-clobber` whenever existing files must stay untouched.
5. Verify completion from the JSON output before reporting success: confirm `"status": "ok"`, expected `output_dimensions` (or `frame_count` and `frame_dimensions`), non-zero `bytes_written`, and expected `palette_colors` / `color_usage`. When aesthetic quality matters, visually inspect the saved image file directly (or pass `--preview` for an inline Braille thumbnail in the JSON report).

## Choose flags and modes cleanly

- **Structured I/O**: Pass `--json` on every agent invocation so stdout returns machine-readable results and errors return structured JSON (`code`, `suggestion`, `valid_values`) on stderr (exit `2` for invalid flags/slugs, exit `1` for I/O errors).
- **Headless safety**: When showing terminal previews in non-interactive shells, pass `--inline` (or `--format kitty|braille|ascii`) rather than fullscreen `--view`, which targets interactive TTYs.
- **Composable presets**: Start from `--preset <slug>` and override individual fields (`--algorithm`, `--colors`, `--pixel-size`, `--contrast`, `--sharpen`, `--max-dim`) instead of specifying every flag manually. Pass `--emit-recipe` to capture the exact resolved JSON recipe, or `--save-preset <slug>` to persist it.
- **Local preset persistence**: Save repo-specific brand styles with `--scope project` (`.dith/presets/<slug>.json`) and personal defaults with `--scope user` (`~/.config/dith/presets/<slug>.json`). Lookup precedence is `$DITH_PRESETS_DIR` → `.dith/presets/` → `~/.config/dith/presets/` → built-ins.
- **Tonal & palette craft**:
  - Keep the default `--color-space oklab` for perceptually accurate gradients; switch to `--color-space srgb` only when replicating legacy hardware math.
  - Pair soft photos with `--sharpen 0.3..0.6` (and `--auto-levels` on low-contrast inputs) before 1-bit or Atkinson dithering.
  - Use `--auto-palette N` (`2..=64`) to extract optimal Oklab colors directly from the source image, or `--colors "#hex1,#hex2,..."` for exact brand palettes.
  - Use `--pixel-size N` for chunky retro pixel grids while preserving output dimensions.
- **Vector SVG sizing**: Keep `--max-dim` modest (`120..320`) when exporting `.svg` so markup stays lean. Use `--svg-shape rect` for crisp RLE-merged pixel art, or `--svg-shape circle|dot --svg-skip-bg` for halftone overlays.
- **Animation stability**: `dith animate` accepts procedural pattern names, animated GIF/APNG/WebP files, numbered image sequences, or a single static photo (auto-defaults to `--temporal boil`). Keep the default shared global palette so colors stay steady across frames, and use `--ping-pong` or `--sweep-mode pulse` for seamless loops.

Read [CLI reference](references/cli.md) for install commands, subcommand recipes, stdin/stdout piping, and animation sweep syntax.
